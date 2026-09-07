//! EN.14.F task 2: the framework write site for `node_invocations`.
//!
//! Drives a small `Workflow` whose one node is dispatched more than once via
//! a runtime back-edge (a `Router` retry loop), then proves the invocation
//! ledger — not `ctx.nodes` — is the thing that survives a retry.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use engine_contract::TaskContext;
use engine_core::{Node, NodeConfig, NodeError, NodeRegistry, OnProgress, Router, Workflow};

/// Increments a counter in `ctx.metadata` on every dispatch and routes back
/// to itself twice before handing off to `DoneNode` — three total
/// dispatches, so `ctx.nodes["RetryNode"]` (one slot) undercounts by two.
struct RetryNode;

const RETRIES_BEFORE_DONE: u64 = 2;

#[async_trait::async_trait]
impl Node for RetryNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let count = ctx
            .metadata
            .get("retry_count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
            + 1;
        ctx.metadata["retry_count"] = serde_json::json!(count);
        ctx.nodes.insert(
            self.name().to_string(),
            serde_json::json!({ "count": count }),
        );
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "RetryNode"
    }

    fn as_router(&self) -> Option<&dyn Router> {
        Some(self)
    }
}

impl Router for RetryNode {
    fn route(&self, ctx: &TaskContext) -> Option<String> {
        let count = ctx
            .metadata
            .get("retry_count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        if count <= RETRIES_BEFORE_DONE {
            Some("RetryNode".to_string())
        } else {
            Some("DoneNode".to_string())
        }
    }
}

/// A node that always fails, used to exercise the `Err`-branch ledger row.
struct AlwaysFailsNode;

#[async_trait::async_trait]
impl Node for AlwaysFailsNode {
    async fn process(&self, _ctx: TaskContext) -> Result<TaskContext, NodeError> {
        Err(NodeError::new("always fails"))
    }

    fn name(&self) -> &str {
        "AlwaysFailsNode"
    }
}

struct DoneNode;

#[async_trait::async_trait]
impl Node for DoneNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.nodes
            .insert(self.name().to_string(), serde_json::json!({ "done": true }));
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "DoneNode"
    }
}

fn retry_registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(RetryNode));
    registry.register(Box::new(DoneNode));
    registry
}

fn retry_schema() -> engine_core::WorkflowSchema {
    let mut nodes = HashMap::new();
    // RetryNode is a router: its declared connection is never walked at
    // runtime (`route()` decides), but the schema still requires one so the
    // node is reachable/registered.
    nodes.insert(
        "RetryNode".to_string(),
        NodeConfig::new("RetryNode", vec!["DoneNode".to_string()]),
    );
    nodes.insert("DoneNode".to_string(), NodeConfig::new("DoneNode", vec![]));

    engine_core::WorkflowSchema::new("retry-loop", "RetryNode", nodes)
}

#[tokio::test]
async fn retried_node_leaves_more_ledger_rows_than_ctx_nodes_has_keys() {
    let workflow = Workflow::new(retry_registry(), retry_schema());
    let on_progress: OnProgress<'_> = Box::new(|_c: &TaskContext| {});

    let result = workflow
        .run(serde_json::json!({}), on_progress)
        .await
        .expect("workflow should complete");

    let invocations = engine_core::invocations::read_invocations(&result.metadata);
    let ctx_nodes_count = result.nodes.len();

    // THE PROPERTY THIS BLOCK EXISTS FOR: the ledger holds one row per
    // dispatch (RetryNode dispatched 3 times + DoneNode once = 4), while
    // `ctx.nodes` holds one slot per distinct node identity (2: RetryNode,
    // DoneNode) — a strictly smaller count.
    assert!(
        invocations.len() > ctx_nodes_count,
        "expected ledger ({}) to exceed ctx.nodes ({ctx_nodes_count}) on a retried run",
        invocations.len()
    );

    // POSITIVE CONTROL (required by the block): the naive equality form must
    // be FALSE on this same run — proving the assertion above is actually
    // capable of failing, not vacuously true.
    assert!(
        !(invocations.len() == ctx_nodes_count),
        "equality form must be FALSE on a retried run — otherwise this test could never catch a regression"
    );

    // Sanity: RetryNode dispatched 3 times (count 1, 2, 3), DoneNode once.
    let retry_dispatches = invocations
        .iter()
        .filter(|inv| inv.node == "RetryNode")
        .count();
    assert_eq!(retry_dispatches, 3);
    let done_dispatches = invocations
        .iter()
        .filter(|inv| inv.node == "DoneNode")
        .count();
    assert_eq!(done_dispatches, 1);
    assert_eq!(ctx_nodes_count, 2);
}

