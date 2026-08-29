//! `DEBRIEF` — a morning brief rendered from a finished campaign's journal
//! rows (`EN.12.G`).
//!
//! Task 1 of this block lands the `JournalReader` seam alone: `engine-core`
//! depends ONLY on `engine-contract` (`crates/engine-core/Cargo.toml`), so
//! it cannot call `engine_store::list_journal_rows_for_campaign` directly —
//! that function lives behind `engine-serve`, which depends on all three
//! crates. The debrief therefore needs an injectable read seam, exactly the
//! reason [`crate::nodes::brain_client::HttpGet`] and
//! [`crate::nodes::http_post::HttpPost`] are injectable rather than direct
//! calls.
//!
//! [`JournalReader`] is the trait; [`StubJournalReader`] is the hermetic
//! test double every debrief test runs against (the gated `cargo nextest`
//! suite never contacts Postgres, mirroring `StubHttpGet`/`StubHttpPost`).
//! The only production implementation lives in `engine-serve`
//! (`crate::journal::journal_reader_live`), wired in by `EN.12.G` task 4's
//! `register_debrief`.
//!
//! # `DebriefNode` (task 3)
//!
//! Campaign id in (from `ctx.event`, mirroring
//! [`crate::nodes::brain_client::RecallNode::resolve_query`]'s two-shape
//! extraction), journal rows fetched through [`JournalReader`], rendered by
//! [`render_brief`] into one deterministic text digest (steps in
//! `created_at` order, every bail named with its reason).
//!
//! **The brief IS the deterministic digest.** `ChannelTransport::send`
//! (`crate::nodes::channel_transport::WorkflowTriggerDispatch`) is a
//! fire-and-forget seam by design — even its in-process `Dispatcher` path
//! runs the dispatched workflow on a detached `spawn_blocking` thread and
//! discards the result (see that module's `send` doc comment), matching
//! every other `OutboundBody::TriggerWorkflow` caller in this crate
//! (`ResearchIngressDispatchNode`, `ActionDispatchNode`). There is no
//! synchronous path back from a dispatched `CONTENT_PIPELINE` run to this
//! node. So `DebriefNode` dispatches the rendered digest to
//! `CONTENT_PIPELINE` for downstream delivery (the `EN.6.x` channel
//! adapters own actually getting it to a phone — out of scope here, per
//! the block record), and **separately, synchronously** writes that same
//! digest back as the `DebriefRendered` journal row — the text this node
//! wrote is the text a reader gets back from `GET /campaigns/{id}/journal`,
//! with nothing lost to an unawaited child run. This is what makes AC2's
//! "enforced in code, not left to the summariser" possible at all:
//! [`brief_names_every_bail`] checks the text this node itself produced,
//! before it is ever written.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use engine_contract::{
    ChannelType, IngressEnvelope, JournalDecisionKind, JournalRow, SourcePayload, TaskContext,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::node::{Node, NodeError};
use crate::nodes::channel_transport::{
    receipt_from_send_result, ChannelTransport, OutboundAction, OutboundBody,
};
use crate::workflow::read_run_id;
use crate::workflows::orchestration::integrate::JournalSinkFn;
use crate::workflows::put_result;

/// The injectable journal-read seam: `rows_for_campaign(campaign_id)` -> the
/// campaign's journal rows on success, or an error string describing the
/// read failure. Patterned on [`crate::nodes::brain_client::HttpGet`]: a
/// trait so production code (`engine-serve`) reaches for a real
/// Postgres-backed reader while `engine-core` tests inject a
/// [`StubJournalReader`] instead.
#[async_trait]
pub trait JournalReader: Send + Sync {
    async fn rows_for_campaign(&self, campaign_id: &Uuid) -> Result<Vec<JournalRow>, String>;
}

/// Test-stub [`JournalReader`]: records the last campaign id it was asked
/// for and returns a configurable success/failure response, mirroring
/// [`crate::nodes::brain_client::StubHttpGet`]. The gated suite never
/// touches Postgres — every debrief test runs on this stub, never on a live
/// reader.
#[derive(Clone)]
pub struct StubJournalReader {
    last_campaign_id: Arc<Mutex<Option<Uuid>>>,
    result: Arc<Mutex<Result<Vec<JournalRow>, String>>>,
}

