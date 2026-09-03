//! `CONDUCTOR` — `EN.12.F`.
//!
//! The run picks tonight's chain from the operator's weekly objective and mev's
//! computed frontier slate, then justifies the choice in the journal. Per Fork 3
//! (2026-08-19): the objective is operator-written; mev's computed slate is the
//! candidate set the conductor may draw from — it is a planner, never a backlog
//! drain, because it can only pick and justify from a set that already exists.
//!
//! **Task 1 lands the two inputs alone**: reading the objective file
//! ([`read_objective`]) and obtaining mev's computed slate via `mev frontier
//! --json` ([`fetch_frontier_slate`]), shelled out through the SAME
//! `CommandRunner` convention [`crate::policy::emit_state::EmitStateNode`]
//! already uses for `mev emit-state --write` — the injectable
//! [`crate::policy::emit_state::Runner`] seam, so no test in this module (or
//! anywhere downstream of it) ever shells out to a real `mev` binary. Later
//! tasks turn these two inputs into a slate-constrained, justified chain
//! proposal (with the `git log -S` pre-flight and the invented-block /
//! missing-`tasks.json` refusals) and the journal write; this module does not
//! yet propose or journal anything.
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
}

impl Default for ConductorConfig {
    fn default() -> Self {
        Self {
            objective_path: PathBuf::from(DEFAULT_OBJECTIVE_PATH),
            mev_cwd: PathBuf::from("."),
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

    #[must_use]
    pub fn objective_path(&self) -> &Path {
        &self.objective_path
    }

    #[must_use]
    pub fn mev_cwd(&self) -> &Path {
        &self.mev_cwd
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
    /// `repo:block_id` is not present in the slate this run's `mev frontier
    /// --json` returned. Covers both a real corpus block that just isn't in
    /// tonight's slate and an outright invented id — see the module note
    /// above for why the two are indistinguishable from here. The WHOLE
    /// proposal is rejected; nothing is silently trimmed.
    NotInSlate { repo: String, block_id: String },
    /// `repo:block_id` is in the slate but has no `tasks.json` yet, so
    /// dispatching it would only fail. Refused rather than dispatched.
    MissingTasksJson { repo: String, block_id: String },
}

impl std::fmt::Display for ConductorProposalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConductorProposalError::Objective(err) => write!(f, "{err}"),
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
        }
    }
}

impl std::error::Error for ConductorProposalError {}

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
/// `slate` is an independent input (typically [`fetch_frontier_slate`]'s
/// result), never derived from `proposed` — per carryover
/// `gate-scope-must-be-shown-capable-of-failing`, a subset check whose slate
/// is derived from the proposal itself can never fail.
pub fn propose_chain(
    config: &ConductorConfig,
    proposed: &[(String, String)],
    slate: &FrontierArtifact,
    tasks_json_exists: &TasksJsonChecker,
) -> Result<Vec<crate::workflows::orchestration::chain::ChainStep>, ConductorProposalError> {
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

    Ok(crate::workflows::orchestration::chain::resolve_explicit_chain(proposed.to_vec()))
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

    #[test]
    fn a_slate_subset_proposal_with_tasks_json_succeeds() {
        let config = config_with_objective("subset-ok");
        let slate = slate_from(INDEPENDENT_SLATE);
        let proposed = vec![("engine-rs".to_string(), "EN.12.F".to_string())];
        let tasks_json_exists = checker(&[("engine-rs", "EN.12.F")]);

        let chain = propose_chain(&config, &proposed, &slate, &tasks_json_exists)
            .expect("proposal is a subset of the slate and has tasks.json");

        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].repo, "engine-rs");
        assert_eq!(chain[0].block_id, "EN.12.F");
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

        let err = propose_chain(&config, &proposed, &slate, &tasks_json_exists)
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

        let err = propose_chain(&config, &proposed, &slate, &tasks_json_exists)
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

        let err = propose_chain(&config, &proposed, &slate, &tasks_json_exists)
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

        let err = propose_chain(&config, &proposed, &slate, &tasks_json_exists)
            .expect_err("no objective file present");

        match err {
            ConductorProposalError::Objective(ConductorInputError::ObjectiveMissing { path }) => {
                assert_eq!(path, missing_path);
            }
            other => panic!("expected Objective(ObjectiveMissing), got {other:?}"),
        }
    }
}
