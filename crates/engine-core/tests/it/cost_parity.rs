//! `EN.17.F` — integration coverage for the cost-parity knobs task 7 ships:
//! child-policy forwarding (tasks 1-2), the final-attempt model escalation
//! (task 3), per-stage turn ceilings (task 4), and the planning-fallback
//! context cap (task 5) — plus the review-session-count claim the block's
//! own MEASUREMENT section rests on. Every test name is prefixed
//! `cost_parity_`, per the block's `testing_strategy`.
//!
//! # What this file can and cannot assert
//!
//! `sdlc_flow_event`/`sdlc_task_event` — the two functions that actually
//! compose the JSON `"policy"` key on a child event — are private to
//! `execute.rs`, which is NOT in this task's `files[]` (touching it is out
//! of scope: task 7 is tests/docs only). Their per-engine routing (a
//! `child_sdlc_task_policy` value never reaching an `EngineKind::Flow`
//! child's composed event, and vice versa) is already unit-tested in
//! `execute.rs` itself (`sdlc_flow_event_includes_policy_when_...`,
//! `sdlc_task_event_omits_the_policy_key_without_an_override`, and their
//! inverses). This file instead asserts at the two PUBLIC seams a caller
//! outside `execute.rs` actually has: the [`FlowInvocation`] `execute_step`
//! hands to a [`FlowRunner`] (both child-policy fields, forwarded
//! unconditionally regardless of engine — the raw material the private
//! routing consumes), and the four-layer `resolve_policy_for_run_from` each
//! engine's own policy module exposes (what a forwarded override actually
//! RESOLVES to, which is what every acceptance criterion here asks for:
//! "assert on the resolved policy, not the forwarded JSON").
//!
//! STANDING RULE 8: this file is a `mod` of `tests/it/main.rs`, never a new
//! `crates/engine-core/tests/*.rs` binary.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use claude_code_rs::{Config, Outcome};
use serde_json::json;
use uuid::Uuid;

use engine_contract::TaskContext;
use engine_core::node::Node;
use engine_core::policy::permission::PermissionProfile;
use engine_core::policy::PolicyConfigSource;
use engine_core::repo_registry::RepoRegistry;
use engine_core::sessions::{ClaudeSession, SESSIONS_METADATA_KEY};
use engine_core::workflow::Workflow;
use engine_core::workflows::orchestration::chain::ChainStep;
use engine_core::workflows::orchestration::execute::{
    execute_step, EngineKind, FlowInvocation, FlowRunner,
};
use engine_core::workflows::sdlc_flow;
use engine_core::workflows::sdlc_flow::docs::PatchDocsNode;
use engine_core::workflows::sdlc_flow::end_review::{EndReviewNode, EndReviewRouterNode};
use engine_core::workflows::sdlc_flow::final_validation::FinalValidationNode;
use engine_core::workflows::sdlc_flow::graph as flow_graph;
use engine_core::workflows::sdlc_flow::policy::{ModelTier, ReviewMode, SdlcPolicy};
use engine_core::workflows::sdlc_flow::setup::{
    CommandOutput, CommandRunner, RESOLVED_POLICY_IDENTITY,
};
use engine_core::workflows::sdlc_flow::task_loop::{
    ConsolidatedReviewNode, ImplementTaskNode, SaveStateNode, TestTaskNode, TriageTaskNode,
};
use engine_core::workflows::sdlc_flow::wrap_up::WrapUpNode;
use engine_core::workflows::sdlc_flow::ModelTransport;
use engine_core::workflows::sdlc_task;
use engine_core::NodeRegistry;

// ── Shared micro-fixtures ────────────────────────────────────────────────

fn step(repo: &str, block_id: &str) -> ChainStep {
    ChainStep {
        repo: repo.to_string(),
        block_id: block_id.to_string(),
        directives: None,
        ..Default::default()
    }
}

/// A tempdir `brain.toml` + one real repo directory — the lightest fixture
/// `execute_step` needs (mirrors `execute.rs`'s own `two_repo_registry`
/// test helper, which this file cannot reuse since it is private).
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

