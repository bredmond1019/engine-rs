//! `EN.17.E` task 4 — integration tests over a temp lock dir and a canned-verdict stub
//! transport, driving [`integrate_chain_with_inbox_triage`] (the public entry point
//! `EN.17.E` task 3 added) from OUTSIDE the crate, exactly the same "drive the real public
//! surface, never a crate-internal helper" discipline `coord_chain.rs` already established
//! for RENDEZVOUS/LEASE_RELEASE. Every fixture helper below is duplicated from
//! `coord_chain.rs` rather than exported across the crate boundary, matching that module's
//! own stated convention (`one_repo_registry`, `write_done_state`, `recording_runner`,
//! `coord_now_iso`, `step`) — plus new helpers this module needs of its own:
//! `write_message` (a `subject.block`-carrying, arbitrary-kind envelope) and the
//! canned-verdict transports (`queued_transport`/`text_transport`/`erroring_transport`/
//! `panicking_transport`).
//!
//! OBSERVED RED, recorded per this task's own acceptance criteria: run against the
//! pre-task-3 drain (`git stash` task 3's `integrate.rs` diff, or read task 3's own commit
//! message), the reply and requeue tests here find no reply and no second dispatch — the
//! boundary's `_ => {}` arm dropped `EdgeReleased`/`Finding`/`Query` unconditionally, exactly
//! as the block's own `what` field states. Task 3 already landed by the time this task runs,
//! so this is recorded here rather than re-derived live, per the task's own instruction.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use claude_code_rs::parse::Usage as SdkUsage;
use claude_code_rs::{Config, Outcome};

use engine_core::repo_registry::RepoRegistry;
use engine_core::workflows::orchestration::chain::ChainStep;
use engine_core::workflows::orchestration::coord_lane::CoordHandle;
use engine_core::workflows::orchestration::corpus_gates::BlockPresence;
use engine_core::workflows::orchestration::execute::{EngineKind, ExecutionOutcome, FlowRunner};
use engine_core::workflows::orchestration::gates::{AdmissionGate, DependencyEdge};
use engine_core::workflows::orchestration::graph::{BailChannel, OnBail};
use engine_core::workflows::orchestration::inbox_triage::{
    Action, InboxTriageConfig, InboxTriageRunner, ProcessedMessage, Verdict,
};
use engine_core::workflows::orchestration::integrate::{
    integrate_chain_with_inbox_triage, ChainReport, IntegrateError, NeverHeld, OnUnjudged,
    StepProgress,
};
use engine_core::workflows::orchestration::preflight::{BlockPreflight, PreflightOutcome};
use engine_core::workflows::ModelTransport;

// ── Shared fixture helpers — duplicated from `coord_chain.rs`, see this module's own doc ──

fn step(repo: &str, block_id: &str) -> ChainStep {
    ChainStep {
        repo: repo.to_string(),
        block_id: block_id.to_string(),
        directives: None,
        ..Default::default()
    }
}

/// A tempdir `brain.toml` + one real `repo-a` directory, plus its own roadmap and lock
/// directories, all kept alive together for one test's duration.
struct Fixture {
    brain_root: tempfile::TempDir,
    roadmap_dir: tempfile::TempDir,
    lock_dir: tempfile::TempDir,
    registry: RepoRegistry,
}

fn fixture() -> Fixture {
    let brain_root = tempfile::tempdir().expect("tempdir");
    let repo_a = brain_root.path().join("repo-a");
    std::fs::create_dir_all(&repo_a).unwrap();
    std::fs::write(
        brain_root.path().join("brain.toml"),
        "[[repos]]\nslug = \"repo-a\"\nrepo_path = \"repo-a\"\n",
    )
    .unwrap();
    // A real (if minimal) git checkout — `subject_repo_short_sha` (`integrate.rs`) shells out
    // to `git rev-parse --short=7 HEAD` to resolve a FINDING escalation's `verified_at_sha`;
    // without a real commit here, that call fails and `EscalationRecord::new` refuses the
    // resulting `"unknown"` (`sha_re()` rejects it), silently dropping every escalation this
    // suite composes.
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "test@example.com"],
        vec!["config", "user.name", "test"],
        vec!["commit", "-q", "--allow-empty", "-m", "init"],
    ] {
        let status = std::process::Command::new("git")
            .args(&args)
            .current_dir(&repo_a)
            .status()
            .unwrap_or_else(|err| panic!("git {args:?} must run: {err}"));
        assert!(status.success(), "git {args:?} must succeed");
    }
    let registry = RepoRegistry::from_brain_root(brain_root.path()).expect("registry");
    Fixture {
        registry,
        roadmap_dir: tempfile::tempdir().expect("tempdir"),
        lock_dir: tempfile::tempdir().expect("tempdir"),
        brain_root,
    }
}

/// Writes the `"status": "done"` state file `verify_state_write` looks for after a `Flow`
/// step completes.
fn write_done_state(repo_path: &Path, block_id: &str) {
    let dir = repo_path.join("planning").join(block_id).join("sdlc");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("sdlc-flow-state.json"),
        json!({"status": "done"}).to_string(),
    )
    .unwrap();
}

/// A `FlowRunner` that records every `block_id` it was invoked with and always succeeds —
/// the actual state-write is `write_done_state`'s job, done ahead of time by the caller.
fn recording_runner() -> (FlowRunner, Arc<Mutex<Vec<String>>>) {
    let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let runner: FlowRunner = Arc::new(move |invocation| {
        recorded.lock().unwrap().push(invocation.block_id.clone());
        Box::pin(async {
            Ok(engine_contract::TaskContext {
                event: json!({}),
                nodes: HashMap::new(),
                metadata: json!({}),
                node_runs: HashMap::new(),
            })
        })
    });
    (runner, calls)
}

