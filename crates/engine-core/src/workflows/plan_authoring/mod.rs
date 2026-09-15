//! `plan_authoring` — the `PLAN_AUTHORING` sub-workflow (`EN.19.B`): a
//! Node-composed port of `.claude/commands/plan.md`'s single-repo path
//! that turns a `notes.md`/`sequence.md` pre-plan folder into a rendered
//! `plan.md` narrative plus schema-valid candidate block records staged
//! for human review — it never calls `mev create-block --write` itself
//! (see this block's own `why`/`out_of_scope` in
//! `planning/blocks/EN.19.B.json`).
//!
//! # Stage-not-register
//!
//! Every candidate block this workflow produces lands under
//! `$BRAIN_ROOT/planning/open-work/pre-plan/<slug>/candidate-blocks/` as
//! schema-validated JSON — **not** in `planning/blocks/`, and no node here
//! ever shells or links against `mev create-block`. Registering a staged
//! candidate for real is a deliberate, separate, human-reviewed action
//! this block hands off to (see `EN.19.B.json`'s `out_of_scope`): a
//! single-model decomposition with no red-team or initiative-wide
//! consistency pass is a known, accepted quality gap, which is exactly why
//! nothing here is allowed to write into `state.json` or `planning/blocks/`
//! on its own.
//!
//! # Module layout
//!
//! Each leaf module is owned by a task in `planning/EN.19.B/tasks.json`:
//! - `check_existing` — [`CheckExistingPlanNode`], short-circuits a repeat
//!   run against an already-staged slug unless `force_regenerate: true`
//!   (task 1). Also owns the shared [`check_existing::PlanAuthoringFs`]
//!   seam and [`check_existing::pre_plan_dir`] helper both this module's
//!   file-touching nodes use.
//! - `gather_context` — [`gather_context::GatherPlanContextNode`], reads
//!   `CLAUDE.md`/`planning/context.md`/`planning/state.json` plus the
//!   pre-plan folder (task 1).
//! - `decompose` — `DecomposePlanNode`, the one model-calling stage
//!   (task 2, not yet implemented).
//! - `stage_candidate_blocks` — `StageCandidateBlocksNode` (task 3, not
//!   yet implemented).
//! - `write_narrative` — `WritePlanNarrativeNode` (task 4, not yet
//!   implemented).
//!
//! `WORKFLOW_TYPE`, the assembled `WorkflowSchema`/`NodeRegistry`, and the
//! `register_plan_authoring` wiring into `crates/engine-serve/src/workflows.rs`
//! all land in task 5 — this module exports only the two task-1 nodes
//! today.
//!
//! # On the deferred `ExistsGuardNode` extraction
//!
//! `EN.19.B.json`'s 3rd amendment asks that [`check_existing::CheckExistingPlanNode`]'s
//! "exists()-check a path, short-circuit unless `force_regenerate`" shape be
//! factored out into a shared generic alongside `EN.19.A`'s
//! `CheckExistingNotesNode`, extracted *before* this node is written, not
//! after. As of this task, `EN.19.A` is BLOCKED (`planning/status.md`,
//! 2026-09-15 entry) and `crates/engine-core/src/workflows/pre_plan/` does
//! not exist anywhere in this tree — there is no second concrete instance
//! yet to extract a generic *from*. Building a generic off one instance
//! would be speculative abstraction, not reuse, so `check_existing.rs` is
//! written directly. The extraction remains the right call once `EN.19.A`
//! actually lands; whichever of the two blocks lands second should migrate
//! onto a shared `ExistsGuardNode` rather than leaving two hand-copies in
//! the tree.

pub mod check_existing;
pub mod gather_context;
