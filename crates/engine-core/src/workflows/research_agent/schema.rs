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

/// A reachable contact channel surfaced during a research run — shaped
/// field-for-field like okf-core's `Contact` (`../okf-core/src/doc/
/// opportunity.rs`) so `MergeContactsNode` (task 7) can hand this straight to
/// mev's `plan_merge_contacts` without a lossy remap. Every field is
/// `#[serde(default)]`-tolerant so a partial model response still
/// deserializes rather than failing the whole brief/lead.
///
/// **Anti-fabrication contract (load-bearing).** Only ever populate a field
/// with a channel that appeared verbatim in a fetched source. Never
/// construct an email/phone/handle from a domain or a person's name. A
/// generic channel with no named human (e.g. `contato@acme.example`, a
/// storefront WhatsApp number) is still a valid contact — record it with an
/// empty `name` rather than discarding it. An empty `contacts[]` on the
/// enclosing brief/lead is the correct, expected answer when nothing
/// reachable was found; that reason belongs in `note` or the brief summary,
/// never invented data.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ResearchContact {
    /// The named individual this channel reaches, if one was identified.
    /// Empty for a generic/company-level channel (e.g. `contato@`, a
    /// storefront WhatsApp) — that is a valid contact, not a missing one.
    #[serde(default)]
    pub name: String,
    /// The named individual's role/title, if stated in the source.
    #[serde(default)]
    pub role: String,
    /// Email addresses seen verbatim in a fetched source.
    #[serde(default)]
    pub emails: Vec<String>,
    /// WhatsApp numbers/links seen verbatim in a fetched source.
    #[serde(default)]
    pub whatsapp: Vec<String>,
    /// Phone numbers seen verbatim in a fetched source.
    #[serde(default)]
    pub phones: Vec<String>,
    /// Other reachable links (LinkedIn/Instagram/Facebook profile, contact
    /// page, etc.) seen verbatim in a fetched source.
    #[serde(default)]
    pub links: Vec<String>,
    /// Free-form context — e.g. why this channel was recorded, or why no
    /// contact could be found for this brief/lead.
    #[serde(default)]
    pub note: String,
}

/// A single, named company's research brief — the structured output of
/// `CompanyResearchNode`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompanyBrief {
    /// The researched company's name (echoes/normalizes the event input).
    /// **Load-bearing key** — mev's `detect_kind` classifies a brief by this
    /// field's presence; do not rename or remove it.
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
    /// Reachable contact channels surfaced during the run. Empty is the
    /// correct answer when nothing verbatim was found — see
    /// [`ResearchContact`]'s anti-fabrication contract.
    #[serde(default)]
    pub contacts: Vec<ResearchContact>,
    /// The company's URL/domain. Deterministically stamped from the
    /// trigger event's `company_url` by `CompanyResearchNode` when the event
    /// carries one (task 4) — this field is not solely model-dependent.
    #[serde(default)]
    pub company_url: Option<String>,
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
    /// Reachable contact channels surfaced for this prospect. Empty is the
    /// normal, expected result for most leads — see
    /// [`ResearchContact`]'s anti-fabrication contract.
    #[serde(default)]
    pub contacts: Vec<ResearchContact>,
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

/// Shared sub-schema for a [`ResearchContact`] entry, used by both
/// `company_brief_json_schema()` and `prospecting_result_json_schema()` so
/// the shape stays identical wherever `contacts` appears. `required` is at
/// most `["name"]` intentionally left EMPTY — even `name` is optional, since
/// a generic channel with no named human is still a valid contact. This
/// sub-schema is INVARIANT across `contact_enrichment` policy settings
/// (task 3+); only the prompt text varies with depth, never the emitted
/// schema, so `detect_kind` and the okf-core mapping stay stable regardless
/// of policy.
fn research_contact_json_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "role": { "type": "string" },
            "emails": { "type": "array", "items": { "type": "string" } },
            "whatsapp": { "type": "array", "items": { "type": "string" } },
            "phones": { "type": "array", "items": { "type": "string" } },
            "links": { "type": "array", "items": { "type": "string" } },
            "note": { "type": "string" },
        },
        "required": [],
    })
}

