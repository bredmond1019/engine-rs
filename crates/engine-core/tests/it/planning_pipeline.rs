//! Hermetic end-to-end suite for `PLANNING_PIPELINE` (`EN.19.D` task 9) —
//! drives the real graph `planning_pipeline::{schema_for_stages,
//! workflow_for_stages}` declare through `Workflow::run`/`run_from`, with
//! every model-calling node's transport stubbed (never a real `claude` CLI
//! spawn) and a stubbed `HttpPost` for `DispatchNode`.
//!
//! `planning_pipeline::registry_for_stages` (task 7's own composition
//! entry point) hardcodes the REAL, non-stubbable node constructors for
//! `pre_plan`/`plan_authoring`'s model-calling nodes — by design, since it
//! is the production wiring `engine-serve` dispatches through. This suite
//! therefore builds its own registry ([`full_registry`] below), composing
//! the exact same real node TYPES `registry_for_stages` does, substituting
//! only each node's own already-public `with_transport`/`with_http_post`
//! test seam — mirroring the established precedent in
//! `tests/it/pre_plan.rs`'s `registry_with_stub` and
//! `tests/it/plan_authoring.rs`'s `registry_at` (neither of those suites
//! calls their own workflow's `registry()` either, for the identical
//! reason). [`full_registry`]'s gate-wiring section duplicates
//! `planning_pipeline::mod.rs`'s private `gate_identity`/`gate_loop_spec`
//! helpers (both undocumented-private, so unreachable from here) — a
//! narrow, deliberate re-derivation of a small, stable, doc-comment-pinned
//! algorithm, the same class of duplication `generate_tasks_for_block.rs`'s
//! own `spec_dir` helper already documents as acceptable in this repo.
//!
//! Covers the block record's remaining acceptance criteria:
//!
//! - `stages: [pre_plan]` on a fresh slug produces exactly `PRE_PLAN`'s own
//!   behavior — no gate at all;
//! - `stages: [pre_plan, plan]` (with `pre_plan` auto-approved) produces a
//!   real `notes.md` AND a real `plan.md`/candidate blocks, with no
//!   `generate_tasks`/`dispatch` side effects;
//! - `stages: [plan, generate_tasks, dispatch]` against a slug whose
//!   `notes.md` already exists skips `pre_plan` (it was never requested),
//!   produces `plan.md`/candidates, then `tasks.json`, then a real dispatch
//!   returning a `run_id`;
//! - a gap/out-of-order `stages` list is rejected by
//!   `workflow_for_stages`/`schema_for_stages` BEFORE any `Workflow` is ever
//!   constructed — "dispatches nothing" by construction;
//! - re-dispatching the same slug/stages after success short-circuits every
//!   stage via its own idempotency guard, with zero further model calls;
//! - `DispatchNode` refuses a block already `in_progress`/`closed`, and
//!   permits + returns a `run_id` for an eligible one;
//! - an event naming no `approval` map pauses after every requested stage's
//!   gate;
//! - approve/reject/discuss verdicts are each recorded distinctly through
//!   `operator::ledger::record_decision`, readable back, never conflated;
//! - a stale/mismatched digest is refused and requeued, never authorizing
//!   continuation.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, TimeZone, Utc};
use claude_code_rs::{Config, Outcome};
use engine_contract::TaskContext;
use engine_core::node::{NodeExt, NodeRegistry};
use engine_core::nodes::channel_transport::DEFAULT_EVENTS_URL;
use engine_core::nodes::http_post::{HttpPost, StubHttpPost};
use engine_core::operator::ledger::{ApprovalLedger, ApprovalLedgerRow, LedgerDecision};
use engine_core::operator::queue::{OperatorQueue, OperatorQueuePolicy};
use engine_core::workflow::{ResumeState, Workflow};
use engine_core::workflows::llm_node::TransportSlotted;
use engine_core::workflows::plan_authoring::check_existing::{
    self as pa_check_existing, CheckExistingPlanNode,
};
use engine_core::workflows::plan_authoring::decompose::DecomposePlanNode;
use engine_core::workflows::plan_authoring::gather_context::GatherPlanContextNode;
use engine_core::workflows::plan_authoring::stage_candidate_blocks::StageCandidateBlocksNode;
use engine_core::workflows::plan_authoring::write_narrative::{
    self as pa_write_narrative, WritePlanNarrativeNode,
};
use engine_core::workflows::planning_pipeline::approval_gate::{
    self, ApprovalGateNode, PLANNING_PIPELINE_APPROVE, PLANNING_PIPELINE_DISCUSS,
    PLANNING_PIPELINE_REJECT,
};
use engine_core::workflows::planning_pipeline::dispatch::{self, DispatchNode};
use engine_core::workflows::planning_pipeline::generate_tasks_for_block::{
    self, GenerateTasksForBlockNode,
};
use engine_core::workflows::planning_pipeline::policy::ApprovalGatePolicy;
use engine_core::workflows::planning_pipeline::stage_selector::{self, StageSelectorNode};
use engine_core::workflows::planning_pipeline::{
    registry_for_stages, schema_for_stages, workflow_for_stages,
};
use engine_core::workflows::pre_plan::{
    check_existing as pp_check_existing, intake as pp_intake, research as pp_research,
    secret_guard as pp_secret_guard, write_notes as pp_write_notes, PrePlanNotesAlreadyExistsNode,
    PrePlanPolicy,
};
use engine_core::workflows::ModelTransport;
use engine_core::{build_loop, read_suspension, ExitPredicate, LoopSpec, RunOptions};
use futures::FutureExt;
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Shared fixtures
// ---------------------------------------------------------------------------

