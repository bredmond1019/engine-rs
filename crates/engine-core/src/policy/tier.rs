//! Generic model-tier + verbosity + local-transport-config types, shared by
//! any workflow that resolves a run policy (lifted from
//! `workflows::sdlc_flow::policy` — EN.4.0 task 1). Serde reprs are kept
//! byte-identical to the pre-hoist `sdlc_flow` types so `SdlcPolicy`
//! continues to round-trip unchanged once it delegates to these.

use serde::{Deserialize, Serialize};

/// How verbose model-node prompts should ask the model to be.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputVerbosity {
    Terse,
    #[default]
    Normal,
    Verbose,
}

/// A model tier a stage can be resolved to. `Local` routes through an
/// OpenAI-compatible transport (see `sdlc_flow::graph`'s `with_transport`
/// injection); every other variant maps to a concrete cloud model string
/// via [`model_tier_to_model_string`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    #[default]
    Sonnet,
    Haiku,
    Opus,
    Local,
}

/// Map a stage's resolved [`ModelTier`] to a concrete `claude` CLI model
/// string. `Local` resolves to the caller-supplied `local_model` name
/// (display/bookkeeping only — the transport swap itself is a graph-level
/// concern, not this mapping's).
#[must_use]
pub fn model_tier_to_model_string(tier: ModelTier, local_model: &str) -> String {
    match tier {
        ModelTier::Sonnet => "claude-sonnet-4-5".to_string(),
        ModelTier::Haiku => "claude-haiku-4-5".to_string(),
        ModelTier::Opus => "claude-opus-4-8".to_string(),
        ModelTier::Local => local_model.to_string(),
    }
}

/// Configuration for the `local` model tier's OpenAI-compatible transport.
/// Not present in any workflow's built-in default (no stage defaults to
/// `local`), but shaped here so `harness.json`/event overrides can supply
/// it once any stage opts into `local`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalConfig {
    /// Base URL of the OpenAI-compatible endpoint, e.g. `http://localhost:11434`.
    pub endpoint: String,
    /// Model name to request, e.g. `qwen2.5-coder:7b`.
    pub model: String,
    /// Whether to pass the stage's JSON schema as a constrained-decoding
    /// `response_format` and skip the JSON-repair retry for that stage.
    pub constrained_json: bool,
}

impl Default for LocalConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:11434".to_string(),
            model: "qwen2.5-coder:7b".to_string(),
            constrained_json: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_each_tier_to_its_model_string() {
        let local = LocalConfig::default();
        assert_eq!(
            model_tier_to_model_string(ModelTier::Sonnet, &local.model),
            "claude-sonnet-4-5"
        );
        assert_eq!(
            model_tier_to_model_string(ModelTier::Haiku, &local.model),
            "claude-haiku-4-5"
        );
        assert_eq!(
            model_tier_to_model_string(ModelTier::Opus, &local.model),
            "claude-opus-4-8"
        );
        assert_eq!(
            model_tier_to_model_string(ModelTier::Local, &local.model),
            "qwen2.5-coder:7b"
        );
    }

    #[test]
    fn local_tier_uses_supplied_local_model_name() {
        assert_eq!(
            model_tier_to_model_string(ModelTier::Local, "custom-model:1b"),
            "custom-model:1b"
        );
    }

    #[test]
    fn output_verbosity_default_is_normal() {
        assert_eq!(OutputVerbosity::default(), OutputVerbosity::Normal);
    }

    #[test]
    fn model_tier_default_is_sonnet() {
        assert_eq!(ModelTier::default(), ModelTier::Sonnet);
    }

    #[test]
    fn local_config_default_matches_pre_hoist_baseline() {
        let local = LocalConfig::default();
        assert_eq!(local.endpoint, "http://localhost:11434");
        assert_eq!(local.model, "qwen2.5-coder:7b");
        assert!(!local.constrained_json);
    }
}
