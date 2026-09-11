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
use std::sync::Arc;

use serde_json::json;

use engine_contract::TaskContext;
use engine_core::node::Node;
use engine_core::workflows::orchestration::execute::FlowInvocation;
use engine_core::workflows::orchestration::graph::{OrchestrationRunNode, NODE_NAME};

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

/// This fleet's REAL HQ `planning/harness.json` — reached from this repo at
/// `../../planning/harness.json` (engine-rs has no `brain.toml`; HQ is the
/// brain root a real chain resolves against, per this file's module doc).
/// Looks for both `child_sdlc_flow_policy` and `child_sdlc_task_policy`
/// under `orchestration.policy`, and SKIPS CLEANLY (never fails) when
/// either the file or the `orchestration` key is absent — that write is
/// made afterward, by hand, by the orchestrating lane (this block's own
/// notes), not by this task.
#[test]
fn hq_orchestration_policy_real_hq_file_sets_the_switches() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    // crates/engine-core -> engine-rs repo root -> core -> agentic-portfolio (HQ root).
    let hq_harness_path = manifest_dir.join("../../../../planning/harness.json");

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
}
