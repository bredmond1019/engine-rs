//! `ResearchAgentEventSchema`, `ResearchMode`, and the two structured output
//! types (`CompanyBrief` / `ProspectingResult`) — filled in task 3.
//!
//! - [`ResearchAgentEventSchema`] is the triggering `data` shape: a
//!   `mode: ResearchMode` field routes `ResearchModeRouterNode` (task 7) to
//!   the matching terminal node, plus a `policy`/`profile` override pair
//!   mirroring `sdlc_flow::schema::SDLCFlowEventSchema`, plus the per-mode
//!   inputs (`company_name`/`company_url` for [`ResearchMode::Company`],
//!   `vertical`/`topic` for [`ResearchMode::Prospecting`]).
//! - [`CompanyBrief`] is the structured output of `CompanyResearchNode`
//!   (task 5): a single-company research brief.
//! - [`ProspectingResult`] is the structured output of
//!   `ProspectingResearchNode` (task 6): a forum/web sweep distilled into
//!   pain points, a four-pillar vertical mapping, and outreach hooks.
//! - [`company_brief_json_schema`] / [`prospecting_result_json_schema`]
//!   build the `serde_json::Value` schemas passed as `Config.json_schema` by
//!   the two terminal nodes, mirroring
//!   `sdlc_flow::task_loop::implement_output_schema`.

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::policy::PartialResearchAgentPolicy;

/// Which terminal node `ResearchModeRouterNode` routes to: a single-company
/// brief, or a broader prospecting sweep. Serializes to the lowercase
/// strings used in `event.mode` (`"company"` / `"prospecting"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResearchMode {
    Company,
    Prospecting,
}

/// Inbound event schema for the `RESEARCH_AGENT` workflow. Mirrors the
/// `policy`/`profile` override-layer fields of `SDLCFlowEventSchema`
/// (`sdlc_flow::schema`), plus a `mode` field and the per-mode inputs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResearchAgentEventSchema {
    /// Which terminal node this run routes to.
    pub mode: ResearchMode,
    /// Company name to research (`ResearchMode::Company`).
    #[serde(default)]
    pub company_name: Option<String>,
    /// Company URL/domain, if known (`ResearchMode::Company`).
    #[serde(default)]
    pub company_url: Option<String>,
    /// Vertical/industry seed to sweep for prospects
    /// (`ResearchMode::Prospecting`).
    #[serde(default)]
    pub vertical: Option<String>,
    /// Free-form topic seed narrowing the prospecting sweep
    /// (`ResearchMode::Prospecting`).
    #[serde(default)]
    pub topic: Option<String>,
    /// Optional per-run policy override — the highest-precedence of the
    /// four `ResearchAgentPolicy` resolution layers (event override >
    /// named `profile` > `harness.json` `research_agent.policy` defaults >
    /// built-in default).
    #[serde(default)]
    pub policy: Option<PartialResearchAgentPolicy>,
    /// Optional name of a built-in or `harness.json`-defined policy
    /// profile bundle (e.g. `"baseline"`) to apply for this run.
    #[serde(default)]
    pub profile: Option<String>,
}

/// A single, named company's research brief — the structured output of
/// `CompanyResearchNode`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompanyBrief {
    /// The researched company's name (echoes/normalizes the event input).
    pub company_name: String,
    /// A short paragraph summarizing what the company does.
    pub summary: String,
    /// Notable recent developments (funding, launches, hiring signals).
    #[serde(default)]
    pub recent_developments: Vec<String>,
    /// Likely pain points this practice's services could address.
    #[serde(default)]
    pub pain_points: Vec<String>,
    /// Suggested angles/hooks for outreach to this company.
    #[serde(default)]
    pub outreach_hooks: Vec<String>,
    /// Source URLs the brief was drawn from.
    #[serde(default)]
    pub sources: Vec<String>,
}

/// A single prospect discovered during a prospecting sweep, mapped onto one
/// of the four service pillars.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProspectLead {
    /// Name of the prospective company/individual, if identifiable.
    pub name: String,
    /// The pain point(s) surfaced for this prospect.
    #[serde(default)]
    pub pain_points: Vec<String>,
    /// Which of the four service pillars this prospect maps to.
    pub pillar: String,
    /// Suggested outreach hook for this prospect.
    #[serde(default)]
    pub outreach_hook: Option<String>,
    /// Source URL where this prospect was found.
    #[serde(default)]
    pub source: Option<String>,
}

/// A forum/web sweep distilled into pain points, a four-pillar vertical
/// mapping, and outreach hooks — the structured output of
/// `ProspectingResearchNode`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProspectingResult {
    /// The vertical/topic this sweep targeted (echoes the event input).
    pub vertical: String,
    /// Individual prospects surfaced by the sweep.
    #[serde(default)]
    pub prospects: Vec<ProspectLead>,
    /// Cross-cutting pain-point themes observed across the sweep.
    #[serde(default)]
    pub common_pain_points: Vec<String>,
    /// Source URLs the sweep was drawn from.
    #[serde(default)]
    pub sources: Vec<String>,
}

