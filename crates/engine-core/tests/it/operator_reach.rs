//! `EN.17.C` task 7 — black-box integration coverage for the whole block: a
//! chain that bails composes one `notification` escalation per bailed block
//! via `record_bail_escalation`, then a single post-loop `run_sweep_pass`
//! call routes it through the one shared SWEEP router — dedup, permission
//! profile and the per-pass operator budget included — never a parallel
//! delivery path.
//!
//! Driven entirely through `engine_core`'s PUBLIC surface
//! (`OrchestrationRunNode::process` + `with_operator_transport`), mirroring
//! the discipline `orchestration_bail.rs`/`orchestration_chain.rs` already
//! use: fixture helpers are duplicated here rather than exported across the
//! crate boundary, and every fixture repo is a REAL `tempfile::tempdir()`
//! git repository (`git init` + one commit) — never a bare directory stand-
//! in — because `record_bail_escalation`'s own `subject_repo_short_sha`
//! shells out to `git rev-parse --short=7 HEAD` in the repo path and skips
//! composing the escalation entirely (with only a `tracing::warn!`) when
//! that fails, exactly as it does for a plain directory in `graph.rs`'s own
//! `#[cfg(test)]` fixtures (which is why those never inspect
//! `escalations.jsonl` content).
//!
//! ## The per-pass operator budget — why "one send per bailed block" reads
//! ## as exactly one send, not `bailed.len()`
//!
//! `route::route_escalation`'s own module doc (`sweep/route.rs`) is explicit:
//! "At most one operator notify per pass" — a single [`route::Budget`] is
//! shared, by `&mut`, across every escalation `run_sweep_pass` routes in one
//! call, and a chain calls `run_sweep_pass` **exactly once** for the whole
//! chain (`EN.17.C` task 3), never once per bailed block. So a chain with
//! two bailed blocks under `bail_channel: notification` writes TWO
//! escalation lines, and `run_sweep_pass` routes both — but only the FIRST
//! reaches [`OperatorTransport::send`]; the second is recorded as
//! `action: "skip-operator-budget"`, `routed: false` (never silently
//! dropped — the same "recorded, never dropped" discipline
//! `suppressed_by_profile` uses), to retry on a later sweep. This is
//! verified directly below (`operator_reach_one_send_per_bailed_block`)
//! against the written `sweeps/<ts>.json` document, not invented.
//!
//! ## `record_bail_escalation`'s payload text does not enumerate skipped
//! ## dependents
//!
//! Per this task's own instruction ("construct the payload text assertion
//! against whatever `record_bail_escalation` actually composes, do not
//! invent a format"): its `summary` field is `err_display` truncated to
//! `SUMMARY_MAX_CHARS` (`integrate.rs`) — the failing step's own error
//! text — and nothing else. A skipped dependent is recorded only in
//! `ChainReport::skipped` (never routed through SWEEP at all — the skip
//! loop never calls `record_bail_escalation`), so this file asserts the
//! sent payload names its OWN bailed block (via the runner's error message,
//! which is constructed to include the block id), and asserts the skipped
//! dependent separately, against `chain_report.skipped`, rather than
//! inventing a payload shape that folds the two together.
//!
//! STANDING RULE 8: this file is a `mod` of `tests/it/main.rs`, never a new
//! `crates/engine-core/tests/*.rs` binary.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use engine_contract::TaskContext;
use engine_core::node::Node;
use engine_core::operator::transport::{
    DeliveredMessage, NotifyError, OperatorResponse, OperatorTransport, UpdateCursor,
};
use engine_core::operator::ValidatedOperatorPayload;
use engine_core::workflows::orchestration::execute::{FlowInvocation, FlowRunner};
use engine_core::workflows::orchestration::gates::DependencyEdge;
use engine_core::workflows::orchestration::graph::{OrchestrationRunNode, NODE_NAME};
use engine_core::WorkflowError;

const ROADMAP: &str = "operator-reach-fixture";

