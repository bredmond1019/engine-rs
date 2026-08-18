//! `held_session` (`EN.10.A` task 1) — `HeldSessionNode`: a tmux session
//! acquired ONCE and carried across node boundaries under the `EN.9.B`
//! lease, rather than the per-node scripted session `TerminalSessionNode`
//! (`session.rs`) creates and lets lapse.
//!
//! # Why this is not just `TerminalSessionNode` again
//!
//! `TerminalSessionNode` acquires a lease sized to cover roughly one
//! node's own work; nothing renews it between node boundaries, so a
//! multi-node workflow that wants the SAME session alive across an
//! arbitrarily long gap (an operator thinking, a cross-repo lane running
//! for hours per `EN.10.A`'s "why") would need every intervening node to
//! re-acquire — and a re-acquire after the lease has lapsed and been
//! reaped is indistinguishable from a fresh session.
//!
//! `HeldSessionNode` instead spawns a background renewal loop the FIRST
//! time it runs for a given run, and every later node boundary that asks
//! for the same session finds that loop already keeping the lease alive
//! rather than starting a second one. The loop, not any single node
//! invocation, is what "holds" the session — this is what makes the
//! session survive gaps between node calls, not merely within one.
//!
//! # Session identity is process-global, not per-call
//!
//! [`registry`] is a process-wide map from tmux session name to the
//! [`HeldSessionHandle`] that owns its renewal loop. Two consecutive
//! `HeldSessionNode` invocations for the same `run_id` (a back-edge
//! re-entry, exactly the case `session.rs`'s module doc calls out) derive
//! the identical [`super::identity::session_name_for`] name and therefore
//! find the SAME entry: the second call does not touch tmux or the lease
//! at all, it just confirms the held session is still registered. That is
//! the whole task-1 deliverable — verified directly in the tests below,
//! not assumed from the naming function alone.
//!
//! # Reconciling a crash
//!
//! This node deliberately does nothing special for a crash-restart: it
//! relies entirely on the `EN.9.B` lease already being fail-closed
//! (`term_core::lease::LeaseError::NoStealWindow` — an expired lease is
//! never silently reacquired) and on `EN.9.C`'s boot-sweep orphan
//! recovery to reconcile a run whose renewal loop died with its process.
//! A held session that stops renewing (process crash) simply lets its
//! lease expire like any other; nothing here needs to detect that itself
//! for the lease to stay safe. Task 2 adds explicit external-kill
//! detection on top of this.
//!
//! # Policy knobs (CLAUDE.md standing rule 6)
//!
//! - [`HeldSessionPolicy::lease_ttl_ms`] — how long an acquired/renewed
//!   lease is valid for. Defaults to `TerminalSessionNode`'s own
//!   `DEFAULT_LEASE_TTL` (300s), so a workflow that never touches this
//!   knob at all gets exactly the TTL an existing terminal node would.
//! - [`HeldSessionPolicy::renew_interval_ms`] — how often the background
//!   loop renews, always kept well inside `lease_ttl_ms` by every
//!   built-in default and every named profile (a renewal interval at or
//!   past the TTL is a session that lets its own lease lapse, which is
//!   indistinguishable from an orphan and will be reaped).
//!
//! Resolved through the same four-layer precedence every other policy
//! surface in this crate uses (per-run `ctx.event.policy` override > a
//! named `profile` bundle > `<`[`WORKFLOW_KEY`]`>.policy`/`.profiles` in
//! `planning/harness.json` > [`HeldSessionPolicy::default`]) — mirrors
//! [`super::hold_policy`] exactly, generalized via
//! `crate::policy::read_harness_policy_defaults_from` /
//! `crate::policy::resolve_profile_from`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use engine_contract::TaskContext;
use serde::{Deserialize, Serialize};
use term_core::driver::TerminalDriver;
use term_core::lease::{AcquireRequest, SessionLease};
use term_core::model::parse_session_line;

use crate::node::{Node, NodeError};
use crate::policy::{
    merge_opt, read_harness_policy_defaults_from, resolve as resolve_policy_layers,
    resolve_profile_from, Policy, PolicyConfigSource,
};
use crate::workflow::read_run_id;
use crate::workflows::put_result;

