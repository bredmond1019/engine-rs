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
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;

use engine_contract::TaskContext;
use engine_core::nodes::terminal::held_session::NODE_NAME as HELD_SESSION_NODE_NAME;
use engine_core::nodes::terminal::HeldSessionNode;
use engine_core::workflows::orchestration::escalate::{
    EscalationChannel, EscalationKind, EscalationRecord, EscalationSeverity, NewEscalation,
};
use engine_core::workflows::orchestration::graph::{
    held_session_name, held_session_outcome_status, HELD_SESSION_OUTCOME_DONE,
    HELD_SESSION_OUTCOME_SESSION_LOST,
};
use engine_core::Node;
use term_core::driver::{GuardedSendRequest, GuardedSender, SendError, TerminalDriver, TmuxDriver};
use term_core::hold::HoldError;
use term_core::model::parse_session_line;

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

// ── `EN.15.I` task 3: real tmux — attach / kill-server / naming ─────────
//
// Everything above in this file cross-validates `check_escalations.py`
// against synthesised lines. This section drives the ACTUAL `HELD_SESSION`
// acceptance criteria against a REAL tmux server — never a mock — exactly
// like `tests/it/held_session.rs`'s own real-tmux suite (`EN.10.A` task
// 4): a genuine `tmux` process, a private `-L <socket>` this test alone
// can see, torn down unconditionally on drop. The helpers below are kept
// as this file's own copies of that suite's pattern rather than reaching
// across a sibling test module — the same "kept as its own copy... rather
// than introducing cross-module coupling" precedent this file's own
// `find_brain_root` doc states above.

fn held_unique_run_id(tag: &str) -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    format!("en15i-t3-held-{tag}-{}-{n}", std::process::id())
}

/// One tmux socket per test — pid + a nanosecond stamp, so no other test
/// process or human session anywhere on the machine ever shares it.
fn held_socket_name(tag: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("en15i-t3-sock-{tag}-{}-{nanos}", std::process::id())
}

fn held_ctx(run_id: &str) -> TaskContext {
    let mut ctx = TaskContext {
        event: json!({}),
        nodes: Default::default(),
        metadata: json!({}),
        node_runs: Default::default(),
    };
    ctx.metadata["run_id"] = json!(run_id);
    ctx
}

/// A compressed lease TTL/renewal-interval event override — the same
/// `ctx.event.policy` override surface a real caller uses — sized so
/// several renewal ticks land inside this suite's bounded waits.
fn held_fast_policy_ctx(run_id: &str) -> TaskContext {
    let mut ctx = held_ctx(run_id);
    ctx.event = json!({ "policy": { "lease_ttl_ms": 400, "renew_interval_ms": 40 } });
    ctx
}

fn held_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Whether `session_name` appears in a real `list-sessions -F` listing,
/// parsed the same way production code does.
fn held_session_listed(list_output: &str, session_name: &str) -> bool {
    list_output
        .lines()
        .filter_map(|line| parse_session_line(line).ok())
        .any(|session| session.name == session_name)
}

/// Kills the WHOLE per-process tmux socket's server on drop, regardless of
/// how the test exits — mirrors `tests/it/held_session.rs::KillOnDrop`
/// exactly (see that type's own doc for why killing the whole socket,
/// rather than one named session, is what makes this panic-proof on a
/// socket the test privately owns).
struct HeldKillOnDrop {
    socket: String,
}

impl Drop for HeldKillOnDrop {
    fn drop(&mut self) {
        let _ = std::process::Command::new("tmux")
            .args(["-L", &self.socket, "kill-server"])
            .output();
    }
}

/// Boots this test's private socket's tmux server via a throwaway session
/// under an unrelated name and leaves it running — mirrors
/// `tests/it/held_session.rs::bootstrap_socket` exactly.
async fn held_bootstrap_socket(driver: &TmuxDriver, tag: &str) {
    let boot = format!("{tag}-boot");
    driver
        .new_session(&boot, None)
        .await
        .expect("bootstrapping this test's private tmux socket must succeed");
}