/// JSON schema matching [`CompanyBrief`], passed as `Config.json_schema` by
/// `CompanyResearchNode`. `contacts` is deliberately absent from
/// `required` — an empty `contacts[]` is a valid, expected answer.
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
            "contacts": { "type": "array", "items": research_contact_json_schema() },
            "company_url": { "type": "string" },
        },
        "required": ["company_name", "summary"],
    })
}

/// JSON schema matching [`ProspectingResult`], passed as `Config.json_schema`
/// by `ProspectingResearchNode`. `contacts` (per-prospect) is deliberately
/// absent from each prospect's `required` list — an empty `contacts[]` is a
/// valid, expected answer for most leads.
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
                        "contacts": { "type": "array", "items": research_contact_json_schema() },
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
            contacts: vec![ResearchContact {
                name: "Jane Founder".to_string(),
                role: "Founder".to_string(),
                emails: vec!["jane@acme.example".to_string()],
                whatsapp: vec![],
                phones: vec![],
                links: vec![],
                note: String::new(),
            }],
            company_url: Some("https://acme.example".to_string()),
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
                contacts: vec![],
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

    #[test]
    fn research_contact_round_trips_through_serde_json() {
        let contact = ResearchContact {
            name: "Jane Founder".to_string(),
            role: "Founder".to_string(),
            emails: vec!["jane@acme.example".to_string()],
            whatsapp: vec!["+55 11 99999-0000".to_string()],
            phones: vec![],
            links: vec!["https://linkedin.com/in/jane".to_string()],
            note: "Named decision-maker".to_string(),
        };
        let json = serde_json::to_string(&contact).expect("serializes");
        let round_tripped: ResearchContact = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(contact, round_tripped);
    }

    #[test]
    fn research_contact_defaults_when_fields_absent() {
        let contact: ResearchContact =
            serde_json::from_value(serde_json::json!({})).expect("deserializes with defaults");
        assert_eq!(contact, ResearchContact::default());
    }

    #[test]
    fn company_brief_deserializes_with_no_contacts_or_company_url_keys() {
        let json = serde_json::json!({
            "company_name": "Acme Corp",
            "summary": "Widget manufacturer.",
        });
        let brief: CompanyBrief = serde_json::from_value(json).expect("deserializes with defaults");
        assert_eq!(brief.contacts, Vec::new());
        assert_eq!(brief.company_url, None);
    }

    #[test]
    fn prospect_lead_deserializes_with_no_contacts_key() {
        let json = serde_json::json!({
            "name": "Jane Doe Legal",
            "pillar": "automation",
        });
        let lead: ProspectLead = serde_json::from_value(json).expect("deserializes with defaults");
        assert_eq!(lead.contacts, Vec::new());
    }

    #[test]
    fn company_brief_schema_does_not_require_contacts() {
        let schema = company_brief_json_schema();
        let required = schema["required"].as_array().expect("required is an array");
        assert!(!required.iter().any(|v| v == "contacts"));
        // The contact sub-schema itself is likewise not name-required.
        assert_eq!(
            schema["properties"]["contacts"]["items"]["required"],
            serde_json::json!([])
        );
    }

    #[test]
    fn prospecting_result_schema_does_not_require_contacts() {
        let schema = prospecting_result_json_schema();
        let prospect_required = schema["properties"]["prospects"]["items"]["required"]
            .as_array()
            .expect("required is an array");
        assert!(!prospect_required.iter().any(|v| v == "contacts"));
    }

    #[test]
    fn detect_kind_guard_keys_survive_serialization() {
        // mev's `detect_kind` classifies a brief by the presence of
        // `company_name`, and a prospecting result by `prospects`/
        // `vertical`. Contact enrichment must never move or rename these.
        let brief = CompanyBrief {
            company_name: "Acme Corp".to_string(),
            summary: "Widget manufacturer.".to_string(),
            recent_developments: vec![],
            pain_points: vec![],
            outreach_hooks: vec![],
            sources: vec![],
            contacts: vec![],
            company_url: None,
        };
        let brief_json = serde_json::to_value(&brief).expect("serializes");
        assert!(brief_json.get("company_name").is_some());

        let result = ProspectingResult {
            vertical: "legal-tech".to_string(),
            prospects: vec![],
            common_pain_points: vec![],
            sources: vec![],
        };
        let result_json = serde_json::to_value(&result).expect("serializes");
        assert!(result_json.get("prospects").is_some());
        assert!(result_json.get("vertical").is_some());
    }
}
