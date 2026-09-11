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

// ---------------------------------------------------------------------------------------------
// `EN.17.A` task 7 — lease-threshold source-text parity, the one-definition scanner, and the
// holder-conflict parity case against `fleet_concurrency_check.py register`.
//
// Same discipline as the TTL section above: every threshold compared below is parsed out of its
// AUTHORITY's own source text at test time, never read from this crate's own constant on the
// "trust me, it matches" side of the comparison, and never hand-copied as a literal into this
// file. [`threshold_parser_detects_an_altered_product`] proves the parser is load-bearing the
// same way [`ttl_edit_in_a_fixture_copy_is_detected_as_a_disagreement`] does above.
// ---------------------------------------------------------------------------------------------

/// `<brain_root>/core/mev/src/brain/lease.rs` — the authority for
/// `engine_core::coord::LEASE_STALE_THRESHOLD_SECONDS`.
fn mev_lease_source_path(brain_root: &Path) -> PathBuf {
    brain_root
        .join("core")
        .join("mev")
        .join("src")
        .join("brain")
        .join("lease.rs")
}

/// `<brain_root>/base-template/scripts/check_lane_agents.py` — the authority for
/// `crate::workflows::sweep::snapshot::REGISTRY_STALE_THRESHOLD_SECONDS`.
fn check_lane_agents_source_path(brain_root: &Path) -> PathBuf {
    brain_root
        .join("base-template")
        .join("scripts")
        .join("check_lane_agents.py")
}

/// Resolve `<brain_root>`, or `None` with a loud `eprintln!` naming `test_name` when this
/// checkout has no sibling vault to find one in — the brain-root half of
/// [`require_parity_environment`]'s skip contract, reused by the threshold/scanner tests below
/// that need a sibling repo but not `python3`.
fn find_brain_root_or_skip(test_name: &str) -> Option<PathBuf> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let Some(brain_root) = find_brain_root(manifest_dir) else {
        eprintln!(
            "SKIPPING {test_name}: no brain.toml found walking up from {} \
             (this checkout has no sibling vault to locate the authority source in)",
            manifest_dir.display()
        );
        return None;
    };
    Some(brain_root)
}

/// Resolve `path` as an authority source file for `test_name`, or `None` with a loud `eprintln!`
/// when it is absent (a brain root exists but the specific sibling repo/file was never checked
/// out here).
fn require_source_file(path: &Path, test_name: &str) -> Option<()> {
    if !path.is_file() {
        eprintln!(
            "SKIPPING {test_name}: authority source not found at {}",
            path.display()
        );
        return None;
    }
    Some(())
}

/// Parse a `<const_name> = <number> * <number>` (Rust: `pub const NAME: f64 = 180.0 * 60.0;`;
/// Python: `NAME = 180 * 60`) product out of `source`'s own text. Deliberately line-oriented and
/// requiring an `=` on the SAME line as `const_name`, so a doc comment that merely mentions the
/// const's name (every threshold const in this crate is referenced that way in multiple doc
/// comments — see `coord/mod.rs`, `coord/write.rs`, `coord_lane.rs`, `sweep/snapshot.rs`) is
/// never mistaken for its declaration.
fn parse_threshold_product(source: &str, const_name: &str) -> f64 {
    for line in source.lines() {
        let Some(name_idx) = line.find(const_name) else {
            continue;
        };
        let after_name = &line[name_idx + const_name.len()..];
        let Some(eq_idx) = after_name.find('=') else {
            continue;
        };
        let expr = &after_name[eq_idx + 1..];
        let expr = expr.split(';').next().unwrap_or(expr);
        let expr = expr.split("//").next().unwrap_or(expr);
        let expr = expr.split('#').next().unwrap_or(expr);
        let parts: Vec<&str> = expr.split('*').map(str::trim).collect();
        if parts.len() == 2 && !parts[0].is_empty() && !parts[1].is_empty() {
            let a: f64 = parts[0].parse().unwrap_or_else(|e| {
                panic!(
                    "could not parse '{}' as f64 in `{const_name}` product: {e}",
                    parts[0]
                )
            });
            let b: f64 = parts[1].parse().unwrap_or_else(|e| {
                panic!(
                    "could not parse '{}' as f64 in `{const_name}` product: {e}",
                    parts[1]
                )
            });
            return a * b;
        }
    }
    panic!("could not find a `{const_name} = <number> * <number>` line in the given source text");
}

#[test]
fn parse_threshold_product_reads_a_rust_style_product() {
    let source = "pub const LEASE_STALE_THRESHOLD_SECONDS: f64 = 180.0 * 60.0;\n";
    assert_eq!(
        parse_threshold_product(source, "LEASE_STALE_THRESHOLD_SECONDS"),
        10_800.0
    );
}

