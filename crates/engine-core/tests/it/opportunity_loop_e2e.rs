//! Hermetic end-to-end test of the closed RESEARCH -> opportunity loop
//! (`EN.7.B` task 7) — the real `Workflow::run` pointer-walk loop for
//! `RESEARCH_AGENT` (stubbed model transport, REAL `MevDocMaterializer`
//! against a `tempfile::tempdir()` corpus) followed by the real
//! `OPPORTUNITY_SET_STAGE` / `OPPORTUNITY_ADD_ACTION` micro-workflows over
//! the same corpus.
//!
//! Follows `research_agent_e2e.rs` for the stub-transport registry pattern
//! and `materialize_doc.rs` for the tempdir-corpus pattern (pre-create
//! `<root>/business/docs/opportunities/` before any write). Unlike
//! `research_agent_e2e.rs`'s `drive()` helper (which drives nodes directly,
//! bypassing `Workflow::run`'s walk loop, per that file's module doc), this
//! suite drives the REAL `Workflow::run` for `RESEARCH_AGENT` — its module
//! doc's rationale for avoiding `Workflow::run` (no way to pre-stamp a
//! `SetupWorktreeNode` result / controlled worktree) does not apply here:
//! `RESEARCH_AGENT` has no setup node and needs no worktree, only a brain
//! root, which is passed explicitly to `MaterializeDocNode::with_brain_root`
//! (never via `ENGINE_BRAIN_ROOT`, so this suite stays hermetic and immune
//! to any other test's env-var mutation).
//!
//! Every test is hermetic: no network, and no write anywhere outside its
//! own tempdir — in particular nothing under the real `agentic-portfolio`
//! corpus.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use claude_code_rs::{Config, Outcome};
use engine_contract::{NodeRunStatus, TaskContext};
use engine_core::node::{NodeExt, NodeRegistry};
use engine_core::nodes::opportunity_edit::{OpportunityEditNode, OpportunityEditOp};
use engine_core::policy::{PolicyConfigSource, RESOLVED_POLICY_IDENTITY};
use engine_core::workflow::Workflow;
use engine_core::workflows::opportunity_edit::graph as edit_graph;
use engine_core::workflows::research_agent::graph as research_graph;
use engine_core::workflows::research_agent::ingress_dispatch::ResearchIngressDispatchNode;
use engine_core::workflows::research_agent::profiles;
use engine_core::workflows::research_agent::prospecting::ProspectingResearchNode;
use engine_core::workflows::research_agent::CompanyResearchNode;
use futures::FutureExt;
use okf_core::{parse_nested_frontmatter, Opportunity};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Fixtures / transports
// ---------------------------------------------------------------------------

fn fixture_company_brief() -> Value {
    let raw = std::fs::read_to_string("tests/fixtures/company_brief.json").expect("fixture");
    serde_json::from_str(&raw).expect("valid JSON")
}

fn fixture_prospecting_result() -> Value {
    let raw = std::fs::read_to_string("tests/fixtures/prospecting_result.json").expect("fixture");
    serde_json::from_str(&raw).expect("valid JSON")
}