/// `ENGINE_BRAIN_ROOT` is process-global state — every test that relies on
/// `pre_plan`'s env-var-only root resolution guards it with this
/// mutex/RAII pair, mirroring `tests/it/pre_plan.rs`'s own
/// `BrainRootGuard`. `cargo nextest` forks one process per test, so this is
/// defense-in-depth rather than a strict requirement, but it documents the
/// dependency and stays correct under any future non-nextest run.
const BRAIN_TOML: &str = r#"
[[repos]]
slug = "engine-rs"
prefix = "EN"
tier = "core"
repo_path = "."
"#;

static ENV_GUARD: Mutex<()> = Mutex::new(());

struct BrainRootGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    previous: Option<String>,
}

impl BrainRootGuard {
    fn set(root: &Path) -> Self {
        let lock = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var(engine_core::brain_root::ENGINE_BRAIN_ROOT_ENV).ok();
        std::env::set_var(engine_core::brain_root::ENGINE_BRAIN_ROOT_ENV, root);
        Self {
            _lock: lock,
            previous,
        }
    }
}

impl Drop for BrainRootGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(v) => std::env::set_var(engine_core::brain_root::ENGINE_BRAIN_ROOT_ENV, v),
            None => std::env::remove_var(engine_core::brain_root::ENGINE_BRAIN_ROOT_ENV),
        }
    }
}

/// A local `ApprovalLedger` stub — `engine_core::operator::ledger::
/// InMemoryApprovalLedger` is `#[cfg(test)]`-gated inside `engine-core`
/// itself, so it is invisible to this external integration crate. This
/// mirrors that type's own shape exactly (no real file I/O), so this
/// suite's ledger assertions exercise the identical `ApprovalLedger`
/// contract `approval_gate.rs`'s own unit tests do.
#[derive(Default)]
struct TestApprovalLedger {
    rows: Mutex<Vec<ApprovalLedgerRow>>,
}

impl TestApprovalLedger {
    fn new() -> Self {
        Self::default()
    }
}

impl ApprovalLedger for TestApprovalLedger {
    fn append(&self, row: ApprovalLedgerRow) {
        self.rows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(row);
    }

    fn read_all(&self) -> Vec<ApprovalLedgerRow> {
        self.rows.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn rows_for(&self, item_id: &str) -> Vec<ApprovalLedgerRow> {
        self.rows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|row| row.item_id == item_id)
            .cloned()
            .collect()
    }
}

fn ts(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap()
}

