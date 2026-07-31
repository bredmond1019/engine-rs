//! Hermetic end-to-end suite for `EN.4.F`'s locale + firewalled rate card:
//! drives the real `PROPOSAL_GENERATOR` node chain (mirroring
//! `proposal_generator_e2e.rs`'s harness — stubbed `ModelTransport` +
//! `HttpPost`, no real `claude` subprocess, no live network call) once per
//! locale and asserts:
//!
//! 1. a `pt-BR` run's persisted roadmap carries `authored_locale: "pt-BR"`,
//!    `investment.currency: "BRL"`, and figures matching the harness BRL
//!    sheet;
//! 2. the same for `en-US` / `"USD"` / the USD sheet;
//! 3. an event omitting `locale` behaves identically to an explicit
//!    `"pt-BR"` event;
//! 4. no single run's serialized `TaskContext` contains figures from the
//!    *other* sheet — asserted on the JSON string, not just the typed
//!    value;
//! 5. neither run quotes below its own sheet's hourly floor;
//! 6. `ProposalWriterNode`'s resolved `system_prompt` is byte-identical
//!    across the two locale runs (the cache-breakpoint invariant, CLAUDE.md
//!    rule 6);
//! 7. (the firewall guard) no source file anywhere in `crates/` contains a
//!    BRL<->USD conversion helper — this is the code-level enforcement of
//!    `business/docs/rates.md`'s firewall rule: "never quoted in the same
//!    conversation, never cross-converted".

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use claude_code_rs::parse::{ModelUsage as SdkModelUsage, Usage as SdkUsage};
use claude_code_rs::{Config, Outcome};
use engine_contract::{NodeRunStatus, TaskContext};
use engine_core::locale::{Currency, Locale, RateCard};
use engine_core::node::NodeRegistry;
use engine_core::nodes::http_post::StubHttpPost;
use engine_core::workflows::diagnostic_intake::schema::{DiagnosticIntake, WorkflowCandidate};
use engine_core::workflows::proposal_generator::company_research::ProposalCompanyResearchNode;
use engine_core::workflows::proposal_generator::opportunity_identifier::OpportunityIdentifierNode;
use engine_core::workflows::proposal_generator::persist_to_brain::PersistToBrainNode;
use engine_core::workflows::proposal_generator::profiles;
use engine_core::workflows::proposal_generator::review::ProposalReviewNode;
use engine_core::workflows::proposal_generator::review_router::ProposalReviewRouterNode;
use engine_core::workflows::proposal_generator::revise::ProposalReviseNode;
use engine_core::workflows::proposal_generator::schema::AutomationRoadmap;
use engine_core::workflows::proposal_generator::writer::ProposalWriterNode;
use engine_core::workflows::ModelTransport;
use futures::FutureExt;
use serde_json::{json, Value};

const TEST_BRAIN_URL: &str = "https://brain.example/ingest/locale-rate-card";

fn temp_worktree(tag: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "engine-core-locale-rate-card-e2e-{tag}-{}-{n}",
        std::process::id()
    ));
    // Guarantee-empty: see engine-core src's `sdlc_flow/setup.rs` `temp_dir_named`
    // doc comment for why PID-recycling makes this removal necessary, not
    // optional. Remove the ROOT dir before recreating the `planning` subdir.
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("planning")).unwrap();
    dir
}

fn set_worktree(ctx: &mut TaskContext, worktree: &Path) {
    ctx.nodes.insert(
        "SetupWorktreeNode".to_string(),
        json!({ "worktree_path": worktree.to_string_lossy() }),
    );
}

fn stamp_resolved_policy(ctx: &mut TaskContext, worktree: &Path) {
    let resolved = profiles::resolve_policy_for_run(ctx, worktree)
        .expect("PROPOSAL_GENERATOR policy should resolve for this event");
    engine_core::policy::stamp_resolved_policy(ctx, &resolved).expect("policy should stamp");
}