fn coord_now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Writes a message envelope directly into `<lock_dir>/queue/<to_repo>/<to_lane>/inbox/` —
/// standing in for a sibling lane's delivery. Unlike `coord_chain.rs`'s own
/// `write_inbox_message`, this carries an optional `subject.block` and a caller-chosen
/// `body`/`sender`, which every EDGE_RELEASED/FINDING/QUERY test here needs.
#[allow(clippy::too_many_arguments)]
fn write_message(
    lock_dir: &Path,
    to_repo: &str,
    to_lane: &str,
    message_id: &str,
    kind: &str,
    sent_at: &str,
    subject_repo: &str,
    subject_block: Option<&str>,
    body: &str,
    sender_repo: &str,
    sender_lane: &str,
) {
    let inbox = lock_dir
        .join("queue")
        .join(to_repo)
        .join(to_lane)
        .join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    let mut subject = serde_json::Map::new();
    subject.insert("repo".to_string(), json!(subject_repo));
    if let Some(block) = subject_block {
        subject.insert("block".to_string(), json!(block));
    }
    let envelope = json!({
        "message_id": message_id,
        "sender": {
            "agent_name": "peer-lane",
            "repo": sender_repo,
            "lane": sender_lane,
            "roadmap": "coordination-layer-port",
        },
        "sent_at": sent_at,
        "kind": kind,
        "subject": Value::Object(subject),
        "body": body,
        "durable_home": {
            "channel": "lane-log",
            "ref": "lane-log.jsonl#1",
        },
        "verified_by": "test fixture",
    });
    let filename_ts: String = sent_at.chars().filter(|c| *c != '-' && *c != ':').collect();
    std::fs::write(
        inbox.join(format!("{filename_ts}-{message_id}.json")),
        serde_json::to_string(&envelope).unwrap(),
    )
    .unwrap();
}

// ── Canned-verdict stub transports ──────────────────────────────────────────────────────

fn success_outcome(text: impl Into<String>) -> Outcome {
    Outcome {
        cost_usd: 0.0,
        usage: SdkUsage {
            input_tokens: 1,
            output_tokens: 1,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
        model_usage: std::collections::BTreeMap::new(),
        text: text.into(),
        is_error: false,
        api_error_status: None,
        session_id: None,
        structured_output: None,
    }
}

/// A transport that returns each of `replies` in order, one JSON verdict per call — panics
/// if called more times than there are queued replies (a test-fixture bug, never a
/// production path).
fn queued_transport(replies: Vec<Value>) -> ModelTransport {
    let queue = Arc::new(Mutex::new(VecDeque::from(replies)));
    Arc::new(move |_config: Config, _prompt: String| {
        let queue = queue.clone();
        Box::pin(async move {
            let value = queue
                .lock()
                .unwrap()
                .pop_front()
                .expect("queued_transport: more judgment calls than queued replies");
            Ok(success_outcome(value.to_string()))
        })
    })
}

/// A transport that returns each of `replies` verbatim as the call's raw text — for a
/// reply that is deliberately not JSON at all (`JudgmentError::NoStructuredResult`).
fn text_transport(replies: Vec<String>) -> ModelTransport {
    let queue = Arc::new(Mutex::new(VecDeque::from(replies)));
    Arc::new(move |_config: Config, _prompt: String| {
        let queue = queue.clone();
        Box::pin(async move {
            let text = queue
                .lock()
                .unwrap()
                .pop_front()
                .expect("text_transport: more judgment calls than queued replies");
            Ok(success_outcome(text))
        })
    })
}

/// A transport that always fails with the given `claude_code_rs::Error`.
fn erroring_transport(err: fn() -> claude_code_rs::Error) -> ModelTransport {
    Arc::new(move |_config: Config, _prompt: String| Box::pin(async move { Err(err()) }))
}

/// A transport that panics if ever invoked — proves a code path never reaches
/// `JudgmentNode` at all (used for every EDGE_RELEASED and cap-violation case, which must
/// bill zero judgment sessions).
fn panicking_transport() -> ModelTransport {
    Arc::new(|_config: Config, _prompt: String| {
        Box::pin(async { panic!("JudgmentNode must never be called on this path") })
    })
}

// ── The one driver every test below calls ───────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn run_chain(
    chain: &[ChainStep],
    registry: &RepoRegistry,
    runner: &FlowRunner,
    roadmap_dir: &Path,
    coord: Option<&CoordHandle>,
    resolve_depends_on: &dyn Fn(&str, &str) -> Vec<DependencyEdge>,
    is_edge_met: &dyn Fn(&str, &str) -> bool,
    on_bail: OnBail,
    inbox_triage_enabled: bool,
    inbox_triage_runner: Option<&InboxTriageRunner>,
) -> (
    Result<Vec<ExecutionOutcome>, IntegrateError>,
    ChainReport,
    Vec<ProcessedMessage>,
) {
    let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;
    let admission = AdmissionGate::with_default_policy();
    let mut report = ChainReport::default();
    let mut preflight_report: Vec<BlockPreflight> = Vec::new();
    let mut inbox_report: Vec<ProcessedMessage> = Vec::new();

    let result = integrate_chain_with_inbox_triage(
        chain,
        resolve_depends_on,
        is_edge_met,
        &admission,
        &NeverHeld,
        Duration::from_millis(1),
        None,
        None,
        None,
        &resolve_engine,
        registry,
        runner,
        roadmap_dir,
        None,
        &|_: &StepProgress| {},
        false,
        true,
        uuid::Uuid::new_v4(),
        &|_repo: &str, _id: &str| {},
        coord,
        None,
        None,
        on_bail,
        BailChannel::Session,
        &|_repo: &str, _id: &str| BlockPresence::Row("open".to_string()),
        &mut report,
        &|_repo: &str, _id: &str| PreflightOutcome::Disabled,
        OnUnjudged::Proceed,
        &mut preflight_report,
        inbox_triage_enabled,
        inbox_triage_runner,
        &mut inbox_report,
    )
    .await;

    (result, report, inbox_report)
}

fn reply_inbox_path(lock_dir: &Path, sender_repo: &str, sender_lane: &str) -> std::path::PathBuf {
    lock_dir
        .join("queue")
        .join(sender_repo)
        .join(sender_lane)
        .join("inbox")
}