/// **AC1**: attaching mid-chain makes `tmux display-message -p
/// '#{session_attached}'` report 1, and the engine's next send WAITS.
///
/// The attaching "client" is a REAL `tmux -C` (control-mode) client
/// process — a genuine client registration tmux itself counts toward
/// `#{session_attached}`, not a flag this test sets by hand. Control mode
/// needs no pty (unlike a raw `tmux attach-session`, which requires one):
/// it talks to the session over a plain stdio pipe, which is what makes it
/// driveable from a test harness with no controlling terminal of its own.
/// Kept alive for as long as this test needs it by holding onto its piped
/// stdin — dropping (or closing) that pipe is what ends the client.
#[tokio::test]
async fn real_attach_makes_session_attached_and_the_engines_next_send_waits() {
    let socket = held_socket_name("attach");
    let driver = Arc::new(TmuxDriver::new(Duration::from_secs(5)).with_socket(socket.clone()));
    let _guard = HeldKillOnDrop {
        socket: socket.clone(),
    };
    held_bootstrap_socket(&driver, "attach").await;

    let run_id = held_unique_run_id("attach");
    let session_name =
        engine_core::nodes::terminal::session_name_for(&run_id, HELD_SESSION_NODE_NAME);
    let node = HeldSessionNode::new(driver.clone() as Arc<dyn TerminalDriver>);
    let ctx = node
        .process(held_ctx(&run_id))
        .await
        .expect("real acquire against real tmux must succeed");
    let nonce = ctx.nodes[HELD_SESSION_NODE_NAME]["lease_nonce"]
        .as_str()
        .expect("lease_nonce is a string")
        .to_string();

    // Pre-seed `@operator_hold@<session>` to a real, explicitly-empty
    // value. Real tmux's `show-option -g` errors ("invalid option") on a
    // NEVER-set option rather than succeeding with an empty string — the
    // exact real-tmux defect class `term_core::driver::StubOutcome::
    // invalid_option`'s own doc describes for the lease option, and
    // `term_core::hold::OperatorHold::read_operator_hold`'s unconditional
    // `?` propagates that error straight out of `guard_send` before this
    // test ever reaches the `#{session_attached}` signal it means to
    // exercise. Pre-seeding is a test-side workaround for that
    // (out-of-scope-for-this-task) `hold.rs` gap, not a change to it —
    // `set_option` is real tmux too, so `show_option` now succeeds with a
    // real empty read-back exactly as it would once a managed attach path
    // has ever cleared the option.
    driver
        .set_option(
            &term_core::hold::operator_hold_option_name(&session_name),
            "",
        )
        .await
        .expect("real tmux set-option must succeed");

    let before = driver
        .display_message(&session_name, "#{session_attached}")
        .await
        .expect("real display-message must succeed");
    assert_eq!(
        before.trim(),
        "0",
        "must not read attached before any client has attached: {before:?}"
    );

    let mut client = std::process::Command::new("tmux")
        .args(["-L", &socket, "-C", "attach-session", "-t", &session_name])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn a real tmux control-mode attach client");

    // Poll real tmux's own signal until the real client is observed
    // attached — bounded, not a fixed sleep guess.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let raw = driver
                .display_message(&session_name, "#{session_attached}")
                .await
                .expect("real display-message must succeed");
            if raw.trim() == "1" {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("a real tmux client must be observed attached within a bounded time");

    let attached = driver
        .display_message(&session_name, "#{session_attached}")
        .await
        .expect("real display-message must succeed");
    assert_eq!(attached.trim(), "1");

    // AC1's second half: the engine's next send WAITS — refused by the
    // REAL `GuardedSender`/`OperatorHold`, which reads this exact real
    // tmux signal (`EN.12.B`, closed, consumed as-built).
    let guarded = GuardedSender::new(driver.as_ref() as &dyn TerminalDriver);
    let now = held_now_ms();
    let send_result = guarded
        .send_keys(GuardedSendRequest {
            session_name: &session_name,
            keys: "echo hi",
            run_id: &run_id,
            nonce: &nonce,
            identity: HELD_SESSION_NODE_NAME,
            lease_expires_at_ms: now + 60_000,
            now_ms: now,
        })
        .await;
    match send_result {
        Err(SendError::Hold(HoldError::Paused { attached, .. })) => {
            assert!(
                attached,
                "must be refused because the session is attached right now"
            );
        }
        other => panic!(
            "expected the engine's next send to WAIT (refused by the real operator hold) \
             while a real client is attached, got: {other:?}"
        ),
    }

    // End the real client before this test's socket-wide guard tears the
    // server down.
    drop(client.stdin.take());
    let _ = client.kill();
    let _ = client.wait();
}

/// **AC2**: killing the tmux server makes the chain record `session_lost`,
/// not `done`.
///
/// This is a DIFFERENT scenario from `tests/it/held_session.rs`'s own
/// `real_tmux_external_kill_surfaces_a_node_error_within_a_bounded_time_not_a_hang`,
/// which kills only the one held SESSION (leaving the socket's server
/// alive) and reaches `HeldSessionFailure::ExternallyKilled`. Killing the
/// whole SERVER never reaches that variant: `renewal_loop`'s
/// `list_sessions` call errors outright against a dead server rather than
/// succeeding with the session merely absent, so the loop falls through to
/// `SessionLease::renew`, which fails the same way and is recorded as
/// `HeldSessionFailure::LeaseLost` with a `TmuxError::NoServer`-flavored
/// reason instead — verified here against a REAL killed server, not
/// assumed from reading the loop. `held_session_outcome_status` (this
/// block's own new classifier, `graph.rs`) is what folds that shape into
/// `session_lost` too, exercised end to end below.
#[tokio::test]
async fn real_kill_server_makes_the_chain_record_session_lost_not_done() {
    let socket = held_socket_name("kill-server");
    let driver = Arc::new(TmuxDriver::new(Duration::from_secs(5)).with_socket(socket.clone()));
    // The scenario itself kills this socket's server (the LAST session on
    // it), but the guard still runs unconditionally on drop so a panic
    // anywhere before that point can never leave a server behind.
    let _guard = HeldKillOnDrop {
        socket: socket.clone(),
    };
    held_bootstrap_socket(&driver, "kill-server").await;

    let run_id = held_unique_run_id("kill-server");
    let node = HeldSessionNode::new(driver.clone() as Arc<dyn TerminalDriver>);

    let ok_result = node.process(held_fast_policy_ctx(&run_id)).await;
    assert_eq!(
        held_session_outcome_status(&ok_result),
        Ok(HELD_SESSION_OUTCOME_DONE),
        "a healthy real acquire must record 'done'"
    );

    // Kill the WHOLE tmux SERVER on this private socket for real.
    let kill = std::process::Command::new("tmux")
        .args(["-L", &socket, "kill-server"])
        .output()
        .expect("must be able to invoke tmux kill-server at all");
    assert!(
        kill.status.success(),
        "real tmux kill-server must succeed: {kill:?}"
    );

    // Poll re-entry until the renewal loop's next tick (bounded by
    // `renew_interval_ms`, 40ms) notices, wrapped in an outer bound so a
    // hang fails the test loudly instead of wedging `cargo nextest`.
    let lost_result: Result<TaskContext, engine_core::NodeError> =
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let result = node.process(held_ctx(&run_id)).await;
                if result.is_err() {
                    return result;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("a killed server must be recorded within a bounded time, never hang");

    assert_eq!(
        held_session_outcome_status(&lost_result),
        Ok(HELD_SESSION_OUTCOME_SESSION_LOST),
        "the chain must record session_lost, not done, once its held session's tmux \
         server was killed for real: {lost_result:?}"
    );
}

/// **AC3**: a `session:<slug>` escalation names a tmux session that
/// actually exists at the moment it is written — checked against a REAL
/// session `HeldSessionNode` acquired for real, and against real tmux's
/// own `list-sessions` output at the point `to_jsonl_line()` (the write)
/// is produced.
#[tokio::test]
async fn session_escalation_names_a_real_tmux_session_that_exists_at_write_time() {
    let socket = held_socket_name("escalation-name");
    let driver = Arc::new(TmuxDriver::new(Duration::from_secs(5)).with_socket(socket.clone()));
    let _guard = HeldKillOnDrop {
        socket: socket.clone(),
    };
    held_bootstrap_socket(&driver, "escalation-name").await;

    let run_id = held_unique_run_id("escalation-name");
    let node = HeldSessionNode::new(driver.clone() as Arc<dyn TerminalDriver>);
    let ctx = node
        .process(held_ctx(&run_id))
        .await
        .expect("real acquire against real tmux must succeed");
    let session_name = ctx.nodes[HELD_SESSION_NODE_NAME]["session_name"]
        .as_str()
        .expect("session_name is a string")
        .to_string();

    let record = EscalationRecord::new(NewEscalation {
        ts_utc: "2026-09-10T00:00:00Z".to_string(),
        repo: "engine-rs".to_string(),
        lane: "engine-rs-held".to_string(),
        kind: EscalationKind::Bail,
        severity: EscalationSeverity::Blocking,
        channel: EscalationChannel::session(session_name.clone()).unwrap(),
        block: Some("EN.15.I".to_string()),
        gate_id: "coordination-layer-port/engine-rs/EN.15.I".to_string(),
        summary: "Task 3: a held chain waiting for a human attach.".to_string(),
        verified_by: "cargo nextest run -p engine-core --test it \
                       escalate::session_escalation_names_a_real_tmux_session_that_exists_at_write_time\nok"
            .to_string(),
        durable_home: json!({
            "channel": "run-record",
            "ref": "engine-rs/planning/orchestration-run/coordination-layer-port/notes.md#en-15-i",
        }),
        verified_at_sha: "abc1234".to_string(),
        clears_when: None,
        host: None,
    })
    .expect("valid session-channel record composes");

    // "At the moment it is written" — the write itself.
    let line = record.to_jsonl_line();
    assert!(
        line.contains(&session_name),
        "the escalation line must actually name the session: {line}"
    );

    let listed = driver
        .list_sessions()
        .await
        .expect("real tmux list-sessions must succeed");
    assert!(
        held_session_listed(&listed, &session_name),
        "the session named in the escalation must actually exist in real tmux \
         at write time: {listed:?}"
    );
}

/// **AC4**: session names follow `lane-<repo>-<lane>` exactly, verified
/// against a resolved `HeldSessionNode` session — `held_session_name`
/// (task 1's pure derivation, `graph.rs`) names a session that real tmux
/// actually accepts and lists back under that identical name.
#[tokio::test]
async fn held_session_name_resolves_to_a_real_existing_tmux_session() {
    let socket = held_socket_name("lane-naming");
    let driver = Arc::new(TmuxDriver::new(Duration::from_secs(5)).with_socket(socket.clone()));
    let _guard = HeldKillOnDrop {
        socket: socket.clone(),
    };
    held_bootstrap_socket(&driver, "lane-naming").await;

    let repo = "engine-rs";
    let lane = "lane-en15i-t3";
    let name = held_session_name(repo, lane);
    assert_eq!(name, format!("lane-{repo}-{lane}"));

    driver
        .new_session(&name, None)
        .await
        .expect("real tmux must accept a session literally named lane-<repo>-<lane>");

    let listed = driver
        .list_sessions()
        .await
        .expect("real tmux list-sessions must succeed");
    assert!(
        held_session_listed(&listed, &name),
        "a session named `lane-<repo>-<lane>` must resolve in real tmux's own listing: {listed:?}"
    );
}
