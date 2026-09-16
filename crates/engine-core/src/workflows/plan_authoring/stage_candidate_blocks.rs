//! `StageCandidateBlocksNode` — no model call (`EN.19.B` task 3).
//!
//! Reads [`super::decompose::DecomposePlanNode`]'s structured candidates,
//! mints a candidate block ID per candidate (`<PREFIX>.<phase>.<letter>`,
//! the prefix/slug resolved from this repo's own `brain.toml` `[[repos]]`
//! entry, the phase taken from [`super::gather_context::GatherPlanContextNode`]'s
//! `highest_wave`), validates the resulting record against the required-
//! field/enum contract of `.claude/workflows/block.schema.json`, and writes
//! each to `<brain_root>/planning/open-work/pre-plan/<slug>/candidate-blocks/<ID>.json`.
//!
//! # This node's one job is to stage, never to register
//!
//! Per the block record's own `out_of_scope`, nothing in this file may
//! shell out to, import, or otherwise reference the block-registration
//! command line tool, nor touch any repo's live block ledger or graph-of-
//! record document — enforced by a source grep over this exact file (task
//! 3's own acceptance criteria), not merely a doc comment's promise. A
//! staged candidate only ever becomes a real, scheduled block through a
//! separate, human-reviewed action this workflow deliberately does not
//! take.
//!
//! # No JSON Schema validator dependency
//!
//! This workspace carries no JSON Schema validation crate (confirmed by
//! grepping `Cargo.lock` before writing this file), so this node does not
//! add one for a single call site. Instead [`validate_candidate_record`]
//! hand-checks exactly the required-field, non-empty, and enum constraints
//! `.claude/workflows/block.schema.json` declares for a `kind: "block"`
//! record — the same "mirror the schema's shape in a plain Rust check"
//! precedent this fleet's own block-record reader already uses for its own
//! required-field diagnostics. The staging-only `_incomplete`/
//! `_missing_fields` markers [`super::decompose::DecomposePlanNode`] may
//! attach are a deliberate, narrow divergence from that schema's
//! `additionalProperties: false`: they exist so a human reviewing a staged
//! candidate can see exactly what the decomposition could not fill, and a
//! field named in `_missing_fields` is exempted from this node's own
//! required-field check rather than blocking the whole candidate from ever
//! being staged.
//!
//! # Repo identity, not a hardcoded prefix
//!
//! The `<PREFIX>` and `repo` slug a candidate's `id`/`repo` fields need are
//! read from `brain.toml`'s `[[repos]]` table (matched by this run's target
//! repo path relative to the brain root) rather than hardcoded to this
//! repo's own `EN`/`engine-rs` — this workflow is dispatchable against any
//! pre-plan folder in the fleet, per the block record's own `what`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use engine_contract::TaskContext;
use serde_json::{json, Value};

use crate::brain_root::resolve_brain_root;
use crate::node::{Node, NodeError};
use crate::workflows::{get_result, put_result};

use super::check_existing::{parse_slug, pre_plan_dir, PlanAuthoringFs, RealPlanAuthoringFs};
use super::decompose;
use super::gather_context;

/// The `Node::name()` identity `StageCandidateBlocksNode` is registered
/// under, and the `ctx.nodes` key this node's staging report is stamped
/// onto.
pub const NODE_NAME: &str = "StageCandidateBlocksNode";

/// The candidate-block fields this node requires present and non-empty
/// before writing, mirroring [`super::decompose::REQUIRED_CANDIDATE_FIELDS`]'s
/// intent (that const is private to `decompose.rs`; this list is kept
/// deliberately identical in content, not re-exported, since it is a fixed
/// enumeration rather than shared logic). A field named in a candidate's
/// own `_missing_fields` array is exempt from this check — see this
/// module's doc comment.
const REQUIRED_STRING_FIELDS: [&str; 4] = ["title", "description", "what", "why"];
/// `files` is included here, not checked as `block.schema.json`'s
/// `{new: [...], modified: [...]}` object shape — [`super::decompose::DecomposePlanNode`]'s
/// own response schema (already shipped in task 2) emits `files` as a flat
/// array of path strings, which is what this node actually receives. A
/// staged candidate is reviewed and reshaped by a human before real
/// registration (this block's own `out_of_scope`), so this validator
/// checks the shape decompose truly produces rather than one it cannot.
const REQUIRED_ARRAY_FIELDS: [&str; 3] = ["files", "out_of_scope", "acceptance_criteria"];

