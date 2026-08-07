//! `EN.6.G` task 3 — end-to-end scheduled digest test: a stubbed clock
//! ticks a [`ScheduleRegistry`] (task 2) whose one registered entry targets
//! a `CONTENT_PIPELINE`-shaped stub graph built from `FanOutNode` /
//! `AggregateNode` (task 1) plus a `PersistToBrainNode`-shaped sink and an
//! `ActionDispatchNode`-shaped egress stub, dispatched through
//! `dispatch_scheduled_entry`'s exact non-blocking-trigger sequence
//! (`dispatch_with_event` -> mint `run_id` -> `SpawnedRun` -> `spawn_run`)
//! — no HTTP round-trip against this same server.
//!
//! Asserts a single scheduled fire produces exactly one merged
//! `PersistToBrainNode` payload (joining every fanned-out source) and
//! exactly one dispatched `OutboundAction`-shaped record.
//!
//! Stub, not the real `workflows::content_pipeline` graph: this block does
//! not touch that module (real `PersistToBrainNode` needs a live
//! `HttpPost`/harvest-gate wiring out of scope here) — only the shape
//! (one persist payload, one outbound action) is under test.

use std::collections::HashMap as StdHashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeZone, Utc};
use engine_contract::TaskContext;
use engine_core::cron::record::{CronRecord, FireOutcome};
use engine_core::cron::store::FileCronStore;
use engine_core::cron::CronSchedule;
use engine_core::{Node, NodeConfig, NodeError, NodeRegistry, Workflow, WorkflowSchema};
use engine_serve::abort::RunRegistry;
use engine_serve::dispatch::Dispatcher;
use engine_serve::durable::spawn_durable_writer;
use engine_serve::http::AppState;
use engine_serve::live_state::LiveStateStore;
use engine_serve::schedule::{
    build_seeded_registry, dispatch_scheduled_entry, spawn_schedule_loop, ScheduleEntry,
    ScheduleRegistry,
};
use serde_json::json;
use std::sync::Mutex as StdMutex;
use uuid::Uuid;

const WORKFLOW_TYPE: &str = "CONTENT_PIPELINE_FIXTURE";
const SOURCE_COUNT: usize = 3;

fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
}

/// A trivial source node, mirroring `nodes::fan_out::tests::SourceNode` —
/// one of the `SOURCE_COUNT` sources a scheduled digest fans out to.
struct SourceNode {
    value: serde_json::Value,
}

#[async_trait::async_trait]
impl Node for SourceNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.nodes
            .insert(self.name().to_string(), self.value.clone());
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "SourceNode"
    }
}

/// Stands in for `workflows::content_pipeline::PersistToBrainNode` — reads
/// the upstream `Aggregate` result and stamps exactly one merged digest
/// payload (no real Synapse POST happens in this stub, per D51).
struct PersistStubNode;

#[async_trait::async_trait]
impl Node for PersistStubNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let digest = ctx.nodes.get("Aggregate").cloned().ok_or_else(|| {
            NodeError::new("PersistStubNode: missing upstream 'Aggregate' result")
        })?;
        ctx.nodes.insert(
            self.name().to_string(),
            json!({ "posted": true, "digest": digest }),
        );
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "PersistToBrainNode"
    }
}

/// Stands in for `workflows::content_pipeline::ActionDispatchNode` — reads
/// the persisted digest and stamps exactly one dispatched
/// `OutboundAction`-shaped record.
struct ActionDispatchStubNode;

#[async_trait::async_trait]
impl Node for ActionDispatchStubNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let persisted = ctx
            .nodes
            .get("PersistToBrainNode")
            .cloned()
            .ok_or_else(|| {
                NodeError::new(
                    "ActionDispatchStubNode: missing upstream 'PersistToBrainNode' result",
                )
            })?;
        ctx.nodes.insert(
            self.name().to_string(),
            json!([{ "kind": "Digest", "sent": true, "digest": persisted }]),
        );
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "ActionDispatchNode"
    }
}

fn fixture_schema() -> WorkflowSchema {
    let mut nodes = StdHashMap::new();
    nodes.insert(
        "FanOut".to_string(),
        NodeConfig::new("FanOut", vec!["Aggregate".to_string()]),
    );
    nodes.insert(
        "Aggregate".to_string(),
        NodeConfig::new("Aggregate", vec!["PersistToBrainNode".to_string()]),
    );
    nodes.insert(
        "PersistToBrainNode".to_string(),
        NodeConfig::new("PersistToBrainNode", vec!["ActionDispatchNode".to_string()]),
    );
    nodes.insert(
        "ActionDispatchNode".to_string(),
        NodeConfig::new("ActionDispatchNode", vec![]),
    );
    WorkflowSchema::new(WORKFLOW_TYPE, "FanOut", nodes)
}

