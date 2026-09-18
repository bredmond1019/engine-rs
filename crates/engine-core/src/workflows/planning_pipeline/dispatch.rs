//! `DispatchNode` — `PLANNING_PIPELINE`'s `dispatch` stage (`EN.19.D` task
//! 6).
//!
//! Reads the target block's authored `status` — `planning/state.json`'s
//! `tracks[].blocks[]`, keyed by `id == slug` — READ-ONLY, and refuses to
//! dispatch when it is already `in_progress` or `closed` (a named reason,
//! never a bare rejection), writing nothing in either refusal case: no HTTP
//! call is ever made, and this node never writes `state.json` itself (the
//! block record's own AC3). Otherwise it fires the existing, unmodified
//! dispatch path — `POST /events/` (`engine-serve::http::post_events`,
//! `EN.5.F`'s `202 {run_id, event_id}` contract) — over the same injectable
//! `HttpPost` seam `PersistToBrainNode`/`WorkflowTriggerDispatch` already
//! use, and returns the `run_id` the endpoint hands back.
//!
//! **Composition, never a fork.** This node never reimplements chain
//! execution, integration, PR creation, or merge — it POSTs to the exact
//! same endpoint every other in-repo trigger (`WorkflowTriggerDispatch`,
//! scheduled runs, email webhooks) already POSTs to, and stops the moment a
//! `run_id` comes back. Unlike `WorkflowTriggerDispatch` (whose loopback
//! path is fire-and-forget and discards the response body), this node reads
//! the `HttpPost` response body directly — `HttpPost::post_with_headers`
//! already returns the full JSON body, so capturing `run_id` needs no new
//! seam, just reading a field `WorkflowTriggerDispatch` was throwing away.
//!
//! **Target `workflow_type` resolution.** An explicit `workflow_type` on
//! the dispatched event (`"ORCHESTRATION"` | `"SDLC_FLOW"` | `"SDLC_TASK"`)
//! always wins. Absent that, this node resolves the block's own authored
//! `sdlc_workflow` field through
//! [`crate::workflows::orchestration::engine_kind::EngineKind::from_sdlc_workflow`]
//! — the one sanctioned `sdlc_workflow -> engine` mapping `/orchestrate`
//! already uses, reused here rather than re-implemented, so a block
//! declaring an unsupported value (or none) fails the same
//! `UnsupportedSdlcWorkflow` diagnostic `/orchestrate` would give it,
//! instead of silently defaulting to one engine or the other.
//!
//! **Root resolution** mirrors
//! `generate_tasks_for_block::GenerateTasksForBlockNode` exactly:
//! `PLANNING_PIPELINE` resolves within one target repo per run
//! (`EN.19.D`'s `out_of_scope`), so this node never re-derives a `repo`
//! slug through `RepoRegistry` — it defaults to `std::env::current_dir()`,
//! overridable via [`DispatchNode::with_target_root`] for tests.
//!
//! Leaves `SpecExistsRouterNode` (`sdlc_flow::setup`) untouched — it still
//! fires normally, inside the `SDLC_FLOW`/`SDLC_TASK` run this node
//! dispatches, when the target block has no `tasks.json` yet.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use engine_contract::TaskContext;
use serde_json::{json, Value};

use crate::node::{Node, NodeError};
use crate::nodes::channel_transport::DEFAULT_EVENTS_URL;
use crate::nodes::http_post::{http_post_live, HttpPost};
use crate::workflows::orchestration::engine_kind::EngineKind;
use crate::workflows::put_result;

/// The `Node::name()` identity this node registers under, and the
/// `ctx.nodes` key its verdict is stamped onto.
pub const NODE_NAME: &str = "DispatchNode";

/// Env var the loopback `POST /events/` call's `X-API-Key` header reads —
/// byte-identical name to `channel_transport::WorkflowTriggerDispatch`'s own
/// (private) `EVENTS_API_KEY_ENV`, so both dispatch paths pick up the same
/// deployment-configured key. Duplicated rather than imported because that
/// const is private to its module; a caller needing a different key (tests,
/// a future `engine-serve` wiring) overrides it via
/// [`DispatchNode::with_api_key`].
const EVENTS_API_KEY_ENV: &str = "ENGINE_EVENTS_API_KEY";

/// `ctx.nodes[NODE_NAME]` keys.
const REFUSED_KEY: &str = "refused";
const REASON_KEY: &str = "reason";

