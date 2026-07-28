//! Hermetic end-to-end suite for `EN.4.E` task 9 — the real
//! `RESEARCH_AGENT` `Workflow::run`, stubbed model transports, REAL
//! `MevDocMaterializer` against a `tempfile::tempdir()` corpus, driven all
//! the way through the terminal `MergeContactsNode` step.
//!
//! Follows `opportunity_loop_e2e.rs`'s pattern exactly: the same
//! stub-transport registry shape, the same `seeded_nodes_for` policy-seed +
//! `SetupWorktreeNode` trick (so `research-agent-state.json` writes land
//! inside this test's own tempdir), and the same
//! `MaterializeDocNode::with_brain_root` / `MergeContactsNode::with_brain_root`
//! pinning rather than `ENGINE_BRAIN_ROOT`, so this suite stays hermetic and
//! immune to any other test's env-var mutation.
//!
//! Every test is hermetic: no network, and no write anywhere outside its own
//! tempdir — in particular nothing under the real `agentic-portfolio` corpus.
//! Assertions read the written document back through
//! `okf_core::parse_nested_frontmatter` + `Opportunity::from_frontmatter`
//! rather than raw string matching, per the task's acceptance criteria.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use claude_code_rs::{Config, Outcome};
use engine_contract::{NodeRunStatus, TaskContext};
use engine_core::node::{Node, NodeRegistry};
use engine_core::policy::{PolicyConfigSource, RESOLVED_POLICY_IDENTITY};
use engine_core::workflow::Workflow;
use engine_core::workflows::research_agent::graph as research_graph;
use engine_core::workflows::research_agent::profiles;
use engine_core::workflows::research_agent::prospecting::ProspectingResearchNode;
use engine_core::workflows::research_agent::CompanyResearchNode;
use futures::FutureExt;
use okf_core::{parse_nested_frontmatter, Opportunity};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Fixtures / transports
// ---------------------------------------------------------------------------

/// A company brief carrying one named contact plus two `sources[]` entries,
/// used by the "contacts present" / idempotency / merge-policy scenarios.
fn company_brief_with_contact() -> Value {
    json!({
        "company_name": "Acme Corp",
        "summary": "Widgets and gadgets.",
        "pain_points": ["Slow onboarding"],
        "recent_developments": ["Raised a seed round"],
        "outreach_hooks": ["Congrats on the raise"],
        "sources": [
            "https://acme.example.com/news",
            "https://acme.example.com/about",
        ],
        "contacts": [
            {
                "name": "Jane Doe",
                "role": "Head of Ops",
                "emails": ["jane@acme.example.com"],
                "whatsapp": [],
                "phones": [],
                "links": [],
                "note": "",
            }
        ],
    })
}

/// Same shape as [`company_brief_with_contact`], but the contact carries a
/// second, distinct email — used to prove a re-run unions channels on a
/// name match rather than appending a duplicate contact.
fn company_brief_with_contact_second_email() -> Value {
    json!({
        "company_name": "Acme Corp",
        "summary": "Widgets and gadgets.",
        "pain_points": ["Slow onboarding"],
        "recent_developments": ["Raised a seed round"],
        "outreach_hooks": ["Congrats on the raise"],
        "sources": [
            "https://acme.example.com/news",
            "https://acme.example.com/about",
        ],
        "contacts": [
            {
                "name": "Jane Doe",
                "role": "",
                "emails": ["jane.doe@acme.example.com"],
                "whatsapp": [],
                "phones": [],
                "links": [],
                "note": "",
            }
        ],
    })
}

/// A company brief with an empty `contacts[]` — the "none found" success
/// path.
fn company_brief_zero_contacts() -> Value {
    json!({
        "company_name": "Acme Corp",
        "summary": "Widgets and gadgets.",
        "pain_points": ["Slow onboarding"],
        "recent_developments": ["Raised a seed round"],
        "outreach_hooks": ["Congrats on the raise"],
        "sources": ["https://acme.example.com/news"],
        "contacts": [],
    })
}

