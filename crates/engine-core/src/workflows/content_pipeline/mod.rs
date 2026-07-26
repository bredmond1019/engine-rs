//! `CONTENT_PIPELINE` — the envelope-based, channel-agnostic content
//! workflow (EN.5.A). Rebuilds Synapse's `CONTENT_PIPELINE` around the
//! shared `engine_contract::envelope::IngressEnvelope` contract so one graph
//! serves web articles and YouTube transcripts today and every Phase 6
//! channel (Slack, Telegram, WhatsApp, Email, ResearchAgent, workflow
//! triggers) tomorrow, with a bounded self-critic loop, optional PT-BR
//! translation, digest render, and a `PersistToBrainNode` write through the
//! `EN.4.C` `HttpPost` seam (D51: no embedding or pgvector in engine-rs).
//!
//! Module layout (each leaf file owned by a task in
//! `planning/EN.5.A-content-pipeline/tasks.json`; source of truth for exact
//! shapes and routing is `planning/EN.5.A-content-pipeline/architecture.md`):
//! - `schema` — `ContentPipelineInput`/`ContentPipelineOutput`,
//!   `CriticVerdict`, `CriticEvaluation` (task 2).
//! - `policy` — `ContentPipelinePolicy` / `PartialContentPipelinePolicy`,
//!   per-stage `ModelTier` for `{summarize, critic, revise, translate}`,
//!   and the loop bounds (`max_critic_iterations`,
//!   `critic_confidence_threshold`) as policy fields (task 3).
//! - `profiles` — `WORKFLOW_KEY = "content_pipeline"`, named profile
//!   bundles, `resolve_policy_for_run` (task 3).
//! - `source_router` — `SourceRouterNode`, routes on `SourcePayload` kind,
//!   not `channel_type` (task 4).
//! - `fetch_article` — `FetchArticleNode`, fetches `SourcePayload::Url`
//!   content behind an injectable seam (task 5).
//! - `fetch_transcript` — `FetchTranscriptNode`, same shape over
//!   `SourcePayload::VideoId` (task 5).
//! - `normalize_channel_content` — `NormalizeChannelContentNode`,
//!   deterministic passthrough for `SourcePayload::ChannelMessage` (task 5).
//! - `summarize` — `SummarizeNode`, the `summarize`-stage model node
//!   (task 6).
//! - `self_critic` — `SelfCriticNode`, emits `CriticEvaluation` each loop
//!   pass (task 7).
//! - `critic_router` — `CriticRouterNode`, the bounded-loop guard (task 8).
//! - `increment_critic_iteration` — `IncrementCriticIterationNode`, the
//!   counter bump on the back-edge (task 8).
//! - `revise` — `ReviseNode`, the `revise`-stage correction node (task 9).
//! - `translate` — `TranslateSkipRouterNode` + `TranslateNode` (task 10).
//! - `digest_render` — `DigestRenderNode`, deterministic renderer
//!   (task 11).
//! - `persist_to_brain` — `PersistToBrainNode`, terminal `HttpPost` write
//!   (task 11).
//! - `graph` — the declared `WorkflowSchema` / `NodeRegistry` /
//!   `registry_for_policy` (Local rewire) / `Workflow` assembly (task 12).
//!
//! `WORKFLOW_TYPE` lives in `graph` (mirrors `proposal_generator::graph` /
//! `research_agent::graph` / `diagnostic_intake::graph` /
//! `sdlc_flow::graph`).

pub mod critic_router;
pub mod digest_render;
pub mod fetch_article;
pub mod fetch_transcript;
pub mod graph;
pub mod increment_critic_iteration;
pub mod normalize_channel_content;
pub mod persist_to_brain;
pub mod policy;
pub mod profiles;
pub mod revise;
pub mod schema;
pub mod self_critic;
pub mod source_router;
pub mod summarize;
pub mod translate;

pub use schema::{ContentPipelineInput, ContentPipelineOutput, CriticEvaluation, CriticVerdict};