impl StubJournalReader {
    /// A stub that always succeeds with the given rows.
    #[must_use]
    pub fn succeeding(rows: Vec<JournalRow>) -> Self {
        Self {
            last_campaign_id: Arc::new(Mutex::new(None)),
            result: Arc::new(Mutex::new(Ok(rows))),
        }
    }

    /// A stub that always fails with the given message.
    #[must_use]
    pub fn failing(message: impl Into<String>) -> Self {
        Self {
            last_campaign_id: Arc::new(Mutex::new(None)),
            result: Arc::new(Mutex::new(Err(message.into()))),
        }
    }

    /// The campaign id the most recent call to [`JournalReader::rows_for_campaign`]
    /// was asked for, if any — lets a test assert on the outbound request
    /// shape, not just the returned rows.
    #[must_use]
    pub fn last_campaign_id(&self) -> Option<Uuid> {
        *self.last_campaign_id.lock().expect("stub mutex poisoned")
    }
}

#[async_trait]
impl JournalReader for StubJournalReader {
    async fn rows_for_campaign(&self, campaign_id: &Uuid) -> Result<Vec<JournalRow>, String> {
        *self.last_campaign_id.lock().expect("stub mutex poisoned") = Some(*campaign_id);
        self.result.lock().expect("stub mutex poisoned").clone()
    }
}

/// [`DebriefNode`]'s identity — the `ctx.nodes`/`ctx.node_runs` map key and
/// the string this module's diagnostics prefix.
pub const DEBRIEF_NODE_NAME: &str = "DebriefNode";

/// The `ctx.event` field an object-shaped campaign id is read from —
/// mirrors [`crate::nodes::brain_client`]'s `QUERY_FIELD` convention.
const CAMPAIGN_ID_FIELD: &str = "campaign_id";

/// The `Dispatcher` registry key `DebriefNode` dispatches the rendered
/// digest to by default. Overridable via
/// [`DebriefNode::with_dispatch_workflow_type`] so a test never needs a
/// literal `CONTENT_PIPELINE` registration to exercise the dispatch path.
const DEFAULT_DISPATCH_WORKFLOW_TYPE: &str = "CONTENT_PIPELINE";

/// Pull a campaign id out of a `ctx.event` JSON value: a bare JSON string
/// is parsed directly; a JSON object is read via its `"campaign_id"` field
/// (also a string). Either way the string must parse as a [`Uuid`]. Mirrors
/// [`crate::nodes::brain_client`]'s `query_from_value` two-shape
/// extraction — this is the block's stated input contract: "the campaign
/// id is the ONLY input".
fn campaign_id_from_value(value: &Value) -> Result<Uuid, NodeError> {
    let candidate = match value {
        Value::String(text) => Some(text.as_str()),
        Value::Object(_) => value.get(CAMPAIGN_ID_FIELD).and_then(Value::as_str),
        _ => None,
    };

    match candidate.map(str::trim) {
        Some(text) if !text.is_empty() => Uuid::parse_str(text).map_err(|err| {
            NodeError::new(format!(
                "{DEBRIEF_NODE_NAME}: campaign id \"{text}\" on ctx.event is not a valid UUID: \
                 {err}"
            ))
        }),
        _ => Err(NodeError::new(format!(
            "{DEBRIEF_NODE_NAME}: no non-empty campaign id found on ctx.event (expected a bare \
             JSON string, or an object with a \"{CAMPAIGN_ID_FIELD}\" string field)"
        ))),
    }
}

/// Bail-worthy [`JournalDecisionKind`]s (per the block's "THE BAIL RULE"
/// note): a debrief that hides any of these is worse than no debrief
/// (AC2). A free function, not inlined, so a test can check membership
/// directly rather than re-deriving the list.
#[must_use]
pub fn is_bail_worthy(kind: JournalDecisionKind) -> bool {
    matches!(
        kind,
        JournalDecisionKind::StepBailed
            | JournalDecisionKind::GateRefused
            | JournalDecisionKind::StateWriteVerificationFailed
            | JournalDecisionKind::BudgetHalted
    )
}

