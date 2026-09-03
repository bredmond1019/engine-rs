//! Generic `mev emit-state --write` subprocess node, lifted from
//! `workflows::sdlc_flow::emit_state` (EN.4.0 task 2).
//!
//! `sdlc_flow` remains the sole owner of `CommandRunner`/`CommandOutput`
//! (subprocess execution) — this module accepts a runner via generic
//! injection instead of importing `sdlc_flow`'s concrete types, so it has
//! no dependency in that direction. A workflow that wants an
//! `EmitStateNode` supplies its own runner closure and an output type that
//! implements [`CommandOutputLike`] (`sdlc_flow`'s `CommandOutput` does, via
//! a thin impl added when `sdlc_flow` delegates to this module).

use std::path::Path;
use std::sync::Arc;

use engine_contract::TaskContext;
use serde_json::json;

use crate::node::{Node, NodeError};

/// The minimal shape an injected command's output must expose for
/// [`EmitStateNode`] to report on it — a generic stand-in for
/// `sdlc_flow::CommandOutput` so this module never imports `sdlc_flow`.
pub trait CommandOutputLike: Send + Sync {
    /// Process exit status (`0` == success).
    fn status(&self) -> i32;
    /// Captured stdout.
    fn stdout(&self) -> &str;
    /// Captured stderr.
    fn stderr(&self) -> &str;
}

/// Injectable command-runner seam: `(program, args, cwd) -> io::Result<O>`,
/// generic over the output type `O: CommandOutputLike`. Mirrors
/// `sdlc_flow::CommandRunner`'s shape without depending on it.
pub type Runner<O> = Arc<dyn Fn(&str, &[&str], &Path) -> std::io::Result<O> + Send + Sync>;

/// Resolve the worktree path to run `mev` in: `SetupWorktreeNode`'s output
/// if present, else `.` (e.g. a unit test driving this node in isolation).
fn worktree_path(ctx: &TaskContext) -> String {
    ctx.nodes
        .get("SetupWorktreeNode")
        .and_then(|value| value.get("worktree_path"))
        .and_then(|value| value.as_str())
        .unwrap_or(".")
        .to_string()
}

/// Deterministic node: runs `mev emit-state --write` in the worktree via an
/// injected [`Runner`]. Generic over the output type `O` the runner
/// produces so this module carries no `sdlc_flow` dependency.
pub struct EmitStateNode<O: CommandOutputLike> {
    runner: Runner<O>,
    /// Lane identity passed as `--agent <id>` so the node self-exempts an
    /// exclusive lease it owns, while a lease held by a DIFFERENT agent
    /// still refuses it (mev's `refuse_if_quiesced` gate,
    /// `EN.ticket.emit-state-node-must-self-exempt-its-own-lease`).
    /// `None` (the default) keeps the argv byte-identical to today —
    /// behavior-stable per standing rule 6.
    agent: Option<String>,
}

impl<O: CommandOutputLike> EmitStateNode<O> {
    /// Construct a node that runs `mev emit-state --write` via `runner`.
    /// There is no parameterless `new`/`Default` here — unlike
    /// `sdlc_flow::EmitStateNode`, this generic node has no built-in
    /// subprocess runner to default to (that lives in `sdlc_flow`).
    #[must_use]
    pub fn new(runner: Runner<O>) -> Self {
        Self {
            runner,
            agent: None,
        }
    }

    /// Override the command runner used for the `mev` invocation.
    #[must_use]
    pub fn with_runner(mut self, runner: Runner<O>) -> Self {
        self.runner = runner;
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

#[async_trait::async_trait]
impl<O: CommandOutputLike + 'static> Node for EmitStateNode<O> {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let cwd_string = worktree_path(&ctx);
        let cwd = Path::new(&cwd_string);

        let mut args: Vec<&str> = vec!["emit-state", "--write"];
        if let Some(agent) = self.agent.as_deref() {
            args.push("--agent");
            args.push(agent);
        }

        let output = (self.runner)("mev", &args, cwd)
            .map_err(|err| NodeError::new(format!("mev emit-state failed to spawn: {err}")))?;

        let emitted = output.status() == 0;
        let stdout_tail: String = output
            .stdout()
            .lines()
            .rev()
            .take(5)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");

        if !emitted {
            ctx.nodes.insert(
                "EmitStateNode".to_string(),
                json!({
                    "emitted": false,
                    "stdout_tail": stdout_tail,
                    "stderr": output.stderr(),
                }),
            );
            return Ok(ctx);
        }

        ctx.nodes.insert(
            "EmitStateNode".to_string(),
            json!({
                "emitted": true,
                "stdout_tail": stdout_tail,
            }),
        );

        Ok(ctx)
    }

