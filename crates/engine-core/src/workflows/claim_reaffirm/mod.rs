//! The `CLAIM_REAFFIRM` workflow (`EN.6.L`) — turns mev's stale-claim wall
//! (D35-distilled `knowledge.md`/`memory.md` entries past their
//! `freshness:` threshold) into a reviewed verdict per claim
//! (`bump-freshness` / `supersede` / `archive` / `needs-human`, each with
//! corpus-evidence citations), delivered as one reviewable proposal report
//! through the mev/materializer seam. The engine proposes; mev writes; a
//! human approves (inherited from Synapse `OR.K3`, load-bearing — see
//! `planning/en-6l-claim-reaffirm/tasks.md`).
//!
//! Module layout (each leaf file owned by the task in
//! `planning/en-6l-claim-reaffirm/tasks.json` that introduces it):
//! - `schema` (task 1) — `ClaimReaffirmInput` (the triggering event, with
//!   the standard `policy`/`profile` override-layer fields), the per-claim
//!   state types (`ClaimItem`/`ClaimStatus`/`Verdict`/`VerdictAction`/
//!   `Citation`/`TransportInfo`), `ClaimReaffirmState`, and the
//!   `ClaimReaffirmPolicy` four-layer policy (standing rule 6) — this
//!   workflow's `policy.rs`/`profiles.rs` equivalent, inlined here since no
//!   task in this spec owns a separate file for it.
//! - `load_claims` (task 1) — `LoadClaimsNode`: reads mev's stale-claim
//!   lane through the injectable `ClaimLaneFs` seam and builds the initial
//!   `ClaimReaffirmState`. See that module's doc comment for the finding on
//!   how mev exposes the lane and why the library path was chosen over
//!   shelling out to `mev attention-queue`.
//! - `queue_router` / `judge` / `save_verdict` (task 2) — the queue-drain
//!   loop: per-claim recall evidence, one `ClaudeCodeStep` judgment, and
//!   the read-modify-write verdict accumulator.
//! - `render_report` / `graph` (task 3) — the reviewable markdown report
//!   through the mev/materializer seam, and the declared workflow graph.
//! - Registration lives in `crates/engine-serve/src/workflows.rs` (task 3),
//!   the same one-fn-per-type pattern as the other workflow types.

pub mod judge;
pub mod load_claims;
pub mod queue_router;
pub mod save_verdict;
pub mod schema;
