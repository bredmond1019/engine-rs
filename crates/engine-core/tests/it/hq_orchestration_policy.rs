//! `EN.17.F` task 7 — proves the EFFECTIVE switch the block's own notes
//! describe: `OrchestrationRunNode` resolves its policy from the event's
//! `brain_root` (`resolve_policy_for_run_from(&PolicyConfigSource::
//! Worktree(event.brain_root))`, `graph.rs`), so a chain reaching a real
//! child only gets JS-parity review/escalation shape when the BRAIN
//! ROOT's own `planning/harness.json` carries `orchestration.policy`'s
//! `child_sdlc_flow_policy`/`child_sdlc_task_policy` keys — engine-rs's own
//! `planning/harness.json` never governs a real chain (this repo has no
//! `brain.toml`).
//!
//! Two tests:
//! - [`hq_orchestration_policy_brain_root_child_policy_reaches_the_child`] —
//!   a temp brain root proves the mechanism end to end, hermetically.
//! - [`hq_orchestration_policy_real_hq_file_sets_the_switches`] — reads
//!   THIS FLEET's real HQ `planning/harness.json` and looks for both keys,
//!   skipping cleanly (not failing) when they are absent — per the task's
//!   own contract, that write is made afterward, by hand, by the
//!   orchestrating lane, never by this task.
//!
//! STANDING RULE 8: this file is a `mod` of `tests/it/main.rs`, never a new
//! `crates/engine-core/tests/*.rs` binary.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;

use engine_contract::TaskContext;
use engine_core::node::Node;
use engine_core::policy::PolicyConfigSource;
use engine_core::repo_registry::RepoRegistry;
use engine_core::workflows::orchestration::chain::ChainStep;
use engine_core::workflows::orchestration::execute::{EngineKind, FlowInvocation, FlowRunner};
use engine_core::workflows::orchestration::gates::{AdmissionGate, DependencyEdge};
use engine_core::workflows::orchestration::graph::{
    resolve_policy_for_run_from, OnBail, OrchestrationRunNode, NODE_NAME,
};
use engine_core::workflows::orchestration::integrate::{
    integrate_chain_with_coord_and_policy, ChainReport, NeverHeld, StepProgress,
};
use engine_core::WorkflowError;

/// A tempdir `brain.toml` + one real repo directory, mirroring
/// `orchestration.rs`'s own `isolation_matrix_brain_root` (private to that
/// module, so duplicated here rather than reused).
fn one_repo_brain_root() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("repo-a")).unwrap();
    std::fs::create_dir_all(
        dir.path()
            .join("planning")
            .join("roadmaps")
            .join("hq-orchestration-policy-fixture"),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("brain.toml"),
        "[[repos]]\nslug = \"repo-a\"\nrepo_path = \"repo-a\"\n",
    )
    .unwrap();
    dir
}

/// Writes `<brain_root>/planning/harness.json` with an `orchestration.policy`
/// block carrying `child_sdlc_flow_policy` — a surgical single-purpose
/// fixture write (this is a tempdir fixture, not a tracked file, so a
/// full-object write is fine here — CLAUDE.md trap 3 governs edits to
/// TRACKED corpus JSON, which this is not).
fn write_child_flow_policy_harness(brain_root: &Path, review_mode: &str) {
    let harness = json!({
        "orchestration": {
            "policy": {
                "child_sdlc_flow_policy": { "review_mode": review_mode }
            }
        }
    });
    std::fs::create_dir_all(brain_root.join("planning")).unwrap();
    std::fs::write(
        brain_root.join("planning").join("harness.json"),
        serde_json::to_string_pretty(&harness).unwrap(),
    )
    .unwrap();
}

/// `EN.17.B` task 6: writes `<brain_root>/planning/harness.json` with an
/// `orchestration.policy.on_bail` switch and, deliberately, no
/// `orchestration.profiles` section at all — the PROFILE RULE this block's
/// own notes describe (`graph.rs`'s `cheap_fast`/`thorough` doc comments):
/// an HQ profile section would replace the built-in bundle wholesale rather
/// than merge onto it, so this fixture never adds one, matching the real
/// HQ file's own shape.
fn write_on_bail_harness(brain_root: &Path, on_bail: &str) {
    let harness = json!({
        "orchestration": {
            "policy": { "on_bail": on_bail }
        }
    });
    std::fs::create_dir_all(brain_root.join("planning")).unwrap();
    std::fs::write(
        brain_root.join("planning").join("harness.json"),
        serde_json::to_string_pretty(&harness).unwrap(),
    )
    .unwrap();
}

fn event_ctx(event: serde_json::Value) -> TaskContext {
    TaskContext {
        event,
        nodes: HashMap::new(),
        metadata: json!({}),
        node_runs: HashMap::new(),
    }
}

