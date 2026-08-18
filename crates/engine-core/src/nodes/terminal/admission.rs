//! `admission` (`EN.9.F` task 3) — a semaphore bounding how many
//! terminal-node runs may be in flight at once.
//!
//! # Why this exists
//!
//! There is no admission control anywhere in the fleet today. This is also
//! the precondition for `EN.10.B`: a workflow that fans out across repos
//! with no admission control breaches the heavy-lane ceiling at machine
//! speed, where a human driver simply reads the lane file and stops.
//!
//! [`AdmissionControl`] bounds terminal-node fan-out specifically. Runs
//! beyond the configured limit **queue** — they neither start nor fail —
//! until an in-flight run releases its permit.
//!
//! Deliberately does **not** touch `ParallelNode`. `ParallelNode` polls
//! branches in-place on one task, so terminal fan-out currently serializes
//! and stalls the worker rather than fork-bombing — harder to diagnose,
//! but not this block's job to fix, and changing it here would change the
//! shape of every existing parallel workflow (standing rule 6: a knob must
//! not change a declared graph's node set).
//!
//! # Policy — a knob per CLAUDE.md standing rule 6
//!
//! The concurrency limit is a policy knob, resolved through the same
//! generic four-layer [`crate::policy::resolve`] precedence every other
//! policy surface in this crate uses (per-run `ctx.event.policy` override >
//! a named `profile` bundle > `harness_defaults` >
//! [`AdmissionPolicy::default`]) — mirrors
//! [`no_match_alarm`](super::no_match_alarm) and
//! [`await_node`](super::await_node). The default is behavior-stable
//! (single-run behaviour is unchanged: nothing queues until concurrent
//! demand exceeds the default limit) and every named profile in
//! [`profiles`] sets it explicitly. Whichever node eventually drives this
//! control is expected to stamp the *resolved* limit into its own
//! `ctx.nodes` result, per standing rule 6's "stamp the resolved value"
//! requirement — this module supplies the policy and the control; wiring a
//! `Node` around it is out of this task's scope.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::policy::{merge_opt, resolve as resolve_policy_layers, Policy};

// ── Policy ───────────────────────────────────────────────────────────────

/// The fully-resolved, per-run admission-control policy: how many
/// terminal-node runs may execute concurrently before further runs queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionPolicy {
    pub max_concurrent_terminal_runs: u32,
}

impl Default for AdmissionPolicy {
    /// The behavior-stable baseline: 8 concurrent terminal-node runs. Set
    /// generously above any single-run workflow's actual demand so the
    /// default leaves existing single-run behaviour completely unchanged —
    /// nothing queues until concurrent demand genuinely exceeds this.
    fn default() -> Self {
        Self {
            max_concurrent_terminal_runs: 8,
        }
    }
}

/// All-optional mirror of [`AdmissionPolicy`] used by the override layers
/// (a node's `harness_defaults`/`profile`, and a per-run `ctx.event.policy`
/// override). A `None` field falls through to the next-lower-precedence
/// layer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialAdmissionPolicy {
    pub max_concurrent_terminal_runs: Option<u32>,
}

impl Policy for AdmissionPolicy {
    type Partial = PartialAdmissionPolicy;

    fn apply(self, over: &PartialAdmissionPolicy) -> Self {
        Self {
            max_concurrent_terminal_runs: merge_opt(
                self.max_concurrent_terminal_runs,
                over.max_concurrent_terminal_runs,
            ),
        }
    }
}

/// Resolve the four policy layers into one concrete [`AdmissionPolicy`],
/// high->low precedence: `event_override` beats `profile` beats
/// `harness_defaults` beats [`AdmissionPolicy::default`]. Delegates to the
/// generic `crate::policy::resolve`.
#[must_use]
pub fn resolve(
    harness_defaults: Option<&PartialAdmissionPolicy>,
    profile: Option<&PartialAdmissionPolicy>,
    event_override: Option<&PartialAdmissionPolicy>,
) -> AdmissionPolicy {
    resolve_policy_layers(
        AdmissionPolicy::default(),
        harness_defaults,
        profile,
        event_override,
    )
}

/// Named [`PartialAdmissionPolicy`] bundles, per CLAUDE.md standing rule
/// 6's "every workflow ships the three named profiles" — `baseline`
/// restates the built-in default verbatim (a legible no-op), `cheap-fast`
/// admits fewer concurrent runs (cheaper to bound resource usage tightly
/// when cost/latency matter more than throughput), `thorough` admits more
/// (higher throughput for a run that values completing a large burst
/// quickly over a tight resource ceiling).
pub mod profiles {
    use super::{AdmissionPolicy, PartialAdmissionPolicy};

