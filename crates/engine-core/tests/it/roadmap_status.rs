//! `EN.15.H` task 2 — parity between `engine_core::roadmap_status`'s reader and
//! `base-template/scripts/roadmap_status_discovery.py`, the 1382-line canonical oracle
//! (`--self-test` is a gated check in base-template's own harness).
//!
//! # The false-oracle hazard this module exists to avoid
//!
//! `engine-rs` carries NO copy of `roadmap_status_discovery.py` of its own (confirmed by
//! [`no_stray_copy_of_the_oracle_lives_in_this_repo`] below), but the brain root ALSO carries an
//! **891-line, deliberately unmodified, DEPRECATED fork** at `<brain_root>/scripts/
//! roadmap_status_discovery.py` (`HQ.12.A`, 2026-09-07). A relative path or a naive walk-up from
//! this crate would land on that fork just as easily as on the real oracle — both are named
//! identically and both are plain, runnable Python. [`find_oracle_script`] therefore joins
//! `base-template/scripts/...` EXPLICITLY (never a bare `scripts/...`), and
//! [`oracle_is_the_canonical_copy_not_hqs_deprecated_fork`] asserts, before any test below trusts
//! it, that the resolved file is actually the canonical one — failing loudly and naming the path
//! if it is not.
//!
//! # Why this shells out instead of asserting against a recorded snapshot
//!
//! Same rationale as `coord_parity.rs`: `roadmap_status_discovery.py` is a plain, dependency-free
//! (standard-library-only) script living in a sibling repo under the same brain vault on a real
//! fleet checkout, so this module shells out to the REAL oracle, live, against a shared fixture
//! tree, rather than reimplementing it in Rust or pinning a recorded answer.
//!
//! # What is — and is not — compared
//!
//! The two readers' `lane_registry` / `leases` / message-queue joins read DIFFERENT physical
//! layouts by design (see `roadmap_status.rs`'s own `MessageQueueState` doc comment): the Python
//! oracle assumes a per-repo/per-lane `<lock_dir>/queue/<repo>/<lane>/inbox/` tree (which this
//! module's `discover_queue_state` mirrors field-for-field), while `engine_core::coord`'s
//! `lane_registry`/`leases` join reads the fleet's actual, verified-live `.fleet-locks` layout
//! (`EN.15.A`). This module's fixture therefore builds NO coordination artifacts at all (no
//! `.fleet-locks` directory), so both sides trivially agree that the coordination-derived
//! sections are empty — sidestepping a layout question this block does not own — while every
//! field derived from `lane-log.jsonl`, `planning/orchestration-run/`, `planning/<spec>/sdlc/`,
//! and `planning/state.json` is compared directly, field for field, against the live oracle
//! output. `age_hours` (a wall-clock delta computed independently by each process at a slightly
//! different instant) is compared as a bucketed `liveness` string, never as a raw float — an
//! exact-float comparison there would be flaky by construction, not a real divergence.
//!
//! `malformed_lines` is the ONE deliberate, asserted divergence: the Python's `read_lane_log`
//! skips a truncated line outright (it never appears anywhere in the Python's JSON), while this
//! module's `discover_at` reports it at the top level with its byte offset. Both halves of that
//! divergence are asserted explicitly below, rather than the field being excluded from
//! comparison.

use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::Utc;
use serde_json::Value;

use engine_core::roadmap_status::{self, COVERAGE_CAVEAT, NO_LANE_NAME_NOTE};

/// Walk up from `start` looking for `brain.toml` — byte-identical to `coord_parity.rs`'s and
/// `orchestration.rs`'s own copies (kept separate per-file, per those modules' precedent).
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

/// The CANONICAL oracle path — `<brain_root>/base-template/scripts/roadmap_status_discovery.py`.
/// Deliberately never a bare `scripts/roadmap_status_discovery.py` (that is HQ's deprecated
/// fork's own path, one directory up) and never derived by any walk that could resolve either
/// one interchangeably.
fn canonical_oracle_script_path(brain_root: &Path) -> PathBuf {
    brain_root
        .join("base-template")
        .join("scripts")
        .join("roadmap_status_discovery.py")
}