fn read_only_reply(lock_dir: &Path, sender_repo: &str, sender_lane: &str) -> String {
    let inbox = reply_inbox_path(lock_dir, sender_repo, sender_lane);
    let mut entries: Vec<_> = std::fs::read_dir(&inbox)
        .unwrap_or_else(|err| panic!("reply inbox {inbox:?} must exist: {err}"))
        .collect();
    assert_eq!(entries.len(), 1, "expected exactly one reply in {inbox:?}");
    std::fs::read_to_string(entries.remove(0).unwrap().path()).expect("read reply envelope")
}

// ── (1) EDGE_RELEASED — deterministic, no JudgmentNode call ─────────────────────────────

/// An EDGE_RELEASED naming the block a previously skipped step depends on, with that block
/// now considered met, makes the skipped step dispatch exactly once, after the chain's
/// other remaining steps.
#[tokio::test]
async fn inbox_triage_edge_released_requeues_now_met_step() {
    let fx = fixture();
    write_done_state(&fx.brain_root.path().join("repo-a"), "A.1");
    write_done_state(&fx.brain_root.path().join("repo-a"), "A.2");
    write_done_state(&fx.brain_root.path().join("repo-a"), "A.3");

    let lock_dir_path = fx.lock_dir.path().to_path_buf();
    let released = Arc::new(Mutex::new(false));
    let released_check = released.clone();
    let message_id = "aaaaaaaa-1111-4e21-9f10-000000000001";
    let is_edge_met = move |repo: &str, id: &str| {
        if repo == "repo-a" && id == "DEP.1" {
            let mut guard = released_check.lock().unwrap();
            if !*guard {
                write_message(
                    &lock_dir_path,
                    "repo-a",
                    "engine-rs",
                    message_id,
                    "EDGE_RELEASED",
                    "2026-09-11T00:00:00Z",
                    "repo-a",
                    Some("DEP.1"),
                    "released",
                    "bastion",
                    "types",
                );
                *guard = true;
                false
            } else {
                true
            }
        } else {
            true
        }
    };
    let resolve_depends_on = |repo: &str, id: &str| {
        if repo == "repo-a" && id == "A.2" {
            vec![DependencyEdge::Block {
                repo: "repo-a".to_string(),
                block_id: "DEP.1".to_string(),
            }]
        } else {
            Vec::new()
        }
    };

    let (runner, _calls) = recording_runner();
    let coord = CoordHandle::new(
        fx.lock_dir.path().to_path_buf(),
        "repo-a",
        "engine-rs",
        "engine-rs-1",
        coord_now_iso,
    );
    // Never reached for EDGE_RELEASED — a panic here would prove this path billed a
    // JudgmentNode session, which it must never do (see `inbox_triage_edge_released_bills_
    // nothing` below).
    let inbox_runner =
        InboxTriageRunner::new(InboxTriageConfig::default()).with_transport(panicking_transport());

    let chain = vec![
        step("repo-a", "A.1"),
        step("repo-a", "A.2"),
        step("repo-a", "A.3"),
    ];
    let (result, report, inbox_report) = run_chain(
        &chain,
        &fx.registry,
        &runner,
        fx.roadmap_dir.path(),
        Some(&coord),
        &resolve_depends_on,
        &is_edge_met,
        OnBail::SkipDependents,
        true,
        Some(&inbox_runner),
    )
    .await;

    let outcomes = result.expect("chain must complete");
    let dispatched: Vec<&str> = outcomes.iter().map(|o| o.block_id.as_str()).collect();
    assert_eq!(
        dispatched,
        vec!["A.1", "A.3", "A.2"],
        "A.2 must dispatch exactly once, after the chain's other remaining steps"
    );
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(inbox_report.len(), 1);
    assert_eq!(inbox_report[0].verdict, Some(Verdict::Accepted));

    let reply = read_only_reply(fx.lock_dir.path(), "bastion", "types");
    assert!(reply.contains("ACK ACCEPTED"), "{reply}");
}

/// An EDGE_RELEASED naming a skipped step's dependency, while that dependency is STILL
/// unmet, leaves the step skipped and the reply names `VERIFIED-FALSE`.
#[tokio::test]
async fn inbox_triage_edge_released_still_unmet_is_verified_false() {
    let fx = fixture();
    write_done_state(&fx.brain_root.path().join("repo-a"), "A.1");
    write_done_state(&fx.brain_root.path().join("repo-a"), "A.3");

    let lock_dir_path = fx.lock_dir.path().to_path_buf();
    let written = Arc::new(Mutex::new(false));
    let written_check = written.clone();
    let message_id = "aaaaaaaa-2222-4e21-9f10-000000000002";
    let is_edge_met = move |repo: &str, id: &str| {
        if repo == "repo-a" && id == "DEP.1" {
            let mut guard = written_check.lock().unwrap();
            if !*guard {
                write_message(
                    &lock_dir_path,
                    "repo-a",
                    "engine-rs",
                    message_id,
                    "EDGE_RELEASED",
                    "2026-09-11T00:00:01Z",
                    "repo-a",
                    Some("DEP.1"),
                    "released",
                    "bastion",
                    "types",
                );
                *guard = true;
            }
            false
        } else {
            true
        }
    };
    let resolve_depends_on = |repo: &str, id: &str| {
        if repo == "repo-a" && id == "A.2" {
            vec![DependencyEdge::Block {
                repo: "repo-a".to_string(),
                block_id: "DEP.1".to_string(),
            }]
        } else {
            Vec::new()
        }
    };

    let (runner, _calls) = recording_runner();
    let coord = CoordHandle::new(
        fx.lock_dir.path().to_path_buf(),
        "repo-a",
        "engine-rs",
        "engine-rs-1",
        coord_now_iso,
    );
    let inbox_runner =
        InboxTriageRunner::new(InboxTriageConfig::default()).with_transport(panicking_transport());

    let chain = vec![
        step("repo-a", "A.1"),
        step("repo-a", "A.2"),
        step("repo-a", "A.3"),
    ];
    let (result, report, inbox_report) = run_chain(
        &chain,
        &fx.registry,
        &runner,
        fx.roadmap_dir.path(),
        Some(&coord),
        &resolve_depends_on,
        &is_edge_met,
        OnBail::SkipDependents,
        true,
        Some(&inbox_runner),
    )
    .await;

    let outcomes = result.expect("chain must complete");
    let dispatched: Vec<&str> = outcomes.iter().map(|o| o.block_id.as_str()).collect();
    assert_eq!(
        dispatched,
        vec!["A.1", "A.3"],
        "A.2 must remain skipped, never dispatched"
    );
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(inbox_report.len(), 1);
    assert_eq!(inbox_report[0].verdict, Some(Verdict::VerifiedFalse));

    let reply = read_only_reply(fx.lock_dir.path(), "bastion", "types");
    assert!(reply.contains("ACK VERIFIED-FALSE"), "{reply}");
}

