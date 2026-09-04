//! The `APPROVE_AND_RUN` micro-workflow (`EN.8.D`) — drains the
//! pending-harvest queue `HARVEST_APPROVE` already serves through `EN.8.B`'s
//! depth-limited operator queue and `EN.8.C`'s approval ledger, so a
//! deferred harvest arrives as a decision the operator can answer in one
//! tap and, on approval, executes against the exact payload they reviewed.
//!
//! Composes existing primitives; it invents no new one:
//! [`crate::nodes::harvest_gate`]/[`crate::nodes::harvest_gate::pending_harvest_record`]
//! (`EN.7.C`), the payload contract in [`crate::operator`] (`EN.8.A`), the
//! queue in [`crate::operator::queue`] (`EN.8.B`), and the ledger in
//! [`crate::operator::ledger`] (`EN.8.C`).
//!
//! Module layout (built up task by task per `planning/EN.8.D/tasks.json`):
//! - [`render`] (task 1) — the pure pending-harvest-record -> validated
//!   operator payload step.
//! - [`policy`] / [`profiles`] (task 2) — the `drain_batch_max` /
//!   `harvest_item_priority` / `session_fallback_slug` knobs this block
//!   introduces, resolved through the standard four-layer precedence and
//!   documented in `planning/harness.json`'s `approve_and_run` section.
//! - [`drain`] (task 3) — renders + validates a batch of pending-harvest
//!   records, enqueues the conforming ones onto an
//!   [`crate::operator::queue::OperatorQueue`], routes the rest to
//!   `session-<slug>`, and makes exactly one `next_deliverable` call.
//! - [`verdict`] (task 4) — resolves one operator verdict for a delivered
//!   item into exactly one [`crate::operator::ledger`] row via
//!   `record_decision`, without re-implementing its digest-mismatch ->
//!   `Requeued` enforcement, and authorizes execution against the
//!   pending-harvest record's stored payload — never a re-derived one —
//!   only on a matched `Approved` verdict.
//! - [`graph`] (task 5) — the declared `WorkflowSchema` / `NodeRegistry` /
//!   `Workflow` assembly. Its single node drives the existing
//!   `nodes::harvest_approve::HarvestApproveNode` over the injectable
//!   `HttpPost` seam against task 4's authorization — no second push path —
//!   and stamps the resolved policy from [`policy`] into its own
//!   `ctx.nodes` result.
//! - [`ApproveAndRunSeams`] (task 6) — the two public seams
//!   `bastion:BA.18.B` left open (`core/bastion/src/serve/notify/mod.rs`):
//!   [`ApproveAndRunSeams::lookup_pending`] resolves a `gate_id` to the
//!   payload currently open on the queue (bastion's `PendingLookup` shape),
//!   and [`ApproveAndRunSeams::resolve_verdict`] runs [`verdict::decide`]
//!   and, on authorization, [`graph::ApproveAndRunExecuteNode`] (bastion's
//!   `VerdictSink` shape). `engine-serve`'s `register_approve_and_run`
//!   registers the declared graph itself for dispatch; this crate does not
//!   touch `core/bastion` — the final `run_server` wiring that passes these
//!   seams to bastion's transport is a bastion block.

pub mod drain;
pub mod graph;
pub mod policy;
pub mod profiles;
pub mod render;
pub mod verdict;

pub use drain::{drain, DrainReport, SessionRoutedRecord};
pub use graph::{
    execution_event, registry, registry_with, schema, workflow, ApproveAndRunExecuteNode,
    APPROVE_AND_RUN_NODE_NAME, APPROVE_AND_RUN_WORKFLOW_TYPE,
};
pub use policy::{policy_state, ApproveAndRunPolicy, PartialApproveAndRunPolicy};
pub use profiles::{
    profile_by_name, read_harness_policy_defaults_from, resolve_policy_for_run_from,
};
pub use render::{
    gate_id_for, render, render_and_validate, PendingHarvestRecord, OPTION_APPROVE,
    OPTION_OPEN_SESSION, OPTION_SKIP,
};
pub use verdict::{decide, ExecutionAuthorization, UnknownOptionKey, VerdictOutcome};

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use engine_contract::TaskContext;

