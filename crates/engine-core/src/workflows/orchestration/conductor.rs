//! `CONDUCTOR` — `EN.12.F`.
//!
//! The run picks tonight's chain from the operator's weekly objective and mev's
//! computed frontier slate, then justifies the choice in the journal. Per Fork 3
//! (2026-08-19): the objective is operator-written; mev's computed slate is the
//! candidate set the conductor may draw from — it is a planner, never a backlog
//! drain, because it can only pick and justify from a set that already exists.
//!
//! **Task 1** lands the two inputs: reading the objective file
//! ([`read_objective`]) and obtaining mev's computed slate via `mev frontier
//! --json` ([`fetch_frontier_slate`]), shelled out through the SAME
//! `CommandRunner` convention [`crate::policy::emit_state::EmitStateNode`]
//! already uses for `mev emit-state --write` — the injectable
//! [`crate::policy::emit_state::Runner`] seam, so no test in this module (or
//! anywhere downstream of it) ever shells out to a real `mev` binary.
//!
//! **Task 2** adds [`propose_chain`]: the subset-only constraint and its two
//! refusal cases (a candidate outside the slate, and a slate candidate with
//! no `tasks.json`).
//!
//! **Task 3** adds the `git log -S` pre-flight ([`git_log_dash_s_preflight`]),
//! wired into [`propose_chain`] as the last step before the chain is
//! finalised. Unlike the two refusals above, a pre-flight match does not
//! reject the whole proposal — it DROPS just that candidate and records why
//! on [`DroppedCandidate`] (surfaced via [`ProposalOutcome::dropped`]) for a
//! later task to write to the EN.12.D journal; a pre-flight that cannot even
//! run (spawn failure, non-zero exit) IS a hard refusal
//! ([`ConductorProposalError::GitPreflightFailed`]), since an inconclusive
//! check must never be treated as "history is clean". The actual journal
//! write is a later task's job — this module only produces the record.
//!
//! # `--agent` quiesce hazard (carry forward)
//!
//! `mev`'s WRITE verbs are refused under a live exclusive quiesce lease unless
//! `--agent` is passed (CONFIRMED by execution 2026-09-03 — see carryover
//! `sdlc-engines-pass-no-agent-so-a-lane-may-be-quiescing-its-own-emit`).
//! `mev frontier --json` is a READ verb and is exempt from this — nothing in
//! [`fetch_frontier_slate`] passes `--agent`. **If the conductor ever grows a
//! call to a WRITE verb** (e.g. filing a block through MV.14.B), that call
//! MUST pass `--agent` or it will be refused the exact same way a same-lane
//! `emit-state --write` was.

use std::path::{Path, PathBuf};

use crate::policy::emit_state::{CommandOutputLike, Runner};
use crate::workflows::orchestration::gates::FrontierArtifact;

/// Default path to the operator-written weekly objective file. Resolved
/// relative to [`ConductorConfig::mev_cwd`] (the brain/HQ root) — this is
/// **not** engine-rs's own `planning/`, which is a vaulted symlink into a
/// different tree entirely; the real artifact is `agentic-portfolio/planning/
/// objective.md` at HQ. Overridable via [`ConductorConfig::with_objective_path`]
/// so a test can point at a fixture instead of the real HQ artifact, per
/// standing rule 6 (nodes are configurable, not hardcoded) — adding this
/// knob does not change existing behavior since nothing reads it yet.
pub const DEFAULT_OBJECTIVE_PATH: &str = "planning/objective.md";

/// Everything that can go wrong obtaining the conductor's two inputs. Every
/// variant names what was being read/run — mirrors [`super::gates::FrontierError`]'s
/// own discipline of never failing silently or falling back to a default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConductorInputError {
    /// No objective file at the configured path. Per the block's AC ("With no
    /// objective file present the conductor refuses to propose anything") a
    /// later task turns this into a refusal, never an inferred goal.
    ObjectiveMissing { path: PathBuf },
    /// The objective file exists but could not be read (permissions, not
    /// valid UTF-8, etc).
    ObjectiveUnreadable { path: PathBuf, reason: String },
    /// `mev frontier --json` failed to spawn at all (binary not on `PATH`,
    /// runner I/O error).
    FrontierSpawnFailed { reason: String },
    /// `mev frontier --json` spawned but exited non-zero.
    FrontierCommandFailed { status: i32, stderr: String },
    /// `mev frontier --json` exited `0` but stdout did not parse as a
    /// [`FrontierArtifact`] (`{derived_at, entries[], gate_ranks[]}`).
    FrontierUnparsable { reason: String },
}

impl std::fmt::Display for ConductorInputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConductorInputError::ObjectiveMissing { path } => write!(
                f,
                "conductor: no weekly objective file at '{}' — the conductor \
                 never falls back to an inferred goal",
                path.display()
            ),
            ConductorInputError::ObjectiveUnreadable { path, reason } => write!(
                f,
                "conductor: objective file '{}' could not be read: {reason}",
                path.display()
            ),
            ConductorInputError::FrontierSpawnFailed { reason } => {
                write!(
                    f,
                    "conductor: `mev frontier --json` failed to spawn: {reason}"
                )
            }
            ConductorInputError::FrontierCommandFailed { status, stderr } => write!(
                f,
                "conductor: `mev frontier --json` exited {status}: {stderr}"
            ),
            ConductorInputError::FrontierUnparsable { reason } => write!(
                f,
                "conductor: `mev frontier --json` output did not parse as \
                 {{derived_at, entries[], gate_ranks[]}}: {reason}"
            ),
        }
    }
}

impl std::error::Error for ConductorInputError {}

