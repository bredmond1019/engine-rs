//! `PullRequestNode` — deterministic subprocess tail node (bottom-half, EN.3.B).
//!
//! Ported from `orchestrator/app/workflows/sdlc_flow_workflow_nodes/pull_request_node.py`:
//! a deterministic `Node` (no model call) that pushes the task's git branch
//! and opens a PR for human review. Deliberately never auto-merges (human
//! review gate, D25) — merging is a separate, human-triggered action.
//!
//! Reads `worktree_path` / `branch_name` from `SetupWorktreeNode`'s output
//! and `auto_pr` / `spec_slug` from the inbound event. When `auto_pr` is
//! `false` this node is a no-op: it stamps `{pr_url: null, skipped: true,
//! branch_name}` and never touches git/gh. Otherwise it runs
//! `git push origin <branch>` then `gh pr create ...` via the hoisted
//! [`CommandRunner`] seam, surfacing a non-zero exit as a [`NodeError`].
//!
//! Both stamped shapes also publish `branch_name` — the branch this node
//! pushed (or would have pushed, on the `auto_pr: false` short-circuit) —
//! so the chain's merge stage (`orchestration/integrate.rs`, EN.11.C) can
//! resolve which branch to merge without re-deriving it from
//! `SetupWorktreeNode` itself. This node still deliberately never merges
//! (human review gate, D25); publishing the branch name is not merging.

use std::path::Path;

use engine_contract::TaskContext;
use serde::Deserialize;
use serde_json::json;

use crate::node::{Node, NodeError};

use super::{default_command_runner, get_result, put_result, CommandRunner};

/// The subset of the inbound `SDLC_FLOW` event this node needs.
#[derive(Debug, Deserialize)]
struct PrEvent {
    spec_slug: String,
    #[serde(default = "default_auto_pr")]
    auto_pr: bool,
}

fn default_auto_pr() -> bool {
    true
}

fn parse_event(ctx: &TaskContext) -> Result<PrEvent, NodeError> {
    serde_json::from_value(ctx.event.clone())
        .map_err(|err| NodeError::new(format!("invalid SDLC_FLOW event: {err}")))
}