/// EDGE_RELEASED handling bills zero `JudgmentNode` sessions — a panicking transport would
/// fail this test if the code path ever reached the judge.
#[tokio::test]
async fn inbox_triage_edge_released_bills_nothing() {
    let fx = fixture();
    write_done_state(&fx.brain_root.path().join("repo-a"), "A.1");

    write_message(
        fx.lock_dir.path(),
        "repo-a",
        "engine-rs",
        "aaaaaaaa-3333-4e21-9f10-000000000003",
        "EDGE_RELEASED",
        "2026-09-11T00:00:02Z",
        "bastion",
        Some("BA.1"),
        "released",
        "bastion",
        "types",
    );

    let resolve_depends_on = |_repo: &str, _id: &str| Vec::new();
    let is_edge_met = |_repo: &str, _id: &str| true;
    let (runner, _calls) = recording_runner();
    let coord = CoordHandle::new(
        fx.lock_dir.path().to_path_buf(),
        "repo-a",
        "engine-rs",
        "engine-rs-1",
        coord_now_iso,
    );
    let inbox_runner =
        InboxTriageRunner::new(InboxTriageConfig::default()).with_transport(panicking_transport());

    let chain = vec![step("repo-a", "A.1")];
    let (result, _report, inbox_report) = run_chain(
        &chain,
        &fx.registry,
        &runner,
        fx.roadmap_dir.path(),
        Some(&coord),
        &resolve_depends_on,
        &is_edge_met,
        OnBail::SkipDependents,
        true,
        Some(&inbox_runner),
    )
    .await;

    result.expect("chain must complete");
    assert_eq!(inbox_report.len(), 1);
    // No skipped step exists at all — the released block matches nothing, so this can only
    // ever resolve to NOT-MINE; reaching this point without a panic proves the judge was
    // never touched.
    assert_eq!(inbox_report[0].verdict, Some(Verdict::NotMine));
}

// ── (2) FINDING / QUERY — one bounded JudgmentNode call each ────────────────────────────

/// A FINDING and a QUERY each cause exactly one `JudgmentNode` call, and each verdict
/// deserializes into `InboxVerdict`.
#[tokio::test]
async fn inbox_triage_finding_and_query_one_judgment_each() {
    let fx = fixture();
    write_done_state(&fx.brain_root.path().join("repo-a"), "A.1");

    write_message(
        fx.lock_dir.path(),
        "repo-a",
        "engine-rs",
        "id-finding-1",
        "FINDING",
        "2026-09-11T00:00:03Z",
        "repo-a",
        Some("A.1"),
        "a finding",
        "bastion",
        "types",
    );
    write_message(
        fx.lock_dir.path(),
        "repo-a",
        "engine-rs",
        "id-query-1",
        "QUERY",
        "2026-09-11T00:00:04Z",
        "repo-a",
        Some("A.1"),
        "a query",
        "bastion",
        "types",
    );

    let resolve_depends_on = |_repo: &str, _id: &str| Vec::new();
    let is_edge_met = |_repo: &str, _id: &str| true;
    let (runner, _calls) = recording_runner();
    let coord = CoordHandle::new(
        fx.lock_dir.path().to_path_buf(),
        "repo-a",
        "engine-rs",
        "engine-rs-1",
        coord_now_iso,
    );
    let verdict = json!({"verdict": "ACCEPTED", "action": "NONE", "reason": "noted"});
    let inbox_runner = InboxTriageRunner::new(InboxTriageConfig::default())
        .with_transport(queued_transport(vec![verdict.clone(), verdict]));

    let chain = vec![step("repo-a", "A.1")];
    let (result, _report, inbox_report) = run_chain(
        &chain,
        &fx.registry,
        &runner,
        fx.roadmap_dir.path(),
        Some(&coord),
        &resolve_depends_on,
        &is_edge_met,
        OnBail::SkipDependents,
        true,
        Some(&inbox_runner),
    )
    .await;

    result.expect("chain must complete");
    // Two queued replies, exactly two `JudgmentNode` calls consumed (one per message) — a
    // third call would panic `queued_transport`, proving neither message was judged twice.
    assert_eq!(inbox_report.len(), 2);
    assert!(inbox_report
        .iter()
        .all(|p| p.verdict == Some(Verdict::Accepted)));
}

