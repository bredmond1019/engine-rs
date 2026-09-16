//! Hermetic end-to-end suite for `PLAN_AUTHORING` (`EN.19.B` task 6) — the
//! real `Workflow::run` pointer-walk over `plan_authoring::{schema, registry}`'s
//! declared graph, with a stubbed `DecomposePlanNode` transport (no real
//! `claude` spawn) and a real filesystem pointed at a `tempfile::tempdir()`
//! brain root. Covers the block record's remaining acceptance criteria:
//!
//! - (a) a fixture dispatch produces `plan.md` plus `candidate-blocks/<ID>.json`
//!   files that pass `.claude/workflows/block.schema.json`'s required-field
//!   contract;
//! - (b) a repeat dispatch against the same slug short-circuits at
//!   `CheckExistingPlanNode` with the stubbed transport never called again;
//! - (c) `force_regenerate: true` proceeds through the full graph and
//!   overwrites;
//! - (d) the no-`mev`/no-`state.json` grep from task 3's `stage_candidate_blocks.rs`
//!   is re-asserted here via `std::process::Command`, so it runs as part of
//!   the gated `cargo nextest` suite rather than only a shell
//!   `validation_command`.
//!
//! Every test writes only under its own `tempfile::tempdir()` — nothing under
//! the real `agentic-portfolio` corpus is ever touched.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use claude_code_rs::{Config, Outcome};
use engine_contract::TaskContext;
use engine_core::node::NodeRegistry;
use engine_core::workflow::Workflow;
use engine_core::workflows::llm_node::TransportSlotted;
use engine_core::workflows::plan_authoring::{
    check_existing::CheckExistingPlanNode, decompose::DecomposePlanNode,
    gather_context::GatherPlanContextNode, schema,
    stage_candidate_blocks::StageCandidateBlocksNode, write_narrative::WritePlanNarrativeNode,
};
use engine_core::workflows::ModelTransport;
use futures::FutureExt;
use serde_json::{json, Value};

const BRAIN_TOML: &str = r#"
[[repos]]
slug = "engine-rs"
prefix = "EN"
tier = "core"
repo_path = "."
"#;

/// The fields `.claude/workflows/block.schema.json` requires on every
/// `kind: "block"` record — kept in sync by hand with that schema, per this
/// suite's own acceptance criterion ("candidate-blocks/ JSON files that pass
/// block.schema.json validation").
const REQUIRED_BLOCK_SCHEMA_FIELDS: [&str; 15] = [
    "id",
    "repo",
    "kind",
    "title",
    "description",
    "what",
    "why",
    "sdlc_workflow",
    "model",
    "files",
    "out_of_scope",
    "acceptance_criteria",
    "spec_dir",
    "created",
    "updated",
];

fn stub_outcome(structured: Value) -> Outcome {
    Outcome {
        cost_usd: 0.0,
        usage: claude_code_rs::parse::Usage {
            input_tokens: 1,
            output_tokens: 1,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
        model_usage: Default::default(),
        text: serde_json::to_string(&structured).unwrap(),
        is_error: false,
        api_error_status: None,
        session_id: None,
        structured_output: Some(structured),
    }
}

/// A stub `ModelTransport` that records how many times it was called via
/// `call_count`, so a short-circuited run can be asserted to have made
/// zero model calls.
fn counting_stub_transport(structured: Value, call_count: Arc<AtomicUsize>) -> ModelTransport {
    Arc::new(move |_config: Config, _prompt: String| {
        let structured = structured.clone();
        let call_count = call_count.clone();
        async move {
            call_count.fetch_add(1, Ordering::SeqCst);
            Ok(stub_outcome(structured))
        }
        .boxed()
    })
}

fn one_full_candidate() -> Value {
    json!({
        "candidates": [
            {
                "title": "Fixture candidate block",
                "description": "A candidate produced by the plan_authoring e2e fixture.",
                "what": "Does the thing.",
                "why": "Because the fixture needs it.",
                "files": ["crates/engine-core/src/fixture.rs"],
                "out_of_scope": ["Everything else."],
                "acceptance_criteria": ["cargo nextest run -p engine-core --lib passes"],
            }
        ]
    })
}

/// Build a fresh `NodeRegistry` for `PLAN_AUTHORING`'s declared graph, real
/// filesystem, real writer, both pinned at `brain_root`/`repo_root`, and the
/// given stub transport for `DecomposePlanNode`.
fn registry_at(brain_root: &Path, repo_root: &Path, transport: ModelTransport) -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(
        CheckExistingPlanNode::new().with_brain_root(brain_root.to_path_buf()),
    ));
    registry.register(Box::new(
        GatherPlanContextNode::new()
            .with_brain_root(brain_root.to_path_buf())
            .with_repo_root(repo_root.to_path_buf()),
    ));
    registry.register(Box::new(DecomposePlanNode::new().with_transport(transport)));
    registry.register(Box::new(
        StageCandidateBlocksNode::new()
            .with_brain_root(brain_root.to_path_buf())
            .with_repo_root(repo_root.to_path_buf()),
    ));
    registry.register(Box::new(
        WritePlanNarrativeNode::new().with_brain_root(brain_root.to_path_buf()),
    ));
    registry
}

