//! Per-node cost/token accounting + a pre-dispatch budget gate (EN.2.B task 2).
//!
//! `Workflow::run` (EN.2.B task 3) consults [`BudgetLedger::check`] before
//! dispatching each node and halts the walk when the accumulated spend has
//! already reached a configured [`Budget`] cap, folding each completed
//! node's `NodeRun.usage` into the ledger afterward. Absent [`Budget`]
//! config, [`BudgetLedger::check`] always allows and the run loop's
//! behavior is unchanged from before this block.
//!
//! `engine_contract::Usage` (contract §6) carries `input_tokens` /
//! `output_tokens` / `model` but no cost figure, so token spend is folded in
//! from `NodeRun.usage` directly while cost spend is folded in separately
//! via [`BudgetLedger::record`]'s `cost_usd` parameter — callers that have a
//! cost figure (e.g. `ClaudeCodeStep`'s SDK `Outcome::cost_usd`) pass it
//! alongside the usage.

use engine_contract::{TaskContext, Usage};

use crate::workflow::node_cost_usd;

/// Optional per-run spend caps. Any field left `None` is not enforced.
/// `Budget::default()` (all `None`) means "no gate" — the ledger's
/// [`BudgetLedger::check`] always allows.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Budget {
    /// Halt once accumulated `input_tokens + output_tokens` across all
    /// completed nodes reaches this cap.
    pub max_total_tokens: Option<u64>,
    /// Halt once accumulated cost (USD) across all completed nodes reaches
    /// this cap.
    pub max_cost_usd: Option<f64>,
}

/// Which cap tripped a [`BudgetDecision::Halt`], and the spend/limit that
/// tripped it — enough detail for the run loop to write a self-explanatory
/// `TaskContext::metadata` entry (task 3) without re-deriving it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BudgetHaltReason {
    TotalTokens { spent: u64, limit: u64 },
    CostUsd { spent: f64, limit: f64 },
}

impl BudgetHaltReason {
    /// Which cap this reason names, as the contract-friendly lowercase
    /// snake_case string the run loop stamps into metadata.
    pub fn cap_name(&self) -> &'static str {
        match self {
            BudgetHaltReason::TotalTokens { .. } => "max_total_tokens",
            BudgetHaltReason::CostUsd { .. } => "max_cost_usd",
        }
    }

    /// A structured JSON rendering of this reason — `{ "cap": ..., "spent":
    /// ..., "limit": ... }`. Ergonomics for task 3, which writes this (or an
    /// equivalent shape) into `TaskContext::metadata`; this module never
    /// mutates a `TaskContext` itself.
    pub fn to_json(self) -> serde_json::Value {
        match self {
            BudgetHaltReason::TotalTokens { spent, limit } => serde_json::json!({
                "cap": self.cap_name(),
                "spent": spent,
                "limit": limit,
            }),
            BudgetHaltReason::CostUsd { spent, limit } => serde_json::json!({
                "cap": self.cap_name(),
                "spent": spent,
                "limit": limit,
            }),
        }
    }
}

/// The result of a pre-dispatch [`BudgetLedger::check`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BudgetDecision {
    /// No configured cap has been reached — dispatch the node.
    Allow,
    /// A configured cap has already been reached — stop the walk before
    /// dispatching the next node.
    Halt(BudgetHaltReason),
}

/// Accumulates spend across a run's completed nodes and answers the
/// pre-dispatch [`BudgetLedger::check`] gate.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BudgetLedger {
    total_tokens: u64,
    total_cost_usd: f64,
}

impl BudgetLedger {
    /// A fresh ledger with zero accumulated spend.
    pub fn new() -> Self {
        Self::default()
    }

    /// Exact restore from a `metadata.suspension` ledger snapshot — the
    /// authoritative resume path. Always prefer this over [`from_context`]
    /// when a suspension marker actually carries a ledger snapshot.
    ///
    /// [`from_context`]: BudgetLedger::from_context
    pub fn from_parts(total_tokens: u64, total_cost_usd: f64) -> Self {
        Self {
            total_tokens,
            total_cost_usd,
        }
    }