/// A reply carrying a verdict string outside the four values is a hard parse error
/// (`JudgmentError::SchemaViolation`), never a silent pass-through — the message is
/// deferred, not judged.
#[tokio::test]
async fn inbox_triage_unknown_verdict_is_a_parse_error() {
    let fx = fixture();
    write_done_state(&fx.brain_root.path().join("repo-a"), "A.1");

    write_message(
        fx.lock_dir.path(),
        "repo-a",
        "engine-rs",
        "id-finding-bad",
        "FINDING",
        "2026-09-11T00:00:05Z",
        "repo-a",
        Some("A.1"),
        "a finding",
        "bastion",
        "types",
    );

    let resolve_depends_on = |_repo: &str, _id: &str| Vec::new();
    let is_edge_met = |_repo: &str, _id: &str| true;
    let (runner, _calls) = recording_runner();
    let coord = CoordHandle::new(
        fx.lock_dir.path().to_path_buf(),
        "repo-a",
        "engine-rs",
        "engine-rs-1",
        coord_now_iso,
    );
    let bad_verdict = json!({"verdict": "MAYBE", "action": "NONE", "reason": "x"});
    let inbox_runner = InboxTriageRunner::new(InboxTriageConfig::default())
        .with_transport(queued_transport(vec![bad_verdict]));

    let chain = vec![step("repo-a", "A.1")];
    let (result, _report, inbox_report) = run_chain(
        &chain,
        &fx.registry,
        &runner,
        fx.roadmap_dir.path(),
        Some(&coord),
        &resolve_depends_on,
        &is_edge_met,
        OnBail::SkipDependents,
        true,
        Some(&inbox_runner),
    )
    .await;

    result.expect("chain must complete");
    assert_eq!(inbox_report.len(), 1);
    assert_eq!(
        inbox_report[0].error_kind.as_deref(),
        Some("schema_violation"),
        "an out-of-enum verdict must be a SchemaViolation, never a silent pass-through"
    );
    assert!(inbox_report[0].verdict.is_none());

    let reply = read_only_reply(fx.lock_dir.path(), "bastion", "types");
    assert!(reply.contains("ACK DEFERRED"), "{reply}");
    assert!(reply.contains("schema_violation"), "{reply}");
}

// ── (3) Reply composition — every processed message gets exactly one reply ──────────────

/// Every processed message produces exactly one reply, addressed at the sender's own
/// `repo`/`lane`, whose body begins `ACK <VERDICT>` and names a durable-home reference; the
/// original message ends in `done/` with its receipt present.
#[tokio::test]
async fn inbox_triage_every_message_gets_one_reply_with_durable_home() {
    let fx = fixture();
    write_done_state(&fx.brain_root.path().join("repo-a"), "A.1");

    write_message(
        fx.lock_dir.path(),
        "repo-a",
        "engine-rs",
        "id-finding-durable",
        "FINDING",
        "2026-09-11T00:00:06Z",
        "repo-a",
        Some("A.1"),
        "a finding",
        "peer-repo",
        "peer-lane",
    );

    let resolve_depends_on = |_repo: &str, _id: &str| Vec::new();
    let is_edge_met = |_repo: &str, _id: &str| true;
    let (runner, _calls) = recording_runner();
    let coord = CoordHandle::new(
        fx.lock_dir.path().to_path_buf(),
        "repo-a",
        "engine-rs",
        "engine-rs-1",
        coord_now_iso,
    );
    let verdict = json!({"verdict": "ACCEPTED", "action": "NONE", "reason": "seen"});
    let inbox_runner = InboxTriageRunner::new(InboxTriageConfig::default())
        .with_transport(queued_transport(vec![verdict]));

    let chain = vec![step("repo-a", "A.1")];
    let (result, _report, inbox_report) = run_chain(
        &chain,
        &fx.registry,
        &runner,
        fx.roadmap_dir.path(),
        Some(&coord),
        &resolve_depends_on,
        &is_edge_met,
        OnBail::SkipDependents,
        true,
        Some(&inbox_runner),
    )
    .await;

    result.expect("chain must complete");
    assert_eq!(inbox_report.len(), 1);
    let reply_path = inbox_report[0]
        .reply_path
        .clone()
        .expect("reply_path must be recorded once the reply send succeeds");
    let reply_text = std::fs::read_to_string(&reply_path).expect("read reply envelope");
    let reply: Value = serde_json::from_str(&reply_text).expect("reply must parse");
    assert!(
        reply["body"]
            .as_str()
            .unwrap_or_default()
            .starts_with("ACK ACCEPTED"),
        "{reply}"
    );
    assert!(
        reply["durable_home"]["ref"]
            .as_str()
            .unwrap_or_default()
            .contains("id-finding-durable"),
        "the reply must name a durable-home reference: {reply}"
    );

    let queue_dir = fx
        .lock_dir
        .path()
        .join("queue")
        .join("repo-a")
        .join("engine-rs");
    let done_dir = queue_dir.join("done");
    assert_eq!(
        std::fs::read_dir(&done_dir)
            .unwrap_or_else(|err| panic!("done/ must exist: {err}"))
            .count(),
        1,
        "the original message must end in done/ with its receipt present"
    );
    let receipts =
        std::fs::read_to_string(queue_dir.join("receipts.jsonl")).expect("receipts.jsonl");
    assert!(receipts.contains("id-finding-durable"));
}

// ── (4) Escalation — a FINDING may escalate; a QUERY never does ─────────────────────────

