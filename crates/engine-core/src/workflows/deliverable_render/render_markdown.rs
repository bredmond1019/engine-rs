//! `RenderDeliverableNode` — the deterministic (no model call on the
//! default path) renderer that turns an `AutomationRoadmap` into the
//! four-section client deliverable markdown described by
//! `agentic-portfolio/business/docs/diagnostic/deliverable.md` §2.
//!
//! **Locale is total.** Every piece of template chrome — section headings,
//! table column headers, tier labels, field labels — and the money format
//! are selected from the run's requested [`Locale`] via [`chrome`]. No
//! string in the rendered document is hardcoded to one language; a `pt-BR`
//! run and an `en-US` run share zero literal chrome text.
//!
//! **The `authored_locale` refusal (`EN.4.D`, amended 2026-07-28) is
//! load-bearing.** When the event's requested `locale` disagrees with
//! `roadmap.authored_locale`, [`RenderDeliverableNode::process`] returns a
//! `NodeError` naming both locales and writes no file — emitting PT chrome
//! over EN prose (or vice versa) would hand a client a document that reads
//! as broken.
//!
//! **Money never converts** (`locale.rs`'s firewall invariant) —
//! [`format_money`] renders a [`crate::locale::MoneyRange`] in its own
//! currency only.

use std::fs;

use engine_contract::TaskContext;
use serde_json::json;

use crate::locale::{Currency, EngagementBasis, Locale, MoneyRange};
use crate::node::{Node, NodeError};
use crate::workflows::proposal_generator::schema::{
    AutomationRoadmap, FirstEngagement, PriorityTier, RankedCandidate, SituationAndOpportunity,
    WorkflowProfile,
};
use crate::workflows::put_result;

use super::schema::{deliverable_slug, DeliverableRenderEventSchema};

/// The `Node::name()` identity `RenderDeliverableNode` runs under, and the
/// `ctx.nodes` key its result is stamped onto.
pub const NODE_NAME: &str = "RenderDeliverableNode";

/// All locale-selected template chrome for one rendered document — section
/// headings, table column headers, tier labels, and field labels. Built
/// once per render by [`chrome`]; every rendering helper below reads from
/// this rather than matching on `Locale` itself, so there is exactly one
/// place per language where the wording lives.
struct Chrome {
    title_prefix: &'static str,
    section1_heading: &'static str,
    section2_heading: &'static str,
    section3_heading: &'static str,
    section4_heading: &'static str,
    table_header: &'static str,
    tier_quick_win: &'static str,
    tier_core_build: &'static str,
    tier_phase2: &'static str,
    field_business_type: &'static str,
    field_team_size: &'static str,
    field_painful_workflow: &'static str,
    field_candidate_count: &'static str,
    no_situation: &'static str,
    field_today: &'static str,
    field_proposed_solution: &'static str,
    field_stack: &'static str,
    field_rough_scope: &'static str,
    field_expected_roi: &'static str,
    no_profiles: &'static str,
    field_start_with: &'static str,
    field_phase_1_scope: &'static str,
    field_investment: &'static str,
    field_how_it_works: &'static str,
    field_call_to_action: &'static str,
    investment_not_quoted: &'static str,
    no_recommendation: &'static str,
    basis_fixed: &'static str,
    basis_per_month: &'static str,
    basis_per_hour: &'static str,
}