/// A short, human-scannable label for one [`JournalDecisionKind`] —
/// upper-cased for the four bail-worthy kinds so a skimmed brief cannot
/// mistake a failure for a routine line.
fn kind_label(kind: JournalDecisionKind) -> &'static str {
    match kind {
        JournalDecisionKind::StepIntegrated => "integrated",
        JournalDecisionKind::StepBailed => "BAILED",
        JournalDecisionKind::GateRefused => "GATE REFUSED",
        JournalDecisionKind::StateWriteVerificationFailed => "STATE WRITE VERIFICATION FAILED",
        JournalDecisionKind::BudgetHalted => "BUDGET HALTED",
        JournalDecisionKind::ResolvedPolicy => "resolved policy",
        JournalDecisionKind::RecallConsulted => "recall consulted",
        JournalDecisionKind::DebriefRendered => "debrief rendered",
    }
}

/// Render `rows` (a campaign's full journal) into one deterministic text
/// digest, steps in `created_at` order — the text `DebriefNode` both hands
/// to `CONTENT_PIPELINE` and writes back as the `DebriefRendered` journal
/// row's brief (see the module doc's "the brief IS the deterministic
/// digest" note).
///
/// Zero rows renders a brief that says nothing ran, never an empty
/// string — AC3's "not an empty or absent response".
///
/// Exposed (not `pub(crate)`) so a test can call it directly, or build "a
/// renderer that omits the bail" out of a stripped/hand-edited row set,
/// per `EN.12.G` task 5's load-bearing test.
#[must_use]
pub fn render_brief(rows: &[JournalRow]) -> String {
    if rows.is_empty() {
        return "No steps ran for this campaign.".to_string();
    }

    let mut ordered: Vec<&JournalRow> = rows.iter().collect();
    ordered.sort_by_key(|row| row.created_at);

    let mut lines = vec![format!("{} step(s) ran:", ordered.len())];
    for row in &ordered {
        lines.push(format!(
            "- [{}] {}: {}",
            row.step,
            kind_label(row.kind),
            row.reason
        ));
    }

    let bails: Vec<&&JournalRow> = ordered
        .iter()
        .filter(|row| is_bail_worthy(row.kind))
        .collect();
    if bails.is_empty() {
        lines.push("Clean run - every step integrated.".to_string());
    } else {
        lines.push(format!("{} step(s) did NOT complete cleanly:", bails.len()));
        for row in &bails {
            lines.push(format!(
                "- FAILURE [{}] {}: {}",
                row.step,
                kind_label(row.kind),
                row.reason
            ));
        }
    }

    lines.join("\n")
}

/// `true` iff `brief` names every bail-worthy row's reason text verbatim —
/// the code-enforced check [`DebriefNode::process`] runs before writing a
/// row, per AC2 ("the gate must fail on a debrief that hides a failure").
/// A debrief asked nicely to mention a failure is not a gate; this
/// function is what makes the check independent of how `brief` was
/// produced.
#[must_use]
pub fn brief_names_every_bail(brief: &str, rows: &[JournalRow]) -> bool {
    rows.iter()
        .filter(|row| is_bail_worthy(row.kind))
        .all(|row| brief.contains(row.reason.as_str()))
}

/// The `DEBRIEF` workflow's only node (`EN.12.G`): campaign id in, journal
/// rows fetched through [`JournalReader`], rendered by [`render_brief`],
/// dispatched to `CONTENT_PIPELINE` over [`ChannelTransport`], and written
/// back as a [`JournalDecisionKind::DebriefRendered`] row through the
/// injected [`JournalSinkFn`] — see the module doc for why the written
/// brief is the deterministic digest, not anything read back from the
/// dispatched run.
pub struct DebriefNode {
    journal_reader: Arc<dyn JournalReader>,
    transport: Arc<dyn ChannelTransport>,
    journal_sink: Option<Arc<JournalSinkFn>>,
    dispatch_workflow_type: String,
}

impl DebriefNode {
    /// Build a `DebriefNode` reading through `journal_reader` and
    /// dispatching through `transport`. No journal sink is wired by
    /// default — [`Self::with_journal_sink`] wires one; production
    /// wiring (`EN.12.G` task 4's `register_debrief`) always wires a live
    /// sink, matching `OrchestrationRunNode`'s `Option<&JournalSinkFn>`
    /// "a `None` sink is a true no-op" convention.
    #[must_use]
    pub fn new(
        journal_reader: Arc<dyn JournalReader>,
        transport: Arc<dyn ChannelTransport>,
    ) -> Self {
        Self {
            journal_reader,
            transport,
            journal_sink: None,
            dispatch_workflow_type: DEFAULT_DISPATCH_WORKFLOW_TYPE.to_string(),
        }
    }

