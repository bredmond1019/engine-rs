//! Integration suite for `EN.16.B` (`AgentBackend::Pi`) — task 9.
//!
//! Every unit-level piece of this block (`translate` in `agent_outcome.rs`,
//! `PiTransport`'s parser/lifecycle in `pi_transport.rs`, the two
//! registration-path dispatch tests in `sdlc_task/graph.rs`, the
//! git-derived `modified_files` tests in `sdlc_flow/task_loop.rs`) already
//! has its own `#[cfg(test)]` coverage colocated with the code it tests.
//! This module exists to prove those pieces work TOGETHER, through the
//! crate's public API only (`engine_core::...`), the way a real
//! `agent_backend: pi` run actually composes them — in particular the
//! cost-honesty chain `PiTransport`'s translation feeds:
//! `AgentCodeStep` -> `BudgetLedger`/`sessions::ledger_totals` ->
//! `RunTelemetry` (`agent_backend_pi_dollars_unknown_is_unknown_on_every_channel`).
//!
//! Every test name is prefixed `agent_backend_` so
//! `cargo nextest run -p engine-core -E 'test(agent_backend_)'` selects
//! exactly this suite.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use claude_code_rs::{Config, Outcome};
use engine_contract::TaskContext;
use engine_core::nodes::{pi_meta_transport, translate_agent_outcome, AgentOutcome, CostEstimate};
use engine_core::policy::telemetry::{harvest as harvest_telemetry, RunTelemetryInputs};
use engine_core::policy::{AgentBackend, LocalConfig, RESOLVED_POLICY_IDENTITY};
use engine_core::sessions;
use engine_core::workflows::sdlc_flow::policy::SdlcPolicy;
use engine_core::workflows::sdlc_flow::schema::{SDLCState, SDLCTask};
use engine_core::workflows::sdlc_flow::task_loop::{ImplementTaskNode, TestTaskNode};
use engine_core::workflows::sdlc_task::graph::{
    registry_for_policy, registry_for_policy_with_cancellation,
};
use engine_core::workflows::sdlc_task::policy::SdlcTaskPolicy;
use engine_core::workflows::{CommandOutput, CommandRunner, ModelTransport};
use engine_core::{
    BudgetLedger, CancellationToken, Node, NodeConfig, NodeRegistry, Workflow, WorkflowSchema,
};

/// Serializes every test in this module that mutates the process-global
/// `PI_BINARY` env var. `cargo nextest run` (standing rule 8) forks one
/// process per test, so this only guards a stray plain `cargo test` run —
/// mirrors `pi_transport.rs`'s own `PI_BINARY_ENV_LOCK` and
/// `graph.rs`'s `BACKEND_ENV_LOCK`.
static PI_BINARY_ENV_LOCK: Mutex<()> = Mutex::new(());

/// A fake `pi` script body that prints one `agent_end`/`message_end` pair
/// with known token usage on stdout — enough for
/// [`crate::pi_transport::parse_pi_stream`] (exercised here only
/// indirectly, through the public transport) to accept. Uses `printf`
/// rather than a literal here-string so the JSON's own braces/quotes are
/// never interpreted as shell syntax.
const SUCCESS_STREAM: &str = r#"printf '{"type":"agent_end","messages":[{"role":"assistant","content":[{"type":"text","text":"hi"}]}]}\n'
printf '{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"done"}],"usage":{"totalTokens":5}}}\n'
"#;

#[cfg(unix)]
fn write_fake_pi_binary(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let script_path = dir.path().join("fake-pi.sh");
    std::fs::write(&script_path, format!("#!/bin/sh\n{body}\n")).expect("write script");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod +x");
    (dir, script_path)
}

fn local_config() -> LocalConfig {
    LocalConfig::default()
}

