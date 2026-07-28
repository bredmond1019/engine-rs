//! `DiagnosticIntakeEventSchema` (the triggering event `data` shape) and the
//! `DiagnosticIntake` structured output type, plus the
//! [`diagnostic_intake_json_schema`] builder passed as `Config.json_schema`
//! by `IntakeExtractNode` (task 5).
//!
//! - [`DiagnosticIntakeEventSchema`] carries the raw diagnostic-call
//!   notes/transcript input plus the `policy`/`profile` override-layer
//!   fields, mirroring `research_agent::schema::ResearchAgentEventSchema` /
//!   `sdlc_flow::schema::SDLCFlowEventSchema`.
//! - [`DiagnosticIntake`] faithfully models the evidence contract from
//!   `agentic-portfolio/business/docs/diagnostic/intake.md §3`: company
//!   context plus `top_workflows: Vec<WorkflowCandidate>`, each carrying the
//!   `*_evidence` fields the rubric scores from. This name is
//!   **load-bearing** — EN.4.C imports `DiagnosticIntake` by name.
//! - [`diagnostic_intake_json_schema`] builds the `serde_json::Value` schema
//!   for schema-constrained extraction, mirroring
//!   `research_agent::schema::company_brief_json_schema`.

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::locale::Locale;

use super::policy::PartialDiagnosticIntakePolicy;

/// Inbound event schema for the `DIAGNOSTIC_INTAKE` workflow. Mirrors the
/// `policy`/`profile` override-layer fields of `ResearchAgentEventSchema` /
/// `SDLCFlowEventSchema`, plus the raw call notes/transcript this workflow
/// extracts from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticIntakeEventSchema {
    /// Raw diagnostic-call notes or transcript text. `IntakeExtractNode`'s
    /// prompt ports `intake.md`'s interview groups + evidence discipline
    /// against this text.
    pub notes: String,
    /// The client's locale — drives the language this run's prose is written
    /// in. Deliberately NOT on `DiagnosticIntakePolicy`: per CLAUDE.md rule 6
    /// a policy knob trades cost, latency, or quality, and a client's market
    /// is none of those. It is a per-client attribute and belongs on the
    /// event.
    #[serde(default)]
    pub locale: Locale,
    /// Optional per-run policy override — the highest-precedence of the
    /// four `DiagnosticIntakePolicy` resolution layers (event override >
    /// named `profile` > `harness.json` `diagnostic_intake.policy`
    /// defaults > built-in default).
    #[serde(default)]
    pub policy: Option<PartialDiagnosticIntakePolicy>,
    /// Optional name of a built-in or `harness.json`-defined policy
    /// profile bundle (e.g. `"baseline"`, `"local-extract"`) to apply for
    /// this run.
    #[serde(default)]
    pub profile: Option<String>,
}

/// A single automation candidate surfaced during the intake interview
/// (`intake.md §3` `WorkflowCandidate`). The `*_evidence` fields hold the
/// client's own words or the interviewer's direct observation — not
/// inference; scoring (`rubric.md §1`) reads these fields directly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowCandidate {
    /// Short label, e.g. "WhatsApp order tracking".
    pub name: String,
    /// 1-3 sentences, plain language.
    pub description: String,
    /// Raw quote or observation feeding the Frequency rubric axis.
    #[serde(default)]
    pub frequency_evidence: String,
    /// Raw quote or observation feeding the Time-cost rubric axis.
    #[serde(default)]
    pub time_cost_evidence: String,
    /// What we know about the systems, APIs, edge cases (Buildability
    /// axis).
    #[serde(default)]
    pub buildability_notes: String,
    /// Who holds the knowledge (e.g. "only Maria knows the supplier
    /// list").
    #[serde(default)]
    pub knowledge_holder: String,
    /// What breaks when this workflow goes wrong.
    #[serde(default)]
    pub failure_mode: String,
}

/// The evidence contract extracted from raw diagnostic-call notes/
/// transcript (`intake.md §3`). Produced by `IntakeExtractNode`; consumed
/// by EN.4.C's scoring node, which imports this type by name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticIntake {
    pub company_name: String,
    /// e.g. "retail SMB", "service business", "e-commerce".
    pub company_type: String,
    pub team_size: u32,
    /// e.g. ["WhatsApp", "Mercado Livre", "Instagram"].
    #[serde(default)]
    pub primary_channels: Vec<String>,
    /// The daily tool stack.
    #[serde(default)]
    pub existing_tools: Vec<String>,
    /// What the client has already built or tried to automate.
    #[serde(default)]
    pub existing_automations: Vec<String>,
    /// The candidate automation workflows surfaced during the interview.
    #[serde(default)]
    pub top_workflows: Vec<WorkflowCandidate>,
}

