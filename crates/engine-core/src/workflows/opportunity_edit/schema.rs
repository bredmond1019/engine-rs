//! The two inbound event schemas for `EN.7.B`'s opportunity-edit
//! micro-workflows — the `POST /events/` payloads `BW.7.A` will send.
//!
//! [`SetOpportunityStageEvent`] drives `OPPORTUNITY_SET_STAGE`;
//! [`AddOpportunityActionEvent`] drives `OPPORTUNITY_ADD_ACTION`. Both mirror
//! `crate::nodes::opportunity_edit::OpportunityEditNode::build_edit`'s field
//! reads field-for-field — that node constructs one of these two types (via
//! `serde_json::from_value`) as the single source of truth for "which
//! fields does this event carry", rather than re-deriving the field list.
//!
//! Neither carries a `policy`/`profile` override pair like
//! `research_agent::schema::ResearchAgentEventSchema` — these workflows are
//! deterministic and model-free (see `graph.rs`'s module doc), so there is
//! no policy layer to override.

use serde::{Deserialize, Serialize};

/// Inbound event schema for `OPPORTUNITY_SET_STAGE`: change an existing
/// opportunity's `stage` to one of mev's `VALID_STAGES`
/// (`mev::doc::opportunity::VALID_STAGES` — this schema deliberately does
/// not fork that vocabulary into a Rust enum; an unrecognized `stage`
/// string surfaces as a `NodeError` naming the valid stages, sourced from
/// mev at run time).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetOpportunityStageEvent {
    /// The opportunity document's slug (`business/docs/opportunities/<slug>.md`).
    pub slug: String,
    /// The stage to set. Validated by mev's `plan_set_stage`, not here.
    pub stage: String,
}

/// Inbound event schema for `OPPORTUNITY_ADD_ACTION`: append one
/// `{at, kind, note}` entry to an existing opportunity's `actions[]`. An
/// identical `{at, kind, note}` triple already present is not re-appended
/// (mev's `plan_add_action` idempotency) — a repeat plans zero actions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AddOpportunityActionEvent {
    /// The opportunity document's slug (`business/docs/opportunities/<slug>.md`).
    pub slug: String,
    /// When the action happened/was logged (free-form, mirrors mev's
    /// `plan_add_action` signature — not parsed/validated as a date here).
    pub at: String,
    /// The action's kind (e.g. `"email"`, `"call"`).
    pub kind: String,
    /// A free-form note describing the action.
    pub note: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_opportunity_stage_event_round_trips_through_serde_json() {
        let event = SetOpportunityStageEvent {
            slug: "acme-corp".to_string(),
            stage: "contacted".to_string(),
        };
        let json = serde_json::to_string(&event).expect("serializes");
        let round_tripped: SetOpportunityStageEvent =
            serde_json::from_str(&json).expect("deserializes");
        assert_eq!(event, round_tripped);
    }

    #[test]
    fn add_opportunity_action_event_round_trips_through_serde_json() {
        let event = AddOpportunityActionEvent {
            slug: "acme-corp".to_string(),
            at: "2026-07-27".to_string(),
            kind: "email".to_string(),
            note: "Sent intro email".to_string(),
        };
        let json = serde_json::to_string(&event).expect("serializes");
        let round_tripped: AddOpportunityActionEvent =
            serde_json::from_str(&json).expect("deserializes");
        assert_eq!(event, round_tripped);
    }

    #[test]
    fn set_opportunity_stage_event_deserializes_from_expected_payload_shape() {
        let json = serde_json::json!({ "slug": "acme-corp", "stage": "contacted" });
        let event: SetOpportunityStageEvent = serde_json::from_value(json).expect("deserializes");
        assert_eq!(event.slug, "acme-corp");
        assert_eq!(event.stage, "contacted");
    }

    #[test]
    fn add_opportunity_action_event_deserializes_from_expected_payload_shape() {
        let json = serde_json::json!({
            "slug": "acme-corp",
            "at": "2026-07-27",
            "kind": "email",
            "note": "Sent intro email",
        });
        let event: AddOpportunityActionEvent = serde_json::from_value(json).expect("deserializes");
        assert_eq!(event.slug, "acme-corp");
        assert_eq!(event.at, "2026-07-27");
        assert_eq!(event.kind, "email");
        assert_eq!(event.note, "Sent intro email");
    }

    #[test]
    fn set_opportunity_stage_event_missing_field_fails_to_deserialize() {
        let json = serde_json::json!({ "slug": "acme-corp" });
        let result: Result<SetOpportunityStageEvent, _> = serde_json::from_value(json);
        assert!(result.is_err());
    }

    #[test]
    fn add_opportunity_action_event_missing_field_fails_to_deserialize() {
        let json = serde_json::json!({ "slug": "acme-corp", "at": "2026-07-27" });
        let result: Result<AddOpportunityActionEvent, _> = serde_json::from_value(json);
        assert!(result.is_err());
    }
}
