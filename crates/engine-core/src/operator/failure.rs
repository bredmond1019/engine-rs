//! Terminal run-failure notification (`ticket-run-failure-notification`).
//!
//! Per `planning/ticket-run-failure-notification/tasks.md`: when a run ends
//! **failed** (or `budget_halted`), the operator hears about it through the
//! same `EN.8.A` payload contract and the same `EN.8.B` queue/depth limit
//! as every other operator item — not a log line nobody reads. This ticket
//! adds no new primitive: it is one decision function (this task), one
//! payload renderer (task 2), and one hook at `engine-serve`'s single
//! terminal choke point (task 3).
//!
//! ## Which statuses notify, and why `cancelled` does not
//!
//! `http.rs::derive_terminal_status` reports exactly four outcomes, in
//! precedence order: `"cancelled"` -> `"budget_halted"` -> `"failed"` ->
//! `"succeeded"`. The built-in default notifies on `failed` and
//! `budget_halted`, and does not notify on `cancelled` or `succeeded`:
//!
//! - **`failed`** — the run crashed or a node errored. This is exactly the
//!   silence this ticket exists to fix.
//! - **`budget_halted`** — a judgement call, flagged for reversal in the
//!   spec's Notes: this is not a crash, but a run that stopped early
//!   because it hit its own cost ceiling is exactly the thing an operator
//!   wants to hear about unprompted. Reversing it is a config change (see
//!   [`NotifyOnStatuses`]), not a code change.
//! - **`cancelled`** — the operator caused the cancellation themselves;
//!   telling them about it is noise.
//! - **`succeeded`** — obviously does not fire.
//!
//! This module defines the decision only. The once-per-run guarantee comes
//! from where task 3 wires this in: `live_state.rs::mark_terminal`, the
//! single choke point every run passes through on its way out of the live
//! map, on every exit path.
//!
//! ## Policy knobs
//!
//! Two knobs, resolved through the standard four-layer precedence (per-run
//! event `policy` override > named `profile` bundle >
//! `planning/harness.json`'s `run_failure_notification.policy` defaults >
//! built-in default) per `CLAUDE.md` standing rule 6:
//!
//! - [`FailureNotifyPolicy::notify_on_statuses`] — which of the four
//!   terminal statuses enqueue a notification.
//! - [`FailureNotifyPolicy::failure_item_priority`] — the `effective_priority`
//!   a rendered failure item carries into the [`crate::operator::queue::OperatorQueue`].
//!
//! Both are behavior-stable by default (an unconfigured run reproduces the
//! documented defaults above) and are set explicitly in all three named
//! profiles (`baseline`, `cheap-fast`, `thorough`) — a knob absent from the
//! profile bundles is a knob nobody will find.

use serde::{Deserialize, Serialize};

use crate::node::NodeError;
use crate::operator::limits::OperatorPayloadLimits;
use crate::operator::payload::{OperatorPayload, OperatorResponseOption};
use crate::operator::validate::{validate, OperatorValidationError, ValidatedOperatorPayload};
use crate::policy::{merge_opt, Policy, PolicyConfigSource};

/// The `harness.json` section key this policy's knobs live under
/// (`run_failure_notification.policy` / `run_failure_notification.profiles`).
const WORKFLOW_KEY: &str = "run_failure_notification";

/// Which of `http.rs::derive_terminal_status`'s four terminal outcomes
/// enqueue an operator notification. See the module docs for the built-in
/// default and the rationale behind each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyOnStatuses {
    pub failed: bool,
    pub budget_halted: bool,
    pub cancelled: bool,
    pub succeeded: bool,
}

impl Default for NotifyOnStatuses {
    /// `failed` and `budget_halted` notify; `cancelled` and `succeeded` do
    /// not. See the module docs for why.
    fn default() -> Self {
        Self {
            failed: true,
            budget_halted: true,
            cancelled: false,
            succeeded: false,
        }
    }
}

/// All-optional mirror of [`NotifyOnStatuses`] for per-status partial
/// overrides. A `None` field leaves the base value untouched — an override
/// flips one status without resetting the others.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialNotifyOnStatuses {
    pub failed: Option<bool>,
    pub budget_halted: Option<bool>,
    pub cancelled: Option<bool>,
    pub succeeded: Option<bool>,
}

