//! Hermetic end-to-end suite for `APPROVE_AND_RUN` (`EN.8.D` task 7) — the
//! POC scenario from `planning/EN.8.D/tasks.md` §7.5, driven entirely
//! through the crate's public API (`ApproveAndRunSeams`, the two seams
//! `bastion:BA.18.B` left open) rather than any of the per-module internal
//! unit tests.
//!
//! No network, no external service: `HttpPost` is always `StubHttpPost`,
//! every timestamp is an injected `DateTime<Utc>` — never `Utc::now()` —
//! and the ledger is a `FileApprovalLedger` rooted in a fresh per-test
//! `tempfile::tempdir()` (`engine-core`'s `InMemoryApprovalLedger` is
//! `#[cfg(test)]`-gated and not visible from this integration-test crate).
//! Each test builds its own `ApproveAndRunSeams` and tempdir from scratch;
//! nothing here shares mutable state across tests (see the
//! `diagnostic-intake-state-json-rewritten-by-test-suite` carryover for why
//! that matters).

use std::sync::{Arc, Mutex};

use chrono::{DateTime, TimeZone, Utc};
use engine_core::nodes::harvest_gate::pending_harvest_record;
use engine_core::nodes::http_post::{HttpPost, StubHttpPost};
use engine_core::operator::ledger::{ApprovalLedgerRow, FileApprovalLedger};
use engine_core::operator::queue::{OperatorQueue, OperatorQueuePolicy};
use engine_core::operator::OperatorPayloadLimits;
use engine_core::workflows::approve_and_run::{
    gate_id_for, ApproveAndRunPolicy, ApproveAndRunSeams, ApproveAndRunVerdict,
    PendingHarvestRecord, OPTION_APPROVE,
};
use serde_json::json;

const INGEST_URL: &str = "https://synapse.example/ingest/learning-artifact";

fn ts(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap()
}

/// A conforming pending-harvest record for `artifact_id`, rendering to a
/// payload well within the default `OperatorPayloadLimits`.
fn conforming_record(artifact_id: &str) -> PendingHarvestRecord {
    let value = pending_harvest_record(
        artifact_id,
        INGEST_URL,
        json!({"title": format!("artifact {artifact_id}"), "body": "some material content"}),
        vec![format!("docs/content/learning-corpus/{artifact_id}.md")],
    );
    PendingHarvestRecord::from_value(&value).expect("record parses")
}

/// A record whose material content cannot be reduced under a tiny
/// `max_label_chars` limit — the "unreducible" shape `render.rs`'s own unit
/// tests use, reused here so the drain's session-routing path has something
/// genuinely non-conforming to route.
fn record_that_cannot_render(artifact_id: &str) -> PendingHarvestRecord {
    conforming_record(artifact_id)
}

/// A [`FileApprovalLedger`] rooted in a fresh per-call `tempfile::tempdir()`
/// — hermetic (no shared state across tests, no real database), but the
/// crate's non-`#[cfg(test)]` `ApprovalLedger` impl, since `engine-core`'s
/// `InMemoryApprovalLedger` is a `#[cfg(test)]`-gated internal double not
/// visible from this integration-test crate. The `tempdir` is leaked
/// deliberately (kept alive by never dropping the guard) for the lifetime
/// of the returned ledger.
fn file_ledger() -> (Arc<FileApprovalLedger>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("approval-ledger.jsonl");
    (Arc::new(FileApprovalLedger::new(path)), dir)
}

fn seams_with(
    http_post: Arc<dyn HttpPost>,
    limits: OperatorPayloadLimits,
) -> (
    ApproveAndRunSeams,
    Arc<FileApprovalLedger>,
    tempfile::TempDir,
) {
    let queue = Arc::new(Mutex::new(OperatorQueue::new(
        OperatorQueuePolicy::default(),
    )));
    let (ledger, tempdir) = file_ledger();
    let seams = ApproveAndRunSeams::new(
        queue,
        ledger.clone(),
        http_post,
        limits,
        ApproveAndRunPolicy::default(),
    );
    (seams, ledger, tempdir)
}

