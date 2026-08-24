//! Hermetic end-to-end integration test for the assembled `DELIVERABLE_RENDER`
//! workflow (`EN.4.D` task 6): drives the real declared two-node graph
//! (`RenderDeliverableNode -> RenderPdfNode`) through a real `Workflow::run`
//! pointer-walk (unlike `proposal_generator_e2e.rs` / `diagnostic_intake_e2e.rs`,
//! this workflow has no dedicated setup node AND needs no pre-seeded
//! worktree — both nodes read `event.output_dir` directly — so `Workflow::run`
//! can drive it end to end exactly as `content_pipeline_materialize_e2e.rs`
//! does), with a stubbed `CommandRunner` standing in for `typst` so the
//! gated suite never shells out (confirmed absent on this host,
//! `command -v typst` -> not found, 2026-08-24).
//!
//! Per `CLAUDE.md` standing rule 8 / `docs/testing.md`, this file is a
//! MODULE of the single `tests/it` binary (declared via `mod
//! deliverable_render_e2e;` in `tests/it/main.rs`), never its own
//! `tests/*.rs` binary — `master-plan.md`'s named path
//! (`tests/deliverable_render_e2e.rs`) predates that standing rule; see
//! `planning/EN.4.D/tasks.md`'s "Notes" section for the deliberate deviation.
//!
//! Covers the spec's acceptance criteria for task 6:
//! (a) an end-to-end `pt-BR` run writes both `.md` and `.pdf` basenames
//!     under a temp `output_dir`, with a stubbed `CommandRunner`;
//! (b) an end-to-end `en-US` run does the same, with English chrome;
//! (c) an `en-US` request against a `pt-BR`-authored roadmap fails at
//!     `RenderDeliverableNode` with the mismatch error and writes no file,
//!     and `RenderPdfNode` never runs;
//! (d) a `typst` failure (stubbed non-zero exit) surfaces as a failed
//!     `RenderPdfNode` run carrying the stub's stderr, even though the
//!     markdown file it was meant to render already exists on disk.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use engine_contract::{EventsRow, NodeRunStatus, TaskContext};
use engine_core::locale::{EngagementBasis, Locale, MoneyRange};
use engine_core::workflow::Workflow;
use engine_core::workflows::deliverable_render::graph;
use engine_core::workflows::deliverable_render::render_markdown::RenderDeliverableNode;
use engine_core::workflows::deliverable_render::render_pdf::RenderPdfNode;
use engine_core::workflows::proposal_generator::schema::{
    composite_score, AutomationRoadmap, FirstEngagement, PriorityTier, RankedCandidate,
    SituationAndOpportunity, WorkflowProfile,
};
use engine_core::workflows::{CommandOutput, CommandRunner};
use serde_json::{json, Value};
use uuid::Uuid;

fn temp_output_dir(tag: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "engine-core-deliverable-render-e2e-{tag}-{}-{n}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// One recorded `CommandRunner` invocation: `(program, args, cwd)`.
type RecordedCall = (String, Vec<String>, PathBuf);

/// A stub [`CommandRunner`] that records every call and always returns
/// `output`, never touching a real subprocess.
fn stub_runner(output: CommandOutput) -> (CommandRunner, Arc<Mutex<Vec<RecordedCall>>>) {
    let calls: Arc<Mutex<Vec<RecordedCall>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&calls);
    let runner: CommandRunner = Arc::new(move |program, args, cwd| {
        recorded.lock().unwrap().push((
            program.to_string(),
            args.iter().map(|s| s.to_string()).collect(),
            cwd.to_path_buf(),
        ));
        Ok(output.clone())
    });
    (runner, calls)
}

