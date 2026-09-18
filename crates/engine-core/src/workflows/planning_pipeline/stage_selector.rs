//! `StageSelectorNode` — validates `PLANNING_PIPELINE`'s `stages` list
//! (`EN.19.D` task 3).
//!
//! No model call. `PLANNING_PIPELINE` composes four fixed stages in a fixed
//! order — `pre_plan -> plan -> generate_tasks -> dispatch` — and a
//! dispatched event may request any non-empty, CONTIGUOUS, in-order subset
//! of them (e.g. `[pre_plan]`, `[pre_plan, plan]`,
//! `[plan, generate_tasks, dispatch]`), never a gap (`[pre_plan, dispatch]`
//! skips `plan`/`generate_tasks`), never out-of-order
//! (`[dispatch, pre_plan]`), never empty, and never an unknown stage name.
//! This node is that validation gate, run before task 7's graph wires
//! anything: a rejection here means the run fails before any stage node —
//! and therefore before any `ApprovalGateNode` suspend/dispatch side effect
//! — ever executes, satisfying the block record's "dispatches nothing" on
//! every rejection shape.
//!
//! Unlike `pre_plan::check_existing::CheckExistingNotesNode` (which routes
//! between two *continuation* paths via `Router`), an invalid `stages` list
//! has no continuation path at all — there is nothing sensible to wire it
//! to. So this node reports its rejection as an `Err(NodeError)` carrying a
//! NAMED reason (never a bare "invalid input" guess), which halts the walk
//! before task 7's graph reaches any stage node. On success, the validated,
//! already-ordered stage list is stamped into `ctx.nodes[NODE_NAME]` for
//! task 7's graph wiring to read back.

use engine_contract::TaskContext;

use crate::node::{Node, NodeError};
use crate::workflows::put_result;

/// The `Node::name()` identity this node registers under, and the
/// `ctx.nodes` key its verdict is stamped onto.
pub const NODE_NAME: &str = "StageSelectorNode";

/// `ctx.nodes[NODE_NAME]` keys.
const STAGES_KEY: &str = "stages";
const REASON_KEY: &str = "reason";

/// `PLANNING_PIPELINE`'s four stages, in their one fixed canonical order.
/// Every accepted `stages` list is a contiguous run of this slice — task 7's
/// graph wiring walks stages in this same order.
pub const CANONICAL_STAGE_ORDER: [&str; 4] = ["pre_plan", "plan", "generate_tasks", "dispatch"];

/// Named rejection reasons — matched by `route()`-less callers (this node's
/// own tests, and `tests/it/planning_pipeline.rs` in task 9) that assert on
/// WHICH validation failed, not just that something did.
pub mod reject_reason {
    pub const EMPTY: &str = "stages list is empty";
    pub const UNKNOWN_STAGE: &str = "unknown stage name";
    pub const GAP: &str = "stages list has a gap";
    pub const OUT_OF_ORDER: &str = "stages list is out of order";
    pub const DUPLICATE: &str = "stages list has a duplicate stage";
}

/// No-model validator: rejects an empty, gapped, out-of-order, duplicated,
/// or unknown-stage `stages` list with a named reason; on success, emits the
/// validated, ordered stage set into `ctx` for task 7's graph wiring.
pub struct StageSelectorNode;

impl StageSelectorNode {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for StageSelectorNode {
    fn default() -> Self {
        Self::new()
    }
}

/// Read `stages` from the dispatched event as a list of trimmed strings,
/// failing loudly (naming the field) when it is missing or not an array —
/// the same validation shape `pre_plan::check_existing::require_slug` uses
/// for `slug`. An empty array is NOT rejected here — that is
/// [`reject_reason::EMPTY`]'s job, reported with its own named reason
/// rather than folded into this generic shape error.
fn require_stages(ctx: &TaskContext) -> Result<Vec<String>, NodeError> {
    let raw = ctx
        .event
        .get("stages")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| NodeError::new("stage_selector: missing or non-array 'stages'"))?;

    raw.iter()
        .map(|value| {
            value
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .ok_or_else(|| {
                    NodeError::new("stage_selector: 'stages' entries must be non-empty strings")
                })
        })
        .collect()
}