fn text_outcome(text: &str) -> Outcome {
    Outcome {
        cost_usd: 0.0,
        usage: claude_code_rs::parse::Usage {
            input_tokens: 1,
            output_tokens: 1,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
        model_usage: Default::default(),
        text: text.to_string(),
        is_error: false,
        api_error_status: None,
        session_id: None,
        structured_output: None,
    }
}

fn structured_outcome(structured: Value) -> Outcome {
    Outcome {
        cost_usd: 0.0,
        usage: claude_code_rs::parse::Usage {
            input_tokens: 1,
            output_tokens: 1,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
        model_usage: Default::default(),
        text: serde_json::to_string(&structured).unwrap(),
        is_error: false,
        api_error_status: None,
        session_id: None,
        structured_output: Some(structured),
    }
}

/// A counting `ModelTransport` that always replies with a fixed text body.
fn counting_text_transport(body: &'static str, call_count: Arc<AtomicUsize>) -> ModelTransport {
    Arc::new(move |_config: Config, _prompt: String| {
        call_count.fetch_add(1, Ordering::SeqCst);
        async move { Ok(text_outcome(body)) }.boxed()
    })
}

/// A counting `ModelTransport` that always replies with a fixed structured
/// JSON body (for `DecomposePlanNode`/`GenerateTasksForBlockNode`).
fn counting_structured_transport(
    structured: Value,
    call_count: Arc<AtomicUsize>,
) -> ModelTransport {
    Arc::new(move |_config: Config, _prompt: String| {
        let structured = structured.clone();
        let call_count = call_count.clone();
        async move {
            call_count.fetch_add(1, Ordering::SeqCst);
            Ok(structured_outcome(structured))
        }
        .boxed()
    })
}

fn panics_if_called(label: &'static str) -> ModelTransport {
    Arc::new(move |_config: Config, _prompt: String| {
        panic!("model transport '{label}' must never be called on this path")
    })
}

fn one_full_candidate() -> Value {
    json!({
        "candidates": [
            {
                "title": "Fixture candidate block",
                "description": "A candidate produced by the planning_pipeline e2e fixture.",
                "what": "Does the thing.",
                "why": "Because the fixture needs it.",
                "files": ["crates/engine-core/src/fixture.rs"],
                "out_of_scope": ["Everything else."],
                "acceptance_criteria": ["cargo nextest run -p engine-core --lib passes"],
            }
        ]
    })
}

fn one_generated_task() -> Value {
    json!({
        "tasks": [
            { "task_id": 1, "title": "first", "description": "d", "files": [] }
        ],
        "tasks_markdown": "# Tasks\n\n1. first\n",
    })
}

fn queue() -> Arc<Mutex<OperatorQueue>> {
    Arc::new(Mutex::new(OperatorQueue::new(
        OperatorQueuePolicy::default(),
    )))
}

fn write_state_json_block(root: &Path, block: Value) {
    let planning_dir = root.join("planning");
    std::fs::create_dir_all(&planning_dir).unwrap();
    let state = json!({ "tracks": [ { "title": "Track", "blocks": [block] } ] });
    std::fs::write(
        planning_dir.join("state.json"),
        serde_json::to_string_pretty(&state).unwrap(),
    )
    .unwrap();
}

// ---------------------------------------------------------------------------
// Registry construction — mirrors `planning_pipeline::registry_for_stages`'s
// node set exactly, substituting only test-friendly transports/http_post.
// See this file's module doc for why this cannot simply call
// `registry_for_stages` itself.
// ---------------------------------------------------------------------------

/// Duplicates `planning_pipeline::mod.rs`'s private `gate_identity` helper
/// verbatim (`format!("{NODE_NAME}#{stage}")`) — see the module doc.
fn gate_identity(stage: &str) -> String {
    format!("{}#{stage}", approval_gate::NODE_NAME)
}

/// Duplicates `planning_pipeline::mod.rs`'s private `gate_loop_spec` helper:
/// a cap-only backstop cluster (`exit_predicate` always `false`), body entry
/// is the gate itself, exit is the next stage's entry identity.
fn gate_loop_spec(stage: &str, next_entry: &str, max_discussion_rounds: u32) -> LoopSpec {
    let never_exits: ExitPredicate = Arc::new(|_ctx: &TaskContext| false);
    LoopSpec::new(
        format!("PlanningPipelineGate{stage}"),
        max_discussion_rounds.max(1),
        never_exits,
        gate_identity(stage),
        next_entry.to_string(),
    )
}

fn register_gate(
    registry: &mut NodeRegistry,
    stage: &str,
    next_entry: &str,
    op_queue: &Arc<Mutex<OperatorQueue>>,
    policy: &ApprovalGatePolicy,
) {
    let gate_id = gate_identity(stage);
    let gate = ApprovalGateNode::new(stage.to_string(), Arc::clone(op_queue))
        .with_policy(policy.clone())
        .with_identity(gate_id);
    registry.register(Box::new(gate));

    let cluster = build_loop(gate_loop_spec(
        stage,
        next_entry,
        policy.max_discussion_rounds,
    ));
    for node in cluster.nodes {
        registry.register(node);
    }
}

/// Builds the full `{pre_plan, plan, generate_tasks, dispatch}` node set
/// plus all three inter-stage gates — a strict superset of any one dispatch's
/// declared schema (`schema_for_stages` only ever declares a subset of these
/// identities), which `WorkflowValidator::validate` accepts (it only
/// requires every SCHEMA node to be registered, never the reverse). Callers
/// pair this one registry with whichever `schema_for_stages(&stages)` they
/// are testing.
#[allow(clippy::too_many_arguments)]
fn full_registry(
    root: &Path,
    op_queue: Arc<Mutex<OperatorQueue>>,
    policy: &ApprovalGatePolicy,
    research_transport: ModelTransport,
    decompose_transport: ModelTransport,
    generate_tasks_transport: ModelTransport,
    http_post: Arc<dyn HttpPost>,
) -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(StageSelectorNode::new()));

    // -- pre_plan --------------------------------------------------------
    registry.register(Box::new(pp_check_existing::CheckExistingNotesNode::new()));
    registry.register(Box::new(PrePlanNotesAlreadyExistsNode::new()));
    registry.register(Box::new(pp_intake::IntakeIdeaNode::new()));
    registry.register(Box::new(
        pp_research::ResearchCodebaseNode::new().with_transport(research_transport),
    ));
    registry.register(Box::new(pp_secret_guard::SecretGuardNode::new(
        PrePlanPolicy::default(),
    )));
    registry.register(Box::new(pp_write_notes::WriteNotesNode::new()));

    register_gate(
        &mut registry,
        "pre_plan",
        pa_check_existing::NODE_NAME,
        &op_queue,
        policy,
    );

    // -- plan --------------------------------------------------------------
    registry.register(Box::new(
        CheckExistingPlanNode::new().with_brain_root(root.to_path_buf()),
    ));
    registry.register(Box::new(
        GatherPlanContextNode::new()
            .with_brain_root(root.to_path_buf())
            .with_repo_root(root.to_path_buf()),
    ));
    registry.register(Box::new(
        DecomposePlanNode::new().with_transport(decompose_transport),
    ));
    registry.register(Box::new(
        StageCandidateBlocksNode::new()
            .with_brain_root(root.to_path_buf())
            .with_repo_root(root.to_path_buf()),
    ));
    registry.register(Box::new(
        WritePlanNarrativeNode::new().with_brain_root(root.to_path_buf()),
    ));

    register_gate(
        &mut registry,
        "plan",
        generate_tasks_for_block::NODE_NAME,
        &op_queue,
        policy,
    );

    // -- generate_tasks ------------------------------------------------------
    registry.register(Box::new(
        GenerateTasksForBlockNode::new()
            .with_target_root(root.to_path_buf())
            .with_transport(generate_tasks_transport),
    ));

    register_gate(
        &mut registry,
        "generate_tasks",
        dispatch::NODE_NAME,
        &op_queue,
        policy,
    );

    // -- dispatch --------------------------------------------------------
    registry.register(Box::new(
        DispatchNode::new()
            .with_target_root(root.to_path_buf())
            .with_http_post(http_post)
            .with_events_url(DEFAULT_EVENTS_URL)
            .with_api_key("test-key"),
    ));

    registry
}

#[allow(clippy::too_many_arguments)]
async fn run_stages(
    root: &Path,
    op_queue: Arc<Mutex<OperatorQueue>>,
    policy: &ApprovalGatePolicy,
    stages: &[&str],
    research_transport: ModelTransport,
    decompose_transport: ModelTransport,
    generate_tasks_transport: ModelTransport,
    http_post: Arc<dyn HttpPost>,
    mut event: Value,
) -> TaskContext {
    let stage_list: Vec<String> = stages.iter().map(|s| s.to_string()).collect();
    event["stages"] = json!(stage_list);
    let schema = schema_for_stages(&stage_list).expect("valid stage slice");
    let registry = full_registry(
        root,
        op_queue,
        policy,
        research_transport,
        decompose_transport,
        generate_tasks_transport,
        http_post,
    );
    let workflow =
        Workflow::new_validated(registry, schema).expect("composed graph should validate");
    workflow
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("PLANNING_PIPELINE run should complete")
}

