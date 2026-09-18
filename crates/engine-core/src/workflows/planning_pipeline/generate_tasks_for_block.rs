//! `GenerateTasksForBlockNode` — composes `sdlc_flow::setup::GenerateTasksNode`
//! (EN.19.C's block-record-aware decomposer) directly for `PLANNING_PIPELINE`'s
//! `generate_tasks` stage (`EN.19.D` task 5).
//!
//! **Composition, never a fork.** This node never re-implements
//! `GenerateTasksNode`'s block-record-vs-planning-fallback branching, its
//! prompt, or its `model_tiers.generate_from_block` resolution — it builds a
//! fresh, minimal inner [`TaskContext`] (an `{"spec_slug": <slug>}` event,
//! a `SetupWorktreeNode`-shaped `worktree_path` stamp, and that root's own
//! resolved `SdlcPolicy` stamped under
//! [`crate::policy::RESOLVED_POLICY_IDENTITY`] — exactly the two stamps a
//! real `SDLC_FLOW` walk leaves before `GenerateTasksNode` ever runs) and
//! hands it to [`GenerateTasksNode::process`] unmodified. Every `ctx.nodes`
//! entry that inner walk produces (`GenerateTasksNode`'s own cost/output
//! stamp, the `ResolvedPolicy` stamp) is merged back into the outer `ctx`
//! this node was called with, so downstream telemetry
//! (`RunTelemetry`/`PolicyAggregate`) sees this stage exactly as it would
//! from the full `SDLC_FLOW` graph.
//!
//! Stops after `tasks.json`/`tasks.md` are written — no `dispatch` side
//! effects; task 6's `DispatchNode` owns that stage.
//!
//! **Root resolution.** `PLANNING_PIPELINE` resolves within one target repo
//! per run, never cross-repo (`EN.19.D`'s `out_of_scope`), so this node
//! never re-derives a `repo` slug through `RepoRegistry`. It defaults to
//! `std::env::current_dir()` — byte-identical to
//! `sdlc_flow::setup::resolve_target_root`'s own `repo`-less fallback — and
//! [`GenerateTasksForBlockNode::with_target_root`] overrides it for a test,
//! or for a future caller that already resolved a worktree.
//!
//! **Idempotency.** Mirrors `SpecExistsRouterNode`'s own `exists()`-based
//! guard (`setup.rs`): before ever composing `GenerateTasksNode` (i.e.
//! before any model call), this node checks whether
//! `<target_root>/planning/<slug>/tasks.json` already exists. When it does,
//! the stage short-circuits — reporting the existing path, never
//! regenerating or overwriting it — the same guard that keeps a resumed
//! `SDLC_FLOW`/`SDLC_TASK` walk from re-decomposing a spec that already has
//! a task list.
//!
//! **Transport/cancellation forwarding.** This node's own LLM call happens
//! entirely inside the composed `GenerateTasksNode`, which already
//! implements `llm_node::{TransportSlotted, Cancellable}` (standing rule
//! 11). Rather than holding a `TransportSlot` this node cannot forward
//! (its fields are private to `workflows::transport_slot`), this node
//! holds the same plain/meta transport pair `GenerateTasksNode` accepts and
//! an optional `CancellationToken`, forwarding whichever is set onto the
//! freshly-constructed inner `GenerateTasksNode` before calling
//! `process` — meta transport taking precedence over plain, mirroring
//! `TransportSlot::apply`'s own documented precedence.

use std::path::PathBuf;

use engine_contract::TaskContext;
use serde_json::json;

use crate::cancellation::CancellationToken;
use crate::node::{Node, NodeError};
use crate::nodes::MetaTransport;
use crate::policy::stamp_resolved_policy;
use crate::workflows::llm_node::{Cancellable, TransportSlotted};
use crate::workflows::put_result;
use crate::workflows::sdlc_flow::setup::{resolve_policy_for_run, GenerateTasksNode};
use crate::workflows::ModelTransport;

/// The `Node::name()` identity this node registers under, and the
/// `ctx.nodes` key its verdict is stamped onto.
pub const NODE_NAME: &str = "GenerateTasksForBlockNode";

/// `ctx.nodes[NODE_NAME]` keys.
const SLUG_KEY: &str = "slug";
const TASKS_JSON_KEY: &str = "tasks_json";
const TASKS_MD_KEY: &str = "tasks_md";
const TASK_COUNT_KEY: &str = "task_count";
const SHORT_CIRCUITED_KEY: &str = "short_circuited";

