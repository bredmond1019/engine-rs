//! Hermetic integration test for `RecallNode` (`EN.6.K` task 2) — proves it
//! composes as an ordinary node in a minimal *registered* workflow (a real
//! `NodeRegistry` + `WorkflowSchema` + `Workflow::new_validated` + a real
//! `Workflow::run` pointer-walk), not merely as a hand-called `process`
//! call, and that a downstream node reads its stamped output straight off
//! `ctx.nodes` the same way `crate::workflows::get_result` would inside the
//! crate. Mirrors `composition.rs`'s registry-driving style
//! (`EN.5.E`).
//!
//! Hermetic by construction: `StubHttpGet` never makes a live network call,
//! so this suite never contacts a real Synapse instance.

use std::collections::HashMap;

use engine_contract::TaskContext;
use engine_core::node::{Node, NodeError, NodeRegistry};
use engine_core::nodes::brain_client::{
    BrainConfig, HttpGet, RecallNode, RecallResponse, StubHttpGet, RECALL_NODE_NAME,
};
use engine_core::schema::{NodeConfig, WorkflowSchema};
use engine_core::workflow::Workflow;
use serde_json::{json, Value};
use std::sync::Arc;

/// The downstream node under test: reads `RecallNode`'s stamped output
/// straight off `ctx.nodes[RECALL_NODE_NAME]` — the same lookup
/// `crate::workflows::get_result` performs inside the crate (that helper is
/// `pub(crate)`, so an external integration-test binary reads the
/// equivalent `ctx.nodes` entry directly) — and stamps a derived summary,
/// proving the recall result is genuinely readable by a node downstream of
/// it in a real walk, not just present on the final `TaskContext`.
struct SummarizeRecallNode;

#[async_trait::async_trait]
impl Node for SummarizeRecallNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let recall = ctx.nodes.get(RECALL_NODE_NAME).ok_or_else(|| {
            NodeError::new(format!(
                "SummarizeRecallNode: no upstream \"{RECALL_NODE_NAME}\" result"
            ))
        })?;
        let count = recall
            .get("count")
            .and_then(Value::as_u64)
            .ok_or_else(|| NodeError::new("SummarizeRecallNode: missing \"count\""))?;
        let saw_query = recall.get("query").cloned();

        let mut ctx = ctx;
        ctx.nodes.insert(
            self.name().to_string(),
            json!({ "recalled_count": count, "saw_query": saw_query }),
        );
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "SummarizeRecallNode"
    }
}

fn sample_recall_body() -> Value {
    json!({
        "query": "roadmap",
        "count": 2,
        "results": [
            {
                "doc_id": "d1",
                "file_path": "docs/roadmap.md",
                "title": "Roadmap",
                "section": "full",
                "content": "the roadmap content",
                "score": 0.91,
                "via": "semantic",
            },
            {
                "doc_id": null,
                "file_path": "docs/plan.md",
                "title": null,
                "section": null,
                "content": "a related plan",
                "score": 0.62,
                "via": "keyword",
            }
        ],
    })
}

/// Build the minimal two-node registered workflow: `RecallNode ->
/// SummarizeRecallNode`, walked by a real `Workflow::run`.
fn build_workflow(http_get: Arc<dyn HttpGet>, config: BrainConfig) -> Workflow {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(
        RecallNode::new(config)
            .with_http_get(http_get)
            .with_limit(5),
    ));
    registry.register(Box::new(SummarizeRecallNode));

    let mut nodes = HashMap::new();
    nodes.insert(
        RECALL_NODE_NAME.to_string(),
        NodeConfig::new(RECALL_NODE_NAME, vec!["SummarizeRecallNode".to_string()]),
    );
    nodes.insert(
        "SummarizeRecallNode".to_string(),
        NodeConfig::new("SummarizeRecallNode", vec![]),
    );

    let schema = WorkflowSchema::new("BRAIN_RECALL_FIXTURE", RECALL_NODE_NAME, nodes);
    Workflow::new_validated(registry, schema).expect("fixture recall graph should validate")
}