/// Minimal `ctx` `ImplementTaskNode::process` needs when dispatched through
/// SDLC_TASK's registry: a durable `SDLCState` carrying one task
/// (`LoadTaskStateNode`), that task dequeued (`TaskQueueRouterNode`), and a
/// resolved `SdlcPolicy` stamp (`RESOLVED_POLICY_IDENTITY`) — mirrors
/// `sdlc_task/graph.rs`'s own private `ctx_for_implement` test helper,
/// reconstructed here from public types only since that helper is not
/// reachable from an integration binary.
fn sdlc_task_ctx(agent_backend: AgentBackend) -> TaskContext {
    let task = SDLCTask::new(1, "One", "d1");
    let mut state = SDLCState::new("EN.16.B-task9-it");
    state.tasks = vec![task.clone()];

    let mut nodes = HashMap::new();
    nodes.insert(
        "LoadTaskStateNode".to_string(),
        serde_json::to_value(&state).expect("SDLCState serializes"),
    );
    nodes.insert(
        "TaskQueueRouterNode".to_string(),
        serde_json::json!({
            "current_task_id": task.task_id,
            "title": task.title,
            "description": task.description,
            "acceptance_criteria": task.acceptance_criteria,
            "attempt_count": task.attempt_count,
            "max_attempts": task.max_attempts,
        }),
    );
    let policy = SdlcPolicy {
        agent_backend,
        ..SdlcPolicy::default()
    };
    nodes.insert(
        RESOLVED_POLICY_IDENTITY.to_string(),
        serde_json::to_value(&policy).expect("SdlcPolicy serializes"),
    );

    TaskContext {
        event: serde_json::json!({}),
        nodes,
        metadata: serde_json::json!({}),
        node_runs: HashMap::new(),
    }
}

/// Same shape as [`sdlc_task_ctx`] but for `sdlc_flow::task_loop`'s
/// standalone `ImplementTaskNode`/`TestTaskNode` (used by tests 10/11,
/// which never go through a `NodeRegistry`), with a `SetupWorktreeNode`
/// entry pointing at `worktree`.
fn task_loop_ctx(agent_backend: AgentBackend, worktree: &Path) -> TaskContext {
    let mut ctx = sdlc_task_ctx(agent_backend);
    ctx.nodes.insert(
        "SetupWorktreeNode".to_string(),
        serde_json::json!({ "worktree_path": worktree.to_string_lossy() }),
    );
    ctx
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "engine-core-agent-backend-it-{tag}-{}-{n}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("planning")).expect("create worktree dir");
    dir
}

fn write_empty_harness(dir: &Path) {
    let harness = serde_json::json!({ "validation": { "checks": [] } });
    std::fs::write(
        dir.join("planning").join("harness.json"),
        serde_json::to_string(&harness).unwrap(),
    )
    .unwrap();
}

