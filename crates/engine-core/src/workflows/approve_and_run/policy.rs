//! `ApproveAndRunPolicy` / `PartialApproveAndRunPolicy` — the drain-bound
//! and fallback knobs `APPROVE_AND_RUN` introduces (`EN.8.D` task 2),
//! resolved through the standard four-layer precedence (per-run event
//! `policy` override > named `profile` bundle > `planning/harness.json`
//! `approve_and_run.policy` defaults > built-in default) per `CLAUDE.md`
//! standing rule 6.
//!
//! Three knobs, none of them model-tier — this workflow drives no
//! `ClaudeCodeStep`, so there is no `ModelTiers`/`LocalConfig` here the way
//! `content_pipeline`/`diagnostic_intake` carry one:
//!
//! - `drain_batch_max` — how many pending-harvest records one drain pass
//!   (`drain::drain`, task 3) considers before reporting a truncated pass.
//! - `harvest_item_priority` — the uniform `effective_priority` a drained
//!   harvest item enqueues under (see the Notes assumption in
//!   `planning/EN.8.D/tasks.md`: pending-harvest records carry no priority
//!   field of their own, so ordering among harvest items falls back to
//!   [`crate::operator::queue::compare_items`]'s secondary keys).
//! - `session_fallback_slug` — the `session-<slug>` a record that cannot be
//!   reduced to a conforming payload routes to instead of `notification`
//!   (task 3).
//!
//! Every default here is behavior-stable in the only sense that applies to
//! a brand-new surface: an unset knob reproduces these exact values, not
//! some other number, so introducing the knob changes nothing about a run
//! that never overrides it.

use serde::{Deserialize, Serialize};

use crate::policy::{merge_opt, Policy};

/// The fully-resolved, per-run `APPROVE_AND_RUN` policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApproveAndRunPolicy {
    /// Maximum number of pending-harvest records one drain pass
    /// ([`super::drain::drain`]) considers. A batch larger than this is
    /// truncated, and the truncation is reported to the caller rather than
    /// silently dropped.
    pub drain_batch_max: u32,
    /// The uniform `effective_priority` assigned to every drained harvest
    /// item. Pending-harvest records carry no priority field of their own
    /// (see the module header), so this is a policy-resolved default,
    /// uniform across harvest items in a given run.
    pub harvest_item_priority: i32,
    /// The `session-<slug>` a pending-harvest record that cannot be reduced
    /// to a conforming [`crate::operator::OperatorPayload`] routes to
    /// instead of `notification` (task 1's `render_and_validate`
    /// `Err` path, consumed by task 3's drain).
    pub session_fallback_slug: String,
}

impl Default for ApproveAndRunPolicy {
    /// Behavior-stable baseline: a drain pass considers up to 60 records —
    /// enough to cover the §7.5 Invariant 3 storm scenario (a 60-item
    /// pending-harvest set) in one pass without truncation — harvest items
    /// enqueue at priority `0` (the queue's neutral/default priority, so
    /// ordering among them falls back to `compare_items`' `enqueued_at` /
    /// `item_id` secondary keys), and a non-conforming record routes to
    /// `session-harvest-review`.
    fn default() -> Self {
        Self {
            drain_batch_max: 60,
            harvest_item_priority: 0,
            session_fallback_slug: "harvest-review".to_string(),
        }
    }
}

/// All-optional mirror of [`ApproveAndRunPolicy`] used by the override
/// layers (`harness.json`'s `approve_and_run.policy`, a named `profile`,
/// and a per-run event's `policy` field). Every field left `None` falls
/// through to the next-lower-precedence layer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialApproveAndRunPolicy {
    pub drain_batch_max: Option<u32>,
    pub harvest_item_priority: Option<i32>,
    pub session_fallback_slug: Option<String>,
}

impl Policy for ApproveAndRunPolicy {
    type Partial = PartialApproveAndRunPolicy;

    /// Apply one override layer on top of `self`, field-by-field (`Some` in
    /// `over` wins, `None` falls through to `self`).
    fn apply(self, over: &PartialApproveAndRunPolicy) -> Self {
        Self {
            drain_batch_max: merge_opt(self.drain_batch_max, over.drain_batch_max),
            harvest_item_priority: merge_opt(
                self.harvest_item_priority,
                over.harvest_item_priority,
            ),
            session_fallback_slug: merge_opt(
                self.session_fallback_slug,
                over.session_fallback_slug.clone(),
            ),
        }
    }
}

