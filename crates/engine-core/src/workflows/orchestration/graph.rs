//! ORCHESTRATION workflow assembly — `EN.10.B` Task 5.
//!
//! Declared graph shape:
//!
//! ```text
//! OrchestrationRunNode
//! ```
//!
//! `OrchestrationRunNode` is both the start node and the sole (terminal)
//! node — the whole ORCHESTRATION workflow is one node wrapping
//! [`chain::resolve_lane_chain`]/[`chain::resolve_explicit_chain`] (Task 1)
//! and [`integrate::integrate_chain`] (Task 4, which itself composes Task 2's
//! gates and Task 3's `SDLC_FLOW` invocation) — there is nothing left to
//! branch a graph over. Mirrors `diagnostic_intake::graph`'s single-node
//! shape.
//!
//! # Policy
//!
//! This workflow carries three knobs: `hold_poll_interval_ms` — how often
//! [`integrate::wait_for_clearance`] re-polls an operator hold while paused
//! — `default_use_worktree` — the row-3 fallback
//! [`execute::resolve_isolation`] consults for any repo that isn't one of
//! the two non-negotiable rows (`base-template` always `true`, the brain
//! root always `false`) — and (EN.11.L Task 3) `hold_deadline_ms` — the
//! total budget [`integrate::wait_for_clearance`] allows a single hold
//! before it fails the chain loudly (`None`, the built-in default,
//! preserves the pre-Task-2 unbounded wait). All three are pure
//! latency/overhead/isolation/reliability trades that never change the
//! declared node set, so — unlike `sdlc_flow`/`diagnostic_intake` — there
//! is no `registry_for_policy`: [`registry`] alone is what every profile
//! runs.
//! [`OrchestrationRunNode::process`] resolves the four-layer policy
//! (`crate::policy::resolve`) itself, exactly where `diagnostic_intake`'s
//! sole `IntakeExtractNode` does, since there is likewise no dedicated setup
//! node here.
//!
//! # Injectable seams, sane defaults
//!
//! [`OrchestrationRunNode::new`] defaults every closure this workflow needs
//! ([`chain::resolve_lane_chain`]'s `is_block_open`, [`gates::check_dependencies`]'s
//! `resolve_depends_on`/`is_edge_met`, [`execute::execute_step`]'s
//! `resolve_engine`) and its [`integrate::HoldSource`] to the permissive,
//! always-proceed shape (`NeverHeld`, no declared dependencies, every edge
//! already met, every block runs on `EngineKind::Flow`) — the same
//! "behavior-stable built-in default" discipline CLAUDE.md standing rule 6
//! requires of every knob. Each is overridable via a `with_*` builder,
//! mirroring `sdlc_flow::setup::SetupWorktreeNode::with_registry`'s
//! established convention, so a caller wiring this node against the real
//! corpus graph (or a test) supplies its own resolvers without touching this
//! module.
//!
//! # Abort stops the chain BETWEEN steps, not mid-step
//!
//! [`OrchestrationRunNode::with_cancellation_token`] mirrors
//! `TerminalAwaitNode::with_cancellation_token`: the token is taken through
//! this node's OWN builder (never read from the runner's between-node
//! check alone), threaded across the `spawn_blocking` boundary, and
//! checked by [`integrate::integrate_chain`] at the top of every step and
//! while parked in [`integrate::wait_for_clearance`]. A cancel win stops
//! the chain and returns the outcomes already integrated as `Ok` —
//! cancellation is a pause point, not a failure. **It cannot interrupt a
//! step already in flight**: a block whose `SDLC_FLOW` run has already
//! started runs to completion regardless, because there is no child run
//! id yet to thread a cancellation into (see [`integrate::integrate_chain`]'s
//! own doc). An operator issuing an abort stops the *next* step, not the
//! current one.
//!
//! # Dispatch steps stay inside the single node (`EN.12.E` Task 4)
//!
//! A [`chain::ChainStep`] whose `kind` is [`chain::StepKind::Dispatch`] runs
//! [`super::dispatch::execute_dispatch_step`] instead of
//! [`execute::execute_step`]'s SDLC engine path — but that routing decision
//! happens *inside* [`integrate::integrate_chain`]'s per-step loop, not as a
//! new graph node here. The declared shape stays exactly what the module
//! doc's ASCII diagram says (one node, `OrchestrationRunNode`, start and
//! terminal) for every named policy profile ([`baseline`], [`cheap_fast`],
//! [`thorough`]) and for a chain that mixes `block`/`dispatch` steps alike —
//! [`schema`]/[`registry`] take no policy or chain-shape input at all, so
//! there is no combination of profile, per-run event override, or step
//! `kind` that could ever change the declared node set (CLAUDE.md standing
//! rule 6's "keep the shape invariant across settings"; see the existing
//! `every_named_profile_leaves_the_declared_node_set_identical` test below).
//! A dispatch step is a no-op path *within* `OrchestrationRunNode::process`
//! for a chain that has none, never a conditional rewire of the graph.
//!
//! A cancelled chain is never mistaken for a failed or a completed one:
//! [`OrchestrationRunNode::process`] stamps `ctx.nodes[NODE_NAME]["cancellation"]`
//! with whether the run was cancelled and, if so, at which step index of
//! how many — under the same `"cancellation"` key `crate::cancellation::stamp_cancelled`
//! uses at the framework level (`ctx.metadata`), which is also stamped for
//! the same event rather than left to the between-node check alone. Every
//! step already integrated before the cancel keeps its `lane-log.jsonl`
//! line; nothing is rolled back.

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use uuid::Uuid;

use engine_contract::TaskContext;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::cancellation::{stamp_cancelled, CancellationToken};
use crate::node::{Node, NodeError, NodeRegistry};
use crate::policy::{read_harness_policy_defaults_from, resolve_profile_from, PolicyConfigSource};
use crate::repo_registry::RepoRegistry;
use crate::schema::{NodeConfig, WorkflowSchema};
use crate::workflow::Workflow;

use super::chain::{resolve_explicit_chain, resolve_lane_chain, ChainStep};
use super::execute::{default_flow_runner, EngineKind, FlowRunner};
use super::gates::{AdmissionGate, DependencyEdge};
use super::integrate::{integrate_chain, resolve_roadmap_dir, HoldSource, NeverHeld, StepProgress};

/// The registered workflow type string, used both to register the workflow
/// (`engine-serve`, this task) and as `WorkflowSchema::workflow_type`.
pub const WORKFLOW_TYPE: &str = "ORCHESTRATION";

/// The sole node's identity — both `Node::name()` and the schema/registry
/// key.
pub const NODE_NAME: &str = "OrchestrationRunNode";

/// The `harness.json` section key this workflow's policy/profiles live
/// under (`orchestration.policy` / `orchestration.profiles`).
const WORKFLOW_KEY: &str = "orchestration";

// ── Policy ───────────────────────────────────────────────────────────────

/// The fully-resolved, per-run ORCHESTRATION policy: the merge of built-in
/// defaults, `harness.json`'s `orchestration.policy` defaults, a named
/// `profile`, and any per-run event override, high->low precedence in that
/// order (`crate::policy::resolve`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrchestrationPolicy {
    /// How often [`integrate::wait_for_clearance`] re-polls an operator
    /// hold while the chain is paused.
    pub hold_poll_interval_ms: u64,
    /// The row-3 fallback [`execute::resolve_isolation`] consults for any
    /// repo that is neither `base-template` (always `true`) nor the brain
    /// root (always `false`) — those two rows are external contracts and
    /// are NOT reachable through this knob, whatever it is set to. Defaults
    /// to `true`.
    ///
    /// **Deliberate exception to CLAUDE.md standing rule 6's
    /// behavior-stability clause.** Rule 6 governs *adding* a knob: a new
    /// knob must not change what an existing run does. This is not that —
    /// it is an existing knob's default changed on purpose, because
    /// in-place execution was never a cost/quality trade to begin with; it
    /// was a correctness hazard whose cheapness depended on an assumption
    /// (nothing else is using this checkout) that this environment
    /// routinely violates. Measured 2026-09-02: an `ORCHESTRATION` dispatch
    /// with no `policy` override ran in-place while a concurrent session
    /// checked out a different branch in the same tree mid-run, silently
    /// stealing `HEAD` and landing three of the orchestrated run's commits
    /// on the other lane's branch. Do not "restore" this to `false` as a
    /// rule-6 fix — see `planning/blocks/EN.ticket.orchestration-worktree-by-default.json`.
    /// An operator who knowingly owns the checkout exclusively can still
    /// pass `"policy": {"default_use_worktree": false}` to opt back into
    /// in-place execution — the knob itself is unchanged, only which way
    /// the unstated case falls.
    pub default_use_worktree: bool,
    /// EN.11.L Task 3: the TOTAL budget [`integrate::wait_for_clearance`]
    /// allows a single operator hold to consume before it fails the chain
    /// loudly with [`integrate::IntegrateError::HoldDeadlineExceeded`].
    /// `None` (the built-in default) preserves the pre-Task-2 behavior
    /// exactly — an unbounded wait — so adding this knob does not change
    /// what an existing run does, per CLAUDE.md standing rule 6.
    pub hold_deadline_ms: Option<u64>,
    /// Whether a `SDLC_FLOW` invocation this chain dispatches should open a
    /// real PR (`SdlcFlowEventSchema::auto_pr`, which
    /// [`super::execute::sdlc_flow_event`] seeds from this resolved value).
    /// Defaults to `true` — behavior-stable per CLAUDE.md standing rule 6:
    /// this is a brand-new knob, so adding it must not change what any
    /// existing run does. `false` is exactly what a rehearsal, a fixture
    /// run, or a sandboxed chain without a real GitHub remote wants —
    /// `PullRequestNode` short-circuits cleanly (`pr.rs`'s
    /// `auto_pr_false_short_circuits_without_calling_runner`) without
    /// shelling out to `gh` at all.
    pub default_auto_pr: bool,
}