/// A [`FlowRunner`] test double that records every [`FlowInvocation`] it
/// receives and returns a fixed, empty successful [`TaskContext`] — mirrors
/// `execute.rs`'s own `recording_runner`.
fn recording_runner() -> (FlowRunner, Arc<std::sync::Mutex<Vec<FlowInvocation>>>) {
    let calls: Arc<std::sync::Mutex<Vec<FlowInvocation>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let runner: FlowRunner = Arc::new(move |invocation: FlowInvocation| {
        recorded.lock().unwrap().push(invocation);
        Box::pin(async {
            Ok(TaskContext {
                event: json!({}),
                nodes: HashMap::new(),
                metadata: json!({}),
                node_runs: HashMap::new(),
            })
        })
    });
    (runner, calls)
}

#[allow(clippy::too_many_arguments)]
async fn call_execute_step(
    registry: &RepoRegistry,
    runner: &FlowRunner,
    engine: EngineKind,
    child_sdlc_flow_policy: Option<&serde_json::Value>,
    child_sdlc_task_policy: Option<&serde_json::Value>,
) {
    let resolve_engine = move |_repo: &str, _id: &str| engine;
    let s = step("repo-a", "A.1");
    execute_step(
        &s,
        &resolve_engine,
        registry,
        runner,
        false,
        true,
        Uuid::new_v4(),
        None,
        None,
        PermissionProfile::Standard,
        None,
        child_sdlc_flow_policy,
        child_sdlc_task_policy,
    )
    .await
    .expect("step should execute");
}

fn empty_ctx(event: serde_json::Value) -> TaskContext {
    TaskContext {
        event,
        nodes: HashMap::new(),
        metadata: json!({}),
        node_runs: HashMap::new(),
    }
}

// ── (1) child event composition, unchanged without an override ─────────────

/// Golden invocation-level regression check for task 2's "omit the key when
/// `None`" rule: with neither child-policy knob set, `execute_step` hands
/// the runner a [`FlowInvocation`] whose two child-policy fields are both
/// `None` — for BOTH sanctioned engines — exactly the shape that existed
/// before `EN.17.F` task 1 added the fields at all.
#[tokio::test]
async fn cost_parity_child_event_unchanged_without_override() {
    let (_dir, registry) = one_repo_registry();

    for engine in [EngineKind::Flow, EngineKind::Task] {
        let (runner, calls) = recording_runner();
        call_execute_step(&registry, &runner, engine, None, None).await;

        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert!(
            recorded[0].child_sdlc_flow_policy.is_none(),
            "no override was forwarded — child_sdlc_flow_policy must stay None for {engine:?}"
        );
        assert!(
            recorded[0].child_sdlc_task_policy.is_none(),
            "no override was forwarded — child_sdlc_task_policy must stay None for {engine:?}"
        );
    }
}

// ── (2) resolved policy: end_only reaches an SDLC_FLOW child ──────────────

/// "Assert on the resolved policy, not the forwarded JSON": the exact
/// value `child_sdlc_flow_policy: {review_mode: end_only}` composes into a
/// child event's `"policy"` key (per `execute.rs`'s own routing tests), fed
/// here as that key straight into `sdlc_flow::setup::resolve_policy_for_run_from`
/// — the same four-layer resolver a real SDLC_FLOW child's `SetupWorktreeNode`
/// calls. `PolicyConfigSource::Builtin` resolves with no filesystem access,
/// so a child worktree's own `harness.json` cannot interfere here.
#[test]
fn cost_parity_child_flow_resolves_end_only() {
    let ctx = empty_ctx(json!({
        "spec_slug": "cost-parity-fixture",
        "policy": { "review_mode": "end_only" },
    }));
    let resolved =
        sdlc_flow::setup::resolve_policy_for_run_from(&ctx, &PolicyConfigSource::Builtin)
            .expect("policy resolves");
    assert_eq!(resolved.review_mode, ReviewMode::EndOnly);

    // Inverse: with no override at all, the built-in default is PerTask —
    // the fact this block's OBSERVED RED baseline names explicitly.
    let ctx_no_override = empty_ctx(json!({ "spec_slug": "cost-parity-fixture" }));
    let default_resolved = sdlc_flow::setup::resolve_policy_for_run_from(
        &ctx_no_override,
        &PolicyConfigSource::Builtin,
    )
    .expect("policy resolves");
    assert_eq!(default_resolved.review_mode, ReviewMode::PerTask);
}