// ── Git process helper — a REAL repo, so `subject_repo_short_sha` succeeds ─

fn run_git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn `git {}` in {cwd:?}: {e}", args.join(" ")));
    assert!(
        output.status.success(),
        "`git {}` in {cwd:?} failed:\nstdout: {}\nstderr: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// `git init` plus one commit — the minimum `subject_repo_short_sha` needs
/// (`git rev-parse --short=7 HEAD` inside `path`). No remote, no worktree:
/// this task's fixtures never push or fetch.
fn init_real_git_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    run_git(path, &["init", "-q"]);
    run_git(path, &["checkout", "-q", "-b", "main"]);
    run_git(
        path,
        &[
            "config",
            "user.email",
            "operator-reach-fixture@example.invalid",
        ],
    );
    run_git(path, &["config", "user.name", "operator_reach fixture"]);
    std::fs::write(path.join("README.md"), "operator_reach fixture\n").unwrap();
    run_git(path, &["add", "-A"]);
    run_git(path, &["commit", "-q", "-m", "initial commit"]);
}

// ── `[permission_profiles]` fragments — mirrors
// `tests/fixtures/permission_profiles_brain.toml`'s shape (`EN.12.C`) rather
// than reusing that file directly, since this module builds its `brain.toml`
// in a tempdir it also owns the `[[repos]]` lines for. `resolve_permission_
// profile` (`policy/permission.rs`) fails CLOSED to `Locked` when
// `[permission_profiles]` is absent entirely — that fail-closed default is
// what `operator_reach_locked_profile_suppresses` below relies on, rather
// than spelling out a `locked`-default table by hand. ────────────────────

const PERMISSION_PROFILES_STANDARD: &str = r#"
[permission_profiles]
never_allowed = ["clear_operator_gate"]
default = "standard"

[permission_profiles.levels.locked]
id = "locked"
meaning = "locked"
mini_install = false
main_push = false
cross_repo_write = false

[permission_profiles.levels.standard]
id = "standard"
meaning = "standard"
mini_install = false
main_push = true
cross_repo_write = true

[permission_profiles.levels.unrestricted]
id = "unrestricted"
meaning = "unrestricted"
mini_install = true
main_push = true
cross_repo_write = true
"#;

/// A tempdir `brain.toml` + two REAL git repos (`repo-a`, `repo-b`), plus
/// `planning/roadmaps/<ROADMAP>/` — mirrors `graph.rs`'s own `#[cfg(test)]`
/// `two_repo_brain_root`, but with real git repos (so a bail's escalation is
/// actually composed, not silently skipped) and a configurable
/// `[permission_profiles]` fragment appended verbatim.
fn two_repo_brain_root(permission_profiles_toml: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    init_real_git_repo(&dir.path().join("repo-a"));
    init_real_git_repo(&dir.path().join("repo-b"));
    std::fs::create_dir_all(dir.path().join("planning").join("roadmaps").join(ROADMAP)).unwrap();
    let brain_toml = format!(
        "[[repos]]\nslug = \"repo-a\"\nrepo_path = \"repo-a\"\n\
         [[repos]]\nslug = \"repo-b\"\nrepo_path = \"repo-b\"\n{permission_profiles_toml}"
    );
    std::fs::write(dir.path().join("brain.toml"), brain_toml).unwrap();
    dir
}

fn sweeps_dir_for(dir: &tempfile::TempDir) -> std::path::PathBuf {
    dir.path()
        .join("planning")
        .join("roadmaps")
        .join(ROADMAP)
        .join("sweeps")
}

fn escalations_path_for(dir: &tempfile::TempDir) -> std::path::PathBuf {
    dir.path()
        .join("planning")
        .join("roadmaps")
        .join(ROADMAP)
        .join("escalations.jsonl")
}

fn read_escalation_lines(dir: &tempfile::TempDir) -> Vec<Value> {
    let path = escalations_path_for(dir);
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    contents
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("bad json line {l:?}: {e}")))
        .collect()
}