#[test]
fn parse_threshold_product_reads_a_python_style_product() {
    let source = "STALE_THRESHOLD_SECONDS = 180 * 60\n";
    assert_eq!(
        parse_threshold_product(source, "STALE_THRESHOLD_SECONDS"),
        10_800.0
    );
}

#[test]
fn parse_threshold_product_ignores_a_doc_comment_mentioning_the_name_without_an_assignment() {
    let source = "\
/// See `LEASE_STALE_THRESHOLD_SECONDS` below.\n\
pub const LEASE_STALE_THRESHOLD_SECONDS: f64 = 180.0 * 60.0;\n";
    assert_eq!(
        parse_threshold_product(source, "LEASE_STALE_THRESHOLD_SECONDS"),
        10_800.0
    );
}

/// The proof obligation for this section, mirroring
/// [`ttl_edit_in_a_fixture_copy_is_detected_as_a_disagreement`] above: the SAME parser, fed a
/// source string with a different product than the real constant, must report a mismatch rather
/// than silently agreeing because nothing actually looked.
#[test]
fn threshold_parser_detects_an_altered_product() {
    let real = parse_threshold_product(
        "pub const LEASE_STALE_THRESHOLD_SECONDS: f64 = 180.0 * 60.0;\n",
        "LEASE_STALE_THRESHOLD_SECONDS",
    );
    let altered = parse_threshold_product(
        "pub const LEASE_STALE_THRESHOLD_SECONDS: f64 = 90.0 * 60.0;\n",
        "LEASE_STALE_THRESHOLD_SECONDS",
    );
    assert_ne!(
        real, altered,
        "a hand-edited product must produce a detectable disagreement — it did not"
    );
}

/// `engine_core::coord::LEASE_STALE_THRESHOLD_SECONDS` must equal mev's own
/// `LEASE_STALE_THRESHOLD_SECONDS` (`core/mev/src/brain/lease.rs`), parsed from that file's
/// current source text at test time — never a literal copied into this test.
#[test]
fn lease_threshold_matches_mev_source() {
    let Some(brain_root) = find_brain_root_or_skip("lease_threshold_matches_mev_source") else {
        return;
    };
    let path = mev_lease_source_path(&brain_root);
    if require_source_file(&path, "lease_threshold_matches_mev_source").is_none() {
        return;
    }
    let source = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));
    let mev_value = parse_threshold_product(&source, "LEASE_STALE_THRESHOLD_SECONDS");
    assert_eq!(
        mev_value,
        engine_core::coord::LEASE_STALE_THRESHOLD_SECONDS,
        "engine-rs's LEASE_STALE_THRESHOLD_SECONDS has drifted from its authority at {}",
        path.display()
    );
}

/// `crate::workflows::sweep::snapshot::REGISTRY_STALE_THRESHOLD_SECONDS` must equal
/// `base-template/scripts/check_lane_agents.py`'s `STALE_THRESHOLD_SECONDS`, parsed from that
/// file's current source text at test time.
#[test]
fn registry_threshold_matches_check_lane_agents_source() {
    let Some(brain_root) =
        find_brain_root_or_skip("registry_threshold_matches_check_lane_agents_source")
    else {
        return;
    };
    let path = check_lane_agents_source_path(&brain_root);
    if require_source_file(&path, "registry_threshold_matches_check_lane_agents_source").is_none() {
        return;
    }
    let source = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));
    let python_value = parse_threshold_product(&source, "STALE_THRESHOLD_SECONDS");
    assert_eq!(
        python_value,
        engine_core::workflows::sweep::snapshot::REGISTRY_STALE_THRESHOLD_SECONDS,
        "engine-rs's REGISTRY_STALE_THRESHOLD_SECONDS has drifted from its authority at {}",
        path.display()
    );
}

