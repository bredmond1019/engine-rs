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
use crate::nodes::brain_client::BrainConfig;
use crate::nodes::http_post::{http_post_live, HttpPost};
use crate::workflows::{get_result, put_result};

use super::schema::{AutomationRoadmap, Investment, ProposalGeneratorEventSchema};
use crate::locale::Currency;

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

/// The path this node POSTs to, joined onto [`BrainConfig::base_url`]
/// (`OR.Q`, `POST /ingest/*`). Not configurable via
/// `ProposalGeneratorPolicy` — the endpoint address is deployment topology,
/// not a per-run policy knob. `EN.6.K` task 3: the base URL/key are no
/// longer a hardcoded `localhost:8000` const — they come from
/// [`BrainConfig`], resolved from [`BrainConfig::from_env`] unless
/// overridden via [`PersistToBrainNode::with_config`] /
/// [`PersistToBrainNode::with_url`].
const BRAIN_INGEST_PATH: &str = "/ingest/proposal";

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

/// Format a money range in its OWN currency only. Never annotates with a
/// second currency and never converts — see EN.4.F's firewall invariant.
fn format_money(range: &Investment) -> String {
    let symbol = match range.currency {
        Currency::Brl => "R$",
        Currency::Usd => "$",
    };
    let min = range.min;
    let max = range.max;
    let suffix = match range.basis {
        crate::locale::EngagementBasis::Fixed => "fixed",
        crate::locale::EngagementBasis::PerMonth => "per month",
        crate::locale::EngagementBasis::PerHour => "per hour",
    };
    format!("{symbol}{min:.0}-{max:.0} {suffix}")
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

    let investment = roadmap
        .recommendation
        .as_ref()
        .and_then(|r| r.investment.as_ref())
        .map(format_money)
        .unwrap_or_default();

    format!(
        "AutomationRoadmap for {company_name}. Situation: {situation_summary}. \
         Candidates: {candidates}. Recommended first engagement: {recommended}. \
         Investment: {investment}.",
        candidates = candidate_names.join(", "),
    )
}

/// The terminal node that POSTs the finished `AutomationRoadmap` to the
/// brain ingest endpoint over the injectable `HttpPost` seam. No forward
/// connection.
pub struct PersistToBrainNode {
    http_post: std::sync::Arc<dyn HttpPost>,
    /// Full target URL override. `None` (the production default) derives
    /// the URL from [`BrainConfig::base_url`] + [`BRAIN_INGEST_PATH`] at
    /// call time; tests set this directly to assert on an exact stub URL
    /// without also having to construct a [`BrainConfig`].
    url: Option<String>,
    /// `BrainConfig` override. `None` (the production default) resolves
    /// [`BrainConfig::from_env`] at call time — a missing `BRAIN_API_URL`
    /// then surfaces as a `NodeError` naming the env var, not a silent
    /// unauthenticated POST. Tests set this to assert the `X-API-Key`
    /// header the stub receives.
    config: Option<BrainConfig>,
}

