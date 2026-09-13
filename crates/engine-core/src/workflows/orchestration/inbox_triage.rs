//! `inbox_triage` (`EN.17.E` task 2) — how EDGE_RELEASED, FINDING and QUERY are acted on at
//! the chain's block boundary.
//!
//! Three behaviors, none of them wired into the boundary drain yet (`super::integrate`,
//! `EN.17.E` task 3, does that):
//!
//! 1. [`handle_edge_released`] — deterministic, no `JudgmentNode` call. Given the chain's
//!    skipped steps and an `EDGE_RELEASED` envelope's `subject`, finds the skipped step (if
//!    any) that depends on the now-released block and re-runs [`check_dependencies`] for it.
//! 2. [`InboxTriageRunner::judge_message`] — one bounded [`JudgmentNode`] call per FINDING or
//!    QUERY, after [`okf_core::MessageRecord::cap_violations`] is checked (an over-cap
//!    envelope never reaches the judgment call at all). The reply is deserialized into
//!    [`InboxVerdict`], a closed four-verdict enum — an out-of-enum string is a
//!    [`crate::nodes::JudgmentError::SchemaViolation`], never a silent pass-through.
//! 3. [`compose_finding_escalation`] — a `FINDING` judged `Accepted` + `Escalate` composes an
//!    escalation record through `super::escalate`'s own types (`EscalationChannel::notification`,
//!    `EscalationRecord::new`) — the exact composition [`super::integrate::record_bail_escalation`]
//!    already uses for a bail's notification channel, reused here rather than duplicated.
//!    Appending it to `escalations.jsonl` is the caller's job (mirrors
//!    [`super::escalate::append_escalation_line`]'s own doc: composition and writing are
//!    deliberately separate).
//!
//! Every processed message gets exactly one reply, composed by [`build_reply_envelope`] and
//! sent via [`send_reply_and_complete`] to the sender's own `repo`/`lane` inbox, naming a
//! durable home; the original message is then completed in OUR OWN `processing/` directory.
//! [`process_drained_message`] is the one entry point [`super::integrate`]'s boundary drain
//! calls per drained `EdgeReleased`/`Finding`/`Query` message — it never writes into the
//! caller's own `ctx.nodes`, since [`JudgmentNode::judge`] already runs against a clone (see
//! that type's own doc comment).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use engine_contract::TaskContext;
use okf_core::{CapViolation, MessageKind, MessageRecord};

use crate::coord::write::{self, CoordWriteError};
use crate::nodes::{InputSlice, JudgmentError, JudgmentNode, JudgmentSpec, MetaTransport};
use crate::policy::ModelTier;
use crate::workflows::ModelTransport;

use super::chain::ChainStep;
use super::coord_lane::CoordHandle;
use super::escalate::{
    EscalationChannel, EscalationError, EscalationKind, EscalationOption, EscalationRecord,
    EscalationSeverity, NewEscalation, SUMMARY_MAX_CHARS,
};
use super::gates::{check_dependencies, DependencyEdge, GateError};

/// The `Node::name()`-style identity the composed FINDING/QUERY judgment call runs under.
const INBOX_TRIAGE_NODE_NAME: &str = "InboxTriage";

/// The stable inbox-triage prompt (D24 / standing rule 7) — a whole task prompt, not a
/// cache-anchor prefix. Lives under `workflows/orchestration/` (not `nodes/`) so
/// `tests/it/prompt_externalization.rs`'s regex, which only walks `src/workflows/**`,
/// actually scans this const.
const INBOX_TRIAGE_PROMPT: &str = include_str!("prompts/inbox_triage.md");

// ---------------------------------------------------------------------------
// InboxVerdict — the judged reply schema
// ---------------------------------------------------------------------------

/// The `ping-agent` skill's four-verdict response contract, as a closed enum — never a free
/// string. Wire spellings are the skill's own hyphenated vocabulary
/// (`ACCEPTED`/`VERIFIED-FALSE`/`DEFERRED`/`NOT-MINE`), reused verbatim in every `ACK <VERDICT>`
/// reply body this module composes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    #[serde(rename = "ACCEPTED")]
    Accepted,
    #[serde(rename = "VERIFIED-FALSE")]
    VerifiedFalse,
    #[serde(rename = "DEFERRED")]
    Deferred,
    #[serde(rename = "NOT-MINE")]
    NotMine,
}