/// A FINDING judged `Accepted` + `Escalate` produces exactly one notification escalation; a
/// QUERY judged the SAME verdict produces zero — escalation is scoped to `MessageKind::
/// Finding` alone.
#[tokio::test]
async fn inbox_triage_escalate_sends_once_query_never() {
    let fx = fixture();
    write_done_state(&fx.brain_root.path().join("repo-a"), "A.1");

    write_message(
        fx.lock_dir.path(),
        "repo-a",
        "engine-rs",
        "id-finding-escalate",
        "FINDING",
        "2026-09-11T00:00:07Z",
        "repo-a",
        Some("A.1"),
        "escalate me",
        "bastion",
        "types",
    );
    write_message(
        fx.lock_dir.path(),
        "repo-a",
        "engine-rs",
        "id-query-escalate",
        "QUERY",
        "2026-09-11T00:00:08Z",
        "repo-a",
        Some("A.1"),
        "escalate me too",
        "bastion",
        "types",
    );

    let resolve_depends_on = |_repo: &str, _id: &str| Vec::new();
    let is_edge_met = |_repo: &str, _id: &str| true;
    let (runner, _calls) = recording_runner();
    let coord = CoordHandle::new(
        fx.lock_dir.path().to_path_buf(),
        "repo-a",
        "engine-rs",
        "engine-rs-1",
        coord_now_iso,
    );
    let escalate_verdict =
        json!({"verdict": "ACCEPTED", "action": "ESCALATE", "reason": "needs a human"});
    let inbox_runner = InboxTriageRunner::new(InboxTriageConfig::default()).with_transport(
        queued_transport(vec![escalate_verdict.clone(), escalate_verdict]),
    );

    let chain = vec![step("repo-a", "A.1")];
    let (result, _report, inbox_report) = run_chain(
        &chain,
        &fx.registry,
        &runner,
        fx.roadmap_dir.path(),
        Some(&coord),
        &resolve_depends_on,
        &is_edge_met,
        OnBail::SkipDependents,
        true,
        Some(&inbox_runner),
    )
    .await;

    result.expect("chain must complete");
    assert_eq!(inbox_report.len(), 2);
    assert!(inbox_report
        .iter()
        .all(|p| p.verdict == Some(Verdict::Accepted) && p.action == Some(Action::Escalate)));

    let escalations_path = fx.roadmap_dir.path().join("escalations.jsonl");
    let contents = std::fs::read_to_string(&escalations_path).expect("escalations.jsonl");
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "exactly one escalation — the FINDING's, never the QUERY's: {contents}"
    );
    let escalation: Value = serde_json::from_str(lines[0]).expect("valid JSON line");
    assert_eq!(escalation["kind"], "finding");
}

// ── (5) Caps precede judgment ────────────────────────────────────────────────────────────

/// An envelope with a non-empty `cap_violations()` is never passed to `JudgmentNode` — a
/// panicking transport proves it — and gets an `ACK DEFERRED` reply naming the violated
/// field.
#[tokio::test]
async fn inbox_triage_over_cap_is_never_judged() {
    let fx = fixture();
    write_done_state(&fx.brain_root.path().join("repo-a"), "A.1");

    let over_cap_body = "a".repeat(okf_core::BODY_MAX_CHARS + 1);
    write_message(
        fx.lock_dir.path(),
        "repo-a",
        "engine-rs",
        "id-over-cap",
        "FINDING",
        "2026-09-11T00:00:09Z",
        "repo-a",
        Some("A.1"),
        &over_cap_body,
        "bastion",
        "types",
    );

    let resolve_depends_on = |_repo: &str, _id: &str| Vec::new();
    let is_edge_met = |_repo: &str, _id: &str| true;
    let (runner, _calls) = recording_runner();
    let coord = CoordHandle::new(
        fx.lock_dir.path().to_path_buf(),
        "repo-a",
        "engine-rs",
        "engine-rs-1",
        coord_now_iso,
    );
    let inbox_runner =
        InboxTriageRunner::new(InboxTriageConfig::default()).with_transport(panicking_transport());

    let chain = vec![step("repo-a", "A.1")];
    let (result, _report, inbox_report) = run_chain(
        &chain,
        &fx.registry,
        &runner,
        fx.roadmap_dir.path(),
        Some(&coord),
        &resolve_depends_on,
        &is_edge_met,
        OnBail::SkipDependents,
        true,
        Some(&inbox_runner),
    )
    .await;

    result.expect("chain must complete");
    assert_eq!(inbox_report.len(), 1);
    assert_eq!(inbox_report[0].error_kind.as_deref(), Some("cap_violation"));

    let reply = read_only_reply(fx.lock_dir.path(), "bastion", "types");
    assert!(reply.contains("ACK DEFERRED"), "{reply}");
    assert!(reply.contains("body"), "{reply}");
}

// ── (6) Boundary-only timing ─────────────────────────────────────────────────────────────

/// A FINDING delivered while a step is still executing is never acted on mid-block: no
/// reply exists before that step even starts, and the reply is already present by the time
/// the NEXT step starts — it was handled at the boundary between them, never mid-execution.
/// The in-flight step is never abandoned: both steps complete.
#[tokio::test]
async fn inbox_triage_mid_block_message_waits_for_boundary() {
    let fx = fixture();
    write_done_state(&fx.brain_root.path().join("repo-a"), "A.1");
    write_done_state(&fx.brain_root.path().join("repo-a"), "A.2");

    let lock_dir_path = fx.lock_dir.path().to_path_buf();
    let reply_inbox = reply_inbox_path(fx.lock_dir.path(), "bastion", "types");
    let message_id = "id-mid-block";
    let sent_at = "2026-09-11T00:00:10Z";

    let reply_seen_before_a1 = Arc::new(Mutex::new(false));
    let reply_seen_before_a1_check = reply_seen_before_a1.clone();
    let reply_seen_at_a2_start = Arc::new(Mutex::new(false));
    let reply_seen_at_a2_start_check = reply_seen_at_a2_start.clone();

    fn reply_exists(reply_inbox: &Path) -> bool {
        std::fs::read_dir(reply_inbox)
            .map(|mut d| d.next().is_some())
            .unwrap_or(false)
    }

    let runner: FlowRunner = {
        let lock_dir_path = lock_dir_path.clone();
        let reply_inbox = reply_inbox.clone();
        Arc::new(
            move |invocation: engine_core::workflows::orchestration::execute::FlowInvocation| {
                let lock_dir_path = lock_dir_path.clone();
                let reply_inbox = reply_inbox.clone();
                let reply_seen_before_a1_check = reply_seen_before_a1_check.clone();
                let reply_seen_at_a2_start_check = reply_seen_at_a2_start_check.clone();
                let block_id = invocation.block_id.clone();
                Box::pin(async move {
                    if block_id == "A.1" {
                        *reply_seen_before_a1_check.lock().unwrap() = reply_exists(&reply_inbox);
                        write_message(
                            &lock_dir_path,
                            "repo-a",
                            "engine-rs",
                            message_id,
                            "FINDING",
                            sent_at,
                            "repo-a",
                            Some("A.1"),
                            "arrived mid A.1",
                            "bastion",
                            "types",
                        );
                    }
                    if block_id == "A.2" {
                        *reply_seen_at_a2_start_check.lock().unwrap() = reply_exists(&reply_inbox);
                    }
                    Ok(engine_contract::TaskContext {
                        event: json!({}),
                        nodes: HashMap::new(),
                        metadata: json!({}),
                        node_runs: HashMap::new(),
                    })
                })
            },
        )
    };

    let resolve_depends_on = |_repo: &str, _id: &str| Vec::new();
    let is_edge_met = |_repo: &str, _id: &str| true;
    let coord = CoordHandle::new(
        fx.lock_dir.path().to_path_buf(),
        "repo-a",
        "engine-rs",
        "engine-rs-1",
        coord_now_iso,
    );
    let verdict = json!({"verdict": "ACCEPTED", "action": "NONE", "reason": "noted"});
    let inbox_runner = InboxTriageRunner::new(InboxTriageConfig::default())
        .with_transport(queued_transport(vec![verdict]));

    let chain = vec![step("repo-a", "A.1"), step("repo-a", "A.2")];
    let (result, _report, inbox_report) = run_chain(
        &chain,
        &fx.registry,
        &runner,
        fx.roadmap_dir.path(),
        Some(&coord),
        &resolve_depends_on,
        &is_edge_met,
        OnBail::SkipDependents,
        true,
        Some(&inbox_runner),
    )
    .await;

    let outcomes = result.expect("chain must complete");
    assert_eq!(
        outcomes.len(),
        2,
        "the in-flight step must not be abandoned"
    );
    assert!(
        !*reply_seen_before_a1.lock().unwrap(),
        "no reply can exist before A.1 even starts"
    );
    assert!(
        *reply_seen_at_a2_start.lock().unwrap(),
        "the FINDING must already be answered by the time A.2 starts — handled at the \
         boundary between A.1 and A.2, never mid A.1"
    );
    assert_eq!(inbox_report.len(), 1);
}

