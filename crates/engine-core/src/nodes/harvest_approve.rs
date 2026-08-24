//! `HarvestApproveNode` (`EN.7.C` task 6) — the generic node that completes a
//! deferred harvest: reads the `{artifact_id, url, payload, doc_paths}`
//! pending-harvest record (`crate::nodes::harvest_gate::pending_harvest_record`,
//! task 1) off `ctx.event` and POSTs `payload` to `url` over the injectable
//! `HttpPost` seam.
//!
//! This is the completion half of `HarvestMode::Approval`
//! (`crate::workflows::content_pipeline::persist_to_brain::PersistToBrainNode`,
//! task 4): the deferring node stamps a pending record whose `payload` is
//! the exact JSON body an `in_process` push would have sent; this node POSTs
//! that payload verbatim to the same `url`, so the eventual push is
//! byte-identical to what the in-process path would have produced — no
//! second indexing path, no divergence between what got written and what
//! got indexed.
//!
//! Model-free, like `MaterializeDocNode`/`OpportunityEditNode`: no
//! `ClaudeCodeStep`, no `ModelTier`, nothing for a policy layer to resolve.
//! Stamps under `self.name()` rather than the bare [`NODE_NAME`] const,
//! mirroring `MaterializeDocNode`, so an identity override
//! (`crate::node::NodeExt::with_identity`) never collides with another
//! instance's result in a shared `ctx.nodes` map.
//!
//! An approved-but-failed harvest is a loud failure, never a silent drop: a
//! malformed/absent pending record is a `NodeError` naming the missing
//! field, and a non-2xx / transport failure from the `HttpPost` seam is a
//! `NodeError` too.
//!
//! `EN.8.C` task 3 adds an **optional** [`ApprovalLedger`] seam
//! (`with_ledger`), `None` by default so existing behavior is unchanged
//! (standing rule 6). When configured, and the triggering event carries an
//! optional `digest` field, this node records the completed harvest through
//! [`record_decision`] before POSTing: a `presented_digest` field (defaulting
//! to `digest` when absent, i.e. no mismatch) that differs from `digest`
//! records a `Requeued` row and skips the POST entirely — a digest mismatch
//! never executes, enforced inside `record_decision` itself, not by
//! convention here. The resolved knob (whether a ledger is configured) is
//! stamped into this node's `ctx.nodes` result either way.
//!
//! `EN.6.K` task 3: this node replays a stored payload to a stored `url` —
//! a pending-harvest record can target a real Brain endpoint, so it must not
//! be the one unauthenticated door into `POST /ingest/*`. The `X-API-Key`
//! header now comes from [`BrainConfig`], resolved from
//! [`BrainConfig::from_env`] unless overridden via [`Self::with_config`];
//! when neither is available (no config override and `BRAIN_API_URL` unset)
//! the POST proceeds with no auth header rather than erroring — the target
//! `url` is data, not necessarily a Brain endpoint, so a missing Brain
//! config here is not itself a construction-time failure the way it is for
//! `PersistToBrainNode`/`RecallNode`, which always target the Brain.

use chrono::{DateTime, Utc};
use engine_contract::TaskContext;
use serde_json::json;

use crate::node::{Node, NodeError};
use crate::nodes::brain_client::BrainConfig;
use crate::nodes::http_post::{http_post_live, HttpPost};
use crate::operator::ledger::{record_decision, ApprovalLedger, LedgerDecision};
use crate::workflows::put_result;

/// The `Node::name()` identity `HarvestApproveNode` runs under by default,
/// and the `ctx.nodes` key its result is stamped onto when no identity
/// override is applied.
pub const NODE_NAME: &str = "HarvestApproveNode";

/// The node that completes a deferred harvest by POSTing a pending-harvest
/// record's `payload` to its `url` over the injectable `HttpPost` seam.
pub struct HarvestApproveNode {
    http_post: std::sync::Arc<dyn HttpPost>,
    ledger: Option<std::sync::Arc<dyn ApprovalLedger>>,
    /// `BrainConfig` override supplying the `X-API-Key` header sent
    /// alongside the replayed POST. `None` (the default) resolves
    /// [`BrainConfig::from_env`] at call time, falling back to no header
    /// when the environment is also unconfigured (see the module doc).
    config: Option<BrainConfig>,
}

impl HarvestApproveNode {
    /// Construct with the live `reqwest`-backed `HttpPost` impl and no
    /// approval ledger — the behavior-stable default (standing rule 6).
    #[must_use]
    pub fn new() -> Self {
        Self {
            http_post: http_post_live(),
            ledger: None,
            config: None,
        }
    }

