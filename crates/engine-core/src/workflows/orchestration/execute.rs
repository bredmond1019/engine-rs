//! Block execution — `EN.10.B` Task 3.
//!
//! Runs one [`ChainStep`] by **invoking** the existing `SDLC_FLOW` workflow
//! (`workflows::sdlc_flow::graph::WORKFLOW_TYPE`), never reimplementing it —
//! the Rust `SDLC_FLOW` port already exists and a second copy would drift
//! within a week. The engine to run a block with is selected from the
//! block's own authored `sdlc_workflow` field, supplied here via an
//! injectable closure so this module stays independent of *how* that field
//! is loaded (an `okf_core::TrackBlock`, a JSON block record, or a test
//! double) — the same seam shape [`super::chain`] and [`super::gates`]
//! already use for "is this open" / "is this met".
//!
//! # `EngineKind` — closed today, on purpose
//!
//! `EN.10.C` is the block that makes the sanctioned-engine seam
//! *structurally* closed (its own `engine_kind.rs`, plus the test that
//! proves the escape is unreachable). This module does not wait for that:
//! [`EngineKind`] is already the same two-variant closed enum, not a
//! string. Introducing a string-typed runner here would only have to be
//! torn out again the moment `EN.10.C` lands — this task does not do that
//! and undo it.
//!
//! Only `EngineKind::Flow` is runnable today, because only `SDLC_FLOW` has
//! been ported to this engine (`workflows::sdlc_flow`) — there is no Rust
//! `SDLC_TASK` workflow yet. `EngineKind::Task` therefore fails loudly with
//! [`ExecuteError::UnsupportedEngine`] naming the block and repo, rather
//! than silently falling through to `Flow` or panicking.
//!
//! # Short-lived, cwd-scoped invocation
//!
//! Claude Code (and this engine's own `SDLC_FLOW` port, via
//! `sdlc_flow::setup::resolve_target_root`) picks up a repo's harness and
//! `CLAUDE.md` from its **working directory** — a long-lived session cannot
//! span repos, but a *workflow* may. [`default_flow_runner`] mirrors
//! `engine-serve`'s own `register_sdlc_flow_with_registry` factory: for
//! every step it resolves the step's repo slug through the injected
//! [`RepoRegistry`] to an absolute path, builds a **fresh**
//! `SDLC_FLOW` `Workflow` (policy-aware registry + schema, registered with
//! that same registry so `SetupWorktreeNode` resolves `event.repo` too),
//! seeds `event.repo` on the dispatched event, and runs it to completion —
//! nothing is kept alive or reused across steps. That keeps `/orchestrate`
//! rule 3 (one repo per session) true per invocation instead of per lane.

use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use engine_contract::{NodeRunStatus, TaskContext};
use serde_json::json;

use crate::completion::derive_terminal_status;
use crate::policy::PolicyConfigSource;
use crate::repo_registry::{RepoRegistry, RepoRegistryError};
use crate::workflows::sdlc_flow;
use crate::{OnProgress, Workflow, WorkflowError};

use super::chain::ChainStep;

// ── Engine selection ────────────────────────────────────────────────────

/// Which sanctioned SDLC engine runs a block — taken verbatim from the
/// block's authored `sdlc_workflow` field (`okf_core::TrackBlock`), which
/// carries exactly this two-value closed vocabulary (`"task"` / `"flow"`).
///
/// Re-exported from [`super::engine_kind`] (`EN.10.C` Task 1), which owns the
/// definition and the `sdlc_workflow -> EngineKind` mapping
/// ([`EngineKind::from_sdlc_workflow`]) — kept as `execute::EngineKind` here
/// so every existing caller and test of this module's public API keeps
/// working unchanged. `EngineKind` is a closed, two-variant type: there is no
/// third variant and no string-typed constructor, so an unsanctioned runner
/// cannot be represented, only diagnosed as an
/// [`super::engine_kind::UnsupportedSdlcWorkflow`].
pub use super::engine_kind::EngineKind;

// ── Invocation / runner seam ────────────────────────────────────────────

/// One resolved `SDLC_FLOW` invocation: the step's repo slug, its resolved
/// absolute filesystem root (the cwd the run must target), and the block
/// id to run as `spec_slug`. Handed to a [`FlowRunner`] so a test double
/// can assert exactly what cwd a step actually resolved to, not merely
/// what the caller intended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowInvocation {
    pub repo: String,
    pub repo_path: PathBuf,
    pub block_id: String,
}

