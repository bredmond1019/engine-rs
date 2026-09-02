//! The SDLC Flow (`SDLC_FLOW`) workflow — a Rust port of the Python
//! `orchestrator/app/workflows/sdlc_flow_workflow.py` pipeline's top half:
//! setup → generate/load tasks → the implement/test/triage/review task loop
//! with its runtime retry back-edges.
//!
//! Module layout (each leaf file owned by exactly one task in
//! `planning/EN.3.A-sdlc-flow-setup-task-loop/tasks.json` or
//! `planning/EN.3.B-sdlc-flow-docs-wrapup-pr/tasks.json`):
//! - `schema` — the ported `SDLCState`/`SDLCTask`/`SDLCFlowEventSchema` types.
//! - `setup` — `SetupWorktreeNode` / `SpecExistsRouterNode` /
//!   `GenerateTasksNode` / `LoadTaskStateNode`.
//! - `task_loop` — the implement→test→triage→review→update/save loop nodes
//!   and routers.
//! - `docs` — `PatchDocsNode` (bottom-half, EN.3.B).
//! - `wrap_up` — `WrapUpNode` (bottom-half, EN.3.B).
//! - `pr` — `PullRequestNode` (bottom-half, EN.3.B).
//! - `close_block` — `CloseBlockNode`: closes this run's block in
//!   `planning/state.json` through mev, under mev's advisory lock and D71
//!   operator gate (`EN.ticket.wrap-up-closes-the-block`).
//! - `emit_state` — `EmitStateNode` (bottom-half, EN.3.B).
//! - `final_validation` — `FinalValidationNode`: the unconditional run-level
//!   full-suite gate on the task-loop drain branch (`EN.3.E`).
//! - `graph` — assembles the declared `WorkflowSchema` + `NodeRegistry` for
//!   the whole workflow.
//! - `aggregate` — the cross-run `(policy -> cost, time, quality)`
//!   aggregator (EN.3.C task 7): reads a set of `sdlc-flow-state.json`
//!   snapshots and tabulates one row per distinct resolved policy.
//!
//! The node-plumbing seams shared by every submodule — `CommandOutput` /
//! `CommandRunner` / `default_command_runner` (hoisted in EN.3.B task 1 out
//! of `setup.rs`/`task_loop.rs`, which had byte-identical private copies) —
//! together with `ModelTransport`, the `put_result`/`get_result` context
//! helpers, `strip_json_fence`, and `parse_structured_or_fenced`, all now
//! live in `workflows::mod` (EN.4.0 task 4 hoisted the model-node seams;
//! `EN.11.M` task 2 hoisted the command-runner cluster), since none of them
//! are SDLC-specific; this module re-exports them so every existing
//! `super::`/`sdlc_flow::` import site keeps resolving unchanged.

// `strip_json_fence` has no direct callers left in this module now that
// `parse_structured_or_fenced` (also re-exported here) is the sole caller,
// but it stays re-exported for back-compat — any `super::strip_json_fence`
// import site elsewhere in the crate must keep resolving unchanged.
#[allow(unused_imports)]
pub(crate) use super::{get_result, parse_structured_or_fenced, put_result, strip_json_fence};
// `session_baseline`/`sessions_since` (EN.14.C task 1) — the ledger-delta
// helper pair a wrapper reads before its inner `ClaudeCodeStep` call and
// attaches to any `NodeError` it constructs after that call, so a billed
// session survives a post-billed-call wrapper failure.
#[allow(unused_imports)]
pub(crate) use super::{session_baseline, sessions_since};
pub use super::{ModelTransport, TransportSlot};
#[allow(unused_imports)]
pub use crate::policy::command_floor::{self, evaluate_command, CommandDecision};

pub mod aggregate;
pub mod close_block;
pub mod docs;
pub mod emit_state;
pub mod end_review;
pub mod final_validation;
pub mod graph;
pub mod policy;
pub mod pr;
pub mod profiles;
pub mod schema;
pub mod setup;
pub mod task_loop;
pub mod wrap_up;

