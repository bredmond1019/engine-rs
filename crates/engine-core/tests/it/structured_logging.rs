//! Integration coverage for `EN.11.I` task 5: pins the JSON layer's actual
//! wire shape and the zero-`eprintln!` gate, both scoped so each can
//! genuinely pass AND genuinely fail — never a check that quietly matches
//! nothing.
//!
//! Uses `tracing_subscriber`'s real `fmt().json()` formatter (the same
//! builder `engine_serve::init_tracing` installs in production, including
//! its `flatten_event(true)`) writing into an in-memory buffer, never a log
//! file on disk — matching the block's `testing_strategy` exactly. This is
//! deliberately a DIFFERENT instrument than `workflow.rs`'s
//! `instrumentation::CaptureLayer` (task 2's custom `Layer`, which merges
//! span-inherited fields for testing *propagation*): this suite proves the
//! literal JSON *wire shape* the block's acceptance criterion names,
//! `jq -e 'select(.run_id==$ID) | .node'` — a query that only works if
//! `run_id` and `node` are top-level keys on the line, which is exactly
//! what `flatten_event` plus `workflow::node_context`'s explicit per-node
//! event (task 5's own addition to `workflow.rs`) produce.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use engine_contract::TaskContext;
use engine_core::{
    Node, NodeConfig, NodeError, NodeRegistry, RunOptions, Workflow, WorkflowSchema,
};
use serde_json::Value;
use uuid::Uuid;

// ---------------------------------------------------------------------
// Shared in-memory writer + subscriber builder
// ---------------------------------------------------------------------

/// An in-process, in-memory sink for `tracing-subscriber`'s `fmt` writer —
/// never a real file. Cloning shares the same underlying buffer (required
/// by `MakeWriter`, which is asked for a fresh writer per event/span).
#[derive(Clone, Default)]
struct SharedBuf(Arc<Mutex<Vec<u8>>>);

impl io::Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for SharedBuf {
    type Writer = SharedBuf;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

impl SharedBuf {
    /// Every captured line, parsed as JSON, in emission order. Panics on a
    /// malformed line — a shape assertion should fail loudly, not silently
    /// skip a line the formatter produced.
    fn lines(&self) -> Vec<Value> {
        let bytes = self.0.lock().unwrap().clone();
        String::from_utf8(bytes)
            .expect("tracing-subscriber's JSON writer must emit valid UTF-8")
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("each captured line must be valid JSON"))
            .collect()
    }
}

/// Installs the real JSON formatter — same builder shape as
/// `engine_serve::init_tracing` (`.json().flatten_event(true)`) — as the
/// CURRENT THREAD's default subscriber only (`set_default`, not
/// `set_global_default`), so this never collides with another test's
/// subscriber under `cargo nextest run`'s process-per-test model (CLAUDE.md
/// standing rule 7) or with `engine_serve::init_tracing`'s own
/// idempotent-global-install test.
fn capturing_json() -> (SharedBuf, tracing::subscriber::DefaultGuard) {
    let buf = SharedBuf::default();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .flatten_event(true)
        .with_writer(buf.clone())
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    (buf, guard)
}

// ---------------------------------------------------------------------
// Fixture: a 3-node linear workflow, each node a plain success node
// ---------------------------------------------------------------------

struct StampNode(&'static str);

#[async_trait::async_trait]
impl Node for StampNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.nodes
            .insert(self.0.to_string(), serde_json::json!({ "ran": true }));
        Ok(ctx)
    }

    fn name(&self) -> &str {
        self.0
    }
}

