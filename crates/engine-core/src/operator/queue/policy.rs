//! `OperatorQueuePolicy` — the depth-limit and answer-timeout knobs for the
//! operator queue (`EN.8.B` task 3), resolved through the standard
//! four-layer precedence (per-run event `policy` override > named `profile`
//! bundle > `planning/harness.json` `operator_queue.policy` defaults >
//! built-in default) per `CLAUDE.md` standing rule 6.
//!
//! `operator_queue_depth` defaults to `1` — the §7.5 Invariant 3 "at most
//! one open operator-facing item at a time" contract. The default is
//! behavior-stable in the trivial sense that matters for a brand-new
//! surface: an unset knob reproduces the depth-1 behavior the acceptance
//! criteria require, not some other number.
//!
//! Task 4 (`EN.8.B` task 4) extends this same file with the digest/storm-
//! suppression knobs (`suppression_window_secs`, `digest_schedule_secs`) —
//! this task defines only `operator_queue_depth` and `answer_timeout_secs`.

use serde::{Deserialize, Serialize};

use crate::node::NodeError;
use crate::policy::{merge_opt, Policy, PolicyConfigSource};

/// The `harness.json` section key this queue's policy/profiles live under
/// (`operator_queue.policy` / `operator_queue.profiles`).
const WORKFLOW_KEY: &str = "operator_queue";

/// The fully-resolved, per-run operator queue policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorQueuePolicy {
    /// Maximum number of items the queue will hold open (delivered, awaiting
    /// an answer) at once. The §7.5 Invariant 3 contract fixes this at `1`
    /// by default — see the module header.
    pub operator_queue_depth: u32,
    /// How long an open item may go unanswered before
    /// [`super::OperatorQueue::next_deliverable`] releases the next item.
    /// The timed-out item is re-queued, never dropped.
    pub answer_timeout_secs: i64,
    /// Task 4 (`EN.8.B` task 4, [`super::digest`]): the storm-suppression
    /// window. Items whose `enqueued_at` falls within this many seconds of
    /// `now` collapse into one digest delivery (top item plus a count)
    /// rather than one message per item — this is a separate mechanism from
    /// `bastion:BA.18.A`'s seed-before-emit fix and does not replace it; see
    /// `super::digest`'s module header.
    pub suppression_window_secs: i64,
    /// Task 4: how often the low-priority digest tail may be pulled. The
    /// digest is **pulled on this schedule, never pushed on arrival** — this
    /// knob bounds how stale a pulled digest is allowed to be, it does not
    /// itself trigger anything.
    pub digest_schedule_secs: i64,
}

impl Default for OperatorQueuePolicy {
    /// Behavior-stable baseline: at most one open item, released after 15
    /// minutes (900s) unanswered; a 60s storm-suppression window and an
    /// hourly (3600s) digest pull schedule.
    fn default() -> Self {
        Self {
            operator_queue_depth: 1,
            answer_timeout_secs: 900,
            suppression_window_secs: 60,
            digest_schedule_secs: 3600,
        }
    }
}

/// All-optional mirror of [`OperatorQueuePolicy`] used by the override
/// layers (`harness.json`'s `operator_queue.policy`, a named `profile`, and
/// a per-run event's `policy` field). Every field left `None` falls through
/// to the next-lower-precedence layer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialOperatorQueuePolicy {
    pub operator_queue_depth: Option<u32>,
    pub answer_timeout_secs: Option<i64>,
    pub suppression_window_secs: Option<i64>,
    pub digest_schedule_secs: Option<i64>,
}

impl Policy for OperatorQueuePolicy {
    type Partial = PartialOperatorQueuePolicy;

