//! `CloseBlockNode` — closes this run's block in `planning/state.json`,
//! through mev, under mev's own guards, with validate-then-rollback on a
//! net-new diagnostic
//! (`EN.ticket.wrap-up-closes-the-block`, tasks 3-4).
//!
//! Nothing in this workflow ever flipped `tracks[].blocks[].status` to
//! `"closed"`, so ORCHESTRATION's readiness resolution (which reads block
//! statuses off `depends_on` edges) could never see a Rust-driven chain's
//! first block finish — every dependent block stayed not-ready forever, with
//! no error to explain it. This node closes the block `WrapUpNode` just
//! finished a run for, through `mev::set_block_status`
//! (`mev/src/lib.rs:977`) — **this module writes no `state.json` JSON of its
//! own**; mev owns the corpus write, the snapshot, and the restore, and a
//! second copy of that contract here would drift.
//!
//! ## The two CLI-only guards, taken explicitly
//!
//! Confirmed 2026-08-20 (task 2 of this ticket): `mev::set_block_status` has
//! neither of the two guards `mev main.rs`'s `set-block-status` CLI command
//! wraps it in:
//!
//! - **The advisory `.mev-emit.lock`** (`mev::brain::lock::acquire_lock`,
//!   public) — `set_block_status` chains into `emit_state` on a successful
//!   write, so racing it against a concurrent `mev emit-state --write`
//!   regenerates derived views mid-edit. Held for the duration of the write
//!   here and released on every exit path (`LockGuard`'s `Drop`).
//! - **The D71 operator gate** (`E_BLOCK_OPERATOR_GATED`) — built in
//!   `main.rs` on a *private* helper, `block_has_unmet_operator_gate`
//!   (`main.rs:1319`), so it cannot be called directly from here.
//!   [`block_has_unmet_operator_gate`] below is a reimplementation against
//!   only the *public* mev items the private helper itself uses
//!   (`mev::brain::config::find_brain_config`,
//!   `mev::brain::state::{discover_state_files, load_state, BlockedBy}`).
//!   Taken unconditionally here, not only on a transition to `in_progress`
//!   (the upstream check's literal condition): this node is an autonomous
//!   caller closing blocks with no human in the loop — exactly what D71
//!   exists to constrain — and the condition is one string comparison away
//!   from applying to `"closed"` too.
//!
//! A guard that exists only in a CLI is not a guard on a library, and this
//! node is the first in-process caller of `set_block_status`. The durable
//! fix — moving both guards into the library so no caller can bypass them —
//! is filed upstream as `mev:MV.ticket.set-block-status-cli-only-guards`
//! (`core/mev/planning/blocks/MV.ticket.set-block-status-cli-only-guards.json`)
//! and referenced here rather than duplicated further.
//!
//! ## Validate-then-rollback on the authored write, composed from mev's own
//! ## primitives (task 4)
//!
//! **Confirmed by reading `mev::set_block_status` (`mev/src/lib.rs:977`):
//! the authored `state.json` write it makes is NOT already wrapped in
//! `apply_with_rollback_on_regression`** — that guard exists (`lib.rs:1615`)
//! but is only ever applied, internally, to the *derived* views
//! `set_block_status` chains into via `emit_state` on success (master-plan
//! tables, project caches, tier rollups, boards, …). The one authored write —
//! the block's own `status` field — goes straight through `apply_plan` with
//! no regression check of its own. So this node's guard is NOT redundant
//! with mev's: it covers exactly the one write mev leaves unguarded, and
//! must NOT be widened to re-wrap the derived-view writes mev already
//! protects — that would be a second, drifting copy of a contract mev owns.
//!
//! The guard: snapshot the pre-write revision counter for the target
//! `state.json`, `mev::validate_brain_state` before, call
//! `mev::set_block_status`, `mev::validate_brain_state` after. Attribution
//! is by NET-NEW DELTA, never by path — a write can surface a diagnostic on
//! a *different* file, so a path-scoped check would miss exactly the class
//! this exists to catch, and a diagnostic present in both sets (a
//! pre-existingly red corpus) must never trigger a rollback. On a net-new
//! diagnostic, restore via the exact revision `apply_plan` recorded of the
//! pre-write bytes (`mev::read_revision` + `mev::write_atomic`) — the same
//! primitives `mev state-history --restore` itself composes
//! (`mev/src/main.rs:1080-1099`), called in-process here rather than shelled
//! out to. **No byte snapshot of our own is taken** — the restore source is
//! always mev's own `.mev-history` store.
//!
//! ## Outcomes
//!
//! Every branch is a NAMED, non-error [`CloseOutcome`] stamped into
//! `ctx.nodes["CloseBlockNode"]` — a `CloseBlockNode` returning `NodeError`
//! is reserved for conditions that should halt the whole walk, and "no such
//! block" / "gate refused" / "run was blocked" are all legitimate outcomes
//! of an otherwise-healthy run, not walk failures:
//!
//! - `CLOSED:<repo:id>` — the block closed cleanly.
//! - `NOT_FOUND` — `block_id` named no registered block. Not an error: a
//!   stale or hand-typed `block_id` must not fail the run.
//! - `REJECTED:<repo:id>` — a net-new `[E_`/`[W_` diagnostic appeared after
//!   the write; rolled back via mev's own `.mev-history` revision, block
//!   stays open.
//! - `LOCK_HELD` — the advisory lock is held by another process; the
//!   holder's pid is carried in the outcome, never silently waited out.
//! - `OPERATOR_GATED` — the block carries an unmet D71 operator edge.
//! - `UNVALIDATED:<reason>` — mev itself could not be consulted (no
//!   `brain.toml`, a `state.json` failed to load, or a raised `E_BLOCK_*`
//!   diagnostic other than not-found) — a visible degrade, never a silent
//!   pass.
//! - `SKIPPED` — the close was not attempted at all: a blocked run, no
//!   `block_id` on this run, no worktree to resolve a root from, or no
//!   `planning/state.json` under it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use engine_contract::TaskContext;
use serde_json::json;