/// Validate `stages` against [`CANONICAL_STAGE_ORDER`], returning the
/// canonical-order-indexed, contiguous, in-order stage list on success or a
/// `(reason_code, message)` pair on the first violation found — checked in
/// this order: empty, unknown name, duplicate, out-of-order, gap. Free
/// function (rather than a method) so task 9's integration tests can call it
/// directly without constructing a `TaskContext`.
pub fn validate_stages(stages: &[String]) -> Result<Vec<String>, (&'static str, String)> {
    if stages.is_empty() {
        return Err((
            reject_reason::EMPTY,
            "stage_selector: 'stages' must be a non-empty list".to_string(),
        ));
    }

    // Resolve each requested stage to its index in the canonical order,
    // failing on the first name not in {pre_plan, plan, generate_tasks,
    // dispatch}.
    let mut indices = Vec::with_capacity(stages.len());
    for stage in stages {
        match CANONICAL_STAGE_ORDER.iter().position(|s| s == stage) {
            Some(idx) => indices.push(idx),
            None => {
                return Err((
                    reject_reason::UNKNOWN_STAGE,
                    format!(
                        "stage_selector: unknown stage '{stage}' — must be one of {}",
                        CANONICAL_STAGE_ORDER.join(", ")
                    ),
                ))
            }
        }
    }

    // Duplicates: a canonical index repeated anywhere in the request.
    let mut seen = indices.clone();
    seen.sort_unstable();
    seen.dedup();
    if seen.len() != indices.len() {
        return Err((
            reject_reason::DUPLICATE,
            format!(
                "stage_selector: 'stages' repeats a stage — got [{}]",
                stages.join(", ")
            ),
        ));
    }

    // Out-of-order: the requested indices must already be ascending — a
    // list like [dispatch, pre_plan] resolves to [3, 0], which is not
    // sorted.
    let mut sorted = indices.clone();
    sorted.sort_unstable();
    if indices != sorted {
        return Err((
            reject_reason::OUT_OF_ORDER,
            format!(
                "stage_selector: 'stages' is out of order — got [{}], expected canonical order {}",
                stages.join(", "),
                CANONICAL_STAGE_ORDER.join(", ")
            ),
        ));
    }

    // Gap: once sorted (== the original, per the out-of-order check above),
    // consecutive indices must differ by exactly 1 — a jump means a stage in
    // between was skipped.
    for window in indices.windows(2) {
        if window[1] - window[0] != 1 {
            let skipped: Vec<&str> = CANONICAL_STAGE_ORDER[window[0] + 1..window[1]].to_vec();
            return Err((
                reject_reason::GAP,
                format!(
                    "stage_selector: 'stages' has a gap between '{}' and '{}' — missing {}",
                    CANONICAL_STAGE_ORDER[window[0]],
                    CANONICAL_STAGE_ORDER[window[1]],
                    skipped.join(", ")
                ),
            ));
        }
    }

    Ok(stages.to_vec())
}