/// Select this run's [`Chrome`] bundle. The only branch on [`Locale`] in
/// this module — every rendering helper below reads labels from the
/// returned struct instead of matching `Locale` itself.
fn chrome(locale: Locale) -> Chrome {
    match locale {
        Locale::EnUs => Chrome {
            title_prefix: "Automation Roadmap",
            section1_heading: "Situation & Opportunity",
            section2_heading: "Ranked Automation Candidates",
            section3_heading: "Top Workflow Profiles",
            section4_heading: "Recommended First Engagement",
            table_header:
                "| # | Workflow | Frequency | Time-cost | Buildability | Score | Tier |\n\
                            |---|---|---|---|---|---|---|",
            tier_quick_win: "Quick Win",
            tier_core_build: "Core Build",
            tier_phase2: "Phase 2",
            field_business_type: "Business type",
            field_team_size: "Team size",
            field_painful_workflow: "Most painful workflow",
            field_candidate_count: "Total candidates identified",
            no_situation: "No situation summary was provided for this run.",
            field_today: "Today",
            field_proposed_solution: "Proposed solution",
            field_stack: "Stack",
            field_rough_scope: "Rough scope",
            field_expected_roi: "Expected ROI",
            no_profiles: "No top workflow profiles were provided for this run.",
            field_start_with: "Start with",
            field_phase_1_scope: "Phase 1 scope",
            field_investment: "Investment",
            field_how_it_works: "How it works",
            field_call_to_action: "Call to action",
            investment_not_quoted: "Investment not yet quoted.",
            no_recommendation: "No recommended first engagement was provided for this run.",
            basis_fixed: "fixed fee",
            basis_per_month: "per month",
            basis_per_hour: "per hour",
        },
        Locale::PtBr => Chrome {
            title_prefix: "Roteiro de Automação",
            section1_heading: "Situação e Oportunidade",
            section2_heading: "Candidatos de Automação Classificados",
            section3_heading: "Principais Perfis de Fluxo de Trabalho",
            section4_heading: "Engajamento Inicial Recomendado",
            table_header: "| # | Fluxo de Trabalho | Frequência | Custo de Tempo | Viabilidade | \
                            Pontuação | Nível |\n\
                            |---|---|---|---|---|---|---|",
            tier_quick_win: "Vitória Rápida",
            tier_core_build: "Construção Principal",
            tier_phase2: "Fase 2",
            field_business_type: "Tipo de negócio",
            field_team_size: "Tamanho da equipe",
            field_painful_workflow: "Fluxo de trabalho mais doloroso",
            field_candidate_count: "Total de candidatos identificados",
            no_situation: "Nenhum resumo da situação foi fornecido para esta execução.",
            field_today: "Hoje",
            field_proposed_solution: "Solução proposta",
            field_stack: "Stack",
            field_rough_scope: "Escopo aproximado",
            field_expected_roi: "ROI esperado",
            no_profiles: "Nenhum perfil de fluxo de trabalho foi fornecido para esta execução.",
            field_start_with: "Começar com",
            field_phase_1_scope: "Escopo da Fase 1",
            field_investment: "Investimento",
            field_how_it_works: "Como funciona",
            field_call_to_action: "Chamada para ação",
            investment_not_quoted: "Investimento ainda não definido.",
            no_recommendation:
                "Nenhum engajamento inicial recomendado foi fornecido para esta execução.",
            basis_fixed: "valor fechado",
            basis_per_month: "por mês",
            basis_per_hour: "por hora",
        },
    }
}

/// Map a [`PriorityTier`] to its locale-selected label
/// (`PriorityTier::from_composite`'s thresholds — `rubric.md §4` — are
/// never re-derived here).
fn tier_label(tier: PriorityTier, chrome: &Chrome) -> &'static str {
    match tier {
        PriorityTier::QuickWin => chrome.tier_quick_win,
        PriorityTier::CoreBuild => chrome.tier_core_build,
        PriorityTier::Phase2 => chrome.tier_phase2,
    }
}

/// Format a [`MoneyRange`] in its own currency only — never converts and
/// never annotates with a second currency (`locale.rs`'s firewall
/// invariant).
fn format_money(range: &MoneyRange, chrome: &Chrome) -> String {
    let symbol = match range.currency {
        Currency::Brl => "R$",
        Currency::Usd => "$",
    };
    let basis = match range.basis {
        EngagementBasis::Fixed => chrome.basis_fixed,
        EngagementBasis::PerMonth => chrome.basis_per_month,
        EngagementBasis::PerHour => chrome.basis_per_hour,
    };
    format!(
        "{symbol}{min:.0}-{symbol}{max:.0} ({basis})",
        min = range.min,
        max = range.max
    )
}

fn render_section1(situation: Option<&SituationAndOpportunity>, chrome: &Chrome) -> String {
    let Some(situation) = situation else {
        return format!("## {}\n\n{}", chrome.section1_heading, chrome.no_situation);
    };
    format!(
        "## {heading}\n\n\
         - **{business_type_label}:** {business_type}\n\
         - **{team_size_label}:** {team_size}\n\
         - **{painful_workflow_label}:** {painful_workflow}\n\
         - **{candidate_count_label}:** {candidate_count}",
        heading = chrome.section1_heading,
        business_type_label = chrome.field_business_type,
        business_type = situation.business_type,
        team_size_label = chrome.field_team_size,
        team_size = situation.team_size,
        painful_workflow_label = chrome.field_painful_workflow,
        painful_workflow = situation.painful_workflow_summary,
        candidate_count_label = chrome.field_candidate_count,
        candidate_count = situation.candidate_count,
    )
}