/// The single `sweeps/<ts>.json` document written by the most recent
/// `run_sweep_pass` call, as raw JSON — read directly (rather than through
/// `sweep::load_snapshot`, which parses only the [`RawSnapshot`] half) so
/// `routed`/`diff` are visible too.
fn read_latest_sweep_doc(dir: &tempfile::TempDir) -> Value {
    let sweeps_dir = sweeps_dir_for(dir);
    let mut files: Vec<_> = std::fs::read_dir(&sweeps_dir)
        .unwrap_or_else(|e| panic!("expected {sweeps_dir:?} to exist: {e}"))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    files.sort();
    let latest = files
        .last()
        .unwrap_or_else(|| panic!("expected at least one sweep snapshot under {sweeps_dir:?}"));
    let contents = std::fs::read_to_string(latest).expect("read sweep snapshot");
    serde_json::from_str(&contents).expect("sweep snapshot must be valid JSON")
}

// ── FlowRunner — fails exactly the named block ids, succeeds every other ──

fn run_flow_failing(fail_ids: &'static [&'static str]) -> FlowRunner {
    Arc::new(move |invocation: FlowInvocation| {
        let block_id = invocation.block_id.clone();
        let repo_path = invocation.repo_path.clone();
        Box::pin(async move {
            if fail_ids.contains(&block_id.as_str()) {
                Err(WorkflowError::new(format!(
                    "simulated failure for {block_id}"
                )))
            } else {
                let dir = repo_path.join("planning").join(&block_id).join("sdlc");
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(
                    dir.join("sdlc-flow-state.json"),
                    json!({ "status": "done" }).to_string(),
                )
                .unwrap();
                Ok(TaskContext {
                    event: json!({}),
                    nodes: HashMap::new(),
                    metadata: json!({}),
                    node_runs: HashMap::new(),
                })
            }
        })
    })
}

// ── A counting `OperatorTransport` stub — records both the call count and
// each delivered payload's rendered text, so a test can assert on the
// ACTUAL text `record_bail_escalation`/`route_escalation` composed rather
// than inventing one. ───────────────────────────────────────────────────

struct CountingTransport {
    sends: AtomicUsize,
    texts: Mutex<Vec<String>>,
}

impl CountingTransport {
    fn new() -> Self {
        Self {
            sends: AtomicUsize::new(0),
            texts: Mutex::new(Vec::new()),
        }
    }

    fn send_count(&self) -> usize {
        self.sends.load(Ordering::SeqCst)
    }