#[async_trait::async_trait]
impl Node for StageSelectorNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let stages = require_stages(&ctx)?;

        match validate_stages(&stages) {
            Ok(ordered) => {
                put_result(
                    &mut ctx,
                    NODE_NAME,
                    serde_json::json!({
                        STAGES_KEY: ordered,
                    }),
                );
                Ok(ctx)
            }
            Err((reason_code, message)) => Err(NodeError::new(message.clone()).with_node_result(
                serde_json::json!({
                    REASON_KEY: reason_code,
                    "message": message,
                    "requested": stages,
                }),
            )),
        }
    }

    fn name(&self) -> &str {
        NODE_NAME
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;

    fn empty_context(event: serde_json::Value) -> TaskContext {
        TaskContext {
            event,
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        }
    }

    fn stages(list: &[&str]) -> serde_json::Value {
        json!({"stages": list})
    }

    #[tokio::test]
    async fn accepts_single_stage() {
        let node = StageSelectorNode::new();
        let ctx = node
            .process(empty_context(stages(&["pre_plan"])))
            .await
            .expect("single stage accepted");
        let stored = ctx.nodes.get(NODE_NAME).expect("result stored");
        assert_eq!(stored.get(STAGES_KEY), Some(&json!(["pre_plan"])));
    }

    #[tokio::test]
    async fn accepts_full_contiguous_slice() {
        let node = StageSelectorNode::new();
        let ctx = node
            .process(empty_context(stages(&[
                "pre_plan",
                "plan",
                "generate_tasks",
                "dispatch",
            ])))
            .await
            .expect("full slice accepted");
        let stored = ctx.nodes.get(NODE_NAME).expect("result stored");
        assert_eq!(
            stored.get(STAGES_KEY),
            Some(&json!(["pre_plan", "plan", "generate_tasks", "dispatch"]))
        );
    }

    #[tokio::test]
    async fn accepts_middle_contiguous_slice() {
        let node = StageSelectorNode::new();
        let ctx = node
            .process(empty_context(stages(&[
                "plan",
                "generate_tasks",
                "dispatch",
            ])))
            .await
            .expect("middle-to-end slice accepted");
        let stored = ctx.nodes.get(NODE_NAME).expect("result stored");
        assert_eq!(
            stored.get(STAGES_KEY),
            Some(&json!(["plan", "generate_tasks", "dispatch"]))
        );
    }

    #[tokio::test]
    async fn rejects_empty_list() {
        let node = StageSelectorNode::new();
        let err = node
            .process(empty_context(stages(&[])))
            .await
            .expect_err("empty list rejected");
        let stamped = err.node_result.expect("reason stamped on error");
        assert_eq!(
            stamped.get(REASON_KEY).and_then(|v| v.as_str()),
            Some(reject_reason::EMPTY)
        );
    }

    #[tokio::test]
    async fn rejects_unknown_stage() {
        let node = StageSelectorNode::new();
        let err = node
            .process(empty_context(stages(&["pre_plan", "not_a_stage"])))
            .await
            .expect_err("unknown stage rejected");
        let stamped = err.node_result.expect("reason stamped on error");
        assert_eq!(
            stamped.get(REASON_KEY).and_then(|v| v.as_str()),
            Some(reject_reason::UNKNOWN_STAGE)
        );
    }

    #[tokio::test]
    async fn rejects_gap() {
        let node = StageSelectorNode::new();
        let err = node
            .process(empty_context(stages(&["pre_plan", "dispatch"])))
            .await
            .expect_err("gap rejected");
        let stamped = err.node_result.expect("reason stamped on error");
        assert_eq!(
            stamped.get(REASON_KEY).and_then(|v| v.as_str()),
            Some(reject_reason::GAP)
        );
    }

    #[tokio::test]
    async fn rejects_out_of_order() {
        let node = StageSelectorNode::new();
        let err = node
            .process(empty_context(stages(&["dispatch", "pre_plan"])))
            .await
            .expect_err("out-of-order rejected");
        let stamped = err.node_result.expect("reason stamped on error");
        assert_eq!(
            stamped.get(REASON_KEY).and_then(|v| v.as_str()),
            Some(reject_reason::OUT_OF_ORDER)
        );
    }

    #[tokio::test]
    async fn rejects_duplicate_stage() {
        let node = StageSelectorNode::new();
        let err = node
            .process(empty_context(stages(&["pre_plan", "pre_plan"])))
            .await
            .expect_err("duplicate rejected");
        let stamped = err.node_result.expect("reason stamped on error");
        assert_eq!(
            stamped.get(REASON_KEY).and_then(|v| v.as_str()),
            Some(reject_reason::DUPLICATE)
        );
    }

    #[tokio::test]
    async fn rejects_missing_stages_field() {
        let node = StageSelectorNode::new();
        let err = node
            .process(empty_context(json!({})))
            .await
            .expect_err("missing stages rejected");
        assert!(err.message.contains("stages"));
    }

    #[test]
    fn name_matches_node_name_const() {
        assert_eq!(StageSelectorNode::new().name(), NODE_NAME);
    }

    #[test]
    fn validate_stages_direct_call_accepts_and_rejects() {
        assert!(validate_stages(&["pre_plan".to_string()]).is_ok());
        assert_eq!(validate_stages(&[]).unwrap_err().0, reject_reason::EMPTY);
        assert_eq!(
            validate_stages(&["pre_plan".to_string(), "dispatch".to_string()])
                .unwrap_err()
                .0,
            reject_reason::GAP
        );
    }
}
