//! Generic model-node "shaping" helpers — applying a resolved policy's
//! model-tier, prompt-cache, and verbosity knobs to a model-node's
//! `Config`/prompt — factored out of
//! `workflows::sdlc_flow::task_loop::apply_policy` (EN.4.0 task 1) so any
//! workflow's model nodes can reuse them instead of re-deriving the same
//! three lines of `Config` plumbing.

use std::time::Duration;

use claude_code_rs::Config;

use super::tier::{model_tier_to_model_string, ModelTier, OutputVerbosity};

/// Set `config.model` from the resolved [`ModelTier`], mapping through
/// [`model_tier_to_model_string`] (`local_model` supplies the display name
/// for `ModelTier::Local`).
#[must_use]
pub fn apply_model_tier(mut config: Config, tier: ModelTier, local_model: &str) -> Config {
    config.model = Some(model_tier_to_model_string(tier, local_model));
    config
}

/// When `enabled`, set `config.system_prompt` to a caller-supplied stable
/// prefix so the underlying `claude` CLI has a constant prefix to cache
/// against across calls. A no-op when `enabled` is false, leaving
/// `config.system_prompt` untouched.
#[must_use]
pub fn apply_prompt_cache(mut config: Config, enabled: bool, stable_system_prompt: &str) -> Config {
    if enabled {
        config.system_prompt = Some(stable_system_prompt.to_string());
    }
    config
}

/// When `timeout_secs` is `Some`, set `config.timeout` to that many seconds
/// — the whole-call timeout `claude_code_rs`'s `execute()` enforces. A true
/// no-op when `None`, leaving `config.timeout` at whatever the caller's
/// `Config` already carried (`None` from `Config::default()`, which is
/// `claude-code-rs`'s own unconfigured 300s default).
#[must_use]
pub fn apply_call_timeout(mut config: Config, timeout_secs: Option<u64>) -> Config {
    if let Some(secs) = timeout_secs {
        config.timeout = Some(Duration::from_secs(secs));
    }
    config
}

/// Prompt directive injected when `verbosity` deviates from the baseline
/// `Normal` (`Normal` intentionally injects nothing so the built-in-default
/// prompt text stays byte-identical to a directive-free baseline).
#[must_use]
pub fn verbosity_directive(verbosity: OutputVerbosity) -> Option<&'static str> {
    match verbosity {
        OutputVerbosity::Normal => None,
        OutputVerbosity::Terse => Some(
            "\n\nBe terse: keep your response as short as possible while still \
             satisfying the request. Target well under 200 output tokens where \
             the task allows it.",
        ),
        OutputVerbosity::Verbose => Some(
            "\n\nBe thorough: explain your reasoning and any trade-offs in \
             detail before giving the final answer.",
        ),
    }
}

/// Append [`verbosity_directive`]'s directive (if any) to `prompt`.
#[must_use]
pub fn apply_verbosity_directive(prompt: String, verbosity: OutputVerbosity) -> String {
    match verbosity_directive(verbosity) {
        Some(directive) => format!("{prompt}{directive}"),
        None => prompt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_model_tier_sets_config_model() {
        let config = apply_model_tier(Config::default(), ModelTier::Haiku, "qwen2.5-coder:7b");
        assert_eq!(config.model.as_deref(), Some("claude-haiku-4-5"));
    }

    #[test]
    fn apply_model_tier_local_uses_local_model_name() {
        let config = apply_model_tier(Config::default(), ModelTier::Local, "qwen2.5-coder:7b");
        assert_eq!(config.model.as_deref(), Some("qwen2.5-coder:7b"));
    }

    #[test]
    fn apply_prompt_cache_sets_system_prompt_when_enabled() {
        let config = apply_prompt_cache(Config::default(), true, "stable prefix");
        assert_eq!(config.system_prompt.as_deref(), Some("stable prefix"));
    }

    #[test]
    fn apply_prompt_cache_is_noop_when_disabled() {
        let config = apply_prompt_cache(Config::default(), false, "stable prefix");
        assert_eq!(config.system_prompt, None);
    }

    #[test]
    fn apply_call_timeout_is_noop_when_none() {
        let config = apply_call_timeout(Config::default(), None);
        assert_eq!(config.timeout, None);
    }

    #[test]
    fn apply_call_timeout_sets_duration_when_some() {
        let config = apply_call_timeout(Config::default(), Some(30));
        assert_eq!(config.timeout, Some(Duration::from_secs(30)));
    }

    #[test]
    fn apply_call_timeout_none_leaves_a_preexisting_timeout_alone() {
        let base = Config {
            timeout: Some(Duration::from_secs(45)),
            ..Config::default()
        };
        let config = apply_call_timeout(base, None);
        assert_eq!(config.timeout, Some(Duration::from_secs(45)));
    }

    #[test]
    fn verbosity_directive_normal_is_none() {
        assert_eq!(verbosity_directive(OutputVerbosity::Normal), None);
    }

    #[test]
    fn verbosity_directive_terse_and_verbose_are_some_and_distinct() {
        let terse = verbosity_directive(OutputVerbosity::Terse).unwrap();
        let verbose = verbosity_directive(OutputVerbosity::Verbose).unwrap();
        assert_ne!(terse, verbose);
    }

    #[test]
    fn apply_verbosity_directive_appends_to_prompt() {
        let prompt = apply_verbosity_directive("base prompt".to_string(), OutputVerbosity::Terse);
        assert!(prompt.starts_with("base prompt"));
        assert!(prompt.contains("Be terse"));
    }

    #[test]
    fn apply_verbosity_directive_normal_leaves_prompt_unchanged() {
        let prompt = apply_verbosity_directive("base prompt".to_string(), OutputVerbosity::Normal);
        assert_eq!(prompt, "base prompt");
    }
}
