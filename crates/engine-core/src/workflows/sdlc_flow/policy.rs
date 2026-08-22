//! The resolved `SdlcPolicy` — runtime-configurable cost/time/quality levers
//! for the SDLC Flow (levers #1–#3 from
//! `planning/sdlc-token-time-economics/notes.md`), plus a `PartialPolicy`
//! mirror used by the two override layers and a `resolve` function that
//! merges all three layers into one concrete policy.
//!
//! **Resolution — four layers, high-to-low precedence:** per-run
//! `SdlcFlowEvent` override, then a named `profile:` bundle, then
//! `planning/harness.json` `sdlc.policy` defaults, then built-in defaults.
//! The built-in [`SdlcPolicy::default`] MUST reproduce today's
//! (pre-EN.3.C) behavior exactly — turning any knob away from that
//! baseline is opt-in.
//!
//! **EN.4.0:** `SdlcPolicy` now implements the generic `crate::policy::Policy`
//! trait, and this module's `resolve` delegates to the generic
//! `crate::policy::resolve::resolve`. `OutputVerbosity`/`ModelTier`/
//! `LocalConfig` are re-exported from `crate::policy::tier` (byte-identical
//! serde reprs to the pre-hoist types) rather than redefined here.
//!
//! **EN.5.D task 2:** the hand-written `merge_opt`/`merge_local`/
//! `apply_override` trio is gone — `local: LocalConfig` merges via the
//! shared `crate::policy::Overlay` surface (`policy::overlay`), and the
//! top-level field merge is inlined directly into `Policy::apply` rather
//! than a private free function.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub use crate::policy::tier::{LocalConfig, ModelTier, OutputVerbosity};
pub use crate::policy::PartialLocalConfig;
use crate::policy::{merge_opt, Overlay};

/// How the review gate is applied across a run's tasks (lever #3a).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewMode {
    /// Today's behavior: every task routes to `ConsolidatedReviewNode`.
    #[default]
    PerTask,
    /// A trivial green task (small diff, first-pass green) skips per-task
    /// review; a non-trivial task still routes to review.
    TrivialSkip,
    /// Per-task review is collapsed into a single end-of-run review: the
    /// drain branch's `EndReviewNode` (`end_review.rs`) makes exactly one
    /// review call over the whole run's accumulated diff against the
    /// complete Acceptance Criteria, instead of `ConsolidatedReviewNode`
    /// reviewing each task's diff separately. It is single-pass — a FAIL
    /// verdict blocks the run with a named reason rather than looping a
    /// fix pass the way JS's end review does. Proven by
    /// `end_only_full_run_makes_exactly_one_review_call_with_full_ac_and_multi_task_diff`
    /// and `end_only_fail_verdict_routes_to_wrap_up_with_blocked_status_and_bail_reason`
    /// in `crates/engine-core/tests/it/sdlc_flow_end_review_e2e.rs`.
    EndOnly,
}

/// How much of a task's own check suite `TestTaskNode` runs at a given
/// per-task tripwire (standing rule 6's cost/latency knob for EN.3.D).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestDepth {
    /// Today's Rust behavior (the behavior-stable built-in default): run
    /// every selected check's `command` verbatim.
    #[default]
    Full,
    /// Substitute a check's `fastCommand` for its `command` when the check
    /// declares one, falling back to `command` when it does not. A task
    /// carrying its own `validation_commands` ignores this knob entirely
    /// (see `select_task_checks`'s precedence rule in `task_loop.rs`).
    Fast,
}

/// Per-stage model tier assignment. Field names match the stage identities
/// used across `task_loop.rs`/`graph.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelTiers {
    pub implement: ModelTier,
    pub implement_simple: ModelTier,
    pub review: ModelTier,
    pub triage: ModelTier,
    /// `GenerateTasksNode`'s tier (`setup.rs`). **`Opus`, not `Sonnet`** —
    /// see [`ModelTiers::default`]'s note.
    pub generate: ModelTier,
    /// `PatchDocsNode`'s tier (`docs.rs`). `Sonnet` by default, matching the
    /// model string that node was hardcoded to before it was onboarded to
    /// the policy path.
    pub docs: ModelTier,
}

impl Default for ModelTiers {
    /// Sonnet everywhere **except `generate`**, which is `Opus`.
    ///
    /// The all-Sonnet default was the pre-EN.3.C baseline, but for
    /// `generate` it was never behavior-stable: nothing read
    /// `model_tiers.generate` (it was declared, merged, and reported by
    /// `wrap_up::model_tier_used`, but no node consumed it), while
    /// `GenerateTasksNode` itself hardcoded `claude-opus-4-8`. Telemetry
    /// therefore reported `generate: "sonnet"` for a stage that actually ran
    /// Opus. Corrected to `Opus` so the declared tier matches what that node
    /// runs — a prerequisite for onboarding it to `apply_policy` without
    /// silently downgrading it. `docs` is `Sonnet`, which *is*
    /// behavior-stable against `PatchDocsNode`'s former `claude-sonnet-4-5`
    /// hardcode.
    fn default() -> Self {
        Self {
            implement: ModelTier::Sonnet,
            implement_simple: ModelTier::Sonnet,
            review: ModelTier::Sonnet,
            triage: ModelTier::Sonnet,
            generate: ModelTier::Opus,
            docs: ModelTier::Sonnet,
        }
    }
}

/// Per-stage whole-call timeout, in **seconds**, for the model stages that
/// run through `task_loop.rs`'s `apply_policy`. Field names match the
/// `Stage` variants (`Implement`/`Triage`/`Review`/`Generate`/`Docs`).
///
/// `None` — the behavior-stable built-in default for every field — means
/// "set nothing", leaving `claude_code_rs::Config::timeout` at its own
/// `None`, which is that crate's unconfigured 300s default. Setting a field
/// widens (or narrows) only that stage's `execute()` timeout.
///
/// `generate` (`GenerateTasksNode`) and `docs` (`PatchDocsNode`) are covered
/// too. `docs` reaches `PatchDocsNode` through the config half of the
/// shaping helpers applied directly in `docs.rs` (that node builds its
/// prompt in a `with_prompt_builder` closure, so it cannot use the combined
/// `apply_policy`) — see [`super::docs::PatchDocsNode::process`]. `generate`
/// is consumed by `GenerateTasksNode::process`'s `apply_policy` call — see
/// [`super::setup::GenerateTasksNode::process`].
///
/// Unlike [`ModelTiers`], this derives `Default`: all-`None` is exactly what
/// `#[derive(Default)]` produces, so hand-writing it would only add a place
/// to get it wrong.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallTimeouts {
    pub implement: Option<u64>,
    pub triage: Option<u64>,
    pub review: Option<u64>,
    pub generate: Option<u64>,
    pub docs: Option<u64>,
}

/// Whether (and how much of) the previous attempt's failure output is fed
/// back into `ImplementTaskNode`'s prompt on a retry.
///
/// **Deliberate deviation from CLAUDE.md standing rule 6's
/// "behavior-stable built-in default".** Every other knob in this file
/// defaults to reproducing today's behavior; this one does not. Feeding the
/// prior failure back into the retry prompt *is* the fix this knob ships —
/// a default-off knob would leave the retry loop structurally incapable of
/// self-correction (the live bail that motivated it: three byte-identical
/// attempts against two trivial compile errors). What the knob buys is a
/// cost bound (`max_chars`) and an escape hatch, not opt-in-ness.
///
/// Hand-written `Default` (like [`ModelTiers`], unlike [`CallTimeouts`])
/// because the intended default is NOT the all-zero/all-false derive value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryFeedback {
    /// Feed the prior attempt's failure output into the retry prompt.
    pub enabled: bool,
    /// Upper bound, in characters, on the whole rendered feedback block —
    /// so repeated retries cannot grow the prompt without limit.
    pub max_chars: u32,
}