impl Verdict {
    /// The exact wire string this verdict serializes to — also the token an `ACK <VERDICT>`
    /// reply body names.
    #[must_use]
    pub fn as_wire_str(self) -> &'static str {
        match self {
            Verdict::Accepted => "ACCEPTED",
            Verdict::VerifiedFalse => "VERIFIED-FALSE",
            Verdict::Deferred => "DEFERRED",
            Verdict::NotMine => "NOT-MINE",
        }
    }
}

/// Whether a judged FINDING should page a human. A QUERY never escalates regardless of this
/// value (enforced by [`process_drained_message`], not by this type — a QUERY judged
/// `Escalate` is representable but never acted on).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    #[serde(rename = "NONE")]
    None,
    #[serde(rename = "ESCALATE")]
    Escalate,
}

/// The FINDING/QUERY judgment call's output schema. Deserializing a reply whose `verdict` or
/// `action` string falls outside the closed enums above is a
/// [`crate::nodes::JudgmentError::SchemaViolation`] (via `serde_json::from_value` failing
/// inside [`JudgmentNode::judge`]) — never a silent pass-through.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InboxVerdict {
    pub verdict: Verdict,
    pub action: Action,
    pub reason: String,
}

/// JSON schema matching [`InboxVerdict`] — `Config.json_schema` for the inbox-triage judgment
/// call.
fn inbox_verdict_json_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "verdict": {
                "type": "string",
                "enum": ["ACCEPTED", "VERIFIED-FALSE", "DEFERRED", "NOT-MINE"],
            },
            "action": {
                "type": "string",
                "enum": ["NONE", "ESCALATE"],
            },
            "reason": { "type": "string" },
        },
        "required": ["verdict", "action", "reason"],
    })
}

/// Which of [`JudgmentError`]'s four variants a failed judgment call returned, as the token
/// named in that message's `ACK DEFERRED` reply body.
fn judgment_error_kind(err: &JudgmentError) -> &'static str {
    match err {
        JudgmentError::Timeout => "timeout",
        JudgmentError::CliError { .. } => "cli_error",
        JudgmentError::NoStructuredResult { .. } => "no_structured_result",
        JudgmentError::SchemaViolation { .. } => "schema_violation",
    }
}

// ---------------------------------------------------------------------------
// (1) EDGE_RELEASED — deterministic, no JudgmentNode call
// ---------------------------------------------------------------------------

/// [`handle_edge_released`]'s result: the verdict this message earns, plus (only on
/// `Accepted`) the now-unblocked step to append once to the chain's remaining steps.
#[derive(Debug, Clone)]
pub struct EdgeReleaseOutcome {
    /// `Some` only when `verdict` is [`Verdict::Accepted`] — the caller (`EN.17.E` task 3)
    /// appends this step to the chain's remaining steps exactly once.
    pub requeue_step: Option<ChainStep>,
    pub verdict: Verdict,
}

impl EdgeReleaseOutcome {
    /// The `ACK <VERDICT>` reply body this outcome earns. EDGE_RELEASED bills nothing, so
    /// there is no error kind or judged reason to name — only the verdict itself.
    #[must_use]
    pub fn reply_body(&self) -> String {
        format!("ACK {}", self.verdict.as_wire_str())
    }
}