/// A `CommandRunner` stub whose `git status --porcelain` reports
/// `status_lines` and every other command a no-op success — mirrors
/// `task_loop.rs`'s private `porcelain_runner` test helper.
fn porcelain_runner(status_lines: &'static str) -> CommandRunner {
    Arc::new(move |program, args, _cwd| {
        if program == "git" && args.first() == Some(&"status") {
            Ok(CommandOutput {
                status: 0,
                stdout: status_lines.to_string(),
                stderr: String::new(),
            })
        } else {
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    })
}

/// Poll for `marker` to appear, up to 5s — generous relative to
/// `pi_transport.rs`'s own 1s bound because this suite's kill tests run
/// alongside many concurrent test processes (nextest forks one per test),
/// and subprocess spawn latency under that contention is not this test's
/// concern.
#[cfg(unix)]
fn wait_for_marker(marker: &Path, panic_message: &str) {
    for _ in 0..250 {
        if marker.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("{panic_message}: {} was never created", marker.display());
}

fn canned_outcome(text: String) -> Outcome {
    Outcome {
        cost_usd: 0.0,
        usage: claude_code_rs::parse::Usage {
            input_tokens: 1,
            output_tokens: 1,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
        model_usage: std::collections::BTreeMap::new(),
        text,
        is_error: false,
        api_error_status: None,
        session_id: None,
        structured_output: None,
    }
}

// -- dispatch: both SDLC_TASK registration paths -----------------------

/// `agent_backend: pi` via [`registry_for_policy_with_cancellation`]
/// (`token: None`) must dispatch `PiTransport` — `ImplementTaskNode`'s
/// stamped `transport.backend` reads `"pi"`, not the base registry's
/// billed `claude_cli` node.
#[cfg(unix)]
#[tokio::test]
async fn agent_backend_task_dispatch_wires_pi_transport() {
    let _guard = PI_BINARY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_dir, script) = write_fake_pi_binary(SUCCESS_STREAM);
    // SAFETY: single-threaded within this test's own process (`cargo
    // nextest run` forks one process per test), scoped to this test and
    // serialized via `PI_BINARY_ENV_LOCK`.
    unsafe {
        std::env::set_var("PI_BINARY", &script);
    }

    let policy = SdlcTaskPolicy {
        agent_backend: AgentBackend::Pi,
        ..SdlcTaskPolicy::default()
    };
    let registry = registry_for_policy_with_cancellation(&policy, None);
    let node = registry
        .get("ImplementTaskNode")
        .expect("ImplementTaskNode registered");
    let ctx = sdlc_task_ctx(AgentBackend::Pi);
    let result = node.process(ctx).await;

    unsafe {
        std::env::remove_var("PI_BINARY");
    }

    let out = result.expect("process should succeed against the fake pi script");
    assert_eq!(
        out.nodes["ImplementTaskNode"]["transport"]["backend"],
        serde_json::json!("pi"),
        "agent_backend: pi must dispatch PiTransport — got {out:#?}"
    );
}

/// The SAME `agent_backend: pi` policy through [`registry_for_policy`] —
/// the actual production call site
/// (`orchestration::execute::EngineKind::Task`) — with no cancellation
/// token at all. Before task 8's fix, the re-registration was gated on
/// `token.is_some()` alone, so this exact `token: None` shape silently
/// kept the base registry's billed `claude_cli` node. Would fail if that
/// regressed.
#[cfg(unix)]
#[tokio::test]
async fn agent_backend_task_token_none_does_not_fall_back_to_claude() {
    let _guard = PI_BINARY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_dir, script) = write_fake_pi_binary(SUCCESS_STREAM);
    unsafe {
        std::env::set_var("PI_BINARY", &script);
    }

    let policy = SdlcTaskPolicy {
        agent_backend: AgentBackend::Pi,
        ..SdlcTaskPolicy::default()
    };
    let registry = registry_for_policy(&policy);
    let node = registry
        .get("ImplementTaskNode")
        .expect("ImplementTaskNode registered");
    let ctx = sdlc_task_ctx(AgentBackend::Pi);
    let result = node.process(ctx).await;

    unsafe {
        std::env::remove_var("PI_BINARY");
    }

    let out = result.expect("process should succeed against the fake pi script");
    assert_eq!(
        out.nodes["ImplementTaskNode"]["transport"]["backend"],
        serde_json::json!("pi"),
        "token: None must not fall back to the billed claude_cli node — got {out:#?}"
    );
}

// -- PiTransport subprocess lifecycle, through the public seam ----------

#[cfg(unix)]
#[tokio::test]
async fn agent_backend_pi_transport_uses_worktree_cwd() {
    let _guard = PI_BINARY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_dir, script) = write_fake_pi_binary(
        r#"printf '{"type":"agent_end","messages":[{"role":"assistant","content":[{"type":"text","text":"cwd_is:%s"}]}]}\n' "$(pwd)"
printf '{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"done"}],"usage":{"totalTokens":1}}}\n'
"#,
    );
    unsafe {
        std::env::set_var("PI_BINARY", &script);
    }

    let cwd_dir = tempfile::tempdir().expect("cwd temp dir");
    let expected_cwd = cwd_dir.path().canonicalize().expect("canonicalize cwd");

    let config = Config {
        cwd: Some(expected_cwd.clone()),
        ..Config::default()
    };

    let transport = pi_meta_transport(local_config(), None);
    let result = transport(config, "prompt".to_string()).await;

    unsafe {
        std::env::remove_var("PI_BINARY");
    }

    let (outcome, _info) = result.expect("fake pi script run must succeed");
    assert!(
        outcome.text.contains(&expected_cwd.display().to_string()),
        "child must have run with the configured worktree cwd: {} not found in {}",
        expected_cwd.display(),
        outcome.text
    );
}

