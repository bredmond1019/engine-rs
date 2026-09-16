//! `WritePlanNarrativeNode` — no model call (`EN.19.B` task 4).
//!
//! Renders `<brain_root>/planning/open-work/pre-plan/<slug>/plan.md` from
//! [`super::stage_candidate_blocks::StageCandidateBlocksNode`]'s staged
//! candidate-block records, matching
//! `base-template/.claude/commands/plan.md`'s own Output Format section
//! (OKF frontmatter with `type: Plan`, `doc_id: plan-<slug>`, and a
//! non-empty `related:` — never `related: []`, per that command's own
//! step-9 property self-check).
//!
//! # Read the written records back, don't re-derive them
//!
//! This node reads each staged candidate's own JSON file off disk (via the
//! `path` [`super::stage_candidate_blocks::StageCandidateBlocksNode`]
//! already recorded in `ctx.nodes[StageCandidateBlocksNode][staged]`)
//! rather than re-reading [`super::decompose::DecomposePlanNode`]'s raw
//! model output. The written record is post-mint (it carries the real
//! `id`) and post-validation — it is exactly what a human reviewing
//! `candidate-blocks/<ID>.json` will see, so the heading this node renders
//! for that block is guaranteed to match the file a reader opens next to
//! it, with no risk of the two drifting out of index-order sync.
//!
//! # `related:` default
//!
//! Per the block record's `notes`/`what`, this node has no model call and
//! therefore cannot discover a *fresh* real `doc_id` to relate to. It uses
//! this repo's own `master-plan` doc — every scaffolded repo's
//! `planning/master-plan.md` carries `doc_id: master-plan` (confirmed
//! against this repo's own copy while writing this node) — as the safe,
//! always-real default target, mirroring the pattern
//! `EN.19.A`'s orchestration-run `notes.md` output used for its own
//! `related:` default.
//!
//! # Frontmatter is emitted via `okf-core`, not hand-formatted
//!
//! `okf_core::{OkfFrontmatter, serialize_frontmatter}` (already a workspace
//! dependency of this crate) is the fleet's one YAML-frontmatter writer —
//! using it here, rather than hand-interpolating a `---`-fenced string,
//! is what keeps this node's output parseable by
//! `okf_core::parse_frontmatter` regardless of what a candidate's title or
//! description happens to contain.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use engine_contract::TaskContext;
use okf_core::{serialize_frontmatter, OkfFrontmatter};
use serde_json::Value;

use crate::brain_root::resolve_brain_root;
use crate::node::{Node, NodeError};
use crate::workflows::{get_result, put_result};

use super::check_existing::{parse_slug, pre_plan_dir, PlanAuthoringFs, RealPlanAuthoringFs};
use super::stage_candidate_blocks::{
    CandidateBlockWriter, RealCandidateBlockWriter, NODE_NAME as STAGE_NODE_NAME,
};

/// The `Node::name()` identity `WritePlanNarrativeNode` is registered
/// under, and the `ctx.nodes` key `{"plan_path": "<path>"}` is stamped
/// onto, per the block record's `what`.
pub const NODE_NAME: &str = "WritePlanNarrativeNode";

/// The real doc_id this node relates every rendered `plan.md` to by
/// default (see this module's doc comment).
const DEFAULT_RELATED_DOC_ID: &str = "master-plan";

