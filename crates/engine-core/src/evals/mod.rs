//! `engine-core::evals` — the eval slice runner (EN.5.B1, first half of the
//! `OR.U` port).
//!
//! Ports Synapse's deterministic/structural/reference-based scorer library
//! (`app/brain/eval/scorer.py` in `core/orchestrator`) plus its
//! `EvalCase`/`EvalSlice` concepts, scoped down to generic run/workflow
//! telemetry — **not** retrieval scoring (recall@k, MRR, abstain
//! correctness, citation groundedness), which stays in Synapse's `OR.K2`
//! per the repo `CLAUDE.md` boundary test and this block's Notes. If a
//! scorer here ever wants an embedding call, that is the signal it has
//! drifted onto the wrong side of the D51 boundary.
//!
//! Task 1 (this file + `scorers.rs`) lands the pure scorer functions only.
//! `EvalCase`/`EvalSlice` (task 2) and the runner over
//! `policy::aggregate::aggregate_state_files` (task 3) land in later tasks
//! of this spec.

pub mod scorers;

pub use scorers::{score_deterministic, score_reference_based, score_structural, ScoreResult};
