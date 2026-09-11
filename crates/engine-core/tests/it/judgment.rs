//! Integration tests for `JudgmentNode` (`EN.17.D` task 1).
//!
//! Hermetic (gated) tests stub the transport so the gated suite never
//! spawns a real `claude` subprocess. Every test name is prefixed
//! `judgment_` per the task's testing strategy.
//!
//! The two `#[ignore]`d tests at the bottom make a real `claude` call and
//! are DECLARED UN-GATEABLE (D64) — run once by hand via:
//!
//! ```sh
//! cargo nextest run -p engine-core --run-ignored ignored-only judgment_live
//! ```

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use claude_code_rs::parse::Usage as SdkUsage;
use claude_code_rs::{Config, Outcome};
use engine_contract::TaskContext;
use engine_core::nodes::{InputSlice, JudgmentError, JudgmentNode, JudgmentSpec};
use engine_core::policy::ModelTier;
use engine_core::workflows::ModelTransport;
use futures::future::BoxFuture;
use futures::FutureExt;
use serde::Deserialize;
use serde_json::json;

/// The schema type every test judges against: a minimal `{verdict: bool}`
/// shape, enough to exercise schema-match vs. schema-violation without
/// pulling in a real consumer's schema.
#[derive(Debug, Clone, PartialEq, Deserialize)]
struct TestVerdict {
    verdict: bool,
}

fn test_json_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": { "verdict": { "type": "boolean" } },
        "required": ["verdict"],
    })
}

fn empty_ctx() -> TaskContext {
    TaskContext {
        event: json!({}),
        nodes: HashMap::new(),
        metadata: json!({}),
        node_runs: HashMap::new(),
    }
}

fn base_spec(slices: Vec<InputSlice>) -> JudgmentSpec {
    JudgmentSpec {
        identity: "TestJudgment".to_string(),
        json_schema: test_json_schema(),
        stable_prompt: "Judge the following slices.",
        slices,
        tier: ModelTier::Haiku,
        max_turns: Some(4),
    }
}

fn stub_outcome(text: &str, structured: Option<serde_json::Value>) -> Outcome {
    Outcome {
        cost_usd: 0.01,
        usage: SdkUsage {
            input_tokens: 10,
            output_tokens: 5,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
        model_usage: BTreeMap::new(),
        text: text.to_string(),
        is_error: false,
        api_error_status: None,
        session_id: Some("sess-1".to_string()),
        structured_output: structured,
    }
}

/// A stub transport that always returns the same `Outcome`, and captures
/// the last `Config`/prompt it was called with.
fn stub_success(
    outcome: Outcome,
    captured: Arc<Mutex<Option<(Config, String)>>>,
) -> ModelTransport {
    Arc::new(move |config: Config, prompt: String| {
        *captured.lock().unwrap() = Some((config.clone(), prompt.clone()));
        let outcome = outcome.clone();
        async move { Ok(outcome) }.boxed() as BoxFuture<'static, claude_code_rs::Result<Outcome>>
    })
}

fn stub_error(err: fn() -> claude_code_rs::Error) -> ModelTransport {
    Arc::new(move |_config: Config, _prompt: String| {
        async move { Err(err()) }.boxed() as BoxFuture<'static, claude_code_rs::Result<Outcome>>
    })
}

// ---------------------------------------------------------------------------
// judgment_sets_schema_tier_and_max_turns
// ---------------------------------------------------------------------------

#[tokio::test]
async fn judgment_sets_schema_tier_and_max_turns() {
    let captured: Arc<Mutex<Option<(Config, String)>>> = Arc::new(Mutex::new(None));
    let outcome = stub_outcome(
        &serde_json::to_string(&json!({ "verdict": true })).unwrap(),
        Some(json!({ "verdict": true })),
    );
    let node: JudgmentNode<TestVerdict> =
        JudgmentNode::new().with_transport(stub_success(outcome, captured.clone()));

    let spec = base_spec(vec![]);
    let result = node
        .judge(&empty_ctx(), spec)
        .await
        .expect("judge succeeds");
    assert!(result.verdict.verdict);
    assert_eq!(result.tier, ModelTier::Haiku);
    assert_eq!(result.max_turns, Some(4));

    let (config, _prompt) = captured.lock().unwrap().take().expect("transport called");
    assert_eq!(config.json_schema, Some(test_json_schema()));
    assert_eq!(config.max_turns, Some(4));
    assert_eq!(config.model.as_deref(), Some("claude-haiku-4-5"));
}

// ---------------------------------------------------------------------------
// Slice truncation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn judgment_oversize_slice_is_truncated_with_marker() {
    let captured: Arc<Mutex<Option<(Config, String)>>> = Arc::new(Mutex::new(None));
    let outcome = stub_outcome("", Some(json!({ "verdict": true })));
    let node: JudgmentNode<TestVerdict> =
        JudgmentNode::new().with_transport(stub_success(outcome, captured.clone()));

    let long_text = "x".repeat(100);
    let spec = base_spec(vec![InputSlice {
        name: "what".to_string(),
        text: long_text.clone(),
        max_bytes: 10,
    }]);

    let result = node
        .judge(&empty_ctx(), spec)
        .await
        .expect("judge succeeds");

    assert_eq!(result.truncated_slices, vec!["what".to_string()]);
    let (_name, sent_bytes) = result
        .input_bytes_by_slice
        .iter()
        .find(|(name, _)| name == "what")
        .expect("slice recorded");
    assert!(*sent_bytes <= 10);

    let (_config, prompt) = captured.lock().unwrap().take().expect("transport called");
    // At most 10 bytes of the original slice text reached the transport.
    assert!(!prompt.contains(&long_text));
    assert!(prompt.contains(&"x".repeat(10)));
    assert!(prompt.contains("what"));
    assert!(prompt.to_lowercase().contains("truncat"));
}

