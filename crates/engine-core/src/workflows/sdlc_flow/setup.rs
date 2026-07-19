//! Setup-half nodes for the SDLC Flow workflow: `SetupWorktreeNode`,
//! `SpecExistsRouterNode`, `GenerateTasksNode`, `LoadTaskStateNode`.
//!
//! Scaffolded in EN.3.A task 1; implemented in EN.3.A task 2.
//!
//! Model/deterministic split: only `GenerateTasksNode` ever calls a model
//! (the planning-fallback path, gated off the common path by
//! `SpecExistsRouterNode`) — it composes a `ClaudeCodeStep` (EN.2.A) under
//! its own node identity rather than being one. `SetupWorktreeNode` and
//! `LoadTaskStateNode` are pure Rust; `SetupWorktreeNode` uses an injectable
//! command-runner seam (mirroring `ClaudeCodeStep::with_transport`) so tests
//! never shell out to a real `git` subprocess.

use std::path::{Path, PathBuf};

use claude_code_rs::Config;
use engine_contract::TaskContext;
use serde::Deserialize;
use serde_json::json;

use crate::node::{Node, NodeError};
use crate::nodes::ClaudeCodeStep;
use crate::routing::Router;

use super::policy::{self, PartialPolicy, SdlcPolicy};
use super::schema::{parse_task_range, SDLCFlowEventSchema, SDLCState, SDLCTask};
use super::{get_result, put_result};

/// The `ctx.nodes` identity the resolved policy is stamped under, so every
/// downstream node reads one resolved value rather than re-deriving it.
pub const RESOLVED_POLICY_IDENTITY: &str = "ResolvedPolicy";

/// Read `planning/harness.json`'s `sdlc.policy` section (a [`PartialPolicy`])
/// out of a worktree, if the file and section exist. Reuses the same
/// `worktree/planning/harness.json` path `TestTaskNode` reads
/// (`task_loop.rs`) rather than duplicating its check-running logic — this
/// helper only cares about the `sdlc.policy` subsection.
fn read_harness_policy_defaults(worktree: &Path) -> Result<Option<PartialPolicy>, NodeError> {
    let harness_path = worktree.join("planning").join("harness.json");
    if !harness_path.exists() {
        return Ok(None);
    }

    let raw = std::fs::read_to_string(&harness_path).map_err(|err| {
        NodeError::new(format!("failed to read {}: {err}", harness_path.display()))
    })?;
    let harness: serde_json::Value = serde_json::from_str(&raw).map_err(|err| {
        NodeError::new(format!("failed to parse {}: {err}", harness_path.display()))
    })?;

    let Some(policy_value) = harness.get("sdlc").and_then(|v| v.get("policy")) else {
        return Ok(None);
    };

    let partial: PartialPolicy = serde_json::from_value(policy_value.clone()).map_err(|err| {
        NodeError::new(format!(
            "failed to parse {} sdlc.policy: {err}",
            harness_path.display()
        ))
    })?;
    Ok(Some(partial))
}

/// Resolve the three-layer [`SdlcPolicy`] for this run: the inbound event's
/// `policy` override, the worktree's `planning/harness.json` `sdlc.policy`
/// defaults, and the built-in default, high->low precedence via
/// [`policy::resolve`].
pub fn resolve_policy_for_run(ctx: &TaskContext, worktree: &Path) -> Result<SdlcPolicy, NodeError> {
    let event = parse_event(ctx)?;
    let harness_defaults = read_harness_policy_defaults(worktree)?;
    Ok(policy::resolve(
        SdlcPolicy::default(),
        harness_defaults.as_ref(),
        event.policy.as_ref(),
    ))
}

pub use super::{default_command_runner, CommandOutput, CommandRunner, ModelTransport};

/// Deserialize the inbound `SDLC_FLOW` event from `ctx.event`.
fn parse_event(ctx: &TaskContext) -> Result<SDLCFlowEventSchema, NodeError> {
    serde_json::from_value(ctx.event.clone())
        .map_err(|err| NodeError::new(format!("invalid SDLC_FLOW event: {err}")))
}

