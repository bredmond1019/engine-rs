//! Hermetic end-to-end integration test for the assembled `LINKEDIN_POST`
//! workflow (`EN.5.G` task 7): drives the real declared graph —
//! `WorkSourceNode -> PostDraftNode -> PostCandidateSelectNode ->
//! BrandCriticNode -> CriticRouterNode -> { TranslateNode (exit) |
//! IncrementCriticIterationNode -> ReviseNode -> BrandCriticNode }`
//! (back-edge) — through the real `Workflow::run` pointer-walk loop.
//!
//! `graph::registry()`/`graph::registry_for_policy()` build the served,
//! real-transport registry — no seam to stub an individual node's
//! transport once a node is inside it, and `PostCandidateSelectNode`/
//! `CriticRouterNode`/(the enabled-path of) `TranslateNode` are private
//! types local to `graph.rs`, not constructible from outside that module.
//! This suite therefore builds its own registry against the SAME declared
//! `graph::schema()`, wiring `WorkSourceNode`/`PostDraftNode`/
//! `BrandCriticNode`/`ReviseNode` (all public, all carrying an injectable
//! seam) to stubs, and standing in trivial local replicas of the three
//! private adapter/router nodes under their exact `graph.rs` identities —
//! mirrors `content_pipeline_e2e.rs`'s own "build a fresh registry with
//! every declared node identity, each wired to a stub" pattern. The
//! replica nodes reproduce only the behavior this suite's assertions
//! depend on (select-first-candidate, critic-verdict routing, a translate
//! no-op sink); the real `TranslateGateNode`'s enabled/model-calling path
//! is already covered by `graph.rs`'s own unit tests, so every event here
//! disables translate.
//!
//! Hermetic by construction: no real `claude` subprocess is ever spawned
//! (`PostDraftNode`/`BrandCriticNode`/`ReviseNode` are all built
//! `with_transport(..)` a canned stub) and no real `git`/filesystem access
//! happens (`WorkSourceNode` is built `with_runner(..)`/`with_file_reader(..)`/
//! `with_dir_reader(..)` stubs).
//!
//! Covers task 7's acceptance criteria:
//! (a) [`fixture_week_run_yields_at_least_three_traceable_candidates`] — an
//!     end-to-end run over a fixture week emits >= 3 candidates, each with
//!     a non-empty `sources` traceable to a fixture commit or log entry;
//! (b) [`first_critic_revise_then_pass_completes_with_exactly_one_revise_iteration`]
//!     — a run whose first critic verdict is `revise` and second is `pass`
//!     completes with exactly one revise iteration;
//! (c) [`critic_loop_exhausts_the_iteration_cap_and_still_exits_forward`] —
//!     a run whose critic always returns `revise` terminates by the
//!     `max_critic_iterations` cap, not a hang;
//! (d) [`empty_date_range_yields_no_fabricated_candidates`] — a run over an
//!     empty (inverted) date range short-circuits `WorkSourceNode` and
//!     never emits a candidate.

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use claude_code_rs::parse::{ModelUsage as SdkModelUsage, Usage as SdkUsage};
use claude_code_rs::{Config, Outcome};
use engine_contract::{NodeRunStatus, TaskContext};
use engine_core::node::{Node, NodeError, NodeRegistry};
use engine_core::policy::{self, PolicyConfigSource};
use engine_core::routing::Router;
use engine_core::workflow::Workflow;
use engine_core::workflows::content_pipeline::increment_critic_iteration::{
    self, IncrementCriticIterationNode,
};
use engine_core::workflows::linkedin_post::work_source::{DirReader, FileReader};
use engine_core::workflows::linkedin_post::{
    brand_critic, draft, graph, profiles, revise, work_source, BrandCriticNode, PostCandidate,
    PostDraftNode, ReviseNode, WorkSourceNode,
};
use engine_core::workflows::{CommandOutput, CommandRunner, ModelTransport};
use futures::FutureExt;
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Local replicas of `graph.rs`'s private adapter/router nodes.
//
// Identity strings match `graph.rs` exactly so this suite drives the same
// declared `graph::schema()` the real registration does.
// ---------------------------------------------------------------------------

