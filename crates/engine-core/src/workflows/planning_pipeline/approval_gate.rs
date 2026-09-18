//! `ApprovalGateNode` — `PLANNING_PIPELINE`'s per-stage approval gate
//! (`EN.19.D` task 4), built directly on the existing, generic
//! `crate::operator::{payload, channel, queue, ledger, transport, limits}`
//! primitives — the same seam `crate::workflows::approve_and_run` (`EN.8.D`)
//! already composes for harvest approval. This node never shells a CLI and
//! never introduces a second queue or approval mechanism (see the block
//! record's `why` field).
//!
//! Two halves, mirroring `approve_and_run`'s `render`/`verdict` split:
//!
//! - **Before suspend** ([`ApprovalGateNode`] itself, a [`Node`]): when the
//!   just-completed stage's resolved [`ApprovalMode`] is [`ApprovalMode::Auto`],
//!   `process` is an in-place no-op passthrough (mirroring
//!   `SuspendNode::with_enabled(false)`'s convention — the node stays in the
//!   declared graph at every policy setting, standing rule 6). When
//!   [`ApprovalMode::Manual`], it renders this module's own
//!   [`PLANNING_PIPELINE_APPROVE`]/[`PLANNING_PIPELINE_REJECT`]/
//!   [`PLANNING_PIPELINE_DISCUSS`] options into an [`OperatorPayload`],
//!   validates it, enqueues it onto the shared [`OperatorQueue`], and
//!   requests suspension via the existing [`SuspendNode`] primitive
//!   (composed by field, never re-implemented).
//! - **On resume** ([`resolve_verdict`], a free function — task 7/8 wire the
//!   `POST /events/{run_id}/resume` caller to it, mirroring
//!   `ApproveAndRunSeams::resolve_verdict`): resolves the operator's tapped
//!   option against whatever this gate's `gate_id` currently has open on the
//!   queue, and records exactly one row through
//!   `operator::ledger::record_decision` — approve/reject/discuss are
//!   recorded as the three distinct [`LedgerDecision`] variants task 1
//!   decided (`Approved`/`Rejected`/`RoutedToDiscussion`), never conflated. A
//!   stale/mismatched digest is enforced by `record_decision` itself (never
//!   re-implemented here): the written row is always
//!   [`LedgerDecision::Requeued`] and the item goes back onto the queue's
//!   pending set, exactly as `approve_and_run::verdict::decide` does for its
//!   own digest-mismatch case.

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use engine_contract::TaskContext;
use serde_json::json;

use crate::node::{Node, NodeError};
use crate::nodes::suspend::SuspendNode;
use crate::operator::channel::OperatorChannel;
use crate::operator::ledger::{
    record_decision, ApprovalLedger, LedgerDecision, RecordDecisionOutcome,
};
use crate::operator::queue::{ItemSource, OperatorQueue, OperatorQueueItem};
use crate::operator::{
    validate, OperatorPayload, OperatorPayloadLimits, OperatorResponseOption,
    OperatorValidationError, ValidatedOperatorPayload,
};
use crate::workflows::planning_pipeline::policy::{
    ApprovalChannel, ApprovalGatePolicy, ApprovalMode,
};
use crate::workflows::put_result;

/// The `Node::name()` identity [`ApprovalGateNode`] runs under by default,
/// and the `ctx.nodes` key its result is stamped onto when no identity
/// override (`crate::node::NodeExt::with_identity`) is applied. Task 7's
/// graph wiring inserts one instance per adjacent stage pair, each under its
/// own `with_identity("ApprovalGateNode#<stage>")` — mirroring
/// `SuspendNode#gate-1`'s precedent — so multiple gates in one run never
/// collide in `ctx.nodes`.
pub const NODE_NAME: &str = "ApprovalGateNode";

/// This initiative's OWN named response-option keys — the single source
/// other nodes/tests import, never a string scattered per call site, and
/// never a re-import of `approve_and_run::render`'s harvest-specific
/// `OPTION_APPROVE`/`OPTION_SKIP`/`OPTION_OPEN_SESSION` consts (those mean
/// something different for that use case). Three options, well inside
/// [`OperatorPayloadLimits`]'s default 3-button ceiling.
pub const PLANNING_PIPELINE_APPROVE: &str = "planning_pipeline_approve";
pub const PLANNING_PIPELINE_REJECT: &str = "planning_pipeline_reject";
pub const PLANNING_PIPELINE_DISCUSS: &str = "planning_pipeline_discuss";

