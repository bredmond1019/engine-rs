//! `CheckExistingPlanNode` — no model call. Checks whether
//! `<brain_root>/planning/open-work/pre-plan/<slug>/plan.md` and/or its
//! sibling `candidate-blocks/` directory already exist for this run's slug
//! BEFORE `DecomposePlanNode` (task 2) ever calls a model, short-circuiting
//! the run unless the event sets `force_regenerate: true` (`EN.19.B` task
//! 1; block record acceptance criterion 5).
//!
//! Per the `Router` contract (`crate::routing`), returning `None` from
//! `route()` ends the walk with no further node executing — exactly what
//! the block record's `what` asks for ("the run stops here and reports
//! what already exists rather than silently re-decomposing and
//! overwriting"). This node's own `process()` stamps the full report
//! (which path(s) existed, and why) into `ctx.nodes` before `route()` is
//! ever consulted, so the short-circuit case is a legitimate terminal, not
//! a swallowed failure.
//!
//! See `super` (this module's `mod.rs`) for why the block record's
//! requested `ExistsGuardNode` extraction is deferred rather than done
//! here.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use engine_contract::TaskContext;

use crate::brain_root::resolve_brain_root;
use crate::node::{Node, NodeError};
use crate::routing::Router;
use crate::workflows::{get_result, put_result};

/// The `Node::name()` identity `CheckExistingPlanNode` is registered
/// under, and the `ctx.nodes` key its report is stamped onto.
pub const NODE_NAME: &str = "CheckExistingPlanNode";

/// The next node's identity when nothing exists yet (or `force_regenerate`
/// was set) and decomposition should proceed. Task 5's graph assembly
/// wires this as the router's declared continue-edge.
pub const CONTINUE_TARGET: &str = "GatherPlanContextNode";

/// Injectable filesystem seam so this module's nodes never touch the real
/// disk in tests. Mirrors `claim_reaffirm::load_claims::ClaimLaneFs`'s
/// shape; shared with `super::gather_context`, which reads files rather
/// than only checking existence.
pub trait PlanAuthoringFs: Send + Sync {
    /// Whether `path` exists on disk, as either a file or a directory.
    fn exists(&self, path: &Path) -> bool;

    /// Read `path`'s contents, or `None` when the file does not exist or
    /// cannot be read.
    fn read_to_string(&self, path: &Path) -> Option<String>;
}

/// The live [`PlanAuthoringFs`] backed by real `std::fs` calls — the only
/// place in this module (and its sibling `gather_context.rs`) real
/// filesystem access happens, isolated behind this seam.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealPlanAuthoringFs;

impl PlanAuthoringFs for RealPlanAuthoringFs {
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn read_to_string(&self, path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }
}

/// `<brain_root>/planning/open-work/pre-plan/<slug>` — the D87-centralized
/// pre-plan folder shape, shared by this node and
/// [`super::gather_context::GatherPlanContextNode`] so both resolve the
/// identical folder for the identical slug.
pub fn pre_plan_dir(brain_root: &Path, slug: &str) -> PathBuf {
    brain_root
        .join("planning")
        .join("open-work")
        .join("pre-plan")
        .join(slug)
}

/// Read the required string `slug` out of the inbound event. A
/// `PLAN_AUTHORING` event with no slug cannot resolve any path this
/// workflow touches.
pub(super) fn parse_slug(ctx: &TaskContext) -> Result<String, NodeError> {
    ctx.event
        .get("slug")
        .and_then(|value| value.as_str())
        .map(|slug| slug.to_string())
        .ok_or_else(|| {
            NodeError::new("PLAN_AUTHORING event is missing a required string \"slug\" field")
        })
}