const CANDIDATE_SELECT_NODE_NAME: &str = "PostCandidateSelectNode";
const CRITIC_ROUTER_NODE_NAME: &str = "CriticRouterNode";
const TRANSLATE_NODE_NAME: &str = "TranslateNode";

/// Replica of `graph.rs`'s private `PostCandidateSelectNode`: bridges
/// `PostDraftNode`'s `{candidates, unsupported_claims}` into the single
/// `{draft, sources}` shape `BrandCriticNode`/`ReviseNode` expect, by
/// selecting the first (primary) candidate.
struct TestCandidateSelectNode;

#[async_trait::async_trait]
impl Node for TestCandidateSelectNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let candidates: Vec<PostCandidate> = ctx
            .nodes
            .get(draft::NODE_NAME)
            .and_then(|value| value.get("candidates").cloned())
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default();

        let primary = candidates.into_iter().next().ok_or_else(|| {
            NodeError::new(format!(
                "{CANDIDATE_SELECT_NODE_NAME}: no traceable candidates stored by {}",
                draft::NODE_NAME
            ))
        })?;

        ctx.nodes.insert(
            CANDIDATE_SELECT_NODE_NAME.to_string(),
            json!({ "draft": primary.draft, "sources": primary.sources }),
        );

        Ok(ctx)
    }

    fn name(&self) -> &str {
        CANDIDATE_SELECT_NODE_NAME
    }
}

/// Replica of `graph.rs`'s private `CriticRouterNode`: routes to
/// `TranslateNode` on a `pass` verdict or once `capped`, otherwise to
/// `IncrementCriticIterationNode`.
struct TestCriticRouterNode;

#[async_trait::async_trait]
impl Node for TestCriticRouterNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        Ok(ctx)
    }

    fn name(&self) -> &str {
        CRITIC_ROUTER_NODE_NAME
    }

    fn as_router(&self) -> Option<&dyn Router> {
        Some(self)
    }
}

impl Router for TestCriticRouterNode {
    fn route(&self, ctx: &TaskContext) -> Option<String> {
        let stored = ctx.nodes.get(brand_critic::NODE_NAME)?;
        let verdict_is_pass = stored.get("verdict").and_then(Value::as_str) == Some("pass");
        let capped = stored
            .get("capped")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        Some(if verdict_is_pass || capped {
            TRANSLATE_NODE_NAME.to_string()
        } else {
            increment_critic_iteration::NODE_NAME.to_string()
        })
    }
}

/// Replica of `graph.rs`'s private `TranslateGateNode`'s no-op path — every
/// event this suite drives sets `translate_enabled: false`, so the real
/// node's enabled (model-calling) path is never reached in production
/// either; that path is covered by `graph.rs`'s own unit tests.
struct TestTranslateSinkNode;

#[async_trait::async_trait]
impl Node for TestTranslateSinkNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.nodes.insert(
            TRANSLATE_NODE_NAME.to_string(),
            json!({ "translated": false }),
        );
        Ok(ctx)
    }

    fn name(&self) -> &str {
        TRANSLATE_NODE_NAME
    }
}

// ---------------------------------------------------------------------------
// Stub helpers
// ---------------------------------------------------------------------------