#[allow(unused_imports)]
pub(crate) use super::{commit_all, is_noop_commit};
/// The command-runner cluster (`CommandOutput`/`CommandRunner`/
/// `default_command_runner`/`commit_all`/`is_noop_commit`) now lives in
/// `workflows::mod` (`EN.11.M` task 2) alongside `ModelTransport` and the
/// other shared model-node seams, so a second engine can reuse them without
/// depending on this module. Re-exported here so every existing
/// `super::`/`sdlc_flow::` import site keeps resolving unchanged.
pub use super::{default_command_runner, CommandOutput, CommandRunner};

/// The bare filename every SDLC_FLOW state writer/reader resolves through.
///
/// `EN.11.M` task 4 parameterizes the nine sites across `task_loop.rs` /
/// `wrap_up.rs` / `emit_state.rs` / `setup.rs` that previously hardcoded the
/// literal `"sdlc-flow-state.json"` behind this one const, so a forked
/// writer (the defect class this repo has already paid for once) cannot
/// recur. The three WRITER nodes (`SaveStateNode`, `WrapUpNode`,
/// `EmitStateNode`) expose a `with_state_filename(&'static str)` builder
/// that defaults to this value; `SpecExistsRouterNode` and
/// `LoadTaskStateNode` read the const directly and stay unit structs
/// (converting them to carry their own field is `EN.11.N`'s, per the scope
/// call recorded in this block's amendments).
pub const DEFAULT_STATE_FILENAME: &str = "sdlc-flow-state.json";

/// Carry billing/telemetry fields forward across a wrapper's `put_result`.
///
/// `put_result` (`workflows::mod::put_result`) is a blind
/// `ctx.nodes.insert` — it replaces the whole `ctx.nodes[identity]` entry
/// rather than merging into it. A `COST_BEARING_STAGES` wrapper node that
/// builds a fresh `result` object (its own verdict/content JSON) and then
/// calls `put_result` therefore silently drops everything the inner
/// `ClaudeCodeStep::process` already stamped onto that same identity —
/// `cost_usd`, both cache channels, and the `"transport"` tier stamp
/// (`crates/engine-core/src/nodes/claude_code_step.rs:485-510`). That loss
/// is why `BudgetLedger::node_cost_usd` folds in `None` for every SDLC
/// stage and `Budget::max_cost_usd` can never fire, and why
/// `policy::telemetry::total_cost_usd`'s fallback reports zero for
/// SDLC_FLOW runs that in fact cost money (`EN.14.A`).
///
/// This helper is called BEFORE a wrapper overwrites `result` into
/// `ctx.nodes[identity]`, reading the *prior* `ctx.nodes[identity]` (the
/// inner step's just-stamped entry) and copying exactly four keys —
/// `"transport"`, `"cost_usd"`, `"cache_creation_input_tokens"`,
/// `"cache_read_input_tokens"` — onto the wrapper's `result`, skipping any
/// key that is absent or JSON `null`. It copies nothing else: not
/// `content`, not `structured`, not `model`, not `session_id`.
///
/// Called at five wrapper sites: `ImplementTaskNode`, `TriageTaskNode` and
/// `ConsolidatedReviewNode` (`task_loop.rs`), `GenerateTasksNode`
/// (`setup.rs`), and `EndReviewNode` (`end_review.rs`). The first four are
/// exactly [`super::wrap_up::COST_BEARING_STAGES`]; `EndReviewNode` is a
/// billed call that is deliberately NOT a member of that constant — see the
/// carryover `end-review-node-is-billed-but-absent-from-cost-bearing-stages`,
/// which is why repairing its carry-forward here does not by itself make its
/// spend visible to `total_cost_usd`.
pub(crate) fn carry_forward_billing(
    ctx: &engine_contract::TaskContext,
    identity: &str,
    result: &mut serde_json::Value,
) {
    const BILLING_KEYS: [&str; 4] = [
        "transport",
        "cost_usd",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
    ];
    let Some(prior) = ctx.nodes.get(identity) else {
        return;
    };
    for key in BILLING_KEYS {
        match prior.get(key) {
            Some(value) if !value.is_null() => {
                result[key] = value.clone();
            }
            _ => {}
        }
    }
}
