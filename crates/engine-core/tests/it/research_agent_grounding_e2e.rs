//! Hermetic end-to-end suite for `EN.4.G` task 6 — proves the
//! `needs_further_research` flag (and its derived `validation_required`)
//! survives a real `RESEARCH_AGENT` `Workflow::run` all the way into the
//! written Opportunity's frontmatter, for both company and prospecting
//! mode.
//!
//! Follows `research_agent_contacts_e2e.rs`'s pattern exactly: the same
//! stub-transport registry shape, the same `seeded_nodes_for` policy-seed +
//! `SetupWorktreeNode` trick (so `research-agent-state.json` writes land
//! inside this test's own tempdir), and the same `MaterializeDocNode` /
//! `MergeContactsNode` `with_brain_root` pinning rather than
//! `ENGINE_BRAIN_ROOT`, so this suite stays hermetic and immune to any
//! other test's env-var mutation.
//!
//! Every test is hermetic: no network, and no write anywhere outside its
//! own tempdir — in particular nothing under the real `agentic-portfolio`
//! corpus. Assertions read the written document back through
//! `okf_core::parse_nested_frontmatter` + `Opportunity::from_frontmatter`
//! rather than raw string matching, per the task's acceptance criteria.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use claude_code_rs::{Config, Outcome};
use engine_contract::{NodeRunStatus, TaskContext};
use engine_core::node::NodeRegistry;
use engine_core::policy::{PolicyConfigSource, RESOLVED_POLICY_IDENTITY};
use engine_core::workflow::Workflow;
use engine_core::workflows::research_agent::graph as research_graph;
use engine_core::workflows::research_agent::ingress_dispatch::ResearchIngressDispatchNode;
use engine_core::workflows::research_agent::profiles;
use engine_core::workflows::research_agent::prospecting::ProspectingResearchNode;
use engine_core::workflows::research_agent::CompanyResearchNode;
use futures::FutureExt;
use okf_core::{parse_nested_frontmatter, Opportunity};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Fixtures / transports
// ---------------------------------------------------------------------------

/// A company brief carrying one ungroundable regulatory claim under
/// `needs_further_research`.
fn company_brief_flagged() -> Value {
    json!({
        "company_name": "Acme Corp",
        "summary": "Widgets and gadgets.",
        "pain_points": ["Slow onboarding"],
        "recent_developments": ["Raised a seed round"],
        "outreach_hooks": ["Congrats on the raise"],
        "sources": ["https://acme.example.com/news"],
        "contacts": [],
        "needs_further_research": [
            "FAR/DFARS compliance regime claimed but not sourced",
        ],
    })
}

/// Same shape as [`company_brief_flagged`], but fully grounded — an empty
/// `needs_further_research` list, the correct answer for a fully-grounded
/// brief.
fn company_brief_grounded() -> Value {
    json!({
        "company_name": "Acme Corp",
        "summary": "Widgets and gadgets.",
        "pain_points": ["Slow onboarding"],
        "recent_developments": ["Raised a seed round"],
        "outreach_hooks": ["Congrats on the raise"],
        "sources": ["https://acme.example.com/news"],
        "contacts": [],
        "needs_further_research": [],
    })
}

/// A prospecting result with two leads: one flags a claim, one is clean —
/// exercises the deduped, order-stable sweep-level union.
fn prospecting_result_mixed() -> Value {
    json!({
        "vertical": "legal-tech",
        "topic": "contract review pain points",
        "common_pain_points": ["Manual contract review"],
        "sources": ["https://reddit.com/r/legaltech"],
        "prospects": [
            {
                "name": "Lead One",
                "pain_points": ["Slow contract turnaround"],
                "pillar": "automation",
                "outreach_hook": "Posted about contract delays",
                "source": "https://reddit.com/r/legaltech/one",
                "contacts": [],
                "needs_further_research": [
                    "Brazilian local-LLM data-residency claim unverified",
                ],
            },
            {
                "name": "Lead Two",
                "pain_points": ["No in-house counsel"],
                "pillar": "automation",
                "outreach_hook": "Asked for referrals",
                "source": "https://reddit.com/r/legaltech/two",
                "contacts": [],
                "needs_further_research": [],
            },
        ],
    })
}

