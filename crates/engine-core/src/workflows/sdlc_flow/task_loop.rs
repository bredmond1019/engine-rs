//! Task-loop nodes + routers for the SDLC Flow workflow: implement -> test
//! -> triage -> review -> update/save, closing the loop back to
//! `TaskQueueRouterNode`.
//!
//! Ported from `orchestrator/app/workflows/sdlc_flow_workflow_nodes/`:
//! `task_queue_router_node.py`, `implement_task_node.py`,
//! `test_task_node.py`, `triage_task_node.py`, `review_router_node.py`,
//! `consolidated_review_node.py`, `update_task_status_node.py`,
//! `save_state_node.py`.
//!
//! Model/deterministic split (per the spec's Context Pointers):
//! `ImplementTaskNode` and `ConsolidatedReviewNode` always call a model;
//! `TriageTaskNode` is deterministic by default and only calls a model when
//! triage is enabled — the bare `event.llm_triage` field if set, else the
//! resolved policy's `llm_triage` (profile / `harness.json` / per-run
//! `policy` override). Everything else here — the routers, `TestTaskNode`,
//! `UpdateTaskStatusNode`, `SaveStateNode` — is pure Rust.

use std::path::Path;

use claude_code_rs::Config;
use engine_contract::TaskContext;
use serde::Deserialize;
use serde_json::json;

use crate::node::{Node, NodeError};
use crate::nodes::{ClaudeCodeStep, MetaTransport};
use crate::routing::Router;

#[cfg(test)]
use super::policy::OutputVerbosity;
use super::policy::{ModelTier, RetryFeedback, ReviewMode, SdlcPolicy, TestDepth};
use super::schema::{RunMeta, SDLCState, SDLCTask, SDLCTaskStatus};
use super::{
    get_result, parse_structured_or_fenced, put_result, CommandOutput, CommandRunner,
    ModelTransport, TransportSlot,
};
#[cfg(test)]
use crate::policy::RESOLVED_POLICY_IDENTITY;

/// A stable, run-invariant system-prompt prefix used as the cache-breakpoint
/// anchor when `policy.prompt_cache` is true (lever #2b). `claude-code-rs`'s
/// `Config` has no dedicated `cache_control` field, so the seam this Config
/// type exposes is `system_prompt`: keeping it byte-identical across calls
/// gives the underlying `claude` CLI a stable prefix to cache against,
/// instead of folding the same boilerplate into the ever-changing per-call
/// prompt string.
pub(super) const STABLE_SYSTEM_PROMPT: &str =
    "You are running inside the engine-rs SDLC Flow task loop. This system \
     prompt is held constant across calls so its tokens can be cached.";

/// Stage identity used to look up the resolved policy's per-stage
/// [`ModelTier`] (`policy::ModelTiers` field names).
///
/// `pub(super)` so the sibling `sdlc_flow` modules whose model nodes live
/// outside this file — `setup::GenerateTasksNode` (`Generate`) and
/// `docs::PatchDocsNode` (`Docs`) — can name their own stage rather than
/// re-deriving the tier/timeout lookup by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Stage {
    Implement,
    Triage,
    Review,
    /// `setup::GenerateTasksNode`.
    Generate,
    /// `docs::PatchDocsNode`.
    Docs,
}

/// Read the resolved [`SdlcPolicy`] stamped by dispatch (`engine-serve`'s
/// `seed_resolved_policy`) or by `SetupWorktreeNode`
/// (`setup::RESOLVED_POLICY_IDENTITY`). Fails loudly — `Err` — when the
/// stamp is absent or unparsable, rather than silently falling back to a
/// built-in default (task 8): a ctx driven directly in a unit test must now
/// seed a policy explicitly (`ctx_with_policy`/`ctx_with_current_task`).
/// Delegates to the generic `crate::policy::resolved_policy_strict::<SdlcPolicy>`
/// (EN.4.0/EN.5.D).
pub(super) fn resolved_policy(ctx: &TaskContext) -> Result<SdlcPolicy, NodeError> {
    crate::policy::resolved_policy_strict::<SdlcPolicy>(ctx)
}

/// Apply the resolved policy's model-tier, prompt-cache and call-timeout
/// knobs to a stage's `Config`, then append the `output_verbosity` directive
/// to `prompt`. Returns `(config, prompt)`. Delegates to the generic
/// `crate::policy::shaping::{apply_model_tier, apply_prompt_cache,
/// apply_call_timeout, apply_verbosity_directive}` (EN.4.0).
///
/// `policy.timeouts` is all-`None` by default, so the `apply_call_timeout`
/// call is a no-op unless a run explicitly sets a per-stage timeout.
pub(super) fn apply_policy(
    config: Config,
    prompt: String,
    policy: &SdlcPolicy,
    stage: Stage,
) -> (Config, String) {
    let (config, prompt) = (
        apply_policy_config(config, policy, stage),
        crate::policy::apply_verbosity_directive(prompt, policy.output_verbosity),
    );
    (config, prompt)
}

/// The **config half** of [`apply_policy`] — model tier, prompt cache, and
/// call timeout — without the prompt half.
///
/// Split out for `docs::PatchDocsNode`, which builds its prompt in a
/// `ClaudeCodeStep::with_prompt_builder` closure at call time and so has no
/// prompt string to hand [`apply_policy`] up front; it applies this to its
/// `Config` and `crate::policy::apply_verbosity_directive` inside the
/// closure. Keeping the two halves in one place is what stops the shaping
/// order from drifting between call sites.
#[must_use]
pub(super) fn apply_policy_config(config: Config, policy: &SdlcPolicy, stage: Stage) -> Config {
    let tier = stage_model_tier(policy, stage);
    let timeout_secs = stage_call_timeout(policy, stage);
    let config = crate::policy::apply_model_tier(config, tier, &policy.local.model);
    let config =
        crate::policy::apply_prompt_cache(config, policy.prompt_cache, STABLE_SYSTEM_PROMPT);
    crate::policy::apply_call_timeout(config, timeout_secs)
}

/// The resolved [`ModelTier`] for `stage`.
#[must_use]
pub(super) fn stage_model_tier(policy: &SdlcPolicy, stage: Stage) -> ModelTier {
    match stage {
        Stage::Implement => policy.model_tiers.implement,
        Stage::Triage => policy.model_tiers.triage,
        Stage::Review => policy.model_tiers.review,
        Stage::Generate => policy.model_tiers.generate,
        Stage::Docs => policy.model_tiers.docs,
    }
}

/// The resolved whole-call timeout (seconds) for `stage`, `None` when the
/// run set none.
#[must_use]
pub(super) fn stage_call_timeout(policy: &SdlcPolicy, stage: Stage) -> Option<u64> {
    match stage {
        Stage::Implement => policy.timeouts.implement,
        Stage::Triage => policy.timeouts.triage,
        Stage::Review => policy.timeouts.review,
        Stage::Generate => policy.timeouts.generate,
        Stage::Docs => policy.timeouts.docs,
    }
}

/// Deterministically classify the current task's diff as "trivial" against
/// the resolved policy's `review_skip_max_files`/`review_skip_max_diff_lines`
/// thresholds (lever #3a, `trivial_skip` mode) — zero model tokens spent.
/// Reads `git diff --numstat HEAD` — the **working tree** against `HEAD`,
/// preceded by [`stage_untracked_intent`] so new files are counted — via the
/// injectable [`CommandRunner`] seam: one line per changed file,
/// `<added>\t<deleted>\t<path>`.
///
/// This used to diff the COMMIT range `<base_sha>..HEAD`. Since nothing in
/// the run ever committed code, that range was always empty, so every task
/// classified as trivial (0 files / 0 lines) and `ReviewMode::TrivialSkip`
/// skipped the review unconditionally. Under the commit topology established
/// by [`super::commit_all`], `HEAD` holds every previously completed task's
/// work, so the working-tree delta against it is exactly the current task's
/// changes. Any unparsable line (e.g. a binary file's `-\t-\tpath`) is
/// treated conservatively as non-trivial. Falls back to non-trivial
/// (`false`) when the worktree path or the `git diff` invocation is
/// unavailable, so this never turns a `process` failure into an error path
/// — trivial-skip is an optimization, not a correctness requirement.
fn classify_trivial(ctx: &TaskContext, runner: &CommandRunner, policy: &SdlcPolicy) -> bool {
    let Ok(worktree) = worktree_path(ctx) else {
        return false;
    };
    stage_untracked_intent(runner, Path::new(&worktree));
    let Ok(output) = runner("git", &["diff", "--numstat", "HEAD"], Path::new(&worktree)) else {
        return false;
    };

    let mut files_changed: u32 = 0;
    let mut diff_lines: u32 = 0;
    for line in output.stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        files_changed += 1;
        let mut parts = line.split_whitespace();
        let added = parts.next().and_then(|s| s.parse::<u32>().ok());
        let deleted = parts.next().and_then(|s| s.parse::<u32>().ok());
        match (added, deleted) {
            (Some(a), Some(d)) => diff_lines = diff_lines.saturating_add(a).saturating_add(d),
            // Binary or otherwise unparsable numstat line: unknown size,
            // classify conservatively as non-trivial.
            _ => return false,
        }
    }

    files_changed <= policy.review_skip_max_files && diff_lines <= policy.review_skip_max_diff_lines
}

/// A review with more than this many distinct issues is treated as a
/// structural failure (re-implementation is unlikely to converge) rather
/// than a minor, fixable one. Mirrors
/// `review_router_node._STRUCTURAL_ISSUE_THRESHOLD` in Python.
///
/// `pub(crate)` (not private) because `wrap_up::derive_terminal_signal` also
/// needs this exact threshold to reconstruct, post hoc, whether a
/// `ConsolidatedReviewNode` verdict that reached `WrapUpNode` did so via the
/// structural branch (this same gate `ReviewRouterNode::route` uses) rather
/// than some other path — the two must never independently drift.
pub(crate) const STRUCTURAL_ISSUE_THRESHOLD: usize = 5;

/// The monotonically increasing logical clock `latest_state` orders
/// candidates by. It sums the four counters this loop's four
/// state-mutating nodes advance, one counter each:
///
/// | writer | counter it advances |
/// |---|---|
/// | [`ImplementTaskNode`] | `telemetry.total_attempts` (the attempt it is about to make) |
/// | [`IncrementAttemptNode`] | the retried task's `attempt_count` |
/// | [`ConsolidatedReviewNode`] | `telemetry.review_attempts` |
/// | [`UpdateTaskStatusNode`] | `telemetry.tasks_passed` |
///
/// **Every state write in the loop advances exactly ONE of these by exactly
/// one**, never zero and never two — so the sum increases by exactly one per
/// write, no two writes in a run ever hold the same value, and the ordering
/// `latest_state` derives from it has no ties to break. Adding a counting
/// site, moving one, or adding a state-writing node without a counter of its
/// own silently breaks that: `latest_state` starts picking a stale state
/// instead of the newest one. `tasks_failed` is deliberately absent — nothing
/// writes it (see [`UpdateTaskStatusNode`], and `wrap_up.rs`'s note that a
/// bailed run has `tasks_failed == 0` structurally), so it would contribute a
/// constant.
///
/// Takes the whole [`SDLCState`], not just its telemetry: one of the four
/// counters (`attempt_count`) is per-task.
fn logical_clock(state: &SDLCState) -> u64 {
    u64::from(state.telemetry.total_attempts)
        + u64::from(state.telemetry.review_attempts)
        + u64::from(state.telemetry.tasks_passed)
        + state
            .tasks
            .iter()
            .map(|task| u64::from(task.attempt_count))
            .sum::<u64>()
}

/// The node identities whose `ctx.nodes` result carries the durable
/// `SDLCState` nested under a `"state"` key instead of BEING that state.
/// Read by [`latest_state`]; written by the two nodes named.
const STATE_NESTED_IDENTITIES: [&str; 2] = ["ConsolidatedReviewNode", "ImplementTaskNode"];

/// Return the most recently mutated `SDLCState` among every node identity
/// that can write one: `IncrementAttemptNode` (the retry back-edge target,
/// EN.3.B), `UpdateTaskStatusNode` (a task's PASS), `ConsolidatedReviewNode`
/// (its own `review_attempts` bump, EN.ticket.review-retry-loop-unbounded
/// task 2), `ImplementTaskNode` (the attempt it charges to
/// `total_attempts` — every attempt passes through it, including one that
/// goes on to bail), and `LoadTaskStateNode` (the initial load). Mirrors the
/// `_latest_state_dict` helper shared by `TaskQueueRouterNode`/
/// `UpdateTaskStatusNode`/`SaveStateNode` in Python, extended for the new
/// retry-increment source.
///
/// `ConsolidatedReviewNode` and `ImplementTaskNode` nest their `SDLCState`
/// under their result's `"state"` key rather than being the whole result,
/// because those results are also read for their own payload (the review
/// verdict `ReviewRouterNode` routes on; the `modified_files` the
/// write-verification guard checks). Every other candidate's result IS the
/// whole `SDLCState`. [`STATE_NESTED_IDENTITIES`] is the list.
///
/// A fixed priority order (`IncrementAttemptNode` before `UpdateTaskStatusNode`
/// before `LoadTaskStateNode`) is NOT correct here: across a whole run,
/// `IncrementAttemptNode` may hold a *stale* entry from an earlier task's
/// retries while a *later* task's `UpdateTaskStatusNode` write is actually
/// the newest state (or vice versa, mid-retry, within the same task). Instead
/// this compares each candidate's [`logical_clock`] — a counter every
/// state-mutating node in this loop increments by exactly one on every write,
/// so it is a monotonically increasing logical clock for the whole run — and
/// keeps whichever candidate's value is highest. No wall-clock/`node_runs`
/// dependency needed.
///
/// `pub(crate)` (not private): this is the SINGLE `latest_state`
/// implementation for the whole `sdlc_flow` module (EN.3.G task 2).
/// `wrap_up.rs` (`WrapUpNode::process` and `write_terminal_blocked_state`)
/// calls this one directly rather than keeping a local copy that omits
/// `IncrementAttemptNode` — that omission under-reported `attempt_count` /
/// `telemetry.total_attempts` on a `MAJOR_BAIL` reached after retries,
/// because the retry-incremented state was never considered as a candidate.
pub(crate) fn latest_state(ctx: &TaskContext) -> Result<SDLCState, NodeError> {
    let mut best: Option<SDLCState> = None;
    for identity in [
        "IncrementAttemptNode",
        "UpdateTaskStatusNode",
        "ConsolidatedReviewNode",
        "ImplementTaskNode",
        "LoadTaskStateNode",
    ] {
        let Some(mut value) = get_result(ctx, identity).cloned() else {
            continue;
        };
        // See [`STATE_NESTED_IDENTITIES`]: these two nodes' results carry
        // their own payload with the durable `SDLCState` nested under
        // `"state"`. Every other candidate's result IS the whole `SDLCState`.
        if STATE_NESTED_IDENTITIES.contains(&identity) {
            let Some(state_value) = value.get_mut("state").map(std::mem::take) else {
                continue;
            };
            value = state_value;
        }
        let state: SDLCState = serde_json::from_value(value)
            .map_err(|err| NodeError::new(format!("failed to parse SDLCState: {err}")))?;
        let is_newer = best
            .as_ref()
            .map(|current| logical_clock(&state) > logical_clock(current))
            .unwrap_or(true);
        if is_newer {
            best = Some(state);
        }
    }
    best.ok_or_else(|| {
        NodeError::new(
            "no SDLCState found: none of IncrementAttemptNode, UpdateTaskStatusNode, \
             LoadTaskStateNode has run",
        )
    })
}

pub(crate) fn worktree_path(ctx: &TaskContext) -> Result<String, NodeError> {
    get_result(ctx, "SetupWorktreeNode")
        .and_then(|value| value.get("worktree_path"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .ok_or_else(|| NodeError::new("SetupWorktreeNode output missing worktree_path"))
}

/// Best-effort `git add -N -A` ("intent to add") in `worktree`, run
/// immediately before every diff this module takes against `HEAD`.
///
/// Git's `diff` ignores untracked files entirely. Because implementer agents
/// routinely create brand-new files (see `TestTaskNode::changed_files`, which
/// already parses untracked paths out of `git status --porcelain` as expected
/// output), a plain `git diff HEAD` would show the reviewer and the
/// trivial-skip classifier a diff with those files' content missing. Intent-to-add
/// records a zero-length blob for each untracked path, which makes the file appear
/// in `git diff HEAD` with its full content as `+` lines and in
/// `git diff --numstat HEAD` with its real line count — no custom diff
/// formatting required.
///
/// Deliberately non-fatal and deliberately not undone afterwards: a leftover
/// `-N` index entry on the retry path simply keeps the file visible to the
/// next attempt's diff (desired), and the eventual `git add -A` in
/// [`super::commit_all`] subsumes it. Note this makes a formerly read-only
/// seam a WRITE to the index — and on the `use_worktree: false` path (the
/// schema default) that index belongs to the operator's live repository. See
/// [`super::commit_all`]'s "Blast radius" section. A binary untracked file numstats as
/// `-\t-\t<path>`, which [`classify_trivial`]'s existing conservative arm
/// already treats as non-trivial — correct behavior for free.
pub(super) fn stage_untracked_intent(runner: &CommandRunner, worktree: &Path) {
    let _ = runner("git", &["add", "-N", "-A"], worktree);
}

fn current_task_fields(ctx: &TaskContext) -> Result<&serde_json::Value, NodeError> {
    get_result(ctx, "TaskQueueRouterNode")
        .ok_or_else(|| NodeError::new("TaskQueueRouterNode has not run yet"))
}

/// The current task as it exists in the *live* durable [`SDLCState`], resolved
/// by `TaskQueueRouterNode`'s `current_task_id` stamp.
///
/// `TaskQueueRouterNode`'s own output is a per-dispatch *snapshot*: the fields
/// it stamps are copied out of the state at dequeue time. `attempt_count` is
/// re-stamped on the retry back-edge by [`IncrementAttemptNode`], but nothing
/// re-stamps the rest — so any node needing a field that a *later* node may
/// have mutated (statuses, telemetry, per-task `validation_commands`) must read
/// it from [`latest_state`], not from the stamp.
///
/// `caller` is the node identity used in the not-found error so a mis-wired
/// graph names the node that actually looked. Returns an owned [`SDLCTask`]
/// because [`latest_state`] deserializes a fresh state each call — there is no
/// borrow to hand back.
fn current_task_state(ctx: &TaskContext, caller: &str) -> Result<SDLCTask, NodeError> {
    let current_task_id = current_task_fields(ctx)?
        .get("current_task_id")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| NodeError::new("TaskQueueRouterNode output missing current_task_id"))?
        as u32;
    let state = latest_state(ctx)?;
    state
        .tasks
        .into_iter()
        .find(|task| task.task_id == current_task_id)
        .ok_or_else(|| {
            NodeError::new(format!(
                "{caller}: no task with task_id={current_task_id} found in state"
            ))
        })
}

/// Fallback commit message when `TaskQueueRouterNode` never stamped a
/// current task (a `SaveStateNode` driven directly by a unit test, or a
/// graph shape without the router).
const SAVE_STATE_FALLBACK_MESSAGE: &str = "chore: flow state update";

/// The per-task commit message `SaveStateNode` hands [`super::commit_all`]:
/// `feat(sdlc): {current_task_id} — {title}`, built from the fields
/// `TaskQueueRouterNode` stamps when it dispatches a task.
///
/// `SaveStateNode` sits only on the pass path
/// (`UpdateTaskStatusNode → SaveStateNode`), so one message here is one
/// completed task in `git log` — that is the whole point of carrying the id.
///
/// Deliberately does NOT read `attempt_count` off the same stamp — not because
/// the field is wrong (`ticket-restamp-attempt-count` made it accurate on the
/// retry back-edge) but because `SaveStateNode` only ever runs on the pass
/// path: the attempt number a task happened to succeed on is run trivia, not
/// something a permanent `git log` line should carry.
fn save_state_commit_message(ctx: &TaskContext) -> String {
    let Some(current) = get_result(ctx, "TaskQueueRouterNode") else {
        return SAVE_STATE_FALLBACK_MESSAGE.to_string();
    };
    let Some(task_id) = current.get("current_task_id").and_then(|v| v.as_i64()) else {
        return SAVE_STATE_FALLBACK_MESSAGE.to_string();
    };
    let title = current
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("untitled");
    format!("feat(sdlc): {task_id} — {title}")
}

// --- prior-attempt feedback --------------------------------------------------

/// Opening line of the retry-feedback block appended to `ImplementTaskNode`'s
/// prompt. Deliberately blunt about the worktree already containing the prior
/// attempt's edits: the failure mode this fixes is a model re-entering its own
/// half-finished work, concluding the task is already done, and reporting
/// success without touching anything.
const RETRY_FEEDBACK_HEADER: &str = "\n\n--- PREVIOUS ATTEMPT FAILED — READ THIS BEFORE DOING \
     ANYTHING ---\nA previous attempt at this exact task already ran in this worktree and its \
     validation FAILED. Its edits are still present, so the task is NOT done. Diagnose the \
     failure below, fix its root cause, and re-verify. Do not report success without addressing \
     it.\n\n";

/// Opening line of the failure block appended to `TriageTaskNode`'s prompt on
/// the `llm_triage` branch.
///
/// Deliberately **not** [`RETRY_FEEDBACK_HEADER`]: that text addresses the
/// *implementer* re-entering a worktree that still holds its own half-finished
/// edits ("the task is NOT done, fix the root cause"). `TriageTaskNode` is a
/// *classifier* — it edits nothing — so it needs the opposite framing: here is
/// the evidence, return a verdict. Handing a classifier the implementer's
/// instructions invites it to reason about fixing the failure instead of
/// grading it.
const TRIAGE_FAILURE_HEADER: &str = "\n\n--- FAILING CHECK OUTPUT — CLASSIFY FROM THIS \
     EVIDENCE ---\nThe checks below failed with the output shown. Base the verdict on what \
     actually broke, not on the check names: a localized, mechanical, or transient failure the \
     next attempt can plausibly fix is RETRYABLE; a fundamental mismatch with the task's premise, \
     or the same failure recurring unchanged across attempts, is MAJOR_BAIL.\n\n";

/// Marker appended in place of the characters a `max_chars` bound elides.
const RETRY_FEEDBACK_TRUNCATED: &str = "…[truncated]";

/// Loud, model-facing banner appended when a review's diff is clipped by
/// `policy.review_diff_max_chars`. Shared by `ConsolidatedReviewNode` (this
/// module) and `end_review::EndReviewNode` — both bound their diff through
/// [`bound_review_diff`] and both must show the reviewer the same visible
/// truncation notice; `pub(super)` so the sibling module can reuse it rather
/// than fork a second copy.
///
/// Visibility is the whole point. A silently truncated diff would recreate
/// the exact failure mode this train exists to eliminate — a reviewer
/// confidently returning `PASS` over code it never saw — only with a subtler
/// cause than the empty diff that motivated `ticket-commit-task-work-real-diffs`.
pub(super) const REVIEW_DIFF_TRUNCATED_NOTICE: &str =
    "\n\n--- DIFF TRUNCATED — YOU ARE SEEING A PARTIAL \
     DIFF ---\nThe diff above was clipped to this run's `review_diff_max_chars` policy bound. \
     Changes beyond the cut were NOT shown to you. Do NOT return PASS on the strength of code \
     you could not see: judge only what is visible, and if the visible excerpt is not enough to \
     decide the acceptance criteria, say so explicitly in `summary` and return PARTIAL.\n";

/// Clip `diff` to at most `budget` characters for embedding in a review
/// prompt (`ConsolidatedReviewNode`'s per-task diff or
/// `end_review::EndReviewNode`'s whole-run diff), returning
/// `(text, truncated)`.
///
/// Reuses [`truncate_chars`] — the same character-safe (never byte-slicing)
/// helper the retry-feedback bound uses — and reserves the notice's own
/// length out of the budget so the whole block still fits, mirroring how
/// [`render_feedback_block`] reserves its labels.
///
/// The notice is non-negotiable and is emitted even when the budget is too
/// small to hold it (the same precedence `render_feedback_block` gives its
/// check names): a reviewer told nothing at all is the failure mode; a
/// reviewer told "you are seeing a partial diff" and nothing else is merely
/// useless, and it will say so.
pub(super) fn bound_review_diff(diff: &str, budget: usize) -> (String, bool) {
    if diff.chars().count() <= budget {
        return (diff.to_string(), false);
    }
    let notice_len = REVIEW_DIFF_TRUNCATED_NOTICE.chars().count();
    let mut out = truncate_chars(diff, budget.saturating_sub(notice_len));
    out.push_str(REVIEW_DIFF_TRUNCATED_NOTICE);
    (out, true)
}

/// One unit of prior-attempt evidence. `label` names the failing thing and is
/// never dropped by truncation; `detail` is the (potentially enormous)
/// compiler/rustfmt/reviewer text and is what gets trimmed to fit.
struct FeedbackEntry {
    label: String,
    detail: String,
}

/// Truncate `text` to at most `max` **characters** (not bytes — the model
/// output can contain multi-byte characters, and slicing by byte would panic).
fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let marker_len = RETRY_FEEDBACK_TRUNCATED.chars().count();
    if max <= marker_len {
        return text.chars().take(max).collect();
    }
    let mut out: String = text.chars().take(max - marker_len).collect();
    out.push_str(RETRY_FEEDBACK_TRUNCATED);
    out
}

/// The failed entries of the last `TestTaskNode` run, if it recorded a
/// failure. `None` when the node has not run (first attempt) or when it
/// explicitly passed.
fn test_failure_entries(ctx: &TaskContext) -> Option<Vec<FeedbackEntry>> {
    let result = get_result(ctx, "TestTaskNode")?;
    // Strictly `false` — an absent/garbled `all_passed` is not evidence of a
    // failed attempt and must not manufacture a retry block.
    if result.get("all_passed").and_then(|v| v.as_bool()) != Some(false) {
        return None;
    }
    let checks = result.get("check_results")?.as_array()?;
    let entries: Vec<FeedbackEntry> = checks
        .iter()
        .filter(|check| {
            !check
                .get("passed")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })
        .map(|check| {
            let name = check
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unnamed check");
            let message = check
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let output = check
                .get("output")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let detail = [message, output]
                .into_iter()
                .filter(|part| !part.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            FeedbackEntry {
                label: format!("FAILED CHECK: {name}"),
                detail,
            }
        })
        .collect();
    (!entries.is_empty()).then_some(entries)
}

/// The last `ConsolidatedReviewNode` verdict's findings, used on the
/// `ReviewRouterNode -> IncrementAttemptNode` back-edge where the tests
/// passed but the reviewer did not.
fn review_failure_entries(ctx: &TaskContext) -> Option<Vec<FeedbackEntry>> {
    let result = get_result(ctx, "ConsolidatedReviewNode")?;
    let verdict = result
        .get("verdict")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if verdict.is_empty() || verdict == "PASS" {
        return None;
    }
    let summary = result
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let mut entries = vec![FeedbackEntry {
        label: format!("REVIEW VERDICT: {verdict}"),
        detail: summary.to_string(),
    }];
    if let Some(issues) = result.get("issues").and_then(|v| v.as_array()) {
        for (index, issue) in issues.iter().enumerate() {
            let text = issue
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| issue.to_string());
            entries.push(FeedbackEntry {
                label: format!("REVIEW ISSUE {}:", index + 1),
                detail: text,
            });
        }
    }
    Some(entries)
}

/// Render the immediately-preceding attempt's failure as a prompt block for
/// `ImplementTaskNode`, or `None` when there is nothing to say.
///
/// Pure: reads only `ctx.nodes` and `cfg`, performs no I/O.
///
/// **Retry detection deliberately does NOT read `attempt_count`.** Since
/// `ticket-restamp-attempt-count` that field *is* accurate on the back-edge
/// (`IncrementAttemptNode` re-stamps it), so this is no longer a workaround
/// for a lie — it is the better signal on its own merits. The presence of a
/// *failed* `TestTaskNode`/`ConsolidatedReviewNode` result in `ctx.nodes` is
/// accurate by construction (`ctx.nodes` accumulates across the loop, so a
/// recorded failure can only exist if an attempt already ran and failed) and
/// it is the very thing being rendered: there is nothing to say without a
/// failure entry, whatever the counter reads.
fn prior_attempt_feedback(ctx: &TaskContext, cfg: &RetryFeedback) -> Option<String> {
    if !cfg.enabled {
        return None;
    }
    // Test failure first: it is the immediate cause on the
    // `TriageRouterNode` back-edge. The review findings are the fallback for
    // the `ReviewRouterNode` back-edge, where the checks passed.
    let entries = test_failure_entries(ctx).or_else(|| review_failure_entries(ctx))?;
    render_feedback_block(RETRY_FEEDBACK_HEADER, &entries, cfg.max_chars as usize)
}

/// Render the failed checks of the run that just failed as a prompt block for
/// `TriageTaskNode`'s `llm_triage` branch, or `None` when there is nothing to
/// say.
///
/// Pure: reads only `ctx.nodes` and `cfg`, performs no I/O.
///
/// Deliberately narrower than [`prior_attempt_feedback`] in two ways:
/// - **Test failures only, no review fallback.** `TriageTaskNode` is only ever
///   reached from `TestTaskNode`, and it is classifying *that* run's failure.
///   A stale `ConsolidatedReviewNode` verdict from a previous attempt is not
///   the thing under judgement and would be actively misleading evidence.
/// - **A classifier-facing header** ([`TRIAGE_FAILURE_HEADER`]) rather than the
///   implementer-facing [`RETRY_FEEDBACK_HEADER`].
///
/// Shares `policy.retry_feedback` rather than introducing a knob: this is the
/// same class of text (captured check output), bounded for the same reason,
/// and a run that wants failure evidence trimmed or switched off wants it
/// trimmed or switched off in both places.
fn triage_failure_feedback(ctx: &TaskContext, cfg: &RetryFeedback) -> Option<String> {
    if !cfg.enabled {
        return None;
    }
    let entries = test_failure_entries(ctx)?;
    render_feedback_block(TRIAGE_FAILURE_HEADER, &entries, cfg.max_chars as usize)
}

/// Assemble `header` + `entries` into a prompt block of at most `budget`
/// characters. `None` when there is nothing to render (`budget == 0`, no
/// entries, or an all-whitespace result).
///
/// Labels are non-negotiable — a truncated block must still say *which* checks
/// failed — so only the details compete for the leftover budget.
fn render_feedback_block(header: &str, entries: &[FeedbackEntry], budget: usize) -> Option<String> {
    if budget == 0 || entries.is_empty() {
        return None;
    }
    let fixed: usize = header.chars().count()
        + entries
            .iter()
            .map(|entry| entry.label.chars().count() + 1)
            .sum::<usize>();
    let per_entry = budget.saturating_sub(fixed) / entries.len();

    let mut block = String::from(header);
    for entry in entries {
        block.push_str(&entry.label);
        block.push('\n');
        if per_entry > 0 && !entry.detail.is_empty() {
            block.push_str(&truncate_chars(&entry.detail, per_entry));
            block.push('\n');
        }
    }

    // Belt-and-braces: the per-entry newlines above are not counted in
    // `fixed`, so clamp the assembled block. Skipped when the labels alone
    // already exceed the budget — naming the failed checks outranks the
    // bound, per this knob's contract.
    if fixed <= budget {
        block = truncate_chars(&block, budget);
    }

    (!block.trim().is_empty()).then_some(block)
}

// --- TaskQueueRouterNode ---------------------------------------------------

/// Deterministic router that dispatches the next `PENDING` task or ends the
/// task loop by routing to the `PatchDocsNode` identity (an EN.3.B stub
/// terminal here).
///
/// `Router::route(&self, ctx: &TaskContext)` takes `&ctx` and cannot mutate
/// it, but the Python node writes its own output as a side effect of
/// routing. That write is moved into `Node::process` here (run by the
/// framework before `route` is consulted for a router — see
/// `crate::workflow`), so `process` decides+stores the current task's
/// fields and `route` stays a pure read of the same state to pick
/// `ImplementTaskNode` vs `PatchDocsNode`.
pub struct TaskQueueRouterNode;

impl TaskQueueRouterNode {
    /// Find the first `PENDING` task in `state`, if any.
    fn next_pending(state: &SDLCState) -> Option<&SDLCTask> {
        state
            .tasks
            .iter()
            .find(|task| task.status == SDLCTaskStatus::Pending)
    }
}

#[async_trait::async_trait]
impl Node for TaskQueueRouterNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let state = latest_state(&ctx)?;
        if let Some(task) = Self::next_pending(&state) {
            put_result(
                &mut ctx,
                "TaskQueueRouterNode",
                json!({
                    "current_task_id": task.task_id,
                    "title": task.title,
                    "description": task.description,
                    "acceptance_criteria": task.acceptance_criteria,
                    "attempt_count": task.attempt_count,
                    "max_attempts": task.max_attempts,
                }),
            );
        }
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "TaskQueueRouterNode"
    }

    fn as_router(&self) -> Option<&dyn Router> {
        Some(self)
    }
}

impl Router for TaskQueueRouterNode {
    fn route(&self, ctx: &TaskContext) -> Option<String> {
        let state = latest_state(ctx).ok()?;
        if Self::next_pending(&state).is_some() {
            Some("ImplementTaskNode".to_string())
        } else {
            // Drain branch: route through the run-level `FinalValidationNode`
            // gate (EN.3.E) before `PatchDocsNode`, not directly to it.
            Some("FinalValidationNode".to_string())
        }
    }
}

// --- ImplementTaskNode ------------------------------------------------------

/// Run-invariant path-discipline preamble prepended to every
/// [`ImplementTaskNode`] prompt, first attempt and retry alike.
///
/// Under `use_worktree: true` the model's subprocess `cwd` is scoped to the
/// engine's worktree (see `process`), but `cwd` constrains only the
/// *subprocess*, never the model's choice of an ABSOLUTE path. The target
/// repos' `CLAUDE.md` files are full of literal
/// `/Users/.../core/<repo>/...` and `file:///Users/...` references, and an
/// agent that resolves one of those writes into the MAIN checkout from any
/// cwd — which is exactly how a validated run left its worktree
/// byte-identical to `origin/main` while ~784 lines of real work landed in
/// the main tree.
///
/// Deliberately carries **no run-specific path**: the worktree root is
/// per-run text and is appended separately in `process`, so this constant
/// stays cache-stable (standing rule 6) and the first-attempt prompt stays
/// deterministic.
pub(super) const PATH_DISCIPLINE_PREAMBLE: &str = "PATH DISCIPLINE. Work only \
     through paths relative to your current working directory. Never resolve \
     an absolute path taken from a CLAUDE.md, a doc link, or a `file://` URL \
     — those point at a DIFFERENT checkout, and writing there silently loses \
     your work. Before your first write, confirm the tree you are in is the \
     intended one.\n\n";

