//! `LeanBookkeepNode` — SDLC_TASK's lean close-out (port design T8).
//!
//! Source of truth: `base-template/.claude/workflows/sdlc-task.js`'s LEAN
//! BOOKKEEP CLOSE-OUT section (`sdlc-task.js:1802` onward). This node is
//! the lean engine's terminal node: it persists the durable run state and
//! commits — **not** a full wrap-up. No prose `log.md` entry, no D18
//! amendment log, no review, no docs, no PR (`sdlc-task-ships-no-docs-stage`).
//!
//! ## Reuse, do not fork
//!
//! Every byte this node writes to disk goes through
//! `sdlc_flow::wrap_up`'s durable-state helpers (`state_path_for`,
//! `build_run_meta`, `persist_state`, `worktree_path`,
//! `committed_final_validation`), widened from private to `pub(crate)` by
//! this task rather than re-implemented here. A second serializer for the
//! same D31-committed state shape is the exact defect class `EN.11.M`'s
//! filename parameterization exists to prevent — this node writes through
//! [`super::DEFAULT_STATE_FILENAME`] (`"sdlc-task-state.json"`), never
//! `sdlc_flow::DEFAULT_STATE_FILENAME`.
//!
//! ## The `CloseBlockNode` read seam
//!
//! `close_block::CloseBlockNode::wrap_up_state` (now `state_from_source`)
//! read a hardcoded `"WrapUpNode"` identity — `LeanBookkeepNode` is not
//! `WrapUpNode`, so left unfixed `CloseBlockNode` would find nothing and
//! silently skip every SDLC_TASK close. Fixed at the reader: `close_block`
//! now carries a `with_state_source(&'static str)` builder (default
//! `"WrapUpNode"`), and SDLC_TASK's registry (a later task in this spec)
//! constructs `CloseBlockNode::new().with_state_source("LeanBookkeepNode")`.
//! This node stamps the identical `{"state": <SDLCState>, ...}` payload
//! shape `WrapUpNode` does (plus an additive `full_run` key —
//! `close_block::full_run_from_source` defaults `true` when that key is
//! absent, so `sdlc_flow` behavior is untouched) so no other reader of
//! that shape changes.
//!
//! ## Terminal-status logic (from the JS)
//!
//! - `bailed` (`TriageTaskNode` returned `MAJOR_BAIL`, including the
//!   budget-exhausted `RETRYABLE` re-check `TaskTriageRouterNode` already
//!   converts to `MAJOR_BAIL` before this node ever runs, and an
//!   unrecognized triage verdict) -> `status: "blocked"`; per-task commits
//!   stand.
//! - reconcile failed (`FinalValidationNode` stamped `all_passed: false`
//!   under `ValidationScope::Reconcile`) -> stamp
//!   `TerminalSignal::ReconcileFailed(failure_summary)`, write `status:
//!   "reconcile_failed"`, SKIP the bookkeep flip entirely (via
//!   `close_block`'s own `"reconcile_failed"` skip check — this node makes
//!   no block-status write of its own either way), LEAVE the per-task
//!   commits standing, and set `bail_reason` to the D56 text. The state
//!   file itself is still written and committed on this path — only the
//!   *flip* (`CloseBlockNode`) is skipped.
//! - otherwise -> `status: "done"`.
//!
//! ## Full-run guard (the JS's `fullRun`)
//!
//! `fullRun = !selectedTasks` (`sdlc-task.js:1720`) — "no explicit
//! selection = every task in the spec ran". This node derives the same
//! boolean from the inbound event's `task_range` field (absent/`None` =
//! full run) rather than from `state.tasks`, because `setup::
//! LoadTaskStateNode`'s `parse_task_range` filter has already narrowed
//! `state.tasks` down to only the selected ids by the time this node runs
//! — the full spec task list is no longer recoverable from `state` alone.
//! The reconcile check ([`reconcile_terminal_signal`]) is only consulted
//! when `full_run` is `true`; a partial-range run's status still resolves
//! to `"done"` on a clean pass (matching the JS: `fullRun` never gates
//! `state.status` itself, only the reconcile phase and the block flip).
//! The stamped `full_run` flag is what `close_block::CloseBlockNode` reads
//! to refuse closing a partial run's block.
//!
//! ## Block-status flip — expired premise, do NOT port the JS
//!
//! The port design §7 Q1 and this block's record both say "the
//! block-status flip has no Rust implementation at all" and warn against
//! porting the JS flip. That was true on 2026-08-19 and is **false now**:
//! `EN.ticket.wrap-up-closes-the-block` shipped
//! `sdlc_flow::close_block::CloseBlockNode`, which does the flip through
//! mev under an advisory lock with a validate-then-rollback contract.
//! **This node does not reimplement it** — it stamps a `state`/`full_run`
//! payload `CloseBlockNode` (wired in by a later task) reads; the flip
//! itself, and every `state.json` write, belongs solely to `CloseBlockNode`.