impl Default for OrchestrationPolicy {
    /// Poll every 2s while held, isolate every ordinary repo into its own
    /// worktree by default (see the doc comment on `default_use_worktree`
    /// for why this is a correctness precondition, not the pre-Task-2
    /// behavior), never time out a hold (`hold_deadline_ms: None`), and let
    /// every dispatched block open its PR (`default_auto_pr: true`) — the
    /// pre-existing, unchanged behavior of every run before this knob
    /// existed.
    fn default() -> Self {
        Self {
            hold_poll_interval_ms: 2_000,
            default_use_worktree: true,
            hold_deadline_ms: None,
            default_auto_pr: true,
        }
    }
}

/// All-optional mirror of [`OrchestrationPolicy`] used by the override
/// layers.
///
/// `hold_deadline_ms` is itself an `Option<u64>` in the resolved policy
/// (`None` = no deadline), so its override field is the nested
/// `Option<Option<u64>>` [`crate::policy::merge_opt`] already handles
/// generically: `None` here means "not overridden by this layer" (fall
/// through), `Some(None)` means "override to no deadline", and
/// `Some(Some(ms))` pins an explicit deadline.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialOrchestrationPolicy {
    pub hold_poll_interval_ms: Option<u64>,
    pub default_use_worktree: Option<bool>,
    pub hold_deadline_ms: Option<Option<u64>>,
    pub default_auto_pr: Option<bool>,
}

impl crate::policy::Policy for OrchestrationPolicy {
    type Partial = PartialOrchestrationPolicy;

    fn apply(self, over: &PartialOrchestrationPolicy) -> Self {
        OrchestrationPolicy {
            hold_poll_interval_ms: crate::policy::merge_opt(
                self.hold_poll_interval_ms,
                over.hold_poll_interval_ms,
            ),
            default_use_worktree: crate::policy::merge_opt(
                self.default_use_worktree,
                over.default_use_worktree,
            ),
            hold_deadline_ms: crate::policy::merge_opt(
                self.hold_deadline_ms,
                over.hold_deadline_ms,
            ),
            default_auto_pr: crate::policy::merge_opt(self.default_auto_pr, over.default_auto_pr),
        }
    }
}

/// The explicit control profile: poll every 2s, matching
/// [`OrchestrationPolicy::default`] exactly. Spelled out explicitly (rather
/// than left all-`None`) so selecting `profile: "baseline"` is a legible,
/// self-documenting no-op against the built-in default.
#[must_use]
pub fn baseline() -> PartialOrchestrationPolicy {
    PartialOrchestrationPolicy {
        hold_poll_interval_ms: Some(2_000),
        // Restates the built-in default verbatim, per its own contract —
        // isolated by default. See `default_use_worktree`'s doc comment
        // for why this is a deliberate exception to CLAUDE.md standing
        // rule 6's behavior-stability clause.
        default_use_worktree: Some(true),
        // Restates the built-in default verbatim (no deadline) — baseline's
        // no-op contract, per EN.11.L Task 3.
        hold_deadline_ms: Some(None),
        // Restates the built-in default verbatim — every dispatched block
        // opens its PR, baseline's no-op contract.
        default_auto_pr: Some(true),
    }
}

/// Cheapest/fastest profile: poll far less often — fewer wake-ups, at the
/// cost of noticing an operator clearance later.
///
/// Still isolates by default. Cheapness on this profile applies to poll
/// intervals and hold deadlines — the axes where getting it wrong costs a
/// slightly later reaction — never to sharing a working tree with other
/// processes, where getting it wrong costs someone else's commits. See
/// `default_use_worktree`'s doc comment: this is a deliberate exception to
/// CLAUDE.md standing rule 6's behavior-stability clause, not a knob this
/// profile is free to relax back to the pre-fix default.
///
/// EN.11.L Task 3: a bounded 15-minute hold deadline — the cost floor this
/// profile is already tuned for extends to holds too: a lane parked on an
/// unanswered 3am hold burns a blocking-pool thread and a lane-log slot for
/// as long as it waits, so "cheap" means failing that loudly and fast
/// rather than tying it up indefinitely.
#[must_use]
pub fn cheap_fast() -> PartialOrchestrationPolicy {
    PartialOrchestrationPolicy {
        hold_poll_interval_ms: Some(10_000),
        default_use_worktree: Some(true),
        hold_deadline_ms: Some(Some(15 * 60 * 1_000)),
        // `gh pr create` is a real external process call plus a review
        // ceremony — the same reasoning that already justifies this
        // profile's longer poll interval and bounded hold deadline. A cost
        // floor has no business paying for either.
        default_auto_pr: Some(false),
    }
}

/// Highest-responsiveness profile: poll far more often — a cleared hold is
/// noticed almost immediately, at the cost of more wake-ups. Also
/// quarantines every ordinary repo into its own worktree, the highest-safety
/// isolation option.
///
/// EN.11.L Task 3: a generous 24-hour hold deadline — long enough to survive
/// a full off-hours cycle without falsely declaring an operator absent, but
/// still bounded: "thorough" means giving a run every reasonable chance to
/// succeed, not literally forever.
#[must_use]
pub fn thorough() -> PartialOrchestrationPolicy {
    PartialOrchestrationPolicy {
        hold_poll_interval_ms: Some(500),
        default_use_worktree: Some(true),
        hold_deadline_ms: Some(Some(24 * 60 * 60 * 1_000)),
        default_auto_pr: Some(true),
    }
}

/// Resolve a built-in profile bundle by its kebab-case name. Returns `None`
/// for any name that isn't one of the three canonical profiles.
#[must_use]
pub fn profile_by_name(name: &str) -> Option<PartialOrchestrationPolicy> {
    match name {
        "baseline" => Some(baseline()),
        "cheap-fast" => Some(cheap_fast()),
        "thorough" => Some(thorough()),
        _ => None,
    }
}

/// Resolve the four policy layers for `ctx`'s inbound event against
/// `source`: the event's `policy` override, its `profile` bundle, `source`'s
/// `orchestration.policy` defaults, and the built-in default.
pub fn resolve_policy_for_run_from(
    ctx: &TaskContext,
    source: &PolicyConfigSource,
) -> Result<OrchestrationPolicy, NodeError> {
    let event = parse_event(ctx)?;
    let harness_defaults =
        read_harness_policy_defaults_from::<PartialOrchestrationPolicy>(source, WORKFLOW_KEY)?;
    let profile = resolve_profile_from(
        event.profile.as_deref(),
        source,
        WORKFLOW_KEY,
        profile_by_name,
    )?;
    Ok(crate::policy::resolve(
        OrchestrationPolicy::default(),
        harness_defaults.as_ref(),
        profile.as_ref(),
        event.policy.as_ref(),
    ))
}

// ── Event schema ─────────────────────────────────────────────────────────

/// One explicit `(repo, block_id)` entry in an `event.blocks` chain — see
/// [`chain::resolve_explicit_chain`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockRef {
    pub repo: String,
    pub block_id: String,
}

/// The inbound `ORCHESTRATION` event: either an explicit `blocks` list or a
/// `(roadmap, lane)` pair to resolve via `planning/lane-segments.json`
/// (never both — [`parse_event`]'s caller decides which by which fields are
/// present, `blocks` taking precedence when both are somehow given, mirroring
/// [`chain`]'s own "explicit bypasses the lane file entirely" rule).
///
/// `roadmap_slug` names the roadmap directory
/// ([`integrate::resolve_roadmap_dir`]'s two-location rule) the lane-log
/// lives under; it defaults to `roadmap` when omitted, since the common case
/// is that the lane's roadmap slug and the roadmap directory slug are the
/// same string.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OrchestrationEventSchema {
    /// The brain root: contains `brain.toml` (read by [`RepoRegistry`]) and
    /// `planning/` (read for `lane-segments.json` and the roadmap dir).
    pub brain_root: PathBuf,
    pub roadmap: Option<String>,
    pub lane: Option<String>,
    pub blocks: Option<Vec<BlockRef>>,
    pub roadmap_slug: Option<String>,
    pub policy: Option<PartialOrchestrationPolicy>,
    pub profile: Option<String>,
    /// The campaign this run should rejoin, so a resumed or
    /// operator-restarted chain keeps the SAME campaign identity rather
    /// than minting a second one (`EN.11.E` task 3). Parsed as a string on
    /// the wire (not a native `Uuid`) so a malformed value fails LOUDLY
    /// with a field-naming [`NodeError`] in [`OrchestrationRunNode::process`]
    /// instead of `serde`'s generic deserialize error — a silently-new
    /// campaign is indistinguishable from a correctly-resumed one and is
    /// exactly the confident-wrong result CLAUDE.md's standing rules call
    /// out. `None` (the default) mints a fresh [`Uuid::new_v4`], unchanged
    /// from task 2's placeholder behavior.
    pub campaign_id: Option<String>,
}

fn parse_event(ctx: &TaskContext) -> Result<OrchestrationEventSchema, NodeError> {
    serde_json::from_value(ctx.event.clone())
        .map_err(|err| NodeError::new(format!("invalid ORCHESTRATION event: {err}")))
}