/// JSON schema matching [`CompanyBrief`], passed as `Config.json_schema` by
/// `CompanyResearchNode`.
pub fn company_brief_json_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "company_name": { "type": "string" },
            "summary": { "type": "string" },
            "recent_developments": { "type": "array", "items": { "type": "string" } },
            "pain_points": { "type": "array", "items": { "type": "string" } },
            "outreach_hooks": { "type": "array", "items": { "type": "string" } },
            "sources": { "type": "array", "items": { "type": "string" } },
        },
        "required": ["company_name", "summary"],
    })
}

/// JSON schema matching [`ProspectingResult`], passed as `Config.json_schema`
/// by `ProspectingResearchNode`.
pub fn prospecting_result_json_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "vertical": { "type": "string" },
            "prospects": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "pain_points": { "type": "array", "items": { "type": "string" } },
                        "pillar": { "type": "string" },
                        "outreach_hook": { "type": "string" },
                        "source": { "type": "string" },
                    },
                    "required": ["name", "pillar"],
                },
            },
            "common_pain_points": { "type": "array", "items": { "type": "string" } },
            "sources": { "type": "array", "items": { "type": "string" } },
        },
        "required": ["vertical"],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_serializes_to_expected_strings() {
        assert_eq!(
            serde_json::to_value(ResearchMode::Company).unwrap(),
            serde_json::json!("company")
        );
        assert_eq!(
            serde_json::to_value(ResearchMode::Prospecting).unwrap(),
            serde_json::json!("prospecting")
        );
    }

    #[test]
    fn mode_deserializes_from_expected_strings() {
        let company: ResearchMode = serde_json::from_value(serde_json::json!("company")).unwrap();
        assert_eq!(company, ResearchMode::Company);
        let prospecting: ResearchMode =
            serde_json::from_value(serde_json::json!("prospecting")).unwrap();
        assert_eq!(prospecting, ResearchMode::Prospecting);
    }

    #[test]
    fn event_schema_round_trips_company_mode() {
        let event = ResearchAgentEventSchema {
            mode: ResearchMode::Company,
            company_name: Some("Acme Corp".to_string()),
            company_url: Some("https://acme.example".to_string()),
            vertical: None,
            topic: None,
            policy: None,
            profile: Some("baseline".to_string()),
        };
        let json = serde_json::to_string(&event).expect("serializes");
        let round_tripped: ResearchAgentEventSchema =
            serde_json::from_str(&json).expect("deserializes");
        assert_eq!(event, round_tripped);
    }

    #[test]
    fn event_schema_round_trips_prospecting_mode() {
        let event = ResearchAgentEventSchema {
            mode: ResearchMode::Prospecting,
            company_name: None,
            company_url: None,
            vertical: Some("legal-tech".to_string()),
            topic: Some("contract review pain points".to_string()),
            policy: None,
            profile: None,
        };
        let json = serde_json::to_string(&event).expect("serializes");
        let round_tripped: ResearchAgentEventSchema =
            serde_json::from_str(&json).expect("deserializes");
        assert_eq!(event, round_tripped);
    }

    #[test]
    fn event_schema_deserializes_with_only_mode_present() {
        let json = serde_json::json!({ "mode": "company" });
        let event: ResearchAgentEventSchema =
            serde_json::from_value(json).expect("deserializes with defaults");
        assert_eq!(event.mode, ResearchMode::Company);
        assert_eq!(event.company_name, None);
        assert_eq!(event.policy, None);
        assert_eq!(event.profile, None);
    }

    #[test]
    fn company_brief_round_trips_through_serde_json() {
        let brief = CompanyBrief {
            company_name: "Acme Corp".to_string(),
            summary: "Widget manufacturer expanding into SaaS.".to_string(),
            recent_developments: vec!["Raised Series B".to_string()],
            pain_points: vec!["Manual invoicing".to_string()],
            outreach_hooks: vec!["Recent Series B raise".to_string()],
            sources: vec!["https://acme.example/news".to_string()],
        };
        let json = serde_json::to_string(&brief).expect("serializes");
        let round_tripped: CompanyBrief = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(brief, round_tripped);
    }

    #[test]
    fn prospecting_result_round_trips_through_serde_json() {
        let result = ProspectingResult {
            vertical: "legal-tech".to_string(),
            prospects: vec![ProspectLead {
                name: "Jane Doe Legal".to_string(),
                pain_points: vec!["Slow contract turnaround".to_string()],
                pillar: "automation".to_string(),
                outreach_hook: Some("Posted about contract delays on r/legaltech".to_string()),
                source: Some("https://reddit.com/r/legaltech/abc".to_string()),
            }],
            common_pain_points: vec!["Manual contract review".to_string()],
            sources: vec!["https://reddit.com/r/legaltech".to_string()],
        };
        let json = serde_json::to_string(&result).expect("serializes");
        let round_tripped: ProspectingResult = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(result, round_tripped);
    }

    #[test]
    fn company_brief_schema_requires_name_and_summary() {
        let schema = company_brief_json_schema();
        assert_eq!(
            schema["required"],
            serde_json::json!(["company_name", "summary"])
        );
    }

    #[test]
    fn prospecting_result_schema_requires_vertical() {
        let schema = prospecting_result_json_schema();
        assert_eq!(schema["required"], serde_json::json!(["vertical"]));
    }
}