/// A `FlowRunner` that records every [`FlowInvocation`] it receives, writes
/// a `"done"` state file into the invocation's own `repo_path` (satisfying
/// `integrate_chain`'s state-write verification), and returns an empty
/// successful `TaskContext` — mirrors `orchestration.rs`'s own
/// `isolation_matrix_run_flow`.
fn recording_run_flow() -> (
    engine_core::workflows::orchestration::execute::FlowRunner,
    Arc<std::sync::Mutex<Vec<FlowInvocation>>>,
) {
    let calls: Arc<std::sync::Mutex<Vec<FlowInvocation>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let runner: engine_core::workflows::orchestration::execute::FlowRunner =
        Arc::new(move |invocation: FlowInvocation| {
            let dir = invocation
                .repo_path
                .join("planning")
                .join(&invocation.block_id)
                .join("sdlc");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("sdlc-flow-state.json"),
                json!({ "status": "done" }).to_string(),
            )
            .unwrap();
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

async fn run_one_block_chain(brain_root: &Path) -> Arc<std::sync::Mutex<Vec<FlowInvocation>>> {
    let (run_flow, calls) = recording_run_flow();
    let node = OrchestrationRunNode::new().with_run_flow(run_flow);
    let ctx = TaskContext {
        event: json!({
            "brain_root": brain_root,
            "blocks": [{ "repo": "repo-a", "block_id": "A.1" }],
            "roadmap_slug": "hq-orchestration-policy-fixture",
        }),
        nodes: HashMap::new(),
        metadata: json!({}),
        node_runs: HashMap::new(),
    };
    let out = node
        .process(ctx)
        .await
        .unwrap_or_else(|err| panic!("orchestration run should succeed: {err}"));
    let blocks = out.nodes[NODE_NAME]["blocks"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        blocks.len(),
        1,
        "the one-block chain must have run through to completion: {:?}",
        out.nodes[NODE_NAME]
    );
    calls
}

/// THE MECHANISM: a temp brain root whose `planning/harness.json` sets
/// `orchestration.policy.child_sdlc_flow_policy: {review_mode: end_only}`
/// — with NO inline event `policy` override — composes a child `FlowInvocation`
/// carrying that override, which resolves (via `sdlc_flow::setup::
/// resolve_policy_for_run_from`, matching a real child's own resolution) to
/// `review_mode == EndOnly`. A brain root with no `orchestration` key at
/// all resolves the child to `PerTask`, the built-in default — proving the
/// switch is the brain root's file, not some other ambient default.
#[tokio::test]
async fn hq_orchestration_policy_brain_root_child_policy_reaches_the_child() {
    // (a) brain root WITH the switch.
    let dir = one_repo_brain_root();
    write_child_flow_policy_harness(dir.path(), "end_only");
    let calls = run_one_block_chain(dir.path()).await;
    let forwarded = {
        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        recorded[0].child_sdlc_flow_policy.clone().expect(
            "child_sdlc_flow_policy must be forwarded when the brain root's harness.json sets it",
        )
    };

    let child_ctx = TaskContext {
        event: json!({ "spec_slug": "brain-root-fixture", "policy": forwarded }),
        nodes: HashMap::new(),
        metadata: json!({}),
        node_runs: HashMap::new(),
    };
    let resolved = engine_core::workflows::sdlc_flow::setup::resolve_policy_for_run_from(
        &child_ctx,
        &engine_core::policy::PolicyConfigSource::Builtin,
    )
    .expect("child policy resolves");
    assert_eq!(
        resolved.review_mode,
        engine_core::workflows::sdlc_flow::policy::ReviewMode::EndOnly,
        "a child SDLC_FLOW's RESOLVED policy must read EndOnly when the brain root's \
         orchestration.policy.child_sdlc_flow_policy sets review_mode: end_only"
    );

    // (b) brain root with NO orchestration key at all — the child resolves
    // to the built-in default, PerTask.
    let dir_no_switch = one_repo_brain_root();
    // No harness.json written at all — `PolicyConfigSource::Worktree` over
    // an absent file resolves gracefully to no defaults (the same
    // "no config file" fallback `PolicyConfigSource::Builtin` uses).
    let calls_no_switch = run_one_block_chain(dir_no_switch.path()).await;
    let recorded_no_switch = calls_no_switch.lock().unwrap();
    assert_eq!(recorded_no_switch.len(), 1);
    assert!(
        recorded_no_switch[0].child_sdlc_flow_policy.is_none(),
        "with no orchestration.policy key at all, nothing is forwarded"
    );
}

// ── `EN.17.B` task 6 — `orchestration.policy.on_bail` resolution ────────

/// A temp brain root's `planning/harness.json` carrying
/// `orchestration.policy.on_bail: skip_dependents` (and no
/// `orchestration.profiles` at all) resolves `OnBail::SkipDependents`; a
/// fixture root with no `orchestration` key at all resolves the built-in
/// default, `OnBail::StopChain`.
#[test]
fn hq_orchestration_policy_fixture_root_resolves_on_bail() {
    let dir = one_repo_brain_root();
    write_on_bail_harness(dir.path(), "skip_dependents");
    let source = PolicyConfigSource::Worktree(dir.path().to_path_buf());
    let ctx = event_ctx(json!({ "brain_root": dir.path() }));
    let resolved = resolve_policy_for_run_from(&ctx, &source).expect("resolves");
    assert_eq!(resolved.on_bail, OnBail::SkipDependents);

    let dir_no_switch = one_repo_brain_root();
    let source_no_switch = PolicyConfigSource::Worktree(dir_no_switch.path().to_path_buf());
    let ctx_no_switch = event_ctx(json!({ "brain_root": dir_no_switch.path() }));
    let resolved_no_switch = resolve_policy_for_run_from(&ctx_no_switch, &source_no_switch)
        .expect("resolves with no orchestration key at all");
    assert_eq!(resolved_no_switch.on_bail, OnBail::StopChain);
}

/// `cheap-fast` and `thorough` both leave `on_bail` unset (`graph.rs`'s own
/// PROFILE RULE comment on each), so the HQ brain-root switch flows through
/// unchanged. `baseline` explicitly restates `OnBail::StopChain` — an event
/// `profile` outranks the harness-file default in
/// `crate::policy::resolve`'s precedence, so `baseline` wins over the HQ
/// switch even though `baseline` merely restates the built-in value.
#[test]
fn hq_orchestration_policy_profiles_do_not_override_the_brain_root_switch() {
    let dir = one_repo_brain_root();
    write_on_bail_harness(dir.path(), "skip_dependents");
    let source = PolicyConfigSource::Worktree(dir.path().to_path_buf());

    for profile in ["cheap-fast", "thorough"] {
        let ctx = event_ctx(json!({ "brain_root": dir.path(), "profile": profile }));
        let resolved = resolve_policy_for_run_from(&ctx, &source).expect("resolves");
        assert_eq!(
            resolved.on_bail,
            OnBail::SkipDependents,
            "profile '{profile}' must not override the HQ on_bail switch"
        );
    }

    let ctx_baseline = event_ctx(json!({ "brain_root": dir.path(), "profile": "baseline" }));
    let resolved_baseline = resolve_policy_for_run_from(&ctx_baseline, &source).expect("resolves");
    assert_eq!(
        resolved_baseline.on_bail,
        OnBail::StopChain,
        "baseline explicitly restates StopChain, which outranks the harness default"
    );
}

/// A chain run through `OrchestrationRunNode` from a fixture brain root,
/// with no inline event `policy` override, applies skip-dependents purely
/// because that brain root's own `planning/harness.json` sets
/// `orchestration.policy.on_bail: skip_dependents` — the independent step
/// after a bail is still dispatched, and the dependent never is.
#[tokio::test]
async fn hq_orchestration_policy_node_run_uses_brain_root_policy() {
    let dir = one_repo_brain_root();
    write_on_bail_harness(dir.path(), "skip_dependents");

    let calls: Arc<Mutex<Vec<FlowInvocation>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let run_flow: FlowRunner = Arc::new(move |invocation: FlowInvocation| {
        recorded.lock().unwrap().push(invocation.clone());
        Box::pin(async move {
            if invocation.block_id == "A.1" {
                return Err(WorkflowError::new("simulated failure for A.1".to_string()));
            }
            let dir = invocation
                .repo_path
                .join("planning")
                .join(&invocation.block_id)
                .join("sdlc");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("sdlc-flow-state.json"),
                json!({ "status": "done" }).to_string(),
            )
            .unwrap();
            Ok(TaskContext {
                event: json!({}),
                nodes: HashMap::new(),
                metadata: json!({}),
                node_runs: HashMap::new(),
            })
        })
    });

    let node = OrchestrationRunNode::new()
        .with_run_flow(run_flow)
        .with_resolve_depends_on(Arc::new(|repo: &str, id: &str| -> Vec<DependencyEdge> {
            if repo == "repo-a" && id == "B.1" {
                vec![DependencyEdge::Block {
                    repo: "repo-a".to_string(),
                    block_id: "A.1".to_string(),
                }]
            } else {
                Vec::new()
            }
        }));

    let ctx = event_ctx(json!({
        "brain_root": dir.path(),
        "blocks": [
            { "repo": "repo-a", "block_id": "A.1" },
            { "repo": "repo-a", "block_id": "B.1" },
            { "repo": "repo-a", "block_id": "C.1" },
        ],
        "roadmap_slug": "hq-orchestration-policy-fixture",
    }));

    let err = node
        .process(ctx)
        .await
        .expect_err("A.1 bails, so the node itself reports the run as failed");
    assert!(err.message.contains("A.1"));

    // `EN.17.B`: the SOFT-Err path — `integrate_chain` returned `Ok`
    // (skip_dependents let C.1 run), but `chain_report.bailed` is non-empty,
    // so `process` returns `Err`. `workflow.rs`'s `node_context` reverts
    // `ctx` on `Err` and replays only `err.node_result`, so the report must
    // ride out on the error itself or it never reaches `ctx.nodes`.
    let node_result = err
        .node_result
        .as_ref()
        .expect("the soft-Err path must carry node_result so chain_report survives the ctx revert");
    assert_eq!(
        node_result["chain_report"]["bailed"],
        json!(["repo-a:A.1"]),
        "chain_report in the carried node_result must name the bailed block"
    );
    assert_eq!(
        node_result["steps_integrated"], json!(1),
        "the carried payload is the full success-path stamp, not a chain_report-only stub: {node_result}"
    );

    let dispatched: Vec<String> = calls
        .lock()
        .unwrap()
        .iter()
        .map(|inv| inv.block_id.clone())
        .collect();
    assert!(
        dispatched.contains(&"C.1".to_string()),
        "under skip_dependents (resolved from the brain root's own harness.json), \
         the independent step C.1 must still have been dispatched: {dispatched:?}"
    );
    assert!(
        !dispatched.contains(&"B.1".to_string()),
        "B.1 depends on the bailed A.1 and must never be dispatched: {dispatched:?}"
    );
}

/// `EN.17.D` task 5: writes `<brain_root>/planning/harness.json` with an
/// `orchestration.policy.preflight_enabled: true` switch and nothing else —
/// no `orchestration.profiles` section, matching the real HQ file's shape
/// and this file's other `write_*_harness` fixtures.
fn write_preflight_enabled_harness(brain_root: &Path) {
    let harness = json!({
        "orchestration": {
            "policy": { "preflight_enabled": true }
        }
    });
    std::fs::create_dir_all(brain_root.join("planning")).unwrap();
    std::fs::write(
        brain_root.join("planning").join("harness.json"),
        serde_json::to_string_pretty(&harness).unwrap(),
    )
    .unwrap();
}

/// `EN.17.D` task 5: a temp brain root whose `planning/harness.json` sets
/// `orchestration.policy.preflight_enabled: true` — with NO inline event
/// `policy` override — reaches a real `OrchestrationRunNode` chain run:
/// the resolved knob is stamped into the node's own `preflight_report`
/// (`enabled: true`), exactly as `EN.17.D` task 4's
/// `process_stamps_resolved_preflight_knobs_and_one_entry_per_block_into_preflight_report`
/// proves for an INLINE policy override. This is the harness-file half of
/// that same mechanism, reusing this file's own `one_repo_brain_root`/
/// `event_ctx` fixtures rather than new ones. The node's own `preflight`
/// seam is left at its default (`with_run_flow` only, no `with_preflight`
/// injected) — production wiring of the argv-validating seam itself is
/// `engine-serve`'s `build_preflight_seam` (task 4), out of scope here;
/// this proves only that the brain root's file is what the resolved
/// `OrchestrationPolicy.preflight_enabled` reads, not the runtime
/// validator.
#[tokio::test]
async fn hq_orchestration_policy_brain_root_enables_preflight() {
    let dir = one_repo_brain_root();
    write_preflight_enabled_harness(dir.path());

    let (run_flow, calls) = recording_run_flow();
    let node = OrchestrationRunNode::new().with_run_flow(run_flow);
    let ctx = event_ctx(json!({
        "brain_root": dir.path(),
        "blocks": [{ "repo": "repo-a", "block_id": "A.1" }],
        "roadmap_slug": "hq-orchestration-policy-fixture",
    }));
    let out = node
        .process(ctx)
        .await
        .unwrap_or_else(|err| panic!("orchestration run should succeed: {err}"));
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(
        out.nodes[NODE_NAME]["preflight_report"]["enabled"],
        json!(true),
        "the brain root's harness.json orchestration.policy.preflight_enabled must reach \
         OrchestrationRunNode with no inline policy override: {:?}",
        out.nodes[NODE_NAME]["preflight_report"]
    );

    // A sibling brain root with no `orchestration` key at all resolves to
    // the built-in default, `false` — proving the switch is the brain
    // root's file, not some other ambient default.
    let dir_no_switch = one_repo_brain_root();
    let (run_flow_no_switch, _calls_no_switch) = recording_run_flow();
    let node_no_switch = OrchestrationRunNode::new().with_run_flow(run_flow_no_switch);
    let ctx_no_switch = event_ctx(json!({
        "brain_root": dir_no_switch.path(),
        "blocks": [{ "repo": "repo-a", "block_id": "A.1" }],
        "roadmap_slug": "hq-orchestration-policy-fixture",
    }));
    let out_no_switch = node_no_switch
        .process(ctx_no_switch)
        .await
        .unwrap_or_else(|err| panic!("orchestration run should succeed: {err}"));
    assert_eq!(
        out_no_switch.nodes[NODE_NAME]["preflight_report"]["enabled"],
        json!(false),
        "with no orchestration.policy key at all, preflight_enabled must resolve to its \
         built-in default, false"
    );
}

/// `EN.17.E` task 1/4: writes `<brain_root>/planning/harness.json` with an
/// `orchestration.policy.inbox_triage_enabled: true` switch and nothing else
/// — no `orchestration.profiles` section, matching the real HQ file's shape
/// and this file's other `write_*_harness` fixtures.
fn write_inbox_triage_enabled_harness(brain_root: &Path) {
    let harness = json!({
        "orchestration": {
            "policy": { "inbox_triage_enabled": true }
        }
    });
    std::fs::create_dir_all(brain_root.join("planning")).unwrap();
    std::fs::write(
        brain_root.join("planning").join("harness.json"),
        serde_json::to_string_pretty(&harness).unwrap(),
    )
    .unwrap();
}

/// `EN.17.E` task 4: a temp brain root whose `planning/harness.json` sets
/// `orchestration.policy.inbox_triage_enabled: true` resolves to
/// `inbox_triage_enabled == true` through `resolve_policy_for_run_from` with
/// no inline policy override — mirrors
/// `hq_orchestration_policy_brain_root_enables_preflight`'s shape for the
/// analogous `EN.17.D` switch. `OrchestrationRunNode` is now ACTUALLY WIRED
/// to this switch (`EN.17.E` task 7, `with_inbox_triage_runner`) — see
/// [`hq_orchestration_policy_inbox_triage_runner_processes_a_real_message`]
/// immediately below for the full-node proof (a stub `JudgmentNode`
/// transport, a real FINDING message, a landed reply and a non-empty
/// `inbox_report` node stamp). This test stays as the narrower policy-only
/// proof.
#[tokio::test]
async fn hq_orchestration_policy_brain_root_enables_inbox_triage() {
    let dir = one_repo_brain_root();
    write_inbox_triage_enabled_harness(dir.path());
    let ctx = event_ctx(json!({}));
    let resolved = resolve_policy_for_run_from(
        &ctx,
        &PolicyConfigSource::Worktree(dir.path().to_path_buf()),
    )
    .expect("resolves against the temp brain root's harness.json");
    assert!(
        resolved.inbox_triage_enabled,
        "the brain root's harness.json orchestration.policy.inbox_triage_enabled must reach \
         OrchestrationPolicy with no inline policy override"
    );

    // A sibling brain root with no `orchestration` key at all resolves to the built-in
    // default, `false` — proving the switch is the brain root's file, not some other
    // ambient default.
    let dir_no_switch = one_repo_brain_root();
    let ctx_no_switch = event_ctx(json!({}));
    let resolved_no_switch = resolve_policy_for_run_from(
        &ctx_no_switch,
        &PolicyConfigSource::Worktree(dir_no_switch.path().to_path_buf()),
    )
    .expect("resolves against a brain root with no orchestration key");
    assert!(
        !resolved_no_switch.inbox_triage_enabled,
        "with no orchestration.policy key at all, inbox_triage_enabled must resolve to its \
         built-in default, false"
    );
}

/// Writes a message envelope directly into
/// `<lock_dir>/queue/<to_repo>/<to_lane>/inbox/` — standing in for a sibling
/// lane's delivery. Duplicated from `tests/it/inbox_triage.rs`'s own
/// `write_message` (that module's stated convention: fixture helpers are
/// duplicated per test module rather than exported across the crate
/// boundary), trimmed to only the fields this file's one FINDING test needs.
fn write_finding_message(lock_dir: &Path, to_repo: &str, to_lane: &str) {
    let inbox = lock_dir
        .join("queue")
        .join(to_repo)
        .join(to_lane)
        .join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    let envelope = json!({
        "message_id": "aaaaaaaa-1111-4e21-9f10-000000000099",
        "sender": {
            "agent_name": "peer-lane",
            "repo": "bastion",
            "lane": "types",
            "roadmap": "hq-orchestration-policy-fixture",
        },
        "sent_at": "2026-09-12T00:00:00Z",
        "kind": "FINDING",
        "subject": { "repo": to_repo, "block": "A.1" },
        "body": "a finding delivered mid-run",
        "durable_home": {
            "channel": "lane-log",
            "ref": "lane-log.jsonl#1",
        },
        "verified_by": "test fixture",
    });
    std::fs::write(
        inbox.join("20260912T000000000000000-aaaaaaaa-1111-4e21-9f10-000000000099.json"),
        serde_json::to_string(&envelope).unwrap(),
    )
    .unwrap();
}

/// A stub [`engine_core::workflows::ModelTransport`] that always returns one canned
/// `InboxVerdict` JSON reply — `ACCEPTED`/`NONE` — so the gated suite never spawns a real
/// `claude` subprocess. Mirrors `tests/it/inbox_triage.rs`'s own `queued_transport`, trimmed
/// to a single fixed reply since this test drives exactly one FINDING.
fn accepted_transport() -> engine_core::workflows::ModelTransport {
    use claude_code_rs::parse::Usage as SdkUsage;
    use claude_code_rs::{Config, Outcome};

    Arc::new(move |_config: Config, _prompt: String| {
        Box::pin(async move {
            Ok(Outcome {
                cost_usd: 0.0,
                usage: SdkUsage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
                model_usage: std::collections::BTreeMap::new(),
                text: json!({"verdict": "ACCEPTED", "action": "NONE", "reason": "noted"})
                    .to_string(),
                is_error: false,
                api_error_status: None,
                session_id: None,
                structured_output: None,
            })
        })
    })
}

/// `EN.17.E` task 7 — the production wiring's own regression: with
/// `inbox_triage_enabled` resolved `true` from the brain root's
/// `planning/harness.json` (no inline policy override, mirroring
/// [`hq_orchestration_policy_brain_root_enables_inbox_triage`] immediately
/// above) and a stub-transport [`InboxTriageRunner`] injected via
/// [`OrchestrationRunNode::with_inbox_triage_runner`] — the same builder
/// shape `with_preflight` already proved in
/// [`hq_orchestration_policy_brain_root_enables_preflight`] — a FINDING
/// message sitting in the run's own inbox (`<brain_root>/.fleet-locks/queue/
/// repo-a/repo-a/inbox/`, `repo-a` doubling as both repo and lane per
/// `process`'s own "single-repo lines where lane == repo" convention) is
/// drained, judged, and answered: a reply lands in the sender's
/// (`bastion`/`types`) inbox, and `inbox_report` is non-empty in the node's
/// own `ctx.nodes` result. This replaces
/// `hq_orchestration_policy_brain_root_enables_inbox_triage`'s prior
/// documented gap ("OrchestrationRunNode has no inbox-triage wiring yet ...
/// out of this task's scope").
#[tokio::test]
async fn hq_orchestration_policy_inbox_triage_runner_processes_a_real_message() {
    use engine_core::workflows::orchestration::inbox_triage::{
        InboxTriageConfig, InboxTriageRunner,
    };

    let dir = one_repo_brain_root();
    write_inbox_triage_enabled_harness(dir.path());
    write_finding_message(&dir.path().join(".fleet-locks"), "repo-a", "repo-a");

    let (run_flow, calls) = recording_run_flow();
    let inbox_runner = Arc::new(
        InboxTriageRunner::new(InboxTriageConfig::default()).with_transport(accepted_transport()),
    );
    let node = OrchestrationRunNode::new()
        .with_run_flow(run_flow)
        .with_coord_agent("engine-rs-inbox-triage-test")
        .with_inbox_triage_runner(inbox_runner);
    let ctx = event_ctx(json!({
        "brain_root": dir.path(),
        "blocks": [{ "repo": "repo-a", "block_id": "A.1" }],
        "roadmap_slug": "hq-orchestration-policy-fixture",
    }));

    let out = node
        .process(ctx)
        .await
        .unwrap_or_else(|err| panic!("orchestration run should succeed: {err}"));
    assert_eq!(calls.lock().unwrap().len(), 1);

    let inbox_report = out.nodes[NODE_NAME]["inbox_report"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        inbox_report.len(),
        1,
        "the FINDING message must have been drained and judged exactly once: {:?}",
        out.nodes[NODE_NAME]["inbox_report"]
    );
    assert_eq!(inbox_report[0]["verdict"], json!("ACCEPTED"));

    let reply_inbox = dir
        .path()
        .join(".fleet-locks")
        .join("queue")
        .join("bastion")
        .join("types")
        .join("inbox");
    let entries: Vec<_> = std::fs::read_dir(&reply_inbox)
        .unwrap_or_else(|err| panic!("reply inbox {reply_inbox:?} must exist: {err}"))
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one reply written to the sender's own inbox"
    );
}