use crate::node::{Node, NodeError};
use crate::nodes::http_post::HttpPost;
use crate::operator::ledger::ApprovalLedger;
use crate::operator::queue::OperatorQueue;
use crate::operator::{validate, OperatorPayloadLimits, ValidatedOperatorPayload};

/// The `ctx.nodes` key `crate::workflows::content_pipeline::persist_to_brain`
/// stamps its result under (its own `NODE_NAME` const) — the key
/// [`pending_harvest_records_from_content_pipeline_run`] reads to find a
/// deferred harvest's pending record.
const CONTENT_PIPELINE_PERSIST_TO_BRAIN_NODE_NAME: &str = "PersistToBrainNode";

/// Extract the pending-harvest record(s), if any, out of a finished
/// `CONTENT_PIPELINE` run's `ctx.nodes` map.
///
/// `persist_to_brain.rs`'s `Defer` arm (harvest mode `approval`) stamps a
/// pending-harvest-record-shaped JSON value under
/// `ctx.nodes["PersistToBrainNode"]["pending"]` — the exact shape
/// [`approval_mode_makes_zero_posts_and_stamps_a_pending_record`] fixtures
/// (`crate::workflows::content_pipeline::persist_to_brain`) and that
/// [`PendingHarvestRecord::from_value`] parses. When the run posted
/// in-process instead of deferring (harvest mode `off`, or a matched
/// `Approved` decision), that key is `null` and this returns an empty
/// `Vec` — there is nothing pending to drain.
///
/// This function is pure: no I/O, no clock read, no queue or ledger
/// mutation. It only locates and parses the record(s); it does not enqueue
/// them. **The real caller that holds a live [`ApproveAndRunSeams`]
/// instance and passes these records to [`ApproveAndRunSeams::drain`] lives
/// in `core/bastion`** (where a served `CONTENT_PIPELINE` dispatch actually
/// runs and where the live `ApproveAndRunSeams` is constructed) — this
/// crate has no dependency on `core/bastion` and cannot call `.drain()`
/// from inside a `CONTENT_PIPELINE` run itself.
///
/// # Errors
///
/// Returns the underlying [`serde_json::Error`] if the stamped `pending`
/// value does not parse as a [`PendingHarvestRecord`].
pub fn pending_harvest_records_from_content_pipeline_run(
    nodes: &HashMap<String, serde_json::Value>,
) -> Result<Vec<PendingHarvestRecord>, serde_json::Error> {
    let Some(result) = nodes.get(CONTENT_PIPELINE_PERSIST_TO_BRAIN_NODE_NAME) else {
        return Ok(Vec::new());
    };
    let pending = result
        .get("pending")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    if pending.is_null() {
        return Ok(Vec::new());
    }
    Ok(vec![PendingHarvestRecord::from_value(&pending)?])
}

/// The engine-side shape of one resolved operator verdict — the fields a
/// resolved Telegram response carries, without naming bastion's
/// `telegram::ResponseVerdict` (this crate has no dependency on
/// `core/bastion`, and cannot). [`ApproveAndRunSeams::resolve_verdict`]
/// takes this in place of bastion's `VerdictSink` argument; bastion's own
/// `run_server` wiring is responsible for converting a real
/// `ResponseVerdict` into one of these.
#[derive(Debug, Clone, PartialEq)]
pub struct ApproveAndRunVerdict {
    /// The `gate_id` the operator's response resolved against — the same
    /// value [`render::gate_id_for`] stamped into the delivered payload.
    pub gate_id: String,
    /// The digest presented to the operator at decision time (what the
    /// channel echoes back with the verdict).
    pub presented_digest: String,
    /// The response option key the operator tapped — one of
    /// [`render::OPTION_APPROVE`] / [`render::OPTION_SKIP`] /
    /// [`render::OPTION_OPEN_SESSION`].
    pub option_key: String,
    /// Who decided — attributed on the written ledger row.
    pub who: String,
    /// When the decision was made.
    pub decided_at: DateTime<Utc>,
}

