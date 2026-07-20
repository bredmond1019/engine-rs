//! Deterministic plumbing tests for the four canonical SDLC Flow policy
//! profiles (`Phase 2 · Block C`, task 1): proves each named profile
//! resolves to the documented `SdlcPolicy`, that `profile_by_name` round
//! trips all four kebab-case names (plus `None` for unknown names) to the
//! same resolved policy the direct constructors produce.
//!
//! Hermetic by construction: no `resolve` call here touches disk, a
//! subprocess, or the network — it is pure in-memory struct merging. Later
//! tasks in this block extend this file with precedence, unknown-profile
//! error, and routing assertions (see `planning/plan-sdlc-policy-profiles-C/`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use claude_code_rs::Outcome;
use engine_contract::{NodeRunStatus, TaskContext};
use engine_core::node::{Node, NodeError, NodeRegistry};
use engine_core::workflow::Workflow;
use engine_core::workflows::sdlc_flow::docs::PatchDocsNode;
use engine_core::workflows::sdlc_flow::emit_state::EmitStateNode;
use engine_core::workflows::sdlc_flow::graph;
use engine_core::workflows::sdlc_flow::policy::{
    LocalConfig, ModelTier, ModelTiers, OutputVerbosity, PartialPolicy, ReviewMode, SdlcPolicy,
};
use engine_core::workflows::sdlc_flow::pr::PullRequestNode;
use engine_core::workflows::sdlc_flow::setup::{
    CommandOutput, CommandRunner, RESOLVED_POLICY_IDENTITY,
};
use engine_core::workflows::sdlc_flow::task_loop::{
    ConsolidatedReviewNode, ImplementTaskNode, ReviewRouterNode, SaveStateNode, TestTaskNode,
    TriageRouterNode, TriageTaskNode, UpdateTaskStatusNode,
};
use engine_core::workflows::sdlc_flow::wrap_up::WrapUpNode;
use engine_core::workflows::sdlc_flow::{policy, profiles, setup, ModelTransport};
use serde_json::json;

