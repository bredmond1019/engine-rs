//! `EmitStateNode` — deterministic `mev emit-state --write` subprocess node
//! (bottom-half, EN.3.B; added scope, no Python source).
//!
//! Runs `mev emit-state --write` in the worktree via the hoisted
//! [`CommandRunner`] seam so the flow refreshes the brain freshness spine /
//! `state.json` itself, matching `/log-work` semantics and removing the
//! downstream `/close-out` dependency (economics lever #1 prerequisite).
//! Deterministic — no model call.

use std::path::Path;

use engine_contract::TaskContext;
use serde_json::json;

use crate::node::{Node, NodeError};

use super::{default_command_runner, get_result, put_result, CommandRunner};

/// Resolve the worktree path to run `mev` in: `SetupWorktreeNode`'s output
/// if present, else `.` (e.g. a unit test driving this node in isolation).
fn worktree_path(ctx: &TaskContext) -> String {
    get_result(ctx, "SetupWorktreeNode")
        .and_then(|value| value.get("worktree_path"))
        .and_then(|value| value.as_str())
        .unwrap_or(".")
        .to_string()
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
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let cwd_string = worktree_path(&ctx);
        let cwd = Path::new(&cwd_string);

        let output = (self.runner)("mev", &["emit-state", "--write"], cwd)
            .map_err(|err| NodeError::new(format!("mev emit-state failed to spawn: {err}")))?;

        let emitted = output.status == 0;
        let stdout_tail: String = output
            .stdout
            .lines()
            .rev()
            .take(5)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");

        if !emitted {
            put_result(
                &mut ctx,
                "EmitStateNode",
                json!({
                    "emitted": false,
                    "stdout_tail": stdout_tail,
                    "stderr": output.stderr,
                }),
            );
            return Ok(ctx);
        }

        put_result(
            &mut ctx,
            "EmitStateNode",
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
}
