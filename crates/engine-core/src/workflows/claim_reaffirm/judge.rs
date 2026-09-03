//! `ClaimRecallNode` + `JudgeClaimNode` (`EN.6.L` task 2) — the per-claim
//! evidence fetch and verdict judgment halves of the queue-drain loop.
//! Colocated in one file (no separate `files[]` entry for a recall module
//! exists on this task) since both exist only to feed
//! `save_verdict::SaveVerdictNode`'s single read-modify-write.
//!
//! # `ClaimRecallNode`
//!
//! A thin wrapper around `EN.6.K`'s [`crate::nodes::RecallNode`]: the query
//! is built from the dispatched claim's text + source doc id (the
//! identifier-anchored pattern the Brain's own `knowledge.md` documents as
//! what scores), stashed under a private `ctx.nodes` identity, and handed
//! to an inner `RecallNode` bound to read it. Recall failure — a transport
//! error, not merely a zero-result success — does **not** halt the lane:
//! this node stamps `{"failed": true, ...}` and lets execution continue
//! down the graph's fixed edge to `JudgeClaimNode`, which detects the flag
//! and skips its own model call. `SaveVerdictNode` is what actually decides
//! the claim's fate on a recall failure (bump `attempt`, mark
//! [`super::schema::ClaimStatus::Failed`] once `max_attempts` is spent,
//! else leave it `Pending` so the drain retries it next pass) — this node
//! only reports what happened, per this workflow's single-writer
//! discipline (see `queue_router`'s module doc).
//!
//! # `JudgeClaimNode`
//!
//! One `ClaudeCodeStep` call per claim, asking only for an `action` +
//! `reasoning` — never for the evidence citations themselves. Citations are
//! built deterministically from `ClaimRecallNode`'s actual recall results,
//! not from anything the model claims to have cited, which is what makes
//! the OR.K3 guard structural rather than a prompt instruction the model
//! could ignore: **when the recall result set is empty, any model action of
//! `BumpFreshness`/`Supersede` is forced to `NeedsHuman` in code**,
//! regardless of what the model returned.

use std::sync::Arc;

use claude_code_rs::Config;
use engine_contract::TaskContext;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::node::{Node, NodeError};
use crate::nodes::{
    BrainConfig, ClaudeCodeStep, HttpGet, MetaTransport, RecallNode, RecallResult, RECALL_NODE_NAME,
};
use crate::policy::LocalConfig;
use crate::workflows::{
    get_result, parse_structured_or_fenced, put_result, session_baseline, sessions_since,
    ModelTransport, TransportSlot,
};

use super::queue_router;
use super::schema::{
    Citation, ClaimReaffirmPolicy, TransportInfo as ClaimTransportInfo, Verdict, VerdictAction,
};

// ---------------------------------------------------------------------------
// Shared: reading the dispatched claim + resolved policy off the router
// ---------------------------------------------------------------------------

/// The dispatched claim's identity fields plus the run's resolved
/// [`ClaimReaffirmPolicy`], as `ClaimQueueRouterNode::process` stamps them.
struct DispatchedClaim {
    claim_id: String,
    claim_text: String,
    source_doc_id: String,
    freshness_date: Option<String>,
    policy: ClaimReaffirmPolicy,
}

