//! `ContentPipelinePolicy` / `PartialContentPipelinePolicy` (EN.5.A task 3
//! scaffold). Per-stage `ModelTier` for `{summarize, critic, revise,
//! translate}` plus the bounded self-critic loop's `max_critic_iterations`
//! and `critic_confidence_threshold` as policy fields; built on `EN.5.D`'s
//! derived `Overlay`.
//!
//! Filled in task 3 — see `planning/EN.5.A-content-pipeline/tasks.json`
//! and `architecture.md` §6.

use serde::{Deserialize, Serialize};

/// All-`Option` mirror of `ContentPipelinePolicy` for the per-event
/// `policy:` override and `harness.json` partial deserialization.
///
/// Scaffolded empty at task 2; task 3 fills in the per-stage tier fields
/// and loop bounds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PartialContentPipelinePolicy {}