    /// Wire the seam that records the rendered brief as a
    /// `DebriefRendered` journal row.
    #[must_use]
    pub fn with_journal_sink(mut self, journal_sink: Arc<JournalSinkFn>) -> Self {
        self.journal_sink = Some(journal_sink);
        self
    }

    /// Override the `Dispatcher` registry key the rendered digest is
    /// dispatched to. Defaults to `"CONTENT_PIPELINE"`; a test overrides
    /// this to exercise the dispatch path against a fixture workflow type
    /// without needing a real `CONTENT_PIPELINE` registration.
    #[must_use]
    pub fn with_dispatch_workflow_type(mut self, workflow_type: impl Into<String>) -> Self {
        self.dispatch_workflow_type = workflow_type.into();
        self
    }
}

#[async_trait]
impl Node for DebriefNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let campaign_id = campaign_id_from_value(&ctx.event)?;

        let rows = self
            .journal_reader
            .rows_for_campaign(&campaign_id)
            .await
            .map_err(|err| {
                NodeError::new(format!(
                    "{DEBRIEF_NODE_NAME}: failed to read journal rows for campaign \
                     {campaign_id}: {err}"
                ))
            })?;

        let brief = render_brief(&rows);

        // THE BAIL RULE, enforced in code: a brief that fails to name a
        // bailed step's reason is worse than no debrief at all, so this
        // node fails rather than writing it. See the module doc for why
        // this check is meaningful even though this node built `brief`
        // itself: it is what a later refactor of `render_brief` (or a
        // hand-constructed brief in a test) is checked against.
        if !brief_names_every_bail(&brief, &rows) {
            return Err(NodeError::new(format!(
                "{DEBRIEF_NODE_NAME}: rendered brief for campaign {campaign_id} does not name \
                 every bailed step's reason - refusing to write a brief that hides a failure"
            )));
        }

        // Dispatch the rendered digest to CONTENT_PIPELINE for downstream
        // delivery. Fire-and-forget by construction (see the module doc);
        // its receipt is stamped on this node's own result but never
        // gates the journal write below.
        let envelope = IngressEnvelope {
            envelope_id: format!("debrief:{campaign_id}"),
            channel_type: ChannelType::Schedule,
            sender_id: None,
            reply_context: None,
            timestamp: Utc::now().to_rfc3339(),
            source: SourcePayload::ChannelMessage {
                text: brief.clone(),
                attachments: vec![],
            },
            raw_payload: Value::Null,
        };
        let event = json!({ "envelope": envelope, "chain_depth": 0u64 });
        let action = OutboundAction::new(
            ChannelType::WorkflowTrigger,
            None,
            OutboundBody::TriggerWorkflow {
                workflow_type: self.dispatch_workflow_type.clone(),
                event,
            },
        );
        let receipt = receipt_from_send_result(self.transport.send(&action).await);

        let row_count = rows.len();
        let bailed_steps: Vec<Value> = rows
            .iter()
            .filter(|row| is_bail_worthy(row.kind))
            .map(|row| json!({ "step": row.step, "reason": row.reason }))
            .collect();

        if let Some(sink) = &self.journal_sink {
            let run_id = read_run_id(&ctx.metadata)
                .and_then(|s| Uuid::parse_str(&s).ok())
                .unwrap_or_else(Uuid::new_v4);
            sink(JournalRow {
                id: Uuid::new_v4(),
                campaign_id: campaign_id.to_string(),
                run_id,
                step: DEBRIEF_NODE_NAME.to_string(),
                kind: JournalDecisionKind::DebriefRendered,
                reason: "debrief rendered".to_string(),
                detail: json!({
                    "brief": brief,
                    "row_count": row_count,
                    "bailed_steps": bailed_steps,
                }),
                created_at: Utc::now(),
            });
        }

        let mut ctx = ctx;
        put_result(
            &mut ctx,
            self.name(),
            json!({
                "campaign_id": campaign_id.to_string(),
                "row_count": row_count,
                "brief": brief,
                "dispatch_receipt": receipt,
            }),
        );

        Ok(ctx)
    }

    fn name(&self) -> &str {
        DEBRIEF_NODE_NAME
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use engine_contract::JournalDecisionKind;

    use super::*;

    fn sample_row() -> JournalRow {
        JournalRow {
            id: Uuid::new_v4(),
            campaign_id: "campaign-1".to_string(),
            run_id: Uuid::new_v4(),
            step: "build".to_string(),
            kind: JournalDecisionKind::StepIntegrated,
            reason: "ok".to_string(),
            detail: serde_json::json!({}),
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn succeeding_stub_returns_configured_rows_and_records_campaign_id() {
        let campaign_id = Uuid::new_v4();
        let rows = vec![sample_row()];
        let stub = StubJournalReader::succeeding(rows.clone());

        let result = stub.rows_for_campaign(&campaign_id).await;

        assert_eq!(result, Ok(rows));
        assert_eq!(stub.last_campaign_id(), Some(campaign_id));
    }

    #[tokio::test]
    async fn failing_stub_returns_configured_error_and_records_campaign_id() {
        let campaign_id = Uuid::new_v4();
        let stub = StubJournalReader::failing("journal read failed");

        let result = stub.rows_for_campaign(&campaign_id).await;

        assert_eq!(result, Err("journal read failed".to_string()));
        assert_eq!(stub.last_campaign_id(), Some(campaign_id));
    }

    #[tokio::test]
    async fn stub_records_the_most_recent_campaign_id_across_calls() {
        let stub = StubJournalReader::succeeding(vec![]);
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();

        let _ = stub.rows_for_campaign(&first).await;
        let _ = stub.rows_for_campaign(&second).await;

        assert_eq!(stub.last_campaign_id(), Some(second));
    }

    // ── DebriefNode (task 3) ────────────────────────────────────────────

    use crate::nodes::channel_transport::StubChannelTransport;

    fn row_at(
        campaign_id: &str,
        step: &str,
        kind: JournalDecisionKind,
        reason: &str,
        offset_secs: i64,
    ) -> JournalRow {
        JournalRow {
            id: Uuid::new_v4(),
            campaign_id: campaign_id.to_string(),
            run_id: Uuid::new_v4(),
            step: step.to_string(),
            kind,
            reason: reason.to_string(),
            detail: serde_json::json!({}),
            created_at: Utc::now() + chrono::Duration::seconds(offset_secs),
        }
    }

    fn base_ctx(event: Value) -> TaskContext {
        TaskContext {
            event,
            nodes: Default::default(),
            metadata: serde_json::json!({}),
            node_runs: Default::default(),
        }
    }

    // -- campaign_id_from_value --

    #[test]
    fn campaign_id_from_value_accepts_a_bare_string() {
        let id = Uuid::new_v4();
        let value = Value::String(id.to_string());
        assert_eq!(campaign_id_from_value(&value).unwrap(), id);
    }

    #[test]
    fn campaign_id_from_value_accepts_an_object_with_campaign_id_field() {
        let id = Uuid::new_v4();
        let value = json!({ "campaign_id": id.to_string(), "other": "ignored" });
        assert_eq!(campaign_id_from_value(&value).unwrap(), id);
    }

    #[test]
    fn campaign_id_from_value_rejects_a_non_uuid_string() {
        let value = Value::String("not-a-uuid".to_string());
        assert!(campaign_id_from_value(&value).is_err());
    }

    #[test]
    fn campaign_id_from_value_rejects_an_empty_or_absent_value() {
        assert!(campaign_id_from_value(&Value::Null).is_err());
        assert!(campaign_id_from_value(&json!({})).is_err());
        assert!(campaign_id_from_value(&Value::String(String::new())).is_err());
    }

    // -- render_brief / is_bail_worthy / brief_names_every_bail --

    #[test]
    fn is_bail_worthy_covers_exactly_the_four_named_kinds() {
        assert!(is_bail_worthy(JournalDecisionKind::StepBailed));
        assert!(is_bail_worthy(JournalDecisionKind::GateRefused));
        assert!(is_bail_worthy(
            JournalDecisionKind::StateWriteVerificationFailed
        ));
        assert!(is_bail_worthy(JournalDecisionKind::BudgetHalted));

        assert!(!is_bail_worthy(JournalDecisionKind::StepIntegrated));
        assert!(!is_bail_worthy(JournalDecisionKind::ResolvedPolicy));
        assert!(!is_bail_worthy(JournalDecisionKind::RecallConsulted));
        assert!(!is_bail_worthy(JournalDecisionKind::DebriefRendered));
    }

    #[test]
    fn render_brief_of_empty_rows_says_nothing_ran_not_empty_or_absent() {
        let brief = render_brief(&[]);
        assert!(!brief.is_empty());
        assert!(brief.to_lowercase().contains("no steps ran"));
    }

    #[test]
    fn render_brief_names_every_step_in_created_at_order() {
        let rows = vec![
            row_at(
                "c1",
                "second-step",
                JournalDecisionKind::StepIntegrated,
                "ok",
                5,
            ),
            row_at(
                "c1",
                "first-step",
                JournalDecisionKind::StepIntegrated,
                "ok",
                0,
            ),
            row_at(
                "c1",
                "third-step",
                JournalDecisionKind::StepIntegrated,
                "ok",
                10,
            ),
        ];

        let brief = render_brief(&rows);

        let first_pos = brief.find("first-step").expect("first-step named");
        let second_pos = brief.find("second-step").expect("second-step named");
        let third_pos = brief.find("third-step").expect("third-step named");
        assert!(
            first_pos < second_pos,
            "steps must render in created_at order"
        );
        assert!(
            second_pos < third_pos,
            "steps must render in created_at order"
        );
    }

    #[test]
    fn render_brief_names_a_bailed_step_and_its_reason() {
        let rows = vec![
            row_at("c1", "deploy", JournalDecisionKind::StepIntegrated, "ok", 0),
            row_at(
                "c1",
                "publish",
                JournalDecisionKind::StepBailed,
                "publish target unreachable: connection refused",
                5,
            ),
        ];

        let brief = render_brief(&rows);

        assert!(brief.contains("publish"));
        assert!(brief.contains("publish target unreachable: connection refused"));
        assert!(
            !brief.to_lowercase().contains("clean run"),
            "a bailed campaign must not read as a clean success"
        );
    }

    #[test]
    fn brief_names_every_bail_is_true_when_every_reason_is_present() {
        let rows = vec![row_at(
            "c1",
            "publish",
            JournalDecisionKind::StepBailed,
            "connection refused",
            0,
        )];
        let brief = render_brief(&rows);
        assert!(brief_names_every_bail(&brief, &rows));
    }

    /// This is the check `DebriefNode::process` runs before writing a row
    /// (AC2). Demonstrating it here — a hand-built "renderer" that omits
    /// the reason — is what makes the assertion meaningful rather than
    /// trivially true: it proves the check is capable of returning
    /// `false`, per carryover `gate-scope-must-be-shown-capable-of-failing`.
    #[test]
    fn brief_names_every_bail_is_false_against_a_renderer_that_omits_the_bail() {
        let rows = vec![row_at(
            "c1",
            "publish",
            JournalDecisionKind::StepBailed,
            "connection refused",
            0,
        )];
        let brief_that_hides_the_failure = "1 step(s) ran:\n- [publish] BAILED".to_string();
        assert!(!brief_names_every_bail(
            &brief_that_hides_the_failure,
            &rows
        ));
    }

    // -- DebriefNode::process --

    #[tokio::test]
    async fn process_renders_a_clean_multi_step_campaign_and_writes_a_journal_row() {
        let campaign_id = Uuid::new_v4();
        let rows = vec![
            row_at(
                &campaign_id.to_string(),
                "step-a",
                JournalDecisionKind::StepIntegrated,
                "ok",
                0,
            ),
            row_at(
                &campaign_id.to_string(),
                "step-b",
                JournalDecisionKind::StepIntegrated,
                "ok",
                5,
            ),
        ];
        let reader = Arc::new(StubJournalReader::succeeding(rows));
        let transport = Arc::new(StubChannelTransport::succeeding());
        let written: Arc<Mutex<Vec<JournalRow>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_written = written.clone();
        let sink: Arc<JournalSinkFn> = Arc::new(move |row| sink_written.lock().unwrap().push(row));

        let node = DebriefNode::new(reader.clone(), transport.clone()).with_journal_sink(sink);
        let ctx = base_ctx(Value::String(campaign_id.to_string()));

        let result_ctx = node
            .process(ctx)
            .await
            .expect("clean campaign must not fail");

        assert_eq!(reader.last_campaign_id(), Some(campaign_id));

        let node_result = result_ctx
            .nodes
            .get(DEBRIEF_NODE_NAME)
            .expect("DebriefNode must stamp its result");
        let brief = node_result["brief"].as_str().unwrap();
        assert!(brief.contains("step-a"));
        assert!(brief.contains("step-b"));

        let written_rows = written.lock().unwrap();
        assert_eq!(written_rows.len(), 1);
        assert_eq!(written_rows[0].kind, JournalDecisionKind::DebriefRendered);
        assert_eq!(written_rows[0].campaign_id, campaign_id.to_string());
        assert_eq!(written_rows[0].detail["brief"].as_str().unwrap(), brief);

        let dispatch_calls = transport.calls();
        assert_eq!(dispatch_calls.len(), 1);
        assert_eq!(dispatch_calls[0].channel_type, ChannelType::WorkflowTrigger);
        match &dispatch_calls[0].body {
            OutboundBody::TriggerWorkflow {
                workflow_type,
                event,
            } => {
                assert_eq!(workflow_type, "CONTENT_PIPELINE");
                let envelope = &event["envelope"];
                assert_eq!(envelope["channel_type"], json!("schedule"));
                assert_eq!(envelope["source"]["text"].as_str().unwrap(), brief);
            }
            other => panic!("expected TriggerWorkflow, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn process_names_a_bailed_step_and_its_reason_rather_than_hiding_it() {
        let campaign_id = Uuid::new_v4();
        let rows = vec![
            row_at(
                &campaign_id.to_string(),
                "build",
                JournalDecisionKind::StepIntegrated,
                "ok",
                0,
            ),
            row_at(
                &campaign_id.to_string(),
                "publish",
                JournalDecisionKind::StepBailed,
                "publish target unreachable: connection refused",
                5,
            ),
        ];
        let reader = Arc::new(StubJournalReader::succeeding(rows));
        let transport = Arc::new(StubChannelTransport::succeeding());
        let written: Arc<Mutex<Vec<JournalRow>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_written = written.clone();
        let sink: Arc<JournalSinkFn> = Arc::new(move |row| sink_written.lock().unwrap().push(row));

        let node = DebriefNode::new(reader, transport).with_journal_sink(sink);
        let ctx = base_ctx(Value::String(campaign_id.to_string()));

        node.process(ctx)
            .await
            .expect("a correctly rendered bail must still succeed");

        let written_rows = written.lock().unwrap();
        let brief = written_rows[0].detail["brief"].as_str().unwrap();
        assert!(brief.contains("publish"));
        assert!(brief.contains("publish target unreachable: connection refused"));

        let bailed_steps = written_rows[0].detail["bailed_steps"].as_array().unwrap();
        assert_eq!(bailed_steps.len(), 1);
        assert_eq!(bailed_steps[0]["step"], json!("publish"));
    }

    #[tokio::test]
    async fn process_on_an_empty_campaign_writes_a_says_nothing_ran_brief() {
        let campaign_id = Uuid::new_v4();
        let reader = Arc::new(StubJournalReader::succeeding(vec![]));
        let transport = Arc::new(StubChannelTransport::succeeding());
        let written: Arc<Mutex<Vec<JournalRow>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_written = written.clone();
        let sink: Arc<JournalSinkFn> = Arc::new(move |row| sink_written.lock().unwrap().push(row));

        let node = DebriefNode::new(reader, transport).with_journal_sink(sink);
        let ctx = base_ctx(Value::String(campaign_id.to_string()));

        node.process(ctx)
            .await
            .expect("empty campaign must not fail");

        let written_rows = written.lock().unwrap();
        assert_eq!(written_rows.len(), 1);
        let brief = written_rows[0].detail["brief"].as_str().unwrap();
        assert!(!brief.is_empty());
        assert!(brief.to_lowercase().contains("no steps ran"));
        assert_eq!(written_rows[0].detail["row_count"], json!(0));
    }

    #[tokio::test]
    async fn process_accepts_an_object_shaped_event_with_a_campaign_id_field() {
        let campaign_id = Uuid::new_v4();
        let reader = Arc::new(StubJournalReader::succeeding(vec![]));
        let transport = Arc::new(StubChannelTransport::succeeding());

        let node = DebriefNode::new(reader.clone(), transport);
        let ctx = base_ctx(json!({ "campaign_id": campaign_id.to_string() }));

        node.process(ctx)
            .await
            .expect("object-shaped event must resolve");

        assert_eq!(reader.last_campaign_id(), Some(campaign_id));
    }

    #[tokio::test]
    async fn process_fails_on_an_event_with_no_campaign_id() {
        let reader = Arc::new(StubJournalReader::succeeding(vec![]));
        let transport = Arc::new(StubChannelTransport::succeeding());
        let node = DebriefNode::new(reader, transport);
        let ctx = base_ctx(json!({ "unrelated": "field" }));

        let result = node.process(ctx).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn process_propagates_a_journal_reader_failure_as_a_node_error() {
        let reader = Arc::new(StubJournalReader::failing("journal unreachable"));
        let transport = Arc::new(StubChannelTransport::succeeding());
        let node = DebriefNode::new(reader, transport);
        let ctx = base_ctx(Value::String(Uuid::new_v4().to_string()));

        let result = node.process(ctx).await;

        match result {
            Err(err) => assert!(err.message.contains("journal unreachable")),
            Ok(_) => panic!("a journal read failure must fail the node"),
        }
    }

    #[tokio::test]
    async fn process_with_no_journal_sink_still_succeeds_true_no_op() {
        let campaign_id = Uuid::new_v4();
        let reader = Arc::new(StubJournalReader::succeeding(vec![]));
        let transport = Arc::new(StubChannelTransport::succeeding());
        // No `.with_journal_sink(..)` — mirrors `OrchestrationRunNode`'s
        // "a `None` sink is a true no-op" convention.
        let node = DebriefNode::new(reader, transport);
        let ctx = base_ctx(Value::String(campaign_id.to_string()));

        node.process(ctx)
            .await
            .expect("no journal sink wired must still be a no-op success");
    }

    #[tokio::test]
    async fn process_dispatches_content_pipeline_even_with_no_conductor_present() {
        // The block's remaining dependency is EN.12.D alone (EN.12.F
        // dropped, per the block record's AMENDMENT) — this test pins
        // that a debrief runs from a campaign id alone, with nothing
        // resembling a conductor anywhere in the fixture.
        let campaign_id = Uuid::new_v4();
        let reader = Arc::new(StubJournalReader::succeeding(vec![row_at(
            &campaign_id.to_string(),
            "solo-step",
            JournalDecisionKind::StepIntegrated,
            "ok",
            0,
        )]));
        let transport = Arc::new(StubChannelTransport::succeeding());
        let node = DebriefNode::new(reader, transport.clone());
        let ctx = base_ctx(Value::String(campaign_id.to_string()));

        node.process(ctx)
            .await
            .expect("no-conductor invocation must succeed");

        assert_eq!(transport.calls().len(), 1);
    }

    #[tokio::test]
    async fn process_stamps_the_dispatch_receipt_even_when_transport_fails() {
        // A transport-level failure never fails the node — mirrors every
        // other `ChannelTransport`-backed dispatch node's "never fail the
        // run on a transport error" contract.
        let campaign_id = Uuid::new_v4();
        let reader = Arc::new(StubJournalReader::succeeding(vec![]));
        let transport = Arc::new(StubChannelTransport::failing());
        let node = DebriefNode::new(reader, transport);
        let ctx = base_ctx(Value::String(campaign_id.to_string()));

        let result_ctx = node
            .process(ctx)
            .await
            .expect("a transport failure must not fail the node");

        let receipt = &result_ctx.nodes.get(DEBRIEF_NODE_NAME).unwrap()["dispatch_receipt"];
        assert_eq!(receipt["delivered"], json!(false));
    }

    #[tokio::test]
    async fn process_respects_a_custom_dispatch_workflow_type() {
        let campaign_id = Uuid::new_v4();
        let reader = Arc::new(StubJournalReader::succeeding(vec![]));
        let transport = Arc::new(StubChannelTransport::succeeding());
        let node = DebriefNode::new(reader, transport.clone())
            .with_dispatch_workflow_type("FIXTURE_PIPELINE");
        let ctx = base_ctx(Value::String(campaign_id.to_string()));

        node.process(ctx).await.expect("must succeed");

        match &transport.calls()[0].body {
            OutboundBody::TriggerWorkflow { workflow_type, .. } => {
                assert_eq!(workflow_type, "FIXTURE_PIPELINE");
            }
            other => panic!("expected TriggerWorkflow, got {other:?}"),
        }
    }

    #[test]
    fn debrief_node_name_is_stable() {
        assert_eq!(DEBRIEF_NODE_NAME, "DebriefNode");
    }
}
