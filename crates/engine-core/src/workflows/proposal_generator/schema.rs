//! Event schema (`ProposalGeneratorEventSchema`) and structured output type
//! (`AutomationRoadmap`) for the `PROPOSAL_GENERATOR` workflow, plus the
//! composite/sort/≤3-profile validators and `json_schema` builders.
//!
//! Filled in by `planning/EN.4.C-proposal-generator/tasks.json` task 4. This
//! is a compiling stub — `AutomationRoadmap` exists so `mod.rs`'s
//! `pub use schema::AutomationRoadmap` (load-bearing for `EN.4.D`) resolves
//! at this task boundary; later tasks flesh out its fields and validators.

use serde::{Deserialize, Serialize};

/// The proposal-generator deliverable — faithfully models
/// `agentic-portfolio/business/docs/diagnostic/deliverable.md`'s 4 sections.
/// Fleshed out in task 4; the name is load-bearing (imported by `EN.4.D`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AutomationRoadmap {}