fn stub_outcome(structured: Value, input_tokens: u64, output_tokens: u64) -> Outcome {
    Outcome {
        cost_usd: 0.01,
        usage: SdkUsage {
            input_tokens,
            output_tokens,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
        model_usage: [(
            "claude-sonnet-4-5".to_string(),
            SdkModelUsage {
                input_tokens,
                output_tokens,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                cost_usd: 0.01,
            },
        )]
        .into_iter()
        .collect(),
        text: serde_json::to_string(&structured).unwrap(),
        is_error: false,
        api_error_status: None,
        structured_output: Some(structured),
    }
}

/// Build a transport that always replies with `structured`.
fn stub_transport_returning(structured: Value) -> ModelTransport {
    Arc::new(move |_config: Config, _prompt: String| {
        let structured = structured.clone();
        async move { Ok(stub_outcome(structured, 40, 20)) }.boxed()
    })
}

/// Build a transport that replies with each entry of `sequence` in turn,
/// repeating the last entry once exhausted — used to drive the critic loop
/// through a fixed sequence of verdicts across successive passes.
fn sequenced_transport(sequence: Vec<Value>) -> ModelTransport {
    let index = Arc::new(AtomicUsize::new(0));
    Arc::new(move |_config: Config, _prompt: String| {
        let i = index.fetch_add(1, Ordering::SeqCst);
        let structured = sequence
            .get(i)
            .or_else(|| sequence.last())
            .cloned()
            .expect("sequence must be non-empty");
        async move { Ok(stub_outcome(structured, 40, 20)) }.boxed()
    })
}

/// Build a transport that increments `counter` on every call and always
/// replies with `structured`.
fn counting_transport(structured: Value, counter: Arc<AtomicUsize>) -> ModelTransport {
    Arc::new(move |_config: Config, _prompt: String| {
        counter.fetch_add(1, Ordering::SeqCst);
        let structured = structured.clone();
        async move { Ok(stub_outcome(structured, 40, 20)) }.boxed()
    })
}

/// A `CommandRunner` returning a fixed `git log` stdout for any invocation.
fn fixed_git_log_runner(stdout: &'static str) -> CommandRunner {
    Arc::new(move |_program, _args, _cwd| {
        Ok(CommandOutput {
            status: 0,
            stdout: stdout.to_string(),
            stderr: String::new(),
        })
    })
}

fn fixed_file_reader(content: &'static str) -> FileReader {
    Arc::new(move |_path: &Path| Ok(content.to_string()))
}

fn empty_dir_reader() -> DirReader {
    Arc::new(|_dir: &Path| -> io::Result<Vec<(String, String)>> { Ok(Vec::new()) })
}

// ---------------------------------------------------------------------------
// Fixture data: one week's real work
// ---------------------------------------------------------------------------

const FIXTURE_COMMIT_ID: &str = "aaa1111bbb";
const FIXTURE_COMMIT_SUMMARY: &str = "Shipped the WorkSourceNode";
const FIXTURE_LOG_ENTRY_ID: &str = "2026-08-20-1";
const FIXTURE_LOG_ENTRY_SUMMARY: &str = "Shipped the brand critic";

fn fixture_git_log_stdout() -> &'static str {
    "aaa1111bbb\x1fShipped the WorkSourceNode\n"
}

fn fixture_log_md() -> &'static str {
    "\
## [2026-08-20]

### Shipped the brand critic
- **What:** carried brand.md's anti-slop bank verbatim.
"
}

fn draft_candidate(angle: &str, source_id: &str, source_kind: &str, source_summary: &str) -> Value {
    json!({
        "angle": angle,
        "draft": format!("This week I built {angle}."),
        "sources": [{ "kind": source_kind, "id": source_id, "summary": source_summary }],
    })
}

/// Three traceable candidates, each referencing one of the fixture's real
/// `WorkSource`s (never a fabricated id).
fn three_traceable_candidates_response() -> Value {
    json!({
        "candidates": [
            draft_candidate("the work source node", FIXTURE_COMMIT_ID, "commit", FIXTURE_COMMIT_SUMMARY),
            draft_candidate(
                "the brand critic",
                FIXTURE_LOG_ENTRY_ID,
                "log-entry",
                FIXTURE_LOG_ENTRY_SUMMARY,
            ),
            draft_candidate(
                "the traceability invariant",
                FIXTURE_COMMIT_ID,
                "commit",
                FIXTURE_COMMIT_SUMMARY,
            ),
        ],
        "unsupported_claims": [],
    })
}

fn critic_json(verdict: &str, confidence: f64, issues: Vec<&str>) -> Value {
    json!({
        "verdict": verdict,
        "confidence": confidence,
        "issues": issues,
    })
}