/// A prospecting result with two leads, each carrying one contact
/// (including a generic, name-less channel), plus a `sources[]` entry.
fn prospecting_result_with_contacts() -> Value {
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
                "contacts": [
                    {
                        "name": "",
                        "role": "",
                        "emails": [],
                        "whatsapp": ["+55 11 90000-0000"],
                        "phones": [],
                        "links": [],
                        "note": "storefront WhatsApp",
                    }
                ],
            },
            {
                "name": "Lead Two",
                "pain_points": ["No in-house counsel"],
                "pillar": "automation",
                "outreach_hook": "Asked for referrals",
                "source": "https://reddit.com/r/legaltech/two",
                "contacts": [
                    {
                        "name": "Bob Smith",
                        "role": "Founder",
                        "emails": ["bob@leadtwo.example.com"],
                        "whatsapp": [],
                        "phones": [],
                        "links": [],
                        "note": "",
                    }
                ],
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

fn company_event() -> Value {
    json!({
        "mode": "company",
        "company_name": "Acme Corp",
        "company_url": "https://acme.example.com",
    })
}

fn company_event_with_profile(profile: &str) -> Value {
    json!({
        "mode": "company",
        "company_name": "Acme Corp",
        "company_url": "https://acme.example.com",
        "profile": profile,
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

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

#[tokio::test]
async fn company_mode_lifts_url_links_and_contact_into_the_written_opportunity() {
    let tmp = tempfile::tempdir().expect("tempdir");
    opportunities_dir(tmp.path());

    let ctx = run_company(tmp.path(), company_event(), company_brief_with_contact()).await;

    assert_eq!(
        ctx.node_runs["MaterializeDocNode"].status,
        NodeRunStatus::Success
    );
    assert_eq!(
        ctx.node_runs["MergeContactsNode"].status,
        NodeRunStatus::Success
    );
    let merge_result = &ctx.nodes["MergeContactsNode"];
    assert_eq!(merge_result["merged"], json!(true));
    assert_eq!(merge_result["contacts"], json!(1));

    let path = written_path(&ctx);
    let opp = reconstruct(&path);

    assert_eq!(opp.url.as_deref(), Some("https://acme.example.com"));
    assert_eq!(
        opp.links,
        vec![
            "https://acme.example.com/news".to_string(),
            "https://acme.example.com/about".to_string(),
        ]
    );
    assert_eq!(opp.contacts.len(), 1);
    let contact = &opp.contacts[0];
    assert_eq!(contact.name, "Jane Doe");
    assert_eq!(contact.role, "Head of Ops");
    assert_eq!(contact.emails, vec!["jane@acme.example.com".to_string()]);
}

#[tokio::test]
async fn company_mode_re_run_is_byte_idempotent_no_duplicate_contact_or_link() {
    let tmp = tempfile::tempdir().expect("tempdir");
    opportunities_dir(tmp.path());

    let ctx1 = run_company(tmp.path(), company_event(), company_brief_with_contact()).await;
    let path = written_path(&ctx1);
    let bytes_after_first = std::fs::read(&path).expect("file exists after first run");

    run_company(tmp.path(), company_event(), company_brief_with_contact()).await;
    let bytes_after_second = std::fs::read(&path).expect("file exists after second run");

    assert_eq!(
        bytes_after_first, bytes_after_second,
        "identical re-run must add no duplicate contact and no duplicate link"
    );

    let opp = reconstruct(&path);
    assert_eq!(opp.contacts.len(), 1, "no duplicate contact appended");
    assert_eq!(
        opp.links.len(),
        2,
        "no duplicate link appended, sources stay deduped"
    );
}

#[tokio::test]
async fn company_mode_zero_contacts_writes_the_doc_and_skips_the_merge_call() {
    let tmp = tempfile::tempdir().expect("tempdir");
    opportunities_dir(tmp.path());

    let ctx = run_company(tmp.path(), company_event(), company_brief_zero_contacts()).await;

    assert_eq!(
        ctx.node_runs["MaterializeDocNode"].status,
        NodeRunStatus::Success
    );
    assert_eq!(
        ctx.node_runs["MergeContactsNode"].status,
        NodeRunStatus::Success
    );
    let merge_result = &ctx.nodes["MergeContactsNode"];
    assert_eq!(
        merge_result["merged"],
        json!(false),
        "no contacts means no seam call"
    );
    assert_eq!(merge_result["contacts"], json!(0));

    let path = written_path(&ctx);
    let opp = reconstruct(&path);
    assert!(
        opp.contacts.is_empty(),
        "zero-contact run must leave contacts empty, not fabricate anything"
    );
}

#[tokio::test]
async fn cheap_fast_profile_off_depth_still_writes_url_links_and_walks_merge_as_a_no_op() {
    let tmp = tempfile::tempdir().expect("tempdir");
    opportunities_dir(tmp.path());

    // `cheap-fast` resolves `contact_enrichment.research` to `off`. The
    // stub transport still simulates a model reply with an empty
    // `contacts[]` (an `off`-depth prompt never asks for one, so a real
    // run reports none) — proving the graph shape and the write path are
    // unaffected by the policy depth.
    let ctx = run_company(
        tmp.path(),
        company_event_with_profile("cheap-fast"),
        company_brief_zero_contacts(),
    )
    .await;

    assert_eq!(
        ctx.node_runs["MaterializeDocNode"].status,
        NodeRunStatus::Success
    );
    assert_eq!(
        ctx.node_runs["MergeContactsNode"].status,
        NodeRunStatus::Success,
        "an off-depth run must still walk MergeContactsNode cleanly"
    );
    let merge_result = &ctx.nodes["MergeContactsNode"];
    assert_eq!(merge_result["merged"], json!(false));

    let path = written_path(&ctx);
    let opp = reconstruct(&path);
    assert_eq!(opp.url.as_deref(), Some("https://acme.example.com"));
    assert_eq!(opp.links, vec!["https://acme.example.com/news".to_string()]);
}

#[tokio::test]
async fn prospecting_mode_lifts_sources_and_merges_per_lead_contacts_in_order() {
    let tmp = tempfile::tempdir().expect("tempdir");
    opportunities_dir(tmp.path());

    let ctx = run_prospecting(
        tmp.path(),
        prospecting_event(),
        prospecting_result_with_contacts(),
    )
    .await;

    assert_eq!(
        ctx.node_runs["MaterializeDocNode"].status,
        NodeRunStatus::Success
    );
    assert_eq!(
        ctx.node_runs["MergeContactsNode"].status,
        NodeRunStatus::Success
    );
    let merge_result = &ctx.nodes["MergeContactsNode"];
    assert_eq!(merge_result["merged"], json!(true));
    assert_eq!(merge_result["contacts"], json!(2));

    let path = written_path(&ctx);
    let opp = reconstruct(&path);

    assert_eq!(
        opp.links,
        vec!["https://reddit.com/r/legaltech".to_string()]
    );
    assert_eq!(opp.contacts.len(), 2, "both leads' contacts land, in order");

    // Lead one's contact is a generic, name-less channel — recorded, not
    // discarded.
    assert_eq!(opp.contacts[0].name, "");
    assert_eq!(
        opp.contacts[0].whatsapp,
        vec!["+55 11 90000-0000".to_string()]
    );

    assert_eq!(opp.contacts[1].name, "Bob Smith");
    assert_eq!(
        opp.contacts[1].emails,
        vec!["bob@leadtwo.example.com".to_string()]
    );
}

#[tokio::test]
async fn prospecting_mode_zero_contacts_writes_the_doc_and_skips_the_merge() {
    let tmp = tempfile::tempdir().expect("tempdir");
    opportunities_dir(tmp.path());

    let zero_contacts_result = json!({
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
            },
        ],
    });

    let ctx = run_prospecting(tmp.path(), prospecting_event(), zero_contacts_result).await;

    let merge_result = &ctx.nodes["MergeContactsNode"];
    assert_eq!(merge_result["merged"], json!(false));
    assert_eq!(merge_result["contacts"], json!(0));

    let path = written_path(&ctx);
    let opp = reconstruct(&path);
    assert!(opp.contacts.is_empty());
}

/// A second, later contact merge for a name already on file must union
/// channels rather than append a duplicate — proving the merge step goes
/// through mev's real `plan_merge_contacts`, not a local reimplementation.
///
/// The first write comes from a full `RESEARCH_AGENT` `Workflow::run`
/// (real `MaterializeDocNode` + `MergeContactsNode`, per this suite's
/// pattern). Note the second merge is driven directly against the same
/// terminal `MergeContactsNode` node (still the real, `mev`-backed
/// materializer, over the same tempdir corpus) rather than through a second
/// full `RESEARCH_AGENT` run: `Opportunity::from_company_brief` embeds the
/// *whole* upstream brief verbatim as `research_brief` (by design — "the
/// raw JSON is embedded verbatim... nothing in the shape is re-derived"),
/// so any brief that differs enough to carry a new email also changes
/// `research_brief`, which would make `MaterializeDocNode`'s re-ingest
/// reconcile (and reset) the whole frontmatter block — including
/// `contacts` — as a fresh ingest, the same way it would reset a
/// stage a human had since moved on `OPPORTUNITY_SET_STAGE`. That full-
/// reingest interaction is out of this block's scope (`MaterializeDocNode`
/// / `okf-core`'s mapping is `EN.7.A`/`EN.4.E` task 1 territory, not task
/// 9's). Driving the merge step alone isolates exactly what this
/// criterion asks for: the union policy itself, exercised through the real
/// node and the real mev planner.
#[tokio::test]
async fn a_second_merge_with_a_shared_name_and_a_new_email_unions_channels_via_mevs_planner() {
    let tmp = tempfile::tempdir().expect("tempdir");
    opportunities_dir(tmp.path());

    run_company(tmp.path(), company_event(), company_brief_with_contact()).await;

    let node =
        engine_core::nodes::merge_contacts::MergeContactsNode::new().with_brain_root(tmp.path());
    let ctx = TaskContext {
        event: company_brief_with_contact_second_email(),
        nodes: HashMap::new(),
        metadata: json!({}),
        node_runs: HashMap::new(),
    };
    let ctx = node
        .process(ctx)
        .await
        .expect("second merge should succeed");

    let merge_result = &ctx.nodes["MergeContactsNode"];
    assert_eq!(
        merge_result["merged"],
        json!(true),
        "a new email for an existing name is still a merge, not a no-op"
    );

    let path = tmp.path().join("business/docs/opportunities/acme-corp.md");
    let opp = reconstruct(&path);

    assert_eq!(
        opp.contacts.len(),
        1,
        "a shared name must union channels onto one contact, never append a duplicate"
    );
    let contact = &opp.contacts[0];
    assert_eq!(contact.name, "Jane Doe");
    assert_eq!(
        contact.emails,
        vec![
            "jane@acme.example.com".to_string(),
            "jane.doe@acme.example.com".to_string(),
        ],
        "existing + new email are unioned, order-stable"
    );
    // `role` was set on the first run and left empty on the second's
    // incoming contact — the existing enriched value must survive.
    assert_eq!(contact.role, "Head of Ops");
}

#[tokio::test]
async fn both_emitted_shapes_still_classify_through_mevs_detect_kind_via_plan_ingest() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    opportunities_dir(root);

    let company_plan =
        mev::doc::opportunity::plan_ingest(&company_brief_with_contact(), None, root);
    assert!(
        company_plan
            .diagnostics
            .iter()
            .all(|d| d.severity != mev::Severity::Error),
        "company brief shape must still auto-detect through plan_ingest: {:?}",
        company_plan.diagnostics
    );

    let prospecting_plan =
        mev::doc::opportunity::plan_ingest(&prospecting_result_with_contacts(), None, root);
    assert!(
        prospecting_plan
            .diagnostics
            .iter()
            .all(|d| d.severity != mev::Severity::Error),
        "prospecting result shape must still auto-detect through plan_ingest: {:?}",
        prospecting_plan.diagnostics
    );
}