/// What [`ApproveAndRunSeams::resolve_verdict`] returns: task 4's ledger
/// outcome, plus task 5's execution result (`ctx.nodes` entry) when the
/// verdict authorized one — `None` on skip, requeue, or routed-to-session.
#[derive(Debug, Clone, PartialEq)]
pub struct ApproveAndRunVerdictResolution {
    /// The full [`verdict::VerdictOutcome`] task 4 produced.
    pub outcome: VerdictOutcome,
    /// [`graph::ApproveAndRunExecuteNode`]'s stamped `ctx.nodes` result,
    /// present only when `outcome.authorization` was `Some` (a matched
    /// `Approved` verdict) and execution actually ran.
    pub executed: Option<serde_json::Value>,
}

/// Why [`ApproveAndRunSeams::resolve_verdict`] could not resolve a verdict.
#[derive(Debug, Clone, PartialEq)]
pub enum ApproveAndRunSeamError {
    /// `gate_id` has nothing currently open on the queue, and/or no
    /// pending-harvest record was ever stored for it under
    /// [`ApproveAndRunSeams::drain`].
    UnknownGate(String),
    /// The response option key was not one of the three [`render`] renders.
    UnknownOption(UnknownOptionKey),
    /// Task 5's execution node returned a [`NodeError`].
    Execution(NodeError),
}

impl std::fmt::Display for ApproveAndRunSeamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApproveAndRunSeamError::UnknownGate(gate_id) => {
                write!(f, "unknown gate_id: {gate_id:?}")
            }
            ApproveAndRunSeamError::UnknownOption(err) => write!(f, "{err}"),
            ApproveAndRunSeamError::Execution(err) => write!(f, "execution failed: {err}"),
        }
    }
}

impl std::error::Error for ApproveAndRunSeamError {}

/// The two public seams `bastion:BA.18.B` left open
/// (`core/bastion/src/serve/notify/mod.rs`), composed over this workflow's
/// existing pieces (`EN.8.D` task 6):
///
/// - [`Self::lookup_pending`] — bastion's `PendingLookup` shape: resolve a
///   `gate_id` to `Option<ValidatedOperatorPayload>` for whatever this
///   process currently has open on the queue.
/// - [`Self::resolve_verdict`] — bastion's `VerdictSink` shape: consume a
///   resolved verdict, running task 4's ledger path and, on authorization,
///   task 5's execution.
///
/// Both are constructible with no database and no network call: the queue
/// and the pending-record store are plain in-memory state behind a
/// [`std::sync::Mutex`], and the `ApprovalLedger`/`HttpPost` seams are
/// injected by the caller — `engine-serve`'s registration wires a live
/// `FileApprovalLedger`/`http_post_live()`, tests wire in-memory doubles
/// (`InMemoryApprovalLedger`/`StubHttpPost`). `Send + Sync` follows
/// directly from every field being `Send + Sync`.
pub struct ApproveAndRunSeams {
    queue: Arc<Mutex<OperatorQueue>>,
    records: Arc<Mutex<HashMap<String, PendingHarvestRecord>>>,
    ledger: Arc<dyn ApprovalLedger>,
    http_post: Arc<dyn HttpPost>,
    limits: OperatorPayloadLimits,
    policy: ApproveAndRunPolicy,
}

impl ApproveAndRunSeams {
    /// Construct the seams over an explicit shared queue, ledger, and
    /// `HttpPost` transport. No database, no network call here — everything
    /// is either in-memory state this call allocates, or an injected seam
    /// the caller already owns.
    #[must_use]
    pub fn new(
        queue: Arc<Mutex<OperatorQueue>>,
        ledger: Arc<dyn ApprovalLedger>,
        http_post: Arc<dyn HttpPost>,
        limits: OperatorPayloadLimits,
        policy: ApproveAndRunPolicy,
    ) -> Self {
        Self {
            queue,
            records: Arc::new(Mutex::new(HashMap::new())),
            ledger,
            http_post,
            limits,
            policy,
        }
    }