fn read_dispatched_claim(ctx: &TaskContext, caller: &str) -> Result<DispatchedClaim, NodeError> {
    let stamp = get_result(ctx, queue_router::NODE_NAME).ok_or_else(|| {
        NodeError::new(format!(
            "{caller}: {} has not dispatched a claim yet",
            queue_router::NODE_NAME
        ))
    })?;
    let claim_id = stamp
        .get("current_claim_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            NodeError::new(format!(
                "{caller}: {} output missing current_claim_id",
                queue_router::NODE_NAME
            ))
        })?
        .to_string();
    let claim_text = stamp
        .get("claim_text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let source_doc_id = stamp
        .get("source_doc_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let freshness_date = stamp
        .get("freshness_date")
        .and_then(Value::as_str)
        .map(str::to_string);
    let policy: ClaimReaffirmPolicy = stamp
        .get("policy")
        .cloned()
        .ok_or_else(|| NodeError::new(format!("{caller}: dispatched claim missing policy")))
        .and_then(|value| {
            serde_json::from_value(value)
                .map_err(|err| NodeError::new(format!("{caller}: invalid stamped policy: {err}")))
        })?;

    Ok(DispatchedClaim {
        claim_id,
        claim_text,
        source_doc_id,
        freshness_date,
        policy,
    })
}

// ---------------------------------------------------------------------------
// ClaimRecallNode
// ---------------------------------------------------------------------------

/// The `Node::name()` identity `ClaimRecallNode` runs under, and the
/// `ctx.nodes` key its result is stamped onto.
pub const NODE_NAME: &str = "ClaimRecallNode";

/// Private `ctx.nodes` identity the composed query string is stashed under
/// before delegating to the inner [`RecallNode`] (bound via
/// `with_input_from`) — never read by anything outside this node.
const QUERY_INPUT_IDENTITY: &str = "ClaimRecallQuery";

/// `EN.6.K`'s own `RecallNode` default for `hybrid` — this wrapper does not
/// expose a knob for it (out of this block's scope, matching `RecallNode`'s
/// own "not a Policy knob" reasoning), it simply forwards the seam's
/// default.
const DEFAULT_HYBRID: bool = crate::nodes::DEFAULT_RECALL_HYBRID;

/// Composes `EN.6.K`'s [`RecallNode`], querying on the dispatched claim's
/// text + source doc id. Recall failure is contained here (never halts the
/// lane) — see this module's doc comment for the full failure-handling
/// contract.
pub struct ClaimRecallNode {
    http_get: Arc<dyn HttpGet>,
    config: BrainConfig,
    hybrid: bool,
}

impl ClaimRecallNode {
    /// Build a `ClaimRecallNode` targeting `config`'s Brain, with the live
    /// `reqwest`-backed [`HttpGet`] seam.
    #[must_use]
    pub fn new(config: BrainConfig) -> Self {
        Self {
            http_get: crate::nodes::http_get_live(),
            config,
            hybrid: DEFAULT_HYBRID,
        }
    }

    /// Override the `HttpGet` seam. Tests inject a `StubHttpGet` so the
    /// gated suite never contacts a live Brain.
    #[must_use]
    pub fn with_http_get(mut self, http_get: Arc<dyn HttpGet>) -> Self {
        self.http_get = http_get;
        self
    }

    /// Override the `hybrid` query param the inner `RecallNode` sends.
    #[must_use]
    pub fn with_hybrid(mut self, hybrid: bool) -> Self {
        self.hybrid = hybrid;
        self
    }
}

/// The identifier-anchored recall query: the source doc id first (the
/// pattern the Brain's own `knowledge.md` documents as what scores), then
/// the claim text itself.
fn build_recall_query(source_doc_id: &str, claim_text: &str) -> String {
    format!("{source_doc_id}: {claim_text}")
}

#[async_trait::async_trait]
impl Node for ClaimRecallNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let dispatched = read_dispatched_claim(&ctx, NODE_NAME)?;
        let query = build_recall_query(&dispatched.source_doc_id, &dispatched.claim_text);

        // Cloned BEFORE the query is stashed / the inner RecallNode
        // delegated to: `Node::process` consumes `ctx` by value and the
        // inner node's `Err` path drops it entirely, so without this
        // snapshot a recall failure would have no `ctx` left to record the
        // failure onto (the exact per-item containment this node exists
        // for).
        let mut ctx = ctx;
        let snapshot = ctx.clone();
        put_result(&mut ctx, QUERY_INPUT_IDENTITY, json!(query));

        let inner = RecallNode::new(self.config.clone())
            .with_http_get(self.http_get.clone())
            .with_input_from(QUERY_INPUT_IDENTITY)
            .with_limit(dispatched.policy.recall_limit)
            .with_hybrid(self.hybrid);

        match inner.process(ctx).await {
            Ok(mut ctx) => {
                let results = ctx
                    .nodes
                    .get(RECALL_NODE_NAME)
                    .and_then(|value| value.get("results"))
                    .cloned()
                    .unwrap_or_else(|| json!([]));
                put_result(
                    &mut ctx,
                    NODE_NAME,
                    json!({
                        "failed": false,
                        "results": results,
                    }),
                );
                Ok(ctx)
            }
            Err(err) => {
                let mut ctx = snapshot;
                put_result(
                    &mut ctx,
                    NODE_NAME,
                    json!({
                        "failed": true,
                        "error": err.message,
                        "results": [],
                    }),
                );
                Ok(ctx)
            }
        }
    }

    fn name(&self) -> &str {
        NODE_NAME
    }
}

