//! SWEEP route — `EN.15.E` task 4: the precedence rules, the refire window, and the
//! `GatedAction` integration that makes what a sweep may DO a function of the permission
//! profile.
//!
//! Mirrors `roadmap_sweep.py`'s `classify_and_route` (:613-732) and `route_non_escalation_diff`
//! (:762). THE PYTHON IS THE ORACLE (Fork 2) — every divergence is named at the point it
//! happens, never silent.
//!
//! ## Precedence, in the Python's own order
//!
//! 1. **Dedup / re-fire gate** — checked BEFORE anything else, for every escalation regardless
//!    of `kind`. `DEFAULT_REFIRE_HOURS` (roadmap_sweep.py:99, 6.0) applies to `blocking`
//!    escalations only: one already routed re-fires after 6h if still unresolved. An `advisory`
//!    escalation never auto-re-fires (:54-60) — this constant is deliberately uncoupled from
//!    every other staleness threshold in the fleet.
//! 2. **`kind == "cross-repo-edit"` always wins** (:660-669) — routed to the owning lane's queue,
//!    NEVER to the operator, regardless of `channel`. Not gated by permission profile: it is a
//!    peer-to-peer relay, never operator-facing, so `GatedAction` has no say over it (and no
//!    acceptance criterion asks for one).
//! 3. **At most one operator notify per pass** (the budget at :810) — covers `notify-ask` and
//!    `wake-session` only; any number of cross-repo-edit peer-queue routes may still happen in
//!    the same pass.
//! 4. `channel: notification` (with well-formed `options`) becomes an [`OperatorTransport`] ask
//!    (`GatedAction::Notify`); `channel: session:<slug>`, or anything malformed/missing, becomes
//!    a lane wake (`GatedAction::WakeLane`) — "fail toward the richer channel, never degrade
//!    downward" (roadmap_sweep.py's own comment at `_read_options_verbatim`'s call site).
//!
//! ## `suppressed_by_profile` — recorded, never dropped
//!
//! A route denied by [`crate::policy::permission::decide`] is NOT silently skipped: it is
//! returned as a normal [`RouteOutcome`] with `suppressed_by_profile: true` and `routed: false`,
//! and — critically — it does NOT consume the per-pass operator budget (nothing was actually
//! sent), so a later, permitted escalation in the same pass can still reach the operator. That
//! audit trail is the whole reason a restrictive profile is safe to use.
//!
//! ## The two injectable seams
//!
//! - [`OperatorTransport`] (already defined in `crate::operator::transport`, `EN.12.J`) is used
//!   verbatim for the `notify-ask` branch — no parallel trait is defined here.
//! - [`LaneWake`] is this module's own seam for the two "nudge a lane" actions
//!   (cross-repo-edit's peer-queue route, and a `wake-session` route) — the Python's equivalent
//!   is `Routers.wake` (`wake_commander`, :588). A production impl over
//!   `crate::coord::write::send` is a later task's concern; every test here injects a recording
//!   fake.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::operator::payload::{OperatorPayload, OperatorResponseOption};
use crate::operator::transport::OperatorTransport;
use crate::operator::{validate, OperatorPayloadLimits};
use crate::policy::permission::{decide, Decision, GatedAction, PermissionProfile};

use super::diff::{parse_iso, DedupEntry, DiscoveryDiff};
use super::snapshot::{escalation_stale, utc_now_ts};

/// `DEFAULT_REFIRE_HOURS` (roadmap_sweep.py:99) — hours before a routed `blocking` escalation
/// re-fires if still unresolved. Deliberately uncoupled from every other staleness threshold in
/// the fleet; see this module's doc comment.
pub const DEFAULT_REFIRE_HOURS: f64 = 6.0;

/// Per-pass operator-notify budget (`roadmap_sweep.py`'s `budget = {"operator_sent": False}`,
/// :813) — shared, mutated in place, across every [`route_escalation`] call in one sweep pass.
/// Covers `notify-ask` and `wake-session` only; a cross-repo-edit peer-queue route never touches
/// it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Budget {
    pub operator_sent: bool,
}