    /// Drain `records` (task 3's [`drain`]) into the shared queue,
    /// remembering each record under its rendered `gate_id` so
    /// [`Self::lookup_pending`] / [`Self::resolve_verdict`] can resolve
    /// back to the exact stored `url`/`payload` a later `Approved` verdict
    /// authorizes execution against.
    pub fn drain(
        &self,
        records: &[PendingHarvestRecord],
        enqueued_at: DateTime<Utc>,
    ) -> DrainReport {
        {
            let mut store = self.records.lock().unwrap_or_else(|e| e.into_inner());
            for record in records {
                store.insert(gate_id_for(&record.artifact_id), record.clone());
            }
        }
        let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        drain(records, &mut queue, &self.limits, &self.policy, enqueued_at)
    }

    /// Resolve `gate_id` to the [`ValidatedOperatorPayload`] currently open
    /// on the queue for it — the shape bastion's `PendingLookup` expects.
    /// `None` when nothing is currently open under that id.
    #[must_use]
    pub fn lookup_pending(&self, gate_id: &str) -> Option<ValidatedOperatorPayload> {
        let queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        let (item, _opened_at) = queue.open_item(gate_id)?;
        // The item's payload was already validated once, at render time
        // (task 1, inside `drain`); re-validating against the same limits
        // is idempotent and is the only way to reconstruct the
        // `ValidatedOperatorPayload` wrapper `validate` gate-keeps.
        validate(item.payload.clone(), &self.limits).ok()
    }

    /// Resolve one operator verdict end to end — the shape bastion's
    /// `VerdictSink` expects, modulo the argument type (see
    /// [`ApproveAndRunVerdict`]'s doc comment for why): task 4's
    /// [`verdict::decide`] against whatever is currently open for
    /// `verdict.gate_id`, and — only on a matched `Approved` authorization —
    /// task 5's execution via [`graph::ApproveAndRunExecuteNode`] against
    /// the stored payload from the pending-harvest record, never a
    /// re-derived one.
    pub async fn resolve_verdict(
        &self,
        verdict: ApproveAndRunVerdict,
    ) -> Result<ApproveAndRunVerdictResolution, ApproveAndRunSeamError> {
        let (item, delivered_at) = {
            let queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
            queue
                .open_item(&verdict.gate_id)
                .map(|(item, at)| (item.clone(), at))
                .ok_or_else(|| ApproveAndRunSeamError::UnknownGate(verdict.gate_id.clone()))?
        };
        let record = {
            let store = self.records.lock().unwrap_or_else(|e| e.into_inner());
            store
                .get(&verdict.gate_id)
                .cloned()
                .ok_or_else(|| ApproveAndRunSeamError::UnknownGate(verdict.gate_id.clone()))?
        };

        let outcome = {
            let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
            decide(
                &mut queue,
                &item,
                &record,
                &verdict.presented_digest,
                &verdict.option_key,
                self.ledger.as_ref(),
                verdict.who.clone(),
                delivered_at,
                verdict.decided_at,
            )
            .map_err(ApproveAndRunSeamError::UnknownOption)?
        };

        let executed = if let Some(auth) = outcome.authorization.as_ref() {
            let node = ApproveAndRunExecuteNode::new()
                .with_http_post(self.http_post.clone())
                .with_policy(self.policy.clone());
            let ctx = TaskContext {
                event: execution_event(Some(auth)),
                nodes: HashMap::new(),
                metadata: serde_json::json!({}),
                node_runs: HashMap::new(),
            };
            let ctx = node
                .process(ctx)
                .await
                .map_err(ApproveAndRunSeamError::Execution)?;
            ctx.nodes.get(APPROVE_AND_RUN_NODE_NAME).cloned()
        } else {
            None
        };

        Ok(ApproveAndRunVerdictResolution { outcome, executed })
    }
}

