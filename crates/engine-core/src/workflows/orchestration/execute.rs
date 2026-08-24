//! Block execution — `EN.10.B` Task 3.
//!
//! Runs one [`ChainStep`] by **invoking** the existing `SDLC_FLOW` workflow
//! (`workflows::sdlc_flow::graph::WORKFLOW_TYPE`), never reimplementing it —
//! the Rust `SDLC_FLOW` port already exists and a second copy would drift
//! within a week. The engine to run a block with is selected from the
//! block's own authored `sdlc_workflow` field, supplied here via an
//! injectable closure so this module stays independent of *how* that field
//! is loaded (an `okf_core::TrackBlock`, a JSON block record, or a test
//! double) — the same seam shape [`super::chain`] and [`super::gates`]
//! already use for "is this open" / "is this met".
//!
//! # `EngineKind` — closed today, on purpose
//!
//! `EN.10.C` is the block that makes the sanctioned-engine seam
//! *structurally* closed (its own `engine_kind.rs`, plus the test that
//! proves the escape is unreachable). This module does not wait for that:
//! [`EngineKind`] is already the same two-variant closed enum, not a
//! string. Introducing a string-typed runner here would only have to be
//! torn out again the moment `EN.10.C` lands — this task does not do that
//! and undo it.
//!
//! Only `EngineKind::Flow` is runnable today, because only `SDLC_FLOW` has
//! been ported to this engine (`workflows::sdlc_flow`) — there is no Rust
//! `SDLC_TASK` workflow yet. `EngineKind::Task` therefore fails loudly with
//! [`ExecuteError::UnsupportedEngine`] naming the block and repo, rather
//! than silently falling through to `Flow` or panicking.
//!
//! # Short-lived, cwd-scoped invocation
//!
//! Claude Code (and this engine's own `SDLC_FLOW` port, via
//! `sdlc_flow::setup::resolve_target_root`) picks up a repo's harness and
//! `CLAUDE.md` from its **working directory** — a long-lived session cannot
//! span repos, but a *workflow* may. [`default_flow_runner`] mirrors
//! `engine-serve`'s own `register_sdlc_flow_with_registry` factory: for
//! every step it resolves the step's repo slug through the injected
//! [`RepoRegistry`] to an absolute path, builds a **fresh**
//! `SDLC_FLOW` `Workflow` (policy-aware registry + schema, registered with
//! that same registry so `SetupWorktreeNode` resolves `event.repo` too),
//! seeds `event.repo` on the dispatched event, and runs it to completion —
//! nothing is kept alive or reused across steps. That keeps `/orchestrate`
//! rule 3 (one repo per session) true per invocation instead of per lane.

use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use uuid::Uuid;

use engine_contract::{NodeRunStatus, TaskContext};

use crate::budget::{Budget, BudgetLedger};
use crate::cancellation::CancellationToken;
use crate::workflow::node_cost_usd;
use serde_json::json;

use crate::completion::derive_terminal_status;
use crate::policy::PolicyConfigSource;
use crate::repo_registry::{RepoRegistry, RepoRegistryError};
use crate::workflows::sdlc_flow;
use crate::workflows::sdlc_task;
use crate::{OnProgress, RunOptions, Workflow, WorkflowError};

use super::chain::{ChainStep, StepKind};

// ── Engine selection ────────────────────────────────────────────────────

/// Which sanctioned SDLC engine runs a block — taken verbatim from the
/// block's authored `sdlc_workflow` field (`okf_core::TrackBlock`), which
/// carries exactly this two-value closed vocabulary (`"task"` / `"flow"`).
///
/// Re-exported from [`super::engine_kind`] (`EN.10.C` Task 1), which owns the
/// definition and the `sdlc_workflow -> EngineKind` mapping
/// ([`EngineKind::from_sdlc_workflow`]) — kept as `execute::EngineKind` here
/// so every existing caller and test of this module's public API keeps
/// working unchanged. `EngineKind` is a closed, two-variant type: there is no
/// third variant and no string-typed constructor, so an unsanctioned runner
/// cannot be represented, only diagnosed as an
/// [`super::engine_kind::UnsupportedSdlcWorkflow`].
pub use super::engine_kind::EngineKind;

// ── Invocation / runner seam ────────────────────────────────────────────

/// One resolved `SDLC_FLOW` invocation: the step's repo slug, its resolved
/// absolute filesystem root (the cwd the run must target), and the block
/// id to run as `spec_slug`. Handed to a [`FlowRunner`] so a test double
/// can assert exactly what cwd a step actually resolved to, not merely
/// what the caller intended.
// `PartialEq`/`Eq` are deliberately not derived here (EN.11.F task 3):
// `CancellationToken` wraps a `tokio::sync::watch::Sender` with no
// meaningful equality, so a derived impl either fails to compile or
// silently drops the field from comparison. Nothing in this crate compares
// two whole `FlowInvocation`s — callers compare individual fields.
#[derive(Debug, Clone)]
pub struct FlowInvocation {
    pub repo: String,
    pub repo_path: PathBuf,
    pub block_id: String,
    /// Whether this step's `SDLC_FLOW` run must be isolated into a fresh
    /// worktree (`true`) or run in place against `repo_path` (`false`) —
    /// resolved per step by [`resolve_isolation`] in [`execute_step`].
    /// Deliberately no `Default` and no builder on this struct (see the
    /// module-level "no `#[derive(Default)]`" note on `execute_step`'s
    /// construction site): an invocation with an unstated isolation is the
    /// defect this field exists to make unrepresentable.
    pub use_worktree: bool,
    /// The campaign this step belongs to — the parent id that spans the N
    /// runs of one chain (`EN.11.E`). Deliberately no `Default` and no
    /// builder on this struct, same reasoning as `use_worktree`: an
    /// invocation with an unstated campaign is the defect this field
    /// exists to make unrepresentable. Lands on the wire as a named,
    /// documented, versioned `campaign_id` key of the run's `event` JSON
    /// via [`sdlc_flow_event`] — never in `TaskContext::metadata`.
    pub campaign_id: Uuid,
    /// Which sanctioned engine this step's block authored — resolved once
    /// by `EN.11.P` task 1's `execute_step` and carried through so
    /// [`default_flow_runner`] (and any test double) dispatches the SAME
    /// engine the chain resolved, rather than re-deriving it. Deliberately
    /// no `Default` and no builder, same reasoning as `use_worktree` and
    /// `campaign_id`: an invocation with an unstated engine is exactly the
    /// defect this field exists to make unrepresentable.
    pub engine: EngineKind,
    /// The campaign-wide cancellation token, checked at block boundaries
    /// (`EN.11.F` task 4) — threaded here so [`default_flow_runner`] can
    /// hand it to the child's `RunOptions::cancellation_token`, the ONLY
    /// cancellation path (`RunOptions`'s own doc comment: a cancelled walk
    /// stops at a node boundary and returns `Ok(ctx)`, never `Err`, so a
    /// cancelled run stays distinguishable from a failed one). `None` means
    /// no cancellation checked inside the child at all. Deliberately no
    /// `Default` and no builder on this struct, same reasoning as
    /// `use_worktree` and `campaign_id`: an invocation with an unstated
    /// cancellation token is exactly the defect this field exists to make
    /// unrepresentable — callers must pass `None` explicitly rather than
    /// getting it for free.
    pub cancellation_token: Option<CancellationToken>,
    /// The campaign-scoped spend cap, threaded here so
    /// [`default_flow_runner`] can hand it to the child's
    /// `RunOptions::budget`. This is NOT the campaign-boundary ceiling
    /// checked by `CampaignLedger` (`EN.11.F` task 1/4) between steps —
    /// that check runs in `integrate.rs` before the NEXT step is even
    /// dispatched. This is the same per-node `Budget` shape handed straight
    /// through to `RunOptions`, so the child's own node-level
    /// `BudgetLedger::check` gate is configured too, rather than left at
    /// today's `budget: None`. `None` means no per-node gate inside the
    /// child. Deliberately no `Default` and no builder, same reasoning as
    /// the other fields above.
    pub budget: Option<Budget>,
}

