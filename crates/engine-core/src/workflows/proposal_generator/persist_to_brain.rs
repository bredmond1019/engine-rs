//! `PersistToBrainNode` — the terminal node that POSTs the finished
//! `AutomationRoadmap` to the brain ingest endpoint via the injectable
//! `crate::nodes::http_post::HttpPost` seam. No embedding/pgvector/corpus
//! write here (THE BOUNDARY TEST) — it stops at the POST.
//!
//! Reads whichever of `ProposalReviseNode` (revise branch) /
//! `ProposalWriterNode` (pass branch) most recently stamped the drafted
//! `AutomationRoadmap` onto `ctx.nodes` — preferring the revised roadmap
//! when both are present, since a `revise` verdict means the reviewer
//! rejected the writer's draft. Builds the payload
//! `{artifact_id, company_name, doc_type, section, content, roadmap}` and
//! awaits the (injectable) `HttpPost` seam:
//! - `artifact_id` — a fresh UUID v4, minted per persist call.
//! - `company_name` — from the triggering `PROPOSAL_GENERATOR` event.
//! - `doc_type`/`section` — constants identifying this as a whole
//!   `AutomationRoadmap` document (the deliverable is persisted as one
//!   artifact, not chunked per-section).
//! - `content` — a plain-language rendering of the roadmap for the brain's
//!   own embedding step (which happens *behind* the ingest endpoint, never
//!   here).
//! - `roadmap` — the structured `AutomationRoadmap` itself.
//!
//! On success, stamps `{"posted": true, "status", "artifact_id", "response"}`
//! onto `ctx.nodes["PersistToBrainNode"]`. On failure, surfaces the seam's
//! error string as a `NodeError` — a failed brain push halts the run rather
//! than silently dropping the finished roadmap. This is a **terminal**
//! node: no forward connection, no `NodeRun.usage` (there is no model call
//! to meter) — `NodeRun` status/timing is framework-owned per
//! `crate::node::Node`.

use engine_contract::TaskContext;
use serde_json::json;
use uuid::Uuid;

use crate::node::{Node, NodeError};
use crate::nodes::http_post::{http_post_live, HttpPost};
use crate::workflows::{get_result, put_result};

use super::schema::{AutomationRoadmap, ProposalGeneratorEventSchema};

/// The `Node::name()` identity `PersistToBrainNode` runs under, and the
/// `ctx.nodes` key its result is stamped onto.
pub const NODE_NAME: &str = "PersistToBrainNode";

/// The upstream `ctx.nodes` identity carrying the writer's original draft
/// (filled by `super::writer::ProposalWriterNode`).
const WRITER_NODE_NAME: &str = "ProposalWriterNode";

/// The upstream `ctx.nodes` identity carrying a corrected draft, when the
/// `revise` branch ran (filled by `super::revise::ProposalReviseNode`).
const REVISE_NODE_NAME: &str = "ProposalReviseNode";

/// The `doc_type` this node always POSTs — the deliverable persists as one
/// whole `AutomationRoadmap` document.
const DOC_TYPE: &str = "automation_roadmap";

/// The `section` this node always POSTs — no per-section chunking here
/// (that, if it ever happens, is the brain's own job behind the endpoint).
const SECTION: &str = "full";

/// The Synapse brain ingest endpoint this node POSTs to (`OR.Q`,
/// `POST /ingest/*`). Not configurable via `ProposalGeneratorPolicy` — the
/// endpoint address is deployment topology, not a per-run policy knob.
const BRAIN_INGEST_URL: &str = "http://localhost:8000/ingest/proposal";

/// Deserialize the inbound `PROPOSAL_GENERATOR` event from `ctx.event`.
fn parse_event(ctx: &TaskContext) -> Result<ProposalGeneratorEventSchema, NodeError> {
    serde_json::from_value(ctx.event.clone())
        .map_err(|err| NodeError::new(format!("invalid PROPOSAL_GENERATOR event: {err}")))
}

/// Read the finished `AutomationRoadmap` off `ctx.nodes`, preferring
/// `ProposalReviseNode`'s corrected draft over `ProposalWriterNode`'s
/// original one when both are present (a `revise` verdict means the
/// reviewer rejected the writer's draft, so the revised roadmap is the
/// one that should reach the brain).
fn read_roadmap(ctx: &TaskContext) -> Result<AutomationRoadmap, NodeError> {
    let source = get_result(ctx, REVISE_NODE_NAME)
        .or_else(|| get_result(ctx, WRITER_NODE_NAME))
        .ok_or_else(|| {
            NodeError::new(format!(
                "{NODE_NAME}: no upstream AutomationRoadmap found on ctx.nodes \
                 (expected {REVISE_NODE_NAME} or {WRITER_NODE_NAME})"
            ))
        })?;

    serde_json::from_value(source.clone()).map_err(|err| {
        NodeError::new(format!(
            "{NODE_NAME}: failed to parse the upstream AutomationRoadmap: {err}"
        ))
    })
}

