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
//! - `emit_state` — `EmitStateNode` (bottom-half, EN.3.B).
//! - `graph` — assembles the declared `WorkflowSchema` + `NodeRegistry` for
//!   the whole workflow.
//! - `aggregate` — the cross-run `(policy -> cost, time, quality)`
//!   aggregator (EN.3.C task 7): reads a set of `sdlc-flow-state.json`
//!   snapshots and tabulates one row per distinct resolved policy.
//!
//! The node-plumbing seams shared by every submodule — `CommandOutput` /
//! `CommandRunner` / `ModelTransport` / `default_command_runner` and the
//! `put_result` / `get_result` context helpers — are owned here (hoisted in
//! EN.3.B task 1 out of `setup.rs`/`task_loop.rs`, which had byte-identical
//! private copies) so every leaf module imports a single definition via
//! `super::...`.

use std::path::Path;
use std::sync::Arc;

use claude_code_rs::{Config, Outcome};
use engine_contract::TaskContext;
use futures::future::BoxFuture;

pub mod aggregate;
pub mod docs;
pub mod emit_state;
pub mod graph;
pub mod policy;
pub mod pr;
pub mod profiles;
pub mod schema;
pub mod setup;
pub mod task_loop;
pub mod wrap_up;

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
/// `ClaudeCodeStep::with_transport` (EN.2.A).
pub type CommandRunner =
    Arc<dyn Fn(&str, &[&str], &Path) -> std::io::Result<CommandOutput> + Send + Sync>;

/// The default [`CommandRunner`]: shells out to the real subprocess via
/// `std::process::Command`.
#[must_use]
pub fn default_command_runner() -> CommandRunner {
    Arc::new(|program, args, cwd| {
        let output = std::process::Command::new(program)
            .args(args)
            .current_dir(cwd)
            .output()?;
        Ok(CommandOutput {
            status: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    })
}

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