/// Model node (Sonnet): drives Claude Code to implement the current task.
/// Composes a `ClaudeCodeStep` under its own identity so it can post-process
/// the model's JSON output into `{summary, modified_files, tests_added}`.
///
/// **This node is where `telemetry.total_attempts` is charged** — one per
/// attempt, unconditionally, before the model call, exactly as
/// [`ConsolidatedReviewNode`] charges `telemetry.review_attempts`. An attempt
/// is counted where it is MADE, not at the outcome it happens to reach: this
/// is the one node every attempt passes through — a first dispatch from
/// `TaskQueueRouterNode`, a triage retry and a review retry alike (both
/// arrive via `IncrementAttemptNode`) — including an attempt that goes on to
/// bail. Counting at the outcome instead is the R4 defect: attempt
/// exhaustion and an LLM `MAJOR_BAIL` both leave `TriageRouterNode` for
/// `WrapUpNode` and never touch `UpdateTaskStatusNode`, so a run that made a
/// real attempt reported `total_attempts: 0`. A per-outcome site would have
/// to be re-added for every future terminal path that bypasses
/// `UpdateTaskStatusNode`; this one cannot be bypassed, because no attempt
/// happens without it.
///
/// The counted state rides in this node's result under a `"state"` key
/// ([`STATE_NESTED_IDENTITIES`]) alongside the implement payload, so
/// [`latest_state`] sees it without any reader of `modified_files` changing.
pub struct ImplementTaskNode {
    config: Config,
    transport: Option<ModelTransport>,
}

/// Model output shape `ImplementTaskNode` expects. Non-JSON model output is
/// tolerated (the loop doesn't route on these fields) by falling back to the
/// raw text as `summary` with empty vecs.
#[derive(Debug, Deserialize)]
struct ImplementOutput {
    summary: String,
    #[serde(default)]
    modified_files: Vec<String>,
    #[serde(default)]
    tests_added: Vec<String>,
}

/// JSON schema matching [`ImplementOutput`], passed as `Config.json_schema`
/// so `claude-code-rs` requests (and pre-parses) a schema-constrained reply
/// via `Outcome.structured_output` instead of relying solely on prompt text.
fn implement_output_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "summary": { "type": "string" },
            "modified_files": { "type": "array", "items": { "type": "string" } },
            "tests_added": { "type": "array", "items": { "type": "string" } },
        },
        "required": ["summary"],
    })
}

impl ImplementTaskNode {
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: Config {
                model: Some("claude-sonnet-4-5".to_string()),
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

    /// Override the base `Config` entirely (model/tool-permission/etc.
    /// fields) — `process` still overwrites `model` per the resolved
    /// policy and `cwd` per `SetupWorktreeNode`'s worktree path, but every
    /// other field (e.g. `disallowed_tools`, `dangerously_skip_permissions`)
    /// passes through untouched. Live/manual tests use this to grant real
    /// tool-use permission for a genuine agentic session without changing
    /// this node's own safe-by-default `new()` construction.
    #[must_use]
    pub fn with_config(mut self, config: Config) -> Self {
        self.config = config;
        self
    }
}

impl Default for ImplementTaskNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for ImplementTaskNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let current = current_task_fields(&ctx)?.clone();
        let title = current
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let description = current
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let acceptance_criteria = current
            .get("acceptance_criteria")
            .cloned()
            .unwrap_or_else(|| json!([]));

        // Resolved once, before the prompt is built — the retry-feedback
        // knob is read here and the same `policy` is threaded into
        // `apply_policy` below (no second resolve).
        let policy = resolved_policy(&ctx)?;

        // Charge this attempt BEFORE the call, whatever it goes on to
        // conclude — see this node's doc comment for why the count lives
        // here and not at the outcome, and [`logical_clock`] for the
        // exactly-one-counter-per-write invariant this write upholds.
        let mut counted_state = latest_state(&ctx)?;
        counted_state.telemetry.total_attempts += 1;

        // Resolved once and reused: the same worktree root names the
        // per-run half of the path-discipline preamble below and scopes
        // `config.cwd` further down. Best-effort — a ctx with no
        // `SetupWorktreeNode` result (a unit test, or `use_worktree:
        // false`) yields `None` and the prompt simply omits the path
        // sentence rather than naming a path we do not have.
        let worktree = worktree_path(&ctx).ok();

        // Path discipline goes FIRST, ahead of the task text: the
        // run-invariant sentences from the constant, then (when known) the
        // per-run worktree root. `apply_policy` only ever APPENDS to the
        // prompt (`apply_verbosity_directive`), and the retry block below
        // also appends, so a leading preamble survives both untouched.
        let mut prompt = String::from(PATH_DISCIPLINE_PREAMBLE);
        if let Some(worktree) = worktree.as_deref() {
            prompt.push_str(&format!(
                "Your working tree root is {worktree} — `git rev-parse \
                 --show-toplevel` must resolve there before you write.\n\n"
            ));
        }
        prompt.push_str(&format!(
            "Implement the following SDLC task. Respond with strict JSON of \
             the shape {{\"summary\": str, \"modified_files\": [str], \
             \"tests_added\": [str]}}.\n\nTitle: {title}\nDescription: \
             {description}\nAcceptance criteria: {acceptance_criteria}"
        ));

        // On a retry, tell the model what the previous attempt broke.
        // Without this the retry request is byte-identical to the first
        // attempt and the loop cannot self-correct. `None` on a first
        // attempt leaves the prompt above untouched, byte for byte.
        if let Some(feedback) = prior_attempt_feedback(&ctx, &policy.retry_feedback) {
            prompt.push_str(&feedback);
        }

        let (mut config, prompt) =
            apply_policy(self.config.clone(), prompt, &policy, Stage::Implement);
        // Scope the model's session to the actual worktree so it edits the
        // right checkout rather than inheriting the host process's ambient
        // cwd. Best-effort: a ctx driven directly (no `SetupWorktreeNode`
        // run, e.g. a unit test) falls back to today's behavior (no `cwd`
        // override) instead of failing the node.
        if let Some(worktree) = worktree.as_deref() {
            config.cwd = Some(std::path::PathBuf::from(worktree));
        }

        config.json_schema = Some(implement_output_schema());

        let mut step = ClaudeCodeStep::new("ImplementTaskNode", config, prompt)
            .with_retry_policy(policy.transport_retry);
        if let Some(transport) = self.transport.clone() {
            step = step.with_transport(move |config, prompt| (transport)(config, prompt));
        }

        let mut ctx = step.process(ctx).await?;

        let content = ctx
            .nodes
            .get("ImplementTaskNode")
            .and_then(|value| value.get("content"))
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();

        let parsed: ImplementOutput =
            parse_structured_or_fenced(&ctx, "ImplementTaskNode", &content).unwrap_or(
                ImplementOutput {
                    summary: content.clone(),
                    modified_files: Vec::new(),
                    tests_added: Vec::new(),
                },
            );

        let mut result = json!({
            "summary": parsed.summary,
            "modified_files": parsed.modified_files,
            "tests_added": parsed.tests_added,
        });
        // Nested under `"state"`, not instead of the payload above: this
        // result is also what the write-verification guard reads
        // `modified_files` from. `latest_state` knows to unwrap this key for
        // this identity (see [`STATE_NESTED_IDENTITIES`]).
        result["state"] = serde_json::to_value(&counted_state)
            .map_err(|err| NodeError::new(format!("failed to serialize SDLCState: {err}")))?;
        put_result(&mut ctx, "ImplementTaskNode", result);

        Ok(ctx)
    }

    fn name(&self) -> &str {
        "ImplementTaskNode"
    }
}

// --- TestTaskNode ------------------------------------------------------------

/// Outcome of a single harness check. Mirrors Python's `CheckResult`.
///
/// `pub` (not `pub(crate)`) so [`super::final_validation::FinalValidationNode`]
/// — which shares [`TestTaskNode::run_checks`] rather than forking a second
/// check-kind dispatch — can name the type its stamped result carries, and
/// so [`super::schema::CommittedFinalValidation`] (EN.3.E task 3), itself a
/// `pub` type, can reuse this exact shape for the committed-state
/// `final_validation.check_results` array rather than inventing a parallel
/// one — a `pub(crate)` field type inside a `pub` struct is a
/// `private_interfaces` warning (denied under `clippy -- -D warnings`).
/// `Deserialize`/`PartialEq`/`Eq` are needed for that reuse: the
/// committed-state round trip deserializes `check_results` back out of the
/// on-disk JSON. Fields stay module-private — nothing outside this crate
/// constructs one directly; only the *type name* needs to be nameable.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CheckResult {
    name: String,
    kind: String,
    passed: bool,
    #[serde(default)]
    output: String,
    #[serde(default)]
    message: String,
}

/// Deterministic node: runs the worktree's `planning/harness.json`
/// validation suite via the injectable [`CommandRunner`] seam so tests can
/// drive fail-then-pass across attempts without a real subprocess.
///
/// Every check `kind` from the Python port
/// (`orchestrator/app/workflows/sdlc_flow_workflow_nodes/test_task_node.py`)
/// is supported: `command` (the default), `forbidden-pattern-scan`,
/// `baseline-diff`, `count-delta`, `warning-scan`. Any *other* kind still
/// fails closed via [`Self::run_unsupported_kind`], so a harness.json typo
/// or a genuinely new kind never silently passes.
pub struct TestTaskNode {
    runner: CommandRunner,
}

impl TestTaskNode {
    #[must_use]
    pub fn new() -> Self {
        Self {
            runner: super::default_command_runner(),
        }
    }

    /// Override the command runner used for check invocations. Tests use
    /// this to stub the subprocess so the gated suite never shells out.
    #[must_use]
    pub fn with_runner(mut self, runner: CommandRunner) -> Self {
        self.runner = runner;
        self
    }

    fn run_command_check(&self, check: &serde_json::Value, worktree: &Path) -> CheckResult {
        let name = check
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unnamed")
            .to_string();
        let kind = check
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("command")
            .to_string();
        let command = check.get("command").and_then(|v| v.as_str()).unwrap_or("");

        let outcome = (self.runner)("sh", &["-c", command], worktree);
        match outcome {
            Ok(CommandOutput {
                status,
                stdout,
                stderr,
            }) => {
                let passed = status == 0;
                CheckResult {
                    name,
                    kind,
                    passed,
                    output: format!("{stdout}{stderr}"),
                    message: if passed {
                        String::new()
                    } else {
                        format!("exit code {status}")
                    },
                }
            }
            Err(err) => CheckResult {
                name,
                kind,
                passed: false,
                output: String::new(),
                message: format!("failed to spawn check: {err}"),
            },
        }
    }

    /// Runs `command` (`sh -c <command>`) via the injectable runner,
    /// returning stdout+stderr — the shared shell-out primitive every check
    /// kind below builds on.
    fn shell_out(&self, command: &str, worktree: &Path) -> CommandOutput {
        (self.runner)("sh", &["-c", command], worktree).unwrap_or(CommandOutput {
            status: -1,
            stdout: String::new(),
            stderr: String::new(),
        })
    }

    /// `forbidden-pattern-scan`: grep for each rule's pattern under its
    /// paths, drop matches covered by that rule's `allowlistPattern`, fail
    /// on any match left.
    ///
    /// The pattern is passed as its own argv entry to a directly-invoked
    /// `grep`, never interpolated into an `sh -c` string (EN.3.G task 5) —
    /// a pattern containing `'`, `"`, `$(...)`, or `;` used to terminate the
    /// shell quoting or inject a second command. **Glob carve-out:** the
    /// shell used to expand glob metacharacters (`*`, `?`, `[`) in `paths`;
    /// a direct `grep` invocation does not, so a rule whose `paths` contains
    /// one stays on the `sh -c` route for that rule only, with the pattern
    /// escaped as `'\''` so it still cannot break out of quoting.
    fn run_forbidden_pattern_scan(
        &self,
        check: &serde_json::Value,
        worktree: &Path,
    ) -> CheckResult {
        let name = check
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unnamed")
            .to_string();

        let mut violations: Vec<String> = Vec::new();
        let mut output_parts: Vec<String> = Vec::new();
        for rule in check
            .get("rules")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            let pattern = rule.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
            let paths = rule.get("paths").and_then(|v| v.as_str()).unwrap_or("");
            let path_entries: Vec<&str> = paths.split_whitespace().collect();
            if path_entries.is_empty() {
                // No path operand would make `grep` read stdin and hang;
                // skip the rule and record nothing.
                continue;
            }

            let stdout = if path_entries.iter().any(|p| p.contains(['*', '?', '['])) {
                // Glob carve-out: keep the `sh -c` route so the shell still
                // expands the glob, but escape every `'` in the pattern as
                // `'\''` so it remains inert as shell syntax.
                let escaped_pattern = pattern.replace('\'', r"'\''");
                let grep_command = format!("grep -rnE '{escaped_pattern}' {paths}");
                self.shell_out(&grep_command, worktree).stdout
            } else {
                let mut args: Vec<&str> = vec!["-rnE", pattern];
                args.extend(path_entries.iter().copied());
                (self.runner)("grep", &args, worktree)
                    .map(|out| out.stdout)
                    .unwrap_or_default()
            };

            let mut matches: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
            if let Some(allowlist) = rule.get("allowlistPattern").and_then(|v| v.as_str()) {
                if let Ok(re) = regex::Regex::new(allowlist) {
                    matches.retain(|line| !re.is_match(line));
                }
            }
            violations.extend(matches.into_iter().map(str::to_string));
            output_parts.push(stdout);
        }

        let passed = violations.is_empty();
        let message = if passed {
            String::new()
        } else {
            format!("{} forbidden-pattern match(es)", violations.len())
        };
        CheckResult {
            name,
            kind: "forbidden-pattern-scan".to_string(),
            passed,
            output: output_parts.join("\n"),
            message,
        }
    }

    /// `baseline-diff`: run `baselineCommand` and `command`, both expected
    /// to emit a JSON array; fail on any `command` entry whose `compareKeys`
    /// projection isn't present in the baseline's.
    fn run_baseline_diff(&self, check: &serde_json::Value, worktree: &Path) -> CheckResult {
        let name = check
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unnamed")
            .to_string();
        let compare_keys: Vec<String> = check
            .get("compareKeys")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();

        let baseline_command = check
            .get("baselineCommand")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let command = check.get("command").and_then(|v| v.as_str()).unwrap_or("");
        let baseline_stdout = self.shell_out(baseline_command, worktree).stdout;
        let current_stdout = self.shell_out(command, worktree).stdout;

        let baseline_entries: Vec<serde_json::Value> =
            serde_json::from_str(&baseline_stdout).unwrap_or_default();
        let current_entries: Vec<serde_json::Value> =
            serde_json::from_str(&current_stdout).unwrap_or_default();

        let key = |entry: &serde_json::Value| -> Vec<Option<String>> {
            compare_keys
                .iter()
                .map(|k| entry.get(k).map(|v| v.to_string()))
                .collect()
        };
        let baseline_keys: std::collections::HashSet<Vec<Option<String>>> =
            baseline_entries.iter().map(key).collect();
        let new_entries: usize = current_entries
            .iter()
            .filter(|entry| !baseline_keys.contains(&key(entry)))
            .count();

        let passed = new_entries == 0;
        let message = if passed {
            String::new()
        } else {
            format!("{new_entries} net-new violation(s)")
        };
        CheckResult {
            name,
            kind: "baseline-diff".to_string(),
            passed,
            output: current_stdout,
            message,
        }
    }

    /// `count-delta`: extract a count from `command`'s stdout via
    /// `countPattern`, comparing it to `baseline` in the `failOn` direction
    /// (`"decrease"` fails when the count dropped; anything else fails when
    /// it rose).
    fn run_count_delta(&self, check: &serde_json::Value, worktree: &Path) -> CheckResult {
        let name = check
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unnamed")
            .to_string();
        let baseline_count = check.get("baseline").and_then(|v| v.as_i64()).unwrap_or(0);
        let command = check.get("command").and_then(|v| v.as_str()).unwrap_or("");
        let count_pattern = check
            .get("countPattern")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let stdout = self.shell_out(command, worktree).stdout;
        let current_count = regex::Regex::new(count_pattern)
            .ok()
            .and_then(|re| re.find(&stdout))
            .and_then(|m| m.as_str().split_whitespace().next())
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);

        let fail_on = check
            .get("failOn")
            .and_then(|v| v.as_str())
            .unwrap_or("decrease");
        let passed = if fail_on == "decrease" {
            current_count >= baseline_count
        } else {
            current_count <= baseline_count
        };
        let message = if passed {
            String::new()
        } else {
            format!("count {current_count} vs baseline {baseline_count} ({fail_on})")
        };
        CheckResult {
            name,
            kind: "count-delta".to_string(),
            passed,
            output: stdout,
            message,
        }
    }

    /// `warning-scan`: run `command`, scan combined stdout+stderr for every
    /// `warningPatterns` entry. Only fails the check itself when `gates` is
    /// true (default `false` for this kind — matches the Python port).
    fn run_warning_scan(&self, check: &serde_json::Value, worktree: &Path) -> CheckResult {
        let name = check
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unnamed")
            .to_string();
        let command = check.get("command").and_then(|v| v.as_str()).unwrap_or("");
        let outcome = self.shell_out(command, worktree);
        let combined = format!("{}{}", outcome.stdout, outcome.stderr);

        let found: Vec<String> = check
            .get("warningPatterns")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str())
            .filter(|pattern| {
                regex::Regex::new(pattern)
                    .map(|re| re.is_match(&combined))
                    .unwrap_or(false)
            })
            .map(str::to_string)
            .collect();

        let gates = check
            .get("gates")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let passed = !gates || found.is_empty();
        let message = if found.is_empty() {
            String::new()
        } else {
            format!("warning pattern(s) matched: {found:?}")
        };
        CheckResult {
            name,
            kind: "warning-scan".to_string(),
            passed,
            output: combined,
            message,
        }
    }

    fn run_unsupported_kind(&self, check: &serde_json::Value, kind: &str) -> CheckResult {
        let name = check
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unnamed")
            .to_string();
        CheckResult {
            name,
            kind: kind.to_string(),
            passed: false,
            output: String::new(),
            message: format!(
                "check kind {kind:?} is not yet supported by TestTaskNode \
                 (TODO(EN.3.B+): richer harness kinds)"
            ),
        }
    }

    /// List every path `git status --porcelain` reports as changed
    /// (modified, added, deleted, renamed, or untracked) in `worktree`, via
    /// the injectable [`CommandRunner`] seam. Each porcelain line is either
    /// `XY path` or, for a rename, `XY orig -> new` — this returns the
    /// right-hand path in the rename case (the file's current location) and
    /// the single path otherwise. Returns an empty list (never errors) when
    /// the runner invocation itself fails, so a spawn failure here degrades
    /// to "nothing looks changed" rather than aborting the node.
    fn changed_files(&self, worktree: &Path) -> Vec<String> {
        let output =
            (self.runner)("git", &["status", "--porcelain"], worktree).unwrap_or(CommandOutput {
                status: -1,
                stdout: String::new(),
                stderr: String::new(),
            });

        output
            .stdout
            .lines()
            .filter_map(|line| {
                if line.len() <= 3 {
                    return None;
                }
                let rest = line[3..].trim();
                if rest.is_empty() {
                    return None;
                }
                // Rename/copy lines look like `orig -> new`; keep the
                // destination path.
                let path = rest.rsplit(" -> ").next().unwrap_or(rest).trim();
                if path.is_empty() {
                    None
                } else {
                    Some(path.trim_matches('"').to_string())
                }
            })
            .collect()
    }

    /// Write-verification guard: asks [`Self::changed_files`] whether the
    /// worktree shows ANY change at all, and fails the task when it does
    /// not.
    ///
    /// **The question is asked unconditionally, and the answer does not
    /// depend on `ImplementTaskNode`'s self-reported `modified_files`.**
    /// That self-report is documented-unreliable in both directions: a real
    /// (non-stubbed) `claude` call was observed live leaving it EMPTY on a
    /// genuinely successful write (see `sdlc_flow_live.rs`'s
    /// `live_full_workflow_real_implement_and_review`), and a run whose
    /// implement work landed in the WRONG TREE also reported it empty while
    /// the worktree was untouched. So the claim is not evidence of a write,
    /// and its absence is not evidence of a no-op — only the worktree is
    /// evidence. Note what this does NOT do: it never compares claimed
    /// paths against changed paths. The self-report is unreliable about
    /// WHICH files; "did anything change" is the robust question.
    ///
    /// This guard is the only check that can distinguish "task done" from
    /// "task never ran". Measured on a pristine worktree with zero task
    /// work, `cargo nextest run --workspace --all-features` reported
    /// `187 tests run: 187 passed`; a real run's `check_results` showed
    /// `fmt`, `clippy`, `test` and `build` ALL PASSED beside a failing
    /// `write-verification`. A green check on a tree nobody wrote to
    /// carries no information about the task, so the harness suite cannot
    /// be allowed to stand in for this.
    ///
    /// The legitimately no-op task (investigation-only) is handled by an
    /// EXPLICIT task-level signal — [`SDLCTask::expects_writes`] — rather
    /// than by inferring consent from an empty claim. A task that says
    /// nothing defaults to `expects_writes: true` and is therefore guarded;
    /// only an explicit `"expects_writes": false` in `tasks.json` disarms
    /// it.
    ///
    /// Still catches the guard's original target
    /// (`planning/decisions/D8-autonomous-node-write-permission.md`): a
    /// claimed, narrated write that never touched disk. That case is now a
    /// strict subset of "nothing changed", and the emitted message names
    /// the claim when there was one so triage keeps the same evidence.
    ///
    /// Trips as a failed [`CheckResult`], routed through the normal
    /// triage/retry machinery exactly like a harness-check failure — never
    /// a `NodeError` and never a hard bail.
    fn verify_claimed_writes(
        &self,
        ctx: &TaskContext,
        task: &SDLCTask,
        worktree: &Path,
    ) -> Option<CheckResult> {
        // An explicitly no-op task is the ONE way out. Checked before the
        // runner call because a task that is not expected to write has no
        // question to ask of the worktree.
        if !task.expects_writes {
            return None;
        }

        let changed = self.changed_files(worktree);
        if !changed.is_empty() {
            return None;
        }

        let modified_files: Vec<String> = get_result(ctx, "ImplementTaskNode")
            .and_then(|value| value.get("modified_files"))
            .and_then(|value| value.as_array())
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| entry.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        let claim = if modified_files.is_empty() {
            "ImplementTaskNode claimed no modified_files".to_string()
        } else {
            format!("ImplementTaskNode claimed modified_files {modified_files:?}")
        };

        Some(CheckResult {
            name: "write-verification".to_string(),
            kind: "write-verification".to_string(),
            passed: false,
            output: String::new(),
            message: format!(
                "{claim} and the worktree shows no changes at all (git status --porcelain \
                 reported nothing) — task {} is expected to write, so an unchanged worktree \
                 means the implement work never reached this tree. Harness checks passing \
                 against an untouched checkout say nothing about this task. If this task is \
                 genuinely investigation-only, declare \"expects_writes\": false on it in \
                 tasks.json.",
                task.task_id
            ),
        })
    }

    /// `pub(crate)` so [`super::final_validation::FinalValidationNode`] can
    /// share this exact executor (check-kind dispatch, the `enabled: false`
    /// skip, `gates` semantics) instead of forking a second copy — it
    /// constructs a throwaway `TestTaskNode` carrying its own runner purely
    /// as a handle onto this method.
    pub(crate) fn run_checks(
        &self,
        checks: &[serde_json::Value],
        worktree: &Path,
    ) -> (Vec<CheckResult>, Vec<String>) {
        let mut results = Vec::new();
        let mut failed_names = Vec::new();

        for check in checks {
            if check.get("enabled").and_then(|v| v.as_bool()) == Some(false) {
                continue;
            }

            let kind = check
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("command")
                .to_string();
            let result = match kind.as_str() {
                "command" => self.run_command_check(check, worktree),
                "forbidden-pattern-scan" => self.run_forbidden_pattern_scan(check, worktree),
                "baseline-diff" => self.run_baseline_diff(check, worktree),
                "count-delta" => self.run_count_delta(check, worktree),
                "warning-scan" => self.run_warning_scan(check, worktree),
                _ => self.run_unsupported_kind(check, &kind),
            };

            let gates = check.get("gates").and_then(|v| v.as_bool()).unwrap_or(true);
            if gates && !result.passed {
                failed_names.push(result.name.clone());
            }
            results.push(result);
        }

        (results, failed_names)
    }
}

/// Telemetry record for [`select_task_checks`]: which source produced the
/// checks that ran and what got dropped. Exists PURELY for standing rule 6's
/// "stamp the resolved value" requirement — `RunTelemetry`/`PolicyAggregate`
/// can attribute an observed cost/latency delta to the setting that caused
/// it. Nothing downstream branches on this struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CheckSelection {
    /// Exactly one of `"task_validation_commands"` or `"harness"`.
    source: &'static str,
    /// The depth this selection was resolved at (recorded even when the
    /// `task_validation_commands` branch ignored it, so the telemetry
    /// record always reflects the run's resolved policy).
    depth: TestDepth,
    /// Names of harness checks dropped from the run (`enabled: false`, or
    /// `perTask: false` when `apply_per_task_filter` is true). Always empty
    /// on the `task_validation_commands` branch, since nothing is dropped
    /// there.
    excluded: Vec<String>,
}

/// Pure selection of which checks `run_checks` should execute this attempt.
/// Mirrors `.claude/workflows/sdlc-flow.js` (commit `a21a95e`) exactly:
///
/// 1. If `task_validation_commands` is non-empty, it wins VERBATIM and
///    `depth` is ignored entirely — a self-validating task needs no harness
///    and no fast/full substitution. Each command is synthesized into a
///    `command`-kind check (`{"name": "task-validation-<i>", "kind":
///    "command", "command": "<cmd>", "gates": true}`, 1-indexed) so
///    `run_checks` needs no new executor branch.
/// 2. Otherwise, start from `harness_checks`, drop any check with
///    `enabled: false` (belt-and-braces — `run_checks` also drops these,
///    but excluding them here too keeps `excluded` a truthful, complete
///    record) and, when `apply_per_task_filter` is true, any check with
///    `perTask: false` (the JS filter at `sdlc-flow.js:548`). When
///    `depth == TestDepth::Fast` and a surviving check declares a non-empty
///    string `fastCommand`, return a clone with `command` replaced by that
///    `fastCommand` (the JS substitution at `sdlc-flow.js:624`) — falling
///    back to the check's own `command` when `fastCommand` is absent or not
///    a non-empty string. No other field (`gates`, `kind`, `purpose`,
///    `baselineCommand`, `compareKeys`, `_comment`, `fastCommand` itself,
///    ...) is touched, so `run_checks` and its kind-specific readers see the
///    check otherwise byte-identical.
///
/// `apply_per_task_filter` is the ONE extra parameter this function carries
/// for [`super::final_validation::FinalValidationNode`] (`EN.3.E`): rather
/// than duplicating this whole selection function for the run-level gate,
/// the run-level gate is expressed as one more boolean input. `TestTaskNode`
/// passes `true` (today's behavior, byte-identical); `FinalValidationNode`
/// passes `false` so a `"perTask": false` check — `planning/harness.json`'s
/// `build` check (`cargo build --release`) — IS included, because the
/// per-task tripwire's cost-saving exclusion has no bearing on a
/// once-per-run authoritative gate.
///
/// Pure by design: no runner, no filesystem, no policy stamp — the whole
/// precedence table is unit-testable by constructing `serde_json::Value`
/// arrays directly.
///
pub(crate) fn select_task_checks(
    harness_checks: &[serde_json::Value],
    task_validation_commands: &[String],
    depth: TestDepth,
    apply_per_task_filter: bool,
) -> (Vec<serde_json::Value>, CheckSelection) {
    if !task_validation_commands.is_empty() {
        let synthesized = task_validation_commands
            .iter()
            .enumerate()
            .map(|(i, cmd)| {
                json!({
                    "name": format!("task-validation-{}", i + 1),
                    "kind": "command",
                    "command": cmd,
                    "gates": true,
                })
            })
            .collect();
        return (
            synthesized,
            CheckSelection {
                source: "task_validation_commands",
                depth,
                excluded: Vec::new(),
            },
        );
    }

    let mut selected = Vec::new();
    let mut excluded = Vec::new();

    for check in harness_checks {
        let name = check
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        if check.get("enabled").and_then(|v| v.as_bool()) == Some(false) {
            excluded.push(name);
            continue;
        }
        if apply_per_task_filter && check.get("perTask").and_then(|v| v.as_bool()) == Some(false) {
            excluded.push(name);
            continue;
        }

        if depth == TestDepth::Fast {
            let fast_command = check
                .get("fastCommand")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty());
            if let Some(fast_command) = fast_command {
                let mut substituted = check.clone();
                if let Some(obj) = substituted.as_object_mut() {
                    obj.insert(
                        "command".to_string(),
                        serde_json::Value::String(fast_command.to_string()),
                    );
                }
                selected.push(substituted);
                continue;
            }
        }

        selected.push(check.clone());
    }

    (
        selected,
        CheckSelection {
            source: "harness",
            depth,
            excluded,
        },
    )
}

impl Default for TestTaskNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for TestTaskNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let worktree = worktree_path(&ctx)?;
        let worktree = Path::new(&worktree);

        // Depth comes from the resolved policy (task 4 makes `TestTaskNode`
        // policy-strict for the first time — see `resolved_policy`'s doc
        // comment for why this fails loudly rather than falling back).
        let policy = resolved_policy(&ctx)?;
        let depth = policy.test_depth;

        // The CURRENT task's own `validation_commands`, read from the live
        // durable state by `current_task_id` via the shared
        // [`current_task_state`] helper — `TaskQueueRouterNode`'s output is a
        // per-dispatch snapshot and does not carry `validation_commands` at all.
        let current_task_id = current_task_fields(&ctx)?
            .get("current_task_id")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| NodeError::new("TaskQueueRouterNode output missing current_task_id"))?
            as u32;
        let task = current_task_state(&ctx, "TestTaskNode")?;
        let task_validation_commands = task.validation_commands.clone();

        // Write-verification guard runs before the harness suite so a
        // claimed-but-empty implement never gets a free pass through checks
        // that happen to already be green (e.g. no `harness.json`).
        let write_verification = self.verify_claimed_writes(&ctx, &task, worktree);

        let harness_path = worktree.join("planning").join("harness.json");
        let harness_exists = harness_path.exists();
        let harness_checks: Vec<serde_json::Value> = if harness_exists {
            let raw = std::fs::read_to_string(&harness_path).map_err(|err| {
                NodeError::new(format!("failed to read {}: {err}", harness_path.display()))
            })?;
            let harness: serde_json::Value = serde_json::from_str(&raw).map_err(|err| {
                NodeError::new(format!("failed to parse {}: {err}", harness_path.display()))
            })?;
            harness
                .get("validation")
                .and_then(|v| v.get("checks"))
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        // No harness AND no self-validating `validation_commands`: there is
        // nothing to check. Previously this silently produced
        // `all_passed: true` with zero checks; now it is a gating
        // `harness-missing` failure instead.
        let (mut check_results, mut failed_names, selection) =
            if !harness_exists && task_validation_commands.is_empty() {
                let result = CheckResult {
                    name: "harness-missing".to_string(),
                    kind: "harness-missing".to_string(),
                    passed: false,
                    output: String::new(),
                    message: format!(
                        "no planning/harness.json found at {} and task {current_task_id} \
                         declares no validation_commands: nothing to validate against, so this \
                         is a gating failure rather than a silent pass",
                        harness_path.display()
                    ),
                };
                (
                    vec![result.clone()],
                    vec![result.name.clone()],
                    CheckSelection {
                        source: "harness",
                        depth,
                        excluded: Vec::new(),
                    },
                )
            } else {
                let (selected_checks, selection) =
                    select_task_checks(&harness_checks, &task_validation_commands, depth, true);
                let (results, failed) = self.run_checks(&selected_checks, worktree);
                (results, failed, selection)
            };

        if let Some(guard_result) = write_verification {
            failed_names.insert(0, guard_result.name.clone());
            check_results.insert(0, guard_result);
        }

        let all_passed = failed_names.is_empty();
        let failure_summary = if all_passed {
            String::new()
        } else {
            format!("Failed checks: {}", failed_names.join(", "))
        };

        put_result(
            &mut ctx,
            "TestTaskNode",
            json!({
                "all_passed": all_passed,
                "check_results": check_results,
                "failure_summary": failure_summary,
                "test_depth": serde_json::to_value(selection.depth)
                    .unwrap_or(serde_json::Value::Null),
                "check_source": selection.source,
                "excluded_checks": selection.excluded,
            }),
        );

        Ok(ctx)
    }

    fn name(&self) -> &str {
        "TestTaskNode"
    }
}

// --- TriageTaskNode -----------------------------------------------------------

/// Node that classifies a task's test-failure output into
/// `PASS`/`RETRYABLE`/`MAJOR_BAIL`. Deterministic by default (a passing test
/// forces `PASS`; an over-budget task forces `MAJOR_BAIL`; a failing task
/// still under budget is deterministically `RETRYABLE`), consulting a
/// `ClaudeCodeStep` (Sonnet) only when triage is enabled: the bare
/// `event.llm_triage` field wins if set, else the resolved policy's
/// `llm_triage` (see `resolved_policy` above).
pub struct TriageTaskNode {
    config: Config,
    transport: TransportSlot,
    runner: CommandRunner,
}

#[derive(Debug, Deserialize)]
struct TriageOutput {
    verdict: String,
    reason: String,
}

/// JSON schema matching [`TriageOutput`].
fn triage_output_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "verdict": { "type": "string" },
            "reason": { "type": "string" },
        },
        "required": ["verdict", "reason"],
    })
}

impl TriageTaskNode {
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: Config {
                model: Some("claude-sonnet-4-5".to_string()),
                ..Config::default()
            },
            transport: TransportSlot::default(),
            runner: super::default_command_runner(),
        }
    }

    /// Override the transport used by the composed `ClaudeCodeStep` for the
    /// `llm_triage` model branch. Tests use this to assert it is (or isn't)
    /// invoked.
    #[must_use]
    pub fn with_transport(mut self, transport: ModelTransport) -> Self {
        self.transport.set_plain(transport);
        self
    }

    /// Override the transport with a tier-aware [`MetaTransport`] that
    /// reports the [`TransportInfo`] of whichever call actually executed
    /// (e.g. local vs. cloud fallback), taking precedence over a plain
    /// transport set via [`Self::with_transport`].
    #[must_use]
    pub fn with_meta_transport(mut self, transport: MetaTransport) -> Self {
        self.transport.set_meta(transport);
        self
    }

    /// Override the command runner used for the `git diff --numstat`
    /// trivial-classification invocation. Tests use this to stub the
    /// subprocess.
    #[must_use]
    pub fn with_runner(mut self, runner: CommandRunner) -> Self {
        self.runner = runner;
        self
    }

    /// Override the base `Config` entirely. Live/manual tests use this to
    /// set `isolated: true` when driving a real `claude` call from inside
    /// another interactive session (see `Config::isolated`'s doc comment).
    #[must_use]
    pub fn with_config(mut self, config: Config) -> Self {
        self.config = config;
        self
    }
}

