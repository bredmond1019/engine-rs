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
//! This workflow introduces exactly one knob so far: `hold_poll_interval_ms`
//! — how often [`integrate::wait_for_clearance`] re-polls an operator hold
//! while paused. It is a pure latency/overhead trade (a busier poll notices
//! clearance sooner at the cost of more wake-ups) and never changes the
//! declared node set, so — unlike `sdlc_flow`/`diagnostic_intake` — there is
//! no `registry_for_policy`: [`registry`] alone is what every profile runs.
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

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use engine_contract::TaskContext;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::node::{Node, NodeError, NodeRegistry};
use crate::policy::{read_harness_policy_defaults_from, resolve_profile_from, PolicyConfigSource};
use crate::repo_registry::RepoRegistry;
use crate::schema::{NodeConfig, WorkflowSchema};
use crate::workflow::Workflow;

use super::chain::{resolve_explicit_chain, resolve_lane_chain, ChainStep};
use super::execute::{default_flow_runner, EngineKind, FlowRunner};
use super::gates::{AdmissionGate, DependencyEdge};
use super::integrate::{integrate_chain, resolve_roadmap_dir, HoldSource, NeverHeld};

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
}

impl Default for OrchestrationPolicy {
    /// The behavior-stable baseline: poll every 2s while held.
    fn default() -> Self {
        Self {
            hold_poll_interval_ms: 2_000,
        }
    }
}

/// All-optional mirror of [`OrchestrationPolicy`] used by the override
/// layers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialOrchestrationPolicy {
    pub hold_poll_interval_ms: Option<u64>,
}

impl crate::policy::Policy for OrchestrationPolicy {
    type Partial = PartialOrchestrationPolicy;

    fn apply(self, over: &PartialOrchestrationPolicy) -> Self {
        OrchestrationPolicy {
            hold_poll_interval_ms: crate::policy::merge_opt(
                self.hold_poll_interval_ms,
                over.hold_poll_interval_ms,
            ),
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
    }
}

/// Cheapest/fastest profile: poll far less often — fewer wake-ups, at the
/// cost of noticing an operator clearance later.
#[must_use]
pub fn cheap_fast() -> PartialOrchestrationPolicy {
    PartialOrchestrationPolicy {
        hold_poll_interval_ms: Some(10_000),
    }
}

/// Highest-responsiveness profile: poll far more often — a cleared hold is
/// noticed almost immediately, at the cost of more wake-ups.
#[must_use]
pub fn thorough() -> PartialOrchestrationPolicy {
    PartialOrchestrationPolicy {
        hold_poll_interval_ms: Some(500),
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
}

fn parse_event(ctx: &TaskContext) -> Result<OrchestrationEventSchema, NodeError> {
    serde_json::from_value(ctx.event.clone())
        .map_err(|err| NodeError::new(format!("invalid ORCHESTRATION event: {err}")))
}

// ── The node ─────────────────────────────────────────────────────────────

type DependsOnFn = Arc<dyn Fn(&str, &str) -> Vec<DependencyEdge> + Send + Sync>;
type EdgeMetFn = Arc<dyn Fn(&str, &str) -> bool + Send + Sync>;
type EngineFn = Arc<dyn Fn(&str, &str) -> EngineKind + Send + Sync>;
type BlockOpenFn = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// The sole node in the `ORCHESTRATION` graph: resolves a lane chain, then
/// drives it end to end via [`integrate::integrate_chain`] — dependency
/// gate, admission gate, operator-hold pause/resume, `SDLC_FLOW` invocation
/// per block (cwd-scoped to that block's repo), state-write verification,
/// and exactly one `lane-log.jsonl` line per integrated block.
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
            let lane_segments_path = event.brain_root.join("planning").join("lane-segments.json");
            let is_block_open = self.is_block_open.clone();
            resolve_lane_chain(&lane_segments_path, &roadmap, &lane, &move |token| {
                is_block_open(token)
            })
            .map_err(|err| NodeError::new(err.to_string()))?
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

        let resolve_depends_on = self.resolve_depends_on.clone();
        let is_edge_met = self.is_edge_met.clone();
        let resolve_engine = self.resolve_engine.clone();
        let admission = self.admission.clone();
        let hold_source = self.hold_source.clone();

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
        let outcomes = tokio::task::spawn_blocking(move || {
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
                &move |repo, block_id| resolve_engine(repo, block_id),
                &repo_registry,
                &run_flow,
                &roadmap_dir,
            ))
            .map_err(|err| NodeError::new(err.to_string()))
        })
        .await
        .map_err(|err| NodeError::new(format!("orchestration task panicked: {err}")))??;

        let mut ctx = ctx;
        ctx.nodes.insert(
            NODE_NAME.to_string(),
            json!({
                "steps_integrated": outcomes.len(),
                "blocks": outcomes
                    .iter()
                    .map(|o| json!({ "repo": o.repo, "block_id": o.block_id }))
                    .collect::<Vec<_>>(),
                "policy": { "hold_poll_interval_ms": policy.hold_poll_interval_ms },
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
/// ([`OrchestrationRunNode::new`]). This is the only registry the
/// `ORCHESTRATION` workflow ever runs under — unlike `sdlc_flow` /
/// `diagnostic_intake`, no policy setting here changes which node runs, so
/// there is no `registry_for_policy` sibling.
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
        };
        let resolved = crate::policy::resolve(
            OrchestrationPolicy::default(),
            None,
            Some(&thorough()),
            Some(&event_override),
        );
        assert_eq!(resolved.hold_poll_interval_ms, 42);
    }

    /// Every named profile only ever changes `hold_poll_interval_ms` — the
    /// declared node set ([`registry`]) is identical regardless of which
    /// profile a run selects, since this workflow's sole policy knob never
    /// rewires which node runs (CLAUDE.md standing rule 6: "a policy knob
    /// may change a bound or a tier; it must not change... a declared
    /// node set").
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

        let lane_log = std::fs::read_to_string(
            dir.path()
                .join("planning")
                .join("roadmaps")
                .join("my-roadmap")
                .join("lane-log.jsonl"),
        )
        .expect("lane-log.jsonl should exist");
        assert_eq!(lane_log.lines().count(), 2);
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
}
