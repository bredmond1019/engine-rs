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
//! `default_command_runner` stay in `sdlc_flow` (only SDLC + `EN.4.D` need
//! subprocess access); `sdlc_flow` re-exports the hoisted seams so existing
//! `super::`/`sdlc_flow::` import sites resolve unchanged (EN.4.0 task 4).

use std::sync::Arc;

use claude_code_rs::{Config, Outcome};
use engine_contract::TaskContext;
use futures::future::BoxFuture;

pub mod content_pipeline;
pub mod diagnostic_intake;
pub mod proposal_generator;
pub mod research_agent;
pub mod sdlc_flow;

/// The injectable transport signature for model-calling nodes' composed
/// `ClaudeCodeStep`s — identical shape to `ClaudeCodeStep`'s own (private)
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

/// Prefer the pre-parsed `structured` value written by a `ClaudeCodeStep`
/// (stamped onto `ctx.nodes[node_name]["structured"]`) when present and
/// non-null; otherwise fall back to [`strip_json_fence`] +
/// `serde_json::from_str` on the raw text `content`. Factored out of the
/// byte-identical copies `ImplementTaskNode`/`TriageTaskNode`/
/// `ConsolidatedReviewNode` (`task_loop.rs`), `GenerateTasksNode`
/// (`setup.rs`), and `PatchDocsNode` (`docs.rs`) each carried privately
/// (EN.4.0 task 4).
pub(crate) fn parse_structured_or_fenced<T: serde::de::DeserializeOwned>(
    ctx: &TaskContext,
    node_name: &str,
    content: &str,
) -> Result<T, serde_json::Error> {
    let structured = get_result(ctx, node_name).and_then(|value| value.get("structured").cloned());
    match structured {
        Some(value) if !value.is_null() => serde_json::from_value(value),
        _ => serde_json::from_str(strip_json_fence(content)),
    }
}

#[cfg(test)]
mod tests {
    use super::strip_json_fence;

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
}