fn no_call_transport() -> ModelTransport {
    panics_if_called("unused")
}

fn no_call_http() -> Arc<dyn HttpPost> {
    Arc::new(StubHttpPost::failing("must never be called"))
}

fn resume_state_from(ctx: TaskContext) -> ResumeState {
    let suspension = read_suspension(&ctx.metadata).expect("suspension marker present");
    let resume_at = suspension.resume_at.clone().expect("resume_at present");
    let ledger_snap = suspension.ledger.expect("ledger snapshot present");
    ResumeState {
        ctx,
        at_identity: resume_at,
        ledger: engine_core::BudgetLedger::from_parts(
            ledger_snap.total_tokens,
            ledger_snap.total_cost_usd,
        ),
    }
}

// ---------------------------------------------------------------------------
// AC2: `stages: [pre_plan]` on a fresh slug produces exactly EN.19.A's own
// PRE_PLAN behavior — no gate at all.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn single_pre_plan_stage_runs_exactly_pre_plan_with_no_gate() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    let slug = "fresh-pre-plan-slug";
    let _guard = BrainRootGuard::set(&root);

    let call_count = Arc::new(AtomicUsize::new(0));
    let research = counting_text_transport("VERIFIED — nothing unusual found.", call_count.clone());

    let ctx = run_stages(
        &root,
        queue(),
        &ApprovalGatePolicy::default(),
        &["pre_plan"],
        research,
        no_call_transport(),
        no_call_transport(),
        no_call_http(),
        json!({ "idea": "build a widget", "slug": slug }),
    )
    .await;

    assert_eq!(call_count.load(Ordering::SeqCst), 1);
    assert!(
        read_suspension(&ctx.metadata).is_none(),
        "a single-stage dispatch has no gate to suspend at"
    );
    assert!(
        ctx.nodes
            .keys()
            .all(|identity| !identity.starts_with(approval_gate::NODE_NAME)),
        "no ApprovalGateNode identity should appear for a single requested stage"
    );

    let notes_path = pp_check_existing::notes_path(&root, slug);
    assert!(notes_path.exists(), "notes.md should have been written");
    assert!(
        ctx.nodes.contains_key(pp_write_notes::NODE_NAME),
        "WriteNotesNode should have run"
    );
    assert!(
        !ctx.nodes.contains_key(pa_check_existing::NODE_NAME),
        "plan stage must never have run"
    );
}

// ---------------------------------------------------------------------------
// AC3: `stages: [pre_plan, plan]` produces notes.md AND plan.md/candidate
// blocks, with no generate_tasks/dispatch side effects.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pre_plan_then_plan_produces_notes_and_plan_with_no_further_side_effects() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    let slug = "two-stage-slug";
    let _guard = BrainRootGuard::set(&root);
    std::fs::write(root.join("brain.toml"), BRAIN_TOML).expect("write brain.toml");

    let research_calls = Arc::new(AtomicUsize::new(0));
    let decompose_calls = Arc::new(AtomicUsize::new(0));
    let research = counting_text_transport("VERIFIED — ok.", research_calls.clone());
    let decompose = counting_structured_transport(one_full_candidate(), decompose_calls.clone());

    // pre_plan auto-approved so the run proceeds straight into `plan`
    // without pausing at its gate (this test is about the two stages'
    // combined side effects, not the gate itself — see the dedicated gate
    // tests below).
    let mut approval = std::collections::HashMap::new();
    approval.insert(
        "pre_plan".to_string(),
        engine_core::workflows::planning_pipeline::policy::ApprovalMode::Auto,
    );
    let policy = ApprovalGatePolicy {
        approval,
        ..ApprovalGatePolicy::default()
    };

    let ctx = run_stages(
        &root,
        queue(),
        &policy,
        &["pre_plan", "plan"],
        research,
        decompose,
        no_call_transport(),
        no_call_http(),
        json!({ "idea": "build a widget", "slug": slug }),
    )
    .await;

    assert_eq!(research_calls.load(Ordering::SeqCst), 1);
    assert_eq!(decompose_calls.load(Ordering::SeqCst), 1);
    assert!(
        read_suspension(&ctx.metadata).is_none(),
        "plan is the last requested stage — no gate wraps it, so the run completes"
    );

    let notes_path = pp_check_existing::notes_path(&root, slug);
    assert!(notes_path.exists(), "notes.md should have been written");

    let plan_path = ctx.nodes[pa_write_narrative::NODE_NAME]["plan_path"]
        .as_str()
        .expect("plan_path stamped");
    assert!(
        Path::new(plan_path).exists(),
        "plan.md should have been written"
    );

    let candidate_blocks_dir = root
        .join("planning")
        .join("open-work")
        .join("pre-plan")
        .join(slug)
        .join("candidate-blocks");
    assert!(
        candidate_blocks_dir.is_dir(),
        "candidate-blocks/ should have been staged"
    );
    assert_eq!(
        std::fs::read_dir(&candidate_blocks_dir).unwrap().count(),
        1,
        "one candidate block should be staged"
    );

    assert!(
        !ctx.nodes.contains_key(generate_tasks_for_block::NODE_NAME),
        "generate_tasks must never have run for a [pre_plan, plan] dispatch"
    );
    assert!(
        !ctx.nodes.contains_key(dispatch::NODE_NAME),
        "dispatch must never have run for a [pre_plan, plan] dispatch"
    );
    assert!(
        !root.join("planning").join(slug).join("tasks.json").exists(),
        "no tasks.json should exist — generate_tasks was never requested"
    );
}

