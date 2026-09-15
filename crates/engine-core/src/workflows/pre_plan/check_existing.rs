//! `CheckExistingNotesNode` — `PRE_PLAN`'s idempotency guard (`EN.19.A` task 1).
//!
//! No model call. Mirrors `sdlc_flow::setup::SpecExistsRouterNode`'s own
//! `exists()`-based idempotency pattern: this node checks, BEFORE any
//! research runs, whether
//! `<brain_root>/planning/open-work/pre-plan/<slug>/notes.md` already exists
//! on disk. Unlike `SpecExistsRouterNode` (which only routes between two
//! *continuation* paths), this node's job is to short-circuit the whole run
//! when a prior run (or a human-authored `/capture`) already produced the
//! target file — reporting the existing path rather than silently
//! overwriting it — unless the caller sets `force_regenerate: true` on the
//! dispatched event.
//!
//! The path is resolved through `crate::brain_root::resolve_brain_root()`
//! (`EN.7.A`), never a hardcoded repo-relative string — per the block
//! record's 2026-09-15 amendment correcting an earlier
//! `planning/pre-plan/...` path to the brain-root-centralized
//! `$BRAIN_ROOT/planning/open-work/pre-plan/...` shape (D87).
//!
//! `process()` never writes to the filesystem — only `Path::exists()` reads
//! — and stamps `{"notes_path": <path>, "exists": <bool>}` under its own
//! identity so `route()` (which only sees `&TaskContext`, per
//! `crate::routing::Router`'s contract) can read the verdict `process()` just
//! computed, exactly as `workflow.rs`'s walk loop calls `process()` before
//! `route()` on every step.

use std::path::{Path, PathBuf};

use engine_contract::TaskContext;

use crate::brain_root::resolve_brain_root;
use crate::node::{Node, NodeError};
use crate::routing::Router;
use crate::workflows::put_result;

/// The `Node::name()` identity this node registers under, and the
/// `ctx.nodes` key its verdict is stamped onto.
pub const NODE_NAME: &str = "CheckExistingNotesNode";

/// `route()` identity returned when the target `notes.md` already exists and
/// `force_regenerate` was not set. `EN.19.A` task 4 wires this to a
/// short-circuit terminal node that reports the existing path without
/// running research or touching the file.
pub const EXISTS_ROUTE: &str = "PrePlanNotesAlreadyExistsNode";

/// `route()` identity returned to continue the pipeline — either no existing
/// `notes.md` blocks this run, or the caller asked to regenerate it.
pub const CONTINUE_ROUTE: &str = "IntakeIdeaNode";

/// `ctx.nodes[NODE_NAME]` keys.
const NOTES_PATH_KEY: &str = "notes_path";
const EXISTS_KEY: &str = "exists";

/// No-model idempotency guard: checks whether `PRE_PLAN`'s target `notes.md`
/// already exists for the dispatched slug, short-circuiting the run when it
/// does (unless `force_regenerate: true`).
pub struct CheckExistingNotesNode;

impl CheckExistingNotesNode {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for CheckExistingNotesNode {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolve `<brain_root>/planning/open-work/pre-plan/<slug>/notes.md` for
/// `slug`. Centralized here so `write_notes` (task 3) can reuse the identical
/// join order and never drift from what this node already checked.
pub fn notes_path(brain_root: &Path, slug: &str) -> PathBuf {
    brain_root
        .join("planning")
        .join("open-work")
        .join("pre-plan")
        .join(slug)
        .join("notes.md")
}

/// Read `slug` from the dispatched event, failing loudly (naming the field)
/// when it is missing or empty — the same validation shape
/// `intake::IntakeIdeaNode` and `nodes::email::parse_inbound_email` use.
fn require_slug(ctx: &TaskContext) -> Result<String, NodeError> {
    ctx.event
        .get("slug")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| NodeError::new("pre_plan: missing or empty 'slug'"))
}

#[async_trait::async_trait]
impl Node for CheckExistingNotesNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let slug = require_slug(&ctx)?;

        let brain_root = resolve_brain_root().map_err(|err| {
            NodeError::new(format!("pre_plan: could not resolve brain root: {err}"))
        })?;

        let path = notes_path(&brain_root, &slug);

        let force_regenerate = ctx
            .event
            .get("force_regenerate")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        // Read-only: `Path::exists()` never creates or modifies anything on
        // disk. Existence AND the absence of an explicit `force_regenerate`
        // together decide the short-circuit — a stale `notes.md` the caller
        // explicitly asked to regenerate is not a block.
        let exists = path.exists() && !force_regenerate;

        put_result(
            &mut ctx,
            NODE_NAME,
            serde_json::json!({
                NOTES_PATH_KEY: path.display().to_string(),
                EXISTS_KEY: exists,
            }),
        );

        Ok(ctx)
    }

    fn name(&self) -> &str {
        NODE_NAME
    }

    fn as_router(&self) -> Option<&dyn Router> {
        Some(self)
    }
}