#[tokio::test]
async fn judgment_within_cap_slice_is_verbatim() {
    let captured: Arc<Mutex<Option<(Config, String)>>> = Arc::new(Mutex::new(None));
    let outcome = stub_outcome("", Some(json!({ "verdict": true })));
    let node: JudgmentNode<TestVerdict> =
        JudgmentNode::new().with_transport(stub_success(outcome, captured.clone()));

    let spec = base_spec(vec![InputSlice {
        name: "files".to_string(),
        text: "short text".to_string(),
        max_bytes: 1000,
    }]);

    let result = node
        .judge(&empty_ctx(), spec)
        .await
        .expect("judge succeeds");

    assert!(result.truncated_slices.is_empty());
    let (_name, sent_bytes) = result
        .input_bytes_by_slice
        .iter()
        .find(|(name, _)| name == "files")
        .expect("slice recorded");
    assert_eq!(*sent_bytes, "short text".len());

    let (_config, prompt) = captured.lock().unwrap().take().expect("transport called");
    assert!(prompt.contains("short text"));
    assert!(!prompt.to_lowercase().contains("truncat"));
}

// ---------------------------------------------------------------------------
// Typed failure outcomes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn judgment_timeout_has_no_session() {
    let node: JudgmentNode<TestVerdict> =
        JudgmentNode::new().with_transport(stub_error(|| claude_code_rs::Error::Timeout));

    let err = node
        .judge(&empty_ctx(), base_spec(vec![]))
        .await
        .expect_err("timeout must error");

    assert!(matches!(err, JudgmentError::Timeout));
}

