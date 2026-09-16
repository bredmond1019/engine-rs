//! `GatherPlanContextNode` — no model call. Reads this run's target repo's
//! own `CLAUDE.md` and `planning/context.md`, that repo's highest
//! `wave`/`phase` from its own `planning/state.json`, and any of
//! `<brain_root>/planning/open-work/pre-plan/<slug>/{notes.md,sequence.md,
//! seams.md,assessment.md}` that exist — mirroring `.claude/commands/plan.md`'s
//! own steps 3/3a/4. Stamps everything gathered into
//! `ctx.nodes["GatherPlanContextNode"]` for `DecomposePlanNode` (task 2)
//! to consume.
//!
//! # Repo root vs brain root
//!
//! `CLAUDE.md`/`context.md`/`state.json` are read relative to the run's
//! **target repo** — the process cwd by default, mirroring
//! `sdlc_flow`/`sdlc_task`'s own default before a `repo` slug is resolved
//! through a registry (`sdlc_flow::setup::resolve_target_root`). No
//! repo-registry slug is on the `PLAN_AUTHORING` event schema yet; a later
//! task can add one following that same precedent, so
//! [`GatherPlanContextNode::with_repo_root`] is the seam that call site
//! would use. The pre-plan folder, by contrast, is always read relative to
//! the **brain root** (`crate::brain_root::resolve_brain_root`), per D87 —
//! pre-plan output is centralized at `$BRAIN_ROOT/planning/open-work/pre-plan/<slug>/`,
//! not per-repo.
//!
//! # Highest wave, not highest phase
//!
//! `.claude/workflows/block.schema.json`'s per-block `phase` field is
//! never actually written into this repo's own `planning/state.json` —
//! every `tracks[].blocks[]` entry there carries `wave` instead (confirmed
//! by inspecting this file's own `planning/state.json` while writing this
//! node: every block key present is `{wave, title, status, depends_on,
//! origin, priority, due, sdlc_workflow, model, epics, ...}`, no `phase`
//! key anywhere). [`highest_wave`] reads `phase` first when present (for
//! forward compatibility, should a future `state.json` ever carry it) and
//! falls back to `wave`, so this node degrades gracefully either way.

use std::path::PathBuf;
use std::sync::Arc;

use engine_contract::TaskContext;
use serde_json::Value;

use crate::brain_root::resolve_brain_root;
use crate::node::{Node, NodeError};
use crate::workflows::put_result;

use super::check_existing::{parse_slug, pre_plan_dir, PlanAuthoringFs, RealPlanAuthoringFs};

/// The `Node::name()` identity `GatherPlanContextNode` is registered
/// under, and the `ctx.nodes` key its gathered context is stamped onto.
pub const NODE_NAME: &str = "GatherPlanContextNode";

/// Pre-plan folder file stems this node reads when present, skipping any
/// that are absent (never an error) — mirrors `.claude/commands/plan.md`
/// step 4's own input set.
const PRE_PLAN_FILE_STEMS: [&str; 4] = ["notes.md", "sequence.md", "seams.md", "assessment.md"];

/// Parse the highest `phase` (preferred) or `wave` (fallback) across every
/// `tracks[].blocks[]` entry in a `planning/state.json` document. `None`
/// when the document has no blocks, or fails to parse as JSON — either
/// case degrades this node gracefully rather than erroring (this context
/// is advisory input to `DecomposePlanNode`, not a hard requirement).
fn highest_wave(state_json: &str) -> Option<i64> {
    let parsed: Value = serde_json::from_str(state_json).ok()?;
    let tracks = parsed.get("tracks")?.as_array()?;
    let mut max: Option<i64> = None;
    for track in tracks {
        let Some(blocks) = track.get("blocks").and_then(|value| value.as_array()) else {
            continue;
        };
        for block in blocks {
            let value = block
                .get("phase")
                .and_then(|v| v.as_i64())
                .or_else(|| block.get("wave").and_then(|v| v.as_i64()));
            if let Some(v) = value {
                max = Some(max.map_or(v, |current| current.max(v)));
            }
        }
    }
    max
}