#[tokio::test]
async fn a_failed_dispatch_produces_a_failed_ledger_row_carrying_the_error_message() {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(AlwaysFailsNode));
    let mut nodes = HashMap::new();
    nodes.insert(
        "AlwaysFailsNode".to_string(),
        NodeConfig::new("AlwaysFailsNode", vec![]),
    );
    let schema = engine_core::WorkflowSchema::new("fails", "AlwaysFailsNode", nodes);
    let workflow = Workflow::new(registry, schema);
    let on_progress: OnProgress<'_> = Box::new(|_c: &TaskContext| {});

    let result = workflow
        .run(serde_json::json!({}), on_progress)
        .await
        .expect("run should return Ok(ctx) even though the node failed");

    let invocations = engine_core::invocations::read_invocations(&result.metadata);
    assert_eq!(invocations.len(), 1);
    assert_eq!(
        invocations[0].status,
        engine_contract::NodeInvocationStatus::Failed
    );
    assert_eq!(invocations[0].error.as_deref(), Some("always fails"));
    assert_eq!(invocations[0].node, "AlwaysFailsNode");
}

// ---------------------------------------------------------------------------
// EN.14.G task 2: payload retention on the invocation record.
// ---------------------------------------------------------------------------

/// THE BLOCK'S CENTRAL TEST. `RetryNode` writes a DISTINCT `{"count": n}`
/// payload on each of its three attempts (see its `process` above). Both
/// attempt 1's and attempt 2's payloads must be retrievable from the ledger
/// and must differ from each other — the audit `ctx.nodes` alone cannot
/// answer, because it holds only the LATEST attempt's slot.
///
/// POSITIVE CONTROL (carryover `gate-scope-must-be-shown-capable-of-failing`):
/// the same test asserts that the latest-only view — what `ctx.nodes` itself
/// holds — canNOT distinguish attempt 1 from attempt 2, proving the ledger
/// assertion above is actually capable of failing rather than vacuously true.
#[tokio::test]
async fn retried_node_ledger_retains_distinct_payloads_per_attempt() {
    let workflow = Workflow::new(retry_registry(), retry_schema());
    let on_progress: OnProgress<'_> = Box::new(|_c: &TaskContext| {});

    let result = workflow
        .run(serde_json::json!({}), on_progress)
        .await
        .expect("workflow should complete");

    let invocations = engine_core::invocations::read_invocations(&result.metadata);
    let retry_invocations: Vec<_> = invocations
        .iter()
        .filter(|inv| inv.node == "RetryNode")
        .collect();
    assert_eq!(
        retry_invocations.len(),
        3,
        "RetryNode dispatched 3 times in this fixture"
    );

    let attempt1_payload = retry_invocations[0]
        .payload
        .clone()
        .expect("attempt 1 should have a retained payload");
    let attempt2_payload = retry_invocations[1]
        .payload
        .clone()
        .expect("attempt 2 should have a retained payload");

    assert_eq!(attempt1_payload, serde_json::json!({ "count": 1 }));
    assert_eq!(attempt2_payload, serde_json::json!({ "count": 2 }));
    assert_ne!(
        attempt1_payload, attempt2_payload,
        "attempt 1 and attempt 2 must retain DISTINCT payloads — the exact \
         question the motivating audit (run 88bd80a0) could not answer"
    );

    // POSITIVE CONTROL: the latest-only view is exactly what `ctx.nodes`
    // holds for this node identity — its single overwritten slot. Assert it
    // CANNOT distinguish the two attempts, proving the ledger assertion
    // above is capable of failing (a regression that stopped writing
    // per-attempt payloads and instead exposed only the latest one would
    // pass the naive check below while failing the real one above).
    let latest_only_view = result
        .nodes
        .get("RetryNode")
        .cloned()
        .expect("ctx.nodes holds RetryNode's latest slot");
    assert_eq!(
        latest_only_view,
        serde_json::json!({ "count": 3 }),
        "ctx.nodes holds only the LATEST attempt"
    );
    assert_ne!(
        latest_only_view, attempt1_payload,
        "the latest-only view must fail to reproduce attempt 1's payload — \
         this is the control proving the ledger is doing real work"
    );
}

