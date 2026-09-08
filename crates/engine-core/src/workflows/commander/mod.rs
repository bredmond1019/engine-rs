//! `COMMANDER` — the drain as a workflow (`EN.15.F`).
//!
//! Ports `/orchestration-commander`'s drain loop as a registered engine workflow: discover
//! every lane's inbox under the fleet lock dir, route each message by `kind`, complete each
//! with a receipt, then (later tasks in this same block) emit scoped state under the
//! commander's own identity, commit only the manifest paths, and append the drain-log without
//! ever skipping.
//!
//! Submodules, one per pipeline stage — mirroring [`crate::workflows::sweep`]'s section split:
//! - [`drain`] — discover every queue, drain each inbox, route by kind, complete with receipts
//!   (task 1).
//! - [`emit_commit`] — the scoped `mev::emit_state_as` call and the manifest-ONLY commit,
//!   with the `git add -A` mutation test (task 2).
//! - [`drain_log`] — the drain-log append that never skips (a `roadmap: null` row when no
//!   roadmap resolves, never a skipped append) and the commander heartbeat in the pinned bare
//!   epoch-seconds format (task 3, this task).
//! - `triage` (task 4) lands in a later task of this same block; this module does not yet
//!   declare it.
//!
//! **THIS BLOCK'S CENTRAL SCAR, restated because it is the reason [`drain`] exists at all:**
//! the Python commander drained only its own inbox and reported "drained 0" for THIRTEEN
//! consecutive drains while three messages — one a P0 — sat unread. [`drain::discover_queues`]
//! walks the WHOLE `queue/` tree, mirroring `scripts/drain_log.py`'s `discover_queues`, so a
//! drain against any one lane still finds every other lane's mail.
//!
//! **RE-DERIVES, NEVER DETECTS:** every function here re-reads the tree from scratch on each
//! call rather than tracking a cursor or a "last seen" marker — the property that makes running
//! the same drain twice over an unchanged tree a safe no-op. Do not add a cache here.

pub mod drain;
pub mod drain_log;
pub mod emit_commit;
