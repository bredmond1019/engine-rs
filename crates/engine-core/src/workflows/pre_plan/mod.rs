//! `PRE_PLAN` — an inbound idea becomes a researched
//! `$BRAIN_ROOT/planning/open-work/pre-plan/<slug>/notes.md`, with no human
//! back-and-forth (`EN.19.A`).
//!
//! Composed of atomic, independently model-tier-routable nodes so a later
//! composing workflow (`EN.19.D`) can wire this same node set into a larger
//! graph rather than only reaching it through the standalone `PRE_PLAN`
//! workflow_type — no node here may assume it is the first node of a run.
//!
//! Module layout (source of truth for exact shapes: `planning/EN.19.A.json`
//! and `planning/EN.19.A/tasks.json`):
//! - `check_existing` — `CheckExistingNotesNode`, the idempotency guard: no
//!   model call, `exists()`-checks the target `notes.md` before any research
//!   runs and short-circuits unless `force_regenerate: true` (task 1).
//! - `intake` — `IntakeIdeaNode`, normalizes the dispatched event's idea
//!   text/slug/channel metadata into `TaskContext`; no model call (task 1).
//! - `research` — `ResearchCodebaseNode`, a read-only `AgentCodeStep`
//!   session scoped to Read/Grep/Glob (task 2).
//! - `write_notes` — `WriteNotesNode`, renders the research findings into
//!   `notes.md` matching `.claude/commands/capture.md`'s output shape
//!   (task 3).
//!
//! Graph assembly (`WorkflowSchema`/`NodeRegistry`/`WORKFLOW_TYPE`) lands in
//! task 4, once `research` and `write_notes` exist.

pub mod check_existing;
pub mod intake;
pub mod research;
