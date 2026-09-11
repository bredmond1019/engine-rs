//! Preflight (`EN.17.D` Task 2) — per-block claim extraction and a
//! per-program `argv` validator with no shell.
//!
//! `PreflightRunner` is a plain struct, not a graph node — ORCHESTRATION is
//! a single-node graph, and preflight is called from the chain loop in
//! [`super::integrate`] (Task 3), once per block step. For a block step it
//! reads `planning/blocks/<ID>.json` from the step's repo via
//! [`crate::repo_registry::read_block_record`]; a missing or unparseable
//! record yields [`PreflightOutcome::SkippedNoRecord`] and the block
//! proceeds — preflight is deliberately LIGHT (operator direction
//! 2026-09-08); the main premise check belongs in `GenerateTasksNode` at
//! authoring time, out of scope here.
//!
//! It runs one [`crate::nodes::JudgmentNode::judge`] call over byte-capped
//! excerpts of the record's `what`, `files`, and `acceptance_criteria`
//! fields, judged against a schema of claims: `{claim, load_bearing, argv,
//! expect, needle}`. At most [`PreflightConfig::max_claims`] are executed;
//! any excess is reported as `claims_dropped`.
//!
//! # The safety boundary: per-program `argv` validation, not a prefix allowlist
//!
//! Model output, derived from block text, chooses the commands. A **prefix**
//! allowlist is not safe here: a red-team pass on 2026-09-10 found the
//! original design admitted `rg --pre=<program>` (ripgrep runs the named
//! program on every file searched — confirmed: `rg --pre /bin/echo` matched
//! text that exists only in echo's output) and `git log --output=<file>`
//! (writes a file — also confirmed). So [`validate_argv`] is a compiled-in,
//! per-program table: `argv[0]` must be a bare name (`rg`, `git`, `test`,
//! `ls` — no path), and every flag not explicitly named for that program is
//! refused. [`PreflightConfig::programs`] can only NARROW this table, never
//! add a program or a flag — a config-added flag is exactly the bypass this
//! closes.
//!
//! [`execute_argv`] never shells out through a shell: `std::process::Command`
//! with an explicit argv array, a cleared environment (fixed `PATH`/`HOME`
//! only, plus `GIT_CONFIG_NOSYSTEM=1` and `GIT_CONFIG_GLOBAL=/dev/null` so
//! `RIPGREP_CONFIG_PATH`/`GIT_*` are never inherited from whatever process
//! spawned the engine), and a per-command timeout that kills and reaps the
//! child on expiry. A refused, timed-out, or unspawnable command is recorded
//! [`ClaimVerdict::Unverifiable`] with a reason and never counts as `false`.

use std::path::{Component, Path};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};

use engine_contract::TaskContext;

use crate::nodes::{InputSlice, JudgmentError, JudgmentNode, JudgmentSpec, MetaTransport};
use crate::policy::ModelTier;
use crate::repo_registry::{read_block_record, RepoRegistry};
use crate::workflows::ModelTransport;

/// The `Node::name()`-style identity the composed judgment call runs under.
const PREFLIGHT_NODE_NAME: &str = "PreflightRunner";

/// The stable preflight prompt (D24 / standing rule 7) — a whole task
/// prompt, not a cache-anchor prefix. Lives under `workflows/orchestration/`
/// (not `nodes/`) so `tests/it/prompt_externalization.rs`'s regex, which
/// only walks `src/workflows/**`, actually scans this const.
const PREFLIGHT_PROMPT: &str = include_str!("prompts/preflight.md");

/// The only programs [`execute_argv`] will ever spawn. [`PreflightConfig::programs`]
/// intersects this table; it can never add to it.
const COMPILED_PROGRAMS: &[&str] = &["rg", "git", "test", "ls"];