use mev::brain::config::{find_brain_config, find_brain_root};
use mev::brain::lock::{acquire_lock, LockError, DEFAULT_LOCK_TIMEOUT};
use mev::brain::state::{discover_state_files, load_state, BlockedBy};
use mev::{list_revisions, read_revision, write_atomic, Diagnostic, Report, Severity};

use crate::node::{Node, NodeError};

use super::schema::SDLCState;
use super::{get_result, put_result};

/// Repo slug substituted when the inbound `SDLC_FLOW` event carries no
/// `repo` — i.e. `setup::resolve_target_root` fell back to
/// `current_dir()`, meaning the run targeted this engine's own repo.
/// Matches this repo's own `brain.toml` `[[repos]]` slug (`slug =
/// "engine-rs"`).
pub const DEFAULT_REPO_SLUG: &str = "engine-rs";

/// A block-close attempt's outcome. See the module doc comment for the
/// full vocabulary; this type is what [`CloseOutcome::label`] renders into
/// the stamped `outcome` string and what task 5 reads to stamp
/// `state_write_validated`/`state_write_rejected` into the committed state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloseOutcome {
    /// The block closed cleanly. Carries the full `"<repo>:<id>"` key.
    Closed { key: String },
    /// `block_id` named no registered block (`E_BLOCK_NOT_FOUND`).
    NotFound { key: String },
    /// A net-new diagnostic appeared after the write and mev's own history
    /// was used to restore; the block stayed open.
    Rejected { key: String, net_new: Vec<String> },
    /// The advisory `.mev-emit.lock` is held by another process.
    LockHeld { holder_pid: u32, waited_secs: u64 },
    /// The block carries an unmet D71 operator `depends_on` edge.
    OperatorGated { key: String },
    /// mev could not be consulted around the write, or reported an error
    /// diagnostic that is not `E_BLOCK_NOT_FOUND`.
    Unvalidated { reason: String },
    /// The close was not attempted at all.
    Skipped { reason: String },
}

impl CloseOutcome {
    /// The stamped `outcome` string — the ticket's outcome vocabulary
    /// verbatim: `CLOSED:<id>` / `NOT_FOUND` / `REJECTED:<id>` /
    /// `LOCK_HELD` / `OPERATOR_GATED` / `UNVALIDATED:<reason>` / `SKIPPED`.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Closed { key } => format!("CLOSED:{key}"),
            Self::NotFound { .. } => "NOT_FOUND".to_string(),
            Self::Rejected { key, .. } => format!("REJECTED:{key}"),
            Self::LockHeld { .. } => "LOCK_HELD".to_string(),
            Self::OperatorGated { .. } => "OPERATOR_GATED".to_string(),
            Self::Unvalidated { reason } => format!("UNVALIDATED:{reason}"),
            Self::Skipped { .. } => "SKIPPED".to_string(),
        }
    }

    /// Human-readable detail — the "naming the holder" / "naming the gate"
    /// half of the acceptance criteria, kept out of `label()` so the
    /// stamped `outcome` string stays a short, matchable token.
    #[must_use]
    pub fn detail(&self) -> String {
        match self {
            Self::Closed { key } => format!("closed block '{key}'"),
            Self::NotFound { key } => format!("no registered block matches '{key}'"),
            Self::Rejected { key, net_new } => format!(
                "close of '{key}' introduced net-new diagnostic(s), rolled back via mev's \
                 history: {}",
                net_new.join("; ")
            ),
            Self::LockHeld {
                holder_pid,
                waited_secs,
            } => format!(
                "advisory .mev-emit.lock held by pid {holder_pid} after waiting {waited_secs}s; \
                 retry once it finishes"
            ),
            Self::OperatorGated { key } => format!(
                "'{key}' carries an unmet D71 operator depends_on edge \
                 (E_BLOCK_OPERATOR_GATED) — refused, not silently closed"
            ),
            Self::Unvalidated { reason } => reason.clone(),
            Self::Skipped { reason } => reason.clone(),
        }
    }

    /// The `"<repo>:<id>"` key this outcome concerns, if it got far enough
    /// to have one.
    #[must_use]
    pub fn key(&self) -> Option<&str> {
        match self {
            Self::Closed { key }
            | Self::NotFound { key }
            | Self::Rejected { key, .. }
            | Self::OperatorGated { key } => Some(key.as_str()),
            Self::LockHeld { .. } | Self::Unvalidated { .. } | Self::Skipped { .. } => None,
        }
    }

    /// `true` only for a validated, successful close — what task 5 stamps
    /// as `state_write_validated`.
    #[must_use]
    pub fn is_validated(&self) -> bool {
        matches!(self, Self::Closed { .. })
    }

    /// `true` only for a rolled-back close — what task 5 stamps as
    /// `state_write_rejected`.
    #[must_use]
    pub fn is_rejected(&self) -> bool {
        matches!(self, Self::Rejected { .. })
    }
}

/// The node identity [`CloseBlockNode`] reads the committed `SDLCState`
/// from by default — `WrapUpNode`, SDLC_FLOW's wrap-up node. `EN.11.N`
/// task 5 parameterizes this (see [`CloseBlockNode::with_state_source`])
/// so SDLC_TASK's registry can point the SAME node at `LeanBookkeepNode`
/// instead, without forking a second close-block implementation.
pub const DEFAULT_STATE_SOURCE: &str = "WrapUpNode";

/// Deterministic node: closes `WrapUpNode`'s (or, under
/// [`CloseBlockNode::with_state_source`], another engine's terminal node's)
/// block through `mev::set_block_status`, under the advisory lock and the
/// D71 operator gate, both taken explicitly (see module doc comment).
pub struct CloseBlockNode {
    lock_timeout: Duration,
    state_source: &'static str,
}

impl CloseBlockNode {
    #[must_use]
    pub fn new() -> Self {
        Self {
            lock_timeout: DEFAULT_LOCK_TIMEOUT,
            state_source: DEFAULT_STATE_SOURCE,
        }
    }

    /// Override the advisory-lock wait bound. Tests use a short timeout so
    /// a `LOCK_HELD` fixture doesn't sit for the real 3s default.
    #[must_use]
    pub fn with_lock_timeout(mut self, timeout: Duration) -> Self {
        self.lock_timeout = timeout;
        self
    }