/// Resolve an ORCHESTRATION event's campaign id: an explicit value on the
/// event wins (so a resumed/operator-restarted chain rejoins the SAME
/// campaign), a present-but-unparsable value fails loudly rather than
/// silently minting a fresh one, and `None` mints a fresh [`Uuid::new_v4`].
///
/// Exposed as its own function (not just inlined into `process` below) so
/// `engine-serve`'s `register_orchestration_with_registry` factory can
/// resolve the SAME id up front — before the workflow this node lives in
/// ever runs — and register it (and this run's `CancellationToken`) for
/// `POST /campaigns/{id}/abort` to find. Without a single shared resolver,
/// the factory and this node's own independent resolution would mint two
/// DIFFERENT ids for an event with no explicit `campaign_id`, and an abort
/// against the id the factory registered would never match the id this
/// node actually stamps into its output.
pub fn resolve_campaign_id(raw: Option<&str>) -> Result<Uuid, NodeError> {
    match raw {
        Some(raw) => Uuid::parse_str(raw).map_err(|err| {
            NodeError::new(format!(
                "invalid `campaign_id` on ORCHESTRATION event: {err}"
            ))
        }),
        None => Ok(Uuid::new_v4()),
    }
}

// ── The node ─────────────────────────────────────────────────────────────

type DependsOnFn = Arc<dyn Fn(&str, &str) -> Vec<DependencyEdge> + Send + Sync>;
type EdgeMetFn = Arc<dyn Fn(&str, &str) -> bool + Send + Sync>;
type EngineFn = Arc<dyn Fn(&str, &str) -> EngineKind + Send + Sync>;
type BlockOpenFn = Arc<dyn Fn(&str) -> bool + Send + Sync>;
/// A per-step observer, called once per completed step — see
/// [`OrchestrationRunNode::with_step_observer`] and
/// [`integrate::StepProgress`].
type StepObserverArc = Arc<dyn Fn(&StepProgress) + Send + Sync>;

/// The sole node in the `ORCHESTRATION` graph: resolves a lane chain, then
/// drives it end to end via [`integrate::integrate_chain`] — dependency
/// gate, admission gate, operator-hold pause/resume, `SDLC_FLOW` invocation
/// per block (cwd-scoped to that block's repo), state-write verification,
/// and exactly one `lane-log.jsonl` line per integrated block.
///
/// **Abort stops the chain BETWEEN steps, not mid-step** — see
/// [`Self::with_cancellation_token`] and the module doc's "Abort stops the
/// chain BETWEEN steps, not mid-step" section.
pub struct OrchestrationRunNode {
    resolve_depends_on: DependsOnFn,
    is_edge_met: EdgeMetFn,
    resolve_engine: EngineFn,
    is_block_open: BlockOpenFn,
    hold_source: Arc<dyn HoldSource>,
    admission: AdmissionGate,
    /// `None` (the default) builds a fresh [`default_flow_runner`] per run
    /// from the event's own resolved [`RepoRegistry`] — tests override this
    /// with a recording double via [`Self::with_run_flow`].
    run_flow: Option<FlowRunner>,
    /// Taken through this node's OWN builder, never read from the runner's
    /// between-node check alone — mirrors
    /// `TerminalAwaitNode::with_cancellation_token`. `None` (the default) is
    /// behavior-stable: no token, no cancellation check, identical output to
    /// today. See [`Self::with_cancellation_token`].
    cancellation_token: Option<CancellationToken>,
    /// Called exactly once per completed step by [`integrate::integrate_chain`]
    /// — see [`Self::with_step_observer`]. Defaults to a no-op, so an
    /// un-injected run emits nothing and behaves exactly as before this
    /// seam existed (CLAUDE.md standing rule 6).
    step_observer: StepObserverArc,
    /// A pre-resolved campaign id, supplied by a caller that already ran
    /// [`resolve_campaign_id`] itself (e.g. to register this run's
    /// cancellation token for campaign-scoped abort before the workflow
    /// runs). `None` (the default) is behavior-stable: `process` resolves
    /// the id from the event exactly as it did before this field existed.
    /// See [`Self::with_campaign_id`].
    campaign_id: Option<Uuid>,
}

impl fmt::Debug for OrchestrationRunNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OrchestrationRunNode")
            .finish_non_exhaustive()
    }
}

impl OrchestrationRunNode {
    /// A permissive, always-proceed default: no declared dependencies,
    /// every edge already met, every block runs on [`EngineKind::Flow`],
    /// nothing is ever held, and a fresh [`default_flow_runner`] is built
    /// per run. Behavior-stable per CLAUDE.md standing rule 6 — a run
    /// wired with no overrides at all simply executes the chain in order.
    #[must_use]
    pub fn new() -> Self {
        Self {
            resolve_depends_on: Arc::new(|_repo, _block_id| Vec::new()),
            is_edge_met: Arc::new(|_repo, _block_id| true),
            resolve_engine: Arc::new(|_repo, _block_id| EngineKind::Flow),
            is_block_open: Arc::new(|_held_until| false),
            hold_source: Arc::new(NeverHeld),
            admission: AdmissionGate::with_default_policy(),
            run_flow: None,
            cancellation_token: None,
            step_observer: Arc::new(|_progress: &StepProgress| {}),
            campaign_id: None,
        }
    }

    #[must_use]
    pub fn with_resolve_depends_on(mut self, f: DependsOnFn) -> Self {
        self.resolve_depends_on = f;
        self
    }

    #[must_use]
    pub fn with_is_edge_met(mut self, f: EdgeMetFn) -> Self {
        self.is_edge_met = f;
        self
    }

    #[must_use]
    pub fn with_resolve_engine(mut self, f: EngineFn) -> Self {
        self.resolve_engine = f;
        self
    }

    #[must_use]
    pub fn with_is_block_open(mut self, f: BlockOpenFn) -> Self {
        self.is_block_open = f;
        self
    }

    #[must_use]
    pub fn with_hold_source(mut self, hold_source: Arc<dyn HoldSource>) -> Self {
        self.hold_source = hold_source;
        self
    }

    #[must_use]
    pub fn with_admission(mut self, admission: AdmissionGate) -> Self {
        self.admission = admission;
        self
    }

    #[must_use]
    pub fn with_run_flow(mut self, run_flow: FlowRunner) -> Self {
        self.run_flow = Some(run_flow);
        self
    }

    /// Attach a [`CancellationToken`], checked by
    /// [`integrate::integrate_chain`] at the top of every step and raced
    /// against [`integrate::wait_for_clearance`]'s sleep — mirroring
    /// `TerminalAwaitNode::with_cancellation_token`. A cancel win stops the
    /// chain BETWEEN steps and returns `Ok` with whatever was already
    /// integrated; it CANNOT interrupt a step already in flight (no child
    /// run id exists yet to cancel into — see the module doc). With no
    /// token attached (the default), behavior is unchanged from before this
    /// builder existed.
    #[must_use]
    pub fn with_cancellation_token(mut self, token: CancellationToken) -> Self {
        self.cancellation_token = Some(token);
        self
    }

    /// Attach a per-step observer, called by
    /// [`integrate::integrate_chain`] exactly once per completed step —
    /// after that step's `lane-log.jsonl` line is appended. `Node::process`
    /// has no access to the framework's own `on_progress` (it only fires at
    /// node boundaries, and this workflow is a single node), so this
    /// builder is the only way per-step progress reaches a caller. With no
    /// observer attached (the default), behavior is unchanged: no
    /// emissions, no other side effect. See [`integrate::StepProgress`] for
    /// the payload and its 1-based index convention.
    #[must_use]
    pub fn with_step_observer(mut self, observer: StepObserverArc) -> Self {
        self.step_observer = observer;
        self
    }

    /// Supply a campaign id already resolved by the caller (via
    /// [`resolve_campaign_id`]), overriding `process`'s own resolution of
    /// the event's `campaign_id` field. Intended for a factory that must
    /// know this run's campaign id BEFORE the workflow runs, so it can
    /// register this node's [`Self::with_cancellation_token`] token for
    /// campaign-scoped abort. With no override (the default), behavior is
    /// unchanged: `process` resolves the id from the event itself.
    #[must_use]
    pub fn with_campaign_id(mut self, campaign_id: Uuid) -> Self {
        self.campaign_id = Some(campaign_id);
        self
    }
}

