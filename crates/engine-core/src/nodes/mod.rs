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

pub mod claude_code_step;
pub mod openai_compat_transport;

pub use claude_code_step::ClaudeCodeStep;
pub use openai_compat_transport::{
    default_local_http_post, openai_compat_transport, openai_compat_transport_live, LocalHttpPost,
};
