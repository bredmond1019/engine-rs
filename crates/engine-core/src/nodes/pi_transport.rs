//! `PiTransport` (`EN.16.B` task 6) — the `AgentBackend::Pi` implementation of
//! the existing [`MetaTransport`] alias. No new trait or type alias: this
//! module only builds a closure of the shape `AgentCodeStep`/`ImplementTaskNode`
//! already accept (`with_meta_transport`), mirroring
//! [`super::openai_compat_transport`]'s shape for the `local` model tier.
//!
//! It shells to `pi -p <prompt> --mode json --approval-mode yolo --provider
//! ollama --model <local.model>`, with the resolved policy's `local.model`
//! (never a literal) as the only model/provider string beyond the
//! `--provider ollama` flag itself. `local.endpoint` has no direct `pi` CLI
//! flag on the `ollama` provider, so it is honored best-effort via the
//! `OLLAMA_HOST` environment variable the child inherits — the widely-used
//! convention for redirecting an Ollama-backed client away from the default
//! `http://localhost:11434` — rather than being silently dropped.
//!
//! `Config.cwd` (the scoped git worktree) becomes the child's
//! `current_dir`; `Config.timeout` (default [`DEFAULT_PI_TIMEOUT`], sized
//! for a shared Ollama server — see the block's `notes` "CONTENTION") wraps
//! the whole call in one [`tokio::time::timeout`], mirroring
//! `claude_code_rs::execute`'s own pattern. An optional [`CancellationToken`]
//! is raced against the same call. Either path KILLS the child — the
//! `tokio::process::Command` is built with `kill_on_drop(true)`, so dropping
//! the in-flight future (which owns the `Child`) on either a timeout or a
//! cancellation win reaps it — and the returned error's text says which of
//! the two occurred.
//!
//! # SAFETY BOUNDARY
//!
//! Named, accepted for a local model on the operator's own machine, **not
//! closed**. `--approval-mode yolo` lets the model's own output run shell,
//! file and network actions with no per-tool gate, and
//! `policy::command_floor` cannot reach it — it scopes to
//! `default_command_runner`, a structural gap. The worktree cwd is **NOT**
//! containment: a subprocess's `current_dir` only sets where relative paths
//! resolve, so the model can write anywhere the operator's user can (home
//! directory, `~/.cargo`, the HQ vault behind the worktree's `planning/`
//! symlink), use the operator's git/gh credentials, and reach the network.
//! **Never point this backend at an untrusted task description or a
//! cloud-hosted model without revisiting this.**
//!
//! # The JSON-lines parser
//!
//! `--mode json` streams one JSON event per line. Built and fixture-tested
//! against the operator's real capture,
//! `crates/engine-core/tests/fixtures/pi_transport/real_capture.jsonl`
//! (`planning/open-work/pre-plan/pluggable-code-agent-transport/evidence/pi-real-cli-run.md`
//! in the HQ vault is the exit artifact that captured it) — not the
//! project's README. That capture also surfaced a real, non-degenerate
//! failure mode this parser must survive: a well-formed `agent_end` whose
//! last `toolResult` carries a `pi.tool.approval_denied.v1` denial, because
//! `--yolo`/`--approval-mode yolo` did **not** bypass approval in `-p` mode
//! on the captured build (v0.3.0) — see that evidence file's "Finding"
//! section. A stream carrying only `message_update` deltas and no
//! `message_end` is rejected as malformed, per the same evidence's claim
//! that `message_update` is delta-only and full text arrives only on
//! `message_end`.

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use claude_code_rs::{Config, Error as ClaudeError, Outcome, Result as ClaudeResult};
use futures::future::BoxFuture;
use serde::Deserialize;
use tokio::process::Command;

use crate::cancellation::CancellationToken;
use crate::policy::LocalConfig;

use super::agent_code_step::{MetaTransport, TransportInfo};
use super::agent_outcome::{translate, AgentOutcome, CostEstimate};