/// A cap on how much of a child's stdout is retained in [`ClaimResult`]
/// bookkeeping / `stdout_contains` matching, so a chatty command cannot
/// balloon the report.
const MAX_CAPTURED_STDOUT_BYTES: usize = 64 * 1024;

// ---------------------------------------------------------------------------
// The judged claim schema
// ---------------------------------------------------------------------------

/// One claim, as the model returns it. Opaque outside this module —
/// [`ClaimResult`] is the public, post-execution shape a caller sees; this
/// type is nameable only because `test_support`'s constructors return it
/// across the crate boundary to `tests/it/preflight.rs`. Its fields stay
/// private; external code never constructs or destructures it directly.
#[derive(Debug, Clone, Deserialize)]
pub struct JudgedClaim {
    claim: String,
    load_bearing: bool,
    argv: Vec<String>,
    expect: ClaimExpect,
    #[serde(default)]
    needle: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ClaimExpect {
    ExitZero,
    ExitNonzero,
    StdoutContains,
}

/// The judged reply's top-level shape: `{"claims": [...]}`.
#[derive(Debug, Clone, Deserialize)]
struct JudgedClaims {
    #[serde(default)]
    claims: Vec<JudgedClaim>,
}

/// JSON schema matching [`JudgedClaims`] — `Config.json_schema` for the
/// preflight judgment call.
fn preflight_claims_json_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "claims": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "claim": { "type": "string" },
                        "load_bearing": { "type": "boolean" },
                        "argv": { "type": "array", "items": { "type": "string" } },
                        "expect": {
                            "type": "string",
                            "enum": ["exit_zero", "exit_nonzero", "stdout_contains"],
                        },
                        "needle": { "type": ["string", "null"] },
                    },
                    "required": ["claim", "load_bearing", "argv", "expect"],
                },
            },
        },
        "required": ["claims"],
    })
}

