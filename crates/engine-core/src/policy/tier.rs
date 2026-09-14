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
    /// Model name to request, e.g. `qwen2.5:7b-instruct`.
    pub model: String,
    /// Whether to pass the stage's JSON schema as a constrained-decoding
    /// `response_format` and skip the JSON-repair retry for that stage.
    pub constrained_json: bool,
}

impl Default for LocalConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:11434".to_string(),
            model: "qwen2.5:7b-instruct".to_string(),
            constrained_json: false,
        }
    }
}

/// Default `--tools` allowlist for `AgentBackend::Pi` (`nodes::pi_transport`)
/// — scoped to what a one-shot, headless SDLC coding task actually
/// exercises. Verified against a real `pi --help` (v0.5.1, 2026-09-14):
/// `pi`'s own built-in default is `read,bash,edit,write,grep,find,ls,
/// hashline_edit,web_search,ast_grep,ast_edit,lsp,debug,ask,todo,
/// submit_plan,jobs,hub,current_time`. This list keeps the file read/write/
/// edit tools (`read`/`write`/`edit`/`hashline_edit`), the shell
/// (`bash`), code search/navigation (`grep`/`find`/`ls`/`ast_grep`/
/// `ast_edit`/`lsp`), and the agent's own task list (`todo`) —
/// `ImplementTaskNode`'s real dispatch needs exactly this set to read,
/// search, edit and test a repository. It deliberately drops:
/// - `web_search` — network egress and cost with no SDLC-task use, and part
///   of `pi_transport.rs`'s own named, still-open SAFETY BOUNDARY under
///   `--approval-mode yolo` (no per-tool gate).
/// - `ask` — asks a human for clarification; a headless `-p` run has no one
///   to answer it, so it can only ever dead-end a run.
/// - `submit_plan` — only meaningful under `--plan-mode`, which this
///   transport never sets.
/// - `jobs` — background-job spawning this transport neither needs nor
///   reaps.
/// - `hub` — network extension marketplace: another network/credential
///   surface with no SDLC-task use.
/// - `debug`, `current_time` — not documented in `pi --help` beyond their
///   one-line names and not exercised by any known SDLC dispatch; dropped
///   for the same minimal-but-functional reasoning as the rest of this set
///   rather than kept "just in case".
pub const DEFAULT_PI_TOOLS: &[&str] = &[
    "read",
    "write",
    "edit",
    "hashline_edit",
    "bash",
    "grep",
    "find",
    "ls",
    "ast_grep",
    "ast_edit",
    "lsp",
    "todo",
];

/// `AgentBackend::Pi`'s own CLI-flag knobs (`nodes::pi_transport`) —
/// distinct from [`LocalConfig`], which every local-model transport shares.
/// Not present in any workflow's built-in default until a stage actually
/// resolves to `AgentBackend::Pi`; shaped here so `harness.json`/event
/// overrides can supply it once any stage opts in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PiConfig {
    /// Passes `pi --no-context-files`, suppressing `pi`'s own ancestor-
    /// directory `AGENTS.md`/`CLAUDE.md` auto-discovery. **Verified real
    /// leakage, 2026-09-14** (see `nodes::pi_transport`'s module doc for the
    /// full repro): from a scratch directory nested three levels under a
    /// probe `AGENTS.md` instructing a sign-off, a real
    /// `pi --verbose --no-tools -p "say ok"` call against `qwen2.5-coder:
    /// 7b-ctx16384` over `ollama` replied `"Ok" -- Level A Agent` —
    /// following an instruction from an ancestor directory nobody asked it
    /// to read, unprompted. The identical call with `--no-context-files`
    /// added replied plain `OK`. This fleet's `AGENTS.md`/`CLAUDE.md` chains
    /// cascade several directories deep (a worktree under
    /// `core/engine-rs/trees/sdlc/<slug>` sits under this repo's, the tier's,
    /// and HQ's own), so every un-flagged `-p` call was leaking ambient
    /// fleet-governance prose into a local model's system prompt uninvited.
    /// Default `true` — this FIXES the verified leak rather than
    /// reproducing it: standing rule 6 only requires a new knob to be
    /// behavior-stable for a deliberate prior choice, and nobody chose
    /// today's leak on purpose.
    pub no_context_files: bool,
    /// Passes `pi --no-session`, skipping `pi`'s session JSONL persistence.
    /// **Verification, 2026-09-14:** could NOT reproduce actual session-file
    /// writes under the installed `pi` v0.5.1 in headless `-p` mode — a
    /// dozen `-p` calls (with and without `--no-session`, with and without
    /// an explicit `--session <path>`, `--mode json` and `--mode text`
    /// alike) left `~/.pi/agent/sessions/` empty every time (directory
    /// mtime unchanged across calls). So no live waste was observed to fix
    /// on this build. `--no-session` is kept anyway, defaulted `true`, as a
    /// cheap, zero-downside hygiene guard matching `pi --help`'s own
    /// documented purpose for the flag ("ephemeral" one-shot runs) —
    /// `ImplementTaskNode`'s dispatch is exactly that shape (never reads a
    /// session back), and the flag protects against a future `pi` version
    /// re-enabling session persistence for `-p` mode without this transport
    /// having to notice. Since no waste was reproduced, this default is a
    /// no-op on the currently-installed binary, not a fix — reported
    /// honestly rather than claimed as one.
    pub no_session: bool,
    /// The `pi --tools` allowlist, passed verbatim as one comma-joined
    /// value. Empty means "pass no `--tools` flag at all" (`pi`'s own
    /// built-in default set applies). Default: [`DEFAULT_PI_TOOLS`] — see
    /// its doc comment for what each dropped tool is and why.
    pub tools: Vec<String>,
}

impl Default for PiConfig {
    fn default() -> Self {
        Self {
            no_context_files: true,
            no_session: true,
            tools: DEFAULT_PI_TOOLS.iter().map(|s| (*s).to_string()).collect(),
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
            "qwen2.5:7b-instruct"
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
        assert_eq!(local.model, "qwen2.5:7b-instruct");
        assert!(!local.constrained_json);
    }

    /// The verified-leak fix (`PiConfig` doc comment): both flags default
    /// on, and the tool allowlist defaults to the scoped-down set, not
    /// `pi`'s wider built-in default.
    #[test]
    fn pi_config_default_fixes_the_verified_context_leak_and_scopes_tools() {
        let pi = PiConfig::default();
        assert!(pi.no_context_files);
        assert!(pi.no_session);
        assert_eq!(
            pi.tools,
            DEFAULT_PI_TOOLS
                .iter()
                .map(|s| (*s).to_string())
                .collect::<Vec<_>>()
        );
        // The excluded tools named in `DEFAULT_PI_TOOLS`'s doc comment must
        // actually be absent, not merely "not the whole default list".
        for excluded in ["web_search", "ask", "submit_plan", "jobs", "hub"] {
            assert!(
                !pi.tools.iter().any(|t| t == excluded),
                "default PiConfig::tools must exclude `{excluded}`: {:?}",
                pi.tools
            );
        }
    }
}
