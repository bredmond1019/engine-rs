//! `no_match_alarm` (`EN.9.F` task 2) — the consecutive-Unknown counter and
//! the alarm it raises, naming the manifest and digest currently in use.
//!
//! # Why this exists
//!
//! The claude manifest matches three literal UI strings, and Claude Code
//! ships frequently. If a release rewords "Do you want to proceed?", every
//! session classifies [`term_core::detect::AgentState::Unknown`], the
//! Blocked edge never fires, no approval ever surfaces to the operator, and
//! **nothing errors anywhere** — the pipeline just goes quiet. THIS ALARM
//! IS THE ONLY THING STANDING BETWEEN THAT REWORDED STRING AND A SILENTLY
//! DEAD OPERATOR SURFACE. [`NoMatchAlarmTracker`] exists to make that
//! silence loud: after `N` consecutive unmatched captures it raises an
//! [`Alarm`] naming the manifest in use and its digest (task 1's
//! [`super::manifest_source::ResolvedManifest::digest`]) — the digest is
//! what tells an operator whether the manifest they *think* is deployed is
//! the one actually running.
//!
//! # Edge-triggered, not level-triggered
//!
//! [`NoMatchAlarmTracker::record`] fires exactly once per unmatched streak —
//! on the capture where the running count first reaches `N` — and stays
//! silent on every further unmatched capture in the same streak. A
//! successful match resets the counter to zero, so a *sustained* blind spot
//! raises again only after another full streak of `N`. This is deliberate:
//! the alarm is about a sustained blind spot, not a single ambiguous frame,
//! and an operator who has already been paged once for a still-ongoing
//! outage does not need the same page repeated on every subsequent poll
//! tick.
//!
//! # Policy — a knob per CLAUDE.md standing rule 6
//!
//! The threshold is a policy knob, resolved through the same generic
//! four-layer [`crate::policy::resolve`] precedence every other policy
//! surface in this crate uses (per-run `ctx.event.policy` override > a
//! named `profile` bundle > `harness_defaults` >
//! [`NoMatchAlarmPolicy::default`]) — see [`await_node`](super::await_node)
//! for the identical shape this mirrors. The default is behavior-stable and
//! every named profile in [`profiles`] sets it explicitly. Whichever node
//! eventually drives this tracker is expected to stamp the *resolved*
//! threshold into its own `ctx.nodes` result, per standing rule 6's "stamp
//! the resolved value" requirement — this module supplies the policy and
//! the tracker; wiring a `Node` around them is out of this task's scope
//! (`EN.9.F` task 2's `Relevant Files` cover only this module and
//! `mod.rs`).

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use term_core::detect::AgentState;

use crate::policy::{merge_opt, resolve as resolve_policy_layers, Policy};

// ── Policy ───────────────────────────────────────────────────────────────

/// The fully-resolved, per-run no-match-alarm policy: how many consecutive
/// unmatched captures raise an [`Alarm`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoMatchAlarmPolicy {
    pub consecutive_unmatched_threshold: u32,
}

impl Default for NoMatchAlarmPolicy {
    /// The behavior-stable baseline: five consecutive unmatched captures
    /// before the alarm fires. Chosen to absorb a single stray ambiguous
    /// frame or two without paging, while still catching a genuinely dead
    /// manifest within a handful of poll ticks.
    fn default() -> Self {
        Self {
            consecutive_unmatched_threshold: 5,
        }
    }
}

/// All-optional mirror of [`NoMatchAlarmPolicy`] used by the override
/// layers (a node's `harness_defaults`/`profile`, and a per-run
/// `ctx.event.policy` override). A `None` field falls through to the
/// next-lower-precedence layer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialNoMatchAlarmPolicy {
    pub consecutive_unmatched_threshold: Option<u32>,
}

impl Policy for NoMatchAlarmPolicy {
    type Partial = PartialNoMatchAlarmPolicy;

    fn apply(self, over: &PartialNoMatchAlarmPolicy) -> Self {
        Self {
            consecutive_unmatched_threshold: merge_opt(
                self.consecutive_unmatched_threshold,
                over.consecutive_unmatched_threshold,
            ),
        }
    }
}

