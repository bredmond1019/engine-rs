//! In-memory live run-state store (D42): the local Console reads run state
//! directly from this store, with **no Postgres query** on the hot path.
//!
//! `LiveStateStore` keeps the latest `TaskContext` snapshot per run id in a
//! shared, lock-guarded map. `record` is meant to be called from inside an
//! `on_progress` closure at every node boundary (a cheap clone of the
//! snapshot); `get`/`list_active` are the local read API.
//!
//! Once a run finishes, `mark_terminal` moves it out of the live map and
//! into a bounded ring of the most recent [`COMPLETED_RUN_RETENTION`]
//! completed runs, which is what `GET /events/{event_id}` (EN.5.F) serves a
//! terminal readback from. Live runs are never evicted by the completed-run
//! cap — only entries already marked terminal compete for the ring's slots.
//!
//! `mark_terminal` is also the **single hook point** for terminal
//! run-failure notification (`ticket-run-failure-notification`, task 3):
//! every run passes through here exactly once on its way out of the live
//! map (`is_first_terminal_transition` below is what makes "exactly once"
//! true even if a caller mistakenly called this twice for the same run id),
//! so this is where the decision (`engine_core::operator::failure::should_notify`)
//! and the renderer (`engine_core::operator::failure::render_failure_payload`)
//! get wired in, enqueuing into this store's own `engine_core::operator`
//! `OperatorQueue` (`EN.8.B`) — no new channel, no bypass of the depth
//! limit. The hook is cheap and infallible: no network call, no blocking
//! I/O, no `unwrap`/`expect`/panic path, and a resolved policy that says
//! "don't notify" (or a render/validate failure) is a silent no-op that
//! never blocks the exit path.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, RwLock};

use chrono::{DateTime, Utc};
use engine_contract::{NodeRunStatus, TaskContext};
use engine_core::operator::failure::{self, FailedNode};
use engine_core::operator::queue::{ItemSource, OperatorQueue, OperatorQueueItem};
use engine_core::operator::OperatorPayloadLimits;
use engine_core::policy::PolicyConfigSource;
use uuid::Uuid;

/// Identifies a single workflow run. Matches the `events.id` primary key
/// (contract §4) so the live in-memory snapshot and the durable row share
/// the same identity.
pub type RunId = Uuid;

/// The live map's per-run value: the latest recorded snapshot paired with
/// the wall-clock time it was recorded. See [`LiveStateStore::record`] for
/// why the timestamp lives here rather than on the public API.
type LiveEntry = (TaskContext, DateTime<Utc>);

/// Number of completed runs retained for readback once they go terminal.
/// The oldest completed run is evicted once the ring is full; live
/// (non-terminal) runs are never subject to this cap.
pub const COMPLETED_RUN_RETENTION: usize = 100;

/// A retained record for a completed run — everything the canonical
/// `GET /events/{event_id}` readback needs beyond the bare `TaskContext`
/// snapshot (contract: `{event_id, workflow_type, status, created_at,
/// updated_at, task_context}`, `status` derived from `terminal`).
#[derive(Clone, Debug, PartialEq)]
pub struct RunRecord {
    pub snapshot: TaskContext,
    pub workflow_type: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub terminal: bool,
    /// The campaign id this run belongs to, if any (`EN.11.E` task 4).
    /// `Option` — unlike `FlowInvocation`'s non-optional field (task 2) —
    /// because `RunRecord` describes EVERY run the server has seen, and the
    /// overwhelming majority (a bare `POST /events/` of any other
    /// `workflow_type`) genuinely belongs to no campaign. Absent is the
    /// honest value, not a defect. Resolved by [`read_campaign_id`] from
    /// the snapshot the store is already handed: a child `SDLC_FLOW` run
    /// carries it at `snapshot.event["campaign_id"]` (task 2's wire seam);
    /// the parent `ORCHESTRATION` run carries it in its
    /// `OrchestrationRunNode` entry in `snapshot.nodes` (task 3).
    pub campaign_id: Option<Uuid>,
}

/// One run's membership in a campaign, resolved by
/// [`LiveStateStore::list_campaign_runs`] (`EN.11.E` task 4) from either the
/// live map or the completed ring.
#[derive(Clone, Debug, PartialEq)]
pub struct CampaignRun {
    pub run_id: RunId,
    pub snapshot: TaskContext,
    /// `Some` for a completed run (from its retained [`RunRecord`]); `None`
    /// for a still-live run, which this store has no separate
    /// `workflow_type` for until it goes terminal.
    pub workflow_type: Option<String>,
    /// The deterministic ordering key [`LiveStateStore::list_campaign_runs`]
    /// sorts by: a completed run's `created_at`, or — for a still-live run,
    /// which has no `created_at` in this store — the wall-clock time its
    /// snapshot was last [`LiveStateStore::record`]ed.
    pub ordering_key: DateTime<Utc>,
    pub terminal: bool,
}