#[tokio::test]
async fn judgment_cli_error_keeps_session() {
    let node: JudgmentNode<TestVerdict> =
        JudgmentNode::new().with_transport(stub_error(|| claude_code_rs::Error::Api {
            status: Some(500),
            message: "model overloaded".to_string(),
            session_id: Some("sess-err".to_string()),
            cost_usd: 0.02,
            usage: SdkUsage {
                input_tokens: 20,
                output_tokens: 0,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
        }));

    let err = node
        .judge(&empty_ctx(), base_spec(vec![]))
        .await
        .expect_err("api error must error");

    match err {
        JudgmentError::CliError { sessions, message } => {
            assert_eq!(sessions.len(), 1);
            assert!(message.contains("model overloaded"));
        }
        other => panic!("expected CliError, got {other:?}"),
    }
}

#[tokio::test]
async fn judgment_no_structured_result_keeps_session() {
    let captured: Arc<Mutex<Option<(Config, String)>>> = Arc::new(Mutex::new(None));
    // A successful reply, but the content is not JSON and there is no
    // structured payload — the expected shape of a turns-exhausted call.
    let outcome = stub_outcome("I ran out of turns before finishing.", None);
    let node: JudgmentNode<TestVerdict> =
        JudgmentNode::new().with_transport(stub_success(outcome, captured));

    let err = node
        .judge(&empty_ctx(), base_spec(vec![]))
        .await
        .expect_err("non-JSON content must error");

    match err {
        JudgmentError::NoStructuredResult {
            sessions,
            max_turns,
        } => {
            assert_eq!(sessions.len(), 1);
            assert_eq!(max_turns, Some(4));
        }
        other => panic!("expected NoStructuredResult, got {other:?}"),
    }
}

#[tokio::test]
async fn judgment_schema_violation_keeps_session() {
    let captured: Arc<Mutex<Option<(Config, String)>>> = Arc::new(Mutex::new(None));
    // Valid JSON, but an out-of-schema value: `verdict` is a string, not a
    // bool, so `TestVerdict`'s deserialize fails.
    let structured = json!({ "verdict": "not-a-bool" });
    let outcome = stub_outcome(&structured.to_string(), Some(structured));
    let node: JudgmentNode<TestVerdict> =
        JudgmentNode::new().with_transport(stub_success(outcome, captured));

    let err = node
        .judge(&empty_ctx(), base_spec(vec![]))
        .await
        .expect_err("schema mismatch must error");

    match err {
        JudgmentError::SchemaViolation { sessions, .. } => {
            assert_eq!(sessions.len(), 1);
        }
        other => panic!("expected SchemaViolation, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// judgment_writes_no_ctx_slot
// ---------------------------------------------------------------------------

#[tokio::test]
async fn judgment_writes_no_ctx_slot() {
    let captured: Arc<Mutex<Option<(Config, String)>>> = Arc::new(Mutex::new(None));
    let outcome = stub_outcome("", Some(json!({ "verdict": true })));
    let node: JudgmentNode<TestVerdict> =
        JudgmentNode::new().with_transport(stub_success(outcome, captured));

    let mut ctx = empty_ctx();
    ctx.nodes
        .insert("SomeUpstreamNode".to_string(), json!({ "existing": true }));
    let keys_before: std::collections::BTreeSet<String> = ctx.nodes.keys().cloned().collect();

    let _ = node
        .judge(&ctx, base_spec(vec![]))
        .await
        .expect("judge succeeds");

    let keys_after: std::collections::BTreeSet<String> = ctx.nodes.keys().cloned().collect();
    assert_eq!(
        keys_before, keys_after,
        "judge must not write onto the caller's ctx.nodes — it returns its result instead"
    );
}

// ---------------------------------------------------------------------------
// Live tests (DECLARED UN-GATEABLE, D64) — run once by hand.
// ---------------------------------------------------------------------------

/// Real `claude` call through `JudgmentNode` with `max_turns` set, on a
/// prompt easily answerable in one turn — expects a schema-valid verdict.
///
/// **UNVERIFIED pending a manual run** (this is a `#[ignore]`d live test;
/// neither it nor its sibling below runs in the gated suite). Once run by
/// hand, record here: `claude --version`, the argv actually used (including
/// `--max-turns` and `--json-schema`), the raw envelope returned, and
/// confirmation that the call produced a schema-valid `TestVerdict`.
#[tokio::test]
#[ignore = "DECLARED UN-GATEABLE (D64): makes a real, billed `claude` call. Run by hand: \
            cargo nextest run -p engine-core --run-ignored ignored-only judgment_live"]
async fn judgment_live_max_turns_records_a_real_verdict_or_exhaustion() {
    let node: JudgmentNode<TestVerdict> = JudgmentNode::new();
    let spec = JudgmentSpec {
        identity: "JudgmentLiveTest".to_string(),
        json_schema: test_json_schema(),
        stable_prompt: "Reply with strict JSON matching the schema: is 2 + 2 equal to 4? \
                        Set \"verdict\" to true if so.",
        slices: vec![],
        tier: ModelTier::Haiku,
        max_turns: Some(4),
    };

    let result = node
        .judge(&empty_ctx(), spec)
        .await
        .expect("a real call with ample turns should produce a schema-valid verdict");
    assert!(result.verdict.verdict);
}

/// Real `claude` call through `JudgmentNode` with `max_turns: 1` on a prompt
/// that plausibly needs more than one turn — expects a recorded
/// [`JudgmentError`] rather than a panic or hang.
///
/// **UNVERIFIED pending a manual run.** The installed `claude` binary
/// carries the `--max-turns <turns>` option even though `claude --help`
/// does not list it, so the exhaustion shape this test observes is not yet
/// confirmed to be `JudgmentError::NoStructuredResult` — record the actual
/// variant, the raw envelope, and `claude --version` here once run by hand;
/// if it is not `NoStructuredResult`, update that variant's doc comment in
/// `crates/engine-core/src/nodes/judgment.rs`.
#[tokio::test]
#[ignore = "DECLARED UN-GATEABLE (D64): makes a real, billed `claude` call. Run by hand: \
            cargo nextest run -p engine-core --run-ignored ignored-only judgment_live"]
async fn judgment_live_single_turn_yields_a_recorded_judgment_error() {
    let node: JudgmentNode<TestVerdict> = JudgmentNode::new();
    let spec = JudgmentSpec {
        identity: "JudgmentLiveTest".to_string(),
        json_schema: test_json_schema(),
        stable_prompt: "Before answering, use the Bash tool to run `echo one` then `echo two` \
                        then `echo three`, then finally reply with strict JSON matching the \
                        schema.",
        slices: vec![],
        tier: ModelTier::Haiku,
        max_turns: Some(1),
    };

    let err = node
        .judge(&empty_ctx(), spec)
        .await
        .expect_err("a one-turn budget on a multi-step prompt should exhaust without a verdict");
    println!("judgment_live_single_turn_yields_a_recorded_judgment_error: {err:?}");
}