impl Default for TriageTaskNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for TriageTaskNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let test_result = get_result(&ctx, "TestTaskNode")
            .cloned()
            .ok_or_else(|| NodeError::new("TestTaskNode has not run yet"))?;
        let current = current_task_fields(&ctx)?.clone();

        // `attempt_count`/`max_attempts` come from the *live* durable state via
        // the shared [`current_task_state`] helper, not from `current`
        // (`TaskQueueRouterNode`'s per-dispatch snapshot). `attempt_count` on
        // that snapshot is now re-stamped by `IncrementAttemptNode`
        // (`ticket-restamp-attempt-count`), so the two agree — but the durable
        // state stays the single authority for the bail gate: it is what
        // `bump_task_attempt` actually mutates, and `max_attempts` is only ever
        // sourced from there. See this spec's Amendment Log (EN.3.B
        // retry-bail fix).
        let task = current_task_state(&ctx, "TriageTaskNode")?;
        let attempt_count = u64::from(task.attempt_count);
        let max_attempts = u64::from(task.max_attempts);

        let all_passed = test_result
            .get("all_passed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if all_passed {
            let policy = resolved_policy(&ctx)?;
            let trivial = classify_trivial(&ctx, &self.runner, &policy);
            put_result(
                &mut ctx,
                "TriageTaskNode",
                json!({
                    "verdict": "PASS",
                    "reason": "All harness checks passed.",
                    "trivial": trivial,
                }),
            );
            return Ok(ctx);
        }

        if attempt_count >= max_attempts {
            let task_id = task.task_id;
            put_result(
                &mut ctx,
                "TriageTaskNode",
                json!({
                    "verdict": "MAJOR_BAIL",
                    "reason": format!(
                        "Max attempts ({max_attempts}) reached without a passing run. \
                         To recover: re-run just this task with `retry_task: {task_id}` \
                         (keeps every other task's status, attempt count and commits), \
                         or restart the whole spec with `resume: false` (archives the \
                         current state and reruns from scratch)."
                    ),
                }),
            );
            return Ok(ctx);
        }

        // Precedence: the bare `event.llm_triage` field (the pre-existing,
        // still-supported spelling — `SDLCFlowEventSchema::llm_triage`,
        // `schema.rs:157`) wins when a caller sets it explicitly; otherwise
        // fall through to the resolved policy's `llm_triage` (profile /
        // `harness.json` / per-run `policy` override), which is the
        // canonical spelling going forward. Both are read via `resolved_policy`
        // above so a bad policy layer still errors loudly rather than being
        // silently ignored the way it was before this knob was wired.
        let llm_triage = match ctx.event.get("llm_triage").and_then(|v| v.as_bool()) {
            Some(bare) => bare,
            None => resolved_policy(&ctx)?.llm_triage,
        };

        if !llm_triage {
            put_result(
                &mut ctx,
                "TriageTaskNode",
                json!({
                    "verdict": "RETRYABLE",
                    "reason": format!(
                        "Checks failed; retrying (attempt {} of {max_attempts}).",
                        attempt_count + 1
                    ),
                }),
            );
            return Ok(ctx);
        }

        let failure_summary = test_result
            .get("failure_summary")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let title = current
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        let mut prompt = format!(
            "Classify this task's test failure as RETRYABLE or MAJOR_BAIL. \
             Respond with strict JSON of the shape {{\"verdict\": str, \
             \"reason\": str}}.\n\nTask: {title}\nAttempt {} of \
             {max_attempts}.\nFailure summary: {failure_summary}",
            attempt_count + 1
        );

        let policy = resolved_policy(&ctx)?;

        // `failure_summary` names the failed checks but never says *why* they
        // failed, so the classifier was judging nearly blind. Append the
        // captured check output — the same `check_results[]` data
        // `ImplementTaskNode`'s retry feedback renders, under a
        // classifier-facing header. Appended to the per-run prompt **body**,
        // never to `STABLE_SYSTEM_PROMPT`: run-varying text in the cached
        // prefix would break the cache breakpoint on every call.
        if let Some(feedback) = triage_failure_feedback(&ctx, &policy.retry_feedback) {
            prompt.push_str(&feedback);
        }

        let (mut config, prompt) =
            apply_policy(self.config.clone(), prompt, &policy, Stage::Triage);
        config.json_schema = Some(triage_output_schema());

        let step = self.transport.apply(
            ClaudeCodeStep::new("TriageTaskNode", config, prompt)
                .with_retry_policy(policy.transport_retry),
        );

        let mut ctx = step.process(ctx).await?;
        let content = ctx
            .nodes
            .get("TriageTaskNode")
            .and_then(|value| value.get("content"))
            .and_then(|value| value.as_str())
            .ok_or_else(|| NodeError::new("TriageTaskNode: model returned no content"))?
            .to_string();
        // Carried forward below: `put_result` replaces this node's whole
        // `ctx.nodes` entry, which would otherwise silently drop the
        // `"transport"` stamp `ClaudeCodeStep::process` just wrote — the
        // exact tier-telemetry `RunTelemetry`/`observed_model_tiers`
        // (`policy/telemetry.rs`) reads back out by this same node name.
        let transport_stamp = ctx
            .nodes
            .get("TriageTaskNode")
            .and_then(|value| value.get("transport"))
            .cloned();

        let parsed: TriageOutput = parse_structured_or_fenced(&ctx, "TriageTaskNode", &content)
            .map_err(|err| {
                NodeError::new(format!(
                    "TriageTaskNode: failed to parse model output as JSON: {err}"
                ))
            })?;

        let normalized_verdict = parsed.verdict.trim().to_uppercase();
        let mut result = json!({
            // Same normalization as `ConsolidatedReviewNode`'s, and for the
            // same reason — `TriageRouterNode` exact-matches this string.
            // Normalization narrows the hole (see the observed-live `"pass"`
            // reply that motivated it); it does not close it — an
            // unrecognized value still needs `TriageRouterNode`'s fallback
            // arm below to guarantee the walk reaches `WrapUpNode`.
            "verdict": normalized_verdict,
            "reason": parsed.reason,
        });
        if !matches!(
            normalized_verdict.as_str(),
            "PASS" | "RETRYABLE" | "MAJOR_BAIL"
        ) {
            result["unrecognized_verdict"] = json!(normalized_verdict);
        }
        if let Some(transport) = transport_stamp {
            result["transport"] = transport;
        }
        put_result(&mut ctx, "TriageTaskNode", result);

        Ok(ctx)
    }

    fn name(&self) -> &str {
        "TriageTaskNode"
    }
}

/// Deterministic router: branches on `TriageTaskNode`'s stored verdict.
pub struct TriageRouterNode;

#[async_trait::async_trait]
impl Node for TriageRouterNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "TriageRouterNode"
    }

    fn as_router(&self) -> Option<&dyn Router> {
        Some(self)
    }
}

impl Router for TriageRouterNode {
    fn route(&self, ctx: &TaskContext) -> Option<String> {
        let triage = get_result(ctx, "TriageTaskNode")?;
        let verdict = triage.get("verdict")?.as_str()?;
        match verdict {
            // `review_mode` (lever #3a) decides whether a `PASS` verdict
            // still routes to `ConsolidatedReviewNode`:
            //   - `PerTask` (built-in default): unchanged, always review —
            //     reproduces pre-EN.3.C behavior byte-for-byte.
            //   - `EndOnly`: per-task review is collapsed away entirely (a
            //     single end-of-run review happens elsewhere), so every
            //     `PASS` skips straight to `UpdateTaskStatusNode`.
            //   - `TrivialSkip`: only a task `TriageTaskNode` classified
            //     `trivial` (small diff under `review_skip_max_files`/
            //     `review_skip_max_diff_lines`) skips review; a non-trivial
            //     `PASS` still routes to `ConsolidatedReviewNode`.
            "PASS" => {
                let policy = resolved_policy(ctx).ok()?;
                match policy.review_mode {
                    ReviewMode::PerTask => Some("ConsolidatedReviewNode".to_string()),
                    ReviewMode::EndOnly => Some("UpdateTaskStatusNode".to_string()),
                    ReviewMode::TrivialSkip => {
                        let trivial = triage
                            .get("trivial")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        if trivial {
                            Some("UpdateTaskStatusNode".to_string())
                        } else {
                            Some("ConsolidatedReviewNode".to_string())
                        }
                    }
                }
            }
            // Retry back-edge (EN.3.B fix): routes through `IncrementAttemptNode`
            // first (not straight to `ImplementTaskNode`) so the durable
            // `attempt_count`/`total_attempts` counters actually advance —
            // `Router::route` takes `&ctx` and cannot mutate state itself.
            "RETRYABLE" => Some("IncrementAttemptNode".to_string()),
            "MAJOR_BAIL" => Some("WrapUpNode".to_string()),
            // An unrecognized verdict must never silently halt the walk
            // mid-graph — `WrapUpNode` is already a declared connection from
            // this router (see `graph.rs`), so routing here is a no-op on
            // the graph shape. `TriageTaskNode::process` stamps
            // `unrecognized_verdict` alongside the (unchanged) `verdict` key
            // so `derive_terminal_signal` can surface the offending string
            // in the run's `bail_reason`.
            _ => Some("WrapUpNode".to_string()),
        }
    }
}

// --- ConsolidatedReviewNode ----------------------------------------------------

/// Model node (Sonnet): reviews the task's working-tree diff against `HEAD`
/// (`git add -N -A` then `git diff HEAD` — see [`stage_untracked_intent`])
/// against its acceptance criteria via a composed `ClaudeCodeStep`.
pub struct ConsolidatedReviewNode {
    config: Config,
    transport: TransportSlot,
    runner: CommandRunner,
}

/// The model's raw review reply shape — shared with `end_review::EndReviewNode`
/// (`pub(super)`) so the per-task and end-of-run reviewers parse the same
/// JSON and produce the same result shape; nothing downstream needs a
/// special case for which one ran.
#[derive(Debug, Deserialize)]
pub(super) struct ReviewOutput {
    pub(super) verdict: String,
    pub(super) summary: String,
    #[serde(default)]
    pub(super) issues: Vec<String>,
}

/// JSON schema matching [`ReviewOutput`]. Also used by
/// `end_review::EndReviewNode`.
pub(super) fn review_output_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "verdict": { "type": "string" },
            "summary": { "type": "string" },
            "issues": { "type": "array", "items": { "type": "string" } },
        },
        "required": ["verdict", "summary"],
    })
}

impl ConsolidatedReviewNode {
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: Config {
                model: Some("claude-sonnet-4-5".to_string()),
                ..Config::default()
            },
            transport: TransportSlot::default(),
            runner: super::default_command_runner(),
        }
    }

    /// Override the transport used by the composed `ClaudeCodeStep`.
    #[must_use]
    pub fn with_transport(mut self, transport: ModelTransport) -> Self {
        self.transport.set_plain(transport);
        self
    }

    /// Override the transport with a tier-aware [`MetaTransport`] that
    /// reports the [`TransportInfo`] of whichever call actually executed
    /// (e.g. local vs. cloud fallback), taking precedence over a plain
    /// transport set via [`Self::with_transport`].
    #[must_use]
    pub fn with_meta_transport(mut self, transport: MetaTransport) -> Self {
        self.transport.set_meta(transport);
        self
    }

    /// Override the command runner used for the `git diff` invocation.
    #[must_use]
    pub fn with_runner(mut self, runner: CommandRunner) -> Self {
        self.runner = runner;
        self
    }

    /// Override the base `Config` entirely. Live/manual tests use this to
    /// set `isolated: true` when driving a real `claude` call from inside
    /// another interactive session (see `Config::isolated`'s doc comment).
    #[must_use]
    pub fn with_config(mut self, config: Config) -> Self {
        self.config = config;
        self
    }
}

impl Default for ConsolidatedReviewNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for ConsolidatedReviewNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let worktree = worktree_path(&ctx)?;
        let current = current_task_fields(&ctx)?.clone();
        let acceptance_criteria = current
            .get("acceptance_criteria")
            .cloned()
            .unwrap_or_else(|| json!([]));

        // Bump the run-level `telemetry.review_attempts` ACCOUNTING counter
        // (EN.ticket.review-retry-loop-unbounded task 2) BEFORE the model
        // call, same as `ImplementTaskNode` counts the attempt regardless of its
        // outcome — this node is about to produce a verdict, so the review
        // pass it spends counts whether that verdict is PASS, FAIL, or
        // PARTIAL. This is also what keeps `logical_clock` monotonic across
        // every state write in the loop.
        //
        // It is NOT the retry bound. The bound is charged per-task and only
        // by a NON-PASS verdict, below, once the verdict is known — see
        // [`bump_task_review_attempt`]. Neither counter is
        // `attempt_count`/`total_attempts`; see `SDLCTelemetry::
        // review_attempts`'s doc comment for why they must stay independent.
        let mut counted_state = latest_state(&ctx)?;
        counted_state.telemetry.review_attempts += 1;

        // The reviewer must see the CURRENT task's actual work. Nothing in
        // this run commits code until `SaveStateNode` runs on the pass path,
        // so the reviewable delta lives in the working tree, not in a commit
        // range — `<base_sha>..HEAD` (what this used to diff) was empty on
        // every run, which is why every past review verdict was a rubber
        // stamp. Intent-to-add first so brand-new files appear with content.
        stage_untracked_intent(&self.runner, Path::new(&worktree));
        let diff = (self.runner)("git", &["diff", "HEAD"], Path::new(&worktree))
            .map(|output| output.stdout)
            .unwrap_or_default();

        let policy = resolved_policy(&ctx)?;
        // That real diff is unbounded — it is what makes this prompt's size
        // (and cost) scale with the task. Bound it, visibly.
        let diff_budget = policy.review_diff_max_chars;
        let (diff, diff_truncated) = bound_review_diff(&diff, diff_budget as usize);

        // Policy-varying text lives in the per-run prompt BODY only — never
        // in a `STABLE_SYSTEM_PROMPT` prefix, whose cache breakpoint must
        // stay run-invariant (CLAUDE.md standing rule 6).
        let prompt = format!(
            "Review this task's diff against its acceptance criteria. \
             Respond with strict JSON of the shape {{\"verdict\": str, \
             \"summary\": str, \"issues\": [str]}}.\n\nAcceptance criteria: \
             {acceptance_criteria}\n\nDiff:\n{diff}"
        );

        let (mut config, prompt) =
            apply_policy(self.config.clone(), prompt, &policy, Stage::Review);
        // Scope the model's session to the actual worktree, matching
        // `ImplementTaskNode`'s fix — without this, a real call that reads
        // the filesystem checks the host process's ambient cwd instead of
        // the task's worktree (observed live: the model correctly reported
        // the file it was asked to review as "missing", because it was
        // looking in the wrong directory).
        config.cwd = Some(std::path::PathBuf::from(&worktree));
        config.json_schema = Some(review_output_schema());

        let step = self.transport.apply(
            ClaudeCodeStep::new("ConsolidatedReviewNode", config, prompt)
                .with_retry_policy(policy.transport_retry),
        );

        let mut ctx = step.process(ctx).await?;
        let content = ctx
            .nodes
            .get("ConsolidatedReviewNode")
            .and_then(|value| value.get("content"))
            .and_then(|value| value.as_str())
            .ok_or_else(|| NodeError::new("ConsolidatedReviewNode: model returned no content"))?
            .to_string();
        // Carried forward below: `put_result` replaces this node's whole
        // `ctx.nodes` entry, which would otherwise silently drop the
        // `"transport"` stamp `ClaudeCodeStep::process` just wrote — the
        // exact tier-telemetry `RunTelemetry`/`observed_model_tiers`
        // (`policy/telemetry.rs`) reads back out by this same node name.
        let transport_stamp = ctx
            .nodes
            .get("ConsolidatedReviewNode")
            .and_then(|value| value.get("transport"))
            .cloned();

        let parsed: ReviewOutput =
            parse_structured_or_fenced(&ctx, "ConsolidatedReviewNode", &content).map_err(
                |err| {
                    NodeError::new(format!(
                        "ConsolidatedReviewNode: failed to parse model output as JSON: {err}"
                    ))
                },
            )?;

        let normalized_verdict = parsed.verdict.trim().to_uppercase();
        let mut result = json!({
            // Normalized to the canonical uppercase form `ReviewRouterNode`
            // matches on — a real model reply doesn't reliably preserve
            // the exact casing asked for (observed live: a real Sonnet
            // reply returned "pass"). Normalization narrows the hole; it
            // does not close it — an unrecognized value still needs
            // `ReviewRouterNode`'s fallback arm below to guarantee the walk
            // reaches `WrapUpNode` instead of silently halting here.
            "verdict": normalized_verdict,
            "summary": parsed.summary,
            "issues": parsed.issues,
            // Standing rule 6: stamp the RESOLVED knob value (and whether it
            // actually bit) so `RunTelemetry`/`PolicyAggregate` can attribute
            // an observed cost — or a thin verdict — to the setting that
            // caused it.
            "review_diff_max_chars": diff_budget,
            "review_diff_truncated": diff_truncated,
        });
        if !matches!(normalized_verdict.as_str(), "PASS" | "FAIL" | "PARTIAL") {
            result["unrecognized_verdict"] = json!(normalized_verdict);
        }
        // The retry bound's counter: charged to THIS task, and only when the
        // verdict is one that can send the run back around the loop. A PASS
        // ends this task's review cycle rather than extending it, so it costs
        // nothing — before this, two earlier tasks' PASSes could exhaust a
        // third task's whole budget on its FIRST verdict (measured, run R6).
        let current_task_id = current_task_fields(&ctx)?
            .get("current_task_id")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| NodeError::new("TaskQueueRouterNode output missing current_task_id"))?
            as u32;
        let task_review_attempts = if normalized_verdict == "PASS" {
            task_review_attempts(&counted_state, current_task_id)
        } else {
            bump_task_review_attempt(&mut counted_state, current_task_id)?
        };
        // Standing rule 6: stamp the counter the bound is measured against
        // right next to the verdict that moved it, so `ReviewRouterNode` and
        // `wrap_up::derive_terminal_signal` read the same number this pass
        // produced rather than each re-deriving it.
        result["task_review_attempts"] = json!(task_review_attempts);
        if let Some(transport) = transport_stamp {
            result["transport"] = transport;
        }
        // Nested under `"state"` rather than replacing this result outright
        // — the object above IS the review verdict `ReviewRouterNode` reads
        // via `get_result(ctx, "ConsolidatedReviewNode")`, so the durable
        // `SDLCState` (carrying the just-bumped `review_attempts`) has to
        // ride alongside it, not instead of it. `latest_state` (this file)
        // knows to unwrap this key for this one node identity.
        result["state"] = serde_json::to_value(&counted_state)
            .map_err(|err| NodeError::new(format!("failed to serialize SDLCState: {err}")))?;
        put_result(&mut ctx, "ConsolidatedReviewNode", result);

        Ok(ctx)
    }

    fn name(&self) -> &str {
        "ConsolidatedReviewNode"
    }
}

/// Deterministic router: branches on `ConsolidatedReviewNode`'s stored
/// verdict, distinguishing "structural" from "minor" failures by issue
/// count.
pub struct ReviewRouterNode;

#[async_trait::async_trait]
impl Node for ReviewRouterNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "ReviewRouterNode"
    }

    fn as_router(&self) -> Option<&dyn Router> {
        Some(self)
    }
}

impl Router for ReviewRouterNode {
    fn route(&self, ctx: &TaskContext) -> Option<String> {
        let review = get_result(ctx, "ConsolidatedReviewNode")?;
        let verdict = review.get("verdict")?.as_str()?;
        let issues = review
            .get("issues")
            .and_then(|v| v.as_array())
            .map(|v| v.len())
            .unwrap_or(0);

        match verdict {
            "PASS" => Some("UpdateTaskStatusNode".to_string()),
            "FAIL" | "PARTIAL" => {
                if issues == 0 || issues > STRUCTURAL_ISSUE_THRESHOLD {
                    Some("WrapUpNode".to_string())
                } else {
                    // Minor-issue retry back-edge (EN.3.B fix): same reasoning
                    // as `TriageRouterNode`'s `RETRYABLE` branch — route
                    // through `IncrementAttemptNode` so the retry counters
                    // advance in lockstep across both back-edges.
                    //
                    // EN.ticket.review-retry-loop-unbounded task 3: this is
                    // the back-edge that was unbounded — `TriageTaskNode`'s
                    // own attempt-cap check sits after its `PASS` early
                    // return, so a passing test run never reaches it, and
                    // the cycle Implement -> Test(pass) -> Triage(PASS) ->
                    // Review(FAIL/PARTIAL, minor) -> IncrementAttempt ->
                    // Implement had no exit. The bound has to close here,
                    // where the back-edge is actually chosen: once the
                    // CURRENT TASK's durable `review_attempt_count` (its
                    // non-PASS verdicts, bumped by `ConsolidatedReviewNode`)
                    // reaches `policy.max_review_attempts`, route to
                    // `WrapUpNode` with the last verdict's summary instead of
                    // looping again.
                    //
                    // Per-task and non-PASS-only, NOT the run-level
                    // `telemetry.review_attempts` this used to read: that
                    // counter charges successful reviews and never resets,
                    // so on a default profile the third task to reach review
                    // was the last one that could be reviewed at all (run
                    // R6). See `SDLCTask::review_attempt_count`.
                    let review_attempts = bounded_review_attempts(ctx, review);
                    let max_review_attempts =
                        resolved_policy(ctx).map(|p| p.max_review_attempts).ok();
                    match max_review_attempts {
                        Some(max) if review_attempts >= max => Some("WrapUpNode".to_string()),
                        _ => Some("IncrementAttemptNode".to_string()),
                    }
                }
            }
            // An unrecognized verdict must never silently halt the walk
            // mid-graph — `WrapUpNode` is already a declared connection
            // from this router (see `graph.rs`). `ConsolidatedReviewNode`
            // stamps `unrecognized_verdict` alongside the (unchanged)
            // `verdict` key so `derive_terminal_signal` can surface the
            // offending string in the run's `bail_reason`.
            _ => Some("WrapUpNode".to_string()),
        }
    }
}

/// The current task's `review_attempt_count` in `state`, or `0` when no such
/// task exists (a ctx assembled by a test that drives a node in isolation).
fn task_review_attempts(state: &SDLCState, task_id: u32) -> u32 {
    state
        .tasks
        .iter()
        .find(|task| task.task_id == task_id)
        .map(|task| task.review_attempt_count)
        .unwrap_or(0)
}

/// Bump the task identified by `task_id`'s `review_attempt_count` by exactly
/// one and return the new value. Charged only for a NON-PASS
/// `ConsolidatedReviewNode` verdict — the one that can send the run back
/// around the review loop. Deliberately separate from [`bump_task_attempt`]'s
/// `attempt_count` (see `SDLCTelemetry::review_attempts`)
/// and from `telemetry.review_attempts`' run-level accounting.
fn bump_task_review_attempt(state: &mut SDLCState, task_id: u32) -> Result<u32, NodeError> {
    let spec_slug = state.spec_slug.clone();
    let task = state
        .tasks
        .iter_mut()
        .find(|task| task.task_id == task_id)
        .ok_or_else(|| {
            NodeError::new(format!(
                "no task with task_id={task_id} found in state for spec {spec_slug:?}"
            ))
        })?;
    task.review_attempt_count += 1;
    Ok(task.review_attempt_count)
}

/// The number `policy.max_review_attempts` is compared against: the CURRENT
/// task's non-PASS review verdicts, including the one on the board.
///
/// Reads `ConsolidatedReviewNode`'s own `task_review_attempts` stamp first —
/// the value that node computed when it wrote the verdict `review` — and
/// falls back to looking the current task up in the durable state for a ctx
/// assembled without the stamp (a test driving a node directly, or a state
/// file written before the field existed). `0` when neither is available,
/// which leaves the retry back-edge open rather than bailing a run on a
/// number nobody wrote.
pub(crate) fn bounded_review_attempts(ctx: &TaskContext, review: &serde_json::Value) -> u32 {
    if let Some(stamped) = review
        .get("task_review_attempts")
        .and_then(serde_json::Value::as_u64)
    {
        return stamped as u32;
    }
    let Ok(state) = latest_state(ctx) else {
        return 0;
    };
    let Some(task_id) = current_task_fields(ctx)
        .ok()
        .and_then(|fields| fields.get("current_task_id"))
        .and_then(serde_json::Value::as_u64)
    else {
        return 0;
    };
    task_review_attempts(&state, task_id as u32)
}

/// Bump the task identified by `task_id`'s `attempt_count` by exactly one.
///
/// **`attempt_count` counts RETRIES, not attempts** — a task that passes on
/// its first try ends with `attempt_count == 0` — which is why this is
/// charged here, on the retry back-edge, and not once per attempt. The
/// run-level `telemetry.total_attempts` is a different quantity and is
/// charged in a different place ([`ImplementTaskNode`], once per attempt
/// made); the two are deliberately not reconcilable into one number. See
/// `SDLCTask::attempt_count` and `SDLCTelemetry::total_attempts`.
///
/// Sole caller: [`IncrementAttemptNode`], the target of both retry
/// back-edges.
fn bump_task_attempt(state: &mut SDLCState, task_id: u32) -> Result<(), NodeError> {
    let spec_slug = state.spec_slug.clone();
    let task = state
        .tasks
        .iter_mut()
        .find(|task| task.task_id == task_id)
        .ok_or_else(|| {
            NodeError::new(format!(
                "no task with task_id={task_id} found in state for spec {spec_slug:?}"
            ))
        })?;
    task.attempt_count += 1;
    Ok(())
}

// --- IncrementAttemptNode ---------------------------------------------------

/// Deterministic node: the retry back-edge target for both
/// `TriageRouterNode`'s `RETRYABLE` verdict and `ReviewRouterNode`'s minor
/// `FAIL`/`PARTIAL` verdict (EN.3.B retry-bail fix). Bumps the current
/// task's `attempt_count` — the RETRY counter — in the durable `SDLCState`
/// via [`bump_task_attempt`], then hands off to `ImplementTaskNode` for the
/// retry (the forward hop is a declared graph connection — see `graph.rs`).
///
/// It does NOT touch `telemetry.total_attempts`: the retry it is about to
/// send round is counted as an attempt by `ImplementTaskNode` when that
/// attempt is actually made, one site for every attempt in the run. Charging
/// it here as well would double-count every retry.
///
/// `Router::route(&self, ctx: &TaskContext)` takes `&ctx` and cannot mutate
/// state, so the increment cannot live in `TriageRouterNode`'s or
/// `ReviewRouterNode`'s routing logic itself — it must be a real `Node`
/// sitting on both back-edges, between the router and `ImplementTaskNode`.
///
/// It also **re-stamps `attempt_count` onto `TaskQueueRouterNode`'s output**
/// (`ticket-restamp-attempt-count`). That entry is a snapshot taken once at
/// dequeue; before this fix nothing refreshed it, so
/// `current_task_fields(ctx)["attempt_count"]` read `0` on every retry and
/// three separate call sites had grown comments warning readers off it. The
/// re-stamp is a read-modify-write of the existing JSON object — every other
/// field the router stamped (`title`, `description`, `acceptance_criteria`,
/// `max_attempts`) survives untouched.
pub struct IncrementAttemptNode;

#[async_trait::async_trait]
impl Node for IncrementAttemptNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let current_task_id = current_task_fields(&ctx)?
            .get("current_task_id")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| NodeError::new("TaskQueueRouterNode output missing current_task_id"))?
            as u32;

        let mut state = latest_state(&ctx)?;
        bump_task_attempt(&mut state, current_task_id)?;

        // Re-stamp the router snapshot's `attempt_count` with the value
        // `bump_task_attempt` just wrote, so `current_task_fields` stops lying on
        // the retry back-edge. Read-modify-write: touch only this one key.
        let bumped = state
            .tasks
            .iter()
            .find(|task| task.task_id == current_task_id)
            .map(|task| task.attempt_count);
        if let Some(attempt_count) = bumped {
            if let Some(entry) = ctx
                .nodes
                .get_mut("TaskQueueRouterNode")
                .and_then(serde_json::Value::as_object_mut)
            {
                entry.insert("attempt_count".to_string(), json!(attempt_count));
            }
        }

        let value = serde_json::to_value(&state)
            .map_err(|err| NodeError::new(format!("failed to serialize SDLCState: {err}")))?;
        put_result(&mut ctx, "IncrementAttemptNode", value);
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "IncrementAttemptNode"
    }
}

// --- UpdateTaskStatusNode --------------------------------------------------

/// Deterministic node: marks the current task DONE in the durable
/// `SDLCState` and charges it to `telemetry.tasks_passed`.
///
/// **Only ever entered with a `PASS` triage verdict, in both workflows**, so
/// PASS is the only verdict it accepts; anything else is a graph defect and
/// fails closed with an error rather than being silently counted:
///
/// - `SDLC_FLOW`: two inbound routes, both PASS arms. `ReviewRouterNode`'s
///   `PASS` — reachable only via `TriageRouterNode`'s own `PASS` arm ->
///   `ConsolidatedReviewNode` — and, under `review_mode: EndOnly` (or
///   `TrivialSkip` on a task classified trivial), `TriageRouterNode`'s
///   `PASS` arm routing here directly. The verdict read below comes from
///   `TriageTaskNode`, not from the review, so either way what arrives is a
///   PASS triage verdict. `RETRYABLE` goes to `IncrementAttemptNode` and
///   `MAJOR_BAIL` to `WrapUpNode` (EN.3.B).
/// - `SDLC_TASK`: its only inbound route is `TaskTriageRouterNode`, which
///   has three arms and only three — `PASS` here, under-budget `RETRYABLE`
///   to `IncrementAttemptNode`, everything else (`MAJOR_BAIL`, exhausted
///   `RETRYABLE`, unparseable) to `LeanBookkeepNode`, fail-closed.
///
/// The former `MajorBail` and `Retryable` arms were dead code on both
/// counts. `Retryable` was already labelled unreachable "as of EN.3.B";
/// `MajorBail` was the same class and simply never labelled, and its
/// `total_attempts += 1` was one of the outcome-charged sites that made a
/// bail's attempt uncountable (see [`ImplementTaskNode`]). Both are deleted:
/// a counter must not have a site on a path that cannot execute, and
/// `tasks_failed` consequently has no writer at all — which is the state
/// `wrap_up.rs` already documents and derives its outcome around ("a bailed
/// run has `tasks_failed == 0` structurally").
pub struct UpdateTaskStatusNode;

#[async_trait::async_trait]
impl Node for UpdateTaskStatusNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let current_task_id = current_task_fields(&ctx)?
            .get("current_task_id")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| NodeError::new("TaskQueueRouterNode output missing current_task_id"))?
            as u32;
        let verdict_str = get_result(&ctx, "TriageTaskNode")
            .and_then(|v| v.get("verdict"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| NodeError::new("TriageTaskNode has not run yet"))?
            .to_string();
        if verdict_str != "PASS" {
            return Err(NodeError::new(format!(
                "UpdateTaskStatusNode: reached with a non-PASS triage verdict \
                 {verdict_str:?}; every route into this node in both SDLC_FLOW \
                 and SDLC_TASK carries PASS (see this node's doc comment)"
            )));
        }

        let mut state = latest_state(&ctx)?;
        let spec_slug = state.spec_slug.clone();

        let task = state
            .tasks
            .iter_mut()
            .find(|task| task.task_id == current_task_id)
            .ok_or_else(|| {
                NodeError::new(format!(
                    "UpdateTaskStatusNode: no task with task_id={current_task_id} found \
                     in state for spec {spec_slug:?}"
                ))
            })?;
        task.status = SDLCTaskStatus::Done;
        // The one counter this write advances — see [`logical_clock`]. The
        // attempt that produced this PASS was already counted where it was
        // made, in `ImplementTaskNode`; counting it again here would
        // double-charge every task's last attempt.
        state.telemetry.tasks_passed += 1;

        let value = serde_json::to_value(&state)
            .map_err(|err| NodeError::new(format!("failed to serialize SDLCState: {err}")))?;
        put_result(&mut ctx, "UpdateTaskStatusNode", value);
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "UpdateTaskStatusNode"
    }
}

// --- SaveStateNode ----------------------------------------------------------

