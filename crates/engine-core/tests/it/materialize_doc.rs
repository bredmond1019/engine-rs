//! Hermetic integration tests for `MaterializeDocNode` (`EN.7.A` task 5) —
//! the REAL `MevDocMaterializer`, no stub, driven against a
//! `tempfile::tempdir()` corpus. Mirrors `../../mev/tests/doc_opportunity.rs`'s
//! fixture pattern: pre-create `<root>/business/docs/opportunities/` before
//! any write.
//!
//! Every test is hermetic: no network, no writes outside its own tempdir,
//! and in particular no write anywhere under the real `agentic-portfolio`
//! corpus. The fixture at `tests/fixtures/company_brief.json` is copied
//! verbatim from `core/mev/tests/fixtures/company_brief.json` rather than
//! path-referenced, so `cargo test` here never depends on the sibling repo's
//! working tree.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use engine_contract::TaskContext;
use engine_core::brain_root::ENGINE_BRAIN_ROOT_ENV;
use engine_core::node::{Node, NodeExt};
use engine_core::nodes::materialize_doc::{MaterializeDocNode, NODE_NAME};
use serde_json::json;

/// Guards the single test in this file that touches `ENGINE_BRAIN_ROOT` (a
/// process-global) so it cannot race any other test — see test 7.
static ENV_GUARD: Mutex<()> = Mutex::new(());

fn fixture_brief() -> serde_json::Value {
    let raw =
        std::fs::read_to_string("tests/fixtures/company_brief.json").expect("fixture must exist");
    serde_json::from_str(&raw).expect("fixture must be valid JSON")
}

fn opportunities_dir(root: &Path) -> std::path::PathBuf {
    let dir = root.join("business/docs/opportunities");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn empty_ctx(event: serde_json::Value) -> TaskContext {
    TaskContext {
        event,
        nodes: HashMap::new(),
        metadata: json!({}),
        node_runs: HashMap::new(),
    }
}

fn ctx_with_upstream(upstream: &str, artifact: serde_json::Value) -> TaskContext {
    let mut ctx = empty_ctx(json!({}));
    ctx.nodes.insert(upstream.to_string(), artifact);
    ctx
}

#[tokio::test]
async fn writes_a_valid_opportunity_document() {
    let tmp = tempfile::tempdir().expect("tempdir");
    opportunities_dir(tmp.path());

    let ctx = ctx_with_upstream("UpstreamNode", fixture_brief());

    let node = MaterializeDocNode::new("opportunity")
        .with_brain_root(tmp.path())
        .with_source_node("UpstreamNode");

    let ctx = node.process(ctx).await.expect("process should succeed");

    let result = &ctx.nodes[NODE_NAME];
    assert_eq!(result["materialized"], json!(true));
    assert_eq!(result["dry_run"], json!(false));
    assert_eq!(result["model"], json!("opportunity"));

    let paths = result["paths"].as_array().expect("paths must be an array");
    assert_eq!(paths.len(), 1, "expected exactly one written path");
    let written_path = paths[0].as_str().expect("path must be a string");
    let written_path = Path::new(written_path);
    assert!(written_path.exists(), "expected {written_path:?} to exist");

    let content = std::fs::read_to_string(written_path).expect("file must be readable");
    assert!(content.contains("type: Opportunity"));
    assert!(content.contains("title: Anthropic"));
    assert!(content.contains("kind:"));
    assert!(content.contains("stage:"));
}

#[tokio::test]
async fn is_idempotent_over_an_already_written_corpus() {
    let tmp = tempfile::tempdir().expect("tempdir");
    opportunities_dir(tmp.path());

    let brief = fixture_brief();

    let node = || {
        MaterializeDocNode::new("opportunity")
            .with_brain_root(tmp.path())
            .with_source_node("UpstreamNode")
    };

    let ctx1 = ctx_with_upstream("UpstreamNode", brief.clone());
    let ctx1 = node()
        .process(ctx1)
        .await
        .expect("first process should succeed");
    let path = ctx1.nodes[NODE_NAME]["paths"][0]
        .as_str()
        .expect("path must be a string")
        .to_string();
    let bytes_after_first = std::fs::read(&path).expect("file must exist after first write");

    // mev's `plan_document` carries its own idempotency guard: re-planning
    // over unchanged content yields zero actions, so the second run's
    // `paths` stamp may legitimately be empty. What must hold is the file on
    // disk — its bytes must be unchanged.
    let ctx2 = ctx_with_upstream("UpstreamNode", brief);
    node()
        .process(ctx2)
        .await
        .expect("second process should succeed");
    let bytes_after_second = std::fs::read(&path).expect("file must exist after second write");

    assert_eq!(
        bytes_after_first, bytes_after_second,
        "re-running the node over the same corpus must be idempotent"
    );
}

#[tokio::test]
async fn dry_run_writes_nothing_but_reports_the_planned_path() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let opps_dir = opportunities_dir(tmp.path());

    let ctx = ctx_with_upstream("UpstreamNode", fixture_brief());

    let node = MaterializeDocNode::new("opportunity")
        .with_brain_root(tmp.path())
        .with_source_node("UpstreamNode")
        .with_write(false);

    let ctx = node.process(ctx).await.expect("process should succeed");

    let entries: Vec<_> = std::fs::read_dir(&opps_dir)
        .expect("opportunities dir must be readable")
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .path()
                .extension()
                .map(|ext| ext == "md")
                .unwrap_or(false)
        })
        .collect();
    assert!(
        entries.is_empty(),
        "dry run must not write any .md file, found: {entries:?}"
    );

    let result = &ctx.nodes[NODE_NAME];
    assert_eq!(result["dry_run"], json!(true));
    let paths = result["paths"].as_array().expect("paths must be an array");
    assert_eq!(paths.len(), 1, "dry run must still name the planned path");
    assert!(!paths[0].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn unknown_model_errors_without_writing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    opportunities_dir(tmp.path());

    let ctx = ctx_with_upstream("UpstreamNode", fixture_brief());

    let node = MaterializeDocNode::new("not-a-model")
        .with_brain_root(tmp.path())
        .with_source_node("UpstreamNode");

    let err = node
        .process(ctx)
        .await
        .expect_err("unknown model must error");
    assert!(err.message.contains("opportunity"));
    assert!(err.message.contains("learning-artifact"));
    assert!(err.message.contains("proposal"));
}