    /// Override the `HttpPost` seam. Tests inject a `StubHttpPost` so the
    /// gated suite never contacts a live endpoint.
    #[must_use]
    pub fn with_http_post(mut self, http_post: std::sync::Arc<dyn HttpPost>) -> Self {
        self.http_post = http_post;
        self
    }

    /// Configure the [`ApprovalLedger`] this node records completed harvest
    /// decisions through. `None` (the default from [`Self::new`]) leaves
    /// existing behavior exactly as it was before `EN.8.C`.
    #[must_use]
    pub fn with_ledger(mut self, ledger: std::sync::Arc<dyn ApprovalLedger>) -> Self {
        self.ledger = Some(ledger);
        self
    }

    /// Override the [`BrainConfig`] this node reads the `X-API-Key` header
    /// from instead of [`BrainConfig::from_env`]. Tests use this to assert
    /// the header the stub receives.
    #[must_use]
    pub fn with_config(mut self, config: BrainConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// The `X-API-Key` header pair to attach to the replayed POST: the
    /// [`Self::with_config`] override when set, otherwise
    /// [`BrainConfig::from_env`]'s key when the environment is configured,
    /// otherwise no header at all (never an error — see the module doc for
    /// why this node's auth resolution is lenient where
    /// `PersistToBrainNode`/`RecallNode`'s is not).
    fn auth_headers(&self) -> Vec<(String, String)> {
        self.config
            .clone()
            .or_else(|| BrainConfig::from_env().ok())
            .map(|config| config.auth_headers())
            .unwrap_or_default()
    }

    /// Read a required string field off the pending record in `ctx.event`,
    /// erroring with a message naming the missing field.
    fn required_str<'a>(
        &self,
        event: &'a serde_json::Value,
        field: &str,
    ) -> Result<&'a str, NodeError> {
        event.get(field).and_then(|v| v.as_str()).ok_or_else(|| {
            NodeError::new(format!(
                "{NODE_NAME}: pending-harvest record missing required field `{field}`"
            ))
        })
    }

    /// Read an optional string field off `ctx.event` — used for the ledger
    /// fields, which are additive to the four fixed pending-record keys and
    /// therefore never required for the node's core POST behavior.
    fn optional_str<'a>(&self, event: &'a serde_json::Value, field: &str) -> Option<&'a str> {
        event.get(field).and_then(|v| v.as_str())
    }
}