fn success_output() -> CommandOutput {
    CommandOutput {
        status: 0,
        stdout: String::new(),
        stderr: String::new(),
    }
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

fn sample_roadmap(company_name: &str, locale: Locale) -> AutomationRoadmap {
    let currency = locale.currency();
    AutomationRoadmap {
        situation: Some(SituationAndOpportunity {
            company_name: company_name.to_string(),
            business_type: "retail SMB".to_string(),
            team_size: 4,
            painful_workflow_summary: "Orders tracked by scrolling WhatsApp threads.".to_string(),
            candidate_count: 2,
        }),
        candidates: vec![
            candidate("WhatsApp order tracking", 5.0, 4.5, 4.5),
            candidate("Supplier follow-up messages", 3.0, 3.0, 3.0),
        ],
        top_profiles: vec![WorkflowProfile {
            name: "WhatsApp order tracking".to_string(),
            today: "Manually scrolled.".to_string(),
            proposed_solution: "Automated bot with human approval gate.".to_string(),
            stack: "WhatsApp Business API + small service.".to_string(),
            rough_scope: "2-3 weeks.".to_string(),
            expected_roi: "Saves ~5 hrs/week.".to_string(),
        }],
        recommendation: Some(FirstEngagement {
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
        }),
        authored_locale: locale,
    }
}

fn deliverable_event(roadmap: &AutomationRoadmap, locale: Locale, output_dir: &Path) -> Value {
    json!({
        "roadmap": roadmap,
        "locale": locale,
        "output_dir": output_dir.to_string_lossy(),
    })
}

/// Build the runnable `DELIVERABLE_RENDER` workflow with a stubbed
/// `CommandRunner` in place of a real `typst` invocation.
fn workflow_with_runner(runner: CommandRunner) -> Workflow {
    let mut registry = engine_core::node::NodeRegistry::new();
    registry.register(Box::new(RenderDeliverableNode::new()));
    registry.register(Box::new(RenderPdfNode::new().with_runner(runner)));
    Workflow::new_validated(registry, graph::schema())
        .expect("DELIVERABLE_RENDER declared graph should validate")
}

fn events_row_for(workflow_type: &str, event: Value, ctx: &TaskContext) -> EventsRow {
    let now = chrono::Utc::now();
    EventsRow {
        id: Uuid::new_v4(),
        workflow_type: workflow_type.to_string(),
        data: event,
        task_context: ctx.clone(),
        created_at: now,
        updated_at: now,
    }
}

// ---------------------------------------------------------------------------
// (a)/(b): a pt-BR run and an en-US run each produce both artifacts.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pt_br_run_produces_both_artifacts_under_output_dir() {
    let output_dir = temp_output_dir("pt-br");
    let (runner, calls) = stub_runner(success_output());
    let workflow = workflow_with_runner(runner);

    let roadmap = sample_roadmap("Loja da Ana", Locale::PtBr);
    let event = deliverable_event(&roadmap, Locale::PtBr, &output_dir);

    let ctx = workflow
        .run(event.clone(), Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("DELIVERABLE_RENDER run should complete");

    assert_eq!(
        ctx.node_runs["RenderDeliverableNode"].status,
        NodeRunStatus::Success
    );
    assert_eq!(
        ctx.node_runs["RenderPdfNode"].status,
        NodeRunStatus::Success
    );

    let markdown_path = output_dir.join("loja-da-ana-roadmap.md");
    let pdf_path = output_dir.join("loja-da-ana-roadmap.pdf");
    assert!(markdown_path.exists(), "markdown file should exist on disk");

    let written = std::fs::read_to_string(&markdown_path).unwrap();
    assert!(written.contains("Situação e Oportunidade"));
    assert!(written.contains("R$8000-R$12000"));

    // typst was invoked exactly once, with the expected argv, against the
    // markdown file that RenderDeliverableNode actually wrote.
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let (program, args, cwd) = &calls[0];
    assert_eq!(program, "typst");
    assert_eq!(
        args,
        &vec![
            "compile".to_string(),
            markdown_path.display().to_string(),
            pdf_path.display().to_string(),
        ]
    );
    assert_eq!(cwd, &output_dir);

    let pdf_result = &ctx.nodes["RenderPdfNode"];
    assert_eq!(
        pdf_result["pdf_path"],
        json!(pdf_path.display().to_string())
    );

    assert_events_row_round_trips(event, &ctx);

    std::fs::remove_dir_all(&output_dir).ok();
}

#[tokio::test]
async fn en_us_run_produces_both_artifacts_under_output_dir() {
    let output_dir = temp_output_dir("en-us");
    let (runner, calls) = stub_runner(success_output());
    let workflow = workflow_with_runner(runner);

    let roadmap = sample_roadmap("Acme", Locale::EnUs);
    let event = deliverable_event(&roadmap, Locale::EnUs, &output_dir);

    let ctx = workflow
        .run(event.clone(), Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("DELIVERABLE_RENDER run should complete");

    assert_eq!(
        ctx.node_runs["RenderDeliverableNode"].status,
        NodeRunStatus::Success
    );
    assert_eq!(
        ctx.node_runs["RenderPdfNode"].status,
        NodeRunStatus::Success
    );

    let markdown_path = output_dir.join("acme-roadmap.md");
    let pdf_path = output_dir.join("acme-roadmap.pdf");
    assert!(markdown_path.exists());

    let written = std::fs::read_to_string(&markdown_path).unwrap();
    assert!(written.contains("Situation & Opportunity"));
    assert!(written.contains("$8000-$12000"));
    assert!(!written.contains("R$"));

    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let (program, args, cwd) = &calls[0];
    assert_eq!(program, "typst");
    assert_eq!(
        args,
        &vec![
            "compile".to_string(),
            markdown_path.display().to_string(),
            pdf_path.display().to_string(),
        ]
    );
    assert_eq!(cwd, &output_dir);

    assert_events_row_round_trips(event, &ctx);

    std::fs::remove_dir_all(&output_dir).ok();
}

// ---------------------------------------------------------------------------
// (c): locale-mismatch refusal path — RenderDeliverableNode fails, writes no
// file, and RenderPdfNode never runs (never invokes the stub runner).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn en_us_request_against_a_pt_br_authored_roadmap_fails_and_writes_no_file() {
    let output_dir = temp_output_dir("locale-mismatch");
    let (runner, calls) = stub_runner(success_output());
    let workflow = workflow_with_runner(runner);

    let roadmap = sample_roadmap("Loja da Ana", Locale::PtBr);
    let event = deliverable_event(&roadmap, Locale::EnUs, &output_dir);

    let ctx = workflow
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("DELIVERABLE_RENDER run should still return Ok(ctx) on a node failure");

    let render_run = &ctx.node_runs["RenderDeliverableNode"];
    assert_eq!(render_run.status, NodeRunStatus::Failed);
    let error = render_run
        .error
        .as_ref()
        .expect("failed node run should carry an error message");
    assert!(error.contains("pt-BR") || error.contains("PtBr"));
    assert!(error.contains("en-US") || error.contains("EnUs"));

    // The walk halts at the failed node: RenderPdfNode never dispatched, so
    // the stub runner was never invoked.
    assert_eq!(
        ctx.node_runs["RenderPdfNode"].status,
        NodeRunStatus::Pending
    );
    assert!(calls.lock().unwrap().is_empty());

    let markdown_path = output_dir.join("loja-da-ana-roadmap.md");
    assert!(
        !markdown_path.exists(),
        "no file should be written on refusal"
    );

    std::fs::remove_dir_all(&output_dir).ok();
}

// ---------------------------------------------------------------------------
// (d): typst-failure path — the markdown file is written by
// RenderDeliverableNode, but the stubbed typst invocation returns non-zero,
// surfacing as a failed RenderPdfNode run carrying the stub's stderr.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_typst_failure_surfaces_as_a_failed_render_pdf_node_run() {
    let output_dir = temp_output_dir("typst-failure");
    let failure = CommandOutput {
        status: 1,
        stdout: String::new(),
        stderr: "typst: error: file not found".to_string(),
    };
    let (runner, calls) = stub_runner(failure);
    let workflow = workflow_with_runner(runner);

    let roadmap = sample_roadmap("Acme", Locale::EnUs);
    let event = deliverable_event(&roadmap, Locale::EnUs, &output_dir);

    let ctx = workflow
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("DELIVERABLE_RENDER run should still return Ok(ctx) on a node failure");

    assert_eq!(
        ctx.node_runs["RenderDeliverableNode"].status,
        NodeRunStatus::Success
    );
    let markdown_path = output_dir.join("acme-roadmap.md");
    assert!(
        markdown_path.exists(),
        "RenderDeliverableNode should have already written the markdown file \
         before RenderPdfNode ran"
    );

    let pdf_run = &ctx.node_runs["RenderPdfNode"];
    assert_eq!(pdf_run.status, NodeRunStatus::Failed);
    let error = pdf_run
        .error
        .as_ref()
        .expect("failed node run should carry an error message");
    assert!(error.contains("typst: error: file not found"));
    assert!(error.contains('1'));

    assert_eq!(calls.lock().unwrap().len(), 1);

    std::fs::remove_dir_all(&output_dir).ok();
}