/// Render a plain-language summary of the roadmap for the brain's own
/// embedding step (which happens behind the ingest endpoint, never here).
fn roadmap_to_content(company_name: &str, roadmap: &AutomationRoadmap) -> String {
    let situation_summary = roadmap
        .situation
        .as_ref()
        .map(|s| s.painful_workflow_summary.clone())
        .unwrap_or_default();

    let candidate_names: Vec<&str> = roadmap.candidates.iter().map(|c| c.name.as_str()).collect();

    let recommended = roadmap
        .recommendation
        .as_ref()
        .map(|r| r.start_with.clone())
        .unwrap_or_default();

    format!(
        "AutomationRoadmap for {company_name}. Situation: {situation_summary}. \
         Candidates: {candidates}. Recommended first engagement: {recommended}.",
        candidates = candidate_names.join(", "),
    )
}

/// The terminal node that POSTs the finished `AutomationRoadmap` to the
/// brain ingest endpoint over the injectable `HttpPost` seam. No forward
/// connection.
pub struct PersistToBrainNode {
    http_post: std::sync::Arc<dyn HttpPost>,
    url: String,
}

impl PersistToBrainNode {
    /// Construct with the live `reqwest`-backed `HttpPost` impl, POSTing to
    /// [`BRAIN_INGEST_URL`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            http_post: http_post_live(),
            url: BRAIN_INGEST_URL.to_string(),
        }
    }

    /// Override the `HttpPost` seam. Tests inject a `StubHttpPost` so the
    /// gated suite never contacts a live brain endpoint.
    #[must_use]
    pub fn with_http_post(mut self, http_post: std::sync::Arc<dyn HttpPost>) -> Self {
        self.http_post = http_post;
        self
    }

    /// Override the target URL. Tests use this to assert on the exact URL
    /// the stub was POSTed to; production code leaves it at
    /// [`BRAIN_INGEST_URL`].
    #[must_use]
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = url.into();
        self
    }
}

