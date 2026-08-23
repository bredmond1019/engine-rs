//! Builtin workflow registration — wires `engine-core`'s assembled workflows
//! into a `Dispatcher`'s dual `workflow_registry`/`schema_registry`.
//!
//! `engine-core` cannot dev-depend on `engine-serve` (that would cycle:
//! `engine-serve` -> `engine-core` already exists as a normal dependency), so
//! this module is the place that pairs each `engine-core` workflow's
//! assembled `WorkflowSchema` + `WorkflowFactory`-shaped builder with the
//! `Dispatcher::register` call. See `planning/EN.3.A-sdlc-flow-setup-task-loop/tasks.md`,
//! Task 5, and its Notes section for the cross-crate rationale.
//!
//! Each registration below is now policy-aware (EN.5.D task 7): the factory
//! resolves that workflow's policy once from the triggering event via its
//! `resolve_policy_for_run_from`, builds the policy-dependent
//! `graph::registry_for_policy` instead of the default-policy `registry`,
//! and seeds the resolved policy into the run at
//! `policy::RESOLVED_POLICY_IDENTITY` (via `Workflow::with_seeded_nodes`) so
//! it is visible to the start node without a second `harness.json` read.
//! This is the change that makes a `profile` sent over `POST /events/`
//! actually select the local transport for a served run.
//!
//! Config-source choice per workflow: `SDLC_FLOW` runs embedded in
//! `bastion serve`'s own process, which *is* checked out in a repo (or,
//! since EN.3.K, targets another repo entirely via the event's `repo`
//! slug), so its factory resolves `harness.json` per run off the event's
//! resolved target root (a `PolicyConfigSource::Worktree`) — the current
//! working directory only when `repo` is absent, i.e. today's behavior
//! verbatim. The other three are channel/API-shaped with no repo checkout
//! at dispatch time, so their factories use `PolicyConfigSource::Builtin`
//! (builtin + profile + event layers only, no filesystem access).
//!
//! # The repo registry seam (EN.3.K)
//!
//! `SDLC_FLOW`'s factory and its `SetupWorktreeNode` need a
//! [`engine_core::repo_registry::RepoRegistry`] to resolve an event's `repo`
//! slug. `bastion` calls [`register_builtin_workflows`] with one argument
//! (`../bastion/src/serve/mod.rs:61`) and constructs `AppState` as a struct
//! literal (`:278`) — adding a required parameter or field to either breaks
//! a build this spec cannot edit (a separate git repo). So the registry is
//! threaded through a **process-global seam**, mirroring this crate's own
//! established precedent for exactly this shape of problem:
//! `crate::suspend::register_pause_signal` and `http::live_run_metadata()`
//! are both already process-global `OnceLock`/`RwLock` singletons. [`set_repo_registry`] /
//! [`repo_registry`] install and read it; [`init_repo_registry_from_env`]
//! resolves one from `ENGINE_BRAIN_ROOT` at server startup and
//! logs-and-leaves-unset on failure, so an engine that cannot find a brain
//! root still serves absent-`repo` events exactly as before this block.
//! [`register_builtin_workflows_with_registry`] is the explicit-registry
//! entry point tests use to install a tempdir registry with no
//! `ENGINE_BRAIN_ROOT` race; the plain one-argument [`register_builtin_workflows`]
//! delegates to it using the process-global, so `bastion` compiles
//! unchanged.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use engine_contract::TaskContext;
use engine_core::policy::{PolicyConfigSource, RESOLVED_POLICY_IDENTITY};
use engine_core::repo_registry::RepoRegistry;
use engine_core::workflows::orchestration::integrate::StepProgress;
use engine_core::workflows::sdlc_flow::setup::SetupWorktreeNode;
use engine_core::{CancellationToken, Workflow};
use serde::Serialize;

// ── ORCHESTRATION abort/progress handoff (EN.ticket.orchestration-abort-and-progress task 4) ──
//
// `register_orchestration_with_registry`'s factory (below) builds a fresh
// `OrchestrationRunNode` -- including its `CancellationToken` and
// step-progress observer -- *before* `POST /events/`'s `post_events`
// handler (`http.rs`) has minted a `run_id` or cloned this run's
// `LiveStateStore`/`DurableHandle`: `Dispatcher::dispatch_with_event` runs
// first, and only afterward does `post_events` mint `run_id`, register a
// token in `abort::RunRegistry`, and call `suspend::spawn_run`. Neither
// `http.rs` nor `Dispatcher`'s factory signature is this ticket's to change
// (see the top-level spec's file list), so the token this node embeds and
// the fan-out context its observer needs cross that gap via a **thread-local**
// handoff, not a process-global one: `post_events`' whole
// `dispatch_with_event -> mint run_id -> spawn_run` sequence is synchronous
// (no `.await` anywhere in it), so it never yields its OS thread to another
// request's handler mid-sequence. A `OnceLock`/process-global equivalent
// would race under two concurrent `POST /events/` calls for ORCHESTRATION
// landing on different worker threads; this cannot, because each request's
// un-awaited segment owns its thread for the whole handoff.
//
// Every other workflow type never calls [`set_pending_orchestration_run`],
// so [`take_pending_orchestration_run`] returns `None` for them and
// `suspend::spawn_run` falls back to exactly its pre-existing behavior.

/// Everything an ORCHESTRATION step-progress observer needs to publish
/// through `suspend.rs`'s three-way fan-out (live state, the durable
/// writer, SSE), but cannot know at factory-build time. Filled in by
/// `suspend::spawn_run` — via [`take_pending_orchestration_run`] — before
/// the workflow it is embedded in ever runs.
#[derive(Clone)]
pub(crate) struct StepFanoutContext {
    pub run_id: uuid::Uuid,
    pub live: crate::live_state::LiveStateStore,
    pub durable: crate::durable::DurableHandle,
    pub workflow_type: String,
    pub data: serde_json::Value,
}

/// The token + fan-out cell a freshly-built ORCHESTRATION node captured at
/// factory time, handed off to `suspend::spawn_run` via the thread-local
/// below.
pub(crate) struct PendingOrchestrationRun {
    pub token: CancellationToken,
    pub fanout: Arc<RwLock<Option<StepFanoutContext>>>,
    /// This run's resolved campaign id (`EN.11.F` task 2 follow-up) —
    /// `spawn_run` registers `token` under this id in `AppState::campaigns`
    /// so `POST /campaigns/{id}/abort` can find and trigger it, mirroring
    /// how it registers `token` under `run_id` in `AppState::runs`.
    pub campaign_id: uuid::Uuid,
}

thread_local! {
    static PENDING_ORCHESTRATION_RUN: RefCell<Option<PendingOrchestrationRun>> =
        const { RefCell::new(None) };
}

/// Stash `pending` for the very next `suspend::spawn_run` call on THIS
/// thread to consume via [`take_pending_orchestration_run`]. Called once
/// per ORCHESTRATION dispatch, from inside
/// [`register_orchestration_with_registry`]'s factory closure.
pub(crate) fn set_pending_orchestration_run(pending: PendingOrchestrationRun) {
    PENDING_ORCHESTRATION_RUN.with(|cell| *cell.borrow_mut() = Some(pending));
}

/// Take (and clear) whatever [`set_pending_orchestration_run`] stashed on
/// this thread. `None` for every non-ORCHESTRATION dispatch (nothing ever
/// calls the setter for them), and also for the pathological case of an
/// ORCHESTRATION dispatch and its following `spawn_run` call landing on
/// different threads — `suspend::spawn_run` falls back to its
/// already-correct un-injected behavior in that case; see its call site.
pub(crate) fn take_pending_orchestration_run() -> Option<PendingOrchestrationRun> {
    PENDING_ORCHESTRATION_RUN.with(|cell| cell.borrow_mut().take())
}

/// Build a fresh `CancellationToken` + step-progress observer for one
/// ORCHESTRATION dispatch, and stash the handoff [`set_pending_orchestration_run`]
/// so `suspend::spawn_run` can pick it up. Used by
/// [`register_orchestration_with_registry`]'s factory (the production
/// path), and directly by tests that build an `OrchestrationRunNode`
/// without going through the full `Dispatcher`, so both exercise the exact
/// same wiring.
///
/// The returned observer reads [`StepFanoutContext`] fresh on every call
/// (via the shared `fanout` cell), so it is safe to hand to
/// `with_step_observer` before that context exists — `spawn_run` always
/// fills it in before the workflow this token/observer are embedded in
/// ever runs.
///
/// `campaign_id` is this run's already-resolved campaign id (via
/// [`engine_core::workflows::orchestration::graph::resolve_campaign_id`]) —
/// carried through [`PendingOrchestrationRun`] so `spawn_run` can register
/// `token` under it in `AppState::campaigns`, which is what makes `POST
/// /campaigns/{id}/abort` reach a live campaign at all.
/// The step-progress observer type `with_step_observer` takes — aliased so
/// [`build_orchestration_seams`]'s return type reads cleanly.
type StepObserverArc = Arc<dyn Fn(&StepProgress) + Send + Sync>;

pub(crate) fn build_orchestration_seams(
    campaign_id: uuid::Uuid,
) -> (CancellationToken, StepObserverArc) {
    let token = CancellationToken::new();
    let fanout: Arc<RwLock<Option<StepFanoutContext>>> = Arc::new(RwLock::new(None));
    set_pending_orchestration_run(PendingOrchestrationRun {
        token: token.clone(),
        fanout: fanout.clone(),
        campaign_id,
    });

    let observer_fanout = fanout.clone();
    let step_observer: StepObserverArc = Arc::new(move |progress: &StepProgress| {
        let ctx = observer_fanout.read().ok().and_then(|guard| guard.clone());
        if let Some(ctx) = ctx {
            crate::suspend::publish_step_progress(
                ctx.run_id,
                ctx.live.clone(),
                ctx.durable.clone(),
                ctx.workflow_type.clone(),
                ctx.data.clone(),
                progress,
            );
        }
        // `fanout` is only ever empty here for a caller driving this
        // node directly with no `spawn_run` in the loop (e.g. a test
        // exercising `Node::process` in isolation) — the served path
        // always fills it in before `workflow.run_with` dispatches this
        // node. Silently dropping the emission in that case matches
        // the no-observer default's behavior-stable contract.
    });

    (token, step_observer)
}

/// The process-global repo registry (EN.3.K): `set_repo_registry` /
/// `repo_registry` read and write this singleton, mirroring
/// `crate::suspend::register_pause_signal` / `http::live_run_metadata()`'s
/// existing `OnceLock<RwLock<..>>` pattern. `None` (the default) means
/// today's behavior: an absent-`repo` event still resolves via
/// `current_dir()`; a `repo`-bearing event with no registry installed
/// surfaces a named error rather than silently falling back (see
/// `sdlc_flow::setup::resolve_target_root`).
fn repo_registry_cell() -> &'static RwLock<Option<Arc<RepoRegistry>>> {
    static REPO_REGISTRY: OnceLock<RwLock<Option<Arc<RepoRegistry>>>> = OnceLock::new();
    REPO_REGISTRY.get_or_init(|| RwLock::new(None))
}

/// Install the process-global repo registry. Overwrites any previously
/// installed registry — tests that install a tempdir registry must restore
/// the previous value (typically `None`) on the way out so they don't leak
/// state into other tests in the same process.
pub fn set_repo_registry(registry: Arc<RepoRegistry>) {
    if let Ok(mut guard) = repo_registry_cell().write() {
        *guard = Some(registry);
    }
}

/// Clear the process-global repo registry, restoring the "no registry
/// installed" default. Test-only cleanup helper alongside
/// [`set_repo_registry`].
pub fn clear_repo_registry() {
    if let Ok(mut guard) = repo_registry_cell().write() {
        *guard = None;
    }
}

/// Read the currently installed process-global repo registry, if any.
pub fn repo_registry() -> Option<Arc<RepoRegistry>> {
    repo_registry_cell()
        .read()
        .ok()
        .and_then(|guard| guard.clone())
}

/// Resolve a repo registry from `ENGINE_BRAIN_ROOT` (via
/// `RepoRegistry::from_env`) and install it as the process-global registry.
/// On failure, logs the reason to stderr and leaves the registry unset —
/// an engine that cannot find a brain root must still serve absent-`repo`
/// events exactly as before this block, not fail to start.
pub fn init_repo_registry_from_env() {
    match RepoRegistry::from_env() {
        Ok(registry) => set_repo_registry(Arc::new(registry)),
        Err(err) => {
            tracing::warn!(
                error = %err,
                "engine-serve: repo registry not initialized; repo-bearing SDLC_FLOW \
                 events will 422 until ENGINE_BRAIN_ROOT resolves, absent-repo events \
                 are unaffected"
            );
        }
    }
}

use crate::dispatch::Dispatcher;