/// Resolve the four policy layers into one concrete [`NoMatchAlarmPolicy`],
/// high->low precedence: `event_override` beats `profile` beats
/// `harness_defaults` beats [`NoMatchAlarmPolicy::default`]. Delegates to
/// the generic `crate::policy::resolve`.
#[must_use]
pub fn resolve(
    harness_defaults: Option<&PartialNoMatchAlarmPolicy>,
    profile: Option<&PartialNoMatchAlarmPolicy>,
    event_override: Option<&PartialNoMatchAlarmPolicy>,
) -> NoMatchAlarmPolicy {
    resolve_policy_layers(
        NoMatchAlarmPolicy::default(),
        harness_defaults,
        profile,
        event_override,
    )
}

/// Named [`PartialNoMatchAlarmPolicy`] bundles, per CLAUDE.md standing rule
/// 6's "every workflow ships the three named profiles" — `baseline`
/// restates the built-in default verbatim (a legible no-op), `cheap-fast`
/// pages sooner on a shorter streak (cheaper to catch a dead manifest fast
/// than to keep burning captures against it), `thorough` tolerates a longer
/// streak before paging (fewer false alarms from a noisy or slow-settling
/// pane, at the cost of a longer blind spot before the page fires).
pub mod profiles {
    use super::{NoMatchAlarmPolicy, PartialNoMatchAlarmPolicy};

    /// Restates [`NoMatchAlarmPolicy::default`] verbatim — selecting
    /// `"baseline"` must not silently change behavior.
    #[must_use]
    pub fn baseline() -> PartialNoMatchAlarmPolicy {
        let default = NoMatchAlarmPolicy::default();
        PartialNoMatchAlarmPolicy {
            consecutive_unmatched_threshold: Some(default.consecutive_unmatched_threshold),
        }
    }

    /// A short leash — page after 2 consecutive unmatched captures.
    #[must_use]
    pub fn cheap_fast() -> PartialNoMatchAlarmPolicy {
        PartialNoMatchAlarmPolicy {
            consecutive_unmatched_threshold: Some(2),
        }
    }

    /// A long leash — page only after 10 consecutive unmatched captures.
    #[must_use]
    pub fn thorough() -> PartialNoMatchAlarmPolicy {
        PartialNoMatchAlarmPolicy {
            consecutive_unmatched_threshold: Some(10),
        }
    }

    /// Look up one of the three canonical profile names. `None` for any
    /// other name — callers decide whether an unknown name is an error.
    #[must_use]
    pub fn profile_by_name(name: &str) -> Option<PartialNoMatchAlarmPolicy> {
        match name {
            "baseline" => Some(baseline()),
            "cheap-fast" => Some(cheap_fast()),
            "thorough" => Some(thorough()),
            _ => None,
        }
    }
}

// ── Alarm ────────────────────────────────────────────────────────────────

/// Raised by [`NoMatchAlarmTracker::record`] on the capture where the
/// consecutive-unmatched streak first reaches the resolved threshold.
/// Names both the manifest in use and its digest — the digest is what lets
/// an operator confirm the manifest they think is deployed is the one that
/// actually failed to match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alarm {
    /// The detect manifest's `name` field (e.g. `"claude"`).
    pub manifest_name: String,
    /// Hex-encoded SHA-256 of the manifest's raw TOML source — see
    /// [`super::manifest_source::ResolvedManifest::digest`].
    pub manifest_digest: String,
    /// The consecutive-unmatched count at the moment this alarm fired.
    /// Equal to the resolved [`NoMatchAlarmPolicy::consecutive_unmatched_threshold`]
    /// by construction (the alarm fires on the crossing capture, not
    /// after).
    pub consecutive_unmatched: u32,
}

// ── Tracker ──────────────────────────────────────────────────────────────

/// Tracks a running consecutive-unmatched-capture count and raises exactly
/// one [`Alarm`] per streak — see the module doc for the edge-triggered
/// semantics and why they're the right shape.
///
/// Interior mutability ([`Mutex`]) so one tracker can be shared behind an
/// `Arc` across sequential captures the way [`super::manifest_source::ManifestSource`]
/// shares its cache, without requiring callers to hold `&mut`.
pub struct NoMatchAlarmTracker {
    policy: NoMatchAlarmPolicy,
    consecutive_unmatched: Mutex<u32>,
}

impl NoMatchAlarmTracker {
    /// Build a tracker under the given resolved policy, with the counter
    /// starting at zero.
    #[must_use]
    pub fn new(policy: NoMatchAlarmPolicy) -> Self {
        Self {
            policy,
            consecutive_unmatched: Mutex::new(0),
        }
    }

    /// Build a tracker under [`NoMatchAlarmPolicy::default`].
    #[must_use]
    pub fn with_default_policy() -> Self {
        Self::new(NoMatchAlarmPolicy::default())
    }