/// The result of [`LiveStateStore::list_campaign_runs`]: every run this
/// store currently knows belongs to a campaign, plus whether the completed
/// ring's bounded retention may have already evicted an earlier member.
#[derive(Clone, Debug, PartialEq)]
pub struct CampaignLookup {
    /// Matching runs from both the live map and the completed ring, sorted
    /// by [`CampaignRun::ordering_key`] ascending.
    pub runs: Vec<CampaignRun>,
    /// `true` when the completed ring was at its [`COMPLETED_RUN_RETENTION`]
    /// cap at lookup time, meaning FIFO eviction may have already dropped
    /// an earlier step of a long-running campaign — this store has no
    /// per-campaign counter, so a full ring is the honest, conservative
    /// signal that `runs` might not be the campaign's complete history.
    /// `false` proves the ring has never reached capacity, so nothing has
    /// ever been evicted from it — the ring only ever shrinks a specific
    /// entry via [`CompletedRing::insert`]'s eviction, never on its own.
    pub possibly_truncated: bool,
}

/// Bounded FIFO ring of completed-run records, keyed by run id.
#[derive(Default)]
struct CompletedRing {
    entries: HashMap<RunId, RunRecord>,
    order: VecDeque<RunId>,
}

impl CompletedRing {
    fn insert(&mut self, run_id: RunId, record: RunRecord) {
        if self.entries.insert(run_id, record).is_none() {
            self.order.push_back(run_id);
        }
        while self.order.len() > COMPLETED_RUN_RETENTION {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }
}

/// In-memory store of the latest `TaskContext` snapshot per run, plus a
/// bounded ring of completed-run records for terminal readback.
///
/// Cheap to clone (an `Arc` around each guarded map) so it can be shared
/// between the HTTP handlers, the `on_progress` recorder, and any
/// background tasks without extra synchronization.
#[derive(Clone)]
pub struct LiveStateStore {
    /// Latest snapshot per live run, paired with the wall-clock time it was
    /// last `record`ed. The timestamp is stamped internally by [`Self::record`]
    /// — `record`'s public two-argument signature is locked (see its docs),
    /// so this is an implementation detail, not a new call-site obligation.
    /// `EN.9.C` task 6's stale-run alarm ([`Self::list_live_records`]) is
    /// what this timestamp exists for: the age of a `running`/`suspended`
    /// run past its last-recorded progress.
    inner: Arc<RwLock<HashMap<RunId, LiveEntry>>>,
    completed: Arc<RwLock<CompletedRing>>,
    /// The `EN.8.B` operator queue that terminal run-failure notifications
    /// (`ticket-run-failure-notification` task 3) and the `EN.9.C` task 6
    /// stale-run alarm enqueue into — reusing the existing
    /// depth-limited/digest-tailed queue type rather than a bespoke
    /// channel. Runs under the built-in [`OperatorQueue`] default policy;
    /// not yet threaded through `harness.json` (no other producer is wired
    /// into `engine-serve` for it to share policy with today).
    operator_queue: Arc<RwLock<OperatorQueue>>,
    /// Run ids the `EN.9.C` task 6 stale-run alarm has already enqueued an
    /// `OperatorQueueItem` for. Mirrors `mark_terminal`'s
    /// `is_first_terminal_transition` dedup shape: a repeated pass over the
    /// same stuck run must enqueue nothing further, so one stuck run
    /// produces exactly one item. Cleared for a run id on `mark_terminal`
    /// (a run that has gone terminal is no longer a stale-run candidate and
    /// must not pin memory here forever).
    alarmed_runs: Arc<RwLock<HashSet<RunId>>>,
}

impl Default for LiveStateStore {
    fn default() -> Self {
        Self {
            inner: Arc::default(),
            completed: Arc::default(),
            operator_queue: Arc::new(RwLock::new(OperatorQueue::new(
                engine_core::operator::queue::OperatorQueuePolicy::default(),
            ))),
            alarmed_runs: Arc::default(),
        }
    }
}

impl LiveStateStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Read access to the `EN.8.B` operator queue that terminal
    /// run-failure notifications enqueue into (`mark_terminal`). Exposed so
    /// `engine-serve`'s transport wiring (and tests) can drain/inspect it;
    /// this store never delivers or answers items itself.
    pub fn operator_queue(&self) -> &Arc<RwLock<OperatorQueue>> {
        &self.operator_queue
    }

    /// Record the latest snapshot for `run_id`, overwriting whatever was
    /// there before. Intended to be called from within an `on_progress`
    /// closure at every node boundary.
    ///
    /// Signature and semantics are locked: `bastion`'s `GET /api/runs/{id}`
    /// projection and every existing `on_progress` recorder call this with
    /// exactly these two arguments.
    pub fn record(&self, run_id: RunId, snapshot: &TaskContext) {
        let mut guard = self
            .inner
            .write()
            .expect("live state store lock poisoned on write");
        guard.insert(run_id, (snapshot.clone(), Utc::now()));
    }

    /// Read the latest snapshot for `run_id`, if any. Serves from the live
    /// map first, then falls back to the completed ring, so it keeps
    /// returning a snapshot for a run that has since gone terminal. Touches
    /// only in-memory state — no Postgres access.
    ///
    /// Signature and semantics are locked: `bastion`'s `GET /api/runs/{id}`
    /// projection consumes this over an unpinned path dependency.
    pub fn get(&self, run_id: RunId) -> Option<TaskContext> {
        {
            let guard = self
                .inner
                .read()
                .expect("live state store lock poisoned on read");
            if let Some((snapshot, _)) = guard.get(&run_id) {
                return Some(snapshot.clone());
            }
        }
        let guard = self
            .completed
            .read()
            .expect("completed run ring lock poisoned on read");
        guard.entries.get(&run_id).map(|r| r.snapshot.clone())
    }