fn stub_outcome(structured: Value, input_tokens: u64, output_tokens: u64) -> Outcome {
    Outcome {
        cost_usd: 0.01,
        usage: SdkUsage {
            input_tokens,
            output_tokens,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
        model_usage: [(
            "claude-sonnet-4-5".to_string(),
            SdkModelUsage {
                input_tokens,
                output_tokens,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                cost_usd: 0.01,
            },
        )]
        .into_iter()
        .collect(),
        text: serde_json::to_string(&structured).unwrap(),
        is_error: false,
        api_error_status: None,
        structured_output: Some(structured),
    }
}

fn stub_transport_returning(structured: Value) -> ModelTransport {
    Arc::new(move |_config: Config, _prompt: String| {
        let structured = structured.clone();
        async move { Ok(stub_outcome(structured, 40, 20)) }.boxed()
    })
}

/// Build a transport that records every `Config` it's handed (into
/// `captured`) and always replies with `structured` — used to capture
/// `ProposalWriterNode`'s resolved `system_prompt` per run for the
/// byte-identical-across-locales assertion.
fn capturing_transport(structured: Value, captured: Arc<Mutex<Vec<Config>>>) -> ModelTransport {
    Arc::new(move |config: Config, _prompt: String| {
        captured.lock().unwrap().push(config.clone());
        let structured = structured.clone();
        async move { Ok(stub_outcome(structured, 40, 20)) }.boxed()
    })
}

fn stub_company_brief_json() -> Value {
    json!({
        "company_name": "Loja da Ana",
        "summary": "Retail SMB tracking orders manually over WhatsApp.",
        "recent_developments": ["Opened a second storefront"],
        "pain_points": ["Manual order tracking"],
        "outreach_hooks": ["Recent second-storefront opening"],
        "sources": ["https://loja-da-ana.example/news"],
    })
}

fn stub_scored_candidates_json() -> Value {
    json!({
        "candidates": [
            {
                "name": "Supplier follow-up messages",
                "frequency": 3.0,
                "time_cost": 2.0,
                "buildability": 4.0,
                "rationale": "Frequent but lower time cost."
            },
            {
                "name": "WhatsApp order tracking",
                "frequency": 5.0,
                "time_cost": 4.0,
                "buildability": 4.0,
                "rationale": "Happens daily and is fully manual today."
            }
        ]
    })
}

/// The writer's *model-authored* roadmap draft — deliberately carries no
/// `investment` at all (the model must never be asked to author a price)
/// and an `authored_locale` the event's locale must override.
fn stub_roadmap_json() -> Value {
    json!({
        "situation": {
            "company_name": "Loja da Ana",
            "business_type": "retail SMB",
            "team_size": 4,
            "painful_workflow_summary": "Orders tracked by scrolling WhatsApp threads.",
            "candidate_count": 2,
        },
        "candidates": [
            {
                "name": "WhatsApp order tracking",
                "frequency": 5.0,
                "time_cost": 4.0,
                "buildability": 4.0,
                "composite": 4.35,
                "tier": "quick_win",
                "rationale": "Happens daily and is fully manual today.",
            },
            {
                "name": "Supplier follow-up messages",
                "frequency": 3.0,
                "time_cost": 2.0,
                "buildability": 4.0,
                "composite": 2.85,
                "tier": "core_build",
                "rationale": "Frequent but lower time cost.",
            }
        ],
        "top_profiles": [
            {
                "name": "WhatsApp order tracking",
                "today": "Manually scrolled.",
                "proposed_solution": "Automated bot with human approval gate.",
                "stack": "WhatsApp Business API + small service.",
                "rough_scope": "2-3 weeks.",
                "expected_roi": "Saves ~5 hrs/week.",
            }
        ],
        "recommendation": {
            "start_with": "WhatsApp order tracking",
            "phase_1_scope": ["Order intake bot"],
            "how_it_works": "Connects to WhatsApp Business API.",
            "call_to_action": "Book a call to proceed.",
        },
    })
}

fn stub_review_verdict_json(verdict: &str, notes: &str) -> Value {
    json!({ "verdict": verdict, "notes": notes })
}

/// `locale` is spliced in only when `Some` — an omitted key exercises the
/// `#[serde(default)]` path on `ProposalGeneratorEventSchema::locale`.
fn base_event(locale: Option<&str>, diagnostic_intake: Option<DiagnosticIntake>) -> Value {
    let mut event = json!({
        "company_name": "Loja da Ana",
        "company_url": "https://loja-da-ana.example",
        "diagnostic_intake": diagnostic_intake,
    });
    if let Some(locale) = locale {
        event["locale"] = json!(locale);
    }
    event
}

fn diagnostic_intake_fixture() -> DiagnosticIntake {
    DiagnosticIntake {
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
    }
}

struct Stubs {
    company_research: ModelTransport,
    opportunity: ModelTransport,
    writer: ModelTransport,
    review: ModelTransport,
    revise: ModelTransport,
    http_post: StubHttpPost,
}

impl Stubs {
    fn default_passing() -> Self {
        Self {
            company_research: stub_transport_returning(stub_company_brief_json()),
            opportunity: stub_transport_returning(stub_scored_candidates_json()),
            writer: stub_transport_returning(stub_roadmap_json()),
            review: stub_transport_returning(stub_review_verdict_json("pass", "Looks good.")),
            revise: stub_transport_returning(stub_roadmap_json()),
            http_post: StubHttpPost::succeeding(json!({"ok": true})),
        }
    }
}

fn build_registry(stubs: &Stubs) -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(
        ProposalCompanyResearchNode::new().with_transport(stubs.company_research.clone()),
    ));
    registry.register(Box::new(
        OpportunityIdentifierNode::new().with_transport(stubs.opportunity.clone()),
    ));
    registry.register(Box::new(
        ProposalWriterNode::new().with_transport(stubs.writer.clone()),
    ));
    registry.register(Box::new(
        ProposalReviewNode::new().with_transport(stubs.review.clone()),
    ));
    registry.register(Box::new(ProposalReviewRouterNode));
    registry.register(Box::new(
        ProposalReviseNode::new().with_transport(stubs.revise.clone()),
    ));
    registry.register(Box::new(
        PersistToBrainNode::new()
            .with_http_post(Arc::new(stubs.http_post.clone()))
            .with_url(TEST_BRAIN_URL),
    ));
    registry
}

