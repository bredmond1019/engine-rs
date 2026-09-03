//! `JournalRow` — a durable decision record, one row per journal item (EN.12.D).
//!
//! A journal row answers "why did this run do what it did": a bailed step, a
//! refused gate, a state-write verification failure, an integrated step, a
//! budget halt, and the resolved policy actually used for a step. Rows key on
//! the child `run_id` (EN.11.G) and are queried per campaign (EN.11.E).
//!
//! **No telemetry fields.** Token counts, cost, and per-node attempt counts are
//! deliberately excluded — that data is already captured elsewhere
//! (`ctx.nodes`/`RunOutcomes`) and its open problem is archival, not schema.
//! See `planning/blocks/EN.12.D.json`'s `notes` for the full reasoning.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The kind of decision a journal row records.
///
/// The first five are the decision points that already exist on the
/// orchestration integrate path; `ResolvedPolicy` is the per-step
/// resolved-value item (never the configured value — see the block record's
/// "DECISION INPUTS, NOT OUTPUTS" clause).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalDecisionKind {
    /// A chain step was integrated successfully.
    StepIntegrated,
    /// A chain step bailed.
    StepBailed,
    /// An admission/dependency gate refused a step.
    GateRefused,
    /// A state-write verification failed after a step.
    StateWriteVerificationFailed,
    /// A `CampaignLedger` budget cap tripped and halted the run.
    BudgetHalted,
    /// The resolved policy (profile, model tier, transport, executor) actually
    /// used for a step, read after four-layer resolution.
    ResolvedPolicy,
    /// The engine consulted the brain (a `RECALL` dispatch step) and what it
    /// read changed what the chain did next (EN.12.L, D23 constraint 3). The
    /// branch taken is carried in `detail` alongside the recall result that
    /// caused it, never inferred from the absence of a row.
    RecallConsulted,
    /// A morning brief was rendered from a finished campaign's journal rows
    /// (`DEBRIEF`, EN.12.G). `detail` carries `{ brief, row_count,
    /// bailed_steps }` — the brief text itself, how many rows it summarised,
    /// and the bailed-step reasons it must name (AC2). This IS the brief:
    /// AC4 requires it retrievable over the same `GET /campaigns/{id}/journal`
    /// route family as the raw rows, with no second derivation.
    DebriefRendered,
    /// `CONDUCTOR` (`EN.12.F` Task 4) proposed tonight's chain from the
    /// operator's weekly objective and mev's computed frontier slate.
    /// `detail` carries `{ proposed: [{repo, block_id}], dropped: [{repo,
    /// block_id, reason}] }` — every candidate the proposal finalised with,
    /// and every one the `git log -S` pre-flight dropped and why, so the
    /// choice is auditable the next morning without re-deriving it.
    ConductorProposed,
}

/// One row of the durable run journal.
///
/// `detail` carries kind-specific payloads (e.g. `cap_name`/`spent`/`limit`
/// for `BudgetHalted`; `profile`/`model_tier`/`transport`/`executor` for
/// `ResolvedPolicy`) as a `serde_json::Value` so the table stays one shape
/// rather than a discriminated union pretending to be a table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalRow {
    pub id: Uuid,
    pub campaign_id: String,
    pub run_id: Uuid,
    pub step: String,
    pub kind: JournalDecisionKind,
    pub reason: String,
    pub detail: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_row(kind: JournalDecisionKind) -> JournalRow {
        JournalRow {
            id: Uuid::new_v4(),
            campaign_id: "campaign-1".to_string(),
            run_id: Uuid::new_v4(),
            step: "build".to_string(),
            kind,
            reason: "example reason".to_string(),
            detail: serde_json::json!({ "example": "value" }),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn journal_row_round_trips_through_serde_json() {
        let row = sample_row(JournalDecisionKind::StepBailed);
        let json = serde_json::to_string(&row).unwrap();
        let round_tripped: JournalRow = serde_json::from_str(&json).unwrap();
        assert_eq!(round_tripped, row);
    }

    #[test]
    fn journal_row_has_contract_top_level_fields() {
        let row = sample_row(JournalDecisionKind::StepIntegrated);
        let v = serde_json::to_value(&row).unwrap();
        for key in [
            "id",
            "campaign_id",
            "run_id",
            "step",
            "kind",
            "reason",
            "detail",
            "created_at",
        ] {
            assert!(v.get(key).is_some(), "missing top-level field: {key}");
        }
    }

    #[test]
    fn all_nine_decision_kinds_round_trip() {
        let kinds = [
            JournalDecisionKind::StepIntegrated,
            JournalDecisionKind::StepBailed,
            JournalDecisionKind::GateRefused,
            JournalDecisionKind::StateWriteVerificationFailed,
            JournalDecisionKind::BudgetHalted,
            JournalDecisionKind::ResolvedPolicy,
            JournalDecisionKind::RecallConsulted,
            JournalDecisionKind::DebriefRendered,
            JournalDecisionKind::ConductorProposed,
        ];
        for kind in kinds {
            let row = sample_row(kind);
            let json = serde_json::to_string(&row).unwrap();
            let round_tripped: JournalRow = serde_json::from_str(&json).unwrap();
            assert_eq!(round_tripped, row, "round trip failed for {:?}", kind);
            // Assert the kind itself serializes to the expected snake_case tag.
            let kind_json = serde_json::to_string(&kind).unwrap();
            assert!(
                kind_json
                    .chars()
                    .all(|c| c.is_lowercase() || c == '"' || c == '_'),
                "kind {kind_json} is not snake_case"
            );
        }
    }

    #[test]
    fn recall_consulted_serializes_to_snake_case_wire_string() {
        let kind_json = serde_json::to_string(&JournalDecisionKind::RecallConsulted).unwrap();
        assert_eq!(kind_json, "\"recall_consulted\"");
        let round_tripped: JournalDecisionKind = serde_json::from_str(&kind_json).unwrap();
        assert_eq!(round_tripped, JournalDecisionKind::RecallConsulted);
    }

    #[test]
    fn debrief_rendered_serializes_to_snake_case_wire_string() {
        let kind_json = serde_json::to_string(&JournalDecisionKind::DebriefRendered).unwrap();
        assert_eq!(kind_json, "\"debrief_rendered\"");
        let round_tripped: JournalDecisionKind = serde_json::from_str(&kind_json).unwrap();
        assert_eq!(round_tripped, JournalDecisionKind::DebriefRendered);
    }

    #[test]
    fn conductor_proposed_serializes_to_snake_case_wire_string() {
        let kind_json = serde_json::to_string(&JournalDecisionKind::ConductorProposed).unwrap();
        assert_eq!(kind_json, "\"conductor_proposed\"");
        let round_tripped: JournalDecisionKind = serde_json::from_str(&kind_json).unwrap();
        assert_eq!(round_tripped, JournalDecisionKind::ConductorProposed);
    }

    #[test]
    fn journal_row_carries_no_telemetry_fields() {
        let row = sample_row(JournalDecisionKind::ResolvedPolicy);
        let v = serde_json::to_value(&row).unwrap();
        for forbidden in ["token_count", "tokens", "cost_usd", "cost", "attempt_count"] {
            assert!(
                v.get(forbidden).is_none(),
                "journal row must not carry telemetry field: {forbidden}"
            );
        }
    }
}