/// Handle one `EDGE_RELEASED` envelope: find the skipped step (if any, among
/// `skipped_steps`) whose `depends_on` names the block `message.subject` released, and
/// re-check its dependencies.
///
/// - No skipped step depends on `message.subject`'s block (or `subject.block` is absent —
///   an EDGE_RELEASED with no named block can never match a specific skipped step): this
///   message is [`Verdict::NotMine`], `requeue_step: None`.
/// - A matching skipped step exists and [`check_dependencies`] now reports it ready:
///   [`Verdict::Accepted`], `requeue_step: Some(step)`.
/// - A matching skipped step exists but still has an unmet edge (possibly a *different*
///   edge than the one this message released): [`Verdict::VerifiedFalse`], `requeue_step:
///   None` — it stays skipped.
///
/// Bills zero `JudgmentNode` sessions — this function never touches one.
pub fn handle_edge_released(
    message: &MessageRecord,
    skipped_steps: &[ChainStep],
    resolve_depends_on: &dyn Fn(&str, &str) -> Vec<DependencyEdge>,
    is_edge_met: &dyn Fn(&str, &str) -> bool,
) -> EdgeReleaseOutcome {
    let Some(released_block) = message.subject.block.as_deref() else {
        return EdgeReleaseOutcome {
            requeue_step: None,
            verdict: Verdict::NotMine,
        };
    };
    let released_repo = message.subject.repo.as_str();

    let target = skipped_steps.iter().find(|step| {
        resolve_depends_on(&step.repo, &step.block_id)
            .iter()
            .any(|edge| {
                matches!(
                    edge,
                    DependencyEdge::Block { repo, block_id }
                        if repo == released_repo && block_id == released_block
                )
            })
    });

    let Some(step) = target else {
        return EdgeReleaseOutcome {
            requeue_step: None,
            verdict: Verdict::NotMine,
        };
    };

    match check_dependencies(step, resolve_depends_on, is_edge_met) {
        Ok(()) => EdgeReleaseOutcome {
            requeue_step: Some(step.clone()),
            verdict: Verdict::Accepted,
        },
        Err(GateError::UnmetDependency { .. }) => EdgeReleaseOutcome {
            requeue_step: None,
            verdict: Verdict::VerifiedFalse,
        },
    }
}

// ---------------------------------------------------------------------------
// (2) FINDING / QUERY — one bounded JudgmentNode call, cap-checked first
// ---------------------------------------------------------------------------

/// One FINDING/QUERY message's triage result, before a reply is composed.
#[derive(Debug)]
pub enum TriageOutcome {
    /// `message.cap_violations()` was non-empty — the judgment call was never made. Carries
    /// every violation found, in [`okf_core::MessageRecord::cap_violations`]'s own order.
    CapViolation { violations: Vec<CapViolation> },
    /// The judgment call itself failed; no verdict was ever produced.
    JudgmentFailed { error: JudgmentError },
    /// The judgment call succeeded and deserialized into a valid [`InboxVerdict`].
    Judged { verdict: InboxVerdict },
}

/// The three inbox-triage knobs [`InboxTriageRunner`] consumes — the shape
/// `super::graph::OrchestrationPolicy`'s `inbox_triage_*` fields (`EN.17.E` task 1) resolve
/// into for a real chain run.
#[derive(Debug, Clone)]
pub struct InboxTriageConfig {
    pub model_tier: ModelTier,
    pub max_turns: Option<u32>,
    pub slice_max_bytes: usize,
}

impl Default for InboxTriageConfig {
    fn default() -> Self {
        Self {
            model_tier: ModelTier::Haiku,
            max_turns: None,
            slice_max_bytes: 4_000,
        }
    }
}

/// `EN.17.E` task 7: resolves the three `OrchestrationPolicy::inbox_triage_*` knobs into
/// this config — mirrors `preflight.rs`'s existing
/// `impl From<&super::graph::OrchestrationPolicy> for PreflightConfig`.
impl From<&super::graph::OrchestrationPolicy> for InboxTriageConfig {
    fn from(policy: &super::graph::OrchestrationPolicy) -> Self {
        Self {
            model_tier: policy.inbox_triage_model_tier,
            max_turns: policy.inbox_triage_max_turns,
            slice_max_bytes: policy.inbox_triage_slice_max_bytes,
        }
    }
}

/// Judges FINDING/QUERY messages: cap-checks first, then runs at most one
/// [`JudgmentNode::judge`] call per message. Not a graph node — see `preflight.rs`'s
/// `PreflightRunner`, which this mirrors, for why the analogous type there isn't one either.
pub struct InboxTriageRunner {
    config: InboxTriageConfig,
    judgment: JudgmentNode<InboxVerdict>,
}

impl InboxTriageRunner {
    #[must_use]
    pub fn new(config: InboxTriageConfig) -> Self {
        Self {
            config,
            judgment: JudgmentNode::new(),
        }
    }

    /// Override the judgment call's transport. Tests inject a stub so the gated suite never
    /// spawns a real `claude` subprocess.
    #[must_use]
    pub fn with_transport(mut self, transport: ModelTransport) -> Self {
        self.judgment = self.judgment.with_transport(transport);
        self
    }

    /// Override with a tier-aware [`MetaTransport`]. Takes precedence over
    /// [`Self::with_transport`] when both are set.
    #[must_use]
    pub fn with_meta_transport(mut self, transport: MetaTransport) -> Self {
        self.judgment = self.judgment.with_meta_transport(transport);
        self
    }