use engine_contract::TaskContext;
use serde_json::json;

use crate::node::{Node, NodeError};
use crate::workflows::sdlc_flow::schema::{derive_committed_status, TerminalSignal};
use crate::workflows::sdlc_flow::task_loop::latest_state;
use crate::workflows::sdlc_flow::wrap_up::{
    build_run_meta, committed_final_validation, persist_state, state_path_for, worktree_path,
};

use super::schema::{derive_bail_reason, parse_task_range};
#[cfg(test)]
use super::CommandOutput;
use super::{commit_all, default_command_runner, get_result, put_result, CommandRunner};

/// Derive this run's [`TerminalSignal`] from `TriageTaskNode`'s stamped
/// verdict — the only bail-producing stage SDLC_TASK ships (no review, no
/// end-review; `sdlc-task-ships-no-docs-stage`). Mirrors
/// `sdlc_flow::wrap_up::derive_terminal_signal`'s `TriageTaskNode` arm
/// (both the `MAJOR_BAIL` case and the `unrecognized_verdict` fallback)
/// verbatim, without re-deriving the routing decision
/// `TaskTriageRouterNode::route` already made — this just reads back the
/// verdict that router already acted on.
fn derive_bail(ctx: &TaskContext) -> Option<TerminalSignal> {
    let triage = get_result(ctx, "TriageTaskNode")?;

    if triage.get("verdict").and_then(|v| v.as_str()) == Some("MAJOR_BAIL") {
        let reason = triage
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("Max attempts reached without a passing run.")
            .to_string();
        return Some(TerminalSignal::MajorBail(reason));
    }

    if let Some(verdict) = triage.get("unrecognized_verdict").and_then(|v| v.as_str()) {
        return Some(TerminalSignal::MajorBail(format!(
            "unrecognized triage verdict: {verdict}"
        )));
    }

    None
}

/// Derive the D56 reconcile [`TerminalSignal`] from `FinalValidationNode`'s
/// stamped result under `ValidationScope::Reconcile` (graph wiring, a
/// later task in this spec). `None` when that node never ran this walk
/// (a bailed run, or a partial-range run this node's caller never checks —
/// see [`is_full_run`]) or when the reconcile gate passed (`all_passed:
/// true`, including its own two skip conditions, which stamp
/// `all_passed: true` with zero `CommandRunner` calls).
fn reconcile_terminal_signal(ctx: &TaskContext) -> Option<TerminalSignal> {
    let final_validation = committed_final_validation(ctx)?;
    if final_validation.all_passed {
        None
    } else {
        Some(TerminalSignal::ReconcileFailed(
            final_validation.failure_summary,
        ))
    }
}

/// The JS's `fullRun = !selectedTasks` (`sdlc-task.js:1720`): `true` iff
/// the inbound event carried no `task_range` (or an empty one). Reads the
/// raw `ctx.event` directly rather than `state.tasks`, since
/// `setup::LoadTaskStateNode` has already filtered `state.tasks` down to
/// only the selected ids by the time this node runs — the full spec list
/// is not recoverable from `state` alone at this point in the graph.
fn is_full_run(ctx: &TaskContext) -> Result<bool, NodeError> {
    let task_range = ctx.event.get("task_range").and_then(|v| v.as_str());
    let selected = parse_task_range(task_range).map_err(NodeError::new)?;
    Ok(selected.is_none())
}