/// A block record field, rendered as prompt text: verbatim for a string
/// field (`what`), pretty-printed JSON for a structured field (`files`,
/// `acceptance_criteria`). Absent fields render as an empty string rather
/// than erroring — a record missing an optional field is not preflight's
/// problem to raise.
fn slice_text(record: &Value, key: &str) -> String {
    match record.get(key) {
        Some(Value::String(text)) => text.clone(),
        Some(other) => serde_json::to_string_pretty(other).unwrap_or_default(),
        None => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Public result types
// ---------------------------------------------------------------------------

/// A judged claim's verdict once its `argv` has been validated and (when
/// admitted) executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimVerdict {
    /// The command ran and its outcome matched `expect`.
    Held,
    /// The command ran but its outcome did NOT match `expect`.
    False,
    /// The command was refused by the argv validator, timed out, or could
    /// not be spawned. Never counted as `False`.
    Unverifiable,
}

/// One claim's full record: what was claimed, what was run, and the
/// resulting verdict.
#[derive(Debug, Clone)]
pub struct ClaimResult {
    pub claim: String,
    pub argv: Vec<String>,
    pub load_bearing: bool,
    pub exit_code: Option<i32>,
    pub stdout_bytes: usize,
    pub verdict: ClaimVerdict,
    /// Set for `Unverifiable` (the refusal reason, or the literal string
    /// `"timeout"`); `None` for `Held`/`False`.
    pub reason: Option<String>,
}

/// One block step's preflight outcome.
#[derive(Debug, Clone)]
pub enum PreflightOutcome {
    /// The judgment call succeeded; `claims` is every claim actually
    /// executed (bounded by `PreflightConfig::max_claims`).
    Judged { claims: Vec<ClaimResult> },
    /// The judgment call itself failed — no claim was ever extracted, let
    /// alone executed. `error_kind` names the [`JudgmentError`] variant.
    Unjudged { error_kind: String },
    /// No `planning/blocks/<ID>.json` was found (or it failed to parse) for
    /// this step's repo/block id.
    SkippedNoRecord,
    /// Preflight is disabled for this run. [`super::integrate`] (Task 3)
    /// is what actually produces this variant; it is defined here because
    /// it is part of this module's outcome type.
    Disabled,
}

/// One block step's accumulated preflight record — one of these per block
/// step, in chain order, forms the run's `preflight_report`.
#[derive(Debug, Clone)]
pub struct BlockPreflight {
    pub repo: String,
    pub block_id: String,
    pub outcome: PreflightOutcome,
    /// Every claim actually executed (duplicated from `Judged`'s own
    /// `claims` for a caller that wants the flat list without matching on
    /// `outcome`); empty for every non-`Judged` outcome.
    pub claims: Vec<ClaimResult>,
    /// How many of the judged claims were dropped for exceeding
    /// `PreflightConfig::max_claims`. Zero for every non-`Judged` outcome.
    pub claims_dropped: usize,
}

// ---------------------------------------------------------------------------
// The argv validator
// ---------------------------------------------------------------------------

/// `true` when `arg` is a safe positional path/pattern argument: not
/// absolute, and no `..` path component. Applied uniformly to every
/// positional (non-flag) token across every program — a search pattern like
/// `"x"` trivially passes; `"../secret"` and `"/etc/passwd"` do not.
fn is_safe_positional(arg: &str) -> bool {
    if arg.starts_with('/') {
        return false;
    }
    !Path::new(arg)
        .components()
        .any(|c| matches!(c, Component::ParentDir))
}

fn validate_rg(args: &[String]) -> Result<(), String> {
    const STANDALONE: &[&str] = &["-n", "-l", "-c", "-i", "-F", "-w", "-q", "-L", "--files"];
    const VALUE_FLAGS: &[&str] = &["-e", "-g", "--glob", "-t", "--type"];

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            i += 1;
            continue;
        }
        if arg.starts_with('-') {
            if STANDALONE.contains(&arg.as_str()) {
                i += 1;
                continue;
            }
            if VALUE_FLAGS.contains(&arg.as_str()) {
                if i + 1 >= args.len() {
                    return Err(format!("rg flag '{arg}' requires a value"));
                }
                i += 2;
                continue;
            }
            if let Some((flag, _value)) = arg.split_once('=') {
                if VALUE_FLAGS.contains(&flag) {
                    i += 1;
                    continue;
                }
            }
            return Err(format!("unsupported rg flag '{arg}'"));
        } else {
            if !is_safe_positional(arg) {
                return Err(format!("unsafe rg positional argument '{arg}'"));
            }
            i += 1;
        }
    }
    Ok(())
}

fn validate_git(args: &[String]) -> Result<(), String> {
    const ALLOWED_SUBCOMMANDS: &[&str] = &["log", "show", "rev-parse", "ls-files"];
    const EXPLICIT_REFUSALS: &[&str] = &["--output", "-p", "--patch", "--ext-diff", "--textconv"];
    const EXACT_ALLOWED: &[&str] = &["--oneline", "--name-only", "--name-status", "--"];
    const PREFIX_ALLOWED: &[&str] = &[
        "--format=",
        "--pretty=",
        "--since=",
        "--until=",
        "--author=",
        "--grep=",
    ];

    let Some(subcommand) = args.first() else {
        return Err("git requires a subcommand".to_string());
    };
    if subcommand.starts_with('-') {
        return Err(format!(
            "git global option '{subcommand}' before the subcommand is refused"
        ));
    }
    if !ALLOWED_SUBCOMMANDS.contains(&subcommand.as_str()) {
        return Err(format!("unsupported git subcommand '{subcommand}'"));
    }

    for arg in &args[1..] {
        if let Some(tail) = arg.strip_prefix('-') {
            if EXPLICIT_REFUSALS
                .iter()
                .any(|f| arg == f || arg.starts_with(&format!("{f}=")))
            {
                return Err(format!("git flag '{arg}' is explicitly refused"));
            }
            if EXACT_ALLOWED.contains(&arg.as_str()) {
                continue;
            }
            if PREFIX_ALLOWED.iter().any(|p| arg.starts_with(p)) {
                continue;
            }
            if arg == "-n" {
                continue;
            }
            if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            return Err(format!("unsupported git flag '{arg}'"));
        } else if !is_safe_positional(arg) {
            return Err(format!("unsafe git positional argument '{arg}'"));
        }
    }
    Ok(())
}