    /// Apply one override layer on top of `self`, field-by-field (`Some` in
    /// `over` wins, `None` falls through to `self`).
    fn apply(self, over: &PartialOperatorQueuePolicy) -> Self {
        Self {
            operator_queue_depth: merge_opt(self.operator_queue_depth, over.operator_queue_depth),
            answer_timeout_secs: merge_opt(self.answer_timeout_secs, over.answer_timeout_secs),
            suppression_window_secs: merge_opt(
                self.suppression_window_secs,
                over.suppression_window_secs,
            ),
            digest_schedule_secs: merge_opt(self.digest_schedule_secs, over.digest_schedule_secs),
        }
    }
}

/// The explicit control profile: depth 1, 15-minute timeout — spelled out
/// explicitly (rather than left all-`None`) so selecting `profile:
/// "baseline"` is a legible, self-documenting no-op against the built-in
/// default.
#[must_use]
pub fn baseline() -> PartialOperatorQueuePolicy {
    PartialOperatorQueuePolicy {
        operator_queue_depth: Some(1),
        answer_timeout_secs: Some(900),
        suppression_window_secs: Some(60),
        digest_schedule_secs: Some(3600),
    }
}

/// Cost/latency floor: depth stays 1 (§7.5 Invariant 3 is not a cost dial),
/// but the answer timeout is shortened to 5 minutes so a run optimizing for
/// speed cycles through the queue faster when items go unanswered. The
/// suppression window widens (fewer, more-collapsed messages) and the
/// digest is pulled less often, both cutting the noise a speed-optimized
/// deployment would otherwise pay attention-cost for.
#[must_use]
pub fn cheap_fast() -> PartialOperatorQueuePolicy {
    PartialOperatorQueuePolicy {
        operator_queue_depth: Some(1),
        answer_timeout_secs: Some(300),
        suppression_window_secs: Some(120),
        digest_schedule_secs: Some(7200),
    }
}

/// Quality ceiling: depth stays 1, but the answer timeout is lengthened to
/// 30 minutes so a run optimizing for operator judgement over speed gives
/// more room before an item is released to the next one. The suppression
/// window narrows (fewer arrivals get collapsed together, so the operator
/// sees more distinct items) and the digest is pulled more often.
#[must_use]
pub fn thorough() -> PartialOperatorQueuePolicy {
    PartialOperatorQueuePolicy {
        operator_queue_depth: Some(1),
        answer_timeout_secs: Some(1800),
        suppression_window_secs: Some(30),
        digest_schedule_secs: Some(1800),
    }
}

/// Resolve a built-in profile bundle by its kebab-case name. Returns `None`
/// for any name that isn't one of the three canonical profiles.
#[must_use]
pub fn profile_by_name(name: &str) -> Option<PartialOperatorQueuePolicy> {
    match name {
        "baseline" => Some(baseline()),
        "cheap-fast" => Some(cheap_fast()),
        "thorough" => Some(thorough()),
        _ => None,
    }
}

/// Read `operator_queue.policy` (a [`PartialOperatorQueuePolicy`]) out of
/// the file addressed by `source`. Delegates to the generic
/// `crate::policy::read_harness_policy_defaults_from`, parameterized by
/// [`WORKFLOW_KEY`].
pub fn read_harness_policy_defaults_from(
    source: &PolicyConfigSource,
) -> Result<Option<PartialOperatorQueuePolicy>, NodeError> {
    crate::policy::read_harness_policy_defaults_from(source, WORKFLOW_KEY)
}

/// Resolve a named `profile` to a [`PartialOperatorQueuePolicy`] bundle,
/// preferring a `harness.json` `operator_queue.profiles[name]` entry (read
/// via `source`) over the built-in [`profile_by_name`]. Returns `Ok(None)`
/// when `profile_name` is `None`, and `Err` when a name is given but
/// resolves to neither source (no silent no-op). Delegates to the generic
/// `crate::policy::resolve_profile_from`, parameterized by [`WORKFLOW_KEY`]
/// and [`profile_by_name`].
pub fn resolve_profile_from(
    profile_name: Option<&str>,
    source: &PolicyConfigSource,
) -> Result<Option<PartialOperatorQueuePolicy>, NodeError> {
    crate::policy::resolve_profile_from(profile_name, source, WORKFLOW_KEY, profile_by_name)
}