/// With `inbox_triage_enabled` false (the built-in default), a real
/// `OrchestrationRunNode::process` run's behavior and node_result shape are
/// unchanged apart from the new, empty `inbox_report` key — no
/// `InboxTriageRunner` is ever constructed, and the FINDING message sitting
/// in the inbox is left undrained (no reply, no completion) rather than
/// acted on.
#[tokio::test]
async fn hq_orchestration_policy_inbox_triage_disabled_leaves_node_result_unchanged() {
    let dir = one_repo_brain_root();
    // No harness.json at all — `inbox_triage_enabled` resolves to its
    // built-in default, `false`.
    write_finding_message(&dir.path().join(".fleet-locks"), "repo-a", "repo-a");

    let (run_flow, calls) = recording_run_flow();
    let node = OrchestrationRunNode::new()
        .with_run_flow(run_flow)
        .with_coord_agent("engine-rs-inbox-triage-disabled-test");
    let ctx = event_ctx(json!({
        "brain_root": dir.path(),
        "blocks": [{ "repo": "repo-a", "block_id": "A.1" }],
        "roadmap_slug": "hq-orchestration-policy-fixture",
    }));

    let out = node
        .process(ctx)
        .await
        .unwrap_or_else(|err| panic!("orchestration run should succeed: {err}"));
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(
        out.nodes[NODE_NAME]["inbox_report"],
        json!([]),
        "with inbox_triage_enabled false, inbox_report must be the empty-array no-op stamp"
    );

    let reply_inbox = dir
        .path()
        .join(".fleet-locks")
        .join("queue")
        .join("bastion")
        .join("types")
        .join("inbox");
    assert!(
        !reply_inbox.exists(),
        "no reply should ever be composed when inbox triage is disabled"
    );
}