/// Named refusal reasons — matched by this node's own tests and by
/// `tests/it/planning_pipeline.rs` (task 9), never a bare "refused" guess.
pub mod refusal_reason {
    pub const ALREADY_IN_PROGRESS: &str = "block already in_progress";
    pub const ALREADY_CLOSED: &str = "block already closed";
    pub const BLOCK_NOT_FOUND: &str = "block not found in planning/state.json";
    pub const UNKNOWN_WORKFLOW_TYPE: &str = "unknown workflow_type override";
    pub const UNSUPPORTED_SDLC_WORKFLOW: &str =
        "block's sdlc_workflow is not in the sanctioned {task, flow} vocabulary";
    pub const MISSING_RUN_ID: &str = "dispatch response carried no run_id";
}

/// The three `workflow_type`s this node is allowed to dispatch to — an
/// explicit event-level `workflow_type` outside this set is refused rather
/// than forwarded blind.
const ALLOWED_WORKFLOW_TYPES: [&str; 3] = ["ORCHESTRATION", "SDLC_FLOW", "SDLC_TASK"];

/// Read the required string `slug` out of the dispatched event — the same
/// validation shape `generate_tasks_for_block::require_slug` and
/// `pre_plan::check_existing::require_slug` use.
fn require_slug(ctx: &TaskContext) -> Result<String, NodeError> {
    ctx.event
        .get("slug")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| NodeError::new(format!("{NODE_NAME}: missing or empty 'slug'")))
}

/// `<root>/planning/state.json` — the authored-status source of truth
/// (`docs/state/state-schema.md`: `tracks[].blocks[].status`). Reading this
/// file is the ONLY state.json access this node makes; it is never written.
fn state_json_path(root: &Path) -> PathBuf {
    root.join("planning").join("state.json")
}

fn read_state_json(root: &Path) -> Result<Value, NodeError> {
    let path = state_json_path(root);
    let contents = std::fs::read_to_string(&path).map_err(|err| {
        NodeError::new(format!(
            "{NODE_NAME}: failed to read {}: {err}",
            path.display()
        ))
    })?;
    serde_json::from_str(&contents).map_err(|err| {
        NodeError::new(format!(
            "{NODE_NAME}: failed to parse {} as JSON: {err}",
            path.display()
        ))
    })
}

/// Find the `tracks[].blocks[]` entry whose `id` equals `slug`, scanning
/// every track (a block's own track is not known ahead of time). `None`
/// when no track carries a block with that id at all.
fn find_block<'a>(state: &'a Value, slug: &str) -> Option<&'a Value> {
    state
        .get("tracks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|track| track.get("blocks"))
        .filter_map(Value::as_array)
        .flatten()
        .find(|block| block.get("id").and_then(Value::as_str) == Some(slug))
}

/// Composes `PLANNING_PIPELINE`'s `dispatch` stage: the authored-status
/// double-dispatch guard, then the existing `POST /events/` dispatch path
/// over the injectable `HttpPost` seam. See the module docs for the full
/// contract.
pub struct DispatchNode {
    http_post: Arc<dyn HttpPost>,
    events_url: String,
    /// `X-API-Key` header value sent with the dispatch POST. Defaults to
    /// [`EVENTS_API_KEY_ENV`]'s value at construction time.
    api_key: String,
    /// Overrides root resolution (defaults to `std::env::current_dir()`,
    /// byte-identical to `GenerateTasksForBlockNode::resolve_root`'s own
    /// fallback). Tests use this so nothing here ever depends on the test
    /// binary's own working directory.
    target_root: Option<PathBuf>,
}

impl DispatchNode {
    /// Construct with the live default `HttpPost` (`http_post_live()`),
    /// targeting [`DEFAULT_EVENTS_URL`], with the `X-API-Key` read from
    /// [`EVENTS_API_KEY_ENV`] (empty if unset — override with
    /// [`Self::with_api_key`]).
    #[must_use]
    pub fn new() -> Self {
        Self {
            http_post: http_post_live(),
            events_url: DEFAULT_EVENTS_URL.to_string(),
            api_key: std::env::var(EVENTS_API_KEY_ENV).unwrap_or_default(),
            target_root: None,
        }
    }

    /// Override the `HttpPost` seam. Tests inject a `StubHttpPost` so the
    /// gated suite never hits a live `/events/` endpoint.
    #[must_use]
    pub fn with_http_post(mut self, http_post: Arc<dyn HttpPost>) -> Self {
        self.http_post = http_post;
        self
    }

