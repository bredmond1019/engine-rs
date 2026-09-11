//! `EN.15.K` Task 6 — end-to-end acceptance tests for the `CONSOLIDATE` workflow's single node,
//! [`engine_core::workflows::consolidate::graph::ConsolidateRunNode`], wiring Tasks 1-5 together:
//! discover -> select -> disposal write -> remediation promote -> watermark advance.
//!
//! (a) replay against a stamp-stripped fixture reproduces direct `select_ledger_rows` selection
//! (b) an already-`lifecycle: consolidated` record is excluded when re-fed
//! (c) the watermark advances monotonically from a Python-shaped fixture entry
//! (d) the full pipeline writes a `disposal.json` matching Task 4's field shape, with zero
//!     `state.json` content-hash drift anywhere in the fixture corpus
//! (e) remediation promotion through the full graph is idempotent on the ledger entry's `id`

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use engine_contract::TaskContext;
use serde_json::{json, Value};
use sha2::Digest as _;

use engine_core::node::Node as _;
use engine_core::workflows::consolidate::graph::{ConsolidateRunNode, NODE_NAME};
use engine_core::workflows::consolidate::watermark::read_watermark;
use engine_core::workflows::consolidate::{discover_participants, select_ledger_rows};

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn lane_log_line(repo: &str, ts: &str) -> String {
    json!({
        "repo": repo,
        "lane": format!("{repo}-lane"),
        "block": "EN.1.A",
        "status": "done",
        "ts": ts
    })
    .to_string()
}

fn ledger_row(id: &str, block: &str) -> Value {
    json!({
        "id": id,
        "block": block,
        "capability": "a thing works",
        "status": "untested",
        "env": "dev",
        "how_to_verify": "cargo test",
        "call_site": "src/lib.rs:1",
        "evidence": "ran it",
        "coverage": "covered",
        "covered_by": ["it::sample"],
    })
}

fn write_ledger(dir: &Path, roadmap: &str, entries: Vec<Value>) {
    write(
        &dir.join("verification-ledger.json"),
        &json!({
            "roadmap": roadmap,
            "repo": "irrelevant-for-this-test",
            "lane": "irrelevant-for-this-test",
            "created": "2026-09-08T00:00:00Z",
            "status_values": ["untested", "tested", "partial", "failed", "blocked", "not_applicable"],
            "entries": entries,
        })
        .to_string(),
    );
}

fn write_notes(dir: &Path, lifecycle: &str) {
    write(
        &dir.join("notes.md"),
        &format!("---\nlifecycle: {lifecycle}\n---\n"),
    );
}

fn ctx_with_event(event: Value) -> TaskContext {
    TaskContext {
        event,
        nodes: Default::default(),
        metadata: json!({}),
        node_runs: Default::default(),
    }
}

/// (a) Replay: a fixture modelled on D57 §3's own worked example (`close-the-loop` carrying two
/// `carryover-improvements` blocks, used identically by `select.rs`'s and `disposal.rs`'s own
/// unit tests) — copied here as a STAMP-STRIPPED (`lifecycle: active`, never `consolidated`)
/// fixture under a roadmap named `coordination-layer-port` (this block's own real roadmap). No
/// real ledger-based `disposal.json` exists yet anywhere in the corpus to replay against —
/// `EN.15.L`'s ledger writer landed immediately before this block, so no run has produced one —
/// so this asserts the property Task 6 actually owns: the graph's wiring reproduces EXACTLY what
/// calling `select_ledger_rows` directly over the same records would select.
#[tokio::test]
async fn replay_against_a_stamp_stripped_fixture_reproduces_direct_selection() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();

    write(
        &root.join("planning/roadmaps/coordination-layer-port/lane-log.jsonl"),
        &format!("{}\n", lane_log_line("engine-rs", "2026-09-10T00:00:00Z")),
    );
    let dir = root.join("engine-rs/planning/orchestration-run/coordination-layer-port");
    write_notes(&dir, "active");
    write_ledger(
        &dir,
        "coordination-layer-port",
        vec![ledger_row("engine-rs-en15k-1", "EN.15.K")],
    );

    let node = ConsolidateRunNode::new().with_promote(Arc::new(|_hq, _entry| {
        panic!("no failing row in this fixture; promote must never be called")
    }));
    let ctx = ctx_with_event(json!({
        "brain_root": root.to_string_lossy(),
        "roadmap_slug": "coordination-layer-port",
    }));

    let ctx = node
        .process(ctx)
        .await
        .expect("node should process cleanly");
    let result = ctx.nodes.get(NODE_NAME).expect("result stamped");
    assert_eq!(result["selected_row_count"], json!(1));

    // Cross-check against calling Task 1/2 directly over the same fixture.
    let discovery = discover_participants(root, "coordination-layer-port");
    let direct = select_ledger_rows(&discovery.records, "coordination-layer-port");
    assert_eq!(direct.len(), 1);
    assert_eq!(
        direct[0].row.get("id").unwrap().as_str().unwrap(),
        "engine-rs-en15k-1"
    );
}