// ---------------------------------------------------------------------------
// AC4: `stages: [plan, generate_tasks, dispatch]` against a slug whose
// notes.md already exists skips pre_plan (it was never requested), produces
// plan.md/candidates, then tasks.json, then a real dispatch returning a
// run_id.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn plan_generate_tasks_dispatch_produces_tasks_and_dispatches_with_a_run_id() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    let slug = "plan-to-dispatch-slug";
    let _guard = BrainRootGuard::set(&root);
    std::fs::write(root.join("brain.toml"), BRAIN_TOML).expect("write brain.toml");

    // A pre-existing notes.md — proves this dispatch never needed (and
    // never ran) pre_plan to produce it.
    let notes_path = pp_check_existing::notes_path(&root, slug);
    std::fs::create_dir_all(notes_path.parent().unwrap()).unwrap();
    std::fs::write(&notes_path, "# pre-existing notes\n").unwrap();
    let notes_before = std::fs::read_to_string(&notes_path).unwrap();

    write_state_json_block(
        &root,
        json!({ "id": slug, "status": "open", "sdlc_workflow": "flow" }),
    );

    let decompose_calls = Arc::new(AtomicUsize::new(0));
    let generate_tasks_calls = Arc::new(AtomicUsize::new(0));
    let decompose = counting_structured_transport(one_full_candidate(), decompose_calls.clone());
    let generate_tasks =
        counting_structured_transport(one_generated_task(), generate_tasks_calls.clone());
    let http_post = Arc::new(StubHttpPost::succeeding(json!({
        "run_id": "11111111-1111-1111-1111-111111111111",
        "event_id": "11111111-1111-1111-1111-111111111111",
    })));

    // Every requested stage auto-approved so the run proceeds all the way
    // through to dispatch without pausing (the gate-pausing behavior is
    // covered by its own dedicated tests below).
    let mut approval = std::collections::HashMap::new();
    approval.insert(
        "plan".to_string(),
        engine_core::workflows::planning_pipeline::policy::ApprovalMode::Auto,
    );
    approval.insert(
        "generate_tasks".to_string(),
        engine_core::workflows::planning_pipeline::policy::ApprovalMode::Auto,
    );
    let policy = ApprovalGatePolicy {
        approval,
        ..ApprovalGatePolicy::default()
    };

    let ctx = run_stages(
        &root,
        queue(),
        &policy,
        &["plan", "generate_tasks", "dispatch"],
        no_call_transport(),
        decompose,
        generate_tasks,
        http_post.clone(),
        json!({ "slug": slug }),
    )
    .await;

    assert_eq!(decompose_calls.load(Ordering::SeqCst), 1);
    assert_eq!(generate_tasks_calls.load(Ordering::SeqCst), 1);
    assert!(
        read_suspension(&ctx.metadata).is_none(),
        "no gate wraps the last stage"
    );

    // pre_plan never ran: notes.md is byte-identical to the fixture.
    assert_eq!(std::fs::read_to_string(&notes_path).unwrap(), notes_before);

    let plan_path = ctx.nodes[pa_write_narrative::NODE_NAME]["plan_path"]
        .as_str()
        .expect("plan_path stamped");
    assert!(Path::new(plan_path).exists());

    let tasks_json = root.join("planning").join(slug).join("tasks.json");
    assert!(tasks_json.is_file(), "tasks.json should have been written");
    assert!(
        root.join("planning").join(slug).join("tasks.md").is_file(),
        "tasks.md should have been written"
    );

    let dispatched = ctx.nodes[dispatch::NODE_NAME].clone();
    assert_eq!(dispatched["refused"], json!(false));
    assert_eq!(
        dispatched["run_id"],
        json!("11111111-1111-1111-1111-111111111111")
    );
    let (_, body) = http_post.last_call().expect("dispatch POST recorded");
    assert_eq!(body["data"]["spec_slug"], json!(slug));
}

// ---------------------------------------------------------------------------
// AC5: a gap/out-of-order `stages` list is rejected before any `Workflow` is
// ever built — "dispatches nothing" by construction.
// ---------------------------------------------------------------------------

/// `Workflow` (`workflow_for_stages`'s `Ok` type) is not `Debug`, so this
/// helper extracts the `Err` side by hand instead of `.expect_err(..)`.
fn expect_workflow_for_stages_err(stages: &[String], policy: &ApprovalGatePolicy) -> String {
    match workflow_for_stages(stages, queue(), policy) {
        Ok(_) => panic!("expected stages {stages:?} to be rejected, but a Workflow was built"),
        Err(message) => message,
    }
}

#[test]
fn workflow_for_stages_rejects_a_gap_and_builds_nothing() {
    let err = expect_workflow_for_stages_err(
        &["pre_plan".to_string(), "dispatch".to_string()],
        &ApprovalGatePolicy::default(),
    );
    assert!(err.contains("gap"), "unexpected message: {err}");
}

#[test]
fn workflow_for_stages_rejects_an_out_of_order_list_and_builds_nothing() {
    let err = expect_workflow_for_stages_err(
        &["dispatch".to_string(), "pre_plan".to_string()],
        &ApprovalGatePolicy::default(),
    );
    assert!(err.contains("out of order"), "unexpected message: {err}");
}

#[test]
fn workflow_for_stages_rejects_an_empty_list_and_builds_nothing() {
    let err = expect_workflow_for_stages_err(&[], &ApprovalGatePolicy::default());
    match registry_for_stages(&[], queue(), &ApprovalGatePolicy::default()) {
        Ok(_) => panic!("expected an empty stages list to be rejected by registry_for_stages too"),
        Err((reason, _message)) => assert_eq!(reason, stage_selector::reject_reason::EMPTY),
    }
    assert!(err.contains("non-empty"), "unexpected message: {err}");
}

