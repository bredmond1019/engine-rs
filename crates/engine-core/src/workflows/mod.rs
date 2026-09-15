//! Concrete workflows built on the `engine-core` `Node`/`Router`/`Workflow`
//! primitives. Each submodule owns one ported workflow graph.
//!
//! The model-node seams shared across every workflow — `ModelTransport`, the
//! `ctx.nodes` helpers `put_result`/`get_result`, `strip_json_fence`, and the
//! `parse_structured_or_fenced` structured-output-preferred-over-fence parse
//! pattern (repeated across `ImplementTaskNode`/`TriageTaskNode`/
//! `ConsolidatedReviewNode`/`GenerateTasksNode`/`PatchDocsNode` — see
//! `EN.1-plan.A`) — live here so any future workflow can reuse them without
//! depending on `sdlc_flow`. `CommandOutput`/`CommandRunner`/
//! `default_command_runner`/`commit_all`/`is_noop_commit` also live here
//! (hoisted out of `sdlc_flow` in `EN.11.M` task 2, so a second engine can
//! shell out through the same injectable, org-floor-gated seam without
//! depending on `sdlc_flow`); `sdlc_flow` re-exports the hoisted seams so
//! existing `super::`/`sdlc_flow::` import sites resolve unchanged (EN.4.0
//! task 4, EN.11.M task 2).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use claude_code_rs::{Config, Outcome};
use engine_contract::TaskContext;
use futures::future::BoxFuture;

use crate::policy::command_floor::{self, CommandDecision};
use crate::sessions::{self, ClaudeSession};

pub mod approve_and_run;
pub mod claim_reaffirm;
pub mod commander;
pub mod consolidate;
pub mod content_pipeline;
pub mod deliverable_render;
pub mod diagnostic_intake;
pub mod harvest_approve;
pub mod lead_ingest;
pub mod linkedin_post;
pub mod llm_node;
pub mod opportunity_edit;
pub mod orchestration;
pub mod plan_authoring;
pub mod proposal_generator;
pub mod queue_park;
pub mod recall;
pub mod research_agent;
pub mod sdlc_flow;
pub mod sdlc_task;
pub mod sweep;
pub mod terminal_probe;
pub mod transport_slot;

pub use transport_slot::TransportSlot;

/// The injectable transport signature for model-calling nodes' composed
/// `AgentCodeStep`s — identical shape to `AgentCodeStep`'s own (private)
/// transport type. Defaults to the real `claude_code_rs::execute`; tests
/// substitute a stub via each node's `with_transport`.
pub type ModelTransport = Arc<
    dyn Fn(Config, String) -> BoxFuture<'static, claude_code_rs::Result<Outcome>> + Send + Sync,
>;

/// Stamp a node's output onto `ctx.nodes` under its own identity.
pub(crate) fn put_result(ctx: &mut TaskContext, identity: &str, value: serde_json::Value) {
    ctx.nodes.insert(identity.to_string(), value);
}

/// Look up a prior node's output from `ctx.nodes` by identity.
pub(crate) fn get_result<'a>(
    ctx: &'a TaskContext,
    identity: &str,
) -> Option<&'a serde_json::Value> {
    ctx.nodes.get(identity)
}

/// Snapshot the current length of `ctx`'s session ledger, to be paired with [`sessions_since`]
/// after a wrapper's inner `AgentCodeStep` call returns.
///
/// # Why a length, not a clone
///
/// A wrapper that bills the API via an inner step and then fails at parse/validation time
/// currently constructs a bare `NodeError::new(..)`, whose `sessions` is empty — the billed entry
/// dies with the discarded `ctx` when `node_context`'s `Node::process(ctx)` (by-value) call
/// returns `Err`. `node_context` already has a carry-on-error channel (`NodeError::sessions` /
/// `NodeError::with_sessions`, replayed onto the pre-call ledger on the `Err` branch) — the gap is
/// only that these wrappers never populate it for a post-billed-call failure.
///
/// Pairing a cheap `usize` baseline with [`sessions_since`] — rather than diffing two full ledger
/// clones — keeps this a per-invocation delta, not a per-dispatch whole-context clone. Attaching
/// the *whole* ledger onto `NodeError` would double-count every entry already present in the
/// pre-call snapshot `node_context` replays onto (it prepends `err.sessions`, it does not merge
/// them), inflating the run's reported spend — the opposite of what this exists to fix.
pub(crate) fn session_baseline(ctx: &TaskContext) -> usize {
    sessions::read_sessions(&ctx.metadata).len()
}

/// The ledger entries appended to `ctx` after `baseline`, in order.
///
/// Returns an empty vec when nothing was appended, and also when the ledger is shorter than
/// `baseline` (should not happen — the ledger is append-only — but a telemetry helper must never
/// panic or slice out of bounds over a malformed/shrunk ledger; [`sessions::read_sessions`]
/// follows the same never-panic contract for the same reason).
pub(crate) fn sessions_since(ctx: &TaskContext, baseline: usize) -> Vec<ClaudeSession> {
    let all = sessions::read_sessions(&ctx.metadata);
    if all.len() <= baseline {
        return Vec::new();
    }
    all[baseline..].to_vec()
}

#[cfg(test)]
mod session_delta_tests {
    use super::{session_baseline, sessions_since};
    use crate::sessions::{append_session, ClaudeSession};
    use engine_contract::TaskContext;

    fn make_session(node: &str) -> ClaudeSession {
        ClaudeSession {
            node: node.to_string(),
            session_id: Some(format!("sess-{node}")),
            ok: true,
            cost_usd: 0.01,
            input_tokens: 10,
            output_tokens: 5,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            model: String::new(),
            started_at: None,
            cost_known: true,
        }
    }

    fn ctx_with_metadata(metadata: serde_json::Value) -> TaskContext {
        TaskContext {
            event: serde_json::json!({}),
            nodes: Default::default(),
            metadata,
            node_runs: Default::default(),
        }
    }

    #[test]
    fn baseline_on_empty_ledger_is_zero() {
        let ctx = ctx_with_metadata(serde_json::json!({}));
        assert_eq!(session_baseline(&ctx), 0);
    }

    #[test]
    fn sessions_since_returns_only_appended_entries_in_order() {
        let mut metadata = serde_json::json!({});
        append_session(&mut metadata, make_session("a"));
        let mut ctx = ctx_with_metadata(metadata);
        let baseline = session_baseline(&ctx);
        assert_eq!(baseline, 1);

        append_session(&mut ctx.metadata, make_session("b"));
        append_session(&mut ctx.metadata, make_session("c"));

        let delta = sessions_since(&ctx, baseline);
        assert_eq!(delta.len(), 2);
        assert_eq!(delta[0].node, "b");
        assert_eq!(delta[1].node, "c");
    }

    #[test]
    fn sessions_since_empty_when_nothing_appended() {
        let mut metadata = serde_json::json!({});
        append_session(&mut metadata, make_session("a"));
        let ctx = ctx_with_metadata(metadata);
        let baseline = session_baseline(&ctx);

        let delta = sessions_since(&ctx, baseline);
        assert!(delta.is_empty());
    }