    /// Lossy fallback for a context whose suspension marker carries no
    /// ledger snapshot (a marker written before EN.6.F, or a DB row from an
    /// older process). Sums `node_runs[*].usage` (tokens) and
    /// `nodes[*].cost_usd` (dollars, via the same [`node_cost_usd`] reader
    /// `Workflow::run`'s own fold uses) across every node identity in the
    /// context.
    ///
    /// **LOSSY** — `node_runs` is keyed by node identity, not by
    /// invocation, so a node that ran `N` times through a loop back-edge
    /// contributes only its *last* recorded usage/cost, not the sum across
    /// all `N` runs. This exists so a resume never silently restarts spend
    /// at zero, not because it reconstructs the true historical total.
    pub fn from_context(ctx: &TaskContext) -> Self {
        let total_tokens = ctx
            .node_runs
            .values()
            .filter_map(|run| run.usage.as_ref())
            .map(|usage| usage.input_tokens.unwrap_or(0) + usage.output_tokens.unwrap_or(0))
            .sum();

        let total_cost_usd = ctx
            .nodes
            .keys()
            .filter_map(|identity| node_cost_usd(ctx, identity))
            .sum();

        Self {
            total_tokens,
            total_cost_usd,
        }
    }

    /// Folds a completed node's spend into the ledger.
    ///
    /// `usage: None` (non-LLM nodes, or an LLM node that reported none)
    /// contributes no tokens. `cost_usd: None` contributes no cost — pass
    /// `Some` only when the caller has an actual cost figure in hand.
    pub fn record(&mut self, usage: Option<&Usage>, cost_usd: Option<f64>) {
        if let Some(usage) = usage {
            self.total_tokens += usage.input_tokens.unwrap_or(0) + usage.output_tokens.unwrap_or(0);
        }
        if let Some(cost) = cost_usd {
            self.total_cost_usd += cost;
        }
    }

    /// Accumulated `input_tokens + output_tokens` across every [`record`]
    /// call so far.
    ///
    /// [`record`]: BudgetLedger::record
    pub fn total_tokens(&self) -> u64 {
        self.total_tokens
    }

    /// Accumulated cost (USD) across every [`record`] call so far.
    pub fn total_cost_usd(&self) -> f64 {
        self.total_cost_usd
    }

    /// The pre-dispatch gate: `budget: None` always allows (no config = no
    /// gate). Otherwise halts once any configured cap has already been
    /// *reached* (spend `>=` limit) by the spend accumulated so far — this
    /// is checked BEFORE dispatching the next node, so a cap reached by the
    /// last completed node stops the walk before the node that would have
    /// exceeded it runs.
    pub fn check(&self, budget: Option<&Budget>) -> BudgetDecision {
        evaluate_budget(budget, self.total_tokens, self.total_cost_usd)
    }
}

/// Shared cap-evaluation arithmetic for both [`BudgetLedger::check`] (per
/// node, within one run) and [`CampaignLedger::check`] (per step, across a
/// chain's block boundaries). Both ledgers accumulate the same two figures
/// (`total_tokens`, `total_cost_usd`) against the same [`Budget`] shape, so
/// the halt-or-allow decision is factored out here rather than forked
/// between the two — only the accumulation policy differs between the two
/// ledger types, not the cap check itself.
fn evaluate_budget(
    budget: Option<&Budget>,
    total_tokens: u64,
    total_cost_usd: f64,
) -> BudgetDecision {
    let Some(budget) = budget else {
        return BudgetDecision::Allow;
    };

    if let Some(limit) = budget.max_total_tokens {
        if total_tokens >= limit {
            return BudgetDecision::Halt(BudgetHaltReason::TotalTokens {
                spent: total_tokens,
                limit,
            });
        }
    }

    if let Some(limit) = budget.max_cost_usd {
        if total_cost_usd >= limit {
            return BudgetDecision::Halt(BudgetHaltReason::CostUsd {
                spent: total_cost_usd,
                limit,
            });
        }
    }

    BudgetDecision::Allow
}

/// Accumulates spend across a **campaign's chain STEPS** (each step being
/// one flow-engine run — see `ExecutionOutcome::cost_usd`/`.total_tokens`
/// in `workflows/orchestration/execute.rs`), checked at each block
/// boundary by `workflows/orchestration/integrate.rs` (`EN.11.F` task 4).
///
/// Distinct from [`BudgetLedger`], which accumulates per NODE dispatched
/// within a single run and is checked before every node dispatch. This
/// ledger accumulates once per completed STEP (i.e. once per block in the
/// chain) and is checked once per block boundary — a coarser cadence that
/// answers "should the next block in this campaign even start?" rather
/// than "should the next node in this run even start?". It reuses
/// [`Budget`], [`BudgetDecision`] and [`BudgetHaltReason`] rather than
/// forking parallel campaign-scoped types, because the cap shape and the
/// halt vocabulary are identical — only the thing being accumulated
/// (steps vs. nodes) differs.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CampaignLedger {
    total_tokens: u64,
    total_cost_usd: f64,
    /// Set once any recorded step's `cost_usd` was `None` — i.e. the
    /// step's child run reported no cost figure for ANY node at all (see
    /// `ExecutionOutcome::cost_usd`'s doc: `None` means "nothing to
    /// distinguish from zero", not "confirmed zero"). See
    /// [`CampaignLedger::record_step`] for the documented handling this
    /// flag exists to make visible.
    has_unknown_cost_step: bool,
}