    /// Override the `ctx.nodes` identity this node reads its committed
    /// `SDLCState` (and, for the full-run guard, the sibling `full_run`
    /// flag) from. Defaults to [`DEFAULT_STATE_SOURCE`] (`"WrapUpNode"`),
    /// preserving every existing `sdlc_flow` caller's behavior exactly.
    /// `EN.11.N`'s SDLC_TASK registry constructs this with
    /// `"LeanBookkeepNode"` — `LeanBookkeepNode` is not `WrapUpNode`, so
    /// without this override `CloseBlockNode` would find nothing under the
    /// default identity and every SDLC_TASK close would silently no-op.
    #[must_use]
    pub fn with_state_source(mut self, source: &'static str) -> Self {
        self.state_source = source;
        self
    }
}

impl Default for CloseBlockNode {
    fn default() -> Self {
        Self::new()
    }
}

/// Read `self.state_source`'s stamped `SDLCState` back out of `ctx` — "the
/// state [the terminal node] just wrote", per this task's spec. `None`
/// when that node has not run (e.g. a unit test driving this node in
/// isolation) or its `state` field fails to parse.
fn state_from_source(ctx: &TaskContext, state_source: &str) -> Option<SDLCState> {
    let result = get_result(ctx, state_source)?;
    let state_value = result.get("state")?.clone();
    serde_json::from_value(state_value).ok()
}