fn stub_outcome(structured: Value, input_tokens: u64, output_tokens: u64) -> Outcome {
    Outcome {
        cost_usd: 0.02,
        usage: claude_code_rs::parse::Usage {
            input_tokens,
            output_tokens,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
        model_usage: [(
            "claude-sonnet-4-5".to_string(),
            claude_code_rs::parse::ModelUsage {
                input_tokens,
                output_tokens,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                cost_usd: 0.02,
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

fn stub_company_transport() -> engine_core::workflows::ModelTransport {
    Arc::new(|_config: Config, _prompt: String| {
        let brief = fixture_company_brief();
        async move { Ok(stub_outcome(brief, 100, 50)) }.boxed()
    })
}

fn stub_prospecting_transport() -> engine_core::workflows::ModelTransport {
    Arc::new(|_config: Config, _prompt: String| {
        let result = fixture_prospecting_result();
        async move { Ok(stub_outcome(result, 120, 80)) }.boxed()
    })
}

/// Pre-create `<root>/business/docs/opportunities/`, mirroring
/// `materialize_doc.rs::opportunities_dir`.
fn opportunities_dir(root: &Path) -> std::path::PathBuf {
    let dir = root.join("business/docs/opportunities");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---------------------------------------------------------------------------
// RESEARCH_AGENT: real Workflow::run, stubbed transports, real materializer
// ---------------------------------------------------------------------------

/// Build a fresh `RESEARCH_AGENT` registry: both research branches wired to
/// a stubbed transport, `MaterializeDocNode` pinned at `root` with the real
/// `MevDocMaterializer` (`MaterializeDocNode::new`'s default).
fn research_registry(root: &Path) -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(research_graph::ResearchModeRouterNode));
    registry.register(Box::new(
        CompanyResearchNode::new().with_transport(stub_company_transport()),
    ));
    registry.register(Box::new(
        ProspectingResearchNode::new().with_transport(stub_prospecting_transport()),
    ));
    registry.register(Box::new(
        engine_core::nodes::materialize_doc::MaterializeDocNode::new("opportunity")
            .with_brain_root(root)
            .with_source_nodes(["CompanyResearchNode", "ProspectingResearchNode"]),
    ));
    registry.register(Box::new(
        engine_core::nodes::merge_contacts::MergeContactsNode::new()
            .with_brain_root(root)
            .with_source_nodes(["CompanyResearchNode", "ProspectingResearchNode"]),
    ));
    // `EN.6.E`: the graph's new sole terminal identity. A stub transport
    // keeps this suite hermetic (no network); the resolved policy stamp
    // seeded by `seeded_nodes_for` carries the built-in `enabled: false`
    // default, so this node no-ops in place and records zero sends.
    registry.register(Box::new(ResearchIngressDispatchNode::new().with_transport(
        Arc::new(engine_core::nodes::channel_transport::StubChannelTransport::succeeding()),
    )));
    registry
}

/// Resolve `event`'s `RESEARCH_AGENT` policy the same way
/// `engine-serve::workflows::register_research_agent`'s factory does, and
/// seed it via `Workflow::with_seeded_nodes` — required since
/// `CompanyResearchNode`/`ProspectingResearchNode::process` read the policy
/// via `crate::policy::resolved_policy_strict`, which errors if nothing was
/// stamped/seeded first (EN.5.D task 8: no per-node re-resolution). Also
/// seeds a `SetupWorktreeNode` result pointing at `root` so
/// `persist_state`'s `planning/research-agent-state.json` write lands
/// inside this test's own tempdir rather than falling back to the process's
/// current working directory (this suite's hermeticity requirement).
fn seeded_nodes_for(root: &Path, event: &Value) -> HashMap<String, Value> {
    let probe_ctx = TaskContext {
        event: event.clone(),
        nodes: HashMap::new(),
        metadata: json!({}),
        node_runs: HashMap::new(),
    };
    let policy = profiles::resolve_policy_for_run_from(&probe_ctx, &PolicyConfigSource::Builtin)
        .expect("RESEARCH_AGENT policy should resolve for this event");
    let mut seeded = HashMap::new();
    seeded.insert(
        RESOLVED_POLICY_IDENTITY.to_string(),
        serde_json::to_value(&policy).expect("policy should serialize"),
    );
    seeded.insert(
        "SetupWorktreeNode".to_string(),
        json!({ "worktree_path": root.to_string_lossy() }),
    );
    seeded
}

/// Build the runnable `RESEARCH_AGENT` `Workflow` for this test: real
/// declared graph + registry pinned at `root`, seeded with `event`'s
/// resolved policy.
fn research_workflow(root: &Path, event: &Value) -> Workflow {
    let seeded = seeded_nodes_for(root, event);
    Workflow::new_validated(research_registry(root), research_graph::schema())
        .expect("RESEARCH_AGENT declared graph should validate")
        .with_seeded_nodes(seeded)
}

async fn run_research(root: &Path, event: Value) -> TaskContext {
    let workflow = research_workflow(root, &event);
    workflow
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("RESEARCH_AGENT run should complete")
}

fn company_event() -> Value {
    json!({
        "mode": "company",
        "company_name": "Anthropic",
        "company_url": "https://anthropic.com",
    })
}

fn prospecting_event() -> Value {
    json!({
        "mode": "prospecting",
        "vertical": "legal-tech",
        "topic": "contract review pain points",
    })
}

// ---------------------------------------------------------------------------
// Opportunity-edit micro-workflows: real Workflow::run, no policy layer
// ---------------------------------------------------------------------------

fn set_stage_workflow_at(root: &Path) -> Workflow {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(
        OpportunityEditNode::new(OpportunityEditOp::SetStage)
            .with_brain_root(root)
            .with_identity(edit_graph::SET_STAGE_NODE_NAME),
    ));
    Workflow::new_validated(registry, edit_graph::set_stage_schema())
        .expect("OPPORTUNITY_SET_STAGE declared graph should validate")
}

fn add_action_workflow_at(root: &Path) -> Workflow {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(
        OpportunityEditNode::new(OpportunityEditOp::AddAction)
            .with_brain_root(root)
            .with_identity(edit_graph::ADD_ACTION_NODE_NAME),
    ));
    Workflow::new_validated(registry, edit_graph::add_action_schema())
        .expect("OPPORTUNITY_ADD_ACTION declared graph should validate")
}

async fn run_set_stage(root: &Path, event: Value) -> TaskContext {
    set_stage_workflow_at(root)
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("OPPORTUNITY_SET_STAGE run should complete")
}

async fn run_add_action(root: &Path, event: Value) -> TaskContext {
    add_action_workflow_at(root)
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("OPPORTUNITY_ADD_ACTION run should complete")
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

#[tokio::test]
async fn company_branch_closes_the_loop() {
    let tmp = tempfile::tempdir().expect("tempdir");
    opportunities_dir(tmp.path());

    let ctx = run_research(tmp.path(), company_event()).await;

    let run = &ctx.node_runs["MaterializeDocNode"];
    assert_eq!(run.status, NodeRunStatus::Success);

    let result = &ctx.nodes["MaterializeDocNode"];
    assert_eq!(result["materialized"], json!(true));
    let paths = result["paths"].as_array().expect("paths array");
    assert_eq!(paths.len(), 1);
    let written_path = paths[0].as_str().expect("path string");
    let written_path = Path::new(written_path);
    assert!(written_path.exists(), "expected {written_path:?} to exist");

    let content = std::fs::read_to_string(written_path).expect("readable");
    let fields = parse_nested_frontmatter(&content).expect("must parse frontmatter");
    let opp = Opportunity::from_frontmatter(&fields).expect("must reconstruct");
    assert_eq!(opp.title, "Anthropic");
    assert_eq!(opp.kind.as_deref(), Some("company"));
    assert_eq!(opp.stage.as_deref(), Some("identified"));
    assert!(content.contains("type: Opportunity"));
}

#[tokio::test]
async fn prospecting_branch_closes_the_loop() {
    let tmp = tempfile::tempdir().expect("tempdir");
    opportunities_dir(tmp.path());

    let ctx = run_research(tmp.path(), prospecting_event()).await;

    let run = &ctx.node_runs["MaterializeDocNode"];
    assert_eq!(run.status, NodeRunStatus::Success);

    let result = &ctx.nodes["MaterializeDocNode"];
    assert_eq!(result["materialized"], json!(true));
    let paths = result["paths"].as_array().expect("paths array");
    assert_eq!(paths.len(), 1);
    let written_path = paths[0].as_str().expect("path string");
    let written_path = Path::new(written_path);
    assert!(written_path.exists(), "expected {written_path:?} to exist");

    let content = std::fs::read_to_string(written_path).expect("readable");
    let fields = parse_nested_frontmatter(&content).expect("must parse frontmatter");
    let opp = Opportunity::from_frontmatter(&fields).expect("must reconstruct");
    assert_eq!(opp.title, "legal-tech — Prospecting Sweep");
    assert_eq!(opp.kind.as_deref(), Some("prospecting-sweep"));
    assert_eq!(opp.stage.as_deref(), Some("identified"));
}

#[tokio::test]
async fn research_re_run_is_byte_idempotent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    opportunities_dir(tmp.path());

    let ctx1 = run_research(tmp.path(), company_event()).await;
    let path = ctx1.nodes["MaterializeDocNode"]["paths"][0]
        .as_str()
        .expect("path")
        .to_string();
    let bytes_after_first = std::fs::read(&path).expect("file exists after first run");

    run_research(tmp.path(), company_event()).await;
    let bytes_after_second = std::fs::read(&path).expect("file exists after second run");

    assert_eq!(
        bytes_after_first, bytes_after_second,
        "re-running RESEARCH_AGENT over the same corpus must be idempotent"
    );
}

#[tokio::test]
async fn set_stage_workflow_changes_stage_and_is_idempotent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    opportunities_dir(tmp.path());

    run_research(tmp.path(), company_event()).await;
    let path = tmp.path().join("business/docs/opportunities/anthropic.md");
    let before = std::fs::read_to_string(&path).expect("readable");
    assert!(before.contains("stage: identified"));

    let ctx = run_set_stage(
        tmp.path(),
        json!({ "slug": "anthropic", "stage": "contacted" }),
    )
    .await;
    let run = &ctx.node_runs[edit_graph::SET_STAGE_NODE_NAME];
    assert_eq!(run.status, NodeRunStatus::Success);
    let result = &ctx.nodes[edit_graph::SET_STAGE_NODE_NAME];
    assert_eq!(result["edited"], json!(true));
    assert_eq!(result["no_op"], json!(false));

    let after = std::fs::read_to_string(&path).expect("readable");
    assert!(after.contains("stage: contacted"));
    assert!(!after.contains("stage: identified"));

    // Repeat: identical stage plans zero actions, leaves bytes unchanged.
    let bytes_before_repeat = std::fs::read(&path).expect("readable");
    let ctx2 = run_set_stage(
        tmp.path(),
        json!({ "slug": "anthropic", "stage": "contacted" }),
    )
    .await;
    let run2 = &ctx2.node_runs[edit_graph::SET_STAGE_NODE_NAME];
    assert_eq!(run2.status, NodeRunStatus::Success);
    let result2 = &ctx2.nodes[edit_graph::SET_STAGE_NODE_NAME];
    assert_eq!(result2["no_op"], json!(true));
    let bytes_after_repeat = std::fs::read(&path).expect("readable");
    assert_eq!(bytes_before_repeat, bytes_after_repeat);
}

#[tokio::test]
async fn add_action_workflow_appends_one_entry_and_is_idempotent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    opportunities_dir(tmp.path());

    run_research(tmp.path(), company_event()).await;
    let path = tmp.path().join("business/docs/opportunities/anthropic.md");

    let action_event = json!({
        "slug": "anthropic",
        "at": "2026-07-27",
        "kind": "email",
        "note": "Sent intro email",
    });

    let ctx = run_add_action(tmp.path(), action_event.clone()).await;
    let run = &ctx.node_runs[edit_graph::ADD_ACTION_NODE_NAME];
    assert_eq!(run.status, NodeRunStatus::Success);
    let result = &ctx.nodes[edit_graph::ADD_ACTION_NODE_NAME];
    assert_eq!(result["edited"], json!(true));
    assert_eq!(result["no_op"], json!(false));

    let after = std::fs::read_to_string(&path).expect("readable");
    assert!(after.contains("Sent intro email"));
    let occurrences_after_first = after.matches("Sent intro email").count();

    // Repeat: identical triple plans zero actions, appends nothing.
    let bytes_before_repeat = std::fs::read(&path).expect("readable");
    let ctx2 = run_add_action(tmp.path(), action_event).await;
    let run2 = &ctx2.node_runs[edit_graph::ADD_ACTION_NODE_NAME];
    assert_eq!(run2.status, NodeRunStatus::Success);
    let result2 = &ctx2.nodes[edit_graph::ADD_ACTION_NODE_NAME];
    assert_eq!(result2["no_op"], json!(true));
    let bytes_after_repeat = std::fs::read(&path).expect("readable");
    assert_eq!(bytes_before_repeat, bytes_after_repeat);

    let after_repeat = std::fs::read_to_string(&path).expect("readable");
    let occurrences_after_repeat = after_repeat.matches("Sent intro email").count();
    assert_eq!(
        occurrences_after_first, occurrences_after_repeat,
        "repeated add-action must not append a second entry"
    );
}

#[tokio::test]
async fn invalid_stage_fails_the_run_and_leaves_the_file_unchanged() {
    let tmp = tempfile::tempdir().expect("tempdir");
    opportunities_dir(tmp.path());

    run_research(tmp.path(), company_event()).await;
    let path = tmp.path().join("business/docs/opportunities/anthropic.md");
    let before = std::fs::read(&path).expect("readable");

    let ctx = run_set_stage(
        tmp.path(),
        json!({ "slug": "anthropic", "stage": "not-a-real-stage" }),
    )
    .await;

    let run = &ctx.node_runs[edit_graph::SET_STAGE_NODE_NAME];
    assert_eq!(run.status, NodeRunStatus::Failed);
    let error = run.error.as_deref().unwrap_or_default();
    for stage in mev::doc::opportunity::VALID_STAGES {
        assert!(
            error.contains(stage),
            "expected error to name valid stage '{stage}': {error}"
        );
    }

    let after = std::fs::read(&path).expect("readable");
    assert_eq!(before, after, "invalid stage must not write to disk");
}

#[tokio::test]
async fn unknown_slug_fails_the_run_and_creates_no_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    opportunities_dir(tmp.path());

    let ctx = run_set_stage(
        tmp.path(),
        json!({ "slug": "no-such-opportunity", "stage": "contacted" }),
    )
    .await;

    let run = &ctx.node_runs[edit_graph::SET_STAGE_NODE_NAME];
    assert_eq!(run.status, NodeRunStatus::Failed);

    let path = tmp
        .path()
        .join("business/docs/opportunities/no-such-opportunity.md");
    assert!(!path.exists(), "unknown slug must not create a file");

    // add-action against an unknown slug fails the same way.
    let ctx2 = run_add_action(
        tmp.path(),
        json!({
            "slug": "no-such-opportunity",
            "at": "2026-07-27",
            "kind": "email",
            "note": "Sent intro email",
        }),
    )
    .await;
    let run2 = &ctx2.node_runs[edit_graph::ADD_ACTION_NODE_NAME];
    assert_eq!(run2.status, NodeRunStatus::Failed);
    assert!(!path.exists(), "unknown slug must not create a file");
}