/// `<worktree_path>/planning/<spec_slug>` — mirrors the Python
/// `_shared.get_spec_dir` helper. Falls back to `.` for `worktree_path` when
/// `SetupWorktreeNode` hasn't run yet (e.g. a unit test driving this node in
/// isolation).
fn spec_dir(ctx: &TaskContext, spec_slug: &str) -> PathBuf {
    let worktree = get_result(ctx, "SetupWorktreeNode")
        .and_then(|value| value.get("worktree_path"))
        .and_then(|value| value.as_str())
        .unwrap_or(".");
    Path::new(worktree).join("planning").join(spec_slug)
}

/// Deterministic node: creates or reattaches the spec's git worktree.
///
/// Computes `branch = branch_name.unwrap_or("sdlc/{spec_slug}")` and
/// `worktree_path = trees/{branch}`, then either reattaches (when
/// `event.resume` is true and the worktree already exists on disk) or runs
/// `git worktree add` via the injectable [`CommandRunner`]. On a non-zero
/// exit, attempts a best-effort `git worktree remove --force` cleanup before
/// surfacing the original failure.
///
/// Deliberately omits the orchestrator's `.env`-copy step from the Python
/// source — that step is specific to the orchestrator's own repo layout and
/// has no equivalent here.
pub struct SetupWorktreeNode {
    runner: CommandRunner,
}

impl SetupWorktreeNode {
    #[must_use]
    pub fn new() -> Self {
        Self {
            runner: default_command_runner(),
        }
    }

    /// Override the command runner used for `git` invocations. Tests use
    /// this to stub the subprocess so the gated suite never shells out.
    #[must_use]
    pub fn with_runner(mut self, runner: CommandRunner) -> Self {
        self.runner = runner;
        self
    }
}

impl Default for SetupWorktreeNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for SetupWorktreeNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let event = parse_event(&ctx)?;
        let branch = event
            .branch_name
            .clone()
            .unwrap_or_else(|| format!("sdlc/{}", event.spec_slug));
        let worktree_path = format!("trees/{branch}");

        let reattaching = event.resume && Path::new(&worktree_path).exists();
        if !reattaching {
            let output = (self.runner)(
                "git",
                &[
                    "worktree",
                    "add",
                    &worktree_path,
                    "-b",
                    &branch,
                    "origin/main",
                ],
                Path::new("."),
            )
            .map_err(|err| NodeError::new(format!("failed to spawn git worktree add: {err}")))?;

            if output.status != 0 {
                // Best-effort cleanup; its own outcome doesn't change the
                // failure we're about to report.
                let _ = (self.runner)(
                    "git",
                    &["worktree", "remove", "--force", &worktree_path],
                    Path::new("."),
                );
                return Err(NodeError::new(format!(
                    "git worktree add failed (status {}): {}",
                    output.status, output.stderr
                )));
            }
        }

        put_result(
            &mut ctx,
            "SetupWorktreeNode",
            json!({ "worktree_path": worktree_path.clone(), "branch_name": branch }),
        );

        let resolved_policy = resolve_policy_for_run(&ctx, Path::new(&worktree_path))?;
        let policy_value = serde_json::to_value(&resolved_policy).map_err(|err| {
            NodeError::new(format!("failed to serialize resolved SdlcPolicy: {err}"))
        })?;
        put_result(&mut ctx, RESOLVED_POLICY_IDENTITY, policy_value);

        Ok(ctx)
    }

    fn name(&self) -> &str {
        "SetupWorktreeNode"
    }
}

/// Deterministic router: keeps the Opus planning-fallback path
/// (`GenerateTasksNode`) off the common path. Routes to `LoadTaskStateNode`
/// when the spec directory already has `sdlc-flow-state.json` or
/// `tasks.json`, else to `GenerateTasksNode`.
pub struct SpecExistsRouterNode;

#[async_trait::async_trait]
impl Node for SpecExistsRouterNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "SpecExistsRouterNode"
    }

    fn as_router(&self) -> Option<&dyn Router> {
        Some(self)
    }
}

impl Router for SpecExistsRouterNode {
    fn route(&self, ctx: &TaskContext) -> Option<String> {
        let spec_slug = ctx.event.get("spec_slug")?.as_str()?;
        let dir = spec_dir(ctx, spec_slug);
        if dir.join("sdlc-flow-state.json").exists() || dir.join("tasks.json").exists() {
            Some("LoadTaskStateNode".to_string())
        } else {
            Some("GenerateTasksNode".to_string())
        }
    }
}

