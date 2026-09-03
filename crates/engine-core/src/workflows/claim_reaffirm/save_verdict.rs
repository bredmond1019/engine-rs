//! `SaveVerdictNode` (`EN.6.L` task 2) — the queue-drain loop's sole
//! [`ClaimReaffirmState`] writer. Every pass ends here: a read-modify-write
//! over the durable claims array (`queue_router::latest_state`'s snapshot),
//! folding in whatever this pass produced, and re-stamping the WHOLE array
//! under this node's own identity (`put_result` overwrites wholesale — the
//! `SDLCState` precedent) so `queue_router::latest_state` picks it up as
//! the newest state on the loop's back-edge.
//!
//! Two outcomes reach this node, distinguished by `JudgeClaimNode`'s
//! `"skipped"` flag:
//!
//! - **Judged** (`skipped: false`) — a [`Verdict`] was recorded. The
//!   dispatched claim is set to [`ClaimStatus::Judged`] with that verdict
//!   attached; `attempt` is left as `ClaimQueueRouterNode` dispatched it.
//! - **Recall failed** (`skipped: true`) — `ClaimRecallNode` hit a
//!   transport error, not merely zero results. `attempt` is bumped by one;
//!   once it reaches the resolved `ClaimReaffirmPolicy::max_attempts`, the
//!   claim is marked [`ClaimStatus::Failed`] (the drain gives up and moves
//!   on — this per-item containment is the whole point of the queue-drain
//!   shape, per the spec's Context Pointers); otherwise it is left
//!   [`ClaimStatus::Pending`] so `ClaimQueueRouterNode`'s next pass
//!   re-dispatches the SAME claim (it is still the first `Pending` entry)
//!   for another recall attempt.

use engine_contract::TaskContext;

use crate::node::{Node, NodeError};
use crate::workflows::{get_result, put_result};

use super::judge::JUDGE_NODE_NAME;
use super::queue_router::{self, latest_state};
use super::schema::{ClaimStatus, Verdict};

/// The `Node::name()` identity `SaveVerdictNode` runs under, and the
/// `ctx.nodes` key the whole updated [`super::schema::ClaimReaffirmState`]
/// is stamped onto — `queue_router::SAVE_VERDICT_NODE_NAME`, re-exported
/// here so callers reach for one name.
pub const NODE_NAME: &str = queue_router::SAVE_VERDICT_NODE_NAME;

/// Resolve the max-attempts knob the dispatched claim was judged/recalled
/// under, from `ClaimQueueRouterNode`'s own stamp (it already resolved and
/// carried the full policy for this pass).
fn dispatched_max_attempts(ctx: &TaskContext) -> Result<u32, NodeError> {
    let stamp = get_result(ctx, queue_router::NODE_NAME).ok_or_else(|| {
        NodeError::new(format!(
            "{NODE_NAME}: {} has not dispatched a claim yet",
            queue_router::NODE_NAME
        ))
    })?;
    stamp
        .get("policy")
        .and_then(|policy| policy.get("max_attempts"))
        .and_then(serde_json::Value::as_u64)
        .map(|value| value as u32)
        .ok_or_else(|| {
            NodeError::new(format!(
                "{NODE_NAME}: dispatched claim's policy missing max_attempts"
            ))
        })
}

fn dispatched_claim_id(ctx: &TaskContext) -> Result<String, NodeError> {
    get_result(ctx, queue_router::NODE_NAME)
        .and_then(|stamp| stamp.get("current_claim_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            NodeError::new(format!(
                "{NODE_NAME}: {} output missing current_claim_id",
                queue_router::NODE_NAME
            ))
        })
}