use super::identity::session_name_for;

/// The `Node::name()` identity `HeldSessionNode` runs under, the
/// `ctx.nodes` key its output is stamped onto, AND the identity folded
/// into the session name via `session_name_for` — deliberately shared by
/// every instance (unlike `TerminalSessionNode`'s per-type-only use of its
/// own `NODE_NAME`) so distinct `HeldSessionNode` invocations across one
/// run always resolve to the same held session.
pub const NODE_NAME: &str = "HeldSessionNode";

/// The `harness.json` section key this policy's knobs live under
/// (`held_session.policy` / `held_session.profiles`).
const WORKFLOW_KEY: &str = "held_session";

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ── Policy ───────────────────────────────────────────────────────────────

/// The fully-resolved, per-run held-session policy. See the module docs
/// for what each knob configures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldSessionPolicy {
    pub lease_ttl_ms: u64,
    pub renew_interval_ms: u64,
}

impl Default for HeldSessionPolicy {
    /// The behavior-stable baseline: a 300s lease TTL — identical to
    /// `super::session::DEFAULT_LEASE_TTL` — renewed every 100s, a third
    /// of the TTL, so at least two renewal attempts land inside any single
    /// TTL window before it could lapse.
    fn default() -> Self {
        Self {
            lease_ttl_ms: 300_000,
            renew_interval_ms: 100_000,
        }
    }
}

/// All-optional mirror of [`HeldSessionPolicy`] used by the override
/// layers (`harness.json`'s `held_session.policy`, a named `profile`, and
/// a per-run `ctx.event.policy` override).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialHeldSessionPolicy {
    pub lease_ttl_ms: Option<u64>,
    pub renew_interval_ms: Option<u64>,
}

impl Policy for HeldSessionPolicy {
    type Partial = PartialHeldSessionPolicy;

    fn apply(self, over: &PartialHeldSessionPolicy) -> Self {
        Self {
            lease_ttl_ms: merge_opt(self.lease_ttl_ms, over.lease_ttl_ms),
            renew_interval_ms: merge_opt(self.renew_interval_ms, over.renew_interval_ms),
        }
    }
}

/// Resolve the four policy layers into one concrete [`HeldSessionPolicy`],
/// high->low precedence: `event_override` beats `profile` beats
/// `harness_defaults` beats [`HeldSessionPolicy::default`].
#[must_use]
pub fn resolve(
    harness_defaults: Option<&PartialHeldSessionPolicy>,
    profile: Option<&PartialHeldSessionPolicy>,
    event_override: Option<&PartialHeldSessionPolicy>,
) -> HeldSessionPolicy {
    resolve_policy_layers(
        HeldSessionPolicy::default(),
        harness_defaults,
        profile,
        event_override,
    )
}

/// Resolve [`HeldSessionPolicy`] for one workflow's run, reading
/// `<workflow_key>.policy` / `<workflow_key>.profiles` out of `source` and
/// applying `event_override`/`profile_name` on top, high->low precedence
/// via [`resolve`]. Mirrors `hold_policy::resolve_for_workflow` exactly.
pub fn resolve_for_workflow(
    workflow_key: &str,
    source: &PolicyConfigSource,
    profile_name: Option<&str>,
    event_override: Option<&PartialHeldSessionPolicy>,
) -> Result<HeldSessionPolicy, NodeError> {
    let harness_defaults =
        read_harness_policy_defaults_from::<PartialHeldSessionPolicy>(source, workflow_key)?;
    let profile = resolve_profile_from::<PartialHeldSessionPolicy>(
        profile_name,
        source,
        workflow_key,
        profiles::profile_by_name,
    )?;
    Ok(resolve(
        harness_defaults.as_ref(),
        profile.as_ref(),
        event_override,
    ))
}

