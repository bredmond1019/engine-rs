//! `FinalValidationNode` — the run-level, unconditional full-suite gate on
//! the task-loop drain branch (`EN.3.E`).
//!
//! `TestTaskNode` (`task_loop.rs`) is the per-task tripwire: cheap, honoring
//! `fastCommand` and excluding `"perTask": false` checks (`EN.3.D`). Without
//! a second, full-depth site, the authoritative `cargo nextest run
//! --workspace` and `cargo build --release` would never run at all in a
//! Rust `SDLC_FLOW` run — trading a cost bug for a correctness bug. This
//! node is that second site: it runs the worktree's `planning/harness.json`
//! suite at [`TestDepth::Full`] with an empty `task_validation_commands`
//! slice and `apply_per_task_filter = false`, exactly once per run, only on
//! `TaskQueueRouterNode`'s drain (no-pending) branch.
//!
//! See `planning/decisions/D12-per-task-vs-final-check-depth.md` for why
//! this is an unconditional node rather than a policy knob: whether the
//! authoritative suite runs at all is the run's correctness contract, not a
//! cost lever (CLAUDE.md standing rule 6). Accordingly this node reads no
//! `SdlcPolicy`, no `harness.json` `flow.testDepth`, and has no `Config` /
//! `ModelTransport` — it is a pure [`CommandRunner`] consumer, which is also
//! why `registry_for_policy` (`graph.rs`) carries no branch for it.

use std::path::Path;

use engine_contract::TaskContext;
use serde_json::json;

use crate::node::{Node, NodeError};

use super::policy::TestDepth;
use super::task_loop::{select_task_checks, worktree_path, CheckResult, TestTaskNode};
use super::{put_result, CommandRunner};

/// Deterministic node: runs the worktree's `planning/harness.json`
/// validation suite via the injectable [`CommandRunner`] seam, at full
/// depth and with no per-task filter, so tests can drive it without a real
/// subprocess.
///
/// On failure this node does **not** return `Err`: an `Err` halts the graph
/// walk and would strand the run without a terminal state, but the entire
/// point of this node is to *inform* `WrapUpNode`'s degraded terminal
/// report, not to short-circuit the run. A failing gate is recorded in the
/// stamped result and the node still returns `Ok`.
pub struct FinalValidationNode {
    runner: CommandRunner,
}

impl FinalValidationNode {
    #[must_use]
    pub fn new() -> Self {
        Self {
            runner: super::default_command_runner(),
        }
    }

    /// Override the command runner used for check invocations. Tests use
    /// this to stub the subprocess so the gate never shells out.
    #[must_use]
    pub fn with_runner(mut self, runner: CommandRunner) -> Self {
        self.runner = runner;
        self
    }
}