    #[test]
    fn sessions_since_never_panics_on_shrunk_ledger() {
        let mut metadata = serde_json::json!({});
        append_session(&mut metadata, make_session("a"));
        append_session(&mut metadata, make_session("b"));
        let ctx = ctx_with_metadata(metadata);

        // Baseline claims a length longer than the ledger actually has.
        let delta = sessions_since(&ctx, 99);
        assert!(delta.is_empty());
    }

    #[test]
    fn sessions_since_on_absent_metadata_is_empty() {
        let ctx = ctx_with_metadata(serde_json::json!(null));
        assert_eq!(session_baseline(&ctx), 0);
        assert!(sessions_since(&ctx, 0).is_empty());
    }
}

/// Strip a Markdown code fence (` ```json ... ``` ` or plain ` ``` ... ``` `)
/// wrapping a model's reply, if present, so a strict `serde_json::from_str`
/// parse still succeeds. Every model node here prompts for "strict JSON",
/// but a real `claude` response commonly wraps it in a fence anyway
/// (observed live, `EN.3.C`+ manual verification) — this is the one
/// normalization applied before every model-output JSON parse in this
/// module. Returns the input unchanged (just trimmed) when no fence is
/// present, so a genuinely bare JSON reply round-trips exactly as before.
pub(crate) fn strip_json_fence(text: &str) -> &str {
    let trimmed = text.trim();
    let Some(after_open) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    // Drop an optional language tag (e.g. `json`) up to the first newline.
    let after_lang = match after_open.find('\n') {
        Some(idx) => &after_open[idx + 1..],
        None => after_open,
    };
    match after_lang.rfind("```") {
        Some(idx) => after_lang[..idx].trim(),
        None => trimmed,
    }
}

/// Best-effort extraction of the first balanced JSON object or array
/// embedded anywhere in `text` — the one further normalization needed on top
/// of [`strip_json_fence`] for a chattier local model, which will still wrap
/// its verdict in prose even after the fence is stripped (observed live
/// during the local-model bench, `EN.local-model-bench`: replies of the
/// shape `Here is my review:\n{...}\nLet me know if you have questions.`).
///
/// Conservative by construction: this only narrows the byte range handed to
/// `serde_json::from_str` by matching `{`/`[` against `}`/`]` (tracking
/// string literals and `\`-escapes so a brace inside a quoted string never
/// perturbs the depth count) — it never rewrites a single byte of what falls
/// inside that range. Genuinely malformed JSON inside the boundaries (a
/// single-quoted key, a trailing comma) still fails the caller's subsequent
/// parse; this function's job is finding the substring, not repairing it.
///
/// Returns `None` when `text` contains no `{`/`[` at all, or when the one
/// found never closes (an unbalanced/truncated reply).
pub(crate) fn extract_balanced_json(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let start = text.find(['{', '['])?;
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' | b'[' => depth += 1,
            b'}' | b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..=i]);
                }
                if depth < 0 {
                    // A stray closer before anything opened at `start` — not
                    // a balanced region; bail rather than report a bogus span.
                    return None;
                }
            }
            _ => {}
        }
    }
    None
}

/// Prefer the pre-parsed `structured` value written by a `AgentCodeStep`
/// (stamped onto `ctx.nodes[node_name]["structured"]`) when present and
/// non-null; otherwise fall back to [`strip_json_fence`] +
/// `serde_json::from_str` on the raw text `content`, and — only if that
/// strict parse fails — one further attempt against
/// [`extract_balanced_json`]'s narrower span (a chattier local model's
/// prose-wrapped verdict). Factored out of the byte-identical copies
/// `ImplementTaskNode`/`TriageTaskNode`/`ConsolidatedReviewNode`
/// (`task_loop.rs`), `GenerateTasksNode` (`setup.rs`), and `PatchDocsNode`
/// (`docs.rs`) each carried privately (EN.4.0 task 4); `EndReviewNode`
/// (`end_review.rs`) is the newest caller and the one that motivated the
/// balanced-extraction fallback (local-model bench false negatives).
///
/// On total failure, returns the ORIGINAL strict-parse error (over the
/// balanced-extraction attempt's, if that also ran and also failed) — it is
/// the more informative of the two for a caller reporting "why didn't this
/// parse", since it points at the fence-stripped text as the model actually
/// sent it rather than an already-narrowed substring.
pub(crate) fn parse_structured_or_fenced<T: serde::de::DeserializeOwned>(
    ctx: &TaskContext,
    node_name: &str,
    content: &str,
) -> Result<T, serde_json::Error> {
    let structured = get_result(ctx, node_name).and_then(|value| value.get("structured").cloned());
    if let Some(value) = structured {
        if !value.is_null() {
            return serde_json::from_value(value);
        }
    }
    let fence_stripped = strip_json_fence(content);
    match serde_json::from_str(fence_stripped) {
        Ok(parsed) => Ok(parsed),
        Err(strict_err) => match extract_balanced_json(fence_stripped) {
            Some(candidate) if candidate != fence_stripped => {
                serde_json::from_str(candidate).or(Err(strict_err))
            }
            _ => Err(strict_err),
        },
    }
}

/// Cap on a diagnostics preview of a model's raw reply when it turns out to
/// be unparseable — shared by every [`ModelVerdict::Unparseable`] site so the
/// whole workspace has one convention instead of each node picking its own
/// cap. Established by `EndReviewNode`'s original fix
/// (`EN.ticket.review-mode-endonly-reviews-nothing` /
/// local-model-bench-motivated `UNPARSEABLE` verdict); hoisted here when the
/// pattern was generalized to other nodes (`EN.ticket.model-verdict-shared-
/// abstraction`). Generous but bounded: a diagnostics aid for an operator
/// reading committed state, not a value any Rust branch parses, so it should
/// never risk a multi-KB local-model ramble bloating a run's persisted state.
pub(crate) const RAW_OUTPUT_PREVIEW_MAX_CHARS: usize = 2000;

/// Truncate `text` to at most [`RAW_OUTPUT_PREVIEW_MAX_CHARS`] chars (not
/// bytes — char-boundary-safe on any UTF-8 input), appending a visible marker
/// when truncated so the preview never silently looks complete.
pub(crate) fn truncate_for_diagnostics(text: &str) -> String {
    if text.chars().count() <= RAW_OUTPUT_PREVIEW_MAX_CHARS {
        return text.to_string();
    }
    let truncated: String = text.chars().take(RAW_OUTPUT_PREVIEW_MAX_CHARS).collect();
    format!("{truncated}... [truncated]")
}