    /// Judge one FINDING or QUERY message.
    ///
    /// `cap_violations()` is checked FIRST, before any `JudgmentNode` construction or call —
    /// this task's own acceptance criterion. A non-empty result short-circuits straight to
    /// [`TriageOutcome::CapViolation`]; `self.judgment` is never touched for that message.
    pub async fn judge_message(
        &self,
        ctx: &TaskContext,
        message: &MessageRecord,
        chain_status_summary: &str,
    ) -> TriageOutcome {
        let violations = message.cap_violations();
        if !violations.is_empty() {
            return TriageOutcome::CapViolation { violations };
        }

        let subject_text = serde_json::to_string_pretty(&json!({
            "repo": message.subject.repo,
            "block": message.subject.block,
        }))
        .unwrap_or_default();

        let slices = vec![
            InputSlice {
                name: "subject".to_string(),
                text: subject_text,
                max_bytes: self.config.slice_max_bytes,
            },
            InputSlice {
                name: "body".to_string(),
                text: message.body.clone(),
                max_bytes: self.config.slice_max_bytes,
            },
            InputSlice {
                name: "chain_blocks".to_string(),
                text: chain_status_summary.to_string(),
                max_bytes: self.config.slice_max_bytes,
            },
        ];

        let spec = JudgmentSpec {
            identity: INBOX_TRIAGE_NODE_NAME.to_string(),
            json_schema: inbox_verdict_json_schema(),
            stable_prompt: INBOX_TRIAGE_PROMPT,
            slices,
            tier: self.config.model_tier,
            max_turns: self.config.max_turns,
        };

        match self.judgment.judge(ctx, spec).await {
            Ok(result) => TriageOutcome::Judged {
                verdict: result.verdict,
            },
            Err(error) => TriageOutcome::JudgmentFailed { error },
        }
    }
}

// ---------------------------------------------------------------------------
// (3) A FINDING's escalation — composed through EN.17.C's own escalate.rs types
// ---------------------------------------------------------------------------

/// Everything [`compose_finding_escalation`] needs beyond a fixed `kind`/`severity`/
/// `channel` shape — the caller (`EN.17.E` task 3) supplies the pieces this module cannot
/// derive on its own: the current timestamp, the subject block, and the subject repo's
/// short SHA (resolved the same way [`super::integrate::record_bail_escalation`] resolves
/// one for a bail).
pub struct FindingEscalationInput {
    pub ts_utc: String,
    pub repo: String,
    pub lane: String,
    pub block: Option<String>,
    pub gate_id: String,
    pub summary: String,
    pub verified_by: String,
    pub durable_home: Value,
    pub verified_at_sha: String,
}

/// Compose a schema-valid `escalations.jsonl` line for a FINDING judged `Accepted` +
/// `Escalate` — the `notification` channel with the same two acknowledgement-only options
/// [`super::integrate::record_bail_escalation`]'s own `BailChannel::Notification` arm
/// composes, reusing `super::escalate`'s types rather than duplicating their construction.
/// Appending the result to `escalations.jsonl` is the caller's job (`EN.17.E` task 3), via
/// [`super::escalate::append_escalation_line`].
pub fn compose_finding_escalation(
    input: FindingEscalationInput,
) -> Result<EscalationRecord, EscalationError> {
    let channel = EscalationChannel::notification(vec![
        EscalationOption::new("ack", "Seen")?,
        EscalationOption::new("later", "Later")?,
    ])?;
    EscalationRecord::new(NewEscalation {
        ts_utc: input.ts_utc,
        repo: input.repo,
        lane: input.lane,
        kind: EscalationKind::Finding,
        severity: EscalationSeverity::Advisory,
        channel,
        block: input.block,
        gate_id: input.gate_id,
        summary: input.summary,
        verified_by: input.verified_by,
        durable_home: input.durable_home,
        verified_at_sha: input.verified_at_sha,
        clears_when: None,
        host: None,
    })
}

// ---------------------------------------------------------------------------
// (4) Reply composition — every processed message gets exactly one reply
// ---------------------------------------------------------------------------