/// Read `ClaimRecallNode`'s stamp: `(failed, results)`. Missing entirely
/// (the node has not run) is treated as a failure with no results, rather
/// than panicking — defensive, never expected to be exercised on the wired
/// graph.
fn read_recall_result(ctx: &TaskContext) -> (bool, Vec<RecallResult>) {
    let Some(stamp) = get_result(ctx, NODE_NAME) else {
        return (true, Vec::new());
    };
    let failed = stamp.get("failed").and_then(Value::as_bool).unwrap_or(true);
    let results: Vec<RecallResult> = stamp
        .get("results")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    (failed, results)
}

// ---------------------------------------------------------------------------
// JudgeClaimNode
// ---------------------------------------------------------------------------

/// The `Node::name()` identity `JudgeClaimNode` runs its composed
/// `ClaudeCodeStep` under, and the `ctx.nodes` key its output is stamped
/// onto. Read by `SaveVerdictNode`.
pub const JUDGE_NODE_NAME: &str = "JudgeClaimNode";

/// A stable, run-invariant system-prompt prefix used as the cache-breakpoint
/// anchor when `policy.prompt_cache` is true — kept for parity with the
/// other model nodes in this crate even though `ClaimReaffirmPolicy` does
/// not (yet) expose a `prompt_cache` knob; the prompt text itself must
/// still live in a colocated file per standing rule 7 (D24).
const JUDGE_PROMPT: &str = include_str!("prompts/judge.md");

/// How much of a recall hit's `content` to fold into the judge prompt and
/// the resulting [`Citation::snippet`] — enough context to judge on,
/// without flooding the prompt with a whole document.
const EVIDENCE_SNIPPET_LEN: usize = 400;

/// The model's reply shape: only `action` + `reasoning` — citations are
/// never taken from the model (see this module's doc comment); they are
/// built deterministically from `ClaimRecallNode`'s actual results.
#[derive(Debug, Clone, Deserialize)]
struct JudgeOutput {
    action: String,
    #[serde(default)]
    reasoning: String,
}

/// JSON schema matching [`JudgeOutput`].
fn judge_output_json_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": ["bump_freshness", "supersede", "archive", "needs_human"],
            },
            "reasoning": { "type": "string" },
        },
        "required": ["action", "reasoning"],
    })
}

/// Parse a model's `action` string into a [`VerdictAction`], defaulting to
/// [`VerdictAction::NeedsHuman`] for anything unrecognized — fail closed,
/// mirroring `content_pipeline::self_critic::verdict_from_model_text`'s
/// "an ambiguous signal must not let the loop silently proceed" reasoning.
fn verdict_action_from_model_text(text: &str) -> VerdictAction {
    match text.trim().to_lowercase().as_str() {
        "bump_freshness" | "bump-freshness" => VerdictAction::BumpFreshness,
        "supersede" => VerdictAction::Supersede,
        "archive" => VerdictAction::Archive,
        _ => VerdictAction::NeedsHuman,
    }
}

fn truncate_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// Build the judge prompt from the claim + its freshness date + the actual
/// recall evidence found for it.
fn build_prompt(
    claim_text: &str,
    source_doc_id: &str,
    freshness_date: Option<&str>,
    results: &[RecallResult],
) -> String {
    let freshness = freshness_date.unwrap_or("unknown");
    let mut evidence = String::new();
    if results.is_empty() {
        evidence.push_str("(no corpus evidence was found for this claim)\n");
    } else {
        for (idx, hit) in results.iter().enumerate() {
            let doc_id = hit.doc_id.clone().unwrap_or_else(|| hit.file_path.clone());
            evidence.push_str(&format!(
                "{}. [{doc_id}] {}\n",
                idx + 1,
                truncate_chars(&hit.content, EVIDENCE_SNIPPET_LEN)
            ));
        }
    }

    format!(
        "You are reaffirming a distilled knowledge claim against the corpus's current state. \
         Decide whether the claim still holds, has been superseded, should be archived, or \
         needs a human to decide. Respond with strict JSON matching {{\"action\": \
         \"bump_freshness\" | \"supersede\" | \"archive\" | \"needs_human\", \"reasoning\": \
         string}}.\n\n\
         Claim (source: {source_doc_id}, last freshness stamp: {freshness}):\n{claim_text}\n\n\
         Corpus evidence found for this claim:\n{evidence}"
    )
}

