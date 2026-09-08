//! `EN.15.A` task 2 — parity between `engine_core::coord`'s reader and
//! `base-template/scripts/fleet_concurrency_check.py status`, the oracle Fork 2 keeps
//! deliberately un-retired and un-edited (see the block record's `out_of_scope`).
//!
//! # Why this shells out instead of asserting against a recorded snapshot
//!
//! Unlike `corpus_gates_parity.rs` (which pins a *recorded* answer from an installed binary
//! this workspace cannot invoke), the Python oracle here IS invokable — `fleet_concurrency_check.py`
//! is a plain script, not a compiled binary, and lives in a sibling repo
//! (`base-template/scripts/`) that sits alongside this one under the same company-brain vault
//! on a real fleet checkout. So this module shells out to the REAL script, live, against a
//! shared fixture tree — mirroring `orchestration.rs`'s
//! `engine_written_lane_log_line_is_readable_by_the_real_discovery_script_when_brain_root_present`,
//! which established this exact pattern (walk up for `brain.toml`, skip loudly if the sibling
//! repo isn't checked out, shell to `python3` for the real answer, never a Rust reimplementation
//! of the oracle).
//!
//! # The TTL trap this module exists to catch
//!
//! `okf_core::coord::COORD_STALE_TTL_SECONDS` and `fleet_concurrency_check.py`'s
//! `DEFAULT_TTL_SECONDS` are BOTH `5400` today — okf-core's own doc comment says it adopted the
//! Python's value. Asserting the reader against okf-core's constant would therefore pass while
//! proving nothing about the Python: the two agree today *by construction*, not because anything
//! here checks they still agree. So this module never reads `COORD_STALE_TTL_SECONDS` — every
//! TTL used below is parsed out of the Python's own source text at test time by
//! [`parse_default_ttl_seconds`], and [`ttl_edit_in_a_fixture_copy_is_detected_as_a_disagreement`]
//! proves that parsing is load-bearing by editing the constant in a **temporary copy** of the
//! script (the real file at `base-template/scripts/fleet_concurrency_check.py` is never touched)
//! and showing the comparison this module performs actually notices.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use okf_core::SlotRecord;

use engine_core::coord::{read_coordination_view, FLEET_LOCK_DIR_ENV};

/// Walk up from `start` looking for a `brain.toml` — the marker of the company-brain vault
/// root that houses this repo's sibling `base-template/`. Byte-identical logic to
/// `orchestration.rs`'s own `find_brain_root` helper (kept as a separate copy per-file,
/// matching that module's own precedent, rather than introducing cross-test-module coupling).
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

/// `<brain_root>/base-template/scripts/fleet_concurrency_check.py`.
fn oracle_script_path(brain_root: &Path) -> PathBuf {
    brain_root
        .join("base-template")
        .join("scripts")
        .join("fleet_concurrency_check.py")
}

/// Resolve the real oracle script, or `None` with a loud `eprintln!` when this checkout has no
/// sibling `base-template` to find it in (an isolated CI clone of just this repo, for
/// instance) — the same "skip loudly, never silently pass" contract this module's `python3`
/// guard below also honours.
fn find_oracle_script() -> Option<PathBuf> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let Some(brain_root) = find_brain_root(manifest_dir) else {
        eprintln!(
            "SKIPPING coord_parity test: no brain.toml found walking up from {} \
             (this checkout has no sibling base-template to locate the oracle script in)",
            manifest_dir.display()
        );
        return None;
    };
    let script = oracle_script_path(&brain_root);
    if !script.is_file() {
        eprintln!(
            "SKIPPING coord_parity test: brain root found at {} but {} does not exist",
            brain_root.display(),
            script.display()
        );
        return None;
    }
    Some(script)
}