async fn run_at(
    brain_root: &Path,
    repo_root: &Path,
    transport: ModelTransport,
    event: Value,
) -> TaskContext {
    Workflow::new_validated(registry_at(brain_root, repo_root, transport), schema())
        .expect("declared PLAN_AUTHORING graph should validate")
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("PLAN_AUTHORING run should complete")
}

/// Set up a fresh brain root: `brain.toml` at its root and a
/// `sequence.md` fixture under this slug's pre-plan folder (D87 shape). The
/// repo root is the brain root itself (`repo_path = "."`), matching
/// `BRAIN_TOML`.
fn seed_brain_root(brain_root: &Path, slug: &str) {
    std::fs::write(brain_root.join("brain.toml"), BRAIN_TOML).expect("write brain.toml");
    let pre_plan_dir = brain_root
        .join("planning")
        .join("open-work")
        .join("pre-plan")
        .join(slug);
    std::fs::create_dir_all(&pre_plan_dir).expect("create pre-plan dir");
    std::fs::write(
        pre_plan_dir.join("sequence.md"),
        "# Sequence\n\nOne block: ship the fixture.\n",
    )
    .expect("write sequence.md fixture");
}

fn validate_against_block_schema(record: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    for field in REQUIRED_BLOCK_SCHEMA_FIELDS {
        if record.get(field).map(Value::is_null).unwrap_or(true) {
            errors.push(format!("missing required field `{field}`"));
        }
    }
    errors
}

#[tokio::test]
async fn dispatch_produces_plan_md_and_schema_valid_candidate_blocks() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let brain_root = tmp.path().to_path_buf();
    let slug = "fixture-slug";
    seed_brain_root(&brain_root, slug);

    let call_count = Arc::new(AtomicUsize::new(0));
    let transport = counting_stub_transport(one_full_candidate(), call_count.clone());

    let ctx = run_at(&brain_root, &brain_root, transport, json!({ "slug": slug })).await;

    assert_eq!(
        call_count.load(Ordering::SeqCst),
        1,
        "one model call expected"
    );

    let plan_path = ctx.nodes["WritePlanNarrativeNode"]["plan_path"]
        .as_str()
        .expect("plan_path stamped");
    let plan_contents = std::fs::read_to_string(plan_path).expect("plan.md should exist");
    assert!(
        plan_contents.starts_with("---\n"),
        "plan.md must open with OKF YAML frontmatter"
    );
    assert!(plan_contents.contains("related:"));
    assert!(
        !plan_contents.contains("related: []"),
        "plan.md must never emit an empty related: list"
    );

    let candidate_blocks_dir = brain_root
        .join("planning")
        .join("open-work")
        .join("pre-plan")
        .join(slug)
        .join("candidate-blocks");
    let entries: Vec<PathBuf> = std::fs::read_dir(&candidate_blocks_dir)
        .expect("candidate-blocks dir should exist")
        .map(|e| e.expect("dir entry").path())
        .collect();
    assert_eq!(entries.len(), 1, "one candidate block should be staged");

    let contents = std::fs::read_to_string(&entries[0]).expect("read candidate json");
    let record: Value = serde_json::from_str(&contents).expect("candidate must be valid JSON");
    let errors = validate_against_block_schema(&record);
    assert!(
        errors.is_empty(),
        "staged candidate must validate against block.schema.json's required fields: {errors:?}"
    );

    // The heading `WritePlanNarrativeNode` rendered must reference this exact
    // staged id — proves plan.md and candidate-blocks/ agree.
    let id = record["id"].as_str().expect("id present");
    assert!(
        plan_contents.contains(&format!("### {id} —")),
        "plan.md must contain a heading for staged candidate {id}"
    );
}

#[tokio::test]
async fn repeat_dispatch_short_circuits_with_zero_stubbed_transport_calls() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let brain_root = tmp.path().to_path_buf();
    let slug = "repeat-slug";
    seed_brain_root(&brain_root, slug);

    let call_count = Arc::new(AtomicUsize::new(0));
    let first_transport = counting_stub_transport(one_full_candidate(), call_count.clone());
    run_at(
        &brain_root,
        &brain_root,
        first_transport,
        json!({ "slug": slug }),
    )
    .await;
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        1,
        "first run makes one model call"
    );

    // Second dispatch against the same slug: plan.md + candidate-blocks/
    // already exist, so CheckExistingPlanNode must short-circuit before
    // DecomposePlanNode ever runs.
    let second_transport = counting_stub_transport(one_full_candidate(), call_count.clone());
    let ctx = run_at(
        &brain_root,
        &brain_root,
        second_transport,
        json!({ "slug": slug }),
    )
    .await;

    assert_eq!(
        call_count.load(Ordering::SeqCst),
        1,
        "repeat dispatch must short-circuit with zero additional model calls"
    );
    assert!(
        !ctx.nodes.contains_key("DecomposePlanNode"),
        "DecomposePlanNode must not have run on the short-circuited repeat dispatch"
    );
    let stored = &ctx.nodes["CheckExistingPlanNode"];
    assert_eq!(stored["short_circuit"], json!(true));
}

