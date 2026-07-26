//! Real-CLI experiment harness (Plan `plan-sdlc-policy-profiles`, Phase 2,
//! Block D): runs the four named policy profiles (`baseline`, `cheap-fast`,
//! `pragmatist`, `batch-reviewer`) against a synthetic multi-task fixture
//! spec through the real `SDLC_FLOW` graph, aggregates each run's
//! `sdlc-flow-state.json` via [`engine_core::workflows::sdlc_flow::aggregate::aggregate_state_files`],
//! and prints a ranked cost/time/tokens/attempts/pass-rate table — proving a
//! non-default profile demonstrably changes the resolved policy actually
//! used by a run.
//!
//! **Ignored by default** so the gated `cargo test` suite (and CI) never
//! spawns a real subprocess or spends real tokens. Run manually, with a
//! logged-in `claude` CLI available on `PATH`:
//!
//! ```sh
//! cargo test -p engine-core --test sdlc_flow_experiment -- --ignored --test-threads=1
//! ```
//!
//! This file is an external integration test, so items from
//! `tests/sdlc_flow_live.rs` cannot be cross-imported — the scaffolding
//! (`temp_worktree`, `noop_runner`, `stub_outcome`, `build_*_workflow`) is
//! replicated here rather than shared. See that file's module doc for the
//! full rationale behind the real-vs-stubbed transport split and the
//! `isolated: true` / `dangerously_skip_permissions` config.
//!
//! ## The bug this file's setup node fixes
//!
//! `sdlc_flow_live.rs`'s `FixtureSetupNode` inserts the `SetupWorktreeNode`
//! result directly, **shortcutting the real setup node** — it never calls
//! [`engine_core::workflows::sdlc_flow::setup::resolve_policy_for_run`], so
//! no policy is ever stamped into `ctx.nodes`. That's fine for a live smoke
//! test that doesn't care about policy, but it would silently defeat the
//! entire point of this experiment (proving profiles change behavior).
//! [`ExperimentSetupNode`] below fixes this: it inserts the
//! `SetupWorktreeNode` result *and* calls `resolve_policy_for_run` itself,
//! stamping the result under [`setup::RESOLVED_POLICY_IDENTITY`] via
//! `ctx.nodes.insert` directly (not `put_result`, which is `pub(crate)` and
//! unavailable from an external `tests/` file).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use claude_code_rs::{Config, Outcome};
use engine_contract::TaskContext;
use engine_core::node::{Node, NodeError, NodeRegistry};
use engine_core::workflow::Workflow;
use engine_core::workflows::sdlc_flow::aggregate;
use engine_core::workflows::sdlc_flow::docs::PatchDocsNode;
use engine_core::workflows::sdlc_flow::emit_state::EmitStateNode;
use engine_core::workflows::sdlc_flow::graph;
use engine_core::workflows::sdlc_flow::pr::PullRequestNode;
use engine_core::workflows::sdlc_flow::setup::{
    self, CommandOutput, CommandRunner, GenerateTasksNode, LoadTaskStateNode, SpecExistsRouterNode,
};
use engine_core::workflows::sdlc_flow::task_loop::{
    ConsolidatedReviewNode, ImplementTaskNode, IncrementAttemptNode, ReviewRouterNode,
    SaveStateNode, TaskQueueRouterNode, TestTaskNode, TriageRouterNode, TriageTaskNode,
    UpdateTaskStatusNode,
};
use engine_core::workflows::sdlc_flow::wrap_up::WrapUpNode;
use serde_json::json;

/// Replaces the real `SetupWorktreeNode`, mirroring `sdlc_flow_live.rs`'s
/// `FixtureSetupNode` for the controlled temp-dir `worktree_path` part —
/// but, unlike that fixture node, this one also calls
/// [`setup::resolve_policy_for_run`] and stamps the result, so profiles
/// actually take effect for the rest of the graph.
struct ExperimentSetupNode {
    worktree_path: String,
}