impl PersistToBrainNode {
    /// Construct with the live `reqwest`-backed `HttpPost` impl. The target
    /// URL and `X-API-Key` header are resolved from [`BrainConfig::from_env`]
    /// unless overridden via [`Self::with_url`] / [`Self::with_config`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            http_post: http_post_live(),
            url: None,
            config: None,
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
    /// the stub was POSTed to without needing to also override
    /// [`BrainConfig`] — production code leaves this unset and derives the
    /// URL from [`BrainConfig::base_url`] + [`BRAIN_INGEST_PATH`].
    #[must_use]
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }

    /// Override the [`BrainConfig`] (base URL / `X-API-Key`) this node
    /// resolves instead of reading `BRAIN_API_URL`/`BRAIN_API_KEY` from the
    /// environment. Tests use this to assert the `X-API-Key` header the
    /// stub receives.
    #[must_use]
    pub fn with_config(mut self, config: BrainConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// Resolve `(url, headers)` for this call: an explicit [`Self::with_url`]
    /// override always wins for the URL; the `X-API-Key` header always
    /// comes from [`Self::with_config`] when set, otherwise
    /// [`BrainConfig::from_env`] — falling back to no header (rather than
    /// erroring) only when *neither* a URL nor a config override is set and
    /// the environment is also unconfigured, which keeps every pre-`EN.6.K`
    /// caller that only overrides `with_url` (no live Brain, stub-only)
    /// working unchanged.
    fn resolve_target(&self) -> Result<(String, Vec<(String, String)>), NodeError> {
        match (&self.url, &self.config) {
            (Some(url), Some(config)) => Ok((url.clone(), config.auth_headers())),
            (Some(url), None) => Ok((url.clone(), Vec::new())),
            (None, Some(config)) => Ok((
                format!(
                    "{}{BRAIN_INGEST_PATH}",
                    config.base_url.trim_end_matches('/')
                ),
                config.auth_headers(),
            )),
            (None, None) => {
                let config = BrainConfig::from_env()
                    .map_err(|err| NodeError::new(format!("{NODE_NAME}: {err}")))?;
                let url = format!(
                    "{}{BRAIN_INGEST_PATH}",
                    config.base_url.trim_end_matches('/')
                );
                let headers = config.auth_headers();
                Ok((url, headers))
            }
        }
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

        let (url, headers) = self.resolve_target()?;
        let header_refs: Vec<(&str, &str)> = headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();

        let response = self
            .http_post
            .post_with_headers(&url, payload, &header_refs)
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
            locale: crate::locale::Locale::default(),
            policy: None,
            profile: None,
        }
    }

    fn sample_roadmap_json() -> serde_json::Value {
        json!({
            "authored_locale": "pt-BR",
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
                "investment": {
                    "currency": "BRL",
                    "min": 8_000.0,
                    "max": 12_000.0,
                    "basis": "fixed",
                },
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
        let node = PersistToBrainNode::new()
            .with_http_post(std::sync::Arc::new(stub.clone()))
            .with_url("https://brain.example/ingest/proposal");

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
        let node = PersistToBrainNode::new()
            .with_http_post(std::sync::Arc::new(stub))
            .with_url("https://brain.example/ingest/proposal");

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

    #[tokio::test]
    async fn with_config_derives_the_url_from_the_base_and_sends_the_api_key_header() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = PersistToBrainNode::new()
            .with_http_post(std::sync::Arc::new(stub.clone()))
            .with_config(BrainConfig::new(
                "https://brain.example",
                Some("secret-key".to_string()),
            ));

        let mut ctx = empty_ctx(base_event());
        ctx.nodes
            .insert(WRITER_NODE_NAME.to_string(), sample_roadmap_json());

        node.process(ctx).await.expect("process should succeed");

        let (url, _body) = stub.last_call().expect("post should have been recorded");
        assert_eq!(url, "https://brain.example/ingest/proposal");

        let headers = stub
            .last_headers()
            .expect("post_with_headers should have been used");
        assert!(
            headers.contains(&("X-API-Key".to_string(), "secret-key".to_string())),
            "headers were: {headers:?}"
        );
    }

    #[tokio::test]
    async fn with_config_and_no_api_key_sends_no_auth_header() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = PersistToBrainNode::new()
            .with_http_post(std::sync::Arc::new(stub.clone()))
            .with_config(BrainConfig::new("https://brain.example", None));

        let mut ctx = empty_ctx(base_event());
        ctx.nodes
            .insert(WRITER_NODE_NAME.to_string(), sample_roadmap_json());

        node.process(ctx).await.expect("process should succeed");

        let headers = stub
            .last_headers()
            .expect("post_with_headers should have been used");
        assert!(headers.is_empty());
    }

    #[tokio::test]
    async fn with_no_url_and_no_config_errors_when_brain_api_url_is_unset() {
        // SAFETY: engine-rs's nextest run gives each test its own process,
        // so mutating process env here cannot race another test's reads.
        let previous = std::env::var("BRAIN_API_URL").ok();
        std::env::remove_var("BRAIN_API_URL");

        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = PersistToBrainNode::new().with_http_post(std::sync::Arc::new(stub));

        let mut ctx = empty_ctx(base_event());
        ctx.nodes
            .insert(WRITER_NODE_NAME.to_string(), sample_roadmap_json());

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("BRAIN_API_URL"));

        if let Some(value) = previous {
            std::env::set_var("BRAIN_API_URL", value);
        }
    }

    #[tokio::test]
    async fn payload_roadmap_investment_round_trips_as_an_object() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = PersistToBrainNode::new()
            .with_http_post(std::sync::Arc::new(stub.clone()))
            .with_url("https://brain.example/ingest/proposal");

        let mut ctx = empty_ctx(base_event());
        ctx.nodes
            .insert(WRITER_NODE_NAME.to_string(), sample_roadmap_json());

        node.process(ctx).await.expect("process should succeed");

        let (_url, body) = stub.last_call().expect("post should have been recorded");
        assert_eq!(
            body["roadmap"]["recommendation"]["investment"]["currency"],
            json!("BRL")
        );
        assert_eq!(
            body["roadmap"]["recommendation"]["investment"]["min"],
            json!(8_000.0)
        );
        assert_eq!(
            body["roadmap"]["recommendation"]["investment"]["max"],
            json!(12_000.0)
        );
        assert_eq!(
            body["roadmap"]["recommendation"]["investment"]["basis"],
            json!("fixed")
        );
    }

    #[tokio::test]
    async fn brl_roadmap_content_shows_no_dollar_figure() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = PersistToBrainNode::new()
            .with_http_post(std::sync::Arc::new(stub.clone()))
            .with_url("https://brain.example/ingest/proposal");

        let mut ctx = empty_ctx(base_event());
        ctx.nodes
            .insert(WRITER_NODE_NAME.to_string(), sample_roadmap_json());

        node.process(ctx).await.expect("process should succeed");

        let (_url, body) = stub.last_call().expect("post should have been recorded");
        let content = body["content"].as_str().unwrap();
        assert!(content.contains("R$"));
        // Every literal '$' in a BRL-priced roadmap must be the second
        // character of an "R$" pair — no bare USD figure anywhere.
        assert_eq!(content.matches("R$").count(), content.matches('$').count());
    }

    #[tokio::test]
    async fn usd_roadmap_content_shows_no_real_figure() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = PersistToBrainNode::new()
            .with_http_post(std::sync::Arc::new(stub.clone()))
            .with_url("https://brain.example/ingest/proposal");

        let mut usd_roadmap = sample_roadmap_json();
        usd_roadmap["authored_locale"] = json!("en-US");
        usd_roadmap["recommendation"]["investment"] = json!({
            "currency": "USD",
            "min": 5_000.0,
            "max": 15_000.0,
            "basis": "fixed",
        });

        let mut ctx = empty_ctx(base_event());
        ctx.nodes.insert(WRITER_NODE_NAME.to_string(), usd_roadmap);

        node.process(ctx).await.expect("process should succeed");

        let (_url, body) = stub.last_call().expect("post should have been recorded");
        let content = body["content"].as_str().unwrap();
        assert!(content.contains('$'));
        assert!(!content.contains("R$"));
    }
}