/// The outcome of routing one escalation (or, for [`route_non_escalation_diff`], the bare
/// diff-drift case) — one entry of the stored snapshot's `routed` list
/// (`roadmap_sweep.py`'s per-call return dict).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RouteOutcome {
    /// The escalation's `gate_id`, or `"<missing-gate-id>"` when absent — mirroring the
    /// Python's own default (`escalation.get("gate_id") or "<missing-gate-id>"`). For
    /// [`route_non_escalation_diff`], the sentinel `"<non-escalation-diff>"`.
    pub gate_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
    pub stale: bool,
    /// The Python's `action` string — `"skip-dedup"`, `"route-to-owning-lane"`,
    /// `"skip-operator-budget"`, `"notify-ask"`, `"wake-session"`, or
    /// `"wake-non-escalation-diff"`.
    pub action: String,
    /// Whether this route actually reached its destination this pass. `false` for every
    /// skip/suppress path — a route that is not `routed` is retried on a later sweep, never
    /// dropped.
    pub routed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ts_routed: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// `true` only when [`crate::policy::permission::decide`] denied the `GatedAction` this
    /// route would have performed — recorded, never dropped. See this module's doc comment.
    pub suppressed_by_profile: bool,
}

/// The result of one [`LaneWake::wake`] call — mirrors `wake_commander`'s return dict
/// (`roadmap_sweep.py`:588), trimmed to the one field `classify_and_route` actually reads
/// (`outcome.get("invoked")`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct WakeOutcome {
    pub invoked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Injectable seam for the two "nudge a lane" actions `route_escalation`/
/// `route_non_escalation_diff` perform — a cross-repo-edit escalation's route to its owning
/// lane's queue, and a `wake-session` route. Mirrors the Python's injectable `Routers.wake`
/// (`wake_commander`, roadmap_sweep.py:588). A production impl over `crate::coord::write::send`
/// is a later task's concern; every test in this module injects a recording fake.
pub trait LaneWake: Send + Sync {
    fn wake(&self, repo: &str, lane: &str, reason: &str, context: &Value) -> WakeOutcome;
}

/// Inputs to [`route_escalation`] that stay constant across every escalation in one sweep pass
/// — the Python's own `now`/`current_sha`/`refire_hours` parameters to `classify_and_route`,
/// plus the permission profile this pass resolves under (an engine-rs addition; the Python has
/// no profile layer).
#[derive(Debug, Clone, Copy)]
pub struct RouteInputs<'a> {
    pub now: DateTime<Utc>,
    pub current_sha: Option<&'a str>,
    pub refire_hours: f64,
    pub profile: PermissionProfile,
}

fn str_field(rec: &Value, name: &str) -> Option<String> {
    rec.get(name).and_then(Value::as_str).map(String::from)
}

/// `roadmap_sweep.py`'s `_is_session_channel` (:734).
fn is_session_channel(channel: Option<&str>) -> bool {
    channel.is_some_and(|c| c.starts_with("session:") && c.len() > "session:".len())
}

/// `roadmap_sweep.py`'s `_read_options_verbatim` (:738) — a `notification`-channel escalation's
/// `options` field read verbatim, never composed or reordered. Returns `None` (never an
/// invented pair) on anything but a well-formed 2-3-entry list of `{key, label}` objects, so the
/// caller falls through to the session route instead.
fn read_options_verbatim(raw: Option<&Value>) -> Option<Vec<OperatorResponseOption>> {
    let arr = raw?.as_array()?;
    if !(2..=3).contains(&arr.len()) {
        return None;
    }
    let mut options = Vec::with_capacity(arr.len());
    for opt in arr {
        let obj = opt.as_object()?;
        let key = obj.get("key")?.as_str()?;
        let label = obj.get("label")?.as_str()?;
        if key.is_empty() || label.is_empty() {
            return None;
        }
        options.push(OperatorResponseOption::new(key, label));
    }
    Some(options)
}