/// Read `force_regenerate` out of the inbound event. Defaults to `false`
/// when absent or not exactly `true` — the safe default is "don't
/// overwrite".
fn parse_force_regenerate(ctx: &TaskContext) -> bool {
    ctx.event
        .get("force_regenerate")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

/// Checks whether this slug's `plan.md` and/or `candidate-blocks/` already
/// exist, short-circuiting the run unless `force_regenerate: true`.
pub struct CheckExistingPlanNode {
    fs: Arc<dyn PlanAuthoringFs>,
    brain_root_resolver: Arc<dyn Fn() -> Result<PathBuf, String> + Send + Sync>,
}

impl Default for CheckExistingPlanNode {
    fn default() -> Self {
        Self::new()
    }
}

impl CheckExistingPlanNode {
    /// The production node: real filesystem, real `resolve_brain_root()`
    /// (honouring `ENGINE_BRAIN_ROOT`, then a `brain.toml` walk-up).
    #[must_use]
    pub fn new() -> Self {
        Self {
            fs: Arc::new(RealPlanAuthoringFs),
            brain_root_resolver: Arc::new(|| resolve_brain_root().map_err(|err| err.to_string())),
        }
    }

    /// Override the filesystem seam. Tests use this so nothing here ever
    /// touches the real disk.
    #[must_use]
    pub fn with_fs(mut self, fs: Arc<dyn PlanAuthoringFs>) -> Self {
        self.fs = fs;
        self
    }

    /// Override brain-root resolution entirely (e.g. a fixed tempdir path
    /// in tests), bypassing `ENGINE_BRAIN_ROOT`/`brain.toml` walk-up.
    #[must_use]
    pub fn with_brain_root(mut self, root: PathBuf) -> Self {
        self.brain_root_resolver = Arc::new(move || Ok(root.clone()));
        self
    }
}

#[async_trait::async_trait]
impl Node for CheckExistingPlanNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let slug = parse_slug(&ctx)?;
        let force_regenerate = parse_force_regenerate(&ctx);
        let brain_root = (self.brain_root_resolver)().map_err(NodeError::new)?;

        let dir = pre_plan_dir(&brain_root, &slug);
        let plan_path = dir.join("plan.md");
        let candidate_blocks_dir = dir.join("candidate-blocks");

        let plan_exists = self.fs.exists(&plan_path);
        let candidates_exist = self.fs.exists(&candidate_blocks_dir);
        let short_circuit = (plan_exists || candidates_exist) && !force_regenerate;

        put_result(
            &mut ctx,
            NODE_NAME,
            serde_json::json!({
                "slug": slug,
                "plan_path": plan_path.to_string_lossy(),
                "candidate_blocks_dir": candidate_blocks_dir.to_string_lossy(),
                "plan_exists": plan_exists,
                "candidates_exist": candidates_exist,
                "force_regenerate": force_regenerate,
                "short_circuit": short_circuit,
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

impl Router for CheckExistingPlanNode {
    fn route(&self, ctx: &TaskContext) -> Option<String> {
        let stored = get_result(ctx, NODE_NAME)?;
        let short_circuit = stored.get("short_circuit")?.as_bool().unwrap_or(false);
        if short_circuit {
            None
        } else {
            Some(CONTINUE_TARGET.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::collections::HashSet;

    use serde_json::json;

    use super::*;

    #[derive(Default)]
    struct StubFs {
        existing: HashSet<PathBuf>,
    }

    impl StubFs {
        fn with_existing(paths: &[PathBuf]) -> Self {
            Self {
                existing: paths.iter().cloned().collect(),
            }
        }
    }

    impl PlanAuthoringFs for StubFs {
        fn exists(&self, path: &Path) -> bool {
            self.existing.contains(path)
        }

        fn read_to_string(&self, _path: &Path) -> Option<String> {
            None
        }
    }

    fn ctx_with_event(event: serde_json::Value) -> TaskContext {
        TaskContext {
            event,
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        }
    }

    fn node_with(root: PathBuf, fs: StubFs) -> CheckExistingPlanNode {
        CheckExistingPlanNode::new()
            .with_brain_root(root)
            .with_fs(Arc::new(fs))
    }

    #[tokio::test]
    async fn resolves_paths_under_the_injected_brain_root_never_a_hardcoded_prefix() {
        let root = PathBuf::from("/tmp/en-19-b-brain-root");
        let node = node_with(root.clone(), StubFs::default());
        let ctx = node
            .process(ctx_with_event(json!({ "slug": "my-slug" })))
            .await
            .expect("process should succeed");
        let stored = get_result(&ctx, NODE_NAME).expect("result stored");
        assert_eq!(
            stored.get("plan_path").and_then(|v| v.as_str()),
            Some(
                root.join("planning/open-work/pre-plan/my-slug/plan.md")
                    .to_string_lossy()
                    .as_ref()
            )
        );
        assert_eq!(
            stored.get("candidate_blocks_dir").and_then(|v| v.as_str()),
            Some(
                root.join("planning/open-work/pre-plan/my-slug/candidate-blocks")
                    .to_string_lossy()
                    .as_ref()
            )
        );
    }

    #[tokio::test]
    async fn neither_existing_continues_to_gather_context() {
        let root = PathBuf::from("/tmp/en-19-b-neither-exists");
        let node = node_with(root, StubFs::default());
        let ctx = node
            .process(ctx_with_event(json!({ "slug": "fresh-slug" })))
            .await
            .expect("process should succeed");
        let stored = get_result(&ctx, NODE_NAME).expect("result stored");
        assert_eq!(stored.get("short_circuit"), Some(&json!(false)));
        assert_eq!(
            node.route(&ctx),
            Some(CONTINUE_TARGET.to_string()),
            "must continue toward GatherPlanContextNode"
        );
    }

    #[tokio::test]
    async fn plan_md_existing_short_circuits() {
        let root = PathBuf::from("/tmp/en-19-b-plan-exists");
        let plan_path = pre_plan_dir(&root, "existing-slug").join("plan.md");
        let node = node_with(root, StubFs::with_existing(&[plan_path]));
        let ctx = node
            .process(ctx_with_event(json!({ "slug": "existing-slug" })))
            .await
            .expect("process should succeed");
        let stored = get_result(&ctx, NODE_NAME).expect("result stored");
        assert_eq!(stored.get("short_circuit"), Some(&json!(true)));
        assert_eq!(node.route(&ctx), None, "must stop the walk here");
    }

    #[tokio::test]
    async fn candidate_blocks_dir_existing_short_circuits() {
        let root = PathBuf::from("/tmp/en-19-b-candidates-exist");
        let candidates_dir = pre_plan_dir(&root, "existing-slug").join("candidate-blocks");
        let node = node_with(root, StubFs::with_existing(&[candidates_dir]));
        let ctx = node
            .process(ctx_with_event(json!({ "slug": "existing-slug" })))
            .await
            .expect("process should succeed");
        let stored = get_result(&ctx, NODE_NAME).expect("result stored");
        assert_eq!(stored.get("short_circuit"), Some(&json!(true)));
        assert_eq!(node.route(&ctx), None);
    }

    #[tokio::test]
    async fn force_regenerate_proceeds_even_when_plan_md_exists() {
        let root = PathBuf::from("/tmp/en-19-b-force-regenerate");
        let plan_path = pre_plan_dir(&root, "existing-slug").join("plan.md");
        let node = node_with(root, StubFs::with_existing(&[plan_path]));
        let ctx = node
            .process(ctx_with_event(json!({
                "slug": "existing-slug",
                "force_regenerate": true,
            })))
            .await
            .expect("process should succeed");
        let stored = get_result(&ctx, NODE_NAME).expect("result stored");
        assert_eq!(stored.get("short_circuit"), Some(&json!(false)));
        assert_eq!(node.route(&ctx), Some(CONTINUE_TARGET.to_string()));
    }

    #[tokio::test]
    async fn missing_slug_is_a_named_error() {
        let node = node_with(PathBuf::from("/tmp/en-19-b-no-slug"), StubFs::default());
        let err = node
            .process(ctx_with_event(json!({})))
            .await
            .expect_err("should reject an event with no slug");
        assert!(err.message.contains("slug"));
    }

    #[test]
    fn as_router_is_some() {
        let node = CheckExistingPlanNode::new();
        assert!(node.as_router().is_some());
    }
}