impl CampaignLedger {
    /// A fresh ledger with zero accumulated spend and no unknown-cost
    /// steps recorded yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Folds one completed STEP's spend into the campaign total.
    ///
    /// DOCUMENTED RULE for `cost_usd: None` (the "unknown cost" case,
    /// e.g. a step whose child run reported no cost figure for any node):
    /// the step contributes its `total_tokens` to the token accumulation
    /// as normal, but contributes **nothing** to `total_cost_usd` — an
    /// unknown cost is never counted toward the cost cap, because
    /// treating it as `$0` would let a campaign with real, unreported
    /// spend sail past a cost ceiling that should have caught it (the
    /// inverse failure to `smoke-run.md` §3.6's `total_cost_usd: -0.0`
    /// bug, which came from collapsing this same `Option` the other
    /// direction). This is NOT the same as "this step cost exactly $0" —
    /// [`CampaignLedger::has_unknown_cost_step`] is the flag that keeps
    /// those two cases distinguishable to a caller inspecting the ledger,
    /// even though neither one moves `total_cost_usd`.
    pub fn record_step(&mut self, cost_usd: Option<f64>, total_tokens: u64) {
        self.total_tokens += total_tokens;
        match cost_usd {
            Some(cost) => self.total_cost_usd += cost,
            None => self.has_unknown_cost_step = true,
        }
    }

    /// Accumulated `total_tokens` across every recorded step so far.
    pub fn total_tokens(&self) -> u64 {
        self.total_tokens
    }

    /// Accumulated cost (USD) across every step that reported one.
    /// Steps recorded with `cost_usd: None` do not contribute — see
    /// [`record_step`](CampaignLedger::record_step)'s doc.
    pub fn total_cost_usd(&self) -> f64 {
        self.total_cost_usd
    }

    /// `true` once at least one recorded step reported no cost figure at
    /// all (`cost_usd: None`). A caller that cares whether
    /// `total_cost_usd` is a complete picture of campaign spend — as
    /// opposed to a lower bound that omits any step whose cost went
    /// unreported — should check this alongside the cap decision.
    pub fn has_unknown_cost_step(&self) -> bool {
        self.has_unknown_cost_step
    }