// ---------------------------------------------------------------------------
// AC6: re-dispatching the same slug/stages after success short-circuits
// every stage via its own idempotency guard, with zero further model calls.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn redispatch_short_circuits_every_stage_with_zero_further_model_calls() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    let slug = "resumed-idempotent-slug";
    let _guard = BrainRootGuard::set(&root);
    std::fs::write(root.join("brain.toml"), BRAIN_TOML).expect("write brain.toml");

    let mut approval = std::collections::HashMap::new();
    approval.insert(
        "pre_plan".to_string(),
        engine_core::workflows::planning_pipeline::policy::ApprovalMode::Auto,
    );
    let policy = ApprovalGatePolicy {
        approval,
        ..ApprovalGatePolicy::default()
    };

    let research_calls = Arc::new(AtomicUsize::new(0));
    let decompose_calls = Arc::new(AtomicUsize::new(0));
    let first_ctx = run_stages(
        &root,
        queue(),
        &policy,
        &["pre_plan", "plan"],
        counting_text_transport("VERIFIED — ok.", research_calls.clone()),
        counting_structured_transport(one_full_candidate(), decompose_calls.clone()),
        no_call_transport(),
        no_call_http(),
        json!({ "idea": "build a widget", "slug": slug }),
    )
    .await;
    assert_eq!(research_calls.load(Ordering::SeqCst), 1);
    assert_eq!(decompose_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        first_ctx.nodes[pa_check_existing::NODE_NAME]["short_circuit"],
        json!(false)
    );

    // Second dispatch, identical slug/stages: both idempotency guards
    // (CheckExistingNotesNode, CheckExistingPlanNode) must short-circuit
    // before either model-calling node ever runs again.
    let second_ctx = run_stages(
        &root,
        queue(),
        &policy,
        &["pre_plan", "plan"],
        panics_if_called("research (second dispatch)"),
        panics_if_called("decompose (second dispatch)"),
        no_call_transport(),
        no_call_http(),
        json!({ "idea": "build a widget", "slug": slug }),
    )
    .await;

    assert_eq!(
        research_calls.load(Ordering::SeqCst),
        1,
        "redispatch must make zero further research calls"
    );
    assert_eq!(
        decompose_calls.load(Ordering::SeqCst),
        1,
        "redispatch must make zero further decompose calls"
    );
    assert_eq!(
        second_ctx.nodes[pp_check_existing::EXISTS_ROUTE]["already_exists"],
        json!(true)
    );
    assert_eq!(
        second_ctx.nodes[pa_check_existing::NODE_NAME]["short_circuit"],
        json!(true)
    );
}

// ---------------------------------------------------------------------------
// AC7: `DispatchNode` refuses a block already in_progress/closed, and
// permits + returns a run_id for an eligible one.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dispatch_alone_refuses_an_in_progress_block_and_posts_nothing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    let slug = "already-running-slug";
    write_state_json_block(
        &root,
        json!({ "id": slug, "status": "in_progress", "sdlc_workflow": "flow" }),
    );

    let http_post = Arc::new(StubHttpPost::failing("must never be called"));

    let ctx = run_stages(
        &root,
        queue(),
        &ApprovalGatePolicy::default(),
        &["dispatch"],
        no_call_transport(),
        no_call_transport(),
        no_call_transport(),
        http_post.clone(),
        json!({ "slug": slug }),
    )
    .await;

    let run = ctx
        .node_runs
        .get(dispatch::NODE_NAME)
        .expect("DispatchNode should have run and failed");
    assert_eq!(run.status, engine_contract::NodeRunStatus::Failed);
    assert!(
        http_post.last_call().is_none(),
        "no HTTP call should be made"
    );
}

#[tokio::test]
async fn dispatch_alone_permits_an_open_block_and_returns_a_run_id() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    let slug = "eligible-slug";
    write_state_json_block(
        &root,
        json!({ "id": slug, "status": "open", "sdlc_workflow": "task" }),
    );

    let http_post = Arc::new(StubHttpPost::succeeding(json!({
        "run_id": "22222222-2222-2222-2222-222222222222",
        "event_id": "22222222-2222-2222-2222-222222222222",
    })));

    let ctx = run_stages(
        &root,
        queue(),
        &ApprovalGatePolicy::default(),
        &["dispatch"],
        no_call_transport(),
        no_call_transport(),
        no_call_transport(),
        http_post,
        json!({ "slug": slug }),
    )
    .await;

    let dispatched = &ctx.nodes[dispatch::NODE_NAME];
    assert_eq!(dispatched["refused"], json!(false));
    assert_eq!(dispatched["workflow_type"], json!("SDLC_TASK"));
    assert_eq!(
        dispatched["run_id"],
        json!("22222222-2222-2222-2222-222222222222")
    );
}

// ---------------------------------------------------------------------------
// AC8: an event naming no `approval` map pauses after EVERY requested
// stage's gate.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_approval_map_pauses_after_the_first_stages_gate() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    let slug = "safe-by-default-slug";
    let _guard = BrainRootGuard::set(&root);
    std::fs::write(root.join("brain.toml"), BRAIN_TOML).expect("write brain.toml");

    let research_calls = Arc::new(AtomicUsize::new(0));
    let ctx = run_stages(
        &root,
        queue(),
        &ApprovalGatePolicy::default(), // no override at all: every stage manual
        &["pre_plan", "plan"],
        counting_text_transport("VERIFIED — ok.", research_calls.clone()),
        panics_if_called("decompose must not run before the pre_plan gate resolves"),
        no_call_transport(),
        no_call_http(),
        json!({ "idea": "build a widget", "slug": slug }),
    )
    .await;

    assert_eq!(
        research_calls.load(Ordering::SeqCst),
        1,
        "pre_plan itself still runs"
    );
    let suspension = read_suspension(&ctx.metadata).expect("run must suspend at the gate");
    assert!(suspension.suspended);
    assert_eq!(
        suspension.resume_at.as_deref(),
        Some("PlanningPipelineGatepre_planGuard"),
        "resume pointer is the gate's own loop-guard identity"
    );
    assert_eq!(
        ctx.node_runs[pa_check_existing::NODE_NAME].status,
        engine_contract::NodeRunStatus::Pending,
        "the plan stage must never have started while pre_plan's gate is open"
    );
}