/// Merge one [`PartialNotifyOnStatuses`] override layer over a
/// [`NotifyOnStatuses`] base, field by field.
fn merge_notify_on_statuses(
    mut base: NotifyOnStatuses,
    over: &PartialNotifyOnStatuses,
) -> NotifyOnStatuses {
    if let Some(v) = over.failed {
        base.failed = v;
    }
    if let Some(v) = over.budget_halted {
        base.budget_halted = v;
    }
    if let Some(v) = over.cancelled {
        base.cancelled = v;
    }
    if let Some(v) = over.succeeded {
        base.succeeded = v;
    }
    base
}

/// The fully-resolved, per-run failure-notification policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureNotifyPolicy {
    /// Which terminal statuses enqueue a notification. See
    /// [`NotifyOnStatuses`].
    pub notify_on_statuses: NotifyOnStatuses,
    /// The `effective_priority` a rendered failure item carries into the
    /// operator queue. Every failure item enqueues at this uniform
    /// priority; ordering among failure items falls back to
    /// `operator::queue::compare_items`'s `enqueued_at`/`item_id`
    /// secondary keys, matching `approve_and_run`'s
    /// `harvest_item_priority` convention.
    pub failure_item_priority: i32,
}

impl Default for FailureNotifyPolicy {
    /// Behavior-stable baseline: the documented default statuses (see
    /// [`NotifyOnStatuses::default`]), priority `0`.
    fn default() -> Self {
        Self {
            notify_on_statuses: NotifyOnStatuses::default(),
            failure_item_priority: 0,
        }
    }
}

/// All-optional mirror of [`FailureNotifyPolicy`] used by the override
/// layers (`harness.json`'s `run_failure_notification.policy`, a named
/// `profile`, and a per-run event's `policy` field).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialFailureNotifyPolicy {
    pub notify_on_statuses: Option<PartialNotifyOnStatuses>,
    pub failure_item_priority: Option<i32>,
}

impl Policy for FailureNotifyPolicy {
    type Partial = PartialFailureNotifyPolicy;

    /// Apply one override layer on top of `self`. `notify_on_statuses` is
    /// deep-merged field-by-field via [`merge_notify_on_statuses`], not
    /// all-or-nothing.
    fn apply(self, over: &PartialFailureNotifyPolicy) -> Self {
        Self {
            notify_on_statuses: match &over.notify_on_statuses {
                Some(partial) => merge_notify_on_statuses(self.notify_on_statuses, partial),
                None => self.notify_on_statuses,
            },
            failure_item_priority: merge_opt(
                self.failure_item_priority,
                over.failure_item_priority,
            ),
        }
    }
}

/// The explicit control profile: the documented defaults, spelled out
/// explicitly (rather than left all-`None`) so selecting `profile:
/// "baseline"` is a legible, self-documenting no-op against the built-in
/// default.
#[must_use]
pub fn baseline() -> PartialFailureNotifyPolicy {
    PartialFailureNotifyPolicy {
        notify_on_statuses: Some(PartialNotifyOnStatuses {
            failed: Some(true),
            budget_halted: Some(true),
            cancelled: Some(false),
            succeeded: Some(false),
        }),
        failure_item_priority: Some(0),
    }
}

/// Cost/latency floor: same notify set as baseline (which statuses notify
/// is not a cost dial), but failure items enqueue at a lower priority so a
/// speed-optimized deployment does not let a failure burst crowd out other
/// queued operator items.
#[must_use]
pub fn cheap_fast() -> PartialFailureNotifyPolicy {
    PartialFailureNotifyPolicy {
        notify_on_statuses: Some(PartialNotifyOnStatuses {
            failed: Some(true),
            budget_halted: Some(true),
            cancelled: Some(false),
            succeeded: Some(false),
        }),
        failure_item_priority: Some(-5),
    }
}

/// Quality ceiling: same notify set as baseline, but failure items enqueue
/// at a higher priority so a quality-optimized deployment surfaces run
/// failures ahead of other queued operator items.
#[must_use]
pub fn thorough() -> PartialFailureNotifyPolicy {
    PartialFailureNotifyPolicy {
        notify_on_statuses: Some(PartialNotifyOnStatuses {
            failed: Some(true),
            budget_halted: Some(true),
            cancelled: Some(false),
            succeeded: Some(false),
        }),
        failure_item_priority: Some(5),
    }
}