impl Default for FinalValidationNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for FinalValidationNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let worktree = worktree_path(&ctx)?;
        let worktree = Path::new(&worktree);

        let harness_path = worktree.join("planning").join("harness.json");
        let harness_checks: Vec<serde_json::Value> = if harness_path.exists() {
            let raw = std::fs::read_to_string(&harness_path).map_err(|err| {
                NodeError::new(format!("failed to read {}: {err}", harness_path.display()))
            })?;
            let harness: serde_json::Value = serde_json::from_str(&raw).map_err(|err| {
                NodeError::new(format!("failed to parse {}: {err}", harness_path.display()))
            })?;
            harness
                .get("validation")
                .and_then(|v| v.get("checks"))
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default()
        } else {
            // A missing harness.json yields empty results and
            // `all_passed = true`, exactly as `TestTaskNode` does when it
            // has a self-validating task's `validation_commands` to fall
            // back on. This node has no such fallback (it always passes an
            // empty `task_validation_commands` slice) so an absent harness
            // simply means there is nothing to gate on.
            Vec::new()
        };

        // `depth = TestDepth::Full`, empty `task_validation_commands`,
        // `apply_per_task_filter = false` — this is a run-level gate, not a
        // task's tripwire, so `"perTask": false` checks (e.g. `build`) run
        // too and no `fastCommand` substitution ever applies.
        let (selected_checks, _selection) =
            select_task_checks(&harness_checks, &[], TestDepth::Full, false);

        // Share `TestTaskNode::run_checks` — the check-kind dispatch, the
        // `enabled: false` skip, and the `gates` semantics — rather than
        // forking a second executor. This throwaway `TestTaskNode` is only
        // ever used as a handle onto that shared method; it is never
        // `process`ed as a node itself.
        let (check_results, failed_names): (Vec<CheckResult>, Vec<String>) = TestTaskNode::new()
            .with_runner(self.runner.clone())
            .run_checks(&selected_checks, worktree);

        let all_passed = failed_names.is_empty();
        let failure_summary = if all_passed {
            String::new()
        } else {
            format!("Failed checks: {}", failed_names.join(", "))
        };

        put_result(
            &mut ctx,
            "FinalValidationNode",
            json!({
                "all_passed": all_passed,
                "check_results": check_results,
                "failure_summary": failure_summary,
            }),
        );

        Ok(ctx)
    }

    fn name(&self) -> &str {
        "FinalValidationNode"
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use engine_contract::TaskContext;
    use serde_json::json;

    use super::*;
    use crate::workflows::sdlc_flow::{get_result, put_result, CommandOutput};

    fn ctx_with_worktree(worktree: &std::path::Path) -> TaskContext {
        let mut ctx = TaskContext {
            event: json!({}),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        put_result(
            &mut ctx,
            "SetupWorktreeNode",
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );
        ctx
    }

    fn write_harness(dir: &std::path::Path) {
        std::fs::create_dir_all(dir.join("planning")).unwrap();
        std::fs::write(
            dir.join("planning").join("harness.json"),
            serde_json::to_string(&json!({
                "validation": {
                    "checks": [
                        {
                            "name": "test",
                            "command": "cargo nextest run --workspace",
                            "fastCommand": "cargo nextest run --lib --workspace",
                            "gates": true,
                        },
                        {
                            "name": "build",
                            "command": "cargo build --release",
                            "gates": true,
                            "perTask": false,
                        },
                    ]
                }
            }))
            .unwrap(),
        )
        .unwrap();
    }

    /// A recording [`CommandRunner`] that always succeeds and records every
    /// `sh -c <command>` invocation's `<command>` string, in order.
    fn recording_command_runner() -> (CommandRunner, Arc<Mutex<Vec<String>>>) {
        let recorded: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded_clone = recorded.clone();
        let runner: CommandRunner = Arc::new(move |_program, args, _cwd| {
            recorded_clone.lock().unwrap().push(args.join(" "));
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });
        (runner, recorded)
    }

    fn failing_command_runner_for(failing: &'static str) -> CommandRunner {
        Arc::new(move |_program, args, _cwd| {
            let joined = args.join(" ");
            Ok(CommandOutput {
                status: if joined.contains(failing) { 1 } else { 0 },
                stdout: String::new(),
                stderr: String::new(),
            })
        })
    }

    #[tokio::test]
    async fn runs_full_command_not_fast_command_and_includes_per_task_false_build_check() {
        let dir = tempfile::tempdir().unwrap();
        write_harness(dir.path());
        let (runner, recorded) = recording_command_runner();

        let node = FinalValidationNode::new().with_runner(runner);
        let ctx = ctx_with_worktree(dir.path());
        let ctx = node.process(ctx).await.unwrap();

        let result = get_result(&ctx, "FinalValidationNode").unwrap();
        assert_eq!(result["all_passed"], json!(true));

        let recorded = recorded.lock().unwrap();
        let joined = recorded.join(" | ");
        assert!(
            joined.contains("cargo nextest run --workspace"),
            "expected the FULL command to run: {joined}"
        );
        assert!(
            !joined.contains("cargo nextest run --lib --workspace"),
            "fastCommand must NOT be invoked by the final gate: {joined}"
        );
        assert!(
            joined.contains("cargo build --release"),
            "perTask:false build check must be included: {joined}"
        );

        let check_names: Vec<&str> = result["check_results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap())
            .collect();
        assert!(check_names.contains(&"build"));
    }

    #[tokio::test]
    async fn failing_check_yields_all_passed_false_but_node_still_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        write_harness(dir.path());
        let runner = failing_command_runner_for("cargo build --release");

        let node = FinalValidationNode::new().with_runner(runner);
        let ctx = ctx_with_worktree(dir.path());
        let ctx = node
            .process(ctx)
            .await
            .expect("node must return Ok even on failure");

        let result = get_result(&ctx, "FinalValidationNode").unwrap();
        assert_eq!(result["all_passed"], json!(false));
        assert!(result["failure_summary"]
            .as_str()
            .unwrap()
            .contains("build"));
    }

    #[tokio::test]
    async fn missing_harness_yields_all_passed_true_with_empty_check_results() {
        let dir = tempfile::tempdir().unwrap();
        let (runner, _recorded) = recording_command_runner();

        let node = FinalValidationNode::new().with_runner(runner);
        let ctx = ctx_with_worktree(dir.path());
        let ctx = node.process(ctx).await.unwrap();

        let result = get_result(&ctx, "FinalValidationNode").unwrap();
        assert_eq!(result["all_passed"], json!(true));
        assert_eq!(result["check_results"].as_array().unwrap().len(), 0);
    }
}
