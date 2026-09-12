//! The `agent_backend` knob (`EN.16.B`) — which coding-agent transport
//! `ImplementTaskNode` dispatches to.
//!
//! `ClaudeCli` is the existing, billed `claude` CLI transport and remains
//! the default so every run that never sets this knob is unaffected.
//! `Pi` drives `pi_agent_rust` against a local model instead (wired in a
//! later task of this block) — the engine's first `$0` implement-stage
//! run. `Aider` drives the `aider` CLI against a local model instead
//! (see `nodes::aider_transport`) — a second `$0`, local-model-capable
//! backend (`EN.16.C`). This module carries only the enum; it introduces
//! no model, endpoint, or provider literal — `Pi` and `Aider` read the
//! resolved policy's existing `local.{endpoint, model}` block (standing
//! rule 6).

use serde::{Deserialize, Serialize};

/// Which coding-agent transport `ImplementTaskNode` dispatches to.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentBackend {
    /// The existing, billed `claude` CLI transport.
    #[default]
    ClaudeCli,
    /// `pi_agent_rust` against a local model (see `nodes::pi_transport`).
    Pi,
    /// `aider` against a local model (see `nodes::aider_transport`).
    Aider,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_claude_cli() {
        assert_eq!(AgentBackend::default(), AgentBackend::ClaudeCli);
    }

    #[test]
    fn round_trips_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&AgentBackend::ClaudeCli).unwrap(),
            "\"claude_cli\""
        );
        assert_eq!(serde_json::to_string(&AgentBackend::Pi).unwrap(), "\"pi\"");
        assert_eq!(
            serde_json::to_string(&AgentBackend::Aider).unwrap(),
            "\"aider\""
        );
        assert_eq!(
            serde_json::from_str::<AgentBackend>("\"claude_cli\"").unwrap(),
            AgentBackend::ClaudeCli
        );
        assert_eq!(
            serde_json::from_str::<AgentBackend>("\"pi\"").unwrap(),
            AgentBackend::Pi
        );
    }

    #[test]
    fn aider_round_trips_via_serde() {
        assert_eq!(
            serde_json::from_str::<AgentBackend>("\"aider\"").unwrap(),
            AgentBackend::Aider
        );
    }
}