/// The result of attempting to parse a model's reply into `T` via
/// [`parse_structured_or_fenced`], as a typed value a **node** pattern-matches
/// on to decide fatal-vs-degrade for itself — rather than the parsing layer
/// making that call unilaterally by returning a bare `Result`.
///
/// ## Why this exists
///
/// `EndReviewNode` (`sdlc_flow/end_review.rs`) used to map any JSON-parse
/// failure straight to a fatal `NodeError`, killing the whole `SDLC_FLOW` run
/// even when the underlying work was correct and a small local model had just
/// wrapped its verdict in prose. The fix replaced that with a named
/// `"UNPARSEABLE"` verdict routed through the existing
/// `TriageTaskNode`/`ConsolidatedReviewNode` `unrecognized_verdict` house
/// convention (stamp the out-of-enum string; let the router's catch-all arm
/// send the walk to a safe terminal state) instead of a hard crash.
///
/// An audit of every one of `parse_structured_or_fenced`'s call sites
/// (`EN.ticket.model-verdict-shared-abstraction`) found the same fix does
/// **not** generalize to all of them — most are correctly fatal on a parse
/// failure, because parsing structured data IS the node's entire job with no
/// sensible degraded fallback (a translation, a drafted document, an
/// extracted brief, a generated task list). Forcing every call site onto one
/// non-fatal shape would silently turn "this node produced nothing" into "the
/// run continued with garbage" for nodes whose whole contract is the
/// structured output itself. The audit's three buckets:
///
/// - **`FATAL_CORRECT`** (the majority): `content_pipeline::{translate,
///   summarize,revise}`, `linkedin_post::{revise,draft,graph}`,
///   `sdlc_flow::setup::GenerateTasksNode`, `proposal_generator::{
///   opportunity_identifier,revise,writer,company_research}`,
///   `diagnostic_intake::extract::ExtractNode`,
///   `research_agent::{prospecting,company_research}`. The parsed value IS
///   the node's entire output; there is no meaningful degraded fallback, so a
///   fatal `NodeError` on parse failure is the right, honest outcome. Left
///   unchanged.
/// - **`ALREADY_GRACEFUL`**: `nodes::judgment::JudgmentNode` (a typed
///   `JudgmentError::NoStructuredResult` variant callers already switch on),
///   `sdlc_flow::task_loop::ImplementTaskNode` (falls back to the raw text as
///   the summary and derives `modified_files` from git status instead of
///   trusting the parse). Left unchanged — already does the right thing.
/// - **`FATAL_SHOULD_DEGRADE`**: nodes whose output is already a
///   verdict/enum shape with an established non-fatal path for an
///   *out-of-enum* value (the `unrecognized_verdict` convention), where a
///   parse failure is really the same failure mode one level earlier and
///   deserves the same treatment. All 5 identified call sites are now
///   migrated onto [`ModelVerdict`] (a follow-up recount against
///   `parse_structured_or_fenced`'s call sites found 5, not the original
///   audit's "6" — one per file, no call site was ever double-counted):
///   - `sdlc_flow::task_loop::TriageTaskNode` and `ConsolidatedReviewNode` —
///     `EndReviewNode`'s exact siblings (same verdict shape, same router
///     convention, and `EndReviewNode`'s own comment already cited them as
///     the precedent).
///   - `content_pipeline::self_critic::SelfCriticNode` and
///     `linkedin_post::brand_critic::BrandCriticNode` (`CriticEvaluation
///     {verdict}`) — degrade to `CriticVerdict::Revise` with `confidence:
///     0.0` and a diagnostic `issues[]` entry naming the parse failure,
///     reusing `verdict_from_model_text`'s own fail-closed convention for an
///     ambiguous verdict *value*.
///   - `claim_reaffirm::judge::JudgeClaimNode` (`JudgeOutput{action}`) —
///     degrades straight to `VerdictAction::NeedsHuman`, the same fallback
///     this node already forces structurally when evidence is empty (OR.K3)
///     — an unparseable reply is the same "cannot trust the model's
///     judgment" case one level earlier.
///   - `sdlc_flow::docs::PatchDocsNode` — degrades onto its own
///     `flagged: Vec<String>` non-fatal routing path, flagging the
///     `modified_files` that triggered the pass (the docs-patch outcome is
///     unknown, so the whole batch goes to human review) instead of
///     `files_patched`.
///   - `proposal_generator::review::ProposalReviewNode`
///     (`Verdict::from_model_text`) — degrades to `Verdict::Revise`, the
///     same fail-closed default that function already applies to an
///     ambiguous verdict *value*.
///
///   Every migration stamps a bounded `raw_output_preview` onto its result
///   for operator diagnostics, matching `EndReviewNode`'s convention.
///
/// ## How to use it
///
/// Call [`parse_model_verdict`] instead of `parse_structured_or_fenced(..)?`.
/// On [`ModelVerdict::Unparseable`], build your node's own result shape with
/// whatever verdict string means "unusable model output" in your node's enum
/// (follow `EndReviewNode`'s `"UNPARSEABLE"` convention unless a different
/// out-of-enum string already exists for your node), and let your router's
/// existing catch-all arm carry it to a safe terminal state — the same shape
/// the `unrecognized_verdict` convention already uses for an out-of-enum
/// *value*. Do not invent a second preview-length or naming convention
/// alongside this one.
#[derive(Debug)]
pub(crate) enum ModelVerdict<T> {
    /// The reply parsed cleanly (structured field, bare JSON, or recovered
    /// via [`extract_balanced_json`]).
    Parsed(T),
    /// The reply survived every parse attempt and still was not valid JSON.
    /// `raw_preview` is [`truncate_for_diagnostics`]'s bounded rendering of
    /// the model's actual raw content; `reason` is the underlying
    /// `serde_json::Error` rendered to a string.
    Unparseable { raw_preview: String, reason: String },
}

/// Parse a model's reply into `T` via [`parse_structured_or_fenced`] and
/// return the outcome as a [`ModelVerdict`] instead of a `Result` — the
/// classification decision (fatal vs. degrade) is left to the caller. See
/// [`ModelVerdict`]'s doc comment for when to reach for this instead of
/// propagating `parse_structured_or_fenced`'s `Err` as a fatal `NodeError`.
pub(crate) fn parse_model_verdict<T: serde::de::DeserializeOwned>(
    ctx: &TaskContext,
    node_name: &str,
    content: &str,
) -> ModelVerdict<T> {
    match parse_structured_or_fenced(ctx, node_name, content) {
        Ok(value) => ModelVerdict::Parsed(value),
        Err(err) => ModelVerdict::Unparseable {
            raw_preview: truncate_for_diagnostics(content),
            reason: err.to_string(),
        },
    }
}