/// The branch name `SetupWorktreeNode` stamped, if it has run — `None` when
/// it has not (rather than erroring), so the `auto_pr: false` short-circuit
/// can still stamp an explicit `null` for the chain's merge stage to read.
fn setup_branch_name(ctx: &TaskContext) -> Option<String> {
    get_result(ctx, "SetupWorktreeNode")?
        .get("branch_name")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// `SetupWorktreeNode`'s stamped `{worktree_path, branch_name, ...}` output.
fn setup_output(ctx: &TaskContext) -> Result<(String, String), NodeError> {
    let value = get_result(ctx, "SetupWorktreeNode")
        .ok_or_else(|| NodeError::new("PullRequestNode requires SetupWorktreeNode to have run"))?;
    let worktree_path = value
        .get("worktree_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| NodeError::new("SetupWorktreeNode output missing worktree_path"))?
        .to_string();
    let branch_name = value
        .get("branch_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| NodeError::new("SetupWorktreeNode output missing branch_name"))?
        .to_string();
    Ok((worktree_path, branch_name))
}

/// Deterministic node: pushes the branch and opens a PR (no auto-merge).
pub struct PullRequestNode {
    runner: CommandRunner,
}

impl PullRequestNode {
    #[must_use]
    pub fn new() -> Self {
        Self {
            runner: default_command_runner(),
        }
    }

    /// Override the command runner used for `git`/`gh` invocations. Tests
    /// use this to stub the subprocess so the gated suite never shells out.
    #[must_use]
    pub fn with_runner(mut self, runner: CommandRunner) -> Self {
        self.runner = runner;
        self
    }
}

impl Default for PullRequestNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for PullRequestNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let event = parse_event(&ctx)?;

        if !event.auto_pr {
            let branch_name = setup_branch_name(&ctx);
            put_result(
                &mut ctx,
                "PullRequestNode",
                json!({ "pr_url": null, "skipped": true, "branch_name": branch_name }),
            );
            return Ok(ctx);
        }

        let (worktree_path, branch_name) = setup_output(&ctx)?;
        let cwd = Path::new(&worktree_path);

        let push_output = (self.runner)("git", &["push", "origin", &branch_name], cwd)
            .map_err(|err| NodeError::new(format!("git push failed to spawn: {err}")))?;
        if push_output.status != 0 {
            return Err(NodeError::new(format!(
                "git push failed: {}",
                push_output.stderr
            )));
        }

        let title = format!("SDLC: {}", event.spec_slug);
        let pr_args = vec![
            "pr",
            "create",
            "--base",
            "main",
            "--head",
            branch_name.as_str(),
            "--title",
            title.as_str(),
            "--body",
            "Auto-generated PR — human review required.",
        ];
        let pr_output = (self.runner)("gh", &pr_args, cwd)
            .map_err(|err| NodeError::new(format!("gh pr create failed to spawn: {err}")))?;
        if pr_output.status != 0 {
            return Err(NodeError::new(format!(
                "gh pr create failed: {}",
                pr_output.stderr
            )));
        }

        let pr_url = pr_output.stdout.trim().to_string();

        put_result(
            &mut ctx,
            "PullRequestNode",
            json!({ "pr_url": pr_url, "skipped": false, "branch_name": branch_name }),
        );

        Ok(ctx)
    }

    fn name(&self) -> &str {
        "PullRequestNode"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::sdlc_flow::CommandOutput;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    fn ctx_with(event: serde_json::Value, worktree_path: &str, branch_name: &str) -> TaskContext {
        let mut ctx = TaskContext {
            event,
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({
                "worktree_path": worktree_path,
                "branch_name": branch_name,
            }),
        );
        ctx
    }

    #[tokio::test]
    async fn auto_pr_false_short_circuits_without_calling_runner() {
        let called = Arc::new(Mutex::new(false));
        let called_clone = called.clone();
        let runner: CommandRunner = Arc::new(move |_program, _args, _cwd| {
            *called_clone.lock().unwrap() = true;
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });

        let ctx = ctx_with(
            json!({ "spec_slug": "EN.3.B", "auto_pr": false }),
            "trees/sdlc/EN.3.B",
            "sdlc/EN.3.B",
        );
        let node = PullRequestNode::new().with_runner(runner);
        let out = node.process(ctx).await.expect("process should succeed");

        let result = &out.nodes["PullRequestNode"];
        assert!(result["pr_url"].is_null());
        assert_eq!(result["skipped"], json!(true));
        assert_eq!(result["branch_name"], json!("sdlc/EN.3.B"));
        assert!(!*called.lock().unwrap());
    }

    #[tokio::test]
    async fn auto_pr_false_stamps_null_branch_name_when_setup_has_not_run() {
        let runner: CommandRunner = Arc::new(|_program, _args, _cwd| {
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });

        let ctx = TaskContext {
            event: json!({ "spec_slug": "EN.3.B", "auto_pr": false }),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        let node = PullRequestNode::new().with_runner(runner);
        let out = node.process(ctx).await.expect("process should succeed");

        let result = &out.nodes["PullRequestNode"];
        assert!(result["pr_url"].is_null());
        assert_eq!(result["skipped"], json!(true));
        assert!(result["branch_name"].is_null());
    }

    #[tokio::test]
    async fn auto_pr_true_pushes_and_creates_pr() {
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let calls_clone = calls.clone();
        let runner: CommandRunner = Arc::new(move |program, args, _cwd| {
            calls_clone
                .lock()
                .unwrap()
                .push(format!("{program} {}", args.join(" ")));
            if program == "gh" {
                Ok(CommandOutput {
                    status: 0,
                    stdout: "https://github.com/example/repo/pull/42\n".to_string(),
                    stderr: String::new(),
                })
            } else {
                Ok(CommandOutput {
                    status: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
        });

        let ctx = ctx_with(
            json!({ "spec_slug": "EN.3.B", "auto_pr": true }),
            "trees/sdlc/EN.3.B",
            "sdlc/EN.3.B",
        );
        let node = PullRequestNode::new().with_runner(runner);
        let out = node.process(ctx).await.expect("process should succeed");

        let result = &out.nodes["PullRequestNode"];
        assert_eq!(
            result["pr_url"],
            json!("https://github.com/example/repo/pull/42")
        );
        assert_eq!(result["skipped"], json!(false));
        assert_eq!(result["branch_name"], json!("sdlc/EN.3.B"));

        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 2);
        assert!(recorded[0].starts_with("git push origin sdlc/EN.3.B"));
        assert!(recorded[1].starts_with("gh pr create"));

        // Never-auto-merge contract (D25): no call issues a merge.
        assert!(!recorded
            .iter()
            .any(|c| c.starts_with("gh pr merge") || c.contains("git merge")));
    }

    #[tokio::test]
    async fn nonzero_push_exit_yields_node_error() {
        let runner: CommandRunner = Arc::new(|program, _args, _cwd| {
            if program == "git" {
                Ok(CommandOutput {
                    status: 1,
                    stdout: String::new(),
                    stderr: "push rejected".to_string(),
                })
            } else {
                Ok(CommandOutput {
                    status: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
        });

        let ctx = ctx_with(
            json!({ "spec_slug": "EN.3.B", "auto_pr": true }),
            "trees/sdlc/EN.3.B",
            "sdlc/EN.3.B",
        );
        let node = PullRequestNode::new().with_runner(runner);
        let err = node
            .process(ctx)
            .await
            .expect_err("non-zero git push should error");
        assert!(err.message.contains("git push failed"));
    }

    #[tokio::test]
    async fn nonzero_gh_exit_yields_node_error() {
        let runner: CommandRunner = Arc::new(|program, _args, _cwd| {
            if program == "gh" {
                Ok(CommandOutput {
                    status: 1,
                    stdout: String::new(),
                    stderr: "pr create failed".to_string(),
                })
            } else {
                Ok(CommandOutput {
                    status: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
        });

        let ctx = ctx_with(
            json!({ "spec_slug": "EN.3.B", "auto_pr": true }),
            "trees/sdlc/EN.3.B",
            "sdlc/EN.3.B",
        );
        let node = PullRequestNode::new().with_runner(runner);
        let err = node
            .process(ctx)
            .await
            .expect_err("non-zero gh pr create should error");
        assert!(err.message.contains("gh pr create failed"));
    }
}
