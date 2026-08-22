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
//!
//! [`ValidationScope::Reconcile`] (`EN.11.N` task 7, port design T7) is the
//! second mode this node carries — SDLC_TASK's D56 terminal reconcile,
//! source-of-truth `base-template/.claude/workflows/sdlc-task.js`'s
//! `phase('Reconcile')` block. Where [`ValidationScope::Full`] is
//! SDLC_FLOW's unconditional run-level gate (every selected check, full
//! depth, no per-task filter), `Reconcile` narrows to only the checks the
//! per-task fast tripwire could have substituted or skipped
//! ([`select_reconcile_checks`]) and runs their authoritative form once,
//! after every task has already passed its own tripwire.
//!
//! This node deliberately does NOT implement the JS's `fullRun` guard (a
//! partial task-subset run never reconciles) — that determination belongs
//! to `LeanBookkeepNode` (`EN.11.N` task 8), which is the caller that knows
//! whether this run covers the whole spec. Implementing it here too would
//! give the guard two owners.

use std::path::Path;

use engine_contract::TaskContext;
use serde_json::json;

use crate::node::{Node, NodeError};

use super::policy::TestDepth;
use super::task_loop::{
    resolved_policy, select_task_checks, worktree_path, CheckResult, TestTaskNode,
};
use super::{put_result, CommandRunner};

/// Which depth [`FinalValidationNode`] runs at.
///
/// `Full` is the behavior-stable default (SDLC_FLOW's existing
/// unconditional run-level gate, unchanged by this enum's addition).
/// `Reconcile` is SDLC_TASK's D56 terminal reconcile — see the module doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ValidationScope {
    #[default]
    Full,
    Reconcile,
}

/// The D56 reconcile filter (port design T7), ported verbatim from
/// `sdlc-task.js`'s `phase('Reconcile')` block:
///
/// ```js
/// (harnessCfg?.validation?.checks ?? [])
///   .filter(c => c.gates && ((c.fastCommand && c.fastCommand !== c.command) || c.perTask === false))
/// ```
///
/// A check is kept iff it `gates` (JS truthiness: an absent `gates` field
/// is NOT gating, matching `c.gates &&` — this differs from
/// [`select_task_checks`]'s `unwrap_or(true)`, which is [`run_checks`]'s
/// own, separate default for a different call site) AND either:
///   - it declares a non-empty `fastCommand` that differs from its
///     `command` (or `command` is absent) — the per-task tripwire would
///     have substituted `fastCommand` in its place, so the real `command`
///     was never verified; or
///   - it is `perTask == false` — the per-task tripwire drops these
///     entirely (`sdlc-flow.js:548`), so they never ran at all.
///
/// Pure by design: no runner, no filesystem — unit-testable by constructing
/// `serde_json::Value` arrays directly.
pub(crate) fn select_reconcile_checks(checks: &[serde_json::Value]) -> Vec<serde_json::Value> {
    checks
        .iter()
        .filter(|check| {
            let gates = check
                .get("gates")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if !gates {
                return false;
            }

            let fast_command = check
                .get("fastCommand")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty());
            let command = check.get("command").and_then(|v| v.as_str());
            let fast_command_differs = match fast_command {
                Some(fast) => Some(fast) != command,
                None => false,
            };

            let per_task_false = check.get("perTask").and_then(|v| v.as_bool()) == Some(false);

            fast_command_differs || per_task_false
        })
        .cloned()
        .collect()
}

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
    scope: ValidationScope,
}

impl FinalValidationNode {
    #[must_use]
    pub fn new() -> Self {
        Self {
            runner: super::default_command_runner(),
            scope: ValidationScope::Full,
        }
    }

    /// Override the command runner used for check invocations. Tests use
    /// this to stub the subprocess so the gate never shells out.
    #[must_use]
    pub fn with_runner(mut self, runner: CommandRunner) -> Self {
        self.runner = runner;
        self
    }

