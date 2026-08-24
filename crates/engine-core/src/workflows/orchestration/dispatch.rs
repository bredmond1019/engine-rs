//! The dispatch step — `EN.12.E` Task 3.
//!
//! A sibling of [`super::execute`], not a variant of it: [`execute_dispatch_step`]
//! resolves a [`ChainStep`] whose [`StepKind`](super::chain::StepKind) is
//! [`Dispatch`](super::chain::StepKind::Dispatch) to a **registered workflow**
//! (`RESEARCH_AGENT`, `CONTENT_PIPELINE`, ...) through the existing
//! [`Dispatcher`], and runs it as one chain step — never an SDLC engine, and
//! never through [`super::execute::execute_step`].
//!
//! # `EngineKind` is never in scope here
//!
//! `execute.rs`'s [`EngineKind`](super::engine_kind::EngineKind) selects
//! *which sanctioned SDLC engine* (`/sdlc-task` or `/sdlc-flow`) runs a
//! `block` step. A dispatch step runs a different kind of thing entirely — a
//! registered in-process workflow, not an SDLC spec — so it has no engine to
//! select at all. This module never imports, constructs, or matches on
//! `EngineKind`; that omission is itself part of `EN.12.E`'s acceptance
//! criteria (`EngineKind` stays a closed, two-variant, SDLC-only type).
//!
//! # The registry key IS `ChainStep::block_id`
//!
//! `ChainStep` gained no new field for this ([`StepKind`](super::chain::StepKind)
//! already discriminates *what kind* of step this is): for a `block` step,
//! `block_id` names a corpus block id; for a `dispatch` step, the exact same
//! field names a [`Dispatcher`] registry key (a `workflow_type`, e.g.
//! `"RESEARCH_AGENT"`). [`workflow_key`] is the one accessor that names this
//! reuse, so a reader never has to infer it from the field name alone.
//!
//! # Registry keys are consumed here, never registered
//!
//! Production callers hand this module the same [`Dispatcher`] that
//! `engine-serve::workflows`' `register_research_agent` /
//! `register_content_pipeline` (and friends) already populated elsewhere —
//! this module never calls `Dispatcher::register` itself. An unregistered
//! key is not a bug in the caller to paper over; it is
//! [`DispatchStepError::UnknownWorkflowKey`], and the chain stops. It must
//! never silently fall through to a block invocation — that silent
//! fallthrough is exactly the failure mode this module exists to prevent.

use std::fmt;

use engine_contract::{NodeRunStatus, TaskContext};

use crate::completion::derive_terminal_status;
use crate::dispatch::{DispatchError, Dispatcher};
use crate::workflow::OnProgress;
use crate::WorkflowError;

use super::chain::ChainStep;

/// The [`Dispatcher`] registry key one dispatch [`ChainStep`] names — see
/// the module doc's "The registry key IS `ChainStep::block_id`" section for
/// why this reuses the field rather than adding a new one.
pub fn workflow_key(step: &ChainStep) -> &str {
    &step.block_id
}

/// The outcome of one successfully dispatched step: which registry key ran,
/// the chain step's own `block_id` (identical to `workflow_key` today, but
/// named separately so a caller reading this struct never has to know that
/// fact), and the finished [`TaskContext`] — the durable home a later step
/// (or the journal, `EN.12.E` Task 5) reads the result from.
#[derive(Debug)]
pub struct DispatchOutcome {
    pub workflow_key: String,
    pub block_id: String,
    pub ctx: TaskContext,
}

/// Everything that can go wrong dispatching one [`ChainStep`]. Every variant
/// names the step's `block_id` and the registry key it resolved to, matching
/// `execute.rs`'s "never fail silently" convention for this same workflow.
#[derive(Debug)]
pub enum DispatchStepError {
    /// The step's `workflow_key` is not registered in the [`Dispatcher`]
    /// handed to [`execute_dispatch_step`]. This is the failure this module
    /// exists to make loud: it never falls through to a block invocation.
    UnknownWorkflowKey {
        block_id: String,
        workflow_key: String,
    },
    /// The registered key's factory failed to resolve its own policy against
    /// the triggering event (e.g. an unknown profile name).
    PolicyResolutionFailed {
        block_id: String,
        workflow_key: String,
        message: String,
    },
    /// The dispatched workflow itself returned an error.
    StepFailed {
        block_id: String,
        workflow_key: String,
        source: WorkflowError,
    },
    /// The dispatched workflow returned `Ok(ctx)` (a workflow's own
    /// never-`Err` contract for a failed node — `Workflow::walk` breaks on
    /// failure and falls through to `Ok(ctx)`), but `ctx.node_runs` reports
    /// a failed node via the shared [`derive_terminal_status`] — never a
    /// second "did this run fail?" check. Names the failing node so a
    /// mixed chain's failure is attributable to the exact node that died.
    ChildFailed {
        block_id: String,
        workflow_key: String,
        failing_node: String,
    },
}