/// (b) A record already `lifecycle: consolidated` is excluded when re-fed.
#[tokio::test]
async fn already_consolidated_record_is_excluded_when_re_fed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();

    write(
        &root.join("planning/roadmaps/demo/lane-log.jsonl"),
        &format!("{}\n", lane_log_line("engine-rs", "2026-09-10T00:00:00Z")),
    );
    let dir = root.join("engine-rs/planning/orchestration-run/demo");
    write_notes(&dir, "consolidated");
    write_ledger(&dir, "demo", vec![ledger_row("engine-rs-x", "EN.1.A")]);

    let node = ConsolidateRunNode::new();
    let ctx = ctx_with_event(json!({
        "brain_root": root.to_string_lossy(),
        "roadmap_slug": "demo",
    }));

    let ctx = node
        .process(ctx)
        .await
        .expect("node should process cleanly");
    let result = ctx.nodes.get(NODE_NAME).expect("result stamped");
    assert_eq!(result["selected_row_count"], json!(0));
    assert_eq!(result["disposal_row_count"], json!(0));
}

/// (c) Watermark mixed-writer monotonicity: advance with a Python-shaped fixture entry first,
/// then run the graph's own Rust advance, and assert it moves strictly forward from there.
#[tokio::test]
async fn watermark_advances_monotonically_from_a_python_written_entry() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();

    let line1 = lane_log_line("engine-rs", "2026-09-10T00:00:00Z");
    let line2 = lane_log_line("engine-rs", "2026-09-10T00:05:00Z");
    write(
        &root.join("planning/roadmaps/demo/lane-log.jsonl"),
        &format!("{line1}\n{line2}\n"),
    );

    let mut hasher = sha2::Sha256::new();
    hasher.update(line1.as_bytes());
    let line1_hash = format!("{:x}", hasher.finalize());

    write(
        &root.join("planning/open-work/orchestration-runs/consolidation-watermark.json"),
        &json!({
            "version": 1,
            "roadmaps": {
                "demo": {
                    "line": 1,
                    "last_line_sha256": line1_hash,
                    "last_ts": "2026-09-10T00:00:00Z",
                    "consolidated_at": "2026-09-10T00:01:00Z",
                    "run_id": "python-run-1",
                    "malformed_lines_at_advance": []
                }
            }
        })
        .to_string(),
    );

    let node = ConsolidateRunNode::new();
    let ctx = ctx_with_event(json!({
        "brain_root": root.to_string_lossy(),
        "roadmap_slug": "demo",
        "run_id": "rust-run-1",
    }));

    let ctx = node
        .process(ctx)
        .await
        .expect("node should process cleanly");
    let result = ctx.nodes.get(NODE_NAME).expect("result stamped");
    let watermark = result.get("watermark").expect("watermark stamped").clone();
    assert_eq!(watermark["advanced_from"], json!(1));
    assert_eq!(watermark["advanced_to"], json!(2));
    assert_eq!(watermark["consumed"], json!(1));

    let after = read_watermark(root, "demo").expect("entry present");
    assert_eq!(after.line, 2);
    assert_eq!(after.run_id.as_deref(), Some("rust-run-1"));
}

