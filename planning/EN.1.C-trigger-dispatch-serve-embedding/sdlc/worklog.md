# Worklog — EN.1.C-trigger-dispatch-serve-embedding

## Task 1 — PASSED (1 attempt)
What: Added a Dispatcher in crates/engine-serve/src/dispatch.rs implementing dual-registry (workflow_registry + schema_registry) dispatch keyed by workflow_type, with DispatchError::UnknownWorkflowType for unregistered types, wired into lib.rs via pub mod dispatch, with 3 passing unit tests.
Decisions: Kept Dispatcher::register generic via a boxed WorkflowFactory closure (Box<dyn Fn() -> Workflow + Send + Sync>) rather than adding a NodeRegistry-sharing convenience method, since NodeRegistry/Node trait objects aren't Clone and the task only requires register(...)/dispatch(...) resolution.; Used matches!/manual field comparison instead of assert_eq! against a dispatch() Result because engine_core::Workflow doesn't implement Debug/PartialEq (unwrap_err/assert_eq require those bounds on the Ok variant too).
Validated: gating checks (fast tripwire)

## Task 2 — PASSED (1 attempt)
What: engine-serve now has an in-memory LiveStateStore (Arc<RwLock<HashMap<RunId, TaskContext>>>) with record/get/list_active/remove, giving the local Console a no-DB-poll read path for live run state, wired into lib.rs and covered by 5 unit tests.
Decisions: Used uuid::Uuid (already a workspace dependency) as the RunId type since no RunId type exists yet in engine-core/engine-contract, and EventsRow.id is already a Uuid, keeping identity consistent with the durable row.; Added a remove() method beyond the spec's minimum (record/get/list_active) since it's a natural complement for a live-state store and is trivial to test; did not add it to any public contract beyond the module.
Validated: gating checks (fast tripwire)

## Task 3 — PASSED (1 attempt)
What: Added crates/engine-serve/src/durable.rs — an mpsc-bridged async durable-write seam (DurableHandle/spawn_durable_writer/durable_on_progress) that maps on_progress TaskContext snapshots to engine_contract::EventsRow, inserts the first (all-PENDING) snapshot per run via engine_store::insert_event and updates subsequent ones via update_event/touch, and self-skips Postgres I/O (does not fail) when no pool/DATABASE_URL is configured.
Decisions: Did not touch crates/engine-core/src/workflow.rs — no signature gap surfaced in the existing OnProgress seam, so the append-only edit there was unnecessary for this task.; Added a pure message_to_row(message, created_at, updated_at) -> EventsRow mapping function separate from the async writer so the byte-identical contract-shape assertion can be tested directly without a live Postgres connection.; Added chrono and sqlx as direct dependencies of engine-serve (workspace-pinned versions) since durable.rs needs DateTime<Utc> and PgPool/insert_event/update_event/touch types.; Used an unbounded mpsc channel and a HashSet<Uuid> of seen run ids in the background task to decide insert vs update per run, per the spec's guidance.
Validated: gating checks (fast tripwire)

## Task 4 — PASSED (1 attempt)
What: engine-serve now exposes the four-endpoint actix-web HTTP surface (POST /events/ with X-API-Key gating dispatch/live-state/durable-write, GET /health, GET /workflows, GET /workflows/{type}/graph), with the D3 decision record wired into the dispatch/live_state/durable modules from tasks 1-3.
Decisions: Built the OnProgress trait-object closure inside the web::block blocking closure rather than moving it in pre-built, since OnProgress<'a> = Box<dyn FnMut(&TaskContext) + 'a> carries no Send bound even when its captures are Send — constructing it inside the FnOnce closure keeps the outer closure Send for actix's web::block.
Validated: gating checks (fast tripwire)
