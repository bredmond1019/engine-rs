//! Orphan-recovery policy (`EN.9.C`).
//!
//! Per `planning/EN.9.C/tasks.md`: launchd's `KeepAlive=true` /
//! `ThrottleInterval=10` restarts a crashed `bastion serve` instance within
//! ~10s and hides the evidence by coming back up healthy — a run stranded
//! mid-walk (no `metadata.completion` marker, per `crate::completion`) looks
//! like nothing happened. This module resolves the knobs that govern how
//! aggressively the boot sweep (`engine-serve::orphan::reconcile_orphans`,
//! task 5) and the stale-run alarm (task 6) act on that evidence.
//!
//! Mirrors `operator::failure`'s module shape exactly: a full policy struct,
//! an all-optional partial mirror for override layers, `baseline`/
//! `cheap_fast`/`thorough` profile bundles, `profile_by_name`,
//! `read_harness_policy_defaults_from`, `resolve_profile_from`,
//! `resolve_policy_for_run_from`, and `policy_state`.
//!
//! ## Policy knobs
//!
//! Resolved through the standard four-layer precedence (per-run event
//! `policy` override > named `profile` bundle > `planning/harness.json`'s
//! `orphan_recovery.policy` defaults > built-in default) per `CLAUDE.md`
//! standing rule 6:
//!
//! - [`OrphanPolicy::reconcile_on_boot`] — whether the boot sweep runs at
//!   all. Defaults **enabled** (`true`) — a knob defaulting off would ship
//!   this block's entire purpose behind a flag nobody sets, a deliberate
//!   exception to the behavior-stable-default half of standing rule 6; this
//!   block's whole point is the behavior change.
//! - [`OrphanPolicy::stale_run_alarm_secs`] — how many seconds a run may sit
//!   `running`/`suspended` past its `updated_at` before the stale-run alarm
//!   enqueues an operator item for it. Defaults to `3600` (one hour).
//! - [`OrphanPolicy::orphan_item_priority`] — the `effective_priority` a
//!   reconciled-orphan or stale-run alarm item carries into the
//!   [`crate::operator::queue::OperatorQueue`], mirroring
//!   `operator::failure::FailureNotifyPolicy::failure_item_priority`'s
//!   convention exactly.
//! - [`OrphanPolicy::orphan_scan_limit`] — the hard bound on how many
//!   candidate rows one boot sweep loads from `engine-store`'s
//!   `list_orphan_candidates` query, so a first sweep over a long-lived
//!   database cannot load an unbounded result set into memory. Defaults to
//!   `200`.
//!
//! All four are behavior-stable in the sense that the built-in default is
//! what an unconfigured run gets, and all four are set explicitly in every
//! named profile (`baseline`, `cheap-fast`, `thorough`) — a knob absent from
//! the profile bundles is a knob nobody will find.

use serde::{Deserialize, Serialize};

use crate::node::NodeError;
use crate::policy::{merge_opt, Policy, PolicyConfigSource};

/// The `harness.json` section key this policy's knobs live under
/// (`orphan_recovery.policy` / `orphan_recovery.profiles`).
const WORKFLOW_KEY: &str = "orphan_recovery";

/// The fully-resolved, per-run orphan-recovery policy. See the module docs
/// for each knob's meaning and default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrphanPolicy {
    /// Whether the boot sweep reconciles orphaned runs at all. Defaults
    /// `true` — see the module docs on why this is a deliberate exception
    /// to the behavior-stable-default convention.
    pub reconcile_on_boot: bool,
    /// Seconds a `running`/`suspended` run may sit past its `updated_at`
    /// before the stale-run alarm enqueues an operator item for it.
    pub stale_run_alarm_secs: u64,
    /// The `effective_priority` a reconciled-orphan or stale-run alarm item
    /// carries into the operator queue.
    pub orphan_item_priority: i32,
    /// Hard bound on how many candidate rows one boot sweep loads from the
    /// store's orphan-candidate query.
    pub orphan_scan_limit: i64,
}