/// Whether a routed-once `gate_id` is due to re-fire this pass — the dedup/refire gate at the
/// top of `classify_and_route` (roadmap_sweep.py:636-649). `None` (no prior route on record) is
/// always due — that is the ordinary "never routed yet" path, not a refire at all.
fn refire_due(
    prior: Option<&DedupEntry>,
    now: DateTime<Utc>,
    severity: Option<&str>,
    refire_hours: f64,
) -> bool {
    let Some(prior) = prior else {
        return true;
    };
    let Some(prior_ts) = parse_iso(prior.ts.as_deref()) else {
        // Unparsable/absent prior timestamp: the Python's `_parse_iso` returning `None` leaves
        // `prior_ts` as `None`, and the surrounding `if prior_ts is not None:` guard is simply
        // skipped — no skip-dedup entry is returned, so routing proceeds as if never routed.
        return true;
    };
    let age_hours = (now - prior_ts).num_seconds() as f64 / 3600.0;
    severity == Some("blocking") && age_hours >= refire_hours
}

/// Route ONE escalation record per the precedence rules above — `roadmap_sweep.py`'s
/// `classify_and_route` (:613), minus the Python's `dry_run` flag (a CLI concern, `BA.25.D`,
/// out of scope here).
#[must_use]
pub async fn route_escalation(
    escalation: &Value,
    dedup_history: &BTreeMap<String, DedupEntry>,
    inputs: RouteInputs<'_>,
    budget: &mut Budget,
    transport: &dyn OperatorTransport,
    waker: &dyn LaneWake,
) -> RouteOutcome {
    let gate_id = str_field(escalation, "gate_id").unwrap_or_else(|| "<missing-gate-id>".into());
    let kind = str_field(escalation, "kind");
    let channel = str_field(escalation, "channel");
    let severity = str_field(escalation, "severity");
    let stale = escalation_stale(escalation, inputs.current_sha);

    // --- dedup / re-fire gate, checked for EVERY escalation regardless of kind ----------------
    let prior = dedup_history.get(&gate_id);
    if prior.is_some() && !refire_due(prior, inputs.now, severity.as_deref(), inputs.refire_hours) {
        let age_hours = prior
            .and_then(|p| parse_iso(p.ts.as_deref()))
            .map(|ts| (inputs.now - ts).num_seconds() as f64 / 3600.0);
        return RouteOutcome {
            gate_id,
            kind,
            channel,
            severity: severity.clone(),
            stale,
            action: "skip-dedup".to_string(),
            routed: false,
            ts_routed: None,
            reason: Some(format!(
                "already routed {} ago (< {}h refire threshold, severity={:?})",
                age_hours
                    .map(|h| format!("{h:.2}h"))
                    .unwrap_or_else(|| "an unknown time".to_string()),
                inputs.refire_hours,
                severity,
            )),
            suppressed_by_profile: false,
        };
    }

    let ts_routed = Some(utc_now_ts(inputs.now));
    let repo = str_field(escalation, "repo").unwrap_or_else(|| "<unknown>".into());
    let lane = str_field(escalation, "lane").unwrap_or_else(|| "main".into());
    let summary = str_field(escalation, "summary").unwrap_or_default();
    let stale_note = if stale {
        " [STALE -- verified_at_sha is behind current HEAD; re-verify before acting, do not act on this as a live fact]"
    } else {
        ""
    };

    // --- 1. cross-repo-edit: NEVER the operator, regardless of channel, never budget/profile-gated
    if kind.as_deref() == Some("cross-repo-edit") {
        let outcome = waker.wake(&repo, &lane, "cross-repo-edit", escalation);
        return RouteOutcome {
            gate_id,
            kind,
            channel,
            severity,
            stale,
            action: "route-to-owning-lane".to_string(),
            routed: outcome.invoked,
            ts_routed,
            reason: outcome.error,
            suppressed_by_profile: false,
        };
    }

    // --- operator-facing budget: at most one per sweep ----------------------------------------
    if budget.operator_sent {
        return RouteOutcome {
            gate_id,
            kind,
            channel,
            severity,
            stale,
            action: "skip-operator-budget".to_string(),
            routed: false,
            ts_routed,
            reason: Some("operator already notified once this sweep -- retry next sweep".into()),
            suppressed_by_profile: false,
        };
    }

    // --- 2. channel: notification -> OperatorTransport ask (GatedAction::Notify) --------------
    if channel.as_deref() == Some("notification") {
        if let Some(options) = read_options_verbatim(escalation.get("options")) {
            let payload = OperatorPayload::new(&gate_id, format!("{summary}{stale_note}"), options);
            if let Ok(validated) = validate(payload, &OperatorPayloadLimits::default()) {
                if decide(inputs.profile, GatedAction::Notify) == Decision::Deny {
                    return RouteOutcome {
                        gate_id,
                        kind,
                        channel,
                        severity,
                        stale,
                        action: "notify-ask".to_string(),
                        routed: false,
                        ts_routed,
                        reason: Some(format!(
                            "suppressed by permission profile {:?}: GatedAction::Notify denied",
                            inputs.profile
                        )),
                        suppressed_by_profile: true,
                    };
                }
                budget.operator_sent = true;
                let send_result = transport.send(&validated).await;
                let (routed, reason) = match send_result {
                    Ok(_) => (true, None),
                    Err(err) => (false, Some(err.to_string())),
                };
                return RouteOutcome {
                    gate_id,
                    kind,
                    channel,
                    severity,
                    stale,
                    action: "notify-ask".to_string(),
                    routed,
                    ts_routed,
                    reason,
                    suppressed_by_profile: false,
                };
            }
            // Validation failed (e.g. an oversized summary/label) -- fall through to the
            // session route below, the same "fail toward the richer channel, never degrade
            // downward" rule the Python applies to malformed `options`.
        }
        // options missing/malformed, or payload failed validation -- never invent a decision;
        // fall through to the session route.
    }

    // --- 3/4. channel: session:<slug>, or missing/malformed -> wake-session (GatedAction::WakeLane)
    if decide(inputs.profile, GatedAction::WakeLane) == Decision::Deny {
        return RouteOutcome {
            gate_id,
            kind,
            channel,
            severity,
            stale,
            action: "wake-session".to_string(),
            routed: false,
            ts_routed,
            reason: Some(format!(
                "suppressed by permission profile {:?}: GatedAction::WakeLane denied",
                inputs.profile
            )),
            suppressed_by_profile: true,
        };
    }
    budget.operator_sent = true;
    let slug = if is_session_channel(channel.as_deref()) {
        channel
            .as_deref()
            .and_then(|c| c.split_once(':'))
            .map(|(_, s)| s.to_string())
    } else {
        None
    };
    let wake_outcome = waker.wake(
        &repo,
        slug.as_deref().unwrap_or(&lane),
        "escalation",
        escalation,
    );
    RouteOutcome {
        gate_id,
        kind,
        channel,
        severity,
        stale,
        action: "wake-session".to_string(),
        routed: wake_outcome.invoked,
        ts_routed,
        reason: wake_outcome.error,
        suppressed_by_profile: false,
    }
}