/// Read the `branch_name` stamped by `SetupWorktreeNode`, if the run went
/// through it. Absent in unit tests that drive `SaveStateNode` directly.
fn branch_name(ctx: &TaskContext) -> Option<String> {
    get_result(ctx, "SetupWorktreeNode")
        .and_then(|value| value.get("branch_name"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

/// Read `started_at` back out of an already-committed D31 state file at
/// `state_path`, if one exists and parses. Used to preserve the run's
/// original start time across a resumed run's per-task saves — every write
/// after the first must NOT stamp a fresh `started_at`, or a resumed run
/// would appear to restart its wall-clock every time `SaveStateNode` fires.
fn existing_started_at(state_path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(state_path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    SDLCState::from_committed_state_json(&value)
        .ok()?
        .started_at
}

/// Build this write's [`RunMeta`]: `branch`/`worktree_path` come straight
/// from `SetupWorktreeNode`'s output; `started_at` is preserved from an
/// existing on-disk committed file if one is already there (a resume),
/// otherwise stamped fresh (this run's first save); `updated_at` is always
/// stamped fresh; `run_id` is read back out of `ctx.metadata` via
/// [`crate::read_run_id`] — the stamp `Workflow::run_with`/`run_from` write
/// before the walk starts (`None` when the run carried no `RunOptions::run_id`,
/// e.g. any run driven by base-template's JS `sdlc-flow.js` engine).
fn build_run_meta(ctx: &TaskContext, worktree: &str, state_path: &Path) -> RunMeta {
    let now = chrono::Utc::now()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        .to_string();
    let started_at = existing_started_at(state_path).unwrap_or_else(|| now.clone());
    RunMeta {
        branch: branch_name(ctx).unwrap_or_default(),
        worktree_path: worktree.to_string(),
        started_at,
        updated_at: now,
        run_id: crate::read_run_id(&ctx.metadata),
    }
}

/// Deterministic node: serializes the latest `SDLCState` to
/// `planning/{spec_slug}/sdlc/sdlc-flow-state.json` inside the worktree
/// (the D31-committed path/schema shared with base-template's JS
/// `sdlc-flow.js` engine — see `D10-committed-state-path-schema-alignment.md`)
/// and commits it via the injectable [`CommandRunner`] seam, so state
/// survives across resumed runs. A `git commit` that no-ops (e.g. "nothing to
/// commit", per [`super::is_noop_commit`]) is logged, not treated as a
/// failure — mirrors the Python behavior. A *genuine* `git commit` failure is
/// different: the node returns a [`NodeError`], because a task whose work was
/// never committed must not be recorded done.
///
/// This per-task save point never has `review`/`docs`/`pr` yet (those are
/// end-of-run outputs from `ConsolidatedReviewNode`/`PatchDocsNode`/
/// `PullRequestNode`, none of which have run at this point in the loop) and
/// is never itself a terminal write (`WrapUpNode` is the only node that
/// derives a [`super::schema::TerminalSignal`]) — so it always calls
/// `to_committed_state_json` with `None` for all four.
pub struct SaveStateNode {
    runner: CommandRunner,
    state_filename: &'static str,
}

impl SaveStateNode {
    #[must_use]
    pub fn new() -> Self {
        Self {
            runner: super::default_command_runner(),
            state_filename: super::DEFAULT_STATE_FILENAME,
        }
    }

    /// Override the command runner used for the `git add`/`git commit`
    /// invocations. Tests use this to stub the subprocess.
    #[must_use]
    pub fn with_runner(mut self, runner: CommandRunner) -> Self {
        self.runner = runner;
        self
    }

    /// Override the state filename this node writes to. Defaults to
    /// [`super::DEFAULT_STATE_FILENAME`]; `EN.11.M` task 4 adds this so a
    /// second engine can reuse the node under its own filename without
    /// forking it.
    #[must_use]
    pub fn with_state_filename(mut self, filename: &'static str) -> Self {
        self.state_filename = filename;
        self
    }
}

impl Default for SaveStateNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for SaveStateNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let worktree = worktree_path(&ctx)?;
        let state = latest_state(&ctx)?;

        let state_dir = Path::new(&worktree)
            .join("planning")
            .join(&state.spec_slug)
            .join("sdlc");
        std::fs::create_dir_all(&state_dir).map_err(|err| {
            NodeError::new(format!("failed to create {}: {err}", state_dir.display()))
        })?;
        let state_path = state_dir.join(self.state_filename);
        let run_meta = build_run_meta(&ctx, &worktree, &state_path);
        let committed = state.to_committed_state_json(&run_meta, None, None, None, None, None);
        let json = serde_json::to_string_pretty(&committed)
            .map_err(|err| NodeError::new(format!("failed to serialize SDLCState: {err}")))?;
        std::fs::write(&state_path, json).map_err(|err| {
            NodeError::new(format!("failed to write {}: {err}", state_path.display()))
        })?;

        let state_path_str = state_path.to_string_lossy().to_string();
        // Consulted, not merely stamped: a genuine `git commit` failure means
        // this task's work was never recorded, so the task must NOT be
        // reported done — the node fails, the same `NodeError` way every
        // other failure in this loop surfaces. The ordinary "nothing to
        // commit" no-op (`super::is_noop_commit`, which is where that
        // classification lives) stays benign and completes normally: in this
        // repo `planning/` is a gitignored symlink, so most state commits are
        // no-ops and failing them would break every run.
        let outcome = super::commit_all(
            &self.runner,
            Path::new(&worktree),
            &save_state_commit_message(&ctx),
        );
        if let Some(detail) = outcome.failure_detail() {
            return Err(NodeError::new(format!(
                "git commit failed while saving {state_path_str}; refusing to record the task as \
                 done with its work uncommitted: {detail}"
            )));
        }

        put_result(
            &mut ctx,
            "SaveStateNode",
            json!({ "saved_to": state_path_str, "committed": outcome.is_committed() }),
        );
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "SaveStateNode"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::sdlc_flow::policy::{ModelTiers, TransportRetry};
    use claude_code_rs::Outcome;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    fn empty_context(event: serde_json::Value) -> TaskContext {
        TaskContext {
            event,
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        }
    }

    fn state_with_tasks(tasks: Vec<SDLCTask>) -> SDLCState {
        let mut state = SDLCState::new("my-spec");
        state.tasks = tasks;
        state
    }

    fn ctx_with_state(state: &SDLCState) -> TaskContext {
        let mut ctx = empty_context(json!({ "spec_slug": state.spec_slug }));
        ctx.nodes.insert(
            "LoadTaskStateNode".to_string(),
            serde_json::to_value(state).unwrap(),
        );
        ctx
    }

    /// Builds a task-loop-ready `ctx` and stamps a default [`SdlcPolicy`]
    /// under [`RESOLVED_POLICY_IDENTITY`] — required since task 8's strict
    /// `resolved_policy_strict` read (no more silent `Default` fallback for
    /// an unstamped ctx). Tests wanting a non-default policy call
    /// `ctx_with_policy(ctx, &policy)` afterwards to overwrite this stamp.
    fn ctx_with_current_task(state: &SDLCState, task: &SDLCTask) -> TaskContext {
        let mut ctx = ctx_with_state(state);
        ctx.nodes.insert(
            "TaskQueueRouterNode".to_string(),
            json!({
                "current_task_id": task.task_id,
                "title": task.title,
                "description": task.description,
                "acceptance_criteria": task.acceptance_criteria,
                "attempt_count": task.attempt_count,
                "max_attempts": task.max_attempts,
            }),
        );
        ctx.nodes.insert(
            RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(SdlcPolicy::default()).expect("SdlcPolicy serializes"),
        );
        ctx
    }

    fn panicking_transport() -> ModelTransport {
        Arc::new(|_config, _prompt| {
            panic!("transport should not be invoked for a deterministic branch")
        })
    }

    // --- prior_attempt_feedback ---------------------------------------------

    /// A `TestTaskNode` result with one failing check carrying `output`.
    fn failed_test_result(output: &str) -> serde_json::Value {
        json!({
            "all_passed": false,
            "check_results": [
                {
                    "name": "test",
                    "kind": "test",
                    "passed": false,
                    "message": "cargo nextest run --workspace exited 101",
                    "output": output,
                },
                {
                    "name": "clippy",
                    "kind": "lint",
                    "passed": true,
                    "message": "",
                    "output": "clean",
                }
            ],
            "failure_summary": "Failed checks: test",
        })
    }

    fn ctx_with_node(identity: &str, value: serde_json::Value) -> TaskContext {
        let mut ctx = empty_context(json!({}));
        ctx.nodes.insert(identity.to_string(), value);
        ctx
    }

    #[test]
    fn prior_attempt_feedback_is_none_on_the_first_attempt() {
        // No `TestTaskNode` entry at all == nothing has run yet.
        let ctx = empty_context(json!({}));
        assert_eq!(
            prior_attempt_feedback(&ctx, &RetryFeedback::default()),
            None
        );
    }

    #[test]
    fn prior_attempt_feedback_is_none_when_the_previous_run_passed() {
        let ctx = ctx_with_node(
            "TestTaskNode",
            json!({
                "all_passed": true,
                "check_results": [
                    { "name": "test", "passed": true, "message": "", "output": "ok" }
                ],
                "failure_summary": "",
            }),
        );
        assert_eq!(
            prior_attempt_feedback(&ctx, &RetryFeedback::default()),
            None
        );
    }

    #[test]
    fn prior_attempt_feedback_is_none_when_disabled() {
        let ctx = ctx_with_node("TestTaskNode", failed_test_result("error[E0308]"));
        let cfg = RetryFeedback {
            enabled: false,
            max_chars: 4000,
        };
        assert_eq!(prior_attempt_feedback(&ctx, &cfg), None);
    }

    #[test]
    fn prior_attempt_feedback_renders_the_failed_check_name_and_output() {
        let ctx = ctx_with_node(
            "TestTaskNode",
            failed_test_result(
                "error[E0308]: mismatched types\n  --> src/http.rs:682:5\n  \
                 expected `impl Future`, found `Responder`",
            ),
        );
        let block = prior_attempt_feedback(&ctx, &RetryFeedback::default())
            .expect("a failed TestTaskNode result yields feedback");
        // Names the failing check...
        assert!(block.contains("test"), "block: {block}");
        // ...carries its message...
        assert!(block.contains("exited 101"), "block: {block}");
        // ...and — the whole point — the captured compiler output.
        assert!(block.contains("error[E0308]"), "block: {block}");
        assert!(block.contains("src/http.rs:682"), "block: {block}");
        // The passing check is not noise in the retry prompt.
        assert!(!block.contains("clippy"), "block: {block}");
    }

    /// Retry detection keys on the failed result, never on `attempt_count`
    /// (see [`prior_attempt_feedback`]'s docs). Retired and replaced the
    /// former `prior_attempt_feedback_ignores_a_stale_zero_attempt_count`,
    /// which pinned the same behavior but justified it by the router stamp
    /// being *stale* — a premise `ticket-restamp-attempt-count` made false.
    /// The behavior is unchanged and still worth pinning: a ctx carrying a
    /// failed `TestTaskNode` result must produce feedback regardless of what
    /// the counter reads.
    #[test]
    fn prior_attempt_feedback_keys_on_the_failed_result_not_the_attempt_count() {
        for attempt_count in [0, 1, 7] {
            let mut ctx = ctx_with_node("TestTaskNode", failed_test_result("error[E0308]"));
            ctx.nodes.insert(
                "TaskQueueRouterNode".to_string(),
                json!({ "current_task_id": 1, "attempt_count": attempt_count, "max_attempts": 3 }),
            );
            assert!(
                prior_attempt_feedback(&ctx, &RetryFeedback::default()).is_some(),
                "expected feedback at attempt_count={attempt_count}"
            );
        }
    }

    #[test]
    fn prior_attempt_feedback_is_bounded_by_max_chars_but_keeps_the_check_name() {
        let huge = "E".repeat(200_000);
        let ctx = ctx_with_node("TestTaskNode", failed_test_result(&huge));
        let cfg = RetryFeedback {
            enabled: true,
            max_chars: 900,
        };
        let block = prior_attempt_feedback(&ctx, &cfg).expect("feedback rendered");
        assert!(
            block.chars().count() <= 900,
            "block was {} chars, expected <= 900",
            block.chars().count()
        );
        // Truncation trims the output, never the identification.
        assert!(block.contains("FAILED CHECK: test"), "block: {block}");
        assert!(block.contains(RETRY_FEEDBACK_TRUNCATED), "block: {block}");
    }

    /// The bound holds across repeated retries: the block is rendered fresh
    /// from the latest `TestTaskNode` result each time, so its size is a
    /// function of `max_chars` alone, not of how many attempts have run.
    #[test]
    fn prior_attempt_feedback_size_does_not_grow_with_attempts() {
        let cfg = RetryFeedback {
            enabled: true,
            max_chars: 600,
        };
        let first = prior_attempt_feedback(
            &ctx_with_node("TestTaskNode", failed_test_result(&"A".repeat(50_000))),
            &cfg,
        )
        .unwrap();
        let second = prior_attempt_feedback(
            &ctx_with_node("TestTaskNode", failed_test_result(&"B".repeat(50_000))),
            &cfg,
        )
        .unwrap();
        assert_eq!(first.chars().count(), second.chars().count());
        assert!(first.chars().count() <= 600);
    }

    /// The `ReviewRouterNode` back-edge: checks passed, the reviewer did not.
    #[test]
    fn prior_attempt_feedback_falls_back_to_the_review_findings() {
        let mut ctx = ctx_with_node(
            "ConsolidatedReviewNode",
            json!({
                "verdict": "PARTIAL",
                "summary": "acceptance criterion 2 is unimplemented",
                "issues": ["missing test for the truncation bound"],
            }),
        );
        // A *passing* TestTaskNode must not suppress the review feedback.
        ctx.nodes.insert(
            "TestTaskNode".to_string(),
            json!({ "all_passed": true, "check_results": [], "failure_summary": "" }),
        );
        let block = prior_attempt_feedback(&ctx, &RetryFeedback::default())
            .expect("a non-PASS review yields feedback");
        assert!(block.contains("PARTIAL"), "block: {block}");
        assert!(
            block.contains("criterion 2 is unimplemented"),
            "block: {block}"
        );
        assert!(block.contains("truncation bound"), "block: {block}");
    }

    #[test]
    fn prior_attempt_feedback_is_none_for_a_passing_review() {
        let ctx = ctx_with_node(
            "ConsolidatedReviewNode",
            json!({ "verdict": "PASS", "summary": "looks good", "issues": [] }),
        );
        assert_eq!(
            prior_attempt_feedback(&ctx, &RetryFeedback::default()),
            None
        );
    }

    /// A failed `TestTaskNode` outranks the review findings — it is the
    /// immediate cause on the `TriageRouterNode` back-edge.
    #[test]
    fn prior_attempt_feedback_prefers_the_test_failure_over_the_review() {
        let mut ctx = ctx_with_node("TestTaskNode", failed_test_result("error[E0433]"));
        ctx.nodes.insert(
            "ConsolidatedReviewNode".to_string(),
            json!({ "verdict": "FAIL", "summary": "stale review", "issues": [] }),
        );
        let block = prior_attempt_feedback(&ctx, &RetryFeedback::default()).unwrap();
        assert!(block.contains("error[E0433]"), "block: {block}");
        assert!(!block.contains("stale review"), "block: {block}");
    }

    // --- apply_policy: per-stage call timeouts ------------------------------

    /// The behavior-stability guarantee: an unconfigured policy (the built-in
    /// default) leaves `Config.timeout` at `None` for **every** stage, so a
    /// run that sets nothing is byte-identical to pre-knob behavior and still
    /// falls through to `claude-code-rs`'s own 300s default.
    #[test]
    fn apply_policy_leaves_timeout_none_for_every_stage_by_default() {
        let policy = SdlcPolicy::default();
        for stage in [
            Stage::Implement,
            Stage::Triage,
            Stage::Review,
            Stage::Generate,
            Stage::Docs,
        ] {
            let (config, _) = apply_policy(Config::default(), "p".to_string(), &policy, stage);
            assert_eq!(
                config.timeout, None,
                "stage {stage:?} should set no timeout"
            );
        }
    }

    /// Adding `Stage::Generate`/`Stage::Docs` (and splitting the config half
    /// out of `apply_policy`) must not have moved the three pre-existing
    /// stages by a byte. Pins config AND prompt for a non-trivial policy.
    #[test]
    fn apply_policy_output_is_unchanged_for_the_three_preexisting_stages() {
        let policy = SdlcPolicy {
            model_tiers: ModelTiers {
                implement: ModelTier::Haiku,
                triage: ModelTier::Opus,
                review: ModelTier::Sonnet,
                ..ModelTiers::default()
            },
            timeouts: crate::workflows::sdlc_flow::policy::CallTimeouts {
                implement: Some(600),
                triage: Some(90),
                review: Some(120),
                // The two new stages are set, and must not leak into the
                // three old ones.
                generate: Some(1),
                docs: Some(2),
            },
            prompt_cache: true,
            output_verbosity: OutputVerbosity::Terse,
            ..SdlcPolicy::default()
        };

        for (stage, model, secs) in [
            (Stage::Implement, "claude-haiku-4-5", 600),
            (Stage::Triage, "claude-opus-4-8", 90),
            (Stage::Review, "claude-sonnet-4-5", 120),
        ] {
            let (config, prompt) = apply_policy(Config::default(), "p".to_string(), &policy, stage);
            assert_eq!(config.model.as_deref(), Some(model), "stage {stage:?}");
            assert_eq!(
                config.timeout,
                Some(std::time::Duration::from_secs(secs)),
                "stage {stage:?}"
            );
            assert_eq!(
                config.system_prompt.as_deref(),
                Some(STABLE_SYSTEM_PROMPT),
                "stage {stage:?}"
            );
            assert!(prompt.starts_with("p"), "stage {stage:?}");
            assert!(prompt.contains("Be terse"), "stage {stage:?}");
        }
    }

    /// The two new stages read their own `ModelTiers`/`CallTimeouts` fields.
    #[test]
    fn apply_policy_wires_generate_and_docs_to_their_own_policy_fields() {
        let policy = SdlcPolicy {
            model_tiers: ModelTiers {
                generate: ModelTier::Opus,
                docs: ModelTier::Haiku,
                ..ModelTiers::default()
            },
            timeouts: crate::workflows::sdlc_flow::policy::CallTimeouts {
                generate: Some(1200),
                docs: Some(45),
                ..Default::default()
            },
            ..SdlcPolicy::default()
        };

        let generate = apply_policy_config(Config::default(), &policy, Stage::Generate);
        assert_eq!(generate.model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(generate.timeout, Some(std::time::Duration::from_secs(1200)));

        let docs = apply_policy_config(Config::default(), &policy, Stage::Docs);
        assert_eq!(docs.model.as_deref(), Some("claude-haiku-4-5"));
        assert_eq!(docs.timeout, Some(std::time::Duration::from_secs(45)));
    }

    /// Behavior stability for the two newly-onboarded stages: under the
    /// built-in default they resolve to exactly the model strings the two
    /// nodes used to hardcode, and set no timeout.
    #[test]
    fn default_policy_reproduces_the_generate_and_docs_hardcoded_models() {
        let policy = SdlcPolicy::default();
        let generate = apply_policy_config(Config::default(), &policy, Stage::Generate);
        assert_eq!(generate.model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(generate.timeout, None);

        let docs = apply_policy_config(Config::default(), &policy, Stage::Docs);
        assert_eq!(docs.model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(docs.timeout, None);
    }

    #[test]
    fn apply_policy_sets_the_resolved_per_stage_timeout() {
        let policy = SdlcPolicy {
            timeouts: crate::workflows::sdlc_flow::policy::CallTimeouts {
                implement: Some(600),
                triage: Some(90),
                review: Some(120),
                ..Default::default()
            },
            ..SdlcPolicy::default()
        };

        let (implement, _) = apply_policy(
            Config::default(),
            "p".to_string(),
            &policy,
            Stage::Implement,
        );
        assert_eq!(implement.timeout, Some(std::time::Duration::from_secs(600)));

        let (triage, _) = apply_policy(Config::default(), "p".to_string(), &policy, Stage::Triage);
        assert_eq!(triage.timeout, Some(std::time::Duration::from_secs(90)));

        let (review, _) = apply_policy(Config::default(), "p".to_string(), &policy, Stage::Review);
        assert_eq!(review.timeout, Some(std::time::Duration::from_secs(120)));
    }

    /// A per-stage `None` inside an otherwise-configured `timeouts` block is
    /// still a no-op for that stage — the knob is opt-in stage by stage.
    #[test]
    fn apply_policy_timeout_is_per_stage_not_global() {
        let policy = SdlcPolicy {
            timeouts: crate::workflows::sdlc_flow::policy::CallTimeouts {
                implement: Some(600),
                ..Default::default()
            },
            ..SdlcPolicy::default()
        };
        let (triage, _) = apply_policy(Config::default(), "p".to_string(), &policy, Stage::Triage);
        assert_eq!(triage.timeout, None);
        let (review, _) = apply_policy(Config::default(), "p".to_string(), &policy, Stage::Review);
        assert_eq!(review.timeout, None);
    }

    /// End-to-end through the real four-layer resolver: an event-level
    /// `policy.timeouts.implement` override reaches the built `Config`.
    #[test]
    fn event_override_timeout_resolves_through_to_config() {
        use crate::workflows::sdlc_flow::policy::{resolve, PartialCallTimeouts, PartialPolicy};

        let event = PartialPolicy {
            timeouts: Some(PartialCallTimeouts {
                implement: Some(1800),
                ..Default::default()
            }),
            ..Default::default()
        };
        let policy = resolve(SdlcPolicy::default(), None, None, Some(&event));
        let (config, _) = apply_policy(
            Config::default(),
            "p".to_string(),
            &policy,
            Stage::Implement,
        );
        assert_eq!(config.timeout, Some(std::time::Duration::from_secs(1800)));
    }

    // --- TaskQueueRouterNode -----------------------------------------------

    #[tokio::test]
    async fn task_queue_dispatches_first_pending() {
        let mut task1 = SDLCTask::new(1, "One", "d1");
        task1.status = SDLCTaskStatus::Done;
        let task2 = SDLCTask::new(2, "Two", "d2");
        let state = state_with_tasks(vec![task1, task2]);
        let ctx = ctx_with_state(&state);

        let node = TaskQueueRouterNode;
        let out = node.process(ctx).await.expect("process should succeed");
        let result = out
            .nodes
            .get("TaskQueueRouterNode")
            .expect("output present");
        assert_eq!(result["current_task_id"], 2);

        assert_eq!(node.route(&out), Some("ImplementTaskNode".to_string()));
    }

    #[tokio::test]
    async fn task_queue_ends_on_none_pending() {
        let mut task1 = SDLCTask::new(1, "One", "d1");
        task1.status = SDLCTaskStatus::Done;
        let state = state_with_tasks(vec![task1]);
        let ctx = ctx_with_state(&state);

        let node = TaskQueueRouterNode;
        let out = node.process(ctx).await.expect("process should succeed");
        assert!(!out.nodes.contains_key("TaskQueueRouterNode"));
        assert_eq!(node.route(&out), Some("FinalValidationNode".to_string()));
    }

    // --- TriageTaskNode ------------------------------------------------------

    fn ctx_with_test_result(all_passed: bool, task: &SDLCTask) -> TaskContext {
        let state = state_with_tasks(vec![task.clone()]);
        let mut ctx = ctx_with_current_task(&state, task);
        ctx.nodes.insert(
            "TestTaskNode".to_string(),
            json!({ "all_passed": all_passed, "check_results": [], "failure_summary": "" }),
        );
        ctx
    }

    /// `EN.ticket.retry-one-exhausted-task-without-restarting-the-spec`
    /// (task 2): the attempts-exhausted bail reason must name BOTH supported
    /// recoveries — the new per-task `retry_task` and the pre-existing
    /// `resume: false` full restart — so an operator reading the failure
    /// learns how to recover without opening the source.
    #[tokio::test]
    async fn exhausted_attempts_bail_reason_names_the_supported_recoveries() {
        let mut task = SDLCTask::new(7, "Seven", "d7");
        task.max_attempts = 3;
        task.attempt_count = 3;

        let node = TriageTaskNode::new().with_transport(panicking_transport());
        let ctx = ctx_with_test_result(false, &task);
        let out = node.process(ctx).await.expect("process should succeed");

        assert_eq!(out.nodes["TriageTaskNode"]["verdict"], "MAJOR_BAIL");
        let reason = out.nodes["TriageTaskNode"]["reason"]
            .as_str()
            .expect("reason is a string");
        assert!(
            reason.contains("retry_task: 7"),
            "reason names the per-task retry with this task's id: {reason}"
        );
        assert!(
            reason.contains("resume: false"),
            "reason names the existing full-restart recovery: {reason}"
        );
    }

    #[tokio::test]
    async fn triage_deterministic_branches() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 3;
        task.attempt_count = 0;

        // Passing test -> PASS, no transport call.
        let node = TriageTaskNode::new().with_transport(panicking_transport());
        let ctx = ctx_with_test_result(true, &task);
        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["TriageTaskNode"]["verdict"], "PASS");

        // Over budget -> MAJOR_BAIL, no transport call.
        let mut over_budget = task.clone();
        over_budget.attempt_count = 3;
        let node = TriageTaskNode::new().with_transport(panicking_transport());
        let ctx = ctx_with_test_result(false, &over_budget);
        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["TriageTaskNode"]["verdict"], "MAJOR_BAIL");

        // Under budget + llm_triage=false (default) -> RETRYABLE, no
        // transport call.
        let node = TriageTaskNode::new().with_transport(panicking_transport());
        let ctx = ctx_with_test_result(false, &task);
        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["TriageTaskNode"]["verdict"], "RETRYABLE");
    }

    #[tokio::test]
    async fn triage_llm_gate_invokes_model_when_enabled() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 3;
        task.attempt_count = 0;

        let called = Arc::new(Mutex::new(false));
        let called_clone = called.clone();
        let transport: ModelTransport = Arc::new(move |_config, _prompt| {
            *called_clone.lock().unwrap() = true;
            let outcome = Outcome {
                cost_usd: 0.0,
                usage: claude_code_rs::parse::Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
                model_usage: std::collections::BTreeMap::new(),
                text: json!({ "verdict": "MAJOR_BAIL", "reason": "hopeless" }).to_string(),
                is_error: false,
                api_error_status: None,
                structured_output: None,
            };
            Box::pin(async move { Ok(outcome) })
        });

        let node = TriageTaskNode::new().with_transport(transport);
        let mut ctx = ctx_with_test_result(false, &task);
        ctx.event = json!({ "spec_slug": "my-spec", "llm_triage": true });

        let out = node.process(ctx).await.expect("process should succeed");
        assert!(*called.lock().unwrap());
        assert_eq!(out.nodes["TriageTaskNode"]["verdict"], "MAJOR_BAIL");
    }

    /// `EN.ticket.sdlc-flow-dead-policy-knobs` task 2: the resolved policy's
    /// `llm_triage` must reach `TriageTaskNode` even when the bare
    /// `event.llm_triage` field is absent — that is the whole point of
    /// wiring the policy layer instead of leaving it dead. Companion to
    /// [`triage_llm_gate_invokes_model_when_enabled`] above, which proves
    /// the bare event field alone (against a default `llm_triage: false`
    /// policy) still works — together they cover both spellings per the
    /// task's acceptance criteria.
    #[tokio::test]
    async fn triage_llm_gate_invokes_model_when_only_policy_sets_it() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 3;
        task.attempt_count = 0;

        let called = Arc::new(Mutex::new(false));
        let called_clone = called.clone();
        let transport: ModelTransport = Arc::new(move |_config, _prompt| {
            *called_clone.lock().unwrap() = true;
            let outcome = Outcome {
                cost_usd: 0.0,
                usage: claude_code_rs::parse::Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
                model_usage: std::collections::BTreeMap::new(),
                text: json!({ "verdict": "MAJOR_BAIL", "reason": "hopeless" }).to_string(),
                is_error: false,
                api_error_status: None,
                structured_output: None,
            };
            Box::pin(async move { Ok(outcome) })
        });

        let node = TriageTaskNode::new().with_transport(transport);
        let ctx = ctx_with_test_result(false, &task);
        // No `event.llm_triage` field at all — only the resolved policy
        // enables triage.
        let mut policy = SdlcPolicy::default();
        policy.llm_triage = true;
        let ctx = ctx_with_policy(ctx, &policy);

        let out = node.process(ctx).await.expect("process should succeed");
        assert!(*called.lock().unwrap());
        assert_eq!(out.nodes["TriageTaskNode"]["verdict"], "MAJOR_BAIL");
    }

    /// The bare `event.llm_triage` field is the top-precedence layer: an
    /// explicit `false` on the event must gate the model branch off even
    /// when the resolved policy has `llm_triage: true`.
    #[tokio::test]
    async fn triage_bare_event_field_false_overrides_policy_true() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 3;
        task.attempt_count = 0;

        let node = TriageTaskNode::new().with_transport(panicking_transport());
        let mut ctx = ctx_with_test_result(false, &task);
        let mut policy = SdlcPolicy::default();
        policy.llm_triage = true;
        ctx = ctx_with_policy(ctx, &policy);
        ctx.event = json!({ "spec_slug": "my-spec", "llm_triage": false });

        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["TriageTaskNode"]["verdict"], "RETRYABLE");
    }

    /// `EN.ticket.sdlc-flow-dead-policy-knobs` task 3: a non-default
    /// `transport_retry` on the resolved policy changes the observed
    /// attempt count against a persistently failing transport when
    /// `TriageTaskNode` takes the model path.
    #[tokio::test]
    async fn triage_transport_retry_nondefault_changes_observed_attempts() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 3;
        task.attempt_count = 0;

        let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
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

        let node = TriageTaskNode::new().with_transport(transport);
        let ctx = ctx_with_test_result(false, &task);
        let policy = SdlcPolicy {
            transport_retry: TransportRetry {
                max_attempts: 5,
                initial_backoff_ms: 0,
            },
            ..SdlcPolicy::default()
        };
        let mut ctx = ctx_with_policy(ctx, &policy);
        ctx.event = json!({ "spec_slug": "my-spec", "llm_triage": true });

        let result = node.process(ctx).await;
        assert!(
            result.is_err(),
            "persistent failure must still halt the walk"
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 5);
    }

    /// `EN.ticket.wire-meta-transport-telemetry` task 2: a `with_meta_transport`
    /// override on `TriageTaskNode` must stamp the *actual* transport tier
    /// onto `ctx.nodes["TriageTaskNode"]["transport"]["tier"]` — `"local"` on
    /// a stubbed local success, not a generic `"cloud"` regardless of what
    /// ran (the bug this ticket fixes).
    #[tokio::test]
    async fn triage_meta_transport_stamps_local_tier_on_stubbed_local_success() {
        use crate::nodes::openai_compat_meta_transport;
        use crate::workflows::sdlc_flow::policy::LocalConfig;

        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 3;
        task.attempt_count = 0;

        let local = LocalConfig {
            endpoint: "http://localhost:11434".to_string(),
            model: "qwen2.5-coder:7b".to_string(),
            constrained_json: false,
        };
        let local_http_post: crate::nodes::LocalHttpPost = Arc::new(|_url, _body| {
            Box::pin(async {
                Ok(json!({
                    "choices": [{ "message": {
                        "content": json!({ "verdict": "MAJOR_BAIL", "reason": "hopeless" }).to_string()
                    } }],
                    "usage": { "prompt_tokens": 1, "completion_tokens": 1 },
                }))
            })
        });
        let cloud_fallback: ModelTransport = Arc::new(|_config, _prompt| {
            Box::pin(async { panic!("cloud fallback must not be called when local succeeds") })
        });
        let meta_transport = openai_compat_meta_transport(local, local_http_post, cloud_fallback);

        let node = TriageTaskNode::new().with_meta_transport(meta_transport);
        let mut ctx = ctx_with_test_result(false, &task);
        ctx.event = json!({ "spec_slug": "my-spec", "llm_triage": true });

        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["TriageTaskNode"]["verdict"], "MAJOR_BAIL");
        assert_eq!(out.nodes["TriageTaskNode"]["transport"]["tier"], "local");
        assert_eq!(
            out.nodes["TriageTaskNode"]["transport"]["endpoint"],
            "http://localhost:11434"
        );
    }

    /// Same seam, but the local endpoint fails — the resulting telemetry
    /// must show `"cloud"` (what actually ran, via the fallback), not the
    /// `"local"` tier the resolved policy intended.
    #[tokio::test]
    async fn triage_meta_transport_stamps_cloud_tier_on_local_failure_fallback() {
        use crate::nodes::openai_compat_meta_transport;
        use crate::workflows::sdlc_flow::policy::LocalConfig;

        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 3;
        task.attempt_count = 0;

        let local = LocalConfig {
            endpoint: "http://localhost:11434".to_string(),
            model: "qwen2.5-coder:7b".to_string(),
            constrained_json: false,
        };
        let local_http_post: crate::nodes::LocalHttpPost =
            Arc::new(|_url, _body| Box::pin(async { Err("connection refused".to_string()) }));
        let cloud_fallback: ModelTransport = Arc::new(|_config, _prompt| {
            let outcome = canned_outcome(
                json!({ "verdict": "RETRYABLE", "reason": "cloud fallback ran" }).to_string(),
            );
            Box::pin(async move { Ok(outcome) })
        });
        let meta_transport = openai_compat_meta_transport(local, local_http_post, cloud_fallback);

        let node = TriageTaskNode::new().with_meta_transport(meta_transport);
        let mut ctx = ctx_with_test_result(false, &task);
        ctx.event = json!({ "spec_slug": "my-spec", "llm_triage": true });

        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["TriageTaskNode"]["verdict"], "RETRYABLE");
        assert_eq!(
            out.nodes["TriageTaskNode"]["transport"]["tier"], "cloud",
            "a down local endpoint must stamp the cloud fallback's actual tier, \
             not the resolved policy's intended `local` tier"
        );
        assert!(out.nodes["TriageTaskNode"]["transport"]["endpoint"].is_null());
    }

    /// A real model reply's casing isn't guaranteed to match the prompt's
    /// literal request — a lowercase (or mixed-case) verdict is normalized
    /// to uppercase so `TriageRouterNode`'s exact match still routes
    /// correctly instead of silently falling through to `None`.
    #[tokio::test]
    async fn triage_llm_branch_normalizes_lowercase_verdict() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 3;
        task.attempt_count = 0;

        let transport: ModelTransport = Arc::new(move |_config, _prompt| {
            let outcome = canned_outcome(
                json!({ "verdict": "retryable", "reason": "try again" }).to_string(),
            );
            Box::pin(async move { Ok(outcome) })
        });

        let node = TriageTaskNode::new().with_transport(transport);
        let mut ctx = ctx_with_test_result(false, &task);
        ctx.event = json!({ "spec_slug": "my-spec", "llm_triage": true });

        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["TriageTaskNode"]["verdict"], "RETRYABLE");
        // A recognized verdict must NOT carry the `unrecognized_verdict` key.
        assert!(out.nodes["TriageTaskNode"]
            .get("unrecognized_verdict")
            .is_none());
    }

    /// EN.3.G task 1: a garbage model verdict is stamped as
    /// `unrecognized_verdict` (alongside the byte-identical, unchanged
    /// `verdict` key the router still matches on) so `derive_terminal_signal`
    /// can surface it in the run's `bail_reason`.
    #[tokio::test]
    async fn triage_llm_branch_stamps_unrecognized_verdict() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 3;
        task.attempt_count = 0;

        let transport: ModelTransport = Arc::new(move |_config, _prompt| {
            let outcome =
                canned_outcome(json!({ "verdict": "WAT", "reason": "unclear" }).to_string());
            Box::pin(async move { Ok(outcome) })
        });

        let node = TriageTaskNode::new().with_transport(transport);
        let mut ctx = ctx_with_test_result(false, &task);
        ctx.event = json!({ "spec_slug": "my-spec", "llm_triage": true });

        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["TriageTaskNode"]["verdict"], "WAT");
        assert_eq!(out.nodes["TriageTaskNode"]["unrecognized_verdict"], "WAT");
    }

    // --- TriageTaskNode: failure-output feedback --------------------------

    /// The base triage prompt, exactly as built for a task titled `One` on
    /// attempt 1 of 3 with an empty `failure_summary`. Held as a literal so
    /// the enrichment tests below can assert against
    /// `format!("{TRIAGE_BASE_PROMPT}{block}")` shapes rather than restating
    /// it, and so the byte-identical pin has something to pin *to*.
    const TRIAGE_BASE_PROMPT: &str = "Classify this task's test failure as RETRYABLE or \
         MAJOR_BAIL. Respond with strict JSON of the shape {\"verdict\": str, \"reason\": \
         str}.\n\nTask: One\nAttempt 1 of 3.\nFailure summary: ";

    /// Records the prompt handed to the transport and answers with a valid
    /// [`TriageOutput`]. (`prompt_recording_transport` answers with an
    /// `ImplementOutput`, which `TriageTaskNode` cannot parse.)
    fn triage_prompt_recording_transport() -> (Arc<Mutex<Vec<String>>>, ModelTransport) {
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_clone = seen.clone();
        let transport: ModelTransport = Arc::new(move |_config, prompt| {
            seen_clone.lock().unwrap().push(prompt);
            let outcome = canned_outcome(
                json!({ "verdict": "RETRYABLE", "reason": "try again" }).to_string(),
            );
            Box::pin(async move { Ok(outcome) })
        });
        (seen, transport)
    }

    /// A triage ctx whose `TestTaskNode` result carries real failing-check
    /// detail, with the `llm_triage` gate open.
    fn triage_ctx_with_failure(task: &SDLCTask, output: &str) -> TaskContext {
        let state = state_with_tasks(vec![task.clone()]);
        let mut ctx = ctx_with_current_task(&state, task);
        ctx.nodes
            .insert("TestTaskNode".to_string(), failed_test_result(output));
        ctx.event = json!({ "spec_slug": "my-spec", "llm_triage": true });
        ctx
    }

    /// The headline behavior: the classifier sees the actual compiler output,
    /// not just the check names `failure_summary` lists.
    #[tokio::test]
    async fn triage_llm_prompt_carries_the_failed_check_output() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 3;
        task.attempt_count = 0;
        let ctx = triage_ctx_with_failure(
            &task,
            "error[E0308]: mismatched types\n  --> src/http.rs:682:5",
        );

        let (seen, transport) = triage_prompt_recording_transport();
        let node = TriageTaskNode::new().with_transport(transport);
        let out = node.process(ctx).await.expect("process should succeed");

        let prompts = seen.lock().unwrap().clone();
        assert_eq!(prompts.len(), 1);
        let prompt = &prompts[0];
        // The base prompt is intact and still leads...
        assert!(
            prompt.starts_with("Classify this task's test failure"),
            "prompt: {prompt}"
        );
        assert!(prompt.contains("Failure summary: Failed checks: test"));
        // ...under a classifier-facing header, NOT the implementer's.
        assert!(prompt.contains("CLASSIFY FROM THIS EVIDENCE"), "{prompt}");
        assert!(!prompt.contains("PREVIOUS ATTEMPT FAILED"), "{prompt}");
        // ...carrying name, message, and — the point of the ticket — output.
        assert!(prompt.contains("FAILED CHECK: test"), "{prompt}");
        assert!(prompt.contains("exited 101"), "{prompt}");
        assert!(prompt.contains("error[E0308]"), "{prompt}");
        assert!(prompt.contains("src/http.rs:682"), "{prompt}");
        // The passing check is not noise in the classifier's evidence.
        assert!(!prompt.contains("clippy"), "{prompt}");

        // The stamped result shape is untouched by the prompt enrichment.
        let result = &out.nodes["TriageTaskNode"];
        assert_eq!(result["verdict"], "RETRYABLE");
        assert_eq!(result["reason"], "try again");
        assert!(result.get("unrecognized_verdict").is_none());
    }

    /// The evidence block reuses `retry_feedback.max_chars` — no new knob —
    /// so an enormous compiler dump cannot blow the prompt up, and the bound
    /// never costs us the identification of *which* check failed.
    #[tokio::test]
    async fn triage_llm_prompt_failure_block_is_bounded_by_max_chars() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 3;
        task.attempt_count = 0;
        let huge = "E".repeat(200_000);
        let ctx = triage_ctx_with_failure(&task, &huge);
        let policy = SdlcPolicy {
            retry_feedback: RetryFeedback {
                enabled: true,
                max_chars: 900,
            },
            ..SdlcPolicy::default()
        };
        let ctx = ctx_with_policy(ctx, &policy);

        let (seen, transport) = triage_prompt_recording_transport();
        let node = TriageTaskNode::new().with_transport(transport);
        node.process(ctx).await.expect("process should succeed");

        let prompts = seen.lock().unwrap().clone();
        let prompt = &prompts[0];
        let base_len = TRIAGE_BASE_PROMPT.chars().count() + "Failed checks: test".chars().count();
        assert!(
            prompt.chars().count() <= base_len + 900,
            "prompt was {} chars, expected <= {}",
            prompt.chars().count(),
            base_len + 900
        );
        assert!(prompt.contains("FAILED CHECK: test"), "{prompt}");
        assert!(prompt.contains(RETRY_FEEDBACK_TRUNCATED), "{prompt}");
    }

    /// **Change-detector, asserted against the literal text.** With nothing
    /// failing in `check_results` there is no evidence to append, and the
    /// prompt must be byte-identical to pre-ticket behavior — this ticket
    /// buys an evidence block, not a rewritten request.
    ///
    /// Note this is the *reachable* no-evidence path. The spec named the
    /// `all_passed: true` path for this pin, but that branch returns a `PASS`
    /// verdict before any prompt exists (covered instead by
    /// `triage_deterministic_branches`' panicking transport). See the
    /// Amendment Log.
    #[tokio::test]
    async fn triage_llm_prompt_without_failed_checks_is_byte_identical() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 3;
        task.attempt_count = 0;
        let mut ctx = ctx_with_test_result(false, &task);
        ctx.event = json!({ "spec_slug": "my-spec", "llm_triage": true });

        let (seen, transport) = triage_prompt_recording_transport();
        let node = TriageTaskNode::new().with_transport(transport);
        node.process(ctx).await.expect("process should succeed");

        let prompts = seen.lock().unwrap().clone();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0], TRIAGE_BASE_PROMPT);
    }

    /// The shared knob's off switch covers triage too.
    #[tokio::test]
    async fn triage_llm_prompt_is_unchanged_when_the_knob_is_disabled() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 3;
        task.attempt_count = 0;
        let ctx = triage_ctx_with_failure(&task, "error[E0308]: mismatched types");
        let policy = SdlcPolicy {
            retry_feedback: RetryFeedback {
                enabled: false,
                max_chars: 4000,
            },
            ..SdlcPolicy::default()
        };
        let ctx = ctx_with_policy(ctx, &policy);

        let (seen, transport) = triage_prompt_recording_transport();
        let node = TriageTaskNode::new().with_transport(transport);
        node.process(ctx).await.expect("process should succeed");

        let prompts = seen.lock().unwrap().clone();
        assert_eq!(
            prompts[0],
            format!("{TRIAGE_BASE_PROMPT}Failed checks: test")
        );
    }

    /// Unlike [`prior_attempt_feedback`], the triage block has **no review
    /// fallback**: a stale `ConsolidatedReviewNode` verdict is not the failure
    /// under judgement and must not be presented as if it were.
    #[test]
    fn triage_failure_feedback_does_not_fall_back_to_review_findings() {
        let mut ctx = ctx_with_node(
            "TestTaskNode",
            json!({ "all_passed": true, "check_results": [], "failure_summary": "" }),
        );
        ctx.nodes.insert(
            "ConsolidatedReviewNode".to_string(),
            json!({ "verdict": "FAIL", "summary": "stale finding", "issues": ["nope"] }),
        );
        // `prior_attempt_feedback` *does* fall back here...
        assert!(prior_attempt_feedback(&ctx, &RetryFeedback::default()).is_some());
        // ...the triage block does not.
        assert_eq!(
            triage_failure_feedback(&ctx, &RetryFeedback::default()),
            None
        );
    }

    // --- TriageRouterNode ------------------------------------------------

    #[test]
    fn triage_router_back_edge() {
        let mut ctx = empty_context(json!({}));
        ctx.nodes.insert(
            "TriageTaskNode".to_string(),
            json!({ "verdict": "RETRYABLE", "reason": "retry" }),
        );
        let router = TriageRouterNode;
        assert_eq!(router.route(&ctx), Some("IncrementAttemptNode".to_string()));
    }

    #[test]
    fn triage_router_all_verdicts() {
        let router = TriageRouterNode;
        for (verdict, expected) in [
            ("PASS", "ConsolidatedReviewNode"),
            ("RETRYABLE", "IncrementAttemptNode"),
            ("MAJOR_BAIL", "WrapUpNode"),
        ] {
            let mut ctx = empty_context(json!({}));
            ctx.nodes.insert(
                "TriageTaskNode".to_string(),
                json!({ "verdict": verdict, "reason": "r" }),
            );
            // The `PASS` branch reads the resolved policy's `review_mode`
            // (task 8's `resolved_policy_strict` — no more silent `Default`
            // fallback), so this ctx must carry a stamp even though the
            // other two verdicts never touch it.
            ctx.nodes.insert(
                RESOLVED_POLICY_IDENTITY.to_string(),
                serde_json::to_value(SdlcPolicy::default()).expect("SdlcPolicy serializes"),
            );
            assert_eq!(router.route(&ctx), Some(expected.to_string()));
        }
    }

    /// EN.3.G task 1: an unrecognized verdict string must never silently
    /// halt the walk mid-graph — it routes to `WrapUpNode`, which is already
    /// a declared connection from this router (see `graph.rs`).
    #[test]
    fn triage_router_unrecognized_verdict_routes_to_wrap_up() {
        let mut ctx = empty_context(json!({}));
        ctx.nodes.insert(
            "TriageTaskNode".to_string(),
            json!({ "verdict": "WAT", "reason": "r", "unrecognized_verdict": "WAT" }),
        );
        let router = TriageRouterNode;
        assert_eq!(router.route(&ctx), Some("WrapUpNode".to_string()));
    }

    /// A ctx with no upstream `TriageTaskNode` result at all is a different
    /// condition from an unparseable verdict — the router must still return
    /// `None` (the walk has literally not reached this router yet), not
    /// `Some("WrapUpNode")`.
    #[test]
    fn triage_router_no_upstream_result_returns_none() {
        let ctx = empty_context(json!({}));
        let router = TriageRouterNode;
        assert_eq!(router.route(&ctx), None);
    }

    // --- IncrementAttemptNode / retry-bail (EN.3.B) -------------------------

    #[tokio::test]
    async fn increment_attempt_node_bumps_state() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task(&state, &task);

        let node = IncrementAttemptNode;
        let out = node.process(ctx).await.expect("process should succeed");
        let bumped: SDLCState =
            serde_json::from_value(out.nodes["IncrementAttemptNode"].clone()).unwrap();

        assert_eq!(bumped.tasks[0].attempt_count, 1);
        // The retry counter, and ONLY the retry counter: the attempt this
        // back-edge is about to send round is charged to `total_attempts`
        // by `ImplementTaskNode` when it is actually made, and no
        // `ImplementTaskNode` ran in this isolated drive.
        assert_eq!(bumped.telemetry.total_attempts, 0);
    }

    #[tokio::test]
    async fn increment_attempt_node_compounds_across_retries() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task(&state, &task);

        let node = IncrementAttemptNode;
        let out = node.process(ctx).await.expect("first retry should succeed");
        let out = node
            .process(out)
            .await
            .expect("second retry should succeed");

        // `latest_state` must pick up its own prior write (via the
        // `logical_clock`, to which this node contributes the task's
        // `attempt_count`), not fall back to the stale `LoadTaskStateNode`
        // snapshot, or this would still read 1.
        let bumped: SDLCState =
            serde_json::from_value(out.nodes["IncrementAttemptNode"].clone()).unwrap();
        assert_eq!(bumped.tasks[0].attempt_count, 2);
        assert_eq!(
            bumped.telemetry.total_attempts, 0,
            "attempts are charged at ImplementTaskNode, which never ran here"
        );
    }

    /// `ticket-restamp-attempt-count`: the router snapshot's `attempt_count`
    /// is refreshed on the retry back-edge, so
    /// `current_task_fields(&ctx)["attempt_count"]` stops reading `0` forever.
    /// Every other field of that entry must survive verbatim — the fix is a
    /// read-modify-write, not a rebuild.
    #[tokio::test]
    async fn increment_attempt_node_restamps_the_router_attempt_count() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.acceptance_criteria = vec!["ac-1".to_string(), "ac-2".to_string()];
        task.max_attempts = 5;
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task(&state, &task);

        let before = current_task_fields(&ctx).expect("router stamped").clone();
        assert_eq!(before["attempt_count"], json!(0), "precondition");

        let node = IncrementAttemptNode;
        let out = node.process(ctx).await.expect("process should succeed");

        let after = current_task_fields(&out).expect("router entry still present");
        assert_eq!(after["attempt_count"], json!(1));
        for key in [
            "current_task_id",
            "title",
            "description",
            "acceptance_criteria",
            "max_attempts",
        ] {
            assert_eq!(after[key], before[key], "{key} must survive the re-stamp");
        }
        assert_eq!(
            after.as_object().map(serde_json::Map::len),
            before.as_object().map(serde_json::Map::len),
            "the re-stamp must not add or drop keys"
        );

        // Compounds across back-edges exactly like the durable counter does.
        let out = node
            .process(out)
            .await
            .expect("second retry should succeed");
        assert_eq!(
            current_task_fields(&out).expect("router entry still present")["attempt_count"],
            json!(2)
        );
    }

    #[tokio::test]
    async fn retry_bail_fires_at_exactly_max_attempts_via_triage_back_edge() {
        // A never-passing task with max_attempts = 2: drive the loop by hand
        // (TriageTaskNode -> IncrementAttemptNode, repeated) and assert the
        // bail fires at exactly the 2nd retry attempt, never earlier, never
        // later — proving `IncrementAttemptNode` actually advances the
        // counter `TriageTaskNode`'s bail gate reads.
        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 2;
        let mut ctx = ctx_with_test_result(false, &task);

        let triage = TriageTaskNode::new().with_transport(panicking_transport());
        let increment = IncrementAttemptNode;

        // Attempt 0 (initial dispatch, attempt_count == 0 < max_attempts):
        // RETRYABLE.
        ctx = triage.process(ctx).await.expect("triage should succeed");
        assert_eq!(ctx.nodes["TriageTaskNode"]["verdict"], "RETRYABLE");
        ctx = increment
            .process(ctx)
            .await
            .expect("first increment should succeed");

        // Re-seed TestTaskNode's failing result for the retry (TriageTaskNode
        // reads it fresh every pass) and triage again: attempt_count == 1 <
        // max_attempts (2) -> still RETRYABLE, one retry left.
        ctx.nodes.insert(
            "TestTaskNode".to_string(),
            json!({ "all_passed": false, "check_results": [], "failure_summary": "" }),
        );
        ctx = triage.process(ctx).await.expect("triage should succeed");
        assert_eq!(ctx.nodes["TriageTaskNode"]["verdict"], "RETRYABLE");
        ctx = increment
            .process(ctx)
            .await
            .expect("second increment should succeed");

        // attempt_count is now 2 == max_attempts -> MAJOR_BAIL, exactly here,
        // not before.
        ctx.nodes.insert(
            "TestTaskNode".to_string(),
            json!({ "all_passed": false, "check_results": [], "failure_summary": "" }),
        );
        ctx = triage.process(ctx).await.expect("triage should succeed");
        assert_eq!(ctx.nodes["TriageTaskNode"]["verdict"], "MAJOR_BAIL");

        let router = TriageRouterNode;
        assert_eq!(router.route(&ctx), Some("WrapUpNode".to_string()));

        let final_state: SDLCState =
            serde_json::from_value(ctx.nodes["IncrementAttemptNode"].clone()).unwrap();
        assert_eq!(final_state.tasks[0].attempt_count, 2);
        // This drive walks triage and the back-edge only — no
        // `ImplementTaskNode`, so no attempt is charged. `total_attempts`
        // on a real bail is covered by
        // `triage_major_bail_counts_the_attempt_it_made`.
        assert_eq!(final_state.telemetry.total_attempts, 0);
    }

    #[tokio::test]
    async fn both_retry_back_edges_increment_attempt_count() {
        // TriageRouterNode's RETRYABLE and ReviewRouterNode's minor
        // FAIL/PARTIAL both route to IncrementAttemptNode; assert both
        // paths actually advance the counter (not just one of them).
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task(&state, &task);

        let node = IncrementAttemptNode;

        // Simulates the TriageRouterNode::RETRYABLE back-edge.
        let after_triage_retry = node.process(ctx).await.expect("process should succeed");
        let state_after_triage: SDLCState =
            serde_json::from_value(after_triage_retry.nodes["IncrementAttemptNode"].clone())
                .unwrap();
        assert_eq!(state_after_triage.tasks[0].attempt_count, 1);

        // Simulates the ReviewRouterNode minor FAIL/PARTIAL back-edge,
        // continuing from the same accumulated context.
        let after_review_retry = node
            .process(after_triage_retry)
            .await
            .expect("process should succeed");
        let state_after_review: SDLCState =
            serde_json::from_value(after_review_retry.nodes["IncrementAttemptNode"].clone())
                .unwrap();
        assert_eq!(state_after_review.tasks[0].attempt_count, 2);
        assert_eq!(
            state_after_review.telemetry.total_attempts, 0,
            "both back-edges charge the RETRY counter only; the attempts \
             themselves are charged at ImplementTaskNode"
        );
    }

    // --- attempt counting: `total_attempts` is charged where an attempt is
    // MADE (`ImplementTaskNode`), not at the outcome it happens to reach ---

    /// A transport that answers `ImplementTaskNode`'s prompt with a canned
    /// implement reply, so these drives never spawn a model.
    fn implement_transport() -> ModelTransport {
        Arc::new(|_config, _prompt| {
            let outcome = canned_outcome(
                json!({
                    "summary": "done",
                    "modified_files": ["src/lib.rs"],
                    "tests_added": [],
                })
                .to_string(),
            );
            Box::pin(async move { Ok(outcome) })
        })
    }

    /// Re-stamp `TaskQueueRouterNode`'s per-dispatch snapshot for `task`,
    /// exactly as the router does when it dequeues the next task. Lets one
    /// accumulated `ctx` carry a multi-task run.
    fn dispatch_task(ctx: &mut TaskContext, task: &SDLCTask) {
        ctx.nodes.insert(
            "TaskQueueRouterNode".to_string(),
            json!({
                "current_task_id": task.task_id,
                "title": task.title,
                "description": task.description,
                "acceptance_criteria": task.acceptance_criteria,
                "attempt_count": task.attempt_count,
                "max_attempts": task.max_attempts,
            }),
        );
    }

    /// One implement -> test -> triage attempt: runs the real
    /// `ImplementTaskNode` (stub transport), stamps the test outcome, and
    /// runs the real `TriageTaskNode` (deterministic, `llm_triage` off by
    /// default, so the panicking transport is never called).
    async fn drive_attempt(ctx: TaskContext, all_passed: bool) -> TaskContext {
        let mut ctx = ImplementTaskNode::new()
            .with_transport(implement_transport())
            .process(ctx)
            .await
            .expect("implement should succeed");
        ctx.nodes.insert(
            "TestTaskNode".to_string(),
            json!({ "all_passed": all_passed, "check_results": [], "failure_summary": "" }),
        );
        TriageTaskNode::new()
            .with_transport(panicking_transport())
            .process(ctx)
            .await
            .expect("triage should succeed")
    }

    /// Drive one task to a PASS after `retries` failed attempts: `retries`
    /// failing attempts each followed by the `IncrementAttemptNode` back
    /// edge, then a passing attempt closed out by `UpdateTaskStatusNode`.
    async fn drive_task_to_pass(
        mut ctx: TaskContext,
        task: &SDLCTask,
        retries: u32,
    ) -> TaskContext {
        dispatch_task(&mut ctx, task);
        for _ in 0..retries {
            ctx = drive_attempt(ctx, false).await;
            assert_eq!(ctx.nodes["TriageTaskNode"]["verdict"], "RETRYABLE");
            ctx = IncrementAttemptNode
                .process(ctx)
                .await
                .expect("increment should succeed");
        }
        ctx = drive_attempt(ctx, true).await;
        assert_eq!(ctx.nodes["TriageTaskNode"]["verdict"], "PASS");
        UpdateTaskStatusNode
            .process(ctx)
            .await
            .expect("update status should succeed")
    }

    /// LOAD-BEARING (the R4 defect): a run that terminates through a TRIAGE
    /// `MAJOR_BAIL` must still report the attempt it actually made.
    ///
    /// `TriageRouterNode` routes `MAJOR_BAIL` straight to `WrapUpNode`,
    /// bypassing `UpdateTaskStatusNode` entirely — so an outcome-charged
    /// counter has NO site on this path and R4 reported `total_attempts: 0`
    /// after genuinely running one implement -> test attempt. Charging the
    /// attempt at `ImplementTaskNode` — the one node every attempt passes
    /// through, whatever it goes on to conclude — is what closes it.
    #[tokio::test]
    async fn triage_major_bail_counts_the_attempt_it_made() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let mut ctx = ctx_with_current_task(&state, &task);
        ctx.event = json!({ "spec_slug": "my-spec", "llm_triage": true });

        // One attempt, made in full: implement runs, the checks fail, and
        // triage's model call returns MAJOR_BAIL on the FIRST attempt (not
        // attempt exhaustion — the retry back-edge never fires, so no
        // `IncrementAttemptNode` write happens either).
        let mut ctx = ImplementTaskNode::new()
            .with_transport(implement_transport())
            .process(ctx)
            .await
            .expect("implement should succeed");
        ctx.nodes.insert(
            "TestTaskNode".to_string(),
            json!({ "all_passed": false, "check_results": [], "failure_summary": "boom" }),
        );
        let bail_transport: ModelTransport = Arc::new(|_config, _prompt| {
            let outcome = canned_outcome(
                json!({ "verdict": "MAJOR_BAIL", "reason": "hopeless" }).to_string(),
            );
            Box::pin(async move { Ok(outcome) })
        });
        let ctx = TriageTaskNode::new()
            .with_transport(bail_transport)
            .process(ctx)
            .await
            .expect("triage should succeed");

        assert_eq!(ctx.nodes["TriageTaskNode"]["verdict"], "MAJOR_BAIL");
        assert_eq!(
            TriageRouterNode.route(&ctx),
            Some("WrapUpNode".to_string()),
            "precondition: the bail path bypasses UpdateTaskStatusNode"
        );
        assert!(
            !ctx.nodes.contains_key("UpdateTaskStatusNode"),
            "precondition: no outcome-charged site runs on this path"
        );
        assert!(
            !ctx.nodes.contains_key("IncrementAttemptNode"),
            "precondition: no retry back-edge fires on this path"
        );

        let state = latest_state(&ctx).expect("a state must be readable at the bail");
        assert!(
            state.telemetry.total_attempts >= 1,
            "a run that made one attempt and bailed must not report \
             total_attempts: {} (the R4 defect)",
            state.telemetry.total_attempts
        );
        assert_eq!(state.telemetry.total_attempts, 1);
        assert_eq!(
            state.tasks[0].attempt_count, 0,
            "attempt_count counts RETRIES: a first-attempt bail has none"
        );
    }

    /// REGRESSION (run R5's shape): two tasks, the first passing first try,
    /// the second taking one retry. Four implement -> test attempts is not
    /// what happened; three is. Must hold identically before and after the
    /// counting site moved.
    #[tokio::test]
    async fn r5_shape_two_tasks_one_retry_totals_three_attempts() {
        let task1 = SDLCTask::new(1, "One", "d1");
        let task2 = SDLCTask::new(2, "Two", "d2");
        let state = state_with_tasks(vec![task1.clone(), task2.clone()]);
        let ctx = ctx_with_state(&state);
        let mut ctx = ctx;
        ctx.nodes.insert(
            RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(SdlcPolicy::default()).expect("SdlcPolicy serializes"),
        );

        let ctx = drive_task_to_pass(ctx, &task1, 0).await;
        let ctx = drive_task_to_pass(ctx, &task2, 1).await;

        let state = latest_state(&ctx).expect("state readable");
        assert_eq!(state.telemetry.total_attempts, 3);
        assert_eq!(
            state
                .tasks
                .iter()
                .map(|task| task.attempt_count)
                .collect::<Vec<_>>(),
            vec![0, 1],
            "attempt_count still counts RETRIES, unchanged"
        );
        assert_eq!(state.telemetry.tasks_passed, 2);
    }

    /// REGRESSION (run R6's shape): three tasks taking 1, 2 and 3 retries,
    /// all of which eventually pass — 2 + 3 + 4 = 9 implement -> test
    /// attempts. Must hold identically before and after the counting site
    /// moved. (The live R6 reported 8 for a run whose third task BAILED:
    /// that run's uncounted final attempt is exactly the R4 defect above.)
    #[tokio::test]
    async fn r6_shape_three_tasks_retrying_totals_nine_attempts() {
        let task1 = SDLCTask::new(1, "One", "d1");
        let task2 = SDLCTask::new(2, "Two", "d2");
        let task3 = SDLCTask::new(3, "Three", "d3");
        let state = state_with_tasks(vec![task1.clone(), task2.clone(), task3.clone()]);
        let mut ctx = ctx_with_state(&state);
        ctx.nodes.insert(
            RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(SdlcPolicy::default()).expect("SdlcPolicy serializes"),
        );

        let ctx = drive_task_to_pass(ctx, &task1, 1).await;
        let ctx = drive_task_to_pass(ctx, &task2, 2).await;
        let ctx = drive_task_to_pass(ctx, &task3, 3).await;

        let state = latest_state(&ctx).expect("state readable");
        assert_eq!(state.telemetry.total_attempts, 9);
        assert_eq!(
            state
                .tasks
                .iter()
                .map(|task| task.attempt_count)
                .collect::<Vec<_>>(),
            vec![1, 2, 3],
            "attempt_count still counts RETRIES, unchanged"
        );
        assert_eq!(state.telemetry.tasks_passed, 3);
    }
    // --- ReviewRouterNode --------------------------------------------------

    fn review_ctx(verdict: &str, issue_count: usize) -> TaskContext {
        let issues: Vec<String> = (0..issue_count).map(|i| format!("issue {i}")).collect();
        let mut ctx = empty_context(json!({}));
        ctx.nodes.insert(
            "ConsolidatedReviewNode".to_string(),
            json!({ "verdict": verdict, "summary": "s", "issues": issues }),
        );
        ctx
    }

    #[test]
    fn review_router_structural_vs_minor() {
        let router = ReviewRouterNode;

        assert_eq!(
            router.route(&review_ctx("FAIL", 3)),
            Some("IncrementAttemptNode".to_string())
        );
        assert_eq!(
            router.route(&review_ctx("FAIL", 6)),
            Some("WrapUpNode".to_string())
        );
        assert_eq!(
            router.route(&review_ctx("FAIL", 0)),
            Some("WrapUpNode".to_string())
        );
        assert_eq!(
            router.route(&review_ctx("PASS", 0)),
            Some("UpdateTaskStatusNode".to_string())
        );
        assert_eq!(
            router.route(&review_ctx("PARTIAL", 2)),
            Some("IncrementAttemptNode".to_string())
        );
    }

    /// EN.3.G task 1: an unrecognized review verdict must never silently
    /// halt the walk mid-graph — it routes to `WrapUpNode`, which is already
    /// a declared connection from this router (see `graph.rs`).
    #[test]
    fn review_router_unrecognized_verdict_routes_to_wrap_up() {
        let router = ReviewRouterNode;
        let mut ctx = empty_context(json!({}));
        ctx.nodes.insert(
            "ConsolidatedReviewNode".to_string(),
            json!({ "verdict": "WAT", "summary": "s", "issues": [], "unrecognized_verdict": "WAT" }),
        );
        assert_eq!(router.route(&ctx), Some("WrapUpNode".to_string()));
    }

    /// A ctx with no upstream `ConsolidatedReviewNode` result at all is a
    /// different condition from an unparseable verdict — the router must
    /// still return `None`.
    #[test]
    fn review_router_no_upstream_result_returns_none() {
        let ctx = empty_context(json!({}));
        let router = ReviewRouterNode;
        assert_eq!(router.route(&ctx), None);
    }

    /// `review_ctx` plus the CURRENT task's stamped `review_attempt_count`
    /// and a `max_review_attempts` policy — the shape
    /// `ReviewRouterNode::route` reads via `bounded_review_attempts`/
    /// `resolved_policy` (EN.ticket.review-retry-loop-unbounded task 3, made
    /// per-task by the R6 fix).
    fn review_ctx_with_bound(
        verdict: &str,
        issue_count: usize,
        review_attempts: u32,
        max_review_attempts: u32,
    ) -> TaskContext {
        let mut ctx = review_ctx(verdict, issue_count);
        let mut task = SDLCTask::new(1, "One", "d1");
        task.review_attempt_count = review_attempts;
        let mut state = SDLCState::new("my-spec");
        state.tasks = vec![task];
        ctx.nodes.insert(
            "LoadTaskStateNode".to_string(),
            serde_json::to_value(&state).unwrap(),
        );
        ctx.nodes.insert(
            "TaskQueueRouterNode".to_string(),
            json!({ "current_task_id": 1 }),
        );
        ctx.nodes
            .get_mut("ConsolidatedReviewNode")
            .expect("review_ctx stamps a ConsolidatedReviewNode result")
            .as_object_mut()
            .expect("review result is an object")
            .insert("task_review_attempts".to_string(), json!(review_attempts));
        let policy = SdlcPolicy {
            max_review_attempts,
            ..SdlcPolicy::default()
        };
        ctx.nodes.insert(
            RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(policy).unwrap(),
        );
        ctx
    }

    /// EN.ticket.review-retry-loop-unbounded task 3: below the bound, a
    /// minor-issue verdict still takes the retry back-edge — unchanged
    /// behavior for every run that terminates within budget.
    #[test]
    fn review_router_minor_issue_under_bound_still_retries() {
        let router = ReviewRouterNode;
        let ctx = review_ctx_with_bound("FAIL", 2, 2, 3);
        assert_eq!(router.route(&ctx), Some("IncrementAttemptNode".to_string()));
    }

    /// At the bound, the minor-issue back-edge routes to `WrapUpNode`
    /// instead of looping again — this is the fix: the cycle
    /// Implement -> Test(pass) -> Triage(PASS) -> Review(FAIL/PARTIAL,
    /// minor) -> IncrementAttempt -> Implement now has an exit.
    #[test]
    fn review_router_minor_issue_at_bound_routes_to_wrap_up() {
        let router = ReviewRouterNode;
        let ctx = review_ctx_with_bound("PARTIAL", 2, 3, 3);
        assert_eq!(router.route(&ctx), Some("WrapUpNode".to_string()));
    }

    /// Past the bound (e.g. a stale counter read after the exact tick) still
    /// routes to `WrapUpNode` — the gate is `>=`, not `==`.
    #[test]
    fn review_router_minor_issue_past_bound_routes_to_wrap_up() {
        let router = ReviewRouterNode;
        let ctx = review_ctx_with_bound("FAIL", 1, 4, 3);
        assert_eq!(router.route(&ctx), Some("WrapUpNode".to_string()));
    }

    /// `PASS` and the structural (0 or >threshold issues) arms are
    /// unaffected by the review-attempts bound — they already route to
    /// `WrapUpNode`/`UpdateTaskStatusNode` regardless of attempt count.
    #[test]
    fn review_router_pass_and_structural_arms_unaffected_by_bound() {
        let router = ReviewRouterNode;
        assert_eq!(
            router.route(&review_ctx_with_bound("PASS", 0, 3, 3)),
            Some("UpdateTaskStatusNode".to_string())
        );
        assert_eq!(
            router.route(&review_ctx_with_bound("FAIL", 6, 3, 3)),
            Some("WrapUpNode".to_string())
        );
        assert_eq!(
            router.route(&review_ctx_with_bound("FAIL", 0, 0, 3)),
            Some("WrapUpNode".to_string())
        );
    }

    // --- UpdateTaskStatusNode ------------------------------------------------

    fn ctx_for_update(task: &SDLCTask, verdict: &str) -> TaskContext {
        let state = state_with_tasks(vec![task.clone()]);
        let mut ctx = ctx_with_current_task(&state, task);
        ctx.nodes.insert(
            "TriageTaskNode".to_string(),
            json!({ "verdict": verdict, "reason": "r" }),
        );
        ctx
    }

    #[tokio::test]
    async fn update_status_marks_the_task_done_and_counts_the_pass() {
        let task = SDLCTask::new(1, "One", "d1");

        let node = UpdateTaskStatusNode;
        let ctx = ctx_for_update(&task, "PASS");
        let out = node.process(ctx).await.expect("process should succeed");
        let state: SDLCState =
            serde_json::from_value(out.nodes["UpdateTaskStatusNode"].clone()).unwrap();
        assert_eq!(state.tasks[0].status, SDLCTaskStatus::Done);
        assert_eq!(state.telemetry.tasks_passed, 1);
        // `tasks_passed` is the ONE counter this write advances (see
        // `logical_clock`). The attempt that produced the PASS was already
        // charged where it was made, in `ImplementTaskNode` — which did not
        // run in this isolated drive, hence 0 rather than a re-charge here.
        assert_eq!(state.telemetry.total_attempts, 0);
    }

    /// Neither `RETRYABLE` nor `MAJOR_BAIL` can reach this node in either
    /// workflow (see its doc comment for the route-by-route argument), so
    /// both fail closed instead of silently mutating status or counters.
    /// The deleted arms were the outcome-charged `total_attempts` sites; a
    /// counter must not keep a site on a path that cannot execute.
    #[tokio::test]
    async fn update_status_rejects_every_non_pass_verdict() {
        for verdict in ["RETRYABLE", "MAJOR_BAIL", "NONSENSE"] {
            let task = SDLCTask::new(1, "One", "d1");
            let ctx = ctx_for_update(&task, verdict);
            let err = UpdateTaskStatusNode
                .process(ctx)
                .await
                .expect_err("a non-PASS verdict must fail closed");
            assert!(
                err.message.contains("non-PASS triage verdict"),
                "unexpected error for {verdict}: {}",
                err.message
            );
        }
    }

    #[tokio::test]
    async fn update_status_missing_task_errors() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task]);
        let mut ctx = ctx_with_state(&state);
        ctx.nodes.insert(
            "TaskQueueRouterNode".to_string(),
            json!({ "current_task_id": 99 }),
        );
        ctx.nodes.insert(
            "TriageTaskNode".to_string(),
            json!({ "verdict": "PASS", "reason": "r" }),
        );

        let node = UpdateTaskStatusNode;
        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("no task with task_id=99"));
    }

    // --- TestTaskNode --------------------------------------------------------

    /// `TestTaskNode`'s write-verification guard probes the worktree with a
    /// direct `git status --porcelain` on EVERY run — it is no longer
    /// short-circuitable by an empty `modified_files` claim — so every
    /// stubbed [`CommandRunner`] in these tests now sees that call in
    /// addition to the check commands it exists to observe.
    ///
    /// Runners whose job is to record or score CHECK invocations answer the
    /// probe through this helper and neither record nor count it: the
    /// porcelain line below models the ordinary case these tests are
    /// about — a task that did write — so the guard passes and the test
    /// keeps asserting on what it was written to assert on. A test that
    /// specifically wants a CLEAN worktree stubs `git status` itself (see
    /// [`porcelain_runner`]).
    fn write_verification_probe(program: &str, args: &[&str]) -> Option<CommandOutput> {
        (program == "git" && args.first() == Some(&"status")).then(|| CommandOutput {
            status: 0,
            stdout: " M src/lib.rs\n".to_string(),
            stderr: String::new(),
        })
    }

    fn temp_worktree() -> std::path::PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "engine-core-sdlc-flow-task-loop-test-{}-{n}",
            std::process::id()
        ));
        // Guarantee-empty: see `setup.rs`'s `temp_dir_named` doc comment for
        // why PID-recycling makes this removal necessary, not optional.
        // Remove the ROOT dir before recreating the `planning` subdir.
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("planning")).unwrap();
        // Make the fixture an actual git worktree that HAS changes in it.
        // `TestTaskNode` always runs after an implement step, and its
        // write-verification guard now asks `git status --porcelain`
        // unconditionally (it can no longer be short-circuited by an empty
        // `modified_files` claim), so a bare directory — where `git status`
        // fails and reports nothing — would read as "the implement work
        // never reached this tree" and fail every check-dispatch test for a
        // reason none of them are about. `git init` alone is enough: the
        // seed file below is then untracked, so porcelain reports
        // `?? planning/`. Tests that specifically want a CLEAN worktree
        // stub `git status` through the injected `CommandRunner` instead.
        //
        // The seed FILE is not optional: git does not report an empty
        // directory, so `git init` on a tree whose only content is the
        // empty `planning/` dir still yields empty porcelain output.
        std::fs::write(dir.join("planning").join(".worktree-seed"), "").unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&dir)
            .output()
            .expect("git init should succeed for the test worktree fixture");
        dir
    }

    fn write_harness(dir: &Path, checks: serde_json::Value) {
        let harness = json!({ "validation": { "checks": checks } });
        std::fs::write(
            dir.join("planning").join("harness.json"),
            serde_json::to_string(&harness).unwrap(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn test_task_no_harness_json_passes() {
        // Renamed intent, same coverage: task 4's harness-missing fix means
        // a worktree with no `planning/harness.json` and a task with no
        // `validation_commands` is now a GATING failure, not a silent pass
        // (see the "harness-missing fix" note in tasks.md). This is the
        // exact behavior the spec's acceptance criteria require.
        let worktree = temp_worktree();
        let ctx = ctx_for_worktree(&worktree);

        let node = TestTaskNode::new();
        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], false);
        let results = out.nodes["TestTaskNode"]["check_results"]
            .as_array()
            .unwrap();
        assert_eq!(results[0]["kind"], "harness-missing");
    }

    #[tokio::test]
    async fn test_task_reports_gating_failure() {
        let worktree = temp_worktree();
        write_harness(
            &worktree,
            json!([{ "name": "always_fail", "command": "exit 1", "gates": true }]),
        );

        let ctx = ctx_for_worktree(&worktree);

        let node = TestTaskNode::new();
        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], false);
    }

    #[tokio::test]
    async fn test_task_uses_injected_runner_for_fail_then_pass() {
        let worktree = temp_worktree();
        write_harness(
            &worktree,
            json!([{ "name": "check", "command": "does-not-matter", "gates": true }]),
        );

        let attempt: Arc<std::sync::atomic::AtomicU64> =
            Arc::new(std::sync::atomic::AtomicU64::new(0));
        let attempt_clone = attempt.clone();
        let runner: CommandRunner = Arc::new(move |program, args, _cwd| {
            // The guard's probe must not consume an attempt — this test
            // scores CHECK invocations, not worktree inspections.
            if let Some(probe) = write_verification_probe(program, args) {
                return Ok(probe);
            }
            let n = attempt_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(CommandOutput {
                status: if n == 0 { 1 } else { 0 },
                stdout: String::new(),
                stderr: String::new(),
            })
        });

        let ctx = ctx_for_worktree(&worktree);

        let node = TestTaskNode::new().with_runner(runner.clone());
        let out = node.process(ctx.clone()).await.unwrap();
        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], false);

        let node = TestTaskNode::new().with_runner(runner);
        let out = node.process(ctx).await.unwrap();
        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
    }

    /// Builds a `ctx` with a `SetupWorktreeNode` output plus everything
    /// `TestTaskNode` now needs to reach it (task 4 makes `TestTaskNode`
    /// policy-strict and reads the CURRENT task's `validation_commands` out
    /// of the live durable state): a single default task (no
    /// `validation_commands`), a matching `TaskQueueRouterNode`/
    /// `LoadTaskStateNode` pair, and the built-in `SdlcPolicy` default
    /// (behavior-stable per rule 6, so stamping it changes nothing these
    /// tests assert).
    fn ctx_for_worktree(worktree: &Path) -> TaskContext {
        let task = SDLCTask::new(1, "t", "d");
        let state = state_with_tasks(vec![task.clone()]);
        let mut ctx = ctx_with_current_task(&state, &task);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );
        ctx
    }

    fn ctx_with_implement_claim(worktree: &Path, modified_files: &[&str]) -> TaskContext {
        let mut ctx = ctx_for_worktree(worktree);
        ctx.nodes.insert(
            "ImplementTaskNode".to_string(),
            json!({
                "summary": "did the thing",
                "modified_files": modified_files,
                "tests_added": [],
            }),
        );
        ctx
    }

    fn porcelain_runner(status_lines: &'static str) -> CommandRunner {
        Arc::new(move |program, args, _cwd| {
            if program == "git" && args.first() == Some(&"status") {
                Ok(CommandOutput {
                    status: 0,
                    stdout: status_lines.to_string(),
                    stderr: String::new(),
                })
            } else {
                Ok(CommandOutput {
                    status: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
        })
    }

    /// A non-empty `modified_files` claim with none of the claimed paths
    /// showing up in `git status --porcelain` fails the write-verification
    /// guard, even when no `harness.json` exists, and the failure routes
    /// through the normal `all_passed`/`check_results` shape (i.e. through
    /// the same triage/retry path a harness failure would).
    #[tokio::test]
    async fn write_verification_fails_when_no_claimed_file_changed() {
        let worktree = temp_worktree();
        let ctx = ctx_with_implement_claim(&worktree, &["src/lib.rs"]);

        let node = TestTaskNode::new().with_runner(porcelain_runner(""));
        let out = node.process(ctx).await.expect("process should succeed");

        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], false);
        let results = out.nodes["TestTaskNode"]["check_results"]
            .as_array()
            .unwrap();
        assert_eq!(results[0]["kind"], "write-verification");
        assert_eq!(results[0]["passed"], false);
        assert!(out.nodes["TestTaskNode"]["failure_summary"]
            .as_str()
            .unwrap()
            .contains("write-verification"));
    }

    /// LOAD-BEARING: an EMPTY `modified_files` claim on a task that is
    /// expected to write, with a completely clean worktree, must FAIL the
    /// guard. The model's self-report is documented-unreliable, so an empty
    /// claim carries no information; the worktree does. Measured twice on
    /// real runs: an implement attempt that landed in the WRONG TREE
    /// reported `modified_files: []`, and every harness check then ran
    /// green against an untouched checkout.
    #[tokio::test]
    async fn write_verification_fires_on_empty_claim_and_clean_worktree() {
        let worktree = temp_worktree();
        write_harness(&worktree, json!([]));
        let ctx = ctx_with_implement_claim(&worktree, &[]);

        let node = TestTaskNode::new().with_runner(porcelain_runner(""));
        let out = node.process(ctx).await.expect("process should succeed");

        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], false);
        let results = out.nodes["TestTaskNode"]["check_results"]
            .as_array()
            .unwrap();
        assert_eq!(results[0]["kind"], "write-verification");
        assert_eq!(results[0]["passed"], false);
    }

    /// A claimed file that DOES show up in `git status --porcelain` passes
    /// the guard, and (with no `harness.json`) the task overall passes.
    #[tokio::test]
    async fn write_verification_passes_when_claimed_file_changed() {
        let worktree = temp_worktree();
        // An empty (but present) harness keeps this test isolated to the
        // write-verification guard: task 4's harness-missing fix only gates
        // when `planning/harness.json` is absent entirely.
        write_harness(&worktree, json!([]));
        let ctx = ctx_with_implement_claim(&worktree, &["src/lib.rs"]);

        let node = TestTaskNode::new().with_runner(porcelain_runner(" M src/lib.rs\n"));
        let out = node.process(ctx).await.expect("process should succeed");

        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
        let results = out.nodes["TestTaskNode"]["check_results"]
            .as_array()
            .unwrap();
        assert!(results.is_empty());
    }

    /// An EXPLICITLY no-op task (`expects_writes: false` — investigation
    /// only) passes on a completely clean worktree. This is the one and
    /// only way out of the guard, and it must be DECLARED: consent to a
    /// silent no-op is never inferred from an empty claim.
    #[tokio::test]
    async fn write_verification_does_not_trip_on_explicitly_no_op_task() {
        let worktree = temp_worktree();
        write_harness(&worktree, json!([]));
        let mut task = SDLCTask::new(1, "investigate", "read only");
        task.expects_writes = false;
        let state = state_with_tasks(vec![task.clone()]);
        let mut ctx = ctx_with_current_task_and_worktree(&state, &task, &worktree);
        ctx.nodes.insert(
            "ImplementTaskNode".to_string(),
            json!({ "summary": "looked around", "modified_files": [], "tests_added": [] }),
        );

        let node = TestTaskNode::new().with_runner(porcelain_runner(""));
        let out = node.process(ctx).await.expect("process should succeed");

        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
        let results = out.nodes["TestTaskNode"]["check_results"]
            .as_array()
            .unwrap();
        assert!(results.is_empty());
    }

    /// No `ImplementTaskNode` output at all, on a clean worktree, FIRES the
    /// guard for a task expected to write. Previously this was read as
    /// "behaves like an empty claim — never trips", which is the same
    /// mistake the empty claim was: the absence of a self-report is not
    /// evidence that nothing needed to happen.
    #[tokio::test]
    async fn write_verification_fires_when_implement_never_ran() {
        let worktree = temp_worktree();
        write_harness(&worktree, json!([]));
        let ctx = ctx_for_worktree(&worktree);

        let node = TestTaskNode::new().with_runner(porcelain_runner(""));
        let out = node.process(ctx).await.expect("process should succeed");

        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], false);
        let results = out.nodes["TestTaskNode"]["check_results"]
            .as_array()
            .unwrap();
        assert_eq!(results[0]["kind"], "write-verification");
        assert!(results[0]["message"]
            .as_str()
            .unwrap()
            .contains("claimed no modified_files"));
    }

    /// ANY worktree change passes the guard regardless of the claim — here
    /// with an EMPTY claim, the case a live `claude` call was observed
    /// producing on a genuinely successful write. The claim is never
    /// compared against the changed paths.
    #[tokio::test]
    async fn write_verification_passes_on_empty_claim_when_worktree_changed() {
        let worktree = temp_worktree();
        write_harness(&worktree, json!([]));
        let ctx = ctx_with_implement_claim(&worktree, &[]);

        let node = TestTaskNode::new().with_runner(porcelain_runner("?? src/new.rs\n"));
        let out = node.process(ctx).await.expect("process should succeed");

        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
    }

    /// A change on a path the claim never mentioned still passes: the guard
    /// asks "did anything change", never "did the claimed paths change".
    #[tokio::test]
    async fn write_verification_passes_when_changed_path_differs_from_claim() {
        let worktree = temp_worktree();
        write_harness(&worktree, json!([]));
        let ctx = ctx_with_implement_claim(&worktree, &["src/claimed.rs"]);

        let node = TestTaskNode::new().with_runner(porcelain_runner(" M src/other.rs\n"));
        let out = node.process(ctx).await.expect("process should succeed");

        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
    }

    /// BACKWARD COMPATIBILITY: a task deserialized from a `tasks.json`
    /// entry with no `expects_writes` field — every such file in the fleet
    /// today — defaults to `true` and is therefore GUARDED. The default is
    /// the safe direction: a forgotten field costs one retry, an unguarded
    /// task reports work that never ran as done.
    #[tokio::test]
    async fn write_verification_guards_task_json_without_the_new_field() {
        let task: SDLCTask = serde_json::from_value(json!({
            "task_id": 1,
            "title": "One",
            "description": "d1",
        }))
        .expect("a tasks.json entry without expects_writes must still parse");
        assert!(
            task.expects_writes,
            "absent expects_writes must default to true"
        );

        let worktree = temp_worktree();
        write_harness(&worktree, json!([]));
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task_and_worktree(&state, &task, &worktree);

        let node = TestTaskNode::new().with_runner(porcelain_runner(""));
        let out = node.process(ctx).await.expect("process should succeed");

        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], false);
        let results = out.nodes["TestTaskNode"]["check_results"]
            .as_array()
            .unwrap();
        assert_eq!(results[0]["kind"], "write-verification");
    }

    /// A guard failure and a harness-check failure both surface in
    /// `check_results`/`failure_summary` together when both are present.
    #[tokio::test]
    async fn write_verification_and_harness_failures_both_reported() {
        let worktree = temp_worktree();
        write_harness(
            &worktree,
            json!([{ "name": "always_fail", "command": "exit 1", "gates": true }]),
        );
        let ctx = ctx_with_implement_claim(&worktree, &["src/lib.rs"]);

        let runner: CommandRunner = Arc::new(move |program, args, _cwd| {
            if program == "git" && args.first() == Some(&"status") {
                Ok(CommandOutput {
                    status: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            } else {
                Ok(CommandOutput {
                    status: 1,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
        });

        let node = TestTaskNode::new().with_runner(runner);
        let out = node.process(ctx).await.expect("process should succeed");

        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], false);
        let results = out.nodes["TestTaskNode"]["check_results"]
            .as_array()
            .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["kind"], "write-verification");
        assert_eq!(results[1]["name"], "always_fail");
    }

    #[tokio::test]
    async fn forbidden_pattern_scan_fails_on_unallowlisted_match() {
        let worktree = temp_worktree();
        std::fs::create_dir_all(worktree.join("app")).unwrap();
        std::fs::write(worktree.join("app").join("bad.py"), "open(\"x\")\n").unwrap();
        write_harness(
            &worktree,
            json!([{
                "kind": "forbidden-pattern-scan",
                "name": "open-without-encoding",
                "gates": true,
                "rules": [{ "id": "r1", "pattern": "open\\(", "paths": "--include='*.py' app/" }],
            }]),
        );

        let node = TestTaskNode::new();
        let out = node
            .process(ctx_for_worktree(&worktree))
            .await
            .expect("process should succeed");
        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], false);
        let results = out.nodes["TestTaskNode"]["check_results"]
            .as_array()
            .unwrap();
        assert_eq!(results[0]["kind"], "forbidden-pattern-scan");
        assert_eq!(results[0]["passed"], false);
    }

    #[tokio::test]
    async fn forbidden_pattern_scan_passes_when_match_is_allowlisted() {
        let worktree = temp_worktree();
        std::fs::create_dir_all(worktree.join("app")).unwrap();
        std::fs::write(
            worktree.join("app").join("ok.py"),
            "open(\"x\", encoding=\"utf-8\")\n",
        )
        .unwrap();
        write_harness(
            &worktree,
            json!([{
                "kind": "forbidden-pattern-scan",
                "name": "open-without-encoding",
                "gates": true,
                "rules": [{
                    "id": "r1",
                    "pattern": "open\\(",
                    "paths": "--include='*.py' app/",
                    "allowlistPattern": "encoding=",
                }],
            }]),
        );

        let node = TestTaskNode::new();
        let out = node
            .process(ctx_for_worktree(&worktree))
            .await
            .expect("process should succeed");
        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
    }

    /// Records every invocation's `(program, args)` pair — unlike
    /// [`recording_command_runner`], which assumes the `sh -c <command>`
    /// shape and only ever records `args[1]`. EN.3.G task 5's direct `grep`
    /// invocation has a different shape (`program = "grep"`,
    /// `args = ["-rnE", pattern, ...paths]`), so the forbidden-pattern-scan
    /// tests below need the full argv to assert the pattern lands as its
    /// own unmodified entry rather than being interpolated into a string.
    /// `(program, args)` pairs recorded by [`recording_argv_runner`], shared with the runner
    /// closure via `Arc<Mutex<_>>` so the test can inspect invocations after the fact.
    type RecordedArgvCalls = Arc<Mutex<Vec<(String, Vec<String>)>>>;

    fn recording_argv_runner() -> (CommandRunner, RecordedArgvCalls) {
        let recorded: RecordedArgvCalls = Arc::new(Mutex::new(Vec::new()));
        let recorded_clone = recorded.clone();
        let runner: CommandRunner = Arc::new(move |program, args, _cwd| {
            if let Some(probe) = write_verification_probe(program, args) {
                return Ok(probe);
            }
            recorded_clone.lock().unwrap().push((
                program.to_string(),
                args.iter().map(|a| (*a).to_string()).collect(),
            ));
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });
        (runner, recorded)
    }

    #[tokio::test]
    async fn forbidden_pattern_scan_passes_pattern_as_its_own_argv_entry() {
        // Patterns that would break or inject through `sh -c 'grep ... '{pattern}'
        // ...'` string interpolation must land as a single, unmodified argv
        // entry to a directly-invoked `grep` — never inside an `sh -c` string.
        for pattern in ["it's", "say \"hi\"", "$(touch /tmp/pwned)", "foo; rm -rf /"] {
            let worktree = temp_worktree();
            write_harness(
                &worktree,
                json!([{
                    "kind": "forbidden-pattern-scan",
                    "name": "scan",
                    "gates": true,
                    "rules": [{ "id": "r1", "pattern": pattern, "paths": "app/" }],
                }]),
            );

            let (runner, recorded) = recording_argv_runner();
            let node = TestTaskNode::new().with_runner(runner);
            node.process(ctx_for_worktree(&worktree))
                .await
                .expect("process should succeed");

            let recorded = recorded.lock().unwrap();
            assert_eq!(
                recorded.len(),
                1,
                "expected exactly one invocation for pattern {pattern:?}"
            );
            let (program, args) = &recorded[0];
            assert_eq!(
                program, "grep",
                "pattern {pattern:?} did not use grep directly"
            );
            assert_ne!(
                program, "sh",
                "pattern {pattern:?} leaked into an sh -c string"
            );
            assert!(
                args.iter().any(|a| a == pattern),
                "pattern {pattern:?} was not passed as its own unmodified argv entry: {args:?}"
            );
        }
    }

    #[tokio::test]
    async fn forbidden_pattern_scan_empty_paths_issues_no_invocation() {
        let worktree = temp_worktree();
        write_harness(
            &worktree,
            json!([{
                "kind": "forbidden-pattern-scan",
                "name": "scan",
                "gates": true,
                "rules": [{ "id": "r1", "pattern": "open\\(", "paths": "" }],
            }]),
        );

        let (runner, recorded) = recording_argv_runner();
        let node = TestTaskNode::new().with_runner(runner);
        let out = node
            .process(ctx_for_worktree(&worktree))
            .await
            .expect("process should succeed");

        assert!(recorded.lock().unwrap().is_empty());
        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
    }

    #[tokio::test]
    async fn forbidden_pattern_scan_multi_path_becomes_multiple_argv_entries() {
        let worktree = temp_worktree();
        write_harness(
            &worktree,
            json!([{
                "kind": "forbidden-pattern-scan",
                "name": "scan",
                "gates": true,
                "rules": [{ "id": "r1", "pattern": "open\\(", "paths": "app/ lib/" }],
            }]),
        );

        let (runner, recorded) = recording_argv_runner();
        let node = TestTaskNode::new().with_runner(runner);
        node.process(ctx_for_worktree(&worktree))
            .await
            .expect("process should succeed");

        let recorded = recorded.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        let (program, args) = &recorded[0];
        assert_eq!(program, "grep");
        assert_eq!(args, &vec!["-rnE", "open\\(", "app/", "lib/"]);
    }

    #[tokio::test]
    async fn baseline_diff_fails_on_net_new_entry() {
        let worktree = temp_worktree();
        write_harness(
            &worktree,
            json!([{
                "kind": "baseline-diff",
                "name": "net-new-lint",
                "gates": true,
                "compareKeys": ["file", "code"],
                "baselineCommand": "echo '[{\"file\":\"a.py\",\"code\":\"E1\"}]'",
                "command": "echo '[{\"file\":\"a.py\",\"code\":\"E1\"},{\"file\":\"b.py\",\"code\":\"E2\"}]'",
            }]),
        );

        let node = TestTaskNode::new();
        let out = node
            .process(ctx_for_worktree(&worktree))
            .await
            .expect("process should succeed");
        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], false);
        let results = out.nodes["TestTaskNode"]["check_results"]
            .as_array()
            .unwrap();
        assert_eq!(results[0]["message"], "1 net-new violation(s)");
    }

    #[tokio::test]
    async fn baseline_diff_passes_when_no_net_new_entries() {
        let worktree = temp_worktree();
        write_harness(
            &worktree,
            json!([{
                "kind": "baseline-diff",
                "name": "net-new-lint",
                "gates": true,
                "compareKeys": ["file", "code"],
                "baselineCommand": "echo '[{\"file\":\"a.py\",\"code\":\"E1\"}]'",
                "command": "echo '[{\"file\":\"a.py\",\"code\":\"E1\"}]'",
            }]),
        );

        let node = TestTaskNode::new();
        let out = node
            .process(ctx_for_worktree(&worktree))
            .await
            .expect("process should succeed");
        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
    }

    #[tokio::test]
    async fn count_delta_fails_when_count_decreases() {
        let worktree = temp_worktree();
        write_harness(
            &worktree,
            json!([{
                "kind": "count-delta",
                "name": "pytest-count",
                "gates": true,
                "baseline": 100,
                "countPattern": "\\d+ passed",
                "failOn": "decrease",
                "command": "echo '90 passed'",
            }]),
        );

        let node = TestTaskNode::new();
        let out = node
            .process(ctx_for_worktree(&worktree))
            .await
            .expect("process should succeed");
        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], false);
        let results = out.nodes["TestTaskNode"]["check_results"]
            .as_array()
            .unwrap();
        assert_eq!(results[0]["message"], "count 90 vs baseline 100 (decrease)");
    }

    #[tokio::test]
    async fn count_delta_passes_when_count_holds_or_grows() {
        let worktree = temp_worktree();
        write_harness(
            &worktree,
            json!([{
                "kind": "count-delta",
                "name": "pytest-count",
                "gates": true,
                "baseline": 100,
                "countPattern": "\\d+ passed",
                "failOn": "decrease",
                "command": "echo '101 passed'",
            }]),
        );

        let node = TestTaskNode::new();
        let out = node
            .process(ctx_for_worktree(&worktree))
            .await
            .expect("process should succeed");
        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
    }

    #[tokio::test]
    async fn warning_scan_does_not_gate_by_default() {
        let worktree = temp_worktree();
        write_harness(
            &worktree,
            json!([{
                "kind": "warning-scan",
                "name": "app-import",
                "gates": false,
                "command": "echo 'UserWarning: field shadows an attribute'",
                "warningPatterns": ["UserWarning"],
            }]),
        );

        let node = TestTaskNode::new();
        let out = node
            .process(ctx_for_worktree(&worktree))
            .await
            .expect("process should succeed");
        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
        let results = out.nodes["TestTaskNode"]["check_results"]
            .as_array()
            .unwrap();
        assert_eq!(results[0]["passed"], true);
        assert!(results[0]["message"]
            .as_str()
            .unwrap()
            .contains("warning pattern(s) matched"));
    }

    #[tokio::test]
    async fn warning_scan_gates_when_check_opts_in() {
        let worktree = temp_worktree();
        write_harness(
            &worktree,
            json!([{
                "kind": "warning-scan",
                "name": "app-import",
                "gates": true,
                "command": "echo 'UserWarning: field shadows an attribute'",
                "warningPatterns": ["UserWarning"],
            }]),
        );

        let node = TestTaskNode::new();
        let out = node
            .process(ctx_for_worktree(&worktree))
            .await
            .expect("process should succeed");
        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], false);
    }

    // --- ConsolidatedReviewNode ------------------------------------------

    #[tokio::test]
    async fn review_parses_content_and_uses_diff() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let mut ctx = ctx_with_current_task(&state, &task);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": "." }),
        );

        let diff_called = Arc::new(Mutex::new(false));
        let diff_called_clone = diff_called.clone();
        let runner: CommandRunner = Arc::new(move |_program, args, _cwd| {
            if args == ["diff", "HEAD"] {
                *diff_called_clone.lock().unwrap() = true;
            } else {
                assert_eq!(args, ["add", "-N", "-A"], "unexpected git argv");
            }
            Ok(CommandOutput {
                status: 0,
                stdout: "diff --git a b".to_string(),
                stderr: String::new(),
            })
        });

        let canned =
            json!({ "verdict": "PASS", "summary": "looks good", "issues": [] }).to_string();
        let transport: ModelTransport = Arc::new(move |_config, _prompt| {
            let outcome = Outcome {
                cost_usd: 0.0,
                usage: claude_code_rs::parse::Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
                model_usage: std::collections::BTreeMap::new(),
                text: canned.clone(),
                is_error: false,
                api_error_status: None,
                structured_output: None,
            };
            Box::pin(async move { Ok(outcome) })
        });

        let node = ConsolidatedReviewNode::new()
            .with_runner(runner)
            .with_transport(transport);
        let out = node.process(ctx).await.expect("process should succeed");
        assert!(*diff_called.lock().unwrap());
        assert_eq!(out.nodes["ConsolidatedReviewNode"]["verdict"], "PASS");
    }

    /// The review diff is the WORKING TREE against `HEAD`, taken after an
    /// intent-to-add pass — never a commit range. A `base_sha` stamped by
    /// `SetupWorktreeNode` is run metadata and must NOT be used as a diff
    /// base: nothing commits the implementer's code until `SaveStateNode`
    /// runs on the pass path, so `<base_sha>..HEAD` was empty on every run
    /// and the reviewer reviewed nothing.
    #[tokio::test]
    async fn review_diffs_the_working_tree_against_head_after_intent_add() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let mut ctx = ctx_with_current_task(&state, &task);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": ".", "base_sha": "abc1234" }),
        );

        let calls: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
        let calls_clone = calls.clone();
        let runner: CommandRunner = Arc::new(move |_program, args, _cwd| {
            calls_clone
                .lock()
                .unwrap()
                .push(args.iter().map(|s| (*s).to_string()).collect());
            Ok(CommandOutput {
                status: 0,
                stdout: "diff --git a b".to_string(),
                stderr: String::new(),
            })
        });

        let canned =
            json!({ "verdict": "PASS", "summary": "looks good", "issues": [] }).to_string();
        let transport: ModelTransport = Arc::new(move |_config, _prompt| {
            let outcome = canned_outcome(canned.clone());
            Box::pin(async move { Ok(outcome) })
        });

        let node = ConsolidatedReviewNode::new()
            .with_runner(runner)
            .with_transport(transport);
        let out = node.process(ctx).await.expect("process should succeed");

        let recorded = calls.lock().unwrap();
        assert_eq!(
            *recorded,
            vec![
                vec!["add".to_string(), "-N".to_string(), "-A".to_string()],
                vec!["diff".to_string(), "HEAD".to_string()],
            ],
            "intent-to-add must precede the working-tree diff, and the diff \
             base must be HEAD — not the stamped base_sha"
        );
        assert_eq!(out.nodes["ConsolidatedReviewNode"]["verdict"], "PASS");
    }

    /// **The headline regression for this defect.** Captures the prompt
    /// `ConsolidatedReviewNode` actually sends and asserts the diff text the
    /// runner returned is inside it — i.e. the reviewer is shown real code,
    /// not an empty diff.
    ///
    /// Confirmed to fail against pre-ticket code: the node then built its
    /// argv from `diff_range(ctx)`, so with `base_sha` stamped it invoked
    /// `["diff", "abc1234..HEAD"]`. This stub returns the sentinel ONLY for
    /// `["diff", "HEAD"]` and the empty string otherwise, so pre-ticket the
    /// captured prompt ends in `Diff:\n` and the `contains` assertion fails.
    /// In the real tree the failure was the same for a different reason:
    /// nothing ever committed code, so that commit range was genuinely
    /// empty on every run.
    #[tokio::test]
    async fn review_prompt_carries_the_actual_working_tree_diff() {
        const SENTINEL: &str = "+fn sentinel_reviewed_line()";

        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let mut ctx = ctx_with_current_task(&state, &task);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": ".", "base_sha": "abc1234" }),
        );

        let runner: CommandRunner = Arc::new(|_program, args, _cwd| {
            let stdout = if args == ["diff", "HEAD"] {
                format!("diff --git a/src/lib.rs b/src/lib.rs\n{SENTINEL}\n")
            } else {
                String::new()
            };
            Ok(CommandOutput {
                status: 0,
                stdout,
                stderr: String::new(),
            })
        });

        let seen_prompt: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let seen_prompt_clone = seen_prompt.clone();
        let canned =
            json!({ "verdict": "PASS", "summary": "looks good", "issues": [] }).to_string();
        let transport: ModelTransport = Arc::new(move |_config, prompt: String| {
            *seen_prompt_clone.lock().unwrap() = Some(prompt);
            let outcome = canned_outcome(canned.clone());
            Box::pin(async move { Ok(outcome) })
        });

        let node = ConsolidatedReviewNode::new()
            .with_runner(runner)
            .with_transport(transport);
        node.process(ctx).await.expect("process should succeed");

        let prompt = seen_prompt
            .lock()
            .unwrap()
            .clone()
            .expect("transport should have been called");
        assert!(
            prompt.contains(SENTINEL),
            "reviewer prompt must contain the working-tree diff; got:\n{prompt}"
        );
    }

    // --- Reviewer diff bound (`review_diff_max_chars`) ---------------------

    #[test]
    fn bound_review_diff_leaves_a_diff_inside_the_budget_untouched() {
        let diff = "diff --git a/x b/x\n+one line\n";
        let (out, truncated) = bound_review_diff(diff, 120_000);
        assert_eq!(out, diff);
        assert!(!truncated);
        assert!(!out.contains("DIFF TRUNCATED"));
    }

    /// The property this knob exists for: an over-budget diff is CLIPPED,
    /// not dropped, and the clip is announced to the model. A silent
    /// truncation would let the reviewer PASS code it never saw — the same
    /// rubber stamp the empty-diff bug produced.
    #[test]
    fn bound_review_diff_marks_truncation_visibly_and_respects_the_budget() {
        let diff = "x".repeat(50_000);
        let budget = 1_000;
        let (out, truncated) = bound_review_diff(&diff, budget);

        assert!(truncated);
        assert!(
            out.contains("DIFF TRUNCATED"),
            "truncation must be visible to the model; got:\n{out}"
        );
        assert!(
            out.contains("PARTIAL DIFF"),
            "the notice must say the diff is partial; got:\n{out}"
        );
        assert!(
            out.chars().count() <= budget,
            "bounded diff must fit the budget: {} > {budget}",
            out.chars().count()
        );
        // Clipped, not dropped: the head of the diff still made it through.
        assert!(out.starts_with("xxxx"));
    }

    /// A budget too small to hold the notice still emits the notice — the
    /// same precedence `render_feedback_block` gives its check labels.
    #[test]
    fn bound_review_diff_emits_the_notice_even_on_an_absurd_budget() {
        let (out, truncated) = bound_review_diff(&"x".repeat(500), 5);
        assert!(truncated);
        assert!(out.contains("DIFF TRUNCATED"), "got:\n{out}");
    }

    /// Multi-byte content must not panic (the reason [`truncate_chars`]
    /// counts characters rather than slicing bytes).
    #[test]
    fn bound_review_diff_handles_multibyte_content() {
        let diff = "é".repeat(5_000);
        let (out, truncated) = bound_review_diff(&diff, 800);
        assert!(truncated);
        assert!(out.chars().count() <= 800);
    }

    /// End-to-end through the node: the resolved bound clips the prompt's
    /// diff, the notice reaches the model, and the resolved value + a
    /// `review_diff_truncated` flag are stamped for telemetry (standing
    /// rule 6).
    #[tokio::test]
    async fn review_node_bounds_the_prompt_diff_and_stamps_the_resolved_value() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let mut ctx = ctx_with_current_task(&state, &task);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": "." }),
        );
        let policy = SdlcPolicy {
            review_diff_max_chars: 2_000,
            ..SdlcPolicy::default()
        };
        let ctx = ctx_with_policy(ctx, &policy);

        let runner: CommandRunner = Arc::new(|_program, args, _cwd| {
            let stdout = if args == ["diff", "HEAD"] {
                "z".repeat(100_000)
            } else {
                String::new()
            };
            Ok(CommandOutput {
                status: 0,
                stdout,
                stderr: String::new(),
            })
        });

        let seen_prompt: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let seen_prompt_clone = seen_prompt.clone();
        let transport: ModelTransport = Arc::new(move |_config, prompt: String| {
            *seen_prompt_clone.lock().unwrap() = Some(prompt);
            let outcome = canned_outcome(
                json!({ "verdict": "PASS", "summary": "ok", "issues": [] }).to_string(),
            );
            Box::pin(async move { Ok(outcome) })
        });

        let node = ConsolidatedReviewNode::new()
            .with_runner(runner)
            .with_transport(transport);
        let out = node.process(ctx).await.expect("process should succeed");

        let prompt = seen_prompt
            .lock()
            .unwrap()
            .clone()
            .expect("transport should have been called");
        assert!(
            prompt.contains("DIFF TRUNCATED"),
            "the reviewer must be told its diff was clipped"
        );
        assert!(
            prompt.chars().count() < 100_000,
            "the 100k-character diff must not have reached the prompt whole"
        );

        let result = &out.nodes["ConsolidatedReviewNode"];
        assert_eq!(result["review_diff_max_chars"], json!(2_000));
        assert_eq!(result["review_diff_truncated"], json!(true));
        // Shape invariant: the verdict contract the router reads is unchanged.
        assert_eq!(result["verdict"], "PASS");
    }

    /// Behavior-stable default: a realistic task diff (well under the
    /// built-in 120k ceiling) reaches the reviewer intact and unannotated.
    #[tokio::test]
    async fn review_node_leaves_a_realistic_diff_untruncated_by_default() {
        const SENTINEL: &str = "TAIL-OF-THE-DIFF-SENTINEL";
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let mut ctx = ctx_with_current_task(&state, &task);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": "." }),
        );

        let runner: CommandRunner = Arc::new(|_program, args, _cwd| {
            let stdout = if args == ["diff", "HEAD"] {
                // ~40k characters — the fat end of a realistic one-task diff.
                format!(
                    "{}\n{SENTINEL}\n",
                    "+ a line of changed code\n".repeat(1_600)
                )
            } else {
                String::new()
            };
            Ok(CommandOutput {
                status: 0,
                stdout,
                stderr: String::new(),
            })
        });

        let seen_prompt: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let seen_prompt_clone = seen_prompt.clone();
        let transport: ModelTransport = Arc::new(move |_config, prompt: String| {
            *seen_prompt_clone.lock().unwrap() = Some(prompt);
            let outcome = canned_outcome(
                json!({ "verdict": "PASS", "summary": "ok", "issues": [] }).to_string(),
            );
            Box::pin(async move { Ok(outcome) })
        });

        let node = ConsolidatedReviewNode::new()
            .with_runner(runner)
            .with_transport(transport);
        let out = node.process(ctx).await.expect("process should succeed");

        let prompt = seen_prompt.lock().unwrap().clone().expect("called");
        assert!(prompt.contains(SENTINEL), "the diff's tail must survive");
        assert!(!prompt.contains("DIFF TRUNCATED"));
        assert_eq!(
            out.nodes["ConsolidatedReviewNode"]["review_diff_truncated"],
            json!(false)
        );
        assert_eq!(
            out.nodes["ConsolidatedReviewNode"]["review_diff_max_chars"],
            json!(120_000)
        );
    }

    /// The model's `Config.cwd` is scoped to the run's worktree — without
    /// this, a real review call that reads the filesystem checks the host
    /// process's ambient cwd instead of the task's actual worktree.
    #[tokio::test]
    async fn review_node_scopes_config_cwd_to_worktree() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let mut ctx = ctx_with_current_task(&state, &task);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": "/tmp/some-worktree" }),
        );

        let seen_config: Arc<Mutex<Option<Config>>> = Arc::new(Mutex::new(None));
        let seen_config_clone = seen_config.clone();
        let transport: ModelTransport = Arc::new(move |config, _prompt| {
            *seen_config_clone.lock().unwrap() = Some(config);
            let outcome = canned_outcome(
                json!({ "verdict": "PASS", "summary": "ok", "issues": [] }).to_string(),
            );
            Box::pin(async move { Ok(outcome) })
        });

        let runner: CommandRunner = Arc::new(|_program, _args, _cwd| {
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });
        let node = ConsolidatedReviewNode::new()
            .with_runner(runner)
            .with_transport(transport);
        node.process(ctx).await.expect("process should succeed");

        let config = seen_config
            .lock()
            .unwrap()
            .clone()
            .expect("transport should have been called");
        assert_eq!(
            config.cwd,
            Some(std::path::PathBuf::from("/tmp/some-worktree"))
        );
    }

    /// Same normalization as `TriageTaskNode`'s llm branch, and for the same
    /// reason: a real model reply's casing isn't guaranteed, and an
    /// un-normalized mismatch makes `ReviewRouterNode`'s exact match fail
    /// closed to `None` (observed live: a real reply returned "pass").
    #[tokio::test]
    async fn review_node_normalizes_lowercase_verdict() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let mut ctx = ctx_with_current_task(&state, &task);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": "." }),
        );

        let transport: ModelTransport = Arc::new(move |_config, _prompt| {
            let outcome = canned_outcome(
                json!({ "verdict": "pass", "summary": "ok", "issues": [] }).to_string(),
            );
            Box::pin(async move { Ok(outcome) })
        });
        let runner: CommandRunner = Arc::new(|_program, _args, _cwd| {
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });

        let node = ConsolidatedReviewNode::new()
            .with_runner(runner)
            .with_transport(transport);
        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["ConsolidatedReviewNode"]["verdict"], "PASS");
        // A recognized verdict must NOT carry the `unrecognized_verdict` key.
        assert!(out.nodes["ConsolidatedReviewNode"]
            .get("unrecognized_verdict")
            .is_none());
    }

    /// EN.3.G task 1: a garbage model verdict is stamped as
    /// `unrecognized_verdict` (alongside the byte-identical, unchanged
    /// `verdict` key `ReviewRouterNode` still matches on) so
    /// `derive_terminal_signal` can surface it in the run's `bail_reason`.
    #[tokio::test]
    async fn review_node_stamps_unrecognized_verdict() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let mut ctx = ctx_with_current_task(&state, &task);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": "." }),
        );

        let transport: ModelTransport = Arc::new(move |_config, _prompt| {
            let outcome = canned_outcome(
                json!({ "verdict": "WAT", "summary": "unclear", "issues": [] }).to_string(),
            );
            Box::pin(async move { Ok(outcome) })
        });
        let runner: CommandRunner = Arc::new(|_program, _args, _cwd| {
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });

        let node = ConsolidatedReviewNode::new()
            .with_runner(runner)
            .with_transport(transport);
        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["ConsolidatedReviewNode"]["verdict"], "WAT");
        assert_eq!(
            out.nodes["ConsolidatedReviewNode"]["unrecognized_verdict"],
            "WAT"
        );
    }

    /// A schema-tagged reply (`structured_output: Some(..)`) is consumed via
    /// the `structured` field written by `ClaudeCodeStep`, not the
    /// fence-strip path — proven by making `text` a value that would fail a
    /// strict-JSON parse (an unfenced non-JSON string) while `structured`
    /// carries the real payload.
    #[tokio::test]
    async fn review_prefers_structured_output_over_fence_parse() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let mut ctx = ctx_with_current_task(&state, &task);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": "." }),
        );

        let transport: ModelTransport = Arc::new(move |_config, _prompt| {
            let mut outcome = canned_outcome("not valid json at all".to_string());
            outcome.structured_output =
                Some(json!({ "verdict": "PASS", "summary": "from structured", "issues": [] }));
            Box::pin(async move { Ok(outcome) })
        });
        let runner: CommandRunner = Arc::new(|_program, _args, _cwd| {
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });

        let node = ConsolidatedReviewNode::new()
            .with_runner(runner)
            .with_transport(transport);
        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["ConsolidatedReviewNode"]["verdict"], "PASS");
        assert_eq!(
            out.nodes["ConsolidatedReviewNode"]["summary"],
            "from structured"
        );
    }

    /// A fence-only reply (`structured_output: None`) still parses via the
    /// `strip_json_fence` + `serde_json::from_str` fallback.
    #[tokio::test]
    async fn review_falls_back_to_fence_parse_when_structured_absent() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let mut ctx = ctx_with_current_task(&state, &task);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": "." }),
        );

        let fenced = format!(
            "```json\n{}\n```",
            json!({ "verdict": "PASS", "summary": "from fence", "issues": [] })
        );
        let transport: ModelTransport = Arc::new(move |_config, _prompt| {
            let outcome = canned_outcome(fenced.clone());
            Box::pin(async move { Ok(outcome) })
        });
        let runner: CommandRunner = Arc::new(|_program, _args, _cwd| {
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });

        let node = ConsolidatedReviewNode::new()
            .with_runner(runner)
            .with_transport(transport);
        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["ConsolidatedReviewNode"]["verdict"], "PASS");
        assert_eq!(out.nodes["ConsolidatedReviewNode"]["summary"], "from fence");
    }

    /// `EN.ticket.sdlc-flow-dead-policy-knobs` task 3: a non-default
    /// `transport_retry` on the resolved policy changes the observed
    /// attempt count against a persistently failing transport for
    /// `ConsolidatedReviewNode`.
    #[tokio::test]
    async fn review_transport_retry_nondefault_changes_observed_attempts() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let mut ctx = ctx_with_current_task(&state, &task);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": "." }),
        );
        let policy = SdlcPolicy {
            transport_retry: TransportRetry {
                max_attempts: 4,
                initial_backoff_ms: 0,
            },
            ..SdlcPolicy::default()
        };
        let ctx = ctx_with_policy(ctx, &policy);

        let runner: CommandRunner = Arc::new(|_program, _args, _cwd| {
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });
        let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
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

        let node = ConsolidatedReviewNode::new()
            .with_runner(runner)
            .with_transport(transport);
        let result = node.process(ctx).await;
        assert!(
            result.is_err(),
            "persistent failure must still halt the walk"
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 4);
    }

    // --- Policy consumption (EN.3.C task 3) ---------------------------------

    /// Stamp a resolved [`SdlcPolicy`] into `ctx` under the same identity
    /// `SetupWorktreeNode` uses, so a node's `resolved_policy(&ctx)` read
    /// sees it.
    fn ctx_with_policy(mut ctx: TaskContext, policy: &SdlcPolicy) -> TaskContext {
        put_result(
            &mut ctx,
            RESOLVED_POLICY_IDENTITY,
            serde_json::to_value(policy).expect("SdlcPolicy serializes"),
        );
        ctx
    }

    fn canned_outcome(text: String) -> Outcome {
        Outcome {
            cost_usd: 0.0,
            usage: claude_code_rs::parse::Usage {
                input_tokens: 1,
                output_tokens: 1,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
            model_usage: std::collections::BTreeMap::new(),
            text,
            is_error: false,
            api_error_status: None,
            structured_output: None,
        }
    }

    /// A node built with `model_tiers.implement = haiku` produces a
    /// `Config` carrying the haiku model string.
    #[tokio::test]
    async fn implement_node_consumes_resolved_model_tier() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task(&state, &task);

        let policy = SdlcPolicy {
            model_tiers: ModelTiers {
                implement: ModelTier::Haiku,
                ..ModelTiers::default()
            },
            ..SdlcPolicy::default()
        };
        let ctx = ctx_with_policy(ctx, &policy);

        let seen_config: Arc<Mutex<Option<Config>>> = Arc::new(Mutex::new(None));
        let seen_config_clone = seen_config.clone();
        let transport: ModelTransport = Arc::new(move |config, _prompt| {
            *seen_config_clone.lock().unwrap() = Some(config);
            let outcome = canned_outcome(json!({ "summary": "done" }).to_string());
            Box::pin(async move { Ok(outcome) })
        });

        let node = ImplementTaskNode::new().with_transport(transport);
        node.process(ctx).await.expect("process should succeed");

        let config = seen_config
            .lock()
            .unwrap()
            .clone()
            .expect("transport should have been called");
        assert_eq!(config.model.as_deref(), Some("claude-haiku-4-5"));
    }

    // --- transport_retry (EN.ticket.sdlc-flow-dead-policy-knobs task 3) ----

    /// A transport that counts every invocation and always fails with a
    /// retryable `claude_code_rs::Error::Timeout` — used to observe how many
    /// attempts the resolved `transport_retry` budget actually burns.
    fn always_failing_transport(calls: Arc<std::sync::atomic::AtomicU32>) -> ModelTransport {
        Arc::new(move |_config, _prompt| {
            let calls = calls.clone();
            Box::pin(async move {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Err(claude_code_rs::Error::Timeout)
            })
        })
    }

    /// A default-policy `transport_retry` must reproduce exactly the attempt
    /// count `ClaudeCodeStep`'s own built-in default already produced before
    /// this ticket wired the policy value through — behaviour-stable, proven
    /// rather than assumed (both are literally `TransportRetry::default()`).
    #[tokio::test]
    async fn implement_transport_retry_default_matches_todays_observed_count() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task(&state, &task);

        let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let node = ImplementTaskNode::new().with_transport(always_failing_transport(calls.clone()));
        let result = node.process(ctx).await;

        assert!(
            result.is_err(),
            "persistent failure must still halt the walk"
        );
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            TransportRetry::default().max_attempts,
            "default policy transport_retry must match ClaudeCodeStep's own \
             built-in default attempt count"
        );
    }

    /// A non-default `transport_retry` set on the resolved policy changes
    /// the observed attempt count against a persistently failing transport —
    /// proof the value actually reaches `ImplementTaskNode`'s composed
    /// `ClaudeCodeStep`, not just that it resolves.
    #[tokio::test]
    async fn implement_transport_retry_nondefault_changes_observed_attempts() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task(&state, &task);
        let policy = SdlcPolicy {
            transport_retry: TransportRetry {
                max_attempts: 5,
                initial_backoff_ms: 0,
            },
            ..SdlcPolicy::default()
        };
        let ctx = ctx_with_policy(ctx, &policy);

        let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let node = ImplementTaskNode::new().with_transport(always_failing_transport(calls.clone()));
        let result = node.process(ctx).await;

        assert!(result.is_err());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 5);
    }

    /// `output_verbosity = terse` injects the terseness directive into the
    /// stage prompt; the default (`normal`) leaves the prompt untouched.
    #[tokio::test]
    async fn implement_node_injects_terse_directive() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task(&state, &task);

        let policy = SdlcPolicy {
            output_verbosity: OutputVerbosity::Terse,
            ..SdlcPolicy::default()
        };
        let ctx = ctx_with_policy(ctx, &policy);

        let seen_prompt: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let seen_prompt_clone = seen_prompt.clone();
        let transport: ModelTransport = Arc::new(move |_config, prompt| {
            *seen_prompt_clone.lock().unwrap() = Some(prompt);
            let outcome = canned_outcome(json!({ "summary": "done" }).to_string());
            Box::pin(async move { Ok(outcome) })
        });

        let node = ImplementTaskNode::new().with_transport(transport);
        node.process(ctx).await.expect("process should succeed");

        let prompt = seen_prompt
            .lock()
            .unwrap()
            .clone()
            .expect("transport should have been called");
        assert!(prompt.contains("Be terse"));
    }

    /// Records every prompt the transport is handed, and answers with a
    /// canned `ImplementOutput` success.
    fn prompt_recording_transport() -> (Arc<Mutex<Vec<String>>>, ModelTransport) {
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_clone = seen.clone();
        let transport: ModelTransport = Arc::new(move |_config, prompt| {
            seen_clone.lock().unwrap().push(prompt);
            let outcome = canned_outcome(json!({ "summary": "done" }).to_string());
            Box::pin(async move { Ok(outcome) })
        });
        (seen, transport)
    }

    /// **Change-detector, asserted against the literal text.** With no prior
    /// failure in `ctx` the first-attempt prompt must be byte-identical to
    /// pre-ticket behavior — the retry-feedback knob buys a retry block, not
    /// a rewritten first request. Any future perturbation of this string is
    /// caught here rather than in production.
    #[tokio::test]
    async fn implement_node_first_attempt_prompt_is_byte_identical() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task(&state, &task);

        let (seen, transport) = prompt_recording_transport();
        let node = ImplementTaskNode::new().with_transport(transport);
        node.process(ctx).await.expect("process should succeed");

        let prompts = seen.lock().unwrap().clone();
        assert_eq!(prompts.len(), 1);
        assert_eq!(
            prompts[0],
            format!(
                "{PATH_DISCIPLINE_PREAMBLE}Implement the following SDLC task. Respond with \
                 strict JSON of the shape {{\"summary\": str, \"modified_files\": [str], \
                 \"tests_added\": [str]}}.\n\nTitle: One\nDescription: d1\nAcceptance \
                 criteria: []"
            )
        );
    }

    /// The headline behavior: a ctx carrying a failed `TestTaskNode` result
    /// (i.e. the retry back-edge) puts the previous attempt's captured
    /// output in front of the model.
    #[tokio::test]
    async fn implement_node_retry_prompt_carries_the_prior_failure() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let mut ctx = ctx_with_current_task(&state, &task);
        ctx.nodes.insert(
            "TestTaskNode".to_string(),
            failed_test_result(
                "error[E0308]: `main` function has the wrong type\n  --> src/http.rs:682:1",
            ),
        );

        let (seen, transport) = prompt_recording_transport();
        let node = ImplementTaskNode::new().with_transport(transport);
        node.process(ctx).await.expect("process should succeed");

        let prompts = seen.lock().unwrap().clone();
        let prompt = &prompts[0];
        // The base prompt is still there in full, behind the preamble...
        assert!(
            prompt.starts_with(PATH_DISCIPLINE_PREAMBLE),
            "prompt: {prompt}"
        );
        assert!(prompt.contains("Implement the following SDLC task."));
        assert!(prompt.contains("Title: One"));
        // ...with the prior attempt's failure appended.
        assert!(
            prompt.contains("PREVIOUS ATTEMPT FAILED"),
            "prompt: {prompt}"
        );
        assert!(prompt.contains("FAILED CHECK: test"), "prompt: {prompt}");
        assert!(prompt.contains("error[E0308]"), "prompt: {prompt}");
        assert!(prompt.contains("src/http.rs:682"), "prompt: {prompt}");
    }

    /// The escape hatch: `retry_feedback.enabled = false` restores the
    /// byte-identical prompt even on a retry.
    #[tokio::test]
    async fn implement_node_retry_prompt_is_unchanged_when_knob_disabled() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let mut ctx = ctx_with_current_task(&state, &task);
        ctx.nodes.insert(
            "TestTaskNode".to_string(),
            failed_test_result("error[E0308]: mismatched types"),
        );
        let policy = SdlcPolicy {
            retry_feedback: RetryFeedback {
                enabled: false,
                max_chars: 4000,
            },
            ..SdlcPolicy::default()
        };
        let ctx = ctx_with_policy(ctx, &policy);

        let (seen, transport) = prompt_recording_transport();
        let node = ImplementTaskNode::new().with_transport(transport);
        node.process(ctx).await.expect("process should succeed");

        let prompts = seen.lock().unwrap().clone();
        assert_eq!(
            prompts[0],
            format!(
                "{PATH_DISCIPLINE_PREAMBLE}Implement the following SDLC task. Respond with \
                 strict JSON of the shape {{\"summary\": str, \"modified_files\": [str], \
                 \"tests_added\": [str]}}.\n\nTitle: One\nDescription: d1\nAcceptance \
                 criteria: []"
            )
        );
    }

    /// The P0 fix: with a worktree stamped, the implement prompt carries
    /// BOTH the run-invariant path-discipline sentences and the actual
    /// per-run worktree root. `cwd` alone does not constrain the model's
    /// choice of an absolute path; this text does.
    #[tokio::test]
    async fn implement_node_prompt_carries_path_discipline_and_worktree_root() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let mut ctx = ctx_with_current_task(&state, &task);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": "/tmp/some-worktree" }),
        );

        let (seen, transport) = prompt_recording_transport();
        let node = ImplementTaskNode::new().with_transport(transport);
        node.process(ctx).await.expect("process should succeed");

        let prompts = seen.lock().unwrap().clone();
        let prompt = &prompts[0];
        assert!(
            prompt.starts_with(PATH_DISCIPLINE_PREAMBLE),
            "prompt: {prompt}"
        );
        assert!(prompt.contains("relative to your current working directory"));
        assert!(prompt.contains("CLAUDE.md"), "prompt: {prompt}");
        assert!(prompt.contains("file://"), "prompt: {prompt}");
        assert!(prompt.contains("/tmp/some-worktree"), "prompt: {prompt}");
        assert!(
            prompt.contains("git rev-parse --show-toplevel"),
            "prompt: {prompt}"
        );
        // The task text still follows the preamble.
        assert!(prompt.contains("Title: One"), "prompt: {prompt}");
    }

    /// No `SetupWorktreeNode` result (a unit-test-driven ctx, or
    /// `use_worktree: false`): the node still succeeds, still carries the
    /// run-invariant discipline text, and names no path it does not have.
    #[tokio::test]
    async fn implement_node_prompt_omits_worktree_line_without_worktree() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task(&state, &task);

        let (seen, transport) = prompt_recording_transport();
        let node = ImplementTaskNode::new().with_transport(transport);
        node.process(ctx).await.expect("process should succeed");

        let prompts = seen.lock().unwrap().clone();
        let prompt = &prompts[0];
        assert!(
            prompt.starts_with(PATH_DISCIPLINE_PREAMBLE),
            "prompt: {prompt}"
        );
        assert!(
            !prompt.contains("Your working tree root is"),
            "prompt: {prompt}"
        );
        assert!(
            !prompt.contains("git rev-parse --show-toplevel"),
            "prompt: {prompt}"
        );
        assert!(prompt.contains("Title: One"), "prompt: {prompt}");
    }

    /// When `SetupWorktreeNode` has stamped a `worktree_path`, the model's
    /// `Config.cwd` is scoped to it — so a real session edits the actual
    /// checkout rather than the host process's ambient cwd.
    #[tokio::test]
    async fn implement_node_scopes_config_cwd_to_worktree() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task(&state, &task);
        let mut ctx = ctx;
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": "/tmp/some-worktree" }),
        );

        let seen_config: Arc<Mutex<Option<Config>>> = Arc::new(Mutex::new(None));
        let seen_config_clone = seen_config.clone();
        let transport: ModelTransport = Arc::new(move |config, _prompt| {
            *seen_config_clone.lock().unwrap() = Some(config);
            let outcome = canned_outcome(json!({ "summary": "done" }).to_string());
            Box::pin(async move { Ok(outcome) })
        });

        let node = ImplementTaskNode::new().with_transport(transport);
        node.process(ctx).await.expect("process should succeed");

        let config = seen_config
            .lock()
            .unwrap()
            .clone()
            .expect("transport should have been called");
        assert_eq!(
            config.cwd,
            Some(std::path::PathBuf::from("/tmp/some-worktree"))
        );
    }

    /// Without a `SetupWorktreeNode` result (e.g. a unit test driving the
    /// node directly), `Config.cwd` falls back to `None` rather than
    /// failing the node — today's pre-fix behavior is preserved when no
    /// worktree is known.
    #[tokio::test]
    async fn implement_node_leaves_cwd_none_without_worktree() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task(&state, &task);

        let seen_config: Arc<Mutex<Option<Config>>> = Arc::new(Mutex::new(None));
        let seen_config_clone = seen_config.clone();
        let transport: ModelTransport = Arc::new(move |config, _prompt| {
            *seen_config_clone.lock().unwrap() = Some(config);
            let outcome = canned_outcome(json!({ "summary": "done" }).to_string());
            Box::pin(async move { Ok(outcome) })
        });

        let node = ImplementTaskNode::new().with_transport(transport);
        node.process(ctx).await.expect("process should succeed");

        let config = seen_config
            .lock()
            .unwrap()
            .clone()
            .expect("transport should have been called");
        assert_eq!(config.cwd, None);
    }

    /// The `normal` (default) verbosity injects no directive, reproducing
    /// the pre-EN.3.C prompt text.
    #[tokio::test]
    async fn implement_node_normal_verbosity_adds_no_directive() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task(&state, &task);
        // No RESOLVED_POLICY_IDENTITY stamped -> falls back to built-in
        // default, which is `normal`.

        let seen_prompt: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let seen_prompt_clone = seen_prompt.clone();
        let transport: ModelTransport = Arc::new(move |_config, prompt| {
            *seen_prompt_clone.lock().unwrap() = Some(prompt);
            let outcome = canned_outcome(json!({ "summary": "done" }).to_string());
            Box::pin(async move { Ok(outcome) })
        });

        let node = ImplementTaskNode::new().with_transport(transport);
        node.process(ctx).await.expect("process should succeed");

        let prompt = seen_prompt
            .lock()
            .unwrap()
            .clone()
            .expect("transport should have been called");
        assert!(!prompt.contains("Be terse"));
        assert!(!prompt.contains("Be thorough"));
    }

    /// `prompt_cache = true` sets a stable `system_prompt` cache breakpoint
    /// on the composed `ClaudeCodeStep`'s `Config`; the default
    /// (`prompt_cache = false`) leaves it unset.
    #[tokio::test]
    async fn implement_node_sets_cache_breakpoint_when_prompt_cache_enabled() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task(&state, &task);

        let policy = SdlcPolicy {
            prompt_cache: true,
            ..SdlcPolicy::default()
        };
        let ctx = ctx_with_policy(ctx, &policy);

        let seen_config: Arc<Mutex<Option<Config>>> = Arc::new(Mutex::new(None));
        let seen_config_clone = seen_config.clone();
        let transport: ModelTransport = Arc::new(move |config, _prompt| {
            *seen_config_clone.lock().unwrap() = Some(config);
            let outcome = canned_outcome(json!({ "summary": "done" }).to_string());
            Box::pin(async move { Ok(outcome) })
        });

        let node = ImplementTaskNode::new().with_transport(transport);
        node.process(ctx).await.expect("process should succeed");

        let config = seen_config
            .lock()
            .unwrap()
            .clone()
            .expect("transport should have been called");
        assert_eq!(config.system_prompt.as_deref(), Some(STABLE_SYSTEM_PROMPT));

        // Baseline: no policy stamped -> falls back to the built-in
        // default (`prompt_cache = false`) -> no breakpoint set.
        let ctx2 = ctx_with_current_task(&state, &task);
        let seen_config2: Arc<Mutex<Option<Config>>> = Arc::new(Mutex::new(None));
        let seen_config2_clone = seen_config2.clone();
        let transport2: ModelTransport = Arc::new(move |config, _prompt| {
            *seen_config2_clone.lock().unwrap() = Some(config);
            let outcome = canned_outcome(json!({ "summary": "done" }).to_string());
            Box::pin(async move { Ok(outcome) })
        });
        let node2 = ImplementTaskNode::new().with_transport(transport2);
        node2.process(ctx2).await.expect("process should succeed");
        let config2 = seen_config2
            .lock()
            .unwrap()
            .clone()
            .expect("transport should have been called");
        assert_eq!(config2.system_prompt, None);
    }

    /// `TriageTaskNode`'s `llm_triage` model branch also consumes the
    /// resolved policy's `triage` tier.
    #[tokio::test]
    async fn triage_node_llm_branch_consumes_resolved_model_tier() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 3;
        task.attempt_count = 0;

        let policy = SdlcPolicy {
            model_tiers: ModelTiers {
                triage: ModelTier::Opus,
                ..ModelTiers::default()
            },
            ..SdlcPolicy::default()
        };
        let mut ctx = ctx_with_test_result(false, &task);
        ctx.event = json!({ "spec_slug": "my-spec", "llm_triage": true });
        let ctx = ctx_with_policy(ctx, &policy);

        let seen_config: Arc<Mutex<Option<Config>>> = Arc::new(Mutex::new(None));
        let seen_config_clone = seen_config.clone();
        let transport: ModelTransport = Arc::new(move |config, _prompt| {
            *seen_config_clone.lock().unwrap() = Some(config);
            let outcome =
                canned_outcome(json!({ "verdict": "RETRYABLE", "reason": "r" }).to_string());
            Box::pin(async move { Ok(outcome) })
        });

        let node = TriageTaskNode::new().with_transport(transport);
        node.process(ctx).await.expect("process should succeed");

        let config = seen_config
            .lock()
            .unwrap()
            .clone()
            .expect("transport should have been called");
        assert_eq!(config.model.as_deref(), Some("claude-opus-4-8"));
    }

    /// `ConsolidatedReviewNode` consumes the resolved policy's `review`
    /// tier.
    #[tokio::test]
    async fn review_node_consumes_resolved_model_tier() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let mut ctx = ctx_with_current_task(&state, &task);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": "." }),
        );

        let policy = SdlcPolicy {
            model_tiers: ModelTiers {
                review: ModelTier::Haiku,
                ..ModelTiers::default()
            },
            ..SdlcPolicy::default()
        };
        let ctx = ctx_with_policy(ctx, &policy);

        let runner: CommandRunner = Arc::new(|_program, _args, _cwd| {
            Ok(CommandOutput {
                status: 0,
                stdout: "diff --git a b".to_string(),
                stderr: String::new(),
            })
        });

        let seen_config: Arc<Mutex<Option<Config>>> = Arc::new(Mutex::new(None));
        let seen_config_clone = seen_config.clone();
        let transport: ModelTransport = Arc::new(move |config, _prompt| {
            *seen_config_clone.lock().unwrap() = Some(config);
            let outcome = canned_outcome(
                json!({ "verdict": "PASS", "summary": "ok", "issues": [] }).to_string(),
            );
            Box::pin(async move { Ok(outcome) })
        });

        let node = ConsolidatedReviewNode::new()
            .with_runner(runner)
            .with_transport(transport);
        node.process(ctx).await.expect("process should succeed");

        let config = seen_config
            .lock()
            .unwrap()
            .clone()
            .expect("transport should have been called");
        assert_eq!(config.model.as_deref(), Some("claude-haiku-4-5"));
    }

    // --- review_attempts counter (EN.ticket.review-retry-loop-unbounded task 2) ---

    /// Build a `ConsolidatedReviewNode` that always runs the `diff` runner
    /// and returns a canned PASS verdict via its transport, so each test in
    /// this section only has to vary the incoming `ctx`.
    fn passing_review_node() -> ConsolidatedReviewNode {
        let runner: CommandRunner = Arc::new(|_program, _args, _cwd| {
            Ok(CommandOutput {
                status: 0,
                stdout: "diff --git a b".to_string(),
                stderr: String::new(),
            })
        });
        let transport: ModelTransport = Arc::new(|_config, _prompt| {
            let outcome = canned_outcome(
                json!({ "verdict": "PASS", "summary": "ok", "issues": [] }).to_string(),
            );
            Box::pin(async move { Ok(outcome) })
        });
        ConsolidatedReviewNode::new()
            .with_runner(runner)
            .with_transport(transport)
    }

    fn ctx_for_review(state: &SDLCState, task: &SDLCTask) -> TaskContext {
        let mut ctx = ctx_with_current_task(state, task);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": "." }),
        );
        ctx
    }

    #[tokio::test]
    async fn review_node_increments_review_attempts_once_per_verdict() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_for_review(&state, &task);

        let out = passing_review_node()
            .process(ctx)
            .await
            .expect("process should succeed");

        let bumped: SDLCState =
            serde_json::from_value(out.nodes["ConsolidatedReviewNode"]["state"].clone())
                .expect("ConsolidatedReviewNode result carries a durable SDLCState");
        assert_eq!(bumped.telemetry.review_attempts, 1);
        // Independent of the attempt counters this run has not touched.
        assert_eq!(bumped.telemetry.total_attempts, 0);
        assert_eq!(bumped.tasks[0].attempt_count, 0);
    }

    #[tokio::test]
    async fn review_node_does_not_touch_attempt_count_or_total_attempts() {
        // A task that has already burned two test retries (IncrementAttemptNode
        // would have bumped both `attempt_count` and `telemetry.total_attempts`
        // to 2 by this point) must still arrive at review with a FULL review
        // budget — proving `review_attempts` is counted separately.
        let mut task = SDLCTask::new(1, "One", "d1");
        task.attempt_count = 2;
        let mut state = state_with_tasks(vec![task.clone()]);
        state.telemetry.total_attempts = 2;
        let ctx = ctx_for_review(&state, &task);

        let out = passing_review_node()
            .process(ctx)
            .await
            .expect("process should succeed");

        let bumped: SDLCState =
            serde_json::from_value(out.nodes["ConsolidatedReviewNode"]["state"].clone())
                .expect("ConsolidatedReviewNode result carries a durable SDLCState");
        assert_eq!(bumped.telemetry.review_attempts, 1);
        // Untouched by the review pass.
        assert_eq!(bumped.telemetry.total_attempts, 2);
        assert_eq!(bumped.tasks[0].attempt_count, 2);
    }

    #[tokio::test]
    async fn review_attempts_survives_a_resume_and_keeps_accumulating() {
        // Simulate a resumed run: the loaded state already carries an
        // accumulated `review_attempts` from a prior process invocation
        // (e.g. read back off disk by `LoadTaskStateNode`) — a fresh
        // process() call must add to it, not reset it.
        let task = SDLCTask::new(1, "One", "d1");
        let mut state = state_with_tasks(vec![task.clone()]);
        state.telemetry.review_attempts = 2;
        let ctx = ctx_for_review(&state, &task);

        let out = passing_review_node()
            .process(ctx)
            .await
            .expect("process should succeed");

        let bumped: SDLCState =
            serde_json::from_value(out.nodes["ConsolidatedReviewNode"]["state"].clone())
                .expect("ConsolidatedReviewNode result carries a durable SDLCState");
        assert_eq!(bumped.telemetry.review_attempts, 3);
    }

    /// A `ConsolidatedReviewNode` whose reviewer always returns `verdict`
    /// with `issue_count` distinct issues — the non-PASS counterpart to
    /// [`passing_review_node`].
    fn failing_review_node(verdict: &'static str, issue_count: usize) -> ConsolidatedReviewNode {
        let runner: CommandRunner = Arc::new(|_program, _args, _cwd| {
            Ok(CommandOutput {
                status: 0,
                stdout: "diff --git a b".to_string(),
                stderr: String::new(),
            })
        });
        let transport: ModelTransport = Arc::new(move |_config, _prompt| {
            let issues: Vec<String> = (0..issue_count).map(|i| format!("issue {i}")).collect();
            let outcome = canned_outcome(
                json!({ "verdict": verdict, "summary": "still not there", "issues": issues })
                    .to_string(),
            );
            Box::pin(async move { Ok(outcome) })
        });
        ConsolidatedReviewNode::new()
            .with_runner(runner)
            .with_transport(transport)
    }

    /// Run one `ConsolidatedReviewNode` pass for `task` against `state`,
    /// returning the ctx it produced plus the durable state it wrote — so a
    /// test can chain several reviews across several tasks the way a real
    /// run does.
    async fn review_once(
        node: &ConsolidatedReviewNode,
        state: &SDLCState,
        task: &SDLCTask,
    ) -> (TaskContext, SDLCState) {
        let ctx = ctx_for_review(state, task);
        let out = node.process(ctx).await.expect("process should succeed");
        let next: SDLCState =
            serde_json::from_value(out.nodes["ConsolidatedReviewNode"]["state"].clone())
                .expect("ConsolidatedReviewNode result carries a durable SDLCState");
        (out, next)
    }

    /// **The R6 regression.** Measured on a real 6-task `pragmatist` run:
    /// task 1 passed review, task 2 passed review, and task 3's FIRST
    /// review verdict — a minor `PARTIAL` — was routed straight to
    /// `WrapUpNode` because two SUCCESSFUL reviews had already consumed the
    /// whole run's review budget. The retry bound exists to stop an
    /// unbounded review loop on ONE task; a PASS is not a retry, and an
    /// earlier task's spend is not this task's.
    #[tokio::test]
    async fn earlier_tasks_passing_reviews_do_not_consume_a_later_tasks_retry_budget() {
        let tasks = vec![
            SDLCTask::new(1, "One", "d1"),
            SDLCTask::new(2, "Two", "d2"),
            SDLCTask::new(3, "Three", "d3"),
        ];
        let mut state = state_with_tasks(tasks.clone());

        // Tasks 1 and 2 each pass review on their first pass.
        let passing = passing_review_node();
        for task in &tasks[..2] {
            let (_ctx, next) = review_once(&passing, &state, task).await;
            state = next;
        }

        // Task 3 now gets its FIRST review verdict: a minor PARTIAL.
        let failing = failing_review_node("PARTIAL", 2);
        let (ctx, _next) = review_once(&failing, &state, &tasks[2]).await;

        assert_eq!(
            ReviewRouterNode.route(&ctx),
            Some("IncrementAttemptNode".to_string()),
            "task 3's FIRST non-PASS review must take the retry back-edge — \
             two earlier tasks' PASSING reviews must not have spent its \
             review budget"
        );
    }

    /// The same shape, carried all the way out: after two earlier tasks
    /// passed review, the third task must still get its FULL
    /// `max_review_attempts` allowance — retry, retry, then bail on the
    /// third non-PASS verdict.
    #[tokio::test]
    async fn a_later_task_still_receives_its_full_review_allowance() {
        let tasks = vec![
            SDLCTask::new(1, "One", "d1"),
            SDLCTask::new(2, "Two", "d2"),
            SDLCTask::new(3, "Three", "d3"),
        ];
        let mut state = state_with_tasks(tasks.clone());

        let passing = passing_review_node();
        for task in &tasks[..2] {
            let (_ctx, next) = review_once(&passing, &state, task).await;
            state = next;
        }

        let max = SdlcPolicy::default().max_review_attempts;
        let failing = failing_review_node("FAIL", 2);
        for attempt in 1..=max {
            let (ctx, next) = review_once(&failing, &state, &tasks[2]).await;
            state = next;
            let expected = if attempt < max {
                "IncrementAttemptNode"
            } else {
                "WrapUpNode"
            };
            assert_eq!(
                ReviewRouterNode.route(&ctx),
                Some(expected.to_string()),
                "review attempt {attempt} of {max} on task 3 routed wrongly"
            );
        }
    }

    /// The independence constraint `SDLCTelemetry::review_attempts` argues
    /// for, restated over the per-task bound: a task that already burned
    /// its test retries (`attempt_count`/`total_attempts` at 2) still
    /// arrives at review with a FULL review budget, and a review pass
    /// advances neither attempt counter.
    #[tokio::test]
    async fn review_budget_is_independent_of_the_attempt_counters() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.attempt_count = 2;
        let mut state = state_with_tasks(vec![task.clone()]);
        state.telemetry.total_attempts = 2;

        let failing = failing_review_node("FAIL", 2);
        let (ctx, next) = review_once(&failing, &state, &task).await;

        assert_eq!(
            ReviewRouterNode.route(&ctx),
            Some("IncrementAttemptNode".to_string()),
            "burned test retries must not reduce the review budget"
        );
        assert_eq!(next.tasks[0].review_attempt_count, 1);
        // Untouched by the review pass.
        assert_eq!(next.tasks[0].attempt_count, 2);
        assert_eq!(next.telemetry.total_attempts, 2);
        // And the run-level accounting counter still counts the verdict.
        assert_eq!(next.telemetry.review_attempts, 1);
    }

    /// The genuine bound is untouched: ONE task burning minor non-PASS
    /// verdicts is still stopped at exactly `max_review_attempts`.
    #[tokio::test]
    async fn one_task_burning_non_pass_verdicts_is_still_bounded() {
        let task = SDLCTask::new(1, "One", "d1");
        let mut state = state_with_tasks(vec![task.clone()]);

        let max = SdlcPolicy::default().max_review_attempts;
        let failing = failing_review_node("FAIL", 2);
        for attempt in 1..=max {
            let (ctx, next) = review_once(&failing, &state, &task).await;
            state = next;
            let expected = if attempt < max {
                "IncrementAttemptNode"
            } else {
                "WrapUpNode"
            };
            assert_eq!(
                ReviewRouterNode.route(&ctx),
                Some(expected.to_string()),
                "review attempt {attempt} of {max} routed wrongly"
            );
        }
    }

    #[test]
    fn latest_state_prefers_consolidated_review_node_over_a_stale_load() {
        // `ConsolidatedReviewNode`'s bump must be visible to `latest_state`
        // even when `LoadTaskStateNode` (the initial, now-stale load) is
        // also present in `ctx.nodes` — proving the new candidate and its
        // logical-clock comparison are wired in correctly.
        let task = SDLCTask::new(1, "One", "d1");
        let mut load_state = state_with_tasks(vec![task.clone()]);
        load_state.telemetry.review_attempts = 0;
        let mut ctx = ctx_with_state(&load_state);

        let mut reviewed_state = load_state.clone();
        reviewed_state.telemetry.review_attempts = 1;
        ctx.nodes.insert(
            "ConsolidatedReviewNode".to_string(),
            json!({
                "verdict": "FAIL",
                "summary": "two issues",
                "issues": ["a", "b"],
                "state": reviewed_state,
            }),
        );

        let resolved = latest_state(&ctx).expect("latest_state should resolve");
        assert_eq!(resolved.telemetry.review_attempts, 1);
    }

    #[test]
    fn latest_state_prefers_increment_attempt_node_over_a_stale_review() {
        // The reverse ordering: an `IncrementAttemptNode` write that landed
        // AFTER an earlier `ConsolidatedReviewNode` bump (i.e. the review's
        // minor-issue back-edge already advanced the run) must win, proving
        // the logical clock sums both counters rather than only comparing
        // `review_attempts`.
        let task = SDLCTask::new(1, "One", "d1");
        let mut reviewed_state = state_with_tasks(vec![task.clone()]);
        reviewed_state.telemetry.review_attempts = 1;
        let mut ctx = ctx_with_state(&reviewed_state);
        ctx.nodes.insert(
            "ConsolidatedReviewNode".to_string(),
            json!({
                "verdict": "FAIL",
                "summary": "two issues",
                "issues": ["a", "b"],
                "state": reviewed_state,
            }),
        );

        let mut incremented_state = reviewed_state.clone();
        incremented_state.telemetry.total_attempts = 1;
        incremented_state.tasks[0].attempt_count = 1;
        ctx.nodes.insert(
            "IncrementAttemptNode".to_string(),
            serde_json::to_value(&incremented_state).unwrap(),
        );

        let resolved = latest_state(&ctx).expect("latest_state should resolve");
        assert_eq!(resolved.telemetry.total_attempts, 1);
        assert_eq!(resolved.telemetry.review_attempts, 1);
    }

    // --- Review-gate policy consumption (EN.3.C task 4) ---------------------

    /// A small `git diff --numstat` stub: one changed file, one added +
    /// one deleted line — well under the default
    /// `review_skip_max_files`/`review_skip_max_diff_lines` thresholds.
    fn trivial_diff_runner() -> CommandRunner {
        Arc::new(|_program, args, _cwd| {
            // The classifier intent-adds first so untracked files are
            // counted; only the numstat call carries the diff base.
            if args != ["add", "-N", "-A"] {
                assert_eq!(args, ["diff", "--numstat", "HEAD"]);
            }
            Ok(CommandOutput {
                status: 0,
                stdout: "1\t1\tsrc/lib.rs\n".to_string(),
                stderr: String::new(),
            })
        })
    }

    /// A large `git diff --numstat` stub: two files with a combined diff
    /// line count well past the default thresholds.
    fn non_trivial_diff_runner() -> CommandRunner {
        Arc::new(|_program, _args, _cwd| {
            Ok(CommandOutput {
                status: 0,
                stdout: "50\t50\tsrc/a.rs\n50\t50\tsrc/b.rs\n".to_string(),
                stderr: String::new(),
            })
        })
    }

    fn ctx_with_worktree(mut ctx: TaskContext) -> TaskContext {
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": "." }),
        );
        ctx
    }

    /// A trivial green task (small diff, under the default thresholds)
    /// classifies `trivial: true` and, under `TrivialSkip`, the router
    /// skips `ConsolidatedReviewNode` and goes straight to
    /// `UpdateTaskStatusNode`.
    #[tokio::test]
    async fn trivial_green_task_skips_review_in_trivial_skip_mode() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 3;
        task.attempt_count = 0;

        let policy = SdlcPolicy {
            review_mode: ReviewMode::TrivialSkip,
            ..SdlcPolicy::default()
        };

        let ctx = ctx_with_worktree(ctx_with_test_result(true, &task));
        let ctx = ctx_with_policy(ctx, &policy);

        let node = TriageTaskNode::new()
            .with_transport(panicking_transport())
            .with_runner(trivial_diff_runner());
        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["TriageTaskNode"]["verdict"], "PASS");
        assert_eq!(out.nodes["TriageTaskNode"]["trivial"], true);

        let router = TriageRouterNode;
        assert_eq!(router.route(&out), Some("UpdateTaskStatusNode".to_string()));
    }

    /// `classify_trivial` counts the WORKING TREE against `HEAD`, after an
    /// intent-to-add pass, and ignores any `base_sha` stamp. Before this,
    /// it diffed the always-empty `<base_sha>..HEAD` commit range, so every
    /// task classified as trivial (0 files / 0 lines) and `TrivialSkip`
    /// skipped the review unconditionally.
    #[tokio::test]
    async fn trivial_classification_diffs_working_tree_against_head() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 3;
        task.attempt_count = 0;

        let policy = SdlcPolicy {
            review_mode: ReviewMode::TrivialSkip,
            ..SdlcPolicy::default()
        };

        let ctx = ctx_with_worktree(ctx_with_test_result(true, &task));
        let ctx = ctx_with_policy(ctx, &policy);

        let calls: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
        let calls_clone = calls.clone();
        let runner: CommandRunner = Arc::new(move |_program, args, _cwd| {
            calls_clone
                .lock()
                .unwrap()
                .push(args.iter().map(|s| (*s).to_string()).collect());
            Ok(CommandOutput {
                status: 0,
                stdout: "1\t1\tsrc/lib.rs\n".to_string(),
                stderr: String::new(),
            })
        });

        let node = TriageTaskNode::new()
            .with_transport(panicking_transport())
            .with_runner(runner);
        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["TriageTaskNode"]["trivial"], true);

        let recorded = calls.lock().unwrap();
        assert_eq!(
            *recorded,
            vec![
                vec!["add".to_string(), "-N".to_string(), "-A".to_string()],
                vec![
                    "diff".to_string(),
                    "--numstat".to_string(),
                    "HEAD".to_string()
                ],
            ],
            "intent-to-add must precede the numstat, and the base must be HEAD"
        );
    }

    /// A real-sized working-tree change (120 added + 3 deleted lines in one
    /// file) is counted and classified NON-trivial — the counting path the
    /// always-empty commit range used to short-circuit.
    #[tokio::test]
    async fn trivial_classification_counts_real_changed_lines_as_non_trivial() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 3;
        task.attempt_count = 0;

        let policy = SdlcPolicy {
            review_mode: ReviewMode::TrivialSkip,
            ..SdlcPolicy::default()
        };
        let ctx = ctx_with_worktree(ctx_with_test_result(true, &task));
        let ctx = ctx_with_policy(ctx, &policy);

        let runner: CommandRunner = Arc::new(|_program, args, _cwd| {
            let stdout = if args == ["add", "-N", "-A"] {
                String::new()
            } else {
                "120\t3\tsrc/lib.rs\n".to_string()
            };
            Ok(CommandOutput {
                status: 0,
                stdout,
                stderr: String::new(),
            })
        });

        let node = TriageTaskNode::new()
            .with_transport(panicking_transport())
            .with_runner(runner);
        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["TriageTaskNode"]["trivial"], false);
    }

    /// A non-trivial green task (diff over the thresholds) classifies
    /// `trivial: false` and, even under `TrivialSkip`, still routes to
    /// `ConsolidatedReviewNode`.
    #[tokio::test]
    async fn non_trivial_task_still_routes_to_review_in_trivial_skip_mode() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 3;
        task.attempt_count = 0;

        let policy = SdlcPolicy {
            review_mode: ReviewMode::TrivialSkip,
            ..SdlcPolicy::default()
        };

        let ctx = ctx_with_worktree(ctx_with_test_result(true, &task));
        let ctx = ctx_with_policy(ctx, &policy);

        let node = TriageTaskNode::new()
            .with_transport(panicking_transport())
            .with_runner(non_trivial_diff_runner());
        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["TriageTaskNode"]["verdict"], "PASS");
        assert_eq!(out.nodes["TriageTaskNode"]["trivial"], false);

        let router = TriageRouterNode;
        assert_eq!(
            router.route(&out),
            Some("ConsolidatedReviewNode".to_string())
        );
    }

    /// A failing task's `RETRYABLE` verdict is unaffected by `review_mode`:
    /// it always routes through `IncrementAttemptNode`, never straight to
    /// review or `UpdateTaskStatusNode`.
    #[tokio::test]
    async fn failing_task_still_routes_through_retry_regardless_of_review_mode() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 3;
        task.attempt_count = 0;

        let policy = SdlcPolicy {
            review_mode: ReviewMode::TrivialSkip,
            ..SdlcPolicy::default()
        };

        let ctx = ctx_with_worktree(ctx_with_test_result(false, &task));
        let ctx = ctx_with_policy(ctx, &policy);

        let node = TriageTaskNode::new().with_transport(panicking_transport());
        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["TriageTaskNode"]["verdict"], "RETRYABLE");

        let router = TriageRouterNode;
        assert_eq!(router.route(&out), Some("IncrementAttemptNode".to_string()));
    }

    /// `per_task` (the built-in default `review_mode`) is unchanged: even a
    /// trivial green task still routes to `ConsolidatedReviewNode` — no
    /// policy stamped at all reproduces today's behavior byte-for-byte.
    #[tokio::test]
    async fn per_task_default_routes_trivial_task_to_review() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 3;
        task.attempt_count = 0;

        // No RESOLVED_POLICY_IDENTITY stamped -> falls back to the built-in
        // default, which is `per_task`.
        let ctx = ctx_with_worktree(ctx_with_test_result(true, &task));

        let node = TriageTaskNode::new()
            .with_transport(panicking_transport())
            .with_runner(trivial_diff_runner());
        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["TriageTaskNode"]["trivial"], true);

        let router = TriageRouterNode;
        assert_eq!(
            router.route(&out),
            Some("ConsolidatedReviewNode".to_string())
        );
    }

    /// `end_only` collapses per-task review away entirely: a `PASS` verdict
    /// routes straight to `UpdateTaskStatusNode` regardless of triviality.
    #[tokio::test]
    async fn end_only_mode_skips_per_task_review_regardless_of_triviality() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 3;
        task.attempt_count = 0;

        let policy = SdlcPolicy {
            review_mode: ReviewMode::EndOnly,
            ..SdlcPolicy::default()
        };

        let ctx = ctx_with_worktree(ctx_with_test_result(true, &task));
        let ctx = ctx_with_policy(ctx, &policy);

        let node = TriageTaskNode::new()
            .with_transport(panicking_transport())
            .with_runner(non_trivial_diff_runner());
        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["TriageTaskNode"]["trivial"], false);

        let router = TriageRouterNode;
        assert_eq!(router.route(&out), Some("UpdateTaskStatusNode".to_string()));
    }

    /// `classify_trivial` falls back to non-trivial (`false`) when the
    /// worktree/`git diff` invocation is unavailable, rather than erroring
    /// `TriageTaskNode::process`.
    #[tokio::test]
    async fn trivial_classification_defaults_false_without_worktree() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 3;
        task.attempt_count = 0;

        // No `SetupWorktreeNode` output stamped -> `worktree_path` fails ->
        // `classify_trivial` defensively returns `false`.
        let ctx = ctx_with_test_result(true, &task);

        let node = TriageTaskNode::new().with_transport(panicking_transport());
        let out = node
            .process(ctx)
            .await
            .expect("process should succeed even without a worktree");
        assert_eq!(out.nodes["TriageTaskNode"]["verdict"], "PASS");
        assert_eq!(out.nodes["TriageTaskNode"]["trivial"], false);
    }

    // --- SaveStateNode -------------------------------------------------------

    #[tokio::test]
    async fn save_state_writes_file_and_commits() {
        let worktree = temp_worktree();
        let state = state_with_tasks(vec![SDLCTask::new(1, "One", "d1")]);
        let mut ctx = ctx_with_state(&state);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );

        let calls: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
        let calls_clone = calls.clone();
        let runner: CommandRunner = Arc::new(move |_program, args, _cwd| {
            calls_clone
                .lock()
                .unwrap()
                .push(args.iter().map(|s| (*s).to_string()).collect());
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });

        let node = SaveStateNode::new().with_runner(runner);
        let out = node.process(ctx).await.expect("process should succeed");

        let saved_to = out.nodes["SaveStateNode"]["saved_to"].as_str().unwrap();
        assert!(Path::new(saved_to).exists());
        assert!(saved_to.ends_with("sdlc/sdlc-flow-state.json"));

        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 2);
        // `add -A`, not `add <state_path>`: the per-task commit carries the
        // task's CODE as well as its state file.
        assert_eq!(recorded[0], vec!["add".to_string(), "-A".to_string()]);
        assert_eq!(recorded[1][0], "commit");
        // No TaskQueueRouterNode stamp in this ctx -> fallback message.
        assert_eq!(recorded[1][2], SAVE_STATE_FALLBACK_MESSAGE);
        drop(recorded);

        let content = std::fs::read_to_string(saved_to).unwrap();
        let value: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(value["tasks"].is_object());
        assert_eq!(value["status"], json!("running"));
        assert!(value["review"].is_null());
        assert!(value["docs"].is_null());
        assert!(value["pr"].is_null());
        assert!(value["bail_reason"].is_null());
        assert!(value["started_at"].as_str().is_some());
        assert!(value["updated_at"].as_str().is_some());
    }

    /// With `TaskQueueRouterNode`'s stamp present, the per-task commit
    /// message carries the task id — one commit per completed task, greppable
    /// in `git log`.
    #[tokio::test]
    async fn save_state_commit_message_carries_the_current_task_id() {
        let worktree = temp_worktree();
        let state = state_with_tasks(vec![SDLCTask::new(7, "Widen the commit", "d7")]);
        let mut ctx = ctx_with_state(&state);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );
        ctx.nodes.insert(
            "TaskQueueRouterNode".to_string(),
            json!({ "current_task_id": 7, "title": "Widen the commit" }),
        );

        let calls: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
        let calls_clone = calls.clone();
        let runner: CommandRunner = Arc::new(move |_program, args, _cwd| {
            calls_clone
                .lock()
                .unwrap()
                .push(args.iter().map(|s| (*s).to_string()).collect());
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });

        SaveStateNode::new()
            .with_runner(runner)
            .process(ctx)
            .await
            .expect("process should succeed");

        let recorded = calls.lock().unwrap();
        assert_eq!(recorded[0], vec!["add".to_string(), "-A".to_string()]);
        assert_eq!(recorded[1][0], "commit");
        assert_eq!(recorded[1][2], "feat(sdlc): 7 — Widen the commit");
    }

    /// A router stamp missing `current_task_id` falls back rather than
    /// emitting a half-built message.
    #[test]
    fn save_state_commit_message_falls_back_without_a_router_stamp() {
        let state = state_with_tasks(vec![SDLCTask::new(1, "One", "d1")]);
        let bare = ctx_with_state(&state);
        assert_eq!(
            save_state_commit_message(&bare),
            SAVE_STATE_FALLBACK_MESSAGE
        );

        let mut partial = ctx_with_state(&state);
        partial
            .nodes
            .insert("TaskQueueRouterNode".to_string(), json!({ "title": "One" }));
        assert_eq!(
            save_state_commit_message(&partial),
            SAVE_STATE_FALLBACK_MESSAGE
        );
    }

    #[tokio::test]
    async fn save_state_preserves_started_at_across_resumed_saves() {
        let worktree = temp_worktree();
        let state = state_with_tasks(vec![SDLCTask::new(1, "One", "d1")]);
        let mut ctx = ctx_with_state(&state);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy(), "branch_name": "sdlc/x" }),
        );

        let runner: CommandRunner = Arc::new(|_program, _args, _cwd| {
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });

        let node = SaveStateNode::new().with_runner(runner.clone());
        let out = node.process(ctx.clone()).await.expect("first save");
        let saved_to = out.nodes["SaveStateNode"]["saved_to"]
            .as_str()
            .unwrap()
            .to_string();
        let first_started_at = {
            let content = std::fs::read_to_string(&saved_to).unwrap();
            let value: serde_json::Value = serde_json::from_str(&content).unwrap();
            value["started_at"].as_str().unwrap().to_string()
        };

        // Simulate a resumed second save (e.g. a subsequent task's
        // `SaveStateNode` run) — `started_at` must not change, since it is
        // read back from the file `SaveStateNode` just wrote above rather
        // than recomputed.
        let node2 = SaveStateNode::new().with_runner(runner);
        let out2 = node2.process(ctx).await.expect("second save");
        let saved_to2 = out2.nodes["SaveStateNode"]["saved_to"].as_str().unwrap();
        let content = std::fs::read_to_string(saved_to2).unwrap();
        let value: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(value["started_at"].as_str().unwrap(), first_started_at);
    }

    #[tokio::test]
    async fn save_state_stamps_run_id_from_context_metadata() {
        let worktree = temp_worktree();
        let state = state_with_tasks(vec![SDLCTask::new(1, "One", "d1")]);
        let mut ctx = ctx_with_state(&state);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );
        let run_id = uuid::Uuid::new_v4();
        ctx.metadata = json!({ crate::RUN_ID_METADATA_KEY: run_id.to_string() });

        let runner: CommandRunner = Arc::new(|_program, _args, _cwd| {
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });

        let node = SaveStateNode::new().with_runner(runner);
        let out = node.process(ctx).await.expect("process should succeed");
        let saved_to = out.nodes["SaveStateNode"]["saved_to"].as_str().unwrap();

        let content = std::fs::read_to_string(saved_to).unwrap();
        let value: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(value["run_id"], json!(run_id.to_string()));
    }

    #[tokio::test]
    async fn save_state_writes_null_run_id_for_empty_metadata() {
        let worktree = temp_worktree();
        let state = state_with_tasks(vec![SDLCTask::new(1, "One", "d1")]);
        let mut ctx = ctx_with_state(&state);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );
        // Today-path: no `RunOptions::run_id` was ever stamped, so
        // `ctx.metadata` is the empty object `Workflow::seed_context`
        // always builds.
        assert_eq!(ctx.metadata, json!({}));

        let runner: CommandRunner = Arc::new(|_program, _args, _cwd| {
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });

        let node = SaveStateNode::new().with_runner(runner);
        let out = node.process(ctx).await.expect("process should succeed");
        let saved_to = out.nodes["SaveStateNode"]["saved_to"].as_str().unwrap();

        let content = std::fs::read_to_string(saved_to).unwrap();
        let value: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(value["run_id"].is_null());
        // Otherwise byte-compatible with the pre-task-3 output shape.
        assert!(value["tasks"].is_object());
        assert_eq!(value["status"], json!("running"));
        assert!(value["review"].is_null());
        assert!(value["docs"].is_null());
        assert!(value["pr"].is_null());
        assert!(value["bail_reason"].is_null());
    }

    // EN.ticket.a-task-marked-done-must-have-actually-committed task 1: drive
    // `SaveStateNode` through the three `git commit` outcomes directly (no
    // `is_noop_commit` re-implementation at the test level — the runner
    // stubs the real exit code/stderr `commit_all` would see). Case (i) is
    // RED on purpose: `SaveStateNode::process` today always returns `Ok`
    // regardless of whether the commit genuinely failed, so a task whose
    // commit failed is still recorded done. Task 2 closes this by gating
    // completion on the commit result.

    /// (i) A genuine `git commit` failure (a real git error, not the
    /// ordinary "nothing to commit" no-op) must NOT let the task be
    /// recorded done — `SaveStateNode::process` must surface it as an
    /// `Err`, the same way this node's siblings surface a failure.
    #[tokio::test]
    async fn save_state_fails_the_task_when_commit_genuinely_fails() {
        let worktree = temp_worktree();
        let state = state_with_tasks(vec![SDLCTask::new(1, "One", "d1")]);
        let mut ctx = ctx_with_state(&state);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );

        let runner: CommandRunner = Arc::new(|_program, args, _cwd| {
            if args.first() == Some(&"commit") {
                Ok(CommandOutput {
                    status: 1,
                    stdout: String::new(),
                    stderr: "fatal: unable to write new index file".to_string(),
                })
            } else {
                Ok(CommandOutput {
                    status: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
        });

        let node = SaveStateNode::new().with_runner(runner);
        let result = node.process(ctx).await;

        // RED today: this currently returns `Ok`, so the task is recorded
        // done even though `git commit` genuinely failed.
        assert!(
            result.is_err(),
            "expected SaveStateNode::process to fail when git commit genuinely \
             fails, but it succeeded: {result:?}"
        );
    }

    /// (ii) `is_noop_commit`'s legitimate "nothing to commit" case must
    /// still complete the task normally — this is the distinction that
    /// must not collapse. Already passes today, since `commit_all`'s
    /// `false` return (noop or failure alike) is currently never consulted.
    #[tokio::test]
    async fn save_state_completes_normally_on_noop_commit() {
        let worktree = temp_worktree();
        let state = state_with_tasks(vec![SDLCTask::new(1, "One", "d1")]);
        let mut ctx = ctx_with_state(&state);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );

        let runner: CommandRunner = Arc::new(|_program, args, _cwd| {
            if args.first() == Some(&"commit") {
                Ok(CommandOutput {
                    status: 1,
                    stdout: String::new(),
                    stderr: "nothing to commit, working tree clean".to_string(),
                })
            } else {
                Ok(CommandOutput {
                    status: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
        });

        let node = SaveStateNode::new().with_runner(runner);
        let out = node
            .process(ctx)
            .await
            .expect("a no-op commit must still complete the task normally");
        assert!(out.nodes["SaveStateNode"]["saved_to"].as_str().is_some());
    }

    /// (iii) An ordinary successful `git commit` is unaffected.
    #[tokio::test]
    async fn save_state_completes_normally_when_commit_succeeds() {
        let worktree = temp_worktree();
        let state = state_with_tasks(vec![SDLCTask::new(1, "One", "d1")]);
        let mut ctx = ctx_with_state(&state);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );

        let runner: CommandRunner = Arc::new(|_program, _args, _cwd| {
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });

        let node = SaveStateNode::new().with_runner(runner);
        let out = node
            .process(ctx)
            .await
            .expect("a successful commit must complete the task normally");
        assert!(out.nodes["SaveStateNode"]["saved_to"].as_str().is_some());
    }

    // --- select_task_checks --------------------------------------------------

    fn cmd_check(name: &str, command: &str) -> serde_json::Value {
        json!({ "name": name, "kind": "command", "command": command, "gates": true })
    }

    fn cmd_check_with_fast(name: &str, command: &str, fast_command: &str) -> serde_json::Value {
        json!({
            "name": name,
            "kind": "command",
            "command": command,
            "fastCommand": fast_command,
            "gates": true,
        })
    }

    #[test]
    fn select_task_checks_full_depth_keeps_command_even_with_fast_command() {
        let checks = vec![cmd_check_with_fast(
            "test",
            "cargo test --workspace",
            "cargo test --lib",
        )];
        let (selected, selection) = select_task_checks(&checks, &[], TestDepth::Full, true);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0]["command"], json!("cargo test --workspace"));
        assert_eq!(selection.source, "harness");
        assert_eq!(selection.depth, TestDepth::Full);
        assert!(selection.excluded.is_empty());
    }

    #[test]
    fn select_task_checks_fast_depth_substitutes_fast_command() {
        let checks = vec![cmd_check_with_fast(
            "test",
            "cargo test --workspace",
            "cargo test --lib",
        )];
        let (selected, selection) = select_task_checks(&checks, &[], TestDepth::Fast, true);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0]["command"], json!("cargo test --lib"));
        // No other field is disturbed by the substitution.
        assert_eq!(selected[0]["fastCommand"], json!("cargo test --lib"));
        assert_eq!(selected[0]["gates"], json!(true));
        assert_eq!(selection.source, "harness");
        assert_eq!(selection.depth, TestDepth::Fast);
    }

    #[test]
    fn select_task_checks_fast_depth_falls_back_to_command_when_no_fast_command() {
        let checks = vec![cmd_check("fmt", "cargo fmt --check")];
        let (selected, selection) = select_task_checks(&checks, &[], TestDepth::Fast, true);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0]["command"], json!("cargo fmt --check"));
        assert_eq!(selection.source, "harness");
    }

    #[test]
    fn select_task_checks_excludes_per_task_false_at_both_depths() {
        let mut build = cmd_check("build", "cargo build --release");
        build["perTask"] = json!(false);
        let checks = vec![build];

        for depth in [TestDepth::Full, TestDepth::Fast] {
            let (selected, selection) = select_task_checks(&checks, &[], depth, true);
            assert!(
                selected.is_empty(),
                "depth {depth:?} should exclude perTask:false when apply_per_task_filter is true"
            );
            assert_eq!(selection.excluded, vec!["build".to_string()]);
        }
    }

    /// `EN.3.E` acceptance criterion: `apply_per_task_filter = false` — the
    /// `FinalValidationNode` branch — keeps a `"perTask": false` check
    /// instead of dropping it, at both depths.
    #[test]
    fn select_task_checks_keeps_per_task_false_when_filter_disabled() {
        let mut build = cmd_check("build", "cargo build --release");
        build["perTask"] = json!(false);
        let checks = vec![build];

        for depth in [TestDepth::Full, TestDepth::Fast] {
            let (selected, selection) = select_task_checks(&checks, &[], depth, false);
            assert_eq!(
                selected.len(),
                1,
                "depth {depth:?} should keep perTask:false when apply_per_task_filter is false"
            );
            assert_eq!(selected[0]["name"], json!("build"));
            assert!(selection.excluded.is_empty());
        }
    }

    #[test]
    fn select_task_checks_excludes_enabled_false() {
        let mut fmt = cmd_check("fmt", "cargo fmt --check");
        fmt["enabled"] = json!(false);
        let checks = vec![fmt];

        let (selected, selection) = select_task_checks(&checks, &[], TestDepth::Full, true);
        assert!(selected.is_empty());
        assert_eq!(selection.excluded, vec!["fmt".to_string()]);
    }

    #[test]
    fn select_task_checks_task_validation_commands_replaces_everything() {
        let harness_checks = vec![cmd_check("fmt", "cargo fmt --check")];
        let task_commands = vec![
            "test -f docs/foo.md".to_string(),
            "grep -q bar docs/foo.md".to_string(),
        ];

        for depth in [TestDepth::Full, TestDepth::Fast] {
            let (selected, selection) =
                select_task_checks(&harness_checks, &task_commands, depth, true);
            assert_eq!(
                selected,
                vec![
                    json!({
                        "name": "task-validation-1",
                        "kind": "command",
                        "command": "test -f docs/foo.md",
                        "gates": true,
                    }),
                    json!({
                        "name": "task-validation-2",
                        "kind": "command",
                        "command": "grep -q bar docs/foo.md",
                        "gates": true,
                    }),
                ],
                "depth {depth:?} should not change the synthesized shape"
            );
            assert_eq!(selection.source, "task_validation_commands");
            assert!(selection.excluded.is_empty());
        }
    }

    #[test]
    fn select_task_checks_empty_task_validation_commands_falls_through_to_harness() {
        let harness_checks = vec![cmd_check("fmt", "cargo fmt --check")];
        let (selected, selection) = select_task_checks(&harness_checks, &[], TestDepth::Full, true);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0]["name"], json!("fmt"));
        assert_eq!(selection.source, "harness");
    }

    #[test]
    fn select_task_checks_source_literal_matches_branch() {
        let harness_checks = vec![cmd_check("fmt", "cargo fmt --check")];
        let (_, harness_selection) =
            select_task_checks(&harness_checks, &[], TestDepth::Full, true);
        assert_eq!(harness_selection.source, "harness");

        let (_, override_selection) = select_task_checks(
            &harness_checks,
            &["echo hi".to_string()],
            TestDepth::Full,
            true,
        );
        assert_eq!(override_selection.source, "task_validation_commands");
    }

    // --- TestTaskNode::process x select_task_checks wiring (task 4) --------

    /// A recording [`CommandRunner`] that always succeeds and records every
    /// `sh -c <command>` invocation's `<command>` string, in order.
    fn recording_command_runner() -> (CommandRunner, Arc<Mutex<Vec<String>>>) {
        let recorded: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded_clone = recorded.clone();
        let runner: CommandRunner = Arc::new(move |program, args, _cwd| {
            if let Some(probe) = write_verification_probe(program, args) {
                return Ok(probe);
            }
            if let Some(command) = args.get(1) {
                recorded_clone.lock().unwrap().push((*command).to_string());
            }
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });
        (runner, recorded)
    }

    /// A [`ctx_with_current_task`]-based ctx that also carries a
    /// `SetupWorktreeNode` output pointing at `worktree`, so `TestTaskNode`
    /// can resolve both the current task (for `validation_commands`) and the
    /// worktree path (for `planning/harness.json`).
    fn ctx_with_current_task_and_worktree(
        state: &SDLCState,
        task: &SDLCTask,
        worktree: &Path,
    ) -> TaskContext {
        let mut ctx = ctx_with_current_task(state, task);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );
        ctx
    }

    #[tokio::test]
    async fn test_task_full_depth_runs_command_even_with_fast_command_present() {
        let worktree = temp_worktree();
        write_harness(
            &worktree,
            json!([cmd_check_with_fast(
                "test",
                "cargo nextest run --workspace",
                "cargo nextest run --lib --workspace"
            )]),
        );
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task_and_worktree(&state, &task, &worktree);

        let (runner, recorded) = recording_command_runner();
        let node = TestTaskNode::new().with_runner(runner);
        let out = node.process(ctx).await.expect("process should succeed");

        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
        assert_eq!(
            *recorded.lock().unwrap(),
            vec!["cargo nextest run --workspace"]
        );
        assert_eq!(out.nodes["TestTaskNode"]["test_depth"], json!("full"));
        assert_eq!(out.nodes["TestTaskNode"]["check_source"], json!("harness"));
    }

    #[tokio::test]
    async fn test_task_fast_depth_substitutes_fast_command() {
        let worktree = temp_worktree();
        write_harness(
            &worktree,
            json!([cmd_check_with_fast(
                "test",
                "cargo nextest run --workspace",
                "cargo nextest run --lib --workspace"
            )]),
        );
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task_and_worktree(&state, &task, &worktree);
        let ctx = ctx_with_policy(
            ctx,
            &SdlcPolicy {
                test_depth: TestDepth::Fast,
                ..SdlcPolicy::default()
            },
        );

        let (runner, recorded) = recording_command_runner();
        let node = TestTaskNode::new().with_runner(runner);
        let out = node.process(ctx).await.expect("process should succeed");

        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
        assert_eq!(
            *recorded.lock().unwrap(),
            vec!["cargo nextest run --lib --workspace"]
        );
        assert_eq!(out.nodes["TestTaskNode"]["test_depth"], json!("fast"));
    }

    #[tokio::test]
    async fn test_task_uses_task_validation_commands_over_harness() {
        let worktree = temp_worktree();
        write_harness(&worktree, json!([cmd_check("fmt", "cargo fmt --check")]));
        let mut task = SDLCTask::new(1, "One", "d1");
        task.validation_commands = vec!["test -f docs/foo.md".to_string()];
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task_and_worktree(&state, &task, &worktree);

        let (runner, recorded) = recording_command_runner();
        let node = TestTaskNode::new().with_runner(runner);
        let out = node.process(ctx).await.expect("process should succeed");

        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
        assert_eq!(*recorded.lock().unwrap(), vec!["test -f docs/foo.md"]);
        assert_eq!(
            out.nodes["TestTaskNode"]["check_source"],
            json!("task_validation_commands")
        );
        assert!(out.nodes["TestTaskNode"]["excluded_checks"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn test_task_result_stamps_test_depth_check_source_excluded_checks() {
        let worktree = temp_worktree();
        let mut build = cmd_check("build", "cargo build --release");
        build["perTask"] = json!(false);
        write_harness(
            &worktree,
            json!([cmd_check("fmt", "cargo fmt --check"), build]),
        );
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task_and_worktree(&state, &task, &worktree);

        let (runner, _recorded) = recording_command_runner();
        let node = TestTaskNode::new().with_runner(runner);
        let out = node.process(ctx).await.expect("process should succeed");

        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
        assert_eq!(out.nodes["TestTaskNode"]["test_depth"], json!("full"));
        assert_eq!(out.nodes["TestTaskNode"]["check_source"], json!("harness"));
        assert_eq!(
            out.nodes["TestTaskNode"]["excluded_checks"],
            json!(["build"])
        );
    }

    #[tokio::test]
    async fn test_task_no_harness_and_no_validation_commands_is_gating_failure() {
        let worktree = temp_worktree();
        // No harness.json written — `temp_worktree` only creates the
        // `planning/` directory, not the file.
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task_and_worktree(&state, &task, &worktree);

        let node = TestTaskNode::new();
        let out = node.process(ctx).await.expect("process should succeed");

        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], false);
        let results = out.nodes["TestTaskNode"]["check_results"]
            .as_array()
            .unwrap();
        assert!(results
            .iter()
            .any(|r| r["name"] == json!("harness-missing")));
        assert!(out.nodes["TestTaskNode"]["failure_summary"]
            .as_str()
            .unwrap()
            .contains("harness-missing"));
    }

    #[tokio::test]
    async fn test_task_no_harness_but_with_validation_commands_runs_them_no_harness_missing() {
        let worktree = temp_worktree();
        // No harness.json written.
        let mut task = SDLCTask::new(1, "One", "d1");
        task.validation_commands = vec!["true".to_string()];
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task_and_worktree(&state, &task, &worktree);

        let (runner, recorded) = recording_command_runner();
        let node = TestTaskNode::new().with_runner(runner);
        let out = node.process(ctx).await.expect("process should succeed");

        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
        assert_eq!(*recorded.lock().unwrap(), vec!["true"]);
        let results = out.nodes["TestTaskNode"]["check_results"]
            .as_array()
            .unwrap();
        assert!(!results
            .iter()
            .any(|r| r["name"] == json!("harness-missing")));
    }
}