/// Walk every `.rs` file under `root`, recursively. Byte-identical logic to
/// `prompt_externalization.rs`'s own `collect_rs_files` helper (kept as a separate copy per-file,
/// matching this module's own documented precedent of not sharing cross-test-module code).
fn collect_rs_files(root: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        fs::read_dir(root).unwrap_or_else(|e| panic!("failed to read dir {}: {e}", root.display()));
    for entry in entries {
        let entry = entry.expect("failed to read dir entry");
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Count lines, across every `.rs` file under `root` plus any EXTRA in-memory source texts
/// supplied, that declare `const <const_name>` (i.e. contain the literal substring
/// `const <const_name>`). `extra_sources` is what
/// [`lease_threshold_has_exactly_one_definition`] uses to prove this scanner actually counts
/// rather than always reporting a hardcoded `1` regardless of what is on disk.
fn count_const_declarations(root: &Path, const_name: &str, extra_sources: &[&str]) -> usize {
    let needle = format!("const {const_name}");
    let mut files = Vec::new();
    collect_rs_files(root, &mut files);
    let on_disk = files
        .iter()
        .filter_map(|f| fs::read_to_string(f).ok())
        .flat_map(|s| s.lines().map(str::to_string).collect::<Vec<_>>())
        .filter(|line| line.contains(&needle))
        .count();
    let in_memory = extra_sources
        .iter()
        .flat_map(|s| s.lines())
        .filter(|line| line.contains(&needle))
        .count();
    on_disk + in_memory
}

/// `crates/engine-core/src` — the crate's own source root, resolved from `CARGO_MANIFEST_DIR`
/// exactly as `prompt_externalization.rs`'s `workflows_root()` resolves its own scan root.
fn engine_core_src_root() -> PathBuf {
    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set by cargo");
    Path::new(&manifest_dir).join("src")
}

/// There must be exactly one `const LEASE_STALE_THRESHOLD_SECONDS` declaration anywhere under
/// `crates/engine-core/src` (task 1 landed it in `coord/mod.rs`; every OTHER mention across
/// `coord/write.rs`, `coord_lane.rs` and `sweep/snapshot.rs` is a doc-comment reference, never a
/// second declaration). `REGISTRY_STALE_THRESHOLD_SECONDS` (known present, singly, in
/// `sweep/snapshot.rs`) is the positive control proving the scanner can find a real declaration
/// at all, not merely fail to find a false one. Fed an extra in-memory source string declaring a
/// second `LEASE_STALE_THRESHOLD_SECONDS`, the same scanner must report 2.
#[test]
fn lease_threshold_has_exactly_one_definition() {
    let root = engine_core_src_root();
    let lease_count = count_const_declarations(&root, "LEASE_STALE_THRESHOLD_SECONDS", &[]);
    assert_eq!(
        lease_count, 1,
        "expected exactly one `const LEASE_STALE_THRESHOLD_SECONDS` declaration under {}, found {lease_count}",
        root.display()
    );

    let registry_count = count_const_declarations(&root, "REGISTRY_STALE_THRESHOLD_SECONDS", &[]);
    assert_eq!(
        registry_count, 1,
        "positive control failed: expected exactly one `const REGISTRY_STALE_THRESHOLD_SECONDS` \
         declaration (known present in sweep/snapshot.rs), found {registry_count}"
    );

    let extra_source = "pub const LEASE_STALE_THRESHOLD_SECONDS: f64 = 90.0 * 60.0;\n";
    let with_extra =
        count_const_declarations(&root, "LEASE_STALE_THRESHOLD_SECONDS", &[extra_source]);
    assert_eq!(
        with_extra, 2,
        "scanner fed an extra in-memory declaration must report 2, got {with_extra}"
    );
}

/// One `<lock_dir>/leases/lease-<repo>.json` fixture, written directly as JSON in
/// `okf_core::LeaseRecord`'s own shape — matching what both `coord::write::lease` and
/// `fleet_concurrency_check.py register`'s `_find_blocking_exclusive_lease` read, so neither
/// side needs the other to have produced the file first.
fn write_fixture_lease(lock_dir: &Path, repo: &str, agent: &str, liveness_iso: &str) {
    let leases_dir = lock_dir.join("leases");
    fs::create_dir_all(&leases_dir)
        .unwrap_or_else(|e| panic!("create_dir_all {}: {e}", leases_dir.display()));
    let path = leases_dir.join(format!("lease-{repo}.json"));
    let body = serde_json::json!({
        "repo": repo,
        "lane": format!("{repo}-lane"),
        "agent": agent,
        "acquired_at": liveness_iso,
        "kind": "exclusive",
        "heartbeat": liveness_iso,
    });
    fs::write(&path, body.to_string())
        .unwrap_or_else(|e| panic!("write fixture lease {}: {e}", path.display()));
}

/// Recursively copy `src` into `dst` (`dst` need not exist yet) — gives each side of the
/// holder-conflict parity case its OWN independent copy of the fixture lock dir, so an ALLOWED
/// call on one side (which writes a new lease record) can never be the reason the other side's
/// answer differs, and the acceptance criterion's "ordering between the two sides is irrelevant"
/// is actually true rather than accidentally true.
fn copy_dir_recursive(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap_or_else(|e| panic!("create_dir_all {}: {e}", dst.display()));
    for entry in fs::read_dir(src).unwrap_or_else(|e| panic!("read_dir {}: {e}", src.display())) {
        let entry = entry.expect("dir entry");
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path);
        } else {
            fs::copy(&src_path, &dst_path).unwrap_or_else(|e| {
                panic!("copy {} -> {}: {e}", src_path.display(), dst_path.display())
            });
        }
    }
}