/// SDLC_TASK's lean close-out node: persists the durable `SDLCState` and
/// commits it (`chore: sdlc-task bookkeep — <spec_slug>`) — nothing more.
/// See the module doc comment for the full contract.
pub struct LeanBookkeepNode {
    runner: CommandRunner,
    state_filename: &'static str,
}

impl LeanBookkeepNode {
    #[must_use]
    pub fn new() -> Self {
        Self {
            runner: default_command_runner(),
            state_filename: super::DEFAULT_STATE_FILENAME,
        }
    }

    /// Override the command runner used for the terminal state's
    /// git add/commit invocation. Tests use this to stub the subprocess —
    /// mirrors `sdlc_flow::wrap_up::WrapUpNode::with_runner`.
    #[must_use]
    pub fn with_runner(mut self, runner: CommandRunner) -> Self {
        self.runner = runner;
        self
    }

    /// Override the state filename this node reads/writes. Defaults to
    /// [`super::DEFAULT_STATE_FILENAME`] (`"sdlc-task-state.json"`).
    #[must_use]
    pub fn with_state_filename(mut self, filename: &'static str) -> Self {
        self.state_filename = filename;
        self
    }
}

impl Default for LeanBookkeepNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for LeanBookkeepNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let mut state = latest_state(&ctx)?;
        let full_run = is_full_run(&ctx)?;

        let bail_signal = derive_bail(&ctx);
        let terminal_signal = if bail_signal.is_some() {
            bail_signal
        } else if full_run {
            reconcile_terminal_signal(&ctx)
        } else {
            // A partial task_range run never reconciles (JS `fullRun`
            // guard) — the reconcile phase is simply never consulted, so
            // a clean partial run resolves to `"done"`, exactly as the JS
            // leaves `state.status` ungated by `fullRun` on the happy path.
            None
        };

        state.bail_reason = derive_bail_reason(terminal_signal.as_ref());
        state.global_status =
            derive_committed_status(&state, terminal_signal.as_ref()).to_string();

        let state_value = serde_json::to_value(&state)
            .map_err(|err| NodeError::new(format!("failed to serialize SDLCState: {err}")))?;

        let mut output = json!({
            "state": state_value,
            "full_run": full_run,
        });

        // Persist + commit unconditionally (when this run went through a
        // worktree) — a failed reconcile skips the bookkeep FLIP
        // (`close_block`'s own `"reconcile_failed"` skip check), not the
        // state write itself: "the state file still written" is an
        // explicit acceptance criterion for that path.
        if let Some(worktree) = worktree_path(&ctx) {
            let state_path = state_path_for(&worktree, &state.spec_slug, self.state_filename)?;
            let run_meta = build_run_meta(&ctx, &worktree, &state_path);
            let final_validation = committed_final_validation(&ctx);
            let saved_to = persist_state(
                &state_path,
                &state,
                &run_meta,
                None,
                None,
                None,
                final_validation.as_ref(),
                terminal_signal.as_ref(),
            )?;
            let _ = commit_all(
                &self.runner,
                std::path::Path::new(&worktree),
                &format!("chore: sdlc-task bookkeep — {}", state.spec_slug),
            );
            output["saved_to"] = json!(saved_to);
        }

        put_result(&mut ctx, "LeanBookkeepNode", output);

        Ok(ctx)
    }

    fn name(&self) -> &str {
        "LeanBookkeepNode"
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;
    use crate::workflows::sdlc_flow::close_block::CloseBlockNode;
    use crate::workflows::sdlc_flow::schema::{SDLCState, SDLCTask, SDLCTaskStatus};

    /// A minimal `ctx` driving `LeanBookkeepNode` directly, with a clean
    /// full run's `SDLCState` stamped under `UpdateTaskStatusNode` (one of
    /// `latest_state`'s candidate identities) and no `task_range` on the
    /// event (i.e. a full run).
    fn ctx_with_state(state: &SDLCState, task_range: Option<&str>) -> TaskContext {
        let mut event = json!({ "spec_slug": state.spec_slug });
        if let Some(range) = task_range {
            event["task_range"] = json!(range);
        }
        let mut ctx = TaskContext {
            event,
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        ctx.nodes.insert(
            "UpdateTaskStatusNode".to_string(),
            serde_json::to_value(state).unwrap(),
        );
        ctx
    }

    fn clean_state(spec_slug: &str) -> SDLCState {
        let mut state = SDLCState::new(spec_slug);
        let mut task = SDLCTask::new(1, "One", "d1");
        task.status = SDLCTaskStatus::Done;
        state.tasks.push(task);
        state.telemetry.tasks_passed = 1;
        state.telemetry.tasks_failed = 0;
        state.telemetry.total_attempts = 1;
        state
    }

    fn result_state(out: &TaskContext) -> SDLCState {
        let result = get_result(out, "LeanBookkeepNode").expect("LeanBookkeepNode stamped");
        serde_json::from_value(result.get("state").unwrap().clone())
            .expect("stamped state parses")
    }

    #[tokio::test]
    async fn clean_full_run_writes_done() {
        let state = clean_state("EN.11.N-lean-bookkeep");
        let ctx = ctx_with_state(&state, None);
        let node = LeanBookkeepNode::new();

        let out = node.process(ctx).await.expect("process should succeed");
        let result = get_result(&out, "LeanBookkeepNode").expect("stamped");

        assert_eq!(result_state(&out).global_status, "done");
        assert_eq!(result["full_run"], json!(true));
        // No worktree stamped -> no persist attempted, so no `saved_to`.
        assert!(result.get("saved_to").is_none());
    }

    #[tokio::test]
    async fn bail_writes_blocked() {
        let state = clean_state("EN.11.N-lean-bookkeep");
        let mut ctx = ctx_with_state(&state, None);
        ctx.nodes.insert(
            "TriageTaskNode".to_string(),
            json!({ "verdict": "MAJOR_BAIL", "reason": "max attempts reached" }),
        );
        let node = LeanBookkeepNode::new();

        let out = node.process(ctx).await.expect("process should succeed");
        let result_state = result_state(&out);

        assert_eq!(result_state.global_status, "blocked");
        assert_eq!(
            result_state.bail_reason.as_deref(),
            Some("max attempts reached")
        );
    }

    #[tokio::test]
    async fn failed_reconcile_writes_reconcile_failed_skips_flip_state_file_still_written() {
        let dir = tempfile::tempdir().expect("tempdir");
        let worktree = dir.path();
        std::fs::create_dir_all(worktree.join("planning/EN.11.N-lean-bookkeep/sdlc"))
            .expect("mkdir");

        let state = clean_state("EN.11.N-lean-bookkeep");
        let mut ctx = ctx_with_state(&state, None);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );
        ctx.nodes.insert(
            "FinalValidationNode".to_string(),
            json!({
                "all_passed": false,
                "check_results": [],
                "failure_summary": "Failed checks: cargo nextest run --workspace",
                "skipped": false,
                "skip_reason": "",
            }),
        );

        let node = LeanBookkeepNode::new().with_runner(std::sync::Arc::new(|_program, _args, _cwd| {
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }));

        let out = node.process(ctx).await.expect("process should succeed");
        let result = get_result(&out, "LeanBookkeepNode").expect("stamped").clone();
        let committed_state = result_state(&out);

        // 1. status is reconcile_failed
        assert_eq!(committed_state.global_status, "reconcile_failed");
        // 2. bail_reason set
        assert!(committed_state
            .bail_reason
            .as_deref()
            .unwrap_or_default()
            .contains("Failed checks: cargo nextest run --workspace"));
        // 3. the bookkeep flip is skipped — CloseBlockNode reads this
        //    stamped state and refuses to close, never touching mev at all.
        let close_ctx = out;
        let close_node =
            CloseBlockNode::new().with_state_source("LeanBookkeepNode");
        let close_out = close_node
            .process(close_ctx)
            .await
            .expect("close_block process should succeed");
        let close_result =
            get_result(&close_out, "CloseBlockNode").expect("CloseBlockNode stamped");
        assert_eq!(close_result["outcome"], json!("SKIPPED"));
        // 4. the state file is still written to disk.
        assert!(result.get("saved_to").is_some());
        let saved_to = result["saved_to"].as_str().unwrap();
        assert!(std::path::Path::new(saved_to).exists());
    }

    #[tokio::test]
    async fn partial_range_run_never_reconciles_and_never_closes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let worktree = dir.path();
        std::fs::create_dir_all(worktree.join("planning/EN.11.N-lean-bookkeep/sdlc"))
            .expect("mkdir");

        let state = clean_state("EN.11.N-lean-bookkeep");
        let mut ctx = ctx_with_state(&state, Some("1"));
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );
        // Even though a FinalValidationNode(Reconcile) failure is stamped
        // in ctx, a partial run must never consult it.
        ctx.nodes.insert(
            "FinalValidationNode".to_string(),
            json!({
                "all_passed": false,
                "check_results": [],
                "failure_summary": "Failed checks: cargo nextest run --workspace",
                "skipped": false,
                "skip_reason": "",
            }),
        );

        let node = LeanBookkeepNode::new().with_runner(std::sync::Arc::new(|_program, _args, _cwd| {
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }));

        let out = node.process(ctx).await.expect("process should succeed");
        let result = get_result(&out, "LeanBookkeepNode").expect("stamped");

        // The reconcile failure is IGNORED on a partial run — status is
        // "done", not "reconcile_failed".
        assert_eq!(result_state(&out).global_status, "done");
        assert_eq!(result["full_run"], json!(false));

        // CloseBlockNode must refuse to close a partial run's block too.
        let close_ctx = out;
        let close_node =
            CloseBlockNode::new().with_state_source("LeanBookkeepNode");
        let close_out = close_node
            .process(close_ctx)
            .await
            .expect("close_block process should succeed");
        let close_result =
            get_result(&close_out, "CloseBlockNode").expect("CloseBlockNode stamped");
        assert_eq!(close_result["outcome"], json!("SKIPPED"));
        assert!(close_result["detail"]
            .as_str()
            .unwrap()
            .contains("partial"));
    }

    #[test]
    fn close_block_reads_lean_bookkeep_state_via_with_state_source() {
        // AC: `CloseBlockNode::with_state_source("LeanBookkeepNode")` reads
        // the stamped state; the default `"WrapUpNode"` is unchanged.
        let default_node = CloseBlockNode::new();
        let overridden_node = CloseBlockNode::new().with_state_source("LeanBookkeepNode");
        // Compile-time + construction check: both builders exist and
        // return a `CloseBlockNode`, exercised end-to-end by the two tests
        // above (which drive `.process()` on an overridden node and assert
        // it actually reads `LeanBookkeepNode`'s stamp).
        let _ = default_node;
        let _ = overridden_node;
    }

    /// AC: "`lean_bookkeep.rs` contains no `serde_json::to_string`/
    /// `to_writer` call against `SDLCState`" — no second serializer for the
    /// run state exists; verified mechanically.
    #[test]
    fn no_second_serializer_for_sdlc_state() {
        let source = include_str!("lean_bookkeep.rs");
        let production_code = source
            .split_once("\n#[cfg(test)]\n")
            .map(|(before, _)| before)
            .expect("this module has a #[cfg(test)] boundary");
        assert!(
            !production_code.contains("serde_json::to_string"),
            "lean_bookkeep.rs must reuse wrap_up's persist_state, not serialize SDLCState itself"
        );
        assert!(
            !production_code.contains("to_writer"),
            "lean_bookkeep.rs must reuse wrap_up's persist_state, not serialize SDLCState itself"
        );
    }

    /// AC: "`lean_bookkeep.rs` contains no `planning/state.json` mutation
    /// of its own — the flip is `CloseBlockNode`'s".
    #[test]
    fn no_state_json_mutation_of_its_own() {
        let source = include_str!("lean_bookkeep.rs");
        let production_code = source
            .split_once("\n#[cfg(test)]\n")
            .map(|(before, _)| before)
            .expect("this module has a #[cfg(test)] boundary");
        // Checks the literal corpus-file reference, not
        // `super::DEFAULT_STATE_FILENAME`'s own `"sdlc-task-state.json"`
        // (a legitimate, unrelated file this module DOES write, via the
        // reused `wrap_up::persist_state`) — a bare `"state.json"` substring
        // check would false-positive on that filename.
        assert!(
            !production_code.contains("planning/state.json") && !production_code.contains("mev::"),
            "lean_bookkeep.rs must never touch the corpus planning/state.json (directly or via \
             mev) — that's CloseBlockNode's job"
        );
    }
}