/// Resolve the real canonical oracle script, or `None` with a loud `eprintln!` when this
/// checkout has no sibling `base-template` (an isolated clone of just this repo, say).
fn find_oracle_script() -> Option<PathBuf> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let Some(brain_root) = find_brain_root(manifest_dir) else {
        eprintln!(
            "SKIPPING roadmap_status parity test: no brain.toml found walking up from {} \
             (this checkout has no sibling base-template to locate the oracle script in)",
            manifest_dir.display()
        );
        return None;
    };
    let script = canonical_oracle_script_path(&brain_root);
    if !script.is_file() {
        eprintln!(
            "SKIPPING roadmap_status parity test: brain root found at {} but {} does not exist",
            brain_root.display(),
            script.display()
        );
        return None;
    }
    Some(script)
}

/// `true` iff `python3` is on `PATH` and runs — spawn failure (`ErrorKind::NotFound`) is
/// distinguished from every other error, same discipline as `coord_parity.rs`'s own copy.
fn python3_available() -> bool {
    match Command::new("python3").arg("--version").output() {
        Ok(output) => output.status.success(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => panic!("python3 --version failed unexpectedly: {e}"),
    }
}

/// The single point where every parity test below decides whether it can run at all — skips
/// loudly (never silently) when either the interpreter or the canonical oracle is unavailable.
fn require_parity_environment() -> Option<PathBuf> {
    if !python3_available() {
        eprintln!("SKIPPING roadmap_status parity test: python3 is not available on PATH");
        return None;
    }
    find_oracle_script()
}

/// The literal marker string that appears ONLY in HQ's deprecated fork, never in the canonical
/// base-template copy — used to positively distinguish the two rather than relying on line count
/// alone (which the block record notes "drifted slightly" and could again).
const DEPRECATED_FORK_MARKER: &str = "DEPRECATED 2026-09-07 (HQ.12.A)";

/// Below this many lines, a file resolved as "the oracle" is almost certainly HQ's 891-line
/// fork, not base-template's 1382-line canonical copy. Comfortably between the two so a few
/// lines of drift on either side never flips this.
const CANONICAL_MIN_LINE_COUNT: usize = 1200;

/// The oracle-path assertion itself, callable from any test that needs a verified-canonical
/// script path rather than merely a resolved one.
fn assert_canonical(script: &Path) {
    let source = std::fs::read_to_string(script).unwrap_or_else(|e| {
        panic!(
            "could not read resolved oracle script {}: {e}",
            script.display()
        )
    });
    let line_count = source.lines().count();
    assert!(
        !source.contains(DEPRECATED_FORK_MARKER),
        "resolved oracle script at {} carries HQ's deprecated-fork marker '{DEPRECATED_FORK_MARKER}' \
         — this is the 891-line fork at <brain_root>/scripts/roadmap_status_discovery.py, not the \
         canonical base-template copy. Parity against it would pass while proving nothing about \
         the canonical oracle.",
        script.display()
    );
    assert!(
        line_count >= CANONICAL_MIN_LINE_COUNT,
        "resolved oracle script at {} is only {line_count} lines (expected >= {CANONICAL_MIN_LINE_COUNT}) \
         — this looks like HQ's deprecated 891-line fork, not the canonical ~1382-line base-template copy",
        script.display()
    );
}

/// Run `python3 <script> --roadmap <slug> --root <root>` and parse its JSON stdout.
fn run_python_discover(script: &Path, root: &Path, slug: &str) -> Value {
    let output = Command::new("python3")
        .arg(script)
        .arg("--roadmap")
        .arg(slug)
        .arg("--root")
        .arg(root)
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn python3 {}: {e}", script.display()));
    assert!(
        output.status.success(),
        "roadmap_status_discovery.py --roadmap {slug} --root {} failed: stdout={}\nstderr={}",
        root.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "discover output was not valid JSON: {e}\nstdout={}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn write(path: &Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).expect("create parent dir");
    std::fs::write(path, content).expect("write fixture file");
}

/// Python's several fields fall back to the literal string `"<absent>"` rather than `null` for
/// an absent frontmatter key; the Rust side reports `None`. Normalizes a `serde_json::Value` so
/// the two compare equal.
fn absent_marker_as_null(v: &Value) -> Value {
    match v {
        Value::String(s) if s == "<absent>" => Value::Null,
        other => other.clone(),
    }
}

/// The positive control proving [`find_oracle_script`] resolved the CANONICAL script, not HQ's
/// deprecated fork — the acceptance criterion this block record calls out by name.
#[test]
fn oracle_is_the_canonical_copy_not_hqs_deprecated_fork() {
    let Some(script) = require_parity_environment() else {
        return;
    };
    assert_canonical(&script);
}

/// `engine-rs` itself carries no copy of the oracle script — the false-oracle hazard this
/// module's doc comment names would otherwise let a stray local copy shadow the real one.
#[test]
fn no_stray_copy_of_the_oracle_lives_in_this_repo() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    // crates/engine-core -> repo root is two levels up.
    let repo_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("engine-core sits under crates/ inside the repo root");
    let stray = repo_root
        .join("scripts")
        .join("roadmap_status_discovery.py");
    assert!(
        !stray.exists(),
        "engine-rs now carries its own copy of roadmap_status_discovery.py at {} — the block \
         record's premise (\"engine-rs has NO copy of that script\") is stale; re-derive which \
         copy is canonical before trusting this parity test",
        stray.display()
    );
}

/// Spawn-failure handling for a genuinely absent interpreter — mirrors `coord_parity.rs`'s own
/// version of this test, kept as a separate copy per that module's established precedent.
#[test]
fn a_spawn_of_a_nonexistent_interpreter_is_treated_as_unavailable_not_a_panic() {
    let result = Command::new("python3-does-not-exist-on-this-machine-xyz")
        .arg("--version")
        .output();
    assert!(
        matches!(&result, Err(e) if e.kind() == std::io::ErrorKind::NotFound),
        "expected ErrorKind::NotFound for a nonexistent interpreter, got: {result:?}"
    );
}

/// The core parity case: a two-repo roadmap with one lane-log line resolving to a live `sdlc`
/// state, an operator gate, a carryover entry, and a second repo known only via a run record —
/// compared field-for-field against the live, canonical Python oracle. No coordination
/// artifacts are built (see this module's doc comment for why).
#[test]
fn reader_and_python_agree_field_for_field_on_a_two_repo_roadmap() {
    let Some(script) = require_parity_environment() else {
        return;
    };
    assert_canonical(&script);

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let slug = "demo";
    let roadmap_dir = root.join("planning").join("roadmaps").join(slug);

    let good_line = serde_json::json!({
        "repo": "engine-rs",
        "lane": "lane-a",
        "block": "EN.ticket.demo-thing",
        "status": "done",
        "note": "landed",
        "ts": "2026-09-08T00:00:00Z",
    })
    .to_string();
    let bad_line = "{not valid json at all";
    write(
        &roadmap_dir.join("lane-log.jsonl"),
        &format!("{good_line}\n{bad_line}\n"),
    );

    // engine-rs's own planning/state.json: one operator gate, one carryover entry.
    write(
        &root.join("engine-rs").join("planning").join("state.json"),
        &serde_json::json!({
            "repo": "engine-rs",
            "tracks": [{"blocks": [{
                "id": "EN.ticket.demo-thing",
                "depends_on": [
                    {"type": "operator", "slug": "demo-gate", "exit": "e.md", "start": "s.md", "what": "sign off"}
                ]
            }]}],
            "carryover": [{"kind": "deferred", "summary": "demo carryover"}]
        })
        .to_string(),
    );

    // engine-rs's sdlc state for the ticket the lane-log line resolves to.
    let recent_updated_at = Utc::now().to_rfc3339();
    write(
        &root
            .join("engine-rs")
            .join("planning")
            .join("ticket-demo-thing")
            .join("sdlc")
            .join("sdlc-task-state.json"),
        &serde_json::json!({
            "status": "done",
            "updated_at": recent_updated_at,
            "current_task": Value::Null,
            "tasks_run": [],
        })
        .to_string(),
    );

    // A second repo, known only via a run record — never named in lane-log.jsonl.
    write(
        &root
            .join("mev")
            .join("planning")
            .join("orchestration-run")
            .join(slug)
            .join("notes.md"),
        "---\nlifecycle: active\nrun_started: 2026-09-08\n---\n",
    );

    let python_json = run_python_discover(&script, root, slug);
    let rust_result =
        roadmap_status::discover(root, slug).expect("rust discover resolves this fixture");

    // --- top-level scalars -------------------------------------------------------------
    assert_eq!(python_json["roadmap"], slug);
    assert_eq!(rust_result.roadmap, slug);
    assert_eq!(
        python_json["repos_in_lane_log"],
        serde_json::json!(["engine-rs"])
    );
    assert_eq!(rust_result.repos_in_lane_log, vec!["engine-rs".to_string()]);
    assert_eq!(
        python_json["repos_with_run_record_only"],
        serde_json::json!(["mev"])
    );
    assert_eq!(
        rust_result.repos_with_run_record_only,
        vec!["mev".to_string()]
    );
    assert_eq!(python_json["operator_coverage_total"], 1);
    assert_eq!(rust_result.operator_coverage_total, 1);
    assert_eq!(
        python_json["coverage_caveat"].as_str().unwrap(),
        COVERAGE_CAVEAT,
        "the Rust COVERAGE_CAVEAT constant has drifted from the Python oracle's literal string"
    );
    assert_eq!(rust_result.coverage_caveat, COVERAGE_CAVEAT);

    // --- the ONE deliberate divergence: malformed_lines ---------------------------------
    assert!(
        python_json.get("malformed_lines").is_none(),
        "the Python oracle must never report a malformed_lines field at all — it drops a bad \
         line silently; got: {:?}",
        python_json.get("malformed_lines")
    );
    assert_eq!(
        rust_result.malformed_lines.len(),
        1,
        "the Rust reader must report exactly the one truncated line, never silently dropping it"
    );
    assert_eq!(rust_result.malformed_lines[0].line_number, 2);
    assert_eq!(
        rust_result.malformed_lines[0].byte_offset,
        good_line.len() + 1
    );
    // Confirm the Python really did just drop it: exactly the one good line surfaces as a block.
    let python_engine_blocks = python_json["lanes"]["engine-rs"]["blocks"]
        .as_array()
        .expect("engine-rs lane must have a blocks array");
    assert_eq!(
        python_engine_blocks.len(),
        1,
        "python silently drops the malformed line rather than reporting it, so exactly one \
         block should remain"
    );
    let rust_engine_lane = &rust_result.lanes["engine-rs"];
    assert_eq!(rust_engine_lane.blocks.len(), 1);

    // --- the one surviving block, field for field ---------------------------------------
    let py_block = &python_engine_blocks[0];
    let rust_block = &rust_engine_lane.blocks[0];
    assert_eq!(py_block["block"], "EN.ticket.demo-thing");
    assert_eq!(rust_block.block, "EN.ticket.demo-thing");
    assert_eq!(py_block["status"], "done");
    assert_eq!(rust_block.status, "done");
    assert_eq!(py_block["note"], "landed");
    assert_eq!(rust_block.note.as_deref(), Some("landed"));
    assert_eq!(py_block["ts"], "2026-09-08T00:00:00Z");
    assert_eq!(rust_block.ts.as_deref(), Some("2026-09-08T00:00:00Z"));
    assert_eq!(py_block["spec_slug"], "ticket-demo-thing");
    assert_eq!(rust_block.spec_slug.as_deref(), Some("ticket-demo-thing"));

    // sdlc_state — compare status/status_known/liveness exactly; age_hours is a wall-clock
    // delta computed independently by each process at a slightly different instant, so it is
    // compared as a bucketed liveness string, never as a raw float (see module doc comment).
    let py_sdlc = &py_block["sdlc_state"];
    assert!(
        !py_sdlc.is_null(),
        "python must resolve an sdlc_state for this block"
    );
    let rust_sdlc = rust_block
        .sdlc_state
        .as_ref()
        .expect("rust must resolve an sdlc_state for this block");
    assert_eq!(py_sdlc["status"], "done");
    assert_eq!(rust_sdlc.status, "done");
    assert_eq!(py_sdlc["status_known"], true);
    assert!(rust_sdlc.status_known);
    assert_eq!(py_sdlc["updated_at"], recent_updated_at);
    assert_eq!(
        rust_sdlc.updated_at.as_deref(),
        Some(recent_updated_at.as_str())
    );
    assert_eq!(py_sdlc["liveness"], "live");
    assert_eq!(rust_sdlc.liveness, "live");

    // --- operator_gates, order-independent (dedup order is an implementation detail) ----
    let py_gates: std::collections::BTreeSet<(String, String)> = python_json["lanes"]["engine-rs"]
        ["operator_gates"]["gates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| {
            (
                g["type"].as_str().unwrap().to_string(),
                g["slug"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let rust_gates: std::collections::BTreeSet<(String, String)> = rust_engine_lane
        .operator_gates
        .gates
        .iter()
        .map(|g| (g.kind.clone(), g.slug.clone()))
        .collect();
    assert_eq!(py_gates, rust_gates);
    assert_eq!(
        python_json["lanes"]["engine-rs"]["operator_gates"]["coverage_count"],
        1
    );
    assert_eq!(rust_engine_lane.operator_gates.coverage_count, 1);

    // --- carryover, verbatim JSON equality ----------------------------------------------
    assert_eq!(
        python_json["lanes"]["engine-rs"]["carryover"],
        serde_json::to_value(&rust_engine_lane.carryover).unwrap()
    );

    // --- the run-record-only lane (mev) ---------------------------------------------------
    let py_mev = &python_json["lanes"]["mev"];
    assert_eq!(
        absent_marker_as_null(&py_mev["run_record"]["notes"]["lifecycle"]),
        Value::String("active".to_string())
    );
    let rust_mev = &rust_result.lanes["mev"];
    assert!(rust_mev.blocks.is_empty());
    assert_eq!(
        rust_mev
            .run_record
            .as_ref()
            .and_then(|r| r.notes.as_ref())
            .and_then(|n| n.lifecycle.as_deref()),
        Some("active")
    );

    // --- coordination-derived sections: both sides empty by fixture construction --------
    assert_eq!(
        python_json["lanes"]["engine-rs"]["lane_registry"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert!(rust_engine_lane.lane_registry.is_empty());
    assert_eq!(
        python_json["lanes"]["engine-rs"]["leases"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert!(rust_engine_lane.leases.is_empty());

    // --- message_queue: engine-rs has a known lane name but no lock dir exists -----------
    let py_mq = &python_json["lanes"]["engine-rs"]["message_queue"];
    assert_eq!(py_mq["exists"], false);
    assert_eq!(rust_engine_lane.message_queue.exists, Some(false));
    assert!(py_mq["inbox_count"].is_null());
    assert!(rust_engine_lane.message_queue.inbox_count.is_none());
    let py_queue_dir = py_mq["queue_dir"].as_str().unwrap();
    assert!(py_queue_dir.ends_with("queue/engine-rs/lane-a"));
    assert!(rust_engine_lane
        .message_queue
        .queue_dir
        .as_ref()
        .unwrap()
        .ends_with("queue/engine-rs/lane-a"));

    // --- message_queue: mev has no known lane name (never appears in lane-log.jsonl) ----
    let py_mev_mq = &python_json["lanes"]["mev"]["message_queue"];
    assert_eq!(py_mev_mq["note"], NO_LANE_NAME_NOTE);
    assert_eq!(
        rust_mev.message_queue.note.as_deref(),
        Some(NO_LANE_NAME_NOTE)
    );
    assert!(py_mev_mq["queue_dir"].is_null());
    assert!(rust_mev.message_queue.queue_dir.is_none());

    // --- validate_brain: one corpus-wide invocation, same command on both sides ---------
    let py_vb = &python_json["validate_brain"];
    assert_eq!(
        py_vb["cmd"].as_str().unwrap(),
        rust_result.validate_brain.cmd,
        "the validate-brain command string must be identical on both sides"
    );
    assert_eq!(
        py_vb["exit_code"].as_i64().map(|v| v as i32),
        rust_result.validate_brain.exit_code,
        "both readers invoke the SAME `bastion validate-brain --state <root>` against the SAME \
         root, so a real bastion binary must report the same exit code either way, and an \
         absent one must report None on both sides"
    );
}

/// A roadmap with zero lane-log lines resolves cleanly on BOTH sides — an empty run is a
/// legitimate state, never an error, on both readers.
#[test]
fn zero_line_roadmap_resolves_cleanly_on_both_sides() {
    let Some(script) = require_parity_environment() else {
        return;
    };

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let slug = "empty-demo";
    std::fs::create_dir_all(root.join("planning").join("roadmaps").join(slug))
        .expect("create empty roadmap dir");

    let python_json = run_python_discover(&script, root, slug);
    let rust_result =
        roadmap_status::discover(root, slug).expect("rust discover resolves an empty roadmap");

    assert_eq!(python_json["lanes"].as_object().unwrap().len(), 0);
    assert!(rust_result.lanes.is_empty());
    assert_eq!(
        python_json["repos_in_lane_log"],
        serde_json::json!([] as [String; 0])
    );
    assert!(rust_result.repos_in_lane_log.is_empty());
    assert!(python_json.get("malformed_lines").is_none());
    assert!(rust_result.malformed_lines.is_empty());
}