/// One `ClaudeCodeStep` per claim: reads `ClaimRecallNode`'s evidence,
/// judges via the model, and structurally enforces the OR.K3 guard before
/// storing a [`Verdict`]. Skips the model call entirely (no billed session)
/// when `ClaimRecallNode` reported a recall failure — `SaveVerdictNode`
/// handles that case's attempt-bump/retry instead.
pub struct JudgeClaimNode {
    config: Config,
    transport: TransportSlot,
}

impl JudgeClaimNode {
    /// Construct with the judge-output `json_schema` set; `process`
    /// overwrites `model` per the resolved `judge_model_tier`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: Config {
                json_schema: Some(judge_output_json_schema()),
                ..Config::default()
            },
            transport: TransportSlot::default(),
        }
    }

    /// Override the transport used by the composed `ClaudeCodeStep`. Tests
    /// use this to stub a real subprocess call with a canned `Outcome`.
    #[must_use]
    pub fn with_transport(mut self, transport: ModelTransport) -> Self {
        self.transport.set_plain(transport);
        self
    }

    /// Override the transport with a tier-aware [`MetaTransport`] that
    /// reports its own [`crate::nodes::TransportInfo`] — the seam that
    /// keeps the local->cloud fallback visible per claim (see this
    /// module's doc comment), taking precedence over
    /// [`Self::with_transport`].
    #[must_use]
    pub fn with_meta_transport(mut self, transport: MetaTransport) -> Self {
        self.transport.set_meta(transport);
        self
    }
}