/// Today's date as `YYYY-MM-DD`, matching plan.md's Output Format's
/// `created`/`updated` fields.
fn today_string() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// `my-cool-slug` -> `My Cool Slug` — a readable default title when no
/// richer name is available (this node has no model call to draft one).
fn title_case_slug(slug: &str) -> String {
    slug.split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// One staged candidate block, read back off disk from
/// `StageCandidateBlocksNode`'s own written record.
struct BlockRecord {
    id: String,
    title: String,
    description: String,
    phase: i64,
    repo: String,
    sdlc_workflow: String,
    model: String,
    incomplete: bool,
}

impl BlockRecord {
    fn from_value(id: &str, record: &Value) -> Self {
        let str_field = |field: &str, default: &str| {
            record
                .get(field)
                .and_then(Value::as_str)
                .unwrap_or(default)
                .to_string()
        };
        Self {
            id: id.to_string(),
            title: str_field("title", id),
            description: str_field("description", ""),
            phase: record.get("phase").and_then(Value::as_i64).unwrap_or(0),
            repo: str_field("repo", "unknown"),
            sdlc_workflow: str_field("sdlc_workflow", "task"),
            model: str_field("model", "sonnet"),
            incomplete: record
                .get("_incomplete")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }
    }
}

/// Render the full `plan.md` document (frontmatter + narrative body) from
/// this run's staged block records.
fn render_plan_md(slug: &str, blocks: &[BlockRecord], today: &str) -> String {
    let project = blocks
        .first()
        .map(|b| b.repo.clone())
        .unwrap_or_else(|| "unknown".to_string());
    let title = title_case_slug(slug);
    let description = format!(
        "PLAN_AUTHORING decomposition of `{slug}` into {n} candidate block(s) staged for human review.",
        n = blocks.len()
    );

    let frontmatter = OkfFrontmatter {
        type_: Some("Plan".to_string()),
        title: Some(title.clone()),
        description: Some(description.clone()),
        doc_id: Some(format!("plan-{slug}")),
        layer: Vec::new(),
        project: Some(project),
        status: Some("active".to_string()),
        keywords: vec![slug.to_string(), "plan-authoring".to_string()],
        related: vec![DEFAULT_RELATED_DOC_ID.to_string()],
        created: Some(today.to_string()),
        updated: Some(today.to_string()),
        synced_from: None,
    };

    let mut out = serialize_frontmatter(&frontmatter);

    out.push_str(&format!("\n# {title}\n\n"));
    out.push_str(&format!(
        "*Created {today}. Block definitions live in `planning/blocks/`; this document holds only \
         what is true of the set. Candidate blocks staged by this run live in `candidate-blocks/` \
         next to this file and are not yet registered — see `.claude/workflows/block-registration.md` \
         before promoting one.*\n\n"
    ));

    out.push_str("## The Goal, Stated Plainly\n\n");
    if blocks.is_empty() {
        out.push_str(&format!(
            "PLAN_AUTHORING ran against `{slug}` but produced no candidate blocks. Nothing was \
             staged this run.\n\n"
        ));
    } else {
        out.push_str(&format!(
            "This initiative decomposes `{slug}` into {n} candidate block(s), staged under \
             `candidate-blocks/` for a human (or a later block) to review and register.\n\n",
            n = blocks.len()
        ));
    }

    out.push_str("## The Destination\n\n");
    out.push_str(
        "Every candidate block below is registered by hand (`mev create-block`) once reviewed — \
         this workflow never registers a block itself, see `EN.19.B.json`'s `out_of_scope`.\n\n",
    );

    out.push_str("## Architecture / Design Overview\n\n");
    out.push_str(
        "See this repo's own `CLAUDE.md` and `planning/context.md` for the stack and standing \
         rules the candidates below were decomposed against.\n\n",
    );

    out.push_str("## Sequencing Rationale\n\n");
    out.push_str(
        "Candidates are ordered by the phase `DecomposePlanNode` proposed, following \
         `.claude/commands/plan.md`'s dependency-and-competence sequencing rule.\n\n",
    );

    out.push_str("---\n\n");

    let mut phases: Vec<i64> = blocks.iter().map(|b| b.phase).collect();
    phases.sort_unstable();
    phases.dedup();

    for phase in &phases {
        out.push_str(&format!("## Phase {phase}\n\n"));
        for block in blocks.iter().filter(|b| b.phase == *phase) {
            out.push_str(&format!("### {} — {}\n", block.id, block.title));
            if block.incomplete {
                out.push_str(&format!(
                    "_Staged as `_incomplete: true` — see `planning/blocks/{}.json` for the \
                     fields the decomposition could not fill._\n\n",
                    block.id
                ));
            } else {
                out.push_str(&format!(
                    "_See `planning/blocks/{}.json` for this block's what/why/files/out-of-scope/\
                     acceptance criteria._\n\n",
                    block.id
                ));
            }
        }
        out.push_str("---\n\n");
    }

    out.push_str("## What Is Cut, and Why\n\n");
    out.push_str("| Candidate | Why it is out |\n|---|---|\n");
    let incomplete: Vec<&BlockRecord> = blocks.iter().filter(|b| b.incomplete).collect();
    if incomplete.is_empty() {
        out.push_str("| _(none)_ | No candidate was rejected this run. |\n");
    } else {
        for block in incomplete {
            out.push_str(&format!(
                "| {} | Staged `_incomplete: true` — needs human completion before registration. |\n",
                block.id
            ));
        }
    }
    out.push_str("\n---\n\n");

    out.push_str("## Sequence\n\n");
    out.push_str("| Phase | Block | What | SDLC workflow | Model | Role in the destination |\n");
    out.push_str("|---|---|---|---|---|---|\n");
    for block in blocks {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            block.phase, block.id, block.title, block.sdlc_workflow, block.model, block.description
        ));
    }

    out.push_str("\n---\n\n*Sequenced by dependency and competence, not calendar.*\n");

    out
}

