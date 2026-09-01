//! The run-scoped ledger of Claude CLI sessions a run consumed.
//!
//! # Why this exists
//!
//! A run's real token cost cannot be read off the run itself. It lives in the CLI's session
//! transcripts at `~/.claude/projects/<project>/<session_id>.jsonl`, one file per invocation, and
//! the only exact way to find them is the `session_id` the CLI returns on every envelope. Joining
//! by a `started_at`/`updated_at` timestamp window instead is inference, and it is what an estimate
//! measured ~113x low was being checked against.
//!
//! # Why a ledger and not a single id
//!
//! **An engine-rs run is 1:N with Claude sessions, never 1:1.** Every LLM stage is a separate
//! headless `claude` invocation ([`crate::nodes::claude_code_step::ClaudeCodeStep`], which never
//! passes `--resume`), so one `SDLC_FLOW` run spans one session per stage per attempt. A scalar
//! "the run's session id" would name one of them and silently lose the rest — and the segment it
//! did name would look exact, so nothing would appear wrong.
//!
//! This is not an engine-rs quirk. base-template's JS engines carry a scalar `workflow_run_id`
//! that is stamped by the *resuming* invocation, so a run paused and resumed reports only its
//! second segment; measured on jynx's `JX.3.B`, anchoring on it lost 34% of the run's tokens
//! (25,731,072 + 49,413,222 = 75,144,294 true total). The general truth is 1:N; the JS case is a
//! smaller N.
//!
//! # Shape
//!
//! Entries are **append-only and order-preserving**: the order is the order the invocations
//! happened, which is what makes per-stage attribution (cost per SDLC stage, not merely per run)
//! readable off the list. Each entry names the node that made the call, so a consumer can attribute
//! spend to `implement` vs `review` vs `triage` rather than to the run as a whole.
//!
//! Failed invocations are recorded too, with `ok: false`. A `claude` call that reached the API and
//! came back `is_error` still billed for the attempt, and dropping it would understate exactly the
//! runs a cost comparison cares about most. Transport failures with no envelope at all
//! (`Spawn`/`BinaryNotFound`/`Timeout`/`Parse`) have no session id to record and so appear here not
//! at all — absence of an entry never means a free call, only that no session was ever established.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The `TaskContext::metadata` key under which the session ledger lives. Sibling to
/// `workflow::RUN_ID_METADATA_KEY`/`BUDGET_METADATA_KEY`.
pub const SESSIONS_METADATA_KEY: &str = "claude_sessions";

/// One Claude CLI invocation a run made, with what it cost.
///
/// This is the run's only PER-INVOCATION record. `ctx.nodes` is keyed by node identity, so a stage
/// that runs six times leaves one entry there — the sixth. Six entries land here.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ClaudeSession {
    /// The `Node::name()` identity of the step that made the call — the per-stage attribution key.
    pub node: String,
    /// The CLI's `session_id`, which is literally the transcript's filename stem.
    ///
    /// `None` for an invocation that established no Claude session — a local OpenAI-compatible
    /// transport, or a failure with no envelope. Such a call still has a cost, so it is still
    /// recorded; only its transcript join is missing.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Whether the invocation succeeded. `false` marks a call that reached the API and came back
    /// `is_error` — billed, and deliberately kept.
    pub ok: bool,
    /// The CLI's `total_cost_usd` for this invocation.
    #[serde(default)]
    pub cost_usd: f64,
    /// Uncached input tokens (`usage.input_tokens`).
    #[serde(default)]
    pub input_tokens: u64,
    /// Output tokens (`usage.output_tokens`).
    #[serde(default)]
    pub output_tokens: u64,
    /// Tokens read from the prompt cache. Bills at ~10% of an uncached input token.
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    /// Tokens written to the prompt cache. Bills at ~125% of an uncached input token.
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
}