/// `true` iff `python3` is on `PATH` and runs. Spawn failure (the interpreter genuinely absent)
/// is distinguished from every other error — a `python3` that exists but crashes on `--version`
/// is a different, louder problem this helper does not paper over.
fn python3_available() -> bool {
    match Command::new("python3").arg("--version").output() {
        Ok(output) => output.status.success(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => panic!("python3 --version failed unexpectedly: {e}"),
    }
}

/// The single point where every test in this module decides whether it can run at all. Returns
/// `None` (after an `eprintln!` explaining why) when either half of the parity pair — the
/// interpreter or the oracle script — is unavailable. Never returns `Some` silently swallowing
/// an absence; a caller that gets `None` back must `return` rather than proceed, which is what
/// makes "skips loudly" true of every test below rather than of this helper alone.
fn require_parity_environment() -> Option<PathBuf> {
    if !python3_available() {
        eprintln!("SKIPPING coord_parity test: python3 is not available on PATH");
        return None;
    }
    find_oracle_script()
}

/// Parse `DEFAULT_TTL_SECONDS = <int>` out of `fleet_concurrency_check.py`'s own source text.
/// The acceptance criterion this exists for: the literal `5400` must appear nowhere in this
/// test file as the TTL under test — only as a value derived by reading the Python.
fn parse_default_ttl_seconds(source: &str) -> u64 {
    for line in source.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("DEFAULT_TTL_SECONDS") else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start();
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            return digits.parse().unwrap_or_else(|e| {
                panic!("DEFAULT_TTL_SECONDS digits '{digits}' not a u64: {e}")
            });
        }
    }
    panic!("could not find a `DEFAULT_TTL_SECONDS = <int>` line in the given source text");
}

/// Read the real oracle script and parse its `DEFAULT_TTL_SECONDS`.
fn real_default_ttl_seconds(script: &Path) -> u64 {
    let source = fs::read_to_string(script)
        .unwrap_or_else(|e| panic!("could not read oracle script {}: {e}", script.display()));
    parse_default_ttl_seconds(&source)
}

/// Write one fleet-concurrency slot file at `<lock_dir>/<repo>__agent-<agent>.json` — the exact
/// flat-at-root, `pid_source: "self"` shape `fleet_concurrency_check.py::register` writes.
/// `pid_source: "self"` deliberately, throughout this module: it takes the pid-liveness branch
/// of `_sweep_stale` out of play (that branch only ever distrusts an `"explicit"` pid), so every
/// staleness question this module asks is answered by TTL age alone — the one axis under test.
fn write_slot(lock_dir: &Path, repo: &str, agent: &str, category: &str, started_at_epoch: f64) {
    fs::create_dir_all(lock_dir).expect("create lock_dir");
    let path = lock_dir.join(format!("{repo}__agent-{agent}.json"));
    let body = serde_json::json!({
        "repo": repo,
        "pid": std::process::id(),
        "pid_source": "self",
        "agent": agent,
        "category": category,
        "started_at": started_at_epoch,
    });
    fs::write(&path, body.to_string())
        .unwrap_or_else(|e| panic!("write slot file {}: {e}", path.display()));
}

/// Current wall-clock time as epoch seconds — the same clock `fleet_concurrency_check.py`'s
/// `time.time()` reads, and the same clock a slot's `started_at` is measured against.
fn now_epoch() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_secs_f64()
}

