//! `AgentOutcome`/`CostEstimate` — the backend-agnostic shape a non-`claude_cli`
//! transport (`EN.16.B` task 6's `PiTransport`, and any future backend) reports
//! its result in, plus the single translation into
//! `(claude_code_rs::Outcome, TransportInfo)` that lets such a transport slot
//! into [`super::agent_code_step::MetaTransport`] unchanged.
//!
//! `claude_code_rs::Outcome` is shaped entirely around the `claude` CLI's own
//! JSON envelope (a real dollar cost, per-model usage, a session id). A local
//! model driven through a different binary cannot honestly fill most of that
//! shape — in particular it usually cannot report a real-money cost at all.
//! `AgentOutcome` is the minimal, honest shape such a backend CAN report, and
//! [`translate`] is the ONE place that maps it onto the existing `Outcome`
//! wire type, so every downstream reader (`AgentCodeStep`, `BudgetLedger`,
//! the session ledger, `RunTelemetry`) keeps reading `Outcome`/`TransportInfo`
//! with no new type to special-case.

use std::collections::BTreeMap;

use claude_code_rs::parse::Usage;
use claude_code_rs::Outcome;

use super::agent_code_step::TransportInfo;

/// What a non-`claude_cli` backend reports for one invocation.
///
/// Deliberately narrower than `claude_code_rs::Outcome`: no per-model usage
/// map, no session id, no structured-output slot — those are CLI-specific
/// concepts a different backend has no honest value for. `cost` is an
/// `Option` for the same reason: a backend that cannot report real dollars
/// (task 3's whole point) must be able to say so, not spell it as `0.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentOutcome {
    /// Whether the invocation succeeded. Distinct from `cost` being unknown —
    /// a run can succeed at zero *known* cost.
    pub success: bool,
    /// The model's reply text (or, on failure, whatever diagnostic text the
    /// backend has — e.g. captured stderr).
    pub text: String,
    /// Paths the backend reports having modified, when it can report them at
    /// all. `EN.16.B` task 7 prefers the worktree's own git state over this
    /// field for non-`claude_cli` backends precisely because a local model's
    /// self-report is not schema-constrained the way the `claude` CLI's is —
    /// this field exists for backends that can populate it honestly.
    pub modified_files: Vec<String>,
    /// The invocation's cost, when the backend can estimate one at all.
    /// `None` here is what ultimately makes [`translate`] stamp
    /// `TransportInfo.cost_known: false`.
    pub cost: Option<CostEstimate>,
}

/// A backend's own estimate of what one invocation cost.
///
/// `tokens` and `dollars` vary independently: a backend can know its token
/// count (it controls the API/CLI call that produced it) while having no
/// honest dollar figure at all (a local model has no per-token price to
/// multiply by). `dollars: Some(0.0)` and `dollars: None` are NOT the same
/// claim — the former asserts "this really cost nothing", the latter asserts
/// "I cannot say what this cost" — and [`translate`] is deliberately built so
/// only the `None` case marks the resulting `TransportInfo.cost_known` false.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CostEstimate {
    /// Tokens the backend reports consuming for this invocation. Carried
    /// through to `Outcome.usage` regardless of whether `dollars` is known —
    /// token counts and dollar cost are independent claims (see the struct
    /// doc), so an unknown price must never suppress a known token count.
    pub tokens: u64,
    /// The invocation's cost in USD, when the backend can compute one.
    /// `None` means "unknown", not "zero" — see the struct doc.
    pub dollars: Option<f64>,
}