fn stub_outcome(structured: Value, input_tokens: u64, output_tokens: u64) -> Outcome {
    Outcome {
        cost_usd: 0.02,
        usage: claude_code_rs::parse::Usage {
            input_tokens,
            output_tokens,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
        model_usage: [(
            "claude-sonnet-4-5".to_string(),
            claude_code_rs::parse::ModelUsage {
                input_tokens,
                output_tokens,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                cost_usd: 0.02,
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

/// A stub transport that always replies with `body`, ignoring the prompt —
/// used for the fixed-fixture scenarios.
fn stub_transport(body: Value) -> engine_core::workflows::ModelTransport {
    Arc::new(move |_config: Config, _prompt: String| {
        let body = body.clone();
        async move { Ok(stub_outcome(body, 100, 50)) }.boxed()
    })
}

/// Pre-create `<root>/business/docs/opportunities/`, mirroring
/// `opportunity_loop_e2e.rs::opportunities_dir`.
fn opportunities_dir(root: &Path) -> std::path::PathBuf {
    let dir = root.join("business/docs/opportunities");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---------------------------------------------------------------------------
// RESEARCH_AGENT: real Workflow::run, stubbed transports, real materializers
// ---------------------------------------------------------------------------

/// Build a fresh `RESEARCH_AGENT` registry: both research branches wired to
/// the given stubbed transports, `MaterializeDocNode` + `MergeContactsNode`
/// both pinned at `root` with their real (`mev`-backed) materializers.
fn research_registry(
    root: &Path,
    company_transport: engine_core::workflows::ModelTransport,
    prospecting_transport: engine_core::workflows::ModelTransport,
) -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(research_graph::ResearchModeRouterNode));
    registry.register(Box::new(
        CompanyResearchNode::new().with_transport(company_transport),
    ));
    registry.register(Box::new(
        ProspectingResearchNode::new().with_transport(prospecting_transport),
    ));
    registry.register(Box::new(
        engine_core::nodes::materialize_doc::MaterializeDocNode::new("opportunity")
            .with_brain_root(root)
            .with_source_nodes(["CompanyResearchNode", "ProspectingResearchNode"]),
    ));
    registry.register(Box::new(
        engine_core::nodes::merge_contacts::MergeContactsNode::new()
            .with_brain_root(root)
            .with_source_nodes(["CompanyResearchNode", "ProspectingResearchNode"]),
    ));
    // `EN.6.E`: the graph's new sole terminal identity. A stub transport
    // keeps this suite hermetic; the resolved policy stamp seeded by
    // `seeded_nodes_for` carries the built-in `enabled: false` default, so
    // this node no-ops in place and records zero sends.
    registry.register(Box::new(ResearchIngressDispatchNode::new().with_transport(
        Arc::new(engine_core::nodes::channel_transport::StubChannelTransport::succeeding()),
    )));
    registry
}

/// Resolve `event`'s `RESEARCH_AGENT` policy the same way
/// `engine-serve::workflows::register_research_agent`'s factory does, and
/// seed it via `Workflow::with_seeded_nodes` (see `opportunity_loop_e2e.rs`
/// for the full rationale). Also seeds a `SetupWorktreeNode` result
/// pointing at `root` so `persist_state`'s
/// `planning/research-agent-state.json` write lands inside this test's own
/// tempdir.
fn seeded_nodes_for(root: &Path, event: &Value) -> HashMap<String, Value> {
    let probe_ctx = TaskContext {
        event: event.clone(),
        nodes: HashMap::new(),
        metadata: json!({}),
        node_runs: HashMap::new(),
    };
    let policy = profiles::resolve_policy_for_run_from(&probe_ctx, &PolicyConfigSource::Builtin)
        .expect("RESEARCH_AGENT policy should resolve for this event");
    let mut seeded = HashMap::new();
    seeded.insert(
        RESOLVED_POLICY_IDENTITY.to_string(),
        serde_json::to_value(&policy).expect("policy should serialize"),
    );
    seeded.insert(
        "SetupWorktreeNode".to_string(),
        json!({ "worktree_path": root.to_string_lossy() }),
    );
    seeded
}

async fn run_research(
    root: &Path,
    event: Value,
    company_transport: engine_core::workflows::ModelTransport,
    prospecting_transport: engine_core::workflows::ModelTransport,
) -> TaskContext {
    let seeded = seeded_nodes_for(root, &event);
    let workflow = Workflow::new_validated(
        research_registry(root, company_transport, prospecting_transport),
        research_graph::schema(),
    )
    .expect("RESEARCH_AGENT declared graph should validate")
    .with_seeded_nodes(seeded);

    workflow
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("RESEARCH_AGENT run should complete")
}

/// Convenience wrapper for scenarios that only exercise the company branch —
/// the prospecting transport is never invoked by a `mode: company` event, so
/// a placeholder stub (never called) is enough.
async fn run_company(root: &Path, event: Value, company_body: Value) -> TaskContext {
    run_research(
        root,
        event,
        stub_transport(company_body),
        stub_transport(json!({"vertical": "unused", "prospects": []})),
    )
    .await
}

/// Convenience wrapper for scenarios that only exercise the prospecting
/// branch.
async fn run_prospecting(root: &Path, event: Value, prospecting_body: Value) -> TaskContext {
    run_research(
        root,
        event,
        stub_transport(json!({"company_name": "unused", "contacts": []})),
        stub_transport(prospecting_body),
    )
    .await
}

fn company_event(company_name: &str) -> Value {
    json!({
        "mode": "company",
        "company_name": company_name,
        "company_url": "https://acme.example.com",
    })
}

fn prospecting_event() -> Value {
    json!({
        "mode": "prospecting",
        "vertical": "legal-tech",
        "topic": "contract review pain points",
    })
}

/// Load and reconstruct the `Opportunity` at `path` through okf-core's real
/// parser — never raw string matching, per the task's acceptance criteria.
fn reconstruct(path: &Path) -> Opportunity {
    let content = std::fs::read_to_string(path).expect("written file should be readable");
    let fields = parse_nested_frontmatter(&content).expect("must parse frontmatter");
    Opportunity::from_frontmatter(&fields).expect("must reconstruct Opportunity")
}

fn written_path(ctx: &TaskContext) -> std::path::PathBuf {
    let paths = ctx.nodes["MaterializeDocNode"]["paths"]
        .as_array()
        .expect("paths array");
    assert_eq!(paths.len(), 1);
    Path::new(paths[0].as_str().expect("path string")).to_path_buf()
}

/// Assert nothing was written anywhere outside `root`'s own tree — the
/// written document path must be a descendant of `root`.
fn assert_written_inside(root: &Path, path: &Path) {
    let canonical_root = root.canonicalize().expect("root should canonicalize");
    let canonical_path = path.canonicalize().expect("written path should exist");
    assert!(
        canonical_path.starts_with(&canonical_root),
        "written document {canonical_path:?} escaped the tempdir root {canonical_root:?}"
    );
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

#[tokio::test]
async fn company_mode_flagged_claim_survives_into_the_written_frontmatter() {
    let tmp = tempfile::tempdir().expect("tempdir");
    opportunities_dir(tmp.path());

    let ctx = run_company(
        tmp.path(),
        company_event("Acme Corp"),
        company_brief_flagged(),
    )
    .await;

    assert_eq!(
        ctx.node_runs["MaterializeDocNode"].status,
        NodeRunStatus::Success
    );

    let path = written_path(&ctx);
    assert_written_inside(tmp.path(), &path);
    let opp = reconstruct(&path);

    assert_eq!(
        opp.needs_further_research,
        vec!["FAR/DFARS compliance regime claimed but not sourced".to_string()],
        "the flagged claim must land under needs_further_research: in the written doc"
    );
    assert!(
        opp.validation_required(),
        "a non-empty needs_further_research list must derive validation_required: true"
    );

    let content = std::fs::read_to_string(&path).expect("written file readable");
    assert!(
        content.contains("validation_required: \"true\"\n"),
        "the written frontmatter must carry validation_required: true, present not absent"
    );
}

#[tokio::test]
async fn company_mode_fully_grounded_brief_writes_present_empty_list_and_false() {
    let tmp = tempfile::tempdir().expect("tempdir");
    opportunities_dir(tmp.path());

    let ctx = run_company(
        tmp.path(),
        company_event("Acme Corp"),
        company_brief_grounded(),
    )
    .await;

    assert_eq!(
        ctx.node_runs["MaterializeDocNode"].status,
        NodeRunStatus::Success
    );

    let path = written_path(&ctx);
    assert_written_inside(tmp.path(), &path);
    let opp = reconstruct(&path);

    assert!(
        opp.needs_further_research.is_empty(),
        "a fully-grounded brief must write an empty needs_further_research list"
    );
    assert!(
        !opp.validation_required(),
        "an empty needs_further_research list must derive validation_required: false"
    );

    let content = std::fs::read_to_string(&path).expect("written file readable");
    assert!(
        content.contains("needs_further_research: []\n"),
        "the empty list must be PRESENT in the frontmatter, not omitted"
    );
    assert!(
        content.contains("validation_required: \"false\"\n"),
        "validation_required: false must be PRESENT in the frontmatter, not omitted"
    );
}

#[tokio::test]
async fn prospecting_mode_sweep_document_carries_the_deduped_union_and_true() {
    let tmp = tempfile::tempdir().expect("tempdir");
    opportunities_dir(tmp.path());

    let ctx = run_prospecting(tmp.path(), prospecting_event(), prospecting_result_mixed()).await;

    assert_eq!(
        ctx.node_runs["MaterializeDocNode"].status,
        NodeRunStatus::Success
    );

    let path = written_path(&ctx);
    assert_written_inside(tmp.path(), &path);
    let opp = reconstruct(&path);

    assert_eq!(
        opp.needs_further_research,
        vec!["Brazilian local-LLM data-residency claim unverified".to_string()],
        "the sweep-level union must carry Lead One's flagged claim"
    );
    assert!(
        opp.validation_required(),
        "at least one flagged lead must derive validation_required: true on the sweep doc"
    );

    // The per-lead lists must also survive verbatim inside the embedded
    // `## Research Brief` JSON — the union is additive, not a replacement.
    let content = std::fs::read_to_string(&path).expect("written file readable");
    let brief_start = content
        .find("## Research Brief")
        .expect("Research Brief section must be present");
    let brief_section = &content[brief_start..];
    assert!(
        brief_section.contains("Brazilian local-LLM data-residency claim unverified"),
        "Lead One's per-lead flagged claim must survive verbatim in the embedded brief JSON"
    );
    assert!(
        brief_section.contains("\"name\": \"Lead One\""),
        "the embedded brief JSON must still carry both prospects"
    );
    assert!(
        brief_section.contains("\"name\": \"Lead Two\""),
        "the embedded brief JSON must still carry both prospects"
    );
}