fn fixture_workflow() -> Workflow {
    use engine_core::nodes::aggregate::AggregateNode;
    use engine_core::nodes::fan_out::FanOutNode;

    let mut registry = NodeRegistry::new();
    registry.register(Box::new(FanOutNode::new(
        "FanOut",
        "Source",
        SOURCE_COUNT,
        |i| {
            Box::new(SourceNode {
                value: json!({ "i": i }),
            }) as Box<dyn Node>
        },
    )));
    registry.register(Box::new(AggregateNode::for_fan_out(
        "Aggregate",
        "Source",
        SOURCE_COUNT,
    )));
    registry.register(Box::new(PersistStubNode));
    registry.register(Box::new(ActionDispatchStubNode));

    Workflow::new(registry, fixture_schema())
}

fn test_app_state() -> AppState {
    let mut dispatcher = Dispatcher::new();
    dispatcher.register(
        fixture_schema(),
        Box::new(|_event: &serde_json::Value| Ok(fixture_workflow())),
    );

    AppState {
        dispatcher: Arc::new(dispatcher),
        live: LiveStateStore::new(),
        durable: spawn_durable_writer(None),
        runs: RunRegistry::new(),
        api_key: "schedule-test-key".to_string(),
    }
}

fn schedule_entry() -> ScheduleEntry {
    ScheduleEntry {
        cron_id: "daily-digest".to_string(),
        schedule: CronSchedule::Interval {
            every_ms: 60_000,
            first_fire_at: utc(2026, 1, 1, 0, 0, 0),
        },
        workflow_type: WORKFLOW_TYPE.to_string(),
        profile: Some("cheap-fast".to_string()),
        data: json!({ "source": "daily-digest" }),
        next_fire_at: Some(utc(2026, 1, 1, 0, 0, 0)),
    }
}

/// Parse the `run_id` a [`FireOutcome::Reported`] message from
/// [`dispatch_scheduled_entry`] embeds (`"dispatched run {run_id} for cron
/// {cron_id}"`) — the only channel this test has back to the minted id,
/// since `dispatch_scheduled_entry` itself has nothing else to report it
/// through (mirrors `crate::http::post_events`'s own `event_id` readback
/// pattern, just carried in a string instead of a JSON body here).
fn parse_run_id(outcome: &FireOutcome) -> Uuid {
    match outcome {
        FireOutcome::Reported(message) => message
            .split_whitespace()
            .find_map(|token| Uuid::parse_str(token).ok())
            .unwrap_or_else(|| panic!("expected a run_id in message: {message}")),
        FireOutcome::Silent => panic!("expected a Reported outcome carrying a run_id"),
    }
}

#[actix_web::test]
async fn a_single_scheduled_fire_produces_one_persist_payload_and_one_outbound_action() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(FileCronStore::open(dir.path().join("cron.json")).expect("open store"));

    let now = utc(2026, 1, 1, 0, 0, 0);
    store.upsert(CronRecord {
        id: "daily-digest".to_string(),
        schedule: CronSchedule::Interval {
            every_ms: 60_000,
            first_fire_at: now,
        },
        created_at: now,
        enabled: true,
        last_fired_at: None,
        next_fire_at: Some(now),
    });

    let mut registry = ScheduleRegistry::new(store);
    registry.register(schedule_entry());

    let state = test_app_state();

    // One tick, one fire: capture the minted `run_id` straight out of the
    // dispatch closure via a `Mutex` cell (`registry.tick`'s `dispatch`
    // parameter is `Fn`, not `FnMut`, so the cell — not a plain captured
    // `Option` — is what lets the closure record the id it observes).
    let run_id_cell: std::sync::Mutex<Option<Uuid>> = std::sync::Mutex::new(None);
    let fired = registry.tick(now, |entry| {
        let outcome = dispatch_scheduled_entry(&state, entry, now);
        *run_id_cell.lock().unwrap() = Some(parse_run_id(&outcome));
        outcome
    });
    assert_eq!(fired, 1, "exactly one scheduled entry should fire");
    let run_id = run_id_cell
        .lock()
        .unwrap()
        .expect("run_id should have been captured from the single fire");

    // Poll until the spawned run deregisters (the last action `spawn_run`
    // takes, per `crate::suspend`'s doc comment) — same pattern
    // `async_lifecycle.rs` uses for its own spawned-run readback.
    let deadline = Instant::now() + Duration::from_secs(5);
    while state.runs.get(run_id).is_some() {
        assert!(
            Instant::now() < deadline,
            "scheduled run never deregistered"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    let final_ctx = state
        .live
        .get(run_id)
        .expect("terminal run snapshot should be readable");

    let persisted = final_ctx
        .nodes
        .get("PersistToBrainNode")
        .expect("exactly one PersistToBrainNode payload should be stamped");
    let digest = persisted
        .get("digest")
        .and_then(|d| d.as_array())
        .expect("persisted payload should carry the joined digest array");
    assert_eq!(
        digest.len(),
        SOURCE_COUNT,
        "the merged digest should join every fanned-out source"
    );

    let actions = final_ctx
        .nodes
        .get("ActionDispatchNode")
        .and_then(|v| v.as_array())
        .expect("exactly one dispatched OutboundAction-shaped record should be stamped");
    assert_eq!(
        actions.len(),
        1,
        "exactly one OutboundAction should be dispatched"
    );
}

#[actix_web::test]
async fn a_disabled_scheduled_entry_dispatches_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(FileCronStore::open(dir.path().join("cron.json")).expect("open store"));

    let now = utc(2026, 1, 1, 0, 0, 0);
    store.upsert(CronRecord {
        id: "daily-digest".to_string(),
        schedule: CronSchedule::Interval {
            every_ms: 60_000,
            first_fire_at: now,
        },
        created_at: now,
        enabled: false,
        last_fired_at: None,
        next_fire_at: Some(now),
    });

    let mut registry = ScheduleRegistry::new(store);
    registry.register(schedule_entry());

    let state = test_app_state();
    let fired = registry.tick(now, |entry| dispatch_scheduled_entry(&state, entry, now));

    assert_eq!(fired, 0, "a disabled entry must never fire");
}