#[tokio::test]
async fn recall_node_composes_in_a_registered_workflow_and_downstream_reads_its_output() {
    let stub = StubHttpGet::succeeding(sample_recall_body());
    let config = BrainConfig::new("http://localhost:8000", Some("k-123".to_string()));
    let workflow = build_workflow(Arc::new(stub.clone()), config);

    let ctx = workflow
        .run(json!("roadmap"), Box::new(|_| {}))
        .await
        .expect("run should succeed");

    // RecallNode's own stamped result is present and shaped per the pinned
    // GET /recall contract.
    let recall_result = ctx
        .nodes
        .get(RECALL_NODE_NAME)
        .expect("RecallNode should have stamped a result");
    assert_eq!(recall_result["query"], json!("roadmap"));
    assert_eq!(recall_result["count"], json!(2));
    assert_eq!(
        recall_result["results"][0]["file_path"],
        json!("docs/roadmap.md")
    );
    assert_eq!(recall_result["results"][1]["doc_id"], Value::Null);

    // The downstream node genuinely read that output through the real
    // Workflow::run walk (not just present side-by-side on the final ctx).
    let summary = ctx
        .nodes
        .get("SummarizeRecallNode")
        .expect("SummarizeRecallNode should have stamped a result");
    assert_eq!(summary["recalled_count"], json!(2));
    assert_eq!(summary["saw_query"], json!("roadmap"));

    // The outbound GET carried the query, limit, and X-API-Key header.
    let (url, query, headers) = stub.last_call().expect("fetch should have been recorded");
    assert_eq!(url, "http://localhost:8000/recall");
    assert_eq!(
        query,
        vec![
            ("q".to_string(), "roadmap".to_string()),
            ("limit".to_string(), "5".to_string()),
            ("hybrid".to_string(), "true".to_string()),
        ]
    );
    assert_eq!(
        headers,
        vec![("X-API-Key".to_string(), "k-123".to_string())]
    );
}

#[tokio::test]
async fn recall_node_omits_the_auth_header_when_no_api_key_is_configured() {
    let stub = StubHttpGet::succeeding(sample_recall_body());
    let config = BrainConfig::new("http://localhost:8000", None);
    let workflow = build_workflow(Arc::new(stub.clone()), config);

    workflow
        .run(json!("roadmap"), Box::new(|_| {}))
        .await
        .expect("run should succeed");

    let (_, _, headers) = stub.last_call().expect("fetch should have been recorded");
    assert!(
        headers.is_empty(),
        "expected no auth header when the config carries no API key, got {headers:?}"
    );
}

#[tokio::test]
async fn a_non_2xx_shaped_stub_failure_surfaces_as_a_node_error_and_halts_the_walk() {
    use engine_contract::NodeRunStatus;

    let stub = StubHttpGet::failing("brain read endpoint returned HTTP 401: unauthorized");
    let config = BrainConfig::new("http://localhost:8000", None);
    let workflow = build_workflow(Arc::new(stub.clone()), config);

    let ctx = workflow
        .run(json!("roadmap"), Box::new(|_| {}))
        .await
        .expect("Workflow::run returns Ok with the failed-node's ctx per its own halt contract");

    // RecallNode's own run is stamped FAILED with the transport error
    // folded into a NodeError naming RecallNode.
    let recall_run = ctx
        .node_runs
        .get(RECALL_NODE_NAME)
        .expect("RecallNode should have a node_runs entry");
    assert_eq!(recall_run.status, NodeRunStatus::Failed);
    let message = recall_run
        .error
        .as_deref()
        .expect("failed run should carry an error message");
    assert!(
        message.contains("401") && message.contains(RECALL_NODE_NAME),
        "expected the 401 status and RecallNode's name in the surfaced error, got: {message}"
    );

    // The walk halted: SummarizeRecallNode never ran, so RecallNode's
    // failure is what a downstream node would have seen as "no result".
    assert!(
        !ctx.nodes.contains_key("SummarizeRecallNode"),
        "the downstream node should never have run past RecallNode's failure"
    );
}

/// EN.12.L task 1 — fixture-conformance coverage for `GET /recall`'s
/// response envelope against `tests/fixtures/recall_response.json`, a
/// checked-in capture of Synapse's pinned
/// `app/schemas/read_schema.py::RecallResponse` shape.
///
/// Production's `RecallResponse`/`RecallResult` (`nodes::brain_client`)
/// carry no `#[serde(deny_unknown_fields)]` on purpose — a deployed engine
/// must keep working the day Synapse ships one more additive field. So
/// strictness lives here instead: `StrictRecallResponse`/`StrictRecallResult`
/// are a test-only mirror of the exact same fields, but WITH
/// `deny_unknown_fields`, and the fixture is round-tripped through both the
/// strict mirror and the real production type.
///
/// Demonstrated failing, not assumed: `strict_mirror_fails_when_a_field_is_renamed`
/// below takes the checked-in fixture and renames `"query"` to
/// `"search_query"` before deserializing through the strict mirror, and
/// asserts that fails. That is the drift this module exists to catch —
/// renaming, adding, or removing a field Synapse pins — demonstrated
/// directly against the fixture rather than assumed from the derive macro.
/// (An *added* field is exercised the same way by
/// `strict_mirror_fails_when_a_field_is_added`, and *production* tolerating
/// that same addition is exercised by
/// `production_type_tolerates_an_added_unknown_field`, covering AC5.)
mod recall_response_conformance {
    use super::*;