#[cfg(unix)]
#[tokio::test]
async fn agent_backend_pi_transport_kills_child_on_timeout() {
    let _guard = PI_BINARY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let marker_dir = tempfile::tempdir().expect("marker temp dir");
    let started = marker_dir.path().join("started");
    let done = marker_dir.path().join("done");

    let (_dir, script) = write_fake_pi_binary(
        r#"touch "$STARTED"
sleep 3
touch "$DONE"
"#,
    );
    unsafe {
        std::env::set_var("PI_BINARY", &script);
    }

    let config = Config {
        timeout: Some(Duration::from_millis(300)),
        env: vec![
            ("STARTED".to_string(), started.display().to_string()),
            ("DONE".to_string(), done.display().to_string()),
        ],
        ..Config::default()
    };

    let transport = pi_meta_transport(local_config(), None);
    let result = transport(config, "prompt".to_string()).await;

    unsafe {
        std::env::remove_var("PI_BINARY");
    }

    let err = result.expect_err("a 300ms timeout against a 3s sleep must time out");
    assert!(
        err.to_string().contains("timed out"),
        "failure text must say a timeout occurred: {err}"
    );

    wait_for_marker(&started, "child never started");

    // Give the un-killed case every chance to prove itself.
    std::thread::sleep(Duration::from_secs(4));
    assert!(
        !done.exists(),
        "child was not actually killed — it ran to completion past the timeout"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn agent_backend_pi_transport_kills_child_on_cancel() {
    let _guard = PI_BINARY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let marker_dir = tempfile::tempdir().expect("marker temp dir");
    let started = marker_dir.path().join("started");
    let done = marker_dir.path().join("done");

    let (_dir, script) = write_fake_pi_binary(
        r#"touch "$STARTED"
sleep 8
touch "$DONE"
"#,
    );
    unsafe {
        std::env::set_var("PI_BINARY", &script);
    }

    let token = CancellationToken::new();
    let config = Config {
        env: vec![
            ("STARTED".to_string(), started.display().to_string()),
            ("DONE".to_string(), done.display().to_string()),
        ],
        ..Config::default()
    };

    let transport = pi_meta_transport(local_config(), Some(token.clone()));
    let call = transport(config, "prompt".to_string());

    let cancel_token = token.clone();
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        cancel_token.cancel();
    });

    let result = call.await;
    canceller.await.expect("canceller task must not panic");

    unsafe {
        std::env::remove_var("PI_BINARY");
    }

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
#[tokio::test]
async fn agent_backend_missing_pi_binary_returns_node_error() {
    let _guard = PI_BINARY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        std::env::set_var("PI_BINARY", "/definitely/not/a/real/pi/binary/xyz");
    }

    let transport = pi_meta_transport(local_config(), None);
    let result = transport(Config::default(), "hello".to_string()).await;

    unsafe {
        std::env::remove_var("PI_BINARY");
    }

    let err = result.expect_err("a missing pi binary must error, not panic");
    let message = err.to_string();
    assert!(
        message.contains("/definitely/not/a/real/pi/binary/xyz"),
        "error must name the missing binary: {message}"
    );
    assert!(
        message.contains("install"),
        "error must name the install command: {message}"
    );
}

/// The parser's fixture is the operator's REAL `pi --mode json` capture,
/// recorded at
/// `planning/open-work/pre-plan/pluggable-code-agent-transport/evidence/pi-real-cli-run.md`
/// (HQ vault) and committed alongside this suite as
/// `crates/engine-core/tests/fixtures/pi_transport/real_capture.jsonl` — not
/// a hand-written stream. `PI_BINARY` is stubbed to `cat` it verbatim to
/// stdout so this test drives the same public transport seam as every
/// other test here, not the crate-private parser function directly.
#[cfg(unix)]
#[tokio::test]
async fn agent_backend_pi_parses_real_capture_fixture() {
    let _guard = PI_BINARY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let fixture_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/pi_transport/real_capture.jsonl");
    assert!(
        fixture_path.exists(),
        "the operator's real capture fixture must exist at {}",
        fixture_path.display()
    );

    let (_dir, script) = write_fake_pi_binary(&format!("cat '{}'", fixture_path.display()));
    unsafe {
        std::env::set_var("PI_BINARY", &script);
    }

    let transport = pi_meta_transport(local_config(), None);
    let result = transport(Config::default(), "prompt".to_string()).await;

    unsafe {
        std::env::remove_var("PI_BINARY");
    }

    let (outcome, info) = result.expect("the operator's real capture must parse and succeed");
    assert!(
        !outcome.text.is_empty(),
        "the real capture's terminal agent_end must produce non-empty text"
    );
    assert!(
        outcome.usage.output_tokens > 0,
        "the real capture's message_end events carry non-zero usage.totalTokens"
    );
    assert!(
        !info.cost_known,
        "PiTransport never reports a real dollar cost"
    );
}