/// Run `python3 <script> status --lock-dir <lock_dir> [--ttl <ttl>]` and parse its JSON stdout.
/// `ttl` is `None` when a test wants the script's OWN default TTL (the fixture-copy tests, whose
/// whole point is that the default was edited) rather than an override.
fn run_python_status(script: &Path, lock_dir: &Path, ttl: Option<u64>) -> serde_json::Value {
    let mut cmd = Command::new("python3");
    cmd.arg(script)
        .arg("status")
        .arg("--lock-dir")
        .arg(lock_dir);
    if let Some(ttl) = ttl {
        cmd.arg("--ttl").arg(ttl.to_string());
    }
    let output = cmd
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn python3 {}: {e}", script.display()));
    assert!(
        output.status.success(),
        "fleet_concurrency_check.py status failed: stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "status output was not valid JSON: {e}\nstdout={}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

/// The `"active"` array from a `status` JSON payload, sorted for order-independent comparison.
fn active_lanes_from_python_json(payload: &serde_json::Value) -> Vec<String> {
    let mut active: Vec<String> = payload["active"]
        .as_array()
        .expect("status JSON must carry an `active` array")
        .iter()
        .map(|v| v.as_str().expect("active entries are strings").to_string())
        .collect();
    active.sort();
    active
}

/// Read `engine_core::coord`'s joined view at `brain_root` (with `FLEET_LOCK_DIR` already
/// pointed at the fixture lock dir by the caller) and compute the reader's own notion of
/// "active heavy lanes under `ttl_seconds`" in the SAME `"{repo} ({category})"` shape
/// `fleet_concurrency_check.py status` reports — the reader itself performs no TTL filtering
/// (that is the write-path sweep's job, not this read-only module's), so this helper is where
/// the comparison's TTL window is actually applied, exactly once, on both sides.
fn rust_active_heavy_lanes(brain_root: &Path, ttl_seconds: u64, now: f64) -> Vec<String> {
    let view = read_coordination_view(brain_root);
    let mut active: Vec<String> = view
        .slots
        .iter()
        .filter_map(|entry| {
            let slot: &SlotRecord = entry.slot.typed()?;
            let age = now - slot.started_at;
            if age <= ttl_seconds as f64 {
                Some(format!("{} ({})", slot.repo, slot.category))
            } else {
                None
            }
        })
        .collect();
    active.sort();
    active
}

/// RAII guard restoring `FLEET_LOCK_DIR` to its previous value on drop. `cargo nextest`
/// (CLAUDE.md standing rule 7) runs each test in its own process, so no cross-test mutex is
/// needed the way `coord/mod.rs`'s unit tests need one under plain `cargo test`.
struct LockDirGuard {
    previous: Option<String>,
}

impl LockDirGuard {
    fn set(lock_dir: &Path) -> Self {
        let previous = std::env::var(FLEET_LOCK_DIR_ENV).ok();
        std::env::set_var(FLEET_LOCK_DIR_ENV, lock_dir);
        Self { previous }
    }
}

impl Drop for LockDirGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(v) => std::env::set_var(FLEET_LOCK_DIR_ENV, v),
            None => std::env::remove_var(FLEET_LOCK_DIR_ENV),
        }
    }
}

/// The positive control: the reader and the real, unedited oracle agree on a tree with one
/// fresh slot per heavy category and one long-expired slot both sides must drop.
#[test]
fn reader_and_python_agree_on_active_heavy_lanes_for_a_healthy_tree() {
    let Some(script) = require_parity_environment() else {
        return;
    };
    let ttl = real_default_ttl_seconds(&script);

    let lock_dir = tempfile::tempdir().expect("tempdir");
    let brain_root = tempfile::tempdir().expect("tempdir");
    let now = now_epoch();

    write_slot(
        lock_dir.path(),
        "engine-rs",
        "engine-rs-a1",
        "native-build",
        now - 30.0, // 30s old — comfortably fresh under any real TTL.
    );
    write_slot(
        lock_dir.path(),
        "price-scout",
        "price-scout-b2",
        "browser-automation",
        now - 60.0,
    );
    write_slot(
        lock_dir.path(),
        "amistad",
        "amistad-old",
        "browser-automation",
        now - (ttl as f64) - 3600.0, // an hour past the TTL — must be dropped by both sides.
    );

    let python_active =
        active_lanes_from_python_json(&run_python_status(&script, lock_dir.path(), Some(ttl)));

    let _guard = LockDirGuard::set(lock_dir.path());
    let rust_active = rust_active_heavy_lanes(brain_root.path(), ttl, now);

    assert_eq!(
        rust_active, python_active,
        "reader and fleet_concurrency_check.py status disagree on the same fixture tree"
    );
    assert_eq!(
        rust_active,
        vec![
            "engine-rs (native-build)".to_string(),
            "price-scout (browser-automation)".to_string(),
        ],
        "expected exactly the two fresh slots, sorted, with the expired one dropped"
    );
}