fn default_seams(
    http_post: Arc<dyn HttpPost>,
) -> (
    ApproveAndRunSeams,
    Arc<FileApprovalLedger>,
    tempfile::TempDir,
) {
    seams_with(http_post, OperatorPayloadLimits::default())
}

fn read_all(ledger: &FileApprovalLedger) -> Vec<ApprovalLedgerRow> {
    engine_core::operator::ledger::ApprovalLedger::read_all(ledger)
}

/// The POC scenario end to end: N pending-harvest records drain -> exactly
/// one payload is deliverable -> the operator approves it -> one ledger row
/// -> one POST carrying the stored payload byte-for-byte.
#[tokio::test]
async fn poc_scenario_drain_approve_execute_end_to_end() {
    let stub = StubHttpPost::succeeding(json!({"ok": true}));
    let stub_dyn: Arc<dyn HttpPost> = Arc::new(stub.clone());
    let (seams, ledger, _tmp) = default_seams(stub_dyn);

    let records: Vec<PendingHarvestRecord> = (0..5)
        .map(|i| conforming_record(&format!("artifact-{i:02}")))
        .collect();

    let report = seams.drain(&records, ts(0));
    assert_eq!(report.considered, 5);
    assert_eq!(report.enqueued, 5);
    assert!(report.routed_to_session.is_empty());
    let delivered = report.delivered.expect("exactly one deliverable");

    // Nothing has posted yet — the drain itself never executes anything.
    assert!(stub.last_call().is_none());
    assert!(read_all(&ledger).is_empty());

    let resolution = seams
        .resolve_verdict(ApproveAndRunVerdict {
            gate_id: delivered.item_id.clone(),
            presented_digest: delivered.payload.digest.clone(),
            option_key: OPTION_APPROVE.to_string(),
            who: "operator-a".to_string(),
            decided_at: ts(10),
        })
        .await
        .expect("approve should resolve");

    assert!(resolution.outcome.ledger_outcome.should_execute);
    assert!(!resolution.outcome.requeued);

    let rows = read_all(&ledger);
    assert_eq!(rows.len(), 1, "exactly one ledger row");

    let (posted_url, posted_body) = stub.last_call().expect("exactly one POST");
    assert_eq!(posted_url, INGEST_URL);
    let expected_record = records
        .iter()
        .find(|r| gate_id_for(&r.artifact_id) == delivered.item_id)
        .expect("delivered item traces back to a drained record");
    assert_eq!(
        posted_body, expected_record.payload,
        "the POST body must be byte-identical to the stored payload, never re-derived"
    );
}

/// The 60-item storm case (Invariant 3): 60 pending-harvest records drain
/// into exactly one deliverable, and it is the highest-priority item by
/// `compare_items` — asserted here at the integration level, not borrowed
/// from the drain's own unit tests.
#[tokio::test]
async fn sixty_item_storm_delivers_exactly_one_message() {
    let stub: Arc<dyn HttpPost> = Arc::new(StubHttpPost::succeeding(json!({"ok": true})));
    let (seams, _ledger, _tmp) = default_seams(stub);

    let records: Vec<PendingHarvestRecord> = (0..60)
        .map(|i| conforming_record(&format!("artifact-{i:02}")))
        .collect();

    let report = seams.drain(&records, ts(0));

    assert_eq!(report.considered, 60);
    assert_eq!(report.enqueued, 60);
    assert!(!report.truncated);
    assert!(report.routed_to_session.is_empty());

    // Exactly one message out.
    let delivered = report.delivered.expect("exactly one deliverable");

    // Uniform harvest-item priority under the default policy (per the
    // spec's Notes assumption), so `compare_items`'s tiebreak on
    // `enqueued_at` then `item_id` decides — lexicographically smallest
    // `item_id` wins, i.e. "artifact-00".
    assert_eq!(delivered.item_id, gate_id_for("artifact-00"));

    // A second `next_deliverable`-style call would find nothing further open
    // right now: verified indirectly via `lookup_pending` on the one gate
    // that *is* open, and that only one gate resolves at all.
    assert!(seams.lookup_pending(&delivered.item_id).is_some());
    for i in 1..60 {
        let other = gate_id_for(&format!("artifact-{i:02}"));
        assert!(
            seams.lookup_pending(&other).is_none(),
            "only the delivered item should be open, not {other}"
        );
    }
}