/// Named [`PartialHeldSessionPolicy`] bundles, per CLAUDE.md standing rule
/// 6's "every workflow ships the three named profiles" — `baseline`
/// restates the built-in default verbatim, `cheap-fast` holds a
/// short-lived session with frequent, cheap renewals (fast reclaim if a
/// run dies, at the cost of more `set-option` traffic), `thorough` holds a
/// long-lived session with infrequent renewals (an hours-long cross-repo
/// lane per `EN.10.A`'s "why" should not need this knob touched at all,
/// but a deployment that wants an even wider margin can reach for this).
pub mod profiles {
    use super::{HeldSessionPolicy, PartialHeldSessionPolicy};

    /// Restates [`HeldSessionPolicy::default`] verbatim — selecting
    /// `"baseline"` must not silently change behavior.
    #[must_use]
    pub fn baseline() -> PartialHeldSessionPolicy {
        let default = HeldSessionPolicy::default();
        PartialHeldSessionPolicy {
            lease_ttl_ms: Some(default.lease_ttl_ms),
            renew_interval_ms: Some(default.renew_interval_ms),
        }
    }

    /// The cost/latency floor: a 60s TTL renewed every 20s.
    #[must_use]
    pub fn cheap_fast() -> PartialHeldSessionPolicy {
        PartialHeldSessionPolicy {
            lease_ttl_ms: Some(60_000),
            renew_interval_ms: Some(20_000),
        }
    }

    /// The quality ceiling: a 15-minute TTL renewed every 5 minutes —
    /// still a third of the TTL, just at the scale an hours-long
    /// cross-repo lane can leave alone.
    #[must_use]
    pub fn thorough() -> PartialHeldSessionPolicy {
        PartialHeldSessionPolicy {
            lease_ttl_ms: Some(900_000),
            renew_interval_ms: Some(300_000),
        }
    }

    /// Look up one of the three canonical profile names. `None` for any
    /// other name — callers decide whether an unknown name is an error.
    #[must_use]
    pub fn profile_by_name(name: &str) -> Option<PartialHeldSessionPolicy> {
        match name {
            "baseline" => Some(baseline()),
            "cheap-fast" => Some(cheap_fast()),
            "thorough" => Some(thorough()),
            _ => None,
        }
    }
}

// ── The held-session registry ───────────────────────────────────────────

/// One held session's renewal state, kept alive in [`registry`] for as
/// long as the process runs (or until a future task adds explicit
/// teardown — out of task-1 scope).
pub struct HeldSessionHandle {
    /// The tmux session name this handle is renewing the lease for. Not
    /// read internally today — kept for task 2's external-kill detection,
    /// which needs to name the session in its error.
    #[allow(dead_code)]
    session_name: String,
    /// The deterministic lease nonce this handle's renewal loop renews
    /// under (mirrors `TerminalSessionNode`'s `nonce = session_name`
    /// convention — see `session.rs`'s module doc on why a deterministic
    /// nonce is what makes back-edge re-entry a no-op-success rather than
    /// a foreign-lease collision).
    nonce: String,
    /// Keeps the background renewal task alive for as long as this handle
    /// is held in the registry. Never read directly — its only job is to
    /// not be dropped, mirroring `admission.rs`'s `AdmissionPermit::permit`
    /// field.
    #[allow(dead_code)]
    renewal: tokio::task::JoinHandle<()>,
}

type HeldSessionRegistry = HashMap<String, Arc<HeldSessionHandle>>;

fn registry() -> &'static Mutex<HeldSessionRegistry> {
    static REGISTRY: OnceLock<Mutex<HeldSessionRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Look up an already-held session by tmux session name, if any
/// `HeldSessionNode` invocation in this process has already acquired one.
#[must_use]
fn lookup(session_name: &str) -> Option<Arc<HeldSessionHandle>> {
    registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(session_name)
        .cloned()
}

/// Register a newly acquired held session's handle. Only the FIRST caller
/// for a given `session_name` should reach this — every later call for the
/// same name is served by [`lookup`] instead.
fn register(session_name: String, handle: Arc<HeldSessionHandle>) {
    registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(session_name, handle);
}