    /// Restates [`AdmissionPolicy::default`] verbatim — selecting
    /// `"baseline"` must not silently change behavior.
    #[must_use]
    pub fn baseline() -> PartialAdmissionPolicy {
        let default = AdmissionPolicy::default();
        PartialAdmissionPolicy {
            max_concurrent_terminal_runs: Some(default.max_concurrent_terminal_runs),
        }
    }

    /// A tight ceiling — at most 2 concurrent terminal-node runs.
    #[must_use]
    pub fn cheap_fast() -> PartialAdmissionPolicy {
        PartialAdmissionPolicy {
            max_concurrent_terminal_runs: Some(2),
        }
    }

    /// A generous ceiling — up to 32 concurrent terminal-node runs.
    #[must_use]
    pub fn thorough() -> PartialAdmissionPolicy {
        PartialAdmissionPolicy {
            max_concurrent_terminal_runs: Some(32),
        }
    }

    /// Look up one of the three canonical profile names. `None` for any
    /// other name — callers decide whether an unknown name is an error.
    #[must_use]
    pub fn profile_by_name(name: &str) -> Option<PartialAdmissionPolicy> {
        match name {
            "baseline" => Some(baseline()),
            "cheap-fast" => Some(cheap_fast()),
            "thorough" => Some(thorough()),
            _ => None,
        }
    }
}

// ── Admission control ───────────────────────────────────────────────────

/// A held admission permit. Dropping it releases the slot back to the
/// [`AdmissionControl`] that issued it, admitting the next queued run.
pub struct AdmissionPermit {
    #[allow(dead_code)]
    permit: OwnedSemaphorePermit,
}

/// A semaphore bounding how many terminal-node runs may be in flight at
/// once, under a resolved [`AdmissionPolicy`].
///
/// Backed by [`tokio::sync::Semaphore`], which is already fair FIFO-queued
/// and cooperates with cancellation — a caller awaiting [`Self::acquire`]
/// that gets dropped (e.g. the run is cancelled) simply never occupies a
/// slot, exactly the "queue rather than start" semantics this control
/// needs.
///
/// Cheaply cloneable / shareable behind an `Arc` across concurrent
/// terminal-node runs the way [`super::manifest_source::ManifestSource`]
/// and [`super::no_match_alarm::NoMatchAlarmTracker`] are shared.
#[derive(Clone)]
pub struct AdmissionControl {
    policy: AdmissionPolicy,
    semaphore: Arc<Semaphore>,
}

impl AdmissionControl {
    /// Build an admission control under the given resolved policy.
    #[must_use]
    pub fn new(policy: AdmissionPolicy) -> Self {
        Self {
            policy,
            semaphore: Arc::new(Semaphore::new(policy.max_concurrent_terminal_runs as usize)),
        }
    }

    /// Build an admission control under [`AdmissionPolicy::default`].
    #[must_use]
    pub fn with_default_policy() -> Self {
        Self::new(AdmissionPolicy::default())
    }

    /// The resolved policy this control is running under.
    #[must_use]
    pub fn policy(&self) -> AdmissionPolicy {
        self.policy
    }

    /// The number of permits currently available (i.e. not held by an
    /// in-flight run). Exposed for tests and telemetry.
    #[must_use]
    pub fn available_permits(&self) -> usize {
        self.semaphore.available_permits()
    }

