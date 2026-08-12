//! Hermetic tests for the approval ledger — `EN.8.C` task 5.
//!
//! Exercises the block's acceptance criteria end to end through the public
//! surface (`record_decision`, `ApprovalLedger`, the derived queries): the
//! five-field row, the byte-identical rendered diff, the digest-mismatch
//! re-queue, two-decisions-append-two-rows, and the derived queries — all
//! against [`InMemoryApprovalLedger`], so nothing here touches a real
//! filesystem.
//!
//! The one exception is [`file_ledger_survives_a_process_restart`], which is
//! the block's restart/cross-process proof: it writes through one
//! `FileApprovalLedger` instance, drops it, and reads the same rows back
//! through a second instance constructed on the same path — inside a
//! `tempfile` temp directory, so it never leaves anything behind and never
//! collides with a concurrent test run.
//!
//! Every timestamp here is a fixed offset from a constant epoch — no test
//! reads `Utc::now()` — so nothing in this file can flake on clock
//! granularity or wall-clock skew.

use std::io::Write;

use chrono::{DateTime, TimeZone, Utc};

use super::query::{decisions_per_day, time_to_approval, time_to_approval_stats};
use super::record::{ApprovalLedgerRow, LedgerDecision};
use super::record_decision;
use super::store::{ApprovalLedger, FileApprovalLedger, InMemoryApprovalLedger};

fn ts(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap()
}

fn row(
    item_id: &str,
    decision: LedgerDecision,
    delivered_secs: i64,
    decided_secs: i64,
) -> ApprovalLedgerRow {
    ApprovalLedgerRow {
        item_id: item_id.to_string(),
        digest: "digest-a".to_string(),
        decision,
        who: "operator-a".to_string(),
        delivered_at: ts(delivered_secs),
        decided_at: ts(decided_secs),
        rendered_diff: "rendered summary".to_string(),
    }
}

// ── one decision -> exactly one row, all five contract fields populated ───

#[test]
fn one_decision_writes_exactly_one_row_with_all_five_fields_populated() {
    let ledger = InMemoryApprovalLedger::new();

    let outcome = record_decision(
        &ledger,
        "item-a",
        "digest-a",
        "digest-a",
        "rendered summary",
        LedgerDecision::Approved,
        "operator-a",
        ts(0),
        ts(10),
    );

    let rows = ledger.read_all();
    assert_eq!(rows.len(), 1, "exactly one row must be written");

    let written = &rows[0];
    assert_eq!(written, &outcome.row);
    assert_eq!(written.digest, "digest-a");
    assert_eq!(written.decision, LedgerDecision::Approved);
    assert_eq!(written.who, "operator-a");
    assert_eq!(written.decided_at, ts(10));
    assert_eq!(written.rendered_diff, "rendered summary");
    // Not part of the five contract fields, but required by the block's
    // acceptance criteria for time-to-approval derivation.
    assert_eq!(written.delivered_at, ts(0));
    assert_eq!(written.item_id, "item-a");
}

// ── byte-identical rendered diff ───────────────────────────────────────────

#[test]
fn stored_rendered_diff_is_byte_identical_to_the_delivered_summary() {
    let ledger = InMemoryApprovalLedger::new();
    let delivered_diff = "diff: 3 files changed, +12/-4, byte-for-byte as rendered to the operator";

    let outcome = record_decision(
        &ledger,
        "item-a",
        "digest-a",
        "digest-a",
        delivered_diff,
        LedgerDecision::Approved,
        "operator-a",
        ts(0),
        ts(10),
    );

    assert_eq!(outcome.row.rendered_diff, delivered_diff);
    assert_eq!(ledger.read_all()[0].rendered_diff, delivered_diff);
}

// ── digest mismatch -> Requeued, execution blocked, never Approved ────────

#[test]
fn digest_mismatch_is_recorded_as_requeued_and_blocks_execution() {
    let ledger = InMemoryApprovalLedger::new();

    let outcome = record_decision(
        &ledger,
        "item-a",
        "digest-delivered",
        "digest-presented-different",
        "rendered summary",
        LedgerDecision::Approved,
        "operator-a",
        ts(0),
        ts(10),
    );

    assert_eq!(outcome.row.decision, LedgerDecision::Requeued);
    assert!(
        !outcome.should_execute,
        "a mismatched digest must never authorize execution"
    );

    let rows = ledger.read_all();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].decision, LedgerDecision::Requeued);
    assert!(
        rows.iter().all(|r| r.decision != LedgerDecision::Approved),
        "no row for a mismatched digest may ever be Approved"
    );
}

// ── two decisions on the same item append two rows, never coalesce ────────