fn revised_draft_json(draft: &str) -> Value {
    json!({ "draft": draft })
}

// ---------------------------------------------------------------------------
// Registry / workflow construction
// ---------------------------------------------------------------------------

/// Bundles the per-node stubs a [`build_registry`] call needs.
struct Stubs {
    runner: CommandRunner,
    file_reader: FileReader,
    dir_reader: DirReader,
    draft: ModelTransport,
    critic: ModelTransport,
    revise: ModelTransport,
}

impl Stubs {
    /// A full fixture week (one commit, one log entry, no decisions), a
    /// draft transport proposing three traceable candidates, and a critic
    /// that passes on the first pass.
    fn default_passing() -> Self {
        Self {
            runner: fixed_git_log_runner(fixture_git_log_stdout()),
            file_reader: fixed_file_reader(fixture_log_md()),
            dir_reader: empty_dir_reader(),
            draft: stub_transport_returning(three_traceable_candidates_response()),
            critic: stub_transport_returning(critic_json("pass", 0.95, vec![])),
            revise: stub_transport_returning(revised_draft_json("a corrected draft")),
        }
    }
}

/// Build a fresh registry with every `LINKEDIN_POST` node identity
/// registered against `graph::schema()`, each wired to the matching stub —
/// see this file's module doc comment for why this is a local replica
/// rather than `graph::registry()`.
fn build_registry(stubs: &Stubs) -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(
        WorkSourceNode::new()
            .with_runner(stubs.runner.clone())
            .with_file_reader(stubs.file_reader.clone())
            .with_dir_reader(stubs.dir_reader.clone()),
    ));
    registry.register(Box::new(
        PostDraftNode::new().with_transport(stubs.draft.clone()),
    ));
    registry.register(Box::new(TestCandidateSelectNode));
    registry.register(Box::new(
        BrandCriticNode::new()
            .with_transport(stubs.critic.clone())
            .with_draft_input_from(CANDIDATE_SELECT_NODE_NAME),
    ));
    registry.register(Box::new(TestCriticRouterNode));
    registry.register(Box::new(IncrementCriticIterationNode));
    registry.register(Box::new(
        ReviseNode::new()
            .with_transport(stubs.revise.clone())
            .with_draft_input_from(CANDIDATE_SELECT_NODE_NAME),
    ));
    registry.register(Box::new(TestTranslateSinkNode));
    registry
}

/// Resolve `event`'s `LINKEDIN_POST` policy (built-in + `harness.json` +
/// named `profile:` + inline `policy` override) and stamp it under
/// `RESOLVED_POLICY_IDENTITY` — the same seeding `engine-serve`'s dispatch
/// factory performs before a real node ever sees `ctx` (nodes here never
/// re-resolve policy inside `process()`).
fn seeded_resolved_policy(event: &Value) -> HashMap<String, Value> {
    let ctx = TaskContext {
        event: event.clone(),
        nodes: HashMap::new(),
        metadata: json!({}),
        node_runs: HashMap::new(),
    };
    let resolved = profiles::resolve_policy_for_run_from(&ctx, &PolicyConfigSource::Builtin)
        .expect("LINKEDIN_POST policy should resolve for this event");
    let mut seeded = HashMap::new();
    seeded.insert(
        policy::RESOLVED_POLICY_IDENTITY.to_string(),
        serde_json::to_value(&resolved).expect("policy serializes"),
    );
    seeded
}

fn build_workflow(stubs: &Stubs, event: &Value) -> Workflow {
    Workflow::new_validated(build_registry(stubs), graph::schema())
        .expect("declared LINKEDIN_POST graph should validate")
        .with_seeded_nodes(seeded_resolved_policy(event))
}

/// Drive `workflow` through the real `Workflow::run` pointer-walk loop.
async fn drive(workflow: &Workflow, event: Value) -> TaskContext {
    workflow
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("LINKEDIN_POST run should complete (structural errors only surface as Err)")
}

