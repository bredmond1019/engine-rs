//! The Proposal Generator (`PROPOSAL_GENERATOR`) workflow — a policy-aware,
//! seven-node port of the Python `orchestrator` PROPOSAL_GENERATOR into
//! `engine-rs`, producing the `AutomationRoadmap` deliverable and pushing it
//! to the brain over an injectable `HttpPost` seam (dropping `StorageNode` —
//! no embedding/pgvector in this repo; see THE BOUNDARY TEST in `CLAUDE.md`).
//!
//! Module layout (each leaf file owned by a task in
//! `planning/EN.4.C-proposal-generator/tasks.json`):
//! - `schema` — `ProposalGeneratorEventSchema` and the structured output type
//!   `AutomationRoadmap`, plus the composite/sort/≤3-profile validators and
//!   `json_schema` builders (task 4).
//! - `policy` — `ProposalGeneratorPolicy` / `PartialProposalGeneratorPolicy`
//!   and the `Policy` trait delegation to `crate::policy::resolve::resolve`
//!   (task 3).
//! - `profiles` — named policy bundles, the
//!   `proposal_generator.{policy,profiles}` `harness.json` section, and
//!   `resolve_policy_for_run` (task 5).
//! - `company_research` — `ProposalCompanyResearchNode`, the WebSearch-backed
//!   `research`-stage entry node (task 6).
//! - `opportunity_identifier` — `OpportunityIdentifierNode`, the
//!   evidence-aware `opportunity`-stage scoring node (task 7).
//! - `writer` — `ProposalWriterNode`, the `writer`-stage drafting node
//!   (task 6).
//! - `review` — `ProposalReviewNode`, the `review`-stage verdict node
//!   (task 8).
//! - `review_router` — `ProposalReviewRouterNode`, the `Node` + `Router`
//!   pass/revise branch (task 8).
//! - `revise` — `ProposalReviseNode`, the `revise`-stage correction node
//!   (task 8).
//! - `persist_to_brain` — `PersistToBrainNode`, the terminal HTTP-push node
//!   over the `crate::nodes::http_post` seam (task 9).
//! - `graph` — the declared `WorkflowSchema` / `NodeRegistry` /
//!   `registry_for_policy` (Local rewire) / `Workflow` assembly (task 10).
//!
//! `WORKFLOW_TYPE` lives in `graph` (mirrors `research_agent::graph` /
//! `diagnostic_intake::graph` / `sdlc_flow::graph`).

pub mod company_research;
pub mod graph;
pub mod opportunity_identifier;
pub mod persist_to_brain;
pub mod policy;
pub mod profiles;
pub mod review;
pub mod review_router;
pub mod revise;
pub mod schema;
pub mod writer;

pub use schema::AutomationRoadmap;