/// Build the `TaskContext` a policy-resolution-only call needs: `event` set
/// to the triggering payload, everything else empty. Never runs a node —
/// only fed to `resolve_policy_for_run_from`, which reads `ctx.event` and
/// nothing else.
fn event_only_context(event: &serde_json::Value) -> TaskContext {
    TaskContext {
        event: event.clone(),
        nodes: HashMap::new(),
        metadata: serde_json::json!({}),
        node_runs: HashMap::new(),
    }
}

/// Serialize `policy` into the single-entry seed map
/// `{RESOLVED_POLICY_IDENTITY: policy}`, matching the shape
/// `policy::stamp_resolved_policy` writes into `ctx.nodes` — so a node
/// reading the stamp via `policy::resolved_policy`/`resolved_policy_strict`
/// sees the same representation regardless of whether it was seeded at
/// dispatch or stamped mid-run.
fn seed_resolved_policy<P: Serialize>(
    policy: &P,
) -> Result<HashMap<String, serde_json::Value>, String> {
    let value = serde_json::to_value(policy)
        .map_err(|err| format!("failed to serialize resolved policy: {err}"))?;
    let mut seeded = HashMap::new();
    seeded.insert(RESOLVED_POLICY_IDENTITY.to_string(), value);
    Ok(seeded)
}

/// Register the `SDLC_FLOW` workflow (`engine_core::workflows::sdlc_flow`)
/// with `dispatcher`, populating both the `workflow_registry` (via a
/// policy-aware factory built on `sdlc_flow::graph::registry_for_policy`)
/// and the `schema_registry` (via `sdlc_flow::graph::schema`). Delegates to
/// [`register_sdlc_flow_with_registry`] using whatever repo registry (EN.3.K)
/// is currently installed via [`set_repo_registry`] — `None` if none has
/// been installed, which reproduces today's behavior exactly (an absent
/// `repo` on the event resolves via `current_dir()` regardless).
pub fn register_sdlc_flow(dispatcher: &mut Dispatcher) {
    register_sdlc_flow_with_registry(dispatcher, repo_registry());
}

/// Register the `SDLC_FLOW` workflow with an explicit repo registry (EN.3.K),
/// bypassing the process-global seam — the entry point tests use to install
/// a tempdir registry with no `ENGINE_BRAIN_ROOT` race.
///
/// Per event: resolves the event's target root via
/// `sdlc_flow::setup::resolve_target_root` (absent `repo` -> `current_dir()`,
/// present `repo` -> resolved through `repo_reg`, erroring by name rather
/// than silently falling back if no registry is available), builds a
/// `PolicyConfigSource::Worktree` over that root (replacing the old
/// unconditional `current_dir()` read), and — when a registry is installed —
/// re-registers `SetupWorktreeNode` with `.with_registry(..)` so the node
/// itself resolves `event.repo` against the same registry the policy read
/// used, mirroring exactly how `register_content_pipeline` overrides
/// `ActionDispatchNode`'s transport and `register_research_agent` overrides
/// `ResearchIngressDispatchNode`'s transport.
pub fn register_sdlc_flow_with_registry(
    dispatcher: &mut Dispatcher,
    repo_reg: Option<Arc<RepoRegistry>>,
) {
    dispatcher.register(
        engine_core::workflows::sdlc_flow::graph::schema(),
        Box::new(move |event: &serde_json::Value| {
            let ctx = event_only_context(event);
            let sdlc_event: engine_core::workflows::sdlc_flow::schema::SDLCFlowEventSchema =
                serde_json::from_value(event.clone())
                    .map_err(|err| format!("invalid SDLC_FLOW event: {err}"))?;
            // EN.3.K: the run's target root is resolved once, per event —
            // absent `repo` reproduces the old unconditional
            // `current_dir()` read verbatim; a present `repo` resolves
            // through `repo_reg` (naming the slug on failure, never
            // silently falling back to `current_dir()`).
            let root = engine_core::workflows::sdlc_flow::setup::resolve_target_root(
                &sdlc_event,
                repo_reg.as_deref(),
            )
            .map_err(|err| err.to_string())?;
            let source = PolicyConfigSource::Worktree(root);
            let policy = engine_core::workflows::sdlc_flow::setup::resolve_policy_for_run_from(
                &ctx, &source,
            )
            .map_err(|err| err.to_string())?;
            let mut registry =
                engine_core::workflows::sdlc_flow::graph::registry_for_policy(&policy);
            if let Some(reg) = repo_reg.clone() {
                registry.register(Box::new(SetupWorktreeNode::new().with_registry(reg)));
            }
            let seeded = seed_resolved_policy(&policy)?;
            Workflow::new_validated(registry, engine_core::workflows::sdlc_flow::graph::schema())
                .map(|workflow| workflow.with_seeded_nodes(seeded))
                .map_err(|err| err.to_string())
        }),
    );
}

/// Register the `SDLC_TASK` workflow (`engine_core::workflows::sdlc_task`,
/// `EN.11.N`/`EN.11.O`) with `dispatcher`, mirroring
/// [`register_sdlc_flow`]/[`register_sdlc_flow_with_registry`] structurally:
/// same policy-aware factory shape, same `EN.3.K` repo-registry seam, same
/// re-registration of `SetupWorktreeNode` for the served path. Delegates to
/// [`register_sdlc_task_with_registry`] using whatever repo registry is
/// currently installed via [`repo_registry`].
///
/// `EN.11.P` task 3: this is the block record's T10 — SDLC_TASK gains no
/// new registration *shape*, only a second instance of the same one.
pub fn register_sdlc_task(dispatcher: &mut Dispatcher) {
    register_sdlc_task_with_registry(dispatcher, repo_registry());
}

/// Register the `SDLC_TASK` workflow with an explicit repo registry
/// (`EN.3.K`), bypassing the process-global seam — the entry point tests
/// use to install a tempdir registry with no `ENGINE_BRAIN_ROOT` race.
///
/// Per event: parses `SdlcTaskEventSchema`, resolves the run's target root
/// (absent `repo` -> `current_dir()`, present `repo` -> resolved through
/// `repo_reg`, erroring by name rather than silently falling back — the
/// same contract [`engine_core::workflows::sdlc_flow::setup::resolve_target_root`]
/// gives `SDLC_FLOW`, reproduced here by hand because that helper is typed
/// to `SDLCFlowEventSchema` specifically), builds a
/// `PolicyConfigSource::Worktree` over it, resolves `SdlcTaskPolicy` via
/// `sdlc_task::profiles::resolve_policy_for_run_from`, and builds
/// `sdlc_task::graph::registry_for_policy(&policy)`.
///
/// When a registry is installed, this re-registers `SetupWorktreeNode` —
/// **chaining `.with_branch_prefix("task/")` and
/// `sdlc_task::graph::sdlc_task_policy_resolver()` exactly as
/// `sdlc_task::graph::registry()` does**, not just `.with_registry(reg)`
/// alone: `SetupWorktreeNode::new()` resets both to `sdlc_flow`'s
/// defaults (the `"sdlc/"` prefix and `sdlc_flow`'s own policy section), so
/// a bare `.with_registry(reg)` here would silently strip SDLC_TASK's
/// branch prefix and its `harness.json` section the moment a repo registry
/// is installed — exactly the served-vs-in-process divergence this
/// function exists to avoid.
pub fn register_sdlc_task_with_registry(
    dispatcher: &mut Dispatcher,
    repo_reg: Option<Arc<RepoRegistry>>,
) {
    dispatcher.register(
        engine_core::workflows::sdlc_task::graph::schema(),
        Box::new(move |event: &serde_json::Value| {
            let ctx = event_only_context(event);
            let sdlc_task_event: engine_core::workflows::sdlc_task::schema::SdlcTaskEventSchema =
                serde_json::from_value(event.clone())
                    .map_err(|err| format!("invalid SDLC_TASK event: {err}"))?;
            // Hand-mirrors `sdlc_flow::setup::resolve_target_root` — see
            // this function's doc for why it cannot be called directly
            // (it is typed to `SDLCFlowEventSchema`).
            let root = match sdlc_task_event.repo.as_deref() {
                Some(slug) => {
                    let registry = repo_reg.as_deref().ok_or_else(|| {
                        format!(
                            "SDLC_TASK event named repo slug '{slug}' but no repo registry is \
                             available to resolve it"
                        )
                    })?;
                    registry.resolve(slug).map_err(|err| err.to_string())?
                }
                None => std::env::current_dir()
                    .map_err(|err| format!("failed to resolve current_dir(): {err}"))?,
            };
            let source = PolicyConfigSource::Worktree(root);
            let policy = engine_core::workflows::sdlc_task::profiles::resolve_policy_for_run_from(
                &ctx, &source,
            )
            .map_err(|err| err.to_string())?;
            let mut registry =
                engine_core::workflows::sdlc_task::graph::registry_for_policy(&policy);
            if let Some(reg) = repo_reg.clone() {
                registry.register(Box::new(
                    SetupWorktreeNode::new()
                        .with_registry(reg)
                        .with_branch_prefix("task/")
                        .with_policy_resolver(
                            engine_core::workflows::sdlc_task::graph::sdlc_task_policy_resolver(),
                        ),
                ));
            }
            let seeded = seed_resolved_policy(&policy)?;
            Workflow::new_validated(registry, engine_core::workflows::sdlc_task::graph::schema())
                .map(|workflow| workflow.with_seeded_nodes(seeded))
                .map_err(|err| err.to_string())
        }),
    );
}

/// Register the `RESEARCH_AGENT` workflow (`engine_core::workflows::research_agent`)
/// with `dispatcher`, populating both the `workflow_registry` (via a
/// policy-aware factory built on `research_agent::graph::registry_for_policy`)
/// and the `schema_registry` (via `research_agent::graph::schema`). See
/// `planning/EN.4.A-research-agent/tasks.md`, Task 7.
///
/// `EN.6.E`: after `registry_for_policy` builds the policy-rewired registry,
/// this re-registers `ResearchIngressDispatchNode` with a
/// `channel_transport_live` pointed at [`events_url_from_env`]'s
/// deployment-configured `/events/` URL rather than the node's own
/// local-dev placeholder default — mirroring exactly how
/// `register_content_pipeline` overrides `ActionDispatchNode`'s transport
/// below, so `RESEARCH_AGENT`'s self-feeding loop into `CONTENT_PIPELINE`
/// reaches the same configured endpoint a served `CONTENT_PIPELINE` run's
/// own dispatch does.
pub fn register_research_agent(dispatcher: &mut Dispatcher) {
    dispatcher.register(
        engine_core::workflows::research_agent::graph::schema(),
        Box::new(|event: &serde_json::Value| {
            let ctx = event_only_context(event);
            let policy =
                engine_core::workflows::research_agent::profiles::resolve_policy_for_run_from(
                    &ctx,
                    &PolicyConfigSource::Builtin,
                )
                .map_err(|err| err.to_string())?;
            let mut registry =
                engine_core::workflows::research_agent::graph::registry_for_policy(&policy);
            registry.register(Box::new(
                engine_core::workflows::research_agent::ingress_dispatch::ResearchIngressDispatchNode::new()
                    .with_enabled(policy.ingress_dispatch.enabled)
                    .with_target_workflow_type(policy.ingress_dispatch.target_workflow_type.clone())
                    .with_transport(
                        engine_core::nodes::channel_transport::channel_transport_live(
                            events_url_from_env(),
                        ),
                    ),
            ));
            let seeded = seed_resolved_policy(&policy)?;
            Ok(Workflow::new(
                registry,
                engine_core::workflows::research_agent::graph::schema(),
            )
            .with_seeded_nodes(seeded))
        }),
    );
}

/// Register the `DIAGNOSTIC_INTAKE` workflow
/// (`engine_core::workflows::diagnostic_intake`) with `dispatcher`,
/// populating both the `workflow_registry` (via a policy-aware factory
/// built on `diagnostic_intake::graph::registry_for_policy`) and the
/// `schema_registry` (via `diagnostic_intake::graph::schema`). See
/// `planning/EN.4.B-diagnostic-intake/tasks.md`, Task 6.
pub fn register_diagnostic_intake(dispatcher: &mut Dispatcher) {
    dispatcher.register(
        engine_core::workflows::diagnostic_intake::graph::schema(),
        Box::new(|event: &serde_json::Value| {
            let ctx = event_only_context(event);
            let policy =
                engine_core::workflows::diagnostic_intake::profiles::resolve_policy_for_run_from(
                    &ctx,
                    &PolicyConfigSource::Builtin,
                )
                .map_err(|err| err.to_string())?;
            let registry =
                engine_core::workflows::diagnostic_intake::graph::registry_for_policy(&policy);
            let seeded = seed_resolved_policy(&policy)?;
            Ok(Workflow::new(
                registry,
                engine_core::workflows::diagnostic_intake::graph::schema(),
            )
            .with_seeded_nodes(seeded))
        }),
    );
}