#[async_trait::async_trait]
impl Node for ExperimentSetupNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({
                "worktree_path": self.worktree_path,
                "branch_name": "sdlc/experiment-spec",
            }),
        );

        let worktree = Path::new(&self.worktree_path);
        let resolved_policy = setup::resolve_policy_for_run(&ctx, worktree)?;
        let policy_value = serde_json::to_value(&resolved_policy).map_err(|err| {
            NodeError::new(format!("failed to serialize resolved SdlcPolicy: {err}"))
        })?;
        // `put_result` is `pub(crate)` (see `mod.rs`) and not callable from
        // an external `tests/` file, so stamp directly into `ctx.nodes`
        // under the same identity it would use.
        ctx.nodes
            .insert(setup::RESOLVED_POLICY_IDENTITY.to_string(), policy_value);

        Ok(ctx)
    }

    fn name(&self) -> &str {
        "SetupWorktreeNode"
    }
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

fn temp_worktree(tag: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "engine-core-sdlc-flow-experiment-{tag}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(dir.join("planning").join("experiment-spec")).unwrap();

    // A real `git init` so `TestTaskNode`'s write-verification guard (which
    // shells out to a real `git status --porcelain` via the default,
    // non-stubbed `CommandRunner` in this file's real-CLI harnesses) sees
    // genuine untracked/modified entries after a real `ImplementTaskNode`
    // write, instead of silently reporting "nothing changed" against a
    // directory that was never a repo at all.
    let status = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&dir)
        .status()
        .expect("git init should run");
    assert!(status.success(), "git init failed in {}", dir.display());

    dir
}

const MARKER_FILE: &str = "EXPERIMENT_MARKER.txt";
const MARKER_CONTENT: &str = "experiment-test-ok";
const RETRY_MARKER_FILE: &str = "EXPERIMENT_RETRY_MARKER.txt";
const RETRY_MARKER_CONTENT: &str = "experiment-retry-ok";
const TRIVIAL_MARKER_FILE: &str = "EXPERIMENT_TRIVIAL_MARKER.txt";
const TRIVIAL_MARKER_CONTENT: &str = "x";

/// Writes a synthetic multi-task fixture `tasks.json` (three tasks: a
/// happy-path trivial marker-file edit as in `sdlc_flow_live.rs`'s
/// `write_fixture_files`, a task deliberately framed to require a
/// first-fail-then-pass retry, and a trivial one-liner) plus a `harness.json`
/// whose gating checks really `grep` the worktree for the expected
/// file/content via the *real* `sh -c` subprocess (no stub) — the concrete,
/// independent proof each real `ImplementTaskNode` attempt actually wrote
/// into the scoped worktree.
fn write_fixture_files(worktree: &Path, max_attempts: u32) {
    let spec_dir = worktree.join("planning").join("experiment-spec");
    let tasks = json!([
        {
            "task_id": 1,
            "title": "Create a marker file",
            "description": format!(
                "Create a file named exactly `{MARKER_FILE}` in the current \
                 working directory. Its entire content must be the single \
                 line `{MARKER_CONTENT}` and nothing else. Do not create or \
                 modify any other file."
            ),
            "acceptance_criteria": [
                format!("{MARKER_FILE} exists in the working directory"),
                format!("Its content is exactly \"{MARKER_CONTENT}\""),
            ],
            "max_attempts": max_attempts,
        },
        {
            "task_id": 2,
            "title": "Create a retry marker file with an exact two-line format",
            "description": format!(
                "Create a file named exactly `{RETRY_MARKER_FILE}` in the \
                 current working directory. Its entire content must be \
                 exactly two lines: the first line must be the literal text \
                 `{RETRY_MARKER_CONTENT}` and the second line must be the \
                 literal text `confirmed`, with no trailing blank line and \
                 no other content. This exact format is easy to get wrong on \
                 a first attempt (e.g. omitting the second line), so verify \
                 both lines are present before finishing."
            ),
            "acceptance_criteria": [
                format!("{RETRY_MARKER_FILE} exists in the working directory"),
                format!("Its first line is exactly \"{RETRY_MARKER_CONTENT}\""),
                "Its second line is exactly \"confirmed\"".to_string(),
            ],
            "max_attempts": max_attempts,
        },
        {
            "task_id": 3,
            "title": "Create a trivial one-liner marker file",
            "description": format!(
                "Create a file named exactly `{TRIVIAL_MARKER_FILE}` in the \
                 current working directory containing the single character \
                 `{TRIVIAL_MARKER_CONTENT}` and nothing else."
            ),
            "acceptance_criteria": [
                format!("{TRIVIAL_MARKER_FILE} exists in the working directory"),
                format!("Its content is exactly \"{TRIVIAL_MARKER_CONTENT}\""),
            ],
            "max_attempts": max_attempts,
        }
    ]);
    std::fs::write(
        spec_dir.join("tasks.json"),
        serde_json::to_string_pretty(&tasks).unwrap(),
    )
    .unwrap();

    let harness = json!({
        "validation": {
            "checks": [
                {
                    "name": "marker-file-present",
                    "kind": "command",
                    "command": format!("grep -qx {MARKER_CONTENT} {MARKER_FILE}"),
                    "gates": true,
                },
                {
                    "name": "retry-marker-file-present",
                    "kind": "command",
                    "command": format!(
                        "grep -qx {RETRY_MARKER_CONTENT} {RETRY_MARKER_FILE} && grep -qx confirmed {RETRY_MARKER_FILE}"
                    ),
                    "gates": true,
                },
                {
                    "name": "trivial-marker-file-present",
                    "kind": "command",
                    "command": format!(
                        "grep -qx {TRIVIAL_MARKER_CONTENT} {TRIVIAL_MARKER_FILE}"
                    ),
                    "gates": true,
                }
            ]
        }
    });
    std::fs::write(
        worktree.join("planning").join("harness.json"),
        serde_json::to_string_pretty(&harness).unwrap(),
    )
    .unwrap();
}