#[tokio::test]
async fn force_regenerate_proceeds_through_the_full_graph_and_overwrites() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let brain_root = tmp.path().to_path_buf();
    let slug = "force-regen-slug";
    seed_brain_root(&brain_root, slug);

    let call_count = Arc::new(AtomicUsize::new(0));
    let first_transport = counting_stub_transport(one_full_candidate(), call_count.clone());
    run_at(
        &brain_root,
        &brain_root,
        first_transport,
        json!({ "slug": slug }),
    )
    .await;
    assert_eq!(call_count.load(Ordering::SeqCst), 1);

    let mut second_candidate = one_full_candidate();
    second_candidate["candidates"][0]["title"] = json!("Regenerated fixture candidate block");
    let second_transport = counting_stub_transport(second_candidate, call_count.clone());
    let ctx = run_at(
        &brain_root,
        &brain_root,
        second_transport,
        json!({ "slug": slug, "force_regenerate": true }),
    )
    .await;

    assert_eq!(
        call_count.load(Ordering::SeqCst),
        2,
        "force_regenerate: true must call the model again"
    );
    assert!(
        ctx.nodes.contains_key("DecomposePlanNode"),
        "force_regenerate: true must proceed through DecomposePlanNode"
    );

    let plan_path = ctx.nodes["WritePlanNarrativeNode"]["plan_path"]
        .as_str()
        .expect("plan_path stamped");
    let plan_contents = std::fs::read_to_string(plan_path).expect("plan.md should exist");
    assert!(
        plan_contents.contains("Regenerated fixture candidate block"),
        "force_regenerate: true must overwrite plan.md with the fresh decomposition"
    );
}

/// Re-asserts task 3's own grep-verified scope boundary
/// (`grep -c 'mev' stage_candidate_blocks.rs` == 0, and the positive control
/// that the same grep against `sdlc_flow::emit_state` — which DOES touch
/// `state.json` — returns a hit) as part of the gated `cargo nextest` suite,
/// not only a one-off shell `validation_command`.
#[test]
fn stage_candidate_blocks_never_shells_or_references_mev_or_state_json() {
    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set by cargo");
    let stage_candidate_blocks_path =
        Path::new(&manifest_dir).join("src/workflows/plan_authoring/stage_candidate_blocks.rs");
    assert!(
        stage_candidate_blocks_path.is_file(),
        "expected {} to exist",
        stage_candidate_blocks_path.display()
    );

    let mev_count = Command::new("grep")
        .arg("-c")
        .arg("mev")
        .arg(&stage_candidate_blocks_path)
        .output()
        .expect("grep must run");
    // `grep -c` exits 1 (with stdout "0") when there are no matches — read
    // stdout rather than relying on the exit code.
    let mev_count_str = String::from_utf8_lossy(&mev_count.stdout);
    let mev_count: usize = mev_count_str
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("grep -c 'mev' produced non-numeric output: {mev_count_str:?}"));
    assert_eq!(
        mev_count, 0,
        "stage_candidate_blocks.rs must never reference `mev` (out_of_scope: never registers)"
    );

    let state_json_count = Command::new("grep")
        .arg("-c")
        .arg("state.json")
        .arg(&stage_candidate_blocks_path)
        .output()
        .expect("grep must run");
    let state_json_count_str = String::from_utf8_lossy(&state_json_count.stdout);
    let state_json_count: usize = state_json_count_str.trim().parse().unwrap_or_else(|_| {
        panic!("grep -c 'state.json' produced non-numeric output: {state_json_count_str:?}")
    });
    assert_eq!(
        state_json_count, 0,
        "stage_candidate_blocks.rs must never reference state.json"
    );

    // Positive control: the identical grep form against a file that DOES
    // touch state.json must return a non-zero hit — proving this grep is
    // capable of finding what it is supposed to find (CLAUDE.md standing
    // rule 11 / this fleet's `check-blast-radius` discipline).
    let emit_state_path = Path::new(&manifest_dir).join("src/workflows/sdlc_flow/emit_state.rs");
    assert!(
        emit_state_path.is_file(),
        "expected positive-control file {} to exist",
        emit_state_path.display()
    );
    let control_count = Command::new("grep")
        .arg("-c")
        .arg("state.json")
        .arg(&emit_state_path)
        .output()
        .expect("grep must run");
    let control_count_str = String::from_utf8_lossy(&control_count.stdout);
    let control_count: usize = control_count_str.trim().parse().unwrap_or_else(|_| {
        panic!("positive-control grep produced non-numeric output: {control_count_str:?}")
    });
    assert!(
        control_count > 0,
        "positive control failed: sdlc_flow/emit_state.rs must contain a `state.json` reference \
         or this grep form cannot be trusted to find anything"
    );
}