fn stamp_success(ctx: &mut TaskContext, identity: &str) {
    if let Some(run) = ctx.node_runs.get_mut(identity) {
        run.status = NodeRunStatus::Success;
        run.completed_at = Some(Utc::now());
    }
}

/// Drive the declared `PROPOSAL_GENERATOR` walk end to end against the
/// exact node instances `build_registry` would hand to a real `Workflow` —
/// mirrors `proposal_generator_e2e.rs::drive`.
async fn drive(event: Value, worktree: &Path, registry: &NodeRegistry) -> (String, TaskContext) {
    let mut ctx = TaskContext {
        event,
        nodes: HashMap::new(),
        metadata: json!({}),
        node_runs: HashMap::new(),
    };
    set_worktree(&mut ctx, worktree);
    stamp_resolved_policy(&mut ctx, worktree);

    for identity in [
        "ProposalCompanyResearchNode",
        "OpportunityIdentifierNode",
        "ProposalWriterNode",
        "ProposalReviewNode",
    ] {
        let node = registry
            .get(identity)
            .unwrap_or_else(|| panic!("'{identity}' should be registered"));
        ctx = node
            .process(ctx)
            .await
            .unwrap_or_else(|err| panic!("'{identity}' should process successfully: {err}"));
        stamp_success(&mut ctx, identity);
    }

    let router = registry
        .get("ProposalReviewRouterNode")
        .expect("router should be registered");
    let branch = router
        .as_router()
        .expect("ProposalReviewRouterNode should be a Router")
        .route(&ctx)
        .expect("router should resolve a target identity for a valid verdict");

    if branch == "ProposalReviseNode" {
        let node = registry
            .get("ProposalReviseNode")
            .expect("ProposalReviseNode should be registered");
        ctx = node
            .process(ctx)
            .await
            .expect("ProposalReviseNode should process successfully");
        stamp_success(&mut ctx, "ProposalReviseNode");
    }

    let persist = registry
        .get("PersistToBrainNode")
        .expect("PersistToBrainNode should be registered");
    ctx = persist
        .process(ctx)
        .await
        .expect("PersistToBrainNode should process successfully");
    stamp_success(&mut ctx, "PersistToBrainNode");

    (branch, ctx)
}