/// Renders `plan.md` from [`super::stage_candidate_blocks::StageCandidateBlocksNode`]'s
/// staged output.
pub struct WritePlanNarrativeNode {
    fs: Arc<dyn PlanAuthoringFs>,
    writer: Arc<dyn CandidateBlockWriter>,
    brain_root_resolver: Arc<dyn Fn() -> Result<PathBuf, String> + Send + Sync>,
    clock: Arc<dyn Fn() -> String + Send + Sync>,
}

impl Default for WritePlanNarrativeNode {
    fn default() -> Self {
        Self::new()
    }
}

impl WritePlanNarrativeNode {
    /// The production node: real filesystem/writer, real `resolve_brain_root()`,
    /// real system clock.
    #[must_use]
    pub fn new() -> Self {
        Self {
            fs: Arc::new(RealPlanAuthoringFs),
            writer: Arc::new(RealCandidateBlockWriter),
            brain_root_resolver: Arc::new(|| resolve_brain_root().map_err(|err| err.to_string())),
            clock: Arc::new(today_string),
        }
    }

    /// Override the read filesystem seam. Tests use this so nothing here
    /// ever touches the real disk.
    #[must_use]
    pub fn with_fs(mut self, fs: Arc<dyn PlanAuthoringFs>) -> Self {
        self.fs = fs;
        self
    }

    /// Override the write seam.
    #[must_use]
    pub fn with_writer(mut self, writer: Arc<dyn CandidateBlockWriter>) -> Self {
        self.writer = writer;
        self
    }

    /// Override brain-root resolution entirely (e.g. a fixed tempdir path
    /// in tests), bypassing `ENGINE_BRAIN_ROOT`/`brain.toml` walk-up.
    #[must_use]
    pub fn with_brain_root(mut self, root: PathBuf) -> Self {
        self.brain_root_resolver = Arc::new(move || Ok(root.clone()));
        self
    }