/// Deterministic node: loads `sdlc-flow-state.json` if present, else
/// bootstraps a fresh `SDLCState` from `tasks.json`; applies the
/// `task_range` filter; writes the resulting `SDLCState` (`model_dump`
/// shape) to its own output.
pub struct LoadTaskStateNode;

#[async_trait::async_trait]
impl Node for LoadTaskStateNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let event = parse_event(&ctx)?;
        let dir = spec_dir(&ctx, &event.spec_slug);
        let state_path = dir.join("sdlc-flow-state.json");
        let tasks_path = dir.join("tasks.json");

        let mut state: SDLCState = if state_path.exists() {
            let raw = std::fs::read_to_string(&state_path).map_err(|err| {
                NodeError::new(format!("failed to read {}: {err}", state_path.display()))
            })?;
            serde_json::from_str(&raw).map_err(|err| {
                NodeError::new(format!("failed to parse {}: {err}", state_path.display()))
            })?
        } else if tasks_path.exists() {
            let raw = std::fs::read_to_string(&tasks_path).map_err(|err| {
                NodeError::new(format!("failed to read {}: {err}", tasks_path.display()))
            })?;
            let tasks: Vec<SDLCTask> = serde_json::from_str(&raw).map_err(|err| {
                NodeError::new(format!("failed to parse {}: {err}", tasks_path.display()))
            })?;
            let mut bootstrapped = SDLCState::new(event.spec_slug.clone());
            bootstrapped.tasks = tasks;
            bootstrapped
        } else {
            return Err(NodeError::new(format!(
                "no state or tasks file found for spec {:?} under {}",
                event.spec_slug,
                dir.display()
            )));
        };

        if let Some(ids) = parse_task_range(event.task_range.as_deref()).map_err(NodeError::new)? {
            state.tasks.retain(|task| ids.contains(&task.task_id));
        }

        let value = serde_json::to_value(&state)
            .map_err(|err| NodeError::new(format!("failed to serialize SDLCState: {err}")))?;
        put_result(&mut ctx, "LoadTaskStateNode", value);
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "LoadTaskStateNode"
    }
}

/// Model output shape expected from `GenerateTasksNode`'s prompt: the task
/// list plus its rendered `tasks.md` body.
#[derive(Debug, Deserialize)]
struct GeneratedTasks {
    tasks: Vec<SDLCTask>,
    tasks_markdown: String,
}

/// JSON schema matching [`GeneratedTasks`], passed as `Config.json_schema` so
/// `claude-code-rs` requests (and pre-parses) a schema-constrained reply via
/// `Outcome.structured_output` instead of relying solely on prompt text.
fn generated_tasks_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "tasks": { "type": "array" },
            "tasks_markdown": { "type": "string" },
        },
        "required": ["tasks", "tasks_markdown"],
    })
}

/// Prefer the pre-parsed `structured` value written by [`ClaudeCodeStep`]
/// when present and non-null; otherwise fall back to
/// `strip_json_fence` + `serde_json::from_str` on the raw text `content`.
fn parse_structured_or_fenced<T: serde::de::DeserializeOwned>(
    ctx: &TaskContext,
    node_name: &str,
    content: &str,
) -> Result<T, serde_json::Error> {
    let structured = get_result(ctx, node_name).and_then(|value| value.get("structured").cloned());
    match structured {
        Some(value) if !value.is_null() => serde_json::from_value(value),
        _ => serde_json::from_str(super::strip_json_fence(content)),
    }
}

