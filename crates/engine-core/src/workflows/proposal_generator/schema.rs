//! Event schema (`ProposalGeneratorEventSchema`) and structured output type
//! (`AutomationRoadmap`) for the `PROPOSAL_GENERATOR` workflow, plus the
//! composite/sort/≤3-profile validators and `json_schema` builders.
//!
//! - [`ProposalGeneratorEventSchema`] is the triggering `data` shape: the
//!   company inputs, an optional [`DiagnosticIntake`] (EN.4.B's evidence
//!   contract — when present, `OpportunityIdentifierNode` scores from its
//!   `*_evidence` fields per `rubric.md §1`; when absent it falls back to
//!   the web brief), plus the `policy`/`profile` override-layer fields
//!   mirroring `research_agent::schema::ResearchAgentEventSchema`.
//! - [`AutomationRoadmap`] faithfully models
//!   `agentic-portfolio/business/docs/diagnostic/deliverable.md`'s 4
//!   sections: situation & opportunity, ranked candidates, ≤3 top workflow
//!   profiles, and the recommended first engagement. This name is
//!   **load-bearing** — `EN.4.D` imports it.
//! - [`validate_composite_scores`] / [`validate_candidates_sorted`] /
//!   [`validate_top_profiles_count`] check the deliverable's scoring
//!   invariants from `rubric.md §3-4`.
//! - [`automation_roadmap_json_schema`] builds the `serde_json::Value`
//!   schema passed as `Config.json_schema` by `ProposalWriterNode` /
//!   `ProposalReviseNode`, mirroring
//!   `research_agent::schema::company_brief_json_schema`.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::workflows::diagnostic_intake::DiagnosticIntake;

use super::policy::PartialProposalGeneratorPolicy;

/// Floating-point tolerance for composite-score comparisons.
const COMPOSITE_TOLERANCE: f64 = 0.05;

/// The maximum number of "Top 3 Workflow Profiles" the deliverable's
/// Section 3 may carry (`deliverable.md §2` Section 3).
pub const MAX_TOP_PROFILES: usize = 3;

/// Rubric weight applied to the Frequency axis (`rubric.md §3`).
pub const FREQUENCY_WEIGHT: f64 = 0.35;
/// Rubric weight applied to the Time-cost axis (`rubric.md §3`).
pub const TIME_COST_WEIGHT: f64 = 0.40;
/// Rubric weight applied to the Buildability axis (`rubric.md §3`).
pub const BUILDABILITY_WEIGHT: f64 = 0.25;

/// Compute the composite score for a candidate's three 1-5 axis scores,
/// per `rubric.md §3`: `(freq*0.35) + (time*0.40) + (build*0.25)`.
#[must_use]
pub fn composite_score(frequency: f64, time_cost: f64, buildability: f64) -> f64 {
    frequency * FREQUENCY_WEIGHT + time_cost * TIME_COST_WEIGHT + buildability * BUILDABILITY_WEIGHT
}

/// Inbound event schema for the `PROPOSAL_GENERATOR` workflow. Mirrors the
/// `policy`/`profile` override-layer fields of `ResearchAgentEventSchema` /
/// `SDLCFlowEventSchema`, plus the company inputs
/// `ProposalCompanyResearchNode` seeds from and the optional
/// [`DiagnosticIntake`] evidence contract `OpportunityIdentifierNode`
/// scores from when present.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposalGeneratorEventSchema {
    /// Company name — seeds `ProposalCompanyResearchNode`'s web research and
    /// echoes into the deliverable's Section 1.
    pub company_name: String,
    /// Company URL/domain, if known.
    #[serde(default)]
    pub company_url: Option<String>,
    /// EN.4.B's evidence contract, when a diagnostic-intake call already
    /// happened for this company. When present, `OpportunityIdentifierNode`
    /// scores candidates from its `*_evidence` fields (`rubric.md §1`);
    /// when absent, it falls back to the web research brief.
    #[serde(default)]
    pub diagnostic_intake: Option<DiagnosticIntake>,
    /// Optional per-run policy override — the highest-precedence of the
    /// four `ProposalGeneratorPolicy` resolution layers (event override >
    /// named `profile` > `harness.json` `proposal_generator.policy`
    /// defaults > built-in default).
    #[serde(default)]
    pub policy: Option<PartialProposalGeneratorPolicy>,
    /// Optional name of a built-in or `harness.json`-defined policy
    /// profile bundle (e.g. `"baseline"`, `"local-judgment"`) to apply for
    /// this run.
    #[serde(default)]
    pub profile: Option<String>,
}

