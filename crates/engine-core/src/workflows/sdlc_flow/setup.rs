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
use std::sync::Arc;

use claude_code_rs::Config;
use engine_contract::TaskContext;
use serde::Deserialize;
use serde_json::json;

use crate::node::{Node, NodeError};
use crate::nodes::ClaudeCodeStep;
use crate::repo_registry::RepoRegistry;
use crate::routing::Router;

use crate::policy::PolicyConfigSource;

use super::policy::{self, PartialPolicy, SdlcPolicy};
use super::profiles;
use super::schema::{parse_task_range, SDLCFlowEventSchema, SDLCState, SDLCTask};
use super::task_loop::{apply_policy, resolved_policy, worktree_path, Stage};
use super::{get_result, parse_structured_or_fenced, put_result};

/// The `ctx.nodes` identity the resolved policy is stamped under, so every
/// downstream node reads one resolved value rather than re-deriving it.
/// Re-exported from the generic `crate::policy::profiles` plumbing (EN.4.0)
/// so existing `setup::RESOLVED_POLICY_IDENTITY` import sites keep
/// resolving unchanged.
pub use crate::policy::RESOLVED_POLICY_IDENTITY;

/// The `harness.json` section key this workflow's policy/profiles live
/// under (`sdlc.policy` / `sdlc.profiles`), passed to the generic
/// `crate::policy::profiles` plumbing.
const WORKFLOW_KEY: &str = "sdlc";

/// Read `<workflow_key>.policy` (a [`PartialPolicy`]) out of the file
/// addressed by `source`. Delegates to the generic
/// `crate::policy::read_harness_policy_defaults_from` (EN.5.D), parameterized
/// by [`WORKFLOW_KEY`].
fn read_harness_policy_defaults_from(
    source: &PolicyConfigSource,
) -> Result<Option<PartialPolicy>, NodeError> {
    crate::policy::read_harness_policy_defaults_from(source, WORKFLOW_KEY)
}

/// Resolve `event.profile` (a named profile) to a [`PartialPolicy`] bundle,
/// preferring a `harness.json` `sdlc.profiles[name]` entry (read via
/// `source`) over the built-in [`profiles::profile_by_name`]. Returns
/// `Ok(None)` when the event carries no `profile` field, and `Err` when a
/// `profile` name is given but resolves to neither source (no silent no-op).
/// Delegates to the generic `crate::policy::resolve_profile_from` (EN.5.D),
/// parameterized by [`WORKFLOW_KEY`] and `profiles::profile_by_name`.
fn resolve_profile_from(
    profile_name: Option<&str>,
    source: &PolicyConfigSource,
) -> Result<Option<PartialPolicy>, NodeError> {
    crate::policy::resolve_profile_from(
        profile_name,
        source,
        WORKFLOW_KEY,
        profiles::profile_by_name,
    )
}

/// Resolve the four-layer [`SdlcPolicy`] for this run against an arbitrary
/// [`PolicyConfigSource`]: the inbound event's `policy` override, the
/// resolved `profile` bundle, `source`'s `sdlc.policy` defaults, and the
/// built-in default, high->low precedence via [`policy::resolve`]. A
/// [`PolicyConfigSource::Builtin`] source resolves successfully with no
/// filesystem access — the case a worktree-free (channel/API-triggered) run
/// needs.
pub fn resolve_policy_for_run_from(
    ctx: &TaskContext,
    source: &PolicyConfigSource,
) -> Result<SdlcPolicy, NodeError> {
    let event = parse_event(ctx)?;
    let harness_defaults = read_harness_policy_defaults_from(source)?;
    let profile = resolve_profile_from(event.profile.as_deref(), source)?;
    Ok(policy::resolve(
        SdlcPolicy::default(),
        harness_defaults.as_ref(),
        profile.as_ref(),
        event.policy.as_ref(),
    ))
}

/// Resolve the four-layer [`SdlcPolicy`] for this run: the inbound event's
/// `policy` override, the resolved `profile` bundle, the worktree's
/// `planning/harness.json` `sdlc.policy` defaults, and the built-in default,
/// high->low precedence via [`policy::resolve`]. Thin wrapper over
/// [`resolve_policy_for_run_from`] with [`PolicyConfigSource::Worktree`].
pub fn resolve_policy_for_run(ctx: &TaskContext, worktree: &Path) -> Result<SdlcPolicy, NodeError> {
    resolve_policy_for_run_from(ctx, &PolicyConfigSource::Worktree(worktree.to_path_buf()))
}

pub use super::{default_command_runner, CommandOutput, CommandRunner, ModelTransport};

/// Resolve the filesystem root this run targets.
///
/// `event.repo` absent -> `std::env::current_dir()`, i.e. today's behavior
/// verbatim: byte-identical relative `worktree_path`, `Path::new(".")` git
/// cwds, `PolicyConfigSource::Worktree(current_dir())`. `event.repo` present
/// -> resolved through `registry` (a **slug**, never a path — see the
/// security-property doc comment on [`SDLCFlowEventSchema::repo`]). A
/// `Some(slug)` with no `registry` available is an error naming the slug: a
/// run must never quietly retarget the wrong repo by silently falling back
/// to `current_dir()`.
pub fn resolve_target_root(
    event: &SDLCFlowEventSchema,
    registry: Option<&crate::repo_registry::RepoRegistry>,
) -> Result<PathBuf, NodeError> {
    match &event.repo {
        Some(slug) => {
            let registry = registry.ok_or_else(|| {
                NodeError::new(format!(
                    "SDLC_FLOW event named repo slug '{slug}' but no repo registry is \
                     available to resolve it"
                ))
            })?;
            registry
                .resolve(slug)
                .map_err(|err| NodeError::new(err.to_string()))
        }
        None => std::env::current_dir()
            .map_err(|err| NodeError::new(format!("failed to resolve current_dir(): {err}"))),
    }
}

/// `true` when `event.repo` is absent, i.e. this run must produce
/// byte-identical behavior to today (relative `worktree_path`, `"."` git
/// cwds, `PolicyConfigSource::Worktree(current_dir())`). One named predicate
/// so downstream code branches explicitly instead of scattering `is_none()`
/// checks.
pub fn target_root_is_default(event: &SDLCFlowEventSchema) -> bool {
    event.repo.is_none()
}

/// Deserialize the inbound `SDLC_FLOW` event from `ctx.event`.
fn parse_event(ctx: &TaskContext) -> Result<SDLCFlowEventSchema, NodeError> {
    serde_json::from_value(ctx.event.clone())
        .map_err(|err| NodeError::new(format!("invalid SDLC_FLOW event: {err}")))
}