impl fmt::Display for DispatchStepError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DispatchStepError::UnknownWorkflowKey {
                block_id,
                workflow_key,
            } => write!(
                f,
                "dispatch step '{block_id}' names workflow key '{workflow_key}', which is not \
                 registered — stopping the chain rather than falling through to a block \
                 invocation"
            ),
            DispatchStepError::PolicyResolutionFailed {
                block_id,
                workflow_key,
                message,
            } => write!(
                f,
                "dispatch step '{block_id}' (workflow key '{workflow_key}') failed to resolve \
                 policy: {message}"
            ),
            DispatchStepError::StepFailed {
                block_id,
                workflow_key,
                source,
            } => write!(
                f,
                "dispatch step '{block_id}' (workflow key '{workflow_key}') failed: {source}"
            ),
            DispatchStepError::ChildFailed {
                block_id,
                workflow_key,
                failing_node,
            } => write!(
                f,
                "dispatch step '{block_id}' (workflow key '{workflow_key}') failed: node \
                 '{failing_node}' did not succeed"
            ),
        }
    }
}

impl std::error::Error for DispatchStepError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DispatchStepError::StepFailed { source, .. } => Some(source),
            DispatchStepError::UnknownWorkflowKey { .. }
            | DispatchStepError::PolicyResolutionFailed { .. }
            | DispatchStepError::ChildFailed { .. } => None,
        }
    }
}

