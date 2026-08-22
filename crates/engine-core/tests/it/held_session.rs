//! Real-tmux integration suite for `HeldSessionNode` (`EN.10.A` task 4).
//!
//! The module's own unit tests (`held_session.rs::tests`) already cover
//! every behaviour against `StubTerminalDriver` — they exist to pin exact
//! call sequences and error text cheaply. This suite intentionally
//! duplicates the four acceptance-criteria behaviours against a REAL tmux
//! server instead: the block record's testing strategy is explicit that
//! the failure modes `EN.10.A` exists to prevent (a leaked session, a
//! hung node) are all real-process behaviours a mock can never reproduce,
//! since a mock only proves the mock holds the state it was told to hold.
//!
//! Every test runs on its own private tmux socket (pid + a nanosecond
//! stamp, one per OS process — see `test_socket_name`), so no test or
//! human session anywhere else on the machine can observe or disturb it,
//! and kills that socket's whole server before returning, on every path
//! including a failing assertion (via the `KillOnDrop` guard).
//!
//! **tmux version pinning.** No test in this file asserts on verbatim
//! tmux output — every assertion here is either on this crate's own typed
//! return values or on a `Lease` parsed by `term_core::lease::Lease`
//! itself, so nothing here is pinned to a particular tmux release.
//! `crates/term-core/src/tmux.rs`/`lease.rs`'s real-tmux tests, which DO
//! assert on raw `show-option` text shape, are pinned to tmux 3.7b (this
//! dev box) with `EN.9.D`'s note that 3.5a (the Mini) reproduces the same
//! shapes.

use std::sync::Arc;
use std::time::Duration;

use engine_contract::TaskContext;
use engine_core::nodes::terminal::held_session::NODE_NAME;
use engine_core::nodes::terminal::{session_name_for, HeldSessionNode};
use engine_core::{Node, NodeError};
use term_core::driver::{TerminalDriver, TmuxDriver};
use term_core::lease::{AcquireRequest, Lease, LeaseError, SessionLease, LEASE_OPTION};
use term_core::model::parse_session_line;

/// Bound every driver call in this suite by the same 5s timeout
/// `term-core`'s own real-tmux tests use.
const DRIVER_TIMEOUT: Duration = Duration::from_secs(5);

/// A unique, throwaway tmux session name per test — pid + a monotonic
/// counter, so concurrent `cargo nextest` processes (each test its own
/// process) and repeated runs within one test never collide.
fn unique_run_id(tag: &str) -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    format!("en10a-held-session-it-{tag}-{}-{n}", std::process::id())
}

fn ctx_with_run_id(run_id: &str) -> TaskContext {
    let mut ctx = TaskContext {
        event: serde_json::json!({}),
        nodes: Default::default(),
        metadata: serde_json::json!({}),
        node_runs: Default::default(),
    };
    ctx.metadata["run_id"] = serde_json::json!(run_id);
    ctx
}

/// A compressed lease TTL/renewal-interval event override — the same
/// override surface a real caller uses (`ctx.event.policy`), sized so
/// several renewal ticks land inside this suite's bounded waits without
/// the whole file taking multiple seconds per test.
fn fast_policy_ctx(run_id: &str) -> TaskContext {
    let mut ctx = ctx_with_run_id(run_id);
    ctx.event = serde_json::json!({
        "policy": { "lease_ttl_ms": 400, "renew_interval_ms": 40 }
    });
    ctx
}

fn lease_option_name(session_name: &str) -> String {
    format!("{LEASE_OPTION}@{session_name}")
}

/// Kills the WHOLE per-process tmux socket's server on drop, regardless
/// of how the test exits (pass, a failing `assert!`/`panic!`, or an early
/// return) — this is safe and cheap because every driver in this suite is
/// now built on [`test_socket_name`], a socket private to this OS process
/// that no other test or human process ever touches (see that function's
/// doc). Killing the server rather than a single named session is what
/// makes cleanup panic-proof: a leaked session on a shared default server
/// used to keep that server alive for the next unrelated test (the exact
/// self-masking defect this block exists to remove); on a private socket
/// there is nothing left to leak into.
struct KillOnDrop {
    socket: String,
}

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        // `Drop` cannot be async, and there is no requirement that it be:
        // `tmux -L <socket> kill-server` is a short-lived synchronous
        // process call, so it runs directly here rather than needing a
        // tokio runtime handle to still be alive during unwind.
        let _ = std::process::Command::new("tmux")
            .args(["-L", &self.socket, "kill-server"])
            .output();
    }
}

