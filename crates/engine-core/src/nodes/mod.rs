//! Reusable `Node` implementations that wrap external transports (as opposed to
//! `crate::node`, which defines the `Node` trait/registry themselves).
//!
//! `claude_code_step` (`EN.2.A`) wires the `core/claude-code-rs` SDK's async
//! `execute()` into a `Node`, mapping its `Outcome` into `NodeRun`/`TaskContext`.
//!
//! `openai_compat_transport` (`EN.3.C` task 5) builds a `ModelTransport` for
//! the `local` model tier: an OpenAI-compatible HTTP transport with the same
//! signature as `claude_code_rs::execute`, so it slots into
//! `ClaudeCodeStep::with_transport` (or any task-loop node's own
//! `with_transport`) with zero changes to `ClaudeCodeStep` itself.
//!
//! `http_post` (`EN.4.C` task 2) is the injectable engine-brain HTTP-POST
//! seam `PersistToBrainNode` calls to push the finished `AutomationRoadmap`
//! to the brain ingest endpoint (Synapse's `POST /ingest/*`, `OR.Q`) — a
//! `reqwest`-backed live implementation plus a test stub that records the
//! last payload it was handed.
//!
//! `channel_transport` (`EN.6.A` task 1) is the injectable egress seam
//! `ActionDispatchNode` calls to deliver a run's outbound actions (digest
//! replies, workflow-trigger chaining) to the channel that originated it —
//! mirrors `http_post`'s trait + live impl + recording stub shape.

pub mod channel_transport;
pub mod claude_code_step;
pub mod http_post;
pub mod openai_compat_transport;

pub use channel_transport::{
    ChannelSendReceipt, ChannelTransport, OutboundAction, OutboundBody, StubChannelTransport,
    UnwiredChannelTransport, WorkflowTriggerDispatch,
};
pub use claude_code_step::{ClaudeCodeStep, MetaTransport, TransportInfo};
pub use http_post::{http_post_live, HttpPost, HttpPostResponse, ReqwestHttpPost, StubHttpPost};
pub use openai_compat_transport::{
    default_local_http_post, openai_compat_meta_transport, openai_compat_meta_transport_live,
    openai_compat_transport, openai_compat_transport_live, LocalHttpPost,
};
