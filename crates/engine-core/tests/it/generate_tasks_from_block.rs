//! Integration suite for `EN.19.C` task 3 — the block-record-aware branch of
//! `GenerateTasksNode` (`EN.19.C` task 2), exercised through this crate's
//! public API rather than `setup.rs`'s own `#[cfg(test)]` module.
//!
//! Covers task 3's five acceptance criteria:
//! - (a) disjoint-or-sequential file ownership, modeled on EN.19.A/EN.19.B's
//!   own real shared files (`http.rs`/`README.md`/`harness.json`), plus a
//!   deliberately-broken fixture proving the checker actually detects the
//!   defect it exists to catch.
//! - (b) a near-miss `spec_slug` falls through to the existing
//!   planning-fallback path, byte-identical to a fixed golden output.
//! - (c) `model_tiers.generate_from_block` moves only the block-record
//!   path's resolved model; the fallback path is unaffected, both
//!   directions.
//! - (d) every generated task's `acceptance_criteria` round-trips into a
//!   real `validation_commands` entry.
//! - (e) a real `ORCHESTRATION` `execute_step` dispatch against a fixture
//!   block with a registered `planning/blocks/<ID>.json` and no
//!   `tasks.json` routes `SpecExistsRouterNode -> GenerateTasksNode` and
//!   produces a block-record-aware `tasks.json`, with no change to
//!   `orchestration/*.rs`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use claude_code_rs::Outcome;
use engine_contract::TaskContext;
use engine_core::node::{Node, NodeError, NodeRegistry};
use engine_core::policy::emit_state::EmitStateNode as GenericEmitStateNode;
use engine_core::policy::permission::PermissionProfile;
use engine_core::policy::PolicyConfigSource;
use engine_core::repo_registry::RepoRegistry;
use engine_core::workflow::Workflow;
use engine_core::workflows::llm_node::TransportSlotted as LlmTransportSlotted;
use engine_core::workflows::orchestration::chain::{ChainStep, StepKind};
use engine_core::workflows::orchestration::execute::{execute_step, EngineKind, FlowFuture};
use engine_core::workflows::sdlc_flow::close_block::CloseBlockNode;
use engine_core::workflows::sdlc_flow::final_validation::{FinalValidationNode, ValidationScope};
use engine_core::workflows::sdlc_flow::policy::{ModelTier, SdlcPolicy};
use engine_core::workflows::sdlc_flow::setup::{
    CommandOutput, CommandRunner, GenerateTasksNode, LoadTaskStateNode, SpecExistsRouterNode,
    RESOLVED_POLICY_IDENTITY,
};
use engine_core::workflows::sdlc_flow::task_loop::{
    ImplementTaskNode, IncrementAttemptNode, SaveStateNode, TaskQueueRouterNode, TestTaskNode,
    TriageTaskNode, UpdateTaskStatusNode,
};
use engine_core::workflows::sdlc_task::graph as sdlc_task_graph;
use engine_core::workflows::sdlc_task::lean_bookkeep::LeanBookkeepNode;
use engine_core::workflows::sdlc_task::profiles::resolve_policy_for_run_from;
use engine_core::workflows::sdlc_task::task_triage_router::TaskTriageRouterNode;
use engine_core::workflows::sdlc_task::DEFAULT_STATE_FILENAME;
use engine_core::workflows::ModelTransport;
use serde_json::json;

// ── Shared fixtures ─────────────────────────────────────────────────────

static TMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn temp_dir() -> PathBuf {
    let n = TMP_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "engine-core-generate-tasks-from-block-{}-{n}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn stub_outcome_with_text(text: &str) -> Outcome {
    Outcome {
        cost_usd: 0.0,
        usage: claude_code_rs::parse::Usage {
            input_tokens: 1,
            output_tokens: 1,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
        model_usage: std::collections::BTreeMap::new(),
        text: text.to_string(),
        is_error: false,
        api_error_status: None,
        session_id: None,
        structured_output: None,
    }
}

fn write_block_record(root: &Path, spec_slug: &str, record: serde_json::Value) {
    let blocks_dir = root.join("planning").join("blocks");
    std::fs::create_dir_all(&blocks_dir).unwrap();
    std::fs::write(
        blocks_dir.join(format!("{spec_slug}.json")),
        serde_json::to_string_pretty(&record).unwrap(),
    )
    .unwrap();
}

fn minimal_block_record(what: &str) -> serde_json::Value {
    json!({
        "what": what,
        "files": {
            "new": [],
            "modified": [
                { "path": "crates/engine-serve/src/http.rs", "change": "wires in the new route" },
                { "path": "README.md", "change": "documents the new route" },
                { "path": "planning/harness.json", "change": "adds the new gate" },
            ],
        },
        "acceptance_criteria": ["the new route exists and is documented"],
        "out_of_scope": [],
        "interfaces": [],
    })
}

fn ctx_with_worktree_and_policy(
    spec_slug: &str,
    worktree: &Path,
    policy: &SdlcPolicy,
) -> TaskContext {
    let mut ctx = TaskContext {
        event: json!({ "spec_slug": spec_slug }),
        nodes: std::collections::HashMap::new(),
        metadata: json!({}),
        node_runs: std::collections::HashMap::new(),
    };
    ctx.nodes.insert(
        "SetupWorktreeNode".to_string(),
        json!({ "worktree_path": worktree.to_string_lossy() }),
    );
    ctx.nodes.insert(
        RESOLVED_POLICY_IDENTITY.to_string(),
        serde_json::to_value(policy).expect("policy serializes"),
    );
    ctx
}

fn ctx_with_worktree(spec_slug: &str, worktree: &Path) -> TaskContext {
    ctx_with_worktree_and_policy(spec_slug, worktree, &SdlcPolicy::default())
}

/// A transport that records every `(Config, prompt)` it was called with and
/// replies with `canned`.
fn recording_transport(
    captured: Arc<Mutex<Option<(claude_code_rs::Config, String)>>>,
    canned: String,
) -> ModelTransport {
    Arc::new(move |config, prompt| {
        *captured.lock().unwrap() = Some((config.clone(), prompt.clone()));
        let outcome = stub_outcome_with_text(&canned);
        Box::pin(async move { Ok(outcome) })
    })
}

// ── (a) disjoint-or-sequential file ownership ───────────────────────────

/// Checks task 3's own rule directly against the RAW model-output task
/// shape (`files: Vec<String>`, `dependsOn: Vec<u32>`) — the shape the
/// model actually replies with, per `generate_tasks_from_block.md`'s
/// prompt. This is deliberately checked against the raw JSON the transport
/// returns, not the round-tripped `tasks.json` on disk: `SDLCTask` (the
/// runtime struct `GenerateTasksNode` persists) carries no `dependsOn`
/// field at all, so that edge cannot survive a serialize/deserialize
/// round-trip through it — verifying the RULE (not the unrelated,
/// pre-existing gap in what `SDLCTask` persists) means checking it at the
/// point the model's own JSON shape actually carries it.
///
/// Returns `Err` naming the first offending pair on a violation: two tasks
/// with no `dependsOn` edge between them (in either direction) sharing at
/// least one file.
fn check_disjoint_or_sequential(tasks: &[serde_json::Value]) -> Result<(), String> {
    let extract = |t: &serde_json::Value| -> (u64, Vec<String>, Vec<u64>) {
        let id = t["task_id"].as_u64().unwrap_or(0);
        let files: Vec<String> = t["files"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let depends_on: Vec<u64> = t["dependsOn"]
            .as_array()
            .map(|arr| arr.iter().filter_map(serde_json::Value::as_u64).collect())
            .unwrap_or_default();
        (id, files, depends_on)
    };

    let parsed: Vec<(u64, Vec<String>, Vec<u64>)> = tasks.iter().map(extract).collect();

    for (i, (id_a, files_a, deps_a)) in parsed.iter().enumerate() {
        for (id_b, files_b, deps_b) in parsed.iter().skip(i + 1) {
            let shares_a_file = files_a.iter().any(|f| files_b.contains(f));
            if !shares_a_file {
                continue;
            }
            let linked = deps_a.contains(id_b) || deps_b.contains(id_a);
            if !linked {
                return Err(format!(
                    "task {id_a} and task {id_b} share a file with no dependsOn edge \
                     between them"
                ));
            }
        }
    }
    Ok(())
}

/// A fixture modeled on EN.19.A/EN.19.B's own real overlap: two pairs of
/// tasks share a file each, but every sharing pair carries an explicit
/// `dependsOn` edge — the compliant shape.
fn compliant_tasks_fixture() -> Vec<serde_json::Value> {
    vec![
        json!({
            "task_id": 1, "title": "Add the http.rs route", "description": "desc",
            "files": ["crates/engine-serve/src/http.rs"], "dependsOn": [],
        }),
        json!({
            "task_id": 2, "title": "Document the README section", "description": "desc",
            "files": ["README.md"], "dependsOn": [],
        }),
        json!({
            "task_id": 3, "title": "Wire the harness gate", "description": "desc",
            "files": ["crates/engine-serve/src/http.rs", "planning/harness.json"],
            "dependsOn": [1],
        }),
        json!({
            "task_id": 4, "title": "Finish the README section", "description": "desc",
            "files": ["README.md"], "dependsOn": [2],
        }),
    ]
}

/// Same shape, but task 4 shares `README.md` with task 2 with NO
/// `dependsOn` edge — the defect the checker must catch.
fn broken_tasks_fixture() -> Vec<serde_json::Value> {
    vec![
        json!({
            "task_id": 1, "title": "Add the http.rs route", "description": "desc",
            "files": ["crates/engine-serve/src/http.rs"], "dependsOn": [],
        }),
        json!({
            "task_id": 2, "title": "Document the README section", "description": "desc",
            "files": ["README.md"], "dependsOn": [],
        }),
        json!({
            "task_id": 4, "title": "Finish the README section (racing)", "description": "desc",
            "files": ["README.md"], "dependsOn": [],
        }),
    ]
}

/// AC1 (task 3): the checker PASSES a compliant fixture and the pipeline
/// itself (`GenerateTasksNode`, driven exactly as a real
/// block-record-aware run would) writes every one of those tasks to disk.
#[tokio::test]
async fn compliant_shared_files_fixture_passes_and_persists() {
    check_disjoint_or_sequential(&compliant_tasks_fixture())
        .expect("compliant fixture must satisfy the disjoint-or-sequential rule");

    let worktree = temp_dir();
    std::fs::create_dir_all(worktree.join("planning").join("my-spec")).unwrap();
    write_block_record(
        &worktree,
        "my-spec",
        minimal_block_record("Add the new route"),
    );

    let canned = json!({
        "tasks": compliant_tasks_fixture(),
        "tasks_markdown": "# Tasks\n\n1-4",
    })
    .to_string();

    let node = GenerateTasksNode::new().with_transport(Arc::new(move |_config, _prompt| {
        let outcome = stub_outcome_with_text(&canned);
        Box::pin(async move { Ok(outcome) })
    }));
    let ctx = ctx_with_worktree("my-spec", &worktree);
    node.process(ctx).await.expect("generate should succeed");

    let dir = worktree.join("planning").join("my-spec");
    let tasks: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(dir.join("tasks.json")).unwrap()).unwrap();
    assert_eq!(tasks.len(), 4, "every compliant task must be persisted");
}

/// AC1 (task 3, the meta-test): the checker actually DETECTS the defect it
/// exists to catch — a deliberately-broken fixture where two
/// concurrently-runnable tasks share a file with no `dependsOn` edge.
#[test]
fn broken_shared_files_fixture_is_flagged_by_the_checker() {
    let err = check_disjoint_or_sequential(&broken_tasks_fixture())
        .expect_err("broken fixture must be flagged, proving the checker can detect the defect");
    assert!(err.contains("task 2") && err.contains("task 4"));
}

// ── (b) near-miss slug falls through to the fallback, golden output ─────

/// AC2 (task 3): a `spec_slug` that does not exactly match any
/// `planning/blocks/<ID>.json` — including a one-character-different near
/// miss — produces byte-identical fallback output to a fixed golden
/// fixture, at the public-API integration level (not just `setup.rs`'s own
/// unit test).
#[tokio::test]
async fn near_miss_slug_falls_through_to_byte_identical_golden_fallback() {
    let worktree = temp_dir();
    std::fs::create_dir_all(worktree.join("planning").join("my-spec")).unwrap();
    // Near miss: trailing character differs from the real spec_slug.
    write_block_record(
        &worktree,
        "my-specx",
        minimal_block_record("Add the new route"),
    );

    let canned = json!({
        "tasks": [{ "task_id": 1, "title": "Fallback task", "description": "desc" }],
        "tasks_markdown": "# Tasks\n\n1. Fallback task",
    })
    .to_string();

    let captured = Arc::new(Mutex::new(None));
    let node =
        GenerateTasksNode::new().with_transport(recording_transport(captured.clone(), canned));
    let ctx = ctx_with_worktree("my-spec", &worktree);
    node.process(ctx).await.expect("generate should succeed");

    let (_, prompt) = captured.lock().unwrap().clone().expect("transport called");
    assert!(
        prompt.starts_with("Generate the task list for spec"),
        "fallback prompt must be unchanged, got: {prompt}"
    );
    assert!(!prompt.contains("decomposing an already-authored block record"));

    let dir = worktree.join("planning").join("my-spec");
    let tasks_json = std::fs::read_to_string(dir.join("tasks.json")).unwrap();
    // Golden fixture rendered through `SDLCTask` itself (via `serde_json::to_string_pretty`,
    // the same serializer `GenerateTasksNode` uses) rather than a hand-typed JSON literal, so
    // this assertion is a byte-for-byte pin on the field ORDER `#[derive(Serialize)]` emits,
    // not a coincidental match to whatever key order a `json!` macro literal happens to produce.
    let golden_task =
        engine_core::workflows::sdlc_flow::schema::SDLCTask::new(1, "Fallback task", "desc");
    let golden = serde_json::to_string_pretty(&vec![golden_task]).unwrap();
    assert_eq!(
        tasks_json, golden,
        "fallback tasks.json must be byte-identical to the golden fixture"
    );
}

// ── (c) tier isolation, both directions ─────────────────────────────────

/// AC3 (task 3): tuning `model_tiers.generate_from_block` moves only the
/// block-record path's resolved model; the fallback path's resolved model,
/// under the very same policy, is unchanged — both directions asserted.
#[tokio::test]
async fn generate_from_block_tier_moves_only_the_block_record_path() {
    let mut tiered_policy = SdlcPolicy::default();
    tiered_policy.model_tiers.generate_from_block = ModelTier::Haiku;
    assert_ne!(
        tiered_policy.model_tiers.generate_from_block, tiered_policy.model_tiers.generate,
        "fixture must actually diverge the two knobs"
    );

    let canned = json!({
        "tasks": [{ "task_id": 1, "title": "Do it", "description": "desc" }],
        "tasks_markdown": "# Tasks\n\n1. Do it",
    })
    .to_string();

    // Block-record branch: resolves through generate_from_block (Haiku).
    let worktree = temp_dir();
    std::fs::create_dir_all(worktree.join("planning").join("my-spec")).unwrap();
    write_block_record(
        &worktree,
        "my-spec",
        minimal_block_record("Add the new route"),
    );
    let captured = Arc::new(Mutex::new(None));
    let node = GenerateTasksNode::new()
        .with_transport(recording_transport(captured.clone(), canned.clone()));
    let ctx = ctx_with_worktree_and_policy("my-spec", &worktree, &tiered_policy);
    node.process(ctx).await.expect("generate should succeed");
    let (config, _) = captured.lock().unwrap().clone().expect("transport called");
    assert_eq!(config.model, Some("claude-haiku-4-5".to_string()));

    // Fallback branch, same tuned policy: resolved model is unaffected.
    let worktree2 = temp_dir();
    std::fs::create_dir_all(worktree2.join("planning").join("other-spec")).unwrap();
    let captured2 = Arc::new(Mutex::new(None));
    let node2 =
        GenerateTasksNode::new().with_transport(recording_transport(captured2.clone(), canned));
    let ctx2 = ctx_with_worktree_and_policy("other-spec", &worktree2, &tiered_policy);
    node2.process(ctx2).await.expect("generate should succeed");
    let (config2, prompt2) = captured2.lock().unwrap().clone().expect("transport called");
    assert!(prompt2.starts_with("Generate the task list for spec"));
    assert_eq!(
        config2.model,
        Some(engine_core::policy::model_tier_to_model_string(
            tiered_policy.model_tiers.generate,
            &tiered_policy.local.model
        ))
    );
}

// ── (d) acceptance_criteria round-trips into validation_commands ────────

/// AC4 (task 3): every generated task's `acceptance_criteria` has a
/// matching `validation_commands` entry — not left as bare prose.
#[tokio::test]
async fn acceptance_criteria_round_trips_into_validation_commands() {
    let worktree = temp_dir();
    std::fs::create_dir_all(worktree.join("planning").join("my-spec")).unwrap();
    write_block_record(
        &worktree,
        "my-spec",
        minimal_block_record("Add the new route"),
    );

    let canned = json!({
        "tasks": [
            {
                "task_id": 1,
                "title": "Add the route",
                "description": "desc",
                "acceptance_criteria": ["the route exists", "the route is tested"],
                "validation_commands": ["cargo build", "cargo nextest run -p engine-serve"],
            },
        ],
        "tasks_markdown": "# Tasks\n\n1. Add the route",
    })
    .to_string();

    let node = GenerateTasksNode::new().with_transport(Arc::new(move |_config, _prompt| {
        let outcome = stub_outcome_with_text(&canned);
        Box::pin(async move { Ok(outcome) })
    }));
    let ctx = ctx_with_worktree("my-spec", &worktree);
    node.process(ctx).await.expect("generate should succeed");

    let dir = worktree.join("planning").join("my-spec");
    let tasks: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(dir.join("tasks.json")).unwrap()).unwrap();
    let task = &tasks[0];
    let acceptance = task["acceptance_criteria"].as_array().unwrap();
    let validation = task["validation_commands"].as_array().unwrap();
    assert_eq!(
        acceptance.len(),
        validation.len(),
        "every acceptance criterion must round-trip into a real validation_commands entry, \
         not be left as bare prose with nothing to gate it: acceptance={acceptance:?} \
         validation={validation:?}"
    );
    assert!(validation
        .iter()
        .all(|v| !v.as_str().unwrap_or("").is_empty()));
}

