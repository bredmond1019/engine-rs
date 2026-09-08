//! `EN.15.G` Task 4 — cross-validate every line `escalate.rs` composes by SHELLING OUT to
//! `base-template/scripts/check_escalations.py`, so the Rust and Python halves are PROVED to
//! agree rather than assumed to. Same "skip loudly, never silently pass" pattern as
//! `coord_parity.rs` / `coord::write`'s `fleet_concurrency_check.py` parity tests: walk up for
//! `brain.toml`, and if this checkout has no sibling `base-template` (or no `python3`), skip
//! with a loud `eprintln!` rather than fail or silently no-op.
//!
//! # This checks the checker's behaviour on disk, not a Rust reimplementation of it
//!
//! Every assertion below runs the REAL `check_escalations.py` against a REAL file on disk. This
//! module never re-derives `check_escalations.py`'s rules in Rust and asserts against that
//! instead — that would prove only that this file agrees with itself.
//!
//! # `observed_red` is now impossible, by design, and this file does not attempt it
//!
//! The block record's `testing_strategy` originally said to "record observed_red for the
//! schema-validity case using one of the 24 live failing lines." Re-derived 2026-09-08 (see the
//! block record's AMENDED note): `check_escalations.py` now reports 44 records checked, 0
//! gating failures — the corpus has no live failing record left, because both blocks this
//! record once treated as pending (BT.8.C, HQ.12.A) have since closed and fixed every one.
//! Searching for a live failing record here would come back empty for the right reason and read
//! like a broken instrument. Instead this file SYNTHESISES two known-bad lines in a temp
//! roadmap directory — one missing a required field, one whose `verified_by` holds prose — and
//! asserts the checker rejects each, plus a POSITIVE CONTROL (a well-formed synthesised line,
//! same invocation) proving the checker is not simply rejecting everything.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::json;

use engine_core::workflows::orchestration::escalate::{
    EscalationChannel, EscalationKind, EscalationRecord, EscalationSeverity, NewEscalation,
};

/// Walk up from `start` looking for a `brain.toml` — same logic as every other parity test
/// module in this crate (`coord_parity.rs`, `orchestration.rs`, `roadmap_status.rs`); kept as
/// its own copy per that established precedent rather than introducing cross-module coupling.
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

/// `<brain_root>/base-template/scripts/check_escalations.py`.
fn checker_script_path(brain_root: &Path) -> PathBuf {
    brain_root
        .join("base-template")
        .join("scripts")
        .join("check_escalations.py")
}

/// Resolve the real checker script, or `None` (after a loud `eprintln!`) when this checkout has
/// no sibling `base-template` to find it in.
fn find_checker_script() -> Option<PathBuf> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let Some(brain_root) = find_brain_root(manifest_dir) else {
        eprintln!(
            "SKIPPING escalate parity test: no brain.toml found walking up from {} \
             (this checkout has no sibling base-template to locate check_escalations.py in)",
            manifest_dir.display()
        );
        return None;
    };
    let script = checker_script_path(&brain_root);
    if !script.is_file() {
        eprintln!(
            "SKIPPING escalate parity test: brain root found at {} but {} does not exist",
            brain_root.display(),
            script.display()
        );
        return None;
    }
    Some(script)
}

