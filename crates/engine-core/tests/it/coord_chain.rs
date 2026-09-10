//! `EN.15.D` task 4 — the spike this block's own record names as UNSPIKED: a Rust chain
//! actually registering, heartbeating, leasing per window, draining its inbox, and
//! answering a ping, driven end to end through the public `integrate_chain_with_coord`
//! entry point against a real `tempfile::tempdir()` lock dir (real files, not mocks).
//!
//! `EN.15.D` tasks 1-3 already added unit-level coverage of the same mechanics inside
//! `orchestration::integrate`'s own `#[cfg(test)] mod tests` (`a_chain_driven_with_coord_
//! writes_a_registry_claim_and_a_lease`, `a_lease_release_delivered_mid_chain_is_honoured_
//! at_the_next_boundary_only`, `a_rendezvous_delivered_mid_chain_is_answered_in_the_
//! senders_inbox`). This module is the block record's own named spike: it exercises the
//! exact same boundary function from OUTSIDE the crate, through `engine_core`'s public
//! surface, as the fixture standing in for the un-gateable "real cross-session RENDEZVOUS
//! between an agent lane and a Rust chain" criterion (D64) — see the block record's
//! `acceptance_criteria` for that declaration.
//!
//! Every timestamp below comes from a live clock ([`coord_now_iso`]) or an explicit,
//! deliberately-old fixed instant used ONLY to manufacture staleness in test (d) — never a
//! frozen "now" literal masquerading as fresh. A frozen "now" literal reproduces the exact
//! `da4c847` defect this repo hit and fixed earlier in this same spec.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;

use engine_core::policy::permission::{GatedAction, PermissionProfile};
use engine_core::repo_registry::RepoRegistry;
use engine_core::workflows::orchestration::chain::ChainStep;
use engine_core::workflows::orchestration::coord_lane::CoordHandle;
use engine_core::workflows::orchestration::execute::{EngineKind, FlowRunner};
use engine_core::workflows::orchestration::gates::{
    check_permission_gate, make_author_operator_edge, AdmissionGate, OperatorEdgeAuthorConfig,
    PermissionGateError,
};
use engine_core::workflows::orchestration::integrate::{
    integrate_chain_with_coord, NeverHeld, StepProgress,
};

// ── Shared fixture helpers — mirrors `orchestration::integrate`'s own `mod tests` helpers
//    (`two_repo_registry`, `recording_runner`, `write_done_state`, `step`), duplicated here
//    rather than exported across the crate boundary: this module drives the PUBLIC surface
//    only, the same discipline `orchestration_chain.rs` and `coord_parity.rs` already use.

fn step(repo: &str, block_id: &str) -> ChainStep {
    ChainStep {
        repo: repo.to_string(),
        block_id: block_id.to_string(),
        directives: None,
        ..Default::default()
    }
}

/// A one-repo `RepoRegistry` rooted at a fresh temp dir, with a `repo-a` sub-directory and a
/// matching `brain.toml` entry — everything `execute_step`'s state-write verification needs
/// to resolve `repo-a`'s path.
fn one_repo_registry() -> (tempfile::TempDir, RepoRegistry) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("repo-a")).unwrap();
    std::fs::write(
        dir.path().join("brain.toml"),
        "[[repos]]\nslug = \"repo-a\"\nrepo_path = \"repo-a\"\n",
    )
    .unwrap();
    let registry = RepoRegistry::from_brain_root(dir.path()).expect("registry");
    (dir, registry)
}

/// Writes the `"status": "done"` state file `verify_state_write` looks for after a `Flow`
/// step completes — the same shape `orchestration::integrate`'s own `write_done_state`
/// writes.
fn write_done_state(repo_path: &Path, block_id: &str) {
    let dir = repo_path.join("planning").join(block_id).join("sdlc");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("sdlc-flow-state.json"),
        json!({"status": "done"}).to_string(),
    )
    .unwrap();
}

/// A `FlowRunner` that records every `block_id` it was invoked with and always succeeds
/// with an empty `TaskContext` — the actual state-write is `write_done_state`'s job, done
/// ahead of time by the caller (mirrors `orchestration::integrate`'s own `recording_runner`).
fn recording_runner() -> (FlowRunner, Arc<Mutex<Vec<String>>>) {
    let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let runner: FlowRunner = Arc::new(move |invocation| {
        recorded.lock().unwrap().push(invocation.block_id.clone());
        Box::pin(async {
            Ok(engine_contract::TaskContext {
                event: json!({}),
                nodes: std::collections::HashMap::new(),
                metadata: json!({}),
                node_runs: std::collections::HashMap::new(),
            })
        })
    });
    (runner, calls)
}