impl Default for RetryFeedback {
    fn default() -> Self {
        Self {
            enabled: true,
            max_chars: 4000,
        }
    }
}

/// All-optional mirror of [`RetryFeedback`]. Mirrors [`PartialModelTiers`]'
/// derive set exactly — notably **no** `Copy`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialRetryFeedback {
    pub enabled: Option<bool>,
    pub max_chars: Option<u32>,
}

/// Bounded in-node retry-with-backoff budget for `ClaudeCodeStep`'s transport
/// call (`ticket-implement-node-transport-retry`, shape (A) — see that
/// ticket's Amendment Log). Applies uniformly across all five
/// `ClaudeCodeStep` consumers (implement/triage/review/generate/docs — one
/// knob, not six, per CLAUDE.md standing rule 6) rather than being
/// implement-only: a transient transport blip is exactly as disruptive to a
/// cheap triage call as to an implement call, since either still halts the
/// whole `Workflow::walk` (`run_halts_walk_on_failure`).
///
/// Only `claude_code_rs::Error` variants the ticket's investigation marked
/// retryable (`Spawn`/`Timeout`/`Cli`/`Api`) consume this budget;
/// `BinaryNotFound`/`Parse`/`Isolation` are treated as persistent and fail
/// fast regardless of `max_attempts`.
///
/// **Deliberate deviation from CLAUDE.md standing rule 6's "behavior-stable
/// built-in default"** — mirrors [`RetryFeedback`]'s precedent. On the
/// success path (`ClaudeCodeStep::process` AC7) the default is exactly
/// behavior-stable: a first-attempt success invokes the transport once,
/// byte-identical to today. On the *failure* path it is not: today a single
/// transient transport error halts the run; the default here retries it.
/// That change of behavior on failure is the fix this knob ships, not a
/// side effect of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportRetry {
    /// Total attempts per transport call, including the first — never an
    /// unbounded loop. `1` disables retry outright (today's behavior on the
    /// failure path).
    pub max_attempts: u32,
    /// Backoff before the second attempt, in milliseconds; doubles on each
    /// subsequent retry (capped — see `nodes::claude_code_step`'s
    /// `MAX_TRANSPORT_BACKOFF_MS`) so a persistent failure defers the halt
    /// briefly rather than hammering a dead transport or, worse, compounding
    /// a `Timeout` variant's already-expensive wait.
    pub initial_backoff_ms: u64,
}

impl Default for TransportRetry {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_backoff_ms: 200,
        }
    }
}

/// All-optional mirror of [`TransportRetry`]. Mirrors [`PartialRetryFeedback`]'
/// derive set exactly — notably **no** `Copy`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialTransportRetry {
    pub max_attempts: Option<u32>,
    pub initial_backoff_ms: Option<u64>,
}

/// The fully-resolved, per-run SDLC Flow policy — the merge of built-in
/// defaults, `harness.json`'s `sdlc.policy` defaults, and any per-run event
/// override, high->low precedence in that order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SdlcPolicy {
    pub output_verbosity: OutputVerbosity,
    pub prompt_cache: bool,
    pub review_mode: ReviewMode,
    pub review_skip_max_files: u32,
    pub review_skip_max_diff_lines: u32,
    /// How much of a task's own check suite `TestTaskNode` runs (EN.3.D).
    pub test_depth: TestDepth,
    pub model_tiers: ModelTiers,
    /// Per-stage whole-call timeout in seconds; all-`None` (no override) by
    /// default, i.e. `claude-code-rs`'s own 300s default applies.
    pub timeouts: CallTimeouts,
    /// Configuration for the `local` model tier, when any stage uses it.
    pub local: LocalConfig,
    // `simple_task_max_files` was the selector for a simple-task path
    // (paired with `ModelTiers::implement_simple`) that was never built —
    // deleted 2026-08-21 (EN.ticket.sdlc-flow-dead-policy-knobs task 4)
    // as a dead knob per CLAUDE.md standing rule 6. If that path is ever
    // built, this knob and `implement_simple` come back together.
    /// Enables `TriageTaskNode`'s model-triage branch. **Precedence: the
    /// bare `event.llm_triage` field (`SDLCFlowEventSchema::llm_triage`,
    /// the pre-existing, still-supported spelling) wins when a caller sets
    /// it explicitly; otherwise this resolved policy value applies** (a
    /// per-run `policy` override, then a named `profile`, then
    /// `harness.json`, then this built-in default of `false`). This is the
    /// canonical spelling going forward — see `TriageTaskNode` in
    /// `task_loop.rs`.
    pub llm_triage: bool,
    /// Run-wide SEED for [`super::schema::SDLCTask::max_attempts`], applied
    /// at task-creation/load time by `setup.rs`'s `GenerateTasksNode` and
    /// `LoadTaskStateNode`'s bootstrap-from-`tasks.json` path — never an
    /// override. **Precedence: task-declared `max_attempts` > this policy
    /// value > `SDLCTask`'s own built-in default of `3`.** A task whose
    /// JSON explicitly sets its own `max_attempts` always keeps that value,
    /// even when it happens to equal the default; only a task that omits
    /// the field is seeded from here. Distinguishing "declared" from
    /// "defaulted" requires inspecting the raw task JSON before
    /// `serde(default)` collapses that distinction into a concrete `u32`.
    pub max_attempts: u32,
    /// Bound on the review-retry loop that `ReviewRouterNode` closes: a
    /// `FAIL`/`PARTIAL` verdict with 1..=`STRUCTURAL_ISSUE_THRESHOLD` issues
    /// routes back to `IncrementAttemptNode` at most `max_review_attempts`
    /// times before the run is routed to `WrapUpNode` with a terminal
    /// `ReviewExhausted` signal instead. Counted separately from
    /// [`Self::max_attempts`] — see `review_attempts` on the durable
    /// telemetry (`schema.rs`) for why the two counters must never be
    /// conflated. Built-in default `3`, matching JS's `MAX_REVIEW_ATTEMPTS`
    /// (`base-template/.claude/workflows/sdlc-flow.js:252-253`). Behavior-
    /// stable in the sense that matters: it changes an unbounded loop into
    /// a bounded one (the fix), but does not change any run that reviews
    /// clean within three passes — i.e. every run that terminates today.
    pub max_review_attempts: u32,
    /// Whether the previous attempt's failure output is fed back into
    /// `ImplementTaskNode`'s retry prompt, and how large that block may get.
    pub retry_feedback: RetryFeedback,
    /// Bounded in-node retry-with-backoff budget for `ClaudeCodeStep`'s
    /// transport call, applied uniformly to all five consumers. See
    /// [`TransportRetry`].
    pub transport_retry: TransportRetry,
    /// Upper bound, in **characters**, on the working-tree diff embedded in
    /// `ConsolidatedReviewNode`'s prompt.
    ///
    /// Sibling of [`RetryFeedback::max_chars`] and bounded for the same
    /// reason: `ticket-commit-task-work-real-diffs` made the reviewer see
    /// the REAL `git diff HEAD` (before it, the string was always empty),
    /// which made reviewer prompt size — and therefore context consumption
    /// and cost — scale with diff size, unbounded. A large task could
    /// produce a prompt that blows the context window.
    ///
    /// Truncation is deliberately **visible** to the model (see
    /// `task_loop::bound_review_diff`): a silently clipped diff would
    /// recreate the very rubber-stamp failure the real-diff fix exists to
    /// eliminate.
    pub review_diff_max_chars: u32,
}