/// The one translation from a backend-agnostic [`AgentOutcome`] to the
/// `claude_code_rs::Outcome`/[`TransportInfo`] pair every existing reader
/// already understands.
///
/// `backend` names the transport family that produced `outcome` (e.g.
/// `"pi"`) and is stamped verbatim onto the returned `TransportInfo.backend`.
///
/// Mapping rules (task 3's contract):
/// - `TransportInfo.cost_known` is `true` **only** when `outcome.cost.dollars`
///   is `Some` — including `Some(0.0)`, which is a real zero, not an unknown.
///   `outcome.cost` being `None` at all is treated the same as
///   `dollars: None`: no dollar claim was made, so cost is unknown.
/// - `Outcome.cost_usd` carries the real dollar figure when known, and a
///   `0.0` placeholder when `cost_known` is false. Per task 3's contract, no
///   writer downstream may read `cost_usd` when `cost_known` is false — the
///   placeholder exists only because `Outcome.cost_usd` is a bare `f64` with
///   no way to encode "unknown" itself.
/// - Token counts reach `Outcome.usage` in every case, dollars-known or not —
///   an unknown price must never cost the run its token accounting.
///   `claude_code_rs::Usage` has no single "total tokens" field, so the
///   backend's one token count is carried as `output_tokens` (input/cache
///   fields are CLI-specific breakdowns a single-number backend cannot
///   honestly split; the OpenAI-compatible local transport takes the same
///   shortcut for its `completion_tokens` count).
/// - `claude_code_rs::Outcome` has no `#[non_exhaustive]`, so this
///   constructs it by literal (per the block's `notes`).
pub fn translate(outcome: AgentOutcome, backend: &str) -> (Outcome, TransportInfo) {
    let tokens = outcome.cost.map(|c| c.tokens).unwrap_or(0);
    let dollars = outcome.cost.and_then(|c| c.dollars);
    let cost_known = dollars.is_some();
    let cost_usd = dollars.unwrap_or(0.0);

    let claude_outcome = Outcome {
        cost_usd,
        usage: Usage {
            input_tokens: 0,
            output_tokens: tokens,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
        model_usage: BTreeMap::new(),
        text: outcome.text,
        is_error: !outcome.success,
        api_error_status: None,
        session_id: None,
        structured_output: None,
    };

    let info = TransportInfo {
        tier: "local".to_string(),
        model: String::new(),
        endpoint: None,
        backend: backend.to_string(),
        cost_known,
    };

    (claude_outcome, info)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(cost: Option<CostEstimate>) -> AgentOutcome {
        AgentOutcome {
            success: true,
            text: "did the thing".to_string(),
            modified_files: vec!["src/lib.rs".to_string()],
            cost,
        }
    }

    /// `dollars: Some(x)` -> `cost_known: true` and `cost_usd == x`.
    #[test]
    fn agent_outcome_dollars_known_maps_to_cost_known_true() {
        let (claude_outcome, info) = translate(
            outcome(Some(CostEstimate {
                tokens: 42,
                dollars: Some(1.23),
            })),
            "pi",
        );

        assert!(info.cost_known);
        assert_eq!(claude_outcome.cost_usd, 1.23);
        assert_eq!(claude_outcome.usage.output_tokens, 42);
        assert_eq!(info.backend, "pi");
    }

    /// `dollars: None` -> `cost_known: false`, but tokens are still carried.
    #[test]
    fn agent_outcome_dollars_unknown_maps_to_cost_known_false_but_keeps_tokens() {
        let (claude_outcome, info) = translate(
            outcome(Some(CostEstimate {
                tokens: 42,
                dollars: None,
            })),
            "pi",
        );

        assert!(!info.cost_known);
        assert_eq!(claude_outcome.usage.output_tokens, 42);
    }

    /// No `CostEstimate` at all behaves the same as `dollars: None`.
    #[test]
    fn agent_outcome_no_cost_estimate_maps_to_cost_known_false_with_zero_tokens() {
        let (claude_outcome, info) = translate(outcome(None), "pi");

        assert!(!info.cost_known);
        assert_eq!(claude_outcome.usage.output_tokens, 0);
    }

    /// `dollars: Some(0.0)` is a REAL zero, not an unknown — this is what
    /// distinguishes "cost nothing" from "cost unknown" (task 3's whole
    /// point, and the case task 9's dollars-unknown integration test
    /// re-runs against to prove the two are told apart).
    #[test]
    fn agent_outcome_dollars_some_zero_is_cost_known_true() {
        let (claude_outcome, info) = translate(
            outcome(Some(CostEstimate {
                tokens: 7,
                dollars: Some(0.0),
            })),
            "pi",
        );

        assert!(info.cost_known);
        assert_eq!(claude_outcome.cost_usd, 0.0);
    }

    /// A failed invocation is reflected as `is_error: true` on the translated
    /// `Outcome`.
    #[test]
    fn agent_outcome_failure_maps_to_is_error_true() {
        let mut o = outcome(None);
        o.success = false;
        let (claude_outcome, _info) = translate(o, "pi");

        assert!(claude_outcome.is_error);
    }
}
