//! The declared `WorkflowSchema` / `NodeRegistry` / `registry_for_policy`
//! (Local rewire for `IntakeExtractNode`) / `Workflow` assembly for
//! `DIAGNOSTIC_INTAKE`. Filled by task 6
//! (`planning/EN.4.B-diagnostic-intake/tasks.json`).

/// The registered workflow type string (mirrors `research_agent::graph` /
/// `sdlc_flow::graph`, both of which hold `WORKFLOW_TYPE` here rather than
/// in `mod.rs`).
pub const WORKFLOW_TYPE: &str = "DIAGNOSTIC_INTAKE";