/// A future producing a finished run's [`TaskContext`] or a [`WorkflowError`].
/// Deliberately **not** bounded `Send` — `Workflow::run`'s `OnProgress`
/// callback (`Box<dyn FnMut(&TaskContext)>`) is not `Send`, so a future that
/// awaits it cannot be either. [`execute_step`] only ever `.await`s this
/// directly (never spawns it onto a multi-threaded executor), so the
/// missing `Send` bound costs nothing here.
pub type FlowFuture = Pin<Box<dyn Future<Output = Result<TaskContext, WorkflowError>>>>;

/// The injectable "run `SDLC_FLOW` for this invocation" seam — the same
/// `Arc<dyn Fn(..) -> <boxed future>>` shape this crate's other injectable
/// seams (e.g. [`crate::workflows::ModelTransport`]) use, minus the `Send`
/// future bound (see [`FlowFuture`]). Production code uses
/// [`default_flow_runner`]; tests substitute a stub that records the
/// [`FlowInvocation`]s it was called with, so a step's resolved cwd is
/// asserted directly rather than inferred from intent.
pub type FlowRunner = Arc<dyn Fn(FlowInvocation) -> FlowFuture + Send + Sync>;

// ── Isolation policy ─────────────────────────────────────────────────────

/// The repo slug the `/begin-orchestration` Step 2 isolation table treats as
/// "always worktree" — a chain there edits `.claude/workflows/sdlc-*.js`
/// while those very engines are executing it, so an in-place run would be
/// editing the machinery driving it.
const ALWAYS_WORKTREE_REPO_SLUG: &str = "base-template";

/// Resolve whether one step should run `SDLC_FLOW` in a worktree (`true`)
/// or in place against the live checkout (`false`), per `/begin-orchestration`
/// Step 2's three-row table:
///
/// 1. `base-template` (matched by slug) -> **always** `true`. A chain there
///    edits `.claude/workflows/sdlc-*.js` while those very engines drive it
///    — running in place would rewrite the machinery mid-run.
/// 2. the brain root (matched by canonicalized `repo_path`, **not** slug —
///    HQ's slug varies by how a chain names it) -> **always** `false`.
///    `validate-brain` inside a worktree resolves the gitignored sub-repos
///    against the worktree's own `brain.toml`; measured 64 structure / 601
///    state errors versus 0/0 in the main tree, so a worktree there cannot
///    pass its own gates.
/// 3. anything else -> `default_use_worktree`, the resolved
///    `OrchestrationPolicy::default_use_worktree` knob.
///
/// Rows 1 and 2 are external contracts (standing rule 6's "fixed by an
/// external contract" qualifier), **not** policy knobs — they are matched
/// before `default_use_worktree` is ever consulted, so no combination of
/// policy, profile, or per-run event override can reach them. Both are
/// covered by a test that sets `default_use_worktree` the wrong way for
/// each row and still gets the right answer; that is what distinguishes
/// "the policy is implemented" from "the default happens to be right
/// today".
///
/// Canonicalization failure (a path that does not exist, e.g. an
/// as-yet-unresolved fixture path in a test) is treated as "does not match
/// the brain root" rather than propagated as an error — this function's
/// contract is a bool, and a step whose `repo_path` cannot be canonicalized
/// falls through to the ordinary-repo row exactly as if the comparison had
/// legitimately failed to match.
///
/// # Named seam: the lane files' `ISOLATION:` directive is NOT read here
///
/// 37 live lane files carry a per-lane `ISOLATION:` directive today, and
/// nothing consumes it — this function resolves isolation purely from
/// `repo_slug`, `repo_path`, and the policy fallback, never from a lane
/// directive. That is deliberate, not an oversight: mev's emitted
/// `LaneDirectives` carries only `held_until` / `budget` /
/// `exclusive_repos`, so `ISOLATION:` cannot be consumed here without a
/// mev-side change first to add it to that struct. Adding a second,
/// engine-side lane-file parser to read the raw directive text would
/// duplicate mev's parsing and drift from it within a release. The
/// follow-on: once mev's `LaneDirectives` grows an `isolation` field, plumb
/// it through `ChainStep::directives` (see [`super::chain::ChainStep`]) as
/// a fourth, highest-precedence row ahead of `default_use_worktree` — it
/// must stay subordinate to the two structural rows above, which are
/// external contracts and must remain unreachable by any per-lane override.
pub fn resolve_isolation(
    repo_slug: &str,
    repo_path: &Path,
    brain_root: &Path,
    default_use_worktree: bool,
) -> bool {
    if repo_slug == ALWAYS_WORKTREE_REPO_SLUG {
        return true;
    }

    let canonical_repo_path = repo_path.canonicalize();
    let canonical_brain_root = brain_root.canonicalize();
    if let (Ok(repo_path), Ok(brain_root)) = (canonical_repo_path, canonical_brain_root) {
        if repo_path == brain_root {
            return false;
        }
    }

    default_use_worktree
}

// ── Errors ───────────────────────────────────────────────────────────────