#[cfg(test)]
mod seams_tests {
    use super::*;
    use crate::nodes::harvest_gate::pending_harvest_record;
    use crate::nodes::http_post::StubHttpPost;
    use crate::operator::ledger::InMemoryApprovalLedger;
    use crate::operator::queue::OperatorQueuePolicy;
    use chrono::TimeZone;
    use serde_json::json;

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap()
    }

    fn record(artifact_id: &str) -> PendingHarvestRecord {
        let value = pending_harvest_record(
            artifact_id,
            "https://synapse.example/ingest/learning-artifact",
            json!({"title": "some artifact", "body": "content"}),
            vec!["docs/foo.md".to_string()],
        );
        PendingHarvestRecord::from_value(&value).expect("record parses")
    }

    fn seams(http_post: Arc<dyn HttpPost>) -> (ApproveAndRunSeams, Arc<InMemoryApprovalLedger>) {
        let queue = Arc::new(Mutex::new(OperatorQueue::new(
            OperatorQueuePolicy::default(),
        )));
        let ledger = Arc::new(InMemoryApprovalLedger::new());
        let seams = ApproveAndRunSeams::new(
            queue,
            ledger.clone(),
            http_post,
            OperatorPayloadLimits::default(),
            ApproveAndRunPolicy::default(),
        );
        (seams, ledger)
    }

    #[test]
    fn lookup_pending_returns_the_payload_for_an_open_gate() {
        let stub: Arc<dyn HttpPost> = Arc::new(StubHttpPost::succeeding(json!({"ok": true})));
        let (seams, _ledger) = seams(stub);

        let report = seams.drain(&[record("artifact-1")], ts(0));
        let delivered = report.delivered.expect("one item delivered");

        let found = seams
            .lookup_pending(&delivered.item_id)
            .expect("open gate should resolve");
        assert_eq!(found.payload().gate_id, delivered.item_id);
    }

    #[test]
    fn lookup_pending_returns_none_for_unknown_gate() {
        let stub: Arc<dyn HttpPost> = Arc::new(StubHttpPost::succeeding(json!({"ok": true})));
        let (seams, _ledger) = seams(stub);

        assert!(seams.lookup_pending("nonexistent").is_none());
    }

    #[tokio::test]
    async fn resolve_verdict_drives_an_approve_end_to_end_with_one_ledger_row_and_one_post() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let stub_dyn: Arc<dyn HttpPost> = Arc::new(stub.clone());
        let (seams, ledger) = seams(stub_dyn);

        let report = seams.drain(&[record("artifact-1")], ts(0));
        let delivered = report.delivered.expect("one item delivered");

        let resolution = seams
            .resolve_verdict(ApproveAndRunVerdict {
                gate_id: delivered.item_id.clone(),
                presented_digest: delivered.payload.digest.clone(),
                option_key: OPTION_APPROVE.to_string(),
                who: "operator-a".to_string(),
                decided_at: ts(10),
            })
            .await
            .expect("approve should resolve");

        assert!(resolution.outcome.ledger_outcome.should_execute);
        assert_eq!(ledger.read_all().len(), 1);
        let executed = resolution.executed.expect("execution result present");
        assert_eq!(executed["posted"], json!(true));
        let (url, body) = stub.last_call().expect("exactly one POST should occur");
        assert_eq!(url, "https://synapse.example/ingest/learning-artifact");
        assert_eq!(body, json!({"title": "some artifact", "body": "content"}));
    }

    #[tokio::test]
    async fn resolve_verdict_requeues_a_mismatched_digest_with_zero_posts() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let stub_dyn: Arc<dyn HttpPost> = Arc::new(stub.clone());
        let (seams, ledger) = seams(stub_dyn);

        let report = seams.drain(&[record("artifact-1")], ts(0));
        let delivered = report.delivered.expect("one item delivered");

        let resolution = seams
            .resolve_verdict(ApproveAndRunVerdict {
                gate_id: delivered.item_id.clone(),
                presented_digest: "a-different-digest-than-was-delivered".to_string(),
                option_key: OPTION_APPROVE.to_string(),
                who: "operator-a".to_string(),
                decided_at: ts(10),
            })
            .await
            .expect("mismatched digest should still resolve, as a requeue");

        assert!(!resolution.outcome.ledger_outcome.should_execute);
        assert!(resolution.outcome.requeued);
        assert_eq!(ledger.read_all().len(), 1);
        assert!(resolution.executed.is_none());
        assert!(
            stub.last_call().is_none(),
            "a digest mismatch must never POST"
        );
    }

    #[tokio::test]
    async fn resolve_verdict_unknown_gate_id_errors() {
        let stub: Arc<dyn HttpPost> = Arc::new(StubHttpPost::succeeding(json!({"ok": true})));
        let (seams, _ledger) = seams(stub);

        let err = seams
            .resolve_verdict(ApproveAndRunVerdict {
                gate_id: "nonexistent".to_string(),
                presented_digest: "whatever".to_string(),
                option_key: OPTION_APPROVE.to_string(),
                who: "operator-a".to_string(),
                decided_at: ts(10),
            })
            .await
            .expect_err("unknown gate_id should error");

        assert_eq!(
            err,
            ApproveAndRunSeamError::UnknownGate("nonexistent".to_string())
        );
    }

    #[test]
    fn seams_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ApproveAndRunSeams>();
    }

    #[test]
    fn pending_harvest_records_from_content_pipeline_run_round_trips_through_drain() {
        // Build a `ctx.nodes`-shaped fixture matching what persist_to_brain.rs's
        // `Defer` arm stamps under `PersistToBrainNode`'s result — the same
        // shape `approval_mode_makes_zero_posts_and_stamps_a_pending_record`
        // (`crate::workflows::content_pipeline::persist_to_brain`) fixtures.
        let pending = pending_harvest_record(
            "artifact-1",
            "https://brain.example/ingest/artifact",
            json!({"title": "some artifact", "body": "content"}),
            vec!["brain/content/learning/artifact-1.md".to_string()],
        );
        let mut nodes: HashMap<String, serde_json::Value> = HashMap::new();
        nodes.insert(
            CONTENT_PIPELINE_PERSIST_TO_BRAIN_NODE_NAME.to_string(),
            json!({
                "posted": false,
                "skipped": true,
                "harvest_mode": "approval",
                "pending": pending,
            }),
        );

        let records = pending_harvest_records_from_content_pipeline_run(&nodes)
            .expect("stamped pending record should parse");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].artifact_id, "artifact-1");

        let stub: Arc<dyn HttpPost> = Arc::new(StubHttpPost::succeeding(json!({"ok": true})));
        let (seams, _ledger) = seams(stub);

        let report = seams.drain(&records, ts(0));
        let delivered = report.delivered.expect("one item delivered");

        let found = seams
            .lookup_pending(&delivered.item_id)
            .expect("open gate should resolve");
        assert_eq!(found.payload().gate_id, delivered.item_id);
    }

    #[test]
    fn pending_harvest_records_from_content_pipeline_run_is_empty_when_pending_is_null() {
        let mut nodes: HashMap<String, serde_json::Value> = HashMap::new();
        nodes.insert(
            CONTENT_PIPELINE_PERSIST_TO_BRAIN_NODE_NAME.to_string(),
            json!({"posted": true, "skipped": false, "harvest_mode": "in_process", "pending": null}),
        );

        let records = pending_harvest_records_from_content_pipeline_run(&nodes)
            .expect("null pending should parse to empty");
        assert!(records.is_empty());
    }

    #[test]
    fn pending_harvest_records_from_content_pipeline_run_is_empty_when_node_missing() {
        let nodes: HashMap<String, serde_json::Value> = HashMap::new();
        let records = pending_harvest_records_from_content_pipeline_run(&nodes)
            .expect("missing node should parse to empty");
        assert!(records.is_empty());
    }
}
