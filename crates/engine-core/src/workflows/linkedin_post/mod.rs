//! `LINKEDIN_POST` (`EN.5.G`) — reads the week's actual work (fleet git
//! history, `log.md`, new `planning/decisions/` files) and drafts LinkedIn
//! post candidates, gated by a self-critic pointed at
//! `business/docs/brand.md`'s anti-slop bank.
//!
//! Module layout (source of truth for exact shapes and routing:
//! `planning/EN.5.G/tasks.md` + `tasks.json`):
//! - `schema` — [`LinkedInPostEventSchema`], [`PostCandidate`],
//!   [`WorkSource`] (task 1).
//! - `work_source` — `WorkSourceNode`, reads git/log/decisions over the
//!   injectable `CommandRunner` seam (task 2).
//! - `policy` / `profiles` — the four-layer policy resolve + the three
//!   named profiles (task 3).
//! - `draft` — `PostDraftNode`, the model node proposing candidates
//!   (task 4).
//! - `brand_critic` / `revise` — the brand-rubric critic and bounded
//!   revise loop (task 5).
//! - `graph` — the declared `WorkflowSchema` / `NodeRegistry` / `Workflow`
//!   assembly, `WORKFLOW_TYPE = "LINKEDIN_POST"` (task 6).

pub mod draft;
pub mod policy;
pub mod profiles;
pub mod schema;
pub mod work_source;

pub use draft::PostDraftNode;
pub use policy::LinkedInPostPolicy;
pub use schema::{LinkedInPostEventSchema, PostCandidate, WorkSource, WorkSourceKind};
pub use work_source::WorkSourceNode;