    /// Override the clock (the source of `created`/`updated`). Tests use
    /// this for a deterministic date.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Fn() -> String + Send + Sync>) -> Self {
        self.clock = clock;
        self
    }

    /// Read every staged candidate's own written JSON record back off disk,
    /// in the order [`super::stage_candidate_blocks::StageCandidateBlocksNode`]
    /// staged them.
    fn load_staged_blocks(&self, ctx: &TaskContext) -> Result<Vec<BlockRecord>, NodeError> {
        let staged = get_result(ctx, STAGE_NODE_NAME)
            .and_then(|value| value.get("staged"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let mut blocks = Vec::with_capacity(staged.len());
        for entry in &staged {
            let id = entry
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| NodeError::new(format!("{NODE_NAME}: staged entry missing `id`")))?;
            let path_str = entry.get("path").and_then(Value::as_str).ok_or_else(|| {
                NodeError::new(format!(
                    "{NODE_NAME}: staged entry `{id}` is missing `path`"
                ))
            })?;
            let path = Path::new(path_str);
            let contents = self.fs.read_to_string(path).ok_or_else(|| {
                NodeError::new(format!(
                    "{NODE_NAME}: could not read staged candidate record at {}",
                    path.display()
                ))
            })?;
            let record: Value = serde_json::from_str(&contents).map_err(|err| {
                NodeError::new(format!(
                    "{NODE_NAME}: staged candidate record at {} is not valid JSON: {err}",
                    path.display()
                ))
            })?;
            blocks.push(BlockRecord::from_value(id, &record));
        }
        Ok(blocks)
    }
}