/// A node whose `process` always fails, so the failure path
/// (`tracing::error!` in `node_context`) is exercised too — the block's own
/// acceptance criterion ("a node that fails emits a structured event naming
/// the node and the failure") is not a side effect of the success-path test
/// below, it is asserted directly.
struct FailingNode(&'static str);

#[async_trait::async_trait]
impl Node for FailingNode {
    async fn process(&self, _ctx: TaskContext) -> Result<TaskContext, NodeError> {
        Err(NodeError::new("deliberate failure for EN.11.I task 5"))
    }

    fn name(&self) -> &str {
        self.0
    }
}

/// `start_node -> node2 -> node3` (terminal), wired via `connections[0]`
/// only — the same linear-3 shape `budget.rs`/`cancellation.rs` already use
/// in this suite.
fn linear_three_schema() -> WorkflowSchema {
    let mut nodes = HashMap::new();
    nodes.insert(
        "start_node".to_string(),
        NodeConfig::new("start_node", vec!["node2".to_string()]),
    );
    nodes.insert(
        "node2".to_string(),
        NodeConfig::new("node2", vec!["node3".to_string()]),
    );
    nodes.insert("node3".to_string(), NodeConfig::new("node3", vec![]));
    WorkflowSchema::new("linear-3", "start_node", nodes)
}

fn linear_three_registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(StampNode("start_node")));
    registry.register(Box::new(StampNode("node2")));
    registry.register(Box::new(StampNode("node3")));
    registry
}

// ---------------------------------------------------------------------
// (1) One JSON line per node, in dispatch order, run_id populated —
//     asserted the way the AC literally reads: select on run_id, read node.
// ---------------------------------------------------------------------

#[tokio::test]
async fn one_json_line_per_node_in_dispatch_order_with_run_id_populated() {
    let (buf, _guard) = capturing_json();

    let workflow = Workflow::new(linear_three_registry(), linear_three_schema());
    let run_id = Uuid::new_v4();
    let options = RunOptions {
        cancellation_token: None,
        budget: None,
        pause_signal: None,
        run_id: Some(run_id),
    };
    workflow
        .run_with(
            serde_json::json!({}),
            Box::new(|_ctx: &TaskContext| {}),
            options,
        )
        .await
        .expect("linear-3 run should succeed");

    let run_id_str = run_id.to_string();

    // The literal AC shape: `jq -e 'select(.run_id==$ID) | .node'` — select
    // on the top-level `run_id` key, then read the top-level `node` key.
    // Asserted over the parsed JSON objects (not a real `jq` shell-out —
    // exactly what `testing_strategy` calls for, since a shell-out would
    // make the test depend on an external binary).
    let node_names: Vec<String> = buf
        .lines()
        .into_iter()
        .filter(|line| line.get("run_id").and_then(Value::as_str) == Some(run_id_str.as_str()))
        .map(|line| {
            line.get("node")
                .and_then(Value::as_str)
                .expect("a line selected by this run's run_id must carry a top-level `node` key")
                .to_string()
        })
        .collect();

    assert_eq!(
        node_names,
        vec!["start_node", "node2", "node3"],
        "exactly one line per node, in dispatch order, each carrying this run's run_id"
    );
}

/// Negative control for the test above: a DIFFERENT run's id selects none
/// of these lines. Proves the `select(.run_id==$ID)` filter is actually
/// discriminating on the field, not vacuously matching every line.
#[tokio::test]
async fn a_different_run_id_selects_none_of_this_runs_lines() {
    let (buf, _guard) = capturing_json();

    let workflow = Workflow::new(linear_three_registry(), linear_three_schema());
    let run_id = Uuid::new_v4();
    let options = RunOptions {
        cancellation_token: None,
        budget: None,
        pause_signal: None,
        run_id: Some(run_id),
    };
    workflow
        .run_with(
            serde_json::json!({}),
            Box::new(|_ctx: &TaskContext| {}),
            options,
        )
        .await
        .expect("linear-3 run should succeed");

    let unrelated_id = Uuid::new_v4().to_string();
    let matches = buf
        .lines()
        .into_iter()
        .filter(|line| line.get("run_id").and_then(Value::as_str) == Some(unrelated_id.as_str()))
        .count();

    assert_eq!(matches, 0, "an unrelated run_id must select zero lines");
}