// ── (3) child policy routes by engine ──────────────────────────────────────

/// `execute_step` forwards BOTH child-policy parameters onto every
/// [`FlowInvocation`] it builds, unconditionally — the engine-conditioned
/// SELECTION of which one actually reaches the composed event's `"policy"`
/// key happens downstream, inside the private `sdlc_flow_event`/
/// `sdlc_task_event` (unit-tested in `execute.rs`: a `child_sdlc_task_policy`
/// value never appears in an `EngineKind::Flow` child's event, and vice
/// versa). This test proves the raw material that routing consumes is
/// threaded correctly regardless of which engine the step resolves to, and
/// separately proves the two engines' OWN resolvers are mutually blind to a
/// knob the other engine's policy shape does not carry: `review_mode` is a
/// [`SdlcPolicy`]-only field (`SdlcTaskPolicy` has none), so resolving it
/// through `sdlc_task`'s own resolver leaves that engine's own knobs
/// unaffected by a flow-shaped override.
#[tokio::test]
async fn cost_parity_child_policy_routes_by_engine() {
    let (_dir, registry) = one_repo_registry();
    let flow_value = json!({ "review_mode": "end_only" });
    let task_value = json!({ "llm_triage": true });

    for engine in [EngineKind::Flow, EngineKind::Task] {
        let (runner, calls) = recording_runner();
        call_execute_step(
            &registry,
            &runner,
            engine,
            Some(&flow_value),
            Some(&task_value),
        )
        .await;

        let recorded = calls.lock().unwrap();
        assert_eq!(
            recorded[0].child_sdlc_flow_policy,
            Some(flow_value.clone()),
            "child_sdlc_flow_policy must reach the invocation for {engine:?}"
        );
        assert_eq!(
            recorded[0].child_sdlc_task_policy,
            Some(task_value.clone()),
            "child_sdlc_task_policy must reach the invocation for {engine:?}"
        );
    }

    // The resolution-level half: an SDLC_TASK child's own resolver applies
    // `llm_triage` from its event override; an SDLC_FLOW-shaped
    // `review_mode` override fed to the SAME resolver has no field to land
    // on and is silently ignored rather than corrupting task policy.
    let task_ctx =
        empty_ctx(json!({ "spec_slug": "routing-fixture-task", "policy": task_value.clone() }));
    let resolved_task =
        sdlc_task::profiles::resolve_policy_for_run_from(&task_ctx, &PolicyConfigSource::Builtin)
            .expect("sdlc_task policy resolves");
    assert!(resolved_task.llm_triage);

    let flow_ctx = empty_ctx(json!({
        "spec_slug": "routing-fixture",
        "policy": flow_value,
    }));
    let resolved_flow =
        sdlc_flow::setup::resolve_policy_for_run_from(&flow_ctx, &PolicyConfigSource::Builtin)
            .expect("sdlc_flow policy resolves");
    assert_eq!(resolved_flow.review_mode, ReviewMode::EndOnly);
}

// ── ImplementTaskNode fixture: final-attempt tier + turn ceiling ──────────

