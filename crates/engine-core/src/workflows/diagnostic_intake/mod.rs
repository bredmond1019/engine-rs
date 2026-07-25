//! The Diagnostic Intake (`DIAGNOSTIC_INTAKE`) workflow — a net-new,
//! policy-aware, single-node extractor that turns raw diagnostic-call
//! notes/transcript into a validated `DiagnosticIntake` evidence contract
//! (`agentic-portfolio/business/docs/diagnostic/intake.md §3`), built on the
//! EN.4.0 shared policy framework.
//!
//! Module layout (each leaf file owned by a task in
//! `planning/EN.4.B-diagnostic-intake/tasks.json`):
//! - `schema` — `DiagnosticIntakeEventSchema` and the structured output type
//!   `DiagnosticIntake` (task 3).
//! - `policy` — `DiagnosticIntakePolicy` / `PartialDiagnosticIntakePolicy`
//!   and the `Policy` trait delegation to `crate::policy::resolve::resolve`
//!   (task 2).
//! - `profiles` — named policy bundles, the `diagnostic_intake.{policy,
//!   profiles}` `harness.json` section, and `resolve_policy_for_run`
//!   (task 4).
//! - `extract` — `IntakeExtractNode`, the single terminal node (task 5).
//! - `graph` — the declared `WorkflowSchema` / `NodeRegistry` /
//!   `registry_for_policy` (Local rewire) / `Workflow` assembly (task 6).
//!
//! `WORKFLOW_TYPE` lives in `graph` (mirrors `research_agent::graph` /
//! `sdlc_flow::graph`).
//!
//! Unlike `research_agent`, this workflow has exactly one node — it is both
//! the start and the terminal node; there is no router.

pub mod extract;
pub mod graph;
pub mod policy;
pub mod profiles;
pub mod schema;

pub use extract::IntakeExtractNode;
pub use schema::DiagnosticIntake;