/// Fields this node always synthesizes itself before validation runs — a
/// defensive check only; these should never actually be found missing.
const SYNTHESIZED_FIELDS: [&str; 9] = [
    "id",
    "repo",
    "kind",
    "phase",
    "sdlc_workflow",
    "model",
    "spec_dir",
    "created",
    "updated",
];

const VALID_KINDS: [&str; 3] = ["block", "ticket", "chore"];
const VALID_SDLC_WORKFLOWS: [&str; 5] = ["none", "patch", "task", "run", "flow"];
const VALID_MODELS: [&str; 4] = ["sonnet", "gemini-pro", "gemini-flash", "either"];

/// A repo's identity as registered in `brain.toml`'s `[[repos]]` table —
/// the `slug` a candidate's `repo` field needs, and the `prefix` its `id`
/// needs.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RepoIdentity {
    slug: String,
    prefix: String,
}

/// Injectable write seam so this node never touches the real disk in
/// tests — the write-side counterpart of [`PlanAuthoringFs`] (which is
/// read/exists-only).
pub trait CandidateBlockWriter: Send + Sync {
    /// Create `path` and all missing parent directories.
    fn create_dir_all(&self, path: &Path) -> Result<(), String>;
    /// Write `contents` to `path`, creating or truncating it.
    fn write(&self, path: &Path, contents: &str) -> Result<(), String>;
}

/// The live [`CandidateBlockWriter`] backed by real `std::fs` calls.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealCandidateBlockWriter;

impl CandidateBlockWriter for RealCandidateBlockWriter {
    fn create_dir_all(&self, path: &Path) -> Result<(), String> {
        std::fs::create_dir_all(path).map_err(|err| err.to_string())
    }

    fn write(&self, path: &Path, contents: &str) -> Result<(), String> {
        std::fs::write(path, contents).map_err(|err| err.to_string())
    }
}

/// Today's date as `YYYY-MM-DD`, matching `.claude/workflows/block.schema.json`'s
/// `created`/`updated` pattern.
fn today_string() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// Spreadsheet-style column letters (`A`, `B`, ..., `Z`, `AA`, `AB`, ...)
/// for the `<letter>` segment of a minted candidate id, so a decomposition
/// proposing more than 26 blocks in one run still mints distinct,
/// schema-pattern-valid ids rather than colliding or erroring.
fn candidate_letter(index: usize) -> String {
    let mut n = index;
    let mut letters = Vec::new();
    loop {
        let rem = (n % 26) as u8;
        letters.push((b'A' + rem) as char);
        if n < 26 {
            break;
        }
        n = n / 26 - 1;
    }
    letters.iter().rev().collect()
}

/// Read `field`'s presence in `candidate`'s own `_missing_fields` array
/// (stamped by [`super::decompose::DecomposePlanNode`]'s `mark_incomplete`).
fn is_exempt(missing_fields: &[String], field: &str) -> bool {
    missing_fields.iter().any(|f| f == field)
}