    /// The resolved policy this tracker is running under.
    #[must_use]
    pub fn policy(&self) -> NoMatchAlarmPolicy {
        self.policy
    }

    /// Record one capture's match outcome for the manifest named
    /// `manifest_name`/`manifest_digest` (task 1's [`super::manifest_source::ResolvedManifest`]
    /// fields — pass the OVERRIDE manifest's name/digest when an override
    /// is active, never the embedded const's, so a raised alarm always
    /// names whichever manifest actually ran the classification).
    ///
    /// `matched` is `true` when the capture's detection matched a rule
    /// (any [`AgentState`] other than [`AgentState::Unknown`]) — `false`
    /// resets the counter to zero and returns `None`. `matched: false`
    /// increments the counter; returns `Some(Alarm)` exactly on the capture
    /// where the counter first reaches the resolved threshold, and `None`
    /// on every other unmatched capture (both before the threshold and
    /// after it, within the same still-unresolved streak).
    pub fn record(
        &self,
        manifest_name: &str,
        manifest_digest: &str,
        matched: bool,
    ) -> Option<Alarm> {
        let mut count = self
            .consecutive_unmatched
            .lock()
            .expect("no-match alarm tracker mutex poisoned");

        if matched {
            *count = 0;
            return None;
        }

        *count += 1;
        if *count == self.policy.consecutive_unmatched_threshold {
            Some(Alarm {
                manifest_name: manifest_name.to_string(),
                manifest_digest: manifest_digest.to_string(),
                consecutive_unmatched: *count,
            })
        } else {
            None
        }
    }

    /// Convenience wrapper over [`Self::record`] for a raw [`AgentState`]:
    /// every state other than [`AgentState::Unknown`] counts as matched.
    pub fn record_state(
        &self,
        manifest_name: &str,
        manifest_digest: &str,
        state: AgentState,
    ) -> Option<Alarm> {
        self.record(manifest_name, manifest_digest, state != AgentState::Unknown)
    }