fn persisted_roadmap(stub: &StubHttpPost) -> Value {
    let (_url, body) = stub
        .last_call()
        .expect("PersistToBrainNode should have POSTed");
    body["roadmap"].clone()
}

/// Runs the full `PROPOSAL_GENERATOR` chain once for the given `locale` arg
/// (`None` exercises the omitted-locale default path) and returns the
/// persisted `AutomationRoadmap` plus the final `TaskContext` (for the
/// cross-sheet-leak / no-mixing assertions) and the writer's resolved
/// `Config` (for the byte-identical-system-prompt assertion).
async fn run_locale(locale: Option<&str>, tag: &str) -> (AutomationRoadmap, TaskContext, Config) {
    let worktree = temp_worktree(tag);
    let captured: Arc<Mutex<Vec<Config>>> = Arc::new(Mutex::new(Vec::new()));
    let mut stubs = Stubs::default_passing();
    stubs.writer = capturing_transport(stub_roadmap_json(), captured.clone());
    let registry = build_registry(&stubs);

    let event = base_event(locale, Some(diagnostic_intake_fixture()));
    let (branch, final_ctx) = drive(event, &worktree, &registry).await;
    assert_eq!(branch, "PersistToBrainNode", "review stub always passes");

    let roadmap_value = persisted_roadmap(&stubs.http_post);
    let roadmap: AutomationRoadmap =
        serde_json::from_value(roadmap_value).expect("valid AutomationRoadmap");

    let writer_config = captured
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("writer transport should have been called");

    std::fs::remove_dir_all(&worktree).ok();
    (roadmap, final_ctx, writer_config)
}

#[tokio::test]
async fn pt_br_run_prices_from_the_brl_sheet() {
    let (roadmap, _ctx, _config) = run_locale(Some("pt-BR"), "pt-br").await;

    assert_eq!(roadmap.authored_locale, Locale::PtBr);
    let investment = roadmap
        .recommendation
        .as_ref()
        .and_then(|r| r.investment.as_ref())
        .expect("recommendation.investment should be populated from the rate card");
    assert_eq!(investment.currency, Currency::Brl);

    let expected = RateCard::default().sheet(Locale::PtBr).project;
    assert_eq!(investment.min, expected.min);
    assert_eq!(investment.max, expected.max);
}

#[tokio::test]
async fn en_us_run_prices_from_the_usd_sheet() {
    let (roadmap, _ctx, _config) = run_locale(Some("en-US"), "en-us").await;

    assert_eq!(roadmap.authored_locale, Locale::EnUs);
    let investment = roadmap
        .recommendation
        .as_ref()
        .and_then(|r| r.investment.as_ref())
        .expect("recommendation.investment should be populated from the rate card");
    assert_eq!(investment.currency, Currency::Usd);

    let expected = RateCard::default().sheet(Locale::EnUs).project;
    assert_eq!(investment.min, expected.min);
    assert_eq!(investment.max, expected.max);
}

#[tokio::test]
async fn omitted_locale_behaves_exactly_like_pt_br() {
    let (omitted_roadmap, _omitted_ctx, _omitted_config) = run_locale(None, "omitted").await;
    let (explicit_roadmap, _explicit_ctx, _explicit_config) =
        run_locale(Some("pt-BR"), "explicit-pt-br").await;

    assert_eq!(omitted_roadmap.authored_locale, Locale::PtBr);
    assert_eq!(explicit_roadmap.authored_locale, Locale::PtBr);

    let omitted_investment = omitted_roadmap
        .recommendation
        .as_ref()
        .and_then(|r| r.investment.as_ref())
        .expect("omitted-locale run should still price");
    let explicit_investment = explicit_roadmap
        .recommendation
        .as_ref()
        .and_then(|r| r.investment.as_ref())
        .expect("explicit pt-BR run should price");
    assert_eq!(omitted_investment, explicit_investment);
}