/// One tmux socket name per OS PROCESS, unique and collision-proof
/// (pid + a nanosecond stamp, the same derivation `unique_run_id` uses for
/// session names). `cargo nextest` forks a process per test, so every
/// test in this file gets its own private tmux server on this socket —
/// nothing else on the machine, concurrent test process or human session,
/// can ever observe or disturb it. A `new_session` call against this
/// socket boots its server itself (see [`bootstrap_socket`]), so there is
/// no retry/probe dance to get right the way this file's old
/// boot-then-kill helper tried to — just one create, left running until
/// the whole socket is torn down at teardown.
fn test_socket_name() -> String {
    static SOCKET: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SOCKET
        .get_or_init(|| {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            format!("eng-en10a-held-session-it-{}-{nanos}", std::process::id())
        })
        .clone()
}

/// Starts this test's private socket's tmux server WITHOUT touching the
/// name the test itself is about to acquire: creates a throwaway session
/// under an unrelated name and — unlike this file's old boot-then-kill
/// helper — leaves it running instead of killing it right back.
/// `HeldSessionNode::process`'s own first driver call is `list_sessions`,
/// and (like every tmux subcommand except `new-session`) that errors
/// against a socket whose server has never started, so a bare
/// `TmuxDriver::new(..).with_socket(..)` still needs exactly one
/// `new_session` before the node's own logic can run. Killing that boot
/// session immediately was the actual defect this block removes (killing
/// the last session on a socket terminates its server); leaving it alone
/// and letting the whole-socket [`KillOnDrop`] guard reap it at teardown
/// is what makes this safe on a socket the test privately owns.
async fn bootstrap_socket(driver: &TmuxDriver, tag: &str) {
    let boot = format!("{tag}-boot");
    driver
        .new_session(&boot, None)
        .await
        .expect("bootstrapping this test's private tmux socket must succeed");
}

/// Whether `session_name` appears in a real `list-sessions -F` listing,
/// parsed the same way production code does (`parse_session_line`) rather
/// than assuming a particular raw delimiter.
fn session_listed(list_output: &str, session_name: &str) -> bool {
    list_output
        .lines()
        .filter_map(|line| parse_session_line(line).ok())
        .any(|session| session.name == session_name)
}

async fn read_lease(driver: &TmuxDriver, session_name: &str) -> Option<Lease> {
    let raw = driver
        .show_option(&lease_option_name(session_name))
        .await
        .ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Lease::parse(trimmed)
    }
}

// ── Session identity across two consecutive nodes ──────────────────────

#[tokio::test]
async fn real_tmux_two_consecutive_nodes_reuse_one_session_with_identical_id() {
    let socket = test_socket_name();
    let driver = Arc::new(TmuxDriver::new(DRIVER_TIMEOUT).with_socket(socket.clone()));
    let run_id = unique_run_id("identity");
    let session_name = session_name_for(&run_id, NODE_NAME);
    let _guard = KillOnDrop { socket };
    bootstrap_socket(&driver, "identity").await;

    let node = HeldSessionNode::new(driver.clone() as Arc<dyn TerminalDriver>);

    // First node boundary: creates the real tmux session and acquires the
    // lease against it.
    let ctx1 = node
        .process(ctx_with_run_id(&run_id))
        .await
        .expect("first HeldSessionNode boundary against real tmux must succeed");
    let stamped1 = &ctx1.nodes[NODE_NAME];
    assert_eq!(stamped1["session_name"], serde_json::json!(session_name));
    assert_eq!(stamped1["created"], serde_json::json!(true));

    let listed = driver
        .list_sessions()
        .await
        .expect("real tmux list-sessions must succeed");
    assert!(
        session_listed(&listed, &session_name),
        "expected the newly created session in real tmux's own listing: {listed:?}"
    );

    // Second node boundary: a fresh `process` call, as a later node in the
    // same run would make. Must observe the IDENTICAL session id and must
    // create nothing new in tmux.
    let ctx2 = node
        .process(ctx_with_run_id(&run_id))
        .await
        .expect("re-entry against an already-held real session must succeed");
    let stamped2 = &ctx2.nodes[NODE_NAME];
    assert_eq!(
        stamped2["session_name"], stamped1["session_name"],
        "session id must be identical across both node boundaries"
    );
    assert_eq!(stamped2["lease_nonce"], stamped1["lease_nonce"]);
    assert_eq!(stamped2["created"], serde_json::json!(false));
    assert_eq!(stamped2["acquired_by_this_call"], serde_json::json!(false));
}