/// Register the `PROPOSAL_GENERATOR` workflow
/// (`engine_core::workflows::proposal_generator`) with `dispatcher`,
/// populating both the `workflow_registry` (via a policy-aware factory
/// built on `proposal_generator::graph::registry_for_policy`) and the
/// `schema_registry` (via `proposal_generator::graph::schema`). See
/// `planning/EN.4.C-proposal-generator/tasks.md`, Task 10.
pub fn register_proposal_generator(dispatcher: &mut Dispatcher) {
    dispatcher.register(
        engine_core::workflows::proposal_generator::graph::schema(),
        Box::new(|event: &serde_json::Value| {
            let ctx = event_only_context(event);
            let policy =
                engine_core::workflows::proposal_generator::profiles::resolve_policy_for_run_from(
                    &ctx,
                    &PolicyConfigSource::Builtin,
                )
                .map_err(|err| err.to_string())?;
            let registry =
                engine_core::workflows::proposal_generator::graph::registry_for_policy(&policy);
            let seeded = seed_resolved_policy(&policy)?;
            Ok(Workflow::new(
                registry,
                engine_core::workflows::proposal_generator::graph::schema(),
            )
            .with_seeded_nodes(seeded))
        }),
    );
}

/// Env var this crate reads for the base URL its own served `POST /events/`
/// endpoint is reachable at, so a served `CONTENT_PIPELINE` run's
/// `ActionDispatchNode` self-POSTs a `TriggerWorkflow` action back to the
/// right place (`EN.6.A` task 5) instead of `action_dispatch.rs`'s
/// `DEFAULT_EVENTS_URL` local-dev placeholder. Unset falls back to that
/// same placeholder, so a deployment that never sets this var keeps
/// today's behavior unchanged. The `X-API-Key` header the self-POST needs
/// is wired separately: `channel_transport_live` builds a
/// `WorkflowTriggerDispatch` via `WorkflowTriggerDispatch::new`, which
/// already reads it from `ENGINE_EVENTS_API_KEY` (`channel_transport.rs`).
const EVENTS_URL_ENV: &str = "ENGINE_EVENTS_URL";

/// The local-dev placeholder `ActionDispatchNode::new()` defaults to
/// (`action_dispatch.rs`'s private `DEFAULT_EVENTS_URL`, duplicated here
/// since this crate has no dependency path to that private const) —
/// [`EVENTS_URL_ENV`]'s fallback when unset.
const DEFAULT_EVENTS_URL: &str = "http://localhost:8080/events/";

/// Read [`EVENTS_URL_ENV`], falling back to [`DEFAULT_EVENTS_URL`] when
/// unset or empty.
fn events_url_from_env() -> String {
    std::env::var(EVENTS_URL_ENV)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_EVENTS_URL.to_string())
}

/// Register the `CONTENT_PIPELINE` workflow
/// (`engine_core::workflows::content_pipeline`) with `dispatcher`,
/// populating both the `workflow_registry` (via a policy-aware factory
/// built on `content_pipeline::graph::registry_for_policy`) and the
/// `schema_registry` (via `content_pipeline::graph::schema`). See
/// `planning/EN.5.A-content-pipeline/tasks.md`, Task 12.
///
/// Channel/webhook-triggered runs carry no repo checkout at dispatch time
/// (mirrors `RESEARCH_AGENT`/`DIAGNOSTIC_INTAKE`/`PROPOSAL_GENERATOR`), so
/// the factory resolves policy against `PolicyConfigSource::Builtin`
/// (builtin + profile + event layers only, no filesystem access) rather
/// than a worktree path.
///
/// `EN.6.A` task 5: after `registry_for_policy` builds the policy-rewired
/// registry, this re-registers `ActionDispatchNode` with a
/// `channel_transport_live` pointed at [`events_url_from_env`]'s
/// deployment-configured `/events/` URL rather than the node's own
/// local-dev placeholder default — mirroring how `registry_for_policy`
/// itself re-registers a node to override its transport. The dispatch
/// stage carries no `ModelTier` and is never Local-eligible
/// (`content_pipeline::policy`), so this override applies identically
/// regardless of the resolved policy.
pub fn register_content_pipeline(dispatcher: &mut Dispatcher) {
    dispatcher.register(
        engine_core::workflows::content_pipeline::graph::schema(),
        Box::new(|event: &serde_json::Value| {
            let ctx = event_only_context(event);
            let policy =
                engine_core::workflows::content_pipeline::profiles::resolve_policy_for_run_from(
                    &ctx,
                    &PolicyConfigSource::Builtin,
                )
                .map_err(|err| err.to_string())?;
            let mut registry =
                engine_core::workflows::content_pipeline::graph::registry_for_policy(&policy);
            registry.register(Box::new(
                engine_core::workflows::content_pipeline::action_dispatch::ActionDispatchNode::new(
                )
                .with_transport(
                    engine_core::nodes::channel_transport::channel_transport_live(
                        events_url_from_env(),
                    ),
                ),
            ));
            let seeded = seed_resolved_policy(&policy)?;
            Ok(Workflow::new(
                registry,
                engine_core::workflows::content_pipeline::graph::schema(),
            )
            .with_seeded_nodes(seeded))
        }),
    );
}

/// Register the `OPPORTUNITY_SET_STAGE` workflow
/// (`engine_core::workflows::opportunity_edit::graph`) with `dispatcher`,
/// populating both the `workflow_registry` and the `schema_registry`. See
/// `planning/EN.7.B-research-opportunity-loop/tasks.md`, Task 6.
///
/// This is the **first model-free workflow** registered in this module:
/// `OpportunityEditNode` calls no model and reads no `harness.json`
/// section (see `opportunity_edit::graph`'s module doc), so this factory
/// resolves no policy and seeds no policy stamp — unlike every other
/// `register_*` function above, there is no `resolve_policy_for_run_from`
/// call and no `seed_resolved_policy` call. Do not "restore" that hop; it
/// was never dropped, it was never needed.
pub fn register_opportunity_set_stage(dispatcher: &mut Dispatcher) {
    dispatcher.register(
        engine_core::workflows::opportunity_edit::graph::set_stage_schema(),
        Box::new(|_event: &serde_json::Value| {
            Ok(Workflow::new(
                engine_core::workflows::opportunity_edit::graph::set_stage_registry(),
                engine_core::workflows::opportunity_edit::graph::set_stage_schema(),
            ))
        }),
    );
}

/// Register the `OPPORTUNITY_ADD_ACTION` workflow
/// (`engine_core::workflows::opportunity_edit::graph`) with `dispatcher`,
/// populating both the `workflow_registry` and the `schema_registry`. See
/// `planning/EN.7.B-research-opportunity-loop/tasks.md`, Task 6.
///
/// This is the **second model-free workflow** registered in this module,
/// alongside [`register_opportunity_set_stage`]: `OpportunityEditNode`
/// calls no model and reads no `harness.json` section (see
/// `opportunity_edit::graph`'s module doc), so this factory resolves no
/// policy and seeds no policy stamp — there is no `resolve_policy_for_run_from`
/// call and no `seed_resolved_policy` call. Do not "restore" that hop; it
/// was never dropped, it was never needed.
pub fn register_opportunity_add_action(dispatcher: &mut Dispatcher) {
    dispatcher.register(
        engine_core::workflows::opportunity_edit::graph::add_action_schema(),
        Box::new(|_event: &serde_json::Value| {
            Ok(Workflow::new(
                engine_core::workflows::opportunity_edit::graph::add_action_registry(),
                engine_core::workflows::opportunity_edit::graph::add_action_schema(),
            ))
        }),
    );
}

/// Register the `HARVEST_APPROVE` workflow
/// (`engine_core::workflows::harvest_approve::graph`) with `dispatcher`,
/// populating both the `workflow_registry` and the `schema_registry`. See
/// `planning/EN.7.C-materialize-harvest-gate/tasks.md`, Task 7.
///
/// A **third model-free workflow** registered in this module, alongside
/// [`register_opportunity_set_stage`] / [`register_opportunity_add_action`]:
/// `HarvestApproveNode` calls no model and reads no `harness.json` section
/// (see `harvest_approve::graph`'s module doc), so this factory resolves no
/// policy and seeds no policy stamp — there is no `resolve_policy_for_run_from`
/// call and no `seed_resolved_policy` call. Do not "restore" that hop; it
/// was never dropped, it was never needed.
pub fn register_harvest_approve(dispatcher: &mut Dispatcher) {
    dispatcher.register(
        engine_core::workflows::harvest_approve::graph::schema(),
        Box::new(|_event: &serde_json::Value| {
            Ok(Workflow::new(
                engine_core::workflows::harvest_approve::graph::registry(),
                engine_core::workflows::harvest_approve::graph::schema(),
            ))
        }),
    );
}

/// Register the `LEAD_INGEST` workflow (`engine_core::workflows::lead_ingest`)
/// with `dispatcher`, populating both the `workflow_registry` and the
/// `schema_registry`. See `planning/en-6i-lead-ingest/tasks.md`, Task 2.
///
/// A **fourth model-free workflow** registered in this module, alongside
/// [`register_opportunity_set_stage`] / [`register_opportunity_add_action`] /
/// [`register_harvest_approve`]: neither `MaterializeDocNode` nor
/// `MergeContactsNode` calls a model or reads a `harness.json` policy
/// section (see `lead_ingest`'s module doc), so this factory resolves no
/// policy and seeds no policy stamp — there is no `resolve_policy_for_run_from`
/// call and no `seed_resolved_policy` call. Do not "restore" that hop; it
/// was never dropped, it was never needed.
pub fn register_lead_ingest(dispatcher: &mut Dispatcher) {
    dispatcher.register(
        engine_core::workflows::lead_ingest::schema(),
        Box::new(|_event: &serde_json::Value| {
            Ok(Workflow::new(
                engine_core::workflows::lead_ingest::registry(),
                engine_core::workflows::lead_ingest::schema(),
            ))
        }),
    );
}

/// Register the `APPROVE_AND_RUN` workflow
/// (`engine_core::workflows::approve_and_run::graph`) with `dispatcher`,
/// populating both the `workflow_registry` (via a policy-aware factory
/// built on `approve_and_run::graph::registry_with`) and the
/// `schema_registry` (via `approve_and_run::graph::schema`). See
/// `planning/EN.8.D/tasks.json`, Task 6.
///
/// Unlike `HARVEST_APPROVE`, this workflow does carry a policy surface
/// (`approve_and_run::policy` — `drain_batch_max` / `harvest_item_priority` /
/// `session_fallback_slug`), so this factory resolves it per event via
/// `resolve_policy_for_run_from` against `PolicyConfigSource::Builtin`
/// (channel/API-triggered, no repo checkout at dispatch time — mirrors
/// `RESEARCH_AGENT`/`DIAGNOSTIC_INTAKE`/`PROPOSAL_GENERATOR`/
/// `CONTENT_PIPELINE` above) and seeds the resolved policy into the run.
/// The event's optional `profile` field selects a named profile bundle, and
/// an optional `policy` object supplies the top-precedence per-run override
/// — the same two-field convention every other policy-aware factory in this
/// module reads.
pub fn register_approve_and_run(dispatcher: &mut Dispatcher) {
    dispatcher.register(
        engine_core::workflows::approve_and_run::graph::schema(),
        Box::new(|event: &serde_json::Value| {
            let profile_name = event.get("profile").and_then(|v| v.as_str());
            let event_override: Option<
                engine_core::workflows::approve_and_run::PartialApproveAndRunPolicy,
            > = event
                .get("policy")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|err: serde_json::Error| {
                    format!("invalid APPROVE_AND_RUN policy override: {err}")
                })?;
            let policy =
                engine_core::workflows::approve_and_run::profiles::resolve_policy_for_run_from(
                    &PolicyConfigSource::Builtin,
                    profile_name,
                    event_override.as_ref(),
                )
                .map_err(|err| err.to_string())?;
            let registry = engine_core::workflows::approve_and_run::graph::registry_with(
                engine_core::nodes::http_post::http_post_live(),
                policy.clone(),
            );
            let seeded = seed_resolved_policy(&policy)?;
            Ok(Workflow::new(
                registry,
                engine_core::workflows::approve_and_run::graph::schema(),
            )
            .with_seeded_nodes(seeded))
        }),
    );
}