/// The background loop a [`HeldSessionNode`] spawns once per session: wait
/// `policy.renew_interval_ms`, then renew the lease for another
/// `policy.lease_ttl_ms` from now. Renewing on a schedule strictly shorter
/// than the TTL (every built-in default and every profile keeps the
/// renewal interval at roughly a third of the TTL) is what keeps a held
/// session from ever looking like an orphan to `EN.9.C`'s reconciliation
/// while this loop is alive.
///
/// Stops on the first renewal failure (a foreign lease won a race, the
/// driver errored, or anything else `SessionLease::renew` reports) rather
/// than spinning against a lease this process no longer holds. Detecting
/// and surfacing that loss to an in-flight node is task 2's job — this
/// loop takes no other action here.
async fn renewal_loop(
    driver: Arc<dyn TerminalDriver>,
    session_name: String,
    run_id: String,
    nonce: String,
    policy: HeldSessionPolicy,
) {
    let renew_every = Duration::from_millis(policy.renew_interval_ms);
    loop {
        tokio::time::sleep(renew_every).await;
        let lease = SessionLease::new(driver.as_ref());
        let new_expires_at_ms = now_ms() + policy.lease_ttl_ms;
        let renewed = lease
            .renew(&session_name, &nonce, new_expires_at_ms, &run_id, NODE_NAME)
            .await;
        if renewed.is_err() {
            break;
        }
    }
}

/// Whether `session_name` already appears in `list_sessions`' raw output.
/// Mirrors `session::TerminalSessionNode::session_present` exactly —
/// malformed lines are skipped, not treated as a listing failure.
fn session_present(list_output: &str, session_name: &str) -> bool {
    list_output
        .lines()
        .filter_map(|line| parse_session_line(line).ok())
        .any(|session| session.name == session_name)
}

// ── The node ─────────────────────────────────────────────────────────────

/// Acquire a tmux session ONCE per run and carry it across node
/// boundaries: the first `HeldSessionNode` invocation for a run creates
/// the session (if absent), acquires the `EN.9.B` lease, and spawns the
/// background [`renewal_loop`]; every later invocation for the same run
/// finds that loop already running via [`registry`] and does nothing more
/// than confirm it.
pub struct HeldSessionNode {
    driver: Arc<dyn TerminalDriver>,
    /// Working directory a newly created session launches in. `None`
    /// creates the session in tmux's default directory.
    dir: Option<String>,
    /// Where to read `held_session.policy`/`.profiles` from. Defaults to
    /// [`PolicyConfigSource::Builtin`] (no filesystem access) unless
    /// [`Self::with_source`] is called.
    source: PolicyConfigSource,
}

impl HeldSessionNode {
    /// Construct with the given driver, no launch directory, and no
    /// `harness.json` policy source ([`PolicyConfigSource::Builtin`]).
    #[must_use]
    pub fn new(driver: Arc<dyn TerminalDriver>) -> Self {
        Self {
            driver,
            dir: None,
            source: PolicyConfigSource::Builtin,
        }
    }

    /// Set the working directory a newly created session launches in.
    #[must_use]
    pub fn with_dir(mut self, dir: impl Into<String>) -> Self {
        self.dir = Some(dir.into());
        self
    }

    /// Read `held_session.policy`/`.profiles` from `source` instead of the
    /// [`PolicyConfigSource::Builtin`] default.
    #[must_use]
    pub fn with_source(mut self, source: PolicyConfigSource) -> Self {
        self.source = source;
        self
    }

    /// Read this node's own `policy`/`profile` arguments off `ctx.event`,
    /// mirroring `HoldPolicyNode::read_event_args`'s convention.
    fn read_event_args(
        &self,
        ctx: &TaskContext,
    ) -> Result<(Option<PartialHeldSessionPolicy>, Option<String>), NodeError> {
        let event_override = match ctx.event.get("policy") {
            Some(value) => Some(
                serde_json::from_value::<PartialHeldSessionPolicy>(value.clone()).map_err(
                    |err| NodeError::new(format!("{NODE_NAME}: invalid ctx.event.policy: {err}")),
                )?,
            ),
            None => None,
        };
        let profile_name = ctx
            .event
            .get("profile")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        Ok((event_override, profile_name))
    }
}