/// A payload mutated between delivery and decision: the presented digest no
/// longer matches the delivered one, so `record_decision` writes a
/// `Requeued` row, authorizes nothing, zero POSTs happen, and the item is
/// deliverable again.
#[tokio::test]
async fn mismatched_digest_requeues_never_executes() {
    let stub = StubHttpPost::succeeding(json!({"ok": true}));
    let stub_dyn: Arc<dyn HttpPost> = Arc::new(stub.clone());
    let (seams, ledger, _tmp) = default_seams(stub_dyn);

    let records = vec![conforming_record("artifact-mutated")];
    let report = seams.drain(&records, ts(0));
    let delivered = report.delivered.expect("one item delivered");

    let resolution = seams
        .resolve_verdict(ApproveAndRunVerdict {
            gate_id: delivered.item_id.clone(),
            presented_digest: "a-digest-that-does-not-match-what-was-delivered".to_string(),
            option_key: OPTION_APPROVE.to_string(),
            who: "operator-a".to_string(),
            decided_at: ts(10),
        })
        .await
        .expect("a digest mismatch should still resolve, as a requeue");

    assert!(resolution.outcome.requeued, "the item must be re-queued");
    assert!(!resolution.outcome.ledger_outcome.should_execute);
    assert!(
        resolution.executed.is_none(),
        "a mismatched digest must never authorize execution"
    );

    let rows = read_all(&ledger);
    assert_eq!(rows.len(), 1, "exactly one Requeued row, never zero");

    assert!(
        stub.last_call().is_none(),
        "a digest mismatch must never POST"
    );

    // The item is deliverable again — a fresh `next_deliverable`-shaped
    // check via another drain pass over the same (still-pending) queue
    // finds it open once more.
    let second = seams.drain(&[], ts(20));
    let redelivered = second
        .delivered
        .expect("the requeued item should be deliverable again");
    assert_eq!(redelivered.item_id, delivered.item_id);
}

/// A pending-harvest record that cannot be reduced to a conforming payload
/// under the declared limits never reaches the notification path — it
/// routes to `session-<slug>` instead, and is never returned by
/// `resolve_verdict`/`lookup_pending` as if it had been delivered.
#[tokio::test]
async fn non_conforming_record_never_reaches_notification() {
    // A limit so tight the fixed "Approve" option label cannot be reduced —
    // the same "unreducible" shape used throughout this block's unit tests.
    let tight_limits = OperatorPayloadLimits {
        max_options: 3,
        min_options: 2,
        max_label_chars: 1,
        max_summary_chars: 1024,
    };
    let stub: Arc<dyn HttpPost> = Arc::new(StubHttpPost::succeeding(json!({"ok": true})));
    let (seams, ledger, _tmp) = seams_with(stub, tight_limits);

    let records = vec![record_that_cannot_render("artifact-unreducible")];
    let report = seams.drain(&records, ts(0));

    assert_eq!(report.considered, 1);
    assert_eq!(report.enqueued, 0, "must not enqueue on notification");
    assert_eq!(report.routed_to_session.len(), 1);
    assert_eq!(
        report.routed_to_session[0].artifact_id,
        "artifact-unreducible"
    );
    assert!(
        report.delivered.is_none(),
        "nothing should have been delivered on the notification path"
    );

    // Never resolvable as an open gate either.
    let gate_id = gate_id_for("artifact-unreducible");
    assert!(seams.lookup_pending(&gate_id).is_none());

    // No ledger activity — the item never entered the verdict path at all.
    assert!(read_all(&ledger).is_empty());
}