/// The `pi` binary name resolved on `PATH`, absent an override (see
/// [`PI_BINARY_ENV`]).
const PI_BINARY: &str = "pi";

/// Env var that overrides which `pi` binary to spawn — mirrors
/// `claude_code_rs::execute`'s `CLAUDE_BINARY`. Tests use this to substitute
/// a fake script instead of a real `pi` install.
const PI_BINARY_ENV: &str = "PI_BINARY";

/// Whole-call wall-clock ceiling applied when `Config.timeout` carries no
/// override — generous relative to `claude_code_rs`'s own 300s default
/// because concurrent lanes share one local Ollama server and queue on it
/// (block `notes`, "CONTENTION").
const DEFAULT_PI_TIMEOUT: Duration = Duration::from_secs(600);

/// `pi`'s documented exit code for "the model could not use tools" — an
/// approval surface unavailable to a `-p` (headless) run, e.g. a tool call
/// denied because the resolved approval mode was not actually bypassed (the
/// operator's real capture hit exactly this, though as a `0`-exit denial
/// inside the stream rather than this exit code — both are handled).
/// `evidence/pi-agent-rust.md` claim 6.
const PI_EXIT_TOOL_APPROVAL_UNAVAILABLE: i32 = 3;

/// Build a [`MetaTransport`] that drives `pi_agent_rust` against `local`'s
/// resolved model instead of the `claude` CLI, honoring `cancellation` if
/// supplied (in addition to whatever cancellation the calling node's own
/// `with_cancellation_token` races the whole future against — this transport
/// checks it too so a cancellation is distinguishable, in the returned
/// error's text, from a timeout).
#[must_use]
pub fn pi_meta_transport(
    local: LocalConfig,
    cancellation: Option<CancellationToken>,
) -> MetaTransport {
    Arc::new(move |config: Config, prompt: String| {
        let local = local.clone();
        let cancellation = cancellation.clone();
        Box::pin(run_pi(config, prompt, local, cancellation))
            as BoxFuture<'static, ClaudeResult<(Outcome, TransportInfo)>>
    })
}

/// Convenience: [`pi_meta_transport`] with no cancellation token, for a
/// caller that only needs the composed node's own `with_cancellation_token`
/// race (whose drop-on-cancel still kills the child via `kill_on_drop`, just
/// without this transport's own distinguishing error text).
#[must_use]
pub fn pi_meta_transport_live(local: LocalConfig) -> MetaTransport {
    pi_meta_transport(local, None)
}

/// What the whole-call race resolved to, before the `Output` (or lack of
/// one) is turned into a `claude_code_rs::Error`.
enum RaceOutcome {
    Completed(std::io::Result<std::process::Output>),
    Cancelled,
}