// ---------------------------------------------------------------------------
// Event builders
// ---------------------------------------------------------------------------

/// A fixture-week event. Translate is always disabled — the real
/// `TranslateGateNode`'s enabled path is covered by `graph.rs`'s own unit
/// tests, not this suite (see module doc comment).
fn fixture_week_event(max_critic_iterations: u32) -> Value {
    json!({
        "since": "2026-08-17",
        "until": "2026-08-24",
        "policy": {
            "translate_enabled": false,
            "max_critic_iterations": max_critic_iterations,
        },
    })
}

fn empty_range_event() -> Value {
    json!({
        "since": "2026-08-24",
        "until": "2026-08-17",
        "policy": { "translate_enabled": false },
    })
}

// ---------------------------------------------------------------------------
// (a) Fixture week -> at least three traceable candidates
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fixture_week_run_yields_at_least_three_traceable_candidates() {
    let stubs = Stubs::default_passing();
    let event = fixture_week_event(3);
    let workflow = build_workflow(&stubs, &event);

    let ctx = drive(&workflow, event).await;

    // WorkSourceNode actually gathered the fixture's real work.
    let work_sources = ctx.nodes[work_source::NODE_NAME]["sources"]
        .as_array()
        .expect("sources array");
    let fixture_ids: Vec<String> = work_sources
        .iter()
        .map(|s| s["id"].as_str().unwrap().to_string())
        .collect();
    assert!(fixture_ids.contains(&FIXTURE_COMMIT_ID.to_string()));
    assert!(fixture_ids.contains(&FIXTURE_LOG_ENTRY_ID.to_string()));

    let candidates = ctx.nodes[draft::NODE_NAME]["candidates"]
        .as_array()
        .expect("candidates array");
    assert!(
        candidates.len() >= 3,
        "expected at least 3 candidates, got {}: {candidates:#?}",
        candidates.len()
    );

    for candidate in candidates {
        let sources = candidate["sources"].as_array().expect("sources array");
        assert!(!sources.is_empty(), "candidate must carry sources");
        for source in sources {
            let id = source["id"].as_str().expect("source id");
            assert!(
                fixture_ids.contains(&id.to_string()),
                "candidate source id {id:?} must trace to a real fixture source, got fixture ids {fixture_ids:?}"
            );
        }
    }

    // The run reaches the terminal translate sink successfully — a
    // traceable draft is never blocked by the critic gate on a clean
    // first pass.
    assert_eq!(
        ctx.node_runs[TRANSLATE_NODE_NAME].status,
        NodeRunStatus::Success
    );
    assert_eq!(
        ctx.node_runs[CANDIDATE_SELECT_NODE_NAME].status,
        NodeRunStatus::Success
    );
    assert_eq!(
        ctx.node_runs[brand_critic::NODE_NAME].status,
        NodeRunStatus::Success
    );
}

// ---------------------------------------------------------------------------
// (b) First critic pass fails, second passes -> exactly one revise iteration
// ---------------------------------------------------------------------------

#[tokio::test]
async fn first_critic_revise_then_pass_completes_with_exactly_one_revise_iteration() {
    let revise_calls = Arc::new(AtomicUsize::new(0));
    let mut stubs = Stubs::default_passing();
    stubs.critic = sequenced_transport(vec![
        critic_json("revise", 0.3, vec!["hedge phrase: \"typically\""]),
        critic_json("pass", 0.9, vec![]),
    ]);
    stubs.revise = counting_transport(
        revised_draft_json("a corrected, plainer draft"),
        revise_calls.clone(),
    );

    let event = fixture_week_event(3);
    let workflow = build_workflow(&stubs, &event);

    let ctx = drive(&workflow, event).await;

    assert_eq!(revise_calls.load(Ordering::SeqCst), 1);

    let evaluation = &ctx.nodes[brand_critic::NODE_NAME];
    assert_eq!(evaluation["verdict"], json!("pass"));
    assert_eq!(evaluation["iteration"], json!(1));
    assert_eq!(evaluation["capped"], json!(false));

    // The revised draft actually propagated to `ReviseNode`'s own output —
    // this is what `BrandCriticNode`'s read-preference reads on the second
    // pass.
    assert_eq!(
        ctx.nodes[revise::NODE_NAME]["draft"],
        json!("a corrected, plainer draft")
    );

    assert_eq!(
        ctx.node_runs[TRANSLATE_NODE_NAME].status,
        NodeRunStatus::Success
    );
}