#[async_trait::async_trait]
impl Node for HeldSessionNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let run_id = read_run_id(&ctx.metadata).ok_or_else(|| {
            NodeError::new(format!(
                "{NODE_NAME}: no run_id stamped on ctx.metadata — cannot derive a session name"
            ))
        })?;
        let session_name = session_name_for(&run_id, NODE_NAME);

        let (event_override, profile_name) = self.read_event_args(&ctx)?;
        let policy = resolve_for_workflow(
            WORKFLOW_KEY,
            &self.source,
            profile_name.as_deref(),
            event_override.as_ref(),
        )?;

        // Back-edge / later-node re-entry: another `HeldSessionNode` call
        // (this one or a sibling node bound to the same identity) already
        // acquired the session and its renewal loop is running. Reuse it
        // — no tmux calls, no lease acquire, no second loop.
        if let Some(existing) = lookup(&session_name) {
            put_result(
                &mut ctx,
                self.name(),
                serde_json::json!({
                    "session_name": session_name,
                    "lease_nonce": existing.nonce,
                    "created": false,
                    "acquired_by_this_call": false,
                    "lease_ttl_ms": policy.lease_ttl_ms,
                    "renew_interval_ms": policy.renew_interval_ms,
                }),
            );
            return Ok(ctx);
        }

        let listed =
            self.driver.list_sessions().await.map_err(|err| {
                NodeError::new(format!("{NODE_NAME}: list_sessions failed: {err}"))
            })?;
        let already_present = session_present(&listed, &session_name);

        if !already_present {
            self.driver
                .new_session(&session_name, self.dir.as_deref())
                .await
                .map_err(|err| NodeError::new(format!("{NODE_NAME}: new_session failed: {err}")))?;
        }

        // Deterministic nonce (the session name itself), exactly
        // `TerminalSessionNode`'s convention — see `session.rs`'s module
        // doc on why this hits the lease's same-nonce no-op-success path
        // on any future back-edge acquire rather than colliding as a
        // foreign lease.
        let nonce = session_name.clone();
        let now = now_ms();
        let lease = SessionLease::new(self.driver.as_ref());
        let acquired = lease
            .acquire(AcquireRequest {
                session_name: &session_name,
                run_id: &run_id,
                nonce: &nonce,
                identity: NODE_NAME,
                expires_at_ms: now + policy.lease_ttl_ms,
                now_ms: now,
                steal_after: None,
            })
            .await
            .map_err(|err| {
                NodeError::new(format!("{NODE_NAME}: lease acquisition failed: {err}"))
            })?;

        let renewal = tokio::spawn(renewal_loop(
            self.driver.clone(),
            session_name.clone(),
            run_id.clone(),
            acquired.nonce.clone(),
            policy,
        ));
        let handle = Arc::new(HeldSessionHandle {
            session_name: session_name.clone(),
            nonce: acquired.nonce.clone(),
            renewal,
        });
        register(session_name.clone(), handle);

        put_result(
            &mut ctx,
            self.name(),
            serde_json::json!({
                "session_name": session_name,
                "lease_nonce": acquired.nonce,
                "created": !already_present,
                "acquired_by_this_call": true,
                "lease_ttl_ms": policy.lease_ttl_ms,
                "renew_interval_ms": policy.renew_interval_ms,
            }),
        );

        Ok(ctx)
    }

    fn name(&self) -> &str {
        NODE_NAME
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use term_core::driver::{StubOutcome, StubTerminalDriver};

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

    fn list_sessions_line(session_name: &str) -> String {
        format!("{session_name}\t0\t1\t0\t\t/tmp")
    }

    /// Mirrors `session.rs::tests::seed_own_lease` exactly: points the
    /// stub's single configured `show_option` value at a live lease
    /// carrying this node's deterministic nonce, so every `acquire`/
    /// `renew` call in a test hits the "this is our own lease" path.
    fn seed_own_lease(driver: &StubTerminalDriver, run_id: &str, session_name: &str) {
        let far_future = now_ms() + Duration::from_secs(3600).as_millis() as u64;
        driver.set_show_option_result_for(
            format!("@engine_lease@{session_name}"),
            StubOutcome::Ok(format!("{run_id}:{session_name}:{NODE_NAME}:{far_future}")),
        );
    }

    /// A unique run id per test, so tests running in the same process
    /// (e.g. under `cargo test`, unlike the per-test-process `cargo
    /// nextest` this repo requires) never collide on the process-global
    /// [`registry`].
    fn unique_run_id(label: &str) -> String {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        format!("held-session-test-{label}-{n}")
    }

    #[test]
    fn default_ttl_matches_terminal_session_nodes_default_and_renewal_is_a_third_of_it() {
        let default = HeldSessionPolicy::default();
        assert_eq!(default.lease_ttl_ms, 300_000);
        assert_eq!(default.renew_interval_ms, 100_000);
        assert!(
            default.renew_interval_ms < default.lease_ttl_ms,
            "renewal must be scheduled strictly inside the TTL"
        );
    }

    #[test]
    fn resolve_with_no_overrides_returns_default() {
        assert_eq!(resolve(None, None, None), HeldSessionPolicy::default());
    }

    #[test]
    fn resolve_precedence_event_beats_profile_beats_harness_beats_default() {
        let harness = PartialHeldSessionPolicy {
            lease_ttl_ms: Some(10_000),
            renew_interval_ms: Some(3_000),
        };
        let profile = PartialHeldSessionPolicy {
            lease_ttl_ms: Some(20_000),
            renew_interval_ms: None,
        };
        let event = PartialHeldSessionPolicy {
            lease_ttl_ms: Some(30_000),
            renew_interval_ms: None,
        };

        assert_eq!(
            resolve(Some(&harness), None, None).lease_ttl_ms,
            10_000,
            "harness beats default"
        );
        assert_eq!(
            resolve(Some(&harness), Some(&profile), None).lease_ttl_ms,
            20_000,
            "profile beats harness"
        );
        assert_eq!(
            resolve(Some(&harness), Some(&profile), Some(&event)).lease_ttl_ms,
            30_000,
            "event beats profile"
        );
        // Neither `profile` nor `event` touched `renew_interval_ms`, so
        // harness's value survives through to the top.
        assert_eq!(
            resolve(Some(&harness), Some(&profile), Some(&event)).renew_interval_ms,
            3_000
        );
    }

    #[test]
    fn profiles_set_both_knobs_explicitly_and_keep_renewal_inside_ttl() {
        let default = HeldSessionPolicy::default();
        assert_eq!(
            profiles::baseline().lease_ttl_ms,
            Some(default.lease_ttl_ms)
        );
        assert_eq!(
            profiles::baseline().renew_interval_ms,
            Some(default.renew_interval_ms)
        );

        for partial in [
            profiles::baseline(),
            profiles::cheap_fast(),
            profiles::thorough(),
        ] {
            let ttl = partial
                .lease_ttl_ms
                .expect("every profile sets lease_ttl_ms");
            let renew = partial
                .renew_interval_ms
                .expect("every profile sets renew_interval_ms");
            assert!(
                renew < ttl,
                "profile {partial:?} lets renewal reach the TTL"
            );
        }

        assert!(profiles::profile_by_name("nonexistent").is_none());
    }

    #[test]
    fn baseline_profile_is_a_genuine_no_op() {
        let baseline = profiles::baseline();
        assert_eq!(
            resolve(None, Some(&baseline), None),
            resolve(None, None, None)
        );
    }

    #[tokio::test]
    async fn creates_a_session_and_stamps_acquired_by_this_call() {
        let driver = Arc::new(StubTerminalDriver::new());
        let run_id = unique_run_id("create");
        let session_name = session_name_for(&run_id, NODE_NAME);
        seed_own_lease(&driver, &run_id, &session_name);
        let node = HeldSessionNode::new(driver.clone());

        let ctx = node.process(ctx_with_run_id(&run_id)).await.unwrap();

        let calls = driver.calls();
        assert!(
            calls
                .iter()
                .any(|c| c.get(1).map(String::as_str) == Some("new-session")),
            "expected a new-session call, got {calls:?}"
        );
        let stamped = ctx.nodes.get(NODE_NAME).unwrap();
        assert_eq!(stamped["created"], serde_json::json!(true));
        assert_eq!(stamped["acquired_by_this_call"], serde_json::json!(true));
        assert_eq!(
            stamped["lease_ttl_ms"],
            serde_json::json!(HeldSessionPolicy::default().lease_ttl_ms)
        );
    }

    #[tokio::test]
    async fn two_consecutive_node_boundaries_reuse_one_session_with_the_identical_id() {
        let driver = Arc::new(StubTerminalDriver::new());
        let run_id = unique_run_id("reuse");
        let session_name = session_name_for(&run_id, NODE_NAME);
        seed_own_lease(&driver, &run_id, &session_name);
        let node = HeldSessionNode::new(driver.clone());

        // First node boundary: creates the session and starts renewing.
        let ctx1 = node.process(ctx_with_run_id(&run_id)).await.unwrap();
        let session_id_1 = ctx1.nodes[NODE_NAME]["session_name"].clone();
        let nonce_1 = ctx1.nodes[NODE_NAME]["lease_nonce"].clone();

        // Second node boundary (a fresh `process` call, as a later node in
        // the same run would make): must observe the IDENTICAL session id
        // and must not touch tmux again.
        let before = driver.calls().len();
        let ctx2 = node.process(ctx_with_run_id(&run_id)).await.unwrap();
        let after_calls = driver.calls();

        assert_eq!(ctx2.nodes[NODE_NAME]["session_name"], session_id_1);
        assert_eq!(ctx2.nodes[NODE_NAME]["lease_nonce"], nonce_1);
        assert_eq!(ctx2.nodes[NODE_NAME]["created"], serde_json::json!(false));
        assert_eq!(
            ctx2.nodes[NODE_NAME]["acquired_by_this_call"],
            serde_json::json!(false)
        );
        assert_eq!(
            after_calls.len(),
            before,
            "a reused held session must issue no further driver calls on re-entry: {after_calls:?}"
        );
    }

    #[tokio::test]
    async fn distinct_runs_never_collide_on_one_held_session() {
        let driver = Arc::new(StubTerminalDriver::new());
        let run_a = unique_run_id("distinct-a");
        let run_b = unique_run_id("distinct-b");
        seed_own_lease(&driver, &run_a, &session_name_for(&run_a, NODE_NAME));
        seed_own_lease(&driver, &run_b, &session_name_for(&run_b, NODE_NAME));
        let node = HeldSessionNode::new(driver.clone());

        let ctx_a = node.process(ctx_with_run_id(&run_a)).await.unwrap();
        let ctx_b = node.process(ctx_with_run_id(&run_b)).await.unwrap();

        assert_ne!(
            ctx_a.nodes[NODE_NAME]["session_name"],
            ctx_b.nodes[NODE_NAME]["session_name"]
        );
    }

    #[tokio::test]
    async fn renewal_loop_renews_the_lease_before_its_own_ttl_elapses() {
        let driver = Arc::new(StubTerminalDriver::new());
        let run_id = unique_run_id("renew");
        let session_name = session_name_for(&run_id, NODE_NAME);
        seed_own_lease(&driver, &run_id, &session_name);
        driver.set_list_sessions_result(StubOutcome::Ok(list_sessions_line(&session_name)));
        let node = HeldSessionNode::new(driver.clone());

        // A compressed TTL/renewal window via the event-layer override —
        // the same override surface a real caller uses.
        let mut ctx = ctx_with_run_id(&run_id);
        ctx.event = serde_json::json!({
            "policy": { "lease_ttl_ms": 200, "renew_interval_ms": 20 }
        });
        let _ = node.process(ctx).await.unwrap();

        let calls_before_wait = driver
            .calls()
            .iter()
            .filter(|c| c.get(1).map(String::as_str) == Some("set-option"))
            .count();

        // Long enough for several renewal ticks at a 20ms interval.
        tokio::time::sleep(Duration::from_millis(150)).await;

        let calls_after_wait = driver
            .calls()
            .iter()
            .filter(|c| c.get(1).map(String::as_str) == Some("set-option"))
            .count();

        assert!(
            calls_after_wait > calls_before_wait,
            "expected the background loop to renew the lease at least once \
             (before={calls_before_wait}, after={calls_after_wait})"
        );
    }
}