// ── (e) real ORCHESTRATION execute_step dispatch ────────────────────────

const ORCH_SPEC_SLUG: &str = "orch-fixture-spec";

/// Replaces the real `SetupWorktreeNode` inside the child `SDLC_TASK`
/// workflow this test's `FlowRunner` assembles — mirrors
/// `sdlc_task_e2e.rs::FixtureSetupNode` exactly, writing the invocation's
/// own resolved `repo_path` as the worktree so the run operates on the real
/// fixture repo `execute_step` resolved through the `RepoRegistry`, rather
/// than a second, disconnected temp directory.
struct FixtureSetupNode {
    worktree_path: String,
}

#[async_trait::async_trait]
impl Node for FixtureSetupNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({
                "worktree_path": self.worktree_path,
                "branch_name": format!("task/{ORCH_SPEC_SLUG}"),
            }),
        );
        let source = PolicyConfigSource::Worktree(PathBuf::from(&self.worktree_path));
        let resolved_task_policy = resolve_policy_for_run_from(&ctx, &source)?;
        let resolved_policy = resolved_task_policy.to_sdlc_policy();
        engine_core::policy::stamp_resolved_policy(&mut ctx, &resolved_policy)?;
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "SetupWorktreeNode"
    }
}

fn always_pass_runner() -> CommandRunner {
    Arc::new(|program, args, _cwd| {
        if program == "git" && args.first() == Some(&"status") {
            return Ok(CommandOutput {
                status: 0,
                stdout: " M src/lib.rs\n".to_string(),
                stderr: String::new(),
            });
        }
        Ok(CommandOutput {
            status: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    })
}

/// Builds the full assembled `SDLC_TASK` `Workflow` used as this test's
/// `FlowRunner` seam — the real declared graph from `sdlc_task_graph`,
/// every model/subprocess node stubbed, mirroring
/// `sdlc_task_e2e.rs::build_workflow` (that suite's own established
/// pattern for a hermetic `SDLC_TASK` run) rather than reimplementing new
/// orchestration scaffolding.
fn build_stubbed_sdlc_task_workflow(worktree: &Path) -> Workflow {
    let mut registry = NodeRegistry::new();

    registry.register(Box::new(FixtureSetupNode {
        worktree_path: worktree.to_string_lossy().to_string(),
    }));
    registry.register(Box::new(
        SpecExistsRouterNode::new().with_state_filename(DEFAULT_STATE_FILENAME),
    ));
    // The node under test: NOT given a stubbed transport override here —
    // each call site below wraps it with a recording/canned transport so
    // the test can assert what path it took.
    registry.register(Box::new(GenerateTasksNode::new().with_transport(Arc::new(
        |_config, _prompt| {
            let outcome = stub_outcome_with_text(
                &json!({
                    "tasks": [{
                        "task_id": 1,
                        "title": "Add the route",
                        "description": "desc",
                        "acceptance_criteria": ["the route exists"],
                        "validation_commands": ["cargo build"],
                    }],
                    "tasks_markdown": "# Tasks\n\n1. Add the route",
                })
                .to_string(),
            );
            Box::pin(async move { Ok(outcome) })
        },
    ))));
    registry.register(Box::new(
        LoadTaskStateNode::new().with_state_filename(DEFAULT_STATE_FILENAME),
    ));
    registry.register(Box::new(TaskQueueRouterNode));

    registry.register(Box::new(ImplementTaskNode::new().with_transport(Arc::new(
        |_config, _prompt| {
            let outcome = stub_outcome_with_text(
                &json!({
                    "summary": "implemented",
                    "modified_files": ["src/lib.rs"],
                    "tests_added": ["it_works"],
                })
                .to_string(),
            );
            Box::pin(async move { Ok(outcome) })
        },
    ))));

    registry.register(Box::new(
        TestTaskNode::new().with_runner(always_pass_runner()),
    ));
    registry.register(Box::new(
        TriageTaskNode::new().with_runner(always_pass_runner()),
    ));
    registry.register(Box::new(TaskTriageRouterNode));

    registry.register(Box::new(UpdateTaskStatusNode));
    registry.register(Box::new(
        SaveStateNode::new()
            .with_runner(always_pass_runner())
            .with_state_filename(DEFAULT_STATE_FILENAME),
    ));
    registry.register(Box::new(IncrementAttemptNode));

    registry.register(Box::new(
        FinalValidationNode::new()
            .with_runner(always_pass_runner())
            .with_scope(ValidationScope::Reconcile),
    ));

    registry.register(Box::new(
        LeanBookkeepNode::new()
            .with_runner(always_pass_runner())
            .with_state_filename(DEFAULT_STATE_FILENAME),
    ));
    registry.register(Box::new(
        CloseBlockNode::new().with_state_source("LeanBookkeepNode"),
    ));
    registry.register(Box::new(GenericEmitStateNode::new(Arc::new(
        |_program: &str, _args: &[&str], _cwd: &Path| {
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        },
    ))));

    let schema = sdlc_task_graph::schema();
    Workflow::new_validated(registry, schema)
        .expect("SDLC_TASK declared graph must pass WorkflowValidator::validate")
}

/// A tempdir `brain.toml` + one real repo directory `repo-a`, mirroring
/// `orchestration.rs`'s own `two_repo_registry` fixture pattern (scaled to
/// one repo since this test dispatches a single step).
fn one_repo_registry() -> (tempfile::TempDir, RepoRegistry) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("repo-a")).unwrap();
    std::fs::write(
        dir.path().join("brain.toml"),
        "[[repos]]\nslug = \"repo-a\"\nrepo_path = \"repo-a\"\n",
    )
    .unwrap();
    let registry = RepoRegistry::from_brain_root(dir.path()).expect("registry");
    (dir, registry)
}

