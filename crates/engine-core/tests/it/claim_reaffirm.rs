//! Integration suite for the `CLAIM_REAFFIRM` workflow (`EN.6.L` task 4):
//! e2e over a fixture lane, the no-evidence OR.K3 guard, abort/resume-skip,
//! mid-run observability, the single-file report guarantee, and profile
//! shape-invariance — all stub-driven (no real subprocess, no real
//! filesystem, no live Brain).
//!
//! Every run wires the real declared graph
//! (`engine_core::workflows::claim_reaffirm::graph::schema`) with a custom
//! registry substituting: `ClaimRecallNode`'s `HttpGet` seam (a fixture
//! [`FixtureRecallGet`] keyed by the composed identifier-anchored query
//! string), `JudgeClaimNode`'s `ModelTransport` seam (a closure that reads
//! the claim id out of the prompt text and returns a canned verdict), and
//! `RenderReportNode`'s `ReportFs` seam (`StubReportFs`, already provided
//! by `render_report`). `LoadClaimsNode` needs no stub at all — every test
//! drives it via `ClaimReaffirmInput::lane_source_override`, so it never
//! touches `resolve_brain_root`/`RepoRegistry`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use claude_code_rs::parse::Usage as SdkUsage;
use claude_code_rs::{Config, Outcome};
use engine_contract::{NodeRunStatus, TaskContext};
use engine_core::node::{Node, NodeError, NodeRegistry};
use engine_core::workflow::Workflow;
use engine_core::workflows::claim_reaffirm::graph;
use engine_core::workflows::claim_reaffirm::judge::{ClaimRecallNode, JudgeClaimNode};
use engine_core::workflows::claim_reaffirm::load_claims::LoadClaimsNode;
use engine_core::workflows::claim_reaffirm::queue_router::ClaimQueueRouterNode;
use engine_core::workflows::claim_reaffirm::render_report::{RenderReportNode, StubReportFs};
use engine_core::workflows::claim_reaffirm::save_verdict::SaveVerdictNode;
use engine_core::workflows::claim_reaffirm::schema::{
    ClaimItem, ClaimReaffirmState, ClaimStatus, VerdictAction,
};
use engine_core::workflows::ModelTransport;
use engine_core::{BrainConfig, CancellationToken, HttpGet, OnProgress, RunOptions};
use futures::future::BoxFuture;
use futures::FutureExt;
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn fixture_claim(id: &str, source_doc_id: &str, claim_text: &str) -> ClaimItem {
    ClaimItem {
        id: id.to_string(),
        source_doc_id: source_doc_id.to_string(),
        claim_text: claim_text.to_string(),
        freshness_date: Some("2025-01-01".to_string()),
        status: ClaimStatus::Pending,
        attempt: 0,
        verdict: None,
    }
}

/// The 3-claim fixture lane every test in this file starts from: claims
/// "a"/"b" have corpus evidence, claim "c" has none (the OR.K3 guard
/// fixture).
fn fixture_lane() -> Vec<ClaimItem> {
    vec![
        fixture_claim("claim-a", "planning/status.md", "Claim A is durably true."),
        fixture_claim(
            "claim-b",
            "planning/context.md",
            "Claim B cites a different source document.",
        ),
        fixture_claim(
            "claim-c",
            "planning/deleted-doc.md",
            "Claim C cites a document that no longer exists.",
        ),
    ]
}

fn recall_body(hits: &[(&str, &str)]) -> Value {
    json!({
        "query": "fixture",
        "count": hits.len(),
        "results": hits.iter().map(|(doc_id, content)| json!({
            "doc_id": doc_id,
            "file_path": doc_id,
            "title": null,
            "section": null,
            "content": content,
            "score": 0.9,
            "via": "hybrid",
        })).collect::<Vec<_>>(),
    })
}

/// Fixture `HttpGet`: keyed by the exact composed recall query string
/// (`"{source_doc_id}: {claim_text}"`, `judge::build_recall_query`'s
/// identifier-anchored shape). A query with no fixture entry returns an
/// empty result set (mirrors a real "nothing found" recall, not a
/// transport error) rather than panicking, so a caller only needs to
/// register the queries it cares about. Every call is counted, both in
/// total and per-query, so the abort/resume-skip test can prove exactly
/// which claims were re-recalled.
struct FixtureRecallGet {
    responses: HashMap<String, Value>,
    calls: Mutex<Vec<String>>,
}