/// Builds the minimal `ctx` `ImplementTaskNode::process` needs, entirely
/// in-memory (no filesystem, no `SetupWorktreeNode`/`TaskQueueRouterNode`
/// walk): `latest_state` and `current_task_fields` (`task_loop.rs`) read
/// straight off `ctx.nodes["LoadTaskStateNode"]` / `["TaskQueueRouterNode"]`,
/// so a hand-stamped `ctx.nodes` is exactly what a real dequeue would have
/// left behind.
fn implement_ctx(policy: &SdlcPolicy, attempt_count: u32, max_attempts: u32) -> TaskContext {
    let mut ctx = empty_ctx(json!({ "spec_slug": "cost-parity-implement" }));
    ctx.nodes.insert(
        "SetupWorktreeNode".to_string(),
        json!({ "worktree_path": "." }),
    );
    ctx.nodes.insert(
        RESOLVED_POLICY_IDENTITY.to_string(),
        serde_json::to_value(policy).expect("SdlcPolicy serializes"),
    );
    ctx.nodes.insert(
        "TaskQueueRouterNode".to_string(),
        json!({ "current_task_id": 1 }),
    );
    ctx.nodes.insert(
        "LoadTaskStateNode".to_string(),
        json!({
            "spec_slug": "cost-parity-implement",
            "tasks": [{
                "task_id": 1,
                "title": "fixture task",
                "description": "fixture description",
                "acceptance_criteria": ["fixture criterion"],
                "status": "pending",
                "attempt_count": attempt_count,
                "max_attempts": max_attempts,
            }],
        }),
    );
    ctx
}