/// AC5 (task 3): a real `ORCHESTRATION` `execute_step` dispatch against a
/// fixture repo carrying a registered `planning/blocks/<ID>.json` and NO
/// `tasks.json` routes `SpecExistsRouterNode -> GenerateTasksNode` and
/// produces a block-record-aware `tasks.json` — proving the wiring
/// `EN.19.C`'s own block record already documents (`SpecExistsRouterNode`
/// already routes this way in both `sdlc_flow::graph` and
/// `sdlc_task::graph`) benefits from this block's extension with zero
/// change to `orchestration/*.rs`.
#[tokio::test]
async fn orchestration_execute_step_routes_to_generate_tasks_node_for_a_registered_block() {
    let (dir, registry) = one_repo_registry();
    let repo_a_path = dir.path().join("repo-a");

    std::fs::create_dir_all(repo_a_path.join("planning").join(ORCH_SPEC_SLUG)).unwrap();
    write_block_record(
        &repo_a_path,
        ORCH_SPEC_SLUG,
        minimal_block_record("Add the orchestration-dispatched route"),
    );
    // Deliberately no tasks.json / sdlc state file under
    // planning/<ORCH_SPEC_SLUG>/ — the exact precondition
    // SpecExistsRouterNode's doc comment names as "the legitimate planning
    // fallback" trigger, now block-record-aware per EN.19.C.

    let run_flow: engine_core::workflows::orchestration::execute::FlowRunner =
        Arc::new(move |invocation| -> FlowFuture {
            let workflow = build_stubbed_sdlc_task_workflow(&invocation.repo_path);
            let event = json!({ "spec_slug": invocation.block_id });
            Box::pin(async move { workflow.run(event, Box::new(|_ctx: &TaskContext| {})).await })
        });

    let step = ChainStep {
        repo: "repo-a".to_string(),
        block_id: ORCH_SPEC_SLUG.to_string(),
        kind: StepKind::Block,
        ..Default::default()
    };
    let resolve_engine = |_repo: &str, _id: &str| EngineKind::Task;

    let outcome = execute_step(
        &step,
        &resolve_engine,
        &registry,
        &run_flow,
        false,
        true,
        uuid::Uuid::new_v4(),
        None,
        None,
        PermissionProfile::Standard,
        None,
        None,
        None,
    )
    .await
    .expect("execute_step should dispatch and succeed");

    assert_eq!(outcome.engine, EngineKind::Task);

    let generate_run = outcome.ctx.node_runs.get("GenerateTasksNode").expect(
        "GenerateTasksNode must have run — SpecExistsRouterNode must route to it \
                 when no tasks.json/state file exists",
    );
    assert_eq!(
        generate_run.status,
        engine_contract::NodeRunStatus::Success,
        "GenerateTasksNode must have run to completion"
    );

    let tasks_json_path = repo_a_path
        .join("planning")
        .join(ORCH_SPEC_SLUG)
        .join("tasks.json");
    assert!(
        tasks_json_path.exists(),
        "a block-record-aware tasks.json must have been written by the dispatched run"
    );
    let tasks: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(&tasks_json_path).unwrap()).unwrap();
    assert_eq!(tasks[0]["title"], "Add the route");
}