    /// Override the target `/events/` URL.
    #[must_use]
    pub fn with_events_url(mut self, events_url: impl Into<String>) -> Self {
        self.events_url = events_url.into();
        self
    }

    /// Override the `X-API-Key` header value sent with the dispatch POST.
    #[must_use]
    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = api_key.into();
        self
    }

    /// Override the target root this node resolves `planning/state.json`
    /// against. Absent this, resolution falls back to
    /// `std::env::current_dir()`.
    #[must_use]
    pub fn with_target_root(mut self, root: PathBuf) -> Self {
        self.target_root = Some(root);
        self
    }

    /// [`Self::target_root`] when set, else `std::env::current_dir()` —
    /// byte-identical to `GenerateTasksForBlockNode::resolve_root`'s own
    /// fallback.
    fn resolve_root(&self) -> Result<PathBuf, NodeError> {
        match &self.target_root {
            Some(root) => Ok(root.clone()),
            None => std::env::current_dir().map_err(|err| {
                NodeError::new(format!(
                    "{NODE_NAME}: failed to resolve current_dir(): {err}"
                ))
            }),
        }
    }

    /// Resolve the target `workflow_type`: an explicit `event.workflow_type`
    /// wins when present (validated against [`ALLOWED_WORKFLOW_TYPES`]);
    /// otherwise the block's own authored `sdlc_workflow` field is mapped
    /// through the sanctioned [`EngineKind::from_sdlc_workflow`].
    fn resolve_workflow_type(
        &self,
        ctx: &TaskContext,
        block: &Value,
        slug: &str,
    ) -> Result<String, NodeError> {
        if let Some(requested) = ctx.event.get("workflow_type").and_then(Value::as_str) {
            return if ALLOWED_WORKFLOW_TYPES.contains(&requested) {
                Ok(requested.to_string())
            } else {
                Err(NodeError::new(format!(
                    "{NODE_NAME}: unknown workflow_type override '{requested}' for '{slug}' — \
                     must be one of {}",
                    ALLOWED_WORKFLOW_TYPES.join(", ")
                ))
                .with_node_result(json!({
                    REFUSED_KEY: true,
                    REASON_KEY: refusal_reason::UNKNOWN_WORKFLOW_TYPE,
                    "slug": slug,
                    "workflow_type": requested,
                })))
            };
        }

        let sdlc_workflow = block.get("sdlc_workflow").and_then(Value::as_str);
        match EngineKind::from_sdlc_workflow(sdlc_workflow) {
            Ok(EngineKind::Flow) => Ok("SDLC_FLOW".to_string()),
            Ok(EngineKind::Task) => Ok("SDLC_TASK".to_string()),
            Err(err) => Err(
                NodeError::new(format!("{NODE_NAME}: {err}")).with_node_result(json!({
                    REFUSED_KEY: true,
                    REASON_KEY: refusal_reason::UNSUPPORTED_SDLC_WORKFLOW,
                    "slug": slug,
                })),
            ),
        }
    }
}