/// Priority tier assigned to a ranked candidate by its composite score
/// (`rubric.md §4`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriorityTier {
    /// Composite >= 4.0 — pitch as Phase 1, the first thing built.
    QuickWin,
    /// Composite 2.5-3.9 — pitch as the main engagement.
    CoreBuild,
    /// Composite < 2.5 — real but lower priority, noted on the roadmap.
    Phase2,
}

impl PriorityTier {
    /// Derive the tier from a composite score, per `rubric.md §4`.
    #[must_use]
    pub fn from_composite(composite: f64) -> Self {
        if composite >= 4.0 {
            PriorityTier::QuickWin
        } else if composite >= 2.5 {
            PriorityTier::CoreBuild
        } else {
            PriorityTier::Phase2
        }
    }
}

/// Section 1 — Situation & Opportunity (`deliverable.md §2` Section 1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SituationAndOpportunity {
    pub company_name: String,
    /// e.g. "retail SMB", "service business", "e-commerce".
    pub business_type: String,
    pub team_size: u32,
    /// The most painful manual workflow, described in plain language.
    pub painful_workflow_summary: String,
    /// How many candidate workflows were identified in total.
    #[serde(default)]
    pub candidate_count: u32,
}

/// One row of Section 2's ranking table — a single scored automation
/// candidate (`deliverable.md §2` Section 2, `rubric.md §2`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RankedCandidate {
    pub name: String,
    /// 1-5, per `rubric.md`'s Frequency anchors.
    pub frequency: f64,
    /// 1-5, per `rubric.md`'s Time-cost anchors.
    pub time_cost: f64,
    /// 1-5, per `rubric.md`'s Buildability anchors.
    pub buildability: f64,
    /// `(frequency*0.35) + (time_cost*0.40) + (buildability*0.25)`.
    pub composite: f64,
    pub tier: PriorityTier,
    /// 1-2 sentence plain-language explanation of the score.
    #[serde(default)]
    pub rationale: String,
}

/// Section 3 — one Top Workflow Profile page (`deliverable.md §2` Section
/// 3). At most [`MAX_TOP_PROFILES`] of these may appear in a well-formed
/// [`AutomationRoadmap`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowProfile {
    pub name: String,
    /// The manual process today, in plain language.
    pub today: String,
    /// The proposed automated version, including the human-review gate.
    pub proposed_solution: String,
    /// Tools involved, named plainly.
    pub stack: String,
    /// e.g. "2-3 weeks to a working version."
    pub rough_scope: String,
    /// e.g. "Saves approximately 5 hrs/week across the ops team."
    pub expected_roi: String,
}

/// Section 4 — Recommended First Engagement (`deliverable.md §2` Section
/// 4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FirstEngagement {
    /// The recommended starting workflow's name — the highest-scoring
    /// Quick Win, or the highest Core Build if there is no Quick Win
    /// (`rubric.md §4`).
    pub start_with: String,
    #[serde(default)]
    pub phase_1_scope: Vec<String>,
    /// e.g. "R$8,000-12,000 fixed fee."
    pub investment: String,
    pub how_it_works: String,
    pub call_to_action: String,
}

/// The proposal-generator deliverable — faithfully models
/// `agentic-portfolio/business/docs/diagnostic/deliverable.md`'s 4
/// sections. This name is **load-bearing** — `EN.4.D` imports it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AutomationRoadmap {
    pub situation: Option<SituationAndOpportunity>,
    #[serde(default)]
    pub candidates: Vec<RankedCandidate>,
    /// The top (at most [`MAX_TOP_PROFILES`]) candidates' one-page profiles
    /// (`deliverable.md §2` Section 3).
    #[serde(default)]
    pub top_profiles: Vec<WorkflowProfile>,
    pub recommendation: Option<FirstEngagement>,
}