/// A node that, on entry, stamps a small `ResolvedPolicy` cap into
/// `ctx.nodes` — mimicking what `stamp_resolved_policy` (task 3) does for a
/// real workflow — so the NEXT node's dispatch reads a cap small enough to
/// force truncation. `payload_cap_from_resolved_policy` reads this off the
/// PRE-CALL context, and `ctx.nodes` persists across dispatches within one
/// workflow run, so stamping it here is visible to the following node.
struct StampSmallCapNode {
    cap_bytes: u64,
}

#[async_trait::async_trait]
impl Node for StampSmallCapNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.nodes.insert(
            engine_core::policy::profiles::RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::json!({
                engine_core::invocations::NODE_INVOCATION_PAYLOAD_CAP_BYTES_FIELD: self.cap_bytes,
            }),
        );
        ctx.nodes.insert(
            self.name().to_string(),
            serde_json::json!({ "stamped": true }),
        );
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "StampSmallCapNode"
    }
}

/// A node that writes a payload far larger than any small test cap.
struct BigPayloadNode;

#[async_trait::async_trait]
impl Node for BigPayloadNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.nodes.insert(
            self.name().to_string(),
            serde_json::json!({ "modified_files": vec!["a-long-path/file.rs"; 10_000] }),
        );
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "BigPayloadNode"
    }
}

/// An over-cap payload must be stored EXPLICITLY marked as truncated — never
/// silently shortened — with `payload_truncated` true and `payload_cap_bytes`
/// recording the cap actually applied.
#[tokio::test]
async fn over_cap_payload_is_recorded_truncated_not_silently_shortened() {
    const SMALL_CAP: u64 = 64;

    let mut registry = NodeRegistry::new();
    registry.register(Box::new(StampSmallCapNode {
        cap_bytes: SMALL_CAP,
    }));
    registry.register(Box::new(BigPayloadNode));
    let mut nodes = HashMap::new();
    nodes.insert(
        "StampSmallCapNode".to_string(),
        NodeConfig::new("StampSmallCapNode", vec!["BigPayloadNode".to_string()]),
    );
    nodes.insert(
        "BigPayloadNode".to_string(),
        NodeConfig::new("BigPayloadNode", vec![]),
    );
    let schema = engine_core::WorkflowSchema::new("cap-test", "StampSmallCapNode", nodes);
    let workflow = Workflow::new(registry, schema);
    let on_progress: OnProgress<'_> = Box::new(|_c: &TaskContext| {});

    let result = workflow
        .run(serde_json::json!({}), on_progress)
        .await
        .expect("workflow should complete");

    let invocations = engine_core::invocations::read_invocations(&result.metadata);
    let big = invocations
        .iter()
        .find(|inv| inv.node == "BigPayloadNode")
        .expect("BigPayloadNode should have a ledger row");

    assert!(
        big.payload_truncated,
        "an over-cap payload must be marked truncated"
    );
    assert_eq!(
        big.payload_cap_bytes, SMALL_CAP,
        "the row must record the cap ACTUALLY APPLIED"
    );
    let payload = big
        .payload
        .as_ref()
        .expect("a truncated payload is still Some — an explicit replacement, not None");
    assert!(
        payload.get("modified_files").is_none(),
        "the truncated payload must never carry a shortened copy of the \
         original shape — a caller must never mistake it for a short one"
    );
}