// ── (7) The switch off is today's drop ───────────────────────────────────────────────────

/// With `inbox_triage_enabled` false (the built-in default), EDGE_RELEASED/FINDING/QUERY
/// are drained (quarantined into `processing/`, per the unconditional drain) but never
/// replied to and never completed into `done/` — exactly today's behavior.
#[tokio::test]
async fn inbox_triage_disabled_is_todays_drop() {
    let fx = fixture();
    write_done_state(&fx.brain_root.path().join("repo-a"), "A.1");

    write_message(
        fx.lock_dir.path(),
        "repo-a",
        "engine-rs",
        "id-disabled-edge",
        "EDGE_RELEASED",
        "2026-09-11T00:00:11Z",
        "repo-a",
        Some("DEP.1"),
        "released",
        "bastion",
        "types",
    );
    write_message(
        fx.lock_dir.path(),
        "repo-a",
        "engine-rs",
        "id-disabled-finding",
        "FINDING",
        "2026-09-11T00:00:12Z",
        "repo-a",
        Some("A.1"),
        "a finding",
        "bastion",
        "types",
    );
    write_message(
        fx.lock_dir.path(),
        "repo-a",
        "engine-rs",
        "id-disabled-query",
        "QUERY",
        "2026-09-11T00:00:13Z",
        "repo-a",
        Some("A.1"),
        "a query",
        "bastion",
        "types",
    );

    let resolve_depends_on = |_repo: &str, _id: &str| Vec::new();
    let is_edge_met = |_repo: &str, _id: &str| true;
    let (runner, _calls) = recording_runner();
    let coord = CoordHandle::new(
        fx.lock_dir.path().to_path_buf(),
        "repo-a",
        "engine-rs",
        "engine-rs-1",
        coord_now_iso,
    );

    let chain = vec![step("repo-a", "A.1")];
    let (result, _report, inbox_report) = run_chain(
        &chain,
        &fx.registry,
        &runner,
        fx.roadmap_dir.path(),
        Some(&coord),
        &resolve_depends_on,
        &is_edge_met,
        OnBail::SkipDependents,
        false,
        None,
    )
    .await;

    result.expect("chain must complete");
    assert!(
        inbox_report.is_empty(),
        "with the switch off, nothing is routed through inbox triage"
    );

    let queue_dir = fx
        .lock_dir
        .path()
        .join("queue")
        .join("repo-a")
        .join("engine-rs");
    assert_eq!(
        std::fs::read_dir(queue_dir.join("processing"))
            .unwrap()
            .count(),
        3,
        "the drain itself is unconditional — all three still move to processing/"
    );
    assert!(
        !queue_dir.join("done").exists(),
        "a disabled switch must never complete any of these three messages"
    );
    assert!(
        !reply_inbox_path(fx.lock_dir.path(), "bastion", "types").exists(),
        "no reply is ever sent while the switch is off"
    );
}

// ── (8) A failed judgment always defers, never escalates ────────────────────────────────