fn validate_test(args: &[String]) -> Result<(), String> {
    const ALLOWED: &[&str] = &["-e", "-f", "-d", "-s"];
    if args.len() != 2 {
        return Err("test requires exactly one flag and one path".to_string());
    }
    if !ALLOWED.contains(&args[0].as_str()) {
        return Err(format!("unsupported test flag '{}'", args[0]));
    }
    if !is_safe_positional(&args[1]) {
        return Err(format!("unsafe test positional argument '{}'", args[1]));
    }
    Ok(())
}

fn validate_ls(args: &[String]) -> Result<(), String> {
    for arg in args {
        if arg == "-1" || arg == "-a" {
            continue;
        }
        if arg.starts_with('-') {
            return Err(format!("unsupported ls flag '{arg}'"));
        }
        if !is_safe_positional(arg) {
            return Err(format!("unsafe ls positional argument '{arg}'"));
        }
    }
    Ok(())
}

/// Intersect the compiled-in program table with `programs_override` — never
/// unions with it. `None` means "the whole compiled-in table".
fn allowed_programs(programs_override: Option<&[String]>) -> Vec<&'static str> {
    match programs_override {
        None => COMPILED_PROGRAMS.to_vec(),
        Some(narrow) => COMPILED_PROGRAMS
            .iter()
            .filter(|p| narrow.iter().any(|n| n == *p))
            .copied()
            .collect(),
    }
}

/// Validate one claim's `argv` against the compiled-in per-program table.
/// `Ok(())` means the command is safe to run; `Err(reason)` names why it was
/// refused. `programs_override` may only narrow [`COMPILED_PROGRAMS`].
fn validate_argv(argv: &[String], programs_override: Option<&[String]>) -> Result<(), String> {
    let Some(program) = argv.first() else {
        return Err("empty argv".to_string());
    };
    if program.contains('/') || program.contains('\\') {
        return Err(format!(
            "program '{program}' must be a bare name, not a path"
        ));
    }
    let allowed = allowed_programs(programs_override);
    if !allowed.contains(&program.as_str()) {
        return Err(format!("program '{program}' is not in the allowed set"));
    }

    let rest = &argv[1..];
    match program.as_str() {
        "rg" => validate_rg(rest),
        "git" => validate_git(rest),
        "test" => validate_test(rest),
        "ls" => validate_ls(rest),
        other => Err(format!("program '{other}' has no validator")),
    }
}

// ---------------------------------------------------------------------------
// The no-shell, cleared-environment runner
// ---------------------------------------------------------------------------