/// `pi`'s exit code `3` ("approval surface unavailable") must be
/// distinguishable from an ordinary non-zero failure — both fail the
/// invocation, but only exit 3's failure text names the approval case, so
/// a caller (or an operator reading the record) can tell the two apart.
#[cfg(unix)]
#[tokio::test]
async fn agent_backend_pi_always_ask_exit_3_is_distinguishable() {
    let _guard = PI_BINARY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let (_dir3, script_3) = write_fake_pi_binary(
        r#"printf '{"type":"agent_end","messages":[]}\n'
printf '{"type":"message_end","message":{"role":"assistant","content":"","usage":{"totalTokens":0}}}\n'
exit 3
"#,
    );
    unsafe {
        std::env::set_var("PI_BINARY", &script_3);
    }
    let transport = pi_meta_transport(local_config(), None);
    let (exit_3_outcome, _) = transport(Config::default(), "prompt".to_string())
        .await
        .expect("exit 3 still parses and returns a failed Outcome, not a transport Err");
    unsafe {
        std::env::remove_var("PI_BINARY");
    }

    let (_dir1, script_1) = write_fake_pi_binary(
        r#"printf '{"type":"agent_end","messages":[]}\n'
printf '{"type":"message_end","message":{"role":"assistant","content":"","usage":{"totalTokens":0}}}\n'
exit 1
"#,
    );
    unsafe {
        std::env::set_var("PI_BINARY", &script_1);
    }
    let transport = pi_meta_transport(local_config(), None);
    let (exit_1_outcome, _) = transport(Config::default(), "prompt".to_string())
        .await
        .expect("exit 1 also parses and returns a failed Outcome");
    unsafe {
        std::env::remove_var("PI_BINARY");
    }

    assert!(exit_3_outcome.is_error);
    assert!(exit_1_outcome.is_error);
    assert!(
        exit_3_outcome.text.contains("approval"),
        "exit-3 text must name the approval-unavailable case: {}",
        exit_3_outcome.text
    );
    assert!(
        !exit_1_outcome.text.contains("approval"),
        "an ordinary exit-1 failure must not be mislabeled as approval-unavailable: {}",
        exit_1_outcome.text
    );
    assert_ne!(exit_3_outcome.text, exit_1_outcome.text);
}

// -- cost honesty: the REAL chain -----------------------------------
// PiTransport's translation (`translate_agent_outcome`, the exact function
// `PiTransport` itself calls) -> `AgentCodeStep` -> `BudgetLedger` and
// `sessions::ledger_totals` -> `RunTelemetry`.

const IMPLEMENT_NODE: &str = "ImplementTaskNode";

fn single_node_schema() -> WorkflowSchema {
    let mut nodes = HashMap::new();
    nodes.insert(
        IMPLEMENT_NODE.to_string(),
        NodeConfig::new(IMPLEMENT_NODE, vec![]),
    );
    WorkflowSchema::new("agent-backend-cost-chain", IMPLEMENT_NODE, nodes)
}