/// `<worktree_path>/planning/<spec_slug>` — mirrors the Python
/// `_shared.get_spec_dir` helper. Falls back to the event's resolved target
/// root (EN.3.K) for `worktree_path` when `SetupWorktreeNode` hasn't run yet
/// (e.g. a unit test driving this node in isolation) — a fallback that still
/// exists, only what it falls back *to* changed: an absent `repo` resolves
/// to `current_dir()` (byte-identical to the old `"."` fallback in effect),
/// a `repo`-bearing event without a registry available degrades to `"."`
/// rather than erroring, since this helper has no `Result` to surface one
/// through and no registry seam to consult.
fn spec_dir(ctx: &TaskContext, spec_slug: &str) -> PathBuf {
    if let Some(worktree) = get_result(ctx, "SetupWorktreeNode")
        .and_then(|value| value.get("worktree_path"))
        .and_then(|value| value.as_str())
    {
        return Path::new(worktree).join("planning").join(spec_slug);
    }

    let root = parse_event(ctx)
        .ok()
        .and_then(|event| resolve_target_root(&event, None).ok())
        .unwrap_or_else(|| PathBuf::from("."));
    root.join("planning").join(spec_slug)
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
/// **Dirty-tree guard (live-checkout runs only).** When `use_worktree` is
/// false — the default — the run targets a live checkout, and the downstream
/// tree-wide staging (`commit_all`'s `git add -A`,
/// `stage_untracked_intent`'s `git add -N -A`) would otherwise sweep the
/// operator's unrelated dirty files into the run's commits. This node
/// therefore runs `git status --porcelain` first and aborts, naming the
/// dirty paths, when anything is uncommitted. Mirrors the JS harness's
/// branch-mode guard (`.claude/workflows/sdlc-flow.js`). A
/// `use_worktree: true` run is isolated and is not guarded.
///
/// **Base-ref freshness.** Both branch-creation sites base on `origin/main`,
/// preceded by a best-effort, non-fatal `git fetch origin main` so the base
/// is not an arbitrarily stale local ref. An offline run degrades to the
/// previous behavior rather than failing.
///
/// Deliberately omits the orchestrator's `.env`-copy step from the Python
/// source — that step is specific to the orchestrator's own repo layout and
/// has no equivalent here.
pub struct SetupWorktreeNode {
    runner: CommandRunner,
    registry: Option<Arc<RepoRegistry>>,
}

impl SetupWorktreeNode {
    #[must_use]
    pub fn new() -> Self {
        Self {
            runner: default_command_runner(),
            registry: None,
        }
    }

    /// Override the command runner used for `git` invocations. Tests use
    /// this to stub the subprocess so the gated suite never shells out.
    #[must_use]
    pub fn with_runner(mut self, runner: CommandRunner) -> Self {
        self.runner = runner;
        self
    }

    /// Install the repo registry (EN.3.K) an `event.repo` slug resolves
    /// against. Defaults to `None`, so a unit test or a CLI/in-tree run that
    /// never installs one keeps today's behavior: an absent `repo` resolves
    /// to `current_dir()` regardless, and a present-but-unresolvable `repo`
    /// (no registry available) surfaces a named error rather than silently
    /// falling back.
    #[must_use]
    pub fn with_registry(mut self, registry: Arc<RepoRegistry>) -> Self {
        self.registry = Some(registry);
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

        // EN.3.K: every relative path this node produces is anchored to
        // `root` — `resolve_target_root` returns `current_dir()` verbatim
        // when `event.repo` is absent, so `is_default_target` gates an
        // explicit branch back to today's exact relative strings and `"."`
        // git cwds rather than relying on `PathBuf` join semantics to
        // happen to reproduce them. `RunMeta.worktree_path` is read by
        // `bastion`/`bastion-web`/`run-sdlc-flow.sh`, so this is a
        // behavior-stability guarantee, not a style choice.
        let is_default_target = target_root_is_default(&event);
        let root = resolve_target_root(&event, self.registry.as_deref())?;

        let worktree_path_buf: PathBuf = if is_default_target {
            if event.use_worktree {
                PathBuf::from(format!("trees/{branch}"))
            } else {
                PathBuf::from(".")
            }
        } else if event.use_worktree {
            root.join("trees").join(&branch)
        } else {
            root.clone()
        };
        let worktree_path = worktree_path_buf.to_string_lossy().to_string();

        let git_cwd: PathBuf = if is_default_target {
            PathBuf::from(".")
        } else {
            root.clone()
        };

        // SAFETY GUARD (ticket-setup-rs-closeout item 1). `use_worktree`
        // defaults to FALSE, and on that path this node checks the run's
        // branch out in a LIVE checkout — `worktree_path == "."` (the
        // default target) or the registry-resolved repo root. Everything
        // downstream then stages tree-wide: `commit_all` (`mod.rs`) runs
        // `git add -A` per passed task and at wrap-up, and
        // `stage_untracked_intent` runs `git add -N -A` before each review
        // diff — so any unrelated dirty file in the operator's repo would be
        // swept into a `feat(sdlc): ...` commit, and a formerly read-only
        // seam would write to that repo's real index.
        //
        // Mirrors the JS harness's branch-mode guard
        // (`.claude/workflows/sdlc-flow.js`, setup STEP 3a: `git status
        // --porcelain`, abort with the dirty paths named) so operators get
        // the same behavior from both engines. A `use_worktree: true` run is
        // already isolated and is deliberately NOT guarded — a dirty main
        // tree cannot leak into a fresh worktree. A CLEAN live-repo run is
        // unaffected, so this is behavior-stable except in the dirty case.
        //
        // Not a policy knob (standing rule 6): this is a safety invariant
        // protecting the operator's repo, not a cost/latency/quality
        // tradeoff, so there is no setting a run could reasonably want the
        // other way. `use_worktree: true` is the supported escape hatch.
        if !event.use_worktree {
            let status =
                (self.runner)("git", &["status", "--porcelain"], &git_cwd).map_err(|err| {
                    NodeError::new(format!("failed to spawn git status --porcelain: {err}"))
                })?;
            if status.status != 0 {
                return Err(NodeError::new(format!(
                    "git status --porcelain failed (status {}): {}",
                    status.status, status.stderr
                )));
            }
            if !status.stdout.trim().is_empty() {
                return Err(NodeError::new(format!(
                    "Working tree is not clean — commit or stash your changes, then re-run \
                     (or set use_worktree: true for an isolated checkout). Dirty paths:\n{}",
                    status.stdout.trim_end()
                )));
            }
        }

        if event.use_worktree {
            let reattaching = event.resume && worktree_path_buf.exists();
            if !reattaching {
                // 0c (tasks.md task 5): refresh `origin/main` before basing a
                // branch on it. Best-effort and non-fatal — an offline run
                // degrades to today's behavior (branching from whatever
                // `origin/main` the local repo last saw). Deliberately NOT
                // hoisted above the `use_worktree` fork: the two reattach /
                // resume paths (`worktree` reattach below, `checkout <branch>`
                // further down) create no branch from `origin/main`, so a
                // fetch there would be a pure no-op network call.
                let _ = (self.runner)("git", &["fetch", "origin", "main"], &git_cwd);

                let output = (self.runner)(
                    "git",
                    &[
                        "worktree",
                        "add",
                        &worktree_path,
                        "-b",
                        &branch,
                        // EN.3.K: base ref intentionally unchanged — a
                        // separate concern from *which repo*.
                        "origin/main",
                    ],
                    &git_cwd,
                )
                .map_err(|err| {
                    NodeError::new(format!("failed to spawn git worktree add: {err}"))
                })?;

                if output.status != 0 {
                    // Best-effort cleanup; its own outcome doesn't change the
                    // failure we're about to report.
                    let _ = (self.runner)(
                        "git",
                        &["worktree", "remove", "--force", &worktree_path],
                        &git_cwd,
                    );
                    return Err(NodeError::new(format!(
                        "git worktree add failed (status {}): {}",
                        output.status, output.stderr
                    )));
                }
            }

            // Ensure the planning symlink exists in the worktree
            let worktree_planning = worktree_path_buf.join("planning");
            if !worktree_planning.exists() {
                let planning_source = if is_default_target {
                    PathBuf::from("planning")
                } else {
                    root.join("planning")
                };
                if let Ok(canonical_planning) = std::fs::canonicalize(&planning_source) {
                    #[cfg(unix)]
                    let _ = std::os::unix::fs::symlink(canonical_planning, worktree_planning);
                }
            }

            // Symlink each workspace sibling path dependency (declared in the
            // root Cargo.toml as `../<crate>`) into the worktree's parent
            // directory. `git worktree add` checks out a copy of Cargo.toml
            // at the worktree location, but path dependencies resolve
            // relative to that checked-out Cargo.toml, not the real repo
            // root — without this, `trees/sdlc/<slug>/../<crate>` doesn't
            // exist and every cargo invocation inside the worktree fails to
            // resolve claude-code-rs/mev/okf-core. Best-effort, same
            // tolerance as the planning symlink above: a failure here
            // surfaces later as a clear cargo resolution error rather than
            // aborting the whole worktree setup.
            for sibling_crate in ["claude-code-rs", "mev", "okf-core"] {
                let worktree_sibling = worktree_path_buf.join("..").join(sibling_crate);
                if !worktree_sibling.exists() {
                    let sibling_source = if is_default_target {
                        PathBuf::from("..").join(sibling_crate)
                    } else {
                        root.join("..").join(sibling_crate)
                    };
                    if let Ok(canonical_sibling) = std::fs::canonicalize(&sibling_source) {
                        #[cfg(unix)]
                        let _ = std::os::unix::fs::symlink(canonical_sibling, worktree_sibling);
                    }
                }
            }
        } else {
            if !event.resume {
                // 0c (tasks.md task 5): same best-effort fetch as the
                // worktree path above — this is the second branch-creation
                // site, and it bases the branch on `origin/main` too.
                let _ = (self.runner)("git", &["fetch", "origin", "main"], &git_cwd);

                let output = (self.runner)(
                    "git",
                    &[
                        "checkout",
                        "-B",
                        &branch,
                        // EN.3.K: base ref intentionally unchanged — a
                        // separate concern from *which repo*.
                        "origin/main",
                    ],
                    &git_cwd,
                )
                .map_err(|err| NodeError::new(format!("failed to spawn git checkout: {err}")))?;

                if output.status != 0 {
                    return Err(NodeError::new(format!(
                        "git checkout failed (status {}): {}",
                        output.status, output.stderr
                    )));
                }
            } else {
                let output =
                    (self.runner)("git", &["checkout", &branch], &git_cwd).map_err(|err| {
                        NodeError::new(format!("failed to spawn git checkout: {err}"))
                    })?;

                if output.status != 0 {
                    return Err(NodeError::new(format!(
                        "git checkout failed (status {}): {}",
                        output.status, output.stderr
                    )));
                }
            }
        }

        // Best-effort: capture the base commit SHA this run started from.
        //
        // This is now **run metadata only**. It used to be the diff base:
        // `task_loop::diff_range` returned `{base_sha}..HEAD` and both
        // `ConsolidatedReviewNode` and `classify_trivial` read it. That was
        // the root of the "review always sees an empty diff" defect — the
        // range is a COMMIT range, and nothing in the run ever committed the
        // implementer's code. `ticket-commit-task-work-real-diffs` deleted
        // both `diff_range` and the `base_sha(ctx)` helper; every diff is now
        // taken against the working tree instead (`git add -N -A` followed by
        // `git diff HEAD` for the reviewer, `git diff --numstat HEAD` for the
        // classifier). There is no `main..HEAD` fallback anywhere any more.
        //
        // The stamp is KEPT deliberately: it records the commit the branch
        // was based on, which is useful forensics on a recorded run. A failed
        // `git rev-parse` (or a no-op stub runner in unit tests) simply omits
        // it, and nothing downstream reads it, so its absence changes no
        // behavior.
        let base_sha = (self.runner)("git", &["rev-parse", "HEAD"], Path::new(&worktree_path))
            .ok()
            .filter(|output| output.status == 0)
            .map(|output| output.stdout.trim().to_string())
            .filter(|sha| !sha.is_empty());

        let mut setup_result =
            json!({ "worktree_path": worktree_path.clone(), "branch_name": branch });
        if let Some(sha) = base_sha {
            setup_result["base_sha"] = json!(sha);
        }

        put_result(&mut ctx, "SetupWorktreeNode", setup_result);

        // Idempotent: a served run's dispatch factory (`engine-serve`'s
        // `seed_resolved_policy`, EN.5.D task 7) already resolved and seeded
        // `RESOLVED_POLICY_IDENTITY` into `ctx.nodes` before this node ran —
        // re-resolving here (against this process's cwd, not necessarily the
        // run's eventual worktree) would clobber that dispatch-time
        // resolution with a different `PolicyConfigSource`. Only resolve +
        // stamp when the run reached this node with no policy seeded at all
        // (in-tree/CLI-driven runs, and unit tests driving this node
        // directly).
        if !ctx
            .nodes
            .contains_key(crate::policy::RESOLVED_POLICY_IDENTITY)
        {
            let resolved_policy = resolve_policy_for_run(&ctx, Path::new(&worktree_path))?;
            crate::policy::stamp_resolved_policy(&mut ctx, &resolved_policy)?;
        }

        Ok(ctx)
    }

    fn name(&self) -> &str {
        "SetupWorktreeNode"
    }
}

/// Deterministic router: keeps the Opus planning-fallback path
/// (`GenerateTasksNode`) off the common path. Routes to `LoadTaskStateNode`
/// when the spec directory already has `sdlc/sdlc-flow-state.json` (the
/// D31-committed path — see
/// `D10-committed-state-path-schema-alignment.md`) or `tasks.json`, else to
/// `GenerateTasksNode`.
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
    // EN.3.K task 5: no change needed here. `post_events`'s pre-flight
    // 422 (crates/engine-serve/src/http.rs) already rejects an absent spec
    // directory before a run is spawned, so a run that reaches this router
    // provably has an existing spec dir — the `else` branch below only ever
    // fires for the legitimate "dir exists, no tasks.json yet" case that
    // routes to `GenerateTasksNode`.
    fn route(&self, ctx: &TaskContext) -> Option<String> {
        let spec_slug = ctx.event.get("spec_slug")?.as_str()?;
        let dir = spec_dir(ctx, spec_slug);
        if dir.join("sdlc").join("sdlc-flow-state.json").exists() || dir.join("tasks.json").exists()
        {
            Some("LoadTaskStateNode".to_string())
        } else {
            Some("GenerateTasksNode".to_string())
        }
    }
}

/// Deterministic node: honours `event.resume` when deciding whether to
/// continue from `sdlc/sdlc-flow-state.json` (the D31-committed path/schema)
/// or restart the spec from `tasks.json`; applies the `task_range` filter;
/// writes the resulting `SDLCState` (`model_dump` shape) to its own output.
///
/// **Restart-vs-resume contract** (ticket `resume-reset-semantics`). `resume`
/// is an existing typed event field (`SDLCFlowEventSchema::resume`,
/// `#[serde(default)]` = `false`) that this node ignored until 2026-08-02 —
/// the state file was loaded whenever it existed. The consequence was that a
/// bailed spec reloaded its own corpse (attempts already exhausted) and
/// re-bailed in seconds, with no API-level way to re-run it short of moving
/// the state file by hand. The four cases are now:
///
/// | `resume` | state file | behavior |
/// |---|---|---|
/// | `true` | present | load it — unchanged, byte-for-byte today's path |
/// | `true` | absent | bootstrap from `tasks.json` (a resume of a never-started spec is a fresh start, not an error) |
/// | `false` | present + `tasks.json` present | **archive** the state to `sdlc-flow-state.json.superseded-{discriminator}.bak`, then bootstrap fresh |
/// | `false` | present, no `tasks.json` | load it — there is nothing to restart *from*, so continuing beats erroring |
/// | either | neither file | `Err` — unchanged |
///
/// The old state is never deleted or overwritten: the corpse is forensics.
/// See [`archive_superseded_state`] for the naming/collision rules.
pub struct LoadTaskStateNode;

/// Deterministic discriminator naming an archived state file, derived from
/// the *old file's own* contents so the same file always archives to the same
/// name (no clock, no RNG — tests must be reproducible).
///
/// Prefers the `run_id` stamped by `EN.6.J` (`RunMeta::run_id`), which is the
/// natural key for "the run whose corpse this is". States written before that
/// fix — and every state written by base-template's JS `sdlc-flow.js` engine,
/// which never emits the key — have no `run_id`; those fall back to the
/// committed `status` plus the summed `attempt_count` across all tasks, which
/// is stable for a given file and distinguishes successive bails of the same
/// spec.
fn superseded_discriminator(value: &serde_json::Value) -> String {
    if let Some(run_id) = value
        .get("run_id")
        .and_then(serde_json::Value::as_str)
        .filter(|run_id| !run_id.trim().is_empty())
    {
        return sanitize_path_component(run_id);
    }

    let status = value
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let attempts: u64 = value
        .get("tasks")
        .and_then(serde_json::Value::as_object)
        .map(|tasks| {
            tasks
                .values()
                .filter_map(|task| {
                    task.get("attempt_count")
                        .and_then(serde_json::Value::as_u64)
                })
                .sum()
        })
        .unwrap_or(0);
    format!("{}-attempts{attempts}", sanitize_path_component(status))
}

/// Reduce an arbitrary JSON string to something safe to embed in a filename:
/// ASCII alphanumerics, `-` and `_` survive; everything else becomes `-`.
fn sanitize_path_component(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "unknown".to_string()
    } else {
        cleaned
    }
}

/// Rename `state_path` out of the way so a restart can bootstrap fresh
/// without destroying the previous run's record.
///
/// The archive is `sdlc-flow-state.json.superseded-{discriminator}.bak` beside
/// the original. If that name is already taken (restarting the same superseded
/// run twice, or two bails that share a fallback discriminator), a `-2`, `-3`,
/// … suffix is appended rather than clobbering the earlier archive. Returns
/// the path actually written.
fn archive_superseded_state(
    state_path: &Path,
    old_state: &serde_json::Value,
) -> Result<PathBuf, NodeError> {
    let dir = state_path.parent().unwrap_or_else(|| Path::new("."));
    let discriminator = superseded_discriminator(old_state);
    let base = format!("sdlc-flow-state.json.superseded-{discriminator}");

    let mut candidate = dir.join(format!("{base}.bak"));
    let mut suffix = 2u32;
    while candidate.exists() {
        candidate = dir.join(format!("{base}-{suffix}.bak"));
        suffix += 1;
        // Defensive: a directory with millions of archives is a bug
        // elsewhere, but never spin forever on one.
        if suffix > 10_000 {
            return Err(NodeError::new(format!(
                "refusing to archive {}: too many existing .superseded-{discriminator} archives",
                state_path.display()
            )));
        }
    }

    std::fs::rename(state_path, &candidate).map_err(|err| {
        NodeError::new(format!(
            "failed to archive {} to {}: {err}",
            state_path.display(),
            candidate.display()
        ))
    })?;
    Ok(candidate)
}