/// A FINDING whose judgment fails — one sub-case per `JudgmentError` variant — gets exactly
/// one `ACK DEFERRED` reply naming that error kind, is moved to `done/`, and causes zero
/// notification sends.
#[tokio::test]
async fn inbox_triage_failed_judgment_defers() {
    let cases: Vec<(&str, ModelTransport, &str)> = vec![
        (
            "timeout",
            erroring_transport(|| claude_code_rs::Error::Timeout),
            "timeout",
        ),
        (
            "cli_error",
            erroring_transport(|| claude_code_rs::Error::Api {
                status: None,
                message: "boom".to_string(),
                session_id: None,
                cost_usd: 0.0,
                usage: claude_code_rs::parse::Usage {
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
            }),
            "cli_error",
        ),
        (
            "no_structured_result",
            text_transport(vec!["not json at all, and no fence either".to_string()]),
            "no_structured_result",
        ),
        (
            "schema_violation",
            queued_transport(vec![
                json!({"verdict": "NOPE", "action": "NONE", "reason": "x"}),
            ]),
            "schema_violation",
        ),
    ];

    for (label, transport, expected_error_kind) in cases {
        let fx = fixture();
        write_done_state(&fx.brain_root.path().join("repo-a"), "A.1");
        let message_id = format!("id-defer-{label}");
        write_message(
            fx.lock_dir.path(),
            "repo-a",
            "engine-rs",
            &message_id,
            "FINDING",
            "2026-09-11T00:00:14Z",
            "repo-a",
            Some("A.1"),
            "a finding",
            "bastion",
            "types",
        );

        let resolve_depends_on = |_repo: &str, _id: &str| Vec::new();
        let is_edge_met = |_repo: &str, _id: &str| true;
        let (runner, _calls) = recording_runner();
        let coord = CoordHandle::new(
            fx.lock_dir.path().to_path_buf(),
            "repo-a",
            "engine-rs",
            "engine-rs-1",
            coord_now_iso,
        );
        let inbox_runner =
            InboxTriageRunner::new(InboxTriageConfig::default()).with_transport(transport);

        let chain = vec![step("repo-a", "A.1")];
        let (result, _report, inbox_report) = run_chain(
            &chain,
            &fx.registry,
            &runner,
            fx.roadmap_dir.path(),
            Some(&coord),
            &resolve_depends_on,
            &is_edge_met,
            OnBail::SkipDependents,
            true,
            Some(&inbox_runner),
        )
        .await;

        result.unwrap_or_else(|err| panic!("[{label}] chain must complete: {err}"));
        assert_eq!(inbox_report.len(), 1, "[{label}]");
        assert_eq!(
            inbox_report[0].error_kind.as_deref(),
            Some(expected_error_kind),
            "[{label}]"
        );
        assert!(inbox_report[0].verdict.is_none(), "[{label}]");

        let reply = read_only_reply(fx.lock_dir.path(), "bastion", "types");
        assert!(reply.contains("ACK DEFERRED"), "[{label}]: {reply}");
        assert!(reply.contains(expected_error_kind), "[{label}]: {reply}");

        let queue_dir = fx
            .lock_dir
            .path()
            .join("queue")
            .join("repo-a")
            .join("engine-rs");
        assert_eq!(
            std::fs::read_dir(queue_dir.join("done")).unwrap().count(),
            1,
            "[{label}]: the message must be moved to done/"
        );

        let escalations_path = fx.roadmap_dir.path().join("escalations.jsonl");
        assert!(
            !escalations_path.exists(),
            "[{label}]: a failed judgment must never escalate"
        );
    }
}

// ── (9) The report accumulates across boundaries ────────────────────────────────────────

/// `inbox_report` holds exactly one entry per processed message across a chain with
/// messages delivered at two DIFFERENT block boundaries.
#[tokio::test]
async fn inbox_triage_report_accumulates() {
    let fx = fixture();
    write_done_state(&fx.brain_root.path().join("repo-a"), "A.1");
    write_done_state(&fx.brain_root.path().join("repo-a"), "A.2");
    write_done_state(&fx.brain_root.path().join("repo-a"), "A.3");

    // Sits in the inbox before the chain even starts — drained at A.1's own boundary.
    write_message(
        fx.lock_dir.path(),
        "repo-a",
        "engine-rs",
        "id-boundary-1",
        "FINDING",
        "2026-09-11T00:00:15Z",
        "repo-a",
        Some("A.1"),
        "first",
        "bastion",
        "types",
    );

    let lock_dir_path = fx.lock_dir.path().to_path_buf();
    // Written from inside A.1's own execution — reaches the queue too late for A.1's own
    // boundary and can only be drained at A.2's, a DIFFERENT boundary than the first message.
    let runner: FlowRunner = {
        let lock_dir_path = lock_dir_path.clone();
        Arc::new(
            move |invocation: engine_core::workflows::orchestration::execute::FlowInvocation| {
                let lock_dir_path = lock_dir_path.clone();
                let block_id = invocation.block_id.clone();
                Box::pin(async move {
                    if block_id == "A.1" {
                        write_message(
                            &lock_dir_path,
                            "repo-a",
                            "engine-rs",
                            "id-boundary-2",
                            "FINDING",
                            "2026-09-11T00:00:16Z",
                            "repo-a",
                            Some("A.2"),
                            "second",
                            "bastion",
                            "types",
                        );
                    }
                    Ok(engine_contract::TaskContext {
                        event: json!({}),
                        nodes: HashMap::new(),
                        metadata: json!({}),
                        node_runs: HashMap::new(),
                    })
                })
            },
        )
    };

    let resolve_depends_on = |_repo: &str, _id: &str| Vec::new();
    let is_edge_met = |_repo: &str, _id: &str| true;
    let coord = CoordHandle::new(
        fx.lock_dir.path().to_path_buf(),
        "repo-a",
        "engine-rs",
        "engine-rs-1",
        coord_now_iso,
    );
    let verdict = json!({"verdict": "ACCEPTED", "action": "NONE", "reason": "noted"});
    let inbox_runner = InboxTriageRunner::new(InboxTriageConfig::default())
        .with_transport(queued_transport(vec![verdict.clone(), verdict]));

    let chain = vec![
        step("repo-a", "A.1"),
        step("repo-a", "A.2"),
        step("repo-a", "A.3"),
    ];
    let (result, _report, inbox_report) = run_chain(
        &chain,
        &fx.registry,
        &runner,
        fx.roadmap_dir.path(),
        Some(&coord),
        &resolve_depends_on,
        &is_edge_met,
        OnBail::SkipDependents,
        true,
        Some(&inbox_runner),
    )
    .await;

    let outcomes = result.expect("chain must complete");
    assert_eq!(outcomes.len(), 3);
    assert_eq!(
        inbox_report.len(),
        2,
        "exactly one entry per processed message across two different block boundaries: \
         {inbox_report:?}"
    );
    let ids: HashSet<&str> = inbox_report.iter().map(|p| p.message_id.as_str()).collect();
    assert!(ids.contains("id-boundary-1"));
    assert!(ids.contains("id-boundary-2"));
}
