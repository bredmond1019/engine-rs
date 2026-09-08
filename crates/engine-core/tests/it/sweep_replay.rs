//! Golden replay of every checked-in `SWEEP` snapshot — `EN.15.E` task 6.
//!
//! Fixtures live under `tests/fixtures/sweep_snapshots/` — a verbatim copy of
//! `planning/roadmaps/autonomous-foundation/sweeps/*.json` taken 2026-09-08. **Never reference
//! the `planning/` path from this file**: `planning/` is a symlink into the private HQ vault and
//! is gitignored, so a test that reads from it would pass on this machine and fail on every CI
//! runner (the block record's own task-6 description names this trap explicitly).
//!
//! Each fixture file already carries the `diff`/`routed` sections `roadmap_sweep.py` itself
//! computed and wrote at the time — that recorded pair is the oracle this test replays against,
//! never a hand-written expectation. [`diff::diff_snapshots`] is asserted for full structural
//! equality against the recorded `diff` object. The recorded `routed` list's shape differs
//! field-for-field from [`route::RouteOutcome`] in ways this crate's own modules already
//! document (e.g. `route_non_escalation_diff`'s Python sibling carries a `summary` field
//! `RouteOutcome` has no slot for, and the Python's dry-run pass never stamps a `reason`) — so
//! this test compares the DECISION fields that matter (`action`, `channel`, `severity`,
//! `stale`, and `gate_id` where the Python's own dict carries one), not a byte-for-byte struct
//! equality that would fail on divergences already named and accepted elsewhere in this crate.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde_json::Value;

use engine_core::policy::permission::PermissionProfile;
use engine_core::workflows::sweep::{
    build_dedup_history, diff_snapshots, list_snapshot_files, load_snapshot,
    route_non_escalation_diff, DiscoveryDiff, NoopLaneWake, NoopOperatorTransport, RouteInputs,
};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sweep_snapshots")
}

/// Parse a snapshot's own `ts_utc` back into the instant [`route_escalation`]/
/// [`route_non_escalation_diff`] must run against for that pass — the same rule `diff.rs`'s own
/// `parse_snapshot_now` documents.
fn parse_ts(ts_utc: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(ts_utc)
        .expect("every fixture ts_utc is a well-formed RFC3339 timestamp")
        .with_timezone(&Utc)
}

/// The decision-relevant slice of one recorded `routed[]` entry — the fields this test actually
/// asserts, deliberately excluding `reason`/`ts_routed`/`routed`(bool)/`dry_run`/`summary`: see
/// this file's module doc for why a byte-for-byte struct comparison is the wrong bar here.
#[derive(Debug, PartialEq)]
struct Decision {
    action: Option<String>,
    channel: Option<String>,
    severity: Option<String>,
    stale: Option<bool>,
    gate_id: Option<String>,
}

fn decision_from_value(entry: &Value) -> Decision {
    Decision {
        action: entry
            .get("action")
            .and_then(Value::as_str)
            .map(String::from),
        channel: entry
            .get("channel")
            .and_then(Value::as_str)
            .map(String::from),
        severity: entry
            .get("severity")
            .and_then(Value::as_str)
            .map(String::from),
        stale: entry.get("stale").and_then(Value::as_bool),
        gate_id: entry
            .get("gate_id")
            .and_then(Value::as_str)
            .map(String::from),
    }
}

fn decision_from_outcome(outcome: &engine_core::workflows::sweep::RouteOutcome) -> Decision {
    Decision {
        action: Some(outcome.action.clone()),
        channel: outcome.channel.clone(),
        severity: outcome.severity.clone(),
        // `route_non_escalation_diff`'s Rust sentinel (`"<non-escalation-diff>"`) has no
        // counterpart in the Python's own dict for that case (no `gate_id`/`stale` key at all
        // — that route is about the diff as a whole, not one escalation) — treat both as
        // "absent" for that action so the two compare equal.
        stale: (outcome.gate_id != "<non-escalation-diff>").then_some(outcome.stale),
        gate_id: (outcome.gate_id != "<non-escalation-diff>").then(|| outcome.gate_id.clone()),
    }
}