// -- Task 4: end-to-end proof through `spawn_schedule_loop` --------------
//
// The tests above drive `ScheduleRegistry::tick` directly. These prove the
// spawnable bootstrap (`build_seeded_registry` + `spawn_schedule_loop`,
// EN.ticket.cron-schedule-startup-wiring tasks 2/3) actually reaches
// dispatch when driven from a temp `harness.json`, not merely that a
// registry can be constructed.

/// Same shape as `test_app_state`, but the dispatcher factory also records
/// every dispatched event into a shared sink — the observation point for
/// "the spawned loop's fire actually reached `dispatch_with_event`", since
/// the loop runs in its own spawned task with no other channel back to the
/// caller. Mirrors `crate::schedule`'s own `test_app_state` fixture
/// (`crates/engine-serve/src/schedule.rs`'s `#[cfg(test)] mod tests`).
fn test_app_state_with_recorder() -> (AppState, Arc<StdMutex<Vec<serde_json::Value>>>) {
    let recorded: Arc<StdMutex<Vec<serde_json::Value>>> = Arc::new(StdMutex::new(Vec::new()));
    let recorded_for_factory = recorded.clone();

    let mut dispatcher = Dispatcher::new();
    dispatcher.register(
        fixture_schema(),
        Box::new(move |event: &serde_json::Value| {
            recorded_for_factory
                .lock()
                .expect("recorded lock poisoned")
                .push(event.clone());
            Ok(fixture_workflow())
        }),
    );

    let state = AppState {
        dispatcher: Arc::new(dispatcher),
        live: LiveStateStore::new(),
        durable: spawn_durable_writer(None),
        runs: RunRegistry::new(),
        api_key: "schedule-loop-test-key".to_string(),
    };
    (state, recorded)
}

/// Write a temp `harness.json` with the given `schedule` block verbatim —
/// the top-level shape [`build_seeded_registry`] /
/// [`spawn_schedule_loop`] read (`schedule.entries`, `schedule.poll_interval_ms`,
/// `schedule.store_path`).
fn write_harness(dir: &std::path::Path, schedule: serde_json::Value) -> std::path::PathBuf {
    let harness_path = dir.join("harness.json");
    let harness = json!({ "schedule": schedule });
    std::fs::write(&harness_path, harness.to_string()).expect("write harness.json");
    harness_path
}