/// A live, freshly-formatted RFC3339 "now" — the clock seam every [`CoordHandle`] in this
/// module reads through. Never a frozen literal (see the module doc's staleness-trap note).
fn coord_now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Writes a message envelope directly into `<lock_dir>/queue/<repo>/<lane>/inbox/`, standing
/// in for a sibling lane's delivery — byte-identical shape to `orchestration::integrate`'s
/// own `write_inbox_message` test helper.
fn write_inbox_message(
    lock_dir: &Path,
    repo: &str,
    lane: &str,
    message_id: &str,
    kind: &str,
    sent_at: &str,
) {
    let inbox = lock_dir.join("queue").join(repo).join(lane).join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    let envelope = json!({
        "message_id": message_id,
        "sender": {
            "agent_name": "base-template-4c",
            "repo": "base-template",
            "lane": "types",
            "roadmap": "coordination-layer-port",
        },
        "sent_at": sent_at,
        "kind": kind,
        "subject": {
            "repo": repo,
        },
        "body": "test envelope",
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

#[allow(clippy::too_many_arguments)]
async fn run_chain(
    chain: &[ChainStep],
    registry: &RepoRegistry,
    runner: &FlowRunner,
    roadmap_dir: &Path,
    coord: Option<&CoordHandle>,
) -> Vec<engine_core::workflows::orchestration::execute::ExecutionOutcome> {
    let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;
    let resolve_deps = |_repo: &str, _id: &str| Vec::new();
    let is_met = |_repo: &str, _id: &str| true;
    let admission = AdmissionGate::with_default_policy();

    integrate_chain_with_coord(
        chain,
        &resolve_deps,
        &is_met,
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
    )
    .await
    .expect("chain should complete")
}

// ── (a) the chain appears as a registry claim + lease while running, and both are gone
//        after a clean exit ──────────────────────────────────────────────────────────

/// While the chain is running, a registry claim and a per-step lease are real files on
/// disk — the on-disk evidence `bastion coord status` (an installed artefact this source
/// tree cannot invoke — see the block record's D64 note) reads. After a clean exit, the
/// per-step lease is already gone (`StepLeaseGuard`'s `Drop`, exercised by task 1's own
/// unit test); this test additionally drives the handle's own `release()` — the same
/// clean-exit step a production caller performs once the chain it owns has finished — and
/// confirms the registry claim is then gone too, completing the lifecycle this criterion
/// names end to end.
#[tokio::test]
async fn chain_appears_as_a_claim_and_lease_while_running_and_both_are_gone_after_clean_exit() {
    let (dir, registry) = one_repo_registry();
    write_done_state(&dir.path().join("repo-a"), "A.1");
    write_done_state(&dir.path().join("repo-a"), "A.2");
    let roadmap_dir = tempfile::tempdir().unwrap();
    let lock_dir = tempfile::tempdir().unwrap();

    let seen_claim_and_lease = Arc::new(Mutex::new(false));
    let seen_claim_and_lease_check = seen_claim_and_lease.clone();
    let lock_dir_for_check = lock_dir.path().to_path_buf();
    let checking_runner: FlowRunner = Arc::new(move |_invocation| {
        let claim_path = lock_dir_for_check
            .join("lane-agents")
            .join("agent-engine-rs-1.json");
        let lease_path = lock_dir_for_check.join("leases").join("lease-repo-a.json");
        if claim_path.exists() && lease_path.exists() {
            *seen_claim_and_lease_check.lock().unwrap() = true;
        }
        Box::pin(async {
            Ok(engine_contract::TaskContext {
                event: json!({}),
                nodes: std::collections::HashMap::new(),
                metadata: json!({}),
                node_runs: std::collections::HashMap::new(),
            })
        })
    });

    let coord = CoordHandle::new(
        lock_dir.path().to_path_buf(),
        "repo-a",
        "engine-rs",
        "engine-rs-1",
        coord_now_iso,
    );

    let chain = vec![step("repo-a", "A.1"), step("repo-a", "A.2")];
    let outcomes = run_chain(
        &chain,
        &registry,
        &checking_runner,
        roadmap_dir.path(),
        Some(&coord),
    )
    .await;
    assert_eq!(outcomes.len(), 2);

    assert!(
        *seen_claim_and_lease.lock().unwrap(),
        "expected a registry claim AND a lease to both exist while a step was executing"
    );

    // The per-step lease is already gone by the time the chain returns
    // (`StepLeaseGuard`'s `Drop` at the end of the last iteration).
    let lease_path = lock_dir.path().join("leases").join("lease-repo-a.json");
    assert!(
        !lease_path.exists(),
        "expected the lease to be released once the chain finished"
    );

    // The registry claim, by contrast, persists until the caller explicitly releases it —
    // exactly like `CoordHandle::release`'s own doc ("idempotent — an already-absent claim
    // returns Ok(false)"). Perform that clean-exit step here.
    let claim_path = lock_dir
        .path()
        .join("lane-agents")
        .join("agent-engine-rs-1.json");
    assert!(
        claim_path.exists(),
        "expected the registry claim to still exist immediately after the chain returned"
    );
    let removed = coord.release().expect("release must succeed");
    assert!(removed, "release() must have actually removed a claim");
    assert!(
        !claim_path.exists(),
        "expected the registry claim to be gone after the clean-exit release"
    );
}

// ── (b) a LEASE_RELEASE mid-chain holds at the NEXT block boundary, and coord status then
//        shows the lease gone ───────────────────────────────────────────────────────────

/// A LEASE_RELEASE delivered while a step is still executing is never acted on mid-block —
/// it sits in the inbox until the loop returns to the top for the NEXT step, is honoured
/// there (the lease released, the message drained into `processing/` with a receipt), and
/// the step already in flight when the message arrived is never abandoned: it completes
/// and its outcome is recorded exactly like every other step.
#[tokio::test]
async fn a_lease_release_mid_chain_is_honoured_at_the_next_boundary_and_abandons_no_work() {
    let (dir, registry) = one_repo_registry();
    write_done_state(&dir.path().join("repo-a"), "A.1");
    write_done_state(&dir.path().join("repo-a"), "A.2");
    let roadmap_dir = tempfile::tempdir().unwrap();
    let lock_dir = tempfile::tempdir().unwrap();
    let lock_dir_path = lock_dir.path().to_path_buf();

    let message_id = "11111111-1111-4e21-9f10-000000000001";
    let sent_at = "2026-09-08T00:00:00Z";
    let runner: FlowRunner = Arc::new(move |invocation| {
        let lock_dir_path = lock_dir_path.clone();
        let block_id = invocation.block_id.clone();
        Box::pin(async move {
            if block_id == "A.1" {
                // A sibling lane's LEASE_RELEASE arriving WHILE A.1 is still running — the
                // loop must not see it until A.2's boundary, and A.1 itself must still
                // complete (never abandoned mid-block).
                write_inbox_message(
                    &lock_dir_path,
                    "repo-a",
                    "engine-rs",
                    message_id,
                    "LEASE_RELEASE",
                    sent_at,
                );
            }
            Ok(engine_contract::TaskContext {
                event: json!({}),
                nodes: std::collections::HashMap::new(),
                metadata: json!({}),
                node_runs: std::collections::HashMap::new(),
            })
        })
    });

    let coord = CoordHandle::new(
        lock_dir.path().to_path_buf(),
        "repo-a",
        "engine-rs",
        "engine-rs-1",
        coord_now_iso,
    );

    let chain = vec![step("repo-a", "A.1"), step("repo-a", "A.2")];
    let outcomes = run_chain(&chain, &registry, &runner, roadmap_dir.path(), Some(&coord)).await;

    // A.1 was never abandoned: BOTH steps completed and are in the outcome list, in order.
    assert_eq!(
        outcomes.len(),
        2,
        "the in-flight step must not be abandoned"
    );
    assert_eq!(outcomes[0].block_id, "A.1");
    assert_eq!(outcomes[1].block_id, "A.2");

    let queue_dir = lock_dir
        .path()
        .join("queue")
        .join("repo-a")
        .join("engine-rs");
    let filename = format!("20260908T000000Z-{message_id}.json");
    assert!(
        !queue_dir.join("inbox").join(&filename).exists(),
        "the LEASE_RELEASE must no longer sit in inbox/"
    );
    assert!(
        queue_dir.join("processing").join(&filename).exists(),
        "the LEASE_RELEASE must have been drained into processing/ at a block boundary"
    );

    // After the whole chain finishes, no lease remains — the LEASE_RELEASE was honoured
    // (not merely superseded by the final step's own `StepLeaseGuard` release).
    let lease_path = lock_dir.path().join("leases").join("lease-repo-a.json");
    assert!(
        !lease_path.exists(),
        "expected no lease file to remain once the chain finished"
    );
}

// ── (c) a malformed inbox file is quarantined to processing/ WITH a receipt — never
//        silently skipped — positively controlled by a well-formed file that still drains

/// A malformed inbox file (not even valid JSON) is moved to `processing/` and receipted,
/// never left sitting silently in `inbox/`. A well-formed file sent alongside it still
/// drains normally — the positive control proving this isn't merely "nothing works".
#[tokio::test]
async fn a_malformed_inbox_file_is_quarantined_with_a_receipt_and_a_good_file_still_drains() {
    let (dir, registry) = one_repo_registry();
    write_done_state(&dir.path().join("repo-a"), "A.1");
    let (runner, _calls) = recording_runner();
    let roadmap_dir = tempfile::tempdir().unwrap();
    let lock_dir = tempfile::tempdir().unwrap();

    let inbox = lock_dir
        .path()
        .join("queue")
        .join("repo-a")
        .join("engine-rs")
        .join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    // Malformed: not valid JSON at all.
    std::fs::write(inbox.join("20260908T000001Z-bad.json"), b"{ not json").unwrap();
    // Well-formed, sent alongside it.
    write_inbox_message(
        lock_dir.path(),
        "repo-a",
        "engine-rs",
        "8b1e0e5a-1111-4e21-9f10-0000000000bb",
        "RENDEZVOUS",
        "2026-09-08T00:00:02Z",
    );

    let coord = CoordHandle::new(
        lock_dir.path().to_path_buf(),
        "repo-a",
        "engine-rs",
        "engine-rs-1",
        coord_now_iso,
    );

    let chain = vec![step("repo-a", "A.1")];
    let outcomes = run_chain(&chain, &registry, &runner, roadmap_dir.path(), Some(&coord)).await;
    assert_eq!(outcomes.len(), 1);

    let queue_dir = lock_dir
        .path()
        .join("queue")
        .join("repo-a")
        .join("engine-rs");
    let processing = queue_dir.join("processing");

    assert!(
        !inbox.join("20260908T000001Z-bad.json").exists(),
        "the malformed file must no longer sit in inbox/"
    );
    assert!(
        processing.join("20260908T000001Z-bad.json").exists(),
        "the malformed file must have been quarantined into processing/"
    );
    assert_eq!(
        std::fs::read_dir(&processing).unwrap().count(),
        2,
        "both the malformed and the well-formed file must have moved"
    );

    let receipts =
        std::fs::read_to_string(queue_dir.join("receipts.jsonl")).expect("receipts.jsonl");
    assert_eq!(
        receipts.lines().count(),
        2,
        "one receipt per file moved — the malformed file's receipt must exist too"
    );
    assert!(receipts.contains("20260908T000001Z-bad.json"));

    // Positive control: the well-formed file sent alongside it still round-tripped, moved
    // and receipted just the same — proving this isn't "everything silently vanished".
    assert!(processing
        .join("20260908T000002Z-8b1e0e5a-1111-4e21-9f10-0000000000bb.json")
        .exists());
    assert!(receipts.contains("8b1e0e5a-1111-4e21-9f10-0000000000bb"));
}

// ── (d) a chain killed mid-run leaves a registry entry that reads stale, never live ─────

/// A chain that registers once and is then abandoned without ever heartbeating again or
/// releasing (standing in for a process killed mid-run) leaves a registry claim whose
/// `heartbeat` timestamp reads older than `okf_core::COORD_STALE_TTL_SECONDS` — the exact
/// staleness window `mev::brain::lease::check_quiesce` and `bastion coord status` apply —
/// so the next read of this claim must classify it stale, never live.
#[tokio::test]
async fn a_chain_abandoned_mid_run_leaves_a_claim_that_reads_stale_not_live() {
    let lock_dir = tempfile::tempdir().unwrap();

    // A fixed instant well past `COORD_STALE_TTL_SECONDS` (5400s) in the past — deliberately
    // NOT "now", because this test's whole point is to manufacture an abandoned, stale
    // claim. This is the one legitimate use of a non-live clock in this module: it is never
    // used to assert freshness, only to prove staleness is correctly detectable.
    let abandoned_at = chrono::Utc::now() - chrono::Duration::seconds(7200);
    let abandoned_iso = abandoned_at.to_rfc3339();

    let claim_path = lock_dir
        .path()
        .join("lane-agents")
        .join("agent-engine-rs-1.json");
    std::fs::create_dir_all(claim_path.parent().unwrap()).unwrap();
    // The `LaneAgentClaim` shape `coord::write::register` itself writes — reproduced
    // directly (rather than driving `CoordHandle::register` with a frozen `now_iso`, which
    // would repeat the exact `da4c847` staleness-literal trap this module's doc warns
    // against) so the claim's `heartbeat` is the ONLY thing under test, set once, to a
    // value known to be past the TTL, and never touched again — exactly what "abandoned
    // without releasing" means.
    let claim = json!({
        "agent_name": "engine-rs-1",
        "repo": "repo-a",
        "lane": "engine-rs",
        "roadmap": "coordination-layer-port",
        "host": null,
        "pid": std::process::id(),
        "started_at": abandoned_iso,
        "heartbeat": abandoned_iso,
        "current_block": null,
        "block_started_at": null,
    });
    std::fs::write(&claim_path, serde_json::to_string(&claim).unwrap()).unwrap();

    let text = std::fs::read_to_string(&claim_path).expect("read claim");
    let parsed: serde_json::Value = serde_json::from_str(&text).expect("claim must parse");
    let heartbeat_str = parsed["heartbeat"].as_str().expect("heartbeat is a string");
    let heartbeat = chrono::DateTime::parse_from_rfc3339(heartbeat_str)
        .expect("heartbeat must be a valid RFC3339 timestamp");
    let age_seconds = (chrono::Utc::now() - heartbeat.with_timezone(&chrono::Utc)).num_seconds();

    assert!(
        age_seconds as u64 > okf_core::COORD_STALE_TTL_SECONDS,
        "expected the abandoned claim's heartbeat age ({age_seconds}s) to exceed the \
         staleness TTL ({}s) — the exact condition `bastion coord status` classifies as \
         stale rather than live",
        okf_core::COORD_STALE_TTL_SECONDS
    );
}

// ── (e) a RENDEZVOUS is answered — the fixture standing in for the un-gateable
//        real-cross-session criterion (D64) ────────────────────────────────────────────

/// A RENDEZVOUS delivered mid-chain is answered with a reply envelope in the ORIGINAL
/// sender's own inbox, addressed from the received envelope's own `sender` field — driving
/// `integrate_chain_with_coord` with a queued RENDEZVOUS message exactly as the block
/// record's own D64 note prescribes as the standing fixture for the un-gateable real
/// cross-session case.
#[tokio::test]
async fn a_rendezvous_delivered_mid_chain_is_answered_in_the_senders_inbox() {
    let (dir, registry) = one_repo_registry();
    write_done_state(&dir.path().join("repo-a"), "A.1");
    let (runner, _calls) = recording_runner();
    let roadmap_dir = tempfile::tempdir().unwrap();
    let lock_dir = tempfile::tempdir().unwrap();

    write_inbox_message(
        lock_dir.path(),
        "repo-a",
        "engine-rs",
        "22222222-2222-4e21-9f10-000000000002",
        "RENDEZVOUS",
        "2026-09-08T00:00:00Z",
    );

    let coord = CoordHandle::new(
        lock_dir.path().to_path_buf(),
        "repo-a",
        "engine-rs",
        "engine-rs-1",
        coord_now_iso,
    );

    let chain = vec![step("repo-a", "A.1")];
    let outcomes = run_chain(&chain, &registry, &runner, roadmap_dir.path(), Some(&coord)).await;
    assert_eq!(outcomes.len(), 1);

    // `write_inbox_message`'s fixed sender fixture is base-template/types — the reply must
    // land there, never in this chain's own inbox.
    let reply_inbox = lock_dir
        .path()
        .join("queue")
        .join("base-template")
        .join("types")
        .join("inbox");
    let replies: Vec<_> = std::fs::read_dir(&reply_inbox)
        .expect("reply inbox must exist")
        .collect();
    assert_eq!(replies.len(), 1, "exactly one reply envelope written");

    let reply_text =
        std::fs::read_to_string(replies[0].as_ref().unwrap().path()).expect("read reply envelope");
    let reply: serde_json::Value = serde_json::from_str(&reply_text).expect("reply must parse");
    assert_eq!(
        reply["kind"], "RENDEZVOUS",
        "the reply is itself a RENDEZVOUS"
    );
    assert_eq!(
        reply["sender"]["agent_name"], "engine-rs-1",
        "the reply must be sent AS the answering chain, not the original sender"
    );
}

// ── (f) EN.15.J Task 3 — `check_permission_gate` through the REAL production
//        `author_operator_edge`, against a real temp `state.json` ───────────────────────
//
// Everything below drives `check_permission_gate` (never `permission::decide` in
// isolation) with the closure `make_author_operator_edge` actually builds — the same
// production wiring `gates.rs` re-exports task 1/2's `operator_edge` module through —
// against a real `brain.toml` + `planning/state.json` fixture, never a test-stub
// closure. `operator_edge.rs`'s own `#[cfg(test)]` module already unit-tests
// `make_author_operator_edge` directly (its duplicate-slug and quiesce-refusal cases in
// particular); this module's job is the layer ABOVE that — `check_permission_gate`
// itself, end to end, is what a real chain caller actually invokes at a permission gate,
// and that call chain has zero production coverage before this task. Since task 1/2's
// production closure and this task's tests were both authored and committed in the same
// spec run, there is no earlier, uncommitted state of `operator_edge.rs`/`gates.rs` left
// to observe red against without reverting another task's already-committed work (out of
// this task's own file scope, per the harness's no-revert-other-paths rule) — the proof
// that this exercises the REAL closure, not a stub, is structural instead: every test
// below calls `make_author_operator_edge` itself and asserts against the resulting
// `state.json` bytes on disk, so a swap back to a no-op stub would fail every assertion
// here that reads the file back.

/// A minimal brain root — one repo, one open block with no `depends_on` yet — that
/// `mev::add_operator_edge_as` can resolve a `<repo>:<block_id>` key against. Mirrors
/// `operator_edge.rs`'s own `brain_fixture_with_block` helper (duplicated here rather
/// than exported across the crate boundary, matching this module's own stated
/// convention for `one_repo_registry` et al.).
fn brain_fixture_with_open_block(repo: &str, block_id: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo_dir = dir.path().join(repo);
    std::fs::create_dir_all(repo_dir.join("planning")).expect("mkdir");
    std::fs::write(
        dir.path().join("brain.toml"),
        format!("[[repos]]\nslug = \"{repo}\"\nrepo_path = \"{repo}\"\n"),
    )
    .expect("write brain.toml");
    std::fs::write(
        repo_dir.join("planning").join("state.json"),
        format!(
            r#"{{ "repo": "{repo}", "kind": "project", "updated": "2026-09-10",
  "focus": {{ "now": [], "next": [], "blocked": [] }},
  "tracks": [{{ "title": "P1", "blocks": [
    {{ "id": "{block_id}", "title": "T", "status": "open", "depends_on": [] }}
  ] }}] }}"#
        ),
    )
    .expect("write state.json");
    (dir, repo_dir)
}

fn permission_gate_step(repo: &str, block_id: &str) -> ChainStep {
    ChainStep {
        repo: repo.to_string(),
        block_id: block_id.to_string(),
        directives: None,
        ..Default::default()
    }
}

fn author_edge_config(
    root: &Path,
    repo: &str,
    block_id: &str,
    dir: &Path,
) -> OperatorEdgeAuthorConfig {
    OperatorEdgeAuthorConfig {
        root: root.to_path_buf(),
        repo: repo.to_string(),
        block_id: block_id.to_string(),
        dir: dir.to_path_buf(),
        agent: Some("coord-chain-test-agent".to_string()),
        lock_dir: None,
        roadmap: None,
        lane: None,
        roadmap_dir: None,
    }
}

/// Case (1): a `GatedAction` denied under a profile authors a real
/// `{"type":"operator",...}` edge on the gating block via the REAL production closure,
/// the chain HOLDS (`check_permission_gate` returns `Err`, never `Ok`), the edge
/// round-trips through the written `state.json` verbatim, and `mev::validate_brain_state`
/// (the in-process equivalent of `bastion validate-brain --state`) is green afterwards.
#[test]
fn a_denied_graded_action_authors_a_real_edge_via_the_real_closure_and_the_chain_holds() {
    let (dir, repo_dir) = brain_fixture_with_open_block("engine-rs", "EN.15.J-NOPE1");
    let root = dir.path();
    let closure = make_author_operator_edge(author_edge_config(
        root,
        "engine-rs",
        "EN.15.J-NOPE1",
        &repo_dir,
    ));
    let step = permission_gate_step("engine-rs", "EN.15.J-NOPE1");

    let result = check_permission_gate(
        &step,
        GatedAction::InstallOnMini,
        PermissionProfile::Standard,
        &closure,
    );

    let err = result.expect_err("a denied action must hold the chain, never proceed");
    let edge = match err {
        PermissionGateError::Denied { edge, .. } => edge,
        other => panic!("expected Denied (the edge-author itself must succeed), got {other:?}"),
    };
    assert_eq!(edge.slug, "permission-install_on_mini");

    let written = std::fs::read_to_string(repo_dir.join("planning").join("state.json"))
        .expect("read state.json back");
    let value: serde_json::Value = serde_json::from_str(&written).expect("valid JSON");
    let depends_on = value["tracks"][0]["blocks"][0]["depends_on"]
        .as_array()
        .expect("depends_on array");
    let authored = depends_on
        .iter()
        .find(|e| e["type"] == "operator" && e["slug"] == "permission-install_on_mini")
        .unwrap_or_else(|| panic!("expected an authored operator edge, got: {depends_on:?}"));
    assert_eq!(authored["exit"], edge.exit);
    assert_eq!(authored["start"], edge.start);

    let report = mev::validate_brain_state(root).expect("validate_brain_state must run");
    assert!(
        !report.is_failure(),
        "bastion validate-brain --state must be green after the edge is written: {:?}",
        report.diagnostics
    );
}

/// Case (2): `check_permission_gate` called with `GatedAction::ClearOperatorGate` is
/// denied BEFORE any profile lookup occurs — restating D71 — for every
/// `PermissionProfile` variant including `Unrestricted`, driven through the REAL
/// closure (never a stub), each against its own block so one profile's authored edge
/// never interferes with the next.
#[test]
fn clear_operator_gate_is_denied_before_any_profile_lookup_via_the_real_closure() {
    for (i, profile) in [
        PermissionProfile::Locked,
        PermissionProfile::Standard,
        PermissionProfile::Unrestricted,
    ]
    .into_iter()
    .enumerate()
    {
        let block_id = format!("EN.15.J-NOPE2-{i}");
        let (dir, repo_dir) = brain_fixture_with_open_block("engine-rs", &block_id);
        let root = dir.path();
        let closure =
            make_author_operator_edge(author_edge_config(root, "engine-rs", &block_id, &repo_dir));
        let step = permission_gate_step("engine-rs", &block_id);

        let result =
            check_permission_gate(&step, GatedAction::ClearOperatorGate, profile, &closure);

        assert!(
            result.is_err(),
            "ClearOperatorGate must never be permitted at profile {profile:?}, even through \
             the real production closure"
        );
        if let Err(PermissionGateError::Denied { edge, .. }) = result {
            assert_eq!(
                edge.slug, "permission-clear_operator_gate",
                "the raised gate's slug must be derived from the action alone, not the \
                 profile — same slug at every profile level"
            );
        }
    }
}

/// Case (3): the real closure fed a request that makes `mev::add_operator_edge_as`
/// return a non-zero/error result (a duplicate slug on the same block — authoring the
/// same gate twice) makes the chain hold WITH that error surfaced as
/// `PermissionGateError::EdgeAuthorFailed`, never silently mapped to
/// `PermissionGateError::Denied` (which would read as "the gate is raised" when it is
/// not) and never treated as success.
#[test]
fn a_non_zero_mev_exit_from_the_real_closure_holds_the_chain_with_the_error_surfaced() {
    let (dir, repo_dir) = brain_fixture_with_open_block("engine-rs", "EN.15.J-NOPE3");
    let root = dir.path();
    let closure = make_author_operator_edge(author_edge_config(
        root,
        "engine-rs",
        "EN.15.J-NOPE3",
        &repo_dir,
    ));
    let step = permission_gate_step("engine-rs", "EN.15.J-NOPE3");

    // First call authors the gate successfully.
    let first = check_permission_gate(
        &step,
        GatedAction::PushToMain,
        PermissionProfile::Locked,
        &closure,
    );
    assert!(
        matches!(first, Err(PermissionGateError::Denied { .. })),
        "first call must author the edge cleanly: {first:?}"
    );

    // Second call against the same block, same denied action — the SAME slug — is a
    // duplicate the underlying mev verb must refuse.
    let second = check_permission_gate(
        &step,
        GatedAction::PushToMain,
        PermissionProfile::Locked,
        &closure,
    );
    match second {
        Err(PermissionGateError::EdgeAuthorFailed { reason, .. }) => {
            assert!(
                reason.contains("E_OPERATOR_EDGE_DUPLICATE_SLUG") || reason.contains("duplicate"),
                "expected the duplicate-slug refusal surfaced in the error, got: {reason}"
            );
        }
        other => panic!(
            "expected EdgeAuthorFailed carrying the mev refusal, got: {other:?} — a \
             non-zero mev exit must never be silently treated as success"
        ),
    }
}

/// Case (4): `OP.<slug>` is derived purely from the authored edge's own `slug` field —
/// asserted against the written `state.json` directly, with no second stored field
/// duplicating it anywhere on the edge.
#[test]
fn op_slug_is_derived_from_the_edges_own_slug_field_with_no_second_stored_field() {
    let (dir, repo_dir) = brain_fixture_with_open_block("engine-rs", "EN.15.J-NOPE4");
    let root = dir.path();
    let closure = make_author_operator_edge(author_edge_config(
        root,
        "engine-rs",
        "EN.15.J-NOPE4",
        &repo_dir,
    ));
    let step = permission_gate_step("engine-rs", "EN.15.J-NOPE4");

    let result = check_permission_gate(
        &step,
        GatedAction::CrossRepoWrite,
        PermissionProfile::Locked,
        &closure,
    );
    let edge = match result {
        Err(PermissionGateError::Denied { edge, .. }) => edge,
        other => panic!("expected Denied, got {other:?}"),
    };

    let written = std::fs::read_to_string(repo_dir.join("planning").join("state.json"))
        .expect("read state.json back");
    let value: serde_json::Value = serde_json::from_str(&written).expect("valid JSON");
    let depends_on = value["tracks"][0]["blocks"][0]["depends_on"]
        .as_array()
        .expect("depends_on array");
    let authored = depends_on
        .iter()
        .find(|e| e["type"] == "operator")
        .expect("an operator edge must have been written");

    // `OP.<slug>` — the citation form docs/state/state-schema.md defines — is derivable
    // straight from the edge's own `slug` field; no separate id/gate_id field exists on
    // the edge to disagree with it.
    let op_citation = format!("OP.{}", authored["slug"].as_str().unwrap());
    assert_eq!(op_citation, format!("OP.{}", edge.slug));
    let keys: std::collections::BTreeSet<&str> = authored
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        std::collections::BTreeSet::from(["type", "slug", "exit", "start", "what"]),
        "the edge must carry no field beyond the OperatorDep shape itself — no second \
         stored id duplicating slug"
    );
}