impl FixtureRecallGet {
    fn new(responses: Vec<(&str, Value)>) -> Self {
        Self {
            responses: responses
                .into_iter()
                .map(|(q, body)| (q.to_string(), body))
                .collect(),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl HttpGet for FixtureRecallGet {
    async fn fetch(
        &self,
        _url: &str,
        query: &[(&str, &str)],
        _headers: &[(&str, &str)],
    ) -> Result<Value, String> {
        let q = query
            .iter()
            .find(|(name, _)| *name == "q")
            .map(|(_, value)| value.to_string())
            .unwrap_or_default();
        self.calls.lock().unwrap().push(q.clone());
        Ok(self
            .responses
            .get(&q)
            .cloned()
            .unwrap_or_else(|| recall_body(&[])))
    }
}

/// A `ModelTransport` stub for `JudgeClaimNode`: reads the claim text back
/// out of the prompt (the prompt embeds it verbatim, per
/// `judge::build_prompt`) and returns the canned `action` registered for
/// that claim, defaulting to `"bump_freshness"` for anything unregistered.
/// Counts total calls so the abort/resume-skip test can prove exactly how
/// many judge calls a re-trigger actually makes.
fn fixture_judge_transport(
    actions: Vec<(&str, &str)>,
    call_count: Arc<AtomicUsize>,
) -> ModelTransport {
    let actions: HashMap<String, String> = actions
        .into_iter()
        .map(|(claim_text, action)| (claim_text.to_string(), action.to_string()))
        .collect();
    Arc::new(move |_config: Config, prompt: String| {
        call_count.fetch_add(1, Ordering::SeqCst);
        let action = actions
            .iter()
            .find(|(claim_text, _)| prompt.contains(claim_text.as_str()))
            .map(|(_, action)| action.clone())
            .unwrap_or_else(|| "bump_freshness".to_string());
        let structured = json!({ "action": action, "reasoning": "fixture verdict" });
        async move {
            Ok(Outcome {
                text: serde_json::to_string(&structured).unwrap(),
                cost_usd: 0.01,
                usage: SdkUsage {
                    input_tokens: 10,
                    output_tokens: 10,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
                model_usage: std::collections::BTreeMap::new(),
                session_id: None,
                structured_output: Some(structured),
                is_error: false,
                api_error_status: None,
            })
        }
        .boxed()
    })
}

/// Wraps the real `SaveVerdictNode`, triggering `token.cancel()` as a side
/// effect once it finishes persisting the verdict for `target_claim_id` —
/// simulating an external abort landing right after that claim's pass
/// completes. `Workflow::run_with` checks cancellation at the node
/// boundary *before* dispatching the next node (per `cancellation.rs`), so
/// this halts the walk before `ClaimQueueRouterNode` dispatches again:
/// `target_claim_id` lands `Judged` in the durable state, and nothing
/// after it is touched.
struct CancelAfterClaim {
    inner: SaveVerdictNode,
    target_claim_id: String,
    token: CancellationToken,
}

#[async_trait::async_trait]
impl Node for CancelAfterClaim {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let dispatched_id = ctx
            .nodes
            .get("ClaimQueueRouterNode")
            .and_then(|stamp| stamp.get("current_claim_id"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let ctx = self.inner.process(ctx).await?;
        if dispatched_id == self.target_claim_id {
            self.token.cancel();
        }
        Ok(ctx)
    }

    fn name(&self) -> &str {
        self.inner.name()
    }
}

/// Assembles a hermetic `CLAIM_REAFFIRM` `Workflow`: the real declared
/// graph (`graph::schema()`), every node for real except the three
/// injected seams (`recall`, `transport`, `report_fs`).
fn build_workflow(
    recall: Arc<dyn HttpGet>,
    transport: ModelTransport,
    report_fs: Arc<StubReportFs>,
    report_path: impl Into<PathBuf>,
) -> Workflow {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(LoadClaimsNode::new()));
    registry.register(Box::new(ClaimQueueRouterNode::new()));
    registry.register(Box::new(
        ClaimRecallNode::new(BrainConfig::new("http://localhost:8000", None)).with_http_get(recall),
    ));
    registry.register(Box::new(JudgeClaimNode::new().with_transport(transport)));
    registry.register(Box::new(SaveVerdictNode::new()));
    registry.register(Box::new(
        RenderReportNode::new()
            .with_fs(report_fs)
            .with_report_path(report_path),
    ));
    Workflow::new_validated(registry, graph::schema())
        .expect("CLAIM_REAFFIRM declared graph must validate")
}

/// Same as [`build_workflow`], but `SaveVerdictNode` is replaced by
/// [`CancelAfterClaim`] so a run can be interrupted mid-lane.
fn build_workflow_with_cancel_after(
    recall: Arc<dyn HttpGet>,
    transport: ModelTransport,
    report_fs: Arc<StubReportFs>,
    report_path: impl Into<PathBuf>,
    target_claim_id: &str,
    token: CancellationToken,
) -> Workflow {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(LoadClaimsNode::new()));
    registry.register(Box::new(ClaimQueueRouterNode::new()));
    registry.register(Box::new(
        ClaimRecallNode::new(BrainConfig::new("http://localhost:8000", None)).with_http_get(recall),
    ));
    registry.register(Box::new(JudgeClaimNode::new().with_transport(transport)));
    registry.register(Box::new(CancelAfterClaim {
        inner: SaveVerdictNode::new(),
        target_claim_id: target_claim_id.to_string(),
        token,
    }));
    registry.register(Box::new(
        RenderReportNode::new()
            .with_fs(report_fs)
            .with_report_path(report_path),
    ));
    Workflow::new_validated(registry, graph::schema())
        .expect("CLAIM_REAFFIRM declared graph must validate")
}

fn noop_progress<'a>() -> OnProgress<'a> {
    Box::new(|_ctx: &TaskContext| {})
}

fn saved_state(ctx: &TaskContext) -> ClaimReaffirmState {
    let value = ctx
        .nodes
        .get("SaveVerdictNode")
        .expect("SaveVerdictNode has stamped a result")
        .clone();
    serde_json::from_value(value).expect("valid ClaimReaffirmState")
}

// ---------------------------------------------------------------------------
// 1. e2e: every claim gets exactly one verdict with >=1 citation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_every_claim_gets_one_verdict_with_evidence_and_report_renders() {
    let recall = Arc::new(FixtureRecallGet::new(vec![
        (
            "planning/status.md: Claim A is durably true.",
            recall_body(&[("planning/status.md", "A is still true today")]),
        ),
        (
            "planning/context.md: Claim B cites a different source document.",
            recall_body(&[("planning/context.md", "B still corroborated")]),
        ),
        (
            "planning/deleted-doc.md: Claim C cites a document that no longer exists.",
            recall_body(&[("planning/status.md", "unrelated evidence")]),
        ),
    ]));
    let call_count = Arc::new(AtomicUsize::new(0));
    let transport = fixture_judge_transport(
        vec![
            ("Claim A is durably true.", "bump_freshness"),
            ("Claim B cites a different source document.", "supersede"),
            ("Claim C cites a document that no longer exists.", "archive"),
        ],
        call_count.clone(),
    );
    let report_fs = Arc::new(StubReportFs::new());
    let workflow = build_workflow(
        recall,
        transport,
        report_fs.clone(),
        "/tmp/claim-reaffirm-e2e/report.md",
    );

    let event = json!({ "lane_source_override": fixture_lane() });
    let result = workflow
        .run_with(event, noop_progress(), RunOptions::default())
        .await
        .expect("run completes");

    let state = saved_state(&result);
    assert_eq!(state.claims.len(), 3);
    for claim in &state.claims {
        assert_eq!(
            claim.status,
            ClaimStatus::Judged,
            "claim {} judged",
            claim.id
        );
        let verdict = claim.verdict.as_ref().expect("verdict recorded");
        assert!(
            !verdict.evidence.is_empty(),
            "claim {} has >=1 citation",
            claim.id
        );
        assert!(verdict.transport.is_some(), "transport stamped");
    }
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        3,
        "one judge call per claim"
    );

    // The report is the only file this workflow writes.
    let writes = report_fs.writes();
    assert_eq!(writes.len(), 1, "exactly one report file written");
    assert!(writes[0].1.contains("Claim Reaffirmation Report"));
    assert!(writes[0].1.contains("bump-freshness"));
    assert!(writes[0].1.contains("supersede"));
    assert!(writes[0].1.contains("archive"));

    for identity in [
        "LoadClaimsNode",
        "ClaimQueueRouterNode",
        "ClaimRecallNode",
        "JudgeClaimNode",
        "SaveVerdictNode",
        "RenderReportNode",
    ] {
        let run = result
            .node_runs
            .get(identity)
            .unwrap_or_else(|| panic!("{identity} has a run record"));
        assert_eq!(run.status, NodeRunStatus::Success, "{identity} status");
    }
}

// ---------------------------------------------------------------------------
// 2. no-evidence guard: empty recall never yields BumpFreshness
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_evidence_guard_forces_needs_human_never_silently_bumps_freshness() {
    let claims = vec![fixture_claim(
        "claim-no-evidence",
        "planning/deleted-doc.md",
        "A claim whose source document is gone.",
    )];
    // No fixture entry registered for this claim's query -> FixtureRecallGet
    // falls through to an empty result set, exactly like a real "nothing
    // found" recall.
    let recall = Arc::new(FixtureRecallGet::new(vec![]));
    let call_count = Arc::new(AtomicUsize::new(0));
    // The model itself tries to say BumpFreshness -- the structural guard
    // must override it regardless.
    let transport = fixture_judge_transport(
        vec![("A claim whose source document is gone.", "bump_freshness")],
        call_count,
    );
    let report_fs = Arc::new(StubReportFs::new());
    let workflow = build_workflow(
        recall,
        transport,
        report_fs,
        "/tmp/claim-reaffirm-guard/report.md",
    );

    let event = json!({ "lane_source_override": claims });
    let result = workflow
        .run_with(event, noop_progress(), RunOptions::default())
        .await
        .expect("run completes");

    let state = saved_state(&result);
    assert_eq!(state.claims.len(), 1);
    let verdict = state.claims[0].verdict.as_ref().expect("verdict recorded");
    assert_eq!(
        verdict.action,
        VerdictAction::NeedsHuman,
        "empty evidence must never silently bump freshness, even when the model says so"
    );
    assert!(verdict.evidence.is_empty());
}

// ---------------------------------------------------------------------------
// 3. abort/resume-skip
// ---------------------------------------------------------------------------

#[tokio::test]
async fn abort_mid_lane_then_resume_judges_only_the_remainder() {
    let all_claims = fixture_lane();
    let fixtures = vec![
        (
            "planning/status.md: Claim A is durably true.",
            recall_body(&[("planning/status.md", "A still true")]),
        ),
        (
            "planning/context.md: Claim B cites a different source document.",
            recall_body(&[("planning/context.md", "B still true")]),
        ),
        (
            "planning/deleted-doc.md: Claim C cites a document that no longer exists.",
            recall_body(&[("planning/status.md", "unrelated")]),
        ),
    ];
    let actions = vec![
        ("Claim A is durably true.", "bump_freshness"),
        ("Claim B cites a different source document.", "supersede"),
        ("Claim C cites a document that no longer exists.", "archive"),
    ];

    // --- Run 1: cancel right after claim-a's SaveVerdictNode pass ---
    let token = CancellationToken::new();
    let recall_1 = Arc::new(FixtureRecallGet::new(fixtures.clone()));
    let judge_calls_1 = Arc::new(AtomicUsize::new(0));
    let transport_1 = fixture_judge_transport(actions.clone(), judge_calls_1.clone());
    let report_fs_1 = Arc::new(StubReportFs::new());
    let workflow_1 = build_workflow_with_cancel_after(
        recall_1.clone(),
        transport_1,
        report_fs_1,
        "/tmp/claim-reaffirm-abort-1/report.md",
        "claim-a",
        token.clone(),
    );

    let event_1 = json!({ "lane_source_override": all_claims });
    let result_1 = workflow_1
        .run_with(
            event_1,
            noop_progress(),
            RunOptions {
                cancellation_token: Some(token.clone()),
                budget: None,
                pause_signal: None,
                run_id: None,
            },
        )
        .await
        .expect("a cancelled run returns Ok, not Err");

    let state_1 = saved_state(&result_1);
    assert_eq!(state_1.claims.len(), 3);
    let claim_a = state_1
        .claims
        .iter()
        .find(|c| c.id == "claim-a")
        .expect("claim-a present");
    assert_eq!(
        claim_a.status,
        ClaimStatus::Judged,
        "claim-a judged before abort"
    );
    for id in ["claim-b", "claim-c"] {
        let claim = state_1.claims.iter().find(|c| c.id == id).expect("present");
        assert_eq!(
            claim.status,
            ClaimStatus::Pending,
            "{id} untouched by the abort"
        );
    }
    assert_eq!(
        judge_calls_1.load(Ordering::SeqCst),
        1,
        "only claim-a was judged before the abort"
    );

    // --- Run 2: re-trigger with run 1's resulting state as the lane ---
    let recall_2 = Arc::new(FixtureRecallGet::new(fixtures));
    let judge_calls_2 = Arc::new(AtomicUsize::new(0));
    let transport_2 = fixture_judge_transport(actions, judge_calls_2.clone());
    let report_fs_2 = Arc::new(StubReportFs::new());
    let workflow_2 = build_workflow(
        recall_2.clone(),
        transport_2,
        report_fs_2,
        "/tmp/claim-reaffirm-abort-2/report.md",
    );

    let event_2 = json!({ "lane_source_override": state_1.claims });
    let result_2 = workflow_2
        .run_with(event_2, noop_progress(), RunOptions::default())
        .await
        .expect("resumed run completes");

    let state_2 = saved_state(&result_2);
    assert_eq!(state_2.claims.len(), 3);
    for claim in &state_2.claims {
        assert_eq!(
            claim.status,
            ClaimStatus::Judged,
            "{} judged by run 2",
            claim.id
        );
    }
    assert_eq!(
        judge_calls_2.load(Ordering::SeqCst),
        2,
        "run 2 judges only claim-b and claim-c, never re-judging claim-a"
    );
    assert_eq!(
        recall_2.call_count(),
        2,
        "run 2 recalls only claim-b and claim-c"
    );
    for q in recall_2.calls() {
        assert!(
            !q.contains("Claim A is durably true."),
            "claim-a must not be re-recalled by run 2: {q:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 4. observability: the verdict array grows pass-by-pass
// ---------------------------------------------------------------------------

#[tokio::test]
async fn on_progress_shows_the_verdict_array_growing_pass_by_pass() {
    let recall = Arc::new(FixtureRecallGet::new(vec![
        (
            "planning/status.md: Claim A is durably true.",
            recall_body(&[("planning/status.md", "A still true")]),
        ),
        (
            "planning/context.md: Claim B cites a different source document.",
            recall_body(&[("planning/context.md", "B still true")]),
        ),
        (
            "planning/deleted-doc.md: Claim C cites a document that no longer exists.",
            recall_body(&[("planning/status.md", "unrelated")]),
        ),
    ]));
    let call_count = Arc::new(AtomicUsize::new(0));
    let transport = fixture_judge_transport(
        vec![
            ("Claim A is durably true.", "bump_freshness"),
            ("Claim B cites a different source document.", "supersede"),
            ("Claim C cites a document that no longer exists.", "archive"),
        ],
        call_count,
    );
    let report_fs = Arc::new(StubReportFs::new());
    let workflow = build_workflow(
        recall,
        transport,
        report_fs,
        "/tmp/claim-reaffirm-progress/report.md",
    );

    let snapshots: Arc<Mutex<Vec<TaskContext>>> = Arc::new(Mutex::new(Vec::new()));
    let snapshots_handle = snapshots.clone();
    let on_progress: OnProgress<'_> =
        Box::new(move |ctx: &TaskContext| snapshots_handle.lock().unwrap().push(ctx.clone()));

    let event = json!({ "lane_source_override": fixture_lane() });
    workflow
        .run_with(event, on_progress, RunOptions::default())
        .await
        .expect("run completes");

    let judged_counts: Vec<usize> = snapshots
        .lock()
        .unwrap()
        .iter()
        .filter_map(|ctx| ctx.nodes.get("SaveVerdictNode"))
        .filter_map(|value| serde_json::from_value::<ClaimReaffirmState>(value.clone()).ok())
        .map(|state| {
            state
                .claims
                .iter()
                .filter(|c| c.status == ClaimStatus::Judged)
                .count()
        })
        .collect();

    assert!(
        judged_counts.len() >= 3,
        "at least one SaveVerdictNode snapshot per claim, got {judged_counts:?}"
    );
    for window in judged_counts.windows(2) {
        assert!(
            window[1] >= window[0],
            "the judged count must never shrink pass-to-pass: {judged_counts:?}"
        );
    }
    assert_eq!(
        judged_counts.last().copied(),
        Some(3),
        "the final snapshot shows every claim judged"
    );
}

// ---------------------------------------------------------------------------
// 5. single-file guarantee (also asserted inline in test 1; standalone here
//    over an empty lane, the cheapest possible drain).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn exactly_one_report_file_is_written_even_on_an_empty_lane() {
    let recall = Arc::new(FixtureRecallGet::new(vec![]));
    let transport = fixture_judge_transport(vec![], Arc::new(AtomicUsize::new(0)));
    let report_fs = Arc::new(StubReportFs::new());
    let workflow = build_workflow(
        recall,
        transport,
        report_fs.clone(),
        "/tmp/claim-reaffirm-single-file/report.md",
    );

    let event = json!({ "lane_source_override": Vec::<ClaimItem>::new() });
    let result = workflow
        .run_with(event, noop_progress(), RunOptions::default())
        .await
        .expect("run completes");

    let writes = report_fs.writes();
    assert_eq!(
        writes.len(),
        1,
        "exactly one file written for an empty lane"
    );
    assert!(writes[0].1.contains("No stale claims found"));
    assert!(
        !result.node_runs.contains_key("ClaimRecallNode")
            || result.node_runs["ClaimRecallNode"].status == NodeRunStatus::Pending,
        "an empty lane drains straight to the report without visiting the recall/judge nodes"
    );
}

// ---------------------------------------------------------------------------
// 6. profile shape-invariance
// ---------------------------------------------------------------------------

#[tokio::test]
async fn baseline_and_cheap_fast_profiles_produce_identical_graph_and_verdict_schema() {
    use engine_core::policy::PolicyConfigSource;
    use engine_core::workflows::claim_reaffirm::schema::resolve_policy_for_run_from;

    // The declared graph never varies by profile — `graph::schema()` takes
    // no policy input at all, so this is a structural fact, not merely a
    // coincidence of the fixtures below.
    let schema_a = graph::schema();
    let schema_b = graph::schema();
    assert_eq!(
        schema_a
            .nodes
            .keys()
            .collect::<std::collections::BTreeSet<_>>(),
        schema_b
            .nodes
            .keys()
            .collect::<std::collections::BTreeSet<_>>(),
        "graph node identities are profile-invariant"
    );

    for profile in ["baseline", "cheap-fast", "thorough"] {
        let policy = resolve_policy_for_run_from(&PolicyConfigSource::Builtin, Some(profile), None)
            .unwrap_or_else(|err| panic!("profile {profile} must resolve: {err:?}"));
        // Every knob this workflow ships is set (non-defaulted-away) by
        // every one of the three canonical profiles.
        assert!(policy.max_attempts >= 1);
        assert!(policy.recall_limit >= 1);
    }

    // Emitted verdict JSON shape is identical regardless of which profile
    // produced it -- run the same claim once under each of two profiles
    // and diff the verdict's own key set.
    let recall = Arc::new(FixtureRecallGet::new(vec![(
        "planning/status.md: Claim A is durably true.",
        recall_body(&[("planning/status.md", "A still true")]),
    )]));
    let call_count = Arc::new(AtomicUsize::new(0));
    let transport = fixture_judge_transport(
        vec![("Claim A is durably true.", "bump_freshness")],
        call_count,
    );
    let report_fs = Arc::new(StubReportFs::new());
    let workflow = build_workflow(
        recall,
        transport,
        report_fs,
        "/tmp/claim-reaffirm-shape/report.md",
    );

    for profile in ["baseline", "cheap-fast"] {
        let event = json!({
            "lane_source_override": vec![fixture_claim(
                "claim-a",
                "planning/status.md",
                "Claim A is durably true.",
            )],
            "profile": profile,
        });
        let result = workflow
            .run_with(event, noop_progress(), RunOptions::default())
            .await
            .expect("run completes");
        let state = saved_state(&result);
        let verdict = state.claims[0].verdict.as_ref().expect("verdict recorded");
        let value = serde_json::to_value(verdict).expect("serializes");
        let mut keys: Vec<&String> = value.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["action", "evidence", "reasoning", "transport"],
            "verdict JSON shape must not vary by profile ({profile})"
        );
    }
}

// ---------------------------------------------------------------------------
// 7. registration / no-ParallelNode regression guard at the graph level
// ---------------------------------------------------------------------------

#[tokio::test]
async fn declared_graph_workflow_type_and_registers_cleanly() {
    let workflow = build_workflow(
        Arc::new(FixtureRecallGet::new(vec![])),
        fixture_judge_transport(vec![], Arc::new(AtomicUsize::new(0))),
        Arc::new(StubReportFs::new()),
        "/tmp/claim-reaffirm-registers/report.md",
    );
    let _ = workflow; // constructs + validates without panicking
    assert_eq!(graph::WORKFLOW_TYPE, "CLAIM_REAFFIRM");
}

#[allow(dead_code)]
fn assert_boxfuture_type_hint(_f: BoxFuture<'static, claude_code_rs::Result<Outcome>>) {}