/// Build a reply envelope addressed at `received.sender`'s own `repo`/`lane` inbox — mirrors
/// [`super::coord_lane::CoordHandle::reply_rendezvous`]'s envelope shape, generalized to any
/// `body`. Replies are sent as `FINDING` — "a cross-lane observation worth relaying" is
/// exactly what an ACK naming a durable home is, regardless of which of the three kinds it
/// answers.
fn build_reply_envelope(
    coord: &CoordHandle,
    received: &MessageRecord,
    body: String,
    now: &str,
) -> Value {
    json!({
        "message_id": uuid::Uuid::new_v4().to_string(),
        "sender": {
            "agent_name": coord.agent,
            "repo": coord.repo,
            "lane": coord.lane,
            "roadmap": received.sender.roadmap,
        },
        "sent_at": now,
        "kind": "FINDING",
        "subject": {
            "repo": received.subject.repo,
            "block": received.subject.block,
        },
        "body": body,
        "durable_home": {
            "channel": "lane-log",
            "ref": format!("reply to message {}", received.message_id),
        },
        "verified_by": "engine-rs inbox_triage",
    })
}

/// Send exactly one reply to `received.sender`'s inbox, then complete `received` in OUR OWN
/// `processing/` directory (the directory [`CoordHandle::drain`] moved it into). The complete
/// call is best-effort — a failure there is logged and swallowed, mirroring every other
/// best-effort write on this loop's bail path (`super::integrate::record_bail_escalation`'s
/// own doc), since the reply having been sent is the durable signal a receiver needs;
/// re-completing an already-processing message on the next boundary is harmless.
pub fn send_reply_and_complete(
    coord: &CoordHandle,
    received: &MessageRecord,
    body: String,
) -> Result<std::path::PathBuf, CoordWriteError> {
    let now = (coord.now_iso)();
    let envelope = build_reply_envelope(coord, received, body, &now);
    let path = write::send(
        &coord.lock_dir,
        &received.sender.repo,
        &received.sender.lane,
        envelope,
        None,
    )?;
    if let Err(err) = write::complete(
        &coord.lock_dir,
        &coord.repo,
        &coord.lane,
        &received.message_id,
        &now,
    ) {
        tracing::warn!(
            error = %err,
            message_id = %received.message_id,
            "inbox_triage: reply sent but failed to complete the original message"
        );
    }
    Ok(path)
}

// ---------------------------------------------------------------------------
// The public entry point — routes one drained message to the behavior above
// ---------------------------------------------------------------------------

/// The pieces [`process_drained_message`] needs to compose a FINDING's escalation, threaded
/// in by the caller (`EN.17.E` task 3) — see [`FindingEscalationInput`]'s own doc for why
/// these specifically cannot be derived inside this module.
#[derive(Debug, Clone, Copy)]
pub struct EscalationContext<'a> {
    pub roadmap: &'a str,
    pub verified_at_sha: &'a str,
    pub now_iso: fn() -> String,
}

/// One processed message's accumulated record — the shape `super::integrate`'s boundary
/// drain (`EN.17.E` task 3) folds into the chain's `inbox_report`.
///
/// `Serialize` (`EN.17.E` task 7) lets `OrchestrationRunNode::process` stamp the whole
/// accumulated `inbox_report` into its `ctx.nodes` result via `json!`, mirroring
/// `BlockPreflight`'s own `Serialize` derive for `preflight_report`.
#[derive(Debug, Clone, Serialize)]
pub struct ProcessedMessage {
    pub message_id: String,
    pub kind: MessageKind,
    pub sender_repo: String,
    pub sender_lane: String,
    /// `None` when the message never reached a verdict (a cap violation or a failed
    /// judgment) — `error_kind` names why in that case.
    pub verdict: Option<Verdict>,
    pub action: Option<Action>,
    /// Set for a cap violation (`"cap_violation"`) or a failed judgment (one of
    /// [`judgment_error_kind`]'s four strings); `None` once a verdict was reached.
    pub error_kind: Option<String>,
    /// The path the reply was written to, when the reply send itself succeeded.
    pub reply_path: Option<std::path::PathBuf>,
}

/// Compose a FINDING/QUERY's reply body, verdict/action bookkeeping, and (only for an
/// escalating FINDING) its composed escalation record, from one [`TriageOutcome`].
struct Composed {
    body: String,
    verdict: Option<Verdict>,
    action: Option<Action>,
    error_kind: Option<String>,
    escalation: Option<EscalationRecord>,
}