/// Spawn `argv` with no shell, a cleared environment (fixed `PATH`/`HOME`
/// only, plus `GIT_CONFIG_NOSYSTEM=1`/`GIT_CONFIG_GLOBAL=/dev/null`), and a
/// hard `timeout`. `rg` always runs with `--no-config` prepended, regardless
/// of what the judged claim's `argv` contained.
///
/// Returns `Ok((exit_code, stdout))` on completion within the timeout;
/// `Err("timeout")` on expiry (the child is killed and reaped first); `Err(reason)`
/// for any other spawn failure.
fn execute_argv(
    argv: &[String],
    repo_root: &Path,
    timeout: Duration,
) -> Result<(Option<i32>, Vec<u8>), String> {
    let program = argv[0].as_str();
    let mut exec_args: Vec<String> = Vec::new();
    if program == "rg" {
        exec_args.push("--no-config".to_string());
    }
    exec_args.extend(argv[1..].iter().cloned());

    let path_env = std::env::var("PATH").unwrap_or_default();
    let home_env = std::env::var("HOME").unwrap_or_default();

    let mut cmd = std::process::Command::new(program);
    cmd.args(&exec_args)
        .current_dir(repo_root)
        .env_clear()
        .env("PATH", &path_env)
        .env("HOME", &home_env)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not spawn '{program}': {e}"))?;

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdout_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = stdout_pipe {
            let _ = std::io::Read::read_to_end(&mut pipe, &mut buf);
        }
        buf
    });
    let stderr_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = stderr_pipe {
            let _ = std::io::Read::read_to_end(&mut pipe, &mut buf);
        }
        buf
    });

    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    break None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => break None,
        }
    };

    match status {
        Some(status) => {
            let mut stdout = stdout_handle.join().unwrap_or_default();
            let _ = stderr_handle.join();
            stdout.truncate(MAX_CAPTURED_STDOUT_BYTES);
            Ok((status.code(), stdout))
        }
        None => {
            // Deadline hit: kill then wait() to reap — never leave a zombie.
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_handle.join();
            let _ = stderr_handle.join();
            Err("timeout".to_string())
        }
    }
}

/// Validate, then (if admitted) execute, one claim's `argv`, and classify
/// the result against its `expect` — or `Unverifiable` if the command was
/// never actually run.
fn run_claim(
    claim: &JudgedClaim,
    repo_root: &Path,
    programs_override: Option<&[String]>,
    command_timeout: Duration,
) -> ClaimResult {
    let base = |reason: String| ClaimResult {
        claim: claim.claim.clone(),
        argv: claim.argv.clone(),
        load_bearing: claim.load_bearing,
        exit_code: None,
        stdout_bytes: 0,
        verdict: ClaimVerdict::Unverifiable,
        reason: Some(reason),
    };

    if let Err(reason) = validate_argv(&claim.argv, programs_override) {
        return base(reason);
    }

    match execute_argv(&claim.argv, repo_root, command_timeout) {
        Ok((exit_code, stdout)) => {
            let verdict = match claim.expect {
                ClaimExpect::ExitZero => {
                    if exit_code == Some(0) {
                        ClaimVerdict::Held
                    } else {
                        ClaimVerdict::False
                    }
                }
                ClaimExpect::ExitNonzero => {
                    if matches!(exit_code, Some(code) if code != 0) {
                        ClaimVerdict::Held
                    } else {
                        ClaimVerdict::False
                    }
                }
                ClaimExpect::StdoutContains => {
                    let needle = claim.needle.as_deref().unwrap_or("");
                    let held = !needle.is_empty()
                        && stdout
                            .windows(needle.len().max(1))
                            .any(|w| w == needle.as_bytes());
                    if held {
                        ClaimVerdict::Held
                    } else {
                        ClaimVerdict::False
                    }
                }
            };
            ClaimResult {
                claim: claim.claim.clone(),
                argv: claim.argv.clone(),
                load_bearing: claim.load_bearing,
                exit_code,
                stdout_bytes: stdout.len(),
                verdict,
                reason: None,
            }
        }
        Err(reason) => base(reason),
    }
}

fn error_kind_of(err: &JudgmentError) -> String {
    match err {
        JudgmentError::Timeout => "timeout".to_string(),
        JudgmentError::CliError { .. } => "cli_error".to_string(),
        JudgmentError::NoStructuredResult { .. } => "no_structured_result".to_string(),
        JudgmentError::SchemaViolation { .. } => "schema_violation".to_string(),
    }
}

// ---------------------------------------------------------------------------
// PreflightRunner
// ---------------------------------------------------------------------------