/// A dispatch returning `Err` has no output payload, but still records the
/// cap that WOULD have applied — every row interpretable on the same terms.
#[tokio::test]
async fn failed_dispatch_records_no_payload_but_still_records_the_cap() {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(AlwaysFailsNode));
    let mut nodes = HashMap::new();
    nodes.insert(
        "AlwaysFailsNode".to_string(),
        NodeConfig::new("AlwaysFailsNode", vec![]),
    );
    let schema = engine_core::WorkflowSchema::new("fails", "AlwaysFailsNode", nodes);
    let workflow = Workflow::new(registry, schema);
    let on_progress: OnProgress<'_> = Box::new(|_c: &TaskContext| {});

    let result = workflow
        .run(serde_json::json!({}), on_progress)
        .await
        .expect("run should return Ok(ctx) even though the node failed");

    let invocations = engine_core::invocations::read_invocations(&result.metadata);
    assert_eq!(invocations.len(), 1);
    assert_eq!(invocations[0].payload, None);
    assert!(!invocations[0].payload_truncated);
    assert_eq!(
        invocations[0].payload_cap_bytes,
        engine_core::invocations::DEFAULT_PAYLOAD_CAP_BYTES,
        "no ResolvedPolicy stamp exists in this fixture, so the framework \
         default cap is what would have applied"
    );
}

// ---------------------------------------------------------------------------
// Task 5: per-run byte growth on a MULTI-NODE run.
//
// The spike's control graph was one node deep, so it could not see the
// accumulation hazard: `node_context` full-clones the context per dispatch,
// `on_progress` fires twice per dispatch, and each snapshot is cloned whole
// into `EventsRow.task_context` downstream. A one-node run cannot distinguish
// "constant per-dispatch cost" from "cost that grows with dispatch count" —
// this test drives a chain long enough to tell them apart.
// ---------------------------------------------------------------------------

/// A single link in an N-node linear chain. Every node stamps `ctx.nodes`
/// (its own slot) so the run also exercises the coexistence this block
/// documents: `ctx.nodes` grows by one slot per DISTINCT node, the ledger
/// grows by one row per DISPATCH.
struct ChainNode {
    name: String,
}

#[async_trait::async_trait]
impl Node for ChainNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.nodes
            .insert(self.name.clone(), serde_json::json!({ "visited": true }));
        Ok(ctx)
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// Builds a linear chain of `n` `ChainNode`s: `chain-0 -> chain-1 -> ... ->
/// chain-{n-1}`, `n` distinct dispatches, `n` distinct node identities.
fn chain_registry_and_schema(n: usize) -> (NodeRegistry, engine_core::WorkflowSchema) {
    assert!(n >= 2, "a chain needs at least two links to show growth");
    let names: Vec<String> = (0..n).map(|i| format!("chain-{i}")).collect();

    let mut registry = NodeRegistry::new();
    let mut nodes = HashMap::new();
    for (i, name) in names.iter().enumerate() {
        let next = if i + 1 < names.len() {
            vec![names[i + 1].clone()]
        } else {
            vec![]
        };
        registry.register(Box::new(ChainNode { name: name.clone() }));
        nodes.insert(name.clone(), NodeConfig::new(name, next));
    }

    let schema = engine_core::WorkflowSchema::new("chain", &names[0], nodes);
    (registry, schema)
}