    /// The block-boundary gate: `budget: None` always allows. Otherwise
    /// halts once any configured cap has already been *reached* (spend
    /// `>=` limit) by the campaign spend accumulated so far — checked
    /// BEFORE the next block starts, so a cap reached by the last
    /// completed block stops the chain before the block that would have
    /// exceeded it starts.
    pub fn check(&self, budget: Option<&Budget>) -> BudgetDecision {
        evaluate_budget(budget, self.total_tokens, self.total_cost_usd)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn usage(input: u64, output: u64) -> Usage {
        Usage {
            input_tokens: Some(input),
            output_tokens: Some(output),
            model: "claude-sonnet-4-5".to_string(),
        }
    }

    #[test]
    fn ledger_accumulates_across_several_usage_entries() {
        let mut ledger = BudgetLedger::new();
        ledger.record(Some(&usage(10, 20)), Some(0.01));
        ledger.record(Some(&usage(5, 15)), Some(0.02));

        assert_eq!(ledger.total_tokens(), 50);
        assert!((ledger.total_cost_usd() - 0.03).abs() < f64::EPSILON);
    }

    #[test]
    fn usage_none_contributes_nothing() {
        let mut ledger = BudgetLedger::new();
        ledger.record(Some(&usage(10, 20)), Some(0.01));
        ledger.record(None, None);

        assert_eq!(ledger.total_tokens(), 30);
        assert!((ledger.total_cost_usd() - 0.01).abs() < f64::EPSILON);
    }

    #[test]
    fn under_cap_allows() {
        let mut ledger = BudgetLedger::new();
        ledger.record(Some(&usage(10, 10)), Some(0.01));

        let budget = Budget {
            max_total_tokens: Some(1000),
            max_cost_usd: Some(1.0),
        };

        assert_eq!(ledger.check(Some(&budget)), BudgetDecision::Allow);
    }

    #[test]
    fn at_cap_halts_naming_total_tokens() {
        let mut ledger = BudgetLedger::new();
        ledger.record(Some(&usage(50, 50)), None);

        let budget = Budget {
            max_total_tokens: Some(100),
            max_cost_usd: None,
        };

        let decision = ledger.check(Some(&budget));
        match decision {
            BudgetDecision::Halt(reason) => {
                assert_eq!(reason.cap_name(), "max_total_tokens");
                assert_eq!(
                    reason,
                    BudgetHaltReason::TotalTokens {
                        spent: 100,
                        limit: 100,
                    }
                );
            }
            BudgetDecision::Allow => panic!("expected a halt at cap"),
        }
    }

    #[test]
    fn over_cap_halts_naming_cost_usd() {
        let mut ledger = BudgetLedger::new();
        ledger.record(None, Some(5.5));

        let budget = Budget {
            max_total_tokens: None,
            max_cost_usd: Some(5.0),
        };

        let decision = ledger.check(Some(&budget));
        match decision {
            BudgetDecision::Halt(reason) => {
                assert_eq!(reason.cap_name(), "max_cost_usd");
                assert_eq!(
                    reason,
                    BudgetHaltReason::CostUsd {
                        spent: 5.5,
                        limit: 5.0,
                    }
                );
            }
            BudgetDecision::Allow => panic!("expected a halt over cap"),
        }
    }

    #[test]
    fn no_config_always_allows() {
        let mut ledger = BudgetLedger::new();
        ledger.record(Some(&usage(u64::MAX / 2, u64::MAX / 2)), Some(1_000_000.0));

        assert_eq!(ledger.check(None), BudgetDecision::Allow);
    }

    fn node_run_with_usage(input: u64, output: u64) -> engine_contract::NodeRun {
        engine_contract::NodeRun {
            status: engine_contract::NodeRunStatus::Success,
            started_at: None,
            completed_at: None,
            error: None,
            input: None,
            usage: Some(usage(input, output)),
        }
    }

    #[test]
    fn from_parts_round_trips_through_check() {
        let ledger = BudgetLedger::from_parts(100, 5.0);

        assert_eq!(ledger.total_tokens(), 100);
        assert!((ledger.total_cost_usd() - 5.0).abs() < f64::EPSILON);

        let cap_above = Budget {
            max_total_tokens: Some(200),
            max_cost_usd: Some(10.0),
        };
        assert_eq!(ledger.check(Some(&cap_above)), BudgetDecision::Allow);

        let cap_below = Budget {
            max_total_tokens: Some(50),
            max_cost_usd: None,
        };
        assert!(matches!(
            ledger.check(Some(&cap_below)),
            BudgetDecision::Halt(BudgetHaltReason::TotalTokens { .. })
        ));
    }

    #[test]
    fn from_context_sums_node_runs_usage_and_nodes_cost_usd() {
        let mut node_runs = HashMap::new();
        node_runs.insert("NodeA".to_string(), node_run_with_usage(10, 20));
        node_runs.insert("NodeB".to_string(), node_run_with_usage(5, 15));

        let mut nodes = HashMap::new();
        nodes.insert("NodeA".to_string(), serde_json::json!({"cost_usd": 0.5}));
        nodes.insert("NodeB".to_string(), serde_json::json!({"cost_usd": 0.25}));

        let ctx = TaskContext {
            event: serde_json::json!({}),
            nodes,
            metadata: serde_json::json!({}),
            node_runs,
        };

        let ledger = BudgetLedger::from_context(&ctx);

        assert_eq!(ledger.total_tokens(), 50);
        assert!((ledger.total_cost_usd() - 0.75).abs() < f64::EPSILON);
    }

    #[test]
    fn from_context_on_empty_context_equals_new() {
        let ctx = TaskContext {
            event: serde_json::json!({}),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        };

        assert_eq!(BudgetLedger::from_context(&ctx), BudgetLedger::new());
    }

    #[test]
    fn from_context_looped_node_contributes_once() {
        // A node that ran twice through a loop back-edge is keyed once in
        // `node_runs` by identity, so only its last recorded usage/cost is
        // visible to `from_context` — documenting the known loss.
        let mut node_runs = HashMap::new();
        node_runs.insert("LoopedNode".to_string(), node_run_with_usage(100, 100));

        let mut nodes = HashMap::new();
        nodes.insert(
            "LoopedNode".to_string(),
            serde_json::json!({"cost_usd": 2.0}),
        );

        let ctx = TaskContext {
            event: serde_json::json!({}),
            nodes,
            metadata: serde_json::json!({}),
            node_runs,
        };

        let ledger = BudgetLedger::from_context(&ctx);

        // Only the single stored (last) usage/cost is visible, not a sum
        // across however many times the loop actually ran.
        assert_eq!(ledger.total_tokens(), 200);
        assert!((ledger.total_cost_usd() - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn campaign_ledger_accumulates_across_several_steps() {
        let mut ledger = CampaignLedger::new();
        ledger.record_step(Some(1.0), 100);
        ledger.record_step(Some(2.5), 200);

        assert_eq!(ledger.total_tokens(), 300);
        assert!((ledger.total_cost_usd() - 3.5).abs() < f64::EPSILON);
        assert!(!ledger.has_unknown_cost_step());
    }

    #[test]
    fn campaign_ledger_trips_on_a_cap_below_one_steps_cost() {
        let mut ledger = CampaignLedger::new();
        // A single step whose cost already exceeds a ceiling set below it
        // must halt at that first boundary, not after the whole chain.
        ledger.record_step(Some(10.0), 500);

        let budget = Budget {
            max_total_tokens: None,
            max_cost_usd: Some(5.0),
        };

        let decision = ledger.check(Some(&budget));
        match decision {
            BudgetDecision::Halt(reason) => {
                assert_eq!(reason.cap_name(), "max_cost_usd");
                assert_eq!(
                    reason,
                    BudgetHaltReason::CostUsd {
                        spent: 10.0,
                        limit: 5.0,
                    }
                );
            }
            BudgetDecision::Allow => panic!("expected a halt below one step's cost"),
        }
    }

    #[test]
    fn campaign_ledger_check_runs_at_each_boundary_independently() {
        // Simulates a two-block chain: the check is consulted at both
        // boundaries, not once for the whole campaign.
        let mut ledger = CampaignLedger::new();
        let budget = Budget {
            max_total_tokens: Some(150),
            max_cost_usd: None,
        };

        ledger.record_step(Some(0.1), 100);
        let first_boundary = ledger.check(Some(&budget));
        assert_eq!(first_boundary, BudgetDecision::Allow);

        ledger.record_step(Some(0.1), 100);
        let second_boundary = ledger.check(Some(&budget));
        assert!(matches!(
            second_boundary,
            BudgetDecision::Halt(BudgetHaltReason::TotalTokens {
                spent: 200,
                limit: 150
            })
        ));
    }

    #[test]
    fn campaign_ledger_unknown_cost_step_does_not_count_as_zero_spend() {
        let mut ledger = CampaignLedger::new();
        // A step reporting a real, known $0 cost.
        ledger.record_step(Some(0.0), 10);
        assert!((ledger.total_cost_usd() - 0.0).abs() < f64::EPSILON);
        assert!(!ledger.has_unknown_cost_step());

        // A step reporting NO cost figure at all — must be distinguishable
        // from the confirmed-$0 case above via `has_unknown_cost_step`,
        // and must not silently move `total_cost_usd`.
        ledger.record_step(None, 20);
        assert!((ledger.total_cost_usd() - 0.0).abs() < f64::EPSILON);
        assert!(ledger.has_unknown_cost_step());
        assert_eq!(ledger.total_tokens(), 30);
    }

    #[test]
    fn campaign_ledger_reuses_budget_decision_and_halt_reason_types() {
        // Compile-time reuse check: `CampaignLedger::check` returns the
        // same `BudgetDecision`/`BudgetHaltReason` types `BudgetLedger`
        // does, not a forked campaign-scoped equivalent.
        let ledger = CampaignLedger::new();
        let decision: BudgetDecision = ledger.check(None);
        assert_eq!(decision, BudgetDecision::Allow);
    }

    #[test]
    fn halt_reason_to_json_names_the_cap() {
        let reason = BudgetHaltReason::TotalTokens {
            spent: 100,
            limit: 100,
        };

        let json = reason.to_json();
        assert_eq!(json["cap"], serde_json::json!("max_total_tokens"));
        assert_eq!(json["spent"], serde_json::json!(100));
        assert_eq!(json["limit"], serde_json::json!(100));
    }
}