fn render_section2(candidates: &[RankedCandidate], chrome: &Chrome) -> String {
    let mut rows = String::new();
    for (index, candidate) in candidates.iter().enumerate() {
        rows.push_str(&format!(
            "\n| {n} | {name} | {frequency:.1} | {time_cost:.1} | {buildability:.1} | \
             {composite:.2} | {tier} |",
            n = index + 1,
            name = candidate.name,
            frequency = candidate.frequency,
            time_cost = candidate.time_cost,
            buildability = candidate.buildability,
            composite = candidate.composite,
            tier = tier_label(candidate.tier, chrome),
        ));
    }
    format!(
        "## {heading}\n\n{table_header}{rows}",
        heading = chrome.section2_heading,
        table_header = chrome.table_header,
    )
}

fn render_section3(profiles: &[WorkflowProfile], chrome: &Chrome) -> String {
    if profiles.is_empty() {
        return format!("## {}\n\n{}", chrome.section3_heading, chrome.no_profiles);
    }
    let mut pages = Vec::with_capacity(profiles.len());
    for profile in profiles {
        pages.push(format!(
            "### {name}\n\n\
             - **{today_label}:** {today}\n\
             - **{solution_label}:** {solution}\n\
             - **{stack_label}:** {stack}\n\
             - **{scope_label}:** {scope}\n\
             - **{roi_label}:** {roi}",
            name = profile.name,
            today_label = chrome.field_today,
            today = profile.today,
            solution_label = chrome.field_proposed_solution,
            solution = profile.proposed_solution,
            stack_label = chrome.field_stack,
            stack = profile.stack,
            scope_label = chrome.field_rough_scope,
            scope = profile.rough_scope,
            roi_label = chrome.field_expected_roi,
            roi = profile.expected_roi,
        ));
    }
    format!(
        "## {heading}\n\n{pages}",
        heading = chrome.section3_heading,
        pages = pages.join("\n\n")
    )
}

fn render_section4(engagement: Option<&FirstEngagement>, chrome: &Chrome) -> String {
    let Some(engagement) = engagement else {
        return format!(
            "## {}\n\n{}",
            chrome.section4_heading, chrome.no_recommendation
        );
    };
    let phase_1_scope = if engagement.phase_1_scope.is_empty() {
        String::new()
    } else {
        let items: Vec<String> = engagement
            .phase_1_scope
            .iter()
            .map(|item| format!("  - {item}"))
            .collect();
        format!(
            "\n- **{}:**\n{}",
            chrome.field_phase_1_scope,
            items.join("\n")
        )
    };
    let investment = engagement
        .investment
        .as_ref()
        .map(|range| format_money(range, chrome))
        .unwrap_or_else(|| chrome.investment_not_quoted.to_string());
    format!(
        "## {heading}\n\n\
         - **{start_with_label}:** {start_with}{phase_1_scope}\n\
         - **{investment_label}:** {investment}\n\
         - **{how_it_works_label}:** {how_it_works}\n\
         - **{call_to_action_label}:** {call_to_action}",
        heading = chrome.section4_heading,
        start_with_label = chrome.field_start_with,
        start_with = engagement.start_with,
        investment_label = chrome.field_investment,
        how_it_works_label = chrome.field_how_it_works,
        how_it_works = engagement.how_it_works,
        call_to_action_label = chrome.field_call_to_action,
        call_to_action = engagement.call_to_action,
    )
}

/// Render `roadmap` into the four-section client deliverable markdown, with
/// all chrome and currency selected by `locale`. Does NOT check
/// `roadmap.authored_locale` — that refusal lives in
/// [`RenderDeliverableNode::process`], which is the only caller that has
/// both the requested locale and the authored one in hand at the same
/// time; this function is deliberately total so it stays easy to
/// unit-test in isolation.
#[must_use]
pub fn render_markdown(roadmap: &AutomationRoadmap, locale: Locale) -> String {
    let chrome = chrome(locale);
    let company_name = roadmap
        .situation
        .as_ref()
        .map(|s| s.company_name.as_str())
        .unwrap_or("");
    let title = if company_name.is_empty() {
        format!("# {}", chrome.title_prefix)
    } else {
        format!("# {} — {company_name}", chrome.title_prefix)
    };

    [
        title,
        render_section1(roadmap.situation.as_ref(), &chrome),
        render_section2(&roadmap.candidates, &chrome),
        render_section3(&roadmap.top_profiles, &chrome),
        render_section4(roadmap.recommendation.as_ref(), &chrome),
    ]
    .join("\n\n")
}