/// Validation error surfaced by [`validate_composite_scores`],
/// [`validate_candidates_sorted`], or [`validate_top_profiles_count`].
#[derive(Debug, Clone, PartialEq)]
pub enum AutomationRoadmapValidationError {
    CompositeMismatch {
        index: usize,
        name: String,
        actual: f64,
        expected: f64,
    },
    NotSortedDescending {
        index: usize,
        name: String,
        composite: f64,
    },
    TooManyTopProfiles {
        actual: usize,
        max: usize,
    },
}

impl fmt::Display for AutomationRoadmapValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AutomationRoadmapValidationError::CompositeMismatch {
                index,
                name,
                actual,
                expected,
            } => write!(
                f,
                "candidate {index} ({name:?}) composite {actual:.2} does not match \
                 (freq*0.35)+(time*0.40)+(build*0.25) = {expected:.2}"
            ),
            AutomationRoadmapValidationError::NotSortedDescending {
                index,
                name,
                composite,
            } => write!(
                f,
                "candidates are not sorted composite-descending: index {index} \
                 ({name:?}, composite {composite:.2}) follows a higher composite"
            ),
            AutomationRoadmapValidationError::TooManyTopProfiles { actual, max } => write!(
                f,
                "too many top workflow profiles: {actual} exceeds the max of {max}"
            ),
        }
    }
}

impl std::error::Error for AutomationRoadmapValidationError {}

/// Validate that every candidate's `composite` field matches
/// `(freq*0.35)+(time*0.40)+(build*0.25)` within a small float tolerance
/// (`rubric.md §3`).
pub fn validate_composite_scores(
    roadmap: &AutomationRoadmap,
) -> Result<(), AutomationRoadmapValidationError> {
    for (index, candidate) in roadmap.candidates.iter().enumerate() {
        let expected = composite_score(
            candidate.frequency,
            candidate.time_cost,
            candidate.buildability,
        );
        if (candidate.composite - expected).abs() > COMPOSITE_TOLERANCE {
            return Err(AutomationRoadmapValidationError::CompositeMismatch {
                index,
                name: candidate.name.clone(),
                actual: candidate.composite,
                expected,
            });
        }
    }
    Ok(())
}

/// Validate that `roadmap.candidates` is sorted composite-descending
/// (`deliverable.md §2` Section 2's ranking table).
pub fn validate_candidates_sorted(
    roadmap: &AutomationRoadmap,
) -> Result<(), AutomationRoadmapValidationError> {
    for index in 1..roadmap.candidates.len() {
        let prev = &roadmap.candidates[index - 1];
        let next = &roadmap.candidates[index];
        if next.composite > prev.composite + COMPOSITE_TOLERANCE {
            return Err(AutomationRoadmapValidationError::NotSortedDescending {
                index,
                name: next.name.clone(),
                composite: next.composite,
            });
        }
    }
    Ok(())
}

/// Validate that `roadmap.top_profiles` carries at most
/// [`MAX_TOP_PROFILES`] entries (`deliverable.md §2` Section 3 — "one
/// profile per the three highest-composite candidates").
pub fn validate_top_profiles_count(
    roadmap: &AutomationRoadmap,
) -> Result<(), AutomationRoadmapValidationError> {
    if roadmap.top_profiles.len() > MAX_TOP_PROFILES {
        return Err(AutomationRoadmapValidationError::TooManyTopProfiles {
            actual: roadmap.top_profiles.len(),
            max: MAX_TOP_PROFILES,
        });
    }
    Ok(())
}

/// Run all three deliverable-scoring validators, short-circuiting on the
/// first failure.
pub fn validate_automation_roadmap(
    roadmap: &AutomationRoadmap,
) -> Result<(), AutomationRoadmapValidationError> {
    validate_composite_scores(roadmap)?;
    validate_candidates_sorted(roadmap)?;
    validate_top_profiles_count(roadmap)?;
    Ok(())
}