/// Replay one sweep pass over `curr` given the immediately preceding fixture `prev` (or `None`
/// on the very first snapshot) and the dedup history built from every fixture strictly before
/// `curr` in the chronological sequence — `engine_core::workflows::sweep::run_sweep_pass`'s own
/// three-phase routing loop (mod.rs), minus the raw-snapshot measurement and the final
/// `sweeps/<ts>.json` write, neither of which a replay over already-measured fixtures needs.
async fn replay_pass(
    prev: Option<&engine_core::workflows::sweep::RawSnapshot>,
    curr: &engine_core::workflows::sweep::RawSnapshot,
    dedup_history: &std::collections::BTreeMap<String, engine_core::workflows::sweep::DedupEntry>,
) -> (
    DiscoveryDiff,
    Vec<engine_core::workflows::sweep::RouteOutcome>,
) {
    use engine_core::workflows::sweep::{escalation_key, route_escalation, EscalationKey};
    use std::collections::HashSet;

    let diff = diff_snapshots(prev, curr);
    let mut dedup_history = dedup_history.clone();

    let now = parse_ts(&curr.ts_utc);
    let inputs = RouteInputs {
        now,
        current_sha: curr.git_sha.as_deref(),
        refire_hours: engine_core::workflows::sweep::DEFAULT_REFIRE_HOURS,
        // Unrestricted: the Python oracle has no profile layer at all, so nothing it ever
        // computed was suppressed by one — replaying under the most permissive profile is what
        // makes `action` comparable to the Python's own unconditional decision tree.
        profile: PermissionProfile::Unrestricted,
    };
    let waker = NoopLaneWake;
    let transport = NoopOperatorTransport;
    let mut budget = engine_core::workflows::sweep::Budget::default();
    let mut routed = Vec::new();

    for escalation in &diff.new_escalations {
        let result = route_escalation(
            escalation,
            &dedup_history,
            inputs,
            &mut budget,
            &transport,
            &waker,
        )
        .await;
        if result.routed {
            if let Some(gate_id) = escalation.get("gate_id").and_then(Value::as_str) {
                dedup_history.insert(
                    gate_id.to_string(),
                    engine_core::workflows::sweep::DedupEntry {
                        ts: result.ts_routed.clone(),
                        severity: result.severity.clone(),
                    },
                );
            }
        }
        routed.push(result);
    }

    let new_keys: HashSet<EscalationKey> =
        diff.new_escalations.iter().map(escalation_key).collect();
    for escalation in &curr.escalations {
        if new_keys.contains(&escalation_key(escalation)) {
            continue;
        }
        let Some(gate_id) = escalation.get("gate_id").and_then(Value::as_str) else {
            continue;
        };
        if !dedup_history.contains_key(gate_id) {
            continue;
        }
        let result = route_escalation(
            escalation,
            &dedup_history,
            inputs,
            &mut budget,
            &transport,
            &waker,
        )
        .await;
        if result.action == "skip-dedup" {
            continue;
        }
        if result.routed {
            dedup_history.insert(
                gate_id.to_string(),
                engine_core::workflows::sweep::DedupEntry {
                    ts: result.ts_routed.clone(),
                    severity: result.severity.clone(),
                },
            );
        }
        routed.push(result);
    }

    if routed.is_empty() && diff.non_escalation_diff {
        routed.push(route_non_escalation_diff(
            &diff,
            &curr.roadmap,
            now,
            inputs.profile,
            &waker,
        ));
    }

    (diff, routed)
}

/// Build the dedup history exactly as [`build_dedup_history`] would see it mid-fleet: only the
/// fixtures strictly BEFORE `upto` (exclusive) have been "written" yet. Copies that prefix into
/// a fresh temp directory rather than pointing at the full fixture tree, so a later snapshot's
/// routed entries can never leak backwards into an earlier pass's dedup state.
fn dedup_history_before(
    all_files: &[PathBuf],
    upto: usize,
) -> std::collections::BTreeMap<String, engine_core::workflows::sweep::DedupEntry> {
    let tmp = tempfile::tempdir().expect("tempdir");
    for path in &all_files[..upto] {
        let bytes = std::fs::read(path).expect("read fixture");
        let filename = path.file_name().expect("fixture has a filename");
        std::fs::write(tmp.path().join(filename), bytes).expect("write into temp dedup dir");
    }
    build_dedup_history(tmp.path(), None)
}