impl Default for OrchestrationRunNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for OrchestrationRunNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let event = parse_event(&ctx)?;
        let policy = resolve_policy_for_run_from(
            &ctx,
            &PolicyConfigSource::Worktree(event.brain_root.clone()),
        )?;

        let repo_registry = Arc::new(
            RepoRegistry::from_brain_root(&event.brain_root)
                .map_err(|err| NodeError::new(format!("repo registry: {err}")))?,
        );

        // The event's real lane for a roadmap+lane chain — threaded into
        // every appended lane-log line. An explicit `blocks` chain has no
        // lane by construction (there is no lane file to have parsed one
        // from): `None` here means `integrate_chain` falls back to each
        // step's own repo slug, matching the fleet's hand-written
        // single-repo lines where `lane == repo`.
        let mut resolved_lane: Option<String> = None;

        let chain: Vec<ChainStep> = if let Some(blocks) = &event.blocks {
            resolve_explicit_chain(
                blocks
                    .iter()
                    .map(|b| (b.repo.clone(), b.block_id.clone()))
                    .collect(),
            )
        } else {
            let roadmap = event.roadmap.clone().ok_or_else(|| {
                NodeError::new("ORCHESTRATION event needs either `blocks` or `roadmap`+`lane`")
            })?;
            let lane = event.lane.clone().ok_or_else(|| {
                NodeError::new("ORCHESTRATION event needs `lane` alongside `roadmap`")
            })?;
            resolved_lane = Some(lane.clone());
            let lane_segments_path = event.brain_root.join("planning").join("lane-segments.json");
            let is_block_open = self.is_block_open.clone();
            resolve_lane_chain(&lane_segments_path, &roadmap, &lane, &move |token| {
                is_block_open(token)
            })
            .map_err(|err| NodeError::new(err.to_string()))?
        };

        // Captured before `chain` is moved into the `spawn_blocking`
        // closure below — the only way to know, after the fact, how many
        // steps the chain was ever going to run, which is what turns
        // "fewer outcomes than steps" into "cancelled at step k of n"
        // rather than merely "a short chain".
        let total_steps = chain.len();

        // Resolution order mirrors `roadmap_slug`/`roadmap` above: an
        // explicit value on the event wins (so a resumed/operator-restarted
        // chain rejoins the SAME campaign), else mint a fresh v4. Unlike
        // that fallback, a *present but unparsable* value must fail loudly
        // rather than silently fall through — see the field's own doc on
        // `OrchestrationEventSchema::campaign_id`.
        let campaign_id = match self.campaign_id {
            Some(id) => id,
            None => resolve_campaign_id(event.campaign_id.as_deref())?,
        };

        let roadmap_slug = event
            .roadmap_slug
            .clone()
            .or_else(|| event.roadmap.clone())
            .ok_or_else(|| {
                NodeError::new(
                    "ORCHESTRATION event needs `roadmap_slug` (or `roadmap`) to resolve the \
                     lane-log directory",
                )
            })?;
        let planning_root = event.brain_root.join("planning");
        let roadmap_dir = resolve_roadmap_dir(&planning_root, &roadmap_slug)
            .map_err(|err| NodeError::new(err.to_string()))?;

        let run_flow = self
            .run_flow
            .clone()
            .unwrap_or_else(|| default_flow_runner(repo_registry.clone()));
        let poll_interval = Duration::from_millis(policy.hold_poll_interval_ms);
        // EN.11.L Task 3: `hold_deadline_ms` resolves through the same four
        // policy layers as `poll_interval` above, rather than the previous
        // hardcoded `None` at the `integrate_chain` call site below. `None`
        // (the built-in default) still means "no deadline" — unchanged
        // pre-Task-2 behavior.
        let hold_deadline = policy.hold_deadline_ms.map(Duration::from_millis);
        // The row-3 fallback `execute::resolve_isolation` consults for any
        // repo that isn't one of the two non-negotiable rows. `bool` is
        // `Copy`, so this is captured into the `spawn_blocking` closure by
        // value like `poll_interval` above — no `Arc`/clone needed.
        let default_use_worktree = policy.default_use_worktree;

        let resolve_depends_on = self.resolve_depends_on.clone();
        let is_edge_met = self.is_edge_met.clone();
        let resolve_engine = self.resolve_engine.clone();
        let admission = self.admission.clone();
        let hold_source = self.hold_source.clone();
        // Cloned before the `spawn_blocking` closure like every other owned
        // value here — `CancellationToken` is cheap to clone (an `Arc`
        // inside) and `Send + 'static`, so it crosses the boundary the same
        // way the rest of this node's seams do. See `with_cancellation_token`.
        let cancellation_token = self.cancellation_token.clone();
        // A second clone kept OUTSIDE the `spawn_blocking` closure — the
        // first is moved into the closure below and consumed there, so
        // this is the only way `process` can still ask "was this run
        // cancelled?" once `integrate_chain` returns. Cheap: `CancellationToken`
        // is an `Arc` inside.
        let cancellation_token_for_stamp = cancellation_token.clone();
        // Cloned before the `spawn_blocking` closure like every other owned
        // seam here — `Arc` clone, `Send + Sync + 'static`.
        let step_observer = self.step_observer.clone();

        // `execute::FlowFuture` is deliberately not `Send` (see its own doc
        // comment: `Workflow::run`'s `OnProgress` callback is not `Send`),
        // so `integrate_chain`'s future is not `Send` either — but `Node`'s
        // `#[async_trait]` requires `process`'s future to be `Send`. Bridge
        // the two by running `integrate_chain` to completion on a dedicated
        // blocking-pool thread, inside its own fresh current-thread runtime:
        // `spawn_blocking`'s *closure* only needs to be `Send + 'static`
        // (every value moved into it below is owned and `Send`), and
        // nothing inside that closure's own `block_on` needs to cross a
        // `Send` boundary at all, since it never leaves that one thread.
        // The returned `JoinHandle<Result<..>>` is `Send` regardless of the
        // task it ran, which is exactly the adapter this seam needs.
        // `EN.11.I` task 2: `tracing`'s span context is thread-local and
        // does NOT cross a `spawn_blocking` boundary by itself — the
        // closure below runs on a fresh blocking-pool OS thread whose span
        // stack starts empty, so anything `integrate_chain` (or a step it
        // drives) logs there would silently carry no `run_id`/`campaign_id`
        // fields despite `OrchestrationRunNode::process` itself running
        // inside `Workflow::walk`'s instrumented span. Fix: capture BOTH
        // halves of the calling thread's tracing context before crossing —
        // the current span (carries the recorded `run_id`/`campaign_id`
        // fields) and the current dispatcher (the subscriber events are
        // actually delivered to) — and re-establish both on the blocking
        // thread. In production this dispatcher forwarding is usually a
        // no-op (the host installs one global subscriber via
        // `engine_serve::init_tracing`, which every thread already sees),
        // but it also makes this call site correct under a *thread-local*
        // test/tool subscriber (`tracing::subscriber::set_default`), which
        // a global default alone would not reach.
        let current_span = tracing::Span::current();
        let current_dispatch = tracing::dispatcher::get_default(|d| d.clone());
        let outcomes = tokio::task::spawn_blocking(move || {
            tracing::dispatcher::with_default(&current_dispatch, || {
                let _span_guard = current_span.enter();
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|err| {
                        NodeError::new(format!("failed to start orchestration runtime: {err}"))
                    })?;
                rt.block_on(integrate_chain(
                    &chain,
                    &move |repo, block_id| resolve_depends_on(repo, block_id),
                    &move |repo, block_id| is_edge_met(repo, block_id),
                    &admission,
                    hold_source.as_ref(),
                    poll_interval,
                    hold_deadline,
                    cancellation_token.as_ref(),
                    // `EN.11.F` task 4 adds the campaign-boundary ceiling
                    // check to `integrate_chain`; wiring a real cap through
                    // `OrchestrationRunNode`'s own policy surface is not
                    // this task's job — `None` here is behavior-identical
                    // to before this parameter existed (no ceiling).
                    None,
                    &move |repo, block_id| resolve_engine(repo, block_id),
                    &repo_registry,
                    &run_flow,
                    &roadmap_dir,
                    resolved_lane.as_deref(),
                    step_observer.as_ref(),
                    default_use_worktree,
                    // The resolved, event-overridable campaign id (EN.11.E
                    // task 3) — replaces task 2's freshly-minted placeholder.
                    campaign_id,
                ))
                .map_err(|err| NodeError::new(err.to_string()))
            })
        })
        .await
        .map_err(|err| NodeError::new(format!("orchestration task panicked: {err}")))??;

        // A cancel win is only real if it actually cut the chain short —
        // `integrate_chain` can return `outcomes.len() == total_steps` even
        // with a cancelled token if the cancel landed after the last step's
        // own top-of-loop check but the loop had already finished (the
        // token was cancelled "too late to matter"). In that case the run
        // completed and must read as COMPLETED, not CANCELLED, even though
        // the token itself is in the cancelled state.
        let cancelled_at_step = if outcomes.len() < total_steps
            && cancellation_token_for_stamp
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
        {
            Some(outcomes.len())
        } else {
            None
        };

        let mut ctx = ctx;
        // Node-level record of the same event `crate::cancellation::stamp_cancelled`
        // marks at the framework/`ctx.metadata` level (`workflow.rs`'s
        // between-node check) — named under the same `"cancellation"` key
        // rather than a rival marker, but scoped here to this node's own
        // result so a reader with only `ctx.nodes[NODE_NAME]` can already
        // tell COMPLETED (`cancelled: false`, `steps_integrated == total`)
        // from CANCELLED (`cancelled: true`, stopped at `at_step` of
        // `total_steps`) from FAILED (this branch is never reached — a
        // failed step returns `Err` above and there is no result to stamp).
        // Every step already integrated before the cancel keeps its
        // `lane-log.jsonl` line; nothing here rolls anything back.
        let cancellation = match cancelled_at_step {
            Some(at_step) => {
                // Also stamp the framework-level marker so a caller reading
                // only `ctx.metadata` (e.g. `RunTelemetry`) sees the same
                // fact `Workflow::walk`'s own between-node cancellation
                // check would have recorded, had this graph had more than
                // one node to check between.
                stamp_cancelled(&mut ctx.metadata);
                json!({
                    "cancelled": true,
                    "at_step": at_step,
                    "total_steps": total_steps,
                })
            }
            None => json!({ "cancelled": false }),
        };

        ctx.nodes.insert(
            NODE_NAME.to_string(),
            json!({
                "steps_integrated": outcomes.len(),
                "blocks": outcomes
                    .iter()
                    .map(|o| json!({
                        "repo": o.repo,
                        "block_id": o.block_id,
                        "use_worktree": o.use_worktree,
                    }))
                    .collect::<Vec<_>>(),
                "policy": {
                    "hold_poll_interval_ms": policy.hold_poll_interval_ms,
                    "default_use_worktree": policy.default_use_worktree,
                    "hold_deadline_ms": policy.hold_deadline_ms,
                },
                "cancellation": cancellation,
                // The addressable subject `GET /campaigns/{id}` (task 5)
                // answers from — this ORCHESTRATION run is itself an
                // ordinary HTTP-triggered run in `LiveStateStore`, so
                // stamping the campaign's members here is what lets that
                // route work without recording every in-process child run
                // and without any relational table (EN.11.E task 3).
                // `cost_usd` stays `Option<f64>` end to end: a step that
                // reported no cost is written as JSON `null`, never `0`.
                "campaign_id": campaign_id,
                "campaign_members": outcomes
                    .iter()
                    .map(|o| json!({
                        "repo": o.repo,
                        "block_id": o.block_id,
                        "use_worktree": o.use_worktree,
                        "cost_usd": o.cost_usd,
                        "total_tokens": o.total_tokens,
                    }))
                    .collect::<Vec<_>>(),
            }),
        );
        Ok(ctx)
    }

    fn name(&self) -> &str {
        NODE_NAME
    }
}