/// Read `self.state_source`'s stamped `full_run` flag, if present.
/// Defaults `true` when absent — every `sdlc_flow` `WrapUpNode` stamp
/// (today) carries no such field, and `sdlc_flow`'s own runs have no
/// partial-run guard to preserve, so an absent flag must mean "proceed",
/// not "skip". Only `LeanBookkeepNode` (`EN.11.N` task 5) stamps this key,
/// `false` for a partial `task_range` run.
fn full_run_from_source(ctx: &TaskContext, state_source: &str) -> bool {
    get_result(ctx, state_source)
        .and_then(|result| result.get("full_run"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true)
}

/// Read the worktree path stamped by `SetupWorktreeNode`, if present.
fn worktree_path(ctx: &TaskContext) -> Option<String> {
    get_result(ctx, "SetupWorktreeNode")
        .and_then(|value| value.get("worktree_path"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

/// Resolve the mev repo slug for this run's key: the inbound event's
/// `repo` when present, else [`DEFAULT_REPO_SLUG`] — mirroring
/// `setup::resolve_target_root`'s own default (`event.repo` absent ->
/// this engine's own repo).
fn repo_slug(ctx: &TaskContext) -> String {
    ctx.event
        .get("repo")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| DEFAULT_REPO_SLUG.to_string())
}

/// Reimplementation of `mev main.rs`'s private
/// `block_has_unmet_operator_gate` (`main.rs:1319`) against only the
/// *public* mev items the original uses — see the module doc comment for
/// why a copy is needed here and the upstream ticket that removes the
/// need for one.
///
/// `None` means "could not determine" (bad key shape, `brain.toml`
/// unreadable, the block not found, or a `state.json` that failed to
/// load) — callers must treat that as "don't gate" and let
/// `mev::set_block_status`'s own diagnostics surface the real problem,
/// never as an implicit pass on the gate.
fn block_has_unmet_operator_gate(root: &Path, key: &str) -> Option<bool> {
    let (repo_slug, block_id) = key.split_once(':')?;
    let config = find_brain_config(root).ok()?;
    let (sources, _diags) = discover_state_files(root, &config);
    for src in &sources {
        if src.repo_slug != repo_slug {
            continue;
        }
        let Ok(file) = load_state(&src.abs_path) else {
            continue;
        };
        for track in &file.tracks {
            for block in &track.blocks {
                if block.id == block_id {
                    return Some(
                        block
                            .depends_on
                            .iter()
                            .any(|dep| matches!(dep, BlockedBy::Operator(_))),
                    );
                }
            }
        }
    }
    None
}

/// Resolve the absolute path of the `state.json` this close will write:
/// the loaded `StateSource` whose `repo_slug` matches `repo_slug`, found the
/// same way [`block_has_unmet_operator_gate`] resolves a block's home file.
/// `None` when no state.json is registered for that repo slug — the caller
/// falls back to letting `mev::set_block_status`'s own diagnostics (e.g.
/// `E_BLOCK_NOT_FOUND`) explain why, rather than failing here first.
fn resolve_state_path(root: &Path, repo_slug: &str) -> Option<PathBuf> {
    let config = find_brain_config(root).ok()?;
    let (sources, _diags) = discover_state_files(root, &config);
    sources
        .into_iter()
        .find(|src| src.repo_slug == repo_slug)
        .map(|src| src.abs_path)
}

/// A diagnostic's identity for net-new comparison: severity, file, locator,
/// and message together — `Diagnostic` carries no `PartialEq`, and identity
/// must include `file` precisely because attribution is by delta, never by
/// path (a write can surface a diagnostic on a *different* file than the
/// one it touched).
fn diagnostic_identity(d: &Diagnostic) -> String {
    format!(
        "{:?}|{}|{}|{}",
        d.severity,
        d.file.display(),
        d.locator,
        d.message
    )
}

/// Human-readable rendering of a diagnostic for a `REJECTED` outcome's
/// `net_new` list.
fn render_diagnostic(d: &Diagnostic) -> String {
    format!("[{}] {} ({})", d.locator, d.message, d.file.display())
}

/// Diagnostics present in `after` but not in `before`, by
/// [`diagnostic_identity`]. A diagnostic present in both — a pre-existingly
/// red corpus — is never net-new, so a permanently red corpus can still
/// close a block.
fn net_new_diagnostics(before: &Report, after: &Report) -> Vec<String> {
    let before_ids: HashSet<String> = before.diagnostics.iter().map(diagnostic_identity).collect();
    after
        .diagnostics
        .iter()
        .filter(|d| !before_ids.contains(&diagnostic_identity(d)))
        .map(render_diagnostic)
        .collect()
}

/// Classify a completed `mev::set_block_status` [`Report`] into a
/// [`CloseOutcome`]. `E_BLOCK_NOT_FOUND` maps to [`CloseOutcome::NotFound`]
/// (not an error — see the ticket's acceptance criteria); any other
/// error-severity diagnostic maps to [`CloseOutcome::Unvalidated`] so mev's
/// own words reach the caller rather than being swallowed; a report with no
/// errors is a clean [`CloseOutcome::Closed`] (this includes the
/// already-closed fixed point, where `set_block_status` plans no action and
/// raises no diagnostic).
fn classify_report(key: &str, report: &Report) -> CloseOutcome {
    let not_found = report
        .diagnostics
        .iter()
        .any(|d| d.locator == "E_BLOCK_NOT_FOUND");
    if not_found {
        return CloseOutcome::NotFound {
            key: key.to_string(),
        };
    }

    let errors: Vec<String> = report
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| format!("[{}] {}", d.locator, d.message))
        .collect();
    if !errors.is_empty() {
        return CloseOutcome::Unvalidated {
            reason: errors.join("; "),
        };
    }

    CloseOutcome::Closed {
        key: key.to_string(),
    }
}

/// The guarded close itself: operator gate, then the advisory lock (held
/// across the write, released via `LockGuard`'s `Drop` on every exit path
/// below), then `mev::set_block_status(root, key, "closed", true)` wrapped
/// in the validate-then-rollback guard (module doc comment, "task 4").
///
/// Production always calls this through [`attempt_close`], which supplies
/// real `mev::validate_brain_state` as `validate`. Tests inject a fake
/// `validate` that returns two canned [`Report`]s (baseline, then after) so
/// the net-new and pre-existing-diagnostic branches are reachable without
/// needing a real corpus defect to appear as a side effect of a real close —
/// while the write, the lock, the operator gate, and the restore itself
/// still go through real mev calls and real files, per the testing strategy.
fn attempt_close_with_validator(
    root: &Path,
    key: &str,
    repo_slug: &str,
    lock_timeout: Duration,
    mut validate: impl FnMut(&Path) -> Result<Report, String>,
) -> CloseOutcome {
    if let Some(true) = block_has_unmet_operator_gate(root, key) {
        return CloseOutcome::OperatorGated {
            key: key.to_string(),
        };
    }

    let _lock_guard = match acquire_lock(root, lock_timeout) {
        Ok(guard) => guard,
        Err(LockError::Held {
            holder_pid,
            waited_secs,
            ..
        }) => {
            return CloseOutcome::LockHeld {
                holder_pid,
                waited_secs,
            };
        }
        Err(LockError::Io { path, source }) => {
            return CloseOutcome::Unvalidated {
                reason: format!(
                    "could not acquire advisory lock at {}: {source}",
                    path.display()
                ),
            };
        }
    };

    // The state.json this close will (or will not) write. `None` means no
    // state.json is registered for this repo slug at all — fall through to
    // `set_block_status` directly and let its own diagnostics (typically
    // `E_BLOCK_NOT_FOUND`) explain why, rather than failing on our own
    // resolution first.
    let Some(state_path) = resolve_state_path(root, repo_slug) else {
        return match mev::set_block_status(root, key, "closed", true) {
            Ok(report) => classify_report(key, &report),
            Err(err) => CloseOutcome::Unvalidated {
                reason: format!("mev::set_block_status errored: {err}"),
            },
        };
    };

    // Revision counter before the write — if the write actually happens,
    // `apply_plan` records the pre-write bytes as the very next revision
    // (mev/src/brain/emit.rs:3610-3634), so this is the restore target on
    // rollback.
    let seq_before = list_revisions(&state_path)
        .map(|revs| revs.last().map(|r| r.seq).unwrap_or(0))
        .unwrap_or(0);

    let baseline = match validate(root) {
        Ok(report) => report,
        Err(err) => {
            return CloseOutcome::Unvalidated {
                reason: format!("could not validate corpus before close: {err}"),
            };
        }
    };

    let close_report = match mev::set_block_status(root, key, "closed", true) {
        Ok(report) => report,
        Err(err) => {
            return CloseOutcome::Unvalidated {
                reason: format!("mev::set_block_status errored: {err}"),
            };
        }
    };
    let outcome = classify_report(key, &close_report);
    let CloseOutcome::Closed { .. } = &outcome else {
        // NotFound / Unvalidated: `set_block_status` took no action on this
        // key, so there is nothing to have regressed and nothing to
        // validate around.
        return outcome;
    };

    // Did a real write happen, or was this the already-closed fixed point
    // (no action planned, nothing on disk changed)? `set_block_status`
    // extends its report with `apply_plan`'s own diagnostics, and
    // `I_EMIT_WROTE` is only pushed for an action that was actually
    // applied.
    let wrote = close_report
        .diagnostics
        .iter()
        .any(|d| d.locator == "I_EMIT_WROTE" && d.file == state_path);
    if !wrote {
        return outcome;
    }

    let after = match validate(root) {
        Ok(report) => report,
        Err(err) => {
            return CloseOutcome::Unvalidated {
                reason: format!("could not validate corpus after close: {err}"),
            };
        }
    };

    let net_new = net_new_diagnostics(&baseline, &after);
    if net_new.is_empty() {
        return outcome;
    }

    // Net-new diagnostic(s) — roll back via mev's own `.mev-history` undo,
    // never a hand-rolled byte snapshot. The revision counter grew by
    // exactly one on a real write (checked above), so `seq_before + 1` is
    // the revision `apply_plan` recorded of the pre-write bytes.
    let restore_seq = seq_before + 1;
    match read_revision(&state_path, restore_seq)
        .map_err(|e| e.to_string())
        .and_then(|bytes| write_atomic(&state_path, &bytes).map_err(|e| e.to_string()))
    {
        Ok(()) => CloseOutcome::Rejected {
            key: key.to_string(),
            net_new,
        },
        Err(reason) => CloseOutcome::Unvalidated {
            reason: format!(
                "close of '{key}' introduced net-new diagnostic(s) but the mev-history \
                 restore failed ({reason}) — {} may be left in the WRITTEN (not rolled \
                 back) state; net-new: {}",
                state_path.display(),
                net_new.join("; ")
            ),
        },
    }
}

/// Production entry point: [`attempt_close_with_validator`] with real
/// `mev::validate_brain_state` as the validator.
fn attempt_close(root: &Path, key: &str, repo_slug: &str, lock_timeout: Duration) -> CloseOutcome {
    attempt_close_with_validator(root, key, repo_slug, lock_timeout, |root| {
        mev::validate_brain_state(root).map_err(|e| e.to_string())
    })
}

impl CloseBlockNode {
    /// The full skip-check -> root-resolution -> guarded-close pipeline,
    /// split out from `process` so it never has to touch `ctx` mutably —
    /// makes the skip/outcome logic trivially unit-testable without a
    /// `#[tokio::test]`.
    fn evaluate(&self, ctx: &TaskContext) -> CloseOutcome {
        let Some(state) = state_from_source(ctx, self.state_source) else {
            return CloseOutcome::Skipped {
                reason: format!("{} has not run — nothing to close", self.state_source),
            };
        };

        // A bailed/blocked run must not close its block — the whole point
        // of EN.ticket.final-validation-failure-must-block. This engine
        // writes only "blocked" for a terminal run (never "bailed" — see
        // `schema::derive_committed_status`'s doc comment), so that is the
        // one status value this checks.
        if state.global_status == "blocked" {
            return CloseOutcome::Skipped {
                reason: format!(
                    "run ended blocked ({}) — a red run must not close its block",
                    state.bail_reason.as_deref().unwrap_or("no reason recorded")
                ),
            };
        }

        // A failed SDLC_TASK reconcile must not close its block either —
        // base-template D56 CALL 2 makes `reconcile_failed` TERMINAL, and
        // closing the block on it would be exactly what that call
        // forbids. Distinct branch (not folded into the `"blocked"` arm
        // above) so the skip reason names D56 rather than reusing the
        // generic "run ended blocked" wording.
        if state.global_status == "reconcile_failed" {
            return CloseOutcome::Skipped {
                reason: format!(
                    "run ended reconcile_failed ({}) — base-template D56 CALL 2 makes this \
                     terminal; a failed reconcile must not close its block",
                    state.bail_reason.as_deref().unwrap_or("no reason recorded")
                ),
            };
        }

        // A partial `task_range` run must not close its block either — the
        // JS engine's `fullRun` guard (`sdlc-task.js`): "the reconcile and
        // the block close only happen on a run covering EVERY task in the
        // spec". `LeanBookkeepNode` (`EN.11.N` task 5) is the only stamper
        // of this flag today; every `sdlc_flow` `WrapUpNode` stamp carries
        // no `full_run` key at all, so [`full_run_from_source`] defaults
        // `true` there and this branch stays inert for SDLC_FLOW runs.
        if !full_run_from_source(ctx, self.state_source) {
            return CloseOutcome::Skipped {
                reason: "run covered only a task_range subset, not the full spec — a partial \
                         run must not close its block"
                    .to_string(),
            };
        }

        let Some(block_id) = state.block_id.clone() else {
            return CloseOutcome::Skipped {
                reason: "no block_id on this run".to_string(),
            };
        };

        let Some(worktree) = worktree_path(ctx) else {
            return CloseOutcome::Skipped {
                reason: "no worktree_path stamped by SetupWorktreeNode — cannot resolve a \
                         brain root"
                    .to_string(),
            };
        };
        let worktree_path_buf = PathBuf::from(&worktree);

        if !worktree_path_buf
            .join("planning")
            .join("state.json")
            .exists()
        {
            return CloseOutcome::Skipped {
                reason: format!("no planning/state.json under {worktree}"),
            };
        }

        let root = match find_brain_root(&worktree_path_buf) {
            Ok(root) => root,
            Err(err) => {
                return CloseOutcome::Unvalidated {
                    reason: format!(
                        "mev unavailable: could not resolve brain.toml from {worktree}: {err}"
                    ),
                };
            }
        };

        let repo = repo_slug(ctx);
        let key = format!("{repo}:{block_id}");
        attempt_close(&root, &key, &repo, self.lock_timeout)
    }
}

#[async_trait::async_trait]
impl Node for CloseBlockNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let outcome = self.evaluate(&ctx);

        let mut result = json!({
            "outcome": outcome.label(),
            "detail": outcome.detail(),
            "state_write_validated": outcome.is_validated(),
            "state_write_rejected": outcome.is_rejected(),
        });
        if let Some(key) = outcome.key() {
            result["block_key"] = json!(key);
        }

        put_result(&mut ctx, "CloseBlockNode", result);
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "CloseBlockNode"
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;

    use super::*;

    /// This module must never write `state.json` directly — every write
    /// goes through `mev::set_block_status`. Guards against a future
    /// "simplification" back to hand-editing the corpus (testing_strategy).
    #[test]
    fn never_writes_state_json_directly() {
        // Scan only the non-test portion of this file — the `#[cfg(test)]`
        // fixtures below legitimately write JSON files to set the scene,
        // and this guard's own assert message names the banned call, which
        // would otherwise trip a naive whole-file substring check.
        let source = include_str!("close_block.rs");
        let production_code = source
            .split_once("\n#[cfg(test)]\n")
            .map(|(before, _)| before)
            .expect("this module has a #[cfg(test)] boundary");
        let banned = ["fs", "::", "write"].concat();
        assert!(
            !production_code.contains(&banned),
            "CloseBlockNode must delegate every state.json write to \
             mev::set_block_status — found a direct filesystem write call in \
             this module's non-test code"
        );
    }

    // --- fixture builders ---------------------------------------------

    /// A minimal brain root: `brain.toml` naming one repo, plus that
    /// repo's `planning/state.json` carrying the given `(id, status,
    /// depends_on_operator)` blocks in a single track. Returns the temp
    /// dir (kept alive for the test's duration) and the repo's own
    /// checkout path (`<root>/<repo>`).
    fn brain_fixture(
        repo: &str,
        blocks: &[(&str, &str, bool)],
    ) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo_dir = dir.path().join(repo);
        std::fs::create_dir_all(repo_dir.join("planning")).expect("mkdir");

        std::fs::write(
            dir.path().join("brain.toml"),
            format!("[[repos]]\nslug = \"{repo}\"\nrepo_path = \"{repo}\"\n"),
        )
        .expect("write brain.toml");

        let block_json: Vec<String> = blocks
            .iter()
            .map(|(id, status, operator_gated)| {
                let depends_on = if *operator_gated {
                    r#"[{ "type": "operator", "slug": "gate-slug", "exit": "human says go", "start": "/begin-session gate-slug" }]"#
                        .to_string()
                } else {
                    "[]".to_string()
                };
                format!(
                    r#"{{ "id": "{id}", "title": "{id}", "status": "{status}", "wave": 1, "depends_on": {depends_on} }}"#
                )
            })
            .collect();

        let raw = format!(
            r#"{{ "repo": "{repo}", "kind": "project", "updated": "2026-08-20",
  "focus": {{ "now": [], "next": [], "blocked": [] }},
  "tracks": [{{ "title": "P1", "blocks": [{}] }}] }}"#,
            block_json.join(",\n")
        );
        std::fs::write(repo_dir.join("planning").join("state.json"), raw)
            .expect("write state.json");

        (dir, repo_dir)
    }

    fn ctx_for(
        worktree: &Path,
        repo: Option<&str>,
        block_id: Option<&str>,
        global_status: &str,
    ) -> TaskContext {
        let mut state = SDLCState::new("some-spec");
        state.block_id = block_id.map(str::to_string);
        state.global_status = global_status.to_string();
        if global_status == "blocked" {
            state.bail_reason = Some("MAJOR_BAIL: max attempts reached".to_string());
        }

        let mut event = json!({ "spec_slug": "some-spec" });
        if let Some(repo) = repo {
            event["repo"] = json!(repo);
        }

        let mut ctx = TaskContext {
            event,
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );
        ctx.nodes.insert(
            "WrapUpNode".to_string(),
            json!({ "state": serde_json::to_value(&state).unwrap() }),
        );
        ctx
    }

    // --- outcome-label / detail shape -----------------------------------

    #[test]
    fn outcome_labels_match_the_ticket_vocabulary() {
        assert_eq!(
            CloseOutcome::Closed {
                key: "mev:MV.10.A".to_string()
            }
            .label(),
            "CLOSED:mev:MV.10.A"
        );
        assert_eq!(
            CloseOutcome::NotFound {
                key: "mev:MV.10.A".to_string()
            }
            .label(),
            "NOT_FOUND"
        );
        assert_eq!(
            CloseOutcome::Rejected {
                key: "mev:MV.10.A".to_string(),
                net_new: vec!["[E_X] boom".to_string()],
            }
            .label(),
            "REJECTED:mev:MV.10.A"
        );
        assert_eq!(
            CloseOutcome::LockHeld {
                holder_pid: 42,
                waited_secs: 3
            }
            .label(),
            "LOCK_HELD"
        );
        assert_eq!(
            CloseOutcome::OperatorGated {
                key: "mev:MV.10.A".to_string()
            }
            .label(),
            "OPERATOR_GATED"
        );
        assert_eq!(
            CloseOutcome::Unvalidated {
                reason: "no brain.toml".to_string()
            }
            .label(),
            "UNVALIDATED:no brain.toml"
        );
        assert_eq!(
            CloseOutcome::Skipped {
                reason: "blocked".to_string()
            }
            .label(),
            "SKIPPED"
        );
    }

    #[test]
    fn lock_held_detail_names_the_holder() {
        let outcome = CloseOutcome::LockHeld {
            holder_pid: 4242,
            waited_secs: 3,
        };
        assert!(outcome.detail().contains("4242"));
    }

    #[test]
    fn operator_gated_detail_names_the_gate() {
        let outcome = CloseOutcome::OperatorGated {
            key: "acme:AC.1".to_string(),
        };
        assert!(outcome.detail().contains("E_BLOCK_OPERATOR_GATED"));
    }

    #[test]
    fn validated_and_rejected_flags_are_mutually_exclusive_and_precise() {
        let closed = CloseOutcome::Closed {
            key: "acme:AC.1".to_string(),
        };
        assert!(closed.is_validated());
        assert!(!closed.is_rejected());

        let rejected = CloseOutcome::Rejected {
            key: "acme:AC.1".to_string(),
            net_new: vec![],
        };
        assert!(!rejected.is_validated());
        assert!(rejected.is_rejected());

        for other in [
            CloseOutcome::NotFound {
                key: "acme:AC.1".to_string(),
            },
            CloseOutcome::LockHeld {
                holder_pid: 1,
                waited_secs: 1,
            },
            CloseOutcome::OperatorGated {
                key: "acme:AC.1".to_string(),
            },
            CloseOutcome::Unvalidated {
                reason: "x".to_string(),
            },
            CloseOutcome::Skipped {
                reason: "x".to_string(),
            },
        ] {
            assert!(!other.is_validated());
            assert!(!other.is_rejected());
        }
    }

    // --- skip conditions (evaluate, no real mev call reached) -----------

    #[tokio::test]
    async fn skips_when_wrap_up_has_not_run() {
        let ctx = TaskContext {
            event: json!({ "spec_slug": "some-spec" }),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        let node = CloseBlockNode::new();
        let out = node.process(ctx).await.expect("process succeeds");
        let result = get_result(&out, "CloseBlockNode").expect("stamped");
        assert_eq!(result["outcome"], json!("SKIPPED"));
        assert_eq!(result["state_write_validated"], json!(false));
        assert_eq!(result["state_write_rejected"], json!(false));
    }

    #[tokio::test]
    async fn skips_a_blocked_run_and_does_not_close() {
        let (_dir, repo_dir) = brain_fixture("acme", &[("AC.1", "open", false)]);
        let ctx = ctx_for(&repo_dir, Some("acme"), Some("AC.1"), "blocked");
        let node = CloseBlockNode::new();
        let out = node.process(ctx).await.expect("process succeeds");
        let result = get_result(&out, "CloseBlockNode").expect("stamped");
        assert_eq!(result["outcome"], json!("SKIPPED"));

        // Verify on disk: the block must still be "open".
        let raw = std::fs::read_to_string(repo_dir.join("planning/state.json")).unwrap();
        assert!(raw.contains("\"open\""));
        assert!(!raw.contains("\"closed\""));
    }

    #[tokio::test]
    async fn skips_a_reconcile_failed_run_and_does_not_close() {
        let (_dir, repo_dir) = brain_fixture("acme", &[("AC.1", "open", false)]);
        let ctx = ctx_for(&repo_dir, Some("acme"), Some("AC.1"), "reconcile_failed");
        let node = CloseBlockNode::new();
        let out = node.process(ctx).await.expect("process succeeds");
        let result = get_result(&out, "CloseBlockNode").expect("stamped");
        assert_eq!(result["outcome"], json!("SKIPPED"));

        // Verify on disk: the block must still be "open".
        let raw = std::fs::read_to_string(repo_dir.join("planning/state.json")).unwrap();
        assert!(raw.contains("\"open\""));
        assert!(!raw.contains("\"closed\""));
    }

    #[test]
    fn evaluate_skips_reconcile_failed_with_a_reason_naming_d56() {
        let (_dir, repo_dir) = brain_fixture("acme", &[("AC.1", "open", false)]);
        let ctx = ctx_for(&repo_dir, Some("acme"), Some("AC.1"), "reconcile_failed");
        let node = CloseBlockNode::new();
        let outcome = node.evaluate(&ctx);
        match outcome {
            CloseOutcome::Skipped { reason } => {
                assert!(reason.contains("D56"), "reason should name D56: {reason}");
                assert!(reason.contains("reconcile_failed"));
            }
            other => panic!("expected Skipped, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn skips_when_no_block_id_on_the_run() {
        let (_dir, repo_dir) = brain_fixture("acme", &[("AC.1", "open", false)]);
        let ctx = ctx_for(&repo_dir, Some("acme"), None, "done");
        let node = CloseBlockNode::new();
        let out = node.process(ctx).await.expect("process succeeds");
        let result = get_result(&out, "CloseBlockNode").expect("stamped");
        assert_eq!(result["outcome"], json!("SKIPPED"));
    }

    #[tokio::test]
    async fn skips_when_no_planning_state_json() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = ctx_for(dir.path(), Some("acme"), Some("AC.1"), "done");
        let node = CloseBlockNode::new();
        let out = node.process(ctx).await.expect("process succeeds");
        let result = get_result(&out, "CloseBlockNode").expect("stamped");
        assert_eq!(result["outcome"], json!("SKIPPED"));
    }

    // --- real close via mev ---------------------------------------------

    #[tokio::test]
    async fn closes_a_clean_block_through_mev() {
        let (_dir, repo_dir) = brain_fixture("acme", &[("AC.1", "open", false)]);
        let ctx = ctx_for(&repo_dir, Some("acme"), Some("AC.1"), "done");
        let node = CloseBlockNode::new();
        let out = node.process(ctx).await.expect("process succeeds");
        let result = get_result(&out, "CloseBlockNode").expect("stamped");
        assert_eq!(result["outcome"], json!("CLOSED:acme:AC.1"));
        assert_eq!(result["state_write_validated"], json!(true));
        assert_eq!(result["state_write_rejected"], json!(false));
        assert_eq!(result["block_key"], json!("acme:AC.1"));

        let raw = std::fs::read_to_string(repo_dir.join("planning/state.json")).unwrap();
        assert!(raw.contains("\"closed\""));
    }

    #[tokio::test]
    async fn default_repo_slug_is_engine_rs_when_event_names_no_repo() {
        let (_dir, repo_dir) = brain_fixture("engine-rs", &[("EN.1.A", "open", false)]);
        let ctx = ctx_for(&repo_dir, None, Some("EN.1.A"), "done");
        let node = CloseBlockNode::new();
        let out = node.process(ctx).await.expect("process succeeds");
        let result = get_result(&out, "CloseBlockNode").expect("stamped");
        assert_eq!(result["outcome"], json!("CLOSED:engine-rs:EN.1.A"));
    }

    #[tokio::test]
    async fn not_found_is_a_named_outcome_not_an_error() {
        let (_dir, repo_dir) = brain_fixture("acme", &[("AC.1", "open", false)]);
        let ctx = ctx_for(&repo_dir, Some("acme"), Some("AC.999"), "done");
        let node = CloseBlockNode::new();
        let out = node.process(ctx).await.expect("process succeeds");
        let result = get_result(&out, "CloseBlockNode").expect("stamped");
        assert_eq!(result["outcome"], json!("NOT_FOUND"));
        assert_eq!(result["state_write_validated"], json!(false));

        // Corpus is unchanged.
        let raw = std::fs::read_to_string(repo_dir.join("planning/state.json")).unwrap();
        assert!(raw.contains("\"open\""));
    }

    #[tokio::test]
    async fn operator_gated_block_is_refused_not_closed() {
        let (_dir, repo_dir) = brain_fixture("acme", &[("AC.1", "open", true)]);
        let ctx = ctx_for(&repo_dir, Some("acme"), Some("AC.1"), "done");
        let node = CloseBlockNode::new();
        let out = node.process(ctx).await.expect("process succeeds");
        let result = get_result(&out, "CloseBlockNode").expect("stamped");
        assert_eq!(result["outcome"], json!("OPERATOR_GATED"));
        assert_eq!(result["state_write_validated"], json!(false));

        let raw = std::fs::read_to_string(repo_dir.join("planning/state.json")).unwrap();
        assert!(raw.contains("\"open\""));
        assert!(!raw.contains("\"closed\""));
    }

    #[tokio::test]
    async fn unvalidated_when_no_brain_toml_is_reachable() {
        // A worktree with a planning/state.json but no brain.toml anywhere
        // above it in the filesystem — `find_brain_root` walks up past the
        // tempdir root and (barring a stray brain.toml on the test host)
        // fails, which is exactly the "mev unavailable" degrade.
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("planning")).expect("mkdir");
        std::fs::write(dir.path().join("planning/state.json"), "{}").expect("write");

        let ctx = ctx_for(dir.path(), Some("acme"), Some("AC.1"), "done");
        let node = CloseBlockNode::new();
        let out = node.process(ctx).await.expect("process succeeds");
        let result = get_result(&out, "CloseBlockNode").expect("stamped");
        let outcome = result["outcome"].as_str().unwrap();
        assert!(
            outcome.starts_with("UNVALIDATED:"),
            "expected UNVALIDATED:..., got {outcome}"
        );
    }

    // --- lock held --------------------------------------------------------

    #[tokio::test]
    async fn lock_held_is_reported_not_hung() {
        let (_dir, repo_dir) = brain_fixture("acme", &[("AC.1", "open", false)]);
        let root = find_brain_root(&repo_dir).expect("brain root resolves");

        // Hold the lock ourselves first.
        let guard = acquire_lock(&root, Duration::from_secs(1)).expect("acquire our own lock");

        let ctx = ctx_for(&repo_dir, Some("acme"), Some("AC.1"), "done");
        let node = CloseBlockNode::new().with_lock_timeout(Duration::from_millis(100));
        let started = std::time::Instant::now();
        let out = node.process(ctx).await.expect("process succeeds");
        // Bounded by the short lock timeout, not a hang.
        assert!(started.elapsed() < Duration::from_secs(2));

        let result = get_result(&out, "CloseBlockNode").expect("stamped");
        assert_eq!(result["outcome"], json!("LOCK_HELD"));
        assert!(result["detail"]
            .as_str()
            .unwrap()
            .contains(&std::process::id().to_string()));

        drop(guard);
    }

    // --- unit-level coverage of the operator-gate reimplementation ------

    #[test]
    fn block_has_unmet_operator_gate_reads_public_state_only() {
        let (_dir, repo_dir) =
            brain_fixture("acme", &[("AC.1", "open", true), ("AC.2", "open", false)]);
        let root = find_brain_root(&repo_dir).expect("brain root resolves");
        assert_eq!(
            block_has_unmet_operator_gate(&root, "acme:AC.1"),
            Some(true)
        );
        assert_eq!(
            block_has_unmet_operator_gate(&root, "acme:AC.2"),
            Some(false)
        );
        assert_eq!(
            block_has_unmet_operator_gate(&root, "acme:AC.999"),
            None,
            "unresolvable block must be None (don't gate), not an implicit pass"
        );
        assert_eq!(
            block_has_unmet_operator_gate(&root, "not-a-key-shape"),
            None
        );
    }

    // --- validate-then-rollback (task 4) ---------------------------------
    //
    // Both tests inject the `validate` seam ([`attempt_close_with_validator`])
    // rather than relying on a real corpus defect appearing as a side effect
    // of a real close, per the testing strategy — the write, the lock, the
    // operator gate, and (for the rollback case) the restore itself still
    // go through real mev calls against real files on disk.

    #[test]
    fn net_new_diagnostic_rolls_back_and_leaves_block_open() {
        // A second block ("AC.2") in the same track is the "unrelated
        // authored field" the rollback must not move.
        let (_dir, repo_dir) =
            brain_fixture("acme", &[("AC.1", "open", false), ("AC.2", "open", false)]);
        let root = find_brain_root(&repo_dir).expect("brain root resolves");
        let state_path = repo_dir.join("planning").join("state.json");
        let before_raw = std::fs::read_to_string(&state_path).expect("read before");

        let call_count = std::cell::Cell::new(0u32);
        let outcome = attempt_close_with_validator(
            &root,
            "acme:AC.1",
            "acme",
            Duration::from_secs(1),
            |_root| {
                let n = call_count.get();
                call_count.set(n + 1);
                if n == 0 {
                    // baseline: clean corpus
                    Ok(Report::default())
                } else {
                    // after: a net-new diagnostic — even on a file the write
                    // never touched, per "attribution is by delta, never by
                    // path".
                    Ok(Report {
                        diagnostics: vec![Diagnostic::error(
                            Path::new("some/unrelated/file.md"),
                            "E_FAKE_NET_NEW",
                            "synthetic regression injected by this test",
                        )],
                    })
                }
            },
        );

        match &outcome {
            CloseOutcome::Rejected { key, net_new } => {
                assert_eq!(key, "acme:AC.1");
                assert!(
                    net_new.iter().any(|line| line.contains("E_FAKE_NET_NEW")),
                    "net_new must name the diagnostic that triggered rollback, got {net_new:?}"
                );
            }
            other => panic!("expected Rejected, got {other:?}"),
        }

        // Restore is byte-identical to the pre-write content — the block's
        // status is back to "open" AND no unrelated authored field (AC.2)
        // moved, because the whole file was restored from mev's own
        // pre-write revision rather than patched field-by-field.
        let after_raw = std::fs::read_to_string(&state_path).expect("read after");
        assert_eq!(
            before_raw, after_raw,
            "rollback must restore the exact pre-write bytes"
        );
        assert!(after_raw.contains(r#""id": "AC.1", "title": "AC.1", "status": "open""#));
        assert!(!after_raw.contains(r#""id": "AC.1", "title": "AC.1", "status": "closed""#));
    }

    #[test]
    fn pre_existing_diagnostic_does_not_roll_back() {
        let (_dir, repo_dir) = brain_fixture("acme", &[("AC.1", "open", false)]);
        let root = find_brain_root(&repo_dir).expect("brain root resolves");

        // The SAME diagnostic, present both before and after the write —
        // a pre-existingly red corpus must not block the close.
        let outcome = attempt_close_with_validator(
            &root,
            "acme:AC.1",
            "acme",
            Duration::from_secs(1),
            |_root| {
                Ok(Report {
                    diagnostics: vec![Diagnostic::error(
                        Path::new("some/other/file.md"),
                        "E_PRE_EXISTING",
                        "already red before this write",
                    )],
                })
            },
        );

        assert_eq!(
            outcome,
            CloseOutcome::Closed {
                key: "acme:AC.1".to_string()
            }
        );

        let raw =
            std::fs::read_to_string(repo_dir.join("planning").join("state.json")).expect("read");
        assert!(raw.contains("\"closed\""));
    }
}