// ---------------------------------------------------------------------------
// (c) Critic loop exhausts the iteration cap
// ---------------------------------------------------------------------------

#[tokio::test]
async fn critic_loop_exhausts_the_iteration_cap_and_still_exits_forward() {
    let critic_calls = Arc::new(AtomicUsize::new(0));
    let revise_calls = Arc::new(AtomicUsize::new(0));
    let mut stubs = Stubs::default_passing();
    stubs.critic = counting_transport(
        critic_json("revise", 0.1, vec!["still hedging"]),
        critic_calls.clone(),
    );
    stubs.revise = counting_transport(revised_draft_json("still not clean"), revise_calls.clone());

    let event = fixture_week_event(2);
    let workflow = build_workflow(&stubs, &event);

    let ctx = drive(&workflow, event).await;

    assert_eq!(
        critic_calls.load(Ordering::SeqCst),
        2,
        "exactly max_critic_iterations critic passes"
    );
    assert_eq!(
        revise_calls.load(Ordering::SeqCst),
        1,
        "the back-edge fires once between the two critic passes, never a third time"
    );

    let evaluation = &ctx.nodes[brand_critic::NODE_NAME];
    assert_eq!(evaluation["verdict"], json!("revise"));
    assert_eq!(evaluation["capped"], json!(true));

    // The cap fired, not a pass verdict — the loop still exits forward
    // rather than hanging.
    assert_eq!(
        ctx.node_runs[TRANSLATE_NODE_NAME].status,
        NodeRunStatus::Success
    );
}

// ---------------------------------------------------------------------------
// (d) Empty date range -> no fabricated candidates
// ---------------------------------------------------------------------------

#[tokio::test]
async fn empty_date_range_yields_no_fabricated_candidates() {
    let mut stubs = Stubs::default_passing();
    // Even with sources absent, the draft transport must never be pressed
    // into fabricating a traceable candidate out of nothing — it replies
    // honestly with an empty candidate list.
    stubs.draft = stub_transport_returning(json!({ "candidates": [], "unsupported_claims": [] }));

    let event = empty_range_event();
    let workflow = build_workflow(&stubs, &event);

    let ctx = drive(&workflow, event).await;

    let work_source_result = &ctx.nodes[work_source::NODE_NAME];
    assert!(work_source_result["sources"]
        .as_array()
        .expect("sources array")
        .is_empty());
    assert!(work_source_result["message"]
        .as_str()
        .unwrap_or_default()
        .contains("empty date range"));

    assert_eq!(
        ctx.node_runs[work_source::NODE_NAME].status,
        NodeRunStatus::Success
    );
    assert_eq!(
        ctx.node_runs[draft::NODE_NAME].status,
        NodeRunStatus::Success
    );
    assert!(ctx.nodes[draft::NODE_NAME]["candidates"]
        .as_array()
        .expect("candidates array")
        .is_empty());

    // No traceable candidate exists to select — the run halts here rather
    // than fabricating one, and nothing downstream ever runs.
    assert_eq!(
        ctx.node_runs[CANDIDATE_SELECT_NODE_NAME].status,
        NodeRunStatus::Failed
    );
    assert!(ctx.node_runs[CANDIDATE_SELECT_NODE_NAME]
        .error
        .as_deref()
        .unwrap_or_default()
        .contains(draft::NODE_NAME));
    assert_eq!(
        ctx.node_runs[brand_critic::NODE_NAME].status,
        NodeRunStatus::Pending
    );
    assert_eq!(
        ctx.node_runs[TRANSLATE_NODE_NAME].status,
        NodeRunStatus::Pending
    );
}