/// The eight `OrchestrationPolicy` preflight knobs (`EN.17.D` Task 4), in
/// the shape [`PreflightRunner`] consumes them. `programs` can only narrow
/// [`COMPILED_PROGRAMS`], never add to it.
#[derive(Debug, Clone)]
pub struct PreflightConfig {
    pub model_tier: ModelTier,
    pub max_claims: usize,
    pub max_turns: Option<u32>,
    pub slice_max_bytes: usize,
    pub programs: Option<Vec<String>>,
    pub command_timeout_ms: u64,
}

impl Default for PreflightConfig {
    fn default() -> Self {
        Self {
            model_tier: ModelTier::Haiku,
            max_claims: 5,
            max_turns: None,
            slice_max_bytes: 4_000,
            programs: None,
            command_timeout_ms: 5_000,
        }
    }
}

/// Runs one block step's preflight: read the record, judge its claims, run
/// each admitted claim's command. Not a graph node — see the module doc
/// comment.
pub struct PreflightRunner {
    config: PreflightConfig,
    judgment: JudgmentNode<JudgedClaims>,
}

impl PreflightRunner {
    #[must_use]
    pub fn new(config: PreflightConfig) -> Self {
        Self {
            config,
            judgment: JudgmentNode::new(),
        }
    }

    /// Override the judgment call's transport. Tests inject a stub so the
    /// gated suite never spawns a real `claude` subprocess — the claim
    /// commands themselves (`rg`/`git`/`test`/`ls`) still run for real,
    /// which is exactly what the argv-validator tests exercise.
    #[must_use]
    pub fn with_transport(mut self, transport: ModelTransport) -> Self {
        self.judgment = self.judgment.with_transport(transport);
        self
    }

    /// Override with a tier-aware [`MetaTransport`]. Takes precedence over
    /// [`Self::with_transport`] when both are set.
    #[must_use]
    pub fn with_meta_transport(mut self, transport: MetaTransport) -> Self {
        self.judgment = self.judgment.with_meta_transport(transport);
        self
    }

    /// Run preflight for one block step: read `repo`'s
    /// `planning/blocks/<block_id>.json` via `registry`, judge its claims,
    /// execute each admitted one, and return the accumulated
    /// [`BlockPreflight`].
    pub async fn run_for_block(
        &self,
        ctx: &TaskContext,
        registry: &RepoRegistry,
        repo: &str,
        block_id: &str,
    ) -> BlockPreflight {
        let no_record = || BlockPreflight {
            repo: repo.to_string(),
            block_id: block_id.to_string(),
            outcome: PreflightOutcome::SkippedNoRecord,
            claims: Vec::new(),
            claims_dropped: 0,
        };

        let Some(record) = read_block_record(registry, repo, block_id) else {
            return no_record();
        };
        let Ok(repo_root) = registry.resolve(repo) else {
            return no_record();
        };

        let slices = vec![
            InputSlice {
                name: "what".to_string(),
                text: slice_text(&record, "what"),
                max_bytes: self.config.slice_max_bytes,
            },
            InputSlice {
                name: "files".to_string(),
                text: slice_text(&record, "files"),
                max_bytes: self.config.slice_max_bytes,
            },
            InputSlice {
                name: "acceptance_criteria".to_string(),
                text: slice_text(&record, "acceptance_criteria"),
                max_bytes: self.config.slice_max_bytes,
            },
        ];

        let spec = JudgmentSpec {
            identity: PREFLIGHT_NODE_NAME.to_string(),
            json_schema: preflight_claims_json_schema(),
            stable_prompt: PREFLIGHT_PROMPT,
            slices,
            tier: self.config.model_tier,
            max_turns: self.config.max_turns,
        };

        match self.judgment.judge(ctx, spec).await {
            Ok(result) => {
                let all_claims = result.verdict.claims;
                let run_count = all_claims.len().min(self.config.max_claims);
                let claims_dropped = all_claims.len().saturating_sub(self.config.max_claims);
                let timeout = Duration::from_millis(self.config.command_timeout_ms);
                let claims: Vec<ClaimResult> = all_claims
                    .iter()
                    .take(run_count)
                    .map(|claim| {
                        run_claim(claim, &repo_root, self.config.programs.as_deref(), timeout)
                    })
                    .collect();

                BlockPreflight {
                    repo: repo.to_string(),
                    block_id: block_id.to_string(),
                    outcome: PreflightOutcome::Judged {
                        claims: claims.clone(),
                    },
                    claims,
                    claims_dropped,
                }
            }
            Err(err) => BlockPreflight {
                repo: repo.to_string(),
                block_id: block_id.to_string(),
                outcome: PreflightOutcome::Unjudged {
                    error_kind: error_kind_of(&err),
                },
                claims: Vec::new(),
                claims_dropped: 0,
            },
        }
    }
}