impl Default for SdlcPolicy {
    /// The safe default: reproduces today's (pre-EN.3.C) behavior exactly.
    /// Normal verbosity, `per_task` review, all-Sonnet tiers, no close-out
    /// reuse, prompt-cache off, `llm_triage` false, `max_attempts` 3,
    /// `test_depth: full`, and no per-stage call-timeout overrides (all
    /// `None` — `claude-code-rs`'s own 300s default applies).
    fn default() -> Self {
        Self {
            output_verbosity: OutputVerbosity::Normal,
            prompt_cache: false,
            review_mode: ReviewMode::PerTask,
            review_skip_max_files: 2,
            review_skip_max_diff_lines: 40,
            test_depth: TestDepth::Full,
            model_tiers: ModelTiers::default(),
            timeouts: CallTimeouts::default(),
            local: LocalConfig::default(),
            llm_triage: false,
            max_attempts: 3,
            // Matching JS's MAX_REVIEW_ATTEMPTS — see the field's doc
            // comment for why it is a separate counter from `max_attempts`.
            max_review_attempts: 3,
            // NOT behavior-stable, deliberately — see `RetryFeedback`'s docs.
            retry_feedback: RetryFeedback::default(),
            // Behavior-stable on the success path; NOT on the failure path,
            // deliberately — see `TransportRetry`'s docs.
            transport_retry: TransportRetry::default(),
            // Behavior-stable in the sense standing rule 6 asks for: chosen
            // so realistic task diffs pass through untruncated (a typical
            // one-task diff is a few hundred lines, ~10-40k characters), yet
            // it is a real ceiling — 120k characters is roughly 30k tokens,
            // well inside a 200k-token window even after the acceptance
            // criteria and the model's own reasoning.
            review_diff_max_chars: 120_000,
        }
    }
}

/// All-optional mirror of [`SdlcPolicy`] used by the two override layers
/// (`harness.json`'s `sdlc.policy` and a per-run event's `policy` field).
/// Every field left `None` falls through to the next-lower-precedence
/// layer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialPolicy {
    pub output_verbosity: Option<OutputVerbosity>,
    pub prompt_cache: Option<bool>,
    pub review_mode: Option<ReviewMode>,
    pub review_skip_max_files: Option<u32>,
    pub review_skip_max_diff_lines: Option<u32>,
    pub test_depth: Option<TestDepth>,
    pub model_tiers: Option<PartialModelTiers>,
    pub timeouts: Option<PartialCallTimeouts>,
    pub local: Option<PartialLocalConfig>,
    pub llm_triage: Option<bool>,
    pub max_attempts: Option<u32>,
    pub max_review_attempts: Option<u32>,
    pub retry_feedback: Option<PartialRetryFeedback>,
    pub transport_retry: Option<PartialTransportRetry>,
    pub review_diff_max_chars: Option<u32>,
}

/// All-optional mirror of [`ModelTiers`] for per-stage partial overrides.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialModelTiers {
    pub implement: Option<ModelTier>,
    pub implement_simple: Option<ModelTier>,
    pub review: Option<ModelTier>,
    pub triage: Option<ModelTier>,
    pub generate: Option<ModelTier>,
    pub docs: Option<ModelTier>,
}

/// All-optional mirror of [`CallTimeouts`] for per-stage partial overrides.
/// Mirrors [`PartialModelTiers`]' derive set exactly — notably **no** `Copy`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialCallTimeouts {
    pub implement: Option<u64>,
    pub triage: Option<u64>,
    pub review: Option<u64>,
    pub generate: Option<u64>,
    pub docs: Option<u64>,
}

fn merge_model_tiers(mut base: ModelTiers, over: &PartialModelTiers) -> ModelTiers {
    if let Some(v) = over.implement {
        base.implement = v;
    }
    if let Some(v) = over.implement_simple {
        base.implement_simple = v;
    }
    if let Some(v) = over.review {
        base.review = v;
    }
    if let Some(v) = over.triage {
        base.triage = v;
    }
    if let Some(v) = over.generate {
        base.generate = v;
    }
    if let Some(v) = over.docs {
        base.docs = v;
    }
    base
}

/// Merge one `PartialCallTimeouts` override layer over a `CallTimeouts`
/// base, field by field. A `None` field in `over` leaves the base value
/// (itself possibly `None`) untouched — so an absent override never turns a
/// timeout *off*, it just declines to set one.
fn merge_call_timeouts(mut base: CallTimeouts, over: &PartialCallTimeouts) -> CallTimeouts {
    if let Some(v) = over.implement {
        base.implement = Some(v);
    }
    if let Some(v) = over.triage {
        base.triage = Some(v);
    }
    if let Some(v) = over.review {
        base.review = Some(v);
    }
    if let Some(v) = over.generate {
        base.generate = Some(v);
    }
    if let Some(v) = over.docs {
        base.docs = Some(v);
    }
    base
}

/// Merge one `PartialRetryFeedback` override layer over a `RetryFeedback`
/// base, field by field. Mirrors [`merge_model_tiers`].
fn merge_retry_feedback(mut base: RetryFeedback, over: &PartialRetryFeedback) -> RetryFeedback {
    if let Some(v) = over.enabled {
        base.enabled = v;
    }
    if let Some(v) = over.max_chars {
        base.max_chars = v;
    }
    base
}

/// Merge one `PartialTransportRetry` override layer over a `TransportRetry`
/// base, field by field. Mirrors [`merge_retry_feedback`].
fn merge_transport_retry(mut base: TransportRetry, over: &PartialTransportRetry) -> TransportRetry {
    if let Some(v) = over.max_attempts {
        base.max_attempts = v;
    }
    if let Some(v) = over.initial_backoff_ms {
        base.initial_backoff_ms = v;
    }
    base
}

impl crate::policy::Policy for SdlcPolicy {
    type Partial = PartialPolicy;

    /// Apply one override layer on top of `self`, field-by-field (`Some` in
    /// `over` wins, `None` falls through to `self`).
    fn apply(self, over: &PartialPolicy) -> Self {
        let base = self;
        SdlcPolicy {
            output_verbosity: merge_opt(base.output_verbosity, over.output_verbosity),
            prompt_cache: merge_opt(base.prompt_cache, over.prompt_cache),
            review_mode: merge_opt(base.review_mode, over.review_mode),
            review_skip_max_files: merge_opt(
                base.review_skip_max_files,
                over.review_skip_max_files,
            ),
            review_skip_max_diff_lines: merge_opt(
                base.review_skip_max_diff_lines,
                over.review_skip_max_diff_lines,
            ),
            test_depth: merge_opt(base.test_depth, over.test_depth),
            model_tiers: match &over.model_tiers {
                Some(mt) => merge_model_tiers(base.model_tiers, mt),
                None => base.model_tiers,
            },
            timeouts: match &over.timeouts {
                Some(t) => merge_call_timeouts(base.timeouts, t),
                None => base.timeouts,
            },
            local: match &over.local {
                Some(l) => base.local.overlay(l),
                None => base.local,
            },
            llm_triage: merge_opt(base.llm_triage, over.llm_triage),
            max_attempts: merge_opt(base.max_attempts, over.max_attempts),
            max_review_attempts: merge_opt(base.max_review_attempts, over.max_review_attempts),
            retry_feedback: match &over.retry_feedback {
                Some(rf) => merge_retry_feedback(base.retry_feedback, rf),
                None => base.retry_feedback,
            },
            transport_retry: match &over.transport_retry {
                Some(tr) => merge_transport_retry(base.transport_retry, tr),
                None => base.transport_retry,
            },
            review_diff_max_chars: merge_opt(
                base.review_diff_max_chars,
                over.review_diff_max_chars,
            ),
        }
    }
}