#[tokio::test]
async fn no_run_mixes_the_two_sheets() {
    let (_pt_roadmap, pt_ctx, _pt_config) = run_locale(Some("pt-BR"), "no-mix-pt").await;
    let (_en_roadmap, en_ctx, _en_config) = run_locale(Some("en-US"), "no-mix-en").await;

    let pt_json = serde_json::to_string(&pt_ctx).expect("TaskContext should serialize");
    let en_json = serde_json::to_string(&en_ctx).expect("TaskContext should serialize");

    // A pt-BR run must never carry a USD figure/tag anywhere in its final
    // TaskContext, and vice versa. Assert on the JSON string, not the typed
    // value, so a stray formatted string couldn't leak a cross-quote past
    // this check.
    assert!(
        !pt_json.contains("\"USD\""),
        "pt-BR run's TaskContext must not contain a USD figure"
    );
    assert!(
        !en_json.contains("\"BRL\""),
        "en-US run's TaskContext must not contain a BRL figure"
    );
}

#[tokio::test]
async fn neither_run_quotes_below_its_own_floor() {
    let (pt_roadmap, _pt_ctx, _pt_config) = run_locale(Some("pt-BR"), "floor-pt").await;
    let (en_roadmap, _en_ctx, _en_config) = run_locale(Some("en-US"), "floor-en").await;

    let card = RateCard::default();

    let pt_investment = pt_roadmap
        .recommendation
        .as_ref()
        .and_then(|r| r.investment.as_ref())
        .expect("pt-BR run should price");
    assert!(pt_investment.min >= card.sheet(Locale::PtBr).hourly_floor);

    let en_investment = en_roadmap
        .recommendation
        .as_ref()
        .and_then(|r| r.investment.as_ref())
        .expect("en-US run should price");
    assert!(en_investment.min >= card.sheet(Locale::EnUs).hourly_floor);
}

#[tokio::test]
async fn stable_system_prompt_is_byte_identical_across_the_two_runs() {
    let (_pt_roadmap, _pt_ctx, pt_config) = run_locale(Some("pt-BR"), "prompt-pt").await;
    let (_en_roadmap, _en_ctx, en_config) = run_locale(Some("en-US"), "prompt-en").await;

    assert_eq!(
        pt_config.system_prompt, en_config.system_prompt,
        "ProposalWriterNode's STABLE_SYSTEM_PROMPT must be byte-identical across \
         locales (CLAUDE.md rule 6, cache breakpoints) — the locale directive \
         belongs only in the per-run prompt body"
    );
}

/// The firewall guard, as real code rather than a spec bullet: no source
/// file anywhere in `crates/` may contain a BRL<->USD conversion helper.
/// See `business/docs/rates.md`'s firewall rule ("never quoted in the same
/// conversation, never cross-converted") and `crate::locale`'s module doc.
#[test]
fn no_currency_conversion_exists_anywhere_in_the_crate() {
    let patterns = [
        "exchange_rate",
        "convert_currency",
        "brl_to_usd",
        "usd_to_brl",
        "to_usd",
        "to_brl",
    ];

    // This test file's own doc comments/identifiers legitimately mention
    // these patterns (they're the very thing being forbidden), so it is
    // excluded from the walk by filename.
    let this_file = "locale_rate_card.rs";

    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set"));
    let crates_root = manifest_dir
        .join("../..")
        .canonicalize()
        .expect("crates/ root should resolve");

    let mut offenses: Vec<String> = Vec::new();
    let mut stack = vec![crates_root.clone()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // Skip build artifacts.
                if path.file_name().and_then(|n| n.to_str()) == Some("target") {
                    continue;
                }
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            if path.file_name().and_then(|n| n.to_str()) == Some(this_file) {
                continue;
            }
            let Ok(contents) = std::fs::read_to_string(&path) else {
                continue;
            };
            for (lineno, line) in contents.lines().enumerate() {
                let lower = line.to_lowercase();
                for pattern in patterns {
                    if lower.contains(pattern) {
                        offenses.push(format!(
                            "{}:{}: matched forbidden pattern '{pattern}' — the two rate \
                             sheets are firewalled per business/docs/rates.md ('never \
                             quoted in the same conversation, never cross-converted'); \
                             no BRL<->USD conversion helper may exist anywhere in this \
                             crate.\n    {}",
                            path.display(),
                            lineno + 1,
                            line.trim()
                        ));
                    }
                }
            }
        }
    }

    assert!(
        offenses.is_empty(),
        "firewall guard failed:\n{}",
        offenses.join("\n")
    );
}
