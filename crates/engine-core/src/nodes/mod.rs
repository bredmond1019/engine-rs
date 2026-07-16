//! Reusable `Node` implementations that wrap external transports (as opposed to
//! `crate::node`, which defines the `Node` trait/registry themselves).
//!
//! `claude_code_step` (`EN.2.A`) wires the `core/claude-code-rs` SDK's async
//! `execute()` into a `Node`, mapping its `Outcome` into `NodeRun`/`TaskContext`.

pub mod claude_code_step;

pub use claude_code_step::ClaudeCodeStep;