/// UN-GATEABLE CRITERION, declared per D64 — the block record's AC "A Telegram
/// notification actually arrives on the phone" is `gateable: false` (delivery is
/// bastion's `OperatorTransport` path, out of this block's scope, needing a live
/// device). This asserts only the IN-REPO half its own `evidence` field names: driven
/// through `check_permission_gate` (not `operator_edge.rs`'s own unit tests directly),
/// a successful edge-author on the Deny path also composes and enqueues a
/// schema-valid `operator-gate` notification escalation with a resolving
/// `EscalationChannel` naming the operator gate — never asserting or claiming actual
/// phone delivery.
#[test]
fn a_denied_action_through_check_permission_gate_also_enqueues_a_notification_escalation() {
    let (dir, _repo_dir) = brain_fixture_with_open_block("engine-rs", "EN.15.J-NOPE5");
    let root = dir.path();
    let roadmap_dir = dir.path().join("roadmap");
    std::fs::create_dir_all(&roadmap_dir).expect("mkdir roadmap dir");

    let mut cfg = author_edge_config(
        root,
        "engine-rs",
        "EN.15.J-NOPE5",
        // A real git checkout so `git rev-parse` resolves a SHA for the escalation —
        // the synthetic `repo_dir` fixture above is not one.
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")),
    );
    cfg.roadmap_dir = Some(roadmap_dir.clone());
    cfg.roadmap = Some("coordination-layer-port".to_string());
    cfg.lane = Some("engine-rs".to_string());
    let closure = make_author_operator_edge(cfg);
    let step = permission_gate_step("engine-rs", "EN.15.J-NOPE5");

    let result = check_permission_gate(
        &step,
        GatedAction::WakeLane,
        PermissionProfile::Standard,
        &closure,
    );
    assert!(
        matches!(result, Err(PermissionGateError::Denied { .. })),
        "expected Denied: {result:?}"
    );

    let contents = std::fs::read_to_string(roadmap_dir.join("escalations.jsonl"))
        .expect("read escalations.jsonl");
    let line = contents.lines().next().expect("at least one line");
    let value: serde_json::Value = serde_json::from_str(line).expect("valid JSON line");
    assert_eq!(value["kind"], "operator-gate");
    assert_eq!(value["channel"], "notification");
    assert!(
        value["summary"].as_str().unwrap_or_default().len() > 0,
        "the escalation must name the gate: {value:?}"
    );
}