impl Default for OrphanPolicy {
    /// Behavior-stable baseline for every knob except `reconcile_on_boot`,
    /// which is deliberately enabled by default. See the module docs.
    fn default() -> Self {
        Self {
            reconcile_on_boot: true,
            stale_run_alarm_secs: 3600,
            orphan_item_priority: 0,
            orphan_scan_limit: 200,
        }
    }
}

/// All-optional mirror of [`OrphanPolicy`] used by the override layers
/// (`harness.json`'s `orphan_recovery.policy`, a named `profile`, and a
/// per-run event's `policy` field).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialOrphanPolicy {
    pub reconcile_on_boot: Option<bool>,
    pub stale_run_alarm_secs: Option<u64>,
    pub orphan_item_priority: Option<i32>,
    pub orphan_scan_limit: Option<i64>,
}

impl Policy for OrphanPolicy {
    type Partial = PartialOrphanPolicy;

    /// Apply one override layer on top of `self`. Each knob overrides
    /// independently — a partial that sets only one field leaves the other
    /// three untouched.
    fn apply(self, over: &PartialOrphanPolicy) -> Self {
        Self {
            reconcile_on_boot: merge_opt(self.reconcile_on_boot, over.reconcile_on_boot),
            stale_run_alarm_secs: merge_opt(self.stale_run_alarm_secs, over.stale_run_alarm_secs),
            orphan_item_priority: merge_opt(self.orphan_item_priority, over.orphan_item_priority),
            orphan_scan_limit: merge_opt(self.orphan_scan_limit, over.orphan_scan_limit),
        }
    }
}

/// The explicit control profile: the documented defaults, spelled out
/// explicitly (rather than left all-`None`) so selecting `profile:
/// "baseline"` is a legible, self-documenting no-op against the built-in
/// default.
#[must_use]
pub fn baseline() -> PartialOrphanPolicy {
    PartialOrphanPolicy {
        reconcile_on_boot: Some(true),
        stale_run_alarm_secs: Some(3600),
        orphan_item_priority: Some(0),
        orphan_scan_limit: Some(200),
    }
}

/// Cost/latency floor: the sweep still runs (disabling it is not a speed
/// dial — it is the block's whole purpose), but the alarm fires sooner and
/// alarm items enqueue at a lower priority so a speed-optimized deployment
/// surfaces stuck runs fast without a burst crowding out other queued
/// operator items.
#[must_use]
pub fn cheap_fast() -> PartialOrphanPolicy {
    PartialOrphanPolicy {
        reconcile_on_boot: Some(true),
        stale_run_alarm_secs: Some(1800),
        orphan_item_priority: Some(-5),
        orphan_scan_limit: Some(100),
    }
}

/// Quality ceiling: a longer alarm grace period (fewer false alarms on
/// intentionally long-running work), a wider scan limit for a thorough
/// sweep, and alarm items enqueue at a higher priority so orphan/stale-run
/// items surface ahead of other queued operator items.
#[must_use]
pub fn thorough() -> PartialOrphanPolicy {
    PartialOrphanPolicy {
        reconcile_on_boot: Some(true),
        stale_run_alarm_secs: Some(7200),
        orphan_item_priority: Some(5),
        orphan_scan_limit: Some(500),
    }
}

/// Resolve a built-in profile bundle by its kebab-case name. Returns `None`
/// for any name that isn't one of the three canonical profiles.
#[must_use]
pub fn profile_by_name(name: &str) -> Option<PartialOrphanPolicy> {
    match name {
        "baseline" => Some(baseline()),
        "cheap-fast" => Some(cheap_fast()),
        "thorough" => Some(thorough()),
        _ => None,
    }
}

/// Read `orphan_recovery.policy` (a [`PartialOrphanPolicy`]) out of the file
/// addressed by `source`. Delegates to the generic
/// `crate::policy::read_harness_policy_defaults_from`, parameterized by
/// [`WORKFLOW_KEY`].
pub fn read_harness_policy_defaults_from(
    source: &PolicyConfigSource,
) -> Result<Option<PartialOrphanPolicy>, NodeError> {
    crate::policy::read_harness_policy_defaults_from(source, WORKFLOW_KEY)
}