/// JSON schema matching [`DiagnosticIntake`], passed as
/// `Config.json_schema` by `IntakeExtractNode` for schema-constrained
/// extraction (`openai_compat_transport.rs`'s `response_format` when the
/// `extract` stage is Local-tier).
pub fn diagnostic_intake_json_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "company_name": { "type": "string" },
            "company_type": { "type": "string" },
            "team_size": { "type": "integer" },
            "primary_channels": { "type": "array", "items": { "type": "string" } },
            "existing_tools": { "type": "array", "items": { "type": "string" } },
            "existing_automations": { "type": "array", "items": { "type": "string" } },
            "top_workflows": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "description": { "type": "string" },
                        "frequency_evidence": { "type": "string" },
                        "time_cost_evidence": { "type": "string" },
                        "buildability_notes": { "type": "string" },
                        "knowledge_holder": { "type": "string" },
                        "failure_mode": { "type": "string" },
                    },
                    "required": ["name", "description"],
                },
            },
        },
        "required": ["company_name", "company_type", "team_size", "top_workflows"],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_workflow_candidate() -> WorkflowCandidate {
        WorkflowCandidate {
            name: "WhatsApp order tracking".to_string(),
            description: "Orders are tracked by scrolling WhatsApp threads.".to_string(),
            frequency_evidence: "\"Every single day, multiple times.\"".to_string(),
            time_cost_evidence: "\"Probably an hour a day just searching chats.\"".to_string(),
            buildability_notes: "WhatsApp Business API available; no current integration."
                .to_string(),
            knowledge_holder: "Only Maria knows which chats matter.".to_string(),
            failure_mode: "Orders get lost when Maria is out sick.".to_string(),
        }
    }

    fn sample_diagnostic_intake() -> DiagnosticIntake {
        DiagnosticIntake {
            company_name: "Loja da Ana".to_string(),
            company_type: "retail SMB".to_string(),
            team_size: 4,
            primary_channels: vec!["WhatsApp".to_string(), "Mercado Livre".to_string()],
            existing_tools: vec!["Google Sheets".to_string(), "WhatsApp Business".to_string()],
            existing_automations: vec!["A Zapier flow that broke after two weeks".to_string()],
            top_workflows: vec![sample_workflow_candidate()],
        }
    }

    #[test]
    fn diagnostic_intake_round_trips_through_serde_json_with_no_loss() {
        let intake = sample_diagnostic_intake();
        let json = serde_json::to_string(&intake).expect("serializes");
        let round_tripped: DiagnosticIntake = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(intake, round_tripped);
    }

    #[test]
    fn workflow_candidate_evidence_fields_round_trip_with_no_loss() {
        let candidate = sample_workflow_candidate();
        let json = serde_json::to_string(&candidate).expect("serializes");
        let round_tripped: WorkflowCandidate = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(candidate, round_tripped);
        assert_eq!(
            round_tripped.frequency_evidence,
            candidate.frequency_evidence
        );
        assert_eq!(
            round_tripped.time_cost_evidence,
            candidate.time_cost_evidence
        );
    }

    #[test]
    fn event_schema_deserializes_from_representative_event_data() {
        let json = serde_json::json!({
            "notes": "Client call transcript: ...",
            "profile": "baseline",
        });
        let event: DiagnosticIntakeEventSchema =
            serde_json::from_value(json).expect("deserializes");
        assert_eq!(event.notes, "Client call transcript: ...");
        assert_eq!(event.profile, Some("baseline".to_string()));
        assert_eq!(event.policy, None);
    }

    #[test]
    fn event_schema_deserializes_with_only_notes_present() {
        let json = serde_json::json!({ "notes": "raw notes only" });
        let event: DiagnosticIntakeEventSchema =
            serde_json::from_value(json).expect("deserializes with defaults");
        assert_eq!(event.notes, "raw notes only");
        assert_eq!(event.policy, None);
        assert_eq!(event.profile, None);
    }

    #[test]
    fn diagnostic_intake_schema_requires_core_fields() {
        let schema = diagnostic_intake_json_schema();
        assert_eq!(
            schema["required"],
            serde_json::json!(["company_name", "company_type", "team_size", "top_workflows"])
        );
    }

    #[test]
    fn event_without_locale_defaults_to_pt_br() {
        let json = serde_json::json!({ "notes": "raw notes only" });
        let event: DiagnosticIntakeEventSchema =
            serde_json::from_value(json).expect("deserializes with defaults");
        assert_eq!(event.locale, crate::locale::Locale::PtBr);
    }

    #[test]
    fn event_with_en_us_locale_parses() {
        let json = serde_json::json!({ "notes": "raw notes only", "locale": "en-US" });
        let event: DiagnosticIntakeEventSchema =
            serde_json::from_value(json).expect("deserializes");
        assert_eq!(event.locale, crate::locale::Locale::EnUs);
    }

    #[test]
    fn event_with_unsupported_locale_tag_fails() {
        let json = serde_json::json!({ "notes": "raw notes only", "locale": "en-GB" });
        let result: Result<DiagnosticIntakeEventSchema, _> = serde_json::from_value(json);
        assert!(result.is_err());
    }
}