/// `true` when `JudgeClaimNode` skipped its model call (a recall failure,
/// per `ClaimRecallNode`'s containment contract), `false` when it produced
/// a real [`Verdict`]. Absent entirely (should not happen on the wired
/// graph) is treated as skipped — never silently records a phantom
/// verdict.
fn judge_skipped(ctx: &TaskContext) -> bool {
    get_result(ctx, JUDGE_NODE_NAME)
        .and_then(|stamp| stamp.get("skipped"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true)
}

/// The `SaveVerdictNode` — read-modify-write accumulator for
/// [`super::schema::ClaimReaffirmState`].
pub struct SaveVerdictNode;

impl SaveVerdictNode {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for SaveVerdictNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for SaveVerdictNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let mut state = latest_state(&ctx)?;
        let claim_id = dispatched_claim_id(&ctx)?;
        let max_attempts = dispatched_max_attempts(&ctx)?;

        let claim = state
            .claims
            .iter_mut()
            .find(|claim| claim.id == claim_id)
            .ok_or_else(|| {
                NodeError::new(format!(
                    "{NODE_NAME}: dispatched claim {claim_id:?} not found in ClaimReaffirmState"
                ))
            })?;

        if judge_skipped(&ctx) {
            claim.attempt += 1;
            if claim.attempt >= max_attempts {
                claim.status = ClaimStatus::Failed;
            }
            // else: left Pending — ClaimQueueRouterNode's next pass
            // re-dispatches this same claim for another recall attempt.
        } else {
            let verdict_value = get_result(&ctx, JUDGE_NODE_NAME).cloned().ok_or_else(|| {
                NodeError::new(format!(
                    "{NODE_NAME}: {JUDGE_NODE_NAME} did not stamp a result"
                ))
            })?;
            let verdict: Verdict = serde_json::from_value(verdict_value).map_err(|err| {
                NodeError::new(format!(
                    "{NODE_NAME}: failed to parse Verdict stamped by {JUDGE_NODE_NAME}: {err}"
                ))
            })?;
            claim.verdict = Some(verdict);
            claim.status = ClaimStatus::Judged;
        }

        let mut ctx = ctx;
        put_result(
            &mut ctx,
            NODE_NAME,
            serde_json::to_value(&state).map_err(|err| {
                NodeError::new(format!(
                    "{NODE_NAME}: failed to serialize ClaimReaffirmState: {err}"
                ))
            })?,
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

    use super::super::schema::{
        Citation, ClaimItem, ClaimReaffirmPolicy, ClaimReaffirmState, VerdictAction,
    };
    use super::*;

    fn empty_ctx() -> TaskContext {
        TaskContext {
            event: json!({}),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        }
    }

    fn claim(id: &str, status: ClaimStatus, attempt: u32) -> ClaimItem {
        ClaimItem {
            id: id.to_string(),
            source_doc_id: "repo/planning/knowledge.md".to_string(),
            claim_text: format!("claim {id}"),
            freshness_date: Some("2025-01-01".to_string()),
            status,
            attempt,
            verdict: None,
        }
    }

    fn ctx_with_state_and_dispatch(
        claims: Vec<ClaimItem>,
        dispatched_id: &str,
        policy: ClaimReaffirmPolicy,
    ) -> TaskContext {
        let mut ctx = empty_ctx();
        put_result(
            &mut ctx,
            "LoadClaimsNode",
            serde_json::to_value(ClaimReaffirmState { claims }).unwrap(),
        );
        put_result(
            &mut ctx,
            queue_router::NODE_NAME,
            json!({
                "current_claim_id": dispatched_id,
                "claim_text": "claim text",
                "source_doc_id": "repo/planning/knowledge.md",
                "freshness_date": "2025-01-01",
                "attempt": 0,
                "policy": policy,
            }),
        );
        ctx
    }

    #[tokio::test]
    async fn records_a_judged_verdict_and_sets_status_judged() {
        let mut ctx = ctx_with_state_and_dispatch(
            vec![claim("a", ClaimStatus::Pending, 0)],
            "a",
            ClaimReaffirmPolicy::default(),
        );
        put_result(
            &mut ctx,
            JUDGE_NODE_NAME,
            json!({
                "action": "bump_freshness",
                "evidence": [{"doc_id": "planning/status.md", "file_path": "planning/status.md", "snippet": "still true"}],
                "reasoning": "corroborated",
                "transport": null,
                "skipped": false,
            }),
        );
        let node = SaveVerdictNode::new();

        let ctx = node.process(ctx).await.expect("process succeeds");
        let state: ClaimReaffirmState =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid state");
        let claim = &state.claims[0];
        assert_eq!(claim.status, ClaimStatus::Judged);
        assert_eq!(
            claim.verdict.as_ref().unwrap().action,
            VerdictAction::BumpFreshness
        );
        assert_eq!(claim.verdict.as_ref().unwrap().evidence.len(), 1);
    }

    #[tokio::test]
    async fn accumulator_grows_without_clobbering_other_claims() {
        let mut ctx = ctx_with_state_and_dispatch(
            vec![
                claim("a", ClaimStatus::Judged, 0),
                claim("b", ClaimStatus::Pending, 0),
            ],
            "b",
            ClaimReaffirmPolicy::default(),
        );
        // "a" already carries a verdict from an earlier pass.
        if let Some(load) = ctx.nodes.get_mut("LoadClaimsNode") {
            load["claims"][0]["verdict"] = json!({
                "action": "archive",
                "evidence": [],
                "reasoning": "prior pass",
                "transport": null,
            });
        }
        put_result(
            &mut ctx,
            JUDGE_NODE_NAME,
            json!({
                "action": "supersede",
                "evidence": [{"doc_id": "x", "file_path": null, "snippet": "s"}],
                "reasoning": "newer doc exists",
                "transport": null,
                "skipped": false,
            }),
        );
        let node = SaveVerdictNode::new();

        let ctx = node.process(ctx).await.expect("process succeeds");
        let state: ClaimReaffirmState =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid state");
        assert_eq!(state.claims.len(), 2, "no claim is dropped");
        let a = state.claims.iter().find(|c| c.id == "a").unwrap();
        assert_eq!(
            a.verdict.as_ref().unwrap().action,
            VerdictAction::Archive,
            "claim a's prior verdict survives untouched"
        );
        let b = state.claims.iter().find(|c| c.id == "b").unwrap();
        assert_eq!(b.status, ClaimStatus::Judged);
        assert_eq!(b.verdict.as_ref().unwrap().action, VerdictAction::Supersede);
    }

    #[tokio::test]
    async fn recall_failure_bumps_attempt_and_stays_pending_below_max() {
        let policy = ClaimReaffirmPolicy {
            max_attempts: 3,
            ..ClaimReaffirmPolicy::default()
        };
        let mut ctx =
            ctx_with_state_and_dispatch(vec![claim("a", ClaimStatus::Pending, 0)], "a", policy);
        put_result(&mut ctx, JUDGE_NODE_NAME, json!({ "skipped": true }));
        let node = SaveVerdictNode::new();

        let ctx = node.process(ctx).await.expect("process succeeds");
        let state: ClaimReaffirmState =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid state");
        let claim = &state.claims[0];
        assert_eq!(claim.status, ClaimStatus::Pending, "retried, not given up");
        assert_eq!(claim.attempt, 1);
    }

    #[tokio::test]
    async fn recall_failure_marks_failed_once_max_attempts_reached() {
        let policy = ClaimReaffirmPolicy {
            max_attempts: 2,
            ..ClaimReaffirmPolicy::default()
        };
        let mut ctx =
            ctx_with_state_and_dispatch(vec![claim("a", ClaimStatus::Pending, 1)], "a", policy);
        put_result(&mut ctx, JUDGE_NODE_NAME, json!({ "skipped": true }));
        let node = SaveVerdictNode::new();

        let ctx = node.process(ctx).await.expect("process succeeds");
        let state: ClaimReaffirmState =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid state");
        let claim = &state.claims[0];
        assert_eq!(
            claim.status,
            ClaimStatus::Failed,
            "the drain gives up on this claim rather than looping forever"
        );
        assert_eq!(claim.attempt, 2);
    }

    #[tokio::test]
    async fn errors_when_dispatched_claim_id_not_found_in_state() {
        let ctx = ctx_with_state_and_dispatch(
            vec![claim("a", ClaimStatus::Pending, 0)],
            "nonexistent",
            ClaimReaffirmPolicy::default(),
        );
        let node = SaveVerdictNode::new();

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("not found in ClaimReaffirmState"));
    }

    #[test]
    fn citation_fields_survive_a_round_trip_via_save_verdict() {
        // Guards that Verdict's serde shape (used to parse JudgeClaimNode's
        // stamp above) round-trips a Citation with a null file_path too.
        let citation = Citation {
            doc_id: "x".to_string(),
            file_path: None,
            snippet: "s".to_string(),
        };
        let json = serde_json::to_value(&citation).unwrap();
        let round_tripped: Citation = serde_json::from_value(json).unwrap();
        assert_eq!(round_tripped, citation);
    }
}