/// Map a raw (already-uppercased) verdict string onto the shared
/// `PASS`/`FAIL`/`PARTIAL` vocabulary `EndReviewNode` and
/// `ConsolidatedReviewNode` both ask for in their prompt. Local-model bench
/// (2026-09-14, `edit/aider/qwen2.5-coder:32b`) observed a real reviewer
/// reply carrying `"NOT_MET"` — the per-criterion MET/NOT_MET vocabulary the
/// rendered Acceptance Criteria itself uses — instead of the instructed
/// top-level verdict word, and the exact match sent an otherwise-legible
/// review to `unrecognized_verdict`/`MajorBail`. This narrows that specific,
/// observed synonym confusion; it is not a general fuzzy-matcher — an
/// unrecognized string still falls through to the caller's existing
/// `unrecognized_verdict` fallback unchanged.
pub(crate) fn normalize_pass_fail_partial_synonym(verdict: &str) -> String {
    match verdict {
        "MET" => "PASS".to_string(),
        "NOT_MET" | "UNMET" => "FAIL".to_string(),
        "PARTIAL_MET" | "PARTIALLY_MET" => "PARTIAL".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod normalize_pass_fail_partial_synonym_tests {
    use super::normalize_pass_fail_partial_synonym;

    #[test]
    fn maps_met_not_met_partial_met_synonyms() {
        assert_eq!(normalize_pass_fail_partial_synonym("MET"), "PASS");
        assert_eq!(normalize_pass_fail_partial_synonym("NOT_MET"), "FAIL");
        assert_eq!(normalize_pass_fail_partial_synonym("UNMET"), "FAIL");
        assert_eq!(
            normalize_pass_fail_partial_synonym("PARTIAL_MET"),
            "PARTIAL"
        );
        assert_eq!(
            normalize_pass_fail_partial_synonym("PARTIALLY_MET"),
            "PARTIAL"
        );
    }

    #[test]
    fn leaves_canonical_and_unknown_values_untouched() {
        assert_eq!(normalize_pass_fail_partial_synonym("PASS"), "PASS");
        assert_eq!(normalize_pass_fail_partial_synonym("FAIL"), "FAIL");
        assert_eq!(normalize_pass_fail_partial_synonym("PARTIAL"), "PARTIAL");
        assert_eq!(normalize_pass_fail_partial_synonym("WAT"), "WAT");
        assert_eq!(
            normalize_pass_fail_partial_synonym("UNPARSEABLE"),
            "UNPARSEABLE"
        );
    }
}

#[cfg(test)]
mod model_verdict_tests {
    use super::{parse_model_verdict, ModelVerdict};
    use engine_contract::TaskContext;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    struct Verdict {
        verdict: String,
    }

    fn ctx_without_structured() -> TaskContext {
        TaskContext {
            event: serde_json::json!({}),
            nodes: Default::default(),
            metadata: serde_json::json!({}),
            node_runs: Default::default(),
        }
    }

    #[test]
    fn happy_path_parses_directly() {
        let ctx = ctx_without_structured();
        let outcome: ModelVerdict<Verdict> =
            parse_model_verdict(&ctx, "SomeNode", r#"{"verdict":"PASS"}"#);
        match outcome {
            ModelVerdict::Parsed(v) => assert_eq!(v.verdict, "PASS"),
            ModelVerdict::Unparseable { .. } => panic!("expected Parsed"),
        }
    }

    #[test]
    fn prose_wrapped_reply_recovers_via_balanced_extraction() {
        let ctx = ctx_without_structured();
        let content = "Here is my review: {\"verdict\":\"PASS\"}\nHope that helps!";
        let outcome: ModelVerdict<Verdict> = parse_model_verdict(&ctx, "SomeNode", content);
        match outcome {
            ModelVerdict::Parsed(v) => assert_eq!(v.verdict, "PASS"),
            ModelVerdict::Unparseable { .. } => {
                panic!("expected recovery via extract_balanced_json")
            }
        }
    }

    #[test]
    fn genuinely_unparseable_reply_is_labeled_not_a_result_err() {
        let ctx = ctx_without_structured();
        let content = "I could not complete this due to an internal error.";
        let outcome: ModelVerdict<Verdict> = parse_model_verdict(&ctx, "SomeNode", content);
        match outcome {
            ModelVerdict::Parsed(_) => panic!("expected Unparseable"),
            ModelVerdict::Unparseable {
                raw_preview,
                reason,
            } => {
                assert_eq!(raw_preview, content);
                assert!(!reason.is_empty());
            }
        }
    }

    #[test]
    fn unparseable_preview_is_truncated_and_bounded() {
        let ctx = ctx_without_structured();
        let huge = "x".repeat(super::RAW_OUTPUT_PREVIEW_MAX_CHARS + 500);
        let outcome: ModelVerdict<Verdict> = parse_model_verdict(&ctx, "SomeNode", &huge);
        match outcome {
            ModelVerdict::Parsed(_) => panic!("expected Unparseable"),
            ModelVerdict::Unparseable { raw_preview, .. } => {
                assert!(raw_preview.len() < huge.len());
                assert!(raw_preview.ends_with("... [truncated]"));
            }
        }
    }

    #[test]
    fn single_quoted_keys_are_unparseable_never_silently_repaired() {
        // Conservative-by-design: must never coerce genuinely malformed JSON
        // into a claimed verdict.
        let ctx = ctx_without_structured();
        let outcome: ModelVerdict<Verdict> =
            parse_model_verdict(&ctx, "SomeNode", "{'verdict': 'PASS'}");
        assert!(matches!(outcome, ModelVerdict::Unparseable { .. }));
    }
}

#[cfg(test)]
mod json_extraction_tests {
    use super::{extract_balanced_json, parse_structured_or_fenced, strip_json_fence};
    use engine_contract::TaskContext;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    struct Verdict {
        verdict: String,
    }

    fn ctx_without_structured() -> TaskContext {
        TaskContext {
            event: serde_json::json!({}),
            nodes: Default::default(),
            metadata: serde_json::json!({}),
            node_runs: Default::default(),
        }
    }

    // --- extract_balanced_json ------------------------------------------

    #[test]
    fn extract_balanced_json_finds_bare_object() {
        let text = r#"{"verdict":"PASS"}"#;
        assert_eq!(extract_balanced_json(text), Some(text));
    }

    #[test]
    fn extract_balanced_json_strips_leading_and_trailing_prose() {
        let text = "Here is my review: {\"verdict\":\"PASS\"}\nLet me know if you have questions.";
        assert_eq!(extract_balanced_json(text), Some(r#"{"verdict":"PASS"}"#));
    }

    #[test]
    fn extract_balanced_json_ignores_braces_inside_string_values() {
        let text = r#"blah {"verdict":"PASS","summary":"looks {ok}"} trailing"#;
        assert_eq!(
            extract_balanced_json(text),
            Some(r#"{"verdict":"PASS","summary":"looks {ok}"}"#)
        );
    }

    #[test]
    fn extract_balanced_json_handles_escaped_quotes_inside_strings() {
        let text = r#"prefix {"summary":"she said \"ok\""} suffix"#;
        assert_eq!(
            extract_balanced_json(text),
            Some(r#"{"summary":"she said \"ok\""}"#)
        );
    }

    #[test]
    fn extract_balanced_json_returns_none_when_unbalanced() {
        assert_eq!(extract_balanced_json("prose { \"verdict\": \"PASS\""), None);
    }

    #[test]
    fn extract_balanced_json_returns_none_with_no_braces() {
        assert_eq!(extract_balanced_json("no json here at all"), None);
    }

    #[test]
    fn extract_balanced_json_finds_array() {
        let text = "issues: [\"a\", \"b\"] end";
        assert_eq!(extract_balanced_json(text), Some(r#"["a", "b"]"#));
    }

    // --- parse_structured_or_fenced --------------------------------------

    #[test]
    fn happy_path_bare_json_unchanged() {
        let ctx = ctx_without_structured();
        let out: Verdict =
            parse_structured_or_fenced(&ctx, "SomeNode", r#"{"verdict":"PASS"}"#).unwrap();
        assert_eq!(out.verdict, "PASS");
    }

    #[test]
    fn code_fenced_json_parses_via_strip_json_fence() {
        let ctx = ctx_without_structured();
        let content = "```json\n{\"verdict\":\"PASS\"}\n```";
        let out: Verdict = parse_structured_or_fenced(&ctx, "SomeNode", content).unwrap();
        assert_eq!(out.verdict, "PASS");
    }

    #[test]
    fn prose_wrapped_json_parses_via_balanced_extraction_fallback() {
        let ctx = ctx_without_structured();
        let content = "Here is my review: {\"verdict\":\"PASS\"}\nHope that helps!";
        let out: Verdict = parse_structured_or_fenced(&ctx, "SomeNode", content).unwrap();
        assert_eq!(out.verdict, "PASS");
    }

    #[test]
    fn prose_and_fence_wrapped_json_parses_via_both_normalizations() {
        let ctx = ctx_without_structured();
        let content =
            "Sure thing, here you go:\n```json\n{\"verdict\":\"PASS\"}\n```\nLet me know!";
        let out: Verdict = parse_structured_or_fenced(&ctx, "SomeNode", content).unwrap();
        assert_eq!(out.verdict, "PASS");
    }

    #[test]
    fn single_quoted_keys_remain_a_reported_parse_failure() {
        // Conservative-by-design: balanced extraction only narrows the byte
        // range, it never repairs syntax. Single-quoted keys are genuinely
        // malformed JSON and must still fail — silently coercing them would
        // risk inventing a verdict the model never actually returned.
        let ctx = ctx_without_structured();
        let content = "{'verdict': 'PASS'}";
        let err = parse_structured_or_fenced::<Verdict>(&ctx, "SomeNode", content).unwrap_err();
        assert!(
            !err.to_string().is_empty(),
            "must surface a real parse error"
        );
    }

    #[test]
    fn trailing_comma_remains_a_reported_parse_failure() {
        let ctx = ctx_without_structured();
        let content = r#"{"verdict": "PASS",}"#;
        assert!(parse_structured_or_fenced::<Verdict>(&ctx, "SomeNode", content).is_err());
    }

    #[test]
    fn genuinely_unparseable_output_is_a_reported_parse_failure() {
        let ctx = ctx_without_structured();
        let content = "I could not complete the review due to an internal error.";
        assert!(parse_structured_or_fenced::<Verdict>(&ctx, "SomeNode", content).is_err());
    }

    #[test]
    fn structured_field_still_wins_over_raw_content() {
        let mut ctx = ctx_without_structured();
        ctx.nodes.insert(
            "SomeNode".to_string(),
            serde_json::json!({ "structured": {"verdict": "PASS"} }),
        );
        // Raw content is deliberately garbage — the structured field must be
        // preferred and this must still succeed.
        let out: Verdict = parse_structured_or_fenced(&ctx, "SomeNode", "not json at all").unwrap();
        assert_eq!(out.verdict, "PASS");
    }

    #[test]
    fn strip_json_fence_still_trims_a_bare_reply() {
        assert_eq!(strip_json_fence("  {\"a\":1}  "), "{\"a\":1}");
    }
}

/// Result of running a single shell command via the injectable
/// [`CommandRunner`] seam.
#[derive(Debug, Clone)]
pub struct CommandOutput {
    /// Process exit status (`-1` when the platform reports no code).
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

/// The injectable command-runner signature nodes use to invoke subprocesses
/// (`git`, `gh`, `mev`, ...). Defaults to the real subprocess via
/// [`default_command_runner`]; tests substitute a stub so the gated
/// `cargo test` suite never shells out — mirrors
/// `AgentCodeStep::with_transport` (EN.2.A).
pub type CommandRunner =
    Arc<dyn Fn(&str, &[&str], &Path) -> std::io::Result<CommandOutput> + Send + Sync>;

/// The default [`CommandRunner`]: a thin delegation onto
/// [`default_spec_runner`] with an empty `env` (inherit-only) and
/// `timeout: None` (wait forever) — today's exact behavior. This keeps
/// [`CommandRunner`]'s signature and every one of its 28 call sites
/// untouched (`EN.ticket.command-runner-timeout-and-env`); the org-floor
/// denylist evaluation and the crate's one `std::process::Command` spawn
/// both live in [`default_spec_runner`] now, not here.
#[must_use]
pub fn default_command_runner() -> CommandRunner {
    let spec_runner = default_spec_runner();
    Arc::new(move |program, args, cwd| {
        let spec = CommandSpec {
            program,
            args,
            cwd,
            env: &[],
            timeout: None,
        };
        spec_runner(&spec)
    })
}

/// Superset description of a subprocess invocation for [`SpecCommandRunner`]
/// — additive to the plain `(program, args, cwd)` triple [`CommandRunner`]
/// takes, carrying per-call environment variables and an optional hard
/// timeout. An empty `env` and `timeout: None` reproduce
/// [`default_command_runner`]'s exact behavior.
#[derive(Debug, Clone, Copy)]
pub struct CommandSpec<'a> {
    pub program: &'a str,
    pub args: &'a [&'a str],
    pub cwd: &'a Path,
    /// Extra environment variables the child sees on top of whatever it
    /// would otherwise inherit from this process. Empty means "inherit
    /// only" — today's behavior. These never mutate this process's own
    /// environment; they are passed straight to `std::process::Command`.
    pub env: &'a [(&'a str, &'a str)],
    /// Hard wall-clock budget for the child. `None` waits forever (today's
    /// behavior). `Some(d)` kills and reaps the child if it has not exited
    /// within `d`, returning a [`CommandTimeout`] error rather than a
    /// success or an empty result.
    pub timeout: Option<Duration>,
}

/// The injectable command-runner signature for callers that need per-call
/// environment variables and/or a hard timeout — the superset seam new
/// subprocess callers (`typst`, `yt-dlp`, `uv run`, Playwright drivers,
/// ...) should reach for instead of a raw `std::process::Command`. Defaults
/// to the real subprocess via [`default_spec_runner`]; tests substitute a
/// stub exactly as they do for [`CommandRunner`].
pub type SpecCommandRunner =
    Arc<dyn Fn(&CommandSpec) -> std::io::Result<CommandOutput> + Send + Sync>;

/// Typed error returned when a [`SpecCommandRunner`] child exceeds its
/// [`CommandSpec::timeout`]. Carried as the source of an
/// `io::Error(ErrorKind::TimedOut, ..)` so the seam's return type stays
/// `std::io::Result<CommandOutput>` — downcast via
/// `err.get_ref().and_then(|e| e.downcast_ref::<CommandTimeout>())` to
/// recover the typed detail. Never a zero-status success and never a
/// silent empty result: whatever stdout/stderr the child had produced
/// before the kill is preserved here.
#[derive(Debug)]
pub struct CommandTimeout {
    pub program: String,
    pub elapsed: Duration,
    pub stdout: String,
    pub stderr: String,
    /// The killed child's pid, for diagnostics/tests that want to confirm
    /// the process is actually gone (not left as a zombie).
    pub pid: u32,
}

impl std::fmt::Display for CommandTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "command `{}` (pid {}) timed out after {:?}",
            self.program, self.pid, self.elapsed
        )
    }
}

impl std::error::Error for CommandTimeout {}

/// The default [`SpecCommandRunner`]: shells out to the real subprocess via
/// `std::process::Command` — gated by the non-overridable
/// [`command_floor::evaluate_command`] org-floor denylist, exactly as
/// [`default_command_runner`] used to do directly. This is now the ONLY
/// place in the crate that evaluates the floor and spawns a child; a
/// denied command never reaches `std::process::Command`.
///
/// Timeout semantics: the child's stdout/stderr are drained on background
/// threads (so a chatty child can't deadlock on a full pipe buffer while
/// this polls), and `try_wait` is polled against a deadline. On expiry the
/// child is `kill()`-ed and then `wait()`-ed to reap it — never left as a
/// zombie — and the call returns a [`CommandTimeout`] naming the program,
/// pid, elapsed duration, and whatever output had been captured so far.
#[must_use]
pub fn default_spec_runner() -> SpecCommandRunner {
    Arc::new(|spec: &CommandSpec| {
        let joined = std::iter::once(spec.program)
            .chain(spec.args.iter().copied())
            .collect::<Vec<_>>()
            .join(" ");
        if let CommandDecision::Deny { reason, matched } = command_floor::evaluate_command(&joined)
        {
            return Ok(CommandOutput {
                status: 126,
                stdout: String::new(),
                stderr: format!("command-policy: blocked ({reason}): {matched}"),
            });
        }

        let mut child = std::process::Command::new(spec.program)
            .args(spec.args)
            .current_dir(spec.cwd)
            .envs(spec.env.iter().map(|(k, v)| (*k, *v)))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        let pid = child.id();

        // Drain stdout/stderr concurrently so a child that writes more
        // than a pipe buffer's worth of output can't deadlock the poll
        // loop below (which never reads the pipes itself).
        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();
        let stdout_handle = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = stdout_pipe {
                let _ = std::io::Read::read_to_end(&mut pipe, &mut buf);
            }
            buf
        });
        let stderr_handle = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = stderr_pipe {
                let _ = std::io::Read::read_to_end(&mut pipe, &mut buf);
            }
            buf
        });

        let start = std::time::Instant::now();
        let exit_status = loop {
            if let Some(status) = child.try_wait()? {
                break Some(status);
            }
            if let Some(timeout) = spec.timeout {
                if start.elapsed() >= timeout {
                    break None;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        };

        let collect = |handle: std::thread::JoinHandle<Vec<u8>>| -> String {
            String::from_utf8_lossy(&handle.join().unwrap_or_default()).into_owned()
        };

        match exit_status {
            Some(status) => Ok(CommandOutput {
                status: status.code().unwrap_or(-1),
                stdout: collect(stdout_handle),
                stderr: collect(stderr_handle),
            }),
            None => {
                // Deadline hit: kill then wait() to reap — a kill() alone
                // leaves a zombie until someone waits on the pid.
                let _ = child.kill();
                let _ = child.wait();
                let elapsed = start.elapsed();
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    CommandTimeout {
                        program: spec.program.to_string(),
                        elapsed,
                        stdout: collect(stdout_handle),
                        stderr: collect(stderr_handle),
                        pid,
                    },
                ))
            }
        }
    })
}

/// Wraps a [`SpecCommandRunner`] into a plain [`CommandRunner`] that stamps
/// every call's [`CommandSpec::env`] with `FLEET_BUILD_PREADMITTED=1` —
/// appended to whatever env the call already carries, never replacing it.
///
/// This is the hand-off contract between an admitted heavy-work job
/// (`coord::heavy_work`, `EN.17.I`) and `scripts/fleet_build.py`: when a
/// Rust SDLC run's test/build stage has already been admitted through the
/// queue, its subprocess calls are routed through this runner so
/// `fleet_build.py` sees the env var and skips its own permit acquisition
/// (`FLEET_BUILD_PREADMITTED` passthrough, task 7) instead of taking a
/// second, redundant slot.
///
/// [`CommandRunner`]'s own signature carries no per-call env — the plain
/// `(program, args, cwd)` triple — so "whatever env the call already
/// carries" is empty on that seam today; this wrapper still builds the
/// `CommandSpec.env` slice by extending rather than overwriting, so a
/// future caller that does thread pre-existing env through keeps it.
#[must_use]
pub fn admitted_command_runner(inner: SpecCommandRunner) -> CommandRunner {
    Arc::new(move |program, args, cwd| {
        let env: Vec<(&str, &str)> = vec![("FLEET_BUILD_PREADMITTED", "1")];
        let spec = CommandSpec {
            program,
            args,
            cwd,
            env: &env,
            timeout: None,
        };
        inner(&spec)
    })
}

/// Stage **everything** in `worktree` (`git add -A`) and commit it with
/// `message`, routing a non-zero commit outcome through [`log_noop_commit`]
/// rather than treating it as a node failure (e.g. "nothing to commit" when
/// the tree is already clean).
///
/// Supersedes the former `commit_state_file`, which staged only the state
/// file's own path. That narrowness was the root cause behind four
/// independent SDLC_FLOW defects: nothing in the run ever committed the
/// implementer's CODE, so the consolidated review saw an empty diff, the
/// trivial-skip classifier always counted zero changed lines, every
/// auto-PR pushed a branch of state-file-only commits, and `PatchDocsNode`'s
/// doc edits (`docs.rs` makes no git calls at all) never reached the branch.
///
/// **The commit topology this establishes** — `HEAD` carries every completed
/// task's code; the working tree delta vs `HEAD` is exactly the current
/// task's in-progress work. `SaveStateNode` runs only on the pass path, so
/// one passed task is one commit; the retry path never reaches it, so
/// retries accumulate uncommitted and each attempt's review sees the
/// cumulative attempt via `git diff HEAD`.
///
/// Whether the state file rides along in that commit is repo-dependent: in
/// **this** repo `planning/` is a gitignored symlink into a brain vault
/// (`.gitignore` `/planning`), so `add -A` cannot stage
/// `planning/<slug>/sdlc/sdlc-flow-state.json` and every commit here carries
/// code only. In a repo that tracks `planning/`, the state file is included.
/// Do not read the doc comments elsewhere in this module as promising the
/// state file is committed in engine-rs — it is not, and was not before this
/// helper widened either.
///
/// **Why `add -A` and not an explicit file list:** `.gitignore` guards build
/// artifacts, and `TestTaskNode::changed_files` already treats untracked
/// paths as expected implementer output — an explicit list would silently
/// drop any file the agent created but did not "claim".
///
/// # Blast radius — this is tree-wide, and `use_worktree` defaults to FALSE
///
/// `SDLCFlowEventSchema::use_worktree` is `#[serde(default)]` **false**, and
/// on that path `SetupWorktreeNode` checks the run's branch out **in a live
/// checkout** (`worktree_path == "."`, or the registry-resolved repo root)
/// rather than under `trees/<branch>`. `add -A` there stages *every* dirty
/// path in that checkout, including edits the operator made and never
/// intended to hand to the run.
///
/// **Guarded since `ticket-setup-rs-closeout`:** on that path
/// `SetupWorktreeNode` now runs `git status --porcelain` first and aborts the
/// run — naming the dirty paths — when the tree is not clean, mirroring
/// `.claude/workflows/sdlc-flow.js`'s branch-mode guard. So `add -A` here can
/// only ever sweep a tree that was clean when the run started, plus whatever
/// the run itself produced. A `use_worktree: true` run is isolated and
/// unguarded by design.
///
/// Prefer `use_worktree: true` anyway for any run you do not want touching
/// the ambient tree at all.
///
/// Returns a [`CommitOutcome`] rather than a bare `bool`: a `false` used to
/// collapse the ordinary "nothing to commit" no-op into the same value as a
/// genuine git failure, so no caller could gate on the difference. A
/// [`CommitOutcome::Failed`] means `HEAD` did **not** advance while there was
/// real work to record — which silently breaks the topology invariant for the
/// next task (its `git diff HEAD` would then include this task's work too) —
/// and callers that record a unit of work as done must refuse to do so on it.
/// [`CommitOutcome::NoOp`] is the benign case and must stay benign.
///
/// The classification lives in [`is_noop_commit`], the single place that
/// decides which of the two a non-zero `git commit` exit is; never
/// re-implement its string matching at a call site.
pub(crate) fn commit_all(runner: &CommandRunner, worktree: &Path, message: &str) -> CommitOutcome {
    let _ = runner("git", &["add", "-A"], worktree);
    let commit = runner("git", &["commit", "-m", message], worktree);
    match &commit {
        Ok(output) if output.status == 0 => CommitOutcome::Committed,
        Ok(output) => {
            // "nothing to commit" or an equivalent no-op — logged, not
            // an error, mirroring `save_state_node.py`. A genuine failure
            // is logged too, and additionally handed back to the caller.
            log_noop_commit(message, output);
            if is_noop_commit(&output.stderr, &output.stdout) {
                CommitOutcome::NoOp
            } else {
                CommitOutcome::Failed {
                    detail: output.stderr.trim().to_string(),
                }
            }
        }
        Err(err) => CommitOutcome::Failed {
            detail: format!("git commit could not be run: {err}"),
        },
    }
}

/// The three distinguishable outcomes of a [`commit_all`] call.
///
/// Split out of the former `bool` because the two `false` cases mean opposite
/// things: `NoOp` is the routine "nothing to commit, working tree clean"
/// (every state commit in this repo, where `planning/` is a gitignored
/// symlink) and must not fail anything, while `Failed` is a real git error
/// whose work was never recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CommitOutcome {
    /// `git commit` exited 0 — `HEAD` advanced.
    Committed,
    /// `git commit` exited non-zero with a "nothing to commit" style message,
    /// per [`is_noop_commit`]. Benign.
    NoOp,
    /// `git commit` genuinely failed; `detail` carries the git stderr.
    Failed { detail: String },
}

impl CommitOutcome {
    /// `true` only when `HEAD` actually advanced.
    pub(crate) fn is_committed(&self) -> bool {
        matches!(self, CommitOutcome::Committed)
    }

    /// The git error text when this is a genuine failure, else `None` — the
    /// accessor callers gate on.
    pub(crate) fn failure_detail(&self) -> Option<&str> {
        match self {
            CommitOutcome::Failed { detail } => Some(detail.as_str()),
            _ => None,
        }
    }
}

/// Pure classifier for a non-zero `git commit` exit: `true` when the
/// stdout/stderr text describes the ordinary "nothing to commit" outcome
/// (re-saving an unchanged file), `false` for anything else (a genuine
/// failure). Split out as a small pure function — rather than folded into
/// [`log_noop_commit`] — so tests can assert on the classification directly
/// instead of capturing `tracing` output.
pub(crate) fn is_noop_commit(stderr: &str, stdout: &str) -> bool {
    let haystack = format!("{stdout}\n{stderr}").to_lowercase();
    haystack.contains("nothing to commit")
        || haystack.contains("working tree clean")
        || haystack.contains("no changes added to commit")
}

/// Logging hook for a non-zero `git commit` outcome from
/// [`commit_all`], distinguishing the ordinary no-op ("nothing to
/// commit, working tree clean") from a genuine failure — that distinction is
/// the entire point of this function; do not collapse it back to a single
/// branch.
///
/// **Why the quiet path matters in THIS repo specifically:** `planning/` is
/// a gitignored symlink (`.gitignore` line 7 `/planning`, `planning ->
/// ../_planning/engine-rs`), so every single state commit in this tree is a
/// no-op. A blanket warn on every non-zero exit would therefore fire on
/// every task of every run and train the reader to ignore it — hence the
/// no-op branch is silent by default and only prints when `ENGINE_DEBUG` is
/// set, while a genuine failure always prints (with the stderr text and the
/// state path) regardless.
///
/// Uses `tracing`'s `debug!`/`warn!` (EN.11.I migrated this off `eprintln!`;
/// the workspace now carries `tracing` as a workspace dependency).
/// `label` is a human-readable identifier for the commit that no-opped — the
/// commit message since the widening to [`commit_all`], the state file's path
/// before it. It exists only for this diagnostic; nothing parses it.
fn log_noop_commit(label: &str, output: &CommandOutput) {
    if is_noop_commit(&output.stderr, &output.stdout) {
        if std::env::var("ENGINE_DEBUG").is_ok() {
            tracing::debug!(
                label = %label,
                stderr = %output.stderr.trim(),
                "sdlc_flow: state commit no-op"
            );
        }
    } else {
        tracing::warn!(
            label = %label,
            stderr = %output.stderr.trim(),
            "sdlc_flow: state commit failed"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{
        commit_all, default_command_runner, default_spec_runner, is_noop_commit, strip_json_fence,
        CommandOutput, CommandRunner, CommandSpec, CommitOutcome,
    };
    use std::sync::Arc;

    /// A runner whose `git commit` returns the given exit code/stderr and
    /// whose every other invocation succeeds.
    fn commit_runner(status: i32, stderr: &'static str) -> CommandRunner {
        Arc::new(move |_program, args: &[&str], _cwd| {
            if args.first() == Some(&"commit") {
                Ok(CommandOutput {
                    status,
                    stdout: String::new(),
                    stderr: stderr.to_string(),
                })
            } else {
                Ok(CommandOutput {
                    status: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
        })
    }

    #[test]
    fn commit_all_reports_a_successful_commit_as_committed() {
        let outcome = commit_all(&commit_runner(0, ""), std::path::Path::new("."), "chore: x");
        assert_eq!(outcome, CommitOutcome::Committed);
        assert!(outcome.is_committed());
        assert_eq!(outcome.failure_detail(), None);
    }

    #[test]
    fn commit_all_reports_nothing_to_commit_as_a_noop_not_a_failure() {
        let outcome = commit_all(
            &commit_runner(1, "nothing to commit, working tree clean"),
            std::path::Path::new("."),
            "chore: x",
        );
        assert_eq!(outcome, CommitOutcome::NoOp);
        assert!(!outcome.is_committed());
        assert_eq!(
            outcome.failure_detail(),
            None,
            "a no-op must never present as a failure — that distinction is the point"
        );
    }

    #[test]
    fn commit_all_reports_a_genuine_git_error_as_a_failure_carrying_its_stderr() {
        let outcome = commit_all(
            &commit_runner(1, "fatal: unable to write new index file"),
            std::path::Path::new("."),
            "chore: x",
        );
        assert!(!outcome.is_committed());
        assert_eq!(
            outcome.failure_detail(),
            Some("fatal: unable to write new index file")
        );
    }

    #[test]
    fn bare_json_passes_through_unchanged_but_trimmed() {
        assert_eq!(strip_json_fence("  {\"a\": 1}  "), "{\"a\": 1}");
    }

    #[test]
    fn strips_fence_with_json_language_tag() {
        let text = "```json\n{\"a\": 1}\n```";
        assert_eq!(strip_json_fence(text), "{\"a\": 1}");
    }

    #[test]
    fn strips_bare_fence_with_no_language_tag() {
        let text = "```\n{\"a\": 1}\n```";
        assert_eq!(strip_json_fence(text), "{\"a\": 1}");
    }

    #[test]
    fn discards_trailing_prose_after_the_closing_fence() {
        let text = "```json\n{\"a\": 1}\n```\nDone!";
        assert_eq!(strip_json_fence(text), "{\"a\": 1}");
    }

    #[test]
    fn unclosed_fence_falls_back_to_the_whole_trimmed_text() {
        let text = "```json\n{\"a\": 1}";
        assert_eq!(strip_json_fence(text), text);
    }

    #[test]
    fn default_command_runner_blocks_a_denied_command_without_spawning() {
        let runner = default_command_runner();
        // A nonexistent cwd proves no real subprocess ran: if the deny path
        // fell through to `std::process::Command`, `current_dir` would fail
        // and this call would return an `Err`, not an `Ok` with status 126.
        let bogus_cwd = std::path::Path::new("/no/such/directory/for/this/test");
        let output = runner("git", &["push", "--force"], bogus_cwd)
            .expect("denied command must short-circuit before spawning, not error");
        assert_eq!(output.status, 126);
        assert!(
            output.stderr.contains("force push"),
            "stderr should name the deny reason: {}",
            output.stderr
        );
        assert!(
            output.stderr.contains("git push --force"),
            "stderr should include the matched text: {}",
            output.stderr
        );
        assert!(output.stdout.is_empty());
    }

    #[test]
    fn default_command_runner_allows_an_ordinary_command_unaffected() {
        let runner = default_command_runner();
        let tmp = std::env::temp_dir();
        let output = runner("echo", &["hi"], &tmp).expect("echo should run normally");
        assert_eq!(output.status, 0);
        assert!(output.stdout.contains("hi"));
        assert!(output.stderr.is_empty());
    }

    #[test]
    fn command_spec_with_empty_env_and_no_timeout_matches_default_command_runner() {
        let tmp = std::env::temp_dir();
        let spec_runner = default_spec_runner();
        let spec = CommandSpec {
            program: "echo",
            args: &["hi"],
            cwd: &tmp,
            env: &[],
            timeout: None,
        };
        let spec_output = spec_runner(&spec).expect("echo via CommandSpec should run normally");

        let plain_runner = default_command_runner();
        let plain_output = plain_runner("echo", &["hi"], &tmp)
            .expect("echo via CommandRunner should run normally");

        assert_eq!(spec_output.status, plain_output.status);
        assert_eq!(spec_output.stdout, plain_output.stdout);
        assert_eq!(spec_output.stderr, plain_output.stderr);
    }

    #[test]
    fn default_spec_runner_blocks_a_denied_command_without_spawning() {
        let runner = default_spec_runner();
        let bogus_cwd = std::path::Path::new("/no/such/directory/for/this/test");
        let spec = CommandSpec {
            program: "git",
            args: &["push", "--force"],
            cwd: bogus_cwd,
            env: &[],
            timeout: None,
        };
        let output =
            runner(&spec).expect("denied command must short-circuit before spawning, not error");
        assert_eq!(output.status, 126);
        assert!(
            output.stderr.contains("force push"),
            "stderr should name the deny reason: {}",
            output.stderr
        );
        assert!(
            output.stderr.contains("git push --force"),
            "stderr should include the matched text: {}",
            output.stderr
        );
        assert!(output.stdout.is_empty());
    }

    #[test]
    fn default_spec_runner_passes_env_to_the_child_without_mutating_the_parent() {
        let tmp = std::env::temp_dir();
        let runner = default_spec_runner();

        // The parent process must never see this var, before or after.
        assert!(std::env::var("ENGINE_RS_SPEC_ENV_TEST_VAR").is_err());

        let spec = CommandSpec {
            program: "sh",
            args: &["-c", "echo $ENGINE_RS_SPEC_ENV_TEST_VAR"],
            cwd: &tmp,
            env: &[("ENGINE_RS_SPEC_ENV_TEST_VAR", "spec-env-value")],
            timeout: None,
        };
        let output = runner(&spec).expect("sh -c echo should run normally");
        assert_eq!(output.status, 0);
        assert_eq!(output.stdout.trim(), "spec-env-value");

        assert!(
            std::env::var("ENGINE_RS_SPEC_ENV_TEST_VAR").is_err(),
            "CommandSpec::env must be scoped to the child, never the parent process"
        );
    }

    #[test]
    fn is_noop_commit_classifies_nothing_to_commit_as_a_noop() {
        assert!(is_noop_commit("nothing to commit, working tree clean", ""));
    }

    #[test]
    fn is_noop_commit_classifies_no_changes_added_as_a_noop() {
        assert!(is_noop_commit(
            "",
            "no changes added to commit (use \"git add\" and/or \"git commit -a\")"
        ));
    }

    #[test]
    fn is_noop_commit_classifies_working_tree_clean_as_a_noop_case_insensitively() {
        assert!(is_noop_commit("Working Tree Clean", ""));
    }

    #[test]
    fn is_noop_commit_classifies_a_genuine_failure_as_not_a_noop() {
        assert!(!is_noop_commit("fatal: unable to write new index file", ""));
    }

    #[test]
    fn is_noop_commit_classifies_unrelated_stderr_as_not_a_noop() {
        assert!(!is_noop_commit(
            "error: pathspec did not match any files",
            ""
        ));
    }
}

/// Named separately from `mod tests` (rather than nested inside it) so that
/// the fully-qualified test path is `workflows::admitted_command_runner_tests::…`
/// — a substring of `workflows::admitted_command_runner`, which is exactly the
/// `cargo nextest run` filter this task's own validation command uses. Mirrors
/// the `session_delta_tests` precedent above in this same file.
#[cfg(test)]
mod admitted_command_runner_tests {
    use super::{admitted_command_runner, CommandOutput, CommandSpec, SpecCommandRunner};
    use std::sync::{Arc, Mutex};

    #[test]
    fn admitted_command_runner_passes_fleet_build_preadmitted_env() {
        let captured: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_clone = Arc::clone(&captured);
        let stub: SpecCommandRunner = Arc::new(move |spec: &CommandSpec| {
            *captured_clone.lock().unwrap() = spec
                .env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });

        let runner = admitted_command_runner(stub);
        let tmp = std::env::temp_dir();
        let output = runner("echo", &["hi"], &tmp).expect("stub runner should not error");
        assert_eq!(output.status, 0);

        let seen = captured.lock().unwrap();
        assert!(
            seen.iter()
                .any(|(k, v)| k == "FLEET_BUILD_PREADMITTED" && v == "1"),
            "expected FLEET_BUILD_PREADMITTED=1 in captured env, got: {seen:?}"
        );
    }
}
