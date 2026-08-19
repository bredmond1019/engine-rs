//! `live_claude` (`EN.10.A` task 3) — `LiveClaudeSessionNode`: launches an
//! INTERACTIVE `claude` session inside the ALREADY-HELD tmux session
//! (`held_session.rs`, task 1), mid-run.
//!
//! # Why this is not `ClaudeCodeStep` again
//!
//! `ClaudeCodeStep`/`nodes/claude_code_step.rs` (`EN.2.A`, per `D4`) is the
//! HEADLESS transport: `claude_code_rs::execute` spawns `claude -p <prompt>
//! --output-format json` as a child of the `engine-core` process itself,
//! captures its stdout, and returns when the call is done. That subprocess
//! is never attached to any tty, so it can never appear in `bastion
//! sessions` — which reads tmux's own `list-sessions`/`pane_current_command`
//! surface (see `bastion/src/sessions/commands.rs`), not the engine's own
//! process tree.
//!
//! Block record `EN.10.A`'s task-3 acceptance criterion is exactly that
//! visibility: "the launched session is visible in a `bastion
//! sessions`-equivalent listing over the same tmux surface". The only way
//! to make `claude` show up as a session's foreground pane command is to
//! have the SHELL running inside that tmux pane start it — i.e. type the
//! invocation into the pane via [`term_core::driver::TerminalDriver`], the
//! same primitive `session::TerminalSessionNode`/`send::TerminalSendNode`
//! already use to drive a held session, exactly as a human operator would
//! type `claude` at a `bastion sessions attach` prompt.
//!
//! This module therefore adds no second SUBPROCESS-SPAWNING path for
//! Claude Code — it never calls `Command::new`/`tokio::process` itself, and
//! it never duplicates `claude_code_rs::execute`'s spawn/wait/parse
//! machinery, which stays entirely inside `core/claude-code-rs` per `D4`.
//! What it DOES reuse from the `claude_code_rs`/`claude_code_step.rs` seam
//! is the shared `claude_code_rs::Config` type (`with_config`) so a caller
//! configures model/resume/continue identically to a headless
//! `ClaudeCodeStep`, and [`resolve_binary_name`] mirrors
//! `claude_code_rs::execute`'s own (private) `CLAUDE_BINARY`-env-var-first
//! precedence exactly — necessarily duplicated rather than imported, since
//! that function is private to `claude_code_rs` and the actual PROCESS
//! START stays the shell's job here, not this module's.
//!
//! # Session identity
//!
//! Resolved through the same `session_input: InputBinding` field
//! convention `identity.rs`/`observe.rs`/`send.rs` establish — unbound
//! falls back to [`super::held_session::NODE_NAME`], the `HeldSessionNode`
//! this node is expected to run after. This node only READS the upstream
//! `session_name`; it never creates, renews, or otherwise mutates the held
//! session's identity or lease, so the held session's id is unchanged by
//! the launch (`EN.10.A` task 3 acceptance criterion).
//!
//! # What is out of scope here
//!
//! Sequencing blocks or driving `SDLC_FLOW` (`EN.10.B`); any change to the
//! marker contract (settled at v0.2.0, `EN.9.E`); replacing `bastion
//! sessions`' interactive CLI — this is its programmatic sibling, per the
//! block record's own `Out of Scope` section.

use std::sync::Arc;

use claude_code_rs::Config;
use engine_contract::TaskContext;
use serde_json::Value;
use term_core::driver::TerminalDriver;

use crate::node::{InputBinding, Node, NodeError};
use crate::workflows::{get_result, put_result};

use super::held_session;
use super::identity::HasSessionInput;

/// The `Node::name()` identity `LiveClaudeSessionNode` runs under, and the
/// `ctx.nodes` key its output is stamped onto.
pub const NODE_NAME: &str = "LiveClaudeSessionNode";