    /// Read the full retained record for a completed run — the snapshot
    /// plus `workflow_type`, `created_at`, `updated_at`, and the terminal
    /// flag the readback endpoint needs. `None` for a run that is still
    /// live (never marked terminal) or unknown entirely.
    pub fn get_record(&self, run_id: RunId) -> Option<RunRecord> {
        let guard = self
            .completed
            .read()
            .expect("completed run ring lock poisoned on read");
        guard.entries.get(&run_id).cloned()
    }

    /// Every run this store currently knows belongs to `campaign_id` —
    /// consulting **both** the live map and the completed ring, since a
    /// campaign's earlier steps are typically terminal while later ones are
    /// still running, and a lookup that consulted only one side would
    /// report a truthful-looking but partial campaign (`EN.11.E` task 4).
    /// Sorted by [`CampaignRun::ordering_key`] ascending so the result is
    /// stable across calls. See [`CampaignLookup::possibly_truncated`] for
    /// the completed-ring retention caveat.
    pub fn list_campaign_runs(&self, campaign_id: Uuid) -> CampaignLookup {
        let mut runs = Vec::new();

        {
            let guard = self
                .inner
                .read()
                .expect("live state store lock poisoned on read");
            for (run_id, (snapshot, recorded_at)) in guard.iter() {
                if read_campaign_id(snapshot) == Some(campaign_id) {
                    runs.push(CampaignRun {
                        run_id: *run_id,
                        snapshot: snapshot.clone(),
                        workflow_type: None,
                        ordering_key: *recorded_at,
                        terminal: false,
                    });
                }
            }
        }

        let possibly_truncated = {
            let guard = self
                .completed
                .read()
                .expect("completed run ring lock poisoned on read");
            for (run_id, record) in guard.entries.iter() {
                if record.campaign_id == Some(campaign_id) {
                    runs.push(CampaignRun {
                        run_id: *run_id,
                        snapshot: record.snapshot.clone(),
                        workflow_type: Some(record.workflow_type.clone()),
                        ordering_key: record.created_at,
                        terminal: true,
                    });
                }
            }
            guard.order.len() >= COMPLETED_RUN_RETENTION
        };

        runs.sort_by_key(|r| r.ordering_key);
        CampaignLookup {
            runs,
            possibly_truncated,
        }
    }

    /// The run ids currently tracked as **live** by the store. Terminal
    /// runs — retained in the completed ring for readback — are excluded.
    pub fn list_active(&self) -> Vec<RunId> {
        let guard = self
            .inner
            .read()
            .expect("live state store lock poisoned on read");
        guard.keys().copied().collect()
    }

    /// Remove a run's snapshot from the live map (e.g. once a run is
    /// complete and no longer needs to be served live). Does not touch the
    /// completed ring.
    pub fn remove(&self, run_id: RunId) -> Option<TaskContext> {
        let mut guard = self
            .inner
            .write()
            .expect("live state store lock poisoned on write");
        guard.remove(&run_id).map(|(snapshot, _)| snapshot)
    }

    /// Every live (non-terminal) run's current snapshot, paired with its
    /// run id and the wall-clock time it was last [`Self::record`]ed.
    /// `EN.9.C` task 6's pure stale-run decision function
    /// (`crate::orphan::stale_run_ids`) reads this — the age of a
    /// `running`/`suspended` run past its last-recorded progress is what
    /// the alarm threshold measures against.
    pub fn list_live_records(&self) -> Vec<(RunId, TaskContext, DateTime<Utc>)> {
        let guard = self
            .inner
            .read()
            .expect("live state store lock poisoned on read");
        guard
            .iter()
            .map(|(run_id, (snapshot, updated_at))| (*run_id, snapshot.clone(), *updated_at))
            .collect()
    }

    /// Record that the `EN.9.C` task 6 stale-run alarm has enqueued an item
    /// for `run_id`. Returns `true` the first time this is called for a
    /// given run id (the caller should enqueue), `false` on every
    /// subsequent call for the same run id (already alarmed — a repeated
    /// pass over the same stuck run must enqueue nothing further).
    pub fn mark_alarmed(&self, run_id: RunId) -> bool {
        let mut guard = self
            .alarmed_runs
            .write()
            .expect("alarmed-runs lock poisoned on write");
        guard.insert(run_id)
    }