/// Hand-checks the required-field, non-empty, and enum constraints
/// `.claude/workflows/block.schema.json` declares for a `kind: "block"`
/// record — see this module's doc comment for why this is not a real JSON
/// Schema validator call. Returns every violation found, not just the
/// first, so a rejected candidate's `NodeError` names everything wrong at
/// once.
fn validate_candidate_record(record: &Value) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();

    let missing_fields: Vec<String> = record
        .get("_missing_fields")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    for field in REQUIRED_STRING_FIELDS {
        if is_exempt(&missing_fields, field) {
            continue;
        }
        let value = record.get(field).and_then(Value::as_str).unwrap_or("");
        if value.trim().is_empty() {
            errors.push(format!("missing or empty required field `{field}`"));
        }
    }

    for field in REQUIRED_ARRAY_FIELDS {
        if is_exempt(&missing_fields, field) {
            continue;
        }
        let non_empty = record
            .get(field)
            .and_then(Value::as_array)
            .map(|arr| !arr.is_empty())
            .unwrap_or(false);
        if !non_empty {
            errors.push(format!(
                "required field `{field}` must be a non-empty array"
            ));
        }
    }

    for field in SYNTHESIZED_FIELDS {
        if record.get(field).map(Value::is_null).unwrap_or(true) {
            errors.push(format!(
                "missing required field `{field}` (should have been synthesized by this node)"
            ));
        }
    }

    if let Some(kind) = record.get("kind").and_then(Value::as_str) {
        if !VALID_KINDS.contains(&kind) {
            errors.push(format!("`kind` has invalid value `{kind}`"));
        }
    }
    if let Some(workflow) = record.get("sdlc_workflow").and_then(Value::as_str) {
        if !VALID_SDLC_WORKFLOWS.contains(&workflow) {
            errors.push(format!("`sdlc_workflow` has invalid value `{workflow}`"));
        }
    }
    if let Some(model) = record.get("model").and_then(Value::as_str) {
        if !VALID_MODELS.contains(&model) {
            errors.push(format!("`model` has invalid value `{model}`"));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Merge the synthesized identity/scheduling fields onto `candidate`,
/// producing the full candidate block record this node validates and
/// writes. `candidate`'s own `id_placeholder` (if any) is dropped — the
/// minted `id` is authoritative.
fn build_record(candidate: Value, id: &str, repo_slug: &str, phase: i64, today: &str) -> Value {
    let mut record = candidate;
    if let Some(obj) = record.as_object_mut() {
        obj.insert("id".to_string(), json!(id));
        obj.insert("repo".to_string(), json!(repo_slug));
        obj.insert("kind".to_string(), json!("block"));
        obj.insert("phase".to_string(), json!(phase));
        obj.entry("sdlc_workflow").or_insert_with(|| json!("task"));
        obj.entry("model").or_insert_with(|| json!("sonnet"));
        obj.insert("spec_dir".to_string(), json!(format!("planning/{id}/")));
        obj.insert("created".to_string(), json!(today));
        obj.insert("updated".to_string(), json!(today));
        obj.remove("id_placeholder");
    }
    record
}

/// Find the `[[repos]]` entry in `brain_toml_contents` whose `repo_path`
/// equals `repo_rel_path`, returning its `slug`/`prefix`.
fn find_repo_identity(
    brain_toml_contents: &str,
    repo_rel_path: &str,
) -> Result<RepoIdentity, String> {
    let parsed: toml::Value = toml::from_str(brain_toml_contents)
        .map_err(|err| format!("failed to parse brain.toml: {err}"))?;
    let repos = parsed
        .get("repos")
        .and_then(toml::Value::as_array)
        .cloned()
        .unwrap_or_default();

    for repo in &repos {
        let repo_path = repo
            .get("repo_path")
            .and_then(toml::Value::as_str)
            .unwrap_or("");
        if repo_path != repo_rel_path {
            continue;
        }
        let slug = repo
            .get("slug")
            .and_then(toml::Value::as_str)
            .unwrap_or_default()
            .to_string();
        let prefix = repo
            .get("prefix")
            .and_then(toml::Value::as_str)
            .unwrap_or_default()
            .to_string();
        if slug.is_empty() || prefix.is_empty() {
            return Err(format!(
                "brain.toml [[repos]] entry for repo_path `{repo_rel_path}` is missing slug/prefix"
            ));
        }
        return Ok(RepoIdentity { slug, prefix });
    }

    Err(format!(
        "no brain.toml [[repos]] entry found for repo_path `{repo_rel_path}`"
    ))
}

/// Mints a candidate block ID per [`super::decompose::DecomposePlanNode`]
/// candidate, validates the resulting record, and writes it under this
/// run's `candidate-blocks/` folder.
pub struct StageCandidateBlocksNode {
    fs: Arc<dyn PlanAuthoringFs>,
    writer: Arc<dyn CandidateBlockWriter>,
    repo_root_resolver: Arc<dyn Fn() -> Result<PathBuf, String> + Send + Sync>,
    brain_root_resolver: Arc<dyn Fn() -> Result<PathBuf, String> + Send + Sync>,
    clock: Arc<dyn Fn() -> String + Send + Sync>,
}

impl Default for StageCandidateBlocksNode {
    fn default() -> Self {
        Self::new()
    }
}

impl StageCandidateBlocksNode {
    /// The production node: real filesystem/writer, `std::env::current_dir()`
    /// as the target repo root (matching [`gather_context::GatherPlanContextNode`]'s
    /// own default), real `resolve_brain_root()`, and the real system clock.
    #[must_use]
    pub fn new() -> Self {
        Self {
            fs: Arc::new(RealPlanAuthoringFs),
            writer: Arc::new(RealCandidateBlockWriter),
            repo_root_resolver: Arc::new(|| std::env::current_dir().map_err(|err| err.to_string())),
            brain_root_resolver: Arc::new(|| resolve_brain_root().map_err(|err| err.to_string())),
            clock: Arc::new(today_string),
        }
    }

    /// Override the read/exists filesystem seam. Tests use this so nothing
    /// here ever touches the real disk.
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

    /// Override repo-root resolution (the source of `brain.toml`'s
    /// `repo_path` match). Defaults to `std::env::current_dir()`.
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

    /// Override the clock (the source of `created`/`updated`). Tests use
    /// this for a deterministic date.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Fn() -> String + Send + Sync>) -> Self {
        self.clock = clock;
        self
    }

    /// Resolve this run's target repo's `slug`/`prefix` from `brain_root`'s
    /// `brain.toml`, matched against `repo_root`'s path relative to it.
    fn resolve_repo_identity(
        &self,
        brain_root: &Path,
        repo_root: &Path,
    ) -> Result<RepoIdentity, NodeError> {
        let brain_toml_path = brain_root.join("brain.toml");
        let contents = self.fs.read_to_string(&brain_toml_path).ok_or_else(|| {
            NodeError::new(format!(
                "{NODE_NAME}: could not read brain.toml at {}",
                brain_toml_path.display()
            ))
        })?;

        let repo_rel_path = if repo_root == brain_root {
            ".".to_string()
        } else {
            repo_root
                .strip_prefix(brain_root)
                .map_err(|_| {
                    NodeError::new(format!(
                        "{NODE_NAME}: repo_root {} is not under brain_root {}",
                        repo_root.display(),
                        brain_root.display()
                    ))
                })?
                .to_string_lossy()
                .to_string()
        };

        find_repo_identity(&contents, &repo_rel_path)
            .map_err(|err| NodeError::new(format!("{NODE_NAME}: {err}")))
    }
}

#[async_trait::async_trait]
impl Node for StageCandidateBlocksNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let slug = parse_slug(&ctx)?;

        let candidates: Vec<Value> = get_result(&ctx, decompose::NODE_NAME)
            .and_then(|value| value.get("candidates"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        if candidates.is_empty() {
            put_result(
                &mut ctx,
                NODE_NAME,
                json!({
                    "slug": slug,
                    "staged": [],
                }),
            );
            return Ok(ctx);
        }

        let repo_root = (self.repo_root_resolver)().map_err(NodeError::new)?;
        let brain_root = (self.brain_root_resolver)().map_err(NodeError::new)?;
        let identity = self.resolve_repo_identity(&brain_root, &repo_root)?;

        let phase = get_result(&ctx, gather_context::NODE_NAME)
            .and_then(|value| value.get("highest_wave"))
            .and_then(Value::as_i64)
            .unwrap_or(1);

        let candidate_blocks_dir = pre_plan_dir(&brain_root, &slug).join("candidate-blocks");
        self.writer
            .create_dir_all(&candidate_blocks_dir)
            .map_err(|err| NodeError::new(format!("{NODE_NAME}: {err}")))?;

        let today = (self.clock)();
        let mut staged = Vec::new();

        for (index, candidate) in candidates.into_iter().enumerate() {
            let id = format!("{}.{}.{}", identity.prefix, phase, candidate_letter(index));
            let record = build_record(candidate, &id, &identity.slug, phase, &today);

            validate_candidate_record(&record).map_err(|errors| {
                NodeError::new(format!(
                    "{NODE_NAME}: candidate {id} failed block-record validation: {}",
                    errors.join("; ")
                ))
            })?;

            let path = candidate_blocks_dir.join(format!("{id}.json"));
            let contents = serde_json::to_string_pretty(&record)
                .map_err(|err| NodeError::new(format!("{NODE_NAME}: {err}")))?;
            self.writer
                .write(&path, &contents)
                .map_err(|err| NodeError::new(format!("{NODE_NAME}: {err}")))?;

            staged.push(json!({ "id": id, "path": path.to_string_lossy() }));
        }

        put_result(
            &mut ctx,
            NODE_NAME,
            json!({
                "slug": slug,
                "staged": staged,
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

    const BRAIN_TOML: &str = r#"
[[repos]]
slug = "engine-rs"
prefix = "EN"
tier = "core"
repo_path = "core/engine-rs"
"#;

    fn ctx_with_event(event: Value) -> TaskContext {
        TaskContext {
            event,
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        }
    }

    fn full_candidate() -> Value {
        json!({
            "title": "Full block",
            "description": "d",
            "what": "w",
            "why": "why",
            "files": ["crates/x.rs"],
            "out_of_scope": ["nothing"],
            "acceptance_criteria": ["passes"],
        })
    }

    fn node_with(
        brain_root: PathBuf,
        repo_root: PathBuf,
        fs: StubFs,
        writer: Arc<RecordingWriter>,
    ) -> StageCandidateBlocksNode {
        StageCandidateBlocksNode::new()
            .with_brain_root(brain_root)
            .with_repo_root(repo_root)
            .with_fs(Arc::new(fs))
            .with_writer(writer)
            .with_clock(Arc::new(|| "2026-09-15".to_string()))
    }

    #[tokio::test]
    async fn stages_a_fully_populated_candidate_and_writes_it_under_candidate_blocks() {
        let brain_root = PathBuf::from("/tmp/en-19-b-stage-brain");
        let repo_root = brain_root.join("core/engine-rs");
        let fs = StubFs::with_files(&[(brain_root.join("brain.toml"), BRAIN_TOML)]);
        let writer = Arc::new(RecordingWriter::default());
        let node = node_with(brain_root.clone(), repo_root, fs, writer.clone());

        let mut ctx = ctx_with_event(json!({ "slug": "my-slug" }));
        put_result_for_test(
            &mut ctx,
            decompose::NODE_NAME,
            json!({ "candidates": [full_candidate()] }),
        );
        put_result_for_test(
            &mut ctx,
            gather_context::NODE_NAME,
            json!({ "highest_wave": 19 }),
        );

        let out = node.process(ctx).await.expect("process should succeed");
        let stored = get_result(&out, NODE_NAME).expect("result stored");
        let staged = stored.get("staged").and_then(Value::as_array).unwrap();
        assert_eq!(staged.len(), 1);
        assert_eq!(staged[0].get("id"), Some(&json!("EN.19.A")));

        let expected_path = pre_plan_dir(&brain_root, "my-slug")
            .join("candidate-blocks")
            .join("EN.19.A.json");
        let written = writer.written.lock().unwrap();
        let contents = written.get(&expected_path).expect("candidate written");
        let record: Value = serde_json::from_str(contents).unwrap();
        assert_eq!(record.get("id"), Some(&json!("EN.19.A")));
        assert_eq!(record.get("repo"), Some(&json!("engine-rs")));
        assert_eq!(record.get("kind"), Some(&json!("block")));
        assert_eq!(record.get("phase"), Some(&json!(19)));
        assert_eq!(record.get("sdlc_workflow"), Some(&json!("task")));
        assert_eq!(record.get("model"), Some(&json!("sonnet")));
        assert_eq!(record.get("spec_dir"), Some(&json!("planning/EN.19.A/")));
        assert_eq!(record.get("created"), Some(&json!("2026-09-15")));
    }

    #[tokio::test]
    async fn defaults_phase_to_one_when_gather_context_never_ran() {
        let brain_root = PathBuf::from("/tmp/en-19-b-stage-no-phase");
        let repo_root = brain_root.join("core/engine-rs");
        let fs = StubFs::with_files(&[(brain_root.join("brain.toml"), BRAIN_TOML)]);
        let writer = Arc::new(RecordingWriter::default());
        let node = node_with(brain_root, repo_root, fs, writer.clone());

        let mut ctx = ctx_with_event(json!({ "slug": "my-slug" }));
        put_result_for_test(
            &mut ctx,
            decompose::NODE_NAME,
            json!({ "candidates": [full_candidate()] }),
        );

        let out = node.process(ctx).await.expect("process should succeed");
        let stored = get_result(&out, NODE_NAME).expect("result stored");
        let staged = stored.get("staged").and_then(Value::as_array).unwrap();
        assert_eq!(staged[0].get("id"), Some(&json!("EN.1.A")));
    }

    #[tokio::test]
    async fn no_candidates_stages_nothing_without_erroring() {
        let brain_root = PathBuf::from("/tmp/en-19-b-stage-empty");
        let repo_root = brain_root.join("core/engine-rs");
        let fs = StubFs::default();
        let writer = Arc::new(RecordingWriter::default());
        let node = node_with(brain_root, repo_root, fs, writer.clone());

        let mut ctx = ctx_with_event(json!({ "slug": "my-slug" }));
        put_result_for_test(&mut ctx, decompose::NODE_NAME, json!({ "candidates": [] }));

        let out = node.process(ctx).await.expect("process should succeed");
        let stored = get_result(&out, NODE_NAME).expect("result stored");
        assert_eq!(stored.get("staged"), Some(&json!([])));
        assert!(writer.written.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_incomplete_candidate_missing_why_still_stages_with_the_marker_present() {
        let brain_root = PathBuf::from("/tmp/en-19-b-stage-incomplete");
        let repo_root = brain_root.join("core/engine-rs");
        let fs = StubFs::with_files(&[(brain_root.join("brain.toml"), BRAIN_TOML)]);
        let writer = Arc::new(RecordingWriter::default());
        let node = node_with(brain_root.clone(), repo_root, fs, writer.clone());

        let mut candidate = full_candidate();
        candidate.as_object_mut().unwrap().remove("why");
        candidate["_incomplete"] = json!(true);
        candidate["_missing_fields"] = json!(["why"]);

        let mut ctx = ctx_with_event(json!({ "slug": "my-slug" }));
        put_result_for_test(
            &mut ctx,
            decompose::NODE_NAME,
            json!({ "candidates": [candidate] }),
        );
        put_result_for_test(
            &mut ctx,
            gather_context::NODE_NAME,
            json!({ "highest_wave": 19 }),
        );

        let out = node
            .process(ctx)
            .await
            .expect("an _incomplete candidate must still stage, not error");
        let stored = get_result(&out, NODE_NAME).expect("result stored");
        let staged = stored.get("staged").and_then(Value::as_array).unwrap();
        assert_eq!(staged.len(), 1);

        let expected_path = pre_plan_dir(&brain_root, "my-slug")
            .join("candidate-blocks")
            .join("EN.19.A.json");
        let written = writer.written.lock().unwrap();
        let contents = written.get(&expected_path).expect("candidate written");
        let record: Value = serde_json::from_str(contents).unwrap();
        assert_eq!(record.get("_incomplete"), Some(&json!(true)));
        assert!(record.get("why").is_none());
    }

    #[tokio::test]
    async fn a_candidate_missing_a_required_field_without_the_incomplete_marker_is_rejected() {
        let brain_root = PathBuf::from("/tmp/en-19-b-stage-rejected");
        let repo_root = brain_root.join("core/engine-rs");
        let fs = StubFs::with_files(&[(brain_root.join("brain.toml"), BRAIN_TOML)]);
        let writer = Arc::new(RecordingWriter::default());
        let node = node_with(brain_root, repo_root, fs, writer.clone());

        let mut candidate = full_candidate();
        candidate.as_object_mut().unwrap().remove("why");

        let mut ctx = ctx_with_event(json!({ "slug": "my-slug" }));
        put_result_for_test(
            &mut ctx,
            decompose::NODE_NAME,
            json!({ "candidates": [candidate] }),
        );

        let err = node.process(ctx).await.expect_err(
            "a candidate missing `why` with no _missing_fields marker must be rejected",
        );
        assert!(err.message.contains("why"));
        assert!(writer.written.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn unresolvable_repo_prefix_is_a_named_error() {
        let brain_root = PathBuf::from("/tmp/en-19-b-stage-no-prefix");
        let repo_root = PathBuf::from("/tmp/en-19-b-stage-no-prefix/core/unknown-repo");
        let fs = StubFs::with_files(&[(brain_root.join("brain.toml"), BRAIN_TOML)]);
        let writer = Arc::new(RecordingWriter::default());
        let node = node_with(brain_root, repo_root, fs, writer.clone());

        let mut ctx = ctx_with_event(json!({ "slug": "my-slug" }));
        put_result_for_test(
            &mut ctx,
            decompose::NODE_NAME,
            json!({ "candidates": [full_candidate()] }),
        );

        let err = node
            .process(ctx)
            .await
            .expect_err("an unregistered repo_path must be a named error, not a silent default");
        assert!(err.message.contains("no brain.toml"));
    }

    #[tokio::test]
    async fn missing_slug_is_a_named_error() {
        let node = StageCandidateBlocksNode::new()
            .with_brain_root(PathBuf::from("/tmp/en-19-b-stage-no-slug"))
            .with_repo_root(PathBuf::from("/tmp/en-19-b-stage-no-slug"))
            .with_fs(Arc::new(StubFs::default()))
            .with_writer(Arc::new(RecordingWriter::default()));
        let err = node
            .process(ctx_with_event(json!({})))
            .await
            .expect_err("should reject an event with no slug");
        assert!(err.message.contains("slug"));
    }

    #[test]
    fn candidate_letter_covers_the_double_letter_rollover() {
        assert_eq!(candidate_letter(0), "A");
        assert_eq!(candidate_letter(25), "Z");
        assert_eq!(candidate_letter(26), "AA");
        assert_eq!(candidate_letter(27), "AB");
    }

    #[test]
    fn validate_candidate_record_accepts_a_fully_populated_record() {
        let record = build_record(full_candidate(), "EN.19.A", "engine-rs", 19, "2026-09-15");
        assert!(validate_candidate_record(&record).is_ok());
    }

    #[test]
    fn validate_candidate_record_rejects_an_empty_out_of_scope_array() {
        let mut candidate = full_candidate();
        candidate["out_of_scope"] = json!([]);
        let record = build_record(candidate, "EN.19.A", "engine-rs", 19, "2026-09-15");
        let errors = validate_candidate_record(&record).expect_err("empty out_of_scope must fail");
        assert!(errors.iter().any(|e| e.contains("out_of_scope")));
    }

    #[test]
    fn validate_candidate_record_rejects_an_invalid_model_enum_value() {
        let mut candidate = full_candidate();
        candidate["model"] = json!("gpt-5");
        let record = build_record(candidate, "EN.19.A", "engine-rs", 19, "2026-09-15");
        let errors = validate_candidate_record(&record).expect_err("bad model enum must fail");
        assert!(errors.iter().any(|e| e.contains("model")));
    }
}