#[tokio::test]
async fn manual_gate_also_pauses_after_a_later_stage_when_only_that_stage_is_manual() {
    // A gate is inserted AFTER a completed stage (it decides whether the run
    // proceeds to the NEXT one), never before — so to observe the gate
    // *after* `generate_tasks`, `dispatch` must also be requested (it is the
    // stage that gate would otherwise let the run continue into).
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    let slug = "later-gate-slug";
    let _guard = BrainRootGuard::set(&root);
    std::fs::write(root.join("brain.toml"), BRAIN_TOML).expect("write brain.toml");
    write_state_json_block(
        &root,
        json!({ "id": slug, "status": "open", "sdlc_workflow": "flow" }),
    );

    let mut approval = std::collections::HashMap::new();
    approval.insert(
        "plan".to_string(),
        engine_core::workflows::planning_pipeline::policy::ApprovalMode::Auto,
    );
    // "generate_tasks" is deliberately absent from the map — must still
    // resolve to Manual (safe by default) and pause there, before `dispatch`
    // ever runs.
    let policy = ApprovalGatePolicy {
        approval,
        ..ApprovalGatePolicy::default()
    };

    let decompose_calls = Arc::new(AtomicUsize::new(0));
    let generate_tasks_calls = Arc::new(AtomicUsize::new(0));
    let ctx = run_stages(
        &root,
        queue(),
        &policy,
        &["plan", "generate_tasks", "dispatch"],
        no_call_transport(),
        counting_structured_transport(one_full_candidate(), decompose_calls.clone()),
        counting_structured_transport(one_generated_task(), generate_tasks_calls.clone()),
        no_call_http(),
        json!({ "slug": slug }),
    )
    .await;

    assert_eq!(decompose_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        generate_tasks_calls.load(Ordering::SeqCst),
        1,
        "generate_tasks itself must still run — the gate sits AFTER it, not before"
    );
    let suspension =
        read_suspension(&ctx.metadata).expect("run must suspend at generate_tasks' own gate");
    assert!(suspension.suspended);
    assert!(
        !ctx.nodes.contains_key(dispatch::NODE_NAME),
        "dispatch must never have run while generate_tasks' own gate is open"
    );
}

// ---------------------------------------------------------------------------
// AC9/AC10: approve/reject/discuss verdicts recorded distinctly through
// `operator::ledger::record_decision`, readable back; a stale digest is
// refused and requeued, never authorizing continuation.
// ---------------------------------------------------------------------------

/// Drives a fresh `[pre_plan]`-then-implicit-gate-less single stage dispatch
/// far enough to have a REAL gate item enqueued on `op_queue` under the
/// deterministic `gate_id_for(slug, "pre_plan")` — reusing the real
/// `ApprovalGateNode::process` path (via a two-stage dispatch that suspends
/// at its only gate) rather than hand-constructing an `OperatorQueueItem`.
async fn suspend_at_pre_plan_gate(root: &Path, slug: &str, op_queue: Arc<Mutex<OperatorQueue>>) {
    let _ = run_stages(
        root,
        Arc::clone(&op_queue),
        &ApprovalGatePolicy::default(),
        &["pre_plan", "plan"],
        counting_text_transport("VERIFIED — ok.", Arc::new(AtomicUsize::new(0))),
        panics_if_called("decompose must not run before the gate resolves"),
        no_call_transport(),
        no_call_http(),
        json!({ "idea": "build a widget", "slug": slug }),
    )
    .await;

    // Deliver the enqueued item so it can be resolved.
    let mut locked = op_queue.lock().unwrap_or_else(|e| e.into_inner());
    locked
        .next_deliverable(Utc::now())
        .expect("the gate's enqueued item is deliverable");
}

#[tokio::test]
async fn approve_verdict_is_recorded_and_authorizes_continuation_readable_back() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    let slug = "approve-verdict-slug";
    let _guard = BrainRootGuard::set(&root);
    std::fs::write(root.join("brain.toml"), BRAIN_TOML).expect("write brain.toml");

    let op_queue = queue();
    suspend_at_pre_plan_gate(&root, slug, Arc::clone(&op_queue)).await;

    let gate_id = approval_gate::gate_id_for(slug, "pre_plan");
    let digest = approval_gate::render(slug, "pre_plan").digest;
    let ledger = TestApprovalLedger::new();

    let outcome = approval_gate::resolve_verdict(
        &mut op_queue.lock().unwrap_or_else(|e| e.into_inner()),
        &ledger,
        &gate_id,
        &digest,
        PLANNING_PIPELINE_APPROVE,
        "operator-a",
        ts(10),
    )
    .expect("approve is a known option");

    assert!(outcome.should_continue());
    assert!(!outcome.is_rejected());
    assert!(!outcome.is_discuss());

    let rows = ledger.rows_for(&gate_id);
    assert_eq!(
        rows.len(),
        1,
        "exactly one row must be readable back for this gate"
    );
    assert_eq!(rows[0].decision, LedgerDecision::Approved);
    assert_eq!(rows[0].who, "operator-a");
}

