//! Validation for [`OperatorPayload`], with route-to-session as the failure
//! path (`EN.8.A` task 3).
//!
//! Per `planning/8.A-operator-payload-contract/tasks.md` and the contract at
//! `initial-architecture.md` §7.5, a gate whose payload fails validation
//! must not be able to emit a degraded notification — the failure forces
//! the `session` channel. This module enforces that at the type level: the
//! only way to obtain a [`ValidatedOperatorPayload`] — the type the
//! notification channel accepts (`EN.8.A` task 4 wires the emit path to
//! it) — is [`validate`] succeeding. There is no public constructor that
//! bypasses validation, so a payload that never validated cannot reach the
//! notification channel no matter what code calls it. A failed validation
//! carries a distinct, typed [`OperatorValidationError`] variant per
//! rejection reason, and every variant means the same thing operationally:
//! this gate must declare `session`, not `notification`.

use std::fmt;

use crate::operator::limits::OperatorPayloadLimits;
use crate::operator::payload::OperatorPayload;

/// Why an [`OperatorPayload`] failed validation. Each variant is a distinct
/// rejection reason (`EN.8.A` task 3's four rejection paths); every variant
/// means the producing gate must declare the `session` channel instead of
/// `notification` (spec Invariant 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperatorValidationError {
    /// The rendered summary is missing or empty — spec Invariant 1: a
    /// payload with no inline rendered summary is rejected outright.
    MissingRenderedSummary,
    /// The rendered summary exceeds the channel's body length limit.
    RenderedSummaryTooLong { chars: usize, max: usize },
    /// Fewer than [`OperatorPayloadLimits::min_options`] response options —
    /// not a real choice (see `EN.8.A` task 1's `OPERATOR_MIN_RESPONSE_OPTIONS`).
    TooFewOptions { count: usize, min: usize },
    /// More than [`OperatorPayloadLimits::max_options`] response options.
    TooManyOptions { count: usize, max: usize },
    /// A response option's label exceeds the channel's label length limit.
    OptionLabelTooLong {
        key: String,
        chars: usize,
        max: usize,
    },
}

impl fmt::Display for OperatorValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OperatorValidationError::MissingRenderedSummary => {
                write!(f, "operator payload has no inline rendered summary")
            }
            OperatorValidationError::RenderedSummaryTooLong { chars, max } => {
                write!(
                    f,
                    "rendered summary is {chars} characters, exceeds the channel limit of {max}"
                )
            }
            OperatorValidationError::TooFewOptions { count, min } => {
                write!(
                    f,
                    "operator payload offers {count} response option(s), fewer than the minimum of {min}"
                )
            }
            OperatorValidationError::TooManyOptions { count, max } => {
                write!(
                    f,
                    "operator payload offers {count} response options, exceeds the maximum of {max}"
                )
            }
            OperatorValidationError::OptionLabelTooLong { key, chars, max } => {
                write!(
                    f,
                    "response option '{key}' label is {chars} characters, exceeds the channel limit of {max}"
                )
            }
        }
    }
}

impl std::error::Error for OperatorValidationError {}

/// An [`OperatorPayload`] that has passed [`validate`] against a declared
/// [`OperatorPayloadLimits`]. This is the only type the `notification`
/// channel accepts (`EN.8.A` task 4's gate-definition wiring emits against
/// this type, never the raw [`OperatorPayload`]) — so a payload that failed,
/// or was never run through, validation has no path onto that channel. The
/// wrapped payload is reachable only via [`Self::payload`]; there is no
/// public constructor other than [`validate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedOperatorPayload(OperatorPayload);

impl ValidatedOperatorPayload {
    /// The validated payload. Anything downstream that needs the rendered
    /// summary or options to build a notification-channel message reads it
    /// from here, never from a raw [`OperatorPayload`] it received directly.
    #[must_use]
    pub fn payload(&self) -> &OperatorPayload {
        &self.0
    }

    /// Consume the wrapper and return the validated payload.
    #[must_use]
    pub fn into_payload(self) -> OperatorPayload {
        self.0
    }
}