/// The fixed three-option set every `PLANNING_PIPELINE` approval gate
/// offers, in the same order every time — pure, no I/O, so [`render`] and
/// this module's tests never risk the option set drifting between a build
/// and a decode.
#[must_use]
pub fn gate_options() -> Vec<OperatorResponseOption> {
    vec![
        OperatorResponseOption::new(PLANNING_PIPELINE_APPROVE, "Approve"),
        OperatorResponseOption::new(PLANNING_PIPELINE_REJECT, "Reject"),
        OperatorResponseOption::new(PLANNING_PIPELINE_DISCUSS, "Discuss"),
    ]
}

/// The stable `gate_id`/`item_id` a `(slug, stage)` pair renders to —
/// deterministic so [`resolve_verdict`] can resolve a resumed event's
/// `gate_id` back to exactly the item [`ApprovalGateNode::process`]
/// enqueued for it.
#[must_use]
pub fn gate_id_for(slug: &str, stage: &str) -> String {
    format!("planning-pipeline:{slug}:{stage}")
}

/// Render the (unvalidated) [`OperatorPayload`] a completed `stage` gate
/// offers the operator for `slug` — spec Invariant 1: "a decision, never a
/// task."
#[must_use]
pub fn render(slug: &str, stage: &str) -> OperatorPayload {
    OperatorPayload::new(
        gate_id_for(slug, stage),
        format!(
            "PLANNING_PIPELINE '{slug}': stage '{stage}' completed. Approve to continue, reject to \
             stop, or discuss for more back-and-forth before deciding."
        ),
        gate_options(),
    )
}

/// [`render`], then [`validate`] against `limits` — the only path to a
/// [`ValidatedOperatorPayload`], mirroring
/// `approve_and_run::render::render_and_validate`'s shape.
pub fn render_and_validate(
    slug: &str,
    stage: &str,
    limits: &OperatorPayloadLimits,
) -> Result<ValidatedOperatorPayload, OperatorValidationError> {
    validate(render(slug, stage), limits)
}

/// `PLANNING_PIPELINE`'s per-stage approval gate. See the module docs for
/// the two-halves split — this type is the "before suspend" half.
pub struct ApprovalGateNode {
    /// Which just-completed stage this instance gates
    /// (`"pre_plan"`/`"plan"`/`"generate_tasks"`/`"dispatch"`) — the key
    /// [`ApprovalGatePolicy::mode_for_stage`] reads.
    stage: String,
    policy: ApprovalGatePolicy,
    queue: Arc<Mutex<OperatorQueue>>,
    limits: OperatorPayloadLimits,
    /// Enqueue priority handed to [`OperatorQueueItem::new`] — this crate
    /// never computes an effective priority itself (`queue::item`'s module
    /// doc), so the caller supplies it.
    priority: i32,
    now: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
}

impl ApprovalGateNode {
    /// Construct a gate for `stage`, sharing `queue` with every other gate
    /// (and the resumed [`resolve_verdict`] caller) in the same run —
    /// `crate::workflows::approve_and_run::ApproveAndRunSeams`'s same
    /// `Arc<Mutex<OperatorQueue>>` convention. Defaults: builtin
    /// [`ApprovalGatePolicy::default`] (safe-by-default `Manual`), builtin
    /// [`OperatorPayloadLimits::default`], priority `0`, clock `Utc::now`.
    #[must_use]
    pub fn new(stage: impl Into<String>, queue: Arc<Mutex<OperatorQueue>>) -> Self {
        Self {
            stage: stage.into(),
            policy: ApprovalGatePolicy::default(),
            queue,
            limits: OperatorPayloadLimits::default(),
            priority: 0,
            now: Arc::new(Utc::now),
        }
    }

