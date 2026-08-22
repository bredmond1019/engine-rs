//! The SDLC Task (`SDLC_TASK`) workflow — a Rust port of base-template's
//! `.claude/workflows/sdlc-task.js` (2041 lines): a lean, single-spec
//! implement -> fast-test -> fix -> commit loop, with no review stage and
//! no PR ceremony (`sdlc-task-ships-no-docs-stage`).
//!
//! **This module REUSES `sdlc_flow`'s node set rather than forking it.**
//! `TaskQueueRouterNode`, `ImplementTaskNode`, `TestTaskNode`,
//! `TriageTaskNode`, `IncrementAttemptNode`, `UpdateTaskStatusNode`,
//! `GenerateTasksNode`, and `FinalValidationNode` all come straight from
//! `crate::workflows::sdlc_flow` unchanged. Only what SDLC_TASK genuinely
//! needs and `sdlc_flow` does not have lives here: this event schema, the
//! `ReconcileFailed` terminal signal (added to `sdlc_flow::schema::
//! TerminalSignal` itself, since the enum and `derive_committed_status`
//! are shared machinery), `TaskTriageRouterNode` (the three-arm fork —
//! `sdlc_flow`'s router routes `PASS` to `ConsolidatedReviewNode`, which
//! SDLC_TASK never registers), and `LeanBookkeepNode` (the close-out that
//! skips the review/docs/PR machinery entirely).
//!
//! Every shared node-plumbing seam this module needs (`get_result`/
//! `put_result`, `parse_structured_or_fenced`, `ModelTransport`,
//! `CommandOutput`/`CommandRunner`/`default_command_runner`, `commit_all`/
//! `is_noop_commit`, `crate::policy::command_floor`) is imported straight
//! from `crate::workflows` / `crate::policy` — **never from `sdlc_flow`**.
//! That is the whole point of `EN.11.M`'s lift: a second engine (this one)
//! reaches the shared seams without depending on the first engine's module.
//!
//! Module layout (mirrors `sdlc_flow`'s one-concern-per-file convention):
//! - `schema` — `SdlcTaskEventSchema`, plus re-exports of the `sdlc_flow`
//!   state types this workflow reuses as-is.
//! - `task_triage_router` — `TaskTriageRouterNode`, the three-arm triage
//!   fork (port design T6).
//! - `lean_bookkeep` — `LeanBookkeepNode`, the lean close-out (port design
//!   T8): persists + commits the durable run state and derives the
//!   terminal status. Ships no block-status flip of its own — that is
//!   `sdlc_flow::close_block::CloseBlockNode`'s job, wired in by a later
//!   task in this spec via `CloseBlockNode::with_state_source`.
//! - `graph` — assembles the declared `WorkflowSchema` + `NodeRegistry`
//!   (added by a later task in this spec — this task lands as an
//!   unreachable-but-tested module with no graph yet).

pub mod lean_bookkeep;
pub mod schema;
pub mod task_triage_router;

#[allow(unused_imports)]
pub(crate) use super::{
    commit_all, default_command_runner, get_result, is_noop_commit, parse_structured_or_fenced,
    put_result, CommandOutput, CommandRunner, ModelTransport,
};
#[allow(unused_imports)]
pub(crate) use crate::policy::command_floor;

/// The bare filename every SDLC_TASK state writer/reader resolves through —
/// the JS engine's own state filename (`sdlc-task.js` STATE header).
///
/// Deliberately NOT `sdlc_flow::DEFAULT_STATE_FILENAME`
/// (`"sdlc-flow-state.json"`) — the two engines write distinct state files
/// so a spec driven by one engine never collides with a run of the other
/// against the same `planning/<slug>/` directory.
pub const DEFAULT_STATE_FILENAME: &str = "sdlc-task-state.json";

#[cfg(test)]
mod tests {
    use super::DEFAULT_STATE_FILENAME;

    #[test]
    fn default_state_filename_is_the_js_engines_own_and_distinct_from_sdlc_flows() {
        assert_eq!(DEFAULT_STATE_FILENAME, "sdlc-task-state.json");
        assert_ne!(
            DEFAULT_STATE_FILENAME,
            crate::workflows::sdlc_flow::DEFAULT_STATE_FILENAME
        );
    }

    /// AC: "No file under `crates/engine-core/src/workflows/sdlc_task/`
    /// imports from `crate::workflows::sdlc_flow` for any seam EN.11.M
    /// lifted" — verified mechanically rather than by inspection, per the
    /// task spec's explicit requirement.
    #[test]
    fn no_sdlc_task_source_file_imports_the_lifted_seams_from_sdlc_flow() {
        let banned = [
            "command_floor",
            "CommandOutput",
            "CommandRunner",
            "default_command_runner",
            "commit_all",
            "is_noop_commit",
            "get_result",
            "put_result",
        ];
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/workflows/sdlc_task");
        let mut offenders = Vec::new();
        for entry in std::fs::read_dir(&dir).expect("read sdlc_task dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            let content = std::fs::read_to_string(&path).expect("read source file");
            for line in content.lines() {
                let trimmed = line.trim();
                // Skip comment lines (`//` doc/line comments) — this check
                // is about actual imports, not prose that names a seam.
                if trimmed.starts_with("//") {
                    continue;
                }
                if !trimmed.contains("sdlc_flow::") {
                    continue;
                }
                for seam in banned {
                    if trimmed.contains(seam) {
                        offenders.push(format!("{}: {}", path.display(), trimmed));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "sdlc_task file(s) import a lifted seam from sdlc_flow instead of workflows::mod: \
             {offenders:?}"
        );
    }
}