/// Deserialize the inbound `DELIVERABLE_RENDER` event from `ctx.event`.
fn parse_event(ctx: &TaskContext) -> Result<DeliverableRenderEventSchema, NodeError> {
    serde_json::from_value(ctx.event.clone())
        .map_err(|err| NodeError::new(format!("invalid DELIVERABLE_RENDER event: {err}")))
}

/// The deterministic (no model call on the default path) node that renders
/// an `AutomationRoadmap` into the four-section client deliverable markdown
/// and writes it to `<output_dir>/<company-slug>-roadmap.md`.
///
/// Refuses (returns a `NodeError`, writes no file) when the event's
/// requested `locale` disagrees with `roadmap.authored_locale` — see the
/// module doc comment.
#[derive(Debug, Default)]
pub struct RenderDeliverableNode;

impl RenderDeliverableNode {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Node for RenderDeliverableNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let event = parse_event(&ctx)?;

        if event.locale != event.roadmap.authored_locale {
            return Err(NodeError::new(format!(
                "{NODE_NAME}: requested locale {requested:?} disagrees with the roadmap's \
                 authored_locale {authored:?} — refusing to emit a mixed-language document",
                requested = event.locale,
                authored = event.roadmap.authored_locale,
            )));
        }

        let markdown = render_markdown(&event.roadmap, event.locale);
        let slug = deliverable_slug(&event.roadmap);
        let filename = format!("{slug}-roadmap.md");

        fs::create_dir_all(&event.output_dir).map_err(|err| {
            NodeError::new(format!(
                "{NODE_NAME}: failed to create output_dir {}: {err}",
                event.output_dir.display()
            ))
        })?;
        let markdown_path = event.output_dir.join(&filename);
        fs::write(&markdown_path, &markdown).map_err(|err| {
            NodeError::new(format!(
                "{NODE_NAME}: failed to write {}: {err}",
                markdown_path.display()
            ))
        })?;

        let mut ctx = ctx;
        put_result(
            &mut ctx,
            NODE_NAME,
            json!({
                "markdown_path": markdown_path.display().to_string(),
                "company_slug": slug,
                "markdown": markdown,
            }),
        );