/// Injected runner for every non-LLM subprocess seam this experiment doesn't
/// care about exercising for real (`git`/`gh`/`mev`) — always succeeds with
/// empty output, same as `sdlc_flow_live.rs`'s `noop_runner`.
fn noop_runner() -> CommandRunner {
    Arc::new(|_program, _args, _cwd| {
        Ok(CommandOutput {
            status: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    })
}

/// Builds the full `SDLC_FLOW` `Workflow` for one profile run: the real
/// declared graph from [`graph::schema`], with [`ExperimentSetupNode`]
/// replacing `SetupWorktreeNode` (fixing the policy-stamping shortcut) and
/// `ImplementTaskNode`/`ConsolidatedReviewNode` left at their real default
/// transport — every other model/subprocess seam stays stubbed, mirroring
/// `sdlc_flow_live.rs::build_live_workflow`.
#[allow(dead_code)]
fn build_experiment_workflow(worktree: &Path) -> Workflow {
    let mut registry = NodeRegistry::new();

    registry.register(Box::new(ExperimentSetupNode {
        worktree_path: worktree.to_string_lossy().to_string(),
    }));
    registry.register(Box::new(SpecExistsRouterNode));
    registry.register(Box::new(GenerateTasksNode::new()));
    registry.register(Box::new(LoadTaskStateNode));
    registry.register(Box::new(TaskQueueRouterNode));

    // Real call: dangerously_skip_permissions is required in headless `-p`
    // mode (no TTY to approve a file-edit tool), scoped down by
    // disallowed_tools so the session can only use file-editing tools, never
    // a shell — see `sdlc_flow_live.rs`'s module doc for the full rationale.
    // `isolated: true` is required whenever this test itself runs from
    // inside another live `claude` session.
    registry.register(Box::new(ImplementTaskNode::new().with_config(Config {
        model: Some("claude-sonnet-4-5".to_string()),
        disallowed_tools: vec!["Bash".to_string()],
        dangerously_skip_permissions: true,
        isolated: true,
        ..Config::default()
    })));

    // Real subprocess: a genuine `sh -c 'grep ...'` against the file(s) the
    // real ImplementTaskNode call (should have) just written.
    registry.register(Box::new(TestTaskNode::new()));
    registry.register(Box::new(TriageTaskNode::new().with_config(Config {
        model: Some("claude-sonnet-4-5".to_string()),
        isolated: true,
        ..Config::default()
    })));
    registry.register(Box::new(TriageRouterNode));

    // Real call: git-diff context is stubbed (a real, empty single-task
    // diff would tell the reviewer nothing useful), transport is real.
    registry.register(Box::new(
        ConsolidatedReviewNode::new()
            .with_runner(noop_runner())
            .with_config(Config {
                model: Some("claude-sonnet-4-5".to_string()),
                isolated: true,
                ..Config::default()
            }),
    ));

    registry.register(Box::new(ReviewRouterNode));
    registry.register(Box::new(UpdateTaskStatusNode));
    registry.register(Box::new(SaveStateNode::new().with_runner(noop_runner())));
    registry.register(Box::new(PatchDocsNode::new().with_transport(Arc::new(
        |_config, _prompt| {
            let outcome = stub_outcome(
                &json!({ "summary": "no stale docs found", "files_patched": [] }).to_string(),
            );
            Box::pin(async move { Ok(outcome) })
        },
    ))));
    registry.register(Box::new(IncrementAttemptNode));
    registry.register(Box::new(WrapUpNode::new()));
    registry.register(Box::new(PullRequestNode::new().with_runner(noop_runner())));
    registry.register(Box::new(EmitStateNode::new().with_runner(noop_runner())));

    let schema = graph::schema();
    Workflow::new_validated(registry, schema)
        .expect("SDLC_FLOW declared graph must pass WorkflowValidator::validate")
}

/// Compile-time-only smoke check (not `#[ignore]`d) that the scaffolding
/// above — in particular [`ExperimentSetupNode`] and [`write_fixture_files`]
/// — builds a workflow and fixture layout without needing a real `claude`
/// CLI. The per-profile real-CLI runs, aggregation, and ranked table are
/// task 2's job; this only guards against the scaffolding silently rotting
/// while task 2 lands.
#[test]
fn experiment_scaffolding_builds_workflow_and_fixtures() {
    let worktree = temp_worktree("scaffold");
    write_fixture_files(&worktree, 2);

    let tasks_path = worktree
        .join("planning")
        .join("experiment-spec")
        .join("tasks.json");
    assert!(tasks_path.exists());
    let tasks: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&tasks_path).unwrap()).unwrap();
    assert_eq!(tasks.as_array().map(|a| a.len()), Some(3));

    let harness_path = worktree.join("planning").join("harness.json");
    assert!(harness_path.exists());

    // Building the workflow validates the declared graph against this
    // registry — a real registration/schema mismatch would panic here.
    let _workflow = build_experiment_workflow(&worktree);

    std::fs::remove_dir_all(&worktree).ok();
}