/// Resolve the `claude` binary NAME (not a path lookup — the shell inside
/// the tmux pane does its own `PATH` resolution when the typed command
/// runs) exactly the way `claude_code_rs::execute`'s private
/// `resolve_binary` does: the `CLAUDE_BINARY` env var first, else the bare
/// name `claude`. Kept as a two-line, obviously-correct mirror rather than
/// an import because `resolve_binary` is private to `claude_code_rs` and
/// this module must never own the actual subprocess spawn (see the module
/// doc's "why this is not `ClaudeCodeStep` again").
#[must_use]
fn resolve_binary_name() -> String {
    std::env::var("CLAUDE_BINARY").unwrap_or_else(|_| "claude".to_string())
}

/// Shell-quote `s` for inclusion in the command line typed into the tmux
/// pane. Simple tokens (the only shape `model`/`resume` values realistically
/// take — CLI model names, UUID-shaped session ids) pass through
/// unquoted for readability; anything else is single-quoted with embedded
/// single quotes escaped the POSIX way (`'\''`), so a value containing
/// spaces or shell metacharacters can never break out of its own argument
/// or be interpreted by the pane's shell.
fn shell_quote(s: &str) -> String {
    let is_simple = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'));
    if is_simple {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Build the INTERACTIVE argv tail from `config` — deliberately NOT
/// `claude_code_rs::Config::build_args`, which always prepends `-p
/// <prompt>` and appends `--output-format json`: that is the headless
/// contract `ClaudeCodeStep` needs and an interactive session must never
/// carry (there is no prompt to pass positionally, and JSON-only output
/// would defeat the whole point of an attached, human-usable session).
/// Only the flags that make sense for an interactive launch are carried
/// over: `--model`, `--continue`, `--resume <id>`.
fn interactive_argv(config: &Config) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(model) = &config.model {
        args.push("--model".to_string());
        args.push(model.clone());
    }
    if config.continue_session {
        args.push("--continue".to_string());
    }
    if let Some(resume) = &config.resume {
        args.push("--resume".to_string());
        args.push(resume.clone());
    }
    args
}

/// Build the full command line — resolved binary name plus the
/// interactive argv, each part shell-quoted — that gets typed into the
/// held tmux pane.
fn build_command_line(config: &Config) -> String {
    let mut parts = vec![shell_quote(&resolve_binary_name())];
    for arg in interactive_argv(config) {
        parts.push(shell_quote(&arg));
    }
    parts.join(" ")
}

/// Launches an interactive `claude` session inside the already-held tmux
/// session (`held_session::HeldSessionNode`), by typing the resolved
/// command line into the pane and pressing Enter — the programmatic
/// sibling of a human running `bastion sessions attach` and typing `claude`
/// themselves.
pub struct LiveClaudeSessionNode {
    driver: Arc<dyn TerminalDriver>,
    /// Interactive-relevant fields read off this: `model`, `continue_session`,
    /// `resume`. Every other `Config` field (prompt-shaping, tool allow/deny,
    /// `json_schema`, `dangerously_skip_permissions`, `timeout`, ...) is
    /// headless-only and has no interactive equivalent, so it is accepted
    /// here (for a caller sharing one `Config` with a sibling `ClaudeCodeStep`)
    /// but silently has no effect on the typed command — see
    /// [`interactive_argv`].
    config: Config,
    /// Resolves which `HeldSessionNode` (by `ctx.nodes` identity) this node
    /// launches into. Unbound falls back to [`held_session::NODE_NAME`].
    session_input: InputBinding,
}

impl LiveClaudeSessionNode {
    /// Construct with the given driver, a default `Config` (no model
    /// override, no resume/continue), and an unbound `session_input`
    /// (falls back to [`held_session::NODE_NAME`]).
    #[must_use]
    pub fn new(driver: Arc<dyn TerminalDriver>) -> Self {
        Self {
            driver,
            config: Config::default(),
            session_input: InputBinding::unbound(),
        }
    }

    /// Set the `claude_code_rs::Config` this node's interactive launch
    /// derives `--model`/`--continue`/`--resume` from. See the field's own
    /// doc for which `Config` fields are interactive-relevant.
    #[must_use]
    pub fn with_config(mut self, config: Config) -> Self {
        self.config = config;
        self
    }

    /// Read the upstream `HeldSessionNode`'s stored `session_name` from
    /// `ctx.nodes`, per the bound (or defaulted) `session_input` identity.
    /// Mirrors `observe.rs`/`send.rs`'s identical read.
    fn read_upstream_session(&self, ctx: &TaskContext) -> Result<String, NodeError> {
        let bound = self.session_input.resolve(held_session::NODE_NAME);
        let stored = get_result(ctx, bound).ok_or_else(|| {
            NodeError::new(format!(
                "{NODE_NAME}: no session recorded by {bound} — LiveClaudeSessionNode must run \
                 after a HeldSessionNode"
            ))
        })?;
        stored
            .get("session_name")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                NodeError::new(format!(
                    "{NODE_NAME}: {bound}'s stored result is missing `session_name`"
                ))
            })
    }
}