// ── Lease renewal over a compressed TTL ─────────────────────────────────

#[tokio::test]
async fn real_tmux_held_session_renews_its_lease_before_expiry() {
    let socket = test_socket_name();
    let driver = Arc::new(TmuxDriver::new(DRIVER_TIMEOUT).with_socket(socket.clone()));
    let run_id = unique_run_id("renew");
    let session_name = session_name_for(&run_id, NODE_NAME);
    let _guard = KillOnDrop { socket };
    bootstrap_socket(&driver, "renew").await;

    let node = HeldSessionNode::new(driver.clone() as Arc<dyn TerminalDriver>);
    node.process(fast_policy_ctx(&run_id))
        .await
        .expect("initial acquire against real tmux must succeed");

    let initial = read_lease(&driver, &session_name)
        .await
        .expect("lease must be readable off real tmux immediately after acquire");

    // Long enough for several renewal ticks at the 40ms interval the
    // compressed policy above set, well inside the 400ms TTL.
    tokio::time::sleep(Duration::from_millis(250)).await;

    let renewed = read_lease(&driver, &session_name)
        .await
        .expect("lease must still be readable after the renewal window");

    assert_eq!(
        renewed.nonce, initial.nonce,
        "renewal must extend the SAME lease (same nonce), not acquire a new one"
    );
    assert!(
        renewed.expires_at_ms > initial.expires_at_ms,
        "expires_at_ms must have advanced: initial={}, renewed={}",
        initial.expires_at_ms,
        renewed.expires_at_ms
    );
}

// ── Orphan reconcile after a simulated crash ────────────────────────────

/// Models a `HeldSessionNode` that crashed mid-run: it acquired the real
/// `EN.9.B` lease and then its process (and with it, the renewal loop)
/// simply stopped — nothing here calls `HeldSessionNode` again, exactly
/// as a crashed process would never call it again either. The lease is
/// therefore never renewed and, per the module doc, this is deliberately
/// indistinguishable from any other lapsed lease: the fix lives entirely
/// in `SessionLease` being fail-closed by default and reconcilable only
/// through an explicit `steal_after` bound (what `EN.9.C`'s boot sweep
/// supplies) — never a silent, always-on reacquire.
#[tokio::test]
async fn real_tmux_abandoned_lease_is_fail_closed_then_reconciled_via_steal_after() {
    let socket = test_socket_name();
    let driver = Arc::new(TmuxDriver::new(DRIVER_TIMEOUT).with_socket(socket.clone()));
    let run_id = unique_run_id("orphan");
    let session_name = session_name_for(&run_id, NODE_NAME);
    let _guard = KillOnDrop { socket };

    driver
        .new_session(&session_name, None)
        .await
        .expect("real tmux creates the throwaway session");

    let lease = SessionLease::new(driver.as_ref() as &dyn TerminalDriver);
    let now = now_ms();
    let crashed_nonce = "crashed-nonce";
    lease
        .acquire(AcquireRequest {
            session_name: &session_name,
            run_id: &run_id,
            nonce: crashed_nonce,
            identity: NODE_NAME,
            // A lease that is already expired by the time we "boot" —
            // stands in for a held session whose owning process crashed
            // long enough ago that nothing has renewed it since.
            expires_at_ms: now.saturating_sub(50),
            now_ms: now.saturating_sub(200),
            steal_after: None,
        })
        .await
        .expect("the crashed process's own original acquire succeeds against a fresh session");

    // Fail-closed: without an explicit steal window, a boot that simply
    // re-ran `HeldSessionNode::acquire` logic must NOT silently reclaim
    // the abandoned lease — the exact leak `EN.10.A`'s "why" exists to
    // prevent looks identical to a silent reacquire succeeding here.
    let now2 = now_ms();
    let refused = lease
        .acquire(AcquireRequest {
            session_name: &session_name,
            run_id: "boot-sweep-run",
            nonce: "boot-sweep-nonce",
            identity: "OrphanSweep",
            expires_at_ms: now2 + 60_000,
            now_ms: now2,
            steal_after: None,
        })
        .await;
    assert!(
        matches!(refused, Err(LeaseError::NoStealWindow)),
        "an abandoned lease must never be silently reacquired with no steal_after bound, got {refused:?}"
    );

    // `EN.9.C`'s boot-sweep reconciliation: an explicit steal window
    // reclaims the abandoned lease instead of leaking the session
    // forever.
    let reconciled = lease
        .acquire(AcquireRequest {
            session_name: &session_name,
            run_id: "boot-sweep-run",
            nonce: "boot-sweep-nonce",
            identity: "OrphanSweep",
            expires_at_ms: now2 + 60_000,
            now_ms: now2,
            steal_after: Some(Duration::from_millis(1)),
        })
        .await
        .expect("an abandoned lease past its steal_after grace must be reconcilable, not leaked forever");
    assert_eq!(reconciled.run_id, "boot-sweep-run");
    assert_eq!(reconciled.nonce, "boot-sweep-nonce");

    // The session itself was never re-created or duplicated by any of
    // this — it is still the one throwaway session this test made.
    let listed = driver
        .list_sessions()
        .await
        .expect("real tmux list-sessions must succeed");
    let count = listed
        .lines()
        .filter_map(|line| parse_session_line(line).ok())
        .filter(|session| session.name == session_name)
        .count();
    assert_eq!(count, 1, "reconciliation must not leak a duplicate session");
}