/// Resolve the four-layer [`OperatorQueuePolicy`] for a run: `event_override`
/// (a per-run `policy` field), the resolved `profile_name` bundle, `source`'s
/// `operator_queue.policy` defaults, and the built-in default, high->low
/// precedence via `crate::policy::resolve`. A [`PolicyConfigSource::Builtin`]
/// source resolves successfully with no filesystem access at all — the case
/// a worktree-free queue (e.g. one embedded directly in `bastion serve`)
/// needs.
pub fn resolve_policy_for_run_from(
    source: &PolicyConfigSource,
    profile_name: Option<&str>,
    event_override: Option<&PartialOperatorQueuePolicy>,
) -> Result<OperatorQueuePolicy, NodeError> {
    let harness_defaults = read_harness_policy_defaults_from(source)?;
    let profile = resolve_profile_from(profile_name, source)?;
    Ok(crate::policy::resolve(
        OperatorQueuePolicy::default(),
        harness_defaults.as_ref(),
        profile.as_ref(),
        event_override,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_default_is_depth_one_fifteen_minute_timeout() {
        let policy = OperatorQueuePolicy::default();
        assert_eq!(policy.operator_queue_depth, 1);
        assert_eq!(policy.answer_timeout_secs, 900);
        assert_eq!(policy.suppression_window_secs, 60);
        assert_eq!(policy.digest_schedule_secs, 3600);
    }

    #[test]
    fn every_named_profile_sets_the_digest_knobs_explicitly() {
        // A knob absent from the profile bundles is a knob nobody will
        // find — standing rule 6.
        assert!(baseline().suppression_window_secs.is_some());
        assert!(baseline().digest_schedule_secs.is_some());
        assert!(cheap_fast().suppression_window_secs.is_some());
        assert!(cheap_fast().digest_schedule_secs.is_some());
        assert!(thorough().suppression_window_secs.is_some());
        assert!(thorough().digest_schedule_secs.is_some());
    }

    #[test]
    fn resolve_with_no_overrides_returns_builtin() {
        let resolved = crate::policy::resolve(OperatorQueuePolicy::default(), None, None, None);
        assert_eq!(resolved, OperatorQueuePolicy::default());
    }

    #[test]
    fn harness_default_overrides_builtin_timeout() {
        let harness = PartialOperatorQueuePolicy {
            answer_timeout_secs: Some(120),
            ..Default::default()
        };
        let resolved =
            crate::policy::resolve(OperatorQueuePolicy::default(), Some(&harness), None, None);
        assert_eq!(resolved.answer_timeout_secs, 120);
        // Untouched knob still falls through to builtin.
        assert_eq!(resolved.operator_queue_depth, 1);
    }

    #[test]
    fn profile_beats_harness_defaults() {
        let harness = PartialOperatorQueuePolicy {
            answer_timeout_secs: Some(120),
            ..Default::default()
        };
        let profile = PartialOperatorQueuePolicy {
            answer_timeout_secs: Some(60),
            ..Default::default()
        };
        let resolved = crate::policy::resolve(
            OperatorQueuePolicy::default(),
            Some(&harness),
            Some(&profile),
            None,
        );
        assert_eq!(resolved.answer_timeout_secs, 60);
    }

    #[test]
    fn event_override_beats_profile() {
        let profile = PartialOperatorQueuePolicy {
            answer_timeout_secs: Some(60),
            ..Default::default()
        };
        let event = PartialOperatorQueuePolicy {
            answer_timeout_secs: Some(30),
            ..Default::default()
        };
        let resolved = crate::policy::resolve(
            OperatorQueuePolicy::default(),
            None,
            Some(&profile),
            Some(&event),
        );
        assert_eq!(resolved.answer_timeout_secs, 30);
    }

    #[test]
    fn profile_by_name_resolves_all_three_canonical_names() {
        assert_eq!(profile_by_name("baseline"), Some(baseline()));
        assert_eq!(profile_by_name("cheap-fast"), Some(cheap_fast()));
        assert_eq!(profile_by_name("thorough"), Some(thorough()));
    }

    #[test]
    fn profile_by_name_returns_none_for_unknown_name() {
        assert_eq!(profile_by_name("nonexistent"), None);
    }

    #[test]
    fn every_named_profile_holds_depth_at_the_invariant_of_one() {
        // §7.5 Invariant 3 is not a cost/quality dial — every profile keeps
        // depth at 1 even though the timeout varies.
        assert_eq!(baseline().operator_queue_depth, Some(1));
        assert_eq!(cheap_fast().operator_queue_depth, Some(1));
        assert_eq!(thorough().operator_queue_depth, Some(1));
    }

    #[test]
    fn deserializes_partial_policy_from_harness_json_shape() {
        let json = r#"{ "operator_queue_depth": 2, "answer_timeout_secs": 42 }"#;
        let partial: PartialOperatorQueuePolicy =
            serde_json::from_str(json).expect("valid PartialOperatorQueuePolicy JSON");
        assert_eq!(partial.operator_queue_depth, Some(2));
        assert_eq!(partial.answer_timeout_secs, Some(42));
    }

    #[test]
    fn partial_policy_round_trips_with_fields_absent() {
        let partial: PartialOperatorQueuePolicy =
            serde_json::from_str("{}").expect("valid empty PartialOperatorQueuePolicy JSON");
        assert_eq!(partial, PartialOperatorQueuePolicy::default());
    }

    #[test]
    fn resolve_policy_for_run_from_builtin_source_needs_no_filesystem() {
        let resolved = resolve_policy_for_run_from(&PolicyConfigSource::Builtin, None, None)
            .expect("resolve should succeed with no filesystem access");
        assert_eq!(resolved, OperatorQueuePolicy::default());
    }

    #[test]
    fn resolve_policy_for_run_from_applies_named_profile() {
        let resolved =
            resolve_policy_for_run_from(&PolicyConfigSource::Builtin, Some("cheap-fast"), None)
                .expect("resolve should succeed");
        assert_eq!(resolved.answer_timeout_secs, 300);
        assert_eq!(resolved.operator_queue_depth, 1);
    }

    #[test]
    fn resolve_policy_for_run_from_unknown_profile_errors() {
        let err =
            resolve_policy_for_run_from(&PolicyConfigSource::Builtin, Some("nonexistent"), None)
                .expect_err("should fail");
        assert!(err.message.contains("unknown profile"));
    }

    #[test]
    fn resolve_policy_for_run_from_event_override_beats_profile() {
        let event = PartialOperatorQueuePolicy {
            answer_timeout_secs: Some(7),
            ..Default::default()
        };
        let resolved = resolve_policy_for_run_from(
            &PolicyConfigSource::Builtin,
            Some("thorough"),
            Some(&event),
        )
        .expect("resolve should succeed");
        assert_eq!(resolved.answer_timeout_secs, 7);
    }

    #[test]
    fn resolve_policy_for_run_from_reads_harness_file_source() {
        let dir = std::env::temp_dir().join(format!(
            "engine-operator-queue-policy-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let harness_file = dir.join("harness.json");
        std::fs::write(
            &harness_file,
            serde_json::json!({
                "operator_queue": { "policy": { "answer_timeout_secs": 55 } }
            })
            .to_string(),
        )
        .expect("write harness file");

        let source = PolicyConfigSource::HarnessFile(harness_file);
        let resolved =
            resolve_policy_for_run_from(&source, None, None).expect("resolve should succeed");
        assert_eq!(resolved.answer_timeout_secs, 55);
        // Untouched knob falls through to builtin.
        assert_eq!(resolved.operator_queue_depth, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