/// Reads `CLAUDE.md`/`planning/context.md`/`planning/state.json` (from the
/// target repo) plus the pre-plan folder (from the brain root) for
/// `DecomposePlanNode` to consume.
pub struct GatherPlanContextNode {
    fs: Arc<dyn PlanAuthoringFs>,
    repo_root_resolver: Arc<dyn Fn() -> Result<PathBuf, String> + Send + Sync>,
    brain_root_resolver: Arc<dyn Fn() -> Result<PathBuf, String> + Send + Sync>,
}

impl Default for GatherPlanContextNode {
    fn default() -> Self {
        Self::new()
    }
}

impl GatherPlanContextNode {
    /// The production node: real filesystem, `std::env::current_dir()` as
    /// the target repo root, real `resolve_brain_root()` for the pre-plan
    /// folder.
    #[must_use]
    pub fn new() -> Self {
        Self {
            fs: Arc::new(RealPlanAuthoringFs),
            repo_root_resolver: Arc::new(|| std::env::current_dir().map_err(|err| err.to_string())),
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

    /// Override repo-root resolution (the source of `CLAUDE.md`/
    /// `context.md`/`state.json`). Defaults to `std::env::current_dir()`.
    #[must_use]
    pub fn with_repo_root(mut self, root: PathBuf) -> Self {
        self.repo_root_resolver = Arc::new(move || Ok(root.clone()));
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
impl Node for GatherPlanContextNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let slug = parse_slug(&ctx)?;
        let repo_root = (self.repo_root_resolver)().map_err(NodeError::new)?;
        let brain_root = (self.brain_root_resolver)().map_err(NodeError::new)?;

        let claude_md = self.fs.read_to_string(&repo_root.join("CLAUDE.md"));
        let context_md = self
            .fs
            .read_to_string(&repo_root.join("planning").join("context.md"));
        let state_json = self
            .fs
            .read_to_string(&repo_root.join("planning").join("state.json"));
        let highest_wave = state_json.as_deref().and_then(highest_wave);

        let pre_plan_folder = pre_plan_dir(&brain_root, &slug);
        let mut pre_plan_files = serde_json::Map::new();
        for stem in PRE_PLAN_FILE_STEMS {
            if let Some(contents) = self.fs.read_to_string(&pre_plan_folder.join(stem)) {
                pre_plan_files.insert(stem.to_string(), Value::String(contents));
            }
        }

        put_result(
            &mut ctx,
            NODE_NAME,
            serde_json::json!({
                "slug": slug,
                "repo_root": repo_root.to_string_lossy(),
                "claude_md": claude_md,
                "context_md": context_md,
                "highest_wave": highest_wave,
                "pre_plan_files": Value::Object(pre_plan_files),
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
    use std::path::Path;

    use serde_json::json;

    use super::*;

    #[derive(Default)]
    struct StubFs {
        files: HashMap<PathBuf, String>,
    }

    impl StubFs {
        fn with_files(files: &[(PathBuf, &str)]) -> Self {
            Self {
                files: files
                    .iter()
                    .map(|(path, contents)| (path.clone(), contents.to_string()))
                    .collect(),
            }
        }
    }

    impl PlanAuthoringFs for StubFs {
        fn exists(&self, path: &Path) -> bool {
            self.files.contains_key(path)
        }

        fn read_to_string(&self, path: &Path) -> Option<String> {
            self.files.get(path).cloned()
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

    #[tokio::test]
    async fn degrades_gracefully_when_every_optional_file_is_absent() {
        let repo_root = PathBuf::from("/tmp/en-19-b-gather-empty-repo");
        let brain_root = PathBuf::from("/tmp/en-19-b-gather-empty-brain");
        let node = GatherPlanContextNode::new()
            .with_repo_root(repo_root)
            .with_brain_root(brain_root)
            .with_fs(Arc::new(StubFs::default()));

        let ctx = node
            .process(ctx_with_event(json!({ "slug": "empty-slug" })))
            .await
            .expect("process should not error when notes.md/sequence.md/seams.md/assessment.md are absent");

        let stored = crate::workflows::get_result(&ctx, NODE_NAME).expect("result stored");
        assert_eq!(stored.get("claude_md"), Some(&Value::Null));
        assert_eq!(stored.get("context_md"), Some(&Value::Null));
        assert_eq!(stored.get("highest_wave"), Some(&Value::Null));
        assert_eq!(stored.get("pre_plan_files"), Some(&json!({})));
    }

    #[tokio::test]
    async fn reads_claude_md_context_md_and_present_pre_plan_files() {
        let repo_root = PathBuf::from("/tmp/en-19-b-gather-full-repo");
        let brain_root = PathBuf::from("/tmp/en-19-b-gather-full-brain");
        let pre_plan_folder = pre_plan_dir(&brain_root, "full-slug");

        let fs = StubFs::with_files(&[
            (repo_root.join("CLAUDE.md"), "# CLAUDE"),
            (repo_root.join("planning").join("context.md"), "# context"),
            (
                repo_root.join("planning").join("state.json"),
                r#"{"tracks":[{"blocks":[{"wave":3},{"wave":7}]}]}"#,
            ),
            (pre_plan_folder.join("notes.md"), "notes body"),
            (pre_plan_folder.join("sequence.md"), "sequence body"),
        ]);

        let node = GatherPlanContextNode::new()
            .with_repo_root(repo_root)
            .with_brain_root(brain_root)
            .with_fs(Arc::new(fs));

        let ctx = node
            .process(ctx_with_event(json!({ "slug": "full-slug" })))
            .await
            .expect("process should succeed");

        let stored = crate::workflows::get_result(&ctx, NODE_NAME).expect("result stored");
        assert_eq!(stored.get("claude_md"), Some(&json!("# CLAUDE")));
        assert_eq!(stored.get("context_md"), Some(&json!("# context")));
        assert_eq!(stored.get("highest_wave"), Some(&json!(7)));
        assert_eq!(
            stored.get("pre_plan_files").and_then(|v| v.get("notes.md")),
            Some(&json!("notes body"))
        );
        assert_eq!(
            stored
                .get("pre_plan_files")
                .and_then(|v| v.get("sequence.md")),
            Some(&json!("sequence body"))
        );
        assert!(stored
            .get("pre_plan_files")
            .and_then(|v| v.get("seams.md"))
            .is_none());
    }

    #[tokio::test]
    async fn missing_slug_is_a_named_error() {
        let node = GatherPlanContextNode::new()
            .with_repo_root(PathBuf::from("/tmp/en-19-b-gather-no-slug-repo"))
            .with_brain_root(PathBuf::from("/tmp/en-19-b-gather-no-slug-brain"))
            .with_fs(Arc::new(StubFs::default()));
        let err = node
            .process(ctx_with_event(json!({})))
            .await
            .expect_err("should reject an event with no slug");
        assert!(err.message.contains("slug"));
    }

    #[test]
    fn highest_wave_prefers_phase_over_wave_when_both_present() {
        let json = r#"{"tracks":[{"blocks":[{"wave":1,"phase":9},{"wave":20}]}]}"#;
        assert_eq!(highest_wave(json), Some(20));
    }

    #[test]
    fn highest_wave_returns_none_for_a_document_with_no_blocks() {
        assert_eq!(highest_wave(r#"{"tracks":[]}"#), None);
    }

    #[test]
    fn highest_wave_returns_none_for_malformed_json() {
        assert_eq!(highest_wave("not json"), None);
    }
}