    #[derive(Debug, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StrictRecallResult {
        #[allow(dead_code)]
        doc_id: Option<String>,
        #[allow(dead_code)]
        file_path: String,
        #[allow(dead_code)]
        title: Option<String>,
        #[allow(dead_code)]
        section: Option<String>,
        #[allow(dead_code)]
        content: String,
        #[allow(dead_code)]
        score: f64,
        #[allow(dead_code)]
        via: String,
    }

    #[derive(Debug, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StrictRecallResponse {
        #[allow(dead_code)]
        query: String,
        #[allow(dead_code)]
        count: usize,
        #[allow(dead_code)]
        results: Vec<StrictRecallResult>,
    }

    const FIXTURE_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/recall_response.json"
    );

    fn load_fixture() -> Value {
        let raw = std::fs::read_to_string(FIXTURE_PATH)
            .unwrap_or_else(|err| panic!("failed to read {FIXTURE_PATH}: {err}"));
        serde_json::from_str(&raw)
            .unwrap_or_else(|err| panic!("fixture {FIXTURE_PATH} is not valid JSON: {err}"))
    }

    #[test]
    fn strict_mirror_deserializes_the_checked_in_fixture_cleanly() {
        let fixture = load_fixture();
        let strict: Result<StrictRecallResponse, _> = serde_json::from_value(fixture);
        assert!(
            strict.is_ok(),
            "the checked-in fixture should match Synapse's pinned RecallResponse \
             field-for-field: {strict:?}"
        );
    }

    #[test]
    fn production_type_deserializes_the_checked_in_fixture_cleanly() {
        let fixture = load_fixture();
        let production: Result<RecallResponse, _> = serde_json::from_value(fixture);
        assert!(
            production.is_ok(),
            "production RecallResponse should also accept the checked-in fixture: {production:?}"
        );
    }

    #[test]
    fn strict_mirror_fails_when_a_field_is_renamed() {
        let mut fixture = load_fixture();
        let renamed_value = fixture
            .as_object_mut()
            .expect("fixture is a JSON object")
            .remove("query")
            .expect("fixture has a \"query\" field to rename");
        fixture
            .as_object_mut()
            .unwrap()
            .insert("search_query".to_string(), renamed_value);

        let strict: Result<StrictRecallResponse, _> = serde_json::from_value(fixture);
        assert!(
            strict.is_err(),
            "renaming \"query\" to \"search_query\" should fail the strict conformance mirror, \
             proving this test can actually detect drift"
        );
    }

    #[test]
    fn strict_mirror_fails_when_a_field_is_removed() {
        let mut fixture = load_fixture();
        fixture
            .as_object_mut()
            .expect("fixture is a JSON object")
            .remove("count")
            .expect("fixture has a \"count\" field to remove");

        let strict: Result<StrictRecallResponse, _> = serde_json::from_value(fixture);
        assert!(
            strict.is_err(),
            "removing \"count\" should fail the strict conformance mirror"
        );
    }

    #[test]
    fn strict_mirror_fails_when_a_field_is_added() {
        let mut fixture = load_fixture();
        fixture
            .as_object_mut()
            .expect("fixture is a JSON object")
            .insert("workspace".to_string(), json!("default"));

        let strict: Result<StrictRecallResponse, _> = serde_json::from_value(fixture);
        assert!(
            strict.is_err(),
            "an added, unpinned field should fail the strict conformance mirror"
        );
    }

    #[test]
    fn production_type_tolerates_an_added_unknown_field() {
        let mut fixture = load_fixture();
        fixture
            .as_object_mut()
            .expect("fixture is a JSON object")
            .insert("workspace".to_string(), json!("default"));

        let production: Result<RecallResponse, _> = serde_json::from_value(fixture);
        assert!(
            production.is_ok(),
            "production RecallResponse must stay forward-compatible with an additive Synapse \
             field, so a deployed engine does not break mid-run: {production:?}"
        );
    }
}