async fn run_pi(
    config: Config,
    prompt: String,
    local: LocalConfig,
    cancellation: Option<CancellationToken>,
) -> ClaudeResult<(Outcome, TransportInfo)> {
    let binary = std::env::var(PI_BINARY_ENV).unwrap_or_else(|_| PI_BINARY.to_string());

    let mut command = Command::new(&binary);
    command
        .arg("-p")
        .arg(&prompt)
        .arg("--mode")
        .arg("json")
        .arg("--approval-mode")
        .arg("yolo")
        .arg("--provider")
        .arg("ollama")
        .arg("--model")
        .arg(&local.model)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    if let Some(cwd) = &config.cwd {
        command.current_dir(cwd);
    }

    // `local.endpoint` has no dedicated `pi` flag on the `ollama` provider —
    // honored best-effort via the env var Ollama-backed clients conventionally
    // read, rather than dropped on the floor.
    command.env("OLLAMA_HOST", &local.endpoint);

    if !config.env.is_empty() {
        command.envs(config.env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    }

    let child = match command.spawn() {
        Ok(child) => child,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(missing_binary_error(&binary));
        }
        Err(err) => return Err(ClaudeError::Spawn(err)),
    };

    let call = child.wait_with_output();
    let timeout_duration = config.timeout.unwrap_or(DEFAULT_PI_TIMEOUT);

    let raced = tokio::time::timeout(timeout_duration, async {
        match &cancellation {
            Some(token) => {
                tokio::select! {
                    () = token.cancelled() => RaceOutcome::Cancelled,
                    result = call => RaceOutcome::Completed(result),
                }
            }
            None => RaceOutcome::Completed(call.await),
        }
    })
    .await;

    let output = match raced {
        Err(_elapsed) => return Err(timeout_error(timeout_duration)),
        Ok(RaceOutcome::Cancelled) => return Err(cancelled_error()),
        Ok(RaceOutcome::Completed(io_result)) => io_result.map_err(ClaudeError::Spawn)?,
    };

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let exit_code = output.status.code();

    let parsed = parse_pi_stream(&stdout).map_err(|parse_err| ClaudeError::Cli {
        status: exit_code,
        stderr: format!("{parse_err} (raw stderr: {})", stderr.trim()),
    })?;

    let agent_outcome = build_agent_outcome(parsed, exit_code, &stderr);

    Ok(translate(agent_outcome, "pi"))
}

/// `Error::Spawn` naming the binary and its install command, for a spawn
/// that failed because `binary` was not found — never a spawn panic.
fn missing_binary_error(binary: &str) -> ClaudeError {
    ClaudeError::Spawn(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!(
            "`{binary}` not found on PATH. Install pi_agent_rust: \
             see https://github.com/Dicklesworthstone/pi_agent_rust#installation \
             (the operator's real capture used the project's pinned-version curl \
             installer, landing the binary at ~/.local/bin/pi)."
        ),
    ))
}

/// The call exceeded `timeout` — the child is killed by `kill_on_drop` when
/// the timed-out future (which owns it) is dropped.
fn timeout_error(timeout: Duration) -> ClaudeError {
    ClaudeError::Cli {
        status: None,
        stderr: format!(
            "pi call timed out after {timeout:?} waiting on the child process \
             (a shared local Ollama server can queue concurrent lanes) — child killed"
        ),
    }
}

/// A `CancellationToken` won the race — the child is killed by `kill_on_drop`
/// when the dropped future (which owns it) is dropped.
fn cancelled_error() -> ClaudeError {
    ClaudeError::Cli {
        status: None,
        stderr: "pi call cancelled via CancellationToken before completion — child killed"
            .to_string(),
    }
}

// -- `--mode json` event-stream parsing --

/// One `--mode json` event, internally tagged on `"type"`. Every event this
/// parser does not need its own fields for (`session`, `agent_start`,
/// `message_start`, `message_update`, `turn_start`, `turn_end`,
/// `tool_execution_start`, `tool_execution_update`, `tool_execution_end`)
/// falls into [`PiEvent::Other`] — `message_update` in particular is
/// deliberately ignored here: it is delta-only, and the full text this
/// parser reports comes from `message_end`/`agent_end` per the evidence
/// capture's claim 2.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum PiEvent {
    #[serde(rename = "message_end")]
    MessageEnd { message: PiMessage },
    #[serde(rename = "agent_end")]
    AgentEnd {
        #[serde(default)]
        messages: Vec<PiMessage>,
    },
    #[serde(rename = "error")]
    Error {
        #[serde(default)]
        phase: Option<String>,
        #[serde(default)]
        code: Option<String>,
        #[serde(rename = "exit_code", default)]
        exit_code: Option<i64>,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Default, Deserialize)]
