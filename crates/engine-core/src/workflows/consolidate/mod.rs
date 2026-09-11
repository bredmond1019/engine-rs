//! The CONSOLIDATE workflow — `EN.15.K`.
//!
//! Ports `/consolidate-run`'s discovery, D57 two-axis selection, and the shared watermark advance
//! into typed Rust nodes, and writes `disposal.json` for the first time. Extraction and mechanism
//! naming stay `ClaudeCodeStep`s (out of scope here — they are genuinely LLM work); this module
//! covers only the mechanical join and the artifact writers around it.
//!
//! Shape: discover -> select -> disposal write -> remediation promote -> watermark advance.
//!
//! Submodules land as their owning task completes:
//! - [`discover`] (Task 1) — brain-root walk, lane-log participant cross-check, and realpath
//!   dedup, reusing [`crate::roadmap_status`]'s existing `discover_run_records` /
//!   `realpath_dedup` / `read_lane_log` / `repos_from_lane_log` rather than re-implementing any
//!   of them.
//! - [`select`] (Task 2) — the D57 two-axis `origin_roadmap` selection rule, plus `--since`
//!   scoping mirrored from `lane_log_watermark.py`'s `since_filter`.
//! - `watermark` (Task 3) — reads the Python watermark writer's last line before advancing, so a
//!   mixed-writer log stays monotonic; refuses on hash drift rather than re-basing.
//! - `disposal` (Task 4) — writes `disposal.json` through the existing `okf_core::coord::disposal`
//!   type, matching the five hand-written `retros/disposal-*.json` files' field shape.
//! - `remediation` (Task 5) — promotes a failing verification-ledger entry into HQ's
//!   `docs/sandbox/remediation.json` + `findings.json`, as the single writer of the global
//!   finding number. Writes no repo's `state.json`.
//! - `graph` (Task 6) — the `CONSOLIDATE` `WorkflowSchema`/`NodeRegistry`/`Workflow` assembly
//!   wrapping the above into one `ConsolidateRunNode`, registered in `engine-serve`.

pub mod discover;
pub mod select;
pub mod watermark;

pub use discover::{discover_participants, DiscoveryFinding, DiscoveryResult};
pub use select::{select_ledger_rows, since_filter, SelectedRow};
pub use watermark::{advance_watermark, read_watermark, AdvanceOutcome, WatermarkEntry, WatermarkError};