impl Default for HarvestApproveNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for HarvestApproveNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let event = ctx.event.clone();

        let artifact_id = self.required_str(&event, "artifact_id")?.to_string();
        let url = self.required_str(&event, "url")?.to_string();
        let payload = event.get("payload").cloned().ok_or_else(|| {
            NodeError::new(format!(
                "{NODE_NAME}: pending-harvest record missing required field `payload`"
            ))
        })?;

        // `EN.8.C` task 3: optional ledger recording. Only engages when a
        // ledger is configured AND the triggering event carries a `digest`
        // field — neither is present in the four fixed pending-record keys,
        // so this is purely additive and never affects the `ledger: None`
        // default path.
        if let Some(ledger) = &self.ledger {
            if let Some(digest) = self.optional_str(&event, "digest") {
                let presented_digest = self
                    .optional_str(&event, "presented_digest")
                    .unwrap_or(digest);
                let who = self
                    .optional_str(&event, "who")
                    .unwrap_or("operator")
                    .to_string();
                let rendered_diff = self
                    .optional_str(&event, "rendered_diff")
                    .map(str::to_string)
                    .unwrap_or_else(|| payload.to_string());
                let delivered_at = self
                    .optional_str(&event, "delivered_at")
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&Utc));
                let decided_at = Utc::now();
                let delivered_at = delivered_at.unwrap_or(decided_at);

                let outcome = record_decision(
                    ledger.as_ref(),
                    artifact_id.clone(),
                    digest.to_string(),
                    presented_digest,
                    rendered_diff,
                    LedgerDecision::Approved,
                    who,
                    delivered_at,
                    decided_at,
                );

                if !outcome.should_execute {
                    let mut ctx = ctx;
                    put_result(
                        &mut ctx,
                        self.name(),
                        json!({
                            "approved": false,
                            "posted": false,
                            "requeued": true,
                            "artifact_id": artifact_id,
                            "ledger_configured": true,
                        }),
                    );
                    return Ok(ctx);
                }
            }
        }

        let headers = self.auth_headers();
        let header_refs: Vec<(&str, &str)> = headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();

        let response = self
            .http_post
            .post_with_headers(&url, payload, &header_refs)
            .await
            .map_err(|err| {
                NodeError::new(format!("{NODE_NAME}: harvest approval push failed: {err}"))
            })?;

        let mut ctx = ctx;
        put_result(
            &mut ctx,
            self.name(),
            json!({
                "approved": true,
                "posted": true,
                "status": response.status,
                "artifact_id": artifact_id,
                "response": response.body,
                "ledger_configured": self.ledger.is_some(),
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
    use serde_json::json;

    use crate::nodes::http_post::StubHttpPost;

    use super::*;

    fn pending_event() -> serde_json::Value {
        json!({
            "artifact_id": "artifact-1",
            "url": "https://brain.example/ingest/learning",
            "payload": {"artifact_id": "artifact-1", "summary": "A digest."},
            "doc_paths": ["brain/content/learning/artifact-1.md"],
        })
    }

    fn ctx_with_event(event: serde_json::Value) -> TaskContext {
        TaskContext {
            event,
            nodes: std::collections::HashMap::new(),
            metadata: json!({}),
            node_runs: std::collections::HashMap::new(),
        }
    }

    #[tokio::test]
    async fn well_formed_record_posts_the_exact_url_and_payload() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = HarvestApproveNode::new().with_http_post(std::sync::Arc::new(stub.clone()));

        let ctx = ctx_with_event(pending_event());
        let ctx = node.process(ctx).await.expect("process should succeed");

        let (url, body) = stub.last_call().expect("post should have been recorded");
        assert_eq!(url, "https://brain.example/ingest/learning");
        assert_eq!(
            body,
            json!({"artifact_id": "artifact-1", "summary": "A digest."})
        );

        let result = &ctx.nodes[NODE_NAME];
        assert_eq!(result["approved"], json!(true));
        assert_eq!(result["posted"], json!(true));
        assert_eq!(result["status"], json!(200));
        assert_eq!(result["artifact_id"], json!("artifact-1"));
    }

    #[tokio::test]
    async fn missing_url_errors_naming_the_field() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = HarvestApproveNode::new().with_http_post(std::sync::Arc::new(stub));

        let mut event = pending_event();
        event.as_object_mut().unwrap().remove("url");
        let ctx = ctx_with_event(event);

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains('`'));
        assert!(err.message.contains("url"));
    }

    #[tokio::test]
    async fn missing_payload_errors_naming_the_field() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = HarvestApproveNode::new().with_http_post(std::sync::Arc::new(stub));

        let mut event = pending_event();
        event.as_object_mut().unwrap().remove("payload");
        let ctx = ctx_with_event(event);

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("payload"));
    }

    #[tokio::test]
    async fn missing_artifact_id_errors_naming_the_field() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = HarvestApproveNode::new().with_http_post(std::sync::Arc::new(stub));

        let mut event = pending_event();
        event.as_object_mut().unwrap().remove("artifact_id");
        let ctx = ctx_with_event(event);

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("artifact_id"));
    }

    #[tokio::test]
    async fn a_stub_failure_surfaces_as_a_node_error() {
        let stub = StubHttpPost::failing("brain endpoint unreachable");
        let node = HarvestApproveNode::new().with_http_post(std::sync::Arc::new(stub));

        let ctx = ctx_with_event(pending_event());
        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("brain endpoint unreachable"));
    }

    #[test]
    fn default_constructs_without_panicking() {
        let _node = HarvestApproveNode::default();
    }

    // ── EN.6.K task 3: X-API-Key on the replayed POST ───────────────────────

    #[tokio::test]
    async fn with_config_sends_the_api_key_header_on_the_replayed_post() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = HarvestApproveNode::new()
            .with_http_post(std::sync::Arc::new(stub.clone()))
            .with_config(crate::nodes::brain_client::BrainConfig::new(
                "https://brain.example",
                Some("secret-key".to_string()),
            ));

        let ctx = ctx_with_event(pending_event());
        node.process(ctx).await.expect("process should succeed");

        let headers = stub
            .last_headers()
            .expect("post_with_headers should have been used");
        assert!(
            headers.contains(&("X-API-Key".to_string(), "secret-key".to_string())),
            "headers were: {headers:?}"
        );
    }

    #[tokio::test]
    async fn no_config_and_no_env_sends_no_auth_header() {
        // SAFETY: engine-rs's nextest run gives each test its own process,
        // so mutating process env here cannot race another test's reads.
        let previous = std::env::var("BRAIN_API_URL").ok();
        std::env::remove_var("BRAIN_API_URL");

        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = HarvestApproveNode::new().with_http_post(std::sync::Arc::new(stub.clone()));

        let ctx = ctx_with_event(pending_event());
        node.process(ctx).await.expect("process should succeed");

        let headers = stub
            .last_headers()
            .expect("post_with_headers should have been used");
        assert!(headers.is_empty());

        if let Some(value) = previous {
            std::env::set_var("BRAIN_API_URL", value);
        }
    }

    // ── EN.8.C task 3: optional ApprovalLedger wiring ──────────────────────

    use crate::operator::ledger::InMemoryApprovalLedger;

    #[tokio::test]
    async fn no_ledger_configured_behaves_exactly_as_before() {
        // Even with a `digest` field present on the event, no ledger
        // configured means no recording and the exact pre-EN.8.C behavior.
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = HarvestApproveNode::new().with_http_post(std::sync::Arc::new(stub.clone()));

        let mut event = pending_event();
        event
            .as_object_mut()
            .unwrap()
            .insert("digest".to_string(), json!("digest-a"));
        let ctx = ctx_with_event(event);
        let ctx = node.process(ctx).await.expect("process should succeed");

        assert!(stub.last_call().is_some());
        let result = &ctx.nodes[NODE_NAME];
        assert_eq!(result["approved"], json!(true));
        assert_eq!(result["posted"], json!(true));
        assert_eq!(result["ledger_configured"], json!(false));
    }

    #[tokio::test]
    async fn ledger_configured_with_no_digest_field_records_nothing_and_still_posts() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let ledger = std::sync::Arc::new(InMemoryApprovalLedger::new());
        let node = HarvestApproveNode::new()
            .with_http_post(std::sync::Arc::new(stub.clone()))
            .with_ledger(ledger.clone());

        let ctx = ctx_with_event(pending_event());
        let ctx = node.process(ctx).await.expect("process should succeed");

        assert!(stub.last_call().is_some());
        assert!(
            ledger.read_all().is_empty(),
            "no digest field means nothing is recorded"
        );
        let result = &ctx.nodes[NODE_NAME];
        assert_eq!(result["ledger_configured"], json!(true));
    }

    #[tokio::test]
    async fn matched_digest_records_one_approved_row_and_still_posts() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let ledger = std::sync::Arc::new(InMemoryApprovalLedger::new());
        let node = HarvestApproveNode::new()
            .with_http_post(std::sync::Arc::new(stub.clone()))
            .with_ledger(ledger.clone());

        let mut event = pending_event();
        let obj = event.as_object_mut().unwrap();
        obj.insert("digest".to_string(), json!("digest-a"));
        obj.insert("who".to_string(), json!("operator-a"));
        obj.insert("rendered_diff".to_string(), json!("rendered summary"));
        let ctx = ctx_with_event(event);

        let ctx = node.process(ctx).await.expect("process should succeed");

        assert!(stub.last_call().is_some(), "matched digest still posts");
        let rows = ledger.rows_for("artifact-1");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].decision, LedgerDecision::Approved);
        assert_eq!(rows[0].digest, "digest-a");
        assert_eq!(rows[0].who, "operator-a");
        assert_eq!(rows[0].rendered_diff, "rendered summary");

        let result = &ctx.nodes[NODE_NAME];
        assert_eq!(result["posted"], json!(true));
    }

    #[tokio::test]
    async fn mismatched_digest_records_requeued_and_never_posts() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let ledger = std::sync::Arc::new(InMemoryApprovalLedger::new());
        let node = HarvestApproveNode::new()
            .with_http_post(std::sync::Arc::new(stub.clone()))
            .with_ledger(ledger.clone());

        let mut event = pending_event();
        let obj = event.as_object_mut().unwrap();
        obj.insert("digest".to_string(), json!("digest-delivered"));
        obj.insert("presented_digest".to_string(), json!("digest-different"));
        let ctx = ctx_with_event(event);

        let ctx = node.process(ctx).await.expect("process should succeed");

        assert!(
            stub.last_call().is_none(),
            "a digest mismatch must never execute the POST"
        );
        let rows = ledger.rows_for("artifact-1");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].decision, LedgerDecision::Requeued);

        let result = &ctx.nodes[NODE_NAME];
        assert_eq!(result["approved"], json!(false));
        assert_eq!(result["posted"], json!(false));
        assert_eq!(result["requeued"], json!(true));
    }

    #[test]
    fn no_node_writes_a_cost_usd_key() {
        // EN.8.C task 3 explicitly forbids stamping a certain budget-ledger
        // key (spelled out below, split so this very assertion string does
        // not trip on itself) into any node's ctx.nodes result — it folds
        // into BudgetLedger untyped, with no provenance check. Static
        // evidence: this file never constructs that key as a quoted JSON
        // field.
        let forbidden_key = format!("{:?}", ["cost", "usd"].join("_"));
        let src = include_str!("harvest_approve.rs");
        let occurrences = src.matches(&forbidden_key).count();
        // The only occurrence of the quoted key text is this test's own
        // `forbidden_key` construction below (not a literal quoted key), so
        // the real result-construction sites must contribute zero.
        assert_eq!(
            occurrences, 0,
            "HarvestApproveNode must never stamp a cost_usd key into ctx.nodes"
        );
    }
}