/// (d) Full pipeline: a small fixture producing a `disposal.json` matching Task 4's field set,
/// with zero `state.json` content-hash changes anywhere in the fixture corpus (this workflow
/// never writes one — see this block's own `out_of_scope`).
#[tokio::test]
async fn full_pipeline_produces_a_disposal_json_matching_task_4_shape_with_no_state_json_drift() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();

    write(
        &root.join("planning/roadmaps/demo/lane-log.jsonl"),
        &format!("{}\n", lane_log_line("engine-rs", "2026-09-10T00:00:00Z")),
    );
    let dir = root.join("engine-rs/planning/orchestration-run/demo");
    write_notes(&dir, "active");
    let row = json!({
        "finding_id": "M1",
        "mechanism": "single-invocation replay",
        "route": "carryover",
        "owner_repo": "engine-rs",
        "needs": "code",
        "severity": "P1",
        "breadth": {"repos": 1, "instances": null},
        "evidence": ["planning/notes.md:1"],
        "payload": {},
        "rationale": "observed once",
    });
    write_ledger(&dir, "demo", vec![row]);

    let state_json_path = root.join("engine-rs/planning/state.json");
    write(&state_json_path, "{\"tracks\": []}\n");
    let state_before = fs::read(&state_json_path).unwrap();

    let node = ConsolidateRunNode::new();
    let ctx = ctx_with_event(json!({
        "brain_root": root.to_string_lossy(),
        "roadmap_slug": "demo",
    }));
    let ctx = node
        .process(ctx)
        .await
        .expect("node should process cleanly");
    let result = ctx.nodes.get(NODE_NAME).expect("result stamped");

    let disposal_path = PathBuf::from(result["disposal_path"].as_str().unwrap());
    let raw = fs::read_to_string(&disposal_path).expect("disposal.json written");
    let disposal: okf_core::Disposal = serde_json::from_str(&raw).expect("parses");
    assert!(!disposal.is_legacy(), "must parse as typed, not Legacy");
    let file = disposal.typed().unwrap();
    assert_eq!(file.rows.len(), 1);
    assert_eq!(file.rows[0].finding_id, "M1");
    assert_eq!(file.rows[0].owner_repo, "engine-rs");
    assert!(file.conventions.get("ungrounded_excludes").is_some());

    let state_after = fs::read(&state_json_path).unwrap();
    assert_eq!(
        state_before, state_after,
        "CONSOLIDATE must never touch a state.json"
    );
}