// ---------------------------------------------------------------------
// (2) A failing node emits a structured event naming the node and the
//     failure — not a bare message.
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_failing_node_emits_a_structured_event_naming_the_node_and_the_failure() {
    let (buf, _guard) = capturing_json();

    let mut registry = NodeRegistry::new();
    registry.register(Box::new(FailingNode("BoomNode")));
    let mut nodes = HashMap::new();
    nodes.insert("BoomNode".to_string(), NodeConfig::new("BoomNode", vec![]));
    let schema = WorkflowSchema::new("single-fail", "BoomNode", nodes);
    let workflow = Workflow::new(registry, schema);

    let run_id = Uuid::new_v4();
    let options = RunOptions {
        cancellation_token: None,
        budget: None,
        pause_signal: None,
        run_id: Some(run_id),
    };
    workflow
        .run_with(
            serde_json::json!({}),
            Box::new(|_ctx: &TaskContext| {}),
            options,
        )
        .await
        .expect("a node failure halts the walk but is still Ok(ctx) at the Workflow level");

    let run_id_str = run_id.to_string();
    let failure_line = buf
        .lines()
        .into_iter()
        .find(|line| {
            line.get("run_id").and_then(Value::as_str) == Some(run_id_str.as_str())
                && line.get("node").and_then(Value::as_str) == Some("BoomNode")
        })
        .expect("the failing node's dispatch must produce a line naming it");

    assert_eq!(
        failure_line.get("level").and_then(Value::as_str),
        Some("ERROR"),
        "a failure must be logged at error level, not folded into an info line"
    );
    let error_field = failure_line
        .get("error")
        .and_then(Value::as_str)
        .expect("the failure event must carry a structured `error` field");
    assert!(
        error_field.contains("deliberate failure for EN.11.I task 5"),
        "the structured `error` field must name the actual failure, not a bare generic message: {error_field}"
    );
}

// ---------------------------------------------------------------------
// (3) The spawn_blocking field-propagation shape, over the real JSON
//     writer (task 2 already proved propagation itself via the
//     independent CaptureLayer in workflow.rs; this proves it survives
//     the real formatter too).
// ---------------------------------------------------------------------

struct SpawnBlockingStampNode;

#[async_trait::async_trait]
impl Node for SpawnBlockingStampNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let current_span = tracing::Span::current();
        let current_dispatch = tracing::dispatcher::get_default(|d| d.clone());
        tokio::task::spawn_blocking(move || {
            tracing::dispatcher::with_default(&current_dispatch, || {
                let _guard = current_span.enter();
                tracing::info!(
                    marker = "blocking-json-shape",
                    "emitted inside spawn_blocking"
                );
            });
        })
        .await
        .map_err(|err| NodeError::new(format!("blocking task panicked: {err}")))?;

        ctx.nodes
            .insert(self.name().to_string(), serde_json::json!({ "ran": true }));
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "SpawnBlockingStampNode"
    }
}

#[tokio::test]
async fn spawn_blocking_event_carries_run_id_over_the_real_json_writer() {
    let (buf, _guard) = capturing_json();

    let mut registry = NodeRegistry::new();
    registry.register(Box::new(SpawnBlockingStampNode));
    let mut nodes = HashMap::new();
    nodes.insert(
        "SpawnBlockingStampNode".to_string(),
        NodeConfig::new("SpawnBlockingStampNode", vec![]),
    );
    let schema = WorkflowSchema::new("single-blocking", "SpawnBlockingStampNode", nodes);
    let workflow = Workflow::new(registry, schema);

    let run_id = Uuid::new_v4();
    let options = RunOptions {
        cancellation_token: None,
        budget: None,
        pause_signal: None,
        run_id: Some(run_id),
    };
    workflow
        .run_with(
            serde_json::json!({}),
            Box::new(|_ctx: &TaskContext| {}),
            options,
        )
        .await
        .expect("run should succeed");

    let run_id_str = run_id.to_string();
    let blocking_line = buf
        .lines()
        .into_iter()
        .find(|line| line.get("marker").and_then(Value::as_str) == Some("blocking-json-shape"))
        .expect("the spawn_blocking closure's event must have reached the real JSON writer");

    // The span-inherited `run_id` (from `workflow.walk`'s span) reaches the
    // event via `spans`/`span`, NOT via a flattened top-level key — this is
    // the negative distinction documented on `node_context`: only an
    // event's OWN fields flatten to the top level. So this asserts the
    // propagation shape task 2 built (nested under `spans`), which is what
    // actually crosses the `spawn_blocking` boundary here.
    let spans = blocking_line
        .get("spans")
        .and_then(Value::as_array)
        .expect("a `spans` array must be present when the event fired inside an active span");
    let carried_run_id = spans
        .iter()
        .find_map(|span| span.get("run_id").and_then(Value::as_str));
    assert_eq!(
        carried_run_id,
        Some(run_id_str.as_str()),
        "the ancestor `workflow.walk` span's run_id must have crossed the spawn_blocking \
         boundary and appear in this event's inherited span list"
    );
}