fn canned_outcome() -> Outcome {
    Outcome {
        cost_usd: 0.0,
        usage: claude_code_rs::parse::Usage {
            input_tokens: 1,
            output_tokens: 1,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
        model_usage: std::collections::BTreeMap::new(),
        text: json!({ "summary": "ok", "modified_files": [], "tests_added": [] }).to_string(),
        is_error: false,
        api_error_status: None,
        session_id: None,
        structured_output: None,
    }
}

fn capturing_implement_transport() -> (ModelTransport, Arc<std::sync::Mutex<Vec<Config>>>) {
    let configs: Arc<std::sync::Mutex<Vec<Config>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = configs.clone();
    let transport: ModelTransport = Arc::new(move |config, _prompt| {
        captured.lock().unwrap().push(config);
        Box::pin(async move { Ok(canned_outcome()) })
    });
    (transport, configs)
}

// ── (4) final-attempt escalation ────────────────────────────────────────

/// With `implement_final_attempt: Some(Opus)` and `max_attempts: 3`,
/// attempts 1..2 (not final: `attempt_count + 1 < max_attempts`) dispatch
/// with the plain `implement` tier and attempt 3 (final:
/// `attempt_count + 1 == max_attempts`) escalates — asserted on
/// `ctx.nodes["ImplementTaskNode"]["model_tier"]`, the stamp
/// `RunTelemetry`/`PolicyAggregate` reads. With `implement_final_attempt:
/// None`, every attempt (including the final one) stays on `implement`.
#[tokio::test]
async fn cost_parity_final_attempt_escalates_tier() {
    let policy = SdlcPolicy {
        model_tiers: engine_core::workflows::sdlc_flow::policy::ModelTiers {
            implement: ModelTier::Sonnet,
            implement_final_attempt: Some(ModelTier::Opus),
            ..Default::default()
        },
        ..SdlcPolicy::default()
    };

    // attempt_count=1 -> attempt 2 of 3 (not final).
    let ctx_mid = implement_ctx(&policy, 1, 3);
    let node = ImplementTaskNode::new().with_transport(capturing_implement_transport().0);
    let ctx_mid = node.process(ctx_mid).await.expect("process succeeds");
    assert_eq!(
        ctx_mid.nodes["ImplementTaskNode"]["model_tier"],
        json!("sonnet"),
        "a non-final attempt must stay on the plain implement tier"
    );

    // attempt_count=2 -> attempt 3 of 3 (final).
    let ctx_final = implement_ctx(&policy, 2, 3);
    let node = ImplementTaskNode::new().with_transport(capturing_implement_transport().0);
    let ctx_final = node.process(ctx_final).await.expect("process succeeds");
    assert_eq!(
        ctx_final.nodes["ImplementTaskNode"]["model_tier"],
        json!("opus"),
        "the final attempt must escalate to implement_final_attempt"
    );

    // implement_final_attempt: None -> every attempt (including the final
    // one) stays on `implement`.
    let policy_no_escalation = SdlcPolicy::default();
    let ctx_final_no_escalation = implement_ctx(&policy_no_escalation, 2, 3);
    let node = ImplementTaskNode::new().with_transport(capturing_implement_transport().0);
    let ctx_final_no_escalation = node
        .process(ctx_final_no_escalation)
        .await
        .expect("process succeeds");
    assert_eq!(
        ctx_final_no_escalation.nodes["ImplementTaskNode"]["model_tier"],
        json!("sonnet"),
        "with implement_final_attempt unset, the final attempt must NOT escalate"
    );
}

// ── (5) turn ceiling reaches Config ─────────────────────────────────────

/// `max_turns.implement: Some(k)` reaches the `Config` the implement
/// stage's stub transport actually receives; a policy with `max_turns.implement:
/// None` leaves `Config.max_turns` at `claude-code-rs`'s own unbounded
/// default.
#[tokio::test]
async fn cost_parity_turn_ceiling_reaches_config() {
    let policy = SdlcPolicy {
        max_turns: engine_core::workflows::sdlc_flow::policy::StageTurnCeilings {
            implement: Some(7),
            ..Default::default()
        },
        ..SdlcPolicy::default()
    };
    let ctx = implement_ctx(&policy, 0, 3);
    let (transport, captured) = capturing_implement_transport();
    let node = ImplementTaskNode::new().with_transport(transport);
    let _ = node.process(ctx).await.expect("process succeeds");
    {
        let configs = captured.lock().unwrap();
        assert_eq!(configs.len(), 1);
        assert_eq!(
            configs[0].max_turns,
            Some(7),
            "the resolved per-stage turn ceiling must reach Config.max_turns"
        );
    }

    let ctx_none = implement_ctx(&SdlcPolicy::default(), 0, 3);
    let (transport_none, captured_none) = capturing_implement_transport();
    let node_none = ImplementTaskNode::new().with_transport(transport_none);
    let _ = node_none.process(ctx_none).await.expect("process succeeds");
    {
        let configs_none = captured_none.lock().unwrap();
        assert_eq!(
            configs_none[0].max_turns, None,
            "a stage with no resolved ceiling must receive Config.max_turns == None"
        );
    }
}

// ── (6)/(7) context cap ─────────────────────────────────────────────────

fn temp_dir(tag: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "engine-core-cost-parity-{tag}-{}-{n}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn ctx_with_worktree_and_policy(
    spec_slug: &str,
    worktree: &Path,
    policy: &SdlcPolicy,
) -> TaskContext {
    let mut ctx = empty_ctx(json!({ "spec_slug": spec_slug }));
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

/// With `generate_context_max_bytes: Some(b)`, `GenerateTasksNode`'s
/// composed prompt (via `gather_context`, planning-fallback path) must
/// carry a truncation marker and must NOT carry the oversized file's full
/// body verbatim.
#[tokio::test]
async fn cost_parity_context_cap_truncates_with_marker() {
    let worktree = temp_dir("cap");
    let spec_dir = worktree.join("planning").join("cap-spec");
    std::fs::create_dir_all(&spec_dir).unwrap();
    let large = "y".repeat(1000);
    std::fs::write(spec_dir.join("context.md"), &large).unwrap();

    let policy = SdlcPolicy {
        generate_context_max_bytes: Some(64),
        ..SdlcPolicy::default()
    };
    let ctx = ctx_with_worktree_and_policy("cap-spec", &worktree, &policy);

    let captured_prompt: Arc<std::sync::Mutex<Option<String>>> =
        Arc::new(std::sync::Mutex::new(None));
    let captured = captured_prompt.clone();
    let canned = json!({ "tasks": [], "tasks_markdown": "" }).to_string();
    let transport: ModelTransport = Arc::new(move |_config, prompt| {
        *captured.lock().unwrap() = Some(prompt);
        let canned = canned.clone();
        Box::pin(async move { Ok(canned_outcome_with(canned)) })
    });

    let node = sdlc_flow::setup::GenerateTasksNode::new().with_transport(transport);
    let _ = node.process(ctx).await.expect("process succeeds");

    let prompt = captured_prompt
        .lock()
        .unwrap()
        .clone()
        .expect("prompt captured");
    assert!(
        !prompt.contains(&large),
        "prompt must not carry the full uncapped file body"
    );
    assert!(
        prompt.contains("truncated:"),
        "prompt must carry a truncation marker"
    );

    std::fs::remove_dir_all(&worktree).ok();
}

/// With `generate_context_max_bytes: None`, the composed prompt for the
/// same oversized fixture carries the file's body verbatim, byte-identical
/// to the pre-knob behavior — no truncation marker anywhere.
#[tokio::test]
async fn cost_parity_context_unchanged_without_cap() {
    let worktree = temp_dir("nocap");
    let spec_dir = worktree.join("planning").join("nocap-spec");
    std::fs::create_dir_all(&spec_dir).unwrap();
    let body = "z".repeat(1000);
    std::fs::write(spec_dir.join("context.md"), &body).unwrap();

    let ctx = ctx_with_worktree_and_policy("nocap-spec", &worktree, &SdlcPolicy::default());

    let captured_prompt: Arc<std::sync::Mutex<Option<String>>> =
        Arc::new(std::sync::Mutex::new(None));
    let captured = captured_prompt.clone();
    let canned = json!({ "tasks": [], "tasks_markdown": "" }).to_string();
    let transport: ModelTransport = Arc::new(move |_config, prompt| {
        *captured.lock().unwrap() = Some(prompt);
        let canned = canned.clone();
        Box::pin(async move { Ok(canned_outcome_with(canned)) })
    });

    let node = sdlc_flow::setup::GenerateTasksNode::new().with_transport(transport);
    let _ = node.process(ctx).await.expect("process succeeds");

    let prompt = captured_prompt
        .lock()
        .unwrap()
        .clone()
        .expect("prompt captured");
    assert!(
        prompt.contains(&body),
        "with no cap, the prompt must carry the file's full body verbatim"
    );
    assert!(
        !prompt.contains("truncated:"),
        "with no cap, no truncation marker should appear anywhere"
    );

    std::fs::remove_dir_all(&worktree).ok();
}

fn canned_outcome_with(text: String) -> Outcome {
    Outcome {
        cost_usd: 0.0,
        usage: claude_code_rs::parse::Usage {
            input_tokens: 1,
            output_tokens: 1,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
        model_usage: std::collections::BTreeMap::new(),
        text,
        is_error: false,
        api_error_status: None,
        session_id: None,
        structured_output: None,
    }
}

// ── (8) end_only records exactly one review-stage session ─────────────

/// Replaces the real `SetupWorktreeNode`: writes a controlled temp-dir
/// `worktree_path` and stamps an already-resolved [`SdlcPolicy`] directly,
/// mirroring `sdlc_flow_end_review_e2e.rs`'s own `FixtureSetupNode` (which
/// this file cannot reuse — it is private to that module).
struct FixtureSetupNode {
    worktree_path: String,
    resolved_policy: serde_json::Value,
}

#[async_trait::async_trait]
impl Node for FixtureSetupNode {
    async fn process(
        &self,
        mut ctx: TaskContext,
    ) -> Result<TaskContext, engine_core::node::NodeError> {
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({
                "worktree_path": self.worktree_path,
                "branch_name": "sdlc/cost-parity-fixture-spec",
            }),
        );
        ctx.nodes.insert(
            RESOLVED_POLICY_IDENTITY.to_string(),
            self.resolved_policy.clone(),
        );
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "SetupWorktreeNode"
    }
}

const SPEC_SLUG: &str = "cost-parity-fixture-spec";

fn write_fixture_files(worktree: &Path) {
    let spec_dir = worktree.join("planning").join(SPEC_SLUG);
    std::fs::create_dir_all(&spec_dir).unwrap();
    let tasks = json!([{
        "task_id": 1,
        "title": "Task One",
        "description": "d1",
        "acceptance_criteria": ["criterion one"],
        "max_attempts": 3,
    }]);
    std::fs::write(
        spec_dir.join("tasks.json"),
        serde_json::to_string_pretty(&tasks).unwrap(),
    )
    .unwrap();

    let harness = json!({
        "validation": {
            "checks": [{ "name": "tests", "kind": "command", "command": "does-not-matter", "gates": true }]
        }
    });
    std::fs::write(
        worktree.join("planning").join("harness.json"),
        serde_json::to_string_pretty(&harness).unwrap(),
    )
    .unwrap();
}

fn make_runner() -> CommandRunner {
    Arc::new(move |program, args, _cwd| {
        if program == "git" {
            if args.first() == Some(&"status") {
                return Ok(CommandOutput {
                    status: 0,
                    stdout: " M src/lib.rs\n".to_string(),
                    stderr: String::new(),
                });
            }
            if args.first() == Some(&"diff") {
                if args.get(1) == Some(&"--numstat") {
                    return Ok(CommandOutput {
                        status: 0,
                        stdout: "400\t400\tsrc/a.rs\n".to_string(),
                        stderr: String::new(),
                    });
                }
                return Ok(CommandOutput {
                    status: 0,
                    stdout: "diff --git a/one.rs b/one.rs\n+change\n".to_string(),
                    stderr: String::new(),
                });
            }
        }
        Ok(CommandOutput {
            status: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    })
}

fn stub_outcome(text: &str) -> Outcome {
    Outcome {
        cost_usd: 0.01,
        usage: claude_code_rs::parse::Usage {
            input_tokens: 10,
            output_tokens: 5,
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

fn implement_transport_for_review_e2e() -> ModelTransport {
    Arc::new(|_config, _prompt| {
        Box::pin(async move {
            Ok(stub_outcome(
                &json!({ "summary": "implemented", "modified_files": ["src/lib.rs"], "tests_added": ["it_works"] })
                    .to_string(),
            ))
        })
    })
}

fn patch_docs_transport_for_review_e2e() -> ModelTransport {
    Arc::new(|_config, _prompt| {
        Box::pin(async move {
            Ok(stub_outcome(
                &json!({ "summary": "no stale docs found", "files_patched": [] }).to_string(),
            ))
        })
    })
}

fn verdict_transport(verdict: &'static str) -> ModelTransport {
    Arc::new(move |_config, _prompt| {
        Box::pin(async move {
            Ok(stub_outcome(
                &json!({ "verdict": verdict, "summary": "reviewed", "issues": [] }).to_string(),
            ))
        })
    })
}

fn build_workflow(
    worktree: &Path,
    policy: &SdlcPolicy,
    consolidated_review_transport: ModelTransport,
    end_review_transport: ModelTransport,
) -> Workflow {
    let mut registry: NodeRegistry = flow_graph::registry_for_policy(policy);

    registry.register(Box::new(FixtureSetupNode {
        worktree_path: worktree.to_string_lossy().to_string(),
        resolved_policy: serde_json::to_value(policy).expect("SdlcPolicy should serialize"),
    }));
    registry.register(Box::new(
        ImplementTaskNode::new().with_transport(implement_transport_for_review_e2e()),
    ));
    registry.register(Box::new(TestTaskNode::new().with_runner(make_runner())));
    registry.register(Box::new(
        TriageTaskNode::new()
            .with_transport(Arc::new(|_c, _p| {
                Box::pin(async { panic!("llm_triage is false") })
            }))
            .with_runner(make_runner()),
    ));
    registry.register(Box::new(
        ConsolidatedReviewNode::new()
            .with_runner(make_runner())
            .with_transport(consolidated_review_transport),
    ));
    registry.register(Box::new(SaveStateNode::new().with_runner(make_runner())));
    registry.register(Box::new(
        FinalValidationNode::new().with_runner(make_runner()),
    ));
    registry.register(Box::new(
        EndReviewNode::new()
            .with_runner(make_runner())
            .with_transport(end_review_transport),
    ));
    registry.register(Box::new(EndReviewRouterNode));
    registry.register(Box::new(
        PatchDocsNode::new().with_transport(patch_docs_transport_for_review_e2e()),
    ));
    registry.register(Box::new(WrapUpNode::new().with_runner(make_runner())));

    let schema = flow_graph::schema();
    Workflow::new_validated(registry, schema).expect("SDLC_FLOW graph should validate")
}

fn session_count(ctx: &TaskContext, node: &str) -> usize {
    ctx.metadata
        .get(SESSIONS_METADATA_KEY)
        .and_then(|v| v.as_array())
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| serde_json::from_value::<ClaudeSession>(entry.clone()).ok())
                .filter(|session| session.node == node)
                .count()
        })
        .unwrap_or(0)
}

/// THE MEASUREMENT criterion: a stub-transport SDLC_FLOW run over a
/// one-task fixture records exactly ONE `EndReviewNode` session-ledger
/// entry under `end_only`, and ZERO under `per_task` — where `per_task`
/// instead records exactly one `ConsolidatedReviewNode` entry. Asserted on
/// the session ledger (`ctx.metadata[SESSIONS_METADATA_KEY]`), never on
/// `total_cost_usd` — per the block's carryover constraint
/// (`end-review-node-is-billed-but-absent-from-cost-bearing-stages`).
#[tokio::test]
async fn cost_parity_end_only_child_records_one_review_session() {
    let worktree = temp_dir("end-only-ledger");
    write_fixture_files(&worktree);

    let policy = SdlcPolicy {
        review_mode: ReviewMode::EndOnly,
        ..SdlcPolicy::default()
    };
    let workflow = build_workflow(
        &worktree,
        &policy,
        verdict_transport("PASS"),
        verdict_transport("PASS"),
    );
    let ctx = workflow
        .run(
            json!({ "spec_slug": SPEC_SLUG, "auto_pr": false, "llm_triage": false }),
            Box::new(|_ctx: &TaskContext| {}),
        )
        .await
        .expect("workflow run should not error");

    assert_eq!(
        session_count(&ctx, "EndReviewNode"),
        1,
        "end_only must record exactly one EndReviewNode session"
    );
    assert_eq!(
        session_count(&ctx, "ConsolidatedReviewNode"),
        0,
        "end_only must record zero ConsolidatedReviewNode sessions"
    );

    std::fs::remove_dir_all(&worktree).ok();
}

/// Inverse: `per_task` over the same one-task fixture records exactly one
/// `ConsolidatedReviewNode` session and zero `EndReviewNode` sessions.
#[tokio::test]
async fn cost_parity_per_task_child_records_zero_end_review_sessions() {
    let worktree = temp_dir("per-task-ledger");
    write_fixture_files(&worktree);

    let policy = SdlcPolicy {
        review_mode: ReviewMode::PerTask,
        ..SdlcPolicy::default()
    };
    let workflow = build_workflow(
        &worktree,
        &policy,
        verdict_transport("PASS"),
        verdict_transport("PASS"),
    );
    let ctx = workflow
        .run(
            json!({ "spec_slug": SPEC_SLUG, "auto_pr": false, "llm_triage": false }),
            Box::new(|_ctx: &TaskContext| {}),
        )
        .await
        .expect("workflow run should not error");

    assert_eq!(
        session_count(&ctx, "ConsolidatedReviewNode"),
        1,
        "per_task must record exactly one ConsolidatedReviewNode session"
    );
    assert_eq!(
        session_count(&ctx, "EndReviewNode"),
        0,
        "per_task must record zero EndReviewNode sessions"
    );

    std::fs::remove_dir_all(&worktree).ok();
}