struct PiMessage {
    #[serde(default)]
    role: String,
    #[serde(default)]
    content: PiContent,
    #[serde(default)]
    usage: Option<PiUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum PiContent {
    Text(String),
    Parts(Vec<PiContentPart>),
}

impl Default for PiContent {
    fn default() -> Self {
        PiContent::Text(String::new())
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum PiContentPart {
    #[serde(rename = "text")]
    Text {
        #[serde(default)]
        text: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct PiUsage {
    #[serde(rename = "totalTokens", default)]
    total_tokens: u64,
}

/// Concatenate every `text` part of `content`, trimmed. A plain-string
/// `content` is returned as-is (trimmed); a parts array skips `toolCall`/
/// other non-text parts, matching the real capture's assistant messages,
/// which interleave empty text parts around a `toolCall`.
fn extract_text(content: &PiContent) -> String {
    match content {
        PiContent::Text(s) => s.trim().to_string(),
        PiContent::Parts(parts) => parts
            .iter()
            .filter_map(|part| match part {
                PiContentPart::Text { text } if !text.is_empty() => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string(),
    }
}

/// What one parsed `--mode json` stream reported.
#[derive(Debug)]
struct ParsedStream {
    /// `phase`/`code`/`exit_code` from an explicit `{"type":"error",...}`
    /// event, when the stream carried one.
    error_event: Option<(Option<String>, Option<String>, Option<i64>)>,
    /// The last assistant message's text from the terminal `agent_end`
    /// event, when the stream carried one (and no `error_event`).
    final_text: String,
    /// Sum of every assistant `message_end`'s `usage.totalTokens`.
    total_tokens: u64,
}

/// Parse a `--mode json` event stream (one JSON object per line) into a
/// [`ParsedStream`], or an error string describing why the stream could not
/// be trusted.
///
/// A stream is accepted only if it carries an explicit `error` event, OR
/// both at least one `message_end` and a terminal `agent_end` — a
/// `message_update`-only stream (no `message_end` at all) is rejected,
/// per this module's doc.
fn parse_pi_stream(stdout: &str) -> Result<ParsedStream, String> {
    let mut saw_message_end = false;
    let mut saw_agent_end = false;
    let mut error_event = None;
    let mut final_text = String::new();
    let mut total_tokens: u64 = 0;

    for (idx, line) in stdout.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let event: PiEvent = serde_json::from_str(line)
            .map_err(|err| format!("malformed pi JSON-lines event at line {}: {err}", idx + 1))?;

        match event {
            PiEvent::MessageEnd { message } => {
                saw_message_end = true;
                if message.role == "assistant" {
                    if let Some(usage) = &message.usage {
                        total_tokens += usage.total_tokens;
                    }
                }
            }
            PiEvent::AgentEnd { messages } => {
                saw_agent_end = true;
                if let Some(last_assistant) = messages.iter().rev().find(|m| m.role == "assistant")
                {
                    final_text = extract_text(&last_assistant.content);
                }
            }
            PiEvent::Error {
                phase,
                code,
                exit_code,
            } => {
                error_event = Some((phase, code, exit_code));
            }
            PiEvent::Other => {}
        }
    }

    if error_event.is_none() && !(saw_message_end && saw_agent_end) {
        return Err(format!(
            "pi JSON-lines stream ended without a message_end/agent_end pair \
             (saw_message_end={saw_message_end}, saw_agent_end={saw_agent_end}) — \
             a message_update-only stream cannot be trusted (message_update is \
             delta-only; full text only ever arrives on message_end)"
        ));
    }

    Ok(ParsedStream {
        error_event,
        final_text,
        total_tokens,
    })
}

/// Turn a successfully-PARSED stream (never a malformed one — that is
/// [`parse_pi_stream`]'s `Err` path) plus the process's own exit status into
/// the [`AgentOutcome`] `translate` maps onto `(Outcome, TransportInfo)`.
///
/// Success is judged by the process's own exit code, not by stream content:
/// the operator's real capture exited `0` even though the model's only tool
/// call was denied and it merely explained the denial in prose — a real,
/// non-degenerate case task 7 (`ImplementTaskNode`) — not this transport —
/// judges via the worktree's git state, per the block's `what` (5).
fn build_agent_outcome(parsed: ParsedStream, exit_code: Option<i32>, stderr: &str) -> AgentOutcome {
    if let Some((phase, code, event_exit_code)) = &parsed.error_event {
        let text = format!(
            "pi reported a fatal error (phase={phase:?}, code={code:?}, exit_code={event_exit_code:?}). stderr: {}",
            stderr.trim()
        );
        return AgentOutcome {
            success: false,
            text,
            modified_files: Vec::new(),
            cost: Some(CostEstimate {
                tokens: parsed.total_tokens,
                dollars: None,
            }),
        };
    }

    let success = exit_code == Some(0);
    let text = if success {
        parsed.final_text
    } else if exit_code == Some(PI_EXIT_TOOL_APPROVAL_UNAVAILABLE) {
        format!(
            "pi exited {PI_EXIT_TOOL_APPROVAL_UNAVAILABLE} (approval surface unavailable — \
             the model could not use tools under the resolved approval mode). stderr: {}",
            stderr.trim()
        )
    } else {
        format!(
            "pi exited with status {exit_code:?}. stderr: {}",
            stderr.trim()
        )
    };

    AgentOutcome {
        success,
        text,
        modified_files: Vec::new(),
        cost: Some(CostEstimate {
            tokens: parsed.total_tokens,
            dollars: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Mutex;

    // Every test in this module mutates the process-global `PI_BINARY` env
    // var. `cargo nextest run` (standing rule 8) forks one process per test,
    // so this lock only guards against a stray plain `cargo test` run —
    // matches `claude_code_rs::execute`'s own `CLAUDE_BINARY_ENV_LOCK`.
    static PI_BINARY_ENV_LOCK: Mutex<()> = Mutex::new(());

    fn test_local_config() -> LocalConfig {
        LocalConfig {
            endpoint: "http://localhost:11434".to_string(),
            model: "test-model".to_string(),
            constrained_json: false,
        }
    }

    #[cfg(unix)]
    fn write_fake_binary(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let script_path = dir.path().join("fake-pi.sh");
        let mut file = std::fs::File::create(&script_path).expect("create script");
        writeln!(file, "#!/bin/sh\n{body}").expect("write script");
        drop(file);

        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod +x");

        (dir, script_path)
    }

    #[cfg(unix)]
    fn set_pi_binary(path: &std::path::Path) {
        // SAFETY: single-threaded within this test's own process (nextest
        // forks one process per test), scoped to this test.
        unsafe {
            std::env::set_var(PI_BINARY_ENV, path);
        }
    }

    #[cfg(unix)]
    fn clear_pi_binary() {
        // SAFETY: see `set_pi_binary`.
        unsafe {
            std::env::remove_var(PI_BINARY_ENV);
        }
    }

    // -- missing binary --

    #[tokio::test]
    async fn missing_binary_produces_error_naming_binary_and_install_command() {
        let _guard = PI_BINARY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: scoped to this test, serialized via the lock above.
        unsafe {
            std::env::set_var(PI_BINARY_ENV, "/definitely/not/a/real/pi/binary/xyz");
        }

        let transport = pi_meta_transport(test_local_config(), None);
        let result = transport(Config::default(), "hello".to_string()).await;

        unsafe {
            std::env::remove_var(PI_BINARY_ENV);
        }

        let err = result.expect_err("missing binary must error, not panic");
        let message = err.to_string();
        assert!(
            message.contains("/definitely/not/a/real/pi/binary/xyz"),
            "error must name the binary that was missing: {message}"
        );
        assert!(
            message.contains("install"),
            "error must name the install command: {message}"
        );
    }

    // -- JSON-lines parser --

    #[test]
    fn parser_rejects_message_update_only_stream_with_no_message_end() {
        let stream = r#"{"type":"message_update","message":{"role":"assistant","content":[{"type":"text","text":"partial"}]}}
{"type":"message_update","message":{"role":"assistant","content":[{"type":"text","text":"partial more"}]}}
"#;

        let result = parse_pi_stream(stream);

        let err = result.expect_err("message_update-only stream must fail to parse");
        assert!(
            err.contains("message_end"),
            "parse error must name what was missing: {err}"
        );
    }

    #[test]
    fn parser_accepts_the_operators_real_capture_fixture() {
        let fixture_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/pi_transport/real_capture.jsonl");
        let stream = std::fs::read_to_string(&fixture_path)
            .unwrap_or_else(|e| panic!("read fixture {}: {e}", fixture_path.display()));

        let parsed = parse_pi_stream(&stream)
            .expect("the operator's real pi-real-cli-run-raw.jsonl capture must parse");

        assert!(parsed.error_event.is_none());
        assert!(
            !parsed.final_text.is_empty(),
            "real capture's terminal agent_end carries assistant text"
        );
        assert!(
            parsed.total_tokens > 0,
            "real capture's message_end events carry non-zero usage.totalTokens"
        );
    }

    #[test]
    fn build_agent_outcome_success_on_exit_zero_uses_final_text() {
        let parsed = ParsedStream {
            error_event: None,
            final_text: "hello from the model".to_string(),
            total_tokens: 42,
        };

        let outcome = build_agent_outcome(parsed, Some(0), "");

        assert!(outcome.success);
        assert_eq!(outcome.text, "hello from the model");
        assert_eq!(outcome.cost.unwrap().tokens, 42);
    }

    #[test]
    fn build_agent_outcome_distinguishes_exit_3_approval_unavailable_from_ordinary_failure() {
        let parsed_for = |tokens| ParsedStream {
            error_event: None,
            final_text: String::new(),
            total_tokens: tokens,
        };

        let exit_3 = build_agent_outcome(parsed_for(0), Some(3), "denied");
        let exit_1 = build_agent_outcome(parsed_for(0), Some(1), "boom");

        assert!(!exit_3.success);
        assert!(!exit_1.success);
        assert!(
            exit_3.text.contains("approval"),
            "exit-3 text must name the approval-unavailable case: {}",
            exit_3.text
        );
        assert!(
            !exit_1.text.contains("approval"),
            "an ordinary exit-1 failure must not be mislabeled as approval-unavailable: {}",
            exit_1.text
        );
        assert_ne!(exit_3.text, exit_1.text);
    }

    // This block's acceptance criterion "no hardcoded local model names" is
    // checked from OUTSIDE Rust, against this file's own source text:
    // `rg -n '"qwen|llama|ollama_chat/' crates/engine-core/src/nodes/pi_transport.rs`
    // must exit 1, with `rg -n 'fn ' crates/engine-core/src/nodes/pi_transport.rs`
    // exiting 0 as the positive control. An inline `include_str!("pi_transport.rs")`
    // unit test cannot check this: the check string itself would have to spell
    // the forbidden substrings, which would then make THIS file contain them.

    // -- subprocess lifecycle: cwd, timeout-kill, cancellation-kill --

    #[cfg(unix)]
    #[tokio::test]
    async fn subprocess_runs_with_configured_cwd_as_current_dir() {
        let _guard = PI_BINARY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let (_script_dir, script) = write_fake_binary(
            r#"printf '{"type":"agent_end","messages":[{"role":"assistant","content":[{"type":"text","text":"cwd_is:%s"}]}]}\n' "$(pwd)"
printf '{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"done"}],"usage":{"totalTokens":1}}}\n'
"#,
        );
        set_pi_binary(&script);

        let cwd_dir = tempfile::tempdir().expect("cwd temp dir");
        let expected_cwd = cwd_dir.path().canonicalize().expect("canonicalize cwd");

        let config = Config {
            cwd: Some(expected_cwd.clone()),
            ..Config::default()
        };

        let transport = pi_meta_transport(test_local_config(), None);
        let result = transport(config, "prompt".to_string()).await;
        clear_pi_binary();

        let (outcome, _info) = result.expect("fake pi script run must succeed");
        assert!(
            outcome.text.contains(&expected_cwd.display().to_string()),
            "child must have run with the configured cwd: {} not found in {}",
            expected_cwd.display(),
            outcome.text
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_kills_the_child_process() {
        let _guard = PI_BINARY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let marker_dir = tempfile::tempdir().expect("marker temp dir");
        let started = marker_dir.path().join("started");
        let done = marker_dir.path().join("done");

        // Writes `started` immediately, sleeps well past the configured
        // timeout, then writes `done` — a marker that must never appear if
        // the child was actually killed rather than left to run to
        // completion in the background. `kill -0` cannot distinguish a
        // killed-but-unreaped zombie from a live process, so this proxy
        // (did the sleep's own continuation ever run) is what actually
        // proves termination.
        let (_script_dir, script) = write_fake_binary(
            r#"touch "$STARTED"
sleep 3
touch "$DONE"
"#,
        );
        set_pi_binary(&script);

        let config = Config {
            timeout: Some(Duration::from_millis(300)),
            env: vec![
                ("STARTED".to_string(), started.display().to_string()),
                ("DONE".to_string(), done.display().to_string()),
            ],
            ..Config::default()
        };

        let transport = pi_meta_transport(test_local_config(), None);
        let result = transport(config, "prompt".to_string()).await;
        clear_pi_binary();

        let err = result.expect_err("a 300ms timeout against a 3s sleep must time out");
        assert!(
            err.to_string().contains("timed out"),
            "failure text must say a timeout occurred: {err}"
        );

        wait_for_marker(&started, "child never started");
        // Give the un-killed case every chance to prove itself: wait well
        // past the 3s sleep the script would need to reach `touch "$DONE"`.
        std::thread::sleep(Duration::from_secs(4));
        assert!(
            !done.exists(),
            "child was not actually killed — it ran to completion past the timeout"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_kills_the_child_process() {
        let _guard = PI_BINARY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let marker_dir = tempfile::tempdir().expect("marker temp dir");
        let started = marker_dir.path().join("started");
        let done = marker_dir.path().join("done");

        let (_script_dir, script) = write_fake_binary(
            r#"touch "$STARTED"
sleep 3
touch "$DONE"
"#,
        );
        set_pi_binary(&script);

        let token = CancellationToken::new();
        let config = Config {
            env: vec![
                ("STARTED".to_string(), started.display().to_string()),
                ("DONE".to_string(), done.display().to_string()),
            ],
            ..Config::default()
        };

        let transport = pi_meta_transport(test_local_config(), Some(token.clone()));
        let call = transport(config, "prompt".to_string());

        let cancel_token = token.clone();
        let canceller = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(400)).await;
            cancel_token.cancel();
        });

        let result = call.await;
        canceller.await.expect("canceller task must not panic");
        clear_pi_binary();

        let err = result.expect_err("a cancelled call must error, not succeed");
        assert!(
            err.to_string().contains("cancel"),
            "failure text must say a cancellation occurred: {err}"
        );

        wait_for_marker(&started, "child never started");
        std::thread::sleep(Duration::from_secs(3));
        assert!(
            !done.exists(),
            "child was not actually killed — it ran to completion past the cancellation"
        );
    }

    #[cfg(unix)]
    fn wait_for_marker(marker: &std::path::Path, panic_message: &str) {
        // 750 * 20ms = 15s, matching the sibling helper in
        // `tests/it/agent_backend.rs` — widened from 1s (50 iterations) for
        // the same reason: under a full `--workspace --all-features` run
        // competing for CPU against thousands of other tests, spawning the
        // fake child and having it touch its marker file can take far
        // longer than in isolation. See also this test's nextest `retries`
        // override in `.config/nextest.toml`.
        for _ in 0..750 {
            if marker.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("{panic_message}: {} was never created", marker.display());
    }
}