impl Default for PersistToBrainNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for PersistToBrainNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let event = parse_event(&ctx)?;
        let roadmap = read_roadmap(&ctx)?;

        let artifact_id = Uuid::new_v4().to_string();
        let content = roadmap_to_content(&event.company_name, &roadmap);
        let roadmap_value = serde_json::to_value(&roadmap).map_err(|err| {
            NodeError::new(format!("failed to serialize AutomationRoadmap: {err}"))
        })?;

        let payload = json!({
            "artifact_id": artifact_id,
            "company_name": event.company_name,
            "doc_type": DOC_TYPE,
            "section": SECTION,
            "content": content,
            "roadmap": roadmap_value,
        });

        let response = self
            .http_post
            .post(&self.url, payload)
            .await
            .map_err(|err| {
                NodeError::new(format!("{NODE_NAME}: brain ingest push failed: {err}"))
            })?;

        let mut ctx = ctx;
        put_result(
            &mut ctx,
            NODE_NAME,
            json!({
                "posted": true,
                "status": response.status,
                "artifact_id": artifact_id,
                "response": response.body,
            }),
        );

        Ok(ctx)
    }

    fn name(&self) -> &str {
        NODE_NAME
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use crate::nodes::http_post::StubHttpPost;

    use super::*;

    fn empty_ctx(event: ProposalGeneratorEventSchema) -> TaskContext {
        TaskContext {
            event: serde_json::to_value(event).unwrap(),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        }
    }

    fn base_event() -> ProposalGeneratorEventSchema {
        ProposalGeneratorEventSchema {
            company_name: "Loja da Ana".to_string(),
            company_url: None,
            diagnostic_intake: None,
            policy: None,
            profile: None,
        }
    }

    fn sample_roadmap_json() -> serde_json::Value {
        json!({
            "situation": {
                "company_name": "Loja da Ana",
                "business_type": "retail SMB",
                "team_size": 4,
                "painful_workflow_summary": "Orders tracked by scrolling WhatsApp threads.",
                "candidate_count": 1,
            },
            "candidates": [
                {
                    "name": "WhatsApp order tracking",
                    "frequency": 5.0,
                    "time_cost": 4.0,
                    "buildability": 4.0,
                    "composite": 4.35,
                    "tier": "quick_win",
                    "rationale": "Happens daily and is fully manual today.",
                }
            ],
            "top_profiles": [
                {
                    "name": "WhatsApp order tracking",
                    "today": "Manually scrolled.",
                    "proposed_solution": "Automated bot with human approval gate.",
                    "stack": "WhatsApp Business API + small service.",
                    "rough_scope": "2-3 weeks.",
                    "expected_roi": "Saves ~5 hrs/week.",
                }
            ],
            "recommendation": {
                "start_with": "WhatsApp order tracking",
                "phase_1_scope": ["Order intake bot"],
                "investment": "R$8,000-12,000 fixed fee",
                "how_it_works": "Connects to WhatsApp Business API.",
                "call_to_action": "Book a call to proceed.",
            },
        })
    }

    #[tokio::test]
    async fn process_posts_the_expected_payload_shape_from_the_writer_draft() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = PersistToBrainNode::new()
            .with_http_post(std::sync::Arc::new(stub.clone()))
            .with_url("https://brain.example/ingest/proposal");

        let mut ctx = empty_ctx(base_event());
        ctx.nodes
            .insert(WRITER_NODE_NAME.to_string(), sample_roadmap_json());

        let ctx = node.process(ctx).await.expect("process should succeed");

        let (url, body) = stub.last_call().expect("post should have been recorded");
        assert_eq!(url, "https://brain.example/ingest/proposal");

        let object = body.as_object().expect("payload is an object");
        assert_eq!(
            object
                .keys()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>(),
            [
                "artifact_id",
                "company_name",
                "doc_type",
                "section",
                "content",
                "roadmap"
            ]
            .iter()
            .map(|s| s.to_string())
            .collect()
        );
        assert_eq!(body["company_name"], json!("Loja da Ana"));
        assert_eq!(body["doc_type"], json!("automation_roadmap"));
        assert_eq!(body["section"], json!("full"));
        assert_eq!(body["roadmap"], sample_roadmap_json());
        assert!(body["content"].as_str().unwrap().contains("Loja da Ana"));
        assert!(!body["artifact_id"].as_str().unwrap().is_empty());

        let result = &ctx.nodes[NODE_NAME];
        assert_eq!(result["posted"], json!(true));
        assert_eq!(result["status"], json!(200));
        assert_eq!(result["artifact_id"], body["artifact_id"]);
    }

    #[tokio::test]
    async fn process_prefers_the_revised_roadmap_over_the_writer_draft() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = PersistToBrainNode::new().with_http_post(std::sync::Arc::new(stub.clone()));

        let mut ctx = empty_ctx(base_event());
        ctx.nodes
            .insert(WRITER_NODE_NAME.to_string(), sample_roadmap_json());
        let mut revised = sample_roadmap_json();
        revised["situation"]["painful_workflow_summary"] =
            json!("Corrected: orders tracked in a shared spreadsheet.");
        ctx.nodes
            .insert(REVISE_NODE_NAME.to_string(), revised.clone());

        node.process(ctx).await.expect("process should succeed");

        let (_url, body) = stub.last_call().expect("post should have been recorded");
        assert_eq!(body["roadmap"], revised);
        assert!(body["content"]
            .as_str()
            .unwrap()
            .contains("Corrected: orders tracked in a shared spreadsheet."));
    }

    #[tokio::test]
    async fn process_surfaces_a_stub_failure_as_a_node_error() {
        let stub = StubHttpPost::failing("brain endpoint unreachable");
        let node = PersistToBrainNode::new().with_http_post(std::sync::Arc::new(stub));

        let mut ctx = empty_ctx(base_event());
        ctx.nodes
            .insert(WRITER_NODE_NAME.to_string(), sample_roadmap_json());

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("brain endpoint unreachable"));
    }

    #[tokio::test]
    async fn process_errors_when_no_upstream_roadmap_is_present() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = PersistToBrainNode::new().with_http_post(std::sync::Arc::new(stub));

        let ctx = empty_ctx(base_event());

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("no upstream AutomationRoadmap"));
    }

    #[tokio::test]
    async fn process_errors_when_event_is_invalid() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = PersistToBrainNode::new().with_http_post(std::sync::Arc::new(stub));

        let mut ctx = TaskContext {
            event: json!({ "not_company_name": "oops" }),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        ctx.nodes
            .insert(WRITER_NODE_NAME.to_string(), sample_roadmap_json());

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("invalid PROPOSAL_GENERATOR event"));
    }

    #[test]
    fn default_constructs_without_panicking() {
        let _node = PersistToBrainNode::default();
    }
}