/// Run `python3 <script> register --repo <repo> --agent <agent> --lock-dir <lock_dir>` and
/// return whether it was allowed, per `main()`'s own documented contract (exit 0 allowed, exit 3
/// refused). Any OTHER exit code is a genuine test failure, not a third outcome to paper over.
fn run_python_register(script: &Path, lock_dir: &Path, repo: &str, agent: &str) -> bool {
    let output = Command::new("python3")
        .arg(script)
        .arg("register")
        .arg("--repo")
        .arg(repo)
        .arg("--agent")
        .arg(agent)
        .arg("--lock-dir")
        .arg(lock_dir)
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn python3 {}: {e}", script.display()));
    match output.status.code() {
        Some(0) => true,
        Some(3) => false,
        other => panic!(
            "fleet_concurrency_check.py register exited unexpectedly ({other:?}):\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    }
}

/// Run Rust `coord::write::lease` for `agent`/`repo` against `lock_dir` and return whether it
/// was allowed (`Ok(())`) or refused (`Err(_)`) — the Rust half of one holder-conflict parity
/// case.
fn run_rust_lease(lock_dir: &Path, repo: &str, agent: &str) -> bool {
    let now_iso = chrono::Utc::now().to_rfc3339();
    let no_blocks: Vec<String> = Vec::new();
    let req = engine_core::coord::write::LeaseRequest {
        repo,
        lane: "test-lane",
        agent,
        kind: okf_core::LeaseKind::Exclusive,
        scope: None,
        host: None,
        now_iso: &now_iso,
        window: None,
        lane_blocks: &no_blocks,
    };
    engine_core::coord::write::lease(lock_dir, &req).is_ok()
}

/// The holder-conflict parity case: for a live foreign Exclusive lease, a stale foreign
/// Exclusive lease, and a same-agent lease, `fleet_concurrency_check.py register` and Rust
/// `coord::write::lease` must reach the SAME allow/refuse decision. Each side reads its own
/// independent temp copy of the fixture tree (never the same directory both sides read/write),
/// so ordering between the two calls cannot be the reason they agree or disagree. Skips cleanly
/// when the Python oracle is unavailable, exactly as `reader_and_python_agree_on_active_heavy_lanes_for_a_healthy_tree`
/// above already does.
#[test]
fn lease_holder_conflict_matches_fleet_concurrency_check() {
    let Some(script) = require_parity_environment() else {
        return;
    };

    struct Case {
        name: &'static str,
        holder_agent: &'static str,
        liveness_age_seconds: f64,
        requester_agent: &'static str,
    }

    let cases = [
        Case {
            name: "live foreign Exclusive lease",
            holder_agent: "agent-a",
            liveness_age_seconds: 60.0, // a minute old — comfortably live.
            requester_agent: "agent-b",
        },
        Case {
            name: "stale foreign Exclusive lease",
            holder_agent: "agent-a",
            liveness_age_seconds: engine_core::coord::LEASE_STALE_THRESHOLD_SECONDS + 3600.0,
            requester_agent: "agent-b",
        },
        Case {
            name: "same-agent lease (a renewal, not a conflict)",
            holder_agent: "agent-a",
            liveness_age_seconds: 60.0, // fresh AND same agent — must allow either way.
            requester_agent: "agent-a",
        },
    ];

    for case in cases {
        let repo = "engine-rs";
        let base = tempfile::tempdir().expect("tempdir");
        let liveness = chrono::Utc::now()
            - chrono::Duration::milliseconds((case.liveness_age_seconds * 1000.0) as i64);
        write_fixture_lease(base.path(), repo, case.holder_agent, &liveness.to_rfc3339());

        let python_dir = tempfile::tempdir().expect("tempdir");
        copy_dir_recursive(base.path(), python_dir.path());
        let rust_dir = tempfile::tempdir().expect("tempdir");
        copy_dir_recursive(base.path(), rust_dir.path());

        let python_allowed =
            run_python_register(&script, python_dir.path(), repo, case.requester_agent);
        let rust_allowed = run_rust_lease(rust_dir.path(), repo, case.requester_agent);

        assert_eq!(
            python_allowed, rust_allowed,
            "case `{}`: fleet_concurrency_check.py register allowed={python_allowed} but \
             write::lease allowed={rust_allowed} — holder-conflict parity broken",
            case.name
        );
    }
}
