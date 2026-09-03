//! `ClaimQueueRouterNode` (`EN.6.L` task 2) — the queue-drain loop's
//! router, copying `TaskQueueRouterNode`'s split
//! (`sdlc_flow::task_loop.rs:432-492`, per this spec's Context Pointers):
//! `process` finds the first [`ClaimStatus::Pending`] claim and stamps its
//! identity fields (plus the run's resolved [`ClaimReaffirmPolicy`]) into
//! its own `ctx.nodes` slot; `route` re-reads the durable
//! [`ClaimReaffirmState`] (a pure read — `Router::route` takes `&ctx`) to
//! pick `ClaimRecallNode` (pending remains) or `RenderReportNode` (drained,
//! `EN.6.L` task 3's identity).
//!
//! # Where the durable state lives
//!
//! Per this file's own module docs and `save_verdict.rs`'s, `SaveVerdictNode`
//! is the loop's **sole** [`ClaimReaffirmState`] writer — every pass's
//! per-claim outcome (a recorded [`Verdict`], a recall failure's
//! attempt-bump, or nothing when the pass short-circuited) is folded back
//! into the state there, under `SaveVerdictNode`'s own identity, via a
//! read-modify-write (the `SDLCState` precedent — `put_result` overwrites
//! an identity's slot wholesale, so each write carries the WHOLE claims
//! array forward, not a delta). [`latest_state`] below is this loop's
//! single source of truth for "what does the state look like right now":
//! `SaveVerdictNode`'s output when any pass has completed, falling back to
//! `LoadClaimsNode`'s initial load on the very first dispatch.
//!
//! # Policy resolution
//!
//! `LoadClaimsNode` (task 1) never touches
//! [`ClaimReaffirmInput::policy`]/[`ClaimReaffirmInput::profile`] — those
//! fields exist on the event schema for this router to resolve, once per
//! pass, against [`PolicyConfigSource::Builtin`] (mirrors
//! `content_pipeline::source_router::SourceRouterNode` — this workflow, like
//! a channel-triggered one, is invoked `POST /events/` with no worktree to
//! derive a `harness.json` path from). The resolved
//! [`ClaimReaffirmPolicy`] is stamped alongside the dispatched claim's
//! fields on every pass (cheap to re-resolve; `ctx.event` never changes
//! mid-run) so `ClaimRecallNode`/`JudgeClaimNode` read one place for both
//! "which claim" and "under what knobs".

use engine_contract::TaskContext;
use serde_json::json;

use crate::node::{Node, NodeError};
use crate::policy::PolicyConfigSource;
use crate::routing::Router;
use crate::workflows::{get_result, put_result};

use super::schema::{
    resolve_policy_for_run_from, ClaimItem, ClaimReaffirmInput, ClaimReaffirmState, ClaimStatus,
};

/// The `Node::name()` identity `ClaimQueueRouterNode` runs under, and the
/// `ctx.nodes` key its per-pass dispatch snapshot is stamped onto.
pub const NODE_NAME: &str = "ClaimQueueRouterNode";

/// The identity `LoadClaimsNode` (task 1) stamps the initial
/// [`ClaimReaffirmState`] under.
const LOAD_CLAIMS_NODE_NAME: &str = "LoadClaimsNode";

/// The identity `SaveVerdictNode` (this task) stamps the whole updated
/// [`ClaimReaffirmState`] under on every pass — the loop's sole durable
/// writer. See this module's doc comment.
pub(super) const SAVE_VERDICT_NODE_NAME: &str = "SaveVerdictNode";

/// The downstream identity a pending claim routes to.
const RECALL_NODE_TARGET: &str = "ClaimRecallNode";

/// The downstream identity a drained lane routes to (`EN.6.L` task 3).
const REPORT_NODE_TARGET: &str = "RenderReportNode";

/// Read the current [`ClaimReaffirmState`], preferring `SaveVerdictNode`'s
/// output (the newest state, once any pass has completed one full
/// recall/judge/save cycle) and falling back to `LoadClaimsNode`'s initial
/// load on the very first dispatch. `SaveVerdictNode` never runs before
/// `LoadClaimsNode` has, so this priority order alone (no logical-clock
/// comparison, unlike `sdlc_flow::task_loop::latest_state`) is sufficient:
/// once `SaveVerdictNode` has written at all, its output is always the
/// newer of the two.
pub(super) fn latest_state(ctx: &TaskContext) -> Result<ClaimReaffirmState, NodeError> {
    for identity in [SAVE_VERDICT_NODE_NAME, LOAD_CLAIMS_NODE_NAME] {
        if let Some(value) = get_result(ctx, identity) {
            return serde_json::from_value(value.clone()).map_err(|err| {
                NodeError::new(format!(
                    "{NODE_NAME}: failed to parse ClaimReaffirmState stamped by {identity}: {err}"
                ))
            });
        }
    }
    Err(NodeError::new(format!(
        "{NODE_NAME}: no ClaimReaffirmState found — neither {SAVE_VERDICT_NODE_NAME} nor \
         {LOAD_CLAIMS_NODE_NAME} has run yet"
    )))
}

/// Find the first [`ClaimStatus::Pending`] claim in `state`, if any.
fn next_pending(state: &ClaimReaffirmState) -> Option<&ClaimItem> {
    state
        .claims
        .iter()
        .find(|claim| claim.status == ClaimStatus::Pending)
}

/// Deterministic queue-drain router: dispatches the next `Pending` claim,
/// or drains to `RenderReportNode` once none remain.
pub struct ClaimQueueRouterNode;

impl ClaimQueueRouterNode {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for ClaimQueueRouterNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for ClaimQueueRouterNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let input: ClaimReaffirmInput = serde_json::from_value(ctx.event.clone())
            .map_err(|err| NodeError::new(format!("invalid CLAIM_REAFFIRM event: {err}")))?;
        let state = latest_state(&ctx)?;