/// Resolve a built-in profile bundle by its kebab-case name. Returns `None`
/// for any name that isn't one of the three canonical profiles.
#[must_use]
pub fn profile_by_name(name: &str) -> Option<PartialFailureNotifyPolicy> {
    match name {
        "baseline" => Some(baseline()),
        "cheap-fast" => Some(cheap_fast()),
        "thorough" => Some(thorough()),
        _ => None,
    }
}

/// Read `run_failure_notification.policy` (a [`PartialFailureNotifyPolicy`])
/// out of the file addressed by `source`. Delegates to the generic
/// `crate::policy::read_harness_policy_defaults_from`, parameterized by
/// [`WORKFLOW_KEY`].
pub fn read_harness_policy_defaults_from(
    source: &PolicyConfigSource,
) -> Result<Option<PartialFailureNotifyPolicy>, NodeError> {
    crate::policy::read_harness_policy_defaults_from(source, WORKFLOW_KEY)
}

/// Resolve a named `profile` to a [`PartialFailureNotifyPolicy`] bundle,
/// preferring a `harness.json` `run_failure_notification.profiles[name]`
/// entry (read via `source`) over the built-in [`profile_by_name`].
/// Returns `Ok(None)` when `profile_name` is `None`, and `Err` when a name
/// is given but resolves to neither source (no silent no-op). Delegates to
/// the generic `crate::policy::resolve_profile_from`, parameterized by
/// [`WORKFLOW_KEY`] and [`profile_by_name`].
pub fn resolve_profile_from(
    profile_name: Option<&str>,
    source: &PolicyConfigSource,
) -> Result<Option<PartialFailureNotifyPolicy>, NodeError> {
    crate::policy::resolve_profile_from(profile_name, source, WORKFLOW_KEY, profile_by_name)
}

/// Resolve the four-layer [`FailureNotifyPolicy`] for a run: `event_override`
/// (a per-run `policy` field), the resolved `profile_name` bundle, `source`'s
/// `run_failure_notification.policy` defaults, and the built-in default,
/// high->low precedence via `crate::policy::resolve`. A
/// [`PolicyConfigSource::Builtin`] source resolves successfully with no
/// filesystem access at all — the case the hot `mark_terminal` exit path
/// (task 3) needs.
pub fn resolve_policy_for_run_from(
    source: &PolicyConfigSource,
    profile_name: Option<&str>,
    event_override: Option<&PartialFailureNotifyPolicy>,
) -> Result<FailureNotifyPolicy, NodeError> {
    let harness_defaults = read_harness_policy_defaults_from(source)?;
    let profile = resolve_profile_from(profile_name, source)?;
    Ok(crate::policy::resolve(
        FailureNotifyPolicy::default(),
        harness_defaults.as_ref(),
        profile.as_ref(),
        event_override,
    ))
}

/// Serializes the resolved policy so a caller can stamp it into `ctx.nodes`
/// (`RunTelemetry`/`PolicyAggregate` per `CLAUDE.md` standing rule 6) and
/// attribute observed behavior to the setting that caused it. Never
/// includes a `cost_usd` key — `workflow.rs:552-554` folds that into
/// `BudgetLedger` untyped with no provenance check.
#[must_use]
pub fn policy_state(policy: &FailureNotifyPolicy) -> serde_json::Value {
    serde_json::to_value(policy).unwrap_or_else(|_| serde_json::json!({}))
}

/// PURE decision: given a terminal status string
/// (`http.rs::derive_terminal_status`'s four outcomes — `"cancelled"`,
/// `"budget_halted"`, `"failed"`, `"succeeded"`) and the resolved policy,
/// return whether to notify. An unrecognized status string never notifies
/// — this function has no fifth outcome to guess at.
#[must_use]
pub fn should_notify(terminal_status: &str, policy: &FailureNotifyPolicy) -> bool {
    match terminal_status {
        "failed" => policy.notify_on_statuses.failed,
        "budget_halted" => policy.notify_on_statuses.budget_halted,
        "cancelled" => policy.notify_on_statuses.cancelled,
        "succeeded" => policy.notify_on_statuses.succeeded,
        _ => false,
    }
}