/// Route the bare state/lease/queue/validate-brain drift case — `roadmap_sweep.py`'s
/// `route_non_escalation_diff` (:762). At most ONE such wake per sweep (never one per changed
/// repo); the woken agent gets the whole diff as context and decides whether it rises to an
/// escalation. Gated by `GatedAction::WakeLane`, same as the escalation `wake-session` route —
/// this is also "nudging a lane's session into action", not a peer-queue relay.
#[must_use]
pub fn route_non_escalation_diff(
    diff: &DiscoveryDiff,
    roadmap: &str,
    now: DateTime<Utc>,
    profile: PermissionProfile,
    waker: &dyn LaneWake,
) -> RouteOutcome {
    let ts_routed = Some(utc_now_ts(now));
    if decide(profile, GatedAction::WakeLane) == Decision::Deny {
        return RouteOutcome {
            gate_id: "<non-escalation-diff>".to_string(),
            kind: None,
            channel: None,
            severity: None,
            stale: false,
            action: "wake-non-escalation-diff".to_string(),
            routed: false,
            ts_routed,
            reason: Some(format!(
                "suppressed by permission profile {profile:?}: GatedAction::WakeLane denied"
            )),
            suppressed_by_profile: true,
        };
    }
    let context = serde_json::json!({
        "roadmap": roadmap,
        "diff": diff.non_escalation_summary,
    });
    // No single repo/lane owns a bare drift wake -- use the brain's own commander lane
    // (HQ/main), same fallback `commander_drain.sh` itself defaults to when unaddressed.
    let outcome = waker.wake("brain", "main", "non-escalation-diff", &context);
    RouteOutcome {
        gate_id: "<non-escalation-diff>".to_string(),
        kind: None,
        channel: None,
        severity: None,
        stale: false,
        action: "wake-non-escalation-diff".to_string(),
        routed: outcome.invoked,
        ts_routed,
        reason: outcome.error,
        suppressed_by_profile: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operator::transport::NoopTransport;
    use std::sync::Mutex;

    /// A `LaneWake` fake that records every call and always reports `invoked: true` — mirrors
    /// the Python tests' own fake `Routers.wake`.
    #[derive(Default)]
    struct RecordingWaker {
        calls: Mutex<Vec<(String, String, String)>>,
    }

    impl RecordingWaker {
        fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    impl LaneWake for RecordingWaker {
        fn wake(&self, repo: &str, lane: &str, reason: &str, _context: &Value) -> WakeOutcome {
            self.calls.lock().unwrap().push((
                repo.to_string(),
                lane.to_string(),
                reason.to_string(),
            ));
            WakeOutcome {
                invoked: true,
                error: None,
            }
        }
    }

    fn escalation(fields: serde_json::Value) -> Value {
        fields
    }

    fn inputs(now: DateTime<Utc>, profile: PermissionProfile) -> RouteInputs<'static> {
        RouteInputs {
            now,
            current_sha: Some("abc1234"),
            refire_hours: DEFAULT_REFIRE_HOURS,
            profile,
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-08T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    // -----------------------------------------------------------------------------------------
    // AC: cross-repo-edit always wins, regardless of channel, and is never budget/profile-gated
    // -----------------------------------------------------------------------------------------

    #[tokio::test]
    async fn cross_repo_edit_routes_to_owning_lane_even_when_channel_says_notification() {
        let rec = escalation(serde_json::json!({
            "gate_id": "G1", "kind": "cross-repo-edit", "channel": "notification",
            "repo": "engine-rs", "lane": "engine-rs", "severity": "blocking",
            "options": [{"key": "a", "label": "A"}, {"key": "b", "label": "B"}],
        }));
        let dedup = BTreeMap::new();
        let mut budget = Budget::default();
        let transport = NoopTransport;
        let waker = RecordingWaker::default();
        let outcome = route_escalation(
            &rec,
            &dedup,
            inputs(now(), PermissionProfile::Standard),
            &mut budget,
            &transport,
            &waker,
        )
        .await;
        assert_eq!(outcome.action, "route-to-owning-lane");
        assert!(outcome.routed);
        assert_eq!(waker.call_count(), 1);
        assert!(
            !budget.operator_sent,
            "cross-repo-edit must never touch the operator budget"
        );
    }

    #[tokio::test]
    async fn cross_repo_edit_peer_queue_routes_are_not_limited_by_the_one_notify_budget() {
        let dedup = BTreeMap::new();
        let mut budget = Budget {
            operator_sent: true,
        };
        let transport = NoopTransport;
        let waker = RecordingWaker::default();
        for gate_id in ["G1", "G2", "G3"] {
            let rec = escalation(serde_json::json!({
                "gate_id": gate_id, "kind": "cross-repo-edit", "repo": "engine-rs", "lane": "engine-rs",
            }));
            let outcome = route_escalation(
                &rec,
                &dedup,
                inputs(now(), PermissionProfile::Standard),
                &mut budget,
                &transport,
                &waker,
            )
            .await;
            assert_eq!(outcome.action, "route-to-owning-lane");
            assert!(outcome.routed);
        }
        assert_eq!(waker.call_count(), 3);
    }

    // -----------------------------------------------------------------------------------------
    // AC: at most one operator notify per pass
    // -----------------------------------------------------------------------------------------

    #[tokio::test]
    async fn three_notify_worthy_escalations_produce_exactly_one_operator_ask() {
        let dedup = BTreeMap::new();
        let mut budget = Budget::default();
        let transport = NoopTransport;
        let waker = RecordingWaker::default();
        let mut asks = 0;
        for gate_id in ["G1", "G2", "G3"] {
            let rec = escalation(serde_json::json!({
                "gate_id": gate_id, "kind": "advisory", "channel": "notification",
                "severity": "advisory", "summary": "something happened",
                "options": [{"key": "ack", "label": "Ack"}, {"key": "skip", "label": "Skip"}],
            }));
            let outcome = route_escalation(
                &rec,
                &dedup,
                inputs(now(), PermissionProfile::Standard),
                &mut budget,
                &transport,
                &waker,
            )
            .await;
            if outcome.action == "notify-ask" && outcome.routed {
                asks += 1;
            }
        }
        assert_eq!(asks, 1, "at most one operator notify per pass");
        assert!(budget.operator_sent);
    }

    // -----------------------------------------------------------------------------------------
    // AC: refire — blocking re-fires after 6h, advisory never
    // -----------------------------------------------------------------------------------------

    #[tokio::test]
    async fn a_blocking_escalation_still_unresolved_after_6h_refires() {
        let mut dedup = BTreeMap::new();
        dedup.insert(
            "G1".to_string(),
            DedupEntry {
                ts: Some("2026-09-08T00:00:00Z".to_string()),
                severity: Some("blocking".to_string()),
            },
        );
        let rec = escalation(serde_json::json!({
            "gate_id": "G1", "kind": "advisory", "channel": "session:engine-rs",
            "severity": "blocking",
        }));
        let mut budget = Budget::default();
        let transport = NoopTransport;
        let waker = RecordingWaker::default();
        // now is 12:00, prior routed at 00:00 -> 12h elapsed, past the 6h threshold.
        // Unrestricted (rather than Standard) so the refire itself, not a profile suppression,
        // is what this test is proving.
        let outcome = route_escalation(
            &rec,
            &dedup,
            inputs(now(), PermissionProfile::Unrestricted),
            &mut budget,
            &transport,
            &waker,
        )
        .await;
        assert_eq!(outcome.action, "wake-session");
        assert!(outcome.routed);
    }

    #[tokio::test]
    async fn an_advisory_escalation_never_auto_refires() {
        let mut dedup = BTreeMap::new();
        dedup.insert(
            "G1".to_string(),
            DedupEntry {
                ts: Some("2026-09-08T00:00:00Z".to_string()),
                severity: Some("advisory".to_string()),
            },
        );
        let rec = escalation(serde_json::json!({
            "gate_id": "G1", "kind": "advisory", "channel": "session:engine-rs",
            "severity": "advisory",
        }));
        let mut budget = Budget::default();
        let transport = NoopTransport;
        let waker = RecordingWaker::default();
        let outcome = route_escalation(
            &rec,
            &dedup,
            inputs(now(), PermissionProfile::Standard),
            &mut budget,
            &transport,
            &waker,
        )
        .await;
        assert_eq!(outcome.action, "skip-dedup");
        assert!(!outcome.routed);
    }

    // -----------------------------------------------------------------------------------------
    // AC: suppressed_by_profile — recorded, never dropped; positive control
    // -----------------------------------------------------------------------------------------

    #[tokio::test]
    async fn under_standard_a_wake_lane_route_is_recorded_suppressed_never_dropped() {
        let dedup = BTreeMap::new();
        let mut budget = Budget::default();
        let transport = NoopTransport;
        let waker = RecordingWaker::default();
        let rec = escalation(serde_json::json!({
            "gate_id": "G1", "kind": "advisory", "channel": "session:engine-rs",
            "severity": "advisory",
        }));
        let outcome = route_escalation(
            &rec,
            &dedup,
            inputs(now(), PermissionProfile::Standard),
            &mut budget,
            &transport,
            &waker,
        )
        .await;
        assert_eq!(outcome.action, "wake-session");
        assert!(!outcome.routed);
        assert!(outcome.suppressed_by_profile);
        assert_eq!(
            waker.call_count(),
            0,
            "a suppressed route must never actually wake anyone"
        );
        assert!(
            !budget.operator_sent,
            "a suppressed route must not consume the operator budget"
        );
    }

    #[tokio::test]
    async fn an_allowed_route_under_the_same_profile_is_not_marked_suppressed() {
        // Positive control for the test above: under Standard, Notify IS allowed, so a
        // notify-ask route must not be marked suppressed.
        let dedup = BTreeMap::new();
        let mut budget = Budget::default();
        let transport = NoopTransport;
        let waker = RecordingWaker::default();
        let rec = escalation(serde_json::json!({
            "gate_id": "G1", "kind": "advisory", "channel": "notification",
            "severity": "advisory", "summary": "something happened",
            "options": [{"key": "ack", "label": "Ack"}, {"key": "skip", "label": "Skip"}],
        }));
        let outcome = route_escalation(
            &rec,
            &dedup,
            inputs(now(), PermissionProfile::Standard),
            &mut budget,
            &transport,
            &waker,
        )
        .await;
        assert_eq!(outcome.action, "notify-ask");
        assert!(outcome.routed);
        assert!(!outcome.suppressed_by_profile);
    }

    #[test]
    fn decide_standard_wake_lane_is_deny_and_decide_locked_notify_is_deny() {
        // Restates the block record's own named acceptance criteria (task 1's, exercised here
        // in this module's own context) so a future edit to `permission.rs` that broke either
        // is caught by this module's own test run too, not only `permission.rs`'s.
        assert_eq!(
            decide(PermissionProfile::Standard, GatedAction::WakeLane),
            Decision::Deny
        );
        assert_eq!(
            decide(PermissionProfile::Locked, GatedAction::Notify),
            Decision::Deny
        );
    }

    // -----------------------------------------------------------------------------------------
    // route_non_escalation_diff
    // -----------------------------------------------------------------------------------------

    #[test]
    fn route_non_escalation_diff_wakes_the_brain_main_lane_when_permitted() {
        let waker = RecordingWaker::default();
        let mut summary = BTreeMap::new();
        summary.insert("engine-rs".to_string(), vec!["blocks".to_string()]);
        let diff = DiscoveryDiff {
            changed: true,
            new_escalations: vec![],
            escalation_count_prev: 0,
            escalation_count_curr: 0,
            non_escalation_diff: true,
            non_escalation_summary: Some(summary),
            first_sweep: false,
        };
        let outcome = route_non_escalation_diff(
            &diff,
            "demo-roadmap",
            now(),
            PermissionProfile::Unrestricted,
            &waker,
        );
        assert_eq!(outcome.action, "wake-non-escalation-diff");
        assert!(outcome.routed);
        assert!(!outcome.suppressed_by_profile);
        assert_eq!(waker.call_count(), 1);
    }

    #[test]
    fn route_non_escalation_diff_is_suppressed_under_locked() {
        let waker = RecordingWaker::default();
        let diff = DiscoveryDiff {
            changed: true,
            new_escalations: vec![],
            escalation_count_prev: 0,
            escalation_count_curr: 0,
            non_escalation_diff: true,
            non_escalation_summary: None,
            first_sweep: false,
        };
        let outcome = route_non_escalation_diff(
            &diff,
            "demo-roadmap",
            now(),
            PermissionProfile::Locked,
            &waker,
        );
        assert!(outcome.suppressed_by_profile);
        assert!(!outcome.routed);
        assert_eq!(waker.call_count(), 0);
    }
}