/// Test-only helpers exposed to `tests/it/preflight.rs`, which needs to
/// exercise the argv validator and the no-shell runner directly (not only
/// through a full judged run) to keep the safety-boundary tests hermetic
/// and fast.
#[doc(hidden)]
pub mod test_support {
    use super::{execute_argv, validate_argv, ClaimExpect, ClaimResult, ClaimVerdict, JudgedClaim};
    use std::path::Path;
    use std::time::Duration;

    /// Build a claim directly (bypassing the judgment call) for a validator
    /// or runner test.
    #[must_use]
    pub fn claim(
        text: &str,
        load_bearing: bool,
        argv: Vec<String>,
        expect_stdout_contains: Option<&str>,
    ) -> JudgedClaim {
        let (expect, needle) = match expect_stdout_contains {
            Some(needle) => (ClaimExpect::StdoutContains, Some(needle.to_string())),
            None => (ClaimExpect::ExitZero, None),
        };
        JudgedClaim {
            claim: text.to_string(),
            load_bearing,
            argv,
            expect,
            needle,
        }
    }

    /// Build a claim with an explicit `expect` — for `exit_nonzero` cases
    /// [`claim`] cannot express.
    #[must_use]
    pub fn claim_exit_nonzero(text: &str, load_bearing: bool, argv: Vec<String>) -> JudgedClaim {
        JudgedClaim {
            claim: text.to_string(),
            load_bearing,
            argv,
            expect: ClaimExpect::ExitNonzero,
            needle: None,
        }
    }

    /// Run [`super::validate_argv`] directly.
    pub fn run_validate_argv(argv: &[String], programs: Option<&[String]>) -> Result<(), String> {
        validate_argv(argv, programs)
    }

    /// Run [`super::execute_argv`] directly.
    pub fn run_execute_argv(
        argv: &[String],
        repo_root: &Path,
        timeout: Duration,
    ) -> Result<(Option<i32>, Vec<u8>), String> {
        execute_argv(argv, repo_root, timeout)
    }

    /// Run a single claim end to end (validate + execute + classify), as
    /// [`super::PreflightRunner::run_for_block`] does per-claim internally.
    #[must_use]
    pub fn run_one_claim(
        claim: &JudgedClaim,
        repo_root: &Path,
        programs: Option<&[String]>,
        timeout: Duration,
    ) -> ClaimResult {
        super::run_claim(claim, repo_root, programs, timeout)
    }

    #[must_use]
    pub fn verdict_is_held(result: &ClaimResult) -> bool {
        matches!(result.verdict, ClaimVerdict::Held)
    }

    #[must_use]
    pub fn verdict_is_false(result: &ClaimResult) -> bool {
        matches!(result.verdict, ClaimVerdict::False)
    }