/// A future producing a finished run's [`TaskContext`] or a [`WorkflowError`].
/// Deliberately **not** bounded `Send` — `Workflow::run`'s `OnProgress`
/// callback (`Box<dyn FnMut(&TaskContext)>`) is not `Send`, so a future that
/// awaits it cannot be either. [`execute_step`] only ever `.await`s this
/// directly (never spawns it onto a multi-threaded executor), so the
/// missing `Send` bound costs nothing here.
pub type FlowFuture = Pin<Box<dyn Future<Output = Result<TaskContext, WorkflowError>>>>;

/// The injectable "run `SDLC_FLOW` for this invocation" seam — the same
/// `Arc<dyn Fn(..) -> <boxed future>>` shape this crate's other injectable
/// seams (e.g. [`crate::workflows::ModelTransport`]) use, minus the `Send`
/// future bound (see [`FlowFuture`]). Production code uses
/// [`default_flow_runner`]; tests substitute a stub that records the
/// [`FlowInvocation`]s it was called with, so a step's resolved cwd is
/// asserted directly rather than inferred from intent.
pub type FlowRunner = Arc<dyn Fn(FlowInvocation) -> FlowFuture + Send + Sync>;

// ── Errors ───────────────────────────────────────────────────────────────

/// Everything that can go wrong executing one [`ChainStep`]. Every variant
/// names the block and repo involved, per this module's (and its sibling
/// `chain`/`gates` modules') "never fail silently" convention.
#[derive(Debug)]
pub enum ExecuteError {
    /// The step's repo slug did not resolve through the [`RepoRegistry`].
    RepoResolutionFailed {
        repo: String,
        block_id: String,
        source: RepoRegistryError,
    },
    /// The block's authored engine is outside what this task can run
    /// (today: anything but [`EngineKind::Flow`]).
    UnsupportedEngine {
        repo: String,
        block_id: String,
        engine: EngineKind,
    },
    /// The invoked `SDLC_FLOW` run itself returned an error. Surfaced with
    /// the block id and repo so a multi-block chain's failure is
    /// attributable at a glance.
    StepFailed {
        repo: String,
        block_id: String,
        source: WorkflowError,
    },
    /// `run_flow` returned `Ok(ctx)` (`SDLC_FLOW`'s never-`Err` contract,
    /// engine-rs D12), but the child run's own `ctx.node_runs` — read via the
    /// shared [`derive_terminal_status`] rather than a second "did this run
    /// fail?" implementation — reports `"failed"`. `Workflow::walk` breaks on
    /// a failed node and falls through to `Ok(ctx)`, so this is the ONLY
    /// place that failure becomes visible to the chain. Names the failing
    /// node (its `node_runs` key) so a multi-block chain's failure is
    /// attributable to the exact node that died, not just the step.
    ChildFailed {
        repo: String,
        block_id: String,
        failing_node: String,
    },
}

impl fmt::Display for ExecuteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExecuteError::RepoResolutionFailed {
                repo,
                block_id,
                source,
            } => write!(
                f,
                "block '{block_id}' cannot start: repo '{repo}' did not resolve: {source}"
            ),
            ExecuteError::UnsupportedEngine {
                repo,
                block_id,
                engine,
            } => write!(
                f,
                "block '{block_id}' (repo '{repo}') declares engine '{engine}', which this \
                 workflow cannot run yet"
            ),
            ExecuteError::StepFailed {
                repo,
                block_id,
                source,
            } => write!(f, "block '{block_id}' (repo '{repo}') failed: {source}"),
            ExecuteError::ChildFailed {
                repo,
                block_id,
                failing_node,
            } => write!(
                f,
                "block '{block_id}' (repo '{repo}') failed: node '{failing_node}' did not succeed"
            ),
        }
    }
}

impl std::error::Error for ExecuteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ExecuteError::RepoResolutionFailed { source, .. } => Some(source),
            ExecuteError::UnsupportedEngine { .. } => None,
            ExecuteError::StepFailed { source, .. } => Some(source),
            ExecuteError::ChildFailed { .. } => None,
        }
    }
}

/// The outcome of one successfully executed step: which block/repo ran,
/// the resolved cwd it ran against, and the run's finished [`TaskContext`]
/// (`EN.10.B` Task 4 reads this to verify the state write and append the
/// lane-log line).
#[derive(Debug)]
pub struct ExecutionOutcome {
    pub repo: String,
    pub repo_path: PathBuf,
    pub block_id: String,
    pub ctx: TaskContext,
}

// ── Execution ────────────────────────────────────────────────────────────