    /// Override the resolved policy (task 2's [`ApprovalGatePolicy`],
    /// resolved through the standard four-layer precedence by the caller —
    /// this node never re-resolves it itself).
    #[must_use]
    pub fn with_policy(mut self, policy: ApprovalGatePolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Override the [`OperatorPayloadLimits`] this gate's payload is
    /// validated against.
    #[must_use]
    pub fn with_limits(mut self, limits: OperatorPayloadLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Override the enqueue priority.
    #[must_use]
    pub fn with_priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }

    /// Override the clock `process` reads `enqueued_at` from. Tests inject a
    /// fixed instant so the queue's ordering assertions are hermetic.
    #[must_use]
    pub fn with_now(mut self, now: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>) -> Self {
        self.now = now;
        self
    }

    /// Read the dispatched event's `slug`, defaulting to `"unknown"` rather
    /// than erroring — a missing slug degrades the rendered summary's
    /// specificity but must not block a run that otherwise has nothing to
    /// gate on it (task 3's `StageSelectorNode`, not this node, is the
    /// event-shape gate).
    fn slug(ctx: &TaskContext) -> String {
        ctx.event
            .get("slug")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
            .to_string()
    }

    /// The [`OperatorChannel`] this gate enqueues under, per the resolved
    /// policy's `approval_channel` — `Session` names the run's own `slug` as
    /// the operator session, per `EN.19.E`'s interactive-terminal-nodes
    /// split (the block record's `what` field).
    fn channel(&self, slug: &str) -> OperatorChannel {
        match self.policy.approval_channel {
            ApprovalChannel::Notification => OperatorChannel::Notification,
            ApprovalChannel::Session => OperatorChannel::session(slug.to_string()),
        }
    }
}

#[async_trait::async_trait]
impl Node for ApprovalGateNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let mut ctx = ctx;
        let slug = Self::slug(&ctx);
        let mode = self.policy.mode_for_stage(&self.stage);

        if mode == ApprovalMode::Auto {
            put_result(
                &mut ctx,
                self.name(),
                json!({
                    "stage": self.stage,
                    "approval_mode": "auto",
                    "suspended": false,
                }),
            );
            return Ok(ctx);
        }

        let gate_id = gate_id_for(&slug, &self.stage);
        let validated = render_and_validate(&slug, &self.stage, &self.limits).map_err(|err| {
            NodeError::new(format!(
                "{}: stage '{}' payload failed validation: {err}",
                self.name(),
                self.stage
            ))
        })?;

        let channel = self.channel(&slug);
        let item = OperatorQueueItem::new(
            gate_id.clone(),
            validated.into_payload(),
            self.priority,
            (self.now)(),
            ItemSource::GateApproval,
        );
        {
            let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
            queue.enqueue(item);
        }

        // Request suspension via the existing SuspendNode primitive — never
        // a hand-rolled `metadata.suspension` write here.
        let suspend = SuspendNode::new(format!("{}#suspend", self.name()))
            .with_enabled(true)
            .with_reason_label(format!(
                "awaiting operator approval for PLANNING_PIPELINE stage '{}'",
                self.stage
            ));
        ctx = suspend.process(ctx).await?;

        put_result(
            &mut ctx,
            self.name(),
            json!({
                "stage": self.stage,
                "approval_mode": "manual",
                "gate_id": gate_id,
                "channel": if channel.is_session() { "session" } else { "notification" },
                "suspended": true,
            }),
        );
        Ok(ctx)
    }

    fn name(&self) -> &str {
        NODE_NAME
    }
}

/// Why [`resolve_verdict`] could not resolve a verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalVerdictError {
    /// `gate_id` has nothing currently open on the queue — either it was
    /// never enqueued by [`ApprovalGateNode::process`], or it was already
    /// answered by an earlier [`resolve_verdict`] call.
    UnknownGate(String),
    /// The tapped option key was not one of [`PLANNING_PIPELINE_APPROVE`]/
    /// [`PLANNING_PIPELINE_REJECT`]/[`PLANNING_PIPELINE_DISCUSS`].
    UnknownOption(String),
}

impl std::fmt::Display for ApprovalVerdictError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApprovalVerdictError::UnknownGate(gate_id) => {
                write!(f, "unknown gate_id: {gate_id:?}")
            }
            ApprovalVerdictError::UnknownOption(key) => {
                write!(f, "unknown operator response option key: {key:?}")
            }
        }
    }
}

impl std::error::Error for ApprovalVerdictError {}

/// What [`resolve_verdict`] tells the caller after resolving one operator
/// tap.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalVerdictOutcome {
    /// The ledger row `record_decision` wrote, plus whether it authorizes
    /// continuation (`should_execute`, `true` only for a matched-digest
    /// `Approved` row).
    pub ledger_outcome: RecordDecisionOutcome,
    /// `true` iff the presented digest did not match the delivered digest —
    /// the row is [`LedgerDecision::Requeued`] and the item was pushed back
    /// onto the queue's pending set, never dropped.
    pub requeued: bool,
}

impl ApprovalVerdictOutcome {
    /// The stage may continue to the next one — a matched `Approved`
    /// verdict, and only that.
    #[must_use]
    pub fn should_continue(&self) -> bool {
        self.ledger_outcome.should_execute
    }