/// Resolve a named `profile` to a [`PartialOrphanPolicy`] bundle, preferring
/// a `harness.json` `orphan_recovery.profiles[name]` entry (read via
/// `source`) over the built-in [`profile_by_name`]. Returns `Ok(None)` when
/// `profile_name` is `None`, and `Err` when a name is given but resolves to
/// neither source (no silent no-op). Delegates to the generic
/// `crate::policy::resolve_profile_from`, parameterized by [`WORKFLOW_KEY`]
/// and [`profile_by_name`].
pub fn resolve_profile_from(
    profile_name: Option<&str>,
    source: &PolicyConfigSource,
) -> Result<Option<PartialOrphanPolicy>, NodeError> {
    crate::policy::resolve_profile_from(profile_name, source, WORKFLOW_KEY, profile_by_name)
}

/// Resolve the four-layer [`OrphanPolicy`] for a run: `event_override` (a
/// per-run `policy` field), the resolved `profile_name` bundle, `source`'s
/// `orphan_recovery.policy` defaults, and the built-in default, high->low
/// precedence via `crate::policy::resolve`. A [`PolicyConfigSource::Builtin`]
/// source resolves successfully with no filesystem access at all — the case
/// a hot boot-sweep or alarm path needs.
pub fn resolve_policy_for_run_from(
    source: &PolicyConfigSource,
    profile_name: Option<&str>,
    event_override: Option<&PartialOrphanPolicy>,
) -> Result<OrphanPolicy, NodeError> {
    let harness_defaults = read_harness_policy_defaults_from(source)?;
    let profile = resolve_profile_from(profile_name, source)?;
    Ok(crate::policy::resolve(
        OrphanPolicy::default(),
        harness_defaults.as_ref(),
        profile.as_ref(),
        event_override,
    ))
}