// ── Graph assembly ──────────────────────────────────────────────────────

/// Build the declared `WorkflowSchema` for the `ORCHESTRATION` workflow: a
/// single node, both start and terminal, with no forward connection.
#[must_use]
pub fn schema() -> WorkflowSchema {
    let mut nodes = HashMap::new();
    nodes.insert(NODE_NAME.to_string(), NodeConfig::new(NODE_NAME, vec![]));
    WorkflowSchema::new(WORKFLOW_TYPE, NODE_NAME, nodes)
}

/// Build a fresh `NodeRegistry` with the single node identity in [`schema`]
/// registered, under its permissive default seams
/// ([`OrchestrationRunNode::new`]).
///
/// **This is the UNWIRED default** — every gate seam is a no-op (no declared
/// dependencies, every edge already met, every block open, never held), which
/// is correct for a bare constructor but was, until
/// `EN.ticket.orchestration-production-gates-unwired`, also the *only*
/// registry production ever got. `engine-serve`'s
/// `register_orchestration_with_registry` is the wired entry point: it builds
/// this same node but installs `corpus_gates::CorpusGates`-backed closures
/// (real `planning/state.json` reads via `RepoRegistry`) instead of calling
/// this function. Use [`registry`] directly only in tests that want the
/// permissive default on purpose — the served workflow does not call it.
#[must_use]
pub fn registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(OrchestrationRunNode::new()));
    registry
}

/// Build the runnable `ORCHESTRATION` `Workflow`: [`registry`] paired with
/// [`schema`], constructed via `Workflow::new_validated` so assembly fails
/// loudly if the declared graph is not structurally sound.
///
/// # Panics
/// Panics if the declared graph fails `WorkflowValidator::validate` — this
/// would be a programming error in this module, not a runtime condition
/// callers should recover from.
#[must_use]
pub fn workflow() -> Workflow {
    Workflow::new_validated(registry(), schema())
        .expect("ORCHESTRATION declared graph must pass WorkflowValidator::validate")
}

// ── `DEBRIEF` workflow assembly (`EN.12.G` task 4) ──────────────────────
//
// `DEBRIEF` is a **separate registered workflow**, not a graph node wired
// into `ORCHESTRATION`'s single-node shape above. The operator's stated
// intent (block record `notes`, AMENDMENT 2026-08-29) is a plain `POST
// /events/` trigger carrying nothing but a campaign id, reachable with no
// conductor, no chain, no roadmap and no lane present — so it is dispatched
// exactly like `RECALL` (`EN.12.L` task 2, `recall::graph`): a single node,
// both start and terminal, registered under its own `workflow_type` string
// so an `EN.12.E` `kind: dispatch` chain step can also name it as
// `block_id: "DEBRIEF"`. [`EngineKind`] (two variants: `Task`/`Flow`) is
// untouched by this — a dispatch step never resolves through it at all
// (`dispatch.rs`'s dispatch-step path bypasses `execute::resolve_engine`
// entirely), and this module adds no third variant.
//
// Registering `DEBRIEF` changes nothing about how an *existing* chain's
// steps are resolved: [`resolve_explicit_chain`]/[`resolve_lane_chain`]
// above build exactly one [`ChainStep`] per declared block/lane entry and
// know nothing of `DEBRIEF` at all — see
// `resolve_explicit_chain_never_gains_an_implicit_debrief_step` below.

/// `DEBRIEF`'s registered workflow type string — the wire spelling an
/// `EN.12.E` dispatch step's `block_id` names, and the `POST /events/`
/// body's `workflow_type` a caller (`routine.sh`, once wired on the HQ
/// side — out of scope for this repo) sends. See
/// `crate::workflows::orchestration::debrief`'s module doc for the node's
/// own behaviour and `engine_serve::workflows::register_debrief`'s doc for
/// the full trigger contract (body shape + required header).
pub const DEBRIEF_WORKFLOW_TYPE: &str = "DEBRIEF";

/// Build the declared `WorkflowSchema` for `DEBRIEF`: a single node
/// ([`super::debrief::DebriefNode`], keyed by
/// [`super::debrief::DEBRIEF_NODE_NAME`]), both start and terminal, with no
/// forward connection — mirrors `recall::graph::schema`'s micro-workflow
/// shape.
#[must_use]
pub fn debrief_schema() -> WorkflowSchema {
    let mut nodes = HashMap::new();
    nodes.insert(
        super::debrief::DEBRIEF_NODE_NAME.to_string(),
        NodeConfig::new(super::debrief::DEBRIEF_NODE_NAME, vec![]),
    );
    WorkflowSchema::new(
        DEBRIEF_WORKFLOW_TYPE,
        super::debrief::DEBRIEF_NODE_NAME,
        nodes,
    )
}