#[async_trait::async_trait]
impl Node for WritePlanNarrativeNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let slug = parse_slug(&ctx)?;
        let brain_root = (self.brain_root_resolver)().map_err(NodeError::new)?;
        let blocks = self.load_staged_blocks(&ctx)?;
        let today = (self.clock)();

        let plan_dir = pre_plan_dir(&brain_root, &slug);
        let plan_path = plan_dir.join("plan.md");

        let rendered = render_plan_md(&slug, &blocks, &today);

        self.writer
            .create_dir_all(&plan_dir)
            .map_err(|err| NodeError::new(format!("{NODE_NAME}: {err}")))?;
        self.writer
            .write(&plan_path, &rendered)
            .map_err(|err| NodeError::new(format!("{NODE_NAME}: {err}")))?;

        put_result(
            &mut ctx,
            NODE_NAME,
            serde_json::json!({ "plan_path": plan_path.to_string_lossy() }),
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
    use std::collections::HashSet;
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::workflows::put_result as put_result_for_test;

    #[derive(Default)]
    struct StubFs {
        files: HashMap<PathBuf, String>,
    }

    impl StubFs {
        fn with_files(files: &[(PathBuf, String)]) -> Self {
            Self {
                files: files.iter().cloned().collect(),
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

    #[derive(Default)]
    struct RecordingWriter {
        created_dirs: Mutex<HashSet<PathBuf>>,
        written: Mutex<HashMap<PathBuf, String>>,
    }

    impl CandidateBlockWriter for RecordingWriter {
        fn create_dir_all(&self, path: &Path) -> Result<(), String> {
            self.created_dirs.lock().unwrap().insert(path.to_path_buf());
            Ok(())
        }

        fn write(&self, path: &Path, contents: &str) -> Result<(), String> {
            self.written
                .lock()
                .unwrap()
                .insert(path.to_path_buf(), contents.to_string());
            Ok(())
        }
    }

    fn ctx_with_event(event: Value) -> TaskContext {
        TaskContext {
            event,
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        }
    }

    fn candidate_record(id: &str, title: &str, phase: i64) -> Value {
        json!({
            "id": id,
            "title": title,
            "description": format!("{title} description"),
            "what": "w",
            "why": "why",
            "files": ["crates/x.rs"],
            "out_of_scope": ["nothing"],
            "acceptance_criteria": ["passes"],
            "repo": "engine-rs",
            "kind": "block",
            "phase": phase,
            "sdlc_workflow": "task",
            "model": "sonnet",
        })
    }

    fn node_with(
        brain_root: PathBuf,
        fs: StubFs,
        writer: Arc<RecordingWriter>,
    ) -> WritePlanNarrativeNode {
        WritePlanNarrativeNode::new()
            .with_brain_root(brain_root)
            .with_fs(Arc::new(fs))
            .with_writer(writer)
            .with_clock(Arc::new(|| "2026-09-15".to_string()))
    }

    #[tokio::test]
    async fn renders_a_heading_per_staged_candidate_matching_its_id_and_title() {
        let brain_root = PathBuf::from("/tmp/en-19-b-narrative-two-blocks");
        let dir = pre_plan_dir(&brain_root, "my-slug").join("candidate-blocks");
        let path_a = dir.join("EN.19.A.json");
        let path_b = dir.join("EN.19.B.json");

        let fs = StubFs::with_files(&[
            (
                path_a.clone(),
                candidate_record("EN.19.A", "First block", 19).to_string(),
            ),
            (
                path_b.clone(),
                candidate_record("EN.19.B", "Second block", 19).to_string(),
            ),
        ]);
        let writer = Arc::new(RecordingWriter::default());
        let node = node_with(brain_root.clone(), fs, writer.clone());

        let mut ctx = ctx_with_event(json!({ "slug": "my-slug" }));
        put_result_for_test(
            &mut ctx,
            STAGE_NODE_NAME,
            json!({
                "slug": "my-slug",
                "staged": [
                    { "id": "EN.19.A", "path": path_a.to_string_lossy() },
                    { "id": "EN.19.B", "path": path_b.to_string_lossy() },
                ],
            }),
        );

        let out = node.process(ctx).await.expect("process should succeed");
        let stored = get_result(&out, NODE_NAME).expect("result stored");
        let expected_plan_path = pre_plan_dir(&brain_root, "my-slug").join("plan.md");
        assert_eq!(
            stored.get("plan_path").and_then(Value::as_str),
            Some(expected_plan_path.to_string_lossy().as_ref())
        );

        let written = writer.written.lock().unwrap();
        let rendered = written.get(&expected_plan_path).expect("plan.md written");
        assert!(rendered.contains("### EN.19.A — First block"));
        assert!(rendered.contains("### EN.19.B — Second block"));
    }

    #[tokio::test]
    async fn frontmatter_carries_a_non_empty_related_list_and_parses_as_valid_yaml() {
        let brain_root = PathBuf::from("/tmp/en-19-b-narrative-frontmatter");
        let dir = pre_plan_dir(&brain_root, "my-slug").join("candidate-blocks");
        let path_a = dir.join("EN.19.A.json");

        let fs = StubFs::with_files(&[(
            path_a.clone(),
            candidate_record("EN.19.A", "First block", 19).to_string(),
        )]);
        let writer = Arc::new(RecordingWriter::default());
        let node = node_with(brain_root.clone(), fs, writer.clone());

        let mut ctx = ctx_with_event(json!({ "slug": "my-slug" }));
        put_result_for_test(
            &mut ctx,
            STAGE_NODE_NAME,
            json!({
                "slug": "my-slug",
                "staged": [ { "id": "EN.19.A", "path": path_a.to_string_lossy() } ],
            }),
        );

        node.process(ctx).await.expect("process should succeed");

        let expected_plan_path = pre_plan_dir(&brain_root, "my-slug").join("plan.md");
        let written = writer.written.lock().unwrap();
        let rendered = written.get(&expected_plan_path).expect("plan.md written");

        let parsed = okf_core::parse_frontmatter(rendered)
            .expect("plan.md frontmatter must parse as valid YAML frontmatter");
        let related = parsed
            .fields
            .get("related")
            .map(|(value, _line)| value.clone())
            .unwrap_or_default();
        assert!(
            !related.trim().is_empty() && related.trim() != "[]",
            "related: must not be empty, got `{related}`"
        );
        assert_eq!(
            parsed.fields.get("doc_id").map(|(v, _)| v.clone()),
            Some("plan-my-slug".to_string())
        );
        assert_eq!(
            parsed.fields.get("type").map(|(v, _)| v.clone()),
            Some("Plan".to_string())
        );
    }

    #[tokio::test]
    async fn zero_staged_candidates_still_renders_a_valid_plan_md() {
        let brain_root = PathBuf::from("/tmp/en-19-b-narrative-empty");
        let fs = StubFs::default();
        let writer = Arc::new(RecordingWriter::default());
        let node = node_with(brain_root.clone(), fs, writer.clone());

        let mut ctx = ctx_with_event(json!({ "slug": "empty-slug" }));
        put_result_for_test(
            &mut ctx,
            STAGE_NODE_NAME,
            json!({ "slug": "empty-slug", "staged": [] }),
        );

        node.process(ctx).await.expect("process should succeed");

        let expected_plan_path = pre_plan_dir(&brain_root, "empty-slug").join("plan.md");
        let written = writer.written.lock().unwrap();
        let rendered = written.get(&expected_plan_path).expect("plan.md written");
        let parsed = okf_core::parse_frontmatter(rendered)
            .expect("plan.md frontmatter must parse even with zero staged candidates");
        assert!(parsed.fields.contains_key("related"));
    }

    #[tokio::test]
    async fn an_incomplete_candidate_is_named_in_the_cut_list() {
        let brain_root = PathBuf::from("/tmp/en-19-b-narrative-incomplete");
        let dir = pre_plan_dir(&brain_root, "my-slug").join("candidate-blocks");
        let path_a = dir.join("EN.19.A.json");
        let mut record = candidate_record("EN.19.A", "Partial block", 19);
        record["_incomplete"] = json!(true);
        record["_missing_fields"] = json!(["why"]);

        let fs = StubFs::with_files(&[(path_a.clone(), record.to_string())]);
        let writer = Arc::new(RecordingWriter::default());
        let node = node_with(brain_root.clone(), fs, writer.clone());

        let mut ctx = ctx_with_event(json!({ "slug": "my-slug" }));
        put_result_for_test(
            &mut ctx,
            STAGE_NODE_NAME,
            json!({
                "slug": "my-slug",
                "staged": [ { "id": "EN.19.A", "path": path_a.to_string_lossy() } ],
            }),
        );

        node.process(ctx).await.expect("process should succeed");

        let expected_plan_path = pre_plan_dir(&brain_root, "my-slug").join("plan.md");
        let written = writer.written.lock().unwrap();
        let rendered = written.get(&expected_plan_path).expect("plan.md written");
        assert!(rendered.contains("_incomplete: true"));
        assert!(rendered.contains("| EN.19.A |"));
    }

    #[tokio::test]
    async fn an_unreadable_staged_record_is_a_named_error() {
        let brain_root = PathBuf::from("/tmp/en-19-b-narrative-unreadable");
        let fs = StubFs::default();
        let writer = Arc::new(RecordingWriter::default());
        let node = node_with(brain_root.clone(), fs, writer.clone());

        let dir = pre_plan_dir(&brain_root, "my-slug").join("candidate-blocks");
        let missing_path = dir.join("EN.19.A.json");

        let mut ctx = ctx_with_event(json!({ "slug": "my-slug" }));
        put_result_for_test(
            &mut ctx,
            STAGE_NODE_NAME,
            json!({
                "slug": "my-slug",
                "staged": [ { "id": "EN.19.A", "path": missing_path.to_string_lossy() } ],
            }),
        );

        let err = node
            .process(ctx)
            .await
            .expect_err("a staged record that cannot be read must be a named error");
        assert!(err.message.contains("EN.19.A") || err.message.contains("could not read"));
    }

    #[tokio::test]
    async fn missing_slug_is_a_named_error() {
        let node = WritePlanNarrativeNode::new()
            .with_brain_root(PathBuf::from("/tmp/en-19-b-narrative-no-slug"))
            .with_fs(Arc::new(StubFs::default()))
            .with_writer(Arc::new(RecordingWriter::default()));
        let err = node
            .process(ctx_with_event(json!({})))
            .await
            .expect_err("should reject an event with no slug");
        assert!(err.message.contains("slug"));
    }

    #[test]
    fn title_case_slug_handles_dashes_and_underscores() {
        assert_eq!(title_case_slug("my-cool_slug"), "My Cool Slug");
    }

    #[test]
    fn node_name_is_stable() {
        assert_eq!(WritePlanNarrativeNode::new().name(), NODE_NAME);
    }
}