#[test]
fn two_decisions_on_the_same_item_append_two_rows_not_one() {
    let ledger = InMemoryApprovalLedger::new();

    record_decision(
        &ledger,
        "item-a",
        "digest-a",
        "digest-a",
        "first summary",
        LedgerDecision::Skipped,
        "operator-a",
        ts(0),
        ts(10),
    );
    record_decision(
        &ledger,
        "item-a",
        "digest-a",
        "digest-a",
        "second summary",
        LedgerDecision::Approved,
        "operator-b",
        ts(0),
        ts(20),
    );

    let rows = ledger.rows_for("item-a");
    assert_eq!(
        rows.len(),
        2,
        "two decisions must produce two distinct rows"
    );
    assert_eq!(rows[0].decision, LedgerDecision::Skipped);
    assert_eq!(rows[1].decision, LedgerDecision::Approved);
    assert_eq!(ledger.read_all().len(), 2);
}

// ── derived queries over a fixed row set ───────────────────────────────────

#[test]
fn time_to_approval_and_stats_return_expected_values_on_a_fixed_row_set() {
    let rows = vec![
        row("item-a", LedgerDecision::Approved, 0, 10),
        row("item-b", LedgerDecision::Requeued, 0, 1_000_000),
        row("item-c", LedgerDecision::Approved, 0, 30),
    ];

    assert_eq!(time_to_approval(&rows[0]), chrono::Duration::seconds(10));

    let stats = time_to_approval_stats(&rows);
    assert_eq!(
        stats.count, 2,
        "the Requeued row must be excluded from the count"
    );
    assert_eq!(stats.median, Some(chrono::Duration::seconds(30)));
    assert_eq!(stats.max, Some(chrono::Duration::seconds(30)));
}

#[test]
fn decisions_per_day_buckets_a_fixed_row_set_by_utc_date() {
    let day_one = Utc.with_ymd_and_hms(2026, 8, 1, 9, 0, 0).unwrap();
    let day_one_later = Utc.with_ymd_and_hms(2026, 8, 1, 21, 0, 0).unwrap();
    let day_two = Utc.with_ymd_and_hms(2026, 8, 2, 9, 0, 0).unwrap();

    let rows = vec![
        ApprovalLedgerRow {
            item_id: "item-a".to_string(),
            digest: "digest-a".to_string(),
            decision: LedgerDecision::Approved,
            who: "operator-a".to_string(),
            delivered_at: day_one,
            decided_at: day_one,
            rendered_diff: "summary".to_string(),
        },
        ApprovalLedgerRow {
            item_id: "item-b".to_string(),
            digest: "digest-b".to_string(),
            decision: LedgerDecision::Skipped,
            who: "operator-a".to_string(),
            delivered_at: day_one,
            decided_at: day_one_later,
            rendered_diff: "summary".to_string(),
        },
        ApprovalLedgerRow {
            item_id: "item-c".to_string(),
            digest: "digest-c".to_string(),
            decision: LedgerDecision::Requeued,
            who: "operator-a".to_string(),
            delivered_at: day_two,
            decided_at: day_two,
            rendered_diff: "summary".to_string(),
        },
    ];

    let counts = decisions_per_day(&rows);
    assert_eq!(counts.len(), 2);
    assert_eq!(counts[&day_one.date_naive()], 2);
    assert_eq!(counts[&day_two.date_naive()], 1);
}

// ── restart survival: the only real-filesystem test in this block ─────────

#[test]
fn file_ledger_survives_a_process_restart() {
    let dir = tempfile::tempdir().expect("tempdir should succeed");
    let path = dir.path().join("approval-ledger.jsonl");

    {
        let writer = FileApprovalLedger::new(&path);
        record_decision(
            &writer,
            "item-a",
            "digest-a",
            "digest-a",
            "first summary",
            LedgerDecision::Approved,
            "operator-a",
            ts(0),
            ts(10),
        );
        record_decision(
            &writer,
            "item-b",
            "digest-b",
            "digest-b",
            "second summary",
            LedgerDecision::Skipped,
            "operator-b",
            ts(0),
            ts(20),
        );
        // `writer` (and its underlying file handles) is dropped here,
        // simulating a process exit.
    }

    // A second, independently constructed instance on the same path stands
    // in for a separate process reading the durable copy after restart.
    let reopened = FileApprovalLedger::new(&path);
    let rows = reopened.read_all();
    assert_eq!(rows.len(), 2, "both rows must survive the restart");
    assert_eq!(rows[0].item_id, "item-a");
    assert_eq!(rows[1].item_id, "item-b");
}

#[test]
fn file_ledger_malformed_line_is_skipped_and_surrounding_rows_still_parse() {
    let dir = tempfile::tempdir().expect("tempdir should succeed");
    let path = dir.path().join("approval-ledger.jsonl");

    let ledger = FileApprovalLedger::new(&path);
    ledger.append(row("item-a", LedgerDecision::Approved, 0, 10));

    {
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open for append should succeed");
        writeln!(file, "this is not valid json").expect("write malformed line should succeed");
    }

    ledger.append(row("item-b", LedgerDecision::Skipped, 0, 20));

    let rows = ledger.read_all();
    assert_eq!(
        rows.len(),
        2,
        "the malformed line must be skipped, not surfaced as an error or a panic"
    );
    assert_eq!(rows[0].item_id, "item-a");
    assert_eq!(rows[1].item_id, "item-b");
}