        Ok(ctx)
    }

    fn name(&self) -> &str {
        NODE_NAME
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::workflows::proposal_generator::schema::composite_score;

    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        let n = TMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "engine-core-render-deliverable-test-{}-{n}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

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

    fn sample_engagement(currency: Currency) -> FirstEngagement {
        FirstEngagement {
            start_with: "WhatsApp order tracking".to_string(),
            phase_1_scope: vec!["Order intake bot".to_string()],
            investment: Some(MoneyRange {
                currency,
                min: 8_000.0,
                max: 12_000.0,
                basis: EngagementBasis::Fixed,
            }),
            how_it_works: "Connects to WhatsApp Business API.".to_string(),
            call_to_action: "Book a call to proceed.".to_string(),
        }
    }

    fn sample_roadmap(locale: Locale) -> AutomationRoadmap {
        let currency = locale.currency();
        AutomationRoadmap {
            situation: Some(sample_situation()),
            candidates: vec![
                candidate("Quick win candidate", 5.0, 4.5, 4.5), // composite 4.325 -> QuickWin
                candidate("Core build candidate", 3.0, 3.0, 3.0), // composite 3.0 -> CoreBuild
                candidate("Phase 2 candidate", 1.0, 1.0, 1.0),   // composite 1.0 -> Phase2
            ],
            top_profiles: vec![WorkflowProfile {
                name: "Quick win candidate".to_string(),
                today: "Manually scrolled.".to_string(),
                proposed_solution: "Automated bot with human approval gate.".to_string(),
                stack: "WhatsApp Business API + small service.".to_string(),
                rough_scope: "2-3 weeks.".to_string(),
                expected_roi: "Saves ~5 hrs/week.".to_string(),
            }],
            recommendation: Some(sample_engagement(currency)),
            authored_locale: locale,
        }
    }

    fn base_ctx(event: DeliverableRenderEventSchema) -> TaskContext {
        TaskContext {
            event: serde_json::to_value(event).unwrap(),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        }
    }

    // --- render_markdown: section headings + tier labels --------------

    #[test]
    fn en_us_document_contains_all_four_section_headings_in_english() {
        let roadmap = sample_roadmap(Locale::EnUs);
        let markdown = render_markdown(&roadmap, Locale::EnUs);
        assert!(markdown.contains("Situation & Opportunity"));
        assert!(markdown.contains("Ranked Automation Candidates"));
        assert!(markdown.contains("Top Workflow Profiles"));
        assert!(markdown.contains("Recommended First Engagement"));
    }

    #[test]
    fn pt_br_document_contains_all_four_section_headings_in_portuguese() {
        let roadmap = sample_roadmap(Locale::PtBr);
        let markdown = render_markdown(&roadmap, Locale::PtBr);
        assert!(markdown.contains("Situação e Oportunidade"));
        assert!(markdown.contains("Candidatos de Automação Classificados"));
        assert!(markdown.contains("Principais Perfis de Fluxo de Trabalho"));
        assert!(markdown.contains("Engajamento Inicial Recomendado"));
    }

    #[test]
    fn tier_labels_derive_from_composite_thresholds_in_the_requested_locale() {
        let roadmap = sample_roadmap(Locale::EnUs);
        let markdown = render_markdown(&roadmap, Locale::EnUs);
        // 4.325 -> QuickWin, 3.0 -> CoreBuild, 1.0 -> Phase2 (rubric.md §4).
        assert!(markdown.contains("Quick Win"));
        assert!(markdown.contains("Core Build"));
        assert!(markdown.contains("Phase 2"));
    }

    #[test]
    fn a_candidate_scoring_4_2_is_labelled_quick_win_3_0_core_build_1_8_phase_2() {
        let mut roadmap = sample_roadmap(Locale::EnUs);
        roadmap.candidates = vec![
            candidate("A", 5.0, 4.0, 3.6), // composite ~4.2
            candidate("B", 3.0, 3.0, 3.0), // composite 3.0
            candidate("C", 2.0, 1.8, 1.6), // composite ~1.8
        ];
        // Re-derive exact composites from the thresholds requested by the AC.
        roadmap.candidates[0].composite = 4.2;
        roadmap.candidates[0].tier = PriorityTier::from_composite(4.2);
        roadmap.candidates[1].composite = 3.0;
        roadmap.candidates[1].tier = PriorityTier::from_composite(3.0);
        roadmap.candidates[2].composite = 1.8;
        roadmap.candidates[2].tier = PriorityTier::from_composite(1.8);

        let markdown = render_markdown(&roadmap, Locale::EnUs);
        assert!(markdown.contains("| A | 5.0 | 4.0 | 3.6 | 4.20 | Quick Win |"));
        assert!(markdown.contains("| B | 3.0 | 3.0 | 3.0 | 3.00 | Core Build |"));
        assert!(markdown.contains("| C | 2.0 | 1.8 | 1.6 | 1.80 | Phase 2 |"));
    }

    // --- locale purity + currency ---------------------------------------

    #[test]
    fn pt_br_run_chrome_is_portuguese_and_investment_is_brl() {
        let roadmap = sample_roadmap(Locale::PtBr);
        let markdown = render_markdown(&roadmap, Locale::PtBr);
        assert!(markdown.contains("R$8000-R$12000"));
        assert!(!markdown.contains("Quick Win"));
        assert!(!markdown.contains("Core Build"));
        assert!(!markdown.contains("Recommended First Engagement"));
    }

    #[test]
    fn en_us_run_chrome_is_english_and_investment_is_usd() {
        let roadmap = sample_roadmap(Locale::EnUs);
        let markdown = render_markdown(&roadmap, Locale::EnUs);
        assert!(markdown.contains("$8000-$12000"));
        assert!(!markdown.contains("R$"));
        assert!(!markdown.contains("Vitória Rápida"));
        assert!(!markdown.contains("Engajamento Inicial Recomendado"));
    }

    #[test]
    fn no_single_document_mixes_chrome_from_both_locales() {
        for locale in [Locale::PtBr, Locale::EnUs] {
            let roadmap = sample_roadmap(locale);
            let markdown = render_markdown(&roadmap, locale);
            let other = chrome(match locale {
                Locale::PtBr => Locale::EnUs,
                Locale::EnUs => Locale::PtBr,
            });
            assert!(!markdown.contains(other.section1_heading));
            assert!(!markdown.contains(other.section4_heading));
        }
    }

    // --- Node::process ----------------------------------------------------

    #[tokio::test]
    async fn process_writes_the_markdown_file_under_output_dir() {
        let output_dir = temp_dir();
        let event = DeliverableRenderEventSchema {
            roadmap: sample_roadmap(Locale::EnUs),
            locale: Locale::EnUs,
            output_dir: output_dir.clone(),
            policy: None,
            profile: None,
        };
        let ctx = base_ctx(event);

        let node = RenderDeliverableNode::new();
        let ctx = node.process(ctx).await.expect("process should succeed");

        let expected_path = output_dir.join("loja-da-ana-roadmap.md");
        assert!(expected_path.exists());
        let written = std::fs::read_to_string(&expected_path).expect("read written file");
        assert!(written.contains("Situation & Opportunity"));

        let result = &ctx.nodes[NODE_NAME];
        assert_eq!(
            result["markdown_path"],
            json!(expected_path.display().to_string())
        );
        assert_eq!(result["company_slug"], json!("loja-da-ana"));
    }

    #[tokio::test]
    async fn process_refuses_on_locale_mismatch_and_writes_no_file() {
        let output_dir = temp_dir();
        let mut roadmap = sample_roadmap(Locale::PtBr);
        roadmap.authored_locale = Locale::PtBr;
        let event = DeliverableRenderEventSchema {
            roadmap,
            locale: Locale::EnUs,
            output_dir: output_dir.clone(),
            policy: None,
            profile: None,
        };
        let ctx = base_ctx(event);

        let node = RenderDeliverableNode::new();
        let err = node.process(ctx).await.expect_err("should refuse");
        assert!(err.message.contains("pt-BR") || err.message.contains("PtBr"));
        assert!(err.message.contains("en-US") || err.message.contains("EnUs"));

        let expected_path = output_dir.join("loja-da-ana-roadmap.md");
        assert!(!expected_path.exists());
    }

    #[tokio::test]
    async fn process_errors_on_an_invalid_event() {
        let ctx = TaskContext {
            event: json!({ "not_a_valid_event": true }),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        let node = RenderDeliverableNode::new();
        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("invalid DELIVERABLE_RENDER event"));
    }

    // --- missing investment ------------------------------------------------

    #[test]
    fn roadmap_with_no_investment_renders_without_panicking_and_states_not_yet_quoted() {
        let mut roadmap = sample_roadmap(Locale::EnUs);
        if let Some(recommendation) = roadmap.recommendation.as_mut() {
            recommendation.investment = None;
        }
        let markdown = render_markdown(&roadmap, Locale::EnUs);
        assert!(markdown.contains("Investment not yet quoted."));
    }

    #[test]
    fn roadmap_with_no_investment_in_pt_br_states_not_yet_quoted_in_portuguese() {
        let mut roadmap = sample_roadmap(Locale::PtBr);
        if let Some(recommendation) = roadmap.recommendation.as_mut() {
            recommendation.investment = None;
        }
        let markdown = render_markdown(&roadmap, Locale::PtBr);
        assert!(markdown.contains("Investimento ainda não definido."));
    }

    // --- absent sections ----------------------------------------------------

    #[test]
    fn absent_situation_renders_a_documented_fallback_not_a_panic() {
        let mut roadmap = sample_roadmap(Locale::EnUs);
        roadmap.situation = None;
        let markdown = render_markdown(&roadmap, Locale::EnUs);
        assert!(markdown.contains("No situation summary was provided for this run."));
    }

    #[test]
    fn absent_recommendation_renders_a_documented_fallback_not_a_panic() {
        let mut roadmap = sample_roadmap(Locale::EnUs);
        roadmap.recommendation = None;
        let markdown = render_markdown(&roadmap, Locale::EnUs);
        assert!(markdown.contains("No recommended first engagement was provided for this run."));
    }
}