/// Run one `AgentCodeStep` through a real `Workflow`, with its meta
/// transport answering via `translate_agent_outcome` (the SAME function
/// `PiTransport::run_pi` calls) for an `AgentOutcome` reporting `tokens`
/// known and `dollars`, returning the resulting `TaskContext`.
async fn run_pi_shaped_outcome(tokens: u64, dollars: Option<f64>) -> TaskContext {
    use engine_core::AgentCodeStep;

    let step = engine_core::NodeExt::with_identity(
        AgentCodeStep::new(IMPLEMENT_NODE, Config::default(), "do the thing").with_meta_transport(
            move |_config, _prompt| {
                let outcome = translate_agent_outcome(
                    AgentOutcome {
                        success: true,
                        text: "did the thing".to_string(),
                        modified_files: vec![],
                        cost: Some(CostEstimate { tokens, dollars }),
                    },
                    "pi",
                );
                Box::pin(async move { Ok(outcome) })
            },
        ),
        IMPLEMENT_NODE,
    );

    let mut registry = NodeRegistry::new();
    registry.register(Box::new(step));

    let workflow = Workflow::new(registry, single_node_schema());
    let on_progress: engine_core::OnProgress<'_> = Box::new(|_c: &TaskContext| {});
    workflow
        .run(serde_json::json!({}), on_progress)
        .await
        .expect("workflow should complete successfully")
}

/// Through the real chain, a Pi outcome with tokens known and dollars
/// UNKNOWN must be unknown on every channel that records spend: `cost_usd`
/// absent from `ctx.nodes`, `BudgetLedger`'s unknown-cost flag set, the
/// session ledger's `cost_known: false`, `RunTelemetry`'s
/// `unknown_cost_invocations >= 1`, and tokens still counted. Re-running
/// with `dollars: Some(0.0)` — a REAL zero, not an unknown — must clear
/// every one of those flags; that re-run is what proves this test tells
/// the two cases apart rather than always reporting "unknown".
#[tokio::test]
async fn agent_backend_pi_dollars_unknown_is_unknown_on_every_channel() {
    let unknown_ctx = run_pi_shaped_outcome(123, None).await;

    assert!(
        unknown_ctx.nodes[IMPLEMENT_NODE].get("cost_usd").is_none(),
        "cost_usd must be ABSENT from ctx.nodes when dollars is unknown: {:#?}",
        unknown_ctx.nodes[IMPLEMENT_NODE]
    );

    let ledger = BudgetLedger::from_context(&unknown_ctx);
    assert!(
        ledger.has_unknown_cost_node(),
        "BudgetLedger must flag the unknown-cost node"
    );

    let sessions = sessions::read_sessions(&unknown_ctx.metadata);
    assert_eq!(sessions.len(), 1, "one invocation must be ledgered");
    assert!(
        !sessions[0].cost_known,
        "the session ledger entry must record cost_known: false"
    );

    let totals = sessions::ledger_totals(&unknown_ctx.metadata);
    assert_eq!(totals.unknown_cost_invocations, 1);
    assert_eq!(
        totals.output_tokens, 123,
        "an unknown dollar cost must never suppress the known token count"
    );

    let telemetry = harvest_telemetry(
        &unknown_ctx,
        chrono::Utc::now(),
        RunTelemetryInputs::default(),
    );
    assert!(
        telemetry.unknown_cost_invocations >= 1,
        "RunTelemetry must surface at least one unknown-cost invocation: {telemetry:#?}"
    );
    assert_eq!(telemetry.total_output_tokens, 123);

    // Re-run with a REAL zero: every flag above must now read as known.
    let known_zero_ctx = run_pi_shaped_outcome(123, Some(0.0)).await;

    assert!(
        known_zero_ctx.nodes[IMPLEMENT_NODE]
            .get("cost_usd")
            .is_some(),
        "cost_usd must be PRESENT (as 0.0) when dollars is a real zero: {:#?}",
        known_zero_ctx.nodes[IMPLEMENT_NODE]
    );
    assert!(
        !BudgetLedger::from_context(&known_zero_ctx).has_unknown_cost_node(),
        "a real $0.00 must not be flagged as unknown-cost"
    );
    let zero_sessions = sessions::read_sessions(&known_zero_ctx.metadata);
    assert!(zero_sessions[0].cost_known);
    assert_eq!(
        sessions::ledger_totals(&known_zero_ctx.metadata).unknown_cost_invocations,
        0
    );
    let zero_telemetry = harvest_telemetry(
        &known_zero_ctx,
        chrono::Utc::now(),
        RunTelemetryInputs::default(),
    );
    assert_eq!(
        zero_telemetry.unknown_cost_invocations, 0,
        "dollars: Some(0.0) must clear the unknown-cost count — this is what \
         proves the test tells the two cases apart"
    );
}