/// Configuration for [`read_objective`] and [`fetch_frontier_slate`]: the
/// objective file path and the working directory `mev` is invoked in. Each
/// defaults to built-in, behavior-stable values (standing rule 6) and is
/// overridable so a test can point at a fixture rather than the real HQ
/// artifact or a real `mev` binary.
#[derive(Debug, Clone)]
pub struct ConductorConfig {
    objective_path: PathBuf,
    mev_cwd: PathBuf,
    git_cwd: Option<PathBuf>,
}

impl Default for ConductorConfig {
    fn default() -> Self {
        Self {
            objective_path: PathBuf::from(DEFAULT_OBJECTIVE_PATH),
            mev_cwd: PathBuf::from("."),
            git_cwd: None,
        }
    }
}

impl ConductorConfig {
    /// A config with the built-in defaults: [`DEFAULT_OBJECTIVE_PATH`] and
    /// `mev` invoked in the current working directory.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Override the objective file path (e.g. to a fixture in a test).
    #[must_use]
    pub fn with_objective_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.objective_path = path.into();
        self
    }

    /// Override the working directory `mev frontier --json` runs in.
    #[must_use]
    pub fn with_mev_cwd(mut self, path: impl Into<PathBuf>) -> Self {
        self.mev_cwd = path.into();
        self
    }

    /// Override the working directory the `git log -S` pre-flight runs in.
    /// Defaults to the same directory as `mev_cwd` (the brain/HQ root) — see
    /// [`Self::git_cwd`].
    #[must_use]
    pub fn with_git_cwd(mut self, path: impl Into<PathBuf>) -> Self {
        self.git_cwd = Some(path.into());
        self
    }

    #[must_use]
    pub fn objective_path(&self) -> &Path {
        &self.objective_path
    }

    #[must_use]
    pub fn mev_cwd(&self) -> &Path {
        &self.mev_cwd
    }

    /// The working directory the `git log -S` pre-flight runs in — the
    /// explicit override if one was given via [`Self::with_git_cwd`],
    /// otherwise the same directory as [`Self::mev_cwd`].
    #[must_use]
    pub fn git_cwd(&self) -> &Path {
        self.git_cwd.as_deref().unwrap_or(&self.mev_cwd)
    }
}

/// Read the operator-written weekly objective at `config.objective_path()`.
///
/// Never falls back to an inferred goal: a missing file is
/// [`ConductorInputError::ObjectiveMissing`], not an empty or default string —
/// the conductor reads this file, it never writes it (that is the
/// `operator-first-weekly-objective` gate's job).
pub fn read_objective(config: &ConductorConfig) -> Result<String, ConductorInputError> {
    let path = config.objective_path();
    if !path.exists() {
        return Err(ConductorInputError::ObjectiveMissing {
            path: path.to_path_buf(),
        });
    }
    std::fs::read_to_string(path).map_err(|err| ConductorInputError::ObjectiveUnreadable {
        path: path.to_path_buf(),
        reason: err.to_string(),
    })
}

/// Obtain mev's computed candidate slate via `mev frontier --json`, shelled
/// out through the injected [`Runner`] — the SAME `CommandRunner` convention
/// [`crate::policy::emit_state::EmitStateNode`] already uses for `mev
/// emit-state --write`. `frontier` is a READ verb and is exempt from the
/// `--agent` quiesce rule (see module doc); this call never passes it.
///
/// Parses stdout as a [`FrontierArtifact`] — the exact `{derived_at,
/// entries[], gate_ranks[]}` shape `mev frontier --json` produces (mirrored,
/// not re-derived, by [`super::gates::FrontierArtifact`]) — so a later task's
/// subset check reads the real slate shape, not a parallel one invented here.
pub fn fetch_frontier_slate<O: CommandOutputLike>(
    runner: &Runner<O>,
    config: &ConductorConfig,
) -> Result<FrontierArtifact, ConductorInputError> {
    let output = runner("mev", &["frontier", "--json"], config.mev_cwd()).map_err(|err| {
        ConductorInputError::FrontierSpawnFailed {
            reason: err.to_string(),
        }
    })?;

    if output.status() != 0 {
        return Err(ConductorInputError::FrontierCommandFailed {
            status: output.status(),
            stderr: output.stderr().to_string(),
        });
    }

    serde_json::from_str(output.stdout()).map_err(|err| ConductorInputError::FrontierUnparsable {
        reason: err.to_string(),
    })
}

// ── Task 2: subset-only proposals, and the refusal cases ───────────────────
//
// The conductor proposes an ordered chain that is a SUBSET of the slate `mev
// frontier --json` returned for this run. It can never invent a block: a
// candidate id that is not in the slate is refused wholesale (never silently
// trimmed) via [`ConductorProposalError::NotInSlate`] — this single check
// covers BOTH the "proposed a real corpus block that just isn't in tonight's
// slate" case and the "proposed a block id that does not exist in the corpus
// at all" (invented) case, because the slate is the only view of the corpus
// this module is allowed to hold (per the block's `out_of_scope`: filing or
// re-deriving corpus membership is `MV.14.B`'s and `mev frontier`'s job, not
// this module's). A separately-invented id is, from here, indistinguishable
// from a real id that simply isn't in tonight's slate, and both are refused
// identically.
//
// A candidate that IS in the slate but has no `tasks.json` yet is refused
// for dispatch (never silently dispatched into a run that can only fail)
// via [`ConductorProposalError::MissingTasksJson`], naming `/generate-tasks`
// in its diagnostic. Per carryover `gate-scope-must-be-shown-capable-of-
// failing`, [`propose_chain`]'s subset check takes the slate as an
// independent input — never one derived from the proposal itself — so the
// check can actually fail on a real input instead of only ever passing.