#[actix_web::test]
async fn spawn_schedule_loop_drives_a_configured_entry_to_an_observed_dispatch() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store_path = dir.path().join("cron-store.json");
    // A calendar entry firing every second (well under the interval
    // schedule's 60_000ms floor, and calendar schedules carry no such
    // floor) so the test does not need to wait a full minute for the
    // load-time interval anchor (`now + every_ms`) to become due.
    let harness_path = write_harness(
        dir.path(),
        json!({
            "entries": [
                {
                    "cron_id": "loop-fires",
                    "cron_expr": "* * * * * *",
                    "timezone": "UTC",
                    "workflow_type": WORKFLOW_TYPE,
                    "profile": "cheap-fast",
                    "data": { "source": "loop-test" }
                }
            ],
            "poll_interval_ms": 50,
            "store_path": store_path.to_string_lossy()
        }),
    );

    let (state, recorded) = test_app_state_with_recorder();
    let state = Arc::new(state);

    let handle = spawn_schedule_loop(&harness_path, state.clone())
        .expect("spawn_schedule_loop should succeed")
        .expect("a configured entry should spawn a loop");

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if !recorded.lock().expect("recorded lock poisoned").is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "spawned loop never dispatched the configured entry"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    handle.abort();

    let events = recorded.lock().expect("recorded lock poisoned").clone();
    assert!(
        !events.is_empty(),
        "the spawned loop should have dispatched at least one fire"
    );
    assert_eq!(
        events[0]["envelope"]["channel_type"],
        json!("schedule"),
        "the dispatched event should carry the Schedule-channel envelope"
    );
}

#[actix_web::test]
async fn spawn_schedule_loop_with_zero_entries_spawns_nothing_and_dispatches_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store_path = dir.path().join("cron-store.json");
    let harness_path = write_harness(
        dir.path(),
        json!({
            "entries": [],
            "poll_interval_ms": 20,
            "store_path": store_path.to_string_lossy()
        }),
    );

    let (state, recorded) = test_app_state_with_recorder();
    let state = Arc::new(state);

    let handle = spawn_schedule_loop(&harness_path, state)
        .expect("spawn_schedule_loop should succeed with zero entries");
    assert!(
        handle.is_none(),
        "zero configured entries must spawn no loop"
    );

    // Give a would-be loop time to have woken up at least once, to prove
    // the negative rather than just the absence of a handle.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        recorded.lock().expect("recorded lock poisoned").is_empty(),
        "zero configured entries must never dispatch"
    );
}

#[actix_web::test]
async fn build_seeded_registry_reseed_after_a_real_fire_does_not_immediately_refire() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store_path = dir.path().join("cron-store.json");
    let harness_path = write_harness(
        dir.path(),
        json!({
            "entries": [
                {
                    "cron_id": "restart-entry",
                    "every_ms": 3_600_000,
                    "workflow_type": WORKFLOW_TYPE
                }
            ],
            "store_path": store_path.to_string_lossy()
        }),
    );

    // First boot: seed the store, then force the record due right now (an
    // hourly entry would not naturally be due within the test) and fire it
    // for real through `registry.tick` + `dispatch_scheduled_entry` — a
    // real fire, not a hand-built `CronFireLogEntry`.
    let first = build_seeded_registry(&harness_path, &store_path).expect("first seed");
    let entry = first
        .get("restart-entry")
        .expect("entry should be registered")
        .clone();
    let due_store = FileCronStore::open(&store_path).expect("reopen store");
    let now = Utc::now();
    due_store.upsert(CronRecord {
        id: "restart-entry".to_string(),
        schedule: entry.schedule.clone(),
        created_at: now,
        enabled: true,
        last_fired_at: None,
        next_fire_at: Some(now),
    });

    let mut manual_registry = ScheduleRegistry::new(Arc::new(due_store));
    manual_registry.register(entry);
    let (state, recorded) = test_app_state_with_recorder();
    let fired = manual_registry.tick(now, |e| dispatch_scheduled_entry(&state, e, now));
    assert_eq!(fired, 1, "the forced-due entry should fire exactly once");
    assert_eq!(
        recorded.lock().expect("recorded lock poisoned").len(),
        1,
        "the fire should have reached dispatch"
    );

    // "Restart": re-seed from the same harness.json + store_path. The
    // record's post-fire `next_fire_at` (now + 1h, advanced by the fire
    // above) must survive, not be reset to a fresh load-time anchor.
    let second = build_seeded_registry(&harness_path, &store_path).expect("re-seed after fire");
    let record = second
        .store()
        .get("restart-entry")
        .expect("record should still exist after restart");

    assert!(
        record.last_fired_at.is_some(),
        "restart must preserve firing history"
    );
    assert!(
        record.next_fire_at.expect("next_fire_at should be set") > now,
        "restart must not immediately re-fire an entry that just fired"
    );

    // And driving a tick at the same `now` again must not fire it a
    // second time.
    let refired = second.tick(now, |e| dispatch_scheduled_entry(&state, e, now));
    assert_eq!(
        refired, 0,
        "a re-seeded registry must not immediately re-fire an entry that already fired"
    );
}