/// Validate `payload` against `limits`, in a fixed order so the first
/// rejection reason encountered is always the one reported: missing/empty
/// summary, then summary length, then option count bounds (too few before
/// too many), then each option's label length in declaration order.
///
/// `Ok` is the only way to obtain a [`ValidatedOperatorPayload`] — and
/// therefore the only way a payload can become eligible for the
/// `notification` channel. `Err` means this gate must declare the `session`
/// channel instead; that decision is not a suggestion the caller can
/// override by constructing a [`ValidatedOperatorPayload`] some other way,
/// because no other way exists.
pub fn validate(
    payload: OperatorPayload,
    limits: &OperatorPayloadLimits,
) -> Result<ValidatedOperatorPayload, OperatorValidationError> {
    if payload.rendered_summary.trim().is_empty() {
        return Err(OperatorValidationError::MissingRenderedSummary);
    }

    let summary_chars = payload.rendered_summary.chars().count();
    if summary_chars > limits.max_summary_chars {
        return Err(OperatorValidationError::RenderedSummaryTooLong {
            chars: summary_chars,
            max: limits.max_summary_chars,
        });
    }

    let option_count = payload.options.len();
    if option_count < limits.min_options {
        return Err(OperatorValidationError::TooFewOptions {
            count: option_count,
            min: limits.min_options,
        });
    }
    if option_count > limits.max_options {
        return Err(OperatorValidationError::TooManyOptions {
            count: option_count,
            max: limits.max_options,
        });
    }

    for option in &payload.options {
        let label_chars = option.label.chars().count();
        if label_chars > limits.max_label_chars {
            return Err(OperatorValidationError::OptionLabelTooLong {
                key: option.key.clone(),
                chars: label_chars,
                max: limits.max_label_chars,
            });
        }
    }

    Ok(ValidatedOperatorPayload(payload))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operator::payload::OperatorResponseOption;

    fn approve_reject() -> Vec<OperatorResponseOption> {
        vec![
            OperatorResponseOption::new("approve", "Approve"),
            OperatorResponseOption::new("reject", "Reject"),
        ]
    }

    #[test]
    fn valid_payload_passes() {
        let payload = OperatorPayload::new("gate-1", "diff summary", approve_reject());
        let limits = OperatorPayloadLimits::default();
        let validated = validate(payload.clone(), &limits).expect("should validate");
        assert_eq!(validated.payload(), &payload);
        assert_eq!(validated.into_payload(), payload);
    }

    #[test]
    fn rejects_empty_rendered_summary() {
        let payload = OperatorPayload::new("gate-1", "", approve_reject());
        let limits = OperatorPayloadLimits::default();
        assert_eq!(
            validate(payload, &limits),
            Err(OperatorValidationError::MissingRenderedSummary)
        );
    }

    #[test]
    fn rejects_whitespace_only_rendered_summary() {
        let payload = OperatorPayload::new("gate-1", "   \n\t  ", approve_reject());
        let limits = OperatorPayloadLimits::default();
        assert_eq!(
            validate(payload, &limits),
            Err(OperatorValidationError::MissingRenderedSummary)
        );
    }

    #[test]
    fn rejects_too_many_options() {
        let payload = OperatorPayload::new(
            "gate-1",
            "diff summary",
            vec![
                OperatorResponseOption::new("a", "A"),
                OperatorResponseOption::new("b", "B"),
                OperatorResponseOption::new("c", "C"),
                OperatorResponseOption::new("d", "D"),
            ],
        );
        let limits = OperatorPayloadLimits::default();
        assert_eq!(
            validate(payload, &limits),
            Err(OperatorValidationError::TooManyOptions { count: 4, max: 3 })
        );
    }

    #[test]
    fn rejects_too_few_options() {
        let payload = OperatorPayload::new(
            "gate-1",
            "diff summary",
            vec![OperatorResponseOption::new("approve", "Approve")],
        );
        let limits = OperatorPayloadLimits::default();
        assert_eq!(
            validate(payload, &limits),
            Err(OperatorValidationError::TooFewOptions { count: 1, min: 2 })
        );
    }

    #[test]
    fn rejects_zero_options() {
        let payload = OperatorPayload::new("gate-1", "diff summary", vec![]);
        let limits = OperatorPayloadLimits::default();
        assert_eq!(
            validate(payload, &limits),
            Err(OperatorValidationError::TooFewOptions { count: 0, min: 2 })
        );
    }

    #[test]
    fn rejects_label_exceeding_length_limit() {
        let long_label = "x".repeat(21);
        let payload = OperatorPayload::new(
            "gate-1",
            "diff summary",
            vec![
                OperatorResponseOption::new("approve", "Approve"),
                OperatorResponseOption::new("reject", long_label.clone()),
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
    }

    #[test]
    fn rejects_summary_exceeding_length_limit() {
        let payload = OperatorPayload::new("gate-1", "x".repeat(1025), approve_reject());
        let limits = OperatorPayloadLimits::default();
        assert_eq!(
            validate(payload, &limits),
            Err(OperatorValidationError::RenderedSummaryTooLong {
                chars: 1025,
                max: 1024,
            })
        );
    }

    #[test]
    fn custom_limits_are_honored_not_hardcoded() {
        // A payload that would be rejected under the default WhatsApp
        // limits validates cleanly under a looser deployment-supplied
        // limit set, proving the bounds are not baked in as magic numbers.
        let payload = OperatorPayload::new(
            "gate-1",
            "diff summary",
            vec![
                OperatorResponseOption::new("a", "A very long label indeed"),
                OperatorResponseOption::new("b", "B"),
                OperatorResponseOption::new("c", "C"),
                OperatorResponseOption::new("d", "D"),
            ],
        );
        let loose_limits = OperatorPayloadLimits {
            max_options: 10,
            min_options: 1,
            max_label_chars: 60,
            max_summary_chars: 4096,
        };
        assert!(validate(payload, &loose_limits).is_ok());
    }

    #[test]
    fn validated_payload_has_no_public_constructor_other_than_validate() {
        // Compile-time evidence: the only way to name a ValidatedOperatorPayload
        // value in this crate is through `validate`'s Ok arm. There is no
        // `ValidatedOperatorPayload::new`, no `From<OperatorPayload>`, and its
        // tuple field is private, so a failing gate literally cannot construct
        // one to hand to a notification-channel emitter.
        let payload = OperatorPayload::new("gate-1", "diff summary", approve_reject());
        let limits = OperatorPayloadLimits::default();
        let validated: ValidatedOperatorPayload =
            validate(payload, &limits).expect("valid payload");
        // The only accessors are read-only.
        let _: &OperatorPayload = validated.payload();
        let _: OperatorPayload = validated.into_payload();
    }
}