impl Router for CheckExistingNotesNode {
    /// Called by the runner AFTER `process()` on this same walk step
    /// (`workflow.rs`'s walk loop), so this reads back the verdict
    /// `process()` just stamped under [`NODE_NAME`] rather than recomputing
    /// it — `route()` takes `&TaskContext` and cannot touch the filesystem
    /// itself.
    fn route(&self, ctx: &TaskContext) -> Option<String> {
        let exists = ctx
            .nodes
            .get(NODE_NAME)
            .and_then(|value| value.get(EXISTS_KEY))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        Some(if exists {
            EXISTS_ROUTE.to_string()
        } else {
            CONTINUE_ROUTE.to_string()
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::brain_root::ENGINE_BRAIN_ROOT_ENV;

    // `ENGINE_BRAIN_ROOT` is process-global state (see `brain_root.rs`'s own
    // tests) — guard every test that touches it so they cannot race the rest
    // of the suite.
    static ENV_GUARD: Mutex<()> = Mutex::new(());

    fn empty_context(event: serde_json::Value) -> TaskContext {
        TaskContext {
            event,
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        }
    }

    /// RAII-ish guard: sets `ENGINE_BRAIN_ROOT` for the duration of the held
    /// lock, restoring the previous value on drop. Deliberately synchronous
    /// (unlike a closure-taking helper) so the env var stays set across an
    /// `.await` inside the test body rather than being restored before the
    /// awaited future's code actually runs.
    struct BrainRootGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        previous: Option<String>,
    }

    impl BrainRootGuard {
        fn set(root: &Path) -> Self {
            let lock = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
            let previous = std::env::var(ENGINE_BRAIN_ROOT_ENV).ok();
            std::env::set_var(ENGINE_BRAIN_ROOT_ENV, root);
            Self {
                _lock: lock,
                previous,
            }
        }
    }

    impl Drop for BrainRootGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(v) => std::env::set_var(ENGINE_BRAIN_ROOT_ENV, v),
                None => std::env::remove_var(ENGINE_BRAIN_ROOT_ENV),
            }
        }
    }

    #[tokio::test]
    async fn process_reports_not_existing_and_routes_to_intake_when_notes_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _guard = BrainRootGuard::set(dir.path());

        let node = CheckExistingNotesNode::new();
        let ctx = empty_context(json!({"slug": "some-idea"}));

        let ctx = node.process(ctx).await.expect("process should succeed");
        let stored = ctx.nodes.get(NODE_NAME).expect("result stored");
        assert_eq!(
            stored.get(EXISTS_KEY).and_then(|v| v.as_bool()),
            Some(false)
        );

        let route = node.route(&ctx);
        assert_eq!(route, Some(CONTINUE_ROUTE.to_string()));
    }

    #[tokio::test]
    async fn process_reports_existing_and_routes_to_short_circuit_when_notes_present() {
        let dir = tempfile::tempdir().expect("tempdir");
        let slug = "already-there";
        let target = notes_path(dir.path(), slug);
        std::fs::create_dir_all(target.parent().unwrap()).expect("mkdir");
        std::fs::write(&target, "# existing notes\n").expect("write fixture");

        let _guard = BrainRootGuard::set(dir.path());

        let node = CheckExistingNotesNode::new();
        let ctx = empty_context(json!({"slug": slug}));

        let ctx = node.process(ctx).await.expect("process should succeed");
        let stored = ctx.nodes.get(NODE_NAME).expect("result stored");
        assert_eq!(stored.get(EXISTS_KEY).and_then(|v| v.as_bool()), Some(true));
        assert_eq!(
            stored.get(NOTES_PATH_KEY).and_then(|v| v.as_str()),
            Some(target.display().to_string().as_str())
        );

        let route = node.route(&ctx);
        assert_eq!(route, Some(EXISTS_ROUTE.to_string()));
    }

    #[tokio::test]
    async fn force_regenerate_continues_even_when_notes_exist() {
        let dir = tempfile::tempdir().expect("tempdir");
        let slug = "force-me";
        let target = notes_path(dir.path(), slug);
        std::fs::create_dir_all(target.parent().unwrap()).expect("mkdir");
        std::fs::write(&target, "# existing notes\n").expect("write fixture");

        let _guard = BrainRootGuard::set(dir.path());

        let node = CheckExistingNotesNode::new();
        let ctx = empty_context(json!({"slug": slug, "force_regenerate": true}));

        let ctx = node.process(ctx).await.expect("process should succeed");
        let stored = ctx.nodes.get(NODE_NAME).expect("result stored");
        assert_eq!(
            stored.get(EXISTS_KEY).and_then(|v| v.as_bool()),
            Some(false)
        );

        let route = node.route(&ctx);
        assert_eq!(route, Some(CONTINUE_ROUTE.to_string()));
    }

    #[tokio::test]
    async fn process_never_writes_to_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _guard = BrainRootGuard::set(dir.path());

        let node = CheckExistingNotesNode::new();
        let ctx = empty_context(json!({"slug": "no-write-check"}));

        node.process(ctx).await.expect("process should succeed");

        // The slug's directory must never have been created by this
        // read-only check.
        let dir_for_slug = dir
            .path()
            .join("planning/open-work/pre-plan/no-write-check");
        assert!(!dir_for_slug.exists());
    }

    #[tokio::test]
    async fn process_errors_when_slug_is_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _guard = BrainRootGuard::set(dir.path());

        let node = CheckExistingNotesNode::new();
        let ctx = empty_context(json!({}));

        let err = node
            .process(ctx)
            .await
            .expect_err("should fail without slug");
        assert!(err.message.contains("slug"));
    }

    #[test]
    fn name_matches_node_name_const() {
        assert_eq!(CheckExistingNotesNode::new().name(), NODE_NAME);
    }
}