#[tokio::test]
async fn missing_upstream_artifact_errors_naming_the_missing_identity() {
    let tmp = tempfile::tempdir().expect("tempdir");
    opportunities_dir(tmp.path());

    let ctx = empty_ctx(json!({}));

    let node = MaterializeDocNode::new("opportunity")
        .with_brain_root(tmp.path())
        .with_source_node("NopeNode");

    let err = node
        .process(ctx)
        .await
        .expect_err("missing upstream artifact must error");
    assert!(err.message.contains("NopeNode"));
}

#[tokio::test]
async fn identity_override_lets_two_instances_co_exist() {
    let tmp = tempfile::tempdir().expect("tempdir");
    opportunities_dir(tmp.path());

    let ctx = ctx_with_upstream("UpstreamNode", fixture_brief());

    let node = MaterializeDocNode::new("opportunity")
        .with_brain_root(tmp.path())
        .with_source_node("UpstreamNode")
        .with_identity("MaterializeOpportunityDoc");

    let ctx = node.process(ctx).await.expect("process should succeed");

    assert!(ctx.nodes.contains_key("MaterializeOpportunityDoc"));
    assert!(!ctx.nodes.contains_key(NODE_NAME));
}

// This is the only test in the file that touches `ENGINE_BRAIN_ROOT` (a
// process-global), so nothing else in this file can race it; the guard
// exists purely so a future test added here can't accidentally do so
// without noticing. Holding it across the `await` below is deliberate —
// the set/restore must bracket the whole call.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn resolves_brain_root_from_engine_brain_root_env_var() {
    let _guard = ENV_GUARD.lock().unwrap();

    let tmp = tempfile::tempdir().expect("tempdir");
    opportunities_dir(tmp.path());

    let previous = std::env::var(ENGINE_BRAIN_ROOT_ENV).ok();
    std::env::set_var(ENGINE_BRAIN_ROOT_ENV, tmp.path());

    let ctx = ctx_with_upstream("UpstreamNode", fixture_brief());

    // Deliberately no `.with_brain_root(..)` — resolution must go through
    // `ENGINE_BRAIN_ROOT`.
    let node = MaterializeDocNode::new("opportunity").with_source_node("UpstreamNode");

    let result = node.process(ctx).await;

    match previous {
        Some(value) => std::env::set_var(ENGINE_BRAIN_ROOT_ENV, value),
        None => std::env::remove_var(ENGINE_BRAIN_ROOT_ENV),
    }

    let ctx = result.expect("process should succeed via ENGINE_BRAIN_ROOT");
    let paths = ctx.nodes[NODE_NAME]["paths"]
        .as_array()
        .expect("paths must be an array");
    let written_path = paths[0].as_str().expect("path must be a string");
    assert!(
        Path::new(written_path).starts_with(tmp.path()),
        "expected write under the tempdir root set via ENGINE_BRAIN_ROOT, got {written_path}"
    );
}