    /// The operator explicitly declined — the run holds in a distinct
    /// terminal state, never silently treated as approve.
    #[must_use]
    pub fn is_rejected(&self) -> bool {
        self.ledger_outcome.row.decision == LedgerDecision::Rejected
    }

    /// The operator wants more back-and-forth — routes to `EN.19.E`'s
    /// `DiscussFurtherNode` seam, never silently treated as approve.
    #[must_use]
    pub fn is_discuss(&self) -> bool {
        self.ledger_outcome.row.decision == LedgerDecision::RoutedToDiscussion
    }
}

/// Resolve one operator verdict for `gate_id`, delivered off `queue`, into
/// exactly one row via `operator::ledger::record_decision` — this module's
/// "on resume" half. See the module docs for how this composes with
/// `ApprovalGateNode::process`'s "before suspend" half.
///
/// Always answers `gate_id` off `queue`'s open slot once a known option key
/// is resolved (the decision has been made, one way or another — mirroring
/// `approve_and_run::verdict::decide`). When the recorded decision is
/// [`LedgerDecision::Requeued`] (a digest mismatch), the item is pushed back
/// onto `queue`'s pending set instead of being dropped.
///
/// Returns [`ApprovalVerdictError::UnknownGate`] when nothing is currently
/// open under `gate_id`, and [`ApprovalVerdictError::UnknownOption`] when
/// `option_key` is not one of this module's three named consts — in both
/// cases no ledger row is written and `queue` is left untouched.
#[allow(clippy::too_many_arguments)]
pub fn resolve_verdict(
    queue: &mut OperatorQueue,
    ledger: &dyn ApprovalLedger,
    gate_id: &str,
    presented_digest: &str,
    option_key: &str,
    who: impl Into<String>,
    decided_at: DateTime<Utc>,
) -> Result<ApprovalVerdictOutcome, ApprovalVerdictError> {
    let (item, delivered_at) = queue
        .open_item(gate_id)
        .map(|(item, at)| (item.clone(), at))
        .ok_or_else(|| ApprovalVerdictError::UnknownGate(gate_id.to_string()))?;

    let requested_decision = match option_key {
        PLANNING_PIPELINE_APPROVE => LedgerDecision::Approved,
        PLANNING_PIPELINE_REJECT => LedgerDecision::Rejected,
        PLANNING_PIPELINE_DISCUSS => LedgerDecision::RoutedToDiscussion,
        other => return Err(ApprovalVerdictError::UnknownOption(other.to_string())),
    };

    let ledger_outcome = record_decision(
        ledger,
        item.item_id.clone(),
        item.payload.digest.clone(),
        presented_digest,
        item.payload.rendered_summary.clone(),
        requested_decision,
        who,
        delivered_at,
        decided_at,
    );

    queue.answer(&item.item_id);

    let requeued = ledger_outcome.row.decision == LedgerDecision::Requeued;
    if requeued {
        queue.enqueue(item.clone());
    }

    Ok(ApprovalVerdictOutcome {
        ledger_outcome,
        requeued,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeExt;
    use crate::operator::ledger::InMemoryApprovalLedger;
    use crate::operator::queue::OperatorQueuePolicy;
    use crate::workflows::get_result;
    use chrono::TimeZone;
    use std::collections::HashMap;

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap()
    }

    fn ctx(event: serde_json::Value) -> TaskContext {
        TaskContext {
            event,
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        }
    }

    fn queue() -> Arc<Mutex<OperatorQueue>> {
        Arc::new(Mutex::new(OperatorQueue::new(
            OperatorQueuePolicy::default(),
        )))
    }

    #[test]
    fn gate_options_are_the_three_named_consts_within_default_limits() {
        let options = gate_options();
        assert_eq!(options.len(), 3);
        assert_eq!(options[0].key, PLANNING_PIPELINE_APPROVE);
        assert_eq!(options[1].key, PLANNING_PIPELINE_REJECT);
        assert_eq!(options[2].key, PLANNING_PIPELINE_DISCUSS);
        assert!(options.len() <= OperatorPayloadLimits::default().max_options);
    }

    #[test]
    fn render_and_validate_succeeds_under_default_limits() {
        let validated =
            render_and_validate("my-slug", "pre_plan", &OperatorPayloadLimits::default())
                .expect("default three-option payload validates");
        assert_eq!(
            validated.payload().gate_id,
            gate_id_for("my-slug", "pre_plan")
        );
    }

    #[tokio::test]
    async fn auto_mode_is_a_pure_no_op_passthrough() {
        let mut approval = HashMap::new();
        approval.insert("pre_plan".to_string(), ApprovalMode::Auto);
        let policy = ApprovalGatePolicy {
            approval,
            ..ApprovalGatePolicy::default()
        };
        let q = queue();
        let node = ApprovalGateNode::new("pre_plan", q.clone()).with_policy(policy);

        let out = node
            .process(ctx(json!({"slug": "my-slug"})))
            .await
            .expect("auto mode never fails");

        assert!(!crate::suspend::suspension_requested(&out.metadata));
        assert_eq!(q.lock().unwrap().pending_count(), 0);
        let stamped = get_result(&out, NODE_NAME).expect("result stamped");
        assert_eq!(stamped["approval_mode"], json!("auto"));
        assert_eq!(stamped["suspended"], json!(false));
    }

    #[tokio::test]
    async fn manual_mode_enqueues_through_the_existing_queue_and_suspends() {
        let q = queue();
        let node = ApprovalGateNode::new("pre_plan", q.clone()).with_now(Arc::new(|| ts(0)));

        let out = node
            .process(ctx(json!({"slug": "my-slug"})))
            .await
            .expect("manual mode enqueues and suspends");

        assert!(crate::suspend::suspension_requested(&out.metadata));
        {
            let mut locked = q.lock().unwrap();
            let delivered = locked
                .next_deliverable(ts(0))
                .expect("the enqueued item is deliverable");
            assert_eq!(delivered.item_id, gate_id_for("my-slug", "pre_plan"));
            assert_eq!(delivered.payload.options.len(), 3);
        }
        let stamped = get_result(&out, NODE_NAME).expect("result stamped");
        assert_eq!(stamped["approval_mode"], json!("manual"));
        assert_eq!(stamped["suspended"], json!(true));
        assert_eq!(stamped["channel"], json!("notification"));
    }

    #[tokio::test]
    async fn session_channel_is_reported_when_policy_selects_it() {
        let q = queue();
        let policy = ApprovalGatePolicy {
            approval_channel: ApprovalChannel::Session,
            ..ApprovalGatePolicy::default()
        };
        let node = ApprovalGateNode::new("plan", q.clone())
            .with_policy(policy)
            .with_now(Arc::new(|| ts(0)));

        let out = node
            .process(ctx(json!({"slug": "my-slug"})))
            .await
            .expect("manual mode with session channel succeeds");

        let stamped = get_result(&out, NODE_NAME).expect("result stamped");
        assert_eq!(stamped["channel"], json!("session"));
    }

    #[tokio::test]
    async fn identity_override_relabels_the_stamp_for_multiple_gate_instances() {
        let q = queue();
        let mut approval = HashMap::new();
        approval.insert("plan".to_string(), ApprovalMode::Auto);
        let policy = ApprovalGatePolicy {
            approval,
            ..ApprovalGatePolicy::default()
        };
        let node = ApprovalGateNode::new("plan", q)
            .with_policy(policy)
            .with_identity("ApprovalGateNode#plan");

        let out = node
            .process(ctx(json!({"slug": "my-slug"})))
            .await
            .expect("auto mode never fails");

        assert!(get_result(&out, NODE_NAME).is_none());
        let stamped = get_result(&out, "ApprovalGateNode#plan").expect("relabeled result stamped");
        assert_eq!(stamped["stage"], json!("plan"));
    }

    fn deliver(
        q: &Arc<Mutex<OperatorQueue>>,
        slug: &str,
        stage: &str,
        at: DateTime<Utc>,
    ) -> String {
        let validated = render_and_validate(slug, stage, &OperatorPayloadLimits::default())
            .expect("renders and validates");
        let item = OperatorQueueItem::new(
            validated.payload().gate_id.clone(),
            validated.into_payload(),
            0,
            at,
            ItemSource::GateApproval,
        );
        let gate_id = item.item_id.clone();
        let mut locked = q.lock().unwrap();
        locked.enqueue(item);
        locked.next_deliverable(at).expect("item delivered");
        gate_id
    }

    #[test]
    fn resolve_verdict_approve_records_approved_and_authorizes_continuation() {
        let q = queue();
        let gate_id = deliver(&q, "my-slug", "pre_plan", ts(0));
        let ledger = InMemoryApprovalLedger::new();

        let outcome = resolve_verdict(
            &mut q.lock().unwrap(),
            &ledger,
            &gate_id,
            &render("my-slug", "pre_plan").digest,
            PLANNING_PIPELINE_APPROVE,
            "operator-a",
            ts(10),
        )
        .expect("approve is a known option");

        assert!(outcome.should_continue());
        assert!(!outcome.is_rejected());
        assert!(!outcome.is_discuss());
        assert_eq!(ledger.read_all().len(), 1);
        assert_eq!(ledger.read_all()[0].decision, LedgerDecision::Approved);
    }

    #[test]
    fn resolve_verdict_reject_is_recorded_distinctly_and_never_authorizes() {
        let q = queue();
        let gate_id = deliver(&q, "my-slug", "pre_plan", ts(0));
        let ledger = InMemoryApprovalLedger::new();

        let outcome = resolve_verdict(
            &mut q.lock().unwrap(),
            &ledger,
            &gate_id,
            &render("my-slug", "pre_plan").digest,
            PLANNING_PIPELINE_REJECT,
            "operator-a",
            ts(10),
        )
        .expect("reject is a known option");

        assert!(!outcome.should_continue());
        assert!(outcome.is_rejected());
        assert!(!outcome.is_discuss());
        assert_eq!(ledger.read_all()[0].decision, LedgerDecision::Rejected);
    }

    #[test]
    fn resolve_verdict_discuss_is_recorded_distinctly_and_never_authorizes() {
        let q = queue();
        let gate_id = deliver(&q, "my-slug", "pre_plan", ts(0));
        let ledger = InMemoryApprovalLedger::new();

        let outcome = resolve_verdict(
            &mut q.lock().unwrap(),
            &ledger,
            &gate_id,
            &render("my-slug", "pre_plan").digest,
            PLANNING_PIPELINE_DISCUSS,
            "operator-a",
            ts(10),
        )
        .expect("discuss is a known option");

        assert!(!outcome.should_continue());
        assert!(!outcome.is_rejected());
        assert!(outcome.is_discuss());
        assert_eq!(
            ledger.read_all()[0].decision,
            LedgerDecision::RoutedToDiscussion
        );
    }

    #[test]
    fn resolve_verdict_stale_digest_is_refused_and_requeued_never_authorizing() {
        let q = queue();
        let gate_id = deliver(&q, "my-slug", "pre_plan", ts(0));
        let ledger = InMemoryApprovalLedger::new();

        let outcome = resolve_verdict(
            &mut q.lock().unwrap(),
            &ledger,
            &gate_id,
            "a-stale-digest-not-what-was-delivered",
            PLANNING_PIPELINE_APPROVE,
            "operator-a",
            ts(10),
        )
        .expect("a stale digest still resolves, as a requeue");

        assert!(!outcome.should_continue());
        assert!(outcome.requeued);
        assert_eq!(ledger.read_all()[0].decision, LedgerDecision::Requeued);
        let locked = q.lock().unwrap();
        assert_eq!(locked.pending_count(), 1);
        assert_eq!(locked.open_count(), 0);
        // Never authorizes continuation even though the option key was approve.
        assert!(locked.open_item(&gate_id).is_none());
    }

    #[test]
    fn resolve_verdict_unknown_gate_errors_and_writes_nothing() {
        let q = queue();
        let ledger = InMemoryApprovalLedger::new();

        let err = resolve_verdict(
            &mut q.lock().unwrap(),
            &ledger,
            "nonexistent",
            "whatever",
            PLANNING_PIPELINE_APPROVE,
            "operator-a",
            ts(10),
        )
        .expect_err("unknown gate_id must error");

        assert_eq!(
            err,
            ApprovalVerdictError::UnknownGate("nonexistent".to_string())
        );
        assert!(ledger.read_all().is_empty());
    }

    #[test]
    fn resolve_verdict_unknown_option_errors_and_writes_nothing() {
        let q = queue();
        let gate_id = deliver(&q, "my-slug", "pre_plan", ts(0));
        let ledger = InMemoryApprovalLedger::new();

        let err = resolve_verdict(
            &mut q.lock().unwrap(),
            &ledger,
            &gate_id,
            &render("my-slug", "pre_plan").digest,
            "not_a_real_option",
            "operator-a",
            ts(10),
        )
        .expect_err("unknown option key must error");

        assert_eq!(
            err,
            ApprovalVerdictError::UnknownOption("not_a_real_option".to_string())
        );
        assert!(ledger.read_all().is_empty());
        // The item is still open — an unresolved verdict does not touch the queue.
        assert!(q.lock().unwrap().open_item(&gate_id).is_some());
    }
}