fn compose_finding_or_query_reply(
    message: &MessageRecord,
    triage: TriageOutcome,
    escalation_ctx: &EscalationContext<'_>,
    lane: &str,
) -> Composed {
    match triage {
        TriageOutcome::CapViolation { violations } => {
            let first = violations
                .first()
                .expect("checked non-empty before returning CapViolation");
            Composed {
                body: format!(
                    "ACK DEFERRED (envelope field '{}' exceeds its {}-char cap, actual {} chars)",
                    first.field, first.max, first.actual
                ),
                verdict: None,
                action: None,
                error_kind: Some("cap_violation".to_string()),
                escalation: None,
            }
        }
        TriageOutcome::JudgmentFailed { error } => {
            let kind = judgment_error_kind(&error);
            Composed {
                body: format!("ACK DEFERRED (judgment {kind})"),
                verdict: None,
                action: None,
                error_kind: Some(kind.to_string()),
                escalation: None,
            }
        }
        TriageOutcome::Judged { verdict } => {
            let should_escalate = message.kind == MessageKind::Finding
                && verdict.verdict == Verdict::Accepted
                && verdict.action == Action::Escalate;
            let escalation = if should_escalate {
                let gate_id = format!(
                    "{}/{}/{}",
                    escalation_ctx.roadmap,
                    message.subject.repo,
                    message.subject.block.as_deref().unwrap_or("(repo)"),
                );
                let summary: String = verdict.reason.chars().take(SUMMARY_MAX_CHARS).collect();
                compose_finding_escalation(FindingEscalationInput {
                    ts_utc: (escalation_ctx.now_iso)(),
                    repo: message.subject.repo.clone(),
                    lane: lane.to_string(),
                    block: message.subject.block.clone(),
                    gate_id,
                    summary,
                    verified_by: format!(
                        "inbox_triage FINDING {}\n{}",
                        message.message_id, verdict.reason
                    ),
                    durable_home: json!({
                        "channel": "lane-log",
                        "ref": format!("reply to message {}", message.message_id),
                    }),
                    verified_at_sha: escalation_ctx.verified_at_sha.to_string(),
                })
                .ok()
            } else {
                None
            };
            Composed {
                body: format!("ACK {} — {}", verdict.verdict.as_wire_str(), verdict.reason),
                verdict: Some(verdict.verdict),
                action: Some(verdict.action),
                error_kind: None,
                escalation,
            }
        }
    }
}