        if let Some(claim) = next_pending(&state) {
            let policy = resolve_policy_for_run_from(
                &PolicyConfigSource::Builtin,
                input.profile.as_deref(),
                input.policy.as_ref(),
            )?;
            put_result(
                &mut ctx,
                NODE_NAME,
                json!({
                    "current_claim_id": claim.id,
                    "claim_text": claim.claim_text,
                    "source_doc_id": claim.source_doc_id,
                    "freshness_date": claim.freshness_date,
                    "attempt": claim.attempt,
                    "policy": policy,
                }),
            );
        }
        // Drain branch: leave any prior stamp untouched (mirrors
        // `TaskQueueRouterNode`) — `route` decides purely from `state`
        // itself, not from this node's own output.

        Ok(ctx)
    }

    fn name(&self) -> &str {
        NODE_NAME
    }

    fn as_router(&self) -> Option<&dyn Router> {
        Some(self)
    }
}

impl Router for ClaimQueueRouterNode {
    fn route(&self, ctx: &TaskContext) -> Option<String> {
        let state = latest_state(ctx).ok()?;
        if next_pending(&state).is_some() {
            Some(RECALL_NODE_TARGET.to_string())
        } else {
            Some(REPORT_NODE_TARGET.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::super::schema::Verdict;
    use super::*;

    fn empty_ctx() -> TaskContext {
        TaskContext {
            event: json!({}),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        }
    }

    fn claim(id: &str, status: ClaimStatus) -> ClaimItem {
        ClaimItem {
            id: id.to_string(),
            source_doc_id: "repo/planning/knowledge.md".to_string(),
            claim_text: format!("claim {id}"),
            freshness_date: Some("2025-01-01".to_string()),
            status,
            attempt: 0,
            verdict: None,
        }
    }

    fn ctx_with_loaded_claims(claims: Vec<ClaimItem>) -> TaskContext {
        let mut ctx = empty_ctx();
        put_result(
            &mut ctx,
            LOAD_CLAIMS_NODE_NAME,
            serde_json::to_value(ClaimReaffirmState { claims }).unwrap(),
        );
        ctx
    }

    #[tokio::test]
    async fn dispatches_the_first_pending_claim() {
        let ctx = ctx_with_loaded_claims(vec![
            claim("a", ClaimStatus::Judged),
            claim("b", ClaimStatus::Pending),
            claim("c", ClaimStatus::Pending),
        ]);
        let node = ClaimQueueRouterNode::new();

        let ctx = node.process(ctx).await.expect("process succeeds");
        let stamp = ctx.nodes.get(NODE_NAME).expect("stamped");
        assert_eq!(
            stamp.get("current_claim_id").and_then(|v| v.as_str()),
            Some("b"),
            "the first Pending claim (not the Judged one before it) is dispatched"
        );
        assert!(stamp.get("policy").is_some(), "resolved policy is stamped");
    }

    #[tokio::test]
    async fn routes_to_claim_recall_when_a_claim_is_pending() {
        let ctx = ctx_with_loaded_claims(vec![claim("a", ClaimStatus::Pending)]);
        let node = ClaimQueueRouterNode::new();

        let ctx = node.process(ctx).await.expect("process succeeds");
        assert_eq!(node.route(&ctx), Some(RECALL_NODE_TARGET.to_string()));
    }

    #[tokio::test]
    async fn routes_to_render_report_when_drained() {
        let ctx = ctx_with_loaded_claims(vec![
            claim("a", ClaimStatus::Judged),
            claim("b", ClaimStatus::Failed),
        ]);
        let node = ClaimQueueRouterNode::new();

        let ctx = node.process(ctx).await.expect("process succeeds");
        assert_eq!(node.route(&ctx), Some(REPORT_NODE_TARGET.to_string()));
    }

    #[tokio::test]
    async fn routes_to_render_report_on_an_empty_lane() {
        let ctx = ctx_with_loaded_claims(vec![]);
        let node = ClaimQueueRouterNode::new();

        let ctx = node.process(ctx).await.expect("process succeeds");
        assert_eq!(node.route(&ctx), Some(REPORT_NODE_TARGET.to_string()));
    }

    #[tokio::test]
    async fn prefers_save_verdict_node_state_over_load_claims_node() {
        let mut ctx = ctx_with_loaded_claims(vec![claim("a", ClaimStatus::Pending)]);
        // SaveVerdictNode has since judged "a" and the state now shows only
        // "b" pending — `latest_state` must prefer this over the stale
        // LoadClaimsNode snapshot.
        let mut judged_a = claim("a", ClaimStatus::Judged);
        judged_a.verdict = Some(Verdict {
            action: super::super::schema::VerdictAction::BumpFreshness,
            evidence: vec![],
            reasoning: "still true".to_string(),
            transport: None,
        });
        put_result(
            &mut ctx,
            SAVE_VERDICT_NODE_NAME,
            serde_json::to_value(ClaimReaffirmState {
                claims: vec![judged_a, claim("b", ClaimStatus::Pending)],
            })
            .unwrap(),
        );
        let node = ClaimQueueRouterNode::new();

        let ctx = node.process(ctx).await.expect("process succeeds");
        let stamp = ctx.nodes.get(NODE_NAME).expect("stamped");
        assert_eq!(
            stamp.get("current_claim_id").and_then(|v| v.as_str()),
            Some("b")
        );
    }

    #[test]
    fn latest_state_errors_when_neither_node_has_run() {
        let ctx = empty_ctx();
        let err = latest_state(&ctx).expect_err("no state stamped yet");
        assert!(err.message.contains("no ClaimReaffirmState found"));
    }
}
