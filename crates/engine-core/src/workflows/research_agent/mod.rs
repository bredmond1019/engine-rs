//! The Research Agent (`RESEARCH_AGENT`) workflow — a policy-aware,
//! WebSearch-backed dual-mode port + broadening of the Python
//! `orchestrator` RESEARCH_AGENT into `engine-rs`.
//!
//! Module layout (each leaf file owned by a task in
//! `planning/EN.4.A-research-agent/tasks.json`):
//! - `schema` — `ResearchAgentEventSchema`, `ResearchMode`, and the two
//!   structured output types `CompanyBrief` / `ProspectingResult` (task 3).
//! - `policy` — `ResearchAgentPolicy` / `PartialResearchAgentPolicy` and the
//!   `Policy` trait delegation to `crate::policy::resolve::resolve` (task 2).
//! - `profiles` — named policy bundles, the `research_agent.{policy,profiles}`
//!   `harness.json` section, and `resolve_policy_for_run` (task 4).
//! - `company_research` — `CompanyResearchNode`, the single-company brief
//!   terminal node (task 5).
//! - `prospecting` — `ProspectingResearchNode`, the prospecting-sweep
//!   terminal node (task 6).
//! - `graph` — `ResearchModeRouterNode` + the declared `WorkflowSchema` /
//!   `NodeRegistry` / `Workflow` assembly (task 7).
//!
//! `WORKFLOW_TYPE` lives in `graph` (mirrors `sdlc_flow::graph`).

pub mod company_research;
pub mod graph;
pub mod policy;
pub mod profiles;
pub mod prospecting;
pub mod schema;

pub use company_research::CompanyResearchNode;