impl Default for DispatchNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for DispatchNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let slug = require_slug(&ctx)?;
        let root = self.resolve_root()?;
        let state = read_state_json(&root)?;

        let block = find_block(&state, &slug).ok_or_else(|| {
            NodeError::new(format!(
                "{NODE_NAME}: no block with id '{slug}' found in planning/state.json"
            ))
            .with_node_result(json!({
                REFUSED_KEY: true,
                REASON_KEY: refusal_reason::BLOCK_NOT_FOUND,
                "slug": slug,
            }))
        })?;

        // The double-dispatch guard: authored status is read-only here and
        // NEVER written by this node (block record AC3). Every other status
        // (`open`, `deferred`, `wontfix`, `superseded`, or absent) permits
        // dispatch.
        let status = block.get("status").and_then(Value::as_str).unwrap_or("");
        if status == "in_progress" || status == "closed" {
            let reason = if status == "in_progress" {
                refusal_reason::ALREADY_IN_PROGRESS
            } else {
                refusal_reason::ALREADY_CLOSED
            };
            return Err(NodeError::new(format!(
                "{NODE_NAME}: refusing to dispatch '{slug}' — authored status is '{status}'"
            ))
            .with_node_result(json!({
                REFUSED_KEY: true,
                REASON_KEY: reason,
                "slug": slug,
                "status": status,
            })));
        }

        let workflow_type = self.resolve_workflow_type(&ctx, block, &slug)?;

        // The existing, unmodified dispatch path's event shape: `spec_slug`
        // is what both `SDLC_FLOW`'s and `SDLC_TASK`'s own event schemas
        // require (`sdlc_flow_event`/`sdlc_task_event` in
        // `orchestration::execute`); a caller targeting `ORCHESTRATION`, or
        // needing extra fields either schema accepts, supplies them via
        // `event.dispatch_event`, merged in on top.
        let mut data = json!({ "spec_slug": slug });
        if let Some(extra) = ctx.event.get("dispatch_event").and_then(Value::as_object) {
            for (key, value) in extra {
                data[key] = value.clone();
            }
        }

        let payload = json!({
            "workflow_type": workflow_type,
            "data": data,
        });

        let response = self
            .http_post
            .post_with_headers(
                &self.events_url,
                payload,
                &[("X-API-Key", self.api_key.as_str())],
            )
            .await
            .map_err(|err| {
                NodeError::new(format!(
                    "{NODE_NAME}: dispatch POST to {} failed: {err}",
                    self.events_url
                ))
            })?;

        let run_id = response
            .body
            .get("run_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                NodeError::new(format!(
                    "{NODE_NAME}: dispatch response carried no 'run_id' field: {}",
                    response.body
                ))
                .with_node_result(json!({
                    REFUSED_KEY: true,
                    REASON_KEY: refusal_reason::MISSING_RUN_ID,
                    "slug": slug,
                    "workflow_type": workflow_type,
                }))
            })?;

        put_result(
            &mut ctx,
            NODE_NAME,
            json!({
                REFUSED_KEY: false,
                "slug": slug,
                "workflow_type": workflow_type,
                "run_id": run_id,
                "status_checked": status,
            }),
        );

        Ok(ctx)
    }

    fn name(&self) -> &str {
        NODE_NAME
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;
    use crate::nodes::http_post::StubHttpPost;
    use crate::workflows::get_result;

    static TMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    /// A named scratch directory under the OS temp dir, guaranteed EMPTY at
    /// the moment it is returned — mirrors
    /// `generate_tasks_for_block::tests::temp_dir`'s own remove-then-recreate
    /// pattern, duplicated here rather than imported since that helper is
    /// private to its module.
    fn temp_dir() -> PathBuf {
        let n = TMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "engine-core-planning-pipeline-dispatch-test-{}-{n}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn write_state_json(root: &Path, block: Value) {
        let planning_dir = root.join("planning");
        std::fs::create_dir_all(&planning_dir).unwrap();
        let state = json!({
            "tracks": [
                { "title": "Track", "blocks": [block] }
            ]
        });
        std::fs::write(
            planning_dir.join("state.json"),
            serde_json::to_string_pretty(&state).unwrap(),
        )
        .unwrap();
    }

    fn empty_context(event: Value) -> TaskContext {
        TaskContext {
            event,
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        }
    }

    fn stub_succeeding(run_id: &str) -> Arc<StubHttpPost> {
        Arc::new(StubHttpPost::succeeding(json!({
            "run_id": run_id,
            "event_id": run_id,
        })))
    }

    #[tokio::test]
    async fn missing_slug_is_a_named_error() {
        let err = DispatchNode::new()
            .process(empty_context(json!({})))
            .await
            .expect_err("missing slug rejected");
        assert!(err.message.contains("slug"));
    }

    #[tokio::test]
    async fn block_not_found_refuses_without_posting() {
        let root = temp_dir();
        std::fs::create_dir_all(root.join("planning")).unwrap();
        std::fs::write(
            root.join("planning").join("state.json"),
            json!({"tracks": []}).to_string(),
        )
        .unwrap();

        let stub = Arc::new(StubHttpPost::failing("must never be called"));
        let node = DispatchNode::new()
            .with_target_root(root)
            .with_http_post(stub.clone());

        let err = node
            .process(empty_context(json!({ "slug": "no-such-block" })))
            .await
            .expect_err("missing block record refused");

        let stamped = err.node_result.expect("reason stamped on error");
        assert_eq!(
            stamped.get(REASON_KEY).and_then(Value::as_str),
            Some(refusal_reason::BLOCK_NOT_FOUND)
        );
        assert!(stub.last_call().is_none(), "no HTTP call should be made");
    }

    #[tokio::test]
    async fn in_progress_status_refuses_without_posting() {
        let root = temp_dir();
        write_state_json(
            &root,
            json!({ "id": "EN.1.A", "status": "in_progress", "sdlc_workflow": "flow" }),
        );

        let stub = Arc::new(StubHttpPost::failing("must never be called"));
        let node = DispatchNode::new()
            .with_target_root(root)
            .with_http_post(stub.clone());

        let err = node
            .process(empty_context(json!({ "slug": "EN.1.A" })))
            .await
            .expect_err("in_progress block refused");

        let stamped = err.node_result.expect("reason stamped on error");
        assert_eq!(
            stamped.get(REASON_KEY).and_then(Value::as_str),
            Some(refusal_reason::ALREADY_IN_PROGRESS)
        );
        assert!(stub.last_call().is_none(), "no HTTP call should be made");
    }

    #[tokio::test]
    async fn closed_status_refuses_without_posting() {
        let root = temp_dir();
        write_state_json(
            &root,
            json!({ "id": "EN.1.A", "status": "closed", "sdlc_workflow": "flow" }),
        );

        let stub = Arc::new(StubHttpPost::failing("must never be called"));
        let node = DispatchNode::new()
            .with_target_root(root)
            .with_http_post(stub.clone());

        let err = node
            .process(empty_context(json!({ "slug": "EN.1.A" })))
            .await
            .expect_err("closed block refused");

        let stamped = err.node_result.expect("reason stamped on error");
        assert_eq!(
            stamped.get(REASON_KEY).and_then(Value::as_str),
            Some(refusal_reason::ALREADY_CLOSED)
        );
        assert!(stub.last_call().is_none(), "no HTTP call should be made");
    }

    #[tokio::test]
    async fn open_status_with_flow_dispatches_sdlc_flow_and_returns_run_id() {
        let root = temp_dir();
        write_state_json(
            &root,
            json!({ "id": "EN.1.A", "status": "open", "sdlc_workflow": "flow" }),
        );

        let stub = stub_succeeding("11111111-1111-1111-1111-111111111111");
        let node = DispatchNode::new()
            .with_target_root(root)
            .with_http_post(stub.clone())
            .with_events_url("http://localhost:8080/events/")
            .with_api_key("test-key");

        let ctx = node
            .process(empty_context(json!({ "slug": "EN.1.A" })))
            .await
            .expect("permitted dispatch succeeds");

        let stored = get_result(&ctx, NODE_NAME).expect("result stored");
        assert_eq!(stored.get(REFUSED_KEY), Some(&json!(false)));
        assert_eq!(stored.get("workflow_type"), Some(&json!("SDLC_FLOW")));
        assert_eq!(
            stored.get("run_id"),
            Some(&json!("11111111-1111-1111-1111-111111111111"))
        );

        let (url, body) = stub.last_call().expect("dispatch POST recorded");
        assert_eq!(url, "http://localhost:8080/events/");
        assert_eq!(body["workflow_type"], json!("SDLC_FLOW"));
        assert_eq!(body["data"]["spec_slug"], json!("EN.1.A"));

        let headers = stub.last_headers().expect("headers recorded");
        assert_eq!(
            headers,
            vec![("X-API-Key".to_string(), "test-key".to_string())]
        );
    }

    #[tokio::test]
    async fn deferred_status_with_task_dispatches_sdlc_task() {
        let root = temp_dir();
        write_state_json(
            &root,
            json!({ "id": "EN.1.A", "status": "deferred", "sdlc_workflow": "task" }),
        );

        let stub = stub_succeeding("22222222-2222-2222-2222-222222222222");
        let node = DispatchNode::new()
            .with_target_root(root)
            .with_http_post(stub.clone());

        let ctx = node
            .process(empty_context(json!({ "slug": "EN.1.A" })))
            .await
            .expect("deferred is not a refusal status");

        let stored = get_result(&ctx, NODE_NAME).expect("result stored");
        assert_eq!(stored.get("workflow_type"), Some(&json!("SDLC_TASK")));
    }

    #[tokio::test]
    async fn explicit_workflow_type_override_wins_over_sdlc_workflow() {
        let root = temp_dir();
        write_state_json(
            &root,
            json!({ "id": "EN.1.A", "status": "open", "sdlc_workflow": "flow" }),
        );

        let stub = stub_succeeding("33333333-3333-3333-3333-333333333333");
        let node = DispatchNode::new()
            .with_target_root(root)
            .with_http_post(stub.clone());

        let ctx = node
            .process(empty_context(json!({
                "slug": "EN.1.A",
                "workflow_type": "ORCHESTRATION",
                "dispatch_event": { "roadmap": "planning-command-nodes" },
            })))
            .await
            .expect("override dispatch succeeds");

        let stored = get_result(&ctx, NODE_NAME).expect("result stored");
        assert_eq!(stored.get("workflow_type"), Some(&json!("ORCHESTRATION")));

        let (_, body) = stub.last_call().expect("dispatch POST recorded");
        assert_eq!(body["workflow_type"], json!("ORCHESTRATION"));
        assert_eq!(
            body["data"]["roadmap"],
            json!("planning-command-nodes"),
            "dispatch_event fields must merge into the outgoing data payload"
        );
        assert_eq!(body["data"]["spec_slug"], json!("EN.1.A"));
    }

    #[tokio::test]
    async fn unknown_workflow_type_override_is_a_named_error() {
        let root = temp_dir();
        write_state_json(
            &root,
            json!({ "id": "EN.1.A", "status": "open", "sdlc_workflow": "flow" }),
        );

        let stub = Arc::new(StubHttpPost::failing("must never be called"));
        let node = DispatchNode::new()
            .with_target_root(root)
            .with_http_post(stub.clone());

        let err = node
            .process(empty_context(json!({
                "slug": "EN.1.A",
                "workflow_type": "NOT_A_REAL_WORKFLOW",
            })))
            .await
            .expect_err("unknown override refused");

        let stamped = err.node_result.expect("reason stamped on error");
        assert_eq!(
            stamped.get(REASON_KEY).and_then(Value::as_str),
            Some(refusal_reason::UNKNOWN_WORKFLOW_TYPE)
        );
        assert!(stub.last_call().is_none(), "no HTTP call should be made");
    }

    #[tokio::test]
    async fn unsupported_sdlc_workflow_is_a_named_error() {
        let root = temp_dir();
        write_state_json(
            &root,
            json!({ "id": "EN.1.A", "status": "open", "sdlc_workflow": "sdlc-run" }),
        );

        let stub = Arc::new(StubHttpPost::failing("must never be called"));
        let node = DispatchNode::new()
            .with_target_root(root)
            .with_http_post(stub.clone());

        let err = node
            .process(empty_context(json!({ "slug": "EN.1.A" })))
            .await
            .expect_err("unsupported sdlc_workflow refused");

        let stamped = err.node_result.expect("reason stamped on error");
        assert_eq!(
            stamped.get(REASON_KEY).and_then(Value::as_str),
            Some(refusal_reason::UNSUPPORTED_SDLC_WORKFLOW)
        );
        assert!(stub.last_call().is_none(), "no HTTP call should be made");
    }

    #[tokio::test]
    async fn missing_run_id_in_response_is_an_error() {
        let root = temp_dir();
        write_state_json(
            &root,
            json!({ "id": "EN.1.A", "status": "open", "sdlc_workflow": "flow" }),
        );

        let stub = Arc::new(StubHttpPost::succeeding(json!({ "event_id": "no-run-id" })));
        let node = DispatchNode::new()
            .with_target_root(root)
            .with_http_post(stub.clone());

        let err = node
            .process(empty_context(json!({ "slug": "EN.1.A" })))
            .await
            .expect_err("missing run_id in response surfaces as an error");

        let stamped = err.node_result.expect("reason stamped on error");
        assert_eq!(
            stamped.get(REASON_KEY).and_then(Value::as_str),
            Some(refusal_reason::MISSING_RUN_ID)
        );
    }

    #[tokio::test]
    async fn failing_transport_surfaces_as_an_error() {
        let root = temp_dir();
        write_state_json(
            &root,
            json!({ "id": "EN.1.A", "status": "open", "sdlc_workflow": "flow" }),
        );

        let stub = Arc::new(StubHttpPost::failing("connection refused"));
        let node = DispatchNode::new()
            .with_target_root(root)
            .with_http_post(stub.clone());

        let err = node
            .process(empty_context(json!({ "slug": "EN.1.A" })))
            .await
            .expect_err("transport failure surfaces");
        assert!(err.message.contains("connection refused"));
    }

    #[test]
    fn name_matches_node_name_const() {
        assert_eq!(DispatchNode::new().name(), NODE_NAME);
    }

    #[test]
    fn default_constructs_without_panicking() {
        let _node = DispatchNode::default();
    }
}