/// Everything that can go wrong executing one [`ChainStep`]. Every variant
/// names the block and repo involved, per this module's (and its sibling
/// `chain`/`gates` modules') "never fail silently" convention.
#[derive(Debug)]
pub enum ExecuteError {
    /// The step's [`StepKind`] is not [`StepKind::Block`] (`EN.12.E` Task 4).
    /// [`execute_step`] is the `block` path only — a `dispatch` step is
    /// routed to [`super::dispatch::execute_dispatch_step`] by the chain
    /// driver *before* it ever reaches this function, never through it (see
    /// that module's own "never through `super::execute::execute_step`"
    /// doc). This variant exists so a caller that skips that routing gets a
    /// loud, named diagnostic instead of `execute_step` silently running
    /// the wrong kind of step through the SDLC engine path.
    WrongStepKind {
        repo: String,
        block_id: String,
        kind: StepKind,
    },
    /// The step's repo slug did not resolve through the [`RepoRegistry`].
    RepoResolutionFailed {
        repo: String,
        block_id: String,
        source: RepoRegistryError,
    },
    /// The block's authored engine is outside what this task can run
    /// (today: anything but [`EngineKind::Flow`]).
    UnsupportedEngine {
        repo: String,
        block_id: String,
        engine: EngineKind,
    },
    /// The invoked `SDLC_FLOW` run itself returned an error. Surfaced with
    /// the block id and repo so a multi-block chain's failure is
    /// attributable at a glance.
    StepFailed {
        repo: String,
        block_id: String,
        source: WorkflowError,
    },
    /// `run_flow` returned `Ok(ctx)` (`SDLC_FLOW`'s never-`Err` contract,
    /// engine-rs D12), but the child run's own `ctx.node_runs` — read via the
    /// shared [`derive_terminal_status`] rather than a second "did this run
    /// fail?" implementation — reports `"failed"`. `Workflow::walk` breaks on
    /// a failed node and falls through to `Ok(ctx)`, so this is the ONLY
    /// place that failure becomes visible to the chain. Names the failing
    /// node (its `node_runs` key) so a multi-block chain's failure is
    /// attributable to the exact node that died, not just the step.
    ChildFailed {
        repo: String,
        block_id: String,
        failing_node: String,
    },
}

impl fmt::Display for ExecuteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExecuteError::WrongStepKind {
                repo,
                block_id,
                kind,
            } => write!(
                f,
                "step '{block_id}' (repo '{repo}') has kind {kind:?}, not Block — \
                 execute_step only runs block steps; a dispatch step must be routed to \
                 the dispatch node instead"
            ),
            ExecuteError::RepoResolutionFailed {
                repo,
                block_id,
                source,
            } => write!(
                f,
                "block '{block_id}' cannot start: repo '{repo}' did not resolve: {source}"
            ),
            ExecuteError::UnsupportedEngine {
                repo,
                block_id,
                engine,
            } => write!(
                f,
                "block '{block_id}' (repo '{repo}') declares engine '{engine}', which this \
                 workflow cannot run yet"
            ),
            ExecuteError::StepFailed {
                repo,
                block_id,
                source,
            } => write!(f, "block '{block_id}' (repo '{repo}') failed: {source}"),
            ExecuteError::ChildFailed {
                repo,
                block_id,
                failing_node,
            } => write!(
                f,
                "block '{block_id}' (repo '{repo}') failed: node '{failing_node}' did not succeed"
            ),
        }
    }
}

impl std::error::Error for ExecuteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ExecuteError::RepoResolutionFailed { source, .. } => Some(source),
            ExecuteError::WrongStepKind { .. } => None,
            ExecuteError::UnsupportedEngine { .. } => None,
            ExecuteError::StepFailed { source, .. } => Some(source),
            ExecuteError::ChildFailed { .. } => None,
        }
    }
}

/// The outcome of one successfully executed step: which block/repo ran,
/// the resolved cwd it ran against, and the run's finished [`TaskContext`]
/// (`EN.10.B` Task 4 reads this to verify the state write and append the
/// lane-log line).
#[derive(Debug)]
pub struct ExecutionOutcome {
    pub repo: String,
    pub repo_path: PathBuf,
    pub block_id: String,
    /// Which sanctioned engine actually ran this step — copied verbatim
    /// from the [`FlowInvocation`] the runner was called with, per CLAUDE.md
    /// standing rule 6 ("stamp the resolved value").
    pub engine: EngineKind,
    pub ctx: TaskContext,
    /// The isolation this step actually resolved to, per [`resolve_isolation`]
    /// — stamped here so a caller (`OrchestrationRunNode::process`) can
    /// attribute observed cost/behavior to the setting that produced it,
    /// per CLAUDE.md standing rule 6.
    pub use_worktree: bool,
    /// The campaign this step ran under — stamped here for the same reason
    /// as `use_worktree` (CLAUDE.md standing rule 6): a finished step must
    /// be attributable to its campaign without re-reading the child ctx.
    pub campaign_id: Uuid,
    /// This step's observed cost (USD), folded from `ctx` via
    /// [`step_spend`] — attributable to THIS `ChainStep` alone, never
    /// accumulated across steps (`EN.11.G` task 2).
    ///
    /// `Some(0.0)` means the child ran and every node that reported a
    /// cost figure reported exactly `$0`. `None` means no node in the
    /// child's `ctx.nodes` reported a cost figure at all — there is
    /// nothing to distinguish that from "zero" once collapsed to a bare
    /// `f64`, which is exactly the silently-wrong shape
    /// smoke-run.md §3.6 recorded (`total_cost_usd: -0.0` for a child
    /// that actually ran real nodes). Collapsing this to `f64` anywhere
    /// downstream reintroduces that bug — keep it `Option`.
    pub cost_usd: Option<f64>,
    /// This step's observed `input_tokens + output_tokens` total, folded
    /// from `ctx` via [`step_spend`]. Unlike `cost_usd`, a token total of
    /// `0` is unambiguous — every `NodeRun.usage` that IS present carries
    /// real token counts, so summing an empty or absent set is a true
    /// zero, not a "we don't know".
    pub total_tokens: u64,
}

/// Fold a completed child run's `ctx` into this step's own spend figures —
/// never the running chain total, only what THIS step's nodes reported.
/// Reuses [`BudgetLedger::from_context`] for the summing arithmetic (the
/// same reader `Workflow::run`'s own budget gate and a resume's lossy
/// ledger restore already trust) rather than re-deriving it here.
///
/// `cost_usd` is `None` unless at least one node identity in `ctx.nodes`
/// carries a `"cost_usd"` number — see [`ExecutionOutcome::cost_usd`]'s
/// doc for why this must stay a tri-state rather than collapsing "no node
/// reported a cost" and "every node reported exactly zero" into the same
/// bare `0.0`.
fn step_spend(ctx: &TaskContext) -> (Option<f64>, u64) {
    let ledger = BudgetLedger::from_context(ctx);
    let any_cost_reported = ctx
        .nodes
        .keys()
        .any(|identity| node_cost_usd(ctx, identity).is_some());
    let cost_usd = any_cost_reported.then(|| ledger.total_cost_usd());
    (cost_usd, ledger.total_tokens())
}

// ── Execution ────────────────────────────────────────────────────────────