#[async_trait::async_trait]
impl Node for LoadTaskStateNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let event = parse_event(&ctx)?;
        let dir = spec_dir(&ctx, &event.spec_slug);
        let state_path = dir.join("sdlc").join("sdlc-flow-state.json");
        let tasks_path = dir.join("tasks.json");

        // Parse the existing state (if any) up front: both the "resume it"
        // and the "archive it" paths need its contents, and a corrupt file
        // should fail loudly either way rather than being silently renamed.
        let existing_state: Option<serde_json::Value> = if state_path.exists() {
            let raw = std::fs::read_to_string(&state_path).map_err(|err| {
                NodeError::new(format!("failed to read {}: {err}", state_path.display()))
            })?;
            Some(serde_json::from_str(&raw).map_err(|err| {
                NodeError::new(format!("failed to parse {}: {err}", state_path.display()))
            })?)
        } else {
            None
        };

        // A restart (`resume: false`) only makes sense when there is a
        // `tasks.json` to bootstrap back from; otherwise the state file is
        // the only description of the spec that exists and continuing from
        // it beats erroring. See the node's doc comment for the full table.
        let restarting = !event.resume && existing_state.is_some() && tasks_path.exists();
        if restarting {
            let old = existing_state
                .as_ref()
                .expect("restarting implies an existing state");
            archive_superseded_state(&state_path, old)?;
        }

        let resumed_state = if restarting { None } else { existing_state };

        let mut state: SDLCState = if let Some(value) = resumed_state {
            SDLCState::from_committed_state_json(&value)?
        } else if tasks_path.exists() {
            let raw = std::fs::read_to_string(&tasks_path).map_err(|err| {
                NodeError::new(format!("failed to read {}: {err}", tasks_path.display()))
            })?;
            let raw_tasks: Vec<serde_json::Value> = serde_json::from_str(&raw).map_err(|err| {
                NodeError::new(format!("failed to parse {}: {err}", tasks_path.display()))
            })?;
            // Fresh bootstrap (no committed run state yet): seed
            // max_attempts from the resolved policy for any task
            // `tasks.json` does not declare its own value for. A resumed
            // run (the `SDLCState::from_committed_state_json` branch above)
            // already has concrete, previously-committed values and must
            // not be re-seeded here.
            let policy = resolved_policy(&ctx)?;
            let tasks = seed_max_attempts(raw_tasks, policy.max_attempts).map_err(|err| {
                NodeError::new(format!(
                    "invalid task shape in {}: {err}",
                    tasks_path.display()
                ))
            })?;
            let mut bootstrapped = SDLCState::new(event.spec_slug.clone());
            bootstrapped.tasks = tasks;
            // Carry block_id/phase_id from the event onto the freshly
            // created state (EN.ticket.wrap-up-closes-the-block task 1).
            // Only done here, on the no-prior-state path: a resumed run
            // (`resumed_state` above, loaded via
            // `SDLCState::from_committed_state_json`) already preserves
            // whatever block_id/phase_id were committed to disk, and must
            // not have them silently overwritten by a differing event
            // value on reattach.
            bootstrapped.block_id = event.block_id.clone();
            bootstrapped.phase_id = event.phase_id.clone();
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
///
/// `tasks` is kept as raw [`serde_json::Value`]s rather than
/// `Vec<SDLCTask>` on purpose: [`seed_max_attempts`] needs the raw shape to
/// tell "the model declared its own `max_attempts`" apart from "the field
/// was absent and `SDLCTask`'s `#[serde(default)]` filled it in" — a
/// distinction a direct `Vec<SDLCTask>` deserialize destroys.
#[derive(Debug, Deserialize)]
struct GeneratedTasks {
    tasks: Vec<serde_json::Value>,
    tasks_markdown: String,
}

/// Convert raw task JSON values into [`SDLCTask`]s, seeding `max_attempts`
/// from `policy_max_attempts` for any task whose own JSON omits the field —
/// never for one that declares it, even if the declared value equals the
/// default. This is the SEED described on [`SdlcPolicy::max_attempts`]'s
/// doc comment: task-declared > policy > `SDLCTask`'s built-in default.
fn seed_max_attempts(
    raw_tasks: Vec<serde_json::Value>,
    policy_max_attempts: u32,
) -> Result<Vec<SDLCTask>, serde_json::Error> {
    raw_tasks
        .into_iter()
        .map(|value| {
            let declared = value.get("max_attempts").is_some();
            let mut task: SDLCTask = serde_json::from_value(value)?;
            if !declared {
                task.max_attempts = policy_max_attempts;
            }
            Ok(task)
        })
        .collect()
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

/// Model node (planning-fallback path only): gathers
/// `planning/{spec_slug}/*.md` context, prompts for a task list, and writes
/// `tasks.md` + `tasks.json`. Composes a `ClaudeCodeStep` (EN.2.A) under its
/// own identity rather than being a bare `ClaudeCodeStep` instance, so it
/// can post-process the model's JSON output into the two files this task's
/// acceptance criteria require.
///
/// The model is **not** hardcoded here: `process` resolves it from the
/// stamped policy's `model_tiers.generate` (`Stage::Generate`), which
/// defaults to the Opus tier — byte-identical to the `claude-opus-4-8`
/// literal this node used to carry in `new()`.
pub struct GenerateTasksNode {
    config: Config,
    transport: Option<ModelTransport>,
}

impl GenerateTasksNode {
    #[must_use]
    pub fn new() -> Self {
        Self {
            // No `model` here — `process` sets it from the resolved policy's
            // `model_tiers.generate` via `apply_policy`, which overwrites
            // `config.model` unconditionally. Leaving the old
            // `claude-opus-4-8` literal in place would be a second, silently
            // dead source of truth for the same value (standing rule 6).
            config: Config::default(),
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

        // Strict read: an absent/unparsable stamp is an error, never a
        // silent fall back to `SdlcPolicy::default()`.
        let policy = resolved_policy(&ctx)?;
        let (mut config, prompt) =
            apply_policy(self.config.clone(), prompt, &policy, Stage::Generate);

        // Scope the model's session to the run's worktree instead of letting
        // it inherit the host process's ambient cwd (under `bastion serve`,
        // the primary checkout on `main`).
        //
        // The severity here is deliberately the *weaker* of the pair this
        // ticket onboarded. `docs::PatchDocsNode` hard-errors on an absent
        // stamp because it is registered with `agentic_write_config`
        // (`dangerously_skip_permissions: true`): a skip-permissions writer
        // must never run unscoped. `GenerateTasksNode` is registered bare
        // (`graph.rs`, no write grant) and post-processes the model's JSON
        // into files itself, via paths derived from `spec_dir(&ctx, ..)`
        // rather than from the session cwd — so best-effort, matching
        // `task_loop::ImplementTaskNode`, keeps a directly-driven ctx (a unit
        // test with no `SetupWorktreeNode` run) working instead of failing
        // the node. In a real walk the stamp is always present:
        // `SetupWorktreeNode -> SpecExistsRouterNode -> GenerateTasksNode`.
        if let Ok(worktree) = worktree_path(&ctx) {
            config.cwd = Some(PathBuf::from(worktree));
        }

        config.json_schema = Some(generated_tasks_schema());

        let mut step = ClaudeCodeStep::new("GenerateTasksNode", config, prompt)
            .with_retry_policy(policy.transport_retry);
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

        // Seed max_attempts from the resolved policy for any task the model
        // did not give its own value (SdlcPolicy::max_attempts's doc
        // comment: task-declared > policy > built-in default).
        let tasks = seed_max_attempts(generated.tasks, policy.max_attempts).map_err(|err| {
            NodeError::new(format!(
                "GenerateTasksNode: invalid task shape in model output: {err}"
            ))
        })?;

        std::fs::create_dir_all(&dir)
            .map_err(|err| NodeError::new(format!("failed to create {}: {err}", dir.display())))?;

        let tasks_json_path = dir.join("tasks.json");
        let tasks_md_path = dir.join("tasks.md");
        let tasks_json = serde_json::to_string_pretty(&tasks)
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
                "task_count": tasks.len(),
                // Stamp the resolved knob values so `RunTelemetry` /
                // `PolicyAggregate` can attribute this stage's observed cost
                // to the settings that caused it (standing rule 6).
                "model_tier": policy.model_tiers.generate,
                "call_timeout_secs": policy.timeouts.generate,
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
    use super::super::policy::{CallTimeouts, ModelTier, ModelTiers};
    use super::super::schema::SDLCTaskStatus;
    use super::*;
    use claude_code_rs::Outcome;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A named scratch directory under the OS temp dir, guaranteed EMPTY at
    /// the moment it is returned.
    ///
    /// Under `cargo nextest` every test runs in its own process, so a
    /// per-process counter (like `TMP_COUNTER` below) restarts at 0 in each
    /// process and a directory name built from `(pid, counter)` is a pure
    /// function of those two numbers — not of the test that requested it.
    /// The OS recycles PIDs (macOS hands them out sequentially and wraps at
    /// ~99998), so a later test run can be handed a directory name a
    /// previous run already used and left populated on disk — `create_dir_all`
    /// on an existing directory succeeds and silently adopts the leftover
    /// contents. That combination produced a false FAIL on 2026-07-31 (PR
    /// #34, block EN.3.J-sdlc-flow-smoke): an absence-asserting test was
    /// handed a directory containing a sibling test's leftover
    /// `sdlc-flow-state.json` from an earlier process that happened to reuse
    /// the same PID. Removing the directory immediately before recreating it
    /// closes that hole for good, independent of what any previous run left
    /// behind.
    fn temp_dir_named(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(name);
        // `remove_dir_all` returns `Err(NotFound)` on the normal first-use
        // case (nothing to remove yet) — that is not a failure, hence `.ok()`.
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// A unique scratch directory under the OS temp dir, cleaned up by the
    /// caller (or left for the OS to reap — tests don't rely on cleanup for
    /// correctness). Guaranteed empty at return time; see `temp_dir_named`.
    fn temp_dir() -> PathBuf {
        let n = TMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        temp_dir_named(&format!(
            "engine-core-sdlc-flow-setup-test-{}-{n}",
            std::process::id()
        ))
    }

    // --- temp_dir_named hermeticity ---------------------------------------

    #[test]
    fn temp_dir_named_returns_empty_dir_even_when_name_was_previously_populated() {
        // Regression guard for the 2026-07-31 false-FAIL (PR #34): a
        // directory handed out under a name that already existed on disk
        // (simulating a PID-recycled leftover from a previous test run)
        // must come back empty, not carrying the old contents.
        let name = "engine-core-hermetic-temp-dir-regression";

        let first = temp_dir_named(name);
        std::fs::write(first.join("sentinel.txt"), "leftover").unwrap();
        let nested = first.join("planning").join("my-spec").join("sdlc");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("sdlc-flow-state.json"), "{}").unwrap();

        let second = temp_dir_named(name);
        assert_eq!(first, second, "same name must yield the same path");

        let entries: Vec<_> = std::fs::read_dir(&second).unwrap().collect();
        assert!(
            entries.is_empty(),
            "expected {} to be empty, found: {entries:?}",
            second.display()
        );
        assert!(
            !nested.join("sdlc-flow-state.json").exists(),
            "leftover state file must not survive a fresh temp_dir_named call"
        );

        std::fs::remove_dir_all(&second).ok();
    }

    #[test]
    fn temp_dir_named_returns_existing_empty_dir_when_name_never_existed() {
        // The Err(NotFound) path: remove_dir_all on a name that has never
        // been used must not fail the helper, and the directory it creates
        // must exist and be empty.
        let name = "engine-core-hermetic-temp-dir-regression-fresh";
        let dir = std::env::temp_dir().join(name);
        std::fs::remove_dir_all(&dir).ok();
        assert!(!dir.exists());

        let result = temp_dir_named(name);
        assert!(result.is_dir());
        let entries: Vec<_> = std::fs::read_dir(&result).unwrap().collect();
        assert!(entries.is_empty());

        std::fs::remove_dir_all(&result).ok();
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
        ctx_with_worktree_and_policy(spec_slug, worktree, &SdlcPolicy::default())
    }

    /// The shape a real walk hands `GenerateTasksNode`: `SetupWorktreeNode`'s
    /// `worktree_path` plus a stamped resolved policy (which the node now
    /// reads strictly).
    fn ctx_with_worktree_and_policy(
        spec_slug: &str,
        worktree: &Path,
        policy: &SdlcPolicy,
    ) -> TaskContext {
        let mut ctx = empty_context(json!({ "spec_slug": spec_slug }));
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );
        ctx.nodes.insert(
            RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(policy).expect("policy serializes"),
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
        let dir = worktree.join("planning").join("my-spec").join("sdlc");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("sdlc-flow-state.json"), "{}").unwrap();

        let ctx = ctx_with_worktree("my-spec", &worktree);
        let router = SpecExistsRouterNode;
        assert_eq!(router.route(&ctx), Some("LoadTaskStateNode".to_string()));
    }

    #[test]
    fn spec_exists_ignores_state_file_at_old_flat_path() {
        // Regression guard for D10: a leftover pre-fix flat-path file must
        // NOT be treated as "spec exists" — only the `sdlc/`-subdirectory
        // path counts.
        let worktree = temp_dir();
        let dir = worktree.join("planning").join("my-spec");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("sdlc-flow-state.json"), "{}").unwrap();

        let ctx = ctx_with_worktree("my-spec", &worktree);
        let router = SpecExistsRouterNode;
        assert_eq!(router.route(&ctx), Some("GenerateTasksNode".to_string()));
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

    /// EN.ticket.sdlc-flow-dead-policy-knobs task 1, AC1: a non-default
    /// policy `max_attempts` changes the bound a task that omits its own
    /// value actually gets, on the bootstrap-from-`tasks.json` path.
    #[tokio::test]
    async fn load_seeds_max_attempts_from_policy_when_task_omits_it() {
        let worktree = temp_dir();
        let dir = worktree.join("planning").join("my-spec");
        std::fs::create_dir_all(&dir).unwrap();
        let tasks = json!([
            { "task_id": 1, "title": "One", "description": "d1" },
        ]);
        std::fs::write(
            dir.join("tasks.json"),
            serde_json::to_string(&tasks).unwrap(),
        )
        .unwrap();

        let policy = SdlcPolicy {
            max_attempts: 7,
            ..SdlcPolicy::default()
        };
        let mut ctx = ctx_with_worktree_and_policy("my-spec", &worktree, &policy);
        ctx.event = json!({ "spec_slug": "my-spec" });

        let out = LoadTaskStateNode.process(ctx).await.expect("load");
        let state = out.nodes.get("LoadTaskStateNode").unwrap();
        assert_eq!(state["tasks"][0]["max_attempts"], 7);
    }

    /// AC2: a task that declares its own `max_attempts` keeps it even when
    /// the policy sets a different value.
    #[tokio::test]
    async fn load_does_not_override_a_task_declared_max_attempts() {
        let worktree = temp_dir();
        let dir = worktree.join("planning").join("my-spec");
        std::fs::create_dir_all(&dir).unwrap();
        let tasks = json!([
            { "task_id": 1, "title": "One", "description": "d1", "max_attempts": 1 },
        ]);
        std::fs::write(
            dir.join("tasks.json"),
            serde_json::to_string(&tasks).unwrap(),
        )
        .unwrap();

        let policy = SdlcPolicy {
            max_attempts: 7,
            ..SdlcPolicy::default()
        };
        let mut ctx = ctx_with_worktree_and_policy("my-spec", &worktree, &policy);
        ctx.event = json!({ "spec_slug": "my-spec" });

        let out = LoadTaskStateNode.process(ctx).await.expect("load");
        let state = out.nodes.get("LoadTaskStateNode").unwrap();
        assert_eq!(state["tasks"][0]["max_attempts"], 1);
    }

    /// AC3: with defaults everywhere (policy default 3, task omits the
    /// field), behaviour is unchanged from before this task.
    #[tokio::test]
    async fn load_with_default_policy_leaves_max_attempts_at_three() {
        let worktree = temp_dir();
        let dir = worktree.join("planning").join("my-spec");
        std::fs::create_dir_all(&dir).unwrap();
        let tasks = json!([
            { "task_id": 1, "title": "One", "description": "d1" },
        ]);
        std::fs::write(
            dir.join("tasks.json"),
            serde_json::to_string(&tasks).unwrap(),
        )
        .unwrap();

        let mut ctx = ctx_with_worktree("my-spec", &worktree);
        ctx.event = json!({ "spec_slug": "my-spec" });

        let out = LoadTaskStateNode.process(ctx).await.expect("load");
        let state = out.nodes.get("LoadTaskStateNode").unwrap();
        assert_eq!(state["tasks"][0]["max_attempts"], 3);
    }

    #[tokio::test]
    async fn load_prefers_state_file_over_tasks_json_when_resuming() {
        let worktree = temp_dir();
        let dir = worktree.join("planning").join("my-spec");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("tasks.json"), "[]").unwrap();
        let sdlc_dir = dir.join("sdlc");
        std::fs::create_dir_all(&sdlc_dir).unwrap();
        let state = SDLCState::new("my-spec");
        let run_meta = super::super::schema::RunMeta {
            branch: "sdlc/my-spec".to_string(),
            worktree_path: worktree.to_string_lossy().to_string(),
            started_at: "2026-07-25T00:00:00Z".to_string(),
            updated_at: "2026-07-25T00:00:00Z".to_string(),
            run_id: None,
        };
        let committed = state.to_committed_state_json(&run_meta, None, None, None, None, None);
        std::fs::write(
            sdlc_dir.join("sdlc-flow-state.json"),
            serde_json::to_string(&committed).unwrap(),
        )
        .unwrap();

        let mut ctx = ctx_with_worktree("my-spec", &worktree);
        ctx.event = json!({ "spec_slug": "my-spec", "resume": true });
        let node = LoadTaskStateNode;
        let out = node.process(ctx).await.expect("load should succeed");
        let loaded = out.nodes.get("LoadTaskStateNode").unwrap();
        assert_eq!(loaded["spec_slug"], "my-spec");
        assert!(
            sdlc_dir.join("sdlc-flow-state.json").exists(),
            "a resume must leave the state file where it is"
        );
    }

    #[tokio::test]
    async fn load_carries_block_id_and_phase_id_from_event_onto_fresh_state() {
        let worktree = temp_dir();
        let dir = worktree.join("planning").join("my-spec");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("tasks.json"), "[]").unwrap();

        let mut ctx = ctx_with_worktree("my-spec", &worktree);
        ctx.event = json!({
            "spec_slug": "my-spec",
            "block_id": "EN.3.A",
            "phase_id": "EN.3",
        });

        let out = LoadTaskStateNode.process(ctx).await.expect("load");
        let loaded = out.nodes.get("LoadTaskStateNode").unwrap();
        assert_eq!(loaded["block_id"], "EN.3.A");
        assert_eq!(loaded["phase_id"], "EN.3");
    }

    #[tokio::test]
    async fn load_leaves_block_id_and_phase_id_null_when_event_supplies_neither() {
        let worktree = temp_dir();
        let dir = worktree.join("planning").join("my-spec");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("tasks.json"), "[]").unwrap();

        let mut ctx = ctx_with_worktree("my-spec", &worktree);
        ctx.event = json!({ "spec_slug": "my-spec" });

        let out = LoadTaskStateNode.process(ctx).await.expect("load");
        let loaded = out.nodes.get("LoadTaskStateNode").unwrap();
        assert!(loaded["block_id"].is_null());
        assert!(loaded["phase_id"].is_null());
    }

    #[tokio::test]
    async fn resume_preserves_block_id_and_phase_id_from_the_state_file() {
        let worktree = temp_dir();
        let dir = worktree.join("planning").join("my-spec");
        let sdlc_dir = dir.join("sdlc");
        std::fs::create_dir_all(&sdlc_dir).unwrap();
        std::fs::write(dir.join("tasks.json"), "[]").unwrap();

        let mut state = SDLCState::new("my-spec");
        state.block_id = Some("EN.3.A".to_string());
        state.phase_id = Some("EN.3".to_string());
        let run_meta = super::super::schema::RunMeta {
            branch: "sdlc/my-spec".to_string(),
            worktree_path: worktree.to_string_lossy().to_string(),
            started_at: "2026-07-25T00:00:00Z".to_string(),
            updated_at: "2026-07-25T00:00:00Z".to_string(),
            run_id: None,
        };
        let committed = state.to_committed_state_json(&run_meta, None, None, None, None, None);
        std::fs::write(
            sdlc_dir.join("sdlc-flow-state.json"),
            serde_json::to_string(&committed).unwrap(),
        )
        .unwrap();

        // A resume must not let a differing event value silently rewrite
        // what was already committed to disk.
        let mut ctx = ctx_with_worktree("my-spec", &worktree);
        ctx.event = json!({
            "spec_slug": "my-spec",
            "resume": true,
            "block_id": "EN.9.Z",
            "phase_id": "EN.9",
        });

        let out = LoadTaskStateNode.process(ctx).await.expect("load");
        let loaded = out.nodes.get("LoadTaskStateNode").unwrap();
        assert_eq!(loaded["block_id"], "EN.3.A");
        assert_eq!(loaded["phase_id"], "EN.3");
    }

    // --- resume / restart semantics ---------------------------------------

    /// Seed `planning/<slug>/` with a `tasks.json` holding `task_count`
    /// pending tasks, plus a committed state file whose tasks are all
    /// `FAILED` with `attempt_count == max_attempts` (a bailed run's
    /// corpse). Returns `(spec_dir, sdlc_dir)`.
    fn seed_bailed_spec(
        worktree: &Path,
        slug: &str,
        task_count: u32,
        run_id: Option<&str>,
    ) -> (PathBuf, PathBuf) {
        let dir = worktree.join("planning").join(slug);
        let sdlc_dir = dir.join("sdlc");
        std::fs::create_dir_all(&sdlc_dir).unwrap();

        let tasks: Vec<serde_json::Value> = (1..=task_count)
            .map(|id| json!({ "task_id": id, "title": format!("T{id}"), "description": "d" }))
            .collect();
        std::fs::write(
            dir.join("tasks.json"),
            serde_json::to_string(&tasks).unwrap(),
        )
        .unwrap();

        let mut state = SDLCState::new(slug);
        for id in 1..=task_count {
            let mut task = SDLCTask::new(id, format!("T{id}"), "d");
            task.status = SDLCTaskStatus::Failed;
            task.attempt_count = task.max_attempts;
            state.tasks.push(task);
        }
        let run_meta = super::super::schema::RunMeta {
            branch: format!("sdlc/{slug}"),
            worktree_path: worktree.to_string_lossy().to_string(),
            started_at: "2026-08-01T00:00:00Z".to_string(),
            updated_at: "2026-08-01T00:00:00Z".to_string(),
            run_id: run_id.map(str::to_string),
        };
        let committed = state.to_committed_state_json(&run_meta, None, None, None, None, None);
        std::fs::write(
            sdlc_dir.join("sdlc-flow-state.json"),
            serde_json::to_string(&committed).unwrap(),
        )
        .unwrap();

        (dir, sdlc_dir)
    }

    fn archived_names(sdlc_dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(sdlc_dir)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.contains(".superseded-"))
            .collect();
        names.sort();
        names
    }

    #[tokio::test]
    async fn resume_true_preserves_attempt_counts_from_the_state_file() {
        let worktree = temp_dir();
        let (_dir, sdlc_dir) = seed_bailed_spec(&worktree, "my-spec", 2, Some("run-aaaa"));

        let mut ctx = ctx_with_worktree("my-spec", &worktree);
        ctx.event = json!({ "spec_slug": "my-spec", "resume": true });
        let out = LoadTaskStateNode.process(ctx).await.expect("load");
        let loaded = out.nodes.get("LoadTaskStateNode").unwrap();

        let attempts: Vec<u64> = loaded["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["attempt_count"].as_u64().unwrap())
            .collect();
        assert_eq!(
            attempts,
            vec![3, 3],
            "resume must reload exhausted attempts"
        );
        assert!(
            archived_names(&sdlc_dir).is_empty(),
            "resume never archives"
        );
        assert!(sdlc_dir.join("sdlc-flow-state.json").exists());
    }

    #[tokio::test]
    async fn resume_false_archives_the_old_state_and_bootstraps_fresh() {
        let worktree = temp_dir();
        let (_dir, sdlc_dir) = seed_bailed_spec(&worktree, "my-spec", 2, Some("run-aaaa"));

        let mut ctx = ctx_with_worktree("my-spec", &worktree);
        ctx.event = json!({ "spec_slug": "my-spec" }); // resume defaults to false
        let out = LoadTaskStateNode.process(ctx).await.expect("load");
        let loaded = out.nodes.get("LoadTaskStateNode").unwrap();

        let attempts: Vec<u64> = loaded["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["attempt_count"].as_u64().unwrap())
            .collect();
        assert_eq!(attempts, vec![0, 0], "a restart bootstraps from tasks.json");
        for task in loaded["tasks"].as_array().unwrap() {
            assert_eq!(task["status"], "pending");
        }

        assert!(
            !sdlc_dir.join("sdlc-flow-state.json").exists(),
            "the live state file must have been moved aside"
        );
        assert_eq!(
            archived_names(&sdlc_dir),
            vec!["sdlc-flow-state.json.superseded-run-aaaa.bak".to_string()],
            "archive name must carry the old file's stamped run_id"
        );

        // The corpse is forensics: its contents must survive verbatim.
        let archived: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(sdlc_dir.join("sdlc-flow-state.json.superseded-run-aaaa.bak"))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(archived["run_id"], "run-aaaa");
        assert_eq!(archived["tasks"]["1"]["attempt_count"], 3);
    }

    #[tokio::test]
    async fn resume_false_without_a_state_file_bootstraps_unchanged() {
        let worktree = temp_dir();
        let dir = worktree.join("planning").join("my-spec");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("tasks.json"),
            r#"[{"task_id":1,"title":"One","description":"d"}]"#,
        )
        .unwrap();

        let mut ctx = ctx_with_worktree("my-spec", &worktree);
        ctx.event = json!({ "spec_slug": "my-spec", "resume": false });
        let out = LoadTaskStateNode.process(ctx).await.expect("load");
        let loaded = out.nodes.get("LoadTaskStateNode").unwrap();
        assert_eq!(loaded["tasks"].as_array().unwrap().len(), 1);
        assert_eq!(loaded["tasks"][0]["attempt_count"], 0);
    }

    #[tokio::test]
    async fn resume_true_without_a_state_file_is_a_graceful_fresh_start() {
        // Pinned choice: resuming a spec that was never started is a fresh
        // start, not an error — the caller asked to continue, and "nothing
        // to continue from" is a normal first run, not a fault.
        let worktree = temp_dir();
        let dir = worktree.join("planning").join("my-spec");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("tasks.json"),
            r#"[{"task_id":1,"title":"One","description":"d"}]"#,
        )
        .unwrap();

        let mut ctx = ctx_with_worktree("my-spec", &worktree);
        ctx.event = json!({ "spec_slug": "my-spec", "resume": true });
        let out = LoadTaskStateNode.process(ctx).await.expect("load");
        let loaded = out.nodes.get("LoadTaskStateNode").unwrap();
        assert_eq!(loaded["tasks"].as_array().unwrap().len(), 1);
        assert_eq!(loaded["tasks"][0]["status"], "pending");
    }

    #[tokio::test]
    async fn repeated_restart_of_the_same_run_id_does_not_clobber_the_first_archive() {
        let worktree = temp_dir();
        let (_dir, sdlc_dir) = seed_bailed_spec(&worktree, "my-spec", 1, Some("run-aaaa"));

        let mut ctx = ctx_with_worktree("my-spec", &worktree);
        ctx.event = json!({ "spec_slug": "my-spec", "resume": false });
        LoadTaskStateNode.process(ctx).await.expect("first restart");

        // Re-seed a state file carrying the SAME stamped run_id, then
        // restart again — the discriminator collides.
        seed_bailed_spec(&worktree, "my-spec", 1, Some("run-aaaa"));
        let mut ctx = ctx_with_worktree("my-spec", &worktree);
        ctx.event = json!({ "spec_slug": "my-spec", "resume": false });
        LoadTaskStateNode
            .process(ctx)
            .await
            .expect("second restart");

        assert_eq!(
            archived_names(&sdlc_dir),
            vec![
                "sdlc-flow-state.json.superseded-run-aaaa-2.bak".to_string(),
                "sdlc-flow-state.json.superseded-run-aaaa.bak".to_string(),
            ],
            "a colliding archive name gets a numeric suffix, never an overwrite"
        );
    }

    #[tokio::test]
    async fn restart_falls_back_to_a_deterministic_name_when_run_id_is_absent() {
        // States written before EN.6.J — and every state written by
        // base-template's JS engine — carry no run_id. The fallback must be
        // derived from the file's own contents, never from a clock.
        let worktree = temp_dir();
        let (_dir, sdlc_dir) = seed_bailed_spec(&worktree, "my-spec", 2, None);

        let mut ctx = ctx_with_worktree("my-spec", &worktree);
        ctx.event = json!({ "spec_slug": "my-spec", "resume": false });
        LoadTaskStateNode.process(ctx).await.expect("restart");

        let names = archived_names(&sdlc_dir);
        assert_eq!(names.len(), 1, "exactly one archive, got {names:?}");
        // 2 blocked tasks x 3 attempts each = 6.
        assert!(
            names[0].ends_with("-attempts6.bak"),
            "fallback discriminator must fold in the summed attempt counts: {names:?}"
        );
    }

    #[tokio::test]
    async fn restart_without_tasks_json_still_loads_the_state_file() {
        // Nothing to bootstrap back from: continuing beats erroring, and the
        // state file must NOT be archived (that would strand the run).
        let worktree = temp_dir();
        let dir = worktree.join("planning").join("my-spec");
        let sdlc_dir = dir.join("sdlc");
        std::fs::create_dir_all(&sdlc_dir).unwrap();
        let mut state = SDLCState::new("my-spec");
        state.tasks.push(SDLCTask::new(1, "One", "d"));
        let run_meta = super::super::schema::RunMeta {
            branch: "sdlc/my-spec".to_string(),
            worktree_path: worktree.to_string_lossy().to_string(),
            started_at: "2026-08-01T00:00:00Z".to_string(),
            updated_at: "2026-08-01T00:00:00Z".to_string(),
            run_id: Some("run-bbbb".to_string()),
        };
        let committed = state.to_committed_state_json(&run_meta, None, None, None, None, None);
        std::fs::write(
            sdlc_dir.join("sdlc-flow-state.json"),
            serde_json::to_string(&committed).unwrap(),
        )
        .unwrap();

        let mut ctx = ctx_with_worktree("my-spec", &worktree);
        ctx.event = json!({ "spec_slug": "my-spec", "resume": false });
        let out = LoadTaskStateNode.process(ctx).await.expect("load");
        assert_eq!(
            out.nodes.get("LoadTaskStateNode").unwrap()["spec_slug"],
            "my-spec"
        );
        assert!(sdlc_dir.join("sdlc-flow-state.json").exists());
        assert!(archived_names(&sdlc_dir).is_empty());
    }

    #[tokio::test]
    async fn restart_keeps_the_task_range_filter() {
        let worktree = temp_dir();
        let (_dir, _sdlc_dir) = seed_bailed_spec(&worktree, "my-spec", 3, Some("run-cccc"));

        let mut ctx = ctx_with_worktree("my-spec", &worktree);
        ctx.event = json!({ "spec_slug": "my-spec", "resume": false, "task_range": "2-3" });
        let out = LoadTaskStateNode.process(ctx).await.expect("load");
        let loaded = out.nodes.get("LoadTaskStateNode").unwrap();
        let ids: Vec<u64> = loaded["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["task_id"].as_u64().unwrap())
            .collect();
        assert_eq!(ids, vec![2, 3]);
    }

    #[tokio::test]
    async fn restart_leaves_a_corrupt_state_file_in_place_and_errors() {
        // A file we cannot parse is not archived — a silent rename would
        // hide the corruption we want reported.
        let worktree = temp_dir();
        let dir = worktree.join("planning").join("my-spec");
        let sdlc_dir = dir.join("sdlc");
        std::fs::create_dir_all(&sdlc_dir).unwrap();
        std::fs::write(dir.join("tasks.json"), "[]").unwrap();
        std::fs::write(sdlc_dir.join("sdlc-flow-state.json"), "{not json").unwrap();

        let mut ctx = ctx_with_worktree("my-spec", &worktree);
        ctx.event = json!({ "spec_slug": "my-spec", "resume": false });
        let err = LoadTaskStateNode
            .process(ctx)
            .await
            .expect_err("should fail");
        assert!(err.message.contains("failed to parse"));
        assert!(sdlc_dir.join("sdlc-flow-state.json").exists());
        assert!(archived_names(&sdlc_dir).is_empty());
    }

    #[tokio::test]
    async fn a_bailed_spec_re_runs_instead_of_re_bailing() {
        // The scenario this ticket exists for. Before the fix, a re-POST of
        // a bailed spec reloaded the corpse — every task FAILED with
        // attempts exhausted — and the task loop re-bailed without doing any
        // work. After the fix the same POST yields a state the loop can
        // actually dispatch: a PENDING task with attempts left.
        let worktree = temp_dir();
        let (_dir, sdlc_dir) = seed_bailed_spec(&worktree, "bailed-spec", 2, Some("run-dead"));

        // Sanity: the seeded corpse really is un-runnable.
        let seeded: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(sdlc_dir.join("sdlc-flow-state.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(seeded["tasks"]["1"]["status"], "failed");
        assert_eq!(
            seeded["tasks"]["1"]["attempt_count"],
            seeded["tasks"]["1"]["max_attempts"]
        );

        let mut ctx = ctx_with_worktree("bailed-spec", &worktree);
        ctx.event = json!({ "spec_slug": "bailed-spec" });
        let out = LoadTaskStateNode.process(ctx).await.expect("load");
        let state: SDLCState =
            serde_json::from_value(out.nodes.get("LoadTaskStateNode").unwrap().clone()).unwrap();

        // The predicate the task loop dispatches on: a pending task still
        // under its attempt budget.
        let next = state
            .tasks
            .iter()
            .find(|task| task.status == SDLCTaskStatus::Pending)
            .expect("a restarted spec must offer a dispatchable task");
        assert_eq!(next.task_id, 1);
        assert!(next.attempt_count < next.max_attempts);
        assert!(
            !archived_names(&sdlc_dir).is_empty(),
            "the previous run's record must still be on disk"
        );
    }

    #[test]
    fn superseded_discriminator_sanitizes_unsafe_characters() {
        let value = json!({ "run_id": "../../etc/passwd" });
        let discriminator = superseded_discriminator(&value);
        assert!(!discriminator.contains('/'));
        assert!(!discriminator.contains('.'));
        assert_eq!(discriminator, "------etc-passwd");
    }

    #[tokio::test]
    async fn load_parses_committed_object_shaped_tasks() {
        let worktree = temp_dir();
        let dir = worktree.join("planning").join("my-spec");
        let sdlc_dir = dir.join("sdlc");
        std::fs::create_dir_all(&sdlc_dir).unwrap();
        let mut state = SDLCState::new("my-spec");
        state.tasks.push(SDLCTask::new(1, "One", "d1"));
        state.tasks.push(SDLCTask::new(2, "Two", "d2"));
        let run_meta = super::super::schema::RunMeta {
            branch: "sdlc/my-spec".to_string(),
            worktree_path: worktree.to_string_lossy().to_string(),
            started_at: "2026-07-25T00:00:00Z".to_string(),
            updated_at: "2026-07-25T00:00:00Z".to_string(),
            run_id: None,
        };
        let committed = state.to_committed_state_json(&run_meta, None, None, None, None, None);
        std::fs::write(
            sdlc_dir.join("sdlc-flow-state.json"),
            serde_json::to_string(&committed).unwrap(),
        )
        .unwrap();

        let ctx = ctx_with_worktree("my-spec", &worktree);
        let node = LoadTaskStateNode;
        let out = node.process(ctx).await.expect("load should succeed");
        let loaded = out.nodes.get("LoadTaskStateNode").unwrap();
        let task_ids: Vec<u64> = loaded["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["task_id"].as_u64().unwrap())
            .collect();
        assert_eq!(task_ids, vec![1, 2]);
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
        let ctx = empty_context(json!({ "spec_slug": "my-spec", "use_worktree": true }));

        let out = node.process(ctx).await.expect("setup should succeed");
        let result = out.nodes.get("SetupWorktreeNode").expect("output present");
        assert_eq!(result["branch_name"], "sdlc/my-spec");
        assert_eq!(result["worktree_path"], "trees/sdlc/my-spec");
    }

    #[tokio::test]
    async fn setup_surfaces_git_failure() {
        let node = SetupWorktreeNode::new().with_runner(stub_runner(1));
        let ctx = empty_context(json!({ "spec_slug": "my-spec", "use_worktree": true }));

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
        let ctx = empty_context(json!({ "spec_slug": "my-spec", "use_worktree": true }));
        let _ = node.process(ctx).await;

        let recorded = calls.lock().unwrap();
        // fetch (best-effort, itself failing here and ignored) + worktree add
        // + cleanup remove.
        assert_eq!(
            recorded.len(),
            3,
            "expected fetch + add + cleanup calls, got: {recorded:?}"
        );
        assert_eq!(recorded[0][0], "fetch");
        assert_eq!(
            recorded[2][0..2],
            ["worktree".to_string(), "remove".to_string()]
        );
    }

    #[tokio::test]
    async fn setup_stamps_base_sha_from_rev_parse() {
        const FIXED_SHA: &str = "abc1234def5678900000000000000000000000";
        let runner: CommandRunner = Arc::new(move |_program, args, _cwd| {
            if args.first() == Some(&"rev-parse") {
                Ok(CommandOutput {
                    status: 0,
                    stdout: format!("{FIXED_SHA}\n"),
                    stderr: String::new(),
                })
            } else {
                Ok(CommandOutput {
                    status: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
        });

        let node = SetupWorktreeNode::new().with_runner(runner);
        let ctx = empty_context(json!({ "spec_slug": "my-spec", "use_worktree": true }));

        let out = node.process(ctx).await.expect("setup should succeed");
        let result = out.nodes.get("SetupWorktreeNode").expect("output present");
        assert_eq!(result["base_sha"], FIXED_SHA);
    }

    #[tokio::test]
    async fn setup_omits_base_sha_when_rev_parse_fails() {
        let node = SetupWorktreeNode::new().with_runner(stub_runner(0));
        let ctx = empty_context(json!({ "spec_slug": "my-spec", "use_worktree": true }));

        let out = node.process(ctx).await.expect("setup should succeed");
        let result = out.nodes.get("SetupWorktreeNode").expect("output present");
        assert!(
            result.get("base_sha").is_none(),
            "base_sha should be absent when rev-parse yields no usable SHA"
        );
    }

    // --- dirty-tree guard (ticket-setup-rs-closeout item 1) -----------------

    /// A runner that answers `git status --porcelain` with `porcelain` and
    /// every other invocation with an empty success, recording all argv.
    #[allow(clippy::type_complexity)]
    fn porcelain_runner(porcelain: &'static str) -> (CommandRunner, Arc<Mutex<Vec<Vec<String>>>>) {
        let calls: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
        let calls_clone = calls.clone();
        let runner: CommandRunner = Arc::new(move |_program, args, _cwd| {
            calls_clone
                .lock()
                .unwrap()
                .push(args.iter().map(|s| (*s).to_string()).collect());
            let is_status = args.first() == Some(&"status");
            Ok(CommandOutput {
                status: 0,
                stdout: if is_status {
                    porcelain.to_string()
                } else {
                    String::new()
                },
                stderr: String::new(),
            })
        });
        (runner, calls)
    }

    #[tokio::test]
    async fn live_repo_run_proceeds_when_the_tree_is_clean() {
        // Behavior-stability guard: the clean live-repo path must be
        // unchanged by the dirty-tree abort.
        let (runner, calls) = porcelain_runner("");
        let node = SetupWorktreeNode::new().with_runner(runner);
        let ctx = empty_context(json!({ "spec_slug": "my-spec" }));

        let out = node.process(ctx).await.expect("clean tree should proceed");
        let result = out.nodes.get("SetupWorktreeNode").expect("output present");
        assert_eq!(result["worktree_path"], ".");

        let recorded = calls.lock().unwrap();
        assert_eq!(
            recorded.first().map(Vec::as_slice),
            Some(["status".to_string(), "--porcelain".to_string()].as_slice()),
            "the guard must run before anything mutating"
        );
        assert!(
            recorded
                .iter()
                .any(|args| args.first().map(String::as_str) == Some("checkout")),
            "a clean run still creates its branch"
        );
    }

    #[tokio::test]
    async fn live_repo_run_aborts_on_a_dirty_tree_naming_the_paths() {
        let (runner, calls) = porcelain_runner(" M src/lib.rs\n?? scratch/notes.txt\n");
        let node = SetupWorktreeNode::new().with_runner(runner);
        let ctx = empty_context(json!({ "spec_slug": "my-spec" }));

        let err = node.process(ctx).await.expect_err("dirty tree must abort");
        assert!(
            err.message.contains("Working tree is not clean"),
            "unexpected message: {}",
            err.message
        );
        assert!(
            err.message.contains("src/lib.rs") && err.message.contains("scratch/notes.txt"),
            "the abort must name the dirty paths: {}",
            err.message
        );
        assert!(
            err.message.contains("use_worktree: true"),
            "the abort must point at the escape hatch: {}",
            err.message
        );

        let recorded = calls.lock().unwrap();
        assert_eq!(
            recorded.len(),
            1,
            "nothing may run after the guard fires, got: {recorded:?}"
        );
        assert!(
            !recorded
                .iter()
                .any(|args| args.first().map(String::as_str) == Some("checkout")),
            "the branch must not be created on an aborted run"
        );
    }

    #[tokio::test]
    async fn dirty_tree_does_not_abort_a_use_worktree_run() {
        // `use_worktree: true` is already isolated: a dirty main tree cannot
        // leak into a freshly-added worktree, so the guard must not fire and
        // must not even ask.
        let (runner, calls) = porcelain_runner(" M src/lib.rs\n");
        let node = SetupWorktreeNode::new().with_runner(runner);
        let ctx = empty_context(json!({ "spec_slug": "my-spec", "use_worktree": true }));

        let out = node
            .process(ctx)
            .await
            .expect("an isolated run is unaffected by a dirty main tree");
        let result = out.nodes.get("SetupWorktreeNode").expect("output present");
        assert_eq!(result["worktree_path"], "trees/sdlc/my-spec");

        let recorded = calls.lock().unwrap();
        assert!(
            !recorded
                .iter()
                .any(|args| args.first().map(String::as_str) == Some("status")),
            "the guard must not run at all under use_worktree: true"
        );
    }

    #[tokio::test]
    async fn live_repo_run_surfaces_a_failing_status_check() {
        // Cannot prove the tree is clean -> refuse to stage tree-wide.
        let runner: CommandRunner = Arc::new(move |_program, args, _cwd| {
            Ok(CommandOutput {
                status: i32::from(args.first() == Some(&"status")),
                stdout: String::new(),
                stderr: "fatal: not a git repository".to_string(),
            })
        });
        let node = SetupWorktreeNode::new().with_runner(runner);
        let ctx = empty_context(json!({ "spec_slug": "my-spec" }));

        let err = node.process(ctx).await.expect_err("must not proceed");
        assert!(
            err.message.contains("git status --porcelain failed"),
            "unexpected message: {}",
            err.message
        );
    }

    // --- best-effort fetch before branch creation (0c / tasks.md task 5) ----

    #[tokio::test]
    async fn fetch_precedes_worktree_branch_creation() {
        let (runner, calls) = porcelain_runner("");
        let node = SetupWorktreeNode::new().with_runner(runner);
        let ctx = empty_context(json!({ "spec_slug": "my-spec", "use_worktree": true }));

        node.process(ctx).await.expect("setup should succeed");

        let recorded = calls.lock().unwrap();
        let fetch_at = recorded
            .iter()
            .position(|args| args.first().map(String::as_str) == Some("fetch"))
            .expect("fetch must run");
        assert_eq!(
            recorded[fetch_at],
            vec![
                "fetch".to_string(),
                "origin".to_string(),
                "main".to_string()
            ]
        );
        let add_at = recorded
            .iter()
            .position(|args| args.first().map(String::as_str) == Some("worktree"))
            .expect("worktree add must run");
        assert!(
            fetch_at < add_at,
            "fetch must precede branch creation: {recorded:?}"
        );
    }

    #[tokio::test]
    async fn fetch_precedes_checkout_branch_creation() {
        let (runner, calls) = porcelain_runner("");
        let node = SetupWorktreeNode::new().with_runner(runner);
        let ctx = empty_context(json!({ "spec_slug": "my-spec" }));

        node.process(ctx).await.expect("setup should succeed");

        let recorded = calls.lock().unwrap();
        let fetch_at = recorded
            .iter()
            .position(|args| args.first().map(String::as_str) == Some("fetch"))
            .expect("fetch must run");
        let checkout_at = recorded
            .iter()
            .position(|args| args.first().map(String::as_str) == Some("checkout"))
            .expect("checkout must run");
        assert!(
            fetch_at < checkout_at,
            "fetch must precede branch creation: {recorded:?}"
        );
        // The base ref is unchanged by this task.
        assert!(recorded[checkout_at].contains(&"origin/main".to_string()));
    }

    #[tokio::test]
    async fn a_failing_fetch_does_not_fail_the_node() {
        let calls: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
        let calls_clone = calls.clone();
        let runner: CommandRunner = Arc::new(move |_program, args, _cwd| {
            calls_clone
                .lock()
                .unwrap()
                .push(args.iter().map(|s| (*s).to_string()).collect());
            if args.first() == Some(&"fetch") {
                // Offline: the subprocess itself fails to complete.
                return Err(std::io::Error::other("network unreachable"));
            }
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });

        let node = SetupWorktreeNode::new().with_runner(runner);
        let ctx = empty_context(json!({ "spec_slug": "my-spec", "use_worktree": true }));

        let out = node
            .process(ctx)
            .await
            .expect("an offline fetch must degrade, not fail the node");
        assert_eq!(
            out.nodes.get("SetupWorktreeNode").expect("output present")["worktree_path"],
            "trees/sdlc/my-spec"
        );

        let recorded = calls.lock().unwrap();
        assert!(recorded
            .iter()
            .any(|args| args.first().map(String::as_str) == Some("worktree")));
    }

    #[tokio::test]
    async fn no_fetch_on_the_reattach_and_resume_paths() {
        // Neither resume path creates a branch from `origin/main`, so a fetch
        // there would be a pure no-op network call.
        let (runner, calls) = porcelain_runner("");
        let node = SetupWorktreeNode::new().with_runner(runner);
        let ctx = empty_context(json!({ "spec_slug": "my-spec", "resume": true }));

        node.process(ctx).await.expect("setup should succeed");

        let recorded = calls.lock().unwrap();
        assert!(
            !recorded
                .iter()
                .any(|args| args.first().map(String::as_str) == Some("fetch")),
            "a resume must not fetch: {recorded:?}"
        );
        assert_eq!(
            recorded
                .iter()
                .find(|args| args.first().map(String::as_str) == Some("checkout"))
                .expect("checkout must run"),
            &vec!["checkout".to_string(), "sdlc/my-spec".to_string()]
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

    #[test]
    fn event_profile_with_no_harness_profiles_resolves_to_builtin_cheap_fast() {
        let worktree = temp_dir();
        std::fs::create_dir_all(&worktree).unwrap();

        let ctx = empty_context(json!({
            "spec_slug": "my-spec",
            "profile": "cheap-fast",
        }));
        let resolved = resolve_policy_for_run(&ctx, &worktree).expect("resolve should succeed");
        assert_eq!(
            resolved.model_tiers.implement,
            super::super::policy::ModelTier::Haiku
        );
        assert_eq!(
            resolved.model_tiers.triage,
            super::super::policy::ModelTier::Haiku
        );
        assert_eq!(
            resolved.model_tiers.review,
            super::super::policy::ModelTier::Haiku
        );
        assert_eq!(
            resolved.output_verbosity,
            super::super::policy::OutputVerbosity::Terse
        );
        assert_eq!(
            resolved.review_mode,
            super::super::policy::ReviewMode::TrivialSkip
        );
    }

    #[test]
    fn event_inline_policy_overrides_profile_but_keeps_profile_tiers() {
        let worktree = temp_dir();
        std::fs::create_dir_all(&worktree).unwrap();

        let ctx = empty_context(json!({
            "spec_slug": "my-spec",
            "profile": "cheap-fast",
            "policy": { "max_attempts": 9 },
        }));
        let resolved = resolve_policy_for_run(&ctx, &worktree).expect("resolve should succeed");
        assert_eq!(
            resolved.model_tiers.implement,
            super::super::policy::ModelTier::Haiku
        );
        assert_eq!(resolved.max_attempts, 9);
    }

    #[test]
    fn unknown_profile_name_returns_node_error() {
        let worktree = temp_dir();
        std::fs::create_dir_all(&worktree).unwrap();

        let ctx = empty_context(json!({
            "spec_slug": "my-spec",
            "profile": "nonexistent",
        }));
        let err = resolve_policy_for_run(&ctx, &worktree).expect_err("should fail");
        assert!(err.message.contains("unknown profile"));
    }

    #[test]
    fn harness_profiles_take_precedence_over_builtin_profiles() {
        let worktree = temp_dir();
        std::fs::create_dir_all(worktree.join("planning")).unwrap();
        std::fs::write(
            worktree.join("planning").join("harness.json"),
            json!({
                "sdlc": {
                    "profiles": {
                        "cheap-fast": { "max_attempts": 42 }
                    }
                }
            })
            .to_string(),
        )
        .unwrap();

        let ctx = empty_context(json!({
            "spec_slug": "my-spec",
            "profile": "cheap-fast",
        }));
        let resolved = resolve_policy_for_run(&ctx, &worktree).expect("resolve should succeed");
        // The harness.json override for "cheap-fast" only sets max_attempts,
        // so it should win over the built-in bundle rather than merge with it.
        assert_eq!(resolved.max_attempts, 42);
        assert_eq!(
            resolved.model_tiers.implement,
            super::super::policy::ModelTier::Sonnet
        );
    }

    #[test]
    fn harness_profiles_with_comment_sibling_key_still_resolves() {
        // Regression: `sdlc.profiles` documented with a sibling `_comment`
        // string entry (see planning/harness.examples.md) must not break
        // deserialization of the map<String, PartialPolicy> as a whole.
        let worktree = temp_dir();
        std::fs::create_dir_all(worktree.join("planning")).unwrap();
        std::fs::write(
            worktree.join("planning").join("harness.json"),
            json!({
                "sdlc": {
                    "profiles": {
                        "_comment": "explanatory text, not a PartialPolicy",
                        "cheap-fast": { "max_attempts": 42 }
                    }
                }
            })
            .to_string(),
        )
        .unwrap();

        let ctx = empty_context(json!({
            "spec_slug": "my-spec",
            "profile": "cheap-fast",
        }));
        let resolved = resolve_policy_for_run(&ctx, &worktree).expect("resolve should succeed");
        assert_eq!(resolved.max_attempts, 42);
    }

    #[test]
    fn resolve_policy_for_run_succeeds_against_repo_harness_json() {
        // Regression: exercise the real committed planning/harness.json
        // (which carries a `_comment` sibling key under `sdlc.profiles`) to
        // guard against reintroducing a deserialization break that only
        // synthetic in-test fixtures would miss.
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../")
            .canonicalize()
            .expect("repo root should resolve");

        let ctx = empty_context(json!({
            "spec_slug": "my-spec",
            "profile": "cheap-fast",
        }));
        resolve_policy_for_run(&ctx, &repo_root).expect(
            "resolving a built-in profile name against the repo's harness.json should succeed",
        );
    }

    // --- resolve_policy_for_run_from / PolicyConfigSource --------------------

    #[test]
    fn resolve_policy_for_run_from_builtin_source_needs_no_worktree() {
        let ctx = empty_context(json!({ "spec_slug": "my-spec" }));
        let resolved =
            resolve_policy_for_run_from(&ctx, &crate::policy::PolicyConfigSource::Builtin)
                .expect("resolve should succeed with no filesystem access");
        assert_eq!(resolved, SdlcPolicy::default());
    }

    #[test]
    fn resolve_policy_for_run_from_builtin_source_still_errors_on_unknown_profile() {
        let ctx = empty_context(json!({
            "spec_slug": "my-spec",
            "profile": "nonexistent",
        }));
        let err = resolve_policy_for_run_from(&ctx, &crate::policy::PolicyConfigSource::Builtin)
            .expect_err("should fail");
        assert!(err.message.contains("unknown profile"));
    }

    #[test]
    fn resolve_policy_for_run_from_harness_file_source_preserves_precedence() {
        let dir = temp_dir();
        let harness_file = dir.join("standalone-harness.json");
        std::fs::write(
            &harness_file,
            json!({ "sdlc": { "policy": { "max_attempts": 5 } } }).to_string(),
        )
        .unwrap();
        let source = crate::policy::PolicyConfigSource::HarnessFile(harness_file);

        let ctx = empty_context(json!({
            "spec_slug": "my-spec",
            "policy": { "max_attempts": 7 },
        }));
        let resolved = resolve_policy_for_run_from(&ctx, &source).expect("resolve should succeed");
        // event > harness default
        assert_eq!(resolved.max_attempts, 7);
    }

    #[test]
    fn resolve_policy_for_run_wrapper_matches_from_worktree_source() {
        let worktree = temp_dir();
        std::fs::create_dir_all(worktree.join("planning")).unwrap();
        std::fs::write(
            worktree.join("planning").join("harness.json"),
            json!({ "sdlc": { "policy": { "max_attempts": 5 } } }).to_string(),
        )
        .unwrap();

        let ctx = empty_context(json!({ "spec_slug": "my-spec" }));
        let via_wrapper = resolve_policy_for_run(&ctx, &worktree).expect("should succeed");
        let via_from = resolve_policy_for_run_from(
            &ctx,
            &crate::policy::PolicyConfigSource::Worktree(worktree.clone()),
        )
        .expect("should succeed");
        assert_eq!(via_wrapper, via_from);
        assert_eq!(via_wrapper.max_attempts, 5);
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

    /// A policy already seeded at dispatch time (`engine-serve`'s
    /// `seed_resolved_policy`, EN.5.D task 7) is not clobbered by
    /// `SetupWorktreeNode`'s own resolution: the stamp is idempotent. Proven
    /// by seeding a `max_attempts` value the built-in default (3) would
    /// never produce and asserting it survives `process` untouched.
    #[tokio::test]
    async fn setup_worktree_node_does_not_clobber_a_policy_seeded_at_dispatch() {
        let node = SetupWorktreeNode::new().with_runner(stub_runner(0));
        let mut ctx = empty_context(json!({ "spec_slug": "my-spec" }));
        let seeded_policy = SdlcPolicy {
            max_attempts: 99,
            ..SdlcPolicy::default()
        };
        ctx.nodes.insert(
            RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(&seeded_policy).expect("SdlcPolicy serializes"),
        );

        let out = node.process(ctx).await.expect("setup should succeed");
        let policy = out
            .nodes
            .get(RESOLVED_POLICY_IDENTITY)
            .expect("resolved policy present in ctx after setup");
        assert_eq!(policy["max_attempts"], 99);
    }

    // --- SetupWorktreeNode + repo registry (EN.3.K) --------------------------

    /// A recording runner: `stub_runner` above discards its inputs, but
    /// these tests need to assert on the exact `cwd` every git invocation
    /// received.
    #[allow(clippy::type_complexity)]
    fn recording_runner(
        status: i32,
    ) -> (
        CommandRunner,
        Arc<Mutex<Vec<(String, Vec<String>, PathBuf)>>>,
    ) {
        let calls: Arc<Mutex<Vec<(String, Vec<String>, PathBuf)>>> =
            Arc::new(Mutex::new(Vec::new()));
        let calls_clone = calls.clone();
        let runner: CommandRunner = Arc::new(move |program, args, cwd| {
            calls_clone.lock().unwrap().push((
                program.to_string(),
                args.iter().map(|s| (*s).to_string()).collect(),
                cwd.to_path_buf(),
            ));
            Ok(CommandOutput {
                status,
                stdout: String::new(),
                stderr: if status == 0 {
                    String::new()
                } else {
                    "git failed".to_string()
                },
            })
        });
        (runner, calls)
    }

    /// Recorded calls minus `git rev-parse HEAD` — that invocation's cwd is
    /// `worktree_path` (item (g) in tasks.md's inventory, unaffected by this
    /// task: it already derives from `worktree_path` and follows
    /// automatically), and in the `use_worktree` case the stub runner never
    /// actually creates that directory on disk, so it cannot be
    /// canonicalized. These tests assert on the `worktree add` /
    /// `remove` / `checkout` cwds, which item (b)-(f) anchor to `root`.
    fn non_rev_parse_calls(
        calls: &[(String, Vec<String>, PathBuf)],
    ) -> Vec<&(String, Vec<String>, PathBuf)> {
        calls
            .iter()
            .filter(|(_, args, _)| args.first().map(String::as_str) != Some("rev-parse"))
            .collect()
    }

    /// A tempdir "brain root" with a `brain.toml` naming one repo (`alpha`),
    /// mirroring `repo_registry.rs`'s own `two_repo_brain` fixture but
    /// scoped to this module's tests.
    fn brain_root_with_alpha() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("alpha")).expect("mkdir alpha");
        std::fs::write(
            dir.path().join("brain.toml"),
            r#"
[[repos]]
slug = "alpha"
repo_path = "alpha"
"#,
        )
        .expect("write brain.toml");
        dir
    }

    #[tokio::test]
    async fn absent_repo_stamps_relative_worktree_path_and_dot_cwds() {
        // Byte-identical-to-today assertion: no `repo` field at all.
        let (runner, calls) = recording_runner(0);
        let node = SetupWorktreeNode::new().with_runner(runner);
        let ctx = empty_context(json!({ "spec_slug": "my-spec", "use_worktree": true }));

        let out = node.process(ctx).await.expect("setup should succeed");
        let result = out.nodes.get("SetupWorktreeNode").expect("output present");
        assert_eq!(result["worktree_path"], "trees/sdlc/my-spec");

        let recorded = calls.lock().unwrap();
        let checked = non_rev_parse_calls(&recorded);
        assert!(!checked.is_empty());
        for (_, _, cwd) in checked {
            assert_eq!(
                cwd,
                &PathBuf::from("."),
                "expected every git cwd to stay \".\""
            );
        }
    }

    #[tokio::test]
    async fn absent_repo_no_worktree_stamps_dot_and_dot_cwds() {
        let (runner, calls) = recording_runner(0);
        let node = SetupWorktreeNode::new().with_runner(runner);
        let ctx = empty_context(json!({ "spec_slug": "my-spec" }));

        let out = node.process(ctx).await.expect("setup should succeed");
        let result = out.nodes.get("SetupWorktreeNode").expect("output present");
        assert_eq!(result["worktree_path"], ".");

        let recorded = calls.lock().unwrap();
        assert!(!recorded.is_empty());
        for (_, _, cwd) in recorded.iter() {
            assert_eq!(cwd, &PathBuf::from("."));
        }
    }

    #[tokio::test]
    async fn known_repo_anchors_worktree_path_and_git_cwds_to_resolved_root() {
        let brain = brain_root_with_alpha();
        let registry =
            Arc::new(RepoRegistry::from_brain_root(brain.path()).expect("registry builds"));
        let alpha_root = registry.resolve("alpha").expect("alpha resolves");

        let (runner, calls) = recording_runner(0);
        let node = SetupWorktreeNode::new()
            .with_runner(runner)
            .with_registry(registry);
        let ctx =
            empty_context(json!({ "spec_slug": "my-spec", "use_worktree": true, "repo": "alpha" }));

        let out = node.process(ctx).await.expect("setup should succeed");
        let result = out.nodes.get("SetupWorktreeNode").expect("output present");
        let expected_worktree = alpha_root.join("trees").join("sdlc/my-spec");
        assert_eq!(
            result["worktree_path"],
            expected_worktree.to_string_lossy().to_string()
        );

        let recorded = calls.lock().unwrap();
        let checked = non_rev_parse_calls(&recorded);
        assert!(!checked.is_empty());
        for (_, _, cwd) in checked {
            assert_eq!(
                cwd.canonicalize().unwrap(),
                alpha_root.canonicalize().unwrap(),
                "expected every git cwd to be alpha's resolved root"
            );
        }
    }

    #[tokio::test]
    async fn known_repo_no_worktree_anchors_to_resolved_root() {
        let brain = brain_root_with_alpha();
        let registry =
            Arc::new(RepoRegistry::from_brain_root(brain.path()).expect("registry builds"));
        let alpha_root = registry.resolve("alpha").expect("alpha resolves");

        let (runner, calls) = recording_runner(0);
        let node = SetupWorktreeNode::new()
            .with_runner(runner)
            .with_registry(registry);
        let ctx = empty_context(json!({ "spec_slug": "my-spec", "repo": "alpha" }));

        let out = node.process(ctx).await.expect("setup should succeed");
        let result = out.nodes.get("SetupWorktreeNode").expect("output present");
        assert_eq!(
            result["worktree_path"],
            alpha_root.to_string_lossy().to_string()
        );

        let recorded = calls.lock().unwrap();
        assert!(!recorded.is_empty());
        for (_, _, cwd) in recorded.iter() {
            assert_eq!(
                cwd.canonicalize().unwrap(),
                alpha_root.canonicalize().unwrap()
            );
        }
    }

    #[tokio::test]
    async fn unknown_repo_slug_errors_and_runs_no_git_command() {
        let brain = brain_root_with_alpha();
        let registry =
            Arc::new(RepoRegistry::from_brain_root(brain.path()).expect("registry builds"));

        let (runner, calls) = recording_runner(0);
        let node = SetupWorktreeNode::new()
            .with_runner(runner)
            .with_registry(registry);
        let ctx = empty_context(
            json!({ "spec_slug": "my-spec", "use_worktree": true, "repo": "not-a-real-slug" }),
        );

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(
            err.message.contains("not-a-real-slug"),
            "error should name the offending slug: {}",
            err.message
        );
        assert!(
            calls.lock().unwrap().is_empty(),
            "no git command should have run"
        );
    }

    #[tokio::test]
    async fn repo_slug_with_no_registry_installed_errors_rather_than_falling_back_to_cwd() {
        let (runner, calls) = recording_runner(0);
        let node = SetupWorktreeNode::new().with_runner(runner);
        let ctx =
            empty_context(json!({ "spec_slug": "my-spec", "use_worktree": true, "repo": "alpha" }));

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("alpha"));
        assert!(calls.lock().unwrap().is_empty());
    }

    // --- SetupWorktreeNode + sibling path-dependency symlinks (ticket-worktree-sibling-path-deps) --

    /// A runner whose `git worktree add` arm actually creates the target
    /// directory on disk (`args[2]`, the worktree path) instead of a no-op —
    /// the sibling-symlink step needs a real worktree directory to compute
    /// `worktree_path_buf.join("..")` against and to assert the resulting
    /// symlinks live at the right place. Every other invocation (`checkout`,
    /// `remove`, `rev-parse`) is a harmless no-op success, matching
    /// `stub_runner`'s tolerance.
    fn worktree_creating_runner(status: i32) -> CommandRunner {
        Arc::new(move |_program, args, _cwd| {
            if args.first() == Some(&"worktree") && args.get(1) == Some(&"add") {
                if let Some(path) = args.get(2) {
                    std::fs::create_dir_all(path).expect("create worktree dir for test");
                }
            }
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

    /// A tempdir "brain root" naming one repo (`alpha`) via `brain.toml`,
    /// with real `claude-code-rs`/`mev`/`okf-core` sibling directories
    /// created one level above `alpha` — mirroring the real on-disk layout
    /// the workspace root `Cargo.toml`'s `../<crate>` path deps assume.
    fn brain_root_with_alpha_and_siblings() -> tempfile::TempDir {
        let brain = brain_root_with_alpha();
        for sibling in ["claude-code-rs", "mev", "okf-core"] {
            std::fs::create_dir_all(brain.path().join(sibling))
                .unwrap_or_else(|e| panic!("mkdir {sibling}: {e}"));
        }
        brain
    }

    fn assert_sibling_symlinks_resolve(worktree_path: &Path, brain_root: &Path) {
        for sibling in ["claude-code-rs", "mev", "okf-core"] {
            let link = worktree_path.join("..").join(sibling);
            assert!(
                link.exists(),
                "expected a symlink for {sibling} to exist at {}",
                link.display()
            );
            let resolved = std::fs::canonicalize(&link)
                .unwrap_or_else(|e| panic!("canonicalize {sibling} symlink: {e}"));
            let expected = std::fs::canonicalize(brain_root.join(sibling))
                .unwrap_or_else(|e| panic!("canonicalize real {sibling} dir: {e}"));
            assert_eq!(
                resolved, expected,
                "{sibling} symlink should resolve to the real sibling directory"
            );
        }
    }

    #[tokio::test]
    async fn setup_symlinks_sibling_path_dependencies_into_worktree() {
        let brain = brain_root_with_alpha_and_siblings();
        let registry =
            Arc::new(RepoRegistry::from_brain_root(brain.path()).expect("registry builds"));

        let node = SetupWorktreeNode::new()
            .with_runner(worktree_creating_runner(0))
            .with_registry(registry);
        let ctx =
            empty_context(json!({ "spec_slug": "my-spec", "use_worktree": true, "repo": "alpha" }));

        let out = node.process(ctx).await.expect("setup should succeed");
        let result = out.nodes.get("SetupWorktreeNode").expect("output present");
        let worktree_path = PathBuf::from(result["worktree_path"].as_str().unwrap());

        assert_sibling_symlinks_resolve(&worktree_path, brain.path());
    }

    #[tokio::test]
    async fn setup_sibling_symlink_step_is_idempotent_on_reattach() {
        let brain = brain_root_with_alpha_and_siblings();
        let registry =
            Arc::new(RepoRegistry::from_brain_root(brain.path()).expect("registry builds"));

        let node = SetupWorktreeNode::new()
            .with_runner(worktree_creating_runner(0))
            .with_registry(registry.clone());
        let first_ctx =
            empty_context(json!({ "spec_slug": "my-spec", "use_worktree": true, "repo": "alpha" }));
        let first_out = node
            .process(first_ctx)
            .await
            .expect("first setup should succeed");
        let worktree_path = PathBuf::from(
            first_out.nodes.get("SetupWorktreeNode").unwrap()["worktree_path"]
                .as_str()
                .unwrap(),
        );
        assert_sibling_symlinks_resolve(&worktree_path, brain.path());

        // Reattach: the worktree directory (and its symlinks) already exist
        // on disk from the first run; `resume: true` routes SetupWorktreeNode
        // past `git worktree add` entirely (`reattaching`), but the symlink
        // step still runs and must not error or disturb the existing links.
        let node = SetupWorktreeNode::new()
            .with_runner(worktree_creating_runner(0))
            .with_registry(registry);
        let second_ctx = empty_context(json!({
            "spec_slug": "my-spec",
            "use_worktree": true,
            "repo": "alpha",
            "resume": true,
        }));
        let second_out = node
            .process(second_ctx)
            .await
            .expect("reattach setup should succeed without error");
        let worktree_path_again = PathBuf::from(
            second_out.nodes.get("SetupWorktreeNode").unwrap()["worktree_path"]
                .as_str()
                .unwrap(),
        );
        assert_eq!(worktree_path_again, worktree_path);
        assert_sibling_symlinks_resolve(&worktree_path_again, brain.path());
    }

    #[tokio::test]
    async fn setup_planning_symlink_remains_correct_alongside_sibling_symlinks() {
        // Regression guard: the pre-existing `planning` symlink block
        // (setup.rs:306-318) must keep working unchanged now that the
        // sibling-crate symlink step (added immediately after it) is in
        // place. No prior test in this module exercised the planning
        // symlink's on-disk effect directly, so this closes that gap.
        let brain = brain_root_with_alpha_and_siblings();
        let registry =
            Arc::new(RepoRegistry::from_brain_root(brain.path()).expect("registry builds"));
        let alpha_root = registry.resolve("alpha").expect("alpha resolves");
        let real_planning = alpha_root.join("planning");
        std::fs::create_dir_all(&real_planning).expect("mkdir planning");
        std::fs::write(real_planning.join("marker.txt"), "hello").expect("write marker");

        let node = SetupWorktreeNode::new()
            .with_runner(worktree_creating_runner(0))
            .with_registry(registry);
        let ctx =
            empty_context(json!({ "spec_slug": "my-spec", "use_worktree": true, "repo": "alpha" }));

        let out = node.process(ctx).await.expect("setup should succeed");
        let result = out.nodes.get("SetupWorktreeNode").expect("output present");
        let worktree_path = PathBuf::from(result["worktree_path"].as_str().unwrap());

        let worktree_planning = worktree_path.join("planning");
        assert!(
            worktree_planning.exists(),
            "expected the planning symlink to exist in the worktree"
        );
        let resolved =
            std::fs::canonicalize(&worktree_planning).expect("planning symlink should resolve");
        let expected =
            std::fs::canonicalize(&real_planning).expect("real planning dir canonicalizes");
        assert_eq!(
            resolved, expected,
            "planning symlink should still resolve to the real planning directory"
        );
        assert_eq!(
            std::fs::read_to_string(worktree_planning.join("marker.txt")).unwrap(),
            "hello",
            "the marker file written through the real planning dir must be visible via the symlink"
        );

        // The sibling-crate symlinks must also be present alongside the
        // unaffected planning symlink.
        assert_sibling_symlinks_resolve(&worktree_path, brain.path());
    }

    #[tokio::test]
    async fn setup_sibling_symlink_step_is_skipped_when_use_worktree_false() {
        let brain = brain_root_with_alpha_and_siblings();
        let registry =
            Arc::new(RepoRegistry::from_brain_root(brain.path()).expect("registry builds"));
        let alpha_root = registry.resolve("alpha").expect("alpha resolves");

        let node = SetupWorktreeNode::new()
            .with_runner(worktree_creating_runner(0))
            .with_registry(registry);
        // No `use_worktree` -> the non-worktree checkout path (setup.rs:319-354).
        let ctx = empty_context(json!({ "spec_slug": "my-spec", "repo": "alpha" }));

        let out = node.process(ctx).await.expect("setup should succeed");
        let result = out.nodes.get("SetupWorktreeNode").expect("output present");
        assert_eq!(
            result["worktree_path"],
            alpha_root.to_string_lossy().to_string()
        );

        for sibling in ["claude-code-rs", "mev", "okf-core"] {
            assert!(
                !alpha_root.join(sibling).exists(),
                "no sibling symlink should be created relative to the checkout root when \
                 use_worktree is false"
            );
        }
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

    /// EN.ticket.sdlc-flow-dead-policy-knobs task 1, AC1: a non-default
    /// policy `max_attempts` seeds a model-generated task that does not
    /// declare its own value.
    #[tokio::test]
    async fn generate_seeds_max_attempts_from_policy_when_model_omits_it() {
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
        let policy = SdlcPolicy {
            max_attempts: 9,
            ..SdlcPolicy::default()
        };
        let ctx = ctx_with_worktree_and_policy("my-spec", &worktree, &policy);

        node.process(ctx).await.expect("generate should succeed");

        let dir = worktree.join("planning").join("my-spec");
        let tasks: Vec<serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string(dir.join("tasks.json")).unwrap())
                .unwrap();
        assert_eq!(tasks[0]["max_attempts"], 9);
    }

    /// `EN.ticket.sdlc-flow-dead-policy-knobs` task 3: a non-default
    /// `transport_retry` on the resolved policy changes the observed
    /// attempt count against a persistently failing transport for
    /// `GenerateTasksNode`.
    #[tokio::test]
    async fn generate_transport_retry_nondefault_changes_observed_attempts() {
        let worktree = temp_dir();
        std::fs::create_dir_all(worktree.join("planning").join("my-spec")).unwrap();

        let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let transport: ModelTransport = Arc::new({
            let calls = calls.clone();
            move |_config, _prompt| {
                let calls = calls.clone();
                Box::pin(async move {
                    calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Err(claude_code_rs::Error::Timeout)
                })
            }
        });

        let node = GenerateTasksNode::new().with_transport(transport);
        let policy = SdlcPolicy {
            transport_retry: crate::workflows::sdlc_flow::policy::TransportRetry {
                max_attempts: 4,
                initial_backoff_ms: 0,
            },
            ..SdlcPolicy::default()
        };
        let ctx = ctx_with_worktree_and_policy("my-spec", &worktree, &policy);

        let result = node.process(ctx).await;
        assert!(
            result.is_err(),
            "persistent failure must still halt the walk"
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 4);
    }

    /// AC2: a model-generated task that declares its own `max_attempts`
    /// keeps it, even when the policy sets a different value.
    #[tokio::test]
    async fn generate_does_not_override_a_model_declared_max_attempts() {
        let worktree = temp_dir();
        std::fs::create_dir_all(worktree.join("planning").join("my-spec")).unwrap();

        let canned = json!({
            "tasks": [{ "task_id": 1, "title": "Do it", "description": "desc", "max_attempts": 1 }],
            "tasks_markdown": "# Tasks\n\n1. Do it",
        })
        .to_string();

        let transport: ModelTransport = Arc::new(move |_config, _prompt| {
            let outcome = stub_outcome_with_text(&canned);
            Box::pin(async move { Ok(outcome) })
        });

        let node = GenerateTasksNode::new().with_transport(transport);
        let policy = SdlcPolicy {
            max_attempts: 9,
            ..SdlcPolicy::default()
        };
        let ctx = ctx_with_worktree_and_policy("my-spec", &worktree, &policy);

        node.process(ctx).await.expect("generate should succeed");

        let dir = worktree.join("planning").join("my-spec");
        let tasks: Vec<serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string(dir.join("tasks.json")).unwrap())
                .unwrap();
        assert_eq!(tasks[0]["max_attempts"], 1);
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

    // --- GenerateTasksNode policy/cwd contract -----------------------------

    /// A transport that records the `Config` and prompt it was handed, then
    /// replies with a valid `GeneratedTasks` payload.
    #[allow(clippy::type_complexity)]
    fn capturing_transport(
        captured: Arc<Mutex<Option<(Config, String)>>>,
    ) -> (ModelTransport, PathBuf) {
        let worktree = temp_dir();
        std::fs::create_dir_all(worktree.join("planning").join("my-spec")).unwrap();
        let transport: ModelTransport = Arc::new(move |config, prompt| {
            *captured.lock().unwrap() = Some((config.clone(), prompt.clone()));
            let outcome = stub_outcome_with_text(
                &json!({
                    "tasks": [{ "task_id": 1, "title": "Do it", "description": "desc" }],
                    "tasks_markdown": "# Tasks\n\n1. Do it",
                })
                .to_string(),
            );
            Box::pin(async move { Ok(outcome) })
        });
        (transport, worktree)
    }

    #[tokio::test]
    async fn generate_uses_baseline_tier_model_matching_the_former_hardcode() {
        let captured = Arc::new(Mutex::new(None));
        let (transport, worktree) = capturing_transport(Arc::clone(&captured));

        let node = GenerateTasksNode::new().with_transport(transport);
        let ctx = ctx_with_worktree("my-spec", &worktree);
        node.process(ctx).await.expect("generate should succeed");

        let (config, _prompt) = captured.lock().unwrap().clone().expect("transport called");
        // Behavior stability: the built-in default `model_tiers.generate`
        // must reproduce the `claude-opus-4-8` literal this node used to
        // hardcode in `new()`.
        assert_eq!(config.model.as_deref(), Some("claude-opus-4-8"));
    }

    #[tokio::test]
    async fn generate_applies_the_resolved_tier_and_timeout_and_stamps_them() {
        let captured = Arc::new(Mutex::new(None));
        let (transport, worktree) = capturing_transport(Arc::clone(&captured));

        let policy = SdlcPolicy {
            model_tiers: ModelTiers {
                generate: ModelTier::Haiku,
                ..SdlcPolicy::default().model_tiers
            },
            timeouts: CallTimeouts {
                generate: Some(42),
                ..SdlcPolicy::default().timeouts
            },
            ..SdlcPolicy::default()
        };

        let node = GenerateTasksNode::new().with_transport(transport);
        let ctx = ctx_with_worktree_and_policy("my-spec", &worktree, &policy);
        let out = node.process(ctx).await.expect("generate should succeed");

        let (config, _prompt) = captured.lock().unwrap().clone().expect("transport called");
        assert_eq!(config.model.as_deref(), Some("claude-haiku-4-5"));
        assert_eq!(config.timeout, Some(std::time::Duration::from_secs(42)));

        let result = out.nodes.get("GenerateTasksNode").expect("output present");
        assert_eq!(result["model_tier"], json!("haiku"));
        assert_eq!(result["call_timeout_secs"], json!(42));
    }

    #[tokio::test]
    async fn generate_scopes_cwd_to_the_stamped_worktree() {
        let captured = Arc::new(Mutex::new(None));
        let (transport, worktree) = capturing_transport(Arc::clone(&captured));

        let node = GenerateTasksNode::new().with_transport(transport);
        let ctx = ctx_with_worktree("my-spec", &worktree);
        node.process(ctx).await.expect("generate should succeed");

        let (config, _prompt) = captured.lock().unwrap().clone().expect("transport called");
        assert_eq!(config.cwd.as_deref(), Some(worktree.as_path()));
    }

    #[tokio::test]
    async fn generate_hard_errors_when_the_resolved_policy_stamp_is_absent() {
        let worktree = temp_dir();
        std::fs::create_dir_all(worktree.join("planning").join("my-spec")).unwrap();

        let transport: ModelTransport = Arc::new(move |_config, _prompt| {
            let outcome = stub_outcome_with_text("{}");
            Box::pin(async move { Ok(outcome) })
        });

        // No `RESOLVED_POLICY_IDENTITY` stamp — strict read must fail rather
        // than silently defaulting.
        let mut ctx = empty_context(json!({ "spec_slug": "my-spec" }));
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );

        let node = GenerateTasksNode::new().with_transport(transport);
        node.process(ctx)
            .await
            .expect_err("absent policy stamp must be an error");
    }

    // --- resolve_target_root / target_root_is_default (EN.3.K task 2) ------

    fn two_repo_registry() -> (tempfile::TempDir, crate::repo_registry::RepoRegistry) {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("alpha")).expect("mkdir alpha");
        std::fs::create_dir_all(dir.path().join("beta")).expect("mkdir beta");
        std::fs::write(
            dir.path().join("brain.toml"),
            r#"
[[repos]]
slug = "alpha"
repo_path = "alpha"

[[repos]]
slug = "beta"
repo_path = "beta"
"#,
        )
        .expect("write brain.toml");
        let registry =
            crate::repo_registry::RepoRegistry::from_brain_root(dir.path()).expect("registry");
        (dir, registry)
    }

    #[test]
    fn resolve_target_root_with_no_repo_returns_current_dir() {
        let event: SDLCFlowEventSchema =
            serde_json::from_value(json!({ "spec_slug": "my-spec" })).unwrap();
        assert!(target_root_is_default(&event));

        let resolved = resolve_target_root(&event, None).expect("should resolve to current_dir");
        assert_eq!(resolved, std::env::current_dir().unwrap());
    }

    #[test]
    fn resolve_target_root_with_no_repo_ignores_a_present_registry() {
        // Absent `repo` must resolve to current_dir() regardless of whether a
        // registry happens to be available — the default path never
        // consults the registry.
        let (_dir, registry) = two_repo_registry();
        let event: SDLCFlowEventSchema =
            serde_json::from_value(json!({ "spec_slug": "my-spec" })).unwrap();

        let resolved =
            resolve_target_root(&event, Some(&registry)).expect("should resolve to current_dir");
        assert_eq!(resolved, std::env::current_dir().unwrap());
    }

    #[test]
    fn resolve_target_root_with_known_repo_resolves_via_registry() {
        let (dir, registry) = two_repo_registry();
        let event: SDLCFlowEventSchema =
            serde_json::from_value(json!({ "spec_slug": "my-spec", "repo": "alpha" })).unwrap();
        assert!(!target_root_is_default(&event));

        let resolved = resolve_target_root(&event, Some(&registry)).expect("alpha should resolve");
        assert_eq!(
            resolved.canonicalize().unwrap(),
            dir.path().join("alpha").canonicalize().unwrap()
        );
    }

    #[test]
    fn resolve_target_root_with_unknown_repo_names_the_slug() {
        let (_dir, registry) = two_repo_registry();
        let event: SDLCFlowEventSchema =
            serde_json::from_value(json!({ "spec_slug": "my-spec", "repo": "not-a-repo" }))
                .unwrap();

        let err = resolve_target_root(&event, Some(&registry)).expect_err("should fail");
        assert!(err.message.contains("not-a-repo"));
    }

    #[test]
    fn resolve_target_root_with_repo_and_no_registry_errors_rather_than_falling_back() {
        // A run must never quietly retarget cwd when a `repo` slug was
        // named but no registry is available to resolve it.
        let event: SDLCFlowEventSchema =
            serde_json::from_value(json!({ "spec_slug": "my-spec", "repo": "alpha" })).unwrap();

        let err = resolve_target_root(&event, None).expect_err("should fail, not fall back");
        assert!(err.message.contains("alpha"));
    }
}