/// Whether `tasks.json` exists for `(repo, block_id)`. Injected exactly like
/// [`Runner`] (`crate::policy::emit_state`'s `CommandRunner` convention) so no
/// test in this module touches a real filesystem or another repo's
/// `planning/` tree — a fake in a test can assert "these ids have a
/// `tasks.json`, these do not" without caring how a real implementation would
/// locate one across repos.
pub type TasksJsonChecker = std::sync::Arc<dyn Fn(&str, &str) -> bool + Send + Sync>;

/// Everything that refuses a conductor proposal outright. Every variant names
/// what was refused and why — mirrors [`ConductorInputError`]'s own
/// discipline of never failing silently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConductorProposalError {
    /// No objective, so the conductor refuses to propose anything at all —
    /// it never falls back to an inferred goal. Wraps [`ConductorInputError`]
    /// so the underlying reason (missing vs. unreadable) is preserved.
    Objective(ConductorInputError),
    /// `mev frontier --json` itself failed (spawn, non-zero exit, or
    /// unparsable output) — see [`fetch_frontier_slate`]. Distinct from
    /// [`Self::Objective`] because a missing objective and a broken slate
    /// fetch are different operator-facing problems (fix the objective file
    /// vs. fix `mev`), even though both are hard refusals here. Only
    /// produced by [`propose_from_frontier`], which is the only caller in
    /// this module that fetches the slate itself rather than taking it as
    /// an argument.
    Frontier(ConductorInputError),
    /// `repo:block_id` is not present in the slate this run's `mev frontier
    /// --json` returned. Covers both a real corpus block that just isn't in
    /// tonight's slate and an outright invented id — see the module note
    /// above for why the two are indistinguishable from here. The WHOLE
    /// proposal is rejected; nothing is silently trimmed.
    NotInSlate { repo: String, block_id: String },
    /// `repo:block_id` is in the slate but has no `tasks.json` yet, so
    /// dispatching it would only fail. Refused rather than dispatched.
    MissingTasksJson { repo: String, block_id: String },
    /// The `git log -S` pre-flight for `repo:block_id` failed to spawn or
    /// exited non-zero. Unlike a matched pre-flight (which drops the
    /// candidate and journals why — see [`DroppedCandidate`]), a pre-flight
    /// that cannot even run is treated as a hard refusal of the whole
    /// proposal: the conductor must not silently proceed as if the
    /// candidate's history were clean when it never actually checked.
    GitPreflightFailed {
        repo: String,
        block_id: String,
        reason: String,
    },
}

impl std::fmt::Display for ConductorProposalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConductorProposalError::Objective(err) => write!(f, "{err}"),
            ConductorProposalError::Frontier(err) => write!(f, "{err}"),
            ConductorProposalError::NotInSlate { repo, block_id } => write!(
                f,
                "conductor: proposed block '{repo}:{block_id}' is not in tonight's \
                 `mev frontier --json` slate — the conductor never proposes a block \
                 outside the computed slate, whether that block exists elsewhere in \
                 the corpus or was invented outright; the whole proposal is rejected"
            ),
            ConductorProposalError::MissingTasksJson { repo, block_id } => write!(
                f,
                "conductor: proposed block '{repo}:{block_id}' has no `tasks.json` \
                 yet — run `/generate-tasks` for it before it can be dispatched"
            ),
            ConductorProposalError::GitPreflightFailed {
                repo,
                block_id,
                reason,
            } => write!(
                f,
                "conductor: `git log -S` pre-flight for '{repo}:{block_id}' \
                 could not run: {reason} — refusing rather than proceeding \
                 as if history were clean"
            ),
        }
    }
}

impl std::error::Error for ConductorProposalError {}

// ── Task 3: the `git log -S` pre-flight ─────────────────────────────────────
//
// The corpus graph lags reality by days — the block record's own evidence:
// two blocks were fully implemented and merged while `state.json` still read
// `open`, because their bookkeeping emits were silently refused by the
// lane's own quiesce lease. A conductor that trusted the graph (via the
// slate) alone could propose work that is already done. The `git log -S`
// pre-flight runs BEFORE the proposal is finalised (inside [`propose_chain`],
// after the subset/tasks.json checks but before the chain is built) and
// drops any candidate whose work is already present in git history, with the
// reason recorded on [`DroppedCandidate`] for [`propose_chain`]'s caller to
// journal (the actual EN.12.D journal write is a later task's job — this one
// only produces the record to write).

/// A candidate the `git log -S` pre-flight dropped from a proposal, and why —
/// the record [`propose_chain`]'s caller journals to EN.12.D so a dropped
/// candidate is auditable, not silently vanished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DroppedCandidate {
    pub repo: String,
    pub block_id: String,
    pub reason: String,
}

/// `(survivors, dropped)` — [`git_log_dash_s_preflight`]'s success shape.
/// Named to keep the function signature under clippy's type-complexity
/// threshold, not because it means anything beyond the tuple it wraps.
pub type PreflightOutcome = (Vec<(String, String)>, Vec<DroppedCandidate>);