/// Build a fresh `NodeRegistry` for `DEBRIEF`: one
/// [`super::debrief::DebriefNode`] wired to `journal_reader`/`transport`,
/// registered under its default `Node::name()` identity
/// ([`super::debrief::DEBRIEF_NODE_NAME`]).
///
/// `journal_sink` is `None` for the production entry point
/// (`engine_serve::workflows::register_debrief`, `EN.12.G` task 4): a
/// `Dispatcher` factory closure runs before `engine-serve`'s `AppState`/
/// `DurableHandle` exist (`Dispatcher::register` happens at process
/// start-up, `DurableHandle` is minted per served request — see
/// `register_orchestration`'s own `StepFanoutContext` doc for the same gap
/// on `ORCHESTRATION`), so there is no live durable-writer seam to wire in
/// at this call site yet. A caller that already holds a
/// [`super::integrate::JournalSinkFn`] (a hermetic test, or a future wiring
/// of that gap) passes `Some(sink)` to get a `DebriefNode` that writes its
/// rendered brief back through it.
#[must_use]
pub fn debrief_registry(
    journal_reader: Arc<dyn super::debrief::JournalReader>,
    transport: Arc<dyn crate::nodes::channel_transport::ChannelTransport>,
    journal_sink: Option<Arc<super::integrate::JournalSinkFn>>,
) -> NodeRegistry {
    let mut node = super::debrief::DebriefNode::new(journal_reader, transport);
    if let Some(sink) = journal_sink {
        node = node.with_journal_sink(sink);
    }
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(node));
    registry
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate::WorkflowValidator;

    #[test]
    fn schema_passes_validation() {
        let schema = schema();
        let registry = registry();

        WorkflowValidator::validate(&registry, &schema).expect("declared graph should validate");
    }

    #[test]
    fn start_node_is_orchestration_run_node() {
        assert_eq!(schema().start_node, NODE_NAME);
    }

    #[test]
    fn workflow_type_is_orchestration() {
        assert_eq!(schema().workflow_type, WORKFLOW_TYPE);
    }

    #[test]
    fn registry_contains_the_single_node() {
        let registry = registry();
        assert!(registry.contains(NODE_NAME));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn declared_graph_has_no_dangling_or_unregistered_identity() {
        let schema = schema();
        let registry = registry();

        for (identity, config) in &schema.nodes {
            assert!(
                registry.contains(identity),
                "declared node '{identity}' is not registered"
            );
            for connection in &config.connections {
                assert!(
                    schema.nodes.contains_key(connection),
                    "'{identity}' declares a connection to unregistered/undeclared '{connection}'"
                );
            }
        }
    }

    #[test]
    fn workflow_builds_without_panicking() {
        let _workflow = workflow();
    }

    // ── Policy ───────────────────────────────────────────────────────────

    #[test]
    fn builtin_default_is_behavior_stable_baseline() {
        assert_eq!(OrchestrationPolicy::default().hold_poll_interval_ms, 2_000);
        // Isolated by default (deliberate exception to standing rule 6 —
        // see the doc comment on `default_use_worktree`): the "behavior
        // stable" name refers to hold_poll_interval_ms and hold_deadline_ms.
        assert!(OrchestrationPolicy::default().default_use_worktree);
        // EN.11.L Task 3: the built-in default must preserve the
        // pre-Task-2 unbounded wait exactly — adding this knob must not
        // change what an existing run does (CLAUDE.md standing rule 6).
        assert_eq!(OrchestrationPolicy::default().hold_deadline_ms, None);
        // This is a brand-new knob (unlike default_use_worktree above):
        // adding it must not change what any existing run does, so the
        // built-in default matches the pre-existing, unconditional-PR
        // behavior.
        assert!(OrchestrationPolicy::default().default_auto_pr);
    }

    #[test]
    fn all_three_profiles_set_default_use_worktree_explicitly() {
        assert_eq!(baseline().default_use_worktree, Some(true));
        assert_eq!(cheap_fast().default_use_worktree, Some(true));
        assert_eq!(thorough().default_use_worktree, Some(true));
    }

    /// Every named profile sets `default_auto_pr` explicitly — a knob
    /// absent from the profile bundles is a knob nobody will find
    /// (CLAUDE.md standing rule 6).
    #[test]
    fn all_three_profiles_set_default_auto_pr_explicitly() {
        assert_eq!(baseline().default_auto_pr, Some(true));
        assert_eq!(cheap_fast().default_auto_pr, Some(false));
        assert_eq!(thorough().default_auto_pr, Some(true));
    }

    #[test]
    fn default_auto_pr_resolves_through_the_policy_layers() {
        let event_override = PartialOrchestrationPolicy {
            default_auto_pr: Some(false),
            ..Default::default()
        };
        // Event override beats the profile.
        let resolved = crate::policy::resolve(
            OrchestrationPolicy::default(),
            None,
            Some(&thorough()),
            Some(&event_override),
        );
        assert!(!resolved.default_auto_pr);

        // With no event override, the profile's value wins.
        let resolved = crate::policy::resolve(
            OrchestrationPolicy::default(),
            None,
            Some(&cheap_fast()),
            None,
        );
        assert!(!resolved.default_auto_pr);

        // With nothing set at all, the behavior-stable built-in default
        // (every block opens its PR) is what resolves.
        let resolved = crate::policy::resolve(OrchestrationPolicy::default(), None, None, None);
        assert!(resolved.default_auto_pr);
    }

    /// EN.11.L Task 3: every named profile explicitly sets
    /// `hold_deadline_ms` — a knob absent from the profile bundles is a
    /// knob nobody will find (standing rule 6).
    #[test]
    fn all_three_profiles_set_hold_deadline_ms_explicitly() {
        // baseline restates the built-in default verbatim: no deadline.
        assert_eq!(baseline().hold_deadline_ms, Some(None));
        assert_eq!(cheap_fast().hold_deadline_ms, Some(Some(15 * 60 * 1_000)));
        assert_eq!(
            thorough().hold_deadline_ms,
            Some(Some(24 * 60 * 60 * 1_000))
        );
    }

    #[test]
    fn hold_deadline_ms_resolves_through_the_policy_layers() {
        let event_override = PartialOrchestrationPolicy {
            hold_deadline_ms: Some(Some(1_234)),
            ..Default::default()
        };
        let resolved = crate::policy::resolve(
            OrchestrationPolicy::default(),
            None,
            Some(&cheap_fast()),
            Some(&event_override),
        );
        // Event override beats the profile.
        assert_eq!(resolved.hold_deadline_ms, Some(1_234));

        // With no event override, the profile's value wins.
        let resolved = crate::policy::resolve(
            OrchestrationPolicy::default(),
            None,
            Some(&cheap_fast()),
            None,
        );
        assert_eq!(resolved.hold_deadline_ms, Some(15 * 60 * 1_000));

        // With nothing set at all, the behavior-stable built-in default
        // (no deadline) is what resolves.
        let resolved = crate::policy::resolve(OrchestrationPolicy::default(), None, None, None);
        assert_eq!(resolved.hold_deadline_ms, None);
    }

    #[test]
    fn profile_by_name_resolves_all_three_canonical_names() {
        assert_eq!(profile_by_name("baseline"), Some(baseline()));
        assert_eq!(profile_by_name("cheap-fast"), Some(cheap_fast()));
        assert_eq!(profile_by_name("thorough"), Some(thorough()));
        assert_eq!(profile_by_name("nonexistent"), None);
    }

    #[test]
    fn baseline_profile_matches_the_builtin_default() {
        let resolved = crate::policy::resolve(
            OrchestrationPolicy::default(),
            None,
            Some(&baseline()),
            None,
        );
        assert_eq!(resolved, OrchestrationPolicy::default());
    }

    #[test]
    fn cheap_fast_polls_less_often_than_baseline() {
        let resolved = crate::policy::resolve(
            OrchestrationPolicy::default(),
            None,
            Some(&cheap_fast()),
            None,
        );
        assert!(
            resolved.hold_poll_interval_ms > OrchestrationPolicy::default().hold_poll_interval_ms
        );
    }

    #[test]
    fn thorough_polls_more_often_than_baseline() {
        let resolved = crate::policy::resolve(
            OrchestrationPolicy::default(),
            None,
            Some(&thorough()),
            None,
        );
        assert!(
            resolved.hold_poll_interval_ms < OrchestrationPolicy::default().hold_poll_interval_ms
        );
    }

    #[test]
    fn event_override_beats_profile() {
        let event_override = PartialOrchestrationPolicy {
            hold_poll_interval_ms: Some(42),
            ..Default::default()
        };
        let resolved = crate::policy::resolve(
            OrchestrationPolicy::default(),
            None,
            Some(&thorough()),
            Some(&event_override),
        );
        assert_eq!(resolved.hold_poll_interval_ms, 42);
    }

    /// Every named profile only ever changes `hold_poll_interval_ms`,
    /// `default_use_worktree`, and (EN.11.L Task 3) `hold_deadline_ms` —
    /// the declared node set ([`registry`]) is identical regardless of
    /// which profile a run selects, since none of these policy knobs
    /// rewires which node runs (CLAUDE.md standing rule 6: "a policy knob
    /// may change a bound or a tier; it must not change... a declared node
    /// set").
    #[test]
    fn every_named_profile_leaves_the_declared_node_set_identical() {
        let default_registry = registry();
        for name in ["baseline", "cheap-fast", "thorough"] {
            let profile = profile_by_name(name).unwrap_or_else(|| panic!("missing profile {name}"));
            let resolved =
                crate::policy::resolve(OrchestrationPolicy::default(), None, Some(&profile), None);
            // Resolving a profile never touches which node identities this
            // workflow registers -- there is no `registry_for_policy` to
            // even call. This assertion documents that invariant directly:
            // the same `registry()` a caller runs the workflow under is
            // used no matter which of these `resolved` values is in force.
            let _ = resolved;
            assert_eq!(registry().len(), default_registry.len());
            assert!(registry().contains(NODE_NAME));
        }
    }

    // ── Event parsing ────────────────────────────────────────────────────

    fn base_ctx(event: serde_json::Value) -> TaskContext {
        TaskContext {
            event,
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        }
    }

    #[test]
    fn parse_event_reads_explicit_blocks() {
        let ctx = base_ctx(json!({
            "brain_root": "/tmp/brain",
            "blocks": [
                { "repo": "repo-a", "block_id": "A.1" },
                { "repo": "repo-b", "block_id": "B.1" }
            ],
            "roadmap_slug": "my-roadmap",
        }));
        let event = parse_event(&ctx).expect("event should parse");
        assert_eq!(event.brain_root, PathBuf::from("/tmp/brain"));
        assert_eq!(event.blocks.as_ref().unwrap().len(), 2);
        assert_eq!(event.roadmap_slug.as_deref(), Some("my-roadmap"));
    }

    #[test]
    fn parse_event_reads_roadmap_and_lane() {
        let ctx = base_ctx(json!({
            "brain_root": "/tmp/brain",
            "roadmap": "r",
            "lane": "l",
        }));
        let event = parse_event(&ctx).expect("event should parse");
        assert_eq!(event.roadmap.as_deref(), Some("r"));
        assert_eq!(event.lane.as_deref(), Some("l"));
        assert!(event.blocks.is_none());
    }

    // ── Node::process — explicit chain, end to end over tempdir fixtures ──

    /// Two tempdir fixture repos wired via a `brain.toml`-backed
    /// `RepoRegistry`, mirroring `execute.rs`'s own test fixture — but this
    /// helper additionally seeds a `done` `sdlc-flow-state.json` for each
    /// block up front (via `run_flow`'s stub writing it), so
    /// `integrate::verify_state_write` and the `lane-log.jsonl` append both
    /// succeed for a `process()`-level exercise of the whole node without
    /// a real `SDLC_FLOW` run.
    fn two_repo_brain_root() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("repo-a")).unwrap();
        std::fs::create_dir_all(dir.path().join("repo-b")).unwrap();
        std::fs::create_dir_all(
            dir.path()
                .join("planning")
                .join("roadmaps")
                .join("my-roadmap"),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("brain.toml"),
            "[[repos]]\nslug = \"repo-a\"\nrepo_path = \"repo-a\"\n\
             [[repos]]\nslug = \"repo-b\"\nrepo_path = \"repo-b\"\n",
        )
        .unwrap();
        dir
    }

    fn write_done_state(repo_path: &std::path::Path, block_id: &str) {
        let dir = repo_path.join("planning").join(block_id).join("sdlc");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("sdlc-flow-state.json"),
            json!({ "status": "done" }).to_string(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn process_drives_an_explicit_two_repo_chain_end_to_end() {
        let dir = two_repo_brain_root();
        write_done_state(&dir.path().join("repo-a"), "A.1");
        write_done_state(&dir.path().join("repo-b"), "B.1");

        let run_flow: FlowRunner = Arc::new(|invocation| {
            Box::pin(async move {
                Ok(TaskContext {
                    event: json!({}),
                    nodes: HashMap::new(),
                    metadata: json!({ "ran": invocation.block_id }),
                    node_runs: HashMap::new(),
                })
            })
        });

        let node = OrchestrationRunNode::new().with_run_flow(run_flow);
        let ctx = base_ctx(json!({
            "brain_root": dir.path(),
            "blocks": [
                { "repo": "repo-a", "block_id": "A.1" },
                { "repo": "repo-b", "block_id": "B.1" }
            ],
            "roadmap_slug": "my-roadmap",
        }));

        let out = node.process(ctx).await.expect("process should succeed");
        let recorded = &out.nodes[NODE_NAME];
        assert_eq!(recorded["steps_integrated"], 2);
        assert_eq!(recorded["policy"]["hold_poll_interval_ms"], 2_000);
        // EN.11.L Task 3: the resolved `hold_deadline_ms` is stamped into
        // `ctx.nodes` alongside the other policy values so telemetry can
        // attribute observed behavior to the setting that caused it.
        assert_eq!(
            recorded["policy"]["hold_deadline_ms"],
            serde_json::Value::Null
        );

        let lane_log = std::fs::read_to_string(
            dir.path()
                .join("planning")
                .join("roadmaps")
                .join("my-roadmap")
                .join("lane-log.jsonl"),
        )
        .expect("lane-log.jsonl should exist");
        let lines: Vec<serde_json::Value> = lane_log
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        // An explicit `blocks` chain has no lane by construction: every
        // appended line's `lane` falls back to that step's own repo slug,
        // asserted explicitly rather than left incidental.
        assert_eq!(lines[0]["lane"], lines[0]["repo"]);
        assert_eq!(lines[1]["lane"], lines[1]["repo"]);
        assert_eq!(lines[0]["repo"], "repo-a");
        assert_eq!(lines[1]["repo"], "repo-b");
    }

    #[tokio::test]
    async fn process_drives_a_roadmap_lane_chain_end_to_end_and_writes_the_real_lane() {
        let dir = two_repo_brain_root();
        write_done_state(&dir.path().join("repo-a"), "A.1");
        write_done_state(&dir.path().join("repo-b"), "B.1");

        std::fs::write(
            dir.path().join("planning").join("lane-segments.json"),
            json!({
                "blocks": [
                    {
                        "roadmap": "my-roadmap",
                        "lane": "backend",
                        "repo": "repo-a",
                        "id": "A.1",
                        "segment": 0,
                        "position": 0
                    },
                    {
                        "roadmap": "my-roadmap",
                        "lane": "backend",
                        "repo": "repo-b",
                        "id": "B.1",
                        "segment": 0,
                        "position": 1
                    }
                ]
            })
            .to_string(),
        )
        .unwrap();

        let run_flow: FlowRunner = Arc::new(|invocation| {
            Box::pin(async move {
                Ok(TaskContext {
                    event: json!({}),
                    nodes: HashMap::new(),
                    metadata: json!({ "ran": invocation.block_id }),
                    node_runs: HashMap::new(),
                })
            })
        });

        let node = OrchestrationRunNode::new().with_run_flow(run_flow);
        let ctx = base_ctx(json!({
            "brain_root": dir.path(),
            "roadmap": "my-roadmap",
            "roadmap_slug": "my-roadmap",
            "lane": "backend",
        }));

        let out = node.process(ctx).await.expect("process should succeed");
        let recorded = &out.nodes[NODE_NAME];
        assert_eq!(recorded["steps_integrated"], 2);

        let lane_log = std::fs::read_to_string(
            dir.path()
                .join("planning")
                .join("roadmaps")
                .join("my-roadmap")
                .join("lane-log.jsonl"),
        )
        .expect("lane-log.jsonl should exist");
        let lines: Vec<serde_json::Value> = lane_log
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        // The real event lane, not the repo slug.
        assert_eq!(lines[0]["lane"], "backend");
        assert_eq!(lines[1]["lane"], "backend");
    }

    // ── Node::process — cancellation stamping (Task 2) ───────────────────

    /// A [`FlowRunner`] that records every block it ran and, the moment it
    /// finishes the one named `cancel_after`, cancels `token` -- the same
    /// deterministic pattern `integrate.rs`'s own cancellation tests use,
    /// duplicated here (rather than shared) because it belongs to a
    /// different crate target (`tests/it` vs this unit-test module).
    fn cancel_after_block(token: CancellationToken, cancel_after: &'static str) -> FlowRunner {
        Arc::new(move |invocation| {
            let token = token.clone();
            Box::pin(async move {
                let done = TaskContext {
                    event: json!({}),
                    nodes: HashMap::new(),
                    metadata: json!({ "ran": invocation.block_id }),
                    node_runs: HashMap::new(),
                };
                if invocation.block_id == cancel_after {
                    token.cancel();
                }
                Ok(done)
            })
        })
    }

    #[tokio::test]
    async fn a_cancelled_run_stamps_cancelled_true_with_the_stopping_step_and_total() {
        let dir = two_repo_brain_root();
        write_done_state(&dir.path().join("repo-a"), "A.1");
        write_done_state(&dir.path().join("repo-a"), "A.2");
        write_done_state(&dir.path().join("repo-a"), "A.3");

        let token = CancellationToken::new();
        let run_flow = cancel_after_block(token.clone(), "A.1");

        let node = OrchestrationRunNode::new()
            .with_run_flow(run_flow)
            .with_cancellation_token(token);
        let ctx = base_ctx(json!({
            "brain_root": dir.path(),
            "blocks": [
                { "repo": "repo-a", "block_id": "A.1" },
                { "repo": "repo-a", "block_id": "A.2" },
                { "repo": "repo-a", "block_id": "A.3" }
            ],
            "roadmap_slug": "my-roadmap",
        }));

        let out = node
            .process(ctx)
            .await
            .expect("a cancelled run is Ok, not Err");
        let recorded = &out.nodes[NODE_NAME];

        // Only A.1 integrated before the cancel won.
        assert_eq!(recorded["steps_integrated"], 1);
        assert_eq!(recorded["cancellation"]["cancelled"], true);
        assert_eq!(recorded["cancellation"]["at_step"], 1);
        assert_eq!(recorded["cancellation"]["total_steps"], 3);

        // The framework-level marker is also stamped, under the same key,
        // rather than only the node-local record existing.
        assert_eq!(out.metadata["cancellation"]["cancelled"], true);
    }

    #[tokio::test]
    async fn a_completed_run_stamps_cancelled_false_even_with_a_token_attached() {
        let dir = two_repo_brain_root();
        write_done_state(&dir.path().join("repo-a"), "A.1");
        write_done_state(&dir.path().join("repo-b"), "B.1");

        // A token is attached but never cancelled -- behavior must still
        // read as a plain completion, not merely as "no cancellation
        // detected because there was no token at all" (the other test
        // below covers that un-injected case).
        let token = CancellationToken::new();
        let run_flow: FlowRunner = Arc::new(|invocation| {
            Box::pin(async move {
                Ok(TaskContext {
                    event: json!({}),
                    nodes: HashMap::new(),
                    metadata: json!({ "ran": invocation.block_id }),
                    node_runs: HashMap::new(),
                })
            })
        });

        let node = OrchestrationRunNode::new()
            .with_run_flow(run_flow)
            .with_cancellation_token(token);
        let ctx = base_ctx(json!({
            "brain_root": dir.path(),
            "blocks": [
                { "repo": "repo-a", "block_id": "A.1" },
                { "repo": "repo-b", "block_id": "B.1" }
            ],
            "roadmap_slug": "my-roadmap",
        }));

        let out = node.process(ctx).await.expect("process should succeed");
        let recorded = &out.nodes[NODE_NAME];
        assert_eq!(recorded["steps_integrated"], 2);
        assert_eq!(recorded["cancellation"]["cancelled"], false);
        assert!(out.metadata.get("cancellation").is_none());
    }

    #[tokio::test]
    async fn no_token_injected_leaves_cancellation_reported_false() {
        let dir = two_repo_brain_root();
        write_done_state(&dir.path().join("repo-a"), "A.1");
        write_done_state(&dir.path().join("repo-b"), "B.1");

        let run_flow: FlowRunner = Arc::new(|invocation| {
            Box::pin(async move {
                Ok(TaskContext {
                    event: json!({}),
                    nodes: HashMap::new(),
                    metadata: json!({ "ran": invocation.block_id }),
                    node_runs: HashMap::new(),
                })
            })
        });

        // No `with_cancellation_token` call at all -- the behavior-stable
        // default path.
        let node = OrchestrationRunNode::new().with_run_flow(run_flow);
        let ctx = base_ctx(json!({
            "brain_root": dir.path(),
            "blocks": [
                { "repo": "repo-a", "block_id": "A.1" },
                { "repo": "repo-b", "block_id": "B.1" }
            ],
            "roadmap_slug": "my-roadmap",
        }));

        let out = node.process(ctx).await.expect("process should succeed");
        let recorded = &out.nodes[NODE_NAME];
        assert_eq!(recorded["steps_integrated"], 2);
        assert_eq!(recorded["cancellation"]["cancelled"], false);
        assert!(out.metadata.get("cancellation").is_none());
    }

    #[tokio::test]
    async fn process_reports_the_missing_roadmap_or_lane_loudly() {
        let dir = two_repo_brain_root();
        let node = OrchestrationRunNode::new();
        let ctx = base_ctx(json!({ "brain_root": dir.path() }));

        let err = node.process(ctx).await.unwrap_err();
        assert!(
            err.message.contains("blocks") || err.message.contains("roadmap"),
            "error should explain what was missing: {}",
            err.message
        );
    }

    // ── Node::process — campaign id resolution and stamping (Task 3) ────

    #[tokio::test]
    async fn an_event_with_no_campaign_id_mints_a_fresh_one_and_stamps_it() {
        let dir = two_repo_brain_root();
        write_done_state(&dir.path().join("repo-a"), "A.1");
        write_done_state(&dir.path().join("repo-b"), "B.1");

        let run_flow: FlowRunner = Arc::new(|invocation| {
            Box::pin(async move {
                Ok(TaskContext {
                    event: json!({}),
                    nodes: HashMap::new(),
                    metadata: json!({ "ran": invocation.block_id }),
                    node_runs: HashMap::new(),
                })
            })
        });

        let node = OrchestrationRunNode::new().with_run_flow(run_flow);
        let ctx = base_ctx(json!({
            "brain_root": dir.path(),
            "blocks": [
                { "repo": "repo-a", "block_id": "A.1" },
                { "repo": "repo-b", "block_id": "B.1" }
            ],
            "roadmap_slug": "my-roadmap",
        }));

        let out = node.process(ctx).await.expect("process should succeed");
        let recorded = &out.nodes[NODE_NAME];

        let stamped = recorded["campaign_id"]
            .as_str()
            .expect("campaign_id should be stamped as a string");
        Uuid::parse_str(stamped).expect("stamped campaign_id should be a valid uuid");

        let members = recorded["campaign_members"]
            .as_array()
            .expect("campaign_members should be an array");
        assert_eq!(members.len(), 2);
        assert_eq!(members[0]["repo"], "repo-a");
        assert_eq!(members[0]["block_id"], "A.1");
        assert_eq!(members[1]["repo"], "repo-b");
        assert_eq!(members[1]["block_id"], "B.1");

        // Whatever this node already stamped is still present alongside
        // the new campaign fields.
        assert_eq!(recorded["steps_integrated"], 2);
        assert_eq!(recorded["cancellation"]["cancelled"], false);
    }

    #[tokio::test]
    async fn an_event_with_an_explicit_campaign_id_reuses_it_rather_than_minting_a_fresh_one() {
        let dir = two_repo_brain_root();
        write_done_state(&dir.path().join("repo-a"), "A.1");

        let run_flow: FlowRunner = Arc::new(|invocation| {
            Box::pin(async move {
                Ok(TaskContext {
                    event: json!({}),
                    nodes: HashMap::new(),
                    metadata: json!({ "ran": invocation.block_id }),
                    node_runs: HashMap::new(),
                })
            })
        });

        let supplied = Uuid::new_v4();
        let node = OrchestrationRunNode::new().with_run_flow(run_flow);
        let ctx = base_ctx(json!({
            "brain_root": dir.path(),
            "blocks": [
                { "repo": "repo-a", "block_id": "A.1" }
            ],
            "roadmap_slug": "my-roadmap",
            "campaign_id": supplied.to_string(),
        }));

        let out = node.process(ctx).await.expect("process should succeed");
        let recorded = &out.nodes[NODE_NAME];
        assert_eq!(recorded["campaign_id"], supplied.to_string());
    }

    #[tokio::test]
    async fn a_malformed_campaign_id_fails_loudly_naming_the_field_instead_of_minting_fresh() {
        let dir = two_repo_brain_root();
        let node = OrchestrationRunNode::new();
        let ctx = base_ctx(json!({
            "brain_root": dir.path(),
            "blocks": [
                { "repo": "repo-a", "block_id": "A.1" }
            ],
            "roadmap_slug": "my-roadmap",
            "campaign_id": "not-a-uuid",
        }));

        let err = node.process(ctx).await.unwrap_err();
        assert!(
            err.message.contains("campaign_id"),
            "error should name the field: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn campaign_members_carry_tri_state_cost_usd_and_summed_total_tokens() {
        let dir = two_repo_brain_root();
        write_done_state(&dir.path().join("repo-a"), "A.1");
        write_done_state(&dir.path().join("repo-b"), "B.1");

        // repo-a's step reports a real cost figure and token usage; repo-b's
        // step reports NEITHER -- its ctx has no `cost_usd` anywhere in
        // `nodes` and no `usage` on any `node_runs` entry, so
        // `ExecutionOutcome::cost_usd` must fold to `None`, not `0.0`.
        let run_flow: FlowRunner = Arc::new(|invocation| {
            let block_id = invocation.block_id.clone();
            Box::pin(async move {
                if block_id == "A.1" {
                    let mut nodes = HashMap::new();
                    nodes.insert("SomeLlmNode".to_string(), json!({ "cost_usd": 1.25 }));
                    let mut node_runs = HashMap::new();
                    node_runs.insert(
                        "SomeLlmNode".to_string(),
                        engine_contract::NodeRun {
                            status: engine_contract::NodeRunStatus::Success,
                            started_at: None,
                            completed_at: None,
                            error: None,
                            input: None,
                            usage: Some(engine_contract::Usage {
                                input_tokens: Some(100),
                                output_tokens: Some(50),
                                model: "test-model".to_string(),
                            }),
                        },
                    );
                    Ok(TaskContext {
                        event: json!({}),
                        nodes,
                        metadata: json!({ "ran": block_id }),
                        node_runs,
                    })
                } else {
                    Ok(TaskContext {
                        event: json!({}),
                        nodes: HashMap::new(),
                        metadata: json!({ "ran": block_id }),
                        node_runs: HashMap::new(),
                    })
                }
            })
        });

        let node = OrchestrationRunNode::new().with_run_flow(run_flow);
        let ctx = base_ctx(json!({
            "brain_root": dir.path(),
            "blocks": [
                { "repo": "repo-a", "block_id": "A.1" },
                { "repo": "repo-b", "block_id": "B.1" }
            ],
            "roadmap_slug": "my-roadmap",
        }));

        let out = node.process(ctx).await.expect("process should succeed");
        let recorded = &out.nodes[NODE_NAME];
        let members = recorded["campaign_members"]
            .as_array()
            .expect("campaign_members should be an array");

        assert_eq!(members[0]["repo"], "repo-a");
        assert_eq!(members[0]["cost_usd"], 1.25);
        assert_eq!(members[0]["total_tokens"], 150);

        assert_eq!(members[1]["repo"], "repo-b");
        assert_eq!(
            members[1]["cost_usd"],
            serde_json::Value::Null,
            "a step that reported no cost must stay `null`, never collapse to 0"
        );
        assert_eq!(members[1]["total_tokens"], 0);
    }

    // ── `DEBRIEF` workflow assembly (`EN.12.G` task 4) ──────────────────

    #[test]
    fn debrief_schema_declares_the_debrief_workflow_type_and_single_node() {
        let schema = debrief_schema();
        assert_eq!(schema.workflow_type, DEBRIEF_WORKFLOW_TYPE);
        assert_eq!(DEBRIEF_WORKFLOW_TYPE, "DEBRIEF");
        assert_eq!(schema.start_node, super::super::debrief::DEBRIEF_NODE_NAME);
        assert_eq!(schema.nodes.len(), 1);
    }

    #[test]
    fn debrief_registry_contains_exactly_one_node() {
        let registry = debrief_registry(
            Arc::new(super::super::debrief::StubJournalReader::succeeding(vec![])),
            Arc::new(crate::nodes::channel_transport::StubChannelTransport::succeeding()),
            None,
        );
        assert!(registry.contains(super::super::debrief::DEBRIEF_NODE_NAME));
        assert_eq!(registry.len(), 1);
    }

    /// `EN.12.G` AC6: a `DEBRIEF` run is invocable for any finished campaign
    /// id with no conductor present — the event below carries nothing but
    /// the bare campaign id string, no `blocks`/`roadmap_slug`/`lane`/
    /// `brain_root` anywhere, proving the node needs nothing else.
    #[tokio::test]
    async fn debrief_workflow_runs_end_to_end_from_a_bare_campaign_id_alone() {
        let campaign_id = Uuid::new_v4();
        let registry = debrief_registry(
            Arc::new(super::super::debrief::StubJournalReader::succeeding(vec![])),
            Arc::new(crate::nodes::channel_transport::StubChannelTransport::succeeding()),
            None,
        );
        let workflow = Workflow::new_validated(registry, debrief_schema())
            .expect("DEBRIEF declared graph must pass WorkflowValidator::validate");

        let ctx = workflow
            .run(json!(campaign_id.to_string()), Box::new(|_| {}))
            .await
            .expect("DEBRIEF should run to completion from a bare campaign id alone");

        let recorded = &ctx.nodes[super::super::debrief::DEBRIEF_NODE_NAME];
        assert_eq!(recorded["campaign_id"], campaign_id.to_string());
        assert_eq!(recorded["row_count"], 0);
    }

    /// `EN.12.G` AC5: registering `DEBRIEF` never rewires how an existing
    /// explicit block-list chain resolves its own steps — `resolve_explicit_chain`
    /// builds exactly one `StepKind::Block` `ChainStep` per declared entry
    /// and never inserts anything else, `DEBRIEF` included.
    #[test]
    fn resolve_explicit_chain_never_gains_an_implicit_debrief_step() {
        let steps = resolve_explicit_chain(vec![
            ("repo-a".to_string(), "A.1".to_string()),
            ("repo-b".to_string(), "B.1".to_string()),
        ]);

        assert_eq!(steps.len(), 2);
        assert!(steps
            .iter()
            .all(|s| s.kind == super::super::chain::StepKind::Block));
        assert!(steps.iter().all(|s| s.block_id != DEBRIEF_WORKFLOW_TYPE));
    }
}