    fn texts(&self) -> Vec<String> {
        self.texts.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl OperatorTransport for CountingTransport {
    async fn send(
        &self,
        payload: &ValidatedOperatorPayload,
    ) -> Result<DeliveredMessage, NotifyError> {
        self.sends.fetch_add(1, Ordering::SeqCst);
        self.texts
            .lock()
            .unwrap()
            .push(payload.payload().rendered_summary.clone());
        Ok(DeliveredMessage {
            transport_message_id: String::new(),
        })
    }

    async fn poll_responses(
        &self,
        since: Option<UpdateCursor>,
    ) -> Result<(Vec<OperatorResponse>, Option<UpdateCursor>), NotifyError> {
        Ok((Vec::new(), since))
    }
}

fn event_ctx(event: Value) -> TaskContext {
    TaskContext {
        event,
        nodes: HashMap::new(),
        metadata: json!({}),
        node_runs: HashMap::new(),
    }
}

// ── operator_reach_one_send_per_bailed_block ──────────────────────────────

/// A chain with two bailed blocks (`A.1`, independent `C.1`) and one
/// dependent skipped by `EN.17.B`'s skip-dependents (`B.1`, which declares a
/// `block` edge on `A.1`) under `bail_channel: notification` and a
/// permission profile that permits `GatedAction::Notify` (`standard`).
///
/// `record_bail_escalation` composes and appends TWO escalation lines (one
/// per bailed block — never one for the merely-skipped `B.1`), and the
/// single post-loop `run_sweep_pass` call routes both — but the shared
/// per-pass [`engine_core::workflows::sweep::Budget`] permits only the
/// FIRST through to [`OperatorTransport::send`] (see this file's module
/// doc). So the transport sees exactly ONE send, naming the bailed block
/// whose escalation was written first (`A.1`, the chain's own dispatch
/// order); the second (`C.1`) is still recorded in the written
/// `sweeps/<ts>.json` as `action: "skip-operator-budget"`, `routed: false`
/// — retried on a later sweep, never dropped.
#[tokio::test]
async fn operator_reach_one_send_per_bailed_block() {
    let dir = two_repo_brain_root(PERMISSION_PROFILES_STANDARD);
    let transport = Arc::new(CountingTransport::new());
    let run_flow = run_flow_failing(&["A.1", "C.1"]);

    let node = OrchestrationRunNode::new()
        .with_run_flow(run_flow)
        .with_operator_transport(transport.clone())
        .with_resolve_depends_on(Arc::new(|repo: &str, id: &str| -> Vec<DependencyEdge> {
            if repo == "repo-a" && id == "B.1" {
                vec![DependencyEdge::Block {
                    repo: "repo-a".to_string(),
                    block_id: "A.1".to_string(),
                }]
            } else {
                Vec::new()
            }
        }));

    let ctx = event_ctx(json!({
        "brain_root": dir.path(),
        "blocks": [
            { "repo": "repo-a", "block_id": "A.1" },
            { "repo": "repo-a", "block_id": "B.1" },
            { "repo": "repo-b", "block_id": "C.1" },
        ],
        "roadmap_slug": ROADMAP,
        "policy": { "on_bail": "skip_dependents", "bail_channel": "notification" },
    }));

    let err = node.process(ctx).await.expect_err(
        "a chain with bailed blocks still surfaces as the chain's own soft-Err outcome",
    );
    let node_result = err
        .node_result
        .as_ref()
        .expect("the soft-Err path must carry node_result");
    assert_eq!(
        node_result["chain_report"]["bailed"],
        json!(["repo-a:A.1", "repo-b:C.1"]),
        "both A.1 and C.1 must be recorded as bailed"
    );
    assert_eq!(
        node_result["chain_report"]["skipped"]
            .as_array()
            .map(Vec::len),
        Some(1),
        "B.1 must be recorded as skipped, not bailed, since it only depends on A.1"
    );

    // Two bailed blocks -> two escalation lines, never one per skipped
    // dependent (the skip loop never calls `record_bail_escalation`).
    let escalations = read_escalation_lines(&dir);
    assert_eq!(
        escalations.len(),
        2,
        "one escalation line per BAILED block, not per skipped dependent: {escalations:?}"
    );

    // The per-pass operator budget permits exactly one send, never
    // `chain_report.bailed.len()` sends -- see this file's module doc.
    assert_eq!(
        transport.send_count(),
        1,
        "the shared per-pass operator-notify budget permits exactly one send per sweep pass, \
         regardless of how many blocks bailed"
    );
    let texts = transport.texts();
    assert_eq!(texts.len(), 1);
    assert!(
        texts[0].contains("A.1"),
        "the one delivered payload must name ITS OWN bailed block (A.1, dispatched first): {texts:?}"
    );

    // The second bail's escalation is still recorded in the written sweep
    // document as suppressed by the per-pass budget -- not silently
    // dropped, retried on a later sweep.
    let doc = read_latest_sweep_doc(&dir);
    let routed = doc["routed"]
        .as_array()
        .expect("sweep document must carry a routed array");
    assert_eq!(
        routed.len(),
        2,
        "both escalations must appear in the routed record: {routed:?}"
    );
    let budget_skipped = routed
        .iter()
        .filter(|r| r["action"] == json!("skip-operator-budget"))
        .count();
    assert_eq!(
        budget_skipped, 1,
        "exactly one route must have been suppressed by the per-pass operator budget: {routed:?}"
    );
    let notified = routed
        .iter()
        .filter(|r| r["action"] == json!("notify-ask") && r["routed"] == json!(true))
        .count();
    assert_eq!(
        notified, 1,
        "exactly one route must have actually notified: {routed:?}"
    );
}

// ── operator_reach_clean_chain_sends_nothing ──────────────────────────────

/// A clean chain (no bail) never calls `run_sweep_pass` at all -- the
/// counting seam here is TWO independent signals: the transport's own zero
/// send count, and the absence of a `sweeps/` directory entirely (a real
/// `run_sweep_pass` call always creates and writes into it, so its
/// nonexistence proves the function itself was never invoked, not merely
/// that it routed nothing).
#[tokio::test]
async fn operator_reach_clean_chain_sends_nothing() {
    let dir = two_repo_brain_root(PERMISSION_PROFILES_STANDARD);
    let transport = Arc::new(CountingTransport::new());
    let run_flow = run_flow_failing(&[]); // nothing fails

    let node = OrchestrationRunNode::new()
        .with_run_flow(run_flow)
        .with_operator_transport(transport.clone());

    let ctx = event_ctx(json!({
        "brain_root": dir.path(),
        "blocks": [
            { "repo": "repo-a", "block_id": "A.1" },
            { "repo": "repo-b", "block_id": "B.1" },
        ],
        "roadmap_slug": ROADMAP,
        "policy": { "bail_channel": "notification" },
    }));

    let out = node.process(ctx).await.expect("a clean chain must succeed");
    assert_eq!(out.nodes[NODE_NAME]["steps_integrated"], 2);
    assert_eq!(
        transport.send_count(),
        0,
        "a clean chain must never route anything through the operator transport"
    );
    assert!(
        !sweeps_dir_for(&dir).exists(),
        "a clean chain must never call run_sweep_pass at all -- no sweeps/ directory should exist"
    );
}

// ── operator_reach_refire_window_dedups ───────────────────────────────────

/// Running the same bailing chain's sweep pass a second time, immediately
/// afterward (well inside SWEEP's `DEFAULT_REFIRE_HOURS` refire window),
/// makes zero ADDITIONAL send calls: the second pass's escalation shares the
/// first's `gate_id`, `route_escalation`'s dedup/refire gate finds a very
/// recent prior route for it in `dedup_history` (built from the FIRST
/// pass's own written `sweeps/<ts>.json`) and returns `action: "skip-dedup"`
/// before ever reaching the transport.
#[tokio::test]
async fn operator_reach_refire_window_dedups() {
    let dir = two_repo_brain_root(PERMISSION_PROFILES_STANDARD);
    let transport = Arc::new(CountingTransport::new());
    let run_flow = run_flow_failing(&["A.1"]);

    let node = OrchestrationRunNode::new()
        .with_run_flow(run_flow)
        .with_operator_transport(transport.clone());

    let ctx = || {
        event_ctx(json!({
            "brain_root": dir.path(),
            "blocks": [{ "repo": "repo-a", "block_id": "A.1" }],
            "roadmap_slug": ROADMAP,
            "policy": { "on_bail": "skip_dependents", "bail_channel": "notification" },
        }))
    };

    node.process(ctx())
        .await
        .expect_err("A.1 bails, so the chain's own outcome is an Err");
    assert_eq!(
        transport.send_count(),
        1,
        "the first pass sends exactly once"
    );

    node.process(ctx())
        .await
        .expect_err("A.1 bails again identically on the second pass");
    assert_eq!(
        transport.send_count(),
        1,
        "a second pass, well inside the refire window, must add ZERO further sends"
    );

    let doc = read_latest_sweep_doc(&dir);
    let routed = doc["routed"].as_array().expect("routed array");
    assert!(
        routed.iter().any(|r| r["action"] == json!("skip-dedup")),
        "the second pass's own routed record must show the refire/dedup gate firing: {routed:?}"
    );
}

// ── operator_reach_locked_profile_suppresses ──────────────────────────────

/// Under permission profile `locked` (the fail-closed default this fixture
/// gets from an ABSENT `[permission_profiles]` table -- `policy/
/// permission.rs`'s `resolve_permission_profile` never fails open), a
/// bailing chain under `bail_channel: notification` makes zero send calls,
/// and the sweep outcome records the route `suppressed_by_profile: true` --
/// recorded, never silently dropped.
#[tokio::test]
async fn operator_reach_locked_profile_suppresses() {
    let dir = two_repo_brain_root(""); // no [permission_profiles] table at all -> Locked
    let transport = Arc::new(CountingTransport::new());
    let run_flow = run_flow_failing(&["A.1"]);

    let node = OrchestrationRunNode::new()
        .with_run_flow(run_flow)
        .with_operator_transport(transport.clone());

    let ctx = event_ctx(json!({
        "brain_root": dir.path(),
        "blocks": [{ "repo": "repo-a", "block_id": "A.1" }],
        "roadmap_slug": ROADMAP,
        "policy": { "on_bail": "skip_dependents", "bail_channel": "notification" },
    }));

    node.process(ctx)
        .await
        .expect_err("A.1 bails, so the chain's own outcome is an Err");

    assert_eq!(
        transport.send_count(),
        0,
        "a locked permission profile must suppress the notify -- zero sends"
    );

    let doc = read_latest_sweep_doc(&dir);
    let routed = doc["routed"].as_array().expect("routed array");
    assert_eq!(routed.len(), 1);
    assert_eq!(routed[0]["action"], json!("notify-ask"));
    assert_eq!(routed[0]["routed"], json!(false));
    assert_eq!(
        routed[0]["suppressed_by_profile"],
        json!(true),
        "the route must be recorded as suppressed by the permission profile, never dropped: {routed:?}"
    );
}

// ── operator_reach_session_default_is_unchanged ───────────────────────────

/// The built-in `bail_channel: session` (no policy override at all):
/// `record_bail_escalation` still writes `channel: "session:<lane>"`
/// exactly as before `EN.17.C` -- the no-op default this whole block's
/// knob is contracted to preserve (CLAUDE.md standing rule 6). An explicit
/// chain has no lane by construction, so `lane` falls back to the step's
/// own repo slug (`record_bail_escalation`'s own `lane` parameter, mirroring
/// `orchestration_chain.rs`'s equivalent assertion for lane-log lines).
#[tokio::test]
async fn operator_reach_session_default_is_unchanged() {
    let dir = two_repo_brain_root(PERMISSION_PROFILES_STANDARD);
    let run_flow = run_flow_failing(&["A.1"]);
    let node = OrchestrationRunNode::new().with_run_flow(run_flow);

    let ctx = event_ctx(json!({
        "brain_root": dir.path(),
        "blocks": [{ "repo": "repo-a", "block_id": "A.1" }],
        "roadmap_slug": ROADMAP,
        // No `policy` key at all -- the built-in default (`bail_channel:
        // session`) must govern.
    }));

    node.process(ctx)
        .await
        .expect_err("A.1 bails, so the chain's own outcome is an Err");

    let escalations = read_escalation_lines(&dir);
    assert_eq!(escalations.len(), 1);
    assert_eq!(
        escalations[0]["channel"],
        json!("session:repo-a"),
        "the built-in bail_channel default must still compose session:<lane>, unchanged: \
         {:?}",
        escalations[0]
    );
    assert!(
        escalations[0].get("options").is_none(),
        "a session channel never carries an options field: {:?}",
        escalations[0]
    );

    // No `run_sweep_pass` call at all is required for this default (StopChain
    // is the built-in `on_bail`, so the hard-Err path returns before ever
    // reaching the sweep call site) -- this test only pins the composed
    // escalation record itself, not delivery.
}