/// JSON schema matching [`AutomationRoadmap`], passed as
/// `Config.json_schema` by `ProposalWriterNode` / `ProposalReviseNode` for
/// schema-constrained replies.
pub fn automation_roadmap_json_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "situation": {
                "type": "object",
                "properties": {
                    "company_name": { "type": "string" },
                    "business_type": { "type": "string" },
                    "team_size": { "type": "integer" },
                    "painful_workflow_summary": { "type": "string" },
                    "candidate_count": { "type": "integer" },
                },
                "required": ["company_name", "business_type", "team_size", "painful_workflow_summary"],
            },
            "candidates": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "frequency": { "type": "number" },
                        "time_cost": { "type": "number" },
                        "buildability": { "type": "number" },
                        "composite": { "type": "number" },
                        "tier": { "type": "string", "enum": ["quick_win", "core_build", "phase2"] },
                        "rationale": { "type": "string" },
                    },
                    "required": ["name", "frequency", "time_cost", "buildability", "composite", "tier"],
                },
            },
            "top_profiles": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "today": { "type": "string" },
                        "proposed_solution": { "type": "string" },
                        "stack": { "type": "string" },
                        "rough_scope": { "type": "string" },
                        "expected_roi": { "type": "string" },
                    },
                    "required": ["name", "today", "proposed_solution", "stack", "rough_scope", "expected_roi"],
                },
            },
            "recommendation": {
                "type": "object",
                "properties": {
                    "start_with": { "type": "string" },
                    "phase_1_scope": { "type": "array", "items": { "type": "string" } },
                    "investment": { "type": "string" },
                    "how_it_works": { "type": "string" },
                    "call_to_action": { "type": "string" },
                },
                "required": ["start_with", "investment", "how_it_works", "call_to_action"],
            },
        },
        "required": ["situation", "candidates", "top_profiles", "recommendation"],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::diagnostic_intake::schema::WorkflowCandidate;

    fn candidate(name: &str, frequency: f64, time_cost: f64, buildability: f64) -> RankedCandidate {
        let composite = composite_score(frequency, time_cost, buildability);
        RankedCandidate {
            name: name.to_string(),
            frequency,
            time_cost,
            buildability,
            composite,
            tier: PriorityTier::from_composite(composite),
            rationale: format!("{name} rationale"),
        }
    }

    fn sample_situation() -> SituationAndOpportunity {
        SituationAndOpportunity {
            company_name: "Loja da Ana".to_string(),
            business_type: "retail SMB".to_string(),
            team_size: 4,
            painful_workflow_summary: "Orders tracked by scrolling WhatsApp threads.".to_string(),
            candidate_count: 2,
        }
    }

    fn sample_engagement() -> FirstEngagement {
        FirstEngagement {
            start_with: "WhatsApp order tracking".to_string(),
            phase_1_scope: vec!["Order intake bot".to_string()],
            investment: "R$8,000-12,000 fixed fee".to_string(),
            how_it_works: "Connects to WhatsApp Business API.".to_string(),
            call_to_action: "Book a call to proceed.".to_string(),
        }
    }

    fn sample_roadmap() -> AutomationRoadmap {
        AutomationRoadmap {
            situation: Some(sample_situation()),
            candidates: vec![
                candidate("WhatsApp order tracking", 5.0, 4.0, 4.0),
                candidate("Supplier follow-up messages", 4.0, 3.0, 4.0),
            ],
            top_profiles: vec![WorkflowProfile {
                name: "WhatsApp order tracking".to_string(),
                today: "Manually scrolled.".to_string(),
                proposed_solution: "Automated bot with human approval gate.".to_string(),
                stack: "WhatsApp Business API + small service.".to_string(),
                rough_scope: "2-3 weeks.".to_string(),
                expected_roi: "Saves ~5 hrs/week.".to_string(),
            }],
            recommendation: Some(sample_engagement()),
        }
    }

    #[test]
    fn event_schema_round_trips_with_diagnostic_intake() {
        let event = ProposalGeneratorEventSchema {
            company_name: "Loja da Ana".to_string(),
            company_url: Some("https://loja-da-ana.example".to_string()),
            diagnostic_intake: Some(DiagnosticIntake {
                company_name: "Loja da Ana".to_string(),
                company_type: "retail SMB".to_string(),
                team_size: 4,
                primary_channels: vec!["WhatsApp".to_string()],
                existing_tools: vec!["Google Sheets".to_string()],
                existing_automations: vec![],
                top_workflows: vec![WorkflowCandidate {
                    name: "WhatsApp order tracking".to_string(),
                    description: "Orders tracked in chat.".to_string(),
                    frequency_evidence: "\"Every day.\"".to_string(),
                    time_cost_evidence: "\"An hour a day.\"".to_string(),
                    buildability_notes: "API available.".to_string(),
                    knowledge_holder: "Maria.".to_string(),
                    failure_mode: "Orders lost.".to_string(),
                }],
            }),
            policy: None,
            profile: Some("baseline".to_string()),
        };
        let json = serde_json::to_string(&event).expect("serializes");
        let round_tripped: ProposalGeneratorEventSchema =
            serde_json::from_str(&json).expect("deserializes");
        assert_eq!(event, round_tripped);
    }

    #[test]
    fn event_schema_deserializes_without_diagnostic_intake() {
        let json = serde_json::json!({ "company_name": "Loja da Ana" });
        let event: ProposalGeneratorEventSchema =
            serde_json::from_value(json).expect("deserializes with defaults");
        assert_eq!(event.company_name, "Loja da Ana");
        assert_eq!(event.diagnostic_intake, None);
        assert_eq!(event.policy, None);
        assert_eq!(event.profile, None);
    }

    #[test]
    fn automation_roadmap_round_trips_through_serde_json_with_no_loss() {
        let roadmap = sample_roadmap();
        let json = serde_json::to_string(&roadmap).expect("serializes");
        let round_tripped: AutomationRoadmap = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(roadmap, round_tripped);
    }

    #[test]
    fn priority_tier_derives_from_composite_per_rubric_thresholds() {
        assert_eq!(PriorityTier::from_composite(4.5), PriorityTier::QuickWin);
        assert_eq!(PriorityTier::from_composite(4.0), PriorityTier::QuickWin);
        assert_eq!(PriorityTier::from_composite(3.9), PriorityTier::CoreBuild);
        assert_eq!(PriorityTier::from_composite(2.5), PriorityTier::CoreBuild);
        assert_eq!(PriorityTier::from_composite(2.4), PriorityTier::Phase2);
    }

    #[test]
    fn validate_composite_scores_accepts_well_formed_roadmap() {
        let roadmap = sample_roadmap();
        assert!(validate_composite_scores(&roadmap).is_ok());
    }

    #[test]
    fn validate_composite_scores_rejects_malformed_composite() {
        let mut roadmap = sample_roadmap();
        roadmap.candidates[0].composite = 1.0; // wrong on purpose
        let err = validate_composite_scores(&roadmap).unwrap_err();
        assert!(matches!(
            err,
            AutomationRoadmapValidationError::CompositeMismatch { index: 0, .. }
        ));
    }

    #[test]
    fn validate_candidates_sorted_accepts_descending_order() {
        let roadmap = sample_roadmap();
        assert!(validate_candidates_sorted(&roadmap).is_ok());
    }

    #[test]
    fn validate_candidates_sorted_rejects_out_of_order_candidates() {
        let mut roadmap = sample_roadmap();
        roadmap.candidates.reverse();
        let err = validate_candidates_sorted(&roadmap).unwrap_err();
        assert!(matches!(
            err,
            AutomationRoadmapValidationError::NotSortedDescending { .. }
        ));
    }

    #[test]
    fn validate_top_profiles_count_accepts_up_to_three() {
        let roadmap = sample_roadmap();
        assert!(validate_top_profiles_count(&roadmap).is_ok());
    }

    #[test]
    fn validate_top_profiles_count_rejects_more_than_three() {
        let mut roadmap = sample_roadmap();
        let profile = roadmap.top_profiles[0].clone();
        roadmap.top_profiles = vec![profile.clone(), profile.clone(), profile.clone(), profile];
        let err = validate_top_profiles_count(&roadmap).unwrap_err();
        assert!(matches!(
            err,
            AutomationRoadmapValidationError::TooManyTopProfiles { actual: 4, max: 3 }
        ));
    }

    #[test]
    fn validate_automation_roadmap_accepts_well_formed_roadmap() {
        let roadmap = sample_roadmap();
        assert!(validate_automation_roadmap(&roadmap).is_ok());
    }

    #[test]
    fn automation_roadmap_json_schema_requires_all_four_sections() {
        let schema = automation_roadmap_json_schema();
        assert_eq!(
            schema["required"],
            serde_json::json!(["situation", "candidates", "top_profiles", "recommendation"])
        );
    }
}