/// Resolve the four policy layers into one concrete [`SdlcPolicy`],
/// high->low precedence: `event_override` beats `profile` beats
/// `harness_defaults` beats `builtin`. Delegates to the generic
/// `crate::policy::resolve::resolve` now that `SdlcPolicy` implements
/// `crate::policy::Policy` (EN.4.0).
///
/// `builtin` is normally `SdlcPolicy::default()`, taken as a parameter so
/// callers/tests can exercise the merge against any base. `profile` is a
/// named bundle (see `profiles::profile_by_name` and `harness.json`'s
/// `sdlc.profiles`) resolved by the caller before this function runs.
#[must_use]
pub fn resolve(
    builtin: SdlcPolicy,
    harness_defaults: Option<&PartialPolicy>,
    profile: Option<&PartialPolicy>,
    event_override: Option<&PartialPolicy>,
) -> SdlcPolicy {
    crate::policy::resolve::resolve(builtin, harness_defaults, profile, event_override)
}

/// Convenience: a `BTreeMap` view of the resolved per-stage tiers, keyed by
/// stage identity, for callers (e.g. telemetry) that want to iterate
/// stage->tier without matching on `ModelTiers`' fixed fields.
#[must_use]
pub fn model_tiers_by_stage(tiers: &ModelTiers) -> BTreeMap<&'static str, ModelTier> {
    let mut map = BTreeMap::new();
    map.insert("implement", tiers.implement);
    map.insert("implement_simple", tiers.implement_simple);
    map.insert("review", tiers.review);
    map.insert("triage", tiers.triage);
    map.insert("generate", tiers.generate);
    map.insert("docs", tiers.docs);
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::test_support::assert_json_subset;
    use crate::workflows::sdlc_flow::profiles;

    #[test]
    fn builtin_default_matches_pre_en_3_c_baseline() {
        let policy = SdlcPolicy::default();
        assert_eq!(policy.output_verbosity, OutputVerbosity::Normal);
        assert_eq!(policy.review_mode, ReviewMode::PerTask);
        assert_eq!(policy.model_tiers.implement, ModelTier::Sonnet);
        assert_eq!(policy.model_tiers.implement_simple, ModelTier::Sonnet);
        assert_eq!(policy.model_tiers.review, ModelTier::Sonnet);
        assert_eq!(policy.model_tiers.triage, ModelTier::Sonnet);
        // NOT Sonnet — see `ModelTiers::default`'s note: `generate` is
        // `Opus` because that is what `GenerateTasksNode` actually runs.
        assert_eq!(policy.model_tiers.generate, ModelTier::Opus);
        assert_eq!(policy.model_tiers.docs, ModelTier::Sonnet);
        assert!(!policy.prompt_cache);
        assert!(!policy.llm_triage);
        assert_eq!(policy.max_attempts, 3);
        assert_eq!(policy.max_review_attempts, 3);
    }

    #[test]
    fn builtin_default_test_depth_is_full() {
        assert_eq!(SdlcPolicy::default().test_depth, TestDepth::Full);
    }

    /// Change-detector: the whole point of this knob is that adding it did
    /// not change what an existing run does. All-`None` means every stage's
    /// `Config.timeout` stays `None`, i.e. `claude-code-rs`'s 300s default.
    #[test]
    fn builtin_default_timeouts_are_none() {
        let timeouts = SdlcPolicy::default().timeouts;
        assert_eq!(timeouts.implement, None);
        assert_eq!(timeouts.triage, None);
        assert_eq!(timeouts.review, None);
        assert_eq!(timeouts.generate, None);
        assert_eq!(timeouts.docs, None);
        assert_eq!(timeouts, CallTimeouts::default());
    }

    /// Change-detector for the `docs` tier: `Sonnet` is exactly the model
    /// `PatchDocsNode` was hardcoded to before it consumed this field, so
    /// onboarding it changed nothing for a run that sets no tier.
    #[test]
    fn builtin_default_docs_tier_is_behavior_stable_sonnet() {
        assert_eq!(SdlcPolicy::default().model_tiers.docs, ModelTier::Sonnet);
        assert_eq!(
            crate::policy::tier::model_tier_to_model_string(
                SdlcPolicy::default().model_tiers.docs,
                &SdlcPolicy::default().local.model,
            ),
            "claude-sonnet-4-5"
        );
    }

    /// Change-detector for the corrected `generate` tier: `Opus` maps to
    /// `claude-opus-4-8`, the model string `GenerateTasksNode` hardcodes —
    /// so onboarding that node will be behavior-stable too.
    #[test]
    fn builtin_default_generate_tier_maps_to_generate_tasks_nodes_model() {
        assert_eq!(SdlcPolicy::default().model_tiers.generate, ModelTier::Opus);
        assert_eq!(
            crate::policy::tier::model_tier_to_model_string(
                SdlcPolicy::default().model_tiers.generate,
                &SdlcPolicy::default().local.model,
            ),
            "claude-opus-4-8"
        );
    }

    #[test]
    fn merge_model_tiers_overrides_generate_and_docs_independently() {
        let base = ModelTiers::default();
        let over = PartialModelTiers {
            docs: Some(ModelTier::Haiku),
            ..Default::default()
        };
        let merged = merge_model_tiers(base, &over);
        assert_eq!(merged.docs, ModelTier::Haiku);
        // `None` in the override leaves every other stage alone.
        assert_eq!(merged.generate, ModelTier::Opus);
        assert_eq!(merged.implement, ModelTier::Sonnet);

        let over = PartialModelTiers {
            generate: Some(ModelTier::Sonnet),
            ..Default::default()
        };
        let merged = merge_model_tiers(base, &over);
        assert_eq!(merged.generate, ModelTier::Sonnet);
        assert_eq!(merged.docs, ModelTier::Sonnet);
    }

    #[test]
    fn generate_and_docs_tiers_resolve_through_all_four_layers() {
        let harness = PartialPolicy {
            model_tiers: Some(PartialModelTiers {
                generate: Some(ModelTier::Haiku),
                docs: Some(ModelTier::Haiku),
                ..Default::default()
            }),
            ..Default::default()
        };
        let profile = PartialPolicy {
            model_tiers: Some(PartialModelTiers {
                docs: Some(ModelTier::Sonnet),
                ..Default::default()
            }),
            ..Default::default()
        };
        let event = PartialPolicy {
            model_tiers: Some(PartialModelTiers {
                docs: Some(ModelTier::Opus),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = resolve(
            SdlcPolicy::default(),
            Some(&harness),
            Some(&profile),
            Some(&event),
        );
        // event > profile > harness > builtin.
        assert_eq!(resolved.model_tiers.docs, ModelTier::Opus);
        // Neither profile nor event touched `generate`, so harness's wins.
        assert_eq!(resolved.model_tiers.generate, ModelTier::Haiku);

        // Nothing anywhere -> the built-in defaults.
        let untouched = resolve(SdlcPolicy::default(), None, None, None);
        assert_eq!(untouched.model_tiers.generate, ModelTier::Opus);
        assert_eq!(untouched.model_tiers.docs, ModelTier::Sonnet);
    }

    #[test]
    fn generate_and_docs_timeouts_resolve_through_all_four_layers() {
        let harness = PartialPolicy {
            timeouts: Some(PartialCallTimeouts {
                generate: Some(400),
                docs: Some(200),
                ..Default::default()
            }),
            ..Default::default()
        };
        let profile = PartialPolicy {
            timeouts: Some(PartialCallTimeouts {
                docs: Some(500),
                ..Default::default()
            }),
            ..Default::default()
        };
        let event = PartialPolicy {
            timeouts: Some(PartialCallTimeouts {
                docs: Some(900),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = resolve(
            SdlcPolicy::default(),
            Some(&harness),
            Some(&profile),
            Some(&event),
        );
        assert_eq!(resolved.timeouts.docs, Some(900));
        // Only the harness layer set `generate`, so it survives.
        assert_eq!(resolved.timeouts.generate, Some(400));
        // Stages nobody touched stay at the built-in `None`.
        assert_eq!(resolved.timeouts.implement, None);
    }

    #[test]
    fn deserializes_generate_and_docs_knobs_from_harness_json_shape() {
        let json = r#"{
            "model_tiers": { "docs": "haiku" },
            "timeouts": { "generate": 1200, "docs": 600 }
        }"#;
        let partial: PartialPolicy = serde_json::from_str(json).expect("valid PartialPolicy JSON");
        assert_eq!(
            partial.model_tiers.as_ref().unwrap().docs,
            Some(ModelTier::Haiku)
        );
        let timeouts = partial.timeouts.as_ref().expect("timeouts present");
        assert_eq!(timeouts.generate, Some(1200));
        assert_eq!(timeouts.docs, Some(600));
        // Absent fields stay `None` and fall through on merge.
        assert_eq!(timeouts.implement, None);
    }

    /// Change-detector pinning this knob's built-in default. Unlike the
    /// other change-detectors in this module it does NOT pin "today's
    /// behavior" — it pins a deliberate behavior change: `enabled: true` is
    /// the fix (see `RetryFeedback`'s docs for the rule-6 deviation).
    #[test]
    fn builtin_default_retry_feedback_is_enabled() {
        let rf = SdlcPolicy::default().retry_feedback;
        assert!(rf.enabled);
        assert_eq!(rf.max_chars, 4000);
        assert_eq!(
            rf,
            RetryFeedback {
                enabled: true,
                max_chars: 4000
            }
        );
    }

    #[test]
    fn merge_retry_feedback_overrides_only_the_fields_it_sets() {
        let base = RetryFeedback {
            enabled: true,
            max_chars: 4000,
        };
        let over = PartialRetryFeedback {
            enabled: None,
            max_chars: Some(500),
        };
        let merged = merge_retry_feedback(base, &over);
        // `None` in the override leaves the base value alone.
        assert!(merged.enabled);
        assert_eq!(merged.max_chars, 500);

        let off = PartialRetryFeedback {
            enabled: Some(false),
            max_chars: None,
        };
        let merged = merge_retry_feedback(base, &off);
        assert!(!merged.enabled);
        assert_eq!(merged.max_chars, 4000);
    }

    #[test]
    fn retry_feedback_resolves_through_all_four_layers_in_precedence_order() {
        let harness = PartialPolicy {
            retry_feedback: Some(PartialRetryFeedback {
                enabled: Some(false),
                max_chars: Some(100),
            }),
            ..Default::default()
        };
        let profile = PartialPolicy {
            retry_feedback: Some(PartialRetryFeedback {
                enabled: Some(true),
                max_chars: Some(200),
            }),
            ..Default::default()
        };
        let event = PartialPolicy {
            retry_feedback: Some(PartialRetryFeedback {
                max_chars: Some(300),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = resolve(
            SdlcPolicy::default(),
            Some(&harness),
            Some(&profile),
            Some(&event),
        );
        // event > profile > harness > builtin.
        assert_eq!(resolved.retry_feedback.max_chars, 300);
        // event left `enabled` unset, so the profile's value survives.
        assert!(resolved.retry_feedback.enabled);

        // Only harness -> harness wins over builtin.
        let resolved = resolve(SdlcPolicy::default(), Some(&harness), None, None);
        assert!(!resolved.retry_feedback.enabled);
        assert_eq!(resolved.retry_feedback.max_chars, 100);

        // Nothing anywhere -> the built-in default.
        let untouched = resolve(SdlcPolicy::default(), None, None, None);
        assert_eq!(untouched.retry_feedback, RetryFeedback::default());
    }

    /// The reviewer-diff bound resolves through all four layers in the same
    /// precedence order as every other knob, and falls back to the built-in
    /// 120k-character ceiling when no layer names it.
    #[test]
    fn review_diff_max_chars_resolves_through_all_four_layers_in_precedence_order() {
        let harness = PartialPolicy {
            review_diff_max_chars: Some(10_000),
            ..Default::default()
        };
        let profile = PartialPolicy {
            review_diff_max_chars: Some(20_000),
            ..Default::default()
        };
        let event = PartialPolicy {
            review_diff_max_chars: Some(30_000),
            ..Default::default()
        };

        let resolved = resolve(
            SdlcPolicy::default(),
            Some(&harness),
            Some(&profile),
            Some(&event),
        );
        assert_eq!(resolved.review_diff_max_chars, 30_000);

        let resolved = resolve(SdlcPolicy::default(), Some(&harness), Some(&profile), None);
        assert_eq!(resolved.review_diff_max_chars, 20_000);

        let resolved = resolve(SdlcPolicy::default(), Some(&harness), None, None);
        assert_eq!(resolved.review_diff_max_chars, 10_000);

        let untouched = resolve(SdlcPolicy::default(), None, None, None);
        assert_eq!(untouched.review_diff_max_chars, 120_000);
    }

    /// Change-detector pinning `TransportRetry`'s built-in default (3
    /// attempts, 200ms initial backoff) — see its docs for why this is
    /// deliberately NOT behavior-stable on the failure path.
    #[test]
    fn builtin_default_transport_retry_allows_two_retries() {
        let tr = SdlcPolicy::default().transport_retry;
        assert_eq!(
            tr,
            TransportRetry {
                max_attempts: 3,
                initial_backoff_ms: 200,
            }
        );
    }

    #[test]
    fn merge_transport_retry_overrides_only_the_fields_it_sets() {
        let base = TransportRetry {
            max_attempts: 3,
            initial_backoff_ms: 200,
        };
        let over = PartialTransportRetry {
            max_attempts: None,
            initial_backoff_ms: Some(500),
        };
        let merged = merge_transport_retry(base, &over);
        // `None` in the override leaves the base value alone.
        assert_eq!(merged.max_attempts, 3);
        assert_eq!(merged.initial_backoff_ms, 500);

        let capped = PartialTransportRetry {
            max_attempts: Some(1),
            initial_backoff_ms: None,
        };
        let merged = merge_transport_retry(base, &capped);
        assert_eq!(merged.max_attempts, 1);
        assert_eq!(merged.initial_backoff_ms, 200);
    }

    #[test]
    fn transport_retry_resolves_through_all_four_layers_in_precedence_order() {
        let harness = PartialPolicy {
            transport_retry: Some(PartialTransportRetry {
                max_attempts: Some(2),
                initial_backoff_ms: Some(100),
            }),
            ..Default::default()
        };
        let profile = PartialPolicy {
            transport_retry: Some(PartialTransportRetry {
                max_attempts: Some(5),
                initial_backoff_ms: Some(400),
            }),
            ..Default::default()
        };
        let event = PartialPolicy {
            transport_retry: Some(PartialTransportRetry {
                initial_backoff_ms: Some(900),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = resolve(
            SdlcPolicy::default(),
            Some(&harness),
            Some(&profile),
            Some(&event),
        );
        // event > profile > harness > builtin.
        assert_eq!(resolved.transport_retry.initial_backoff_ms, 900);
        // event left `max_attempts` unset, so the profile's value survives.
        assert_eq!(resolved.transport_retry.max_attempts, 5);

        // Only harness -> harness wins over builtin.
        let resolved = resolve(SdlcPolicy::default(), Some(&harness), None, None);
        assert_eq!(resolved.transport_retry.max_attempts, 2);
        assert_eq!(resolved.transport_retry.initial_backoff_ms, 100);

        // Nothing anywhere -> the built-in default.
        let untouched = resolve(SdlcPolicy::default(), None, None, None);
        assert_eq!(untouched.transport_retry, TransportRetry::default());
    }

    #[test]
    fn deserializes_partial_transport_retry_from_harness_json_shape() {
        let json = r#"{ "transport_retry": { "max_attempts": 5 } }"#;
        let partial: PartialPolicy = serde_json::from_str(json).expect("valid PartialPolicy JSON");
        let tr = partial
            .transport_retry
            .as_ref()
            .expect("transport_retry present");
        assert_eq!(tr.max_attempts, Some(5));
        // Absent fields stay `None` and fall through on merge.
        assert_eq!(tr.initial_backoff_ms, None);
    }

    #[test]
    fn deserializes_partial_retry_feedback_from_harness_json_shape() {
        let json = r#"{ "retry_feedback": { "max_chars": 1500 } }"#;
        let partial: PartialPolicy = serde_json::from_str(json).expect("valid PartialPolicy JSON");
        let rf = partial
            .retry_feedback
            .as_ref()
            .expect("retry_feedback present");
        assert_eq!(rf.max_chars, Some(1500));
        // Absent fields stay `None` and fall through on merge.
        assert_eq!(rf.enabled, None);
    }

    #[test]
    fn merge_call_timeouts_overrides_only_the_fields_it_sets() {
        let base = CallTimeouts {
            implement: Some(600),
            triage: Some(120),
            review: None,
            ..Default::default()
        };
        let over = PartialCallTimeouts {
            implement: Some(900),
            triage: None,
            review: Some(300),
            ..Default::default()
        };
        let merged = merge_call_timeouts(base, &over);
        assert_eq!(merged.implement, Some(900));
        // `None` in the override leaves the base value alone.
        assert_eq!(merged.triage, Some(120));
        assert_eq!(merged.review, Some(300));
    }

    #[test]
    fn timeouts_resolve_through_all_four_layers_in_precedence_order() {
        let harness = PartialPolicy {
            timeouts: Some(PartialCallTimeouts {
                implement: Some(400),
                triage: Some(200),
                review: Some(100),
                ..Default::default()
            }),
            ..Default::default()
        };
        let profile = PartialPolicy {
            timeouts: Some(PartialCallTimeouts {
                implement: Some(800),
                triage: Some(500),
                ..Default::default()
            }),
            ..Default::default()
        };
        let event = PartialPolicy {
            timeouts: Some(PartialCallTimeouts {
                implement: Some(1200),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = resolve(
            SdlcPolicy::default(),
            Some(&harness),
            Some(&profile),
            Some(&event),
        );
        // event > profile > harness > builtin(None).
        assert_eq!(resolved.timeouts.implement, Some(1200));
        assert_eq!(resolved.timeouts.triage, Some(500));
        assert_eq!(resolved.timeouts.review, Some(100));

        // Nothing anywhere -> still the built-in all-`None`.
        let untouched = resolve(SdlcPolicy::default(), None, None, None);
        assert_eq!(untouched.timeouts, CallTimeouts::default());
    }

    #[test]
    fn deserializes_partial_call_timeouts_from_harness_json_shape() {
        let json = r#"{ "timeouts": { "implement": 900 } }"#;
        let partial: PartialPolicy = serde_json::from_str(json).expect("valid PartialPolicy JSON");
        let timeouts = partial.timeouts.as_ref().expect("timeouts present");
        assert_eq!(timeouts.implement, Some(900));
        // Absent fields stay `None` and fall through on merge.
        assert_eq!(timeouts.triage, None);
        assert_eq!(timeouts.review, None);
    }

    #[test]
    fn test_depth_serde_wire_contract_is_snake_case() {
        assert_eq!(
            serde_json::to_value(TestDepth::Full).unwrap(),
            serde_json::json!("full")
        );
        assert_eq!(
            serde_json::to_value(TestDepth::Fast).unwrap(),
            serde_json::json!("fast")
        );
        assert_eq!(
            serde_json::from_value::<TestDepth>(serde_json::json!("full")).unwrap(),
            TestDepth::Full
        );
        assert_eq!(
            serde_json::from_value::<TestDepth>(serde_json::json!("fast")).unwrap(),
            TestDepth::Fast
        );
    }

    #[test]
    fn harness_default_overrides_builtin_for_test_depth() {
        let harness = PartialPolicy {
            test_depth: Some(TestDepth::Fast),
            ..Default::default()
        };
        let resolved = resolve(SdlcPolicy::default(), Some(&harness), None, None);
        assert_eq!(resolved.test_depth, TestDepth::Fast);
    }

    #[test]
    fn profile_overrides_harness_default_for_test_depth() {
        let harness = PartialPolicy {
            test_depth: Some(TestDepth::Fast),
            ..Default::default()
        };
        let profile = PartialPolicy {
            test_depth: Some(TestDepth::Full),
            ..Default::default()
        };
        let resolved = resolve(SdlcPolicy::default(), Some(&harness), Some(&profile), None);
        assert_eq!(resolved.test_depth, TestDepth::Full);
    }

    #[test]
    fn event_override_beats_profile_for_test_depth() {
        let profile = PartialPolicy {
            test_depth: Some(TestDepth::Fast),
            ..Default::default()
        };
        let event = PartialPolicy {
            test_depth: Some(TestDepth::Full),
            ..Default::default()
        };
        let resolved = resolve(SdlcPolicy::default(), None, Some(&profile), Some(&event));
        assert_eq!(resolved.test_depth, TestDepth::Full);
    }

    #[test]
    fn partial_policy_with_none_test_depth_leaves_lower_layer_untouched() {
        let harness = PartialPolicy {
            test_depth: Some(TestDepth::Fast),
            ..Default::default()
        };
        let event = PartialPolicy {
            test_depth: None,
            ..Default::default()
        };
        let resolved = resolve(SdlcPolicy::default(), Some(&harness), None, Some(&event));
        assert_eq!(resolved.test_depth, TestDepth::Fast);
    }

    #[test]
    fn resolve_with_no_overrides_returns_builtin() {
        let resolved = resolve(SdlcPolicy::default(), None, None, None);
        assert_eq!(resolved, SdlcPolicy::default());
    }

    #[test]
    fn harness_default_overrides_builtin_for_output_verbosity() {
        let harness = PartialPolicy {
            output_verbosity: Some(OutputVerbosity::Terse),
            ..Default::default()
        };
        let resolved = resolve(SdlcPolicy::default(), Some(&harness), None, None);
        assert_eq!(resolved.output_verbosity, OutputVerbosity::Terse);
        // Untouched knobs still fall through to builtin.
        assert_eq!(resolved.review_mode, ReviewMode::PerTask);
    }

    #[test]
    fn event_override_beats_harness_default_for_review_mode() {
        let harness = PartialPolicy {
            review_mode: Some(ReviewMode::TrivialSkip),
            ..Default::default()
        };
        let event = PartialPolicy {
            review_mode: Some(ReviewMode::EndOnly),
            ..Default::default()
        };
        let resolved = resolve(SdlcPolicy::default(), Some(&harness), None, Some(&event));
        assert_eq!(resolved.review_mode, ReviewMode::EndOnly);
    }

    #[test]
    fn event_override_beats_harness_default_for_model_tiers_stage() {
        let harness = PartialPolicy {
            model_tiers: Some(PartialModelTiers {
                implement: Some(ModelTier::Haiku),
                ..Default::default()
            }),
            ..Default::default()
        };
        let event = PartialPolicy {
            model_tiers: Some(PartialModelTiers {
                implement: Some(ModelTier::Opus),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = resolve(SdlcPolicy::default(), Some(&harness), None, Some(&event));
        assert_eq!(resolved.model_tiers.implement, ModelTier::Opus);
        // Other stages untouched by either override fall through to builtin.
        assert_eq!(resolved.model_tiers.review, ModelTier::Sonnet);
    }

    #[test]
    fn harness_default_overrides_builtin_for_max_attempts() {
        let harness = PartialPolicy {
            max_attempts: Some(5),
            ..Default::default()
        };
        let resolved = resolve(SdlcPolicy::default(), Some(&harness), None, None);
        assert_eq!(resolved.max_attempts, 5);
    }

    #[test]
    fn deserializes_partial_policy_from_harness_json_shape() {
        let json = r#"{
            "output_verbosity": "terse",
            "review_mode": "trivial_skip",
            "review_skip_max_files": 2,
            "review_skip_max_diff_lines": 40,
            "model_tiers": { "implement": "haiku", "generate": "opus" },
            "llm_triage": false
        }"#;
        let partial: PartialPolicy = serde_json::from_str(json).expect("valid PartialPolicy JSON");
        assert_eq!(partial.output_verbosity, Some(OutputVerbosity::Terse));
        assert_eq!(partial.review_mode, Some(ReviewMode::TrivialSkip));
        assert_eq!(
            partial.model_tiers.as_ref().unwrap().implement,
            Some(ModelTier::Haiku)
        );
        assert_eq!(
            partial.model_tiers.as_ref().unwrap().generate,
            Some(ModelTier::Opus)
        );
        // Not present in the JSON -> None, falls through on merge.
        assert_eq!(partial.model_tiers.as_ref().unwrap().review, None);
    }

    #[test]
    fn resolve_with_only_profile_matches_documented_cheap_fast_policy() {
        let cheap_fast = profiles::cheap_fast();
        let resolved = resolve(SdlcPolicy::default(), None, Some(&cheap_fast), None);
        assert_eq!(resolved.model_tiers.implement, ModelTier::Haiku);
        assert_eq!(resolved.model_tiers.triage, ModelTier::Haiku);
        assert_eq!(resolved.model_tiers.review, ModelTier::Haiku);
        assert_eq!(resolved.output_verbosity, OutputVerbosity::Terse);
        assert_eq!(resolved.review_mode, ReviewMode::TrivialSkip);
        // `cheap-fast` sets `generate`/`docs` down to Haiku (cost floor).
        assert_eq!(resolved.model_tiers.generate, ModelTier::Haiku);
        assert_eq!(resolved.model_tiers.docs, ModelTier::Haiku);
        // Untouched knobs still fall through to builtin.
        assert_eq!(resolved.model_tiers.implement_simple, ModelTier::Sonnet);
        assert!(!resolved.prompt_cache);
    }

    #[test]
    fn event_override_beats_profile_for_one_field_while_profile_supplies_the_rest() {
        let cheap_fast = profiles::cheap_fast();
        let event = PartialPolicy {
            max_attempts: Some(9),
            ..Default::default()
        };
        let resolved = resolve(SdlcPolicy::default(), None, Some(&cheap_fast), Some(&event));
        // Event-inline field wins.
        assert_eq!(resolved.max_attempts, 9);
        // Profile still supplies everything the event didn't touch.
        assert_eq!(resolved.model_tiers.implement, ModelTier::Haiku);
        assert_eq!(resolved.model_tiers.triage, ModelTier::Haiku);
        assert_eq!(resolved.model_tiers.review, ModelTier::Haiku);
        assert_eq!(resolved.output_verbosity, OutputVerbosity::Terse);
        assert_eq!(resolved.review_mode, ReviewMode::TrivialSkip);
    }

    #[test]
    fn profile_beats_harness_defaults_for_overlapping_fields() {
        let harness = PartialPolicy {
            review_mode: Some(ReviewMode::EndOnly),
            ..Default::default()
        };
        let cheap_fast = profiles::cheap_fast();
        let resolved = resolve(
            SdlcPolicy::default(),
            Some(&harness),
            Some(&cheap_fast),
            None,
        );
        // profile's review_mode wins over harness_defaults'.
        assert_eq!(resolved.review_mode, ReviewMode::TrivialSkip);
    }

    /// EN.4.0 task 5 step 5.7: pin a fixed non-trivial `(builtin, harness,
    /// profile, event)` input set's resolved-JSON output so a future change
    /// to the delegation onto `crate::policy::resolve` can't silently alter
    /// `SdlcPolicy` resolution — the concrete guard that the refactor
    /// changed nothing observable.
    ///
    /// **This assertion is additive-tolerant.** The literal below is still
    /// the pinned expectation, but it is compared with
    /// [`assert_json_subset`] rather than byte-for-byte: every key/value it
    /// names must be present and equal in the freshly-resolved output, while
    /// keys the output has that the literal lacks are ignored. So adding a
    /// new `SdlcPolicy` knob no longer requires hand-editing this string,
    /// yet any drift in an *existing* resolved value — or a field
    /// disappearing, or a list changing length — still fails, naming the
    /// diverging JSON path. The single property given up versus the old
    /// `assert_eq!` on the serialized string is field *order* stability,
    /// which nothing depends on.
    ///
    /// (Historically this test was byte-identical and every additive knob
    /// paid a papercut here: `"timeouts":{...all null}` was appended when
    /// the per-stage call-timeout knob landed — all-`null`, i.e. present in
    /// the shape but carrying no override — and
    /// `"retry_feedback":{"enabled":true,"max_chars":4000}` when the
    /// retry-feedback knob landed, whose default is deliberately NOT a no-op
    /// (see `RetryFeedback`'s docs). In both cases every pre-existing knob's
    /// resolved value was unchanged, which is exactly the property this test
    /// now checks directly. New knobs need no edit here; if you want one
    /// pinned, add it to the literal.)
    #[test]
    fn resolve_output_matches_pre_hoist_baseline_subset() {
        let harness = PartialPolicy {
            max_attempts: Some(4),
            ..Default::default()
        };
        let cheap_fast = profiles::cheap_fast();
        let event = PartialPolicy {
            llm_triage: Some(true),
            ..Default::default()
        };

        let resolved = resolve(
            SdlcPolicy::default(),
            Some(&harness),
            Some(&cheap_fast),
            Some(&event),
        );

        let expected = "{\"output_verbosity\":\"terse\",\"prompt_cache\":false,\"review_mode\":\"trivial_skip\",\"review_skip_max_files\":2,\"review_skip_max_diff_lines\":40,\"test_depth\":\"fast\",\"model_tiers\":{\"implement\":\"haiku\",\"implement_simple\":\"sonnet\",\"review\":\"haiku\",\"triage\":\"haiku\",\"generate\":\"haiku\",\"docs\":\"haiku\"},\"timeouts\":{\"implement\":null,\"triage\":null,\"review\":null,\"generate\":null,\"docs\":null},\"local\":{\"endpoint\":\"http://localhost:11434\",\"model\":\"qwen2.5-coder:7b\",\"constrained_json\":false},\"llm_triage\":true,\"max_attempts\":4,\"retry_feedback\":{\"enabled\":true,\"max_chars\":4000}}";

        let expected: serde_json::Value =
            serde_json::from_str(expected).expect("pinned baseline literal parses as JSON");
        let actual: serde_json::Value =
            serde_json::to_value(&resolved).expect("resolved policy serializes to JSON");

        assert_json_subset(&expected, &actual);
    }
    /// This repo's own `planning/harness.json` must stay parseable as a
    /// `PartialPolicy` and must carry the two policy-path knobs explicitly
    /// in every named profile (CLAUDE.md standing rule 6: a knob absent from
    /// the profile bundles is a knob nobody will find).
    ///
    /// `planning/` is a gitignored symlink into the company-brain vault, so
    /// this SKIPS rather than fails when the vault is not mounted (a bare
    /// clone) — it is a guard for this working tree, not a hard dependency.
    #[test]
    fn repo_harness_json_deserializes_every_sdlc_policy_and_profile() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../planning/harness.json");
        let Ok(raw) = std::fs::read_to_string(path) else {
            tracing::debug!(path = %path, "skipping: planning/ vault not mounted");
            return;
        };
        let root: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");
        let sdlc = &root["sdlc"];
        let _p: PartialPolicy =
            serde_json::from_value(sdlc["policy"].clone()).expect("sdlc.policy is a PartialPolicy");
        for (name, bundle) in sdlc["profiles"].as_object().unwrap() {
            if name.starts_with('_') {
                continue;
            }
            let parsed: PartialPolicy = serde_json::from_value(bundle.clone())
                .unwrap_or_else(|e| panic!("profile {name} must be a PartialPolicy: {e}"));
            let tiers = parsed.model_tiers.expect("model_tiers");
            assert!(tiers.docs.is_some(), "{name} must set model_tiers.docs");
            assert!(
                tiers.generate.is_some(),
                "{name} must set model_tiers.generate"
            );
            let t = parsed.timeouts.expect("timeouts");
            assert_eq!(
                t.docs, None,
                "{name} timeouts.docs must be an explicit null"
            );
            assert_eq!(
                t.generate, None,
                "{name} timeouts.generate must be an explicit null"
            );
            assert!(
                parsed.review_diff_max_chars.is_some(),
                "{name} must set review_diff_max_chars"
            );
            assert!(
                parsed.max_review_attempts.is_some(),
                "{name} must set max_review_attempts"
            );
        }
    }

    /// Destructures `SdlcPolicy` using exactly the identifier list passed
    /// in, then returns those identifiers' names as `&'static str`s. The
    /// destructure has NO `..` — Rust's struct-pattern rules therefore
    /// require this identifier list to name every field `SdlcPolicy` has,
    /// in both directions: a field added to the struct and omitted here is
    /// a "missing field" compile error, and a name listed here that the
    /// struct does not have is a "no field with that name" compile error.
    /// That is the actual guard — `every_sdlc_policy_field_has_a_declared_production_consumer`'s
    /// runtime comparison below only checks this returned list against
    /// `FIELD_CONSUMERS`, which by itself could drift from the struct if
    /// nothing forced this macro's own list to stay honest.
    macro_rules! policy_field_names {
        ($($field:ident),+ $(,)?) => {{
            let SdlcPolicy { $($field),+ } = SdlcPolicy::default();
            // Consume the bindings so an unused-variable lint can't fire —
            // this macro exists purely for its destructuring side effect.
            let _ = ($($field,)+);
            [$(stringify!($field)),+]
        }};
    }

    /// Claim table: every `SdlcPolicy` field paired with the production
    /// module(s)/function(s) that read the RESOLVED policy value for it.
    ///
    /// **What this table proves, and what it does not.** Exhaustiveness —
    /// every field has *some* named entry — is enforced mechanically by
    /// `every_sdlc_policy_field_has_a_declared_production_consumer` below,
    /// via `policy_field_names!`'s compile-time-exhaustive destructure. It
    /// is a CLAIM, not a proof, that the named module actually reads the
    /// field correctly, or reads it at all — a copy-pasted or stale
    /// consumer string would still pass this test. The real proof, for the
    /// knobs where it matters, is an effect-observing test elsewhere in
    /// this crate: `max_attempts` (`setup.rs` tests asserting the burned
    /// attempt count), `llm_triage` (`task_loop.rs`'s
    /// `resolved_policy_llm_triage_*` tests), and `transport_retry`
    /// (`task_loop.rs`'s retry-count tests against a failing transport
    /// double). This table's value is upstream of proof: it forces whoever
    /// adds a knob to write down an answer to "who reads this" at the
    /// moment the knob is added, rather than five specs later.
    const FIELD_CONSUMERS: &[(&str, &str)] = &[
        (
            "output_verbosity",
            "task_loop.rs::apply_verbosity_directive (per-stage prompt), docs.rs (PatchDocsNode)",
        ),
        ("prompt_cache", "task_loop.rs::apply_prompt_cache"),
        (
            "review_mode",
            "end_review.rs (EndOnly branch gate), task_loop.rs (per-task review routing)",
        ),
        (
            "review_skip_max_files",
            "task_loop.rs::should_skip_review (TrivialSkip threshold)",
        ),
        (
            "review_skip_max_diff_lines",
            "task_loop.rs::should_skip_review (TrivialSkip threshold)",
        ),
        ("test_depth", "task_loop.rs (TestTaskNode's check-suite depth)"),
        (
            "model_tiers",
            "task_loop.rs::model_tier_for_stage (all five stages), docs.rs, setup.rs (GenerateTasksNode), graph.rs (local-transport gate)",
        ),
        (
            "timeouts",
            "task_loop.rs::timeout_for_stage (all five stages), docs.rs, setup.rs (GenerateTasksNode)",
        ),
        (
            "local",
            "task_loop.rs::apply_model_tier (local model string), graph.rs (openai_compat_meta_transport_live)",
        ),
        ("llm_triage", "task_loop.rs::TriageTaskNode (resolved_policy fallback)"),
        (
            "max_attempts",
            "setup.rs::seed_max_attempts, called from GenerateTasksNode and LoadTaskStateNode's tasks.json bootstrap",
        ),
        (
            "max_review_attempts",
            "task_loop.rs (ReviewRouterNode's review-retry bound), wrap_up.rs (ReviewExhausted signal)",
        ),
        (
            "retry_feedback",
            "task_loop.rs::prior_attempt_feedback and ::triage_failure_feedback",
        ),
        (
            "transport_retry",
            "task_loop.rs (implement/triage/review ClaudeCodeStep), docs.rs, end_review.rs, setup.rs — all five with_retry_policy call sites",
        ),
        (
            "review_diff_max_chars",
            "task_loop.rs::bound_review_diff, end_review.rs (EndReviewNode's diff budget)",
        ),
    ];

    #[test]
    fn every_sdlc_policy_field_has_a_declared_production_consumer() {
        // See `policy_field_names!`'s doc comment: this call is the
        // compile-time half of the guard. If a field is ever added to
        // `SdlcPolicy` without being added here, this line fails to
        // compile ("missing field in pattern"); a name here that is no
        // longer a field fails to compile the opposite way. Either way,
        // the guard fires before the runtime assertion below can even run.
        let field_names = policy_field_names!(
            output_verbosity,
            prompt_cache,
            review_mode,
            review_skip_max_files,
            review_skip_max_diff_lines,
            test_depth,
            model_tiers,
            timeouts,
            local,
            llm_triage,
            max_attempts,
            max_review_attempts,
            retry_feedback,
            transport_retry,
            review_diff_max_chars,
        );

        let mut actual: Vec<&str> = field_names.to_vec();
        actual.sort_unstable();

        let mut declared: Vec<&str> = FIELD_CONSUMERS.iter().map(|(name, _)| *name).collect();
        declared.sort_unstable();

        assert_eq!(
            actual, declared,
            "FIELD_CONSUMERS must name exactly SdlcPolicy's fields — no more, no fewer. \
             A knob without a table entry is exactly the failure mode this test exists to \
             catch: a policy value that is declared, settable, and silently ignored."
        );

        for (name, consumer) in FIELD_CONSUMERS {
            assert!(
                !consumer.trim().is_empty(),
                "field {name} has an empty consumer entry"
            );
        }
    }
}