/// The proof obligation the block record calls out by name: editing `DEFAULT_TTL_SECONDS` in a
/// TEMPORARY COPY of the oracle (the real file is never opened for writing) must change what the
/// oracle reports for a slot whose age sits between the real TTL and the edited one — and this
/// module's comparison must actually notice, not silently agree because it never looked. This is
/// the "runtime inversion" the acceptance criteria ask for: rather than asserting the whole test
/// suite goes red (which a passing suite cannot demonstrate of itself), this test manufactures
/// the exact disagreement a drifted TTL constant would cause and asserts detection of it.
#[test]
fn ttl_edit_in_a_fixture_copy_is_detected_as_a_disagreement() {
    let Some(script) = require_parity_environment() else {
        return;
    };
    let real_source = fs::read_to_string(&script)
        .unwrap_or_else(|e| panic!("could not read oracle script {}: {e}", script.display()));
    let real_ttl = parse_default_ttl_seconds(&real_source);

    // A small, obviously-different edited TTL. The slot below is aged strictly between the two,
    // so it must read as ACTIVE under the real TTL and STALE under the edited one.
    let edited_ttl: u64 = 30;
    assert!(
        edited_ttl < real_ttl,
        "fixture assumption broken: edited TTL must be smaller than the real one \
         (real={real_ttl}, edited={edited_ttl})"
    );
    let boundary_age = (real_ttl as f64 + edited_ttl as f64) / 2.0; // strictly between the two.

    // Copy the oracle to a scratch file and rewrite ONLY the constant line — never the real
    // script on disk, per the block record's Fork 2 (`fleet_concurrency_check.py` is never
    // retired or edited by this block).
    let scratch_dir = tempfile::tempdir().expect("tempdir");
    let edited_script = scratch_dir.path().join("fleet_concurrency_check_edited.py");
    let edited_source = real_source.replace(
        &format!("DEFAULT_TTL_SECONDS = {real_ttl}"),
        &format!("DEFAULT_TTL_SECONDS = {edited_ttl}"),
    );
    assert_ne!(
        real_source, edited_source,
        "the DEFAULT_TTL_SECONDS = {real_ttl} literal was not found verbatim in the real script \
         — this test's string replacement is stale against the source"
    );
    fs::write(&edited_script, &edited_source).expect("write edited fixture copy");
    // `fleet_concurrency_check.py` late-imports its sibling `check_lane_agents.py` (for lease
    // discovery) from its OWN directory (`sys.path[0]`) — copy that sibling alongside the
    // edited copy too, unmodified, so the edited script can actually run standalone from a
    // scratch directory instead of failing on `ModuleNotFoundError` before it reaches `status`.
    let sibling_module = script
        .parent()
        .expect("oracle script has a parent directory")
        .join("check_lane_agents.py");
    fs::copy(
        &sibling_module,
        scratch_dir.path().join("check_lane_agents.py"),
    )
    .unwrap_or_else(|e| {
        panic!(
            "could not copy sibling module {} into the scratch dir: {e}",
            sibling_module.display()
        )
    });
    // The real file must be provably untouched by this test.
    let real_source_after = fs::read_to_string(&script).expect("re-read real oracle script");
    assert_eq!(
        real_source, real_source_after,
        "the REAL oracle script was modified by this test — it must never be touched"
    );

    let lock_dir = tempfile::tempdir().expect("tempdir");
    let brain_root = tempfile::tempdir().expect("tempdir");
    let now = now_epoch();
    write_slot(
        lock_dir.path(),
        "engine-rs",
        "engine-rs-boundary",
        "native-build",
        now - boundary_age,
    );

    // Reader side: parsed from the REAL script's TTL (as production code should always do).
    let _guard = LockDirGuard::set(lock_dir.path());
    let rust_active_under_real_ttl = rust_active_heavy_lanes(brain_root.path(), real_ttl, now);
    drop(_guard);

    // Oracle side: the EDITED copy, invoked with no `--ttl` override so its own (edited)
    // default applies — this is what proves the constant itself, not merely a CLI flag, moved.
    let python_active_under_edited_default =
        active_lanes_from_python_json(&run_python_status(&edited_script, lock_dir.path(), None));

    assert_eq!(
        rust_active_under_real_ttl,
        vec!["engine-rs (native-build)".to_string()],
        "the boundary slot must read as active under the REAL TTL"
    );
    assert!(
        python_active_under_edited_default.is_empty(),
        "the boundary slot must read as stale under the EDITED (smaller) TTL default, but the \
         edited oracle reported: {python_active_under_edited_default:?}"
    );
    assert_ne!(
        rust_active_under_real_ttl, python_active_under_edited_default,
        "a hand-edited TTL constant must produce a detectable disagreement — it did not"
    );
}

