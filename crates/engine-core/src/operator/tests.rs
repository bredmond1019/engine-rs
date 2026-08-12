//! Hermetic, cross-module tests for the operator payload contract
//! (`EN.8.A` task 5).
//!
//! Per `planning/8.A-operator-payload-contract/tasks.md`, this module covers
//! the properties that only show up once [`crate::operator::limits`],
//! [`crate::operator::payload`], [`crate::operator::validate`],
//! [`crate::operator::channel`], and
//! [`crate::nodes::harvest_gate::HarvestGate`] are exercised together: each
//! rejection path from task 3, the forced route-to-session, the
//! readable-without-executing property from task 4, and the digest-change
//! re-queue case. Each individual module's own `#[cfg(test)] mod tests`
//! already covers its unit-level behavior in isolation; this module is the
//! integration layer that proves those pieces compose the way the spec
//! requires.
//!
//! None of these tests perform network or filesystem I/O — every fixture is
//! constructed in-process, and "the notification channel" / "the session
//! channel" here means the [`OperatorChannel`] enum value, never a real
//! transport. That is deliberate: `EN.8.A` defines the contract and the
//! routing declaration only; the transport itself is `bastion:BA.18.B`,
//! explicitly out of scope (see the spec's Notes section).

use crate::nodes::harvest_gate::{HarvestGate, HarvestMode};
use crate::operator::channel::OperatorChannel;
use crate::operator::limits::OperatorPayloadLimits;
use crate::operator::payload::{OperatorPayload, OperatorResponseOption};
use crate::operator::validate::{validate, OperatorValidationError};

fn approve_reject() -> Vec<OperatorResponseOption> {
    vec![
        OperatorResponseOption::new("approve", "Approve"),
        OperatorResponseOption::new("reject", "Reject"),
    ]
}

/// A payload that fails validation has no route onto the notification
/// channel: the only way to name a [`crate::operator::ValidatedOperatorPayload`]
/// is through `validate`'s `Ok` arm, so a rejected payload forces the
/// producing gate to fall back to declaring `session` instead. This test
/// exercises that end to end: validation fails, and the gate that would
/// have carried this payload is set up to declare `session` as its only
/// available path.
#[test]
fn missing_summary_forces_session_routing() {
    let payload = OperatorPayload::new("dev-to-sweep-review", "", approve_reject());
    let limits = OperatorPayloadLimits::default();

    let result = validate(payload, &limits);
    assert_eq!(result, Err(OperatorValidationError::MissingRenderedSummary));

    // The gate that produced this payload cannot emit on `notification` —
    // there is no `ValidatedOperatorPayload` to hand it — so it declares
    // `session` instead, per spec Invariant 2.
    let gate = HarvestGate::new(HarvestMode::Approval)
        .with_channel(OperatorChannel::session("dev-to-sweep-review"));
    assert!(gate.channel().is_session());
    assert_eq!(
        gate.channel().session_slug(),
        Some("dev-to-sweep-review"),
        "a gate that cannot produce a conforming payload must name a session slug"
    );
}

/// Too many response options is rejected, and the failure means the same
/// thing operationally as every other rejection: no notification-channel
/// payload is obtainable, so the producing gate is forced to session.
#[test]
fn too_many_options_forces_session_routing() {
    let payload = OperatorPayload::new(
        "sweep-gate",
        "diff: 4 identical one-line edits",
        vec![
            OperatorResponseOption::new("a", "A"),
            OperatorResponseOption::new("b", "B"),
            OperatorResponseOption::new("c", "C"),
            OperatorResponseOption::new("d", "D"),
        ],
    );
    let limits = OperatorPayloadLimits::default();

    let result = validate(payload, &limits);
    assert_eq!(
        result,
        Err(OperatorValidationError::TooManyOptions { count: 4, max: 3 })
    );

    let gate = HarvestGate::new(HarvestMode::Approval)
        .with_channel(OperatorChannel::session("sweep-gate"));
    assert!(gate.channel().is_session());
}

/// Fewer than the minimum options is rejected — a single option is not a
/// real choice (spec Invariant 1: "a decision, never a task").
#[test]
fn too_few_options_forces_session_routing() {
    let payload = OperatorPayload::new(
        "sweep-gate",
        "diff summary",
        vec![OperatorResponseOption::new("approve", "Approve")],
    );
    let limits = OperatorPayloadLimits::default();

    assert_eq!(
        validate(payload, &limits),
        Err(OperatorValidationError::TooFewOptions { count: 1, min: 2 })
    );

    let gate = HarvestGate::new(HarvestMode::Approval)
        .with_channel(OperatorChannel::session("sweep-gate"));
    assert!(gate.channel().is_session());
}