/// Marker appended to a rendered summary's node-detail line when it had to
/// be shortened to fit [`OperatorPayloadLimits::max_summary_chars`] — see
/// [`render_failure_payload`]'s module docs on the truncate-rather-than-drop
/// rule.
const TRUNCATION_MARKER: &str = "…[truncated]";

/// One failed node's identity plus its error text, as recorded in
/// `TaskContext::node_runs` (`engine_contract::NodeRun`). Task 3 extracts
/// these from a terminal run's snapshot; this module never reads
/// `TaskContext` directly, keeping [`render_failure_payload`] pure and unit
/// testable without an `engine-contract` dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedNode {
    /// The node's identity — its class name, matching
    /// `TaskContext::node_runs`' map key convention.
    pub name: String,
    /// The node's recorded error text (`NodeRun::error`, or a placeholder
    /// if the node run carried none).
    pub error: String,
}

impl FailedNode {
    /// Construct a failed-node identity/error pair.
    pub fn new(name: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            error: error.into(),
        }
    }
}

/// Render a terminal run into a validated [`OperatorPayload`] naming the
/// workflow type, the run id, and the failing node (`EN.8.A`'s digest-bound
/// shape, run through `operator::validate` before it is ever returned).
///
/// `failed_nodes` follows `engine-serve/src/suspend.rs::failed_node_reason`'s
/// existing first-stamped-node convention: this function names
/// `failed_nodes[0]` and, when there is more than one, states the total
/// count rather than rendering every node into one summary (spec Task 2:
/// "Do not render N nodes into one summary"). Task 3 is responsible for
/// handing this function `failed_nodes` in a deterministic order (the
/// convention that matters for *this* function's contract, since a
/// `HashMap`'s own iteration order is not deterministic). An empty
/// `failed_nodes` renders a status-only summary — the `budget_halted` case,
/// where the walk stops between nodes with no `NodeRunStatus::Failed` entry
/// to name.
///
/// `terminal_status` is expected to be `"failed"` or `"budget_halted"` (the
/// two statuses [`should_notify`] can return `true` for by default); the two
/// render distinguishably in the summary text, per the spec's acceptance
/// criteria — a budget halt is not a crash, and the operator acts on it
/// differently. Any other status string renders as a generic failure
/// (`should_notify` is what gates whether this function is even called for
/// that status; this function does not re-derive that decision).
///
/// The response options are an **acknowledgement set, not an approval
/// set** — `"acknowledge"`/`"view_run"`, stable machine keys, nothing
/// executes on either. `gate_id` on the returned payload identifies this as
/// a run-failure item rather than a gate-approval one (queue wiring is
/// task 3's concern, not this function's).
///
/// A rendered summary that would exceed
/// [`OperatorPayloadLimits::max_summary_chars`] is truncated deterministically
/// rather than causing this function to fail: per the spec's Notes, an
/// unreportable failure has no useful fallback, so silence is the worst
/// outcome. Truncation shortens the node-detail line only (the fixed
/// status/workflow/run-id header is never cut) and always ends with
/// [`TRUNCATION_MARKER`] so the operator knows there is more. The one error
/// path that remains is [`operator::validate`]'s own — e.g. a caller-supplied
/// `limits.max_summary_chars` too small to hold even the fixed header, or an
/// `OperatorPayloadLimits` whose option bounds this function's fixed
/// two-option set cannot satisfy.
pub fn render_failure_payload(
    gate_id: impl Into<String>,
    run_id: &str,
    workflow_type: &str,
    terminal_status: &str,
    failed_nodes: &[FailedNode],
    limits: &OperatorPayloadLimits,
) -> Result<ValidatedOperatorPayload, OperatorValidationError> {
    let status_line = if terminal_status == "budget_halted" {
        "Run halted: budget limit reached"
    } else {
        "Run failed"
    };

    let header = format!("{status_line}\nWorkflow: {workflow_type}\nRun: {run_id}\n");

    let node_line = match failed_nodes.split_first() {
        Some((first, [])) => {
            format!("Failing node: {} — {}", first.name, first.error)
        }
        Some((first, rest)) => {
            format!(
                "Failing node: {} — {} (+{} more failed node{})",
                first.name,
                first.error,
                rest.len(),
                if rest.len() == 1 { "" } else { "s" }
            )
        }
        None => "No specific node failure recorded.".to_string(),
    };

    let max_chars = limits.max_summary_chars;
    let header_chars = header.chars().count();
    let available_for_node_line = max_chars.saturating_sub(header_chars);

    let final_node_line = if node_line.chars().count() <= available_for_node_line {
        node_line
    } else {
        let marker_chars = TRUNCATION_MARKER.chars().count();
        let keep = available_for_node_line.saturating_sub(marker_chars);
        let kept: String = node_line.chars().take(keep).collect();
        format!("{kept}{TRUNCATION_MARKER}")
    };

    let mut rendered_summary = format!("{header}{final_node_line}");
    // Safety net: the header/node-line split above should already guarantee
    // this, but never hand `validate` a summary longer than the declared
    // limit due to an edge case in the arithmetic above.
    if rendered_summary.chars().count() > max_chars {
        rendered_summary = rendered_summary.chars().take(max_chars).collect();
    }

    let options = vec![
        OperatorResponseOption::new("acknowledge", "Acknowledge"),
        OperatorResponseOption::new("view_run", "View run"),
    ];

    let payload = OperatorPayload::new(gate_id, rendered_summary, options);
    validate(payload, limits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_status_maps_to_its_documented_default() {
        let policy = FailureNotifyPolicy::default();
        assert!(should_notify("failed", &policy));
        assert!(should_notify("budget_halted", &policy));
        assert!(!should_notify("cancelled", &policy));
        assert!(!should_notify("succeeded", &policy));
    }

    #[test]
    fn unrecognized_status_never_notifies() {
        let policy = FailureNotifyPolicy::default();
        assert!(!should_notify("suspended", &policy));
        assert!(!should_notify("", &policy));
    }

    #[test]
    fn override_flips_a_status_both_directions() {
        // cancelled: false -> true
        let mut policy = FailureNotifyPolicy::default();
        policy.notify_on_statuses.cancelled = true;
        assert!(should_notify("cancelled", &policy));

        // failed: true -> false
        let mut policy = FailureNotifyPolicy::default();
        policy.notify_on_statuses.failed = false;
        assert!(!should_notify("failed", &policy));
    }

    #[test]
    fn every_named_profile_sets_both_knobs_explicitly() {
        for profile in [baseline(), cheap_fast(), thorough()] {
            assert!(profile.notify_on_statuses.is_some());
            let notify = profile.notify_on_statuses.unwrap();
            assert!(notify.failed.is_some());
            assert!(notify.budget_halted.is_some());
            assert!(notify.cancelled.is_some());
            assert!(notify.succeeded.is_some());
            assert!(profile.failure_item_priority.is_some());
        }
    }

    #[test]
    fn every_named_profile_keeps_the_documented_notify_defaults() {
        // Which statuses notify is not a cost/quality dial — every profile
        // keeps the same set even though priority varies.
        for profile in [baseline(), cheap_fast(), thorough()] {
            let notify = profile.notify_on_statuses.unwrap();
            assert_eq!(notify.failed, Some(true));
            assert_eq!(notify.budget_halted, Some(true));
            assert_eq!(notify.cancelled, Some(false));
            assert_eq!(notify.succeeded, Some(false));
        }
    }

    #[test]
    fn policy_state_never_emits_cost_usd_key() {
        let policy = FailureNotifyPolicy::default();
        let state = policy_state(&policy);
        assert!(state.get("cost_usd").is_none());
        // Sanity: the state is not just an empty fallback object.
        assert!(state.get("notify_on_statuses").is_some());
        assert!(state.get("failure_item_priority").is_some());
    }

    #[test]
    fn resolve_with_no_overrides_returns_builtin() {
        let resolved = crate::policy::resolve(FailureNotifyPolicy::default(), None, None, None);
        assert_eq!(resolved, FailureNotifyPolicy::default());
    }

    #[test]
    fn harness_default_overrides_builtin_priority() {
        let harness = PartialFailureNotifyPolicy {
            failure_item_priority: Some(9),
            ..Default::default()
        };
        let resolved =
            crate::policy::resolve(FailureNotifyPolicy::default(), Some(&harness), None, None);
        assert_eq!(resolved.failure_item_priority, 9);
        // Untouched knob still falls through to builtin.
        assert_eq!(resolved.notify_on_statuses, NotifyOnStatuses::default());
    }

    #[test]
    fn profile_beats_harness_defaults() {
        let harness = PartialFailureNotifyPolicy {
            failure_item_priority: Some(9),
            ..Default::default()
        };
        let profile = PartialFailureNotifyPolicy {
            failure_item_priority: Some(3),
            ..Default::default()
        };
        let resolved = crate::policy::resolve(
            FailureNotifyPolicy::default(),
            Some(&harness),
            Some(&profile),
            None,
        );
        assert_eq!(resolved.failure_item_priority, 3);
    }

    #[test]
    fn event_override_beats_profile() {
        let profile = PartialFailureNotifyPolicy {
            failure_item_priority: Some(3),
            ..Default::default()
        };
        let event = PartialFailureNotifyPolicy {
            failure_item_priority: Some(1),
            ..Default::default()
        };
        let resolved = crate::policy::resolve(
            FailureNotifyPolicy::default(),
            None,
            Some(&profile),
            Some(&event),
        );
        assert_eq!(resolved.failure_item_priority, 1);
    }

    #[test]
    fn event_override_flips_one_status_without_resetting_others() {
        let event = PartialFailureNotifyPolicy {
            notify_on_statuses: Some(PartialNotifyOnStatuses {
                cancelled: Some(true),
                ..Default::default()
            }),
            failure_item_priority: None,
        };
        let resolved =
            crate::policy::resolve(FailureNotifyPolicy::default(), None, None, Some(&event));
        assert!(resolved.notify_on_statuses.cancelled);
        // Untouched statuses keep their documented defaults.
        assert!(resolved.notify_on_statuses.failed);
        assert!(resolved.notify_on_statuses.budget_halted);
        assert!(!resolved.notify_on_statuses.succeeded);
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
    fn resolve_policy_for_run_from_builtin_source_needs_no_filesystem() {
        let resolved = resolve_policy_for_run_from(&PolicyConfigSource::Builtin, None, None)
            .expect("resolve should succeed with no filesystem access");
        assert_eq!(resolved, FailureNotifyPolicy::default());
    }

    #[test]
    fn resolve_policy_for_run_from_applies_named_profile() {
        let resolved =
            resolve_policy_for_run_from(&PolicyConfigSource::Builtin, Some("cheap-fast"), None)
                .expect("resolve should succeed");
        assert_eq!(resolved.failure_item_priority, -5);
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
        let json = r#"{ "notify_on_statuses": { "cancelled": true }, "failure_item_priority": 7 }"#;
        let partial: PartialFailureNotifyPolicy =
            serde_json::from_str(json).expect("valid PartialFailureNotifyPolicy JSON");
        assert_eq!(partial.notify_on_statuses.unwrap().cancelled, Some(true));
        assert_eq!(partial.failure_item_priority, Some(7));
    }

    #[test]
    fn partial_policy_round_trips_with_fields_absent() {
        let partial: PartialFailureNotifyPolicy =
            serde_json::from_str("{}").expect("valid empty PartialFailureNotifyPolicy JSON");
        assert_eq!(partial, PartialFailureNotifyPolicy::default());
    }

    // --- render_failure_payload (task 2) -----------------------------

    #[test]
    fn typical_failure_validates_and_names_all_three_facts() {
        let limits = OperatorPayloadLimits::default();
        let failed = [FailedNode::new(
            "BuildNode",
            "compile error: missing semicolon",
        )];
        let validated = render_failure_payload(
            "run-failure",
            "run-123",
            "sdlc_flow",
            "failed",
            &failed,
            &limits,
        )
        .expect("typical failure should validate");
        let summary = &validated.payload().rendered_summary;
        assert!(
            summary.contains("sdlc_flow"),
            "missing workflow type: {summary}"
        );
        assert!(summary.contains("run-123"), "missing run id: {summary}");
        assert!(
            summary.contains("BuildNode"),
            "missing failing node: {summary}"
        );
        assert!(
            summary.contains("compile error: missing semicolon"),
            "missing error text: {summary}"
        );
    }

    #[test]
    fn error_text_far_over_limit_truncates_deterministically_and_still_validates() {
        let limits = OperatorPayloadLimits::default();
        let huge_error = "x".repeat(5_000);
        let failed = [FailedNode::new("BuildNode", huge_error.clone())];

        let first = render_failure_payload(
            "run-failure",
            "run-123",
            "sdlc_flow",
            "failed",
            &failed,
            &limits,
        )
        .expect("truncated failure should still validate, never fail to notify");
        let second = render_failure_payload(
            "run-failure",
            "run-123",
            "sdlc_flow",
            "failed",
            &failed,
            &limits,
        )
        .expect("truncated failure should still validate, never fail to notify");

        let summary = &first.payload().rendered_summary;
        assert!(
            summary.chars().count() <= limits.max_summary_chars,
            "rendered summary must fit the declared limit"
        );
        assert!(
            summary.contains(TRUNCATION_MARKER),
            "truncation must be marked visibly: {summary}"
        );
        assert!(
            !summary.contains(&huge_error),
            "the full untruncated error text must not appear verbatim"
        );
        // Deterministic: same inputs render byte-identical output.
        assert_eq!(
            first.payload().rendered_summary,
            second.payload().rendered_summary
        );
    }

    #[test]
    fn budget_halted_summary_differs_from_failed_summary() {
        let limits = OperatorPayloadLimits::default();
        let failed = [FailedNode::new("BuildNode", "boom")];

        let failed_payload = render_failure_payload(
            "run-failure",
            "run-123",
            "sdlc_flow",
            "failed",
            &failed,
            &limits,
        )
        .expect("failed run should validate");
        let halted_payload = render_failure_payload(
            "run-failure",
            "run-123",
            "sdlc_flow",
            "budget_halted",
            &[],
            &limits,
        )
        .expect("budget-halted run should validate");

        assert_ne!(
            failed_payload.payload().rendered_summary,
            halted_payload.payload().rendered_summary
        );
        assert!(halted_payload
            .payload()
            .rendered_summary
            .to_lowercase()
            .contains("budget"));
        assert!(!failed_payload
            .payload()
            .rendered_summary
            .to_lowercase()
            .contains("budget"));
    }

    #[test]
    fn multi_node_failure_names_first_and_counts_the_rest() {
        let limits = OperatorPayloadLimits::default();
        let failed = [
            FailedNode::new("FirstNode", "first error"),
            FailedNode::new("SecondNode", "second error"),
            FailedNode::new("ThirdNode", "third error"),
        ];
        let validated = render_failure_payload(
            "run-failure",
            "run-123",
            "sdlc_flow",
            "failed",
            &failed,
            &limits,
        )
        .expect("multi-node failure should validate");
        let summary = &validated.payload().rendered_summary;

        assert!(
            summary.contains("FirstNode"),
            "must name the first failed node: {summary}"
        );
        assert!(
            !summary.contains("SecondNode") && !summary.contains("ThirdNode"),
            "must not render every failed node into one summary: {summary}"
        );
        assert!(
            summary.contains('2'),
            "must state the count of the remaining failed nodes: {summary}"
        );
    }

    #[test]
    fn empty_failed_nodes_renders_a_status_only_summary() {
        let limits = OperatorPayloadLimits::default();
        let validated = render_failure_payload(
            "run-failure",
            "run-123",
            "sdlc_flow",
            "budget_halted",
            &[],
            &limits,
        )
        .expect("status-only summary should validate");
        assert!(!validated.payload().rendered_summary.is_empty());
    }

    #[test]
    fn options_are_a_stable_acknowledgement_set_within_limits() {
        let limits = OperatorPayloadLimits::default();
        let failed = [FailedNode::new("BuildNode", "boom")];
        let validated = render_failure_payload(
            "run-failure",
            "run-123",
            "sdlc_flow",
            "failed",
            &failed,
            &limits,
        )
        .expect("should validate");
        let options = &validated.payload().options;
        assert!(options.len() >= limits.min_options);
        assert!(options.len() <= limits.max_options);
        let keys: Vec<&str> = options.iter().map(|o| o.key.as_str()).collect();
        assert_eq!(keys, vec!["acknowledge", "view_run"]);
    }
}