/// A second, independent disagreement case not mediated by editing the script at all: the same
/// boundary-aged slot, read once through the reader (parsed real TTL) and once through the REAL,
/// unedited oracle but invoked with an explicitly different `--ttl` (standing in for "the two
/// readers were, for whatever reason, given different windows") — proving this module's
/// comparison is sensitive to a genuine substantive split, not merely to the specific
/// edit-the-source-file mechanism the test above exercises.
#[test]
fn a_tree_with_a_boundary_aged_slot_turns_the_parity_check_red_under_differing_ttls() {
    let Some(script) = require_parity_environment() else {
        return;
    };
    let real_ttl = real_default_ttl_seconds(&script);
    let shorter_ttl: u64 = real_ttl / 2;
    assert!(
        shorter_ttl < real_ttl,
        "fixture assumption broken: shorter TTL must be strictly less than the real one"
    );

    let boundary_age = (real_ttl as f64 + shorter_ttl as f64) / 2.0;

    let lock_dir = tempfile::tempdir().expect("tempdir");
    let brain_root = tempfile::tempdir().expect("tempdir");
    let now = now_epoch();
    write_slot(
        lock_dir.path(),
        "mev",
        "mev-boundary",
        "native-build",
        now - boundary_age,
    );

    let _guard = LockDirGuard::set(lock_dir.path());
    let rust_active = rust_active_heavy_lanes(brain_root.path(), real_ttl, now);
    drop(_guard);

    let python_active_under_shorter_ttl = active_lanes_from_python_json(&run_python_status(
        &script,
        lock_dir.path(),
        Some(shorter_ttl),
    ));

    assert_eq!(rust_active, vec!["mev (native-build)".to_string()]);
    assert!(python_active_under_shorter_ttl.is_empty());
    assert_ne!(
        rust_active, python_active_under_shorter_ttl,
        "a boundary-aged slot compared under two different TTL windows must disagree"
    );
}

#[test]
fn parse_default_ttl_seconds_reads_the_constant_from_source_text() {
    let source = "\
# comment\n\
DEFAULT_TTL_SECONDS = 5400  # 90 minutes\n\
OTHER = 1\n";
    assert_eq!(parse_default_ttl_seconds(source), 5400);
}

#[test]
#[should_panic(expected = "could not find a `DEFAULT_TTL_SECONDS")]
fn parse_default_ttl_seconds_panics_loudly_when_the_constant_is_absent() {
    parse_default_ttl_seconds("# no such constant here\nFOO = 1\n");
}

/// Exercises the "skip loudly, never silently pass" contract of [`python3_available`] against a
/// deliberately nonexistent interpreter path, since this development machine always has a real
/// `python3` on `PATH` and so cannot exercise a genuine absence end-to-end. `Command::new` with
/// a bogus program name fails to spawn with `ErrorKind::NotFound`, which is exactly the branch
/// [`python3_available`] treats as "unavailable" rather than panicking on.
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