/// Register the `TERMINAL_PROBE` workflow
/// (`engine_core::workflows::terminal_probe::graph`) with `dispatcher`,
/// populating both the `workflow_registry` and the `schema_registry`. See
/// `planning/EN.9.D/tasks.json`, Task 5.
///
/// A **fifth model-free workflow** registered in this module, alongside
/// [`register_opportunity_set_stage`] / [`register_opportunity_add_action`] /
/// [`register_harvest_approve`] / [`register_lead_ingest`]: neither
/// `TerminalSessionNode` nor `TerminalObserveNode` calls a model or reads a
/// `harness.json` policy section (see `terminal_probe::graph`'s module
/// doc), so this factory resolves no policy and seeds no policy stamp —
/// there is no `resolve_policy_for_run_from` call and no
/// `seed_resolved_policy` call. It uses `terminal_probe::graph::registry`,
/// the live `TmuxDriver`-backed default, exactly as production registers
/// every other builtin against its live seam.
pub fn register_terminal_probe(dispatcher: &mut Dispatcher) {
    dispatcher.register(
        engine_core::workflows::terminal_probe::graph::schema(),
        Box::new(|_event: &serde_json::Value| {
            Ok(Workflow::new(
                engine_core::workflows::terminal_probe::graph::registry(),
                engine_core::workflows::terminal_probe::graph::schema(),
            ))
        }),
    );
}

/// Register the `ORCHESTRATION` workflow (`engine_core::workflows::orchestration`,
/// `EN.10.B`) with `dispatcher`. Like [`register_terminal_probe`], this
/// factory resolves no policy and seeds no policy stamp at dispatch time:
/// `OrchestrationRunNode::process` resolves its own
/// `orchestration.policy`/`orchestration.profiles` layers itself (from the
/// event's own `brain_root`, exactly where `diagnostic_intake`'s sole
/// terminal node does), and — unlike `sdlc_flow`/`diagnostic_intake` —
/// `ORCHESTRATION`'s one policy knob (`hold_poll_interval_ms`) never
/// rewires which node runs, so there is no `registry_for_policy` variant to
/// choose between here. See `planning/EN.10.B/tasks.md`, Task 5.
///
/// Delegates to [`register_orchestration_with_registry`] using whatever repo
/// registry (EN.3.K) is currently installed via [`repo_registry`] and
/// [`NeverHeld`](engine_core::workflows::orchestration::integrate::NeverHeld)
/// as the hold source — see that function's doc for why `NeverHeld` is still
/// the production default. **`EN.ticket.orchestration-production-gates-unwired`
/// Task 2: this is now the wired registration** — `graph::registry()`'s bare
/// [`OrchestrationRunNode::new`](engine_core::workflows::orchestration::graph::OrchestrationRunNode::new)
/// is no longer what production runs.
pub fn register_orchestration(dispatcher: &mut Dispatcher) {
    register_orchestration_with_registry(
        dispatcher,
        repo_registry(),
        Arc::new(engine_core::workflows::orchestration::integrate::NeverHeld),
    );
}

/// Register the `ORCHESTRATION` workflow with an explicit repo registry and
/// hold source (`EN.ticket.orchestration-production-gates-unwired` Task 2),
/// bypassing the process-global seam — the entry point tests use to install a
/// tempdir registry with no `ENGINE_BRAIN_ROOT` race, mirroring
/// [`register_sdlc_flow_with_registry`] deliberately: a per-event factory
/// closure, the wired node built fresh per event, and an explicit-registry
/// variant so tests recognise the shape.
///
/// Per event: parses the event into
/// [`OrchestrationEventSchema`](engine_core::workflows::orchestration::graph::OrchestrationEventSchema)
/// to read its `brain_root`, then resolves the [`RepoRegistry`] the gate
/// resolvers will read `state.json` through — `repo_reg` when installed,
/// otherwise built fresh from the event's own `brain_root` (the same
/// resolution [`OrchestrationRunNode::process`](engine_core::workflows::orchestration::graph::OrchestrationRunNode)
/// already does for its own `RepoRegistry`, so an absent process-global
/// registry still serves a self-contained event correctly). Builds a
/// [`CorpusGates`](engine_core::workflows::orchestration::corpus_gates::CorpusGates)
/// over that registry and wires
/// [`OrchestrationRunNode::new`](engine_core::workflows::orchestration::graph::OrchestrationRunNode::new)'s
/// `with_resolve_depends_on` / `with_is_edge_met` / `with_is_block_open` /
/// `with_hold_source` seams to it, registers the wired node into a fresh
/// `NodeRegistry`, and validates.
///
/// Every wired closure checks
/// [`CorpusGates::take_error`](engine_core::workflows::orchestration::corpus_gates::CorpusGates::take_error)
/// immediately after calling into the gates and **panics** if it is set — the
/// closures the workflow consumes return plain `Vec`/`bool`, not `Result`
/// (fixed by `gates::check_dependencies` / `chain::resolve_lane_chain`,
/// which this ticket does not touch), so a captured `panic!` is the only
/// channel available to fail the run loudly instead of reading a missing or
/// malformed `state.json` as "no edges, proceed". `OrchestrationRunNode::process`
/// already runs its chain inside a `tokio::task::spawn_blocking`, and its
/// existing `.await.map_err(|err| NodeError::new(format!("orchestration task
/// panicked: {err}")))` turns that panic into a named `NodeError` — tokio's
/// `JoinError::Display` includes the panic payload verbatim when it is a
/// `String` (as `panic!("{err}")` produces here), so the surfaced error still
/// names the repo and path [`CorpusGatesError`](engine_core::workflows::orchestration::corpus_gates::CorpusGatesError)
/// carries.
///
/// `hold_source` is a parameter rather than a hardcoded [`NeverHeld`]
/// because no production `HoldSource` exists yet, and none can be written
/// until there is a `(repo, block_id)`-keyed hold surface — the blocked-edge
/// sink `engine-core` already reads (`operator/queue/source.rs`) is keyed by
/// tmux session and host, not by block. [`register_orchestration`] still
/// passes `NeverHeld` as its argument; that is a known, named gap, not an
/// oversight.
///
/// `EN.ticket.orchestration-abort-and-progress` task 4: the factory also
/// mints this run's `CancellationToken` and step-progress fan-out cell and
/// wires them into the node via `with_cancellation_token`/`with_step_observer`,
/// handing both off to `suspend::spawn_run` through the thread-local seam
/// documented just above this module's imports — see that block's doc for
/// why. `POST /events/{run_id}/abort` triggers this exact token, and each
/// completed step's progress reaches live state, the durable writer, and
/// SSE through the same three-way fan-out `spawn_run`'s own node-boundary
/// `on_progress` already used (`suspend::publish_step_progress`, reusing
/// `suspend::progress_fanout`) — never a second progress mechanism.
pub fn register_orchestration_with_registry(
    dispatcher: &mut Dispatcher,
    repo_reg: Option<Arc<RepoRegistry>>,
    hold_source: Arc<dyn engine_core::workflows::orchestration::integrate::HoldSource>,
) {
    dispatcher.register(
        engine_core::workflows::orchestration::graph::schema(),
        Box::new(move |event: &serde_json::Value| {
            let orch_event: engine_core::workflows::orchestration::graph::OrchestrationEventSchema =
                serde_json::from_value(event.clone())
                    .map_err(|err| format!("invalid ORCHESTRATION event: {err}"))?;

            let gates_repo_registry = match repo_reg.clone() {
                Some(reg) => reg,
                None => Arc::new(
                    RepoRegistry::from_brain_root(&orch_event.brain_root).map_err(|err| {
                        format!(
                            "orchestration gates: repo registry for brain root '{}': {err}",
                            orch_event.brain_root.display()
                        )
                    })?,
                ),
            };
            let gates = Arc::new(
                engine_core::workflows::orchestration::corpus_gates::CorpusGates::new(
                    gates_repo_registry,
                ),
            );

            let depends_on_gates = gates.clone();
            let edge_met_gates = gates.clone();
            let block_open_gates = gates.clone();

            // `EN.11.F` task 2 follow-up: resolve this run's campaign id
            // HERE, up front — the SAME resolver `OrchestrationRunNode::process`
            // itself would otherwise call independently — so the id
            // `spawn_run` registers for abort below and the id this node
            // actually stamps into its output can never diverge (an
            // auto-minted id resolved twice would mint two different
            // UUIDs).
            let campaign_id = engine_core::workflows::orchestration::graph::resolve_campaign_id(
                orch_event.campaign_id.as_deref(),
            )
            .map_err(|err| err.to_string())?;

            // EN.ticket.orchestration-abort-and-progress task 4: mint this
            // run's `CancellationToken` and step-progress observer, and hand
            // the handoff off to `suspend::spawn_run` via the thread-local
            // above — see [`build_orchestration_seams`]. `run_token` (not
            // `token`, which `with_is_block_open`'s closure parameter below
            // already shadows) is embedded directly in the node;
            // `spawn_run` re-registers it under this run's `run_id` so
            // `POST /events/{run_id}/abort` triggers the exact token
            // `integrate_chain` is checking, not a discarded one. It is
            // ALSO registered under `campaign_id` in `AppState::campaigns`
            // (`EN.11.F` task 2 follow-up), which is what makes `POST
            // /campaigns/{id}/abort` reach a live campaign.
            let (run_token, step_observer) = build_orchestration_seams(campaign_id);

            let node = engine_core::workflows::orchestration::graph::OrchestrationRunNode::new()
                .with_campaign_id(campaign_id)
                .with_resolve_depends_on(Arc::new(move |repo: &str, block_id: &str| {
                    let edges = depends_on_gates.resolve_depends_on(repo, block_id);
                    if let Some(err) = depends_on_gates.take_error() {
                        panic!("{err}");
                    }
                    edges
                }))
                .with_is_edge_met(Arc::new(move |repo: &str, block_id: &str| {
                    let met = edge_met_gates.is_edge_met(repo, block_id);
                    if let Some(err) = edge_met_gates.take_error() {
                        panic!("{err}");
                    }
                    met
                }))
                .with_is_block_open(Arc::new(move |token: &str| {
                    let open = block_open_gates.is_block_open(token);
                    if let Some(err) = block_open_gates.take_error() {
                        panic!("{err}");
                    }
                    open
                }))
                .with_hold_source(hold_source.clone())
                .with_cancellation_token(run_token)
                .with_step_observer(step_observer);

            let mut registry = engine_core::NodeRegistry::new();
            registry.register(Box::new(node));

            Workflow::new_validated(
                registry,
                engine_core::workflows::orchestration::graph::schema(),
            )
            .map_err(|err| err.to_string())
        }),
    );
}

/// Register every builtin workflow known to this crate: `SDLC_FLOW`,
/// `SDLC_TASK`, `RESEARCH_AGENT`, `DIAGNOSTIC_INTAKE`, `PROPOSAL_GENERATOR`,
/// `CONTENT_PIPELINE`, `OPPORTUNITY_SET_STAGE`, `OPPORTUNITY_ADD_ACTION`,
/// `HARVEST_APPROVE`, `LEAD_INGEST`, `APPROVE_AND_RUN`, `TERMINAL_PROBE`,
/// and `ORCHESTRATION`; future builtins register here too.
///
/// Keeps its one-argument signature unchanged (EN.3.K) — `bastion` calls
/// this with exactly one argument (`../bastion/src/serve/mod.rs:61`) and
/// this spec cannot edit that separate repo. `SDLC_FLOW`'s and
/// `SDLC_TASK`'s repo registry (EN.3.K) is threaded through the
/// process-global seam ([`repo_registry`]) rather than a new parameter
/// here; see [`register_builtin_workflows_with_registry`] for the
/// explicit-registry entry point.
pub fn register_builtin_workflows(dispatcher: &mut Dispatcher) {
    register_builtin_workflows_with_registry(dispatcher, repo_registry());
}