    /// Select which depth this node runs at. Defaults to
    /// [`ValidationScope::Full`] — SDLC_FLOW's existing, unchanged
    /// behavior. SDLC_TASK's registry (`EN.11.N` task 9) constructs this
    /// with [`ValidationScope::Reconcile`] under the SAME registered
    /// identity `"FinalValidationNode"` (`TaskQueueRouterNode::route`'s
    /// hardcoded drain target), rather than adding a distinct node type.
    #[must_use]
    pub fn with_scope(mut self, scope: ValidationScope) -> Self {
        self.scope = scope;
        self
    }

    /// Run `checks` (already selected) via the shared [`TestTaskNode`]
    /// executor and stamp the result. `checks` is empty and `skipped` is
    /// `true` for either of `Reconcile`'s two skip conditions — in that
    /// case no `CommandRunner` call is made at all and `all_passed`
    /// defaults `true` (matching the JS: skipping the reconcile phase
    /// entirely leaves `reconcileFailed` at its initial `false`).
    ///
    /// Never returns `Err` — see the module doc's NEVER-`Err` property.
    fn run_and_stamp(
        &self,
        ctx: &mut TaskContext,
        checks: &[serde_json::Value],
        worktree: &Path,
        skipped: bool,
        skip_reason: String,
    ) {
        let (check_results, failed_names): (Vec<CheckResult>, Vec<String>) = if skipped {
            (Vec::new(), Vec::new())
        } else {
            // Share `TestTaskNode::run_checks` — the check-kind dispatch,
            // the `enabled: false` skip, and the `gates` semantics —
            // rather than forking a second executor. This throwaway
            // `TestTaskNode` is only ever used as a handle onto that
            // shared method; it is never `process`ed as a node itself.
            TestTaskNode::new()
                .with_runner(self.runner.clone())
                .run_checks(checks, worktree)
        };

        let all_passed = failed_names.is_empty();
        let failure_summary = if all_passed {
            String::new()
        } else {
            format!("Failed checks: {}", failed_names.join(", "))
        };

        put_result(
            ctx,
            "FinalValidationNode",
            json!({
                "all_passed": all_passed,
                "check_results": check_results,
                "failure_summary": failure_summary,
                "skipped": skipped,
                "skip_reason": skip_reason,
            }),
        );
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

        match self.scope {
            ValidationScope::Full => {
                // `depth = TestDepth::Full`, empty `task_validation_commands`,
                // `apply_per_task_filter = false` — this is a run-level gate,
                // not a task's tripwire, so `"perTask": false` checks (e.g.
                // `build`) run too and no `fastCommand` substitution ever
                // applies.
                let (selected_checks, _selection) =
                    select_task_checks(&harness_checks, &[], TestDepth::Full, false);
                self.run_and_stamp(&mut ctx, &selected_checks, worktree, false, String::new());
            }
            ValidationScope::Reconcile => {
                // Skip condition 1 (JS: `testDepth === 'fast'` guards
                // entry into the reconcile phase at all): under
                // `TestDepth::Full` every check already ran authoritative
                // on every per-task pass, via the SAME
                // `apply_per_task_filter = false` codepath the `Full`
                // scope above uses — reconciling again would be a pure
                // double-run. Stamp a passthrough result and make zero
                // `CommandRunner` calls.
                let policy = resolved_policy(&ctx)?;
                if policy.test_depth == TestDepth::Full {
                    self.run_and_stamp(
                        &mut ctx,
                        &[],
                        worktree,
                        true,
                        "test_depth=full: every check already ran authoritative on every \
                         per-task pass; reconciling again would be a pure double-run"
                            .to_string(),
                    );
                    return Ok(ctx);
                }

                let reconcile_checks = select_reconcile_checks(&harness_checks);

                // Skip condition 2 (JS: `if (reconcileChecks.length) { ... }
                // else { log('... skipped, zero added cost.') }`): no check
                // needed reconciling.
                if reconcile_checks.is_empty() {
                    self.run_and_stamp(
                        &mut ctx,
                        &[],
                        worktree,
                        true,
                        "no gating check needed reconciling (no fastCommand substitutions, no \
                         perTask:false checks) - skipped, zero added cost"
                            .to_string(),
                    );
                    return Ok(ctx);
                }

                // Run the reconcile checks' authoritative `command` — never
                // `fastCommand` — at full depth, no per-task filter.
                self.run_and_stamp(&mut ctx, &reconcile_checks, worktree, false, String::new());
            }
        }

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

    /// Seed a resolved [`super::super::policy::SdlcPolicy`] into `ctx`, as
    /// `SetupWorktreeNode`/dispatch would — required for
    /// [`ValidationScope::Reconcile`], which reads `resolved_policy` to
    /// check skip condition 1 (`test_depth == Full`).
    fn ctx_with_test_depth(mut ctx: TaskContext, depth: TestDepth) -> TaskContext {
        use crate::workflows::sdlc_flow::policy::SdlcPolicy;
        let policy = SdlcPolicy {
            test_depth: depth,
            ..SdlcPolicy::default()
        };
        put_result(
            &mut ctx,
            crate::policy::RESOLVED_POLICY_IDENTITY,
            serde_json::to_value(&policy).expect("SdlcPolicy serializes"),
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

    fn write_harness_no_reconcile_checks(dir: &std::path::Path) {
        std::fs::create_dir_all(dir.join("planning")).unwrap();
        std::fs::write(
            dir.join("planning").join("harness.json"),
            serde_json::to_string(&json!({
                "validation": {
                    "checks": [
                        {
                            "name": "lint",
                            "kind": "command",
                            "command": "cargo clippy",
                            "gates": true,
                        },
                    ]
                }
            }))
            .unwrap(),
        )
        .unwrap();
    }

    // ---- select_reconcile_checks (D56, port design T7) ----

    #[test]
    fn select_reconcile_checks_filter_table() {
        // (gates, fastCommand, command, perTask, expected_kept)
        let cases: Vec<(bool, Option<&str>, Option<&str>, Option<bool>, bool)> = vec![
            // gating + fastCommand differs from command -> keep
            (true, Some("fast"), Some("full"), None, true),
            // gating + fastCommand equals command -> drop (no gap to reconcile)
            (true, Some("same"), Some("same"), None, false),
            // gating + perTask:false, no fastCommand -> keep
            (true, None, Some("full"), Some(false), true),
            // gating + perTask:true (or absent), no fastCommand diff -> drop
            (true, None, Some("full"), None, false),
            (true, None, Some("full"), Some(true), false),
            // non-gating -> always drop, even with a fastCommand diff
            (false, Some("fast"), Some("full"), None, false),
            // non-gating perTask:false -> still drop (gates is the first gate)
            (false, None, Some("full"), Some(false), false),
            // gating, fastCommand set but command absent -> differs -> keep
            (true, Some("fast"), None, None, true),
            // gating, fastCommand differs AND perTask:false -> keep (either arm suffices)
            (true, Some("fast"), Some("full"), Some(false), true),
        ];

        for (i, (gates, fast, cmd, per_task, expected_kept)) in cases.into_iter().enumerate() {
            let mut check = json!({ "name": format!("check-{i}"), "gates": gates });
            if let Some(fast) = fast {
                check["fastCommand"] = json!(fast);
            }
            if let Some(cmd) = cmd {
                check["command"] = json!(cmd);
            }
            if let Some(per_task) = per_task {
                check["perTask"] = json!(per_task);
            }

            let selected = select_reconcile_checks(std::slice::from_ref(&check));
            assert_eq!(
                !selected.is_empty(),
                expected_kept,
                "case {i} ({check:?}): expected kept={expected_kept}"
            );
        }
    }

    // ---- ValidationScope::Full stays behavior-stable ----

    #[test]
    fn full_scope_selection_matches_select_task_checks_full_no_per_task_filter() {
        let checks = vec![
            json!({
                "name": "test",
                "command": "cargo nextest run --workspace",
                "fastCommand": "cargo nextest run --lib --workspace",
                "gates": true,
            }),
            json!({
                "name": "build",
                "command": "cargo build --release",
                "gates": true,
                "perTask": false,
            }),
        ];

        // This is exactly the call `ValidationScope::Full`'s branch of
        // `process` makes — pinning it here keeps the two call sites from
        // drifting apart.
        let (direct, _selection) = select_task_checks(&checks, &[], TestDepth::Full, false);
        assert_eq!(direct.len(), 2);
        assert_eq!(direct[0]["command"], json!("cargo nextest run --workspace"));
        assert_eq!(direct[1]["name"], json!("build"));
    }

    // ---- ValidationScope::Reconcile ----

    #[tokio::test]
    async fn reconcile_scope_runs_authoritative_command_never_fast_command() {
        let dir = tempfile::tempdir().unwrap();
        write_harness(dir.path());
        let (runner, recorded) = recording_command_runner();

        let node = FinalValidationNode::new()
            .with_scope(ValidationScope::Reconcile)
            .with_runner(runner);
        let ctx = ctx_with_worktree(dir.path());
        let ctx = ctx_with_test_depth(ctx, TestDepth::Fast);
        let ctx = node.process(ctx).await.unwrap();

        let result = get_result(&ctx, "FinalValidationNode").unwrap();
        assert_eq!(result["all_passed"], json!(true));
        assert_eq!(result["skipped"], json!(false));

        let recorded = recorded.lock().unwrap();
        let joined = recorded.join(" | ");
        assert!(
            joined.contains("cargo nextest run --workspace"),
            "expected the authoritative command to run: {joined}"
        );
        assert!(
            !joined.contains("cargo nextest run --lib --workspace"),
            "fastCommand must NEVER be invoked by the reconcile: {joined}"
        );
        assert!(
            joined.contains("cargo build --release"),
            "the perTask:false build check must be reconciled too: {joined}"
        );
    }

    #[tokio::test]
    async fn reconcile_scope_skips_with_zero_calls_when_test_depth_is_full() {
        let dir = tempfile::tempdir().unwrap();
        write_harness(dir.path());
        let (runner, recorded) = recording_command_runner();

        let node = FinalValidationNode::new()
            .with_scope(ValidationScope::Reconcile)
            .with_runner(runner);
        let ctx = ctx_with_worktree(dir.path());
        let ctx = ctx_with_test_depth(ctx, TestDepth::Full);
        let ctx = node.process(ctx).await.unwrap();

        let result = get_result(&ctx, "FinalValidationNode").unwrap();
        assert_eq!(result["all_passed"], json!(true));
        assert_eq!(result["skipped"], json!(true));
        assert!(result["skip_reason"]
            .as_str()
            .unwrap()
            .contains("test_depth=full"));
        assert!(
            recorded.lock().unwrap().is_empty(),
            "test_depth=full must make zero CommandRunner calls"
        );
    }

    #[tokio::test]
    async fn reconcile_scope_skips_with_zero_calls_when_no_check_needs_reconciling() {
        let dir = tempfile::tempdir().unwrap();
        write_harness_no_reconcile_checks(dir.path());
        let (runner, recorded) = recording_command_runner();

        let node = FinalValidationNode::new()
            .with_scope(ValidationScope::Reconcile)
            .with_runner(runner);
        let ctx = ctx_with_worktree(dir.path());
        let ctx = ctx_with_test_depth(ctx, TestDepth::Fast);
        let ctx = node.process(ctx).await.unwrap();

        let result = get_result(&ctx, "FinalValidationNode").unwrap();
        assert_eq!(result["all_passed"], json!(true));
        assert_eq!(result["skipped"], json!(true));
        assert!(result["skip_reason"]
            .as_str()
            .unwrap()
            .contains("no gating check needed reconciling"));
        assert!(
            recorded.lock().unwrap().is_empty(),
            "an empty reconcile selection must make zero CommandRunner calls"
        );
    }

    #[tokio::test]
    async fn failing_reconcile_returns_ok_with_all_passed_false() {
        let dir = tempfile::tempdir().unwrap();
        write_harness(dir.path());
        let runner = failing_command_runner_for("cargo build --release");

        let node = FinalValidationNode::new()
            .with_scope(ValidationScope::Reconcile)
            .with_runner(runner);
        let ctx = ctx_with_worktree(dir.path());
        let ctx = ctx_with_test_depth(ctx, TestDepth::Fast);
        let ctx = node
            .process(ctx)
            .await
            .expect("node must return Ok even on a failing reconcile");

        let result = get_result(&ctx, "FinalValidationNode").unwrap();
        assert_eq!(result["all_passed"], json!(false));
        assert!(result["failure_summary"]
            .as_str()
            .unwrap()
            .contains("build"));
    }
}
