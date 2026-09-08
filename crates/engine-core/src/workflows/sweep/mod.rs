//! `SWEEP` — the roadmap sweep as an engine workflow (`EN.15.E`).
//!
//! Mirrors `roadmap_sweep.py` (HQ root, 908 lines) field-for-field and decision-for-decision
//! — see that script's own module doc comment ("WHY THIS EXISTS" / "PIPELINE" / "THE STALENESS
//! RULE" / "RE-FIRE THRESHOLD") for the design this ports. **THE PYTHON STAYS THE ORACLE**
//! (Fork 2, `EN.15.E`'s block record `notes`): this module reproduces its diff and routing
//! decisions but never replaces, edits, or schedules it (Fork 4 — `planning/harness.json`'s
//! `schedule.entries` stays empty).
//!
//! Submodules, one per pipeline stage — the same section breaks the Python script itself uses:
//! - [`snapshot`] — `build_raw_snapshot` + `semantic_projection` (task 2, this task)
//! - `diff` — `diff_snapshots` / `build_dedup_history` (task 3, not yet ported)
//! - `route` — `route_escalation` / `route_non_escalation_diff` + the three `GatedAction`
//!   integration (task 4, not yet ported)
//!
//! `crates/engine-serve/src/workflows.rs` registers SWEEP as a dispatchable workflow in task 5;
//! nothing in this module is wired to a schedule anywhere (Fork 4 is a hard boundary — see the
//! block record's `out_of_scope`).

pub mod snapshot;

pub use snapshot::{
    build_raw_snapshot, build_raw_snapshot_with, escalation_stale, git_head_sha,
    list_snapshot_files, load_snapshot, read_escalations, semantic_projection, snapshot_filename,
    sweeps_dir, utc_now_ts, ProjectedBlock, ProjectedLane, ProjectedLease, ProjectedMessageQueue,
    ProjectedOperatorGates, ProjectedRegistryEntry, ProjectedSdlcState, RawSnapshot,
    SemanticProjection,
};