/// Serializes the resolved policy so a caller can stamp it into `ctx.nodes`
/// (`RunTelemetry`/`PolicyAggregate` per `CLAUDE.md` standing rule 6) and
/// attribute observed behavior to the setting that caused it.
#[must_use]
pub fn policy_state(policy: &OrphanPolicy) -> serde_json::Value {
    serde_json::to_value(policy).unwrap_or_else(|_| serde_json::json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_matches_documented_baseline_except_reconcile_which_is_deliberately_true() {
        let policy = OrphanPolicy::default();
        assert!(policy.reconcile_on_boot);
        assert_eq!(policy.stale_run_alarm_secs, 3600);
        assert_eq!(policy.orphan_item_priority, 0);
        assert_eq!(policy.orphan_scan_limit, 200);
    }

    #[test]
    fn every_named_profile_sets_all_four_knobs_explicitly() {
        for profile in [baseline(), cheap_fast(), thorough()] {
            assert!(profile.reconcile_on_boot.is_some());
            assert!(profile.stale_run_alarm_secs.is_some());
            assert!(profile.orphan_item_priority.is_some());
            assert!(profile.orphan_scan_limit.is_some());
        }
    }

    #[test]
    fn every_named_profile_keeps_reconcile_on_boot_enabled() {
        // Disabling the sweep is not a cost/quality dial — every profile
        // keeps it on; only the alarm timing and priority vary.
        for profile in [baseline(), cheap_fast(), thorough()] {
            assert_eq!(profile.reconcile_on_boot, Some(true));
        }
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
    fn resolve_with_no_overrides_returns_builtin() {
        let resolved = crate::policy::resolve(OrphanPolicy::default(), None, None, None);
        assert_eq!(resolved, OrphanPolicy::default());
    }

    #[test]
    fn harness_default_overrides_builtin_and_leaves_other_knobs_alone() {
        let harness = PartialOrphanPolicy {
            stale_run_alarm_secs: Some(60),
            ..Default::default()
        };
        let resolved = crate::policy::resolve(OrphanPolicy::default(), Some(&harness), None, None);
        assert_eq!(resolved.stale_run_alarm_secs, 60);
        assert_eq!(resolved.orphan_item_priority, 0);
        assert!(resolved.reconcile_on_boot);
    }

    #[test]
    fn profile_beats_harness_defaults() {
        let harness = PartialOrphanPolicy {
            orphan_item_priority: Some(9),
            ..Default::default()
        };
        let profile = PartialOrphanPolicy {
            orphan_item_priority: Some(3),
            ..Default::default()
        };
        let resolved = crate::policy::resolve(
            OrphanPolicy::default(),
            Some(&harness),
            Some(&profile),
            None,
        );
        assert_eq!(resolved.orphan_item_priority, 3);
    }

    #[test]
    fn event_override_beats_profile() {
        let profile = PartialOrphanPolicy {
            orphan_item_priority: Some(3),
            ..Default::default()
        };
        let event = PartialOrphanPolicy {
            orphan_item_priority: Some(1),
            ..Default::default()
        };
        let resolved =
            crate::policy::resolve(OrphanPolicy::default(), None, Some(&profile), Some(&event));
        assert_eq!(resolved.orphan_item_priority, 1);
    }

    #[test]
    fn event_override_flips_reconcile_on_boot_without_resetting_other_knobs() {
        let event = PartialOrphanPolicy {
            reconcile_on_boot: Some(false),
            ..Default::default()
        };
        let resolved = crate::policy::resolve(OrphanPolicy::default(), None, None, Some(&event));
        assert!(!resolved.reconcile_on_boot);
        assert_eq!(resolved.stale_run_alarm_secs, 3600);
        assert_eq!(resolved.orphan_item_priority, 0);
        assert_eq!(resolved.orphan_scan_limit, 200);
    }

    #[test]
    fn resolve_policy_for_run_from_builtin_source_needs_no_filesystem() {
        let resolved = resolve_policy_for_run_from(&PolicyConfigSource::Builtin, None, None)
            .expect("resolve should succeed with no filesystem access");
        assert_eq!(resolved, OrphanPolicy::default());
    }

    #[test]
    fn resolve_policy_for_run_from_applies_named_profile() {
        let resolved =
            resolve_policy_for_run_from(&PolicyConfigSource::Builtin, Some("cheap-fast"), None)
                .expect("resolve should succeed");
        assert_eq!(resolved.stale_run_alarm_secs, 1800);
        assert_eq!(resolved.orphan_item_priority, -5);
        assert_eq!(resolved.orphan_scan_limit, 100);
    }

    #[test]
    fn resolve_policy_for_run_from_unknown_profile_errors() {
        let err =
            resolve_policy_for_run_from(&PolicyConfigSource::Builtin, Some("nonexistent"), None)
                .expect_err("should fail");
        assert!(err.message.contains("unknown profile"));
    }

    #[test]
    fn deserializes_partial_policy_from_harness_json_shape() {
        let json = r#"{ "reconcile_on_boot": false, "stale_run_alarm_secs": 120 }"#;
        let partial: PartialOrphanPolicy =
            serde_json::from_str(json).expect("valid PartialOrphanPolicy JSON");
        assert_eq!(partial.reconcile_on_boot, Some(false));
        assert_eq!(partial.stale_run_alarm_secs, Some(120));
    }

    #[test]
    fn partial_policy_round_trips_with_fields_absent() {
        let partial: PartialOrphanPolicy =
            serde_json::from_str("{}").expect("valid empty PartialOrphanPolicy JSON");
        assert_eq!(partial, PartialOrphanPolicy::default());
    }

    #[test]
    fn policy_state_stamps_all_four_resolved_values() {
        let policy = OrphanPolicy::default();
        let state = policy_state(&policy);
        assert_eq!(
            state.get("reconcile_on_boot").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            state.get("stale_run_alarm_secs").and_then(|v| v.as_u64()),
            Some(3600)
        );
        assert_eq!(
            state.get("orphan_item_priority").and_then(|v| v.as_i64()),
            Some(0)
        );
        assert_eq!(
            state.get("orphan_scan_limit").and_then(|v| v.as_i64()),
            Some(200)
        );
    }
}