/// The one entry point `super::integrate`'s boundary drain (`EN.17.E` task 3) calls per
/// drained `EdgeReleased`/`Finding`/`Query` message. Routes to [`handle_edge_released`] or
/// [`InboxTriageRunner::judge_message`], composes and sends exactly one reply via
/// [`send_reply_and_complete`], and — for an escalating FINDING — composes (but does not
/// append) its escalation record.
///
/// Returns the accumulated [`ProcessedMessage`], the step to re-queue (only ever `Some` for
/// an accepted EDGE_RELEASED), and the escalation to append (only ever `Some` for a FINDING
/// judged `Accepted` + `Escalate`) — the caller decides where and how to append it, exactly
/// as [`super::escalate::append_escalation_line`]'s own doc separates composition from
/// writing.
#[allow(clippy::too_many_arguments)]
pub async fn process_drained_message(
    runner: &InboxTriageRunner,
    ctx: &TaskContext,
    coord: &CoordHandle,
    message: &MessageRecord,
    skipped_steps: &[ChainStep],
    resolve_depends_on: &dyn Fn(&str, &str) -> Vec<DependencyEdge>,
    is_edge_met: &dyn Fn(&str, &str) -> bool,
    chain_status_summary: &str,
    escalation_ctx: &EscalationContext<'_>,
) -> (
    ProcessedMessage,
    Option<ChainStep>,
    Option<EscalationRecord>,
) {
    match message.kind {
        MessageKind::EdgeReleased => {
            let outcome =
                handle_edge_released(message, skipped_steps, resolve_depends_on, is_edge_met);
            let body = outcome.reply_body();
            let reply_path = send_reply_and_complete(coord, message, body).ok();
            let processed = ProcessedMessage {
                message_id: message.message_id.clone(),
                kind: message.kind,
                sender_repo: message.sender.repo.clone(),
                sender_lane: message.sender.lane.clone(),
                verdict: Some(outcome.verdict),
                action: None,
                error_kind: None,
                reply_path,
            };
            (processed, outcome.requeue_step, None)
        }
        MessageKind::Finding | MessageKind::Query => {
            let triage = runner
                .judge_message(ctx, message, chain_status_summary)
                .await;
            let composed =
                compose_finding_or_query_reply(message, triage, escalation_ctx, &coord.lane);
            let reply_path = send_reply_and_complete(coord, message, composed.body).ok();
            let processed = ProcessedMessage {
                message_id: message.message_id.clone(),
                kind: message.kind,
                sender_repo: message.sender.repo.clone(),
                sender_lane: message.sender.lane.clone(),
                verdict: composed.verdict,
                action: composed.action,
                error_kind: composed.error_kind,
                reply_path,
            };
            (processed, None, composed.escalation)
        }
        // `EN.17.E` scope is EDGE_RELEASED/FINDING/QUERY only — RENDEZVOUS and LEASE_RELEASE
        // are handled at the drain site itself (`super::integrate`), unchanged, and never
        // reach this function.
        MessageKind::Rendezvous | MessageKind::LeaseRelease => {
            let processed = ProcessedMessage {
                message_id: message.message_id.clone(),
                kind: message.kind,
                sender_repo: message.sender.repo.clone(),
                sender_lane: message.sender.lane.clone(),
                verdict: None,
                action: None,
                error_kind: None,
                reply_path: None,
            };
            (processed, None, None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn empty_ctx() -> TaskContext {
        TaskContext {
            event: json!({}),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        }
    }

    fn message(
        kind: MessageKind,
        subject_repo: &str,
        subject_block: Option<&str>,
    ) -> MessageRecord {
        MessageRecord {
            message_id: "70ef6ce8-abcd-4e21-9f10-0000000000aa".to_string(),
            sender: okf_core::MessageSender {
                agent_name: "peer-lane".to_string(),
                repo: "bastion".to_string(),
                lane: "types".to_string(),
                roadmap: "coordination-layer-port".to_string(),
            },
            sent_at: "2026-09-10T02:08:00Z".to_string(),
            kind,
            subject: okf_core::MessageSubject {
                repo: subject_repo.to_string(),
                block: subject_block.map(str::to_string),
            },
            body: "test body".to_string(),
            durable_home: okf_core::MessageDurableHome {
                channel: okf_core::DurableHomeChannel::StateEdge,
                reference: "bastion/planning/state.json#BA.21.A".to_string(),
            },
            verified_by: "UNVERIFIED: test".to_string(),
            host: None,
        }
    }

    fn step(repo: &str, block_id: &str) -> ChainStep {
        ChainStep {
            repo: repo.to_string(),
            block_id: block_id.to_string(),
            ..ChainStep::default()
        }
    }

    // -- Verdict / Action wire spellings -----------------------------------

    #[test]
    fn verdict_uses_ping_agent_hyphenated_wire_values() {
        assert_eq!(
            serde_json::to_string(&Verdict::Accepted).unwrap(),
            "\"ACCEPTED\""
        );
        assert_eq!(
            serde_json::to_string(&Verdict::VerifiedFalse).unwrap(),
            "\"VERIFIED-FALSE\""
        );
        assert_eq!(
            serde_json::to_string(&Verdict::Deferred).unwrap(),
            "\"DEFERRED\""
        );
        assert_eq!(
            serde_json::to_string(&Verdict::NotMine).unwrap(),
            "\"NOT-MINE\""
        );
    }

    #[test]
    fn inbox_verdict_rejects_a_verdict_outside_the_four_values() {
        let value = json!({"verdict": "MAYBE", "action": "NONE", "reason": "x"});
        let parsed: Result<InboxVerdict, _> = serde_json::from_value(value);
        assert!(parsed.is_err(), "an out-of-enum verdict must be refused");
    }

    #[test]
    fn inbox_verdict_round_trips_every_valid_combination() {
        for verdict in [
            Verdict::Accepted,
            Verdict::VerifiedFalse,
            Verdict::Deferred,
            Verdict::NotMine,
        ] {
            for action in [Action::None, Action::Escalate] {
                let original = InboxVerdict {
                    verdict,
                    action,
                    reason: "because".to_string(),
                };
                let json = serde_json::to_value(&original).unwrap();
                let parsed: InboxVerdict = serde_json::from_value(json).unwrap();
                assert_eq!(parsed, original);
            }
        }
    }

    // -- handle_edge_released ------------------------------------------------

    #[test]
    fn edge_released_with_no_matching_skipped_step_is_not_mine() {
        let msg = message(MessageKind::EdgeReleased, "bastion", Some("BA.21.A"));
        let skipped = vec![step("engine-rs", "EN.1.A")];
        let outcome = handle_edge_released(&msg, &skipped, &|_, _| Vec::new(), &|_, _| true);
        assert_eq!(outcome.verdict, Verdict::NotMine);
        assert!(outcome.requeue_step.is_none());
        assert_eq!(outcome.reply_body(), "ACK NOT-MINE");
    }

    #[test]
    fn edge_released_with_no_subject_block_is_not_mine() {
        let msg = message(MessageKind::EdgeReleased, "bastion", None);
        let skipped = vec![step("engine-rs", "EN.1.A")];
        let outcome = handle_edge_released(&msg, &skipped, &|_, _| Vec::new(), &|_, _| true);
        assert_eq!(outcome.verdict, Verdict::NotMine);
        assert!(outcome.requeue_step.is_none());
    }

    #[test]
    fn edge_released_requeues_a_now_met_skipped_step() {
        let msg = message(MessageKind::EdgeReleased, "bastion", Some("BA.21.A"));
        let skipped = vec![step("engine-rs", "EN.1.A")];
        let resolve = |_repo: &str, _block: &str| {
            vec![DependencyEdge::Block {
                repo: "bastion".to_string(),
                block_id: "BA.21.A".to_string(),
            }]
        };
        let outcome = handle_edge_released(&msg, &skipped, &resolve, &|_, _| true);
        assert_eq!(outcome.verdict, Verdict::Accepted);
        assert_eq!(
            outcome.requeue_step.as_ref().map(|s| s.block_id.as_str()),
            Some("EN.1.A")
        );
        assert_eq!(outcome.reply_body(), "ACK ACCEPTED");
    }

    #[test]
    fn edge_released_still_unmet_is_verified_false() {
        let msg = message(MessageKind::EdgeReleased, "bastion", Some("BA.21.A"));
        let skipped = vec![step("engine-rs", "EN.1.A")];
        let resolve = |_repo: &str, _block: &str| {
            vec![DependencyEdge::Block {
                repo: "bastion".to_string(),
                block_id: "BA.21.A".to_string(),
            }]
        };
        let outcome = handle_edge_released(&msg, &skipped, &resolve, &|_, _| false);
        assert_eq!(outcome.verdict, Verdict::VerifiedFalse);
        assert!(outcome.requeue_step.is_none());
        assert_eq!(outcome.reply_body(), "ACK VERIFIED-FALSE");
    }

    // -- cap check precedes any JudgmentNode construction --------------------

    #[tokio::test]
    async fn over_cap_message_is_never_judged() {
        let mut msg = message(MessageKind::Finding, "bastion", Some("BA.21.A"));
        msg.body = "a".repeat(okf_core::BODY_MAX_CHARS + 1);
        // No transport is configured on this runner at all — if the cap check did not
        // short-circuit before the judgment call, this would panic reaching for a live
        // `claude` subprocess in a test process.
        let runner = InboxTriageRunner::new(InboxTriageConfig::default());
        let outcome = runner.judge_message(&empty_ctx(), &msg, "no blocks").await;
        match outcome {
            TriageOutcome::CapViolation { violations } => {
                assert!(violations.iter().any(|v| v.field == "body"));
            }
            other => panic!("expected CapViolation, got {other:?}"),
        }
    }

    // -- compose_finding_escalation reuses escalate.rs's own types -----------

    #[test]
    fn compose_finding_escalation_builds_a_notification_channel_record() {
        let record = compose_finding_escalation(FindingEscalationInput {
            ts_utc: "2026-09-10T02:08:00Z".to_string(),
            repo: "bastion".to_string(),
            lane: "types".to_string(),
            block: Some("BA.21.A".to_string()),
            gate_id: "coordination-layer-port/bastion/BA.21.A".to_string(),
            summary: "a finding worth a human's attention".to_string(),
            verified_by: "UNVERIFIED: peer lane".to_string(),
            durable_home: json!({"channel": "lane-log", "ref": "reply to message x"}),
            verified_at_sha: "abc1234".to_string(),
        })
        .expect("a well-formed finding escalation composes");
        let line = record.to_jsonl_line();
        assert!(line.contains("\"kind\":\"finding\""));
        assert!(line.contains("\"channel\":\"notification\""));
    }
}