    #[must_use]
    pub fn verdict_is_unverifiable(result: &ClaimResult) -> bool {
        matches!(result.verdict, ClaimVerdict::Unverifiable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn rg_pre_is_refused() {
        assert!(validate_argv(&argv(&["rg", "--pre", "x", "y", "."]), None).is_err());
        assert!(validate_argv(&argv(&["rg", "--pre=x", "y", "."]), None).is_err());
        assert!(validate_argv(&argv(&["rg", "--pre-glob", "x", "y", "."]), None).is_err());
    }

    #[test]
    fn rg_search_zip_and_hostname_bin_are_refused() {
        assert!(validate_argv(&argv(&["rg", "-z", "x", "."]), None).is_err());
        assert!(validate_argv(&argv(&["rg", "--search-zip", "x", "."]), None).is_err());
        assert!(validate_argv(&argv(&["rg", "--hostname-bin", "x", "."]), None).is_err());
    }

    #[test]
    fn rg_allowed_flags_pass() {
        assert!(validate_argv(&argv(&["rg", "-n", "-l", "x", "."]), None).is_ok());
        assert!(validate_argv(&argv(&["rg", "-e", "pattern", "."]), None).is_ok());
        assert!(validate_argv(&argv(&["rg", "--glob=*.rs", "x", "."]), None).is_ok());
        assert!(validate_argv(&argv(&["rg", "--files"]), None).is_ok());
    }

    #[test]
    fn git_output_and_global_options_are_refused() {
        assert!(validate_argv(&argv(&["git", "log", "--output=/tmp/x"]), None).is_err());
        assert!(validate_argv(&argv(&["git", "-c", "core.pager=x", "log"]), None).is_err());
        assert!(validate_argv(&argv(&["git", "push"]), None).is_err());
        assert!(validate_argv(&argv(&["git", "log", "-p"]), None).is_err());
    }

    #[test]
    fn git_allowed_subcommands_and_flags_pass() {
        assert!(validate_argv(&argv(&["git", "log", "--oneline", "-n", "5"]), None).is_ok());
        assert!(validate_argv(&argv(&["git", "log", "-5"]), None).is_ok());
        assert!(validate_argv(&argv(&["git", "show", "--format=%H"]), None).is_ok());
        assert!(validate_argv(&argv(&["git", "ls-files", "--"]), None).is_ok());
    }

    #[test]
    fn program_must_be_a_bare_name() {
        assert!(validate_argv(&argv(&["/usr/bin/rg", "x"]), None).is_err());
    }

    #[test]
    fn unknown_program_is_refused() {
        assert!(validate_argv(&argv(&["rm", "/tmp/x"]), None).is_err());
    }

    #[test]
    fn positional_parent_dir_component_is_refused() {
        assert!(validate_argv(&argv(&["rg", "x", "../secret"]), None).is_err());
        assert!(validate_argv(&argv(&["ls", "../secret"]), None).is_err());
    }

    #[test]
    fn test_flag_requires_exactly_one_path() {
        assert!(validate_argv(&argv(&["test", "-e", "some/path"]), None).is_ok());
        assert!(validate_argv(&argv(&["test", "-x", "some/path"]), None).is_err());
        assert!(validate_argv(&argv(&["test", "-e"]), None).is_err());
    }

    #[test]
    fn ls_allows_only_dash_1_and_dash_a() {
        assert!(validate_argv(&argv(&["ls", "-1", "-a", "some/dir"]), None).is_ok());
        assert!(validate_argv(&argv(&["ls", "-l"]), None).is_err());
    }

    #[test]
    fn programs_override_can_only_narrow() {
        // Narrowing to just "test" refuses rg even though rg is compiled in.
        let narrowed = vec!["test".to_string()];
        assert!(validate_argv(&argv(&["rg", "-n", "x", "."]), Some(&narrowed)).is_err());
        assert!(validate_argv(&argv(&["test", "-e", "x"]), Some(&narrowed)).is_ok());

        // Naming a program NOT in the compiled-in table never admits it —
        // the override can only intersect, never union.
        let bogus = vec!["rm".to_string()];
        assert!(validate_argv(&argv(&["rm", "/tmp/x"]), Some(&bogus)).is_err());
    }
}