impl Default for JudgeClaimNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for JudgeClaimNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let dispatched = read_dispatched_claim(&ctx, JUDGE_NODE_NAME)?;
        let (recall_failed, results) = read_recall_result(&ctx);

        let mut ctx = ctx;
        if recall_failed {
            // `ClaimRecallNode` already recorded the failure;
            // `SaveVerdictNode` owns the attempt-bump/retry decision. No
            // billed model call for a claim with no evidence to judge on.
            put_result(&mut ctx, JUDGE_NODE_NAME, json!({ "skipped": true }));
            return Ok(ctx);
        }

        let local_model = LocalConfig::default().model;
        let mut config = self.config.clone();
        config = crate::policy::apply_model_tier(
            config,
            dispatched.policy.judge_model_tier,
            &local_model,
        );
        // `ClaimReaffirmPolicy` (task 1) exposes no `prompt_cache` knob, so
        // this stable system-prompt prefix is applied unconditionally
        // rather than gated behind one — it is still the file this node's
        // const is required to be per standing rule 7 (D24), and a stable
        // prefix costs nothing when prompt caching happens to be off at the
        // transport layer.
        config = crate::policy::apply_prompt_cache(config, true, JUDGE_PROMPT);
        let prompt = build_prompt(
            &dispatched.claim_text,
            &dispatched.source_doc_id,
            dispatched.freshness_date.as_deref(),
            &results,
        );

        let step = self
            .transport
            .apply(ClaudeCodeStep::new(JUDGE_NODE_NAME, config, prompt));

        let baseline = session_baseline(&ctx);
        let mut ctx = step.process(ctx).await?;

        let content = ctx
            .nodes
            .get(JUDGE_NODE_NAME)
            .and_then(|value| value.get("content"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let transport_stamp = ctx
            .nodes
            .get(JUDGE_NODE_NAME)
            .and_then(|value| value.get("transport"))
            .cloned();

        let parsed: JudgeOutput = parse_structured_or_fenced(&ctx, JUDGE_NODE_NAME, &content)
            .map_err(|err| {
                NodeError::new(format!(
                    "{JUDGE_NODE_NAME}: failed to parse a verdict from the model's reply: {err}"
                ))
                .with_sessions(sessions_since(&ctx, baseline))
            })?;

        let mut action = verdict_action_from_model_text(&parsed.action);
        // Structural OR.K3 guard: empty evidence can never yield
        // BumpFreshness/Supersede, regardless of what the model returned.
        if results.is_empty()
            && matches!(
                action,
                VerdictAction::BumpFreshness | VerdictAction::Supersede
            )
        {
            action = VerdictAction::NeedsHuman;
        }

        let evidence: Vec<Citation> = results
            .iter()
            .map(|hit| Citation {
                doc_id: hit.doc_id.clone().unwrap_or_else(|| hit.file_path.clone()),
                file_path: Some(hit.file_path.clone()),
                snippet: truncate_chars(&hit.content, EVIDENCE_SNIPPET_LEN),
            })
            .collect();

        let transport: Option<ClaimTransportInfo> = transport_stamp
            .as_ref()
            .and_then(|value| value.get("tier").and_then(Value::as_str))
            .map(|tier| ClaimTransportInfo {
                tier: tier.to_string(),
                model: transport_stamp
                    .as_ref()
                    .and_then(|value| value.get("model").and_then(Value::as_str))
                    .unwrap_or_default()
                    .to_string(),
                endpoint: transport_stamp
                    .as_ref()
                    .and_then(|value| value.get("endpoint").and_then(Value::as_str))
                    .map(str::to_string),
            });

        let verdict = Verdict {
            action,
            evidence,
            reasoning: parsed.reasoning,
            transport,
        };

        let mut result = serde_json::to_value(&verdict).map_err(|err| {
            NodeError::new(format!("failed to serialize Verdict: {err}"))
                .with_sessions(sessions_since(&ctx, baseline))
        })?;
        result["skipped"] = json!(false);
        put_result(&mut ctx, JUDGE_NODE_NAME, result);
        let _ = &dispatched.claim_id; // identity carried by ClaimQueueRouterNode's own stamp

        Ok(ctx)
    }

    fn name(&self) -> &str {
        JUDGE_NODE_NAME
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use claude_code_rs::parse::{ModelUsage as SdkModelUsage, Usage as SdkUsage};
    use claude_code_rs::Outcome;
    use futures::future::BoxFuture;
    use futures::FutureExt;

    use crate::nodes::StubHttpGet;
    use crate::policy::ModelTier;

    use super::super::schema::ClaimReaffirmPolicy;
    use super::*;

    fn empty_ctx() -> TaskContext {
        TaskContext {
            event: json!({}),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        }
    }

    fn ctx_with_dispatched_claim(claim_text: &str, source_doc_id: &str) -> TaskContext {
        let mut ctx = empty_ctx();
        put_result(
            &mut ctx,
            queue_router::NODE_NAME,
            json!({
                "current_claim_id": "claim-1",
                "claim_text": claim_text,
                "source_doc_id": source_doc_id,
                "freshness_date": "2025-01-01",
                "attempt": 0,
                "policy": ClaimReaffirmPolicy::default(),
            }),
        );
        ctx
    }

    // -- ClaimRecallNode --------------------------------------------------

    #[tokio::test]
    async fn recall_success_stamps_results_and_not_failed() {
        let ctx = ctx_with_dispatched_claim("Claim text", "planning/status.md");
        let body = json!({
            "query": "planning/status.md: Claim text",
            "count": 1,
            "results": [{
                "doc_id": "planning/status.md",
                "file_path": "planning/status.md",
                "title": null,
                "section": null,
                "content": "evidence content",
                "score": 0.9,
                "via": "hybrid",
            }],
        });
        let node = ClaimRecallNode::new(BrainConfig::new("http://localhost:8000", None))
            .with_http_get(Arc::new(StubHttpGet::succeeding(body)));

        let ctx = node.process(ctx).await.expect("process succeeds");
        let stamp = ctx.nodes.get(NODE_NAME).expect("stamped");
        assert_eq!(stamp.get("failed").and_then(Value::as_bool), Some(false));
        let results = stamp.get("results").and_then(Value::as_array).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn recall_query_is_identifier_anchored() {
        let ctx = ctx_with_dispatched_claim("Claim text", "planning/status.md");
        let stub = StubHttpGet::succeeding(json!({ "query": "q", "count": 0, "results": [] }));
        let node = ClaimRecallNode::new(BrainConfig::new("http://localhost:8000", None))
            .with_http_get(Arc::new(stub.clone()));

        node.process(ctx).await.expect("process succeeds");
        let (_, query, _) = stub.last_call().expect("fetch called");
        let q = query.iter().find(|(name, _)| name == "q").unwrap();
        assert_eq!(q.1, "planning/status.md: Claim text");
    }

    #[tokio::test]
    async fn recall_failure_is_contained_not_propagated() {
        let ctx = ctx_with_dispatched_claim("Claim text", "planning/status.md");
        let node = ClaimRecallNode::new(BrainConfig::new("http://localhost:8000", None))
            .with_http_get(Arc::new(StubHttpGet::failing("connection refused")));

        let ctx = node.process(ctx).await.expect("process does not error");
        let stamp = ctx.nodes.get(NODE_NAME).expect("stamped");
        assert_eq!(stamp.get("failed").and_then(Value::as_bool), Some(true));
        assert_eq!(
            stamp
                .get("results")
                .and_then(Value::as_array)
                .unwrap()
                .len(),
            0
        );
        // The dispatched-claim stamp from ClaimQueueRouterNode must survive
        // (the pre-delegation snapshot, not a partially-mutated ctx).
        assert!(ctx.nodes.contains_key(queue_router::NODE_NAME));
    }

    // -- JudgeClaimNode -----------------------------------------------------

    fn stub_transport(structured: Value) -> ModelTransport {
        Arc::new(move |_config: Config, _prompt: String| {
            let structured = structured.clone();
            async move {
                Ok(Outcome {
                    text: serde_json::to_string(&structured).unwrap(),
                    cost_usd: 0.01,
                    usage: SdkUsage {
                        input_tokens: 80,
                        output_tokens: 30,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::from([(
                        "claude-sonnet-4-5".to_string(),
                        SdkModelUsage {
                            input_tokens: 80,
                            output_tokens: 30,
                            cache_read_input_tokens: 0,
                            cache_creation_input_tokens: 0,
                            cost_usd: 0.01,
                        },
                    )]),
                    session_id: None,
                    structured_output: Some(structured.clone()),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        })
    }

    fn put_recall_result(ctx: &mut TaskContext, failed: bool, results: Vec<RecallResult>) {
        put_result(
            ctx,
            NODE_NAME,
            json!({ "failed": failed, "results": results }),
        );
    }

    fn evidence_hit(doc_id: &str, content: &str) -> RecallResult {
        RecallResult {
            doc_id: Some(doc_id.to_string()),
            file_path: doc_id.to_string(),
            title: None,
            section: None,
            content: content.to_string(),
            score: 0.9,
            via: "hybrid".to_string(),
        }
    }

    #[tokio::test]
    async fn judge_emits_verdict_with_evidence_from_recall_results() {
        let mut ctx = ctx_with_dispatched_claim("Claim text", "planning/status.md");
        put_recall_result(
            &mut ctx,
            false,
            vec![evidence_hit("planning/status.md", "still true today")],
        );
        let node = JudgeClaimNode::new().with_transport(stub_transport(json!({
            "action": "bump_freshness",
            "reasoning": "corroborated",
        })));

        let ctx = node.process(ctx).await.expect("process succeeds");
        let verdict: Verdict =
            serde_json::from_value(ctx.nodes[JUDGE_NODE_NAME].clone()).expect("valid Verdict");
        assert_eq!(verdict.action, VerdictAction::BumpFreshness);
        assert_eq!(verdict.evidence.len(), 1);
        assert_eq!(verdict.evidence[0].doc_id, "planning/status.md");
    }

    #[tokio::test]
    async fn judge_forces_needs_human_when_evidence_empty_even_if_model_says_bump() {
        let mut ctx = ctx_with_dispatched_claim("Claim text", "planning/deleted.md");
        put_recall_result(&mut ctx, false, vec![]);
        let node = JudgeClaimNode::new().with_transport(stub_transport(json!({
            "action": "bump_freshness",
            "reasoning": "looks fine to me",
        })));

        let ctx = node.process(ctx).await.expect("process succeeds");
        let verdict: Verdict =
            serde_json::from_value(ctx.nodes[JUDGE_NODE_NAME].clone()).expect("valid Verdict");
        assert_eq!(
            verdict.action,
            VerdictAction::NeedsHuman,
            "OR.K3 guard must override the model's illegal BumpFreshness on empty evidence"
        );
        assert!(verdict.evidence.is_empty());
    }

    #[tokio::test]
    async fn judge_allows_archive_on_empty_evidence_without_override() {
        let mut ctx = ctx_with_dispatched_claim("Claim text", "planning/deleted.md");
        put_recall_result(&mut ctx, false, vec![]);
        let node = JudgeClaimNode::new().with_transport(stub_transport(json!({
            "action": "archive",
            "reasoning": "source document no longer exists",
        })));

        let ctx = node.process(ctx).await.expect("process succeeds");
        let verdict: Verdict =
            serde_json::from_value(ctx.nodes[JUDGE_NODE_NAME].clone()).expect("valid Verdict");
        assert_eq!(verdict.action, VerdictAction::Archive);
    }

    #[tokio::test]
    async fn judge_skips_model_call_when_recall_failed() {
        let mut ctx = ctx_with_dispatched_claim("Claim text", "planning/status.md");
        put_result(
            &mut ctx,
            NODE_NAME,
            json!({ "failed": true, "error": "connection refused", "results": [] }),
        );
        let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let called_clone = called.clone();
        let transport: ModelTransport = Arc::new(move |_config, _prompt| {
            called_clone.store(true, std::sync::atomic::Ordering::SeqCst);
            let fut: BoxFuture<'static, claude_code_rs::Result<Outcome>> =
                async move { unreachable!("must not be called when recall failed") }.boxed();
            fut
        });
        let node = JudgeClaimNode::new().with_transport(transport);

        let ctx = node.process(ctx).await.expect("process succeeds");
        assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
        let stamp = ctx.nodes.get(JUDGE_NODE_NAME).expect("stamped");
        assert_eq!(stamp.get("skipped").and_then(Value::as_bool), Some(true));
    }

    #[tokio::test]
    async fn judge_applies_resolved_model_tier() {
        let mut ctx = empty_ctx();
        put_result(
            &mut ctx,
            queue_router::NODE_NAME,
            json!({
                "current_claim_id": "claim-1",
                "claim_text": "Claim text",
                "source_doc_id": "planning/status.md",
                "freshness_date": "2025-01-01",
                "attempt": 0,
                "policy": ClaimReaffirmPolicy {
                    judge_model_tier: ModelTier::Opus,
                    ..ClaimReaffirmPolicy::default()
                },
            }),
        );
        put_recall_result(&mut ctx, false, vec![evidence_hit("x", "content")]);

        let captured: Arc<std::sync::Mutex<Option<Config>>> = Arc::new(std::sync::Mutex::new(None));
        let captured_clone = captured.clone();
        let structured = json!({ "action": "bump_freshness", "reasoning": "ok" });
        let transport: ModelTransport = Arc::new(move |config, _prompt| {
            *captured_clone.lock().unwrap() = Some(config);
            let structured = structured.clone();
            async move {
                Ok(Outcome {
                    text: serde_json::to_string(&structured).unwrap(),
                    cost_usd: 0.0,
                    usage: SdkUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::new(),
                    session_id: None,
                    structured_output: Some(structured.clone()),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        });
        let node = JudgeClaimNode::new().with_transport(transport);

        node.process(ctx).await.expect("process succeeds");
        let config = captured.lock().unwrap().take().expect("transport called");
        assert_eq!(config.model.as_deref(), Some("claude-opus-4-8"));
    }

    #[test]
    fn verdict_action_from_model_text_defaults_ambiguous_to_needs_human() {
        assert_eq!(
            verdict_action_from_model_text("bump_freshness"),
            VerdictAction::BumpFreshness
        );
        assert_eq!(
            verdict_action_from_model_text("supersede"),
            VerdictAction::Supersede
        );
        assert_eq!(
            verdict_action_from_model_text("archive"),
            VerdictAction::Archive
        );
        assert_eq!(
            verdict_action_from_model_text("needs_human"),
            VerdictAction::NeedsHuman
        );
        assert_eq!(
            verdict_action_from_model_text("something else"),
            VerdictAction::NeedsHuman
        );
    }

    #[test]
    fn build_recall_query_is_identifier_anchored() {
        let query = build_recall_query("planning/status.md", "Claim text");
        assert_eq!(query, "planning/status.md: Claim text");
    }
}