/// Execute one [`ChainStep`]: resolve its repo to an absolute cwd via
/// `registry`, resolve its engine via `resolve_engine(repo, block_id)`, and
/// — for [`EngineKind::Flow`] only — invoke `run_flow` with the resolved
/// [`FlowInvocation`].
///
/// # Kind-routed (`EN.12.E` Task 4)
///
/// This function is the `block`-kind path only. A step whose [`StepKind`]
/// (`step.kind`) is not [`StepKind::Block`] never reaches the SDLC engine
/// path: it fails fast with [`ExecuteError::WrongStepKind`] before
/// `resolve_engine`/`registry`/`run_flow` are ever consulted. A `dispatch`
/// step's actual invocation is [`super::dispatch::execute_dispatch_step`],
/// called by the chain driver directly — never through here — so this
/// guard is a defense-in-depth backstop, not the routing decision itself.
/// A step with no `kind` deserializes to `StepKind::Block` (`EN.12.E` Task
/// 1), so every existing caller and test — none of which set `kind`
/// explicitly — is completely unaffected by this guard.
///
/// `resolve_engine` is consulted before the repo resolves to a cwd is
/// irrelevant to the caller: both must succeed for the step to run, and
/// either failing reports the block id and repo. `run_flow` is never
/// called for an unsupported engine.
///
/// `default_use_worktree` is the resolved `OrchestrationPolicy::default_use_worktree`
/// fallback (row 3 of [`resolve_isolation`]'s table) — this function does not
/// read policy itself, it only consults `registry.brain_root()` (row 2) and
/// `step.repo` (row 1) alongside whatever the caller passed in for row 3.
///
/// `cancellation_token`/`budget` (`EN.11.F` task 3) are threaded straight
/// into the resolved [`FlowInvocation`] for [`default_flow_runner`] to hand
/// to the child's `RunOptions` — this function does not itself check
/// either; the block-boundary check is `integrate.rs`'s job (task 4).
#[allow(clippy::too_many_arguments)]
pub async fn execute_step(
    step: &ChainStep,
    resolve_engine: &dyn Fn(&str, &str) -> EngineKind,
    registry: &RepoRegistry,
    run_flow: &FlowRunner,
    default_use_worktree: bool,
    campaign_id: Uuid,
    cancellation_token: Option<CancellationToken>,
    budget: Option<Budget>,
) -> Result<ExecutionOutcome, ExecuteError> {
    if step.kind != StepKind::Block {
        return Err(ExecuteError::WrongStepKind {
            repo: step.repo.clone(),
            block_id: step.block_id.clone(),
            kind: step.kind,
        });
    }

    let repo_path =
        registry
            .resolve(&step.repo)
            .map_err(|source| ExecuteError::RepoResolutionFailed {
                repo: step.repo.clone(),
                block_id: step.block_id.clone(),
                source,
            })?;

    let engine = resolve_engine(&step.repo, &step.block_id);
    // Both sanctioned engines dispatch through the same `run_flow` seam —
    // `FlowInvocation.engine` tells the runner which one to build and run
    // (`EN.11.P` task 1). This match exists to keep `EngineKind` closed:
    // it is exhaustive over both variants, so it fails to compile the
    // moment a third variant is ever added, which is exactly the
    // compile-time guard the block's acceptance criteria require.
    // `ExecuteError::UnsupportedEngine` is NOT produced from here anymore
    // — `resolve_engine` is infallible over the closed `EngineKind`
    // vocabulary, so both variants it can ever return are runnable. The
    // variant stays reachable for a block whose authored `sdlc_workflow`
    // fails `EngineKind::from_sdlc_workflow` before a chain ever reaches
    // this function (see the module's `UnsupportedEngine` doc and the
    // `unsupported_engine_error_still_displays_the_block_and_repo` test).
    match engine {
        EngineKind::Flow | EngineKind::Task => {}
    }

    let use_worktree = resolve_isolation(
        &step.repo,
        &repo_path,
        registry.brain_root(),
        default_use_worktree,
    );
    let invocation = FlowInvocation {
        repo: step.repo.clone(),
        repo_path: repo_path.clone(),
        block_id: step.block_id.clone(),
        use_worktree,
        campaign_id,
        engine,
        cancellation_token,
        budget,
    };
    let ctx = run_flow(invocation)
        .await
        .map_err(|source| ExecuteError::StepFailed {
            repo: step.repo.clone(),
            block_id: step.block_id.clone(),
            source,
        })?;

    // `run_flow` returning `Ok(ctx)` is not proof the child succeeded:
    // `Workflow::walk` breaks on a failed node and falls through to
    // `Ok(ctx)` (`SDLC_FLOW`'s never-`Err` contract, engine-rs D12), so the
    // failure lives only in `ctx.node_runs`. Read it via the SHARED
    // `derive_terminal_status` — never a second "did this run fail?" check —
    // and stop the chain here rather than integrating a failed step as a
    // success.
    if derive_terminal_status(&ctx) == "failed" {
        let failing_node = ctx
            .node_runs
            .iter()
            .find(|(_, run)| run.status == NodeRunStatus::Failed)
            .map(|(name, _)| name.clone())
            .unwrap_or_else(|| "<unknown>".to_string());
        return Err(ExecuteError::ChildFailed {
            repo: step.repo.clone(),
            block_id: step.block_id.clone(),
            failing_node,
        });
    }

    let (cost_usd, total_tokens) = step_spend(&ctx);

    Ok(ExecutionOutcome {
        repo: step.repo.clone(),
        repo_path,
        block_id: step.block_id.clone(),
        engine,
        ctx,
        use_worktree,
        campaign_id,
        cost_usd,
        total_tokens,
    })
}

/// Build the `SDLC_FLOW` event JSON for one resolved [`FlowInvocation`] —
/// factored out of [`default_flow_runner`] so the seeded shape (including
/// `use_worktree`) is unit-testable without spinning up a real `SDLC_FLOW`
/// `Workflow` run.
fn sdlc_flow_event(invocation: &FlowInvocation) -> serde_json::Value {
    json!({
        "repo": invocation.repo,
        "spec_slug": invocation.block_id,
        "use_worktree": invocation.use_worktree,
        "campaign_id": invocation.campaign_id,
    })
}

/// Build the `SDLC_TASK` event JSON for one resolved [`FlowInvocation`] —
/// same seeded shape as [`sdlc_flow_event`] against
/// `sdlc_task::schema::SdlcTaskEventSchema` (`spec_slug` is the only
/// required field; `repo`/`use_worktree` deserialize the same names as
/// `SDLC_FLOW`'s schema). Deliberately does NOT seed `auto_pr` —
/// `SdlcTaskEventSchema` drops that field entirely (`SDLC_TASK` ships no PR
/// ceremony), so there is nothing here to set.
fn sdlc_task_event(invocation: &FlowInvocation) -> serde_json::Value {
    json!({
        "repo": invocation.repo,
        "spec_slug": invocation.block_id,
        "use_worktree": invocation.use_worktree,
        "campaign_id": invocation.campaign_id,
    })
}