/// This fleet's REAL HQ `planning/harness.json` — reached from this repo at
/// `../../planning/harness.json` (engine-rs has no `brain.toml`; HQ is the
/// brain root a real chain resolves against, per this file's module doc).
/// Looks for `child_sdlc_flow_policy`, `child_sdlc_task_policy` AND (EN.17.B
/// task 6) `on_bail` under `orchestration.policy`, and SKIPS CLEANLY (never
/// fails) when either the file or the `orchestration` key is absent — those
/// writes are made afterward, by hand, by the orchestrating lane (each
/// block's own notes), not by this task. `#[tokio::test]` (not `#[test]`)
/// because the second half drives a real chain, async, through the public
/// `integrate_chain_with_coord_and_policy` entry point — the early-return
/// skip paths above are unaffected by running inside a tokio runtime.
#[tokio::test]
async fn hq_orchestration_policy_real_hq_file_sets_the_switches() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    // Walk up for `brain.toml` (engine_core::brain_root) rather than a
    // hardcoded `../../../../` depth: that hardcoded depth is correct in the
    // main tree but wrong inside a `trees/<branch>` worktree, where two extra
    // path components sit between `crates/engine-core` and the repo root,
    // making the old path resolve to engine-rs's own harness.json instead of
    // HQ's.
    let Ok(hq_root) = engine_core::brain_root::resolve_brain_root_from(manifest_dir) else {
        eprintln!(
            "SKIP: no HQ (brain.toml) checkout found walking up from {manifest_dir:?} — this \
             test only runs when a real HQ checkout sits alongside this repo"
        );
        return;
    };
    let hq_harness_path = hq_root.join("planning/harness.json");

    let Ok(contents) = std::fs::read_to_string(&hq_harness_path) else {
        eprintln!(
            "SKIP: HQ harness.json not found at {hq_harness_path:?} — this test only runs \
             when a real HQ checkout sits alongside this repo"
        );
        return;
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&contents) else {
        eprintln!("SKIP: HQ harness.json did not parse as JSON");
        return;
    };
    let Some(policy) = parsed.pointer("/orchestration/policy") else {
        eprintln!(
            "SKIP: HQ harness.json has no orchestration.policy key yet — the cross-tree write \
             this block's notes describe has not landed"
        );
        return;
    };

    assert!(
        policy.get("child_sdlc_flow_policy").is_some(),
        "HQ's real orchestration.policy is missing child_sdlc_flow_policy"
    );
    assert!(
        policy.get("child_sdlc_task_policy").is_some(),
        "HQ's real orchestration.policy is missing child_sdlc_task_policy"
    );
    let Some(on_bail) = policy.get("on_bail") else {
        eprintln!(
            "SKIP: HQ harness.json's orchestration.policy has no on_bail key yet — EN.17.B \
             task 5c's cross-tree write has not landed"
        );
        return;
    };
    assert_eq!(
        on_bail.as_str(),
        Some("skip_dependents"),
        "HQ's real orchestration.policy.on_bail must be 'skip_dependents' (EN.17.B task 5c)"
    );
    let Some(bail_channel) = policy.get("bail_channel") else {
        eprintln!(
            "SKIP: HQ harness.json's orchestration.policy has no bail_channel key yet -- \
             EN.17.C's cross-tree write has not landed"
        );
        return;
    };
    assert_eq!(
        bail_channel.as_str(),
        Some("notification"),
        "HQ's real orchestration.policy.bail_channel must be 'notification' (EN.17.C)"
    );
    match policy.get("preflight_enabled") {
        Some(preflight_enabled) => assert_eq!(
            preflight_enabled.as_bool(),
            Some(true),
            "HQ's real orchestration.policy.preflight_enabled must be true (EN.17.D)"
        ),
        None => eprintln!(
            "SKIP (partial): HQ harness.json's orchestration.policy has no preflight_enabled \
             key yet -- EN.17.D's cross-tree write has not landed. The rest of this test still \
             runs against on_bail/bail_channel."
        ),
    }
    match policy.get("inbox_triage_enabled") {
        Some(inbox_triage_enabled) => assert_eq!(
            inbox_triage_enabled.as_bool(),
            Some(true),
            "HQ's real orchestration.policy.inbox_triage_enabled must be true (EN.17.E)"
        ),
        None => eprintln!(
            "SKIP (partial): HQ harness.json's orchestration.policy has no inbox_triage_enabled \
             key yet -- EN.17.E's cross-tree write has not landed. The rest of this test still \
             runs against on_bail/bail_channel/preflight_enabled."
        ),
    }

    // Proves task 4's stamping with the REAL resolved value (not a
    // fixture): resolve `OrchestrationPolicy` straight from THIS file, then
    // feed its `on_bail` into a real chain and confirm the terminal
    // `ChainReport` shows the independent step closing while the dependent
    // is skipped — the effect `on_bail: skip_dependents` is contracted to
    // produce.
    let ctx = event_ctx(json!({}));
    let resolved = resolve_policy_for_run_from(
        &ctx,
        &PolicyConfigSource::HarnessFile(hq_harness_path.clone()),
    )
    .expect("resolves against the real HQ harness.json");
    assert_eq!(resolved.on_bail, OnBail::SkipDependents);

    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("repo-a")).unwrap();
    std::fs::write(
        dir.path().join("brain.toml"),
        "[[repos]]\nslug = \"repo-a\"\nrepo_path = \"repo-a\"\n",
    )
    .unwrap();
    let registry = RepoRegistry::from_brain_root(dir.path()).expect("registry");
    let chain = vec![
        ChainStep {
            repo: "repo-a".to_string(),
            block_id: "A.1".to_string(),
            directives: None,
            ..Default::default()
        },
        ChainStep {
            repo: "repo-a".to_string(),
            block_id: "B.1".to_string(),
            directives: None,
            ..Default::default()
        },
        ChainStep {
            repo: "repo-a".to_string(),
            block_id: "C.1".to_string(),
            directives: None,
            ..Default::default()
        },
    ];
    let resolve_deps = |repo: &str, id: &str| -> Vec<DependencyEdge> {
        if repo == "repo-a" && id == "B.1" {
            vec![DependencyEdge::Block {
                repo: "repo-a".to_string(),
                block_id: "A.1".to_string(),
            }]
        } else {
            Vec::new()
        }
    };
    let is_met = |_repo: &str, _id: &str| true;
    let admission = AdmissionGate::with_default_policy();
    let roadmap_dir = tempfile::tempdir().unwrap();
    let repo_a = dir.path().join("repo-a");
    std::fs::create_dir_all(repo_a.join("planning").join("C.1").join("sdlc")).unwrap();
    std::fs::write(
        repo_a
            .join("planning")
            .join("C.1")
            .join("sdlc")
            .join("sdlc-flow-state.json"),
        json!({ "status": "done" }).to_string(),
    )
    .unwrap();
    let run_flow: FlowRunner = Arc::new(move |invocation| {
        Box::pin(async move {
            if invocation.block_id == "A.1" {
                Err(WorkflowError::new("simulated failure for A.1".to_string()))
            } else {
                Ok(TaskContext {
                    event: json!({}),
                    nodes: HashMap::new(),
                    metadata: json!({}),
                    node_runs: HashMap::new(),
                })
            }
        })
    });
    let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;
    let mut report = ChainReport::default();
    let block_status = |_repo: &str, _id: &str| {
        engine_core::workflows::orchestration::corpus_gates::BlockPresence::Row("open".to_string())
    };

    #[allow(clippy::too_many_arguments)]
    let outcomes = integrate_chain_with_coord_and_policy(
        &chain,
        &resolve_deps,
        &is_met,
        &admission,
        &NeverHeld,
        Duration::from_millis(1),
        None,
        None,
        None,
        &resolve_engine,
        &registry,
        &run_flow,
        roadmap_dir.path(),
        None,
        &|_: &StepProgress| {},
        false,
        true,
        uuid::Uuid::new_v4(),
        &|_repo: &str, _id: &str| {},
        None,
        None,
        None,
        resolved.on_bail,
        resolved.bail_channel,
        &block_status,
        &mut report,
    )
    .await
    .expect("skip_dependents keeps the chain going past A.1's bail");

    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].block_id, "C.1");
    assert_eq!(report.bailed, vec!["repo-a:A.1".to_string()]);
    assert_eq!(report.closed, vec!["repo-a:C.1".to_string()]);
    assert_eq!(
        report.skipped.len(),
        1,
        "B.1 must be recorded as skipped in the terminal ChainReport"
    );
    assert_eq!(report.skipped[0].block, "repo-a:B.1");
}
