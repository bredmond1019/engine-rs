//! `EmitStateNode` — deterministic `mev emit-state --write` subprocess node
//! (bottom-half, EN.3.B; added scope, no Python source).
//!
//! Runs `mev emit-state --write` in the worktree via the hoisted
//! [`CommandRunner`] seam so the flow refreshes the brain freshness spine /
//! `state.json` itself, matching `/log-work` semantics and removing the
//! downstream `/close-out` dependency (economics lever #1 prerequisite).
//! Deterministic — no model call.
//!
//! **EN.4.0:** the node body now delegates to the generic
//! `crate::policy::emit_state::EmitStateNode`, adapting `CommandOutput` to
//! its `CommandOutputLike` trait. The public API here (`EmitStateNode::new`/
//! `with_runner`, taking `sdlc_flow`'s `CommandRunner`) is unchanged so
//! `graph.rs`'s `EmitStateNode::new()` call site keeps compiling — the
//! generic node has no parameterless constructor of its own (it has no
//! built-in subprocess runner to default to), so this wrapper is the seam
//! that supplies one.

use std::path::Path;

use engine_contract::TaskContext;

use crate::node::{Node, NodeError};
use crate::policy::emit_state::{CommandOutputLike, EmitStateNode as GenericEmitStateNode};

use super::{commit_all, default_command_runner, get_result, CommandOutput, CommandRunner};

use serde_json::json;

impl CommandOutputLike for CommandOutput {
    fn status(&self) -> i32 {
        self.status
    }
    fn stdout(&self) -> &str {
        &self.stdout
    }
    fn stderr(&self) -> &str {
        &self.stderr
    }
}

/// Parse a PR number out of a `gh pr create` URL's trailing path segment
/// (e.g. `.../pull/42` -> `42`). `PullRequestNode` only emits `pr_url` —
/// `gh pr create` prints the URL and nothing else (`pr.rs:135-141`) — so
/// there is no first-class source for `number`; rather than spend a second
/// `gh` subprocess call on it, this falls back to `0` when the trailing
/// segment does not parse as `u64`. Documented in this spec's `tasks.md`
/// Notes (EN.3.G task 6).
fn parse_pr_number(pr_url: &str) -> u64 {
    pr_url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .and_then(|segment| segment.parse::<u64>().ok())
        .unwrap_or(0)
}