/// Execute one [`ChainStep`]: resolve its repo to an absolute cwd via
/// `registry`, resolve its engine via `resolve_engine(repo, block_id)`, and
/// — for [`EngineKind::Flow`] only — invoke `run_flow` with the resolved
/// [`FlowInvocation`].
///
/// `resolve_engine` is consulted before the repo resolves to a cwd is
/// irrelevant to the caller: both must succeed for the step to run, and
/// either failing reports the block id and repo. `run_flow` is never
/// called for an unsupported engine.
pub async fn execute_step(
    step: &ChainStep,
    resolve_engine: &dyn Fn(&str, &str) -> EngineKind,
    registry: &RepoRegistry,
    run_flow: &FlowRunner,
) -> Result<ExecutionOutcome, ExecuteError> {
    let repo_path =
        registry
            .resolve(&step.repo)
            .map_err(|source| ExecuteError::RepoResolutionFailed {
                repo: step.repo.clone(),
                block_id: step.block_id.clone(),
                source,
            })?;

    let engine = resolve_engine(&step.repo, &step.block_id);
    if engine != EngineKind::Flow {
        return Err(ExecuteError::UnsupportedEngine {
            repo: step.repo.clone(),
            block_id: step.block_id.clone(),
            engine,
        });
    }

    let invocation = FlowInvocation {
        repo: step.repo.clone(),
        repo_path: repo_path.clone(),
        block_id: step.block_id.clone(),
    };
    let ctx = run_flow(invocation)
        .await
        .map_err(|source| ExecuteError::StepFailed {
            repo: step.repo.clone(),
            block_id: step.block_id.clone(),
            source,
        })?;

    // `run_flow` returning `Ok(ctx)` is not proof the child succeeded:
    // `Workflow::walk` breaks on a failed node and falls through to
    // `Ok(ctx)` (`SDLC_FLOW`'s never-`Err` contract, engine-rs D12), so the
    // failure lives only in `ctx.node_runs`. Read it via the SHARED
    // `derive_terminal_status` — never a second "did this run fail?" check —
    // and stop the chain here rather than integrating a failed step as a
    // success.
    if derive_terminal_status(&ctx) == "failed" {
        let failing_node = ctx
            .node_runs
            .iter()
            .find(|(_, run)| run.status == NodeRunStatus::Failed)
            .map(|(name, _)| name.clone())
            .unwrap_or_else(|| "<unknown>".to_string());
        return Err(ExecuteError::ChildFailed {
            repo: step.repo.clone(),
            block_id: step.block_id.clone(),
            failing_node,
        });
    }

    Ok(ExecutionOutcome {
        repo: step.repo.clone(),
        repo_path,
        block_id: step.block_id.clone(),
        ctx,
    })
}