#[tokio::test]
async fn replays_every_fixture_and_the_count_is_derived_not_hardcoded() {
    let dir = fixtures_dir();
    let files = list_snapshot_files(&dir);

    // The replayed count is asserted against the fixture directory's OWN file count, never a
    // literal — the block record's own amendment (a 14th sweep ever written must not falsify
    // this test).
    let on_disk_count = std::fs::read_dir(&dir)
        .expect("fixtures/sweep_snapshots must exist")
        .filter(|entry| {
            entry
                .as_ref()
                .ok()
                .map(|e| e.path().extension().and_then(|ext| ext.to_str()) == Some("json"))
                .unwrap_or(false)
        })
        .count();
    assert_eq!(
        files.len(),
        on_disk_count,
        "list_snapshot_files must see every *.json fixture on disk"
    );
    assert!(on_disk_count > 0, "fixture directory must not be empty");

    let mut prev: Option<engine_core::workflows::sweep::RawSnapshot> = None;
    let mut replayed = 0usize;

    for (idx, path) in files.iter().enumerate() {
        let curr =
            load_snapshot(path).unwrap_or_else(|| panic!("fixture must parse: {}", path.display()));
        let oracle: Value = serde_json::from_str(
            &std::fs::read_to_string(path).unwrap_or_else(|_| panic!("read {}", path.display())),
        )
        .unwrap_or_else(|_| panic!("parse {}", path.display()));

        let dedup_history = dedup_history_before(&files, idx);
        let (diff, routed) = replay_pass(prev.as_ref(), &curr, &dedup_history).await;

        // --- the diff must reproduce the Python's own recorded diff, structurally, in full ---
        let expected_diff: DiscoveryDiff =
            serde_json::from_value(oracle.get("diff").cloned().unwrap_or_else(|| {
                serde_json::json!({
                    "changed": false, "new_escalations": [], "escalation_count_prev": 0,
                    "escalation_count_curr": 0, "non_escalation_diff": false,
                    "non_escalation_summary": null, "first_sweep": true
                })
            }))
            .unwrap_or_else(|err| {
                panic!("{}: recorded diff must deserialize: {err}", path.display())
            });
        assert_eq!(
            diff,
            expected_diff,
            "{}: replayed diff must match the Python's own recorded diff",
            path.display()
        );

        // --- the route decisions must reproduce the Python's own recorded route ---
        let expected_routed: Vec<Value> = oracle
            .get("routed")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let expected_decisions: Vec<Decision> =
            expected_routed.iter().map(decision_from_value).collect();
        let actual_decisions: Vec<Decision> = routed.iter().map(decision_from_outcome).collect();
        assert_eq!(
            actual_decisions,
            expected_decisions,
            "{}: replayed route decisions must match the Python's own recorded route",
            path.display()
        );

        prev = Some(curr);
        replayed += 1;
    }

    assert_eq!(
        replayed, on_disk_count,
        "every fixture on disk must have been replayed exactly once"
    );
}

#[tokio::test]
async fn named_snapshot_routes_bt_2_a_as_session_channel_and_stale() {
    let dir = fixtures_dir();
    let files = list_snapshot_files(&dir);
    let target = files
        .iter()
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n == "2026-08-28T01-30-33Z.json")
                .unwrap_or(false)
        })
        .expect("the named snapshot must be present in the fixture directory");

    let idx = files.iter().position(|p| p == target).unwrap();
    let prev = if idx == 0 {
        None
    } else {
        load_snapshot(&files[idx - 1])
    };
    let curr = load_snapshot(target).expect("named snapshot must parse");

    let dedup_history = dedup_history_before(&files, idx);
    let (_diff, routed) = replay_pass(prev.as_ref(), &curr, &dedup_history).await;

    let bt_route = routed
        .iter()
        .find(|r| r.gate_id == "autonomous-foundation/base-template/BT.2.A")
        .expect("BT.2.A escalation must have been routed this pass");

    assert_eq!(bt_route.action, "route-to-owning-lane");
    assert_eq!(bt_route.channel.as_deref(), Some("session:BT.2.A"));
    assert!(
        bt_route.stale,
        "BT.2.A's verified_at_sha must read as stale"
    );
}