// ---------------------------------------------------------------------
// (4) Zero eprintln! under crates/*/src/ — scoped so it can pass AND fail.
// ---------------------------------------------------------------------

/// Counts `eprintln!(` call sites under `root` (`.rs` files only),
/// recursively. Matches the invocation form specifically (`eprintln!(`,
/// with the opening paren) rather than the bare token `eprintln!` — several
/// doc comments in this tree legitimately mention `eprintln!` in prose
/// (e.g. "migrated this off `eprintln!`") without containing a call, and a
/// bare-token match would misreport those as violations.
fn eprintln_call_count(root: &Path) -> usize {
    let mut count = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                if let Ok(contents) = std::fs::read_to_string(&path) {
                    count += contents.matches("eprintln!(").count();
                }
            }
        }
    }
    count
}

/// `CARGO_MANIFEST_DIR` for this test binary is `crates/engine-core`; the
/// workspace root (where `crates/` lives) is two levels up.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/engine-core must sit two levels under the workspace root")
        .to_path_buf()
}

/// Every crate's `src/` directory — deliberately NOT `crates/` wholesale,
/// which can never reach zero: `engine-core/examples/research.rs` (6 calls)
/// and `engine-contract/tests/round_trip.rs` (1 call) are out of scope by
/// the block record itself and must stay untouched.
fn all_crate_src_dirs(workspace_root: &Path) -> Vec<PathBuf> {
    let crates_dir = workspace_root.join("crates");
    std::fs::read_dir(&crates_dir)
        .expect("workspace crates/ directory must exist")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .map(|crate_dir| crate_dir.join("src"))
        .filter(|src_dir| src_dir.is_dir())
        .collect()
}

#[test]
fn zero_eprintln_calls_remain_under_crates_star_src() {
    let root = workspace_root();
    let total: usize = all_crate_src_dirs(&root)
        .iter()
        .map(|dir| eprintln_call_count(dir))
        .sum();
    assert_eq!(
        total, 0,
        "crates/*/src/ must have zero eprintln! call sites (EN.11.I tasks 3-4 migrated all 17); \
         examples/ and tests/ are deliberately out of scope and are not counted here"
    );
}

/// Proves the check above is actually capable of failing — not a glob that
/// silently matches nothing. Plants a real `eprintln!(...)` call in a
/// throwaway temp directory shaped like a crate's `src/` (never touching
/// this repo's own tree) and asserts `eprintln_call_count` finds it.
#[test]
fn the_zero_count_check_is_demonstrated_capable_of_failing() {
    let tmp = std::env::temp_dir().join(format!("en-11-i-task5-eprintln-probe-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).expect("create temp probe dir");

    // Before planting anything: an empty fixture directory must read 0,
    // same as the real check would report if the glob matched nothing —
    // this is the control that distinguishes "genuinely clean" from
    // "found nothing to search".
    assert_eq!(eprintln_call_count(&tmp), 0);

    std::fs::write(
        tmp.join("planted.rs"),
        "fn f() { eprintln!(\"planted by EN.11.I task 5's own check-can-fail probe\"); }\n",
    )
    .expect("write planted fixture file");

    // The check goes red against the planted call — demonstrating the
    // real assertion (`zero_eprintln_calls_remain_under_crates_star_src`)
    // would have failed had this call existed under `crates/*/src/`.
    assert_eq!(
        eprintln_call_count(&tmp),
        1,
        "the count must detect a deliberately planted eprintln! call — if this assertion fails, \
         the check's glob is not actually searching anything, which is the failure mode that \
         lets a zero-count assertion rot into a permanent, meaningless green"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