// -- what counts as success / write-verification ------------------------

/// A Pi reply in plain PROSE (no schema-constrained JSON) must not fail
/// `ImplementTaskNode`, and `modified_files` must come from the worktree's
/// own git state (via the injected `CommandRunner`), not the model's
/// self-report — `AgentBackend::Pi`'s reply is not schema-constrained the
/// way `claude_cli`'s is (task 7).
#[tokio::test]
async fn agent_backend_pi_prose_reply_with_edits_is_not_a_failure() {
    let worktree = temp_dir("prose-reply");
    let ctx = task_loop_ctx(AgentBackend::Pi, &worktree);

    let prose_transport: ModelTransport = Arc::new(|_config, _prompt| {
        let outcome =
            canned_outcome("I edited the file and everything works now, no JSON here.".to_string());
        Box::pin(async move { Ok(outcome) })
    });

    let node = ImplementTaskNode::new()
        .with_transport(prose_transport)
        .with_runner(porcelain_runner("M  src/real_change.rs\n"));
    let out = node
        .process(ctx)
        .await
        .expect("a non-JSON prose reply must not fail this node");

    assert_eq!(
        out.nodes["ImplementTaskNode"]["summary"],
        serde_json::json!("I edited the file and everything works now, no JSON here."),
        "the text fallback must still supply summary from the raw reply"
    );
    assert_eq!(
        out.nodes["ImplementTaskNode"]["modified_files"],
        serde_json::json!(["src/real_change.rs"]),
        "modified_files must come from the worktree's git state for a \
         non-claude_cli backend"
    );

    std::fs::remove_dir_all(&worktree).ok();
}

/// A Pi run that edits NOTHING (an empty `git status --porcelain`) must
/// fail `TestTaskNode`'s write-verification guard — the model's own
/// self-report is documented-unreliable for a non-`claude_cli` backend
/// (task 5's whole point), so an empty worktree is the only signal that
/// can be trusted, and it must trip the guard exactly as it does for
/// `claude_cli` (`write_verification_fires_on_empty_claim_and_clean_worktree`).
#[tokio::test]
async fn agent_backend_pi_no_edits_fails_write_verification() {
    let worktree = temp_dir("no-edits");
    write_empty_harness(&worktree);

    // Step 1: ImplementTaskNode (Pi backend) against a clean worktree ->
    // modified_files derived from git state is empty.
    let implement_ctx = task_loop_ctx(AgentBackend::Pi, &worktree);
    let implement_transport: ModelTransport = Arc::new(|_config, _prompt| {
        let outcome = canned_outcome(
            serde_json::json!({
                "summary": "did nothing useful",
                "modified_files": ["src/claimed_but_never_touched.rs"],
                "tests_added": [],
            })
            .to_string(),
        );
        Box::pin(async move { Ok(outcome) })
    });
    let implement_node = ImplementTaskNode::new()
        .with_transport(implement_transport)
        .with_runner(porcelain_runner(""));
    let after_implement = implement_node
        .process(implement_ctx)
        .await
        .expect("ImplementTaskNode process should succeed");
    assert_eq!(
        after_implement.nodes["ImplementTaskNode"]["modified_files"],
        serde_json::json!([]),
        "a non-claude_cli backend against a clean worktree must report no \
         modified files, ignoring the model's own claim"
    );

    // Step 2: TestTaskNode's write-verification guard, against the same
    // clean worktree, must fail the task.
    let test_node = TestTaskNode::new().with_runner(porcelain_runner(""));
    let out = test_node
        .process(after_implement)
        .await
        .expect("TestTaskNode process should succeed (the FAILURE is in its output, not an Err)");

    assert_eq!(out.nodes["TestTaskNode"]["all_passed"], false);
    let results = out.nodes["TestTaskNode"]["check_results"]
        .as_array()
        .expect("check_results is an array");
    assert_eq!(results[0]["kind"], "write-verification");
    assert_eq!(results[0]["passed"], false);

    std::fs::remove_dir_all(&worktree).ok();
}