/// A profile's kebab-case name paired with its direct constructor, used to
/// prove `profile_by_name` round-trips to the same resolution.
type NamedProfileCtor = (&'static str, fn() -> PartialPolicy);

/// `baseline` is the explicit control: Sonnet on every tier, `per_task`
/// review, `llm_triage` off — a legible no-op against the built-in default.
#[test]
fn baseline_profile_resolves_to_documented_policy() {
    let resolved = policy::resolve(
        SdlcPolicy::default(),
        None,
        Some(&profiles::baseline()),
        None,
    );

    assert_eq!(
        resolved.model_tiers,
        ModelTiers {
            implement: ModelTier::Sonnet,
            implement_simple: ModelTier::Sonnet,
            review: ModelTier::Sonnet,
            triage: ModelTier::Sonnet,
            generate: ModelTier::Sonnet,
        }
    );
    assert_eq!(resolved.review_mode, ReviewMode::PerTask);
    assert!(!resolved.llm_triage);
}

/// `cheap-fast`: Haiku implement, local triage+review, terse output,
/// trivial-task review skip.
#[test]
fn cheap_fast_profile_resolves_to_documented_policy() {
    let resolved = policy::resolve(
        SdlcPolicy::default(),
        None,
        Some(&profiles::cheap_fast()),
        None,
    );

    assert_eq!(resolved.model_tiers.implement, ModelTier::Haiku);
    assert_eq!(resolved.model_tiers.triage, ModelTier::Local);
    assert_eq!(resolved.model_tiers.review, ModelTier::Local);
    assert_eq!(resolved.output_verbosity, OutputVerbosity::Terse);
    assert_eq!(resolved.review_mode, ReviewMode::TrivialSkip);
}

/// `pragmatist`: Sonnet implement, local review, prompt caching on,
/// trivial-task review skip, `llm_triage` on.
#[test]
fn pragmatist_profile_resolves_to_documented_policy() {
    let resolved = policy::resolve(
        SdlcPolicy::default(),
        None,
        Some(&profiles::pragmatist()),
        None,
    );

    assert_eq!(resolved.model_tiers.implement, ModelTier::Sonnet);
    assert_eq!(resolved.model_tiers.review, ModelTier::Local);
    assert!(resolved.prompt_cache);
    assert_eq!(resolved.review_mode, ReviewMode::TrivialSkip);
    assert!(resolved.llm_triage);
}

/// `batch-reviewer`: Sonnet implement, per-task review collapsed into a
/// single end-of-run review.
#[test]
fn batch_reviewer_profile_resolves_to_documented_policy() {
    let resolved = policy::resolve(
        SdlcPolicy::default(),
        None,
        Some(&profiles::batch_reviewer()),
        None,
    );

    assert_eq!(resolved.model_tiers.implement, ModelTier::Sonnet);
    assert_eq!(resolved.review_mode, ReviewMode::EndOnly);
}

/// `profile_by_name` must resolve all four canonical kebab-case names to a
/// bundle that produces the exact same resolved `SdlcPolicy` as calling the
/// named constructor directly.
#[test]
fn profile_by_name_matches_direct_constructor_resolution() {
    let cases: &[NamedProfileCtor] = &[
        ("baseline", profiles::baseline),
        ("cheap-fast", profiles::cheap_fast),
        ("pragmatist", profiles::pragmatist),
        ("batch-reviewer", profiles::batch_reviewer),
    ];

    for (name, ctor) in cases {
        let by_name = profiles::profile_by_name(name)
            .unwrap_or_else(|| panic!("profile_by_name({name:?}) should resolve to Some(bundle)"));
        let direct = ctor();

        let resolved_by_name = policy::resolve(SdlcPolicy::default(), None, Some(&by_name), None);
        let resolved_direct = policy::resolve(SdlcPolicy::default(), None, Some(&direct), None);

        assert_eq!(
            resolved_by_name, resolved_direct,
            "profile_by_name({name:?}) should resolve identically to the direct constructor"
        );
    }
}

/// An unknown profile name must not silently resolve to a bundle.
#[test]
fn profile_by_name_returns_none_for_unknown_name() {
    assert_eq!(profiles::profile_by_name("does-not-exist"), None);
}

/// A bare `TaskContext` carrying only an `event` — mirrors `setup.rs`'s
/// private `empty_context` test helper, reconstructed here since it isn't
/// exported across the crate boundary.
fn empty_context(event: serde_json::Value) -> TaskContext {
    TaskContext {
        event,
        nodes: HashMap::new(),
        metadata: serde_json::json!({}),
        node_runs: HashMap::new(),
    }
}

/// Event-inline `policy` overrides beat the `profile` layer field-by-field:
/// the overridden field (`max_attempts`) takes the inline value, while every
/// other field still falls through to the profile's bundle. Exercised
/// through the same `resolve` call shape `setup::resolve_policy_for_run`
/// uses (profile arg = `Some(cheap_fast)`, event_override = `Some(inline)`).
#[test]
fn event_inline_policy_overrides_profile_field_but_keeps_profile_tiers() {
    let inline_override = PartialPolicy {
        max_attempts: Some(9),
        ..Default::default()
    };

    let resolved = policy::resolve(
        SdlcPolicy::default(),
        None,
        Some(&profiles::cheap_fast()),
        Some(&inline_override),
    );

    // The inline-overridden field wins...
    assert_eq!(resolved.max_attempts, 9);
    // ...but every other cheap-fast knob still comes through from the
    // profile layer, since the inline override left those fields `None`.
    assert_eq!(resolved.model_tiers.implement, ModelTier::Haiku);
    assert_eq!(resolved.model_tiers.triage, ModelTier::Local);
    assert_eq!(resolved.model_tiers.review, ModelTier::Local);
    assert_eq!(resolved.output_verbosity, OutputVerbosity::Terse);
    assert_eq!(resolved.review_mode, ReviewMode::TrivialSkip);
}

/// The full run-level resolution path (`setup::resolve_policy_for_run`,
/// reachable here since `setup` is a `pub mod` and the function itself is
/// `pub`) errors on an unknown profile name rather than silently no-op'ing.
/// `setup.rs`'s own unit test (`unknown_profile_name_returns_node_error`)
/// covers the same path from inside the crate; this integration-level copy
/// proves the behavior holds through the public API surface too.
#[test]
fn resolve_policy_for_run_errors_on_unknown_profile_name() {
    let worktree = std::env::temp_dir().join(format!(
        "sdlc_flow_profiles_unknown_profile_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&worktree).expect("temp worktree dir should create");

    let ctx = empty_context(serde_json::json!({
        "spec_slug": "my-spec",
        "profile": "does-not-exist",
    }));

    let err = setup::resolve_policy_for_run(&ctx, &worktree)
        .expect_err("an unknown profile name must not resolve");
    assert!(
        err.message.contains("unknown profile"),
        "expected an 'unknown profile' error message, got: {}",
        err.message
    );

    let _ = std::fs::remove_dir_all(&worktree);
}

// ===========================================================================
// Task 3: routing assertions — `trivial_skip` skips review, `local` tier
// rewires transport. Built on `graph::registry_for_policy(&resolved_policy)`,
// driving the real assembled `SDLC_FLOW` graph end-to-end (mirroring
// `tests/sdlc_flow_e2e.rs`'s hermetic seam-injection pattern: `stub_outcome`,
// `write_fixture_files`, `build_workflow`, the `noop_git_runner`-style
// transport/runner spies) rather than calling `TriageRouterNode::route`
// directly, so the policy-driven registry construction itself is exercised.
// ===========================================================================

/// Replaces the real `SetupWorktreeNode`: writes a controlled temp-dir
/// `worktree_path` directly (no real `git worktree add`), and stamps the
/// already-resolved `SdlcPolicy` under `RESOLVED_POLICY_IDENTITY` the way
/// `setup::SetupWorktreeNode::process` does in production — so the rest of
/// the graph (`TriageRouterNode`'s `resolved_policy` read, `graph.rs`'s
/// per-stage transport wiring) sees exactly the policy this test resolved,
/// without needing a real `harness.json`/`sdlc.policy` round trip.
struct FixtureSetupNode {
    worktree_path: String,
    resolved_policy: serde_json::Value,
}

#[async_trait::async_trait]
impl Node for FixtureSetupNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({
                "worktree_path": self.worktree_path,
                "branch_name": "sdlc/fixture-profiles-spec",
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

fn temp_worktree(tag: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "engine-core-sdlc-flow-profiles-{tag}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(dir.join("planning").join("fixture-profiles-spec")).unwrap();
    dir
}

/// Writes the fixture `tasks.json` (one PENDING task) under
/// `<worktree>/planning/fixture-profiles-spec/` — mirrors
/// `sdlc_flow_e2e.rs::write_fixture_files`, reconstructed here since it
/// isn't exported across the integration-test binary boundary.
fn write_fixture_tasks(worktree: &Path, max_attempts: u32) {
    let spec_dir = worktree.join("planning").join("fixture-profiles-spec");
    let tasks = json!([
        {
            "task_id": 1,
            "title": "Implement the thing",
            "description": "Do the work",
            "acceptance_criteria": ["it works"],
            "max_attempts": max_attempts,
        }
    ]);
    std::fs::write(
        spec_dir.join("tasks.json"),
        serde_json::to_string_pretty(&tasks).unwrap(),
    )
    .unwrap();
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
        model_usage: [(
            "claude-sonnet-4-5".to_string(),
            claude_code_rs::parse::ModelUsage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                cost_usd: 0.01,
            },
        )]
        .into_iter()
        .collect(),
        text: text.to_string(),
        is_error: false,
        api_error_status: None,
        structured_output: None,
    }
}

fn always_pass_runner() -> CommandRunner {
    Arc::new(|_program, _args, _cwd| {
        Ok(CommandOutput {
            status: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    })
}

fn noop_runner() -> CommandRunner {
    Arc::new(|_program, _args, _cwd| {
        Ok(CommandOutput {
            status: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    })
}

/// A small `git diff --numstat` stub: one file, well under the default
/// `review_skip_max_files`/`review_skip_max_diff_lines` thresholds — drives
/// `TriageTaskNode::classify_trivial` to `trivial: true`.
fn trivial_diff_runner() -> CommandRunner {
    Arc::new(|_program, _args, _cwd| {
        Ok(CommandOutput {
            status: 0,
            stdout: "1\t1\tsrc/a.rs\n".to_string(),
            stderr: String::new(),
        })
    })
}

fn panicking_transport() -> ModelTransport {
    Arc::new(|_config, _prompt| {
        Box::pin(async { panic!("transport must not be called on this path") })
    })
}

/// A `ConsolidatedReviewNode` transport that always replies `PASS` and
/// counts its own invocations — the "spy counter on the review transport"
/// the spec's Acceptance Criteria calls for.
fn counting_review_transport(calls: Arc<AtomicUsize>) -> ModelTransport {
    Arc::new(move |_config, _prompt| {
        calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Ok(stub_outcome(
                &json!({ "verdict": "PASS", "summary": "looks good", "issues": [] }).to_string(),
            ))
        })
    })
}

/// Builds the full assembled `SDLC_FLOW` `Workflow` on top of
/// `graph::registry_for_policy(policy)` (so the policy-driven per-stage
/// transport wiring under test is the real production wiring, not a
/// reimplementation of it): every model/subprocess node other than
/// `ConsolidatedReviewNode` is stubbed hermetically, `ConsolidatedReviewNode`
/// keeps whatever `registry_for_policy` wired it to (the real tier-dependent
/// transport, `review_transport_override` when the test wants to observe it
/// via its own spy instead).
fn build_task_loop_workflow(
    worktree: &Path,
    policy: &SdlcPolicy,
    diff_runner: CommandRunner,
    review_transport_override: Option<ModelTransport>,
) -> Workflow {
    let mut registry: NodeRegistry = graph::registry_for_policy(policy);

    registry.register(Box::new(FixtureSetupNode {
        worktree_path: worktree.to_string_lossy().to_string(),
        resolved_policy: serde_json::to_value(policy).expect("SdlcPolicy should serialize"),
    }));

    registry.register(Box::new(ImplementTaskNode::new().with_transport(Arc::new(
        |_config, _prompt| {
            Box::pin(async move {
                Ok(stub_outcome(
                    &json!({
                        "summary": "implemented",
                        "modified_files": ["src/lib.rs"],
                        "tests_added": ["it_works"],
                    })
                    .to_string(),
                ))
            })
        },
    ))));

    registry.register(Box::new(
        TestTaskNode::new().with_runner(always_pass_runner()),
    ));
    registry.register(Box::new(
        TriageTaskNode::new()
            .with_transport(panicking_transport())
            .with_runner(diff_runner),
    ));
    registry.register(Box::new(TriageRouterNode));

    if let Some(review_transport) = review_transport_override {
        registry.register(Box::new(
            ConsolidatedReviewNode::new()
                .with_runner(noop_runner())
                .with_transport(review_transport),
        ));
    }

    registry.register(Box::new(ReviewRouterNode));
    registry.register(Box::new(UpdateTaskStatusNode));
    registry.register(Box::new(SaveStateNode::new().with_runner(noop_runner())));
    registry.register(Box::new(PatchDocsNode::new().with_transport(Arc::new(
        |_config, _prompt| {
            Box::pin(async move {
                Ok(stub_outcome(
                    &json!({ "summary": "no stale docs found", "files_patched": [] }).to_string(),
                ))
            })
        },
    ))));
    registry.register(Box::new(WrapUpNode::new()));
    registry.register(Box::new(PullRequestNode::new().with_runner(noop_runner())));
    registry.register(Box::new(EmitStateNode::new().with_runner(noop_runner())));

    let schema = graph::schema();
    Workflow::new_validated(registry, schema)
        .expect("SDLC_FLOW declared graph must pass WorkflowValidator::validate")
}

/// `cheap-fast` (`review_mode: TrivialSkip`) over a trivial diff: the
/// triaged `PASS` classifies `trivial: true`, so `TriageRouterNode` routes
/// straight to `UpdateTaskStatusNode` and `ConsolidatedReviewNode` never
/// runs — its `NodeRun` stays `PENDING` and the spy transport is never
/// called.
#[tokio::test]
async fn trivial_skip_profile_skips_review_on_trivial_diff() {
    let worktree = temp_worktree("trivial-skip");
    write_fixture_tasks(&worktree, 3);

    let policy = policy::resolve(
        SdlcPolicy::default(),
        None,
        Some(&profiles::cheap_fast()),
        None,
    );
    assert_eq!(policy.review_mode, ReviewMode::TrivialSkip);

    let review_calls = Arc::new(AtomicUsize::new(0));
    let workflow = build_task_loop_workflow(
        &worktree,
        &policy,
        trivial_diff_runner(),
        Some(counting_review_transport(Arc::clone(&review_calls))),
    );

    let event = json!({ "spec_slug": "fixture-profiles-spec", "auto_pr": false });
    let final_ctx = workflow
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("workflow run should not error");

    assert_eq!(
        review_calls.load(Ordering::SeqCst),
        0,
        "ConsolidatedReviewNode's transport must not be called on the trivial-skip path"
    );
    assert_eq!(
        final_ctx.node_runs["ConsolidatedReviewNode"].status,
        NodeRunStatus::Pending,
        "ConsolidatedReviewNode must never have run on the trivial-skip path"
    );
    assert_eq!(
        final_ctx.node_runs["UpdateTaskStatusNode"].status,
        NodeRunStatus::Success,
        "the trivial-skip path must still reach UpdateTaskStatusNode"
    );

    let _ = std::fs::remove_dir_all(&worktree);
}

/// Contrast: `baseline` (`review_mode: PerTask`, the built-in default) over
/// the same trivial-diff fixture still routes every `PASS` through
/// `ConsolidatedReviewNode` — proving the skip above is `cheap-fast`'s
/// `TrivialSkip` mode at work, not an artifact of the fixture/harness.
#[tokio::test]
async fn baseline_profile_still_reaches_review_on_same_trivial_diff() {
    let worktree = temp_worktree("per-task-review");
    write_fixture_tasks(&worktree, 3);

    let policy = policy::resolve(
        SdlcPolicy::default(),
        None,
        Some(&profiles::baseline()),
        None,
    );
    assert_eq!(policy.review_mode, ReviewMode::PerTask);

    let review_calls = Arc::new(AtomicUsize::new(0));
    let workflow = build_task_loop_workflow(
        &worktree,
        &policy,
        trivial_diff_runner(),
        Some(counting_review_transport(Arc::clone(&review_calls))),
    );

    let event = json!({ "spec_slug": "fixture-profiles-spec", "auto_pr": false });
    let final_ctx = workflow
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("workflow run should not error");

    assert_eq!(
        review_calls.load(Ordering::SeqCst),
        1,
        "ConsolidatedReviewNode's transport must be called exactly once on the per_task path"
    );
    assert_eq!(
        final_ctx.node_runs["ConsolidatedReviewNode"].status,
        NodeRunStatus::Success,
        "ConsolidatedReviewNode must have run on the per_task path"
    );

    let _ = std::fs::remove_dir_all(&worktree);
}

/// A minimal in-process HTTP/1.1 stub server bound to a loopback port,
/// standing in for a local Ollama-shaped OpenAI-compat endpoint. Real
/// `reqwest` traffic over real TCP, but never leaves `127.0.0.1` and spawns
/// no subprocess — this is what lets the `local`-tier test drive the actual
/// production `openai_compat_transport_live` seam `graph::registry_for_policy`
/// wires in, rather than reimplementing its rewiring logic in the test.
/// Returns the endpoint's base URL and an `AtomicUsize` counting requests
/// received — the "transport spy" this test observes the rewiring through.
async fn spawn_local_http_stub(content: &str) -> (String, Arc<AtomicUsize>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("stub server should bind a loopback port");
    let addr = listener
        .local_addr()
        .expect("bound listener should have a local address");

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_task = Arc::clone(&calls);
    let body = json!({
        "choices": [{ "message": { "content": content } }],
        "usage": { "prompt_tokens": 3, "completion_tokens": 4 },
    })
    .to_string();

    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            calls_for_task.fetch_add(1, Ordering::SeqCst);
            let body = body.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                // Best-effort drain of the request; the fixed small JSON
                // request body always fits in one read for this test.
                let _ = socket.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });

    (format!("http://{addr}"), calls)
}

/// A `local`-tier `review` policy resolves `ConsolidatedReviewNode`'s
/// transport (via `graph::registry_for_policy`) to `openai_compat_transport`
/// pointed at `policy.local.endpoint` — proven by actually driving the
/// resolved-through-`registry_for_policy` node against a stub local
/// endpoint and observing (a) the stub's request-count spy incremented
/// exactly once, and (b) the resulting `NodeRun.usage.model` carries the
/// `local/<model>` marker `openai_compat_transport` synthesizes, which the
/// real `claude-sonnet-4-5` transport never would. `ImplementTaskNode`'s
/// node identity/registration is asserted unchanged, documenting that
/// `registry_for_policy` never rewires it (the `local` tier is scoped to
/// single-shot judgment stages, never the agentic `implement` stage).
#[tokio::test]
async fn local_tier_review_rewires_transport_to_local_endpoint() {
    let worktree = temp_worktree("local-tier");
    write_fixture_tasks(&worktree, 3);

    let (endpoint, http_calls) = spawn_local_http_stub(
        &json!({ "verdict": "PASS", "summary": "ok", "issues": [] }).to_string(),
    )
    .await;

    let policy = SdlcPolicy {
        model_tiers: ModelTiers {
            review: ModelTier::Local,
            ..ModelTiers::default()
        },
        local: LocalConfig {
            endpoint,
            model: "stub-local-model".to_string(),
            constrained_json: false,
        },
        review_mode: ReviewMode::PerTask,
        ..SdlcPolicy::default()
    };

    // `registry_for_policy` never rewires `ImplementTaskNode` regardless of
    // tier — assert the node identity is present exactly as `graph.rs`'s own
    // `registry_for_policy_never_rewires_implement_task_node` documents.
    let policy_registry = graph::registry_for_policy(&policy);
    assert!(policy_registry.contains("ImplementTaskNode"));
    assert_eq!(policy_registry.len(), graph::registry().len());

    // `review_transport_override: None` — deliberately keep whatever
    // `registry_for_policy` wired `ConsolidatedReviewNode` to, so this test
    // observes the real production rewiring rather than a test-local spy
    // standing in for it.
    let workflow = build_task_loop_workflow(&worktree, &policy, trivial_diff_runner(), None);

    let event = json!({ "spec_slug": "fixture-profiles-spec", "auto_pr": false });
    let final_ctx = workflow
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("workflow run should not error");

    assert_eq!(
        http_calls.load(Ordering::SeqCst),
        1,
        "the local stub endpoint must have received exactly one request from \
         ConsolidatedReviewNode's rewired transport"
    );

    let review_run = &final_ctx.node_runs["ConsolidatedReviewNode"];
    assert_eq!(review_run.status, NodeRunStatus::Success);
    let usage = review_run
        .usage
        .as_ref()
        .expect("ConsolidatedReviewNode should have stamped usage");
    assert!(
        usage.model.starts_with("local/"),
        "expected a 'local/<model>' usage marker proving the local transport ran, got: {}",
        usage.model
    );
    assert_eq!(usage.model, "local/stub-local-model");

    let _ = std::fs::remove_dir_all(&worktree);
}