/// Read the worktree path stamped by `SetupWorktreeNode`, if present.
fn worktree_path(ctx: &TaskContext) -> Option<String> {
    get_result(ctx, "SetupWorktreeNode")
        .and_then(|value| value.get("worktree_path"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

/// Read `spec_slug` off the inbound `SDLC_FLOW` event.
fn spec_slug(ctx: &TaskContext) -> Option<String> {
    ctx.event
        .get("spec_slug")
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

/// Patch the already-written `sdlc-flow-state.json`'s top-level `"pr"` key
/// with `PullRequestNode`'s result (EN.3.G task 6).
///
/// This is the only site in the current graph that can see BOTH
/// `PullRequestNode`'s output and the on-disk committed state: the declared
/// graph runs `WrapUpNode -> PullRequestNode -> EmitStateNode`
/// (`graph.rs:165-176`), so `WrapUpNode` writes the file before the PR
/// exists, and `EmitStateNode` — which already runs last and already holds
/// a [`CommandRunner`] — is the correct patch site (see `wrap_up.rs`'s
/// `committed_pr` doc comment, which points here).
///
/// Patches the JSON object in place (parse to [`serde_json::Value`], set
/// the key, re-serialize) rather than round-tripping through `SDLCState`,
/// so no other D31 field can be lost or reordered by an incomplete
/// round-trip. Re-commits the patched file through the same
/// [`commit_all`] helper every other state write uses. This node runs
/// LAST (after `PullRequestNode`), so the widened staging has nothing left
/// to pick up beyond the state patch itself.
///
/// A clean no-op (the file is left byte-for-byte untouched, and is never
/// even opened) when:
/// - `ctx` carries no `PullRequestNode` result at all (e.g. a bare
///   `EmitStateNode` unit test, or a graph shape without that node);
/// - `PullRequestNode`'s result has `skipped: true` (an `auto_pr: false`
///   run — its `pr_url` is `null` and must stay `null` on disk);
/// - `ctx` has no `SetupWorktreeNode`/`spec_slug`, or the state file does
///   not exist at the derived path.
fn patch_pr_into_state(ctx: &TaskContext, runner: &CommandRunner) {
    let Some(pr_result) = get_result(ctx, "PullRequestNode") else {
        return;
    };
    if pr_result
        .get("skipped")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return;
    }
    let Some(pr_url) = pr_result.get("pr_url").and_then(|v| v.as_str()) else {
        return;
    };

    let Some(worktree) = worktree_path(ctx) else {
        return;
    };
    let Some(slug) = spec_slug(ctx) else {
        return;
    };
    let state_path = Path::new(&worktree)
        .join("planning")
        .join(&slug)
        .join("sdlc")
        .join("sdlc-flow-state.json");
    if !state_path.is_file() {
        return;
    }

    let Ok(content) = std::fs::read_to_string(&state_path) else {
        return;
    };
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&content) else {
        return;
    };
    let Some(object) = value.as_object_mut() else {
        return;
    };

    let number = parse_pr_number(pr_url);
    object.insert("pr".to_string(), json!({ "url": pr_url, "number": number }));

    let Ok(patched) = serde_json::to_string_pretty(&value) else {
        return;
    };
    if std::fs::write(&state_path, patched).is_err() {
        return;
    }
    let _ = commit_all(runner, Path::new(&worktree), "chore: flow state update");
}

/// Deterministic node: runs `mev emit-state --write` in the worktree.
pub struct EmitStateNode {
    runner: CommandRunner,
}

impl EmitStateNode {
    #[must_use]
    pub fn new() -> Self {
        Self {
            runner: default_command_runner(),
        }
    }

    /// Override the command runner used for the `mev` invocation. Tests use
    /// this to stub the subprocess so the gated suite never shells out.
    #[must_use]
    pub fn with_runner(mut self, runner: CommandRunner) -> Self {
        self.runner = runner;
        self
    }
}

impl Default for EmitStateNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for EmitStateNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        // `CommandRunner` (`sdlc_flow`) and `crate::policy::emit_state::Runner<CommandOutput>`
        // (the generic seam) are the same underlying `Arc<dyn Fn(...) -> io::Result<CommandOutput>>`
        // type, so `self.runner` is directly usable here.
        let ctx = GenericEmitStateNode::new(self.runner.clone())
            .process(ctx)
            .await?;

        // EN.3.G task 6: patch PullRequestNode's result into the already-
        // committed state file's `pr` block, best-effort — never fails the
        // node (the flow's terminal state is already written; this is
        // enrichment, not a required step).
        patch_pr_into_state(&ctx, &self.runner);

        Ok(ctx)
    }

    fn name(&self) -> &str {
        "EmitStateNode"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::sdlc_flow::CommandOutput;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    fn ctx_with(worktree_path: &str) -> TaskContext {
        let mut ctx = TaskContext {
            event: json!({ "spec_slug": "EN.3.B" }),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree_path }),
        );
        ctx
    }

    type RecordedCall = (String, Vec<String>, String);

    #[tokio::test]
    async fn invokes_mev_emit_state_write_in_the_worktree() {
        let calls: Arc<Mutex<Vec<RecordedCall>>> = Arc::new(Mutex::new(Vec::new()));
        let calls_clone = calls.clone();
        let runner: CommandRunner = Arc::new(move |program, args, cwd| {
            calls_clone.lock().unwrap().push((
                program.to_string(),
                args.iter().map(|s| s.to_string()).collect(),
                cwd.to_string_lossy().into_owned(),
            ));
            Ok(CommandOutput {
                status: 0,
                stdout: "state.json refreshed".to_string(),
                stderr: String::new(),
            })
        });

        let ctx = ctx_with("trees/sdlc/EN.3.B");
        let node = EmitStateNode::new().with_runner(runner);
        let out = node.process(ctx).await.expect("process should succeed");

        let result = &out.nodes["EmitStateNode"];
        assert_eq!(result["emitted"], json!(true));
        assert!(result["stdout_tail"]
            .as_str()
            .unwrap()
            .contains("state.json refreshed"));

        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, "mev");
        assert_eq!(recorded[0].1, vec!["emit-state", "--write"]);
        assert_eq!(recorded[0].2, "trees/sdlc/EN.3.B");
    }

    #[tokio::test]
    async fn records_failure_without_erroring() {
        let runner: CommandRunner = Arc::new(|_program, _args, _cwd| {
            Ok(CommandOutput {
                status: 1,
                stdout: String::new(),
                stderr: "mev: state.json invalid".to_string(),
            })
        });

        let ctx = ctx_with("trees/sdlc/EN.3.B");
        let node = EmitStateNode::new().with_runner(runner);
        let out = node.process(ctx).await.expect("process should succeed");

        let result = &out.nodes["EmitStateNode"];
        assert_eq!(result["emitted"], json!(false));
        assert!(result["stderr"]
            .as_str()
            .unwrap()
            .contains("state.json invalid"));
    }

    fn noop_runner() -> CommandRunner {
        Arc::new(|_program, _args, _cwd| {
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        })
    }

    fn seed_state_file(worktree: &std::path::Path, spec_slug: &str) -> std::path::PathBuf {
        let state_dir = worktree.join("planning").join(spec_slug).join("sdlc");
        std::fs::create_dir_all(&state_dir).unwrap();
        let state_path = state_dir.join("sdlc-flow-state.json");
        std::fs::write(
            &state_path,
            serde_json::to_string_pretty(&json!({
                "spec_slug": spec_slug,
                "status": "done",
                "pr": null,
                "tasks": {},
            }))
            .unwrap(),
        )
        .unwrap();
        state_path
    }

    fn ctx_with_pr(
        worktree_path: &str,
        spec_slug: &str,
        pr_result: serde_json::Value,
    ) -> TaskContext {
        let mut ctx = TaskContext {
            event: json!({ "spec_slug": spec_slug }),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree_path }),
        );
        ctx.nodes.insert("PullRequestNode".to_string(), pr_result);
        ctx
    }

    #[test]
    fn parse_pr_number_extracts_the_trailing_numeric_segment() {
        assert_eq!(parse_pr_number("https://github.com/o/r/pull/42"), 42);
    }

    #[test]
    fn parse_pr_number_falls_back_to_zero_when_unparsable() {
        assert_eq!(
            parse_pr_number("https://github.com/o/r/pull/not-a-number"),
            0
        );
        assert_eq!(parse_pr_number(""), 0);
    }

    #[tokio::test]
    async fn patches_the_pr_block_into_an_existing_state_file() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path();
        let spec_slug = "EN.3.G-terminal-path-robustness";
        let state_path = seed_state_file(worktree, spec_slug);
        let before = std::fs::read_to_string(&state_path).unwrap();
        let before_value: serde_json::Value = serde_json::from_str(&before).unwrap();

        let ctx = ctx_with_pr(
            worktree.to_str().unwrap(),
            spec_slug,
            json!({ "pr_url": "https://github.com/o/r/pull/42", "skipped": false }),
        );
        patch_pr_into_state(&ctx, &noop_runner());

        let after = std::fs::read_to_string(&state_path).unwrap();
        let after_value: serde_json::Value = serde_json::from_str(&after).unwrap();

        assert_eq!(
            after_value["pr"]["url"],
            json!("https://github.com/o/r/pull/42")
        );
        assert_eq!(after_value["pr"]["number"], json!(42));

        // Every other top-level key is byte-identical to before the patch.
        let mut before_minus_pr = before_value.as_object().unwrap().clone();
        let mut after_minus_pr = after_value.as_object().unwrap().clone();
        before_minus_pr.remove("pr");
        after_minus_pr.remove("pr");
        assert_eq!(before_minus_pr, after_minus_pr);
    }

    #[tokio::test]
    async fn skipped_pull_request_result_leaves_the_file_completely_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path();
        let spec_slug = "EN.3.G-terminal-path-robustness";
        let state_path = seed_state_file(worktree, spec_slug);
        let before = std::fs::read_to_string(&state_path).unwrap();

        let ctx = ctx_with_pr(
            worktree.to_str().unwrap(),
            spec_slug,
            json!({ "pr_url": null, "skipped": true }),
        );
        patch_pr_into_state(&ctx, &noop_runner());

        let after = std::fs::read_to_string(&state_path).unwrap();
        assert_eq!(before, after);
    }

    #[tokio::test]
    async fn no_pull_request_node_result_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path();
        let spec_slug = "EN.3.G-terminal-path-robustness";
        let state_path = seed_state_file(worktree, spec_slug);
        let before = std::fs::read_to_string(&state_path).unwrap();

        let mut ctx = TaskContext {
            event: json!({ "spec_slug": spec_slug }),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_str().unwrap() }),
        );
        patch_pr_into_state(&ctx, &noop_runner());

        let after = std::fs::read_to_string(&state_path).unwrap();
        assert_eq!(before, after);
    }

    #[tokio::test]
    async fn a_pr_url_with_no_numeric_trailing_segment_yields_number_zero() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path();
        let spec_slug = "EN.3.G-terminal-path-robustness";
        seed_state_file(worktree, spec_slug);

        let ctx = ctx_with_pr(
            worktree.to_str().unwrap(),
            spec_slug,
            json!({ "pr_url": "https://github.com/o/r/pull/not-a-number", "skipped": false }),
        );
        patch_pr_into_state(&ctx, &noop_runner());

        let state_path = worktree
            .join("planning")
            .join(spec_slug)
            .join("sdlc")
            .join("sdlc-flow-state.json");
        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
        assert_eq!(
            after["pr"]["url"],
            json!("https://github.com/o/r/pull/not-a-number")
        );
        assert_eq!(after["pr"]["number"], json!(0));
    }
}