    /// Mark a run terminal: move it out of the live map (if present) and
    /// into the bounded completed ring, retaining `snapshot`,
    /// `workflow_type`, `created_at`, and `updated_at` for readback. Meant
    /// to be called by the spawned run on every exit path — success, node
    /// error, cancellation, and budget halt.
    ///
    /// Evicts the oldest completed run once the ring exceeds
    /// [`COMPLETED_RUN_RETENTION`]; live runs are never evicted by this cap.
    ///
    /// This is also the single hook point for terminal run-failure
    /// notification (`ticket-run-failure-notification` task 3, see the
    /// module docs): the first time a given `run_id` transitions to
    /// terminal here, [`Self::maybe_enqueue_failure_notification`] runs
    /// once. `is_first_terminal_transition` is what makes "once per run"
    /// hold even if a caller mistakenly invoked this twice for the same
    /// run id — asserted directly by this module's tests rather than
    /// assumed from "every exit path calls this once".
    pub fn mark_terminal(
        &self,
        run_id: RunId,
        snapshot: &TaskContext,
        workflow_type: impl Into<String>,
        created_at: DateTime<Utc>,
        updated_at: DateTime<Utc>,
    ) {
        let workflow_type = workflow_type.into();
        let record = RunRecord {
            snapshot: snapshot.clone(),
            workflow_type: workflow_type.clone(),
            created_at,
            updated_at,
            terminal: true,
            campaign_id: read_campaign_id(snapshot),
        };
        // Insert into the completed ring *before* removing from the live
        // map: `inner` and `completed` are separate locks, so a `get`/
        // `get_record` landing between the two operations must never find
        // the run in neither — briefly present in both is fine, absent from
        // both is a spurious "unknown run".
        let is_first_terminal_transition = {
            let mut completed = self
                .completed
                .write()
                .expect("completed run ring lock poisoned on write");
            let already_terminal = completed.entries.contains_key(&run_id);
            completed.insert(run_id, record);
            !already_terminal
        };
        {
            let mut live = self
                .inner
                .write()
                .expect("live state store lock poisoned on write");
            live.remove(&run_id);
        }
        {
            // A terminal run is no longer a stale-run candidate; drop it
            // from the alarmed-runs set so it does not pin memory forever.
            let mut alarmed = self
                .alarmed_runs
                .write()
                .expect("alarmed-runs lock poisoned on write");
            alarmed.remove(&run_id);
        }
        if is_first_terminal_transition {
            self.maybe_enqueue_failure_notification(run_id, snapshot, &workflow_type);
        }
    }

    /// The decision (task 1) + renderer (task 2) hook: resolve whether
    /// `snapshot`'s terminal status should notify, and if so, render and
    /// enqueue exactly one [`OperatorQueueItem`] into this store's
    /// [`Self::operator_queue`]. A no-op — never a panic, never blocking
    /// I/O — for `cancelled`/`succeeded` runs, for an unconfigured/default
    /// policy that says don't notify, and for the one residual error path
    /// ([`failure::render_failure_payload`]'s own validation failure, e.g.
    /// caller-supplied limits too small to hold even the fixed header):
    /// per the spec's Notes, an unreportable failure has no useful
    /// fallback, but it must still never block this hot exit path, so it
    /// is treated as "nothing to enqueue" rather than propagated.
    ///
    /// Policy resolves from [`PolicyConfigSource::Builtin`] with no
    /// profile/event override — the built-in default (`failed` and
    /// `budget_halted` notify) — since `mark_terminal` has no per-run
    /// `Policy` surface plumbed through it today; that resolution needs no
    /// filesystem access, keeping this path cheap.
    fn maybe_enqueue_failure_notification(
        &self,
        run_id: RunId,
        snapshot: &TaskContext,
        workflow_type: &str,
    ) {
        let terminal_status = crate::http::derive_terminal_status(snapshot);

        let policy = failure::resolve_policy_for_run_from(&PolicyConfigSource::Builtin, None, None)
            .unwrap_or_default();

        if !failure::should_notify(terminal_status, &policy) {
            return;
        }

        let failed_nodes = collect_failed_nodes(snapshot);
        let limits = OperatorPayloadLimits::default();
        let gate_id = format!("run-failure:{run_id}");

        let Ok(validated) = failure::render_failure_payload(
            gate_id.clone(),
            &run_id.to_string(),
            workflow_type,
            terminal_status,
            &failed_nodes,
            &limits,
        ) else {
            return;
        };

        let item = OperatorQueueItem::new(
            gate_id,
            validated.into_payload(),
            policy.failure_item_priority,
            Utc::now(),
            ItemSource::GateApproval,
        );

        let mut queue = self
            .operator_queue
            .write()
            .expect("operator queue lock poisoned on write");
        queue.enqueue(item);
    }
}

/// Collect every `node_runs` entry stamped [`NodeRunStatus::Failed`] into
/// [`FailedNode`]s, sorted by node name ascending. `TaskContext::node_runs`
/// is a `HashMap`, whose iteration order is not guaranteed —
/// `failure::render_failure_payload` names `failed_nodes[0]` as *the*
/// failing node, so which node is "first" must not depend on incidental
/// `HashMap` iteration order. In practice a failed walk halts after its
/// first failing node (`suspend.rs::failed_node_reason`'s doc comment), so
/// this returns at most one entry for most real runs; the sort exists for
/// the pathological/test case of several `Failed` entries in one snapshot.
fn collect_failed_nodes(snapshot: &TaskContext) -> Vec<FailedNode> {
    let mut failed: Vec<(&String, &engine_contract::NodeRun)> = snapshot
        .node_runs
        .iter()
        .filter(|(_, run)| run.status == NodeRunStatus::Failed)
        .collect();
    failed.sort_by_key(|(name, _)| (*name).clone());
    failed
        .into_iter()
        .map(|(name, run)| {
            let error = run
                .error
                .clone()
                .unwrap_or_else(|| "unknown error".to_string());
            FailedNode::new(name.clone(), error)
        })
        .collect()
}