impl HasSessionInput for LiveClaudeSessionNode {
    fn session_input_mut(&mut self) -> &mut InputBinding {
        &mut self.session_input
    }
}

#[async_trait::async_trait]
impl Node for LiveClaudeSessionNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let session_name = self.read_upstream_session(&ctx)?;
        let command = build_command_line(&self.config);

        // `send_keys` (literal, then Enter — `TerminalDriver`'s own default
        // composition) is the exact primitive a human operator's keystrokes
        // become; typing `claude` here is indistinguishable, from tmux's
        // point of view, from a person doing it themselves at an attached
        // pane. No second start path, no `Command::new` here.
        self.driver
            .send_keys(&session_name, &command)
            .await
            .map_err(|err| {
                NodeError::new(format!(
                    "{NODE_NAME}: failed to launch interactive claude session in \
                     '{session_name}': {err}"
                ))
            })?;

        put_result(
            &mut ctx,
            self.name(),
            serde_json::json!({
                "session_name": session_name,
                "launched": true,
                "command": command,
            }),
        );

        Ok(ctx)
    }

    fn name(&self) -> &str {
        NODE_NAME
    }
}

#[cfg(test)]
mod tests {
    use super::super::identity::WithSessionInput;
    use super::*;
    use term_core::driver::{StubOutcome, StubTerminalDriver};

    fn ctx_with_upstream_session(identity: &str, session_name: &str) -> TaskContext {
        let mut ctx = TaskContext {
            event: serde_json::json!({}),
            nodes: Default::default(),
            metadata: serde_json::json!({}),
            node_runs: Default::default(),
        };
        ctx.nodes.insert(
            identity.to_string(),
            serde_json::json!({ "session_name": session_name, "created": true }),
        );
        ctx
    }

    #[test]
    fn resolve_binary_name_prefers_claude_binary_env_var() {
        // SAFETY (env mutation in a test): `cargo nextest run` forks one
        // process per test (CLAUDE.md standing rule 7), so this env var
        // mutation never leaks into a sibling test's process.
        std::env::set_var("CLAUDE_BINARY", "/opt/custom/claude");
        let resolved = resolve_binary_name();
        std::env::remove_var("CLAUDE_BINARY");
        assert_eq!(resolved, "/opt/custom/claude");
    }

    #[test]
    fn resolve_binary_name_falls_back_to_bare_claude() {
        std::env::remove_var("CLAUDE_BINARY");
        assert_eq!(resolve_binary_name(), "claude");
    }

    #[test]
    fn build_command_line_is_bare_binary_with_no_config() {
        std::env::remove_var("CLAUDE_BINARY");
        assert_eq!(build_command_line(&Config::default()), "claude");
    }

    #[test]
    fn build_command_line_carries_model_continue_and_resume() {
        std::env::remove_var("CLAUDE_BINARY");
        let config = Config {
            model: Some("claude-sonnet-4-5".to_string()),
            continue_session: true,
            resume: Some("session-123".to_string()),
            ..Config::default()
        };
        assert_eq!(
            build_command_line(&config),
            "claude --model claude-sonnet-4-5 --continue --resume session-123"
        );
    }