// ---------------------------------------------------------------------------
// Structural smoke test + EventsRow round trip.
// ---------------------------------------------------------------------------

fn assert_events_row_round_trips(event: Value, ctx: &TaskContext) {
    let row = events_row_for(graph::WORKFLOW_TYPE, event, ctx);
    let json_str = serde_json::to_string(&row).expect("EventsRow should serialize");
    let round_tripped: EventsRow =
        serde_json::from_str(&json_str).expect("EventsRow should deserialize");
    assert_eq!(round_tripped, row);
    assert_eq!(round_tripped.workflow_type, "DELIVERABLE_RENDER");
    assert_eq!(round_tripped.task_context, *ctx);
}

#[test]
fn assembled_workflow_builds_without_panicking() {
    let _workflow = graph::workflow();
}

#[test]
fn is_registered_true_after_register_builtin_workflows() {
    let mut dispatcher = engine_serve::dispatch::Dispatcher::new();
    engine_serve::workflows::register_builtin_workflows(&mut dispatcher);

    assert!(dispatcher.is_registered("DELIVERABLE_RENDER"));

    let listed = dispatcher.registered_types();
    assert!(
        listed.iter().any(|t| t == "DELIVERABLE_RENDER"),
        "expected 'DELIVERABLE_RENDER' in the dispatcher's registered_types() \
         (the GET /workflows-equivalent listing), got {listed:?}"
    );
}