/// Gather every `*.md` file directly under `dir` except the ones the task
/// loop itself owns (`tasks.md`, generated `tasks.json`,
/// `sdlc-flow-state.json`), concatenated with a `## <filename>` header each.
/// Mirrors the Python `_gather_context` helper. Missing/unreadable entries
/// are skipped rather than failing the whole gather.
fn gather_context(dir: &Path) -> String {
    let excluded = ["tasks.md", "tasks.json", "sdlc-flow-state.json"];
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().and_then(|ext| ext.to_str()) == Some("md")
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| !excluded.contains(&name))
                    .unwrap_or(false)
        })
        .collect();
    entries.sort();

    entries
        .into_iter()
        .filter_map(|path| {
            let content = std::fs::read_to_string(&path).ok()?;
            let name = path.file_name()?.to_str()?.to_string();
            Some(format!("## {name}\n\n{content}"))
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Model node (Opus tier, planning-fallback path only): gathers
/// `planning/{spec_slug}/*.md` context, prompts for a task list, and writes
/// `tasks.md` + `tasks.json`. Composes a `ClaudeCodeStep` (EN.2.A) under its
/// own identity rather than being a bare `ClaudeCodeStep` instance, so it
/// can post-process the model's JSON output into the two files this task's
/// acceptance criteria require.
pub struct GenerateTasksNode {
    config: Config,
    transport: Option<ModelTransport>,
}

impl GenerateTasksNode {
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: Config {
                model: Some("claude-opus-4-8".to_string()),
                ..Config::default()
            },
            transport: None,
        }
    }

    /// Override the transport used by the composed `ClaudeCodeStep`. Tests
    /// use this to stub a real subprocess call with a canned `Outcome`, so
    /// the gated suite never spawns a real `claude`.
    #[must_use]
    pub fn with_transport(mut self, transport: ModelTransport) -> Self {
        self.transport = Some(transport);
        self
    }
}