/// Read the required string `slug` out of the dispatched event — the same
/// validation shape `pre_plan::check_existing::require_slug` and
/// `plan_authoring::check_existing::parse_slug` use.
fn require_slug(ctx: &TaskContext) -> Result<String, NodeError> {
    ctx.event
        .get(SLUG_KEY)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| NodeError::new(format!("{NODE_NAME}: missing or empty 'slug'")))
}

/// `<root>/planning/<slug>` — mirrors `sdlc_flow::setup::spec_dir`'s own
/// join order exactly (that helper is private to its module, so this is a
/// deliberate, narrow re-derivation rather than an import) so this node's
/// short-circuit check and the inner `GenerateTasksNode` call always agree
/// on which directory is being read/written.
fn spec_dir(root: &std::path::Path, slug: &str) -> PathBuf {
    root.join("planning").join(slug)
}

/// Best-effort task count out of an existing `tasks.json` — `None` when the
/// file is missing, unreadable, unparsable, or not a JSON array. Enriches
/// the short-circuit report only; never gates the short-circuit decision
/// itself (existence alone does, mirroring `SpecExistsRouterNode`).
fn existing_task_count(tasks_json_path: &std::path::Path) -> Option<usize> {
    let content = std::fs::read_to_string(tasks_json_path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    value.as_array().map(std::vec::Vec::len)
}

/// Composes `GenerateTasksNode` directly for `PLANNING_PIPELINE`'s
/// `generate_tasks` stage. See the module docs for the idempotency guard
/// and the merge-back-into-outer-`ctx` composition shape.
#[derive(Default)]
pub struct GenerateTasksForBlockNode {
    /// Overrides root resolution (defaults to `std::env::current_dir()`,
    /// byte-identical to `resolve_target_root`'s own `repo`-less
    /// fallback). Tests use this so nothing here ever depends on the test
    /// binary's own working directory.
    target_root: Option<PathBuf>,
    /// Forwarded onto the inner `GenerateTasksNode` via its own
    /// `with_transport`, never applied directly — see the module docs'
    /// "Transport/cancellation forwarding" section.
    transport: Option<ModelTransport>,
    /// Forwarded onto the inner `GenerateTasksNode` via its own
    /// `with_meta_transport`; takes precedence over `transport` when both
    /// are set, mirroring `TransportSlot::apply`.
    meta_transport: Option<MetaTransport>,
    /// Forwarded onto the inner `GenerateTasksNode` via its own
    /// `with_cancellation_token`.
    cancellation_token: Option<CancellationToken>,
}

impl GenerateTasksForBlockNode {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Override the target root this node resolves `planning/<slug>`
    /// against. Absent this, resolution falls back to
    /// `std::env::current_dir()`.
    #[must_use]
    pub fn with_target_root(mut self, root: PathBuf) -> Self {
        self.target_root = Some(root);
        self
    }

    /// Forwarded onto the inner `GenerateTasksNode`. Tests use this to stub
    /// a real subprocess call with a canned `Outcome`.
    #[must_use]
    pub fn with_transport(mut self, transport: ModelTransport) -> Self {
        self.transport = Some(transport);
        self
    }

    /// Forwarded onto the inner `GenerateTasksNode`; takes precedence over
    /// [`Self::with_transport`] when both are set.
    #[must_use]
    pub fn with_meta_transport(mut self, transport: MetaTransport) -> Self {
        self.meta_transport = Some(transport);
        self
    }

    /// Forwarded onto the inner `GenerateTasksNode`.
    #[must_use]
    pub fn with_cancellation_token(mut self, token: CancellationToken) -> Self {
        self.cancellation_token = Some(token);
        self
    }

    /// [`Self::target_root`] when set, else `std::env::current_dir()` —
    /// byte-identical to `resolve_target_root`'s own `repo`-less fallback.
    fn resolve_root(&self) -> Result<PathBuf, NodeError> {
        match &self.target_root {
            Some(root) => Ok(root.clone()),
            None => std::env::current_dir().map_err(|err| {
                NodeError::new(format!(
                    "{NODE_NAME}: failed to resolve current_dir(): {err}"
                ))
            }),
        }
    }
}

#[async_trait::async_trait]
impl Node for GenerateTasksForBlockNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let slug = require_slug(&ctx)?;
        let root = self.resolve_root()?;
        let dir = spec_dir(&root, &slug);
        let tasks_json_path = dir.join("tasks.json");
        let tasks_md_path = dir.join("tasks.md");

        // Idempotency guard, checked BEFORE any model call — mirrors
        // `SpecExistsRouterNode`'s own exists()-based short-circuit.
        if tasks_json_path.exists() {
            put_result(
                &mut ctx,
                NODE_NAME,
                json!({
                    SLUG_KEY: slug,
                    TASKS_JSON_KEY: tasks_json_path.to_string_lossy(),
                    TASKS_MD_KEY: tasks_md_path.to_string_lossy(),
                    TASK_COUNT_KEY: existing_task_count(&tasks_json_path),
                    SHORT_CIRCUITED_KEY: true,
                }),
            );
            return Ok(ctx);
        }

        // A minimal, `SDLC_FLOW`-shaped inner context: `spec_slug` is the
        // only field `GenerateTasksNode`'s own `parse_event` requires, and
        // the `SetupWorktreeNode` stamp makes its private `spec_dir` helper
        // resolve to exactly the same `dir` this node just checked —
        // mirroring `ctx_with_worktree_and_policy`'s test shape, which is
        // the identical stamp a real `SDLC_FLOW` walk leaves before
        // `GenerateTasksNode` runs.
        let mut inner_ctx = TaskContext {
            event: json!({ "spec_slug": slug }),
            nodes: std::collections::HashMap::new(),
            metadata: json!({}),
            node_runs: std::collections::HashMap::new(),
        };
        inner_ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": root.to_string_lossy() }),
        );

        let resolved_policy = resolve_policy_for_run(&inner_ctx, &root)?;
        stamp_resolved_policy(&mut inner_ctx, &resolved_policy)?;

        let mut inner_node = GenerateTasksNode::new();
        if let Some(meta) = self.meta_transport.clone() {
            inner_node = inner_node.with_meta_transport(meta);
        } else if let Some(plain) = self.transport.clone() {
            inner_node = inner_node.with_transport(plain);
        }
        if let Some(token) = self.cancellation_token.clone() {
            inner_node = inner_node.with_cancellation_token(token);
        }

        let inner_ctx = inner_node.process(inner_ctx).await?;

        // Merge back every `ctx.nodes` entry the inner walk produced — the
        // `ResolvedPolicy` stamp this node itself stamped, plus
        // `GenerateTasksNode`'s own cost/output stamp — so downstream
        // telemetry sees this stage exactly as it would from the full
        // SDLC_FLOW graph, never a fork of GenerateTasksNode's output.
        for (identity, value) in inner_ctx.nodes {
            ctx.nodes.insert(identity, value);
        }

        let task_count = ctx
            .nodes
            .get("GenerateTasksNode")
            .and_then(|value| value.get(TASK_COUNT_KEY))
            .cloned();

        put_result(
            &mut ctx,
            NODE_NAME,
            json!({
                SLUG_KEY: slug,
                TASKS_JSON_KEY: tasks_json_path.to_string_lossy(),
                TASKS_MD_KEY: tasks_md_path.to_string_lossy(),
                TASK_COUNT_KEY: task_count,
                SHORT_CIRCUITED_KEY: false,
            }),
        );

        Ok(ctx)
    }

    fn name(&self) -> &str {
        NODE_NAME
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    use claude_code_rs::Outcome;
    use futures::future::BoxFuture;

    use super::*;
    use crate::workflows::get_result;

    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A named scratch directory under the OS temp dir, guaranteed EMPTY at
    /// the moment it is returned — mirrors
    /// `sdlc_flow::setup::tests::temp_dir`'s own remove-then-recreate
    /// pattern (2026-07-31 PID-reuse false-FAIL fix), duplicated here
    /// rather than imported since that helper is private to its module.
    fn temp_dir() -> PathBuf {
        let n = TMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "engine-core-planning-pipeline-generate-tasks-for-block-test-{}-{n}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn empty_context(event: serde_json::Value) -> TaskContext {
        TaskContext {
            event,
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        }
    }

    fn canned_outcome(text: String) -> Outcome {
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

    fn stub_transport(tasks_json: String) -> ModelTransport {
        Arc::new(move |_config, _prompt| {
            let outcome = canned_outcome(tasks_json.clone());
            Box::pin(async move { Ok(outcome) })
                as BoxFuture<'static, claude_code_rs::Result<Outcome>>
        })
    }

    fn panics_if_called() -> ModelTransport {
        Arc::new(|_config, _prompt| {
            panic!(
                "GenerateTasksForBlockNode called the model transport during a short-circuited run"
            )
        })
    }

    #[tokio::test]
    async fn missing_slug_is_a_named_error() {
        let err = GenerateTasksForBlockNode::new()
            .process(empty_context(json!({})))
            .await
            .expect_err("missing slug rejected");
        assert!(err.message.contains("slug"));
    }

    #[tokio::test]
    async fn short_circuits_when_tasks_json_already_exists_without_calling_the_model() {
        let root = temp_dir();
        let dir = root.join("planning").join("my-slug");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("tasks.json"),
            json!([{ "task_id": 1 }, { "task_id": 2 }]).to_string(),
        )
        .unwrap();

        let node = GenerateTasksForBlockNode::new()
            .with_target_root(root.clone())
            .with_transport(panics_if_called());

        let ctx = node
            .process(empty_context(json!({ "slug": "my-slug" })))
            .await
            .expect("short-circuit path succeeds without ever calling the model");

        let stored = get_result(&ctx, NODE_NAME).expect("result stored");
        assert_eq!(stored.get(SHORT_CIRCUITED_KEY), Some(&json!(true)));
        assert_eq!(stored.get(TASK_COUNT_KEY), Some(&json!(2)));
        assert_eq!(
            stored.get(TASKS_JSON_KEY).and_then(|v| v.as_str()),
            Some(dir.join("tasks.json").to_string_lossy().as_ref())
        );

        // Never rewritten — still the original two-task fixture, byte for
        // byte.
        let on_disk = std::fs::read_to_string(dir.join("tasks.json")).unwrap();
        assert_eq!(
            on_disk,
            json!([{ "task_id": 1 }, { "task_id": 2 }]).to_string()
        );
    }

    #[tokio::test]
    async fn compose_path_calls_generate_tasks_node_and_writes_both_output_files() {
        let root = temp_dir();
        let generated = json!({
            "tasks": [
                { "task_id": 1, "title": "first", "description": "d", "files": [] }
            ],
            "tasks_markdown": "# Tasks\n\n1. first\n",
        })
        .to_string();

        let node = GenerateTasksForBlockNode::new()
            .with_target_root(root.clone())
            .with_transport(stub_transport(generated));

        let ctx = node
            .process(empty_context(json!({ "slug": "fresh-slug" })))
            .await
            .expect("compose path succeeds");

        let dir = root.join("planning").join("fresh-slug");
        assert!(
            dir.join("tasks.json").is_file(),
            "tasks.json must be written"
        );
        assert!(dir.join("tasks.md").is_file(), "tasks.md must be written");
        let tasks_md = std::fs::read_to_string(dir.join("tasks.md")).unwrap();
        assert!(tasks_md.contains("first"));

        let stored = get_result(&ctx, NODE_NAME).expect("result stored");
        assert_eq!(stored.get(SHORT_CIRCUITED_KEY), Some(&json!(false)));
        assert_eq!(stored.get(TASK_COUNT_KEY), Some(&json!(1)));

        // Composition, never a fork: the inner GenerateTasksNode's own
        // result is merged back into the outer ctx.
        let inner_stamp = get_result(&ctx, "GenerateTasksNode")
            .expect("GenerateTasksNode's own result was merged into the outer ctx");
        assert_eq!(inner_stamp.get(TASK_COUNT_KEY), Some(&json!(1)));
    }

    #[tokio::test]
    async fn a_second_run_after_compose_short_circuits() {
        let root = temp_dir();
        let generated = json!({
            "tasks": [{ "task_id": 1, "title": "t", "description": "d", "files": [] }],
            "tasks_markdown": "# Tasks\n",
        })
        .to_string();

        let first = GenerateTasksForBlockNode::new()
            .with_target_root(root.clone())
            .with_transport(stub_transport(generated));
        let ctx = first
            .process(empty_context(json!({ "slug": "resumed-slug" })))
            .await
            .expect("first run composes");
        assert_eq!(
            get_result(&ctx, NODE_NAME)
                .unwrap()
                .get(SHORT_CIRCUITED_KEY),
            Some(&json!(false))
        );

        // A fresh node instance (a real re-dispatch would construct a new
        // one), same target root, model transport that must never fire.
        let second = GenerateTasksForBlockNode::new()
            .with_target_root(root)
            .with_transport(panics_if_called());
        let ctx2 = second
            .process(empty_context(json!({ "slug": "resumed-slug" })))
            .await
            .expect("second run short-circuits instead of regenerating");
        let stored = get_result(&ctx2, NODE_NAME).expect("result stored");
        assert_eq!(stored.get(SHORT_CIRCUITED_KEY), Some(&json!(true)));
        assert_eq!(stored.get(TASK_COUNT_KEY), Some(&json!(1)));
    }

    #[test]
    fn name_matches_node_name_const() {
        assert_eq!(GenerateTasksForBlockNode::new().name(), NODE_NAME);
    }
}