// ---------------------------------------------------------------------------
// Task 7 (EN.4.D, D64) — fixture evidence for the un-gateable live-typst
// criterion.
//
// "A real PDF is produced by a live `typst`" has its evidence in another
// process; `typst` is confirmed absent on this host (`command -v typst` ->
// not found, 2026-08-24) and the gated suite stubs the `CommandRunner` by
// design, so that criterion is structurally un-observable from in here.
// What CAN be gated, and is gated below:
//   (1) the exact rendered markdown, pinned byte-for-byte against a checked-in
//       golden fixture for one pt-BR and one en-US roadmap;
//   (2) the exact argv `RenderPdfNode` would hand to a real `typst`, so an
//       operator can run it by hand once `typst` is installed and diff the
//       resulting PDF/markdown against these fixtures.
// The hand-verification command derived from (2) is also recorded, verbatim,
// in `planning/orchestration-run/autonomous-foundation/notes.md`, alongside a
// NOT-RUN note for the live-render criterion itself. Per D64 this criterion
// must never be claimed passed on the strength of this suite being green.
// ---------------------------------------------------------------------------

const GOLDEN_PT_BR: &str = include_str!("../fixtures/deliverable_render_pt_br.md");
const GOLDEN_EN_US: &str = include_str!("../fixtures/deliverable_render_en_us.md");

#[test]
fn golden_fixture_pt_br_markdown_matches_byte_for_byte() {
    use engine_core::workflows::deliverable_render::render_markdown::render_markdown;

    let roadmap = sample_roadmap("Loja da Ana", Locale::PtBr);
    let rendered = render_markdown(&roadmap, Locale::PtBr);

    assert_eq!(
        rendered, GOLDEN_PT_BR,
        "pt-BR rendered markdown drifted from the checked-in golden fixture \
         at crates/engine-core/tests/fixtures/deliverable_render_pt_br.md — \
         if the drift is intentional, regenerate the fixture and review the diff"
    );
}

#[test]
fn golden_fixture_en_us_markdown_matches_byte_for_byte() {
    use engine_core::workflows::deliverable_render::render_markdown::render_markdown;

    let roadmap = sample_roadmap("Acme", Locale::EnUs);
    let rendered = render_markdown(&roadmap, Locale::EnUs);

    assert_eq!(
        rendered, GOLDEN_EN_US,
        "en-US rendered markdown drifted from the checked-in golden fixture \
         at crates/engine-core/tests/fixtures/deliverable_render_en_us.md — \
         if the drift is intentional, regenerate the fixture and review the diff"
    );
}

/// Pins the EXACT argv `RenderPdfNode` hands to `typst`, over the golden
/// pt-BR fixture written to a temp `output_dir` — this is the same argv
/// shape recorded as a runnable hand-verification command in
/// `planning/orchestration-run/autonomous-foundation/notes.md`.
#[tokio::test]
async fn golden_fixture_pins_the_exact_typst_argv_for_hand_verification() {
    let output_dir = temp_output_dir("golden-argv");
    let (runner, calls) = stub_runner(success_output());
    let workflow = workflow_with_runner(runner);

    let roadmap = sample_roadmap("Loja da Ana", Locale::PtBr);
    let event = deliverable_event(&roadmap, Locale::PtBr, &output_dir);

    let ctx = workflow
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("DELIVERABLE_RENDER run should complete");
    assert_eq!(
        ctx.node_runs["RenderPdfNode"].status,
        NodeRunStatus::Success
    );

    let markdown_path = output_dir.join("loja-da-ana-roadmap.md");
    let pdf_path = output_dir.join("loja-da-ana-roadmap.pdf");

    // The exact hand-verification command (program + argv + cwd), transcribed
    // verbatim into planning/orchestration-run/autonomous-foundation/notes.md:
    //   cd <output_dir> && typst compile <markdown_path> <pdf_path>
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let (program, args, cwd) = &calls[0];
    assert_eq!(program, "typst");
    assert_eq!(
        args,
        &vec![
            "compile".to_string(),
            markdown_path.display().to_string(),
            pdf_path.display().to_string(),
        ]
    );
    assert_eq!(cwd, &output_dir);

    // The markdown typst would actually compile matches the golden fixture
    // byte-for-byte, so a hand-run comparison is meaningful.
    let written = std::fs::read_to_string(&markdown_path).unwrap();
    assert_eq!(written, GOLDEN_PT_BR);

    std::fs::remove_dir_all(&output_dir).ok();
}