/// Serialize `policy` into the plain JSON object [`super::mod`]'s eventual
/// graph node stamps into `ctx.nodes` (task 5, via
/// [`crate::policy::stamp_resolved_policy`]) — deliberately a thin,
/// `cost_usd`-free serialization. `workflow.rs:552-554` folds any `cost_usd`
/// key it finds into `BudgetLedger` untyped, with no provenance check;
/// `ApproveAndRunPolicy` never introduces one and `policy_state` does not
/// add one either.
#[must_use]
pub fn policy_state(policy: &ApproveAndRunPolicy) -> serde_json::Value {
    serde_json::to_value(policy).expect("ApproveAndRunPolicy always serializes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_default_is_behavior_stable_baseline() {
        let policy = ApproveAndRunPolicy::default();
        assert_eq!(policy.drain_batch_max, 60);
        assert_eq!(policy.harvest_item_priority, 0);
        assert_eq!(policy.session_fallback_slug, "harvest-review");
    }

    #[test]
    fn resolve_with_no_overrides_returns_builtin() {
        let resolved = crate::policy::resolve(ApproveAndRunPolicy::default(), None, None, None);
        assert_eq!(resolved, ApproveAndRunPolicy::default());
    }

    #[test]
    fn harness_default_overrides_builtin_for_drain_batch_max() {
        let harness = PartialApproveAndRunPolicy {
            drain_batch_max: Some(10),
            ..Default::default()
        };
        let resolved =
            crate::policy::resolve(ApproveAndRunPolicy::default(), Some(&harness), None, None);
        assert_eq!(resolved.drain_batch_max, 10);
        // Untouched knobs still fall through to builtin.
        assert_eq!(resolved.harvest_item_priority, 0);
        assert_eq!(resolved.session_fallback_slug, "harvest-review");
    }

    #[test]
    fn profile_beats_harness_defaults_for_session_fallback_slug() {
        let harness = PartialApproveAndRunPolicy {
            session_fallback_slug: Some("from-harness".to_string()),
            ..Default::default()
        };
        let profile = PartialApproveAndRunPolicy {
            session_fallback_slug: Some("from-profile".to_string()),
            ..Default::default()
        };
        let resolved = crate::policy::resolve(
            ApproveAndRunPolicy::default(),
            Some(&harness),
            Some(&profile),
            None,
        );
        assert_eq!(resolved.session_fallback_slug, "from-profile");
    }

    #[test]
    fn event_override_beats_profile_for_harvest_item_priority() {
        let profile = PartialApproveAndRunPolicy {
            harvest_item_priority: Some(5),
            ..Default::default()
        };
        let event = PartialApproveAndRunPolicy {
            harvest_item_priority: Some(9),
            ..Default::default()
        };
        let resolved = crate::policy::resolve(
            ApproveAndRunPolicy::default(),
            None,
            Some(&profile),
            Some(&event),
        );
        assert_eq!(resolved.harvest_item_priority, 9);
    }

    #[test]
    fn deserializes_partial_policy_from_harness_json_shape() {
        let json = r#"{
            "drain_batch_max": 25,
            "harvest_item_priority": 3,
            "session_fallback_slug": "custom-slug"
        }"#;
        let partial: PartialApproveAndRunPolicy =
            serde_json::from_str(json).expect("valid PartialApproveAndRunPolicy JSON");
        assert_eq!(partial.drain_batch_max, Some(25));
        assert_eq!(partial.harvest_item_priority, Some(3));
        assert_eq!(
            partial.session_fallback_slug,
            Some("custom-slug".to_string())
        );
    }

    #[test]
    fn partial_policy_round_trips_with_fields_absent() {
        let partial: PartialApproveAndRunPolicy =
            serde_json::from_str("{}").expect("valid empty PartialApproveAndRunPolicy JSON");
        assert_eq!(partial, PartialApproveAndRunPolicy::default());
    }

    #[test]
    fn policy_state_round_trips_and_carries_no_cost_usd_key() {
        let policy = ApproveAndRunPolicy::default();
        let state = policy_state(&policy);
        assert!(
            state.get("cost_usd").is_none(),
            "policy_state must never emit a cost_usd key"
        );
        let round_tripped: ApproveAndRunPolicy =
            serde_json::from_value(state).expect("policy_state output deserializes back");
        assert_eq!(round_tripped, policy);
    }
}