// ── Error-not-hang on external kill ─────────────────────────────────────

#[tokio::test]
async fn real_tmux_external_kill_surfaces_a_node_error_within_a_bounded_time_not_a_hang() {
    let socket = test_socket_name();
    let driver = Arc::new(TmuxDriver::new(DRIVER_TIMEOUT).with_socket(socket.clone()));
    let run_id = unique_run_id("ext-kill");
    let session_name = session_name_for(&run_id, NODE_NAME);
    // The session itself is killed for real mid-test as the scenario
    // itself (and that alone tears down this test's private socket
    // server too, since it is the only session on it — killing the last
    // session on a socket terminates that socket's server). The guard
    // below still runs unconditionally on drop so a panic anywhere
    // before that point — or the loop below timing out — cannot leave
    // this test's socket server behind.
    let _guard = KillOnDrop { socket };
    bootstrap_socket(&driver, "ext-kill").await;

    let node = HeldSessionNode::new(driver.clone() as Arc<dyn TerminalDriver>);
    node.process(fast_policy_ctx(&run_id))
        .await
        .expect("initial acquire against real tmux must succeed");

    // Kill the real tmux session out from under the run.
    driver
        .kill_session(&session_name)
        .await
        .expect("real tmux must accept killing the session we just created");

    // The renewal loop notices on its next tick (bounded by
    // `renew_interval_ms`, 40ms here); poll `process` re-entry until it
    // surfaces the typed error, wrapped in an outer bound so a hang fails
    // the test loudly instead of wedging `cargo nextest` indefinitely.
    let result: Result<NodeError, ()> = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match node.process(ctx_with_run_id(&run_id)).await {
                Err(err) => return Ok(err),
                Ok(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
    })
    .await
    .expect("an external kill must surface a node error within a bounded time, never hang");

    let err = result.expect("loop only returns Ok(err) on the Err path above");
    assert!(
        err.message.contains("vanished externally"),
        "expected an external-kill message, got: {}",
        err.message
    );
    assert!(
        !err.message.contains("lease lost"),
        "external-kill error must read distinctly from a lease-lost message: {}",
        err.message
    );
    // The message names all three failure modes for legibility (see
    // `HeldSessionFailure::into_node_error`'s doc), so it legitimately
    // mentions "timeout" in the clause that RULES it out. The
    // distinguishing signal is that clause itself, not the word's
    // absence.
    assert!(
        err.message.contains("not a driver timeout"),
        "expected the message to explicitly rule out a driver timeout, got: {}",
        err.message
    );
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