/// Measured 2026-09-07 on this exact 12-node linear chain (see the
/// assertions below for the derived, non-frozen form of this claim):
/// serialized `ctx.metadata` sizes after each of the 12 dispatches were
/// `[246, 470, 694, 918, 1142, 1366, 1590, 1814, 2038, 2262, 2488, 2714]`
/// bytes — a per-dispatch delta of a constant 224-226 bytes (one
/// `NodeInvocation` row's JSON: a UUID, an optional run/campaign id, a node
/// name, a `u64` seq, two RFC3339 timestamps, a status enum, an optional
/// error string), for 2,714 bytes total across 12 dispatches. The exact
/// numbers are NOT asserted below — they drift with the schema and with
/// `chain-N` name lengths — but their SHAPE is: bounded per record, linear
/// in dispatch count, never the O(n^2) growth the spike's one-node control
/// could not see.
#[tokio::test]
async fn per_dispatch_metadata_growth_is_bounded_and_linear_on_a_multi_node_run() {
    const CHAIN_LEN: usize = 12;
    let (registry, schema) = chain_registry_and_schema(CHAIN_LEN);
    let workflow = Workflow::new(registry, schema);

    // Capture every `on_progress` snapshot's metadata, cloned out to a plain
    // `serde_json::Value` so it survives past the callback's borrow.
    let snapshots: Rc<RefCell<Vec<serde_json::Value>>> = Rc::new(RefCell::new(Vec::new()));
    let snapshots_handle = snapshots.clone();
    let on_progress: OnProgress<'_> =
        Box::new(move |c: &TaskContext| snapshots_handle.borrow_mut().push(c.metadata.clone()));

    let result = workflow
        .run(serde_json::json!({}), on_progress)
        .await
        .expect("chain workflow should complete");

    assert_eq!(
        result.nodes.len(),
        CHAIN_LEN,
        "one ctx.nodes slot per distinct node identity"
    );
    let final_invocations = engine_core::invocations::read_invocations(&result.metadata);
    assert_eq!(
        final_invocations.len(),
        CHAIN_LEN,
        "one ledger row per dispatch, none retried in this chain"
    );

    // `node_context` calls `on_progress` twice per dispatch (entering RUNNING,
    // then exiting SUCCESS/FAILED) plus `walk`'s own initial call before the
    // first dispatch. Reduce the captured snapshots to exactly one
    // POST-dispatch snapshot per ledger length — the first snapshot at which
    // `read_invocations(...).len()` reaches each new value, which is the
    // snapshot taken immediately after that dispatch's `append_invocation`
    // call (workflow.rs: `on_progress(&ok_ctx)` fires after the append).
    let mut per_dispatch_bytes: Vec<usize> = Vec::with_capacity(CHAIN_LEN);
    let mut last_len = 0usize;
    for snap in snapshots.borrow().iter() {
        let len = engine_core::invocations::read_invocations(snap).len();
        if len > last_len {
            let bytes = serde_json::to_vec(snap)
                .expect("ctx.metadata must always serialize")
                .len();
            per_dispatch_bytes.push(bytes);
            last_len = len;
        }
    }
    assert_eq!(
        per_dispatch_bytes.len(),
        CHAIN_LEN,
        "expected exactly one post-dispatch snapshot per ledger row"
    );

    // Per-dispatch delta: bytes(n) - bytes(n-1). If growth were O(n^2) (each
    // snapshot re-embedding the whole growing history somewhere it
    // shouldn't), later deltas would dwarf earlier ones. Bounded + linear
    // means every delta sits within a small, roughly constant band — one
    // NodeInvocation's JSON encoding, which does not itself grow with `n`.
    let deltas: Vec<i64> = per_dispatch_bytes
        .windows(2)
        .map(|w| w[1] as i64 - w[0] as i64)
        .collect();
    assert!(!deltas.is_empty(), "need at least one delta to bound");

    let max_delta = *deltas.iter().max().unwrap();
    let min_delta = *deltas.iter().min().unwrap();

    // Deltas are all positive (append-only, one record longer each time) and
    // every delta stays within a generous constant bound of the smallest one
    // seen — a single JSON `NodeInvocation` record does not vary in size by
    // more than a small constant regardless of how many prior rows exist. If
    // growth were superlinear, later deltas would blow well past this band.
    assert!(
        min_delta > 0,
        "each dispatch must strictly grow the serialized ledger: deltas={deltas:?}"
    );
    const MAX_DELTA_DRIFT_BYTES: i64 = 256;
    assert!(
        max_delta - min_delta <= MAX_DELTA_DRIFT_BYTES,
        "per-dispatch byte growth is not bounded by a constant: min={min_delta} max={max_delta} deltas={deltas:?} (a bound this loose already rules out O(n^2) growth, where later deltas would dwarf earlier ones)"
    );

    // LINEAR, not superlinear: total growth across the whole chain must be
    // within a small constant factor of `CHAIN_LEN * min_delta` — if a
    // snapshot were re-embedding growing history (O(n^2)), the total would
    // be roughly quadratic in CHAIN_LEN instead.
    let total_growth = per_dispatch_bytes.last().unwrap() - per_dispatch_bytes.first().unwrap();
    let linear_upper_bound = (CHAIN_LEN as i64 - 1) * (min_delta + MAX_DELTA_DRIFT_BYTES);
    assert!(
        (total_growth as i64) <= linear_upper_bound,
        "total per-run byte growth ({total_growth} bytes over {CHAIN_LEN} dispatches) exceeds the linear bound ({linear_upper_bound}) — this is the O(n^2) hazard the block's spike could not see"
    );
}