#[tokio::test]
async fn reject_and_discuss_verdicts_are_recorded_distinctly_and_never_authorize() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    let slug_reject = "reject-verdict-slug";
    let slug_discuss = "discuss-verdict-slug";
    let _guard = BrainRootGuard::set(&root);
    std::fs::write(root.join("brain.toml"), BRAIN_TOML).expect("write brain.toml");

    let ledger = TestApprovalLedger::new();

    // -- reject ------------------------------------------------------------
    let reject_queue = queue();
    suspend_at_pre_plan_gate(&root, slug_reject, Arc::clone(&reject_queue)).await;
    let reject_gate_id = approval_gate::gate_id_for(slug_reject, "pre_plan");
    let reject_digest = approval_gate::render(slug_reject, "pre_plan").digest;
    let reject_outcome = approval_gate::resolve_verdict(
        &mut reject_queue.lock().unwrap_or_else(|e| e.into_inner()),
        &ledger,
        &reject_gate_id,
        &reject_digest,
        PLANNING_PIPELINE_REJECT,
        "operator-a",
        ts(20),
    )
    .expect("reject is a known option");
    assert!(!reject_outcome.should_continue());
    assert!(reject_outcome.is_rejected());
    assert!(!reject_outcome.is_discuss());

    // -- discuss -------------------------------------------------------------
    let discuss_queue = queue();
    suspend_at_pre_plan_gate(&root, slug_discuss, Arc::clone(&discuss_queue)).await;
    let discuss_gate_id = approval_gate::gate_id_for(slug_discuss, "pre_plan");
    let discuss_digest = approval_gate::render(slug_discuss, "pre_plan").digest;
    let discuss_outcome = approval_gate::resolve_verdict(
        &mut discuss_queue.lock().unwrap_or_else(|e| e.into_inner()),
        &ledger,
        &discuss_gate_id,
        &discuss_digest,
        PLANNING_PIPELINE_DISCUSS,
        "operator-a",
        ts(30),
    )
    .expect("discuss is a known option");
    assert!(!discuss_outcome.should_continue());
    assert!(!discuss_outcome.is_rejected());
    assert!(discuss_outcome.is_discuss());

    // -- both rows are readable back, distinctly ----------------------------
    let all_rows = ledger.read_all();
    assert_eq!(all_rows.len(), 2);
    assert_eq!(
        ledger.rows_for(&reject_gate_id)[0].decision,
        LedgerDecision::Rejected
    );
    assert_eq!(
        ledger.rows_for(&discuss_gate_id)[0].decision,
        LedgerDecision::RoutedToDiscussion
    );
    assert_ne!(
        ledger.rows_for(&reject_gate_id)[0].decision,
        ledger.rows_for(&discuss_gate_id)[0].decision
    );
}

#[tokio::test]
async fn stale_digest_tap_is_refused_and_requeued_never_authorizing_continuation() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    let slug = "stale-digest-slug";
    let _guard = BrainRootGuard::set(&root);
    std::fs::write(root.join("brain.toml"), BRAIN_TOML).expect("write brain.toml");

    let op_queue = queue();
    suspend_at_pre_plan_gate(&root, slug, Arc::clone(&op_queue)).await;

    let gate_id = approval_gate::gate_id_for(slug, "pre_plan");
    let ledger = TestApprovalLedger::new();

    let outcome = approval_gate::resolve_verdict(
        &mut op_queue.lock().unwrap_or_else(|e| e.into_inner()),
        &ledger,
        &gate_id,
        "a-stale-digest-not-what-was-delivered",
        PLANNING_PIPELINE_APPROVE,
        "operator-a",
        ts(40),
    )
    .expect("a stale digest still resolves, as a requeue");

    assert!(!outcome.should_continue());
    assert!(outcome.requeued);
    assert_eq!(ledger.read_all()[0].decision, LedgerDecision::Requeued);

    let locked = op_queue.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(
        locked.pending_count(),
        1,
        "the item must be pushed back onto pending"
    );
    assert_eq!(locked.open_count(), 0);
    assert!(
        locked.open_item(&gate_id).is_none(),
        "a stale-digest tap must never authorize continuation"
    );
}

// ---------------------------------------------------------------------------
// `tests/it/main.rs` module registration sanity — this suite is a module of
// the shared integration binary, never a new `tests/*.rs` file (standing
// rule 9). Proven structurally by this file compiling at all under
// `mod planning_pipeline;` in `tests/it/main.rs`; this test just pins the
// resume machinery this suite depends on stays reachable from here.
// ---------------------------------------------------------------------------

#[test]
fn resume_state_from_round_trips_a_real_suspension_marker_shape() {
    // A minimal sanity check that `read_suspension`/`ResumeState` are wired
    // correctly in this file, independent of any full pipeline run.
    let mut ctx = TaskContext {
        event: json!({}),
        nodes: Default::default(),
        metadata: json!({}),
        node_runs: Default::default(),
    };
    engine_core::stamp_suspended(
        &mut ctx.metadata,
        engine_core::Suspension {
            resume_at: "SomeNode",
            reason: engine_core::SuspendReason::SuspendNode,
            origin_identity: Some("Origin"),
            ledger: &engine_core::BudgetLedger::new(),
        },
    );
    let state = resume_state_from(ctx);
    assert_eq!(state.at_identity, "SomeNode");
}

// Silence an unused-import warning on `RunOptions` — imported for parity
// with this suite's own module doc (a future resumed-dispatch test may use
// it directly), kept available rather than re-imported piecemeal later.
#[allow(dead_code)]
fn _unused_run_options_reference(_options: RunOptions) {}