/// Resolve the campaign id a snapshot belongs to, if any (`EN.11.E` task 4).
///
/// Checked in order:
/// 1. `snapshot.event["campaign_id"]` — a child `SDLC_FLOW` run's wire seam
///    (task 2), written by `execute::sdlc_flow_event`.
/// 2. `snapshot.nodes["OrchestrationRunNode"]["campaign_id"]` — the parent
///    `ORCHESTRATION` run's own node result (task 3).
///
/// A non-string or unparsable value reads as `None` and never panics —
/// mirrors `engine_core::workflow::read_run_id`'s defensive shape exactly.
fn read_campaign_id(snapshot: &TaskContext) -> Option<Uuid> {
    parse_campaign_id_value(snapshot.event.get("campaign_id")).or_else(|| {
        snapshot
            .nodes
            .get(engine_core::workflows::orchestration::graph::NODE_NAME)
            .and_then(|node| parse_campaign_id_value(node.get("campaign_id")))
    })
}

/// Parse a single `campaign_id` JSON value into a `Uuid`. `None` for a
/// missing key, a non-string value, or a string that fails to parse as a
/// UUID — never panics.
fn parse_campaign_id_value(value: Option<&serde_json::Value>) -> Option<Uuid> {
    value
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap as StdHashMap;

    fn fixture_context(marker: &str) -> TaskContext {
        TaskContext {
            event: serde_json::json!({ "marker": marker }),
            nodes: StdHashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: StdHashMap::new(),
        }
    }

    #[test]
    fn record_then_get_returns_latest_state() {
        let store = LiveStateStore::new();
        let run_id = Uuid::new_v4();
        let snapshot = fixture_context("first");

        store.record(run_id, &snapshot);

        assert_eq!(store.get(run_id), Some(snapshot));
    }

    #[test]
    fn later_snapshot_overwrites_earlier_one() {
        let store = LiveStateStore::new();
        let run_id = Uuid::new_v4();

        store.record(run_id, &fixture_context("first"));
        store.record(run_id, &fixture_context("second"));

        let latest = store.get(run_id).expect("snapshot should be present");
        assert_eq!(latest.event, serde_json::json!({ "marker": "second" }));
    }

    #[test]
    fn get_on_unknown_run_returns_none() {
        let store = LiveStateStore::new();

        assert_eq!(store.get(Uuid::new_v4()), None);
    }

    #[test]
    fn list_active_reflects_recorded_runs_with_no_database() {
        // This test exercises only in-memory reads/writes: no DATABASE_URL,
        // no engine-store, no sqlx pool involved anywhere in this module —
        // the local read path never touches Postgres.
        let store = LiveStateStore::new();
        let run_a = Uuid::new_v4();
        let run_b = Uuid::new_v4();

        store.record(run_a, &fixture_context("a"));
        store.record(run_b, &fixture_context("b"));

        let mut active = store.list_active();
        active.sort();
        let mut expected = vec![run_a, run_b];
        expected.sort();
        assert_eq!(active, expected);
    }

    #[test]
    fn remove_drops_a_run_from_the_store() {
        let store = LiveStateStore::new();
        let run_id = Uuid::new_v4();
        store.record(run_id, &fixture_context("gone-soon"));

        let removed = store.remove(run_id);

        assert_eq!(removed, Some(fixture_context("gone-soon")));
        assert_eq!(store.get(run_id), None);
    }

    #[test]
    fn terminal_run_is_readable_via_both_accessors() {
        let store = LiveStateStore::new();
        let run_id = Uuid::new_v4();
        let snapshot = fixture_context("done");
        let created_at = Utc::now();
        let updated_at = created_at + chrono::Duration::seconds(5);

        store.record(run_id, &snapshot);
        store.mark_terminal(
            run_id,
            &snapshot,
            "example_workflow",
            created_at,
            updated_at,
        );

        assert_eq!(store.get(run_id), Some(snapshot.clone()));

        let record = store
            .get_record(run_id)
            .expect("terminal run should have a retained record");
        assert_eq!(record.snapshot, snapshot);
        assert_eq!(record.workflow_type, "example_workflow");
        assert_eq!(record.created_at, created_at);
        assert_eq!(record.updated_at, updated_at);
        assert!(record.terminal);
    }

    #[test]
    fn the_101st_completed_run_evicts_the_oldest() {
        let store = LiveStateStore::new();
        let now = Utc::now();
        let mut run_ids = Vec::new();

        for i in 0..(COMPLETED_RUN_RETENTION + 1) {
            let run_id = Uuid::new_v4();
            let snapshot = fixture_context(&format!("run-{i}"));
            store.mark_terminal(run_id, &snapshot, "wf", now, now);
            run_ids.push(run_id);
        }

        let oldest = run_ids[0];
        let newest = *run_ids.last().unwrap();

        assert_eq!(
            store.get(oldest),
            None,
            "oldest completed run should be evicted"
        );
        assert!(store.get_record(oldest).is_none());
        assert!(
            store.get(newest).is_some(),
            "newest completed run should remain"
        );
    }

    #[test]
    fn a_live_run_is_never_evicted_by_the_completed_cap() {
        let store = LiveStateStore::new();
        let live_run = Uuid::new_v4();
        store.record(live_run, &fixture_context("still-running"));

        let now = Utc::now();
        for i in 0..(COMPLETED_RUN_RETENTION + 10) {
            let run_id = Uuid::new_v4();
            let snapshot = fixture_context(&format!("completed-{i}"));
            store.mark_terminal(run_id, &snapshot, "wf", now, now);
        }

        assert_eq!(
            store.get(live_run),
            Some(fixture_context("still-running")),
            "a live run must never be evicted by the completed-run cap"
        );
        assert!(store.list_active().contains(&live_run));
    }

    #[test]
    fn a_run_is_never_absent_from_both_maps_during_mark_terminal() {
        // Regression test: `mark_terminal` used to remove the run from the
        // live map before inserting it into the completed ring, so a
        // `get`/`get_record` landing between the two separate lock
        // acquisitions could find the run in neither. Hammer the
        // transition from a concurrent reader, across many iterations, to
        // catch a reordering regression.
        for _ in 0..500 {
            let store = LiveStateStore::new();
            let run_id = Uuid::new_v4();
            let snapshot = fixture_context("racing");
            store.record(run_id, &snapshot);

            let reader_store = store.clone();
            let missing = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let reader_missing = missing.clone();
            let reader = std::thread::spawn(move || {
                for _ in 0..2000 {
                    if reader_store.get(run_id).is_none() {
                        reader_missing.store(true, std::sync::atomic::Ordering::SeqCst);
                        break;
                    }
                }
            });

            let now = Utc::now();
            store.mark_terminal(run_id, &snapshot, "wf", now, now);
            reader.join().expect("reader thread should not panic");

            assert!(
                !missing.load(std::sync::atomic::Ordering::SeqCst),
                "run vanished from both the live map and the completed ring mid-transition"
            );
        }
    }

    #[test]
    fn list_active_excludes_terminal_runs() {
        let store = LiveStateStore::new();
        let live_run = Uuid::new_v4();
        let terminal_run = Uuid::new_v4();

        store.record(live_run, &fixture_context("live"));
        store.record(terminal_run, &fixture_context("about-to-finish"));

        let now = Utc::now();
        store.mark_terminal(
            terminal_run,
            &fixture_context("about-to-finish"),
            "wf",
            now,
            now,
        );

        let active = store.list_active();
        assert!(active.contains(&live_run));
        assert!(!active.contains(&terminal_run));
    }

    // --- ticket-run-failure-notification, task 3 ------------------------

    fn failed_node_run(error: &str) -> engine_contract::NodeRun {
        engine_contract::NodeRun {
            status: NodeRunStatus::Failed,
            started_at: None,
            completed_at: None,
            error: Some(error.to_string()),
            input: None,
            usage: None,
        }
    }

    fn succeeded_context() -> TaskContext {
        fixture_context("succeeded")
    }

    fn cancelled_context() -> TaskContext {
        let mut ctx = fixture_context("cancelled");
        ctx.metadata = serde_json::json!({ "cancellation": { "cancelled": true } });
        ctx
    }

    fn budget_halted_context() -> TaskContext {
        let mut ctx = fixture_context("budget_halted");
        ctx.metadata = serde_json::json!({ "budget": { "halted": true } });
        ctx
    }

    fn failed_context(failed_node_names: &[&str]) -> TaskContext {
        let mut ctx = fixture_context("failed");
        let mut node_runs = StdHashMap::new();
        for name in failed_node_names {
            node_runs.insert(
                (*name).to_string(),
                failed_node_run(&format!("{name} exploded")),
            );
        }
        ctx.node_runs = node_runs;
        ctx
    }

    #[test]
    fn three_failed_nodes_produce_exactly_one_enqueued_item() {
        let store = LiveStateStore::new();
        let run_id = Uuid::new_v4();
        let snapshot = failed_context(&["NodeA", "NodeB", "NodeC"]);
        let now = Utc::now();

        store.mark_terminal(run_id, &snapshot, "wf", now, now);

        let queue = store.operator_queue();
        let mut guard = queue.write().expect("operator queue lock");
        assert_eq!(guard.pending_count(), 1);
        let delivered = guard
            .next_deliverable(now)
            .expect("exactly one deliverable item");
        assert_eq!(guard.pending_count(), 0);
        assert!(guard.next_deliverable(now).is_none());
        assert!(
            delivered.payload.rendered_summary.contains("NodeA"),
            "should name the first failed node: {}",
            delivered.payload.rendered_summary
        );
    }

    #[test]
    fn cancelled_run_enqueues_nothing() {
        let store = LiveStateStore::new();
        let run_id = Uuid::new_v4();
        let snapshot = cancelled_context();
        let now = Utc::now();

        store.mark_terminal(run_id, &snapshot, "wf", now, now);

        let queue = store.operator_queue();
        assert_eq!(queue.read().expect("lock").pending_count(), 0);
    }

    #[test]
    fn succeeded_run_enqueues_nothing() {
        let store = LiveStateStore::new();
        let run_id = Uuid::new_v4();
        let snapshot = succeeded_context();
        let now = Utc::now();

        store.mark_terminal(run_id, &snapshot, "wf", now, now);

        let queue = store.operator_queue();
        assert_eq!(queue.read().expect("lock").pending_count(), 0);
    }

    #[test]
    fn budget_halted_run_enqueues_exactly_one_item() {
        let store = LiveStateStore::new();
        let run_id = Uuid::new_v4();
        let snapshot = budget_halted_context();
        let now = Utc::now();

        store.mark_terminal(run_id, &snapshot, "wf", now, now);

        let queue = store.operator_queue();
        assert_eq!(queue.read().expect("lock").pending_count(), 1);
    }

    #[test]
    fn budget_halted_and_failed_summaries_are_distinguishable() {
        let store_a = LiveStateStore::new();
        let store_b = LiveStateStore::new();
        let now = Utc::now();

        store_a.mark_terminal(Uuid::new_v4(), &budget_halted_context(), "wf", now, now);
        store_b.mark_terminal(Uuid::new_v4(), &failed_context(&["NodeA"]), "wf", now, now);

        let mut guard_a = store_a.operator_queue().write().expect("lock");
        let mut guard_b = store_b.operator_queue().write().expect("lock");
        let budget_summary = guard_a
            .next_deliverable(now)
            .expect("budget item")
            .payload
            .rendered_summary;
        let failed_summary = guard_b
            .next_deliverable(now)
            .expect("failed item")
            .payload
            .rendered_summary;
        assert_ne!(budget_summary, failed_summary);
    }

    #[test]
    fn calling_mark_terminal_twice_for_the_same_run_enqueues_only_once() {
        let store = LiveStateStore::new();
        let run_id = Uuid::new_v4();
        let snapshot = failed_context(&["NodeA"]);
        let now = Utc::now();

        store.mark_terminal(run_id, &snapshot, "wf", now, now);
        store.mark_terminal(run_id, &snapshot, "wf", now, now);

        let queue = store.operator_queue();
        assert_eq!(
            queue.read().expect("lock").pending_count(),
            1,
            "the second mark_terminal call for the same run id must not enqueue again"
        );
    }

    #[test]
    fn many_failed_runs_produce_only_one_open_deliverable_at_a_time() {
        // The burst case at this module's level: `OperatorQueue`'s own
        // depth limit (default 1) is what a caller reusing this store's
        // queue relies on -- this store never bypasses it.
        let store = LiveStateStore::new();
        let now = Utc::now();

        for i in 0..20 {
            let run_id = Uuid::new_v4();
            let snapshot = failed_context(&[&format!("Node{i}")]);
            store.mark_terminal(run_id, &snapshot, "wf", now, now);
        }

        let queue = store.operator_queue();
        assert_eq!(queue.read().expect("lock").pending_count(), 20);
        let mut guard = queue.write().expect("lock");
        assert!(guard.next_deliverable(now).is_some());
        // Depth 1 (the default `OperatorQueuePolicy`) is now occupied.
        assert!(guard.next_deliverable(now).is_none());
    }

    #[test]
    fn terminal_status_reported_by_derive_terminal_status_is_unaffected_by_the_hook() {
        // Regression assertion (spec Task 3): the notification hook must
        // never change what `derive_terminal_status` reports for the same
        // snapshot -- it only reads that status, never mutates the
        // snapshot or the completed record around it.
        let store = LiveStateStore::new();
        let run_id = Uuid::new_v4();
        let snapshot = failed_context(&["NodeA"]);
        let now = Utc::now();

        let expected_status = crate::http::derive_terminal_status(&snapshot);
        store.mark_terminal(run_id, &snapshot, "wf", now, now);
        let record = store
            .get_record(run_id)
            .expect("terminal run should have a retained record");
        let actual_status = crate::http::derive_terminal_status(&record.snapshot);

        assert_eq!(expected_status, actual_status);
        assert_eq!(expected_status, "failed");
    }

    // --- EN.11.E task 4: campaign id on RunRecord -----------------------

    fn sdlc_flow_context_with_campaign(marker: &str, campaign_id: Uuid) -> TaskContext {
        let mut ctx = fixture_context(marker);
        ctx.event = serde_json::json!({ "campaign_id": campaign_id.to_string() });
        ctx
    }

    fn orchestration_context_with_campaign(marker: &str, campaign_id: Uuid) -> TaskContext {
        let mut ctx = fixture_context(marker);
        ctx.nodes.insert(
            engine_core::workflows::orchestration::graph::NODE_NAME.to_string(),
            serde_json::json!({ "campaign_id": campaign_id.to_string() }),
        );
        ctx
    }

    #[test]
    fn campaign_id_resolves_from_a_child_sdlc_flow_events_wire_seam() {
        let store = LiveStateStore::new();
        let run_id = Uuid::new_v4();
        let campaign_id = Uuid::new_v4();
        let snapshot = sdlc_flow_context_with_campaign("child", campaign_id);
        let now = Utc::now();

        store.mark_terminal(run_id, &snapshot, "sdlc_flow", now, now);

        let record = store.get_record(run_id).expect("record present");
        assert_eq!(record.campaign_id, Some(campaign_id));
    }

    #[test]
    fn campaign_id_resolves_from_the_parent_orchestration_run_nodes_entry() {
        let store = LiveStateStore::new();
        let run_id = Uuid::new_v4();
        let campaign_id = Uuid::new_v4();
        let snapshot = orchestration_context_with_campaign("parent", campaign_id);
        let now = Utc::now();

        store.mark_terminal(run_id, &snapshot, "orchestration", now, now);

        let record = store.get_record(run_id).expect("record present");
        assert_eq!(record.campaign_id, Some(campaign_id));
    }

    #[test]
    fn a_run_with_no_campaign_id_reads_as_none_and_never_panics() {
        let store = LiveStateStore::new();
        let run_id = Uuid::new_v4();
        let snapshot = fixture_context("no-campaign");
        let now = Utc::now();

        store.mark_terminal(run_id, &snapshot, "wf", now, now);

        let record = store.get_record(run_id).expect("record present");
        assert_eq!(record.campaign_id, None);
    }

    #[test]
    fn a_malformed_campaign_id_reads_as_none_and_never_panics() {
        let store = LiveStateStore::new();
        let run_id = Uuid::new_v4();
        let mut snapshot = fixture_context("bad-campaign");
        snapshot.event = serde_json::json!({ "campaign_id": "not-a-uuid" });
        let now = Utc::now();

        store.mark_terminal(run_id, &snapshot, "wf", now, now);

        let record = store.get_record(run_id).expect("record present");
        assert_eq!(record.campaign_id, None);
    }

    #[test]
    fn a_non_string_campaign_id_reads_as_none_and_never_panics() {
        let store = LiveStateStore::new();
        let run_id = Uuid::new_v4();
        let mut snapshot = fixture_context("numeric-campaign");
        snapshot.event = serde_json::json!({ "campaign_id": 12345 });
        let now = Utc::now();

        store.mark_terminal(run_id, &snapshot, "wf", now, now);

        let record = store.get_record(run_id).expect("record present");
        assert_eq!(record.campaign_id, None);
    }

    #[test]
    fn campaign_lookup_returns_runs_from_both_the_live_map_and_the_completed_ring() {
        let store = LiveStateStore::new();
        let campaign_id = Uuid::new_v4();

        let live_run = Uuid::new_v4();
        store.record(
            live_run,
            &sdlc_flow_context_with_campaign("still-running", campaign_id),
        );

        let done_run = Uuid::new_v4();
        let now = Utc::now();
        store.mark_terminal(
            done_run,
            &sdlc_flow_context_with_campaign("done", campaign_id),
            "sdlc_flow",
            now,
            now,
        );

        let other_campaign_run = Uuid::new_v4();
        store.mark_terminal(
            other_campaign_run,
            &sdlc_flow_context_with_campaign("other", Uuid::new_v4()),
            "sdlc_flow",
            now,
            now,
        );

        let lookup = store.list_campaign_runs(campaign_id);
        let run_ids: Vec<Uuid> = lookup.runs.iter().map(|r| r.run_id).collect();

        assert_eq!(lookup.runs.len(), 2);
        assert!(run_ids.contains(&live_run));
        assert!(run_ids.contains(&done_run));
        assert!(!run_ids.contains(&other_campaign_run));
        assert!(!lookup.possibly_truncated);
    }

    #[test]
    fn campaign_lookup_orders_results_deterministically_by_created_at() {
        let store = LiveStateStore::new();
        let campaign_id = Uuid::new_v4();
        let base = Utc::now();

        let earliest = Uuid::new_v4();
        let middle = Uuid::new_v4();
        let latest = Uuid::new_v4();

        store.mark_terminal(
            latest,
            &sdlc_flow_context_with_campaign("latest", campaign_id),
            "sdlc_flow",
            base + chrono::Duration::seconds(20),
            base + chrono::Duration::seconds(20),
        );
        store.mark_terminal(
            earliest,
            &sdlc_flow_context_with_campaign("earliest", campaign_id),
            "sdlc_flow",
            base,
            base,
        );
        store.mark_terminal(
            middle,
            &sdlc_flow_context_with_campaign("middle", campaign_id),
            "sdlc_flow",
            base + chrono::Duration::seconds(10),
            base + chrono::Duration::seconds(10),
        );

        let lookup = store.list_campaign_runs(campaign_id);
        let run_ids: Vec<Uuid> = lookup.runs.iter().map(|r| r.run_id).collect();

        assert_eq!(run_ids, vec![earliest, middle, latest]);
    }

    #[test]
    fn campaign_lookup_reports_possibly_truncated_once_the_completed_ring_is_full() {
        let store = LiveStateStore::new();
        let campaign_id = Uuid::new_v4();
        let now = Utc::now();

        // Fill the ring to capacity with unrelated runs, then add one
        // member of the campaign under test.
        for _ in 0..COMPLETED_RUN_RETENTION {
            store.mark_terminal(
                Uuid::new_v4(),
                &fixture_context("filler"),
                "wf",
                now,
                now,
            );
        }
        let member = Uuid::new_v4();
        store.mark_terminal(
            member,
            &sdlc_flow_context_with_campaign("member", campaign_id),
            "sdlc_flow",
            now,
            now,
        );

        let lookup = store.list_campaign_runs(campaign_id);

        assert_eq!(lookup.runs.len(), 1);
        assert_eq!(lookup.runs[0].run_id, member);
        assert!(
            lookup.possibly_truncated,
            "a full completed ring must be surfaced as possibly-truncated"
        );
    }

    #[test]
    fn campaign_lookup_is_not_truncated_when_the_ring_has_room_to_spare() {
        let store = LiveStateStore::new();
        let campaign_id = Uuid::new_v4();
        let now = Utc::now();

        store.mark_terminal(
            Uuid::new_v4(),
            &sdlc_flow_context_with_campaign("member", campaign_id),
            "sdlc_flow",
            now,
            now,
        );

        let lookup = store.list_campaign_runs(campaign_id);
        assert!(!lookup.possibly_truncated);
    }
}