impl Default for GenerateTasksNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for GenerateTasksNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let event = parse_event(&ctx)?;
        let dir = spec_dir(&ctx, &event.spec_slug);
        let context = gather_context(&dir);
        let prompt = format!(
            "Generate the task list for spec {:?} from the following planning \
             context. Respond with strict JSON of the shape \
             {{\"tasks\": [<SDLCTask>, ...], \"tasks_markdown\": \"<rendered tasks.md body>\"}}.\n\n{context}",
            event.spec_slug
        );

        let mut config = self.config.clone();
        config.json_schema = Some(generated_tasks_schema());

        let mut step = ClaudeCodeStep::new("GenerateTasksNode", config, prompt);
        if let Some(transport) = self.transport.clone() {
            step = step.with_transport(move |config, prompt| (transport)(config, prompt));
        }

        let mut ctx = step.process(ctx).await?;

        let content = ctx
            .nodes
            .get("GenerateTasksNode")
            .and_then(|value| value.get("content"))
            .and_then(|value| value.as_str())
            .ok_or_else(|| NodeError::new("GenerateTasksNode: model returned no content"))?
            .to_string();

        let generated: GeneratedTasks =
            parse_structured_or_fenced(&ctx, "GenerateTasksNode", &content).map_err(|err| {
                NodeError::new(format!(
                    "GenerateTasksNode: failed to parse model output as JSON: {err}"
                ))
            })?;

        std::fs::create_dir_all(&dir)
            .map_err(|err| NodeError::new(format!("failed to create {}: {err}", dir.display())))?;

        let tasks_json_path = dir.join("tasks.json");
        let tasks_md_path = dir.join("tasks.md");
        let tasks_json = serde_json::to_string_pretty(&generated.tasks)
            .map_err(|err| NodeError::new(format!("failed to serialize tasks.json: {err}")))?;
        std::fs::write(&tasks_json_path, tasks_json).map_err(|err| {
            NodeError::new(format!(
                "failed to write {}: {err}",
                tasks_json_path.display()
            ))
        })?;
        std::fs::write(&tasks_md_path, &generated.tasks_markdown).map_err(|err| {
            NodeError::new(format!(
                "failed to write {}: {err}",
                tasks_md_path.display()
            ))
        })?;

        put_result(
            &mut ctx,
            "GenerateTasksNode",
            json!({
                "tasks_json": tasks_json_path.to_string_lossy(),
                "tasks_md": tasks_md_path.to_string_lossy(),
                "task_count": generated.tasks.len(),
            }),
        );

        Ok(ctx)
    }

    fn name(&self) -> &str {
        "GenerateTasksNode"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_code_rs::Outcome;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A unique scratch directory under the OS temp dir, cleaned up by the
    /// caller (or left for the OS to reap — tests don't rely on cleanup for
    /// correctness).
    fn temp_dir() -> PathBuf {
        let n = TMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "engine-core-sdlc-flow-setup-test-{}-{n}",
            std::process::id()
        ));
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

    fn ctx_with_worktree(spec_slug: &str, worktree: &Path) -> TaskContext {
        let mut ctx = empty_context(json!({ "spec_slug": spec_slug }));
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );
        ctx
    }

    // --- SpecExistsRouterNode -------------------------------------------------

    #[test]
    fn spec_exists_routes_to_load_when_tasks_json_present() {
        let worktree = temp_dir();
        let dir = worktree.join("planning").join("my-spec");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("tasks.json"), "[]").unwrap();

        let ctx = ctx_with_worktree("my-spec", &worktree);
        let router = SpecExistsRouterNode;
        assert_eq!(router.route(&ctx), Some("LoadTaskStateNode".to_string()));
    }

    #[test]
    fn spec_exists_routes_to_generate_when_absent() {
        let worktree = temp_dir();
        std::fs::create_dir_all(worktree.join("planning").join("my-spec")).unwrap();

        let ctx = ctx_with_worktree("my-spec", &worktree);
        let router = SpecExistsRouterNode;
        assert_eq!(router.route(&ctx), Some("GenerateTasksNode".to_string()));
    }

    #[test]
    fn spec_exists_routes_to_load_when_state_file_present() {
        let worktree = temp_dir();
        let dir = worktree.join("planning").join("my-spec");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("sdlc-flow-state.json"), "{}").unwrap();

        let ctx = ctx_with_worktree("my-spec", &worktree);
        let router = SpecExistsRouterNode;
        assert_eq!(router.route(&ctx), Some("LoadTaskStateNode".to_string()));
    }

    // --- LoadTaskStateNode ------------------------------------------------

    #[tokio::test]
    async fn load_bootstraps_from_tasks_json_and_filters_by_range() {
        let worktree = temp_dir();
        let dir = worktree.join("planning").join("my-spec");
        std::fs::create_dir_all(&dir).unwrap();
        let tasks = json!([
            { "task_id": 1, "title": "One", "description": "d1" },
            { "task_id": 2, "title": "Two", "description": "d2" },
            { "task_id": 3, "title": "Three", "description": "d3" },
        ]);
        std::fs::write(
            dir.join("tasks.json"),
            serde_json::to_string(&tasks).unwrap(),
        )
        .unwrap();

        let mut ctx = ctx_with_worktree("my-spec", &worktree);
        ctx.event = json!({ "spec_slug": "my-spec", "task_range": "1,3" });

        let node = LoadTaskStateNode;
        let out = node.process(ctx).await.expect("load should succeed");

        let state = out
            .nodes
            .get("LoadTaskStateNode")
            .expect("state output present");
        let task_ids: Vec<u64> = state["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["task_id"].as_u64().unwrap())
            .collect();
        assert_eq!(task_ids, vec![1, 3]);
    }

    #[tokio::test]
    async fn load_prefers_state_file_over_tasks_json() {
        let worktree = temp_dir();
        let dir = worktree.join("planning").join("my-spec");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("tasks.json"), "[]").unwrap();
        let state = SDLCState::new("my-spec");
        std::fs::write(
            dir.join("sdlc-flow-state.json"),
            serde_json::to_string(&state).unwrap(),
        )
        .unwrap();

        let ctx = ctx_with_worktree("my-spec", &worktree);
        let node = LoadTaskStateNode;
        let out = node.process(ctx).await.expect("load should succeed");
        let loaded = out.nodes.get("LoadTaskStateNode").unwrap();
        assert_eq!(loaded["spec_slug"], "my-spec");
    }

    #[tokio::test]
    async fn load_fails_when_neither_file_exists() {
        let worktree = temp_dir();
        std::fs::create_dir_all(worktree.join("planning").join("my-spec")).unwrap();

        let ctx = ctx_with_worktree("my-spec", &worktree);
        let node = LoadTaskStateNode;
        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("no state or tasks file found"));
    }

    // --- SetupWorktreeNode --------------------------------------------------

    fn stub_runner(status: i32) -> CommandRunner {
        Arc::new(move |_program, _args, _cwd| {
            Ok(CommandOutput {
                status,
                stdout: String::new(),
                stderr: if status == 0 {
                    String::new()
                } else {
                    "git failed".to_string()
                },
            })
        })
    }

    #[tokio::test]
    async fn setup_writes_worktree_result_via_stub_runner() {
        let node = SetupWorktreeNode::new().with_runner(stub_runner(0));
        let ctx = empty_context(json!({ "spec_slug": "my-spec" }));

        let out = node.process(ctx).await.expect("setup should succeed");
        let result = out.nodes.get("SetupWorktreeNode").expect("output present");
        assert_eq!(result["branch_name"], "sdlc/my-spec");
        assert_eq!(result["worktree_path"], "trees/sdlc/my-spec");
    }

    #[tokio::test]
    async fn setup_surfaces_git_failure() {
        let node = SetupWorktreeNode::new().with_runner(stub_runner(1));
        let ctx = empty_context(json!({ "spec_slug": "my-spec" }));

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("git worktree add failed"));
    }

    #[tokio::test]
    async fn setup_runs_cleanup_after_failure() {
        let calls: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
        let calls_clone = calls.clone();
        let runner: CommandRunner = Arc::new(move |_program, args, _cwd| {
            calls_clone
                .lock()
                .unwrap()
                .push(args.iter().map(|s| (*s).to_string()).collect());
            let is_remove = args.first() == Some(&"worktree") && args.get(1) == Some(&"remove");
            Ok(CommandOutput {
                status: if is_remove { 0 } else { 1 },
                stdout: String::new(),
                stderr: String::new(),
            })
        });

        let node = SetupWorktreeNode::new().with_runner(runner);
        let ctx = empty_context(json!({ "spec_slug": "my-spec" }));
        let _ = node.process(ctx).await;

        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 2, "expected add + cleanup calls");
        assert_eq!(
            recorded[1][0..2],
            ["worktree".to_string(), "remove".to_string()]
        );
    }

    // --- policy resolution ---------------------------------------------------

    #[test]
    fn harness_policy_defaults_override_builtin() {
        let worktree = temp_dir();
        std::fs::create_dir_all(worktree.join("planning")).unwrap();
        std::fs::write(
            worktree.join("planning").join("harness.json"),
            json!({ "sdlc": { "policy": { "max_attempts": 5 } } }).to_string(),
        )
        .unwrap();

        let ctx = empty_context(json!({ "spec_slug": "my-spec" }));
        let resolved = resolve_policy_for_run(&ctx, &worktree).expect("resolve should succeed");
        assert_eq!(resolved.max_attempts, 5);
    }

    #[test]
    fn event_policy_override_beats_harness_default() {
        let worktree = temp_dir();
        std::fs::create_dir_all(worktree.join("planning")).unwrap();
        std::fs::write(
            worktree.join("planning").join("harness.json"),
            json!({ "sdlc": { "policy": { "max_attempts": 5 } } }).to_string(),
        )
        .unwrap();

        let ctx = empty_context(json!({
            "spec_slug": "my-spec",
            "policy": { "max_attempts": 7 },
        }));
        let resolved = resolve_policy_for_run(&ctx, &worktree).expect("resolve should succeed");
        assert_eq!(resolved.max_attempts, 7);
    }

    #[test]
    fn no_harness_json_falls_through_to_builtin_default() {
        let worktree = temp_dir();
        std::fs::create_dir_all(&worktree).unwrap();

        let ctx = empty_context(json!({ "spec_slug": "my-spec" }));
        let resolved = resolve_policy_for_run(&ctx, &worktree).expect("resolve should succeed");
        assert_eq!(resolved, super::super::policy::SdlcPolicy::default());
    }

    #[tokio::test]
    async fn setup_worktree_node_stamps_resolved_policy_into_ctx() {
        let node = SetupWorktreeNode::new().with_runner(stub_runner(0));
        let ctx = empty_context(json!({ "spec_slug": "my-spec" }));

        let out = node.process(ctx).await.expect("setup should succeed");
        let policy = out
            .nodes
            .get(RESOLVED_POLICY_IDENTITY)
            .expect("resolved policy present in ctx after setup");
        assert_eq!(policy["max_attempts"], 3);
        assert_eq!(policy["review_mode"], "per_task");
    }

    // --- GenerateTasksNode --------------------------------------------------

    fn stub_outcome_with_text(text: &str) -> Outcome {
        Outcome {
            cost_usd: 0.0,
            usage: claude_code_rs::parse::Usage {
                input_tokens: 1,
                output_tokens: 1,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
            model_usage: std::collections::BTreeMap::new(),
            text: text.to_string(),
            is_error: false,
            api_error_status: None,
            structured_output: None,
        }
    }

    fn stub_outcome_with_structured(text: &str, structured: serde_json::Value) -> Outcome {
        Outcome {
            structured_output: Some(structured),
            ..stub_outcome_with_text(text)
        }
    }

    #[tokio::test]
    async fn generate_parses_content_and_writes_files() {
        let worktree = temp_dir();
        std::fs::create_dir_all(worktree.join("planning").join("my-spec")).unwrap();

        let canned = json!({
            "tasks": [{ "task_id": 1, "title": "Do it", "description": "desc" }],
            "tasks_markdown": "# Tasks\n\n1. Do it",
        })
        .to_string();

        let transport: ModelTransport = Arc::new(move |_config, _prompt| {
            let outcome = stub_outcome_with_text(&canned);
            Box::pin(async move { Ok(outcome) })
        });

        let node = GenerateTasksNode::new().with_transport(transport);
        let ctx = ctx_with_worktree("my-spec", &worktree);

        let out = node.process(ctx).await.expect("generate should succeed");
        let result = out.nodes.get("GenerateTasksNode").expect("output present");
        assert_eq!(result["task_count"], 1);

        let dir = worktree.join("planning").join("my-spec");
        assert!(dir.join("tasks.json").exists());
        assert!(dir.join("tasks.md").exists());
        let md = std::fs::read_to_string(dir.join("tasks.md")).unwrap();
        assert!(md.contains("Do it"));
    }

    #[tokio::test]
    async fn generate_prefers_structured_output_over_fence_parse() {
        let worktree = temp_dir();
        std::fs::create_dir_all(worktree.join("planning").join("my-spec")).unwrap();

        // Text is deliberately not valid JSON for GeneratedTasks so the test
        // only passes if the structured path is used, not the fence path.
        let structured = json!({
            "tasks": [{ "task_id": 1, "title": "Structured task", "description": "desc" }],
            "tasks_markdown": "# Tasks\n\n1. Structured task",
        });

        let transport: ModelTransport = Arc::new(move |_config, _prompt| {
            let outcome =
                stub_outcome_with_structured("not fence-parseable json", structured.clone());
            Box::pin(async move { Ok(outcome) })
        });

        let node = GenerateTasksNode::new().with_transport(transport);
        let ctx = ctx_with_worktree("my-spec", &worktree);

        let out = node.process(ctx).await.expect("generate should succeed");
        let result = out.nodes.get("GenerateTasksNode").expect("output present");
        assert_eq!(result["task_count"], 1);

        let dir = worktree.join("planning").join("my-spec");
        let md = std::fs::read_to_string(dir.join("tasks.md")).unwrap();
        assert!(md.contains("Structured task"));
    }

    #[tokio::test]
    async fn generate_falls_back_to_fence_parse_when_structured_absent() {
        let worktree = temp_dir();
        std::fs::create_dir_all(worktree.join("planning").join("my-spec")).unwrap();

        let canned = json!({
            "tasks": [{ "task_id": 1, "title": "Fence task", "description": "desc" }],
            "tasks_markdown": "# Tasks\n\n1. Fence task",
        })
        .to_string();

        let transport: ModelTransport = Arc::new(move |_config, _prompt| {
            let outcome = stub_outcome_with_text(&canned);
            Box::pin(async move { Ok(outcome) })
        });

        let node = GenerateTasksNode::new().with_transport(transport);
        let ctx = ctx_with_worktree("my-spec", &worktree);

        let _out = node.process(ctx).await.expect("generate should succeed");
        let dir = worktree.join("planning").join("my-spec");
        let md = std::fs::read_to_string(dir.join("tasks.md")).unwrap();
        assert!(md.contains("Fence task"));
    }

    #[tokio::test]
    async fn generate_surfaces_invalid_model_output() {
        let worktree = temp_dir();
        std::fs::create_dir_all(worktree.join("planning").join("my-spec")).unwrap();

        let transport: ModelTransport = Arc::new(move |_config, _prompt| {
            let outcome = stub_outcome_with_text("not json");
            Box::pin(async move { Ok(outcome) })
        });

        let node = GenerateTasksNode::new().with_transport(transport);
        let ctx = ctx_with_worktree("my-spec", &worktree);

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("failed to parse model output"));
    }
}