/// The production [`FlowRunner`]: for each [`FlowInvocation`], builds a
/// **fresh** `Workflow` for the invocation's [`EngineKind`] — policy
/// resolved from `PolicyConfigSource::Worktree(invocation.repo_path)`,
/// `SetupWorktreeNode` re-registered with `registry` so `event.repo`
/// resolves inside the run too — and runs it with the matching seeded
/// event as the trigger, mirroring
/// `engine-serve::workflows::register_sdlc_flow_with_registry` /
/// `register_sdlc_task_with_registry`'s factories exactly — isolation
/// included — minus the `Dispatcher` registration (this seam invokes the
/// workflow directly rather than dispatching an HTTP event).
///
/// [`EngineKind::Flow`] keeps today's exact path
/// (`sdlc_flow::setup::resolve_policy_for_run_from` +
/// `sdlc_flow::graph::registry_for_policy` + `sdlc_flow::graph::schema()`).
/// [`EngineKind::Task`] mirrors it against `sdlc_task`'s own entry points
/// (`sdlc_task::profiles::resolve_policy_for_run_from` +
/// `sdlc_task::graph::registry_for_policy` + `sdlc_task::graph::schema()`).
///
/// Never reimplements either engine: every node in a run is that engine's
/// own module's node.
#[must_use]
pub fn default_flow_runner(registry: Arc<RepoRegistry>) -> FlowRunner {
    Arc::new(move |invocation: FlowInvocation| {
        let registry = registry.clone();
        Box::pin(async move {
            match invocation.engine {
                EngineKind::Flow => {
                    let event = sdlc_flow_event(&invocation);

                    let ctx_for_policy = TaskContext {
                        event: event.clone(),
                        nodes: std::collections::HashMap::new(),
                        metadata: json!({}),
                        node_runs: std::collections::HashMap::new(),
                    };
                    let source = PolicyConfigSource::Worktree(invocation.repo_path.clone());
                    let policy =
                        sdlc_flow::setup::resolve_policy_for_run_from(&ctx_for_policy, &source)
                            .map_err(|err| WorkflowError::new(err.to_string()))?;

                    let mut node_registry = sdlc_flow::graph::registry_for_policy(&policy);
                    node_registry.register(Box::new(
                        sdlc_flow::setup::SetupWorktreeNode::new().with_registry(registry.clone()),
                    ));

                    let workflow =
                        Workflow::new_validated(node_registry, sdlc_flow::graph::schema())
                            .map_err(|err| WorkflowError::new(err.to_string()))?;
                    let on_progress: OnProgress<'_> = Box::new(|_ctx: &TaskContext| {});
                    // `RunOptions` threaded from the invocation (`EN.11.F`
                    // task 3): the campaign's cancellation token and
                    // per-node budget are handed straight through to the
                    // child's own boundary checks. `pause_signal`/`run_id`
                    // stay `None` — out of scope here, same as before.
                    let options = RunOptions {
                        cancellation_token: invocation.cancellation_token.clone(),
                        budget: invocation.budget,
                        ..RunOptions::default()
                    };
                    workflow.run_with(event, on_progress, options).await
                }
                EngineKind::Task => {
                    let event = sdlc_task_event(&invocation);

                    let ctx_for_policy = TaskContext {
                        event: event.clone(),
                        nodes: std::collections::HashMap::new(),
                        metadata: json!({}),
                        node_runs: std::collections::HashMap::new(),
                    };
                    let source = PolicyConfigSource::Worktree(invocation.repo_path.clone());
                    let policy =
                        sdlc_task::profiles::resolve_policy_for_run_from(&ctx_for_policy, &source)
                            .map_err(|err| WorkflowError::new(err.to_string()))?;

                    let mut node_registry = sdlc_task::graph::registry_for_policy(&policy);
                    node_registry.register(Box::new(
                        sdlc_flow::setup::SetupWorktreeNode::new().with_registry(registry.clone()),
                    ));

                    let workflow =
                        Workflow::new_validated(node_registry, sdlc_task::graph::schema())
                            .map_err(|err| WorkflowError::new(err.to_string()))?;
                    let on_progress: OnProgress<'_> = Box::new(|_ctx: &TaskContext| {});
                    let options = RunOptions {
                        cancellation_token: invocation.cancellation_token.clone(),
                        budget: invocation.budget,
                        ..RunOptions::default()
                    };
                    workflow.run_with(event, on_progress, options).await
                }
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    fn step(repo: &str, block_id: &str) -> ChainStep {
        ChainStep {
            repo: repo.to_string(),
            block_id: block_id.to_string(),
            directives: None,
            ..Default::default()
        }
    }

    /// A tempdir `brain.toml` + repo registry with two real repo
    /// directories, `repo-a` and `repo-b`, mirroring the tempdir-fixture
    /// pattern `repo_registry.rs`'s own tests already use.
    fn two_repo_registry() -> (tempfile::TempDir, RepoRegistry) {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("repo-a")).unwrap();
        std::fs::create_dir_all(dir.path().join("repo-b")).unwrap();
        std::fs::write(
            dir.path().join("brain.toml"),
            "[[repos]]\nslug = \"repo-a\"\nrepo_path = \"repo-a\"\n\
             [[repos]]\nslug = \"repo-b\"\nrepo_path = \"repo-b\"\n",
        )
        .unwrap();
        let registry = RepoRegistry::from_brain_root(dir.path()).expect("registry");
        (dir, registry)
    }

    /// A `FlowRunner` test double that records every [`FlowInvocation`] it
    /// was called with and returns a fixed, empty successful `TaskContext`.
    fn recording_runner() -> (FlowRunner, Arc<Mutex<Vec<FlowInvocation>>>) {
        let calls: Arc<Mutex<Vec<FlowInvocation>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let runner: FlowRunner = Arc::new(move |invocation: FlowInvocation| {
            recorded.lock().unwrap().push(invocation);
            Box::pin(async {
                Ok(TaskContext {
                    event: json!({}),
                    nodes: std::collections::HashMap::new(),
                    metadata: json!({}),
                    node_runs: std::collections::HashMap::new(),
                })
            })
        });
        (runner, calls)
    }

    #[tokio::test]
    async fn two_repo_chain_executes_each_step_with_cwd_set_to_that_steps_repo() {
        let (dir, registry) = two_repo_registry();
        let (runner, calls) = recording_runner();
        let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;

        let step_a = step("repo-a", "A.1");
        let step_b = step("repo-b", "B.1");

        let outcome_a = execute_step(
            &step_a,
            &resolve_engine,
            &registry,
            &runner,
            false,
            Uuid::new_v4(),
            None,
            None,
        )
        .await
        .expect("step a should execute");
        let outcome_b = execute_step(
            &step_b,
            &resolve_engine,
            &registry,
            &runner,
            false,
            Uuid::new_v4(),
            None,
            None,
        )
        .await
        .expect("step b should execute");

        assert_eq!(outcome_a.repo_path, dir.path().join("repo-a"));
        assert_eq!(outcome_b.repo_path, dir.path().join("repo-b"));

        // Assert the cwd *actually passed* to the runner, not merely the
        // outcome the caller computed independently.
        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].repo_path, dir.path().join("repo-a"));
        assert_eq!(recorded[0].block_id, "A.1");
        assert_eq!(recorded[1].repo_path, dir.path().join("repo-b"));
        assert_eq!(recorded[1].block_id, "B.1");
    }

    #[tokio::test]
    async fn blocks_authored_engine_selects_the_runner() {
        let (_dir, registry) = two_repo_registry();
        let (runner, calls) = recording_runner();
        let call_count = Arc::new(AtomicUsize::new(0));
        let counted = call_count.clone();
        let resolve_engine = move |_repo: &str, _id: &str| {
            counted.fetch_add(1, Ordering::SeqCst);
            EngineKind::Flow
        };

        let s = step("repo-a", "A.1");
        execute_step(
            &s,
            &resolve_engine,
            &registry,
            &runner,
            false,
            Uuid::new_v4(),
            None,
            None,
        )
        .await
        .expect("flow-engine step should execute");

        assert_eq!(call_count.load(Ordering::SeqCst), 1);
        assert_eq!(calls.lock().unwrap().len(), 1);
    }

    /// FLIPPED (`EN.11.P` task 1): the task engine is no longer unsupported
    /// — it dispatches and invokes the runner, exactly like the flow
    /// engine, with `FlowInvocation.engine == EngineKind::Task` so a test
    /// double (or the production runner) can tell the two apart. No other
    /// guard in this module was removed to make this pass:
    /// `ExecuteError::UnsupportedEngine` still exists (see
    /// `unsupported_engine_error_still_displays_the_block_and_repo`) and
    /// `EngineKind` is still asserted closed to two variants by the
    /// exhaustive `match` in `execute_step` (no `_` arm — a third variant
    /// would fail to compile).
    #[tokio::test]
    async fn task_engine_is_dispatched_and_invokes_the_runner() {
        let (_dir, registry) = two_repo_registry();
        let (runner, calls) = recording_runner();
        let resolve_engine = |_repo: &str, _id: &str| EngineKind::Task;

        let s = step("repo-a", "A.1");
        let outcome = execute_step(
            &s,
            &resolve_engine,
            &registry,
            &runner,
            false,
            Uuid::new_v4(),
            None,
            None,
        )
        .await
        .expect("task-engine step should execute");

        assert_eq!(outcome.engine, EngineKind::Task);

        // The runner must have been invoked exactly once, with the
        // resolved task engine on the invocation — asserted via the
        // recording double, not inferred from the outcome alone.
        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].engine, EngineKind::Task);
        assert_eq!(recorded[0].repo, "repo-a");
        assert_eq!(recorded[0].block_id, "A.1");
    }

    /// `ExecuteError::UnsupportedEngine` stays defined and constructible
    /// even though `execute_step`'s exhaustive match over `EngineKind` no
    /// longer produces it for either sanctioned variant — it remains the
    /// diagnostic a caller surfaces for a block whose authored
    /// `sdlc_workflow` fails `EngineKind::from_sdlc_workflow` (an
    /// `UnsupportedSdlcWorkflow`, e.g. `"sdlc-run"`/`"sdlc-block"`/typo/
    /// absent) before a chain ever reaches `execute_step`. Deleting this
    /// variant would remove that guard entirely.
    #[test]
    fn unsupported_engine_error_still_displays_the_block_and_repo() {
        let err = ExecuteError::UnsupportedEngine {
            repo: "repo-a".to_string(),
            block_id: "A.1".to_string(),
            engine: EngineKind::Task,
        };
        let msg = err.to_string();
        assert!(msg.contains("A.1"), "message should name the block: {msg}");
        assert!(
            msg.contains("repo-a"),
            "message should name the repo: {msg}"
        );
    }

    #[tokio::test]
    async fn a_steps_failure_surfaces_with_the_block_id() {
        let (_dir, registry) = two_repo_registry();
        let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;
        let failing_runner: FlowRunner = Arc::new(|invocation: FlowInvocation| {
            Box::pin(async move {
                Err(WorkflowError::new(format!(
                    "boom while running {}",
                    invocation.block_id
                )))
            })
        });

        let s = step("repo-a", "A.1");
        let err = execute_step(
            &s,
            &resolve_engine,
            &registry,
            &failing_runner,
            false,
            Uuid::new_v4(),
            None,
            None,
        )
        .await
        .unwrap_err();

        match &err {
            ExecuteError::StepFailed { repo, block_id, .. } => {
                assert_eq!(repo, "repo-a");
                assert_eq!(block_id, "A.1");
            }
            other => panic!("expected StepFailed, got {other:?}"),
        }
        let msg = err.to_string();
        assert!(msg.contains("A.1"), "message should name the block: {msg}");
        assert!(
            msg.contains("repo-a"),
            "message should name the repo: {msg}"
        );
    }

    #[tokio::test]
    async fn unknown_repo_slug_fails_before_the_runner_is_ever_consulted() {
        let (_dir, registry) = two_repo_registry();
        let (runner, calls) = recording_runner();
        let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;

        let s = step("does-not-exist", "A.1");
        let err = execute_step(
            &s,
            &resolve_engine,
            &registry,
            &runner,
            false,
            Uuid::new_v4(),
            None,
            None,
        )
        .await
        .unwrap_err();

        assert!(matches!(err, ExecuteError::RepoResolutionFailed { .. }));
        assert!(calls.lock().unwrap().is_empty());
    }

    /// `EN.12.E` Task 4: a non-`Block` step (`dispatch` here, standing in
    /// for `command` too — both hit the same guard) never reaches the SDLC
    /// engine path. Asserted two ways: the returned error names the step
    /// and its kind, AND — the criterion that actually matters — the
    /// `FlowRunner` recording double was never invoked at all, proving the
    /// dispatch path never falls through to `run_flow`.
    #[tokio::test]
    async fn a_dispatch_kind_step_never_reaches_the_sdlc_engine_path() {
        let (_dir, registry) = two_repo_registry();
        let (runner, calls) = recording_runner();
        let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;

        let s = ChainStep {
            repo: "repo-a".to_string(),
            block_id: "RESEARCH_AGENT".to_string(),
            directives: None,
            kind: StepKind::Dispatch,
            ..Default::default()
        };
        let err = execute_step(
            &s,
            &resolve_engine,
            &registry,
            &runner,
            false,
            Uuid::new_v4(),
            None,
            None,
        )
        .await
        .unwrap_err();

        match &err {
            ExecuteError::WrongStepKind {
                repo,
                block_id,
                kind,
            } => {
                assert_eq!(repo, "repo-a");
                assert_eq!(block_id, "RESEARCH_AGENT");
                assert_eq!(*kind, StepKind::Dispatch);
            }
            other => panic!("expected WrongStepKind, got {other:?}"),
        }
        let msg = err.to_string();
        assert!(
            msg.contains("RESEARCH_AGENT"),
            "message should name the block: {msg}"
        );
        assert!(
            calls.lock().unwrap().is_empty(),
            "run_flow (the SDLC engine path) must never be consulted for a dispatch step"
        );
    }

    /// A chain step with no `kind` set (via `..Default::default()`, exactly
    /// how every pre-`EN.12.E` test in this module constructs a
    /// `ChainStep`) still deserializes/defaults to `StepKind::Block` and
    /// runs the ordinary SDLC engine path unchanged — the guard above never
    /// fires for it.
    #[tokio::test]
    async fn a_step_with_no_kind_still_runs_the_block_path() {
        let (_dir, registry) = two_repo_registry();
        let (runner, calls) = recording_runner();
        let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;

        let s = step("repo-a", "A.1");
        assert_eq!(s.kind, StepKind::Block);
        let outcome = execute_step(
            &s,
            &resolve_engine,
            &registry,
            &runner,
            false,
            Uuid::new_v4(),
            None,
            None,
        )
        .await
        .expect("a block-kind step should execute exactly as before");

        assert_eq!(outcome.block_id, "A.1");
        assert_eq!(calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn engine_kind_display_names_are_stable() {
        assert_eq!(EngineKind::Task.to_string(), "task");
        assert_eq!(EngineKind::Flow.to_string(), "flow");
    }

    /// A `FlowRunner` test double that returns `Ok(ctx)` where `ctx.node_runs`
    /// records `node_name` as [`NodeRunStatus::Failed`] — mirroring
    /// `SDLC_FLOW`'s real never-`Err` contract (engine-rs D12): `Workflow::walk`
    /// breaks on a failed node and falls through to `Ok(ctx)`, so a successful
    /// `run_flow` future is not proof the child succeeded.
    fn ok_but_child_failed_runner(node_name: &'static str) -> FlowRunner {
        Arc::new(move |_invocation: FlowInvocation| {
            let mut node_runs = std::collections::HashMap::new();
            node_runs.insert(
                node_name.to_string(),
                engine_contract::NodeRun {
                    status: NodeRunStatus::Failed,
                    started_at: None,
                    completed_at: None,
                    error: Some("boom".to_string()),
                    input: None,
                    usage: None,
                },
            );
            Box::pin(async move {
                Ok(TaskContext {
                    event: json!({}),
                    nodes: std::collections::HashMap::new(),
                    metadata: json!({}),
                    node_runs,
                })
            })
        })
    }

    #[tokio::test]
    async fn a_child_that_returns_ok_but_recorded_a_failed_node_still_fails_the_step() {
        let (_dir, registry) = two_repo_registry();
        let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;
        let runner = ok_but_child_failed_runner("SetupWorktreeNode");

        let s = step("repo-a", "A.1");
        let err = execute_step(
            &s,
            &resolve_engine,
            &registry,
            &runner,
            false,
            Uuid::new_v4(),
            None,
            None,
        )
        .await
        .expect_err("a child with a failed node_run must fail the step");

        match &err {
            ExecuteError::ChildFailed {
                repo,
                block_id,
                failing_node,
            } => {
                assert_eq!(repo, "repo-a");
                assert_eq!(block_id, "A.1");
                assert_eq!(failing_node, "SetupWorktreeNode");
            }
            other => panic!("expected ChildFailed, got {other:?}"),
        }
        let msg = err.to_string();
        assert!(msg.contains("A.1"), "message should name the block: {msg}");
        assert!(
            msg.contains("repo-a"),
            "message should name the repo: {msg}"
        );
        assert!(
            msg.contains("SetupWorktreeNode"),
            "message should name the failing node: {msg}"
        );
    }

    #[tokio::test]
    async fn a_child_that_returns_ok_with_no_failed_nodes_still_integrates_as_success() {
        // No over-correction: a genuinely successful run (recording_runner's
        // fixed, empty, all-succeeded TaskContext) must still be integrated.
        let (_dir, registry) = two_repo_registry();
        let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;
        let (runner, _calls) = recording_runner();

        let s = step("repo-a", "A.1");
        let outcome = execute_step(
            &s,
            &resolve_engine,
            &registry,
            &runner,
            false,
            Uuid::new_v4(),
            None,
            None,
        )
        .await
        .expect("a run with no failed nodes should be integrated as success");

        assert_eq!(outcome.block_id, "A.1");
    }

    // ── Per-step cost/token attribution (EN.11.G task 2) ────────────────

    /// A `FlowRunner` test double returning a fixed `TaskContext` whose
    /// `nodes`/`node_runs` carry a known cost/usage shape — the smallest
    /// fixture that lets a test assert `ExecutionOutcome.cost_usd` /
    /// `.total_tokens` against a figure the test itself picked.
    fn runner_with_usage(
        node_name: &'static str,
        cost_usd: f64,
        input: u64,
        output: u64,
    ) -> FlowRunner {
        Arc::new(move |_invocation: FlowInvocation| {
            let mut nodes = std::collections::HashMap::new();
            nodes.insert(node_name.to_string(), json!({"cost_usd": cost_usd}));
            let mut node_runs = std::collections::HashMap::new();
            node_runs.insert(
                node_name.to_string(),
                engine_contract::NodeRun {
                    status: NodeRunStatus::Success,
                    started_at: None,
                    completed_at: None,
                    error: None,
                    input: None,
                    usage: Some(engine_contract::Usage {
                        input_tokens: Some(input),
                        output_tokens: Some(output),
                        model: "claude-sonnet-4-5".to_string(),
                    }),
                },
            );
            Box::pin(async move {
                Ok(TaskContext {
                    event: json!({}),
                    nodes,
                    metadata: json!({}),
                    node_runs,
                })
            })
        })
    }

    #[tokio::test]
    async fn a_steps_outcome_carries_its_own_observed_cost_and_tokens() {
        let (_dir, registry) = two_repo_registry();
        let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;
        let runner = runner_with_usage("SomeNode", 1.25, 100, 200);

        let s = step("repo-a", "A.1");
        let outcome = execute_step(
            &s,
            &resolve_engine,
            &registry,
            &runner,
            false,
            Uuid::new_v4(),
            None,
            None,
        )
        .await
        .expect("step should execute");

        assert_eq!(outcome.cost_usd, Some(1.25));
        assert_eq!(outcome.total_tokens, 300);
    }

    #[tokio::test]
    async fn a_child_that_reports_no_usage_at_all_yields_an_explicit_absent_figure() {
        // recording_runner's fixed TaskContext has empty `nodes` AND empty
        // `node_runs` — no node ever reported a cost or usage figure. This
        // must surface as `None`, never a bare `0.0` that reads as "spent
        // nothing" when the truth is "we don't know" (smoke-run.md §3.6).
        let (_dir, registry) = two_repo_registry();
        let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;
        let (runner, _calls) = recording_runner();

        let s = step("repo-a", "A.1");
        let outcome = execute_step(
            &s,
            &resolve_engine,
            &registry,
            &runner,
            false,
            Uuid::new_v4(),
            None,
            None,
        )
        .await
        .expect("step should execute");

        assert_eq!(outcome.cost_usd, None);
        assert_eq!(outcome.total_tokens, 0);
    }

    #[test]
    fn step_spend_reports_an_explicit_zero_when_every_node_reported_exactly_zero() {
        // Distinct from the "no node reported anything" case above: here a
        // node DID report a cost figure, and that figure happens to be
        // zero — `Some(0.0)`, never collapsed to the same `None` as "no
        // figure at all".
        let mut nodes = std::collections::HashMap::new();
        nodes.insert("SomeNode".to_string(), json!({"cost_usd": 0.0}));
        let ctx = TaskContext {
            event: json!({}),
            nodes,
            metadata: json!({}),
            node_runs: std::collections::HashMap::new(),
        };

        let (cost_usd, total_tokens) = step_spend(&ctx);
        assert_eq!(cost_usd, Some(0.0));
        assert_eq!(total_tokens, 0);
    }

    // ── resolve_isolation ────────────────────────────────────────────

    #[test]
    fn base_template_always_resolves_to_worktree_even_with_default_false() {
        let (dir, _registry) = two_repo_registry();
        let repo_path = dir.path().join("repo-a");
        assert!(resolve_isolation(
            "base-template",
            &repo_path,
            dir.path(),
            false,
        ));
    }

    #[test]
    fn base_template_always_resolves_to_worktree_even_with_default_true() {
        let (dir, _registry) = two_repo_registry();
        let repo_path = dir.path().join("repo-a");
        assert!(resolve_isolation(
            "base-template",
            &repo_path,
            dir.path(),
            true,
        ));
    }

    #[test]
    fn brain_root_always_resolves_to_in_place_even_with_default_true() {
        let (dir, _registry) = two_repo_registry();
        // The step's resolved repo_path IS the brain root itself here —
        // mirrors HQ, where the chain's own repo resolves to the brain
        // root path.
        assert!(!resolve_isolation(
            "agentic-portfolio",
            dir.path(),
            dir.path(),
            true,
        ));
    }

    #[test]
    fn brain_root_always_resolves_to_in_place_even_with_default_false() {
        let (dir, _registry) = two_repo_registry();
        assert!(!resolve_isolation(
            "agentic-portfolio",
            dir.path(),
            dir.path(),
            false,
        ));
    }

    #[test]
    fn brain_root_is_matched_by_canonicalized_path_not_by_slug() {
        let (dir, _registry) = two_repo_registry();
        // A slug that has nothing to do with "brain" or "hq" must still be
        // recognized as the brain root purely by path — the whole point of
        // canonicalized-path matching over slug matching.
        assert!(!resolve_isolation(
            "whatever-this-chain-calls-it",
            dir.path(),
            dir.path(),
            true,
        ));
    }

    #[test]
    fn ordinary_repo_resolves_to_the_default_both_ways() {
        let (dir, _registry) = two_repo_registry();
        let repo_path = dir.path().join("repo-a");
        assert!(!resolve_isolation("repo-a", &repo_path, dir.path(), false,));
        assert!(resolve_isolation("repo-a", &repo_path, dir.path(), true,));
    }

    // ── FlowInvocation.use_worktree threading ───────────────────────────

    #[test]
    fn sdlc_flow_event_seeds_use_worktree_with_the_invocations_value() {
        let invocation_true = FlowInvocation {
            repo: "repo-a".to_string(),
            repo_path: PathBuf::from("/tmp/repo-a"),
            block_id: "A.1".to_string(),
            use_worktree: true,
            campaign_id: Uuid::new_v4(),
            engine: EngineKind::Flow,
            cancellation_token: None,
            budget: None,
        };
        assert_eq!(
            sdlc_flow_event(&invocation_true)["use_worktree"],
            json!(true)
        );

        let invocation_false = FlowInvocation {
            use_worktree: false,
            ..invocation_true
        };
        assert_eq!(
            sdlc_flow_event(&invocation_false)["use_worktree"],
            json!(false)
        );
    }

    // ── FlowInvocation.campaign_id threading ─────────────────────────────

    #[test]
    fn sdlc_flow_event_seeds_campaign_id_with_the_invocations_value() {
        let campaign_id = Uuid::new_v4();
        let invocation = FlowInvocation {
            repo: "repo-a".to_string(),
            repo_path: PathBuf::from("/tmp/repo-a"),
            block_id: "A.1".to_string(),
            use_worktree: true,
            campaign_id,
            engine: EngineKind::Flow,
            cancellation_token: None,
            budget: None,
        };
        assert_eq!(
            sdlc_flow_event(&invocation)["campaign_id"],
            json!(campaign_id)
        );
    }

    /// A registry with an ordinary repo AND a `base-template`-slugged repo,
    /// so a chain step can exercise `resolve_isolation`'s row-1 override
    /// through `execute_step` rather than only through the unit-level
    /// `resolve_isolation` tests above.
    fn registry_with_base_template() -> (tempfile::TempDir, RepoRegistry) {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("repo-a")).unwrap();
        std::fs::create_dir_all(dir.path().join("base-template")).unwrap();
        std::fs::write(
            dir.path().join("brain.toml"),
            "[[repos]]\nslug = \"repo-a\"\nrepo_path = \"repo-a\"\n\
             [[repos]]\nslug = \"base-template\"\nrepo_path = \"base-template\"\n",
        )
        .unwrap();
        let registry = RepoRegistry::from_brain_root(dir.path()).expect("registry");
        (dir, registry)
    }

    #[tokio::test]
    async fn recording_double_observes_the_resolved_isolation_for_each_step_of_a_chain() {
        let (_dir, registry) = registry_with_base_template();
        let (runner, calls) = recording_runner();
        let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;

        // `default_use_worktree` is `true` here so the ordinary step
        // resolves to the non-default `true` and the `base-template` step's
        // structural override (always `true`, regardless of default) is
        // exercised alongside it in the same chain.
        let ordinary = step("repo-a", "A.1");
        let base_template = step("base-template", "BT.1");

        execute_step(
            &ordinary,
            &resolve_engine,
            &registry,
            &runner,
            true,
            Uuid::new_v4(),
            None,
            None,
        )
        .await
        .expect("ordinary step should execute");
        execute_step(
            &base_template,
            &resolve_engine,
            &registry,
            &runner,
            false,
            Uuid::new_v4(),
            None,
            None,
        )
        .await
        .expect("base-template step should execute");

        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 2);
        assert!(
            recorded[0].use_worktree,
            "ordinary repo should resolve to the passed-in default_use_worktree (true)"
        );
        assert!(
            recorded[1].use_worktree,
            "base-template must resolve to worktree=true even with default_use_worktree=false"
        );
    }
}
