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

use super::{
    commit_all, default_command_runner, get_result, CommandOutput, CommandRunner,
    DEFAULT_STATE_FILENAME,
};

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
/// graph runs `WrapUpNode -> CloseBlockNode -> PullRequestNode ->
/// EmitStateNode` (`graph.rs`), so `WrapUpNode` writes the file before the
/// PR exists, and `EmitStateNode` — which already runs last and already
/// holds a [`CommandRunner`] — is the correct patch site (see
/// `wrap_up.rs`'s `committed_pr` doc comment, which points here). Same
/// reasoning applies one node earlier to `CloseBlockNode`'s own result —
/// see [`patch_close_block_into_state`] below.
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
fn patch_pr_into_state(ctx: &TaskContext, runner: &CommandRunner, state_filename: &str) {
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
        .join(state_filename);
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

/// Patches the already-written `sdlc-flow-state.json`'s top-level
/// `"state_write_validated"`/`"state_write_rejected"` keys with
/// `CloseBlockNode`'s own outcome (`EN.ticket.wrap-up-closes-the-block`
/// task 5).
///
/// Same rationale as [`patch_pr_into_state`]: `CloseBlockNode`'s result is
/// a transient, per-write output that only exists after `WrapUpNode` has
/// already written the state file (the declared graph order is
/// `WrapUpNode -> CloseBlockNode -> PullRequestNode -> EmitStateNode`), so
/// there is no earlier point in the walk where a writer could stamp these
/// two booleans onto the committed JSON directly — this node, which
/// already runs last and already holds a [`CommandRunner`], is the correct
/// patch site.
///
/// **Never inferred.** These two booleans come straight off
/// `CloseBlockNode`'s own stamped outcome, not derived from `outcome`'s
/// label string or from whether the close "looks like" it succeeded:
/// `state_write_validated=false` alongside an otherwise-successful-looking
/// run is precisely the `UNVALIDATED` degrade this exists to make
/// distinguishable from a validated `CLOSED` write. Patches both keys
/// together so the JSON never carries a state where one was written and the
/// other wasn't.
///
/// A clean no-op (the file is left byte-for-byte untouched, and is never
/// even opened) when:
/// - `ctx` carries no `CloseBlockNode` result at all (e.g. a bare
///   `EmitStateNode` unit test, or a graph shape without that node);
/// - `ctx` has no `SetupWorktreeNode`/`spec_slug`, or the state file does
///   not exist at the derived path.
fn patch_close_block_into_state(ctx: &TaskContext, runner: &CommandRunner, state_filename: &str) {
    let Some(close_result) = get_result(ctx, "CloseBlockNode") else {
        return;
    };
    let validated = close_result
        .get("state_write_validated")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let rejected = close_result
        .get("state_write_rejected")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

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
        .join(state_filename);
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

    object.insert("state_write_validated".to_string(), json!(validated));
    object.insert("state_write_rejected".to_string(), json!(rejected));

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
    state_filename: &'static str,
    /// Lane identity passed as `--agent <id>` so the node self-exempts an
    /// exclusive lease it owns (`EN.ticket.emit-state-node-must-self-exempt-its-own-lease`).
    /// `None` (the default) keeps the argv byte-identical to today.
    agent: Option<String>,
}

impl EmitStateNode {
    #[must_use]
    pub fn new() -> Self {
        Self {
            runner: default_command_runner(),
            state_filename: DEFAULT_STATE_FILENAME,
            agent: None,
        }
    }

    /// Override the command runner used for the `mev` invocation. Tests use
    /// this to stub the subprocess so the gated suite never shells out.
    #[must_use]
    pub fn with_runner(mut self, runner: CommandRunner) -> Self {
        self.runner = runner;
        self
    }

    /// Override the state filename this node's patch helpers read/write.
    /// Defaults to [`DEFAULT_STATE_FILENAME`]; `EN.11.M` task 4 adds
    /// this so a second engine can reuse the node under its own filename
    /// without forking it.
    #[must_use]
    pub fn with_state_filename(mut self, filename: &'static str) -> Self {
        self.state_filename = filename;
        self
    }

    /// Set the lane identity to pass as `--agent <id>` so this node's own
    /// terminal emit self-exempts an exclusive lease the running chain
    /// holds on its own repo. Leaving this unset keeps the invocation
    /// exactly as it was before this knob existed.
    #[must_use]
    pub fn with_agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = Some(agent.into());
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
        let mut generic = GenericEmitStateNode::new(self.runner.clone());
        if let Some(agent) = self.agent.clone() {
            generic = generic.with_agent(agent);
        }
        let ctx = generic.process(ctx).await?;

        // EN.ticket.wrap-up-closes-the-block task 5: patch CloseBlockNode's
        // outcome into the already-committed state file's
        // `state_write_validated`/`state_write_rejected` keys, best-effort —
        // never fails the node, same rationale as the PR patch below.
        patch_close_block_into_state(&ctx, &self.runner, self.state_filename);

        // EN.3.G task 6: patch PullRequestNode's result into the already-
        // committed state file's `pr` block, best-effort — never fails the
        // node (the flow's terminal state is already written; this is
        // enrichment, not a required step).
        patch_pr_into_state(&ctx, &self.runner, self.state_filename);

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
    async fn with_agent_appends_the_agent_flag_to_argv() {
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
                stdout: String::new(),
                stderr: String::new(),
            })
        });

        let ctx = ctx_with("trees/sdlc/EN.3.B");
        let node = EmitStateNode::new()
            .with_runner(runner)
            .with_agent("lane-engine-rs-d5");
        node.process(ctx).await.expect("process should succeed");

        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(
            recorded[0].1,
            vec!["emit-state", "--write", "--agent", "lane-engine-rs-d5"]
        );
    }

    #[tokio::test]
    async fn no_agent_configured_leaves_argv_byte_identical_to_today() {
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
                stdout: String::new(),
                stderr: String::new(),
            })
        });

        let ctx = ctx_with("trees/sdlc/EN.3.B");
        let node = EmitStateNode::new().with_runner(runner);
        node.process(ctx).await.expect("process should succeed");

        let recorded = calls.lock().unwrap();
        assert_eq!(recorded[0].1, vec!["emit-state", "--write"]);
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
        patch_pr_into_state(&ctx, &noop_runner(), DEFAULT_STATE_FILENAME);

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
        patch_pr_into_state(&ctx, &noop_runner(), DEFAULT_STATE_FILENAME);

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
        patch_pr_into_state(&ctx, &noop_runner(), DEFAULT_STATE_FILENAME);

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
        patch_pr_into_state(&ctx, &noop_runner(), DEFAULT_STATE_FILENAME);

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

    // --- patch_close_block_into_state (EN.ticket.wrap-up-closes-the-block task 5) ---

    fn ctx_with_close_block(
        worktree_path: &str,
        spec_slug: &str,
        close_result: serde_json::Value,
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
        ctx.nodes.insert("CloseBlockNode".to_string(), close_result);
        ctx
    }

    #[tokio::test]
    async fn patches_a_validated_close_into_an_existing_state_file() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path();
        let spec_slug = "EN.3.G-terminal-path-robustness";
        let state_path = seed_state_file(worktree, spec_slug);
        let before = std::fs::read_to_string(&state_path).unwrap();
        let before_value: serde_json::Value = serde_json::from_str(&before).unwrap();

        let ctx = ctx_with_close_block(
            worktree.to_str().unwrap(),
            spec_slug,
            json!({
                "outcome": "CLOSED:engine-rs:EN.9",
                "state_write_validated": true,
                "state_write_rejected": false,
            }),
        );
        patch_close_block_into_state(&ctx, &noop_runner(), DEFAULT_STATE_FILENAME);

        let after = std::fs::read_to_string(&state_path).unwrap();
        let after_value: serde_json::Value = serde_json::from_str(&after).unwrap();

        assert_eq!(after_value["state_write_validated"], json!(true));
        assert_eq!(after_value["state_write_rejected"], json!(false));

        // Every other top-level key is byte-identical to before the patch.
        let mut before_minus = before_value.as_object().unwrap().clone();
        let mut after_minus = after_value.as_object().unwrap().clone();
        before_minus.remove("state_write_validated");
        before_minus.remove("state_write_rejected");
        after_minus.remove("state_write_validated");
        after_minus.remove("state_write_rejected");
        assert_eq!(before_minus, after_minus);
    }

    #[tokio::test]
    async fn patches_a_rejected_close_distinctly_from_unvalidated() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path();
        let spec_slug = "EN.3.G-terminal-path-robustness";
        seed_state_file(worktree, spec_slug);
        let state_path = worktree
            .join("planning")
            .join(spec_slug)
            .join("sdlc")
            .join("sdlc-flow-state.json");

        let ctx = ctx_with_close_block(
            worktree.to_str().unwrap(),
            spec_slug,
            json!({
                "outcome": "REJECTED:engine-rs:EN.9",
                "state_write_validated": false,
                "state_write_rejected": true,
            }),
        );
        patch_close_block_into_state(&ctx, &noop_runner(), DEFAULT_STATE_FILENAME);

        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
        assert_eq!(after["state_write_validated"], json!(false));
        assert_eq!(after["state_write_rejected"], json!(true));
    }

    #[tokio::test]
    async fn an_unvalidated_degrade_is_distinguishable_from_a_validated_close() {
        // `state_write_validated=false` alongside neither a rejection nor a
        // successful close is the UNVALIDATED degrade — both booleans false
        // is a distinct on-disk shape from `validated=true` (CLOSED) and
        // from `rejected=true` (REJECTED), never inferred from the outcome
        // label.
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path();
        let spec_slug = "EN.3.G-terminal-path-robustness";
        seed_state_file(worktree, spec_slug);
        let state_path = worktree
            .join("planning")
            .join(spec_slug)
            .join("sdlc")
            .join("sdlc-flow-state.json");

        let ctx = ctx_with_close_block(
            worktree.to_str().unwrap(),
            spec_slug,
            json!({
                "outcome": "UNVALIDATED:no brain.toml found",
                "state_write_validated": false,
                "state_write_rejected": false,
            }),
        );
        patch_close_block_into_state(&ctx, &noop_runner(), DEFAULT_STATE_FILENAME);

        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
        assert_eq!(after["state_write_validated"], json!(false));
        assert_eq!(after["state_write_rejected"], json!(false));
    }

    #[tokio::test]
    async fn no_close_block_node_result_is_a_no_op() {
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
        patch_close_block_into_state(&ctx, &noop_runner(), DEFAULT_STATE_FILENAME);

        let after = std::fs::read_to_string(&state_path).unwrap();
        assert_eq!(before, after);
    }
}