    #[test]
    fn build_command_line_never_carries_the_headless_p_or_output_format_flags() {
        std::env::remove_var("CLAUDE_BINARY");
        let config = Config {
            system_prompt: Some("be terse".to_string()),
            json_schema: Some(serde_json::json!({"type": "object"})),
            dangerously_skip_permissions: true,
            ..Config::default()
        };
        let line = build_command_line(&config);
        assert!(
            !line.contains("-p "),
            "interactive launch must not carry -p: {line}"
        );
        assert!(
            !line.contains("--output-format"),
            "interactive launch must not carry --output-format: {line}"
        );
    }

    #[test]
    fn shell_quote_passes_simple_tokens_through_unquoted() {
        assert_eq!(shell_quote("claude-sonnet-4-5"), "claude-sonnet-4-5");
        assert_eq!(shell_quote("session-123"), "session-123");
        assert_eq!(shell_quote("/opt/custom/claude"), "/opt/custom/claude");
    }

    #[test]
    fn shell_quote_escapes_values_with_spaces_or_metacharacters() {
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("$(rm -rf /)"), "'$(rm -rf /)'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[tokio::test]
    async fn process_sends_the_resolved_command_into_the_upstream_held_session() {
        std::env::remove_var("CLAUDE_BINARY");
        let driver = Arc::new(StubTerminalDriver::new());
        driver.set_send_literal_result(StubOutcome::empty_ok());
        driver.set_send_enter_result(StubOutcome::empty_ok());

        let node = LiveClaudeSessionNode::new(driver.clone());
        let ctx = ctx_with_upstream_session(held_session::NODE_NAME, "eng-run-1_HeldSessionNode");

        let ctx = node.process(ctx).await.expect("process should succeed");

        let stamped = ctx.nodes.get(NODE_NAME).expect("result stamped");
        assert_eq!(stamped["session_name"], "eng-run-1_HeldSessionNode");
        assert_eq!(stamped["launched"], true);
        assert_eq!(stamped["command"], "claude");

        // Held session identity is unchanged by the launch — this node
        // only reads it, never re-derives or overwrites it.
        assert_eq!(
            ctx.nodes[held_session::NODE_NAME]["session_name"],
            "eng-run-1_HeldSessionNode"
        );

        let calls = driver.calls();
        assert!(
            calls.iter().any(|argv| argv.iter().any(|a| a == "claude")),
            "expected a send-keys call carrying the literal `claude` command, got {calls:?}"
        );
    }

    #[tokio::test]
    async fn process_uses_session_input_binding_when_bound() {
        let driver = Arc::new(StubTerminalDriver::new());
        driver.set_send_literal_result(StubOutcome::empty_ok());
        driver.set_send_enter_result(StubOutcome::empty_ok());

        let node = LiveClaudeSessionNode::new(driver).with_session_input_from("CustomHeldNode");
        let ctx = ctx_with_upstream_session("CustomHeldNode", "eng-run-2_CustomHeldNode");

        let ctx = node.process(ctx).await.expect("process should succeed");
        assert_eq!(
            ctx.nodes[NODE_NAME]["session_name"],
            "eng-run-2_CustomHeldNode"
        );
    }

    #[tokio::test]
    async fn process_errors_when_no_upstream_held_session_is_recorded() {
        let driver = Arc::new(StubTerminalDriver::new());
        let node = LiveClaudeSessionNode::new(driver);
        let ctx = TaskContext {
            event: serde_json::json!({}),
            nodes: Default::default(),
            metadata: serde_json::json!({}),
            node_runs: Default::default(),
        };

        let err = node
            .process(ctx)
            .await
            .expect_err("must error with no upstream");
        assert!(err.to_string().contains(held_session::NODE_NAME));
    }

    #[tokio::test]
    async fn process_errors_when_upstream_result_is_missing_session_name() {
        let driver = Arc::new(StubTerminalDriver::new());
        let node = LiveClaudeSessionNode::new(driver);
        let mut ctx = TaskContext {
            event: serde_json::json!({}),
            nodes: Default::default(),
            metadata: serde_json::json!({}),
            node_runs: Default::default(),
        };
        ctx.nodes
            .insert(held_session::NODE_NAME.to_string(), serde_json::json!({}));

        let err = node
            .process(ctx)
            .await
            .expect_err("must error with malformed upstream result");
        assert!(err.to_string().contains("session_name"));
    }

    // --- Task 1 (EN.ticket.otel-pane-telemetry): failing-first tests for
    // the OTel env-var prefix. `build_command_line` does not yet accept a
    // run_id/node_identity, so these are expected NOT to compile until
    // task 2 changes its signature and adds the prefix.

    #[test]
    fn build_command_line_prepends_otel_env_vars_ahead_of_the_resolved_binary() {
        std::env::remove_var("CLAUDE_BINARY");
        let line = build_command_line(&Config::default(), "eng-run-1", "HeldSessionNode");
        assert_eq!(
            line,
            "CLAUDE_CODE_ENABLE_TELEMETRY=1 OTEL_METRICS_EXPORTER=otlp \
             OTEL_RESOURCE_ATTRIBUTES='run_id=eng-run-1,node.identity=HeldSessionNode' claude"
        );
    }

    #[test]
    fn otel_resource_attributes_is_value_quoted_not_assignment_quoted() {
        std::env::remove_var("CLAUDE_BINARY");
        let line = build_command_line(&Config::default(), "eng-run-1", "HeldSessionNode");
        // The value contains '=' and ',', so shell_quote's is_simple rule
        // forces single-quoting — but only around the VALUE, never around
        // the whole `VAR=value` assignment. Quoting the assignment itself
        // (e.g. `'OTEL_RESOURCE_ATTRIBUTES=run_id=...'`) would make the
        // pane's shell treat it as a literal string argument instead of an
        // env-var assignment, breaking the launch.
        assert!(
            line.contains("OTEL_RESOURCE_ATTRIBUTES='run_id=eng-run-1,node.identity=HeldSessionNode'"),
            "expected value-only quoting on OTEL_RESOURCE_ATTRIBUTES, got: {line}"
        );
        assert!(
            !line.contains("'OTEL_RESOURCE_ATTRIBUTES="),
            "must never quote the VAR= prefix together with the value: {line}"
        );
    }

    #[tokio::test]
    async fn process_never_stamps_a_usage_or_cost_usd_key() {
        std::env::remove_var("CLAUDE_BINARY");
        let driver = Arc::new(StubTerminalDriver::new());
        driver.set_send_literal_result(StubOutcome::empty_ok());
        driver.set_send_enter_result(StubOutcome::empty_ok());

        let node = LiveClaudeSessionNode::new(driver);
        let mut ctx =
            ctx_with_upstream_session(held_session::NODE_NAME, "eng-run-4_HeldSessionNode");
        ctx.metadata["run_id"] = serde_json::json!("eng-run-4");

        let ctx = node.process(ctx).await.expect("process should succeed");

        let stamped = ctx.nodes.get(NODE_NAME).expect("result stamped");
        assert!(
            stamped.get("usage").is_none(),
            "stamped result must never carry a usage key: {stamped:?}"
        );
        assert!(
            stamped.get("cost_usd").is_none(),
            "stamped result must never carry a cost_usd key: {stamped:?}"
        );
    }

    #[tokio::test]
    async fn process_surfaces_a_node_error_on_driver_send_failure() {
        let driver = Arc::new(StubTerminalDriver::new());
        driver.set_send_literal_result(StubOutcome::NoServer);

        let node = LiveClaudeSessionNode::new(driver);
        let ctx = ctx_with_upstream_session(held_session::NODE_NAME, "eng-run-3_HeldSessionNode");

        let err = node
            .process(ctx)
            .await
            .expect_err("driver failure must surface as a node error");
        assert!(err.to_string().contains("eng-run-3_HeldSessionNode"));
    }
}