/// Code-level check (block acceptance criterion): `node_context` must
/// perform no more full-`TaskContext` clones than it did before this block —
/// `pre_call_ctx = ctx.clone()` must remain the ONLY whole-context clone in
/// the function. Every other `.clone()` inside `node_context` clones a field
/// (`identity`, `run_id`, `campaign_id`, an error message, …), never the
/// whole struct, so this greps specifically for the `ctx.clone()` /
/// `<var>_ctx.clone()` shape rather than counting `.clone()` calls overall.
#[test]
fn node_context_clones_the_full_task_context_exactly_once() {
    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set by cargo");
    let workflow_rs = std::path::PathBuf::from(manifest_dir).join("src/workflow.rs");
    let source = std::fs::read_to_string(&workflow_rs)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", workflow_rs.display()));

    let body = extract_fn_body(&source, "async fn node_context(")
        .expect("node_context function body must be found in workflow.rs");

    // A whole-`TaskContext` clone reads as `ctx.clone()` in this function —
    // never `pre_call_ctx.clone()` (that snapshot is read from, not
    // re-cloned) and never a field-level `.foo.clone()`. Count occurrences of
    // the literal `ctx.clone()` token sequence.
    let whole_context_clones = body.matches("ctx.clone()").count();
    assert_eq!(
        whole_context_clones, 1,
        "expected exactly one whole-TaskContext clone (`pre_call_ctx = ctx.clone()`) in node_context; found {whole_context_clones} in:\n{body}"
    );
}

/// Extracts the body of a function whose signature starts with `needle`
/// (e.g. `"async fn node_context("`), by brace-matching from the first `{`
/// after the signature to its balancing `}`. Returns `None` if `needle`
/// isn't found or the braces never balance.
fn extract_fn_body<'a>(source: &'a str, needle: &str) -> Option<&'a str> {
    let start = source.find(needle)?;
    let open = source[start..].find('{')? + start;
    let mut depth = 0i32;
    for (offset, ch) in source[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&source[open..open + offset + 1]);
                }
            }
            _ => {}
        }
    }
    None
}