fn find_brain_root(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        if d.join("brain.toml").is_file() {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

fn python3_available() -> bool {
    match Command::new("python3").arg("--version").output() {
        Ok(output) => output.status.success(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => panic!("python3 --version failed unexpectedly: {e}"),
    }
}

/// Build a hermetic HQ-shaped tempdir carrying the REAL remediation scripts, copied byte-for-byte
/// from the real HQ tree — mirrors `remediation.rs`'s own `hermetic_hq` test helper (D64: those
/// scripts resolve their root via `__file__`, so a real copy in the real sibling layout is the
/// only way to point them at a fixture).
fn hermetic_hq(brain_root: &Path) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("scripts")).unwrap();
    fs::create_dir_all(root.join("docs/sandbox/results")).unwrap();
    fs::create_dir_all(root.join("core/engine-rs/planning")).unwrap();

    for name in [
        "check_remediation.py",
        "render_findings.py",
        "render_remediation.py",
    ] {
        fs::copy(
            brain_root.join("scripts").join(name),
            root.join("scripts").join(name),
        )
        .unwrap_or_else(|e| panic!("copying {name}: {e}"));
    }

    write(
        &root.join("docs/sandbox/remediation.json"),
        &json!({
            "schema_version": 1,
            "updated": "2026-09-10",
            "status_values": {
                "fixed": "Shipped and verified.",
                "partial": "Some blocks closed, named residue still open.",
                "filed": "Block(s) exist and are open; no code has shipped yet.",
                "wontfix": "Deliberately not fixing."
            },
            "remediations": []
        })
        .to_string(),
    );
    write(
        &root.join("docs/sandbox/findings.json"),
        &json!({
            "schema_version": 1,
            "updated": "2026-09-10",
            "status_values": {
                "open": "Not yet ticketed.",
                "ticketed": "A block exists but has not shipped.",
                "closed": "Ticketed and fixed.",
                "fixed": "Fixed and verified.",
                "partial": "Some blocks closed, named residue still open."
            },
            "findings": []
        })
        .to_string(),
    );
    write(
        &root.join("docs/sandbox/test-catalogue.json"),
        &json!({"schema_version": 1, "tests": []}).to_string(),
    );
    write(
        &root.join("core/engine-rs/planning/state.json"),
        &json!({
            "tracks": [{"blocks": [{"id": "EN.99.Z", "status": "open"}]}]
        })
        .to_string(),
    );
    dir
}

/// (e) Remediation promotion + idempotency through the full graph: a failing ledger row with a
/// `remediation` object is promoted on the first run, and the second run over the SAME fixture
/// (same ledger entry `id`) promotes nothing new. Skips loudly (never silently) when this
/// checkout has no sibling HQ tree to copy the real scripts from, or no `python3` on `PATH` — the
/// same un-gateable-evidence posture `remediation.rs`'s own tests take (D64).
#[tokio::test]
async fn remediation_promotion_through_the_full_graph_is_idempotent() {
    let Some(brain_root) = find_brain_root(&std::env::current_dir().unwrap()) else {
        eprintln!("skipping: no sibling HQ tree (brain.toml) found to copy scripts from");
        return;
    };
    if !python3_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }

    let hq = hermetic_hq(&brain_root);
    let hq_root = hq.path();

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    write(
        &root.join("planning/roadmaps/demo/lane-log.jsonl"),
        &format!("{}\n", lane_log_line("engine-rs", "2026-09-10T00:00:00Z")),
    );
    let dir = root.join("engine-rs/planning/orchestration-run/demo");
    write_notes(&dir, "active");
    let mut failing_row = ledger_row("engine-rs-failing-1", "EN.99.Z");
    failing_row["status"] = json!("failed");
    // Uncovered — a `covered_by` test id must resolve in `test-catalogue.json` for
    // `check_remediation.py` to accept it, which this hermetic fixture does not carry.
    failing_row["coverage"] = json!("uncovered");
    failing_row["covered_by"] = json!([]);
    failing_row["remediation"] = json!({
        "block": "EN.99.Z",
        "opened_at": "2026-09-10T00:00:00Z",
        "note": "widget breaks under load; EN.99.Z fixes it",
    });
    write_ledger(&dir, "demo", vec![failing_row]);

    let event = json!({
        "brain_root": root.to_string_lossy(),
        "roadmap_slug": "demo",
        "hq_root": hq_root.to_string_lossy(),
    });

    let node = ConsolidateRunNode::new();
    let ctx = node
        .process(ctx_with_event(event.clone()))
        .await
        .expect("first run ok");
    let result = ctx.nodes.get(NODE_NAME).expect("result stamped");
    let promotions = result["promotions"].as_array().expect("promotions array");
    assert_eq!(promotions.len(), 1);
    assert_eq!(promotions[0]["already_promoted"], json!(false));

    let rem_doc: Value = serde_json::from_str(
        &fs::read_to_string(hq_root.join("docs/sandbox/remediation.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(rem_doc["remediations"].as_array().unwrap().len(), 1);

    // Second run, same fixture: nothing new promoted.
    let node2 = ConsolidateRunNode::new();
    let ctx2 = node2
        .process(ctx_with_event(event))
        .await
        .expect("second run ok");
    let result2 = ctx2.nodes.get(NODE_NAME).expect("result stamped");
    let promotions2 = result2["promotions"].as_array().expect("promotions array");
    assert_eq!(promotions2.len(), 1);
    assert_eq!(promotions2[0]["already_promoted"], json!(true));

    let rem_doc_after: Value = serde_json::from_str(
        &fs::read_to_string(hq_root.join("docs/sandbox/remediation.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        rem_doc_after["remediations"].as_array().unwrap().len(),
        1,
        "idempotent: no duplicate remediation entry written"
    );
}
