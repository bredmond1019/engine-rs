//! The Deliverable Render (`DELIVERABLE_RENDER`) workflow — renders an
//! `AutomationRoadmap` (produced by `PROPOSAL_GENERATOR`, `EN.4.C`) into the
//! four-section, locale-correct client deliverable described by
//! `agentic-portfolio/business/docs/diagnostic/deliverable.md`, plus the PDF
//! rendition produced by `typst` behind the injectable `CommandRunner` seam.
//!
//! Module layout (each leaf file owned by a task in
//! `planning/EN.4.D/tasks.json`):
//! - `schema` — `DeliverableRenderEventSchema`, the triggering event shape
//!   carrying an inline `AutomationRoadmap`, the requested `Locale`, and the
//!   `output_dir` both artifacts are written under, plus the company-slug
//!   basename derivation (task 1).
//! - `policy` / `profiles` — the `Policy` surface for the workflow's one
//!   real knob, the optional model-polish pass over the rendered markdown
//!   (task 2).
//! - `render_markdown` — `RenderDeliverableNode`, the deterministic
//!   four-section renderer with the `authored_locale` mismatch refusal
//!   (task 3).
//! - `render_pdf` — `RenderPdfNode`, the `typst` subprocess node over the
//!   injectable `CommandRunner` seam (task 4).
//! - `graph` — `WORKFLOW_TYPE`, the declared `WorkflowSchema` / `NodeRegistry`
//!   / `registry_for_policy` / `Workflow` assembly, the straight line
//!   `RenderDeliverableNode -> RenderPdfNode` (task 5).

pub mod graph;
pub mod policy;
pub mod profiles;
pub mod render_markdown;
pub mod render_pdf;
pub mod schema;

pub use graph::WORKFLOW_TYPE;
pub use policy::{DeliverableRenderPolicy, PartialDeliverableRenderPolicy};
pub use profiles::resolve_policy_for_run;
pub use render_markdown::{render_markdown, RenderDeliverableNode};
pub use render_pdf::{typst_argv, RenderPdfNode};
pub use schema::{deliverable_slug, DeliverableRenderEventSchema};