    /// Acquire one admission slot, queueing (awaiting) until one is free
    /// when the limit is already saturated. The returned [`AdmissionPermit`]
    /// holds the slot until dropped — drop it (or let it go out of scope)
    /// when the terminal-node run completes to admit the next queued run.
    ///
    /// # Panics
    ///
    /// Panics if the underlying semaphore has been closed, which this type
    /// never does — [`Semaphore::close`] is never called anywhere in this
    /// module, so this is unreachable in practice.
    pub async fn acquire(&self) -> AdmissionPermit {
        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("admission semaphore should never be closed");
        AdmissionPermit { permit }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[test]
    fn default_policy_limit_is_eight() {
        assert_eq!(AdmissionPolicy::default().max_concurrent_terminal_runs, 8);
    }

    #[test]
    fn profiles_set_the_knob_explicitly() {
        assert_eq!(
            profiles::baseline().max_concurrent_terminal_runs,
            Some(AdmissionPolicy::default().max_concurrent_terminal_runs)
        );
        assert_eq!(profiles::cheap_fast().max_concurrent_terminal_runs, Some(2));
        assert_eq!(profiles::thorough().max_concurrent_terminal_runs, Some(32));
        assert!(profiles::profile_by_name("nonexistent").is_none());
    }

    #[test]
    fn resolve_precedence_event_beats_profile_beats_harness_beats_default() {
        let harness = PartialAdmissionPolicy {
            max_concurrent_terminal_runs: Some(4),
        };
        let profile = PartialAdmissionPolicy {
            max_concurrent_terminal_runs: Some(16),
        };
        let event = PartialAdmissionPolicy {
            max_concurrent_terminal_runs: Some(1),
        };

        assert_eq!(resolve(None, None, None), AdmissionPolicy::default());
        assert_eq!(
            resolve(Some(&harness), None, None).max_concurrent_terminal_runs,
            4
        );
        assert_eq!(
            resolve(Some(&harness), Some(&profile), None).max_concurrent_terminal_runs,
            16
        );
        assert_eq!(
            resolve(Some(&harness), Some(&profile), Some(&event)).max_concurrent_terminal_runs,
            1
        );
    }

    #[tokio::test]
    async fn default_limit_leaves_a_single_run_unblocked() {
        let control = AdmissionControl::with_default_policy();
        assert_eq!(
            control.available_permits(),
            AdmissionPolicy::default().max_concurrent_terminal_runs as usize
        );

        // A single acquire under the default (8) limit must not block —
        // existing single-run behaviour is unchanged.
        let permit = tokio::time::timeout(Duration::from_millis(200), control.acquire())
            .await
            .expect("single acquire under the default limit must not block");
        assert_eq!(
            control.available_permits(),
            AdmissionPolicy::default().max_concurrent_terminal_runs as usize - 1
        );
        drop(permit);
        assert_eq!(
            control.available_permits(),
            AdmissionPolicy::default().max_concurrent_terminal_runs as usize
        );
    }

    #[tokio::test]
    async fn runs_beyond_the_limit_queue_rather_than_start() {
        let control = AdmissionControl::new(AdmissionPolicy {
            max_concurrent_terminal_runs: 1,
        });

        let first = control.acquire().await;
        assert_eq!(control.available_permits(), 0);

        // A second acquire under a saturated limit of 1 must NOT resolve
        // until the first permit is released — it queues, it does not
        // start and it does not fail.
        let control2 = control.clone();
        let second_acquired = Arc::new(AtomicUsize::new(0));
        let second_acquired_writer = second_acquired.clone();
        let second_task = tokio::spawn(async move {
            let _permit = control2.acquire().await;
            second_acquired_writer.store(1, Ordering::SeqCst);
        });

        // Give the spawned task every opportunity to (wrongly) proceed
        // before the first permit is released.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            second_acquired.load(Ordering::SeqCst),
            0,
            "a run beyond the limit must queue, not start"
        );

        drop(first);
        second_task
            .await
            .expect("queued run should complete once the permit is released");
        assert_eq!(second_acquired.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn observed_concurrency_never_exceeds_the_configured_limit_under_a_burst() {
        const LIMIT: usize = 3;
        const BURST: usize = 20;

        let control = AdmissionControl::new(AdmissionPolicy {
            max_concurrent_terminal_runs: LIMIT as u32,
        });

        let current = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::with_capacity(BURST);
        for _ in 0..BURST {
            let control = control.clone();
            let current = current.clone();
            let peak = peak.clone();
            handles.push(tokio::spawn(async move {
                let _permit = control.acquire().await;
                let now = current.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                // Hold the slot briefly so overlapping acquires are likely
                // to actually race, not just serialize by scheduling luck.
                tokio::time::sleep(Duration::from_millis(10)).await;
                current.fetch_sub(1, Ordering::SeqCst);
            }));
        }

        for handle in handles {
            handle.await.expect("every burst run should complete");
        }

        assert!(
            peak.load(Ordering::SeqCst) <= LIMIT,
            "observed concurrency {} exceeded the configured limit {}",
            peak.load(Ordering::SeqCst),
            LIMIT
        );
        assert_eq!(control.available_permits(), LIMIT);
    }

    #[tokio::test]
    async fn releasing_a_permit_admits_the_next_queued_run() {
        let control = AdmissionControl::new(AdmissionPolicy {
            max_concurrent_terminal_runs: 1,
        });

        let first = control.acquire().await;

        let control2 = control.clone();
        let admitted = tokio::spawn(async move { control2.acquire().await });

        tokio::time::sleep(Duration::from_millis(20)).await;
        // Not yet admitted — permit is still held.
        assert!(!admitted.is_finished());

        drop(first);
        let second_permit = tokio::time::timeout(Duration::from_millis(200), admitted)
            .await
            .expect("second acquire should complete promptly after release")
            .expect("join should succeed");
        assert_eq!(control.available_permits(), 0);
        drop(second_permit);
        assert_eq!(control.available_permits(), 1);
    }
}