/// Run `git log -S<block_id> --oneline` for each of `candidates` in
/// `config.git_cwd()`, via the SAME injected [`Runner`] convention as
/// [`fetch_frontier_slate`] — no test here ever shells out to real `git`.
///
/// `-S<block_id>` (git's "pickaxe" search) finds commits that changed the
/// number of occurrences of the literal string `block_id` in the tracked
/// tree — i.e. commits that added or removed code/docs referencing this
/// block id, which is what a completed implementation looks like. A
/// candidate with at least one such commit is dropped as already-done; a
/// candidate with none survives untouched, in its original order.
///
/// Returns `(survivors, dropped)` on success. A spawn failure or non-zero
/// exit for any candidate is a hard refusal
/// ([`ConductorProposalError::GitPreflightFailed`]) — an inconclusive
/// pre-flight must never be treated as "history is clean".
pub fn git_log_dash_s_preflight<O: CommandOutputLike>(
    runner: &Runner<O>,
    config: &ConductorConfig,
    candidates: &[(String, String)],
) -> Result<PreflightOutcome, ConductorProposalError> {
    let mut survivors = Vec::with_capacity(candidates.len());
    let mut dropped = Vec::new();

    for (repo, block_id) in candidates {
        let pickaxe_arg = format!("-S{block_id}");
        let output = runner(
            "git",
            &["log", pickaxe_arg.as_str(), "--oneline"],
            config.git_cwd(),
        )
        .map_err(|err| ConductorProposalError::GitPreflightFailed {
            repo: repo.clone(),
            block_id: block_id.clone(),
            reason: err.to_string(),
        })?;

        if output.status() != 0 {
            return Err(ConductorProposalError::GitPreflightFailed {
                repo: repo.clone(),
                block_id: block_id.clone(),
                reason: format!(
                    "`git log -S` exited {}: {}",
                    output.status(),
                    output.stderr()
                ),
            });
        }

        let matched_commits = output
            .stdout()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count();
        if matched_commits > 0 {
            dropped.push(DroppedCandidate {
                repo: repo.clone(),
                block_id: block_id.clone(),
                reason: format!(
                    "conductor: '{repo}:{block_id}' dropped by the `git log -S` \
                     pre-flight — {matched_commits} commit(s) already touch this \
                     block id in git history, so the corpus graph is stale for it"
                ),
            });
        } else {
            survivors.push((repo.clone(), block_id.clone()));
        }
    }

    Ok((survivors, dropped))
}

/// The outcome of a successful [`propose_chain`] call: the finalised chain
/// (already subset-checked, tasks.json-checked, and `git log -S`-checked),
/// plus every candidate the `git log -S` pre-flight dropped along the way,
/// for the caller to journal to EN.12.D.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalOutcome {
    pub chain: Vec<crate::workflows::orchestration::chain::ChainStep>,
    pub dropped: Vec<DroppedCandidate>,
}

/// Validate `proposed` (an ordered, caller-supplied `(repo, block_id)` list —
/// the conductor's candidate picks, in the order it wants them run) against
/// `slate` and `tasks_json_exists`, and turn it into a real chain the same
/// shape [`super::chain::resolve_explicit_chain`] produces, so nothing
/// downstream of this function can tell a conductor-produced chain from an
/// authored one.
///
/// Refuses, in order:
/// 1. No objective file at `config.objective_path()` — see
///    [`ConductorProposalError::Objective`].
/// 2. Any proposed block outside `slate` — see
///    [`ConductorProposalError::NotInSlate`]. The WHOLE proposal is rejected
///    on the first offending entry; nothing is silently trimmed.
/// 3. Any proposed block with no `tasks.json` — see
///    [`ConductorProposalError::MissingTasksJson`].
///
/// 4. The `git log -S` pre-flight ([`git_log_dash_s_preflight`]) — a
///    candidate already present in git history is DROPPED (not refused —
///    the rest of the proposal still goes ahead) with the reason recorded on
///    [`ProposalOutcome::dropped`] for the caller to journal.
///
/// `slate` is an independent input (typically [`fetch_frontier_slate`]'s
/// result), never derived from `proposed` — per carryover
/// `gate-scope-must-be-shown-capable-of-failing`, a subset check whose slate
/// is derived from the proposal itself can never fail.
pub fn propose_chain<O: CommandOutputLike>(
    config: &ConductorConfig,
    proposed: &[(String, String)],
    slate: &FrontierArtifact,
    tasks_json_exists: &TasksJsonChecker,
    git_runner: &Runner<O>,
) -> Result<ProposalOutcome, ConductorProposalError> {
    read_objective(config).map_err(ConductorProposalError::Objective)?;

    let slate_keys: std::collections::HashSet<&str> = slate
        .entries
        .iter()
        .map(|entry| entry.key.as_str())
        .collect();

    for (repo, block_id) in proposed {
        let key = format!("{repo}:{block_id}");
        if !slate_keys.contains(key.as_str()) {
            return Err(ConductorProposalError::NotInSlate {
                repo: repo.clone(),
                block_id: block_id.clone(),
            });
        }
    }

    for (repo, block_id) in proposed {
        if !tasks_json_exists(repo, block_id) {
            return Err(ConductorProposalError::MissingTasksJson {
                repo: repo.clone(),
                block_id: block_id.clone(),
            });
        }
    }

    // The pre-flight runs BEFORE the proposal is finalised (this is the
    // last step, right before the chain is built from whatever survives it).
    let (survivors, dropped) = git_log_dash_s_preflight(git_runner, config, proposed)?;

    Ok(ProposalOutcome {
        chain: crate::workflows::orchestration::chain::resolve_explicit_chain(survivors),
        dropped,
    })
}