/// The production [`FlowRunner`]: for each [`FlowInvocation`], builds a
/// **fresh** `SDLC_FLOW` `Workflow` — policy resolved from
/// `PolicyConfigSource::Worktree(invocation.repo_path)`, `SetupWorktreeNode`
/// re-registered with `registry` so `event.repo` resolves inside the run
/// too — and runs it with `{"repo": invocation.repo, "spec_slug":
/// invocation.block_id}` as the event, mirroring
/// `engine-serve::workflows::register_sdlc_flow_with_registry`'s factory
/// exactly, minus the `Dispatcher` registration (this seam invokes the
/// workflow directly rather than dispatching an HTTP event).
///
/// Never reimplements `SDLC_FLOW`: every node in the run is
/// `workflows::sdlc_flow`'s own.
#[must_use]
pub fn default_flow_runner(registry: Arc<RepoRegistry>) -> FlowRunner {
    Arc::new(move |invocation: FlowInvocation| {
        let registry = registry.clone();
        Box::pin(async move {
            let event = json!({
                "repo": invocation.repo,
                "spec_slug": invocation.block_id,
            });

            let ctx_for_policy = TaskContext {
                event: event.clone(),
                nodes: std::collections::HashMap::new(),
                metadata: json!({}),
                node_runs: std::collections::HashMap::new(),
            };
            let source = PolicyConfigSource::Worktree(invocation.repo_path.clone());
            let policy = sdlc_flow::setup::resolve_policy_for_run_from(&ctx_for_policy, &source)
                .map_err(|err| WorkflowError::new(err.to_string()))?;

            let mut node_registry = sdlc_flow::graph::registry_for_policy(&policy);
            node_registry.register(Box::new(
                sdlc_flow::setup::SetupWorktreeNode::new().with_registry(registry.clone()),
            ));

            let workflow = Workflow::new_validated(node_registry, sdlc_flow::graph::schema())
                .map_err(|err| WorkflowError::new(err.to_string()))?;
            let on_progress: OnProgress<'_> = Box::new(|_ctx: &TaskContext| {});
            workflow.run(event, on_progress).await
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    fn step(repo: &str, block_id: &str) -> ChainStep {
        ChainStep {
            repo: repo.to_string(),
            block_id: block_id.to_string(),
            directives: None,
        }
    }

    /// A tempdir `brain.toml` + repo registry with two real repo
    /// directories, `repo-a` and `repo-b`, mirroring the tempdir-fixture
    /// pattern `repo_registry.rs`'s own tests already use.
    fn two_repo_registry() -> (tempfile::TempDir, RepoRegistry) {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("repo-a")).unwrap();
        std::fs::create_dir_all(dir.path().join("repo-b")).unwrap();
        std::fs::write(
            dir.path().join("brain.toml"),
            "[[repos]]\nslug = \"repo-a\"\nrepo_path = \"repo-a\"\n\
             [[repos]]\nslug = \"repo-b\"\nrepo_path = \"repo-b\"\n",
        )
        .unwrap();
        let registry = RepoRegistry::from_brain_root(dir.path()).expect("registry");
        (dir, registry)
    }

    /// A `FlowRunner` test double that records every [`FlowInvocation`] it
    /// was called with and returns a fixed, empty successful `TaskContext`.
    fn recording_runner() -> (FlowRunner, Arc<Mutex<Vec<FlowInvocation>>>) {
        let calls: Arc<Mutex<Vec<FlowInvocation>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let runner: FlowRunner = Arc::new(move |invocation: FlowInvocation| {
            recorded.lock().unwrap().push(invocation);
            Box::pin(async {
                Ok(TaskContext {
                    event: json!({}),
                    nodes: std::collections::HashMap::new(),
                    metadata: json!({}),
                    node_runs: std::collections::HashMap::new(),
                })
            })
        });
        (runner, calls)
    }

    #[tokio::test]
    async fn two_repo_chain_executes_each_step_with_cwd_set_to_that_steps_repo() {
        let (dir, registry) = two_repo_registry();
        let (runner, calls) = recording_runner();
        let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;

        let step_a = step("repo-a", "A.1");
        let step_b = step("repo-b", "B.1");

        let outcome_a = execute_step(&step_a, &resolve_engine, &registry, &runner)
            .await
            .expect("step a should execute");
        let outcome_b = execute_step(&step_b, &resolve_engine, &registry, &runner)
            .await
            .expect("step b should execute");

        assert_eq!(outcome_a.repo_path, dir.path().join("repo-a"));
        assert_eq!(outcome_b.repo_path, dir.path().join("repo-b"));

        // Assert the cwd *actually passed* to the runner, not merely the
        // outcome the caller computed independently.
        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].repo_path, dir.path().join("repo-a"));
        assert_eq!(recorded[0].block_id, "A.1");
        assert_eq!(recorded[1].repo_path, dir.path().join("repo-b"));
        assert_eq!(recorded[1].block_id, "B.1");
    }

    #[tokio::test]
    async fn blocks_authored_engine_selects_the_runner() {
        let (_dir, registry) = two_repo_registry();
        let (runner, calls) = recording_runner();
        let call_count = Arc::new(AtomicUsize::new(0));
        let counted = call_count.clone();
        let resolve_engine = move |_repo: &str, _id: &str| {
            counted.fetch_add(1, Ordering::SeqCst);
            EngineKind::Flow
        };

        let s = step("repo-a", "A.1");
        execute_step(&s, &resolve_engine, &registry, &runner)
            .await
            .expect("flow-engine step should execute");

        assert_eq!(call_count.load(Ordering::SeqCst), 1);
        assert_eq!(calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn task_engine_is_unsupported_and_never_invokes_the_runner() {
        let (_dir, registry) = two_repo_registry();
        let (runner, calls) = recording_runner();
        let resolve_engine = |_repo: &str, _id: &str| EngineKind::Task;

        let s = step("repo-a", "A.1");
        let err = execute_step(&s, &resolve_engine, &registry, &runner)
            .await
            .unwrap_err();

        match &err {
            ExecuteError::UnsupportedEngine {
                repo,
                block_id,
                engine,
            } => {
                assert_eq!(repo, "repo-a");
                assert_eq!(block_id, "A.1");
                assert_eq!(*engine, EngineKind::Task);
            }
            other => panic!("expected UnsupportedEngine, got {other:?}"),
        }
        // The runner must never have been called for an unsupported engine.
        assert!(calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_steps_failure_surfaces_with_the_block_id() {
        let (_dir, registry) = two_repo_registry();
        let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;
        let failing_runner: FlowRunner = Arc::new(|invocation: FlowInvocation| {
            Box::pin(async move {
                Err(WorkflowError::new(format!(
                    "boom while running {}",
                    invocation.block_id
                )))
            })
        });

        let s = step("repo-a", "A.1");
        let err = execute_step(&s, &resolve_engine, &registry, &failing_runner)
            .await
            .unwrap_err();

        match &err {
            ExecuteError::StepFailed { repo, block_id, .. } => {
                assert_eq!(repo, "repo-a");
                assert_eq!(block_id, "A.1");
            }
            other => panic!("expected StepFailed, got {other:?}"),
        }
        let msg = err.to_string();
        assert!(msg.contains("A.1"), "message should name the block: {msg}");
        assert!(
            msg.contains("repo-a"),
            "message should name the repo: {msg}"
        );
    }

    #[tokio::test]
    async fn unknown_repo_slug_fails_before_the_runner_is_ever_consulted() {
        let (_dir, registry) = two_repo_registry();
        let (runner, calls) = recording_runner();
        let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;

        let s = step("does-not-exist", "A.1");
        let err = execute_step(&s, &resolve_engine, &registry, &runner)
            .await
            .unwrap_err();

        assert!(matches!(err, ExecuteError::RepoResolutionFailed { .. }));
        assert!(calls.lock().unwrap().is_empty());
    }

    #[test]
    fn engine_kind_display_names_are_stable() {
        assert_eq!(EngineKind::Task.to_string(), "task");
        assert_eq!(EngineKind::Flow.to_string(), "flow");
    }

    /// A `FlowRunner` test double that returns `Ok(ctx)` where `ctx.node_runs`
    /// records `node_name` as [`NodeRunStatus::Failed`] — mirroring
    /// `SDLC_FLOW`'s real never-`Err` contract (engine-rs D12): `Workflow::walk`
    /// breaks on a failed node and falls through to `Ok(ctx)`, so a successful
    /// `run_flow` future is not proof the child succeeded.
    fn ok_but_child_failed_runner(node_name: &'static str) -> FlowRunner {
        Arc::new(move |_invocation: FlowInvocation| {
            let mut node_runs = std::collections::HashMap::new();
            node_runs.insert(
                node_name.to_string(),
                engine_contract::NodeRun {
                    status: NodeRunStatus::Failed,
                    started_at: None,
                    completed_at: None,
                    error: Some("boom".to_string()),
                    input: None,
                    usage: None,
                },
            );
            Box::pin(async move {
                Ok(TaskContext {
                    event: json!({}),
                    nodes: std::collections::HashMap::new(),
                    metadata: json!({}),
                    node_runs,
                })
            })
        })
    }

    #[tokio::test]
    async fn a_child_that_returns_ok_but_recorded_a_failed_node_still_fails_the_step() {
        let (_dir, registry) = two_repo_registry();
        let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;
        let runner = ok_but_child_failed_runner("SetupWorktreeNode");

        let s = step("repo-a", "A.1");
        let err = execute_step(&s, &resolve_engine, &registry, &runner)
            .await
            .expect_err("a child with a failed node_run must fail the step");

        match &err {
            ExecuteError::ChildFailed {
                repo,
                block_id,
                failing_node,
            } => {
                assert_eq!(repo, "repo-a");
                assert_eq!(block_id, "A.1");
                assert_eq!(failing_node, "SetupWorktreeNode");
            }
            other => panic!("expected ChildFailed, got {other:?}"),
        }
        let msg = err.to_string();
        assert!(msg.contains("A.1"), "message should name the block: {msg}");
        assert!(
            msg.contains("repo-a"),
            "message should name the repo: {msg}"
        );
        assert!(
            msg.contains("SetupWorktreeNode"),
            "message should name the failing node: {msg}"
        );
    }

    #[tokio::test]
    async fn a_child_that_returns_ok_with_no_failed_nodes_still_integrates_as_success() {
        // No over-correction: a genuinely successful run (recording_runner's
        // fixed, empty, all-succeeded TaskContext) must still be integrated.
        let (_dir, registry) = two_repo_registry();
        let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;
        let (runner, _calls) = recording_runner();

        let s = step("repo-a", "A.1");
        let outcome = execute_step(&s, &resolve_engine, &registry, &runner)
            .await
            .expect("a run with no failed nodes should be integrated as success");

        assert_eq!(outcome.block_id, "A.1");
    }
}