/// `true` iff `python3` is on `PATH` and runs. Spawn failure (interpreter genuinely absent) is
/// distinguished from any other error, same as `coord_parity.rs`'s `python3_available`.
fn python3_available() -> bool {
    match Command::new("python3").arg("--version").output() {
        Ok(output) => output.status.success(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => panic!("python3 --version failed unexpectedly: {e}"),
    }
}

/// The single point where every test in this module decides whether it can run at all.
fn require_checker_environment() -> Option<PathBuf> {
    if !python3_available() {
        eprintln!("SKIPPING escalate parity test: python3 is not available on PATH");
        return None;
    }
    find_checker_script()
}

/// Run `check_escalations.py --roadmaps-dir <roadmaps_dir> --quiet` and return `(exit_code,
/// combined stdout)`. The real oracle, invoked exactly as the block record's acceptance
/// criteria and `planning/harness.json` do — never a Rust reimplementation asserted against
/// itself.
fn run_checker(script: &Path, roadmaps_dir: &Path) -> (i32, String) {
    let output = Command::new("python3")
        .arg(script)
        .arg("--roadmaps-dir")
        .arg(roadmaps_dir)
        .arg("--quiet")
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn python3 check_escalations.py: {e}"));
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    (
        output.status.code().unwrap_or(-1),
        format!("stdout:\n{stdout}\nstderr:\n{stderr}"),
    )
}

/// Write `line` (a single already-terminated `.jsonl` line, or several) as the sole content of
/// `<roadmaps_dir>/<roadmap>/escalations.jsonl`, creating the roadmap directory first.
fn write_escalations_file(roadmaps_dir: &Path, roadmap: &str, contents: &str) -> PathBuf {
    let dir = roadmaps_dir.join(roadmap);
    fs::create_dir_all(&dir).expect("create roadmap dir");
    let path = dir.join("escalations.jsonl");
    fs::write(&path, contents).expect("write escalations.jsonl");
    path
}

fn valid_new_escalation() -> NewEscalation {
    NewEscalation {
        ts_utc: "2026-09-08T18:03:11Z".to_string(),
        repo: "engine-rs".to_string(),
        lane: "engine-rs-92".to_string(),
        kind: EscalationKind::Bail,
        severity: EscalationSeverity::Blocking,
        channel: EscalationChannel::session("dev-to-sweep-review").unwrap(),
        block: Some("EN.15.G".to_string()),
        gate_id: "coordination-layer-port/engine-rs/EN.15.G".to_string(),
        summary: "Task 4 cross-validation: escalation composed by a Rust chain.".to_string(),
        verified_by: "cargo nextest run -p engine-core escalate::tests\nok".to_string(),
        durable_home: json!({
            "channel": "run-record",
            "ref": "engine-rs/planning/orchestration-run/coordination-layer-port/notes.md#en-15-g",
        }),
        verified_at_sha: "abc1234".to_string(),
        clears_when: None,
        host: None,
    }
}

/// **Headline acceptance criterion**: an escalation written by a Rust chain (via
/// `EscalationRecord::to_jsonl_line`) passes `check_escalations.py --quiet` with exit 0,
/// SHELLED from this test.
#[test]
fn rust_composed_escalation_passes_the_real_checker() {
    let Some(script) = require_checker_environment() else {
        return;
    };
    let record = EscalationRecord::new(valid_new_escalation()).expect("valid record composes");

    let dir = tempfile::tempdir().expect("tempdir");
    write_escalations_file(
        dir.path(),
        "coordination-layer-port",
        &record.to_jsonl_line(),
    );

    let (code, output) = run_checker(&script, dir.path());
    assert_eq!(
        code, 0,
        "a Rust-composed escalation must pass check_escalations.py: {output}"
    );
}

/// A synthesised line missing a required field (`gate_id`, dropped by hand — `EscalationRecord`
/// cannot itself produce an invalid line, so this writes raw JSON directly) is REJECTED by the
/// real checker.
#[test]
fn synthesised_line_missing_a_required_field_is_rejected() {
    let Some(script) = require_checker_environment() else {
        return;
    };
    let record = EscalationRecord::new(valid_new_escalation()).expect("valid record composes");
    let mut obj = record
        .to_json()
        .as_object()
        .expect("record json is an object")
        .clone();
    obj.remove("gate_id");
    let line = format!("{}\n", serde_json::Value::Object(obj));

    let dir = tempfile::tempdir().expect("tempdir");
    write_escalations_file(dir.path(), "coordination-layer-port", &line);

    let (code, output) = run_checker(&script, dir.path());
    assert_eq!(
        code, 1,
        "a line missing a required field must be rejected: {output}"
    );
    assert!(
        output.contains("gate_id"),
        "the failure must name the missing field: {output}"
    );
}

/// A synthesised line whose `verified_by` holds prose (a bare adjective, matching neither the
/// evidence-block nor `UNVERIFIED:` shape) is REJECTED by the real checker — the record's other
/// named failure class, distinct from a missing field: "at least one live record once carried
/// all eleven fields and still failed on it."
#[test]
fn synthesised_line_with_prose_verified_by_is_rejected() {
    let Some(script) = require_checker_environment() else {
        return;
    };
    let record = EscalationRecord::new(valid_new_escalation()).expect("valid record composes");
    let mut obj = record
        .to_json()
        .as_object()
        .expect("record json is an object")
        .clone();
    obj.insert("verified_by".to_string(), json!("measured"));
    let line = format!("{}\n", serde_json::Value::Object(obj));

    let dir = tempfile::tempdir().expect("tempdir");
    write_escalations_file(dir.path(), "coordination-layer-port", &line);

    let (code, output) = run_checker(&script, dir.path());
    assert_eq!(code, 1, "a prose verified_by must be rejected: {output}");
    assert!(
        output.contains("verified_by"),
        "the failure must name verified_by: {output}"
    );
}

/// **Positive control, required**: a well-formed synthesised line — the SAME invocation the two
/// negative tests above use — is ACCEPTED. Without this, a checker that rejects everything (or
/// a `--roadmaps-dir` typo pointing nowhere real) would pass both rejections above while proving
/// nothing.
#[test]
fn well_formed_synthesised_line_is_accepted_same_invocation() {
    let Some(script) = require_checker_environment() else {
        return;
    };
    let record = EscalationRecord::new(valid_new_escalation()).expect("valid record composes");
    let line = record.to_jsonl_line();

    let dir = tempfile::tempdir().expect("tempdir");
    write_escalations_file(dir.path(), "coordination-layer-port", &line);

    let (code, output) = run_checker(&script, dir.path());
    assert_eq!(
        code, 0,
        "a well-formed synthesised line must be accepted — the positive control: {output}"
    );
}

/// A well-formed `notification`-channel escalation (2-3 options, each label <= 20 chars) also
/// round-trips through the real checker with exit 0 — the `channel`/`options` conditional
/// exercised end to end, not just the `session:<slug>` shape the other tests use.
#[test]
fn notification_channel_escalation_passes_the_real_checker() {
    use engine_core::workflows::orchestration::escalate::EscalationOption;

    let Some(script) = require_checker_environment() else {
        return;
    };
    let mut args = valid_new_escalation();
    args.channel = EscalationChannel::notification(vec![
        EscalationOption::new("resume", "Resume").unwrap(),
        EscalationOption::new("abandon", "Abandon").unwrap(),
    ])
    .unwrap();
    let record = EscalationRecord::new(args).expect("valid notification record composes");

    let dir = tempfile::tempdir().expect("tempdir");
    write_escalations_file(
        dir.path(),
        "coordination-layer-port",
        &record.to_jsonl_line(),
    );

    let (code, output) = run_checker(&script, dir.path());
    assert_eq!(
        code, 0,
        "a well-formed notification-channel escalation must pass: {output}"
    );
}