/// The four named policy profiles this experiment runs, in the exact order
/// the Acceptance Criteria list them.
const PROFILES: [&str; 4] = ["baseline", "cheap-fast", "pragmatist", "batch-reviewer"];

/// Prints a ranked cost/time/tokens/attempts/pass-rate table — one row per
/// distinct resolved policy — sorted by ascending `avg_cost_usd` (cheapest
/// first). Intentionally plain `println!` (no external table-formatting
/// dependency): this test is a manually-run experiment harness, not a
/// machine-parsed report.
fn print_ranked_table(mut rows: Vec<aggregate::PolicyAggregate>) {
    rows.sort_by(|a, b| {
        a.avg_cost_usd
            .partial_cmp(&b.avg_cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    println!(
        "\n{:<8} {:>10} {:>12} {:>14} {:>14} {:>16} {:>16} {:>10}",
        "runs",
        "avg_cost",
        "avg_wall_s",
        "total_in_tok",
        "total_out_tok",
        "attempts/retries",
        "pass_rate",
        "policy"
    );
    for row in &rows {
        println!(
            "{:<8} {:>10.4} {:>12.2} {:>14} {:>14} {:>7}/{:<8} {:>9.2}% {}",
            row.run_count,
            row.avg_cost_usd,
            row.avg_wall_clock_secs,
            row.total_input_tokens,
            row.total_output_tokens,
            row.total_attempts,
            row.total_retries,
            row.pass_rate * 100.0,
            serde_json::to_string(&row.policy).unwrap_or_default(),
        );
    }
    println!();
}

/// The real-CLI experiment: runs the four named profiles through the full
/// `SDLC_FLOW` graph (with [`ExperimentSetupNode`] stamping the resolved
/// policy so profiles actually take effect), aggregates each run's
/// `sdlc-flow-state.json` via
/// [`engine_core::workflows::sdlc_flow::aggregate::aggregate_state_files`],
/// and prints a ranked table.
///
/// **Ignored by default** — spawns four real `claude` CLI sessions per run
/// (`ImplementTaskNode`, `TriageTaskNode`'s `llm_triage` branch when
/// engaged, `ConsolidatedReviewNode`) and spends real tokens. Run manually:
///
/// ```sh
/// cargo test -p engine-core --test sdlc_flow_experiment -- --ignored --test-threads=1
/// ```
///
/// Every profile in [`PROFILES`] here resolves via
/// [`engine_core::workflows::sdlc_flow::profiles::profile_by_name`] against
/// the built-in `claude-sonnet-4-5`-family models, so no `local`-tier
/// Ollama endpoint is required for this particular set; a profile that
/// *does* resolve to a `local`-tier model would need a reachable Ollama
/// endpoint (or the documented cloud fallback — see
/// `planning/decisions/` for the policy-tier fallback rule) before it could
/// run for real.
#[tokio::test]
#[ignore]
async fn experiment_four_profiles_real_cli_ranked_by_cost() {
    let mut state_paths: Vec<PathBuf> = Vec::new();
    let mut worktrees: Vec<PathBuf> = Vec::new();

    for profile in PROFILES {
        let worktree = temp_worktree(profile);
        // Two attempts of headroom: the retry-marker task is deliberately
        // framed to be easy to get subtly wrong on a first pass, so it
        // needs the real triage/retry loop to demonstrate a second real
        // attempt rather than immediately MAJOR_BAIL-ing.
        write_fixture_files(&worktree, 2);

        let workflow = build_experiment_workflow(&worktree);
        let event = json!({
            "spec_slug": "experiment-spec",
            "auto_pr": false,
            "profile": profile,
        });

        let final_ctx = workflow
            .run(event, Box::new(|_ctx: &TaskContext| {}))
            .await
            .unwrap_or_else(|err| panic!("profile {profile:?} run should not error: {err}"));

        // The resolved policy actually took effect for this run (the bug
        // ExperimentSetupNode fixes over sdlc_flow_live.rs's FixtureSetupNode).
        assert!(
            final_ctx
                .nodes
                .contains_key(setup::RESOLVED_POLICY_IDENTITY),
            "profile {profile:?}: resolved policy should have been stamped into ctx.nodes"
        );

        let state_path = worktree
            .join("planning")
            .join("experiment-spec")
            .join("sdlc")
            .join("sdlc-flow-state.json");
        assert!(
            state_path.exists(),
            "profile {profile:?}: expected WrapUpNode to persist {}",
            state_path.display()
        );
        state_paths.push(state_path);
        worktrees.push(worktree);
    }

    assert_eq!(
        state_paths.len(),
        PROFILES.len(),
        "expected one sdlc-flow-state.json per profile"
    );

    let rows = aggregate::aggregate_state_files(&state_paths)
        .expect("all four sdlc-flow-state.json files should parse");

    print_ranked_table(rows.clone());

    assert!(
        rows.len() > 1,
        "expected more than one distinct resolved-policy row across the four \
         profiles (got {}) — a non-default profile should demonstrably change \
         the resolved policy recorded in its run's state file",
        rows.len()
    );

    for worktree in &worktrees {
        std::fs::remove_dir_all(worktree).ok();
    }
}

const HAPPY_MARKER_FILE_A: &str = "HAPPY_TASK_A.txt";
const HAPPY_MARKER_CONTENT_A: &str = "task-a-ok";
const HAPPY_MARKER_FILE_B: &str = "HAPPY_TASK_B.txt";
const HAPPY_MARKER_CONTENT_B: &str = "task-b-ok";

/// Writes a two-task fixture with **no** deliberately-tricky task — both
/// tasks are trivial one-file marker writes expected to pass on the first
/// `ImplementTaskNode` attempt. Unlike [`write_fixture_files`]'s three-task
/// fixture (which includes a task purpose-built to require a retry), this
/// exists purely to prove the happy path: two short, simple implement
/// phases, no triage/retry loop engaged, real cost/time captured.
fn write_happy_path_fixture_files(worktree: &Path, max_attempts: u32) {
    let spec_dir = worktree.join("planning").join("experiment-spec");
    let tasks = json!([
        {
            "task_id": 1,
            "title": "Create the first marker file",
            "description": format!(
                "Create a file named exactly `{HAPPY_MARKER_FILE_A}` in the \
                 current working directory. Its entire content must be the \
                 single line `{HAPPY_MARKER_CONTENT_A}` and nothing else. Do \
                 not create or modify any other file."
            ),
            "acceptance_criteria": [
                format!("{HAPPY_MARKER_FILE_A} exists in the working directory"),
                format!("Its content is exactly \"{HAPPY_MARKER_CONTENT_A}\""),
            ],
            "max_attempts": max_attempts,
        },
        {
            "task_id": 2,
            "title": "Create the second marker file",
            "description": format!(
                "Create a file named exactly `{HAPPY_MARKER_FILE_B}` in the \
                 current working directory. Its entire content must be the \
                 single line `{HAPPY_MARKER_CONTENT_B}` and nothing else. Do \
                 not create or modify any other file."
            ),
            "acceptance_criteria": [
                format!("{HAPPY_MARKER_FILE_B} exists in the working directory"),
                format!("Its content is exactly \"{HAPPY_MARKER_CONTENT_B}\""),
            ],
            "max_attempts": max_attempts,
        }
    ]);
    std::fs::write(
        spec_dir.join("tasks.json"),
        serde_json::to_string_pretty(&tasks).unwrap(),
    )
    .unwrap();

    // Non-gating (`"gates": false`): `TestTaskNode` runs *every* harness.json
    // check on *every* task's `TestTaskNode` step, with no per-task scoping
    // (confirmed by reading `TestTaskNode::process`/`run_checks` — checks
    // are not indexed by `current_task_id`). A `gates: true` check for
    // task B's marker file would therefore fail task 1's own test step
    // (task B hasn't run yet), routing to `TriageTaskNode` and eventually
    // `MAJOR_BAIL`-ing the whole run before task 2 ever starts — exactly
    // what happened on the first real run of this test. Keeping both
    // checks non-gating still surfaces their pass/fail in
    // `check_results` for visibility, without them blocking task 1 on an
    // artifact only task 2 creates. The real pass/fail gate for the happy
    // path is `TestTaskNode`'s write-verification guard (did
    // `ImplementTaskNode`'s claimed write actually show up in `git status
    // --porcelain`, now real thanks to `temp_worktree`'s `git init`).
    let harness = json!({
        "validation": {
            "checks": [
                {
                    "name": "task-a-marker-file-present",
                    "kind": "command",
                    "command": format!("grep -qx {HAPPY_MARKER_CONTENT_A} {HAPPY_MARKER_FILE_A}"),
                    "gates": false,
                },
                {
                    "name": "task-b-marker-file-present",
                    "kind": "command",
                    "command": format!("grep -qx {HAPPY_MARKER_CONTENT_B} {HAPPY_MARKER_FILE_B}"),
                    "gates": false,
                }
            ]
        }
    });
    std::fs::write(
        worktree.join("planning").join("harness.json"),
        serde_json::to_string_pretty(&harness).unwrap(),
    )
    .unwrap();
}

/// Happy-path smoke test: one real `SDLC_FLOW` run, `baseline` (all-Sonnet)
/// profile, against a two-task fixture with no deliberately-tricky task —
/// just enough to prove two short, simple implement phases go green first
/// try, with the review/wrap-up tail completing, and to read the real
/// cost/wall-clock this run actually took.
///
/// **Ignored by default** — spawns real `claude` CLI sessions
/// (`ImplementTaskNode` x2, `ConsolidatedReviewNode`) and spends real
/// tokens. Run manually:
///
/// ```sh
/// cargo test -p engine-core --test sdlc_flow_experiment happy_path_two_simple_tasks_sonnet -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore]
async fn happy_path_two_simple_tasks_sonnet() {
    let worktree = temp_worktree("happy-path");
    write_happy_path_fixture_files(&worktree, 2);

    let workflow = build_experiment_workflow(&worktree);
    let event = json!({
        "spec_slug": "experiment-spec",
        "auto_pr": false,
        "profile": "baseline",
    });

    let final_ctx = workflow
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .unwrap_or_else(|err| panic!("happy-path run should not error: {err}"));

    assert!(
        final_ctx
            .nodes
            .contains_key(setup::RESOLVED_POLICY_IDENTITY),
        "resolved policy should have been stamped into ctx.nodes"
    );

    let state_path = worktree
        .join("planning")
        .join("experiment-spec")
        .join("sdlc")
        .join("sdlc-flow-state.json");
    assert!(
        state_path.exists(),
        "expected WrapUpNode to persist {}",
        state_path.display()
    );

    let rows =
        aggregate::aggregate_state_files(&[state_path]).expect("sdlc-flow-state.json should parse");

    print_ranked_table(rows.clone());

    assert_eq!(rows.len(), 1, "expected exactly one resolved-policy row");
    assert_eq!(
        rows[0].pass_rate, 1.0,
        "both trivial tasks should have passed on the happy path"
    );

    std::fs::remove_dir_all(&worktree).ok();
}