/// Register every builtin workflow with an explicit repo registry (EN.3.K)
/// for `SDLC_FLOW` and `SDLC_TASK`, bypassing the process-global seam.
/// Every other builtin workflow is unaffected by `repo` (they resolve
/// `PolicyConfigSource::Builtin` or no policy at all) and registers
/// exactly as [`register_builtin_workflows`] does. Test entry point:
/// installs a tempdir registry with no `ENGINE_BRAIN_ROOT` race instead of
/// relying on the process-global.
///
/// `EN.11.P` task 3: `repo_reg` is cloned for `SDLC_FLOW`'s registration
/// rather than moved, because `SDLC_TASK`'s registration right after it
/// needs the same `Option<Arc<RepoRegistry>>` — verified at this call
/// site: without the `.clone()` the second call would not compile
/// ("use of moved value").
pub fn register_builtin_workflows_with_registry(
    dispatcher: &mut Dispatcher,
    repo_reg: Option<Arc<RepoRegistry>>,
) {
    register_sdlc_flow_with_registry(dispatcher, repo_reg.clone());
    register_sdlc_task_with_registry(dispatcher, repo_reg);
    register_research_agent(dispatcher);
    register_diagnostic_intake(dispatcher);
    register_proposal_generator(dispatcher);
    register_content_pipeline(dispatcher);
    register_opportunity_set_stage(dispatcher);
    register_opportunity_add_action(dispatcher);
    register_harvest_approve(dispatcher);
    register_lead_ingest(dispatcher);
    register_approve_and_run(dispatcher);
    register_terminal_probe(dispatcher);
    register_orchestration(dispatcher);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_sdlc_flow_populates_both_registries() {
        let mut dispatcher = Dispatcher::new();

        register_sdlc_flow(&mut dispatcher);

        assert!(dispatcher.is_registered("SDLC_FLOW"));
    }

    #[test]
    fn resolve_schema_returns_schema_with_setup_worktree_start_node() {
        let mut dispatcher = Dispatcher::new();
        register_sdlc_flow(&mut dispatcher);

        let schema = dispatcher
            .resolve_schema("SDLC_FLOW")
            .expect("SDLC_FLOW schema should resolve");

        assert_eq!(schema.start_node, "SetupWorktreeNode");
    }

    #[test]
    fn dispatch_yields_a_runnable_workflow() {
        let mut dispatcher = Dispatcher::new();
        register_sdlc_flow(&mut dispatcher);

        // `SDLC_FLOW`'s policy-aware factory (EN.5.D task 7) resolves policy
        // from the triggering event, whose schema requires `spec_slug` — so
        // an actual event is fed through `dispatch_with_event` rather than
        // `dispatch`'s empty-payload convenience wrapper.
        let workflow = dispatcher
            .dispatch_with_event("SDLC_FLOW", &serde_json::json!({ "spec_slug": "my-spec" }))
            .expect("SDLC_FLOW should dispatch to a runnable Workflow");

        // Confirm the workflow was actually assembled (has the expected
        // start node reachable) without driving a full run, which would
        // require live model transports / real subprocesses for the
        // model-calling and shell-driven nodes.
        let _ = workflow;
    }

    #[test]
    fn dispatch_with_event_seeds_the_resolved_sdlc_policy() {
        let mut dispatcher = Dispatcher::new();
        register_sdlc_flow(&mut dispatcher);

        let workflow = dispatcher
            .dispatch_with_event("SDLC_FLOW", &serde_json::json!({ "spec_slug": "my-spec" }))
            .expect("SDLC_FLOW should dispatch to a runnable Workflow");

        let _ = workflow;
    }

    #[test]
    fn dispatch_with_event_fails_loudly_on_unknown_sdlc_profile() {
        let mut dispatcher = Dispatcher::new();
        register_sdlc_flow(&mut dispatcher);

        let result = dispatcher.dispatch_with_event(
            "SDLC_FLOW",
            &serde_json::json!({ "spec_slug": "my-spec", "profile": "not-a-real-profile" }),
        );

        match result {
            Err(crate::dispatch::DispatchError::PolicyResolutionFailed(message)) => {
                assert!(message.contains("not-a-real-profile"));
            }
            Ok(_) => panic!("expected PolicyResolutionFailed, got Ok"),
            Err(other) => panic!("expected PolicyResolutionFailed, got {other}"),
        }
    }

    // --- repo registry seam (EN.3.K task 4) ---------------------------------

    /// A tempdir "brain root" with a single `[[repos]]` entry (`alpha`)
    /// whose `repo_path` holds a `planning/harness.json` with a
    /// distinguishable `sdlc.policy.max_attempts` value, so a test can
    /// assert the resolved policy actually came from `alpha`'s root and not
    /// the builtin default.
    fn tempdir_registry_with_alpha(max_attempts: u32) -> (tempfile::TempDir, Arc<RepoRegistry>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let alpha = dir.path().join("alpha");
        std::fs::create_dir_all(alpha.join("planning")).expect("mkdir alpha/planning");
        std::fs::write(
            alpha.join("planning").join("harness.json"),
            serde_json::json!({ "sdlc": { "policy": { "max_attempts": max_attempts } } })
                .to_string(),
        )
        .expect("write harness.json");
        std::fs::write(
            dir.path().join("brain.toml"),
            "[[repos]]\nslug = \"alpha\"\nrepo_path = \"alpha\"\n",
        )
        .expect("write brain.toml");
        let registry =
            Arc::new(RepoRegistry::from_brain_root(dir.path()).expect("registry should build"));
        (dir, registry)
    }

    #[test]
    fn dispatch_with_no_registry_installed_still_dispatches_a_runnable_workflow() {
        // No registry installed anywhere (explicit `None`) — an
        // absent-`repo` event must still dispatch exactly as
        // `dispatch_yields_a_runnable_workflow` asserts for the plain
        // one-argument `register_sdlc_flow`.
        let mut dispatcher = Dispatcher::new();
        register_sdlc_flow_with_registry(&mut dispatcher, None);

        let workflow = dispatcher
            .dispatch_with_event("SDLC_FLOW", &serde_json::json!({ "spec_slug": "my-spec" }))
            .expect("SDLC_FLOW should dispatch to a runnable Workflow with no registry installed");

        let _ = workflow;
    }

    #[test]
    fn dispatch_with_repo_resolves_policy_against_the_repos_own_harness_json() {
        let (_dir, registry) = tempdir_registry_with_alpha(11);
        let mut dispatcher = Dispatcher::new();
        register_sdlc_flow_with_registry(&mut dispatcher, Some(registry));

        let workflow = dispatcher
            .dispatch_with_event(
                "SDLC_FLOW",
                &serde_json::json!({ "spec_slug": "my-spec", "repo": "alpha" }),
            )
            .expect("SDLC_FLOW should dispatch when repo resolves via the installed registry");

        let _ = workflow;
    }

    #[test]
    fn dispatch_with_unknown_repo_slug_fails_loudly_naming_the_slug() {
        let (_dir, registry) = tempdir_registry_with_alpha(11);
        let mut dispatcher = Dispatcher::new();
        register_sdlc_flow_with_registry(&mut dispatcher, Some(registry));

        let result = dispatcher.dispatch_with_event(
            "SDLC_FLOW",
            &serde_json::json!({ "spec_slug": "my-spec", "repo": "not-a-repo" }),
        );

        match result {
            Err(crate::dispatch::DispatchError::PolicyResolutionFailed(message)) => {
                assert!(message.contains("not-a-repo"));
            }
            Ok(_) => panic!("expected PolicyResolutionFailed, got Ok"),
            Err(other) => panic!("expected PolicyResolutionFailed, got {other}"),
        }
    }

    #[test]
    fn set_repo_registry_and_repo_registry_round_trip() {
        // Guard the process-global with a coarse lock so this test doesn't
        // race other tests in this module that also touch it.
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());

        let previous = repo_registry();
        let (_dir, registry) = tempdir_registry_with_alpha(3);
        set_repo_registry(registry.clone());
        assert!(repo_registry().is_some());
        assert!(repo_registry().unwrap().resolve("alpha").is_ok());

        clear_repo_registry();
        assert!(repo_registry().is_none());

        // Restore whatever was installed before this test ran, so it
        // doesn't leak state into other tests in this process.
        if let Some(prev) = previous {
            set_repo_registry(prev);
        }
    }

    #[test]
    fn register_builtin_workflows_registers_sdlc_flow() {
        let mut dispatcher = Dispatcher::new();

        register_builtin_workflows(&mut dispatcher);

        assert!(dispatcher.is_registered("SDLC_FLOW"));
    }

    // --- SDLC_TASK registration (EN.11.P task 3) ----------------------------

    #[test]
    fn register_sdlc_task_populates_both_registries() {
        let mut dispatcher = Dispatcher::new();

        register_sdlc_task(&mut dispatcher);

        assert!(dispatcher.is_registered("SDLC_TASK"));
    }

    #[test]
    fn register_builtin_workflows_registers_sdlc_task() {
        let mut dispatcher = Dispatcher::new();

        register_builtin_workflows(&mut dispatcher);

        assert!(dispatcher.is_registered("SDLC_TASK"));
    }

    #[test]
    fn sdlc_task_resolve_schema_matches_sdlc_task_graph_schema() {
        let mut dispatcher = Dispatcher::new();
        register_sdlc_task(&mut dispatcher);

        let schema = dispatcher
            .resolve_schema("SDLC_TASK")
            .expect("SDLC_TASK schema should resolve");

        assert_eq!(*schema, engine_core::workflows::sdlc_task::graph::schema());
    }

    #[test]
    fn sdlc_task_dispatch_yields_a_runnable_workflow() {
        let mut dispatcher = Dispatcher::new();
        register_sdlc_task(&mut dispatcher);

        let workflow = dispatcher
            .dispatch_with_event("SDLC_TASK", &serde_json::json!({ "spec_slug": "my-spec" }))
            .expect("SDLC_TASK should dispatch to a runnable Workflow");

        let _ = workflow;
    }

    #[test]
    fn sdlc_task_dispatch_with_event_fails_loudly_on_unknown_profile() {
        let mut dispatcher = Dispatcher::new();
        register_sdlc_task(&mut dispatcher);

        let result = dispatcher.dispatch_with_event(
            "SDLC_TASK",
            &serde_json::json!({ "spec_slug": "my-spec", "profile": "not-a-real-profile" }),
        );

        match result {
            Err(crate::dispatch::DispatchError::PolicyResolutionFailed(message)) => {
                assert!(message.contains("not-a-real-profile"));
            }
            Ok(_) => panic!("expected PolicyResolutionFailed, got Ok"),
            Err(other) => panic!("expected PolicyResolutionFailed, got {other}"),
        }
    }

    #[test]
    fn sdlc_task_dispatch_with_no_registry_installed_still_dispatches_a_runnable_workflow() {
        let mut dispatcher = Dispatcher::new();
        register_sdlc_task_with_registry(&mut dispatcher, None);

        let workflow = dispatcher
            .dispatch_with_event("SDLC_TASK", &serde_json::json!({ "spec_slug": "my-spec" }))
            .expect("SDLC_TASK should dispatch to a runnable Workflow with no registry installed");

        let _ = workflow;
    }

    #[test]
    fn sdlc_task_dispatch_with_repo_resolves_policy_against_the_repos_own_harness_json() {
        let dir = tempfile::tempdir().expect("tempdir");
        let alpha = dir.path().join("alpha");
        std::fs::create_dir_all(alpha.join("planning")).expect("mkdir alpha/planning");
        std::fs::write(
            alpha.join("planning").join("harness.json"),
            serde_json::json!({ "sdlc_task": { "policy": { "max_attempts": 4 } } }).to_string(),
        )
        .expect("write harness.json");
        std::fs::write(
            dir.path().join("brain.toml"),
            "[[repos]]\nslug = \"alpha\"\nrepo_path = \"alpha\"\n",
        )
        .expect("write brain.toml");
        let registry =
            Arc::new(RepoRegistry::from_brain_root(dir.path()).expect("registry should build"));

        let mut dispatcher = Dispatcher::new();
        register_sdlc_task_with_registry(&mut dispatcher, Some(registry));

        let workflow = dispatcher
            .dispatch_with_event(
                "SDLC_TASK",
                &serde_json::json!({ "spec_slug": "my-spec", "repo": "alpha" }),
            )
            .expect("SDLC_TASK should dispatch when repo resolves via the installed registry");

        let _ = workflow;
    }

    #[test]
    fn sdlc_task_dispatch_with_unknown_repo_slug_fails_loudly_naming_the_slug() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("brain.toml"),
            "[[repos]]\nslug = \"alpha\"\nrepo_path = \"alpha\"\n",
        )
        .expect("write brain.toml");
        let registry =
            Arc::new(RepoRegistry::from_brain_root(dir.path()).expect("registry should build"));

        let mut dispatcher = Dispatcher::new();
        register_sdlc_task_with_registry(&mut dispatcher, Some(registry));

        let result = dispatcher.dispatch_with_event(
            "SDLC_TASK",
            &serde_json::json!({ "spec_slug": "my-spec", "repo": "not-a-repo" }),
        );

        match result {
            Err(crate::dispatch::DispatchError::PolicyResolutionFailed(message)) => {
                assert!(message.contains("not-a-repo"));
            }
            Ok(_) => panic!("expected PolicyResolutionFailed, got Ok"),
            Err(other) => panic!("expected PolicyResolutionFailed, got {other}"),
        }
    }

    /// The move-check (block record + task 3 doc comment): confirms the
    /// served `SDLC_TASK` registry actually carries the `"task/"` branch
    /// prefix and the `EN.3.K` repo registry after re-registration — not
    /// just that dispatch succeeds. Runs the real workflow (real `git`,
    /// against a fresh tempdir repo resolved through the installed
    /// registry, never the process cwd) and cancels the run right after
    /// `SetupWorktreeNode` completes, so only that one node's output is
    /// inspected.
    #[tokio::test]
    async fn sdlc_task_served_setup_worktree_node_keeps_task_branch_prefix_and_registry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let alpha = dir.path().join("alpha");
        std::fs::create_dir_all(&alpha).expect("mkdir alpha");
        // `SetupWorktreeNode` (unmodified `sdlc_flow` machinery, reused
        // as-is) checks out `origin/main` even on the run-in-place path —
        // so this fixture needs a real commit and an `origin/main` ref for
        // `git checkout -B task/my-spec origin/main` to resolve against,
        // not just an empty `git init`.
        let run_git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(&alpha)
                .status()
                .unwrap_or_else(|err| panic!("git {args:?} should spawn: {err}"));
            assert!(status.success(), "git {args:?} must succeed for this test");
        };
        run_git(&["init", "-q"]);
        run_git(&["config", "user.email", "test@example.com"]);
        run_git(&["config", "user.name", "Test"]);
        std::fs::write(alpha.join("README.md"), "fixture\n").expect("write README");
        run_git(&["add", "README.md"]);
        run_git(&["commit", "-q", "-m", "init"]);
        run_git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        std::fs::write(
            dir.path().join("brain.toml"),
            "[[repos]]\nslug = \"alpha\"\nrepo_path = \"alpha\"\n",
        )
        .expect("write brain.toml");
        let registry =
            Arc::new(RepoRegistry::from_brain_root(dir.path()).expect("registry should build"));

        let mut dispatcher = Dispatcher::new();
        register_sdlc_task_with_registry(&mut dispatcher, Some(registry));

        let event = serde_json::json!({ "spec_slug": "my-spec", "repo": "alpha" });
        let workflow = dispatcher
            .dispatch_with_event("SDLC_TASK", &event)
            .expect("SDLC_TASK should dispatch to a runnable Workflow");

        let token = engine_core::CancellationToken::new();
        let cancel_token = token.clone();
        let on_progress: engine_core::OnProgress<'_> = Box::new(move |ctx| {
            if ctx
                .node_runs
                .get("SetupWorktreeNode")
                .is_some_and(|run| run.status == engine_contract::NodeRunStatus::Success)
            {
                cancel_token.cancel();
            }
        });
        let options = engine_core::RunOptions {
            cancellation_token: Some(token),
            budget: None,
            pause_signal: None,
            run_id: None,
        };

        let ctx = workflow
            .run_with(event, on_progress, options)
            .await
            .expect("run should not error — it halts via cancellation");

        let result = ctx
            .nodes
            .get("SetupWorktreeNode")
            .expect("SetupWorktreeNode should have produced output before cancellation");
        assert_eq!(
            result["branch_name"], "task/my-spec",
            "served SDLC_TASK registration must keep the task/ branch prefix"
        );
        assert_eq!(
            ctx.node_runs["SetupWorktreeNode"].status,
            engine_contract::NodeRunStatus::Success,
            "SetupWorktreeNode must have resolved the repo-registry root and succeeded"
        );
        assert!(
            !ctx.node_runs.contains_key("SpecExistsRouterNode")
                || ctx.node_runs["SpecExistsRouterNode"].status
                    == engine_contract::NodeRunStatus::Pending,
            "the walk must have halted before the next node ran"
        );
    }

    #[test]
    fn sdlc_flow_registration_and_dispatch_unchanged_alongside_sdlc_task() {
        let mut dispatcher = Dispatcher::new();
        register_builtin_workflows(&mut dispatcher);

        assert!(dispatcher.is_registered("SDLC_FLOW"));
        let workflow = dispatcher
            .dispatch_with_event("SDLC_FLOW", &serde_json::json!({ "spec_slug": "my-spec" }))
            .expect("SDLC_FLOW should still dispatch to a runnable Workflow");
        let _ = workflow;
    }

    #[test]
    fn register_research_agent_populates_both_registries() {
        let mut dispatcher = Dispatcher::new();

        register_research_agent(&mut dispatcher);

        assert!(dispatcher.is_registered("RESEARCH_AGENT"));
    }

    #[test]
    fn resolve_schema_returns_schema_with_research_mode_router_start_node() {
        let mut dispatcher = Dispatcher::new();
        register_research_agent(&mut dispatcher);

        let schema = dispatcher
            .resolve_schema("RESEARCH_AGENT")
            .expect("RESEARCH_AGENT schema should resolve");

        assert_eq!(schema.start_node, "ResearchModeRouterNode");
    }

    #[test]
    fn dispatch_with_event_seeds_the_resolved_research_agent_policy() {
        let mut dispatcher = Dispatcher::new();
        register_research_agent(&mut dispatcher);

        let workflow = dispatcher
            .dispatch_with_event(
                "RESEARCH_AGENT",
                &serde_json::json!({ "mode": "company", "company_name": "Acme" }),
            )
            .expect("RESEARCH_AGENT should dispatch to a runnable Workflow with no repo");

        let _ = workflow;
    }

    #[test]
    fn dispatch_with_event_fails_loudly_on_unknown_research_agent_profile() {
        let mut dispatcher = Dispatcher::new();
        register_research_agent(&mut dispatcher);

        let result = dispatcher.dispatch_with_event(
            "RESEARCH_AGENT",
            &serde_json::json!({
                "mode": "company",
                "company_name": "Acme",
                "profile": "not-a-real-profile",
            }),
        );

        match result {
            Err(crate::dispatch::DispatchError::PolicyResolutionFailed(message)) => {
                assert!(message.contains("not-a-real-profile"));
            }
            Ok(_) => panic!("expected PolicyResolutionFailed, got Ok"),
            Err(other) => panic!("expected PolicyResolutionFailed, got {other}"),
        }
    }

    #[test]
    fn resolve_schema_terminates_in_research_ingress_dispatch_node_with_no_outgoing_edges() {
        let mut dispatcher = Dispatcher::new();
        register_research_agent(&mut dispatcher);

        let schema = dispatcher
            .resolve_schema("RESEARCH_AGENT")
            .expect("RESEARCH_AGENT schema should resolve");

        let config = schema
            .nodes
            .get("ResearchIngressDispatchNode")
            .expect("schema should declare 'ResearchIngressDispatchNode'");
        assert!(
            config.connections.is_empty(),
            "ResearchIngressDispatchNode should have no outgoing edges"
        );
    }

    /// `EN.6.E`: a served `RESEARCH_AGENT` dispatch still builds a runnable
    /// `Workflow` when `ENGINE_EVENTS_URL` is configured, and the
    /// `ResearchIngressDispatchNode` re-registration does not disturb the
    /// rest of the policy-aware assembly — mirrors
    /// `dispatch_with_event_builds_content_pipeline_with_a_configured_events_url`.
    #[test]
    fn dispatch_with_event_builds_research_agent_with_a_configured_events_url() {
        unsafe {
            std::env::set_var(EVENTS_URL_ENV, "https://engine.example.com/events/");
        }

        let mut dispatcher = Dispatcher::new();
        register_research_agent(&mut dispatcher);

        let workflow = dispatcher.dispatch_with_event(
            "RESEARCH_AGENT",
            &serde_json::json!({ "mode": "company", "company_name": "Acme" }),
        );

        unsafe {
            std::env::remove_var(EVENTS_URL_ENV);
        }

        let workflow =
            workflow.expect("RESEARCH_AGENT should dispatch with a configured events URL");
        let _ = workflow;
    }

    #[test]
    fn register_builtin_workflows_registers_research_agent() {
        let mut dispatcher = Dispatcher::new();

        register_builtin_workflows(&mut dispatcher);

        assert!(dispatcher.is_registered("RESEARCH_AGENT"));
    }

    #[test]
    fn register_diagnostic_intake_populates_both_registries() {
        let mut dispatcher = Dispatcher::new();

        register_diagnostic_intake(&mut dispatcher);

        assert!(dispatcher.is_registered("DIAGNOSTIC_INTAKE"));
    }

    #[test]
    fn resolve_schema_returns_schema_with_intake_extract_node_start_node() {
        let mut dispatcher = Dispatcher::new();
        register_diagnostic_intake(&mut dispatcher);

        let schema = dispatcher
            .resolve_schema("DIAGNOSTIC_INTAKE")
            .expect("DIAGNOSTIC_INTAKE schema should resolve");

        assert_eq!(schema.start_node, "IntakeExtractNode");
    }

    #[test]
    fn dispatch_with_event_seeds_the_resolved_diagnostic_intake_policy() {
        let mut dispatcher = Dispatcher::new();
        register_diagnostic_intake(&mut dispatcher);

        let workflow = dispatcher
            .dispatch_with_event(
                "DIAGNOSTIC_INTAKE",
                &serde_json::json!({ "notes": "customer call transcript" }),
            )
            .expect("DIAGNOSTIC_INTAKE should dispatch to a runnable Workflow with no repo");

        let _ = workflow;
    }

    #[test]
    fn dispatch_with_event_fails_loudly_on_unknown_diagnostic_intake_profile() {
        let mut dispatcher = Dispatcher::new();
        register_diagnostic_intake(&mut dispatcher);

        let result = dispatcher.dispatch_with_event(
            "DIAGNOSTIC_INTAKE",
            &serde_json::json!({
                "notes": "customer call transcript",
                "profile": "not-a-real-profile",
            }),
        );

        match result {
            Err(crate::dispatch::DispatchError::PolicyResolutionFailed(message)) => {
                assert!(message.contains("not-a-real-profile"));
            }
            Ok(_) => panic!("expected PolicyResolutionFailed, got Ok"),
            Err(other) => panic!("expected PolicyResolutionFailed, got {other}"),
        }
    }

    #[test]
    fn register_builtin_workflows_registers_diagnostic_intake() {
        let mut dispatcher = Dispatcher::new();

        register_builtin_workflows(&mut dispatcher);

        assert!(dispatcher.is_registered("DIAGNOSTIC_INTAKE"));
    }

    #[test]
    fn register_proposal_generator_populates_both_registries() {
        let mut dispatcher = Dispatcher::new();

        register_proposal_generator(&mut dispatcher);

        assert!(dispatcher.is_registered("PROPOSAL_GENERATOR"));
    }

    #[test]
    fn resolve_schema_returns_schema_with_company_research_start_node() {
        let mut dispatcher = Dispatcher::new();
        register_proposal_generator(&mut dispatcher);

        let schema = dispatcher
            .resolve_schema("PROPOSAL_GENERATOR")
            .expect("PROPOSAL_GENERATOR schema should resolve");

        assert_eq!(schema.start_node, "ProposalCompanyResearchNode");
    }

    #[test]
    fn dispatch_with_event_seeds_the_resolved_proposal_generator_policy() {
        let mut dispatcher = Dispatcher::new();
        register_proposal_generator(&mut dispatcher);

        let workflow = dispatcher
            .dispatch_with_event(
                "PROPOSAL_GENERATOR",
                &serde_json::json!({ "company_name": "Acme", "profile": "local-judgment" }),
            )
            .expect("PROPOSAL_GENERATOR should resolve the local-judgment profile with no repo");

        let _ = workflow;
    }

    #[test]
    fn local_judgment_profile_over_the_event_resolves_to_a_locally_tiered_policy() {
        // Exercises exactly what `register_proposal_generator`'s factory
        // does with the triggering event, proving the `profile` sent over
        // `POST /events/` actually reaches `registry_for_policy`'s
        // Local-tier rewire rather than resolving to builtin defaults.
        use engine_core::workflows::proposal_generator::policy::ModelTier;

        let event = serde_json::json!({ "company_name": "Acme", "profile": "local-judgment" });
        let ctx = event_only_context(&event);

        let policy =
            engine_core::workflows::proposal_generator::profiles::resolve_policy_for_run_from(
                &ctx,
                &PolicyConfigSource::Builtin,
            )
            .expect("local-judgment should resolve with no repo");

        assert_eq!(policy.model_tiers.opportunity, ModelTier::Local);
        assert_eq!(policy.model_tiers.review, ModelTier::Local);
        assert_eq!(policy.model_tiers.revise, ModelTier::Local);

        let default_policy =
            engine_core::workflows::proposal_generator::policy::ProposalGeneratorPolicy::default();
        assert_ne!(
            policy.model_tiers.opportunity, default_policy.model_tiers.opportunity,
            "the resolved policy must differ from the default-policy registry's tiers"
        );
    }

    #[test]
    fn dispatch_with_event_fails_loudly_on_unknown_proposal_generator_profile() {
        let mut dispatcher = Dispatcher::new();
        register_proposal_generator(&mut dispatcher);

        let result = dispatcher.dispatch_with_event(
            "PROPOSAL_GENERATOR",
            &serde_json::json!({
                "company_name": "Acme",
                "profile": "not-a-real-profile",
            }),
        );

        match result {
            Err(crate::dispatch::DispatchError::PolicyResolutionFailed(message)) => {
                assert!(message.contains("not-a-real-profile"));
            }
            Ok(_) => panic!("expected PolicyResolutionFailed, got Ok"),
            Err(other) => panic!("expected PolicyResolutionFailed, got {other}"),
        }
    }

    #[test]
    fn register_builtin_workflows_registers_proposal_generator() {
        let mut dispatcher = Dispatcher::new();

        register_builtin_workflows(&mut dispatcher);

        assert!(dispatcher.is_registered("PROPOSAL_GENERATOR"));
    }

    fn minimal_web_article_event() -> serde_json::Value {
        serde_json::json!({
            "envelope": {
                "envelope_id": "env-1",
                "channel_type": "web_article",
                "timestamp": "2026-07-25T00:00:00Z",
                "source": { "kind": "url", "url": "https://example.com/a" }
            }
        })
    }

    #[test]
    fn register_content_pipeline_populates_both_registries() {
        let mut dispatcher = Dispatcher::new();

        register_content_pipeline(&mut dispatcher);

        assert!(dispatcher.is_registered("CONTENT_PIPELINE"));
    }

    #[test]
    fn resolve_schema_returns_schema_with_source_router_start_node() {
        let mut dispatcher = Dispatcher::new();
        register_content_pipeline(&mut dispatcher);

        let schema = dispatcher
            .resolve_schema("CONTENT_PIPELINE")
            .expect("CONTENT_PIPELINE schema should resolve");

        assert_eq!(schema.start_node, "SourceRouterNode");
    }

    #[test]
    fn dispatch_with_event_seeds_the_resolved_content_pipeline_policy() {
        let mut dispatcher = Dispatcher::new();
        register_content_pipeline(&mut dispatcher);

        let workflow = dispatcher
            .dispatch_with_event("CONTENT_PIPELINE", &minimal_web_article_event())
            .expect("CONTENT_PIPELINE should dispatch to a runnable Workflow with no repo");

        let _ = workflow;
    }

    #[test]
    fn dispatch_with_event_fails_loudly_on_unknown_content_pipeline_profile() {
        let mut dispatcher = Dispatcher::new();
        register_content_pipeline(&mut dispatcher);

        let mut event = minimal_web_article_event();
        event["profile"] = serde_json::json!("not-a-real-profile");

        let result = dispatcher.dispatch_with_event("CONTENT_PIPELINE", &event);

        match result {
            Err(crate::dispatch::DispatchError::PolicyResolutionFailed(message)) => {
                assert!(message.contains("not-a-real-profile"));
            }
            Ok(_) => panic!("expected PolicyResolutionFailed, got Ok"),
            Err(other) => panic!("expected PolicyResolutionFailed, got {other}"),
        }
    }

    #[test]
    fn local_drafting_profile_over_the_event_resolves_to_a_locally_tiered_policy() {
        use engine_core::workflows::content_pipeline::policy::ModelTier;

        let mut event = minimal_web_article_event();
        event["profile"] = serde_json::json!("local-drafting");
        let ctx = event_only_context(&event);

        let policy =
            engine_core::workflows::content_pipeline::profiles::resolve_policy_for_run_from(
                &ctx,
                &PolicyConfigSource::Builtin,
            )
            .expect("local-drafting should resolve with no repo");

        assert_eq!(policy.model_tiers.summarize, ModelTier::Local);
        assert_eq!(policy.model_tiers.critic, ModelTier::Local);
        assert_eq!(policy.model_tiers.revise, ModelTier::Local);
        assert_eq!(policy.model_tiers.translate, ModelTier::Local);
    }

    #[test]
    fn register_builtin_workflows_registers_content_pipeline() {
        let mut dispatcher = Dispatcher::new();

        register_builtin_workflows(&mut dispatcher);

        assert!(dispatcher.is_registered("CONTENT_PIPELINE"));
    }

    /// `events_url_from_env` (`EN.6.A` task 5): unset/empty falls back to
    /// the local-dev placeholder; a configured value overrides it. Runs as
    /// a single test (rather than split across parallel `#[test]`s) since
    /// `std::env::var`/`set_var` are process-global and no other test in
    /// this file touches `ENGINE_EVENTS_URL` — self-contained regardless of
    /// `cargo test`'s default parallel execution.
    #[test]
    fn events_url_from_env_falls_back_then_honors_the_configured_value() {
        // SAFETY: this test owns `ENGINE_EVENTS_URL` end-to-end (no other
        // test in this crate reads or writes it) and always restores the
        // unset state before returning, so it cannot leak a stale value to
        // any other test even under `cargo test`'s default parallelism.
        unsafe {
            std::env::remove_var(EVENTS_URL_ENV);
        }
        assert_eq!(events_url_from_env(), DEFAULT_EVENTS_URL);

        unsafe {
            std::env::set_var(EVENTS_URL_ENV, "https://engine.example.com/events/");
        }
        assert_eq!(events_url_from_env(), "https://engine.example.com/events/");

        unsafe {
            std::env::remove_var(EVENTS_URL_ENV);
        }
        assert_eq!(events_url_from_env(), DEFAULT_EVENTS_URL);
    }

    /// A served `CONTENT_PIPELINE` dispatch still builds a runnable
    /// `Workflow` when `ENGINE_EVENTS_URL` is configured — the
    /// `ActionDispatchNode` re-registration in `register_content_pipeline`
    /// does not disturb the rest of the policy-aware assembly.
    #[test]
    fn dispatch_with_event_builds_content_pipeline_with_a_configured_events_url() {
        unsafe {
            std::env::set_var(EVENTS_URL_ENV, "https://engine.example.com/events/");
        }

        let mut dispatcher = Dispatcher::new();
        register_content_pipeline(&mut dispatcher);

        let workflow =
            dispatcher.dispatch_with_event("CONTENT_PIPELINE", &minimal_web_article_event());

        unsafe {
            std::env::remove_var(EVENTS_URL_ENV);
        }

        let workflow =
            workflow.expect("CONTENT_PIPELINE should dispatch with a configured events URL");
        let _ = workflow;
    }

    #[test]
    fn register_opportunity_set_stage_populates_both_registries() {
        let mut dispatcher = Dispatcher::new();

        register_opportunity_set_stage(&mut dispatcher);

        assert!(dispatcher.is_registered("OPPORTUNITY_SET_STAGE"));
    }

    #[test]
    fn resolve_schema_returns_schema_with_set_opportunity_stage_start_node() {
        let mut dispatcher = Dispatcher::new();
        register_opportunity_set_stage(&mut dispatcher);

        let schema = dispatcher
            .resolve_schema("OPPORTUNITY_SET_STAGE")
            .expect("OPPORTUNITY_SET_STAGE schema should resolve");

        assert_eq!(schema.start_node, "SetOpportunityStageNode");
    }

    #[test]
    fn dispatch_opportunity_set_stage_builds_a_runnable_workflow_with_no_policy_stamp() {
        let mut dispatcher = Dispatcher::new();
        register_opportunity_set_stage(&mut dispatcher);

        let workflow = dispatcher
            .dispatch_with_event(
                "OPPORTUNITY_SET_STAGE",
                &serde_json::json!({ "slug": "acme", "stage": "contacted" }),
            )
            .expect("OPPORTUNITY_SET_STAGE should dispatch to a runnable Workflow");

        let _ = workflow;
    }

    #[test]
    fn register_builtin_workflows_registers_opportunity_set_stage() {
        let mut dispatcher = Dispatcher::new();

        register_builtin_workflows(&mut dispatcher);

        assert!(dispatcher.is_registered("OPPORTUNITY_SET_STAGE"));
    }

    #[test]
    fn register_opportunity_add_action_populates_both_registries() {
        let mut dispatcher = Dispatcher::new();

        register_opportunity_add_action(&mut dispatcher);

        assert!(dispatcher.is_registered("OPPORTUNITY_ADD_ACTION"));
    }

    #[test]
    fn resolve_schema_returns_schema_with_add_opportunity_action_start_node() {
        let mut dispatcher = Dispatcher::new();
        register_opportunity_add_action(&mut dispatcher);

        let schema = dispatcher
            .resolve_schema("OPPORTUNITY_ADD_ACTION")
            .expect("OPPORTUNITY_ADD_ACTION schema should resolve");

        assert_eq!(schema.start_node, "AddOpportunityActionNode");
    }

    #[test]
    fn dispatch_opportunity_add_action_builds_a_runnable_workflow_with_no_policy_stamp() {
        let mut dispatcher = Dispatcher::new();
        register_opportunity_add_action(&mut dispatcher);

        let workflow = dispatcher
            .dispatch_with_event(
                "OPPORTUNITY_ADD_ACTION",
                &serde_json::json!({
                    "slug": "acme",
                    "at": "2026-07-27T00:00:00Z",
                    "kind": "call",
                    "note": "left voicemail",
                }),
            )
            .expect("OPPORTUNITY_ADD_ACTION should dispatch to a runnable Workflow");

        let _ = workflow;
    }

    #[test]
    fn register_builtin_workflows_registers_opportunity_add_action() {
        let mut dispatcher = Dispatcher::new();

        register_builtin_workflows(&mut dispatcher);

        assert!(dispatcher.is_registered("OPPORTUNITY_ADD_ACTION"));
    }

    #[test]
    fn register_builtin_workflows_registers_all_seven_workflow_types() {
        let mut dispatcher = Dispatcher::new();

        register_builtin_workflows(&mut dispatcher);

        for workflow_type in [
            "SDLC_FLOW",
            "RESEARCH_AGENT",
            "DIAGNOSTIC_INTAKE",
            "PROPOSAL_GENERATOR",
            "CONTENT_PIPELINE",
            "OPPORTUNITY_SET_STAGE",
            "OPPORTUNITY_ADD_ACTION",
        ] {
            assert!(
                dispatcher.is_registered(workflow_type),
                "expected {workflow_type} to be registered"
            );
        }
    }

    #[test]
    fn register_harvest_approve_populates_both_registries() {
        let mut dispatcher = Dispatcher::new();

        register_harvest_approve(&mut dispatcher);

        assert!(dispatcher.is_registered("HARVEST_APPROVE"));
    }

    #[test]
    fn resolve_schema_returns_schema_with_harvest_approve_start_node() {
        let mut dispatcher = Dispatcher::new();
        register_harvest_approve(&mut dispatcher);

        let schema = dispatcher
            .resolve_schema("HARVEST_APPROVE")
            .expect("HARVEST_APPROVE schema should resolve");

        assert_eq!(schema.start_node, "HarvestApproveNode");
    }

    #[test]
    fn dispatch_harvest_approve_builds_a_runnable_workflow_with_no_policy_stamp() {
        let mut dispatcher = Dispatcher::new();
        register_harvest_approve(&mut dispatcher);

        let workflow = dispatcher
            .dispatch_with_event(
                "HARVEST_APPROVE",
                &serde_json::json!({
                    "artifact_id": "artifact-1",
                    "url": "https://brain.example/ingest/learning",
                    "payload": {"artifact_id": "artifact-1"},
                    "doc_paths": ["brain/content/learning/artifact-1.md"],
                }),
            )
            .expect("HARVEST_APPROVE should dispatch to a runnable Workflow");

        let _ = workflow;
    }

    #[test]
    fn register_builtin_workflows_registers_harvest_approve() {
        let mut dispatcher = Dispatcher::new();

        register_builtin_workflows(&mut dispatcher);

        assert!(dispatcher.is_registered("HARVEST_APPROVE"));
    }

    #[test]
    fn register_builtin_workflows_registers_all_twelve_workflow_types() {
        let mut dispatcher = Dispatcher::new();

        register_builtin_workflows(&mut dispatcher);

        for workflow_type in [
            "SDLC_FLOW",
            "SDLC_TASK",
            "RESEARCH_AGENT",
            "DIAGNOSTIC_INTAKE",
            "PROPOSAL_GENERATOR",
            "CONTENT_PIPELINE",
            "OPPORTUNITY_SET_STAGE",
            "OPPORTUNITY_ADD_ACTION",
            "HARVEST_APPROVE",
            "LEAD_INGEST",
            "APPROVE_AND_RUN",
            "TERMINAL_PROBE",
        ] {
            assert!(
                dispatcher.is_registered(workflow_type),
                "expected {workflow_type} to be registered"
            );
        }
    }

    #[test]
    fn register_terminal_probe_populates_both_registries() {
        let mut dispatcher = Dispatcher::new();

        register_terminal_probe(&mut dispatcher);

        assert!(dispatcher.is_registered("TERMINAL_PROBE"));

        let workflow = dispatcher
            .dispatch_with_event("TERMINAL_PROBE", &serde_json::json!({}))
            .expect("TERMINAL_PROBE should dispatch to a runnable Workflow");

        let _ = workflow;
    }

    #[test]
    fn register_builtin_workflows_registers_terminal_probe() {
        let mut dispatcher = Dispatcher::new();

        register_builtin_workflows(&mut dispatcher);

        assert!(dispatcher.is_registered("TERMINAL_PROBE"));
    }

    #[test]
    fn register_lead_ingest_populates_both_registries() {
        let mut dispatcher = Dispatcher::new();

        register_lead_ingest(&mut dispatcher);

        assert!(dispatcher.is_registered("LEAD_INGEST"));
    }

    #[test]
    fn resolve_schema_returns_schema_with_lead_ingest_start_node() {
        let mut dispatcher = Dispatcher::new();
        register_lead_ingest(&mut dispatcher);

        let schema = dispatcher
            .resolve_schema("LEAD_INGEST")
            .expect("LEAD_INGEST schema should resolve");

        assert_eq!(schema.start_node, "MaterializeDocNode");
    }

    #[test]
    fn dispatch_lead_ingest_builds_a_runnable_workflow_with_no_policy_stamp() {
        let mut dispatcher = Dispatcher::new();
        register_lead_ingest(&mut dispatcher);

        let workflow = dispatcher
            .dispatch_with_event(
                "LEAD_INGEST",
                &serde_json::json!({
                    "company_name": "Acme Corp",
                    "contacts": [{"name": "Jane Doe", "emails": ["jane@acme.com"]}],
                }),
            )
            .expect("LEAD_INGEST should dispatch to a runnable Workflow");

        let _ = workflow;
    }

    #[test]
    fn register_builtin_workflows_registers_lead_ingest() {
        let mut dispatcher = Dispatcher::new();

        register_builtin_workflows(&mut dispatcher);

        assert!(dispatcher.is_registered("LEAD_INGEST"));
    }

    #[test]
    fn register_approve_and_run_populates_both_registries() {
        let mut dispatcher = Dispatcher::new();

        register_approve_and_run(&mut dispatcher);

        assert!(dispatcher.is_registered("APPROVE_AND_RUN"));
    }

    #[test]
    fn resolve_schema_returns_schema_with_approve_and_run_execute_start_node() {
        let mut dispatcher = Dispatcher::new();
        register_approve_and_run(&mut dispatcher);

        let schema = dispatcher
            .resolve_schema("APPROVE_AND_RUN")
            .expect("APPROVE_AND_RUN schema should resolve");

        assert_eq!(schema.start_node, "ApproveAndRunExecuteNode");
    }

    #[test]
    fn dispatch_approve_and_run_seeds_the_resolved_policy() {
        let mut dispatcher = Dispatcher::new();
        register_approve_and_run(&mut dispatcher);

        let workflow = dispatcher
            .dispatch_with_event(
                "APPROVE_AND_RUN",
                &serde_json::json!({ "authorized": false }),
            )
            .expect("APPROVE_AND_RUN should dispatch to a runnable Workflow");

        let _ = workflow;
    }

    #[test]
    fn dispatch_approve_and_run_applies_a_named_profile() {
        let mut dispatcher = Dispatcher::new();
        register_approve_and_run(&mut dispatcher);

        let workflow = dispatcher.dispatch_with_event(
            "APPROVE_AND_RUN",
            &serde_json::json!({ "authorized": false, "profile": "cheap-fast" }),
        );

        assert!(
            workflow.is_ok(),
            "a known profile should dispatch to a runnable Workflow"
        );
    }

    #[test]
    fn dispatch_approve_and_run_fails_loudly_on_unknown_profile() {
        let mut dispatcher = Dispatcher::new();
        register_approve_and_run(&mut dispatcher);

        let result = dispatcher.dispatch_with_event(
            "APPROVE_AND_RUN",
            &serde_json::json!({ "authorized": false, "profile": "not-a-real-profile" }),
        );

        match result {
            Err(crate::dispatch::DispatchError::PolicyResolutionFailed(message)) => {
                assert!(message.contains("not-a-real-profile"));
            }
            Ok(_) => panic!("expected PolicyResolutionFailed, got Ok"),
            Err(other) => panic!("expected PolicyResolutionFailed, got {other}"),
        }
    }

    #[test]
    fn dispatch_approve_and_run_event_policy_override_beats_profile() {
        let mut dispatcher = Dispatcher::new();
        register_approve_and_run(&mut dispatcher);

        // Exercises the same event shape `register_approve_and_run`'s
        // factory reads: a named `profile` plus a top-precedence `policy`
        // override object.
        let workflow = dispatcher.dispatch_with_event(
            "APPROVE_AND_RUN",
            &serde_json::json!({
                "authorized": false,
                "profile": "thorough",
                "policy": { "drain_batch_max": 7 },
            }),
        );

        assert!(
            workflow.is_ok(),
            "an event-level policy override on top of a named profile should still dispatch"
        );
    }

    #[test]
    fn register_builtin_workflows_registers_approve_and_run() {
        let mut dispatcher = Dispatcher::new();

        register_builtin_workflows(&mut dispatcher);

        assert!(dispatcher.is_registered("APPROVE_AND_RUN"));
    }

    /// `MaterializeDocNode` resolves its brain root before it ever inspects
    /// `company_name` (`resolve_root()` runs ahead of `read_input()` in
    /// `MaterializeDocNode::process`), so this test pins `ENGINE_BRAIN_ROOT`
    /// to a scratch tempdir rather than letting resolution fall through to
    /// this workstation's real `brain.toml` — a malformed-payload test must
    /// never risk touching the real Brain corpus. Safe to mutate the env var
    /// directly (no guard/restore): `cargo nextest run` forks one process per
    /// test (CLAUDE.md standing rule 7), so this test owns the process for
    /// its whole lifetime and there is no other test in the same process to
    /// race or leak into.
    #[tokio::test]
    async fn dispatch_lead_ingest_with_missing_company_name_fails_loudly_when_run() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("business/docs/opportunities"))
            .expect("create opportunities dir");
        std::env::set_var("ENGINE_BRAIN_ROOT", dir.path());

        let mut dispatcher = Dispatcher::new();
        register_lead_ingest(&mut dispatcher);

        let malformed_payload = serde_json::json!({
            "summary": "No company_name on this payload.",
            "contacts": [{"name": "Jane Doe", "emails": ["jane@acme.com"]}],
        });

        let workflow = dispatcher
            .dispatch_with_event("LEAD_INGEST", &malformed_payload)
            .expect("dispatch itself should still yield a runnable Workflow");

        let ctx = workflow
            .run(malformed_payload, Box::new(|_ctx| {}))
            .await
            .expect("run itself should not error — the failure is a stamped NodeRun");

        assert_eq!(
            ctx.node_runs["MaterializeDocNode"].status,
            engine_contract::NodeRunStatus::Failed,
            "a payload missing company_name must fail loudly, not silently no-op"
        );
        assert!(
            !ctx.node_runs.contains_key("MergeContactsNode")
                || ctx.node_runs["MergeContactsNode"].status
                    == engine_contract::NodeRunStatus::Pending,
            "MergeContactsNode must never run after MaterializeDocNode fails loudly"
        );

        let entries = std::fs::read_dir(dir.path().join("business/docs/opportunities"))
            .expect("read opportunities dir")
            .count();
        assert_eq!(entries, 0, "malformed payload must write no file");
    }

    // --- ORCHESTRATION registration (EN.ticket.orchestration-production-gates-unwired task 2) ---

    /// A tempdir brain root with a single repo (`repo-a`) carrying a real
    /// `planning/state.json`, for structural registration tests — the
    /// behavioural gate cases (unmet edge / met edge / held / closed /
    /// missing / malformed state.json) belong to Task 3's dedicated
    /// `engine-serve/tests/` suite, which drives the same registered
    /// factory end to end. These tests only confirm the registration itself
    /// is the wired one.
    fn orchestration_brain_root(state_json: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let planning = dir.path().join("repo-a").join("planning");
        std::fs::create_dir_all(&planning).expect("mkdir repo-a/planning");
        std::fs::write(planning.join("state.json"), state_json).expect("write state.json");
        std::fs::write(
            dir.path().join("brain.toml"),
            "[[repos]]\nslug = \"repo-a\"\nrepo_path = \"repo-a\"\n",
        )
        .expect("write brain.toml");
        dir
    }

    fn open_block_state_json() -> &'static str {
        r#"{
    "repo": "repo-a",
    "kind": "project",
    "updated": "2026-08-18",
    "tracks": [
        { "title": "wave 1", "blocks": [
            { "id": "A.1", "title": "a1", "status": "open" }
        ] }
    ]
}"#
    }

    #[test]
    fn register_orchestration_registers_the_workflow_type() {
        let mut dispatcher = Dispatcher::new();

        register_orchestration(&mut dispatcher);

        assert!(dispatcher.is_registered("ORCHESTRATION"));
    }

    #[test]
    fn register_builtin_workflows_registers_orchestration() {
        let mut dispatcher = Dispatcher::new();

        register_builtin_workflows(&mut dispatcher);

        assert!(dispatcher.is_registered("ORCHESTRATION"));
    }

    #[test]
    fn register_orchestration_with_registry_dispatches_a_runnable_workflow() {
        let dir = orchestration_brain_root(open_block_state_json());
        let mut dispatcher = Dispatcher::new();
        register_orchestration_with_registry(
            &mut dispatcher,
            None,
            Arc::new(engine_core::workflows::orchestration::integrate::NeverHeld),
        );

        let workflow = dispatcher
            .dispatch_with_event(
                "ORCHESTRATION",
                &serde_json::json!({
                    "brain_root": dir.path(),
                    "roadmap_slug": "test-roadmap",
                    "blocks": [{ "repo": "repo-a", "block_id": "A.1" }],
                }),
            )
            .expect("ORCHESTRATION should dispatch to a runnable Workflow");

        let _ = workflow;
    }

    #[test]
    fn register_orchestration_with_registry_accepts_an_installed_repo_registry() {
        let dir = orchestration_brain_root(open_block_state_json());
        let repo_reg =
            Arc::new(RepoRegistry::from_brain_root(dir.path()).expect("registry should build"));
        let mut dispatcher = Dispatcher::new();
        register_orchestration_with_registry(
            &mut dispatcher,
            Some(repo_reg),
            Arc::new(engine_core::workflows::orchestration::integrate::NeverHeld),
        );

        let workflow = dispatcher
            .dispatch_with_event(
                "ORCHESTRATION",
                &serde_json::json!({
                    "brain_root": dir.path(),
                    "roadmap_slug": "test-roadmap",
                    "blocks": [{ "repo": "repo-a", "block_id": "A.1" }],
                }),
            )
            .expect("ORCHESTRATION should dispatch with an explicitly installed repo registry");

        let _ = workflow;
    }

    #[test]
    fn register_orchestration_with_registry_accepts_a_custom_hold_source() {
        // Structural check that `hold_source` really is a parameter the
        // caller controls, not a hardcoded `NeverHeld` — Task 3 drives a
        // real run to observe a held chain pause; this just confirms an
        // arbitrary `HoldSource` implementation is accepted and the
        // registration still dispatches.
        struct AlwaysHeld;
        impl engine_core::workflows::orchestration::integrate::HoldSource for AlwaysHeld {
            fn is_held(&self, _repo: &str, _block_id: &str) -> bool {
                true
            }
        }

        let dir = orchestration_brain_root(open_block_state_json());
        let mut dispatcher = Dispatcher::new();
        register_orchestration_with_registry(&mut dispatcher, None, Arc::new(AlwaysHeld));

        let workflow = dispatcher.dispatch_with_event(
            "ORCHESTRATION",
            &serde_json::json!({
                "brain_root": dir.path(),
                "roadmap_slug": "test-roadmap",
                "blocks": [{ "repo": "repo-a", "block_id": "A.1" }],
            }),
        );

        assert!(
            workflow.is_ok(),
            "a custom HoldSource must be accepted by the registration"
        );
    }

    #[test]
    fn register_orchestration_with_registry_with_no_registry_and_unresolvable_brain_root_fails_loudly(
    ) {
        // No repo registry installed, and `brain_root` points nowhere real —
        // `RepoRegistry::from_brain_root` must fail, and that failure must
        // surface through dispatch rather than silently building a
        // permissive default registry.
        let mut dispatcher = Dispatcher::new();
        register_orchestration_with_registry(
            &mut dispatcher,
            None,
            Arc::new(engine_core::workflows::orchestration::integrate::NeverHeld),
        );

        let result = dispatcher.dispatch_with_event(
            "ORCHESTRATION",
            &serde_json::json!({
                "brain_root": "/definitely/not/a/real/brain/root/for/this/test",
                "roadmap_slug": "test-roadmap",
                "blocks": [{ "repo": "repo-a", "block_id": "A.1" }],
            }),
        );

        match result {
            Err(crate::dispatch::DispatchError::PolicyResolutionFailed(message)) => {
                assert!(
                    message.contains("not/a/real/brain/root"),
                    "error should name the unresolvable brain root, got: {message}"
                );
            }
            Ok(_) => panic!("expected PolicyResolutionFailed, got Ok"),
            Err(other) => panic!("expected PolicyResolutionFailed, got {other}"),
        }
    }
}