/// End-to-end conductor entry point for the production `EN.12.F` seam
/// ([`crate::workflows::orchestration::graph::ConductorSeamFn`]): fetches
/// tonight's `mev frontier --json` slate via `runner`, proposes the WHOLE
/// slate as the candidate list (every `(repo, block_id)` the slate
/// contains, in slate order), and validates it through [`propose_chain`]
/// exactly as an explicit, caller-supplied proposal would be.
///
/// The conductor itself never narrows the slate — narrowing (single-repo,
/// a max chain length) is [`super::graph::apply_conductor_caps`]'s job,
/// applied by the node AFTER this returns, per standing rule 6 (a knob
/// trims the outcome; it does not change which candidates this function
/// considers). Proposing the full slate here means [`propose_chain`]'s own
/// subset check is trivially satisfied for every candidate — it still runs,
/// so a future caller that narrows the candidate list before calling this
/// keeps the same protection.
///
/// The one input [`propose_chain`] cannot supply on its own is the slate
/// fetch: a spawn failure, non-zero exit, or unparsable `mev frontier
/// --json` output is [`ConductorProposalError::Frontier`], distinct from
/// [`ConductorProposalError::Objective`] so an operator sees which of the
/// two hard-refusal causes actually applies.
pub fn propose_from_frontier<O: CommandOutputLike>(
    config: &ConductorConfig,
    tasks_json_exists: &TasksJsonChecker,
    runner: &Runner<O>,
) -> Result<ProposalOutcome, ConductorProposalError> {
    // Checked here too (ahead of the slate fetch, which shells out) so a
    // missing objective refuses without ever invoking `runner` for `mev
    // frontier --json` — [`propose_chain`] below re-checks this as its own
    // first step regardless (it must, since it is also called directly with
    // a caller-supplied `proposed` list that never went through this
    // function), so this is a fast, side-effect-free short-circuit, not a
    // change to what ultimately gets refused.
    read_objective(config).map_err(ConductorProposalError::Objective)?;

    let slate = fetch_frontier_slate(runner, config).map_err(ConductorProposalError::Frontier)?;

    let proposed: Vec<(String, String)> = slate
        .entries
        .iter()
        .map(|entry| (entry.repo.clone(), entry.id.clone()))
        .collect();

    propose_chain(config, &proposed, &slate, tasks_json_exists, runner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct TestOutput {
        status: i32,
        stdout: String,
        stderr: String,
    }

    impl CommandOutputLike for TestOutput {
        fn status(&self) -> i32 {
            self.status
        }
        fn stdout(&self) -> &str {
            &self.stdout
        }
        fn stderr(&self) -> &str {
            &self.stderr
        }
    }

    fn ok_runner(stdout: &str) -> Runner<TestOutput> {
        let stdout = stdout.to_string();
        Arc::new(move |_program, _args, _cwd| {
            Ok(TestOutput {
                status: 0,
                stdout: stdout.clone(),
                stderr: String::new(),
            })
        })
    }

    const SAMPLE_FRONTIER: &str = r#"{
        "derived_at": "2026-09-03T00:00:00-07:00",
        "entries": [
            {
                "roadmap": "orchestration-extensions",
                "lane": "engine-rs",
                "segment": 0,
                "repo": "engine-rs",
                "key": "engine-rs:EN.12.F",
                "id": "EN.12.F",
                "title": "CONDUCTOR",
                "status": "open",
                "unmet_blocks": [],
                "unmet_gates": [],
                "startable": true
            }
        ],
        "gate_ranks": []
    }"#;

    // ── objective_path default ──────────────────────────────────────────

    #[test]
    fn default_config_uses_the_brain_root_objective_path() {
        let config = ConductorConfig::default();
        assert_eq!(config.objective_path(), Path::new(DEFAULT_OBJECTIVE_PATH));
        assert_eq!(config.mev_cwd(), Path::new("."));
    }

    #[test]
    fn objective_path_is_overridable_for_tests() {
        let config = ConductorConfig::new().with_objective_path("fixtures/objective.md");
        assert_eq!(config.objective_path(), Path::new("fixtures/objective.md"));
    }

    // ── read_objective ───────────────────────────────────────────────────

    #[test]
    fn read_objective_returns_the_file_contents() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("objective.md");
        std::fs::write(&path, "Ship EN.12.F task 1.\n").unwrap();

        let config = ConductorConfig::new().with_objective_path(&path);
        let objective = read_objective(&config).expect("objective file exists");
        assert_eq!(objective, "Ship EN.12.F task 1.\n");
    }

    #[test]
    fn read_objective_refuses_a_missing_file_rather_than_inferring_a_goal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("does-not-exist.md");

        let config = ConductorConfig::new().with_objective_path(&path);
        let err = read_objective(&config).expect_err("no objective file present");
        assert_eq!(
            err,
            ConductorInputError::ObjectiveMissing { path: path.clone() }
        );
        assert!(err.to_string().contains("never falls back"));
    }

    // ── fetch_frontier_slate ─────────────────────────────────────────────

    #[test]
    fn fetch_frontier_slate_invokes_mev_frontier_json_through_the_injected_runner() {
        let calls: Arc<Mutex<Vec<(String, Vec<String>, String)>>> =
            Arc::new(Mutex::new(Vec::new()));
        let calls_clone = calls.clone();
        let stdout = SAMPLE_FRONTIER.to_string();
        let runner: Runner<TestOutput> = Arc::new(move |program, args, cwd| {
            calls_clone.lock().unwrap().push((
                program.to_string(),
                args.iter().map(|s| s.to_string()).collect(),
                cwd.to_string_lossy().into_owned(),
            ));
            Ok(TestOutput {
                status: 0,
                stdout: stdout.clone(),
                stderr: String::new(),
            })
        });

        let config = ConductorConfig::new().with_mev_cwd("hq-root");
        let artifact =
            fetch_frontier_slate(&runner, &config).expect("well-formed frontier output parses");

        assert_eq!(artifact.entries.len(), 1);
        assert_eq!(artifact.entries[0].id, "EN.12.F");

        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, "mev");
        assert_eq!(recorded[0].1, vec!["frontier", "--json"]);
        assert_eq!(recorded[0].2, "hq-root");
        // `frontier` is a READ verb, exempt from the `--agent` quiesce rule —
        // this call must never pass it.
        assert!(!recorded[0].1.iter().any(|a| a == "--agent"));
    }

    #[test]
    fn fetch_frontier_slate_parses_the_real_recorded_shape() {
        let config = ConductorConfig::new();
        let artifact = fetch_frontier_slate(&ok_runner(SAMPLE_FRONTIER), &config).expect("parses");
        assert_eq!(artifact.derived_at, "2026-09-03T00:00:00-07:00");
    }

    #[test]
    fn fetch_frontier_slate_reports_a_nonzero_exit_rather_than_an_empty_slate() {
        let runner: Runner<TestOutput> = Arc::new(|_p, _a, _c| {
            Ok(TestOutput {
                status: 1,
                stdout: String::new(),
                stderr: "mev: brain.toml not found".to_string(),
            })
        });

        let config = ConductorConfig::new();
        let err = fetch_frontier_slate(&runner, &config).expect_err("nonzero exit");
        match err {
            ConductorInputError::FrontierCommandFailed { status, stderr } => {
                assert_eq!(status, 1);
                assert!(stderr.contains("brain.toml"));
            }
            other => panic!("expected FrontierCommandFailed, got {other:?}"),
        }
    }

    #[test]
    fn fetch_frontier_slate_reports_unparsable_output_rather_than_panicking() {
        let config = ConductorConfig::new();
        let err = fetch_frontier_slate(&ok_runner("not json"), &config)
            .expect_err("malformed stdout must not parse");
        assert!(matches!(
            err,
            ConductorInputError::FrontierUnparsable { .. }
        ));
    }

    #[test]
    fn fetch_frontier_slate_reports_a_spawn_failure_distinctly() {
        let runner: Runner<TestOutput> = Arc::new(|_p, _a, _c| {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "mev not on PATH",
            ))
        });

        let config = ConductorConfig::new();
        let err = fetch_frontier_slate(&runner, &config).expect_err("spawn failure");
        assert!(matches!(
            err,
            ConductorInputError::FrontierSpawnFailed { .. }
        ));
    }

    // ── propose_chain ────────────────────────────────────────────────────

    /// Writes `content` to a fresh temp file and returns its path — mirrors
    /// `chain.rs`'s own fixture-writing convention rather than pulling in a
    /// tempdir crate for one string.
    fn write_objective_fixture(name: &str, content: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "engine-rs-conductor-test-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("objective.md");
        std::fs::write(&path, content).unwrap();
        path
    }

    fn config_with_objective(name: &str) -> ConductorConfig {
        let path = write_objective_fixture(name, "Ship EN.12.F.\n");
        ConductorConfig::new().with_objective_path(path)
    }

    fn slate_from(json: &str) -> FrontierArtifact {
        serde_json::from_str(json).expect("well-formed frontier fixture")
    }

    /// A slate fixture built independently of any proposal below — per
    /// carryover `gate-scope-must-be-shown-capable-of-failing`, the subset
    /// check's two inputs must never derive one from the other.
    const INDEPENDENT_SLATE: &str = SAMPLE_FRONTIER;

    fn checker(known: &'static [(&'static str, &'static str)]) -> TasksJsonChecker {
        std::sync::Arc::new(move |repo: &str, block_id: &str| {
            known.iter().any(|(r, b)| *r == repo && *b == block_id)
        })
    }

    /// A `git` runner stub that reports NO matching commits for any pickaxe
    /// search — i.e. every candidate survives the `git log -S` pre-flight.
    /// The default double for tests that exercise the earlier refusal
    /// stages (subset / tasks.json / objective) and don't care about the
    /// pre-flight itself.
    fn no_history_git_runner() -> Runner<TestOutput> {
        Arc::new(|_program, _args, _cwd| {
            Ok(TestOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        })
    }

    /// A `git` runner stub that reports a matching commit for every pickaxe
    /// search — i.e. every candidate is dropped by the `git log -S`
    /// pre-flight.
    fn all_history_git_runner() -> Runner<TestOutput> {
        Arc::new(|_program, _args, _cwd| {
            Ok(TestOutput {
                status: 0,
                stdout: "abc1234 feat: implement already done\n".to_string(),
                stderr: String::new(),
            })
        })
    }

    #[test]
    fn a_slate_subset_proposal_with_tasks_json_succeeds() {
        let config = config_with_objective("subset-ok");
        let slate = slate_from(INDEPENDENT_SLATE);
        let proposed = vec![("engine-rs".to_string(), "EN.12.F".to_string())];
        let tasks_json_exists = checker(&[("engine-rs", "EN.12.F")]);
        let git_runner = no_history_git_runner();

        let outcome = propose_chain(&config, &proposed, &slate, &tasks_json_exists, &git_runner)
            .expect("proposal is a subset of the slate and has tasks.json");

        assert_eq!(outcome.chain.len(), 1);
        assert_eq!(outcome.chain[0].repo, "engine-rs");
        assert_eq!(outcome.chain[0].block_id, "EN.12.F");
        assert!(outcome.dropped.is_empty());
    }

    #[test]
    fn a_proposal_containing_a_non_slate_block_is_rejected_wholesale_not_trimmed() {
        let config = config_with_objective("non-slate");
        let slate = slate_from(INDEPENDENT_SLATE);
        // Two entries: one IS in the slate, one is not. A trimming
        // implementation would drop only the second and succeed with one
        // step; the subset rule instead rejects the WHOLE proposal.
        let proposed = vec![
            ("engine-rs".to_string(), "EN.12.F".to_string()),
            ("engine-rs".to_string(), "EN.99.Z".to_string()),
        ];
        let tasks_json_exists = checker(&[("engine-rs", "EN.12.F"), ("engine-rs", "EN.99.Z")]);
        let git_runner = no_history_git_runner();

        let err = propose_chain(&config, &proposed, &slate, &tasks_json_exists, &git_runner)
            .expect_err("EN.99.Z is not in the slate");

        assert_eq!(
            err,
            ConductorProposalError::NotInSlate {
                repo: "engine-rs".to_string(),
                block_id: "EN.99.Z".to_string(),
            }
        );
    }

    #[test]
    fn an_invented_block_id_is_refused_with_a_named_diagnostic() {
        let config = config_with_objective("invented");
        let slate = slate_from(INDEPENDENT_SLATE);
        let proposed = vec![("engine-rs".to_string(), "EN.NOT.REAL".to_string())];
        // Even a "yes it has tasks.json" checker must not rescue an invented
        // id — the slate check runs first and rejects it regardless.
        let tasks_json_exists = checker(&[("engine-rs", "EN.NOT.REAL")]);
        let git_runner = no_history_git_runner();

        let err = propose_chain(&config, &proposed, &slate, &tasks_json_exists, &git_runner)
            .expect_err("EN.NOT.REAL does not exist in the slate/corpus");

        match &err {
            ConductorProposalError::NotInSlate { repo, block_id } => {
                assert_eq!(repo, "engine-rs");
                assert_eq!(block_id, "EN.NOT.REAL");
            }
            other => panic!("expected NotInSlate, got {other:?}"),
        }
        assert!(err.to_string().contains("EN.NOT.REAL"));
    }

    #[test]
    fn a_candidate_with_no_tasks_json_is_refused_naming_generate_tasks() {
        let config = config_with_objective("no-tasks-json");
        let slate = slate_from(INDEPENDENT_SLATE);
        let proposed = vec![("engine-rs".to_string(), "EN.12.F".to_string())];
        // In the slate, but the checker reports no tasks.json.
        let tasks_json_exists = checker(&[]);
        let git_runner = no_history_git_runner();

        let err = propose_chain(&config, &proposed, &slate, &tasks_json_exists, &git_runner)
            .expect_err("EN.12.F has no tasks.json per the checker");

        assert_eq!(
            err,
            ConductorProposalError::MissingTasksJson {
                repo: "engine-rs".to_string(),
                block_id: "EN.12.F".to_string(),
            }
        );
        assert!(err.to_string().contains("/generate-tasks"));
    }

    #[test]
    fn with_no_objective_file_the_conductor_refuses_to_propose_anything() {
        let dir = std::env::temp_dir().join(format!(
            "engine-rs-conductor-test-no-objective-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let missing_path = dir.join("does-not-exist.md");
        let config = ConductorConfig::new().with_objective_path(&missing_path);
        let slate = slate_from(INDEPENDENT_SLATE);
        // A proposal that WOULD otherwise succeed, to prove the objective
        // check runs first and blocks it regardless.
        let proposed = vec![("engine-rs".to_string(), "EN.12.F".to_string())];
        let tasks_json_exists = checker(&[("engine-rs", "EN.12.F")]);
        let git_runner = no_history_git_runner();

        let err = propose_chain(&config, &proposed, &slate, &tasks_json_exists, &git_runner)
            .expect_err("no objective file present");

        match err {
            ConductorProposalError::Objective(ConductorInputError::ObjectiveMissing { path }) => {
                assert_eq!(path, missing_path);
            }
            other => panic!("expected Objective(ObjectiveMissing), got {other:?}"),
        }
    }

    // ── git_log_dash_s_preflight / propose_chain integration ───────────────

    /// A two-entry slate so pre-flight tests can prove one candidate is
    /// dropped while its neighbour — the positive control — survives.
    const TWO_ENTRY_SLATE: &str = r#"{
        "derived_at": "2026-09-03T00:00:00-07:00",
        "entries": [
            {
                "roadmap": "orchestration-extensions",
                "lane": "engine-rs",
                "segment": 0,
                "repo": "engine-rs",
                "key": "engine-rs:EN.12.F",
                "id": "EN.12.F",
                "title": "CONDUCTOR",
                "status": "open",
                "unmet_blocks": [],
                "unmet_gates": [],
                "startable": true
            },
            {
                "roadmap": "orchestration-extensions",
                "lane": "engine-rs",
                "segment": 1,
                "repo": "engine-rs",
                "key": "engine-rs:EN.12.G",
                "id": "EN.12.G",
                "title": "DEBRIEF",
                "status": "open",
                "unmet_blocks": [],
                "unmet_gates": [],
                "startable": true
            }
        ],
        "gate_ranks": []
    }"#;

    /// A `git` runner stub that reports a matching commit ONLY for
    /// `-SEN.12.F` and nothing for any other pickaxe search — so a test can
    /// prove one candidate is dropped while a second, untouched candidate
    /// (the positive control) survives.
    fn selective_history_git_runner() -> Runner<TestOutput> {
        Arc::new(|_program, args, _cwd| {
            let matched_en_12_f = args.iter().any(|a| *a == "-SEN.12.F");
            Ok(TestOutput {
                status: 0,
                stdout: if matched_en_12_f {
                    "abc1234 feat: implement EN.12.F already\n".to_string()
                } else {
                    String::new()
                },
                stderr: String::new(),
            })
        })
    }

    #[test]
    fn git_log_dash_s_preflight_drops_a_candidate_already_in_history() {
        let config = ConductorConfig::new();
        let candidates = vec![("engine-rs".to_string(), "EN.12.F".to_string())];
        let git_runner = all_history_git_runner();

        let (survivors, dropped) =
            git_log_dash_s_preflight(&git_runner, &config, &candidates).expect("git ran cleanly");

        assert!(survivors.is_empty());
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].repo, "engine-rs");
        assert_eq!(dropped[0].block_id, "EN.12.F");
        assert!(dropped[0].reason.contains("already"));
    }

    #[test]
    fn git_log_dash_s_preflight_lets_a_not_yet_done_candidate_survive() {
        // The positive control: proves the filter isn't simply dropping
        // everything it's given.
        let config = ConductorConfig::new();
        let candidates = vec![("engine-rs".to_string(), "EN.12.G".to_string())];
        let git_runner = no_history_git_runner();

        let (survivors, dropped) =
            git_log_dash_s_preflight(&git_runner, &config, &candidates).expect("git ran cleanly");

        assert_eq!(survivors, candidates);
        assert!(dropped.is_empty());
    }

    #[test]
    fn git_log_dash_s_preflight_invokes_git_log_dash_s_through_the_injected_runner() {
        let calls: Arc<Mutex<Vec<(String, Vec<String>, String)>>> =
            Arc::new(Mutex::new(Vec::new()));
        let calls_clone = calls.clone();
        let runner: Runner<TestOutput> = Arc::new(move |program, args, cwd| {
            calls_clone.lock().unwrap().push((
                program.to_string(),
                args.iter().map(|s| s.to_string()).collect(),
                cwd.to_string_lossy().into_owned(),
            ));
            Ok(TestOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });

        let config = ConductorConfig::new().with_git_cwd("hq-root");
        let candidates = vec![("engine-rs".to_string(), "EN.12.F".to_string())];
        git_log_dash_s_preflight(&runner, &config, &candidates).expect("git ran cleanly");

        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, "git");
        assert_eq!(recorded[0].1, vec!["log", "-SEN.12.F", "--oneline"]);
        assert_eq!(recorded[0].2, "hq-root");
    }

    #[test]
    fn git_log_dash_s_preflight_defaults_git_cwd_to_mev_cwd() {
        let config = ConductorConfig::new().with_mev_cwd("hq-root");
        assert_eq!(config.git_cwd(), Path::new("hq-root"));

        let overridden = config.clone().with_git_cwd("elsewhere");
        assert_eq!(overridden.git_cwd(), Path::new("elsewhere"));
        // Overriding git_cwd must not disturb mev_cwd.
        assert_eq!(overridden.mev_cwd(), Path::new("hq-root"));
    }

    #[test]
    fn git_log_dash_s_preflight_reports_a_spawn_failure_as_a_hard_refusal() {
        let config = ConductorConfig::new();
        let candidates = vec![("engine-rs".to_string(), "EN.12.F".to_string())];
        let runner: Runner<TestOutput> = Arc::new(|_p, _a, _c| {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "git not on PATH",
            ))
        });

        let err = git_log_dash_s_preflight(&runner, &config, &candidates)
            .expect_err("git failed to spawn");

        match err {
            ConductorProposalError::GitPreflightFailed { repo, block_id, .. } => {
                assert_eq!(repo, "engine-rs");
                assert_eq!(block_id, "EN.12.F");
            }
            other => panic!("expected GitPreflightFailed, got {other:?}"),
        }
    }

    #[test]
    fn git_log_dash_s_preflight_reports_a_nonzero_exit_as_a_hard_refusal() {
        let config = ConductorConfig::new();
        let candidates = vec![("engine-rs".to_string(), "EN.12.F".to_string())];
        let runner: Runner<TestOutput> = Arc::new(|_p, _a, _c| {
            Ok(TestOutput {
                status: 128,
                stdout: String::new(),
                stderr: "fatal: not a git repository".to_string(),
            })
        });

        let err = git_log_dash_s_preflight(&runner, &config, &candidates)
            .expect_err("git exited non-zero");

        assert!(matches!(
            err,
            ConductorProposalError::GitPreflightFailed { .. }
        ));
        assert!(err.to_string().contains("could not run"));
    }

    #[test]
    fn propose_chain_runs_the_preflight_before_finalising_and_journals_the_drop() {
        let config = config_with_objective("preflight-drop");
        let slate = slate_from(TWO_ENTRY_SLATE);
        // EN.12.F is already done (per the selective runner); EN.12.G is
        // the not-yet-done positive control.
        let proposed = vec![
            ("engine-rs".to_string(), "EN.12.F".to_string()),
            ("engine-rs".to_string(), "EN.12.G".to_string()),
        ];
        let tasks_json_exists = checker(&[("engine-rs", "EN.12.F"), ("engine-rs", "EN.12.G")]);
        let git_runner = selective_history_git_runner();

        let outcome = propose_chain(&config, &proposed, &slate, &tasks_json_exists, &git_runner)
            .expect("subset + tasks.json checks pass; the pre-flight only drops, never refuses");

        // EN.12.F was dropped by the pre-flight, not proposed downstream —
        // proving the pre-flight ran BEFORE the chain was finalised.
        assert_eq!(outcome.chain.len(), 1);
        assert_eq!(outcome.chain[0].block_id, "EN.12.G");

        assert_eq!(outcome.dropped.len(), 1);
        assert_eq!(outcome.dropped[0].block_id, "EN.12.F");
        assert!(outcome.dropped[0].reason.contains("EN.12.F"));
    }
}