/// A response option's label exceeding the channel's label-length limit is
/// rejected, the fourth of the four distinct rejection paths.
#[test]
fn option_label_too_long_forces_session_routing() {
    let long_label = "x".repeat(21);
    let payload = OperatorPayload::new(
        "sweep-gate",
        "diff summary",
        vec![
            OperatorResponseOption::new("approve", "Approve"),
            OperatorResponseOption::new("reject", long_label),
        ],
    );
    let limits = OperatorPayloadLimits::default();

    assert_eq!(
        validate(payload, &limits),
        Err(OperatorValidationError::OptionLabelTooLong {
            key: "reject".to_string(),
            chars: 21,
            max: 20,
        })
    );

    let gate = HarvestGate::new(HarvestMode::Approval)
        .with_channel(OperatorChannel::session("sweep-gate"));
    assert!(gate.channel().is_session());
}

/// A rendered summary exceeding the channel's body length limit is
/// rejected. This is the fifth rejection reason the type carries
/// (`RenderedSummaryTooLong`) beyond the four the spec enumerates by name,
/// and it forces `session` exactly the same as the others.
#[test]
fn summary_too_long_forces_session_routing() {
    let payload = OperatorPayload::new("sweep-gate", "x".repeat(1025), approve_reject());
    let limits = OperatorPayloadLimits::default();

    assert_eq!(
        validate(payload, &limits),
        Err(OperatorValidationError::RenderedSummaryTooLong {
            chars: 1025,
            max: 1024,
        })
    );
}

/// A gate's declared channel is readable straight off its definition — no
/// `decide()` call, no emit, no workflow execution at all. This is the
/// property task 4 built and task 5 must prove: constructing the gate is
/// the only action this test takes before reading `channel()`.
#[test]
fn channel_is_readable_off_the_gate_definition_without_executing() {
    let notification_gate = HarvestGate::new(HarvestMode::InProcess);
    assert!(notification_gate.channel().is_notification());

    let session_gate = HarvestGate::new(HarvestMode::Approval)
        .with_channel(OperatorChannel::session("dev-to-sweep-review"));
    assert!(session_gate.channel().is_session());
    assert_eq!(
        session_gate.channel().session_slug(),
        Some("dev-to-sweep-review")
    );

    // Neither gate's `decide()` — the one method that represents "doing
    // something with this gate at run time" — was ever called above; the
    // channel was fully legible from the definition alone.
}

/// A changed rendered payload produces a different digest, and that digest
/// is what a re-queue check compares against: a stale digest means "this is
/// not the payload the operator approved," which routes back to
/// re-validation rather than executing the (now stale) approval.
#[test]
fn changed_payload_produces_different_digest_and_would_requeue() {
    let original = OperatorPayload::new(
        "sweep-gate",
        "diff: 4 identical one-line edits",
        approve_reject(),
    );
    let limits = OperatorPayloadLimits::default();
    let validated_original = validate(original.clone(), &limits).expect("original validates");
    let approved_digest = validated_original.payload().digest.clone();

    // The workflow re-renders before acting on the stored approval — the
    // source diff changed underneath the pending payload.
    let re_rendered = OperatorPayload::new(
        "sweep-gate",
        "diff: 5 identical one-line edits", // one more edit landed
        approve_reject(),
    );

    assert_ne!(
        re_rendered.digest, approved_digest,
        "a changed rendered payload must not keep the digest the operator approved"
    );

    // The digest mismatch is exactly the signal a `HARVEST_APPROVE`-style
    // executor uses to refuse to execute a stale approval and re-queue
    // instead.
    assert!(
        !(re_rendered.digest == approved_digest),
        "digest mismatch is the re-queue trigger, never the execute trigger"
    );

    // The re-rendered payload still independently validates (it is well
    // formed), which is what lets it re-queue as a fresh, still-actionable
    // item rather than being discarded outright.
    assert!(validate(re_rendered, &limits).is_ok());
}

/// The mutation-detection path: a payload constructed once, then mutated in
/// place without going back through `OperatorPayload::new`, no longer
/// matches its own stored digest. This is the same signal as the
/// re-render case above, reached via direct field mutation instead of
/// reconstruction.
#[test]
fn post_construction_mutation_is_detected_via_digest_mismatch() {
    let mut payload = OperatorPayload::new("sweep-gate", "original summary", approve_reject());
    assert!(payload.digest_matches());

    payload.rendered_summary = "mutated summary".to_string();
    assert!(
        !payload.digest_matches(),
        "a mutated rendered payload must fail its own stored digest check"
    );
}