    /// The current consecutive-unmatched count. Exposed for tests and for
    /// a future node wiring that wants to stamp the live count alongside
    /// the resolved policy (standing rule 6's "stamp the resolved value").
    #[must_use]
    pub fn current_count(&self) -> u32 {
        *self
            .consecutive_unmatched
            .lock()
            .expect("no-match alarm tracker mutex poisoned")
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_threshold_is_five() {
        assert_eq!(
            NoMatchAlarmPolicy::default().consecutive_unmatched_threshold,
            5
        );
    }

    #[test]
    fn n_consecutive_unmatched_raise_exactly_one_alarm_naming_manifest_and_digest() {
        let tracker = NoMatchAlarmTracker::new(NoMatchAlarmPolicy {
            consecutive_unmatched_threshold: 3,
        });

        assert_eq!(tracker.record("claude", "abc123", false), None);
        assert_eq!(tracker.record("claude", "abc123", false), None);
        let alarm = tracker
            .record("claude", "abc123", false)
            .expect("3rd consecutive unmatched capture should raise");

        assert_eq!(alarm.manifest_name, "claude");
        assert_eq!(alarm.manifest_digest, "abc123");
        assert_eq!(alarm.consecutive_unmatched, 3);
    }

    #[test]
    fn n_minus_one_consecutive_unmatched_raise_nothing() {
        let tracker = NoMatchAlarmTracker::new(NoMatchAlarmPolicy {
            consecutive_unmatched_threshold: 5,
        });

        for _ in 0..4 {
            assert_eq!(tracker.record("claude", "abc123", false), None);
        }
        assert_eq!(tracker.current_count(), 4);
    }

    #[test]
    fn an_unmatched_streak_past_the_threshold_does_not_re_fire() {
        let tracker = NoMatchAlarmTracker::new(NoMatchAlarmPolicy {
            consecutive_unmatched_threshold: 2,
        });

        assert_eq!(tracker.record("claude", "abc123", false), None);
        assert!(tracker.record("claude", "abc123", false).is_some());
        // 3rd, 4th, ... unmatched captures in the SAME streak: silent.
        assert_eq!(tracker.record("claude", "abc123", false), None);
        assert_eq!(tracker.record("claude", "abc123", false), None);
    }

    #[test]
    fn a_successful_match_mid_run_resets_the_counter() {
        let tracker = NoMatchAlarmTracker::new(NoMatchAlarmPolicy {
            consecutive_unmatched_threshold: 3,
        });

        assert_eq!(tracker.record("claude", "abc123", false), None);
        assert_eq!(tracker.record("claude", "abc123", false), None);
        // A match resets — the next 2 unmatched captures must NOT raise.
        assert_eq!(tracker.record("claude", "abc123", true), None);
        assert_eq!(tracker.current_count(), 0);
        assert_eq!(tracker.record("claude", "abc123", false), None);
        assert_eq!(tracker.record("claude", "abc123", false), None);

        // A fresh full streak of 3 raises again.
        let alarm = tracker
            .record("claude", "abc123", false)
            .expect("a fresh full streak after reset should raise again");
        assert_eq!(alarm.consecutive_unmatched, 3);
    }

    #[test]
    fn record_state_treats_unknown_as_unmatched_and_everything_else_as_matched() {
        let tracker = NoMatchAlarmTracker::new(NoMatchAlarmPolicy {
            consecutive_unmatched_threshold: 2,
        });

        assert_eq!(
            tracker.record_state("claude", "abc123", AgentState::Unknown),
            None
        );
        let alarm = tracker
            .record_state("claude", "abc123", AgentState::Unknown)
            .expect("two consecutive Unknown classifications should raise");
        assert_eq!(alarm.consecutive_unmatched, 2);

        // Working/Idle/Blocked all count as matched and reset the counter.
        assert_eq!(
            tracker.record_state("claude", "abc123", AgentState::Working),
            None
        );
        assert_eq!(tracker.current_count(), 0);
    }

    #[test]
    fn alarm_names_the_override_manifests_digest_when_an_override_is_active() {
        // Simulates task 1's ManifestSource resolving to an override: the
        // caller passes the OVERRIDE's name/digest, never the embedded
        // const's — the tracker itself is manifest-agnostic and simply
        // echoes back whatever identity it was given.
        let tracker = NoMatchAlarmTracker::new(NoMatchAlarmPolicy {
            consecutive_unmatched_threshold: 1,
        });

        let embedded_digest = "embedded-digest-would-be-this";
        let override_digest = "override-digest-actually-running";

        let alarm = tracker
            .record("claude-override", override_digest, false)
            .expect("threshold of 1 should raise immediately");

        assert_eq!(alarm.manifest_digest, override_digest);
        assert_ne!(alarm.manifest_digest, embedded_digest);
        assert_eq!(alarm.manifest_name, "claude-override");
    }

    #[test]
    fn resolve_layers_precedence_matches_the_generic_policy_resolver() {
        let harness_defaults = PartialNoMatchAlarmPolicy {
            consecutive_unmatched_threshold: Some(7),
        };
        let profile = PartialNoMatchAlarmPolicy {
            consecutive_unmatched_threshold: Some(4),
        };
        let event_override = PartialNoMatchAlarmPolicy {
            consecutive_unmatched_threshold: Some(1),
        };

        // No layers set -> built-in default.
        assert_eq!(resolve(None, None, None), NoMatchAlarmPolicy::default());
        // harness_defaults only.
        assert_eq!(
            resolve(Some(&harness_defaults), None, None).consecutive_unmatched_threshold,
            7
        );
        // profile beats harness_defaults.
        assert_eq!(
            resolve(Some(&harness_defaults), Some(&profile), None).consecutive_unmatched_threshold,
            4
        );
        // event_override beats everything.
        assert_eq!(
            resolve(
                Some(&harness_defaults),
                Some(&profile),
                Some(&event_override)
            )
            .consecutive_unmatched_threshold,
            1
        );
    }

    #[test]
    fn all_three_named_profiles_resolve_to_distinct_thresholds() {
        let baseline = profiles::profile_by_name("baseline").expect("baseline profile exists");
        let cheap_fast =
            profiles::profile_by_name("cheap-fast").expect("cheap-fast profile exists");
        let thorough = profiles::profile_by_name("thorough").expect("thorough profile exists");

        assert_eq!(
            baseline.consecutive_unmatched_threshold,
            Some(NoMatchAlarmPolicy::default().consecutive_unmatched_threshold)
        );
        assert!(
            cheap_fast.consecutive_unmatched_threshold < baseline.consecutive_unmatched_threshold
        );
        assert!(
            thorough.consecutive_unmatched_threshold > baseline.consecutive_unmatched_threshold
        );
        assert!(profiles::profile_by_name("nonexistent-profile").is_none());
    }
}