/// Append one invocation to `metadata`'s ledger, creating it if absent. Order-preserving.
///
/// # No de-duplication, deliberately
///
/// Called exactly once per invocation, at the point the CLI call returns. Nothing replays it: a
/// resumed run rehydrates the ledger array as data and carries it forward — it does not re-append
/// the entries already in it. So there is no duplicate to suppress.
///
/// An earlier revision deduped, first by `session_id` and then by whole-entry equality. Both are
/// wrong now that entries carry money. Two invocations can legitimately be identical in every
/// recorded field — same stage, same outcome, same cost, and `session_id: None` for a local
/// transport — and collapsing them would silently halve that stage's reported spend. A dedup rule
/// guarding no real path, which can only ever undercount, is worse than none.
pub fn append_session(metadata: &mut Value, session: ClaudeSession) {
    if !metadata.is_object() {
        *metadata = serde_json::json!({});
    }

    match metadata
        .get_mut(SESSIONS_METADATA_KEY)
        .filter(|v| v.is_array())
    {
        Some(Value::Array(entries)) => entries.push(serde_json::json!(session)),
        _ => metadata[SESSIONS_METADATA_KEY] = serde_json::json!([session]),
    }
}

/// Read back the ledger in order. Returns an empty vec for absent, non-array, or non-object
/// metadata, and silently skips any malformed entry — never panics, never errors. A telemetry
/// channel must not be able to fail a run.
pub fn read_sessions(metadata: &Value) -> Vec<ClaudeSession> {
    metadata
        .get(SESSIONS_METADATA_KEY)
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|e| serde_json::from_value::<ClaudeSession>(e.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// The ledger's session ids alone, in order — the flat form a transcript-joining consumer wants.
/// Invocations that established no session contribute nothing here (but still count toward
/// [`ledger_totals`]).
pub fn read_session_ids(metadata: &Value) -> Vec<String> {
    read_sessions(metadata)
        .into_iter()
        .filter_map(|s| s.session_id)
        .collect()
}

/// What a run actually spent, summed over EVERY invocation.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LedgerTotals {
    /// Number of CLI invocations the run made, successful and failed alike.
    pub invocations: usize,
    pub cost_usd: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
}

/// Roll the ledger up into a run total.
///
/// This is the accurate figure, and it differs from the per-stage scan of `ctx.nodes` that
/// preceded it: that scan reads each stage's LAST recorded `cost_usd`, so a stage the task loop ran
/// six times contributed only its sixth call. The ledger holds all six. The harder a run worked —
/// the more retries, the more review rounds — the more the old figure undercounted it.
#[must_use]
pub fn ledger_totals(metadata: &Value) -> LedgerTotals {
    read_sessions(metadata)
        .iter()
        .fold(LedgerTotals::default(), |mut acc, s| {
            acc.invocations += 1;
            acc.cost_usd += s.cost_usd;
            acc.input_tokens += s.input_tokens;
            acc.output_tokens += s.output_tokens;
            acc.cache_read_input_tokens += s.cache_read_input_tokens;
            acc.cache_creation_input_tokens += s.cache_creation_input_tokens;
            acc
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(node: &str, id: Option<&str>, ok: bool, cost: f64) -> ClaudeSession {
        ClaudeSession {
            node: node.to_string(),
            session_id: id.map(str::to_string),
            ok,
            cost_usd: cost,
            input_tokens: 100,
            output_tokens: 10,
            cache_read_input_tokens: 5,
            cache_creation_input_tokens: 1,
        }
    }

    #[test]
    fn appends_in_order_and_reads_back() {
        let mut meta = serde_json::json!({});
        append_session(&mut meta, session("Implement", Some("s1"), true, 0.1));
        append_session(&mut meta, session("Test", Some("s2"), true, 0.2));
        append_session(&mut meta, session("Review", Some("s3"), false, 0.3));

        assert_eq!(read_session_ids(&meta), vec!["s1", "s2", "s3"]);
        assert!(!read_sessions(&meta)[2].ok);
        assert_eq!(read_sessions(&meta)[0].node, "Implement");
    }

    /// THE BUG THIS EXISTS TO FIX. A stage the task loop re-runs contributes every one of its
    /// calls, not just the last. `ctx.nodes["ImplementNode"]` would hold only the third of these.
    #[test]
    fn a_stage_that_ran_three_times_contributes_all_three_costs() {
        let mut meta = serde_json::json!({});
        append_session(&mut meta, session("Implement", Some("s1"), false, 1.0));
        append_session(&mut meta, session("Implement", Some("s2"), false, 2.0));
        append_session(&mut meta, session("Implement", Some("s3"), true, 4.0));

        let totals = ledger_totals(&meta);
        assert_eq!(totals.invocations, 3);
        assert!(
            (totals.cost_usd - 7.0).abs() < 1e-9,
            "all three attempts must count, not just the one that succeeded"
        );
        assert_eq!(totals.input_tokens, 300);
        assert_eq!(totals.output_tokens, 30);
        assert_eq!(totals.cache_read_input_tokens, 15);
        assert_eq!(totals.cache_creation_input_tokens, 3);
    }

    /// Two invocations identical in every recorded field are still two billed calls. An earlier
    /// revision deduped and would have collapsed these, halving the reported spend.
    #[test]
    fn two_indistinguishable_invocations_both_count() {
        let mut meta = serde_json::json!({});
        append_session(&mut meta, session("Implement", None, true, 3.0));
        append_session(&mut meta, session("Implement", None, true, 3.0));

        let totals = ledger_totals(&meta);
        assert_eq!(totals.invocations, 2);
        assert!((totals.cost_usd - 6.0).abs() < 1e-9);
    }

    /// A call with no Claude session (local transport) still cost money and must still be summed;
    /// it simply contributes no transcript id.
    #[test]
    fn an_invocation_without_a_session_id_still_counts_toward_cost() {
        let mut meta = serde_json::json!({});
        append_session(&mut meta, session("Summarize", None, true, 0.5));

        assert!(read_session_ids(&meta).is_empty());
        assert_eq!(ledger_totals(&meta).invocations, 1);
        assert!((ledger_totals(&meta).cost_usd - 0.5).abs() < 1e-9);
    }

    #[test]
    fn append_repairs_a_non_object_or_non_array_ledger() {
        let mut meta = serde_json::json!("not an object");
        append_session(&mut meta, session("Implement", Some("s1"), true, 0.1));
        assert_eq!(read_session_ids(&meta), vec!["s1"]);

        let mut meta = serde_json::json!({ SESSIONS_METADATA_KEY: "not an array" });
        append_session(&mut meta, session("Implement", Some("s1"), true, 0.1));
        assert_eq!(read_session_ids(&meta), vec!["s1"]);
    }

    #[test]
    fn reading_absent_or_malformed_metadata_yields_an_empty_ledger() {
        assert!(read_sessions(&serde_json::json!({})).is_empty());
        assert!(read_sessions(&serde_json::json!(null)).is_empty());
        assert!(read_sessions(&serde_json::json!({ SESSIONS_METADATA_KEY: 7 })).is_empty());
        assert_eq!(
            ledger_totals(&serde_json::json!({})),
            LedgerTotals::default()
        );
    }

    /// A malformed entry is skipped, not fatal, and never hides the valid ones around it.
    #[test]
    fn malformed_entries_are_skipped_not_fatal() {
        let meta = serde_json::json!({
            SESSIONS_METADATA_KEY: [
                { "node": "Implement", "session_id": "s1", "ok": true, "cost_usd": 1.0 },
                { "nonsense": true },
                { "node": "Review", "session_id": "s2", "ok": false, "cost_usd": 2.0 },
            ]
        });

        assert_eq!(read_session_ids(&meta), vec!["s1", "s2"]);
        assert!((ledger_totals(&meta).cost_usd - 3.0).abs() < 1e-9);
    }

    /// An entry from before the billing fields existed reads as zero cost, not as a parse failure.
    #[test]
    fn a_pre_billing_entry_still_parses() {
        let meta = serde_json::json!({
            SESSIONS_METADATA_KEY: [{ "node": "Implement", "session_id": "s1", "ok": true }]
        });

        assert_eq!(ledger_totals(&meta).invocations, 1);
        assert_eq!(ledger_totals(&meta).cost_usd, 0.0);
    }
}