    fn name(&self) -> &str {
        "EmitStateNode"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    struct TestOutput {
        status: i32,
        stdout: String,
        stderr: String,
    }

    impl CommandOutputLike for TestOutput {
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

    fn ctx_with(worktree_path: &str) -> TaskContext {
        let mut ctx = TaskContext {
            event: json!({ "spec_slug": "EN.4.0" }),
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
        let runner: Runner<TestOutput> = Arc::new(move |program, args, cwd| {
            calls_clone.lock().unwrap().push((
                program.to_string(),
                args.iter().map(|s| s.to_string()).collect(),
                cwd.to_string_lossy().into_owned(),
            ));
            Ok(TestOutput {
                status: 0,
                stdout: "state.json refreshed".to_string(),
                stderr: String::new(),
            })
        });

        let ctx = ctx_with("trees/sdlc/EN.4.0");
        let node = EmitStateNode::new(runner);
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
        assert_eq!(recorded[0].2, "trees/sdlc/EN.4.0");
    }

    #[tokio::test]
    async fn with_agent_appends_the_agent_flag_to_argv() {
        let calls: Arc<Mutex<Vec<RecordedCall>>> = Arc::new(Mutex::new(Vec::new()));
        let calls_clone = calls.clone();
        let runner: Runner<TestOutput> = Arc::new(move |program, args, cwd| {
            calls_clone.lock().unwrap().push((
                program.to_string(),
                args.iter().map(|s| s.to_string()).collect(),
                cwd.to_string_lossy().into_owned(),
            ));
            Ok(TestOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });

        let ctx = ctx_with("trees/sdlc/EN.4.0");
        let node = EmitStateNode::new(runner).with_agent("lane-engine-rs-d5");
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
        let runner: Runner<TestOutput> = Arc::new(move |program, args, cwd| {
            calls_clone.lock().unwrap().push((
                program.to_string(),
                args.iter().map(|s| s.to_string()).collect(),
                cwd.to_string_lossy().into_owned(),
            ));
            Ok(TestOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });

        let ctx = ctx_with("trees/sdlc/EN.4.0");
        let node = EmitStateNode::new(runner);
        node.process(ctx).await.expect("process should succeed");

        let recorded = calls.lock().unwrap();
        assert_eq!(recorded[0].1, vec!["emit-state", "--write"]);
    }

    #[tokio::test]
    async fn records_failure_without_erroring() {
        let runner: Runner<TestOutput> = Arc::new(|_program, _args, _cwd| {
            Ok(TestOutput {
                status: 1,
                stdout: String::new(),
                stderr: "mev: state.json invalid".to_string(),
            })
        });

        let ctx = ctx_with("trees/sdlc/EN.4.0");
        let node = EmitStateNode::new(runner);
        let out = node.process(ctx).await.expect("process should succeed");

        let result = &out.nodes["EmitStateNode"];
        assert_eq!(result["emitted"], json!(false));
        assert!(result["stderr"]
            .as_str()
            .unwrap()
            .contains("state.json invalid"));
    }

    #[tokio::test]
    async fn with_runner_overrides_the_configured_runner() {
        let first: Runner<TestOutput> = Arc::new(|_p, _a, _c| {
            Ok(TestOutput {
                status: 1,
                stdout: String::new(),
                stderr: String::new(),
            })
        });
        let second: Runner<TestOutput> = Arc::new(|_p, _a, _c| {
            Ok(TestOutput {
                status: 0,
                stdout: "ok".to_string(),
                stderr: String::new(),
            })
        });

        let ctx = ctx_with(".");
        let node = EmitStateNode::new(first).with_runner(second);
        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["EmitStateNode"]["emitted"], json!(true));
    }
}