/// Dispatch and run one [`ChainStep`] whose `workflow_key`
/// ([`ChainStep::block_id`]) names a workflow registered in `dispatcher`.
///
/// `event` seeds the dispatched workflow's `TaskContext::event` — the same
/// event JSON `Dispatcher::dispatch_with_event`'s factory resolves policy
/// against — and `on_progress` is forwarded verbatim to `Workflow::run_with`
/// (via [`Dispatcher::dispatch_with_event`] then a plain `run`), matching
/// how any other `Workflow` is driven in this crate.
///
/// Resolution (an unregistered key, or a registered key whose factory fails
/// to resolve policy) fails before the workflow is ever run. Once running,
/// an `Err` from the workflow itself, or an `Ok(ctx)` whose `node_runs`
/// reports a failed node, both stop the chain — see [`DispatchStepError`].
pub async fn execute_dispatch_step(
    step: &ChainStep,
    dispatcher: &Dispatcher,
    event: &serde_json::Value,
    on_progress: OnProgress<'_>,
) -> Result<DispatchOutcome, DispatchStepError> {
    let key = workflow_key(step).to_string();

    let workflow = dispatcher
        .dispatch_with_event(&key, event)
        .map_err(|err| match err {
            DispatchError::UnknownWorkflowType(workflow_key) => {
                DispatchStepError::UnknownWorkflowKey {
                    block_id: step.block_id.clone(),
                    workflow_key,
                }
            }
            DispatchError::PolicyResolutionFailed(message) => {
                DispatchStepError::PolicyResolutionFailed {
                    block_id: step.block_id.clone(),
                    workflow_key: key.clone(),
                    message,
                }
            }
        })?;

    let ctx = workflow
        .run(event.clone(), on_progress)
        .await
        .map_err(|source| DispatchStepError::StepFailed {
            block_id: step.block_id.clone(),
            workflow_key: key.clone(),
            source,
        })?;

    if derive_terminal_status(&ctx) == "failed" {
        let failing_node = ctx
            .node_runs
            .iter()
            .find(|(_, run)| run.status == NodeRunStatus::Failed)
            .map(|(name, _)| name.clone())
            .unwrap_or_else(|| "<unknown>".to_string());
        return Err(DispatchStepError::ChildFailed {
            block_id: step.block_id.clone(),
            workflow_key: key,
            failing_node,
        });
    }

    Ok(DispatchOutcome {
        workflow_key: key,
        block_id: step.block_id.clone(),
        ctx,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap as StdHashMap;

    use async_trait::async_trait;

    use crate::dispatch::WorkflowFactory;
    use crate::{Node, NodeConfig, NodeError, NodeRegistry, Workflow, WorkflowSchema};

    use super::super::chain::StepKind;
    use super::*;

    struct MarkerNode;

    #[async_trait]
    impl Node for MarkerNode {
        async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
            ctx.nodes
                .insert(self.name().to_string(), serde_json::json!({ "ran": true }));
            Ok(ctx)
        }

        fn name(&self) -> &str {
            "MarkerNode"
        }
    }

    struct FailingNode;

    #[async_trait]
    impl Node for FailingNode {
        async fn process(&self, _ctx: TaskContext) -> Result<TaskContext, NodeError> {
            Err(NodeError::new("deliberate test failure"))
        }

        fn name(&self) -> &str {
            "FailingNode"
        }
    }

    fn dispatch_step(block_id: &str) -> ChainStep {
        ChainStep {
            repo: "engine-rs".to_string(),
            block_id: block_id.to_string(),
            directives: None,
            roadmap: None,
            lane: None,
            segment: None,
            kind: StepKind::Dispatch,
        }
    }

    fn fixture_schema(workflow_type: &str, node_name: &str) -> WorkflowSchema {
        let mut nodes = StdHashMap::new();
        nodes.insert(node_name.to_string(), NodeConfig::new(node_name, vec![]));
        WorkflowSchema::new(workflow_type, node_name, nodes)
    }

    fn marker_factory(workflow_type: &'static str) -> WorkflowFactory {
        Box::new(move |_event: &serde_json::Value| {
            let mut registry = NodeRegistry::new();
            registry.register(Box::new(MarkerNode));
            Ok(Workflow::new(
                registry,
                fixture_schema(workflow_type, "MarkerNode"),
            ))
        })
    }

    fn failing_node_factory(workflow_type: &'static str) -> WorkflowFactory {
        Box::new(move |_event: &serde_json::Value| {
            let mut registry = NodeRegistry::new();
            registry.register(Box::new(FailingNode));
            Ok(Workflow::new(
                registry,
                fixture_schema(workflow_type, "FailingNode"),
            ))
        })
    }

    #[test]
    fn workflow_key_reuses_block_id() {
        let step = dispatch_step("RESEARCH_AGENT");
        assert_eq!(workflow_key(&step), "RESEARCH_AGENT");
    }

    /// Acceptance criterion: "A dispatch step naming a registered key runs
    /// that workflow and returns its result."
    #[tokio::test]
    async fn registered_key_runs_and_returns_result() {
        let mut dispatcher = Dispatcher::new();
        dispatcher.register(
            fixture_schema("RESEARCH_AGENT", "MarkerNode"),
            marker_factory("RESEARCH_AGENT"),
        );
        let step = dispatch_step("RESEARCH_AGENT");
        let on_progress: OnProgress<'_> = Box::new(|_ctx| {});

        let outcome =
            execute_dispatch_step(&step, &dispatcher, &serde_json::json!({}), on_progress)
                .await
                .expect("registered key must dispatch successfully");

        assert_eq!(outcome.workflow_key, "RESEARCH_AGENT");
        assert_eq!(outcome.block_id, "RESEARCH_AGENT");
        assert_eq!(
            outcome.ctx.nodes.get("MarkerNode"),
            Some(&serde_json::json!({ "ran": true }))
        );
    }

    /// Acceptance criterion: "A dispatch step naming an UNREGISTERED key
    /// produces a diagnostic naming the key and stops the chain — asserted
    /// by a test that would fail if it fell through to a block invocation."
    ///
    /// There is no block-invocation seam reachable from
    /// `execute_dispatch_step` at all — it never imports `execute.rs`'s
    /// `FlowRunner`/`FlowInvocation`/`EngineKind` — so a silent fallthrough
    /// is structurally unreachable, not merely untested. This test pins the
    /// remaining, reachable failure mode: the call must return the named
    /// `UnknownWorkflowKey` error rather than `Ok`.
    #[tokio::test]
    async fn unregistered_key_produces_named_diagnostic_and_stops() {
        let dispatcher = Dispatcher::new();
        let step = dispatch_step("NOT_REGISTERED");
        let on_progress: OnProgress<'_> = Box::new(|_ctx| {});

        let result =
            execute_dispatch_step(&step, &dispatcher, &serde_json::json!({}), on_progress).await;

        match result {
            Err(DispatchStepError::UnknownWorkflowKey {
                block_id,
                workflow_key,
            }) => {
                assert_eq!(block_id, "NOT_REGISTERED");
                assert_eq!(workflow_key, "NOT_REGISTERED");
            }
            other => panic!("expected UnknownWorkflowKey, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn policy_resolution_failure_is_surfaced() {
        let mut dispatcher = Dispatcher::new();
        let factory: WorkflowFactory =
            Box::new(|_event: &serde_json::Value| Err("bad profile".to_string()));
        dispatcher.register(fixture_schema("CONTENT_PIPELINE", "MarkerNode"), factory);
        let step = dispatch_step("CONTENT_PIPELINE");
        let on_progress: OnProgress<'_> = Box::new(|_ctx| {});

        let result =
            execute_dispatch_step(&step, &dispatcher, &serde_json::json!({}), on_progress).await;

        assert!(matches!(
            result,
            Err(DispatchStepError::PolicyResolutionFailed { message, .. }) if message == "bad profile"
        ));
    }

    /// A dispatched workflow whose only node fails must be reported as
    /// `ChildFailed`, naming the failing node — not silently treated as a
    /// success just because `Workflow::run` itself returned `Ok`.
    #[tokio::test]
    async fn failed_child_node_is_reported_as_child_failed() {
        let mut dispatcher = Dispatcher::new();
        dispatcher.register(
            fixture_schema("RESEARCH_AGENT", "FailingNode"),
            failing_node_factory("RESEARCH_AGENT"),
        );
        let step = dispatch_step("RESEARCH_AGENT");
        let on_progress: OnProgress<'_> = Box::new(|_ctx| {});

        let result =
            execute_dispatch_step(&step, &dispatcher, &serde_json::json!({}), on_progress).await;

        match result {
            Err(DispatchStepError::ChildFailed { failing_node, .. }) => {
                assert_eq!(failing_node, "FailingNode");
            }
            other => panic!("expected ChildFailed, got {other:?}"),
        }
    }
}
