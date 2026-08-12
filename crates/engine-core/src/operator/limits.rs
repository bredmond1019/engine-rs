//! Confirmed operator-channel interactive-reply limits (`EN.8.A` task 1).
//!
//! Per `planning/8.A-operator-payload-contract/tasks.md`, the "~3 buttons,
//! short labels" figure carried in `initial-architecture.md` §7.5 is
//! explicitly flagged as unconfirmed. It has now been confirmed against
//! Meta's current WhatsApp Cloud API documentation and is expressed here as
//! configuration ([`OperatorPayloadLimits`]) rather than as a magic number
//! scattered through validation code (`EN.8.A` task 3).
//!
//! ## Confirmed limits — checked 2026-08-12
//!
//! Source: Meta for Developers, WhatsApp Cloud API reference —
//! <https://developers.facebook.com/docs/whatsapp/cloud-api/messages/interactive-reply-buttons-messages/>
//! and <https://developers.facebook.com/docs/whatsapp/cloud-api/messages/interactive-list-messages/>.
//!
//! | Limit | Value | Source field |
//! |---|---|---|
//! | Max reply buttons per interactive message | **3** | `action.buttons` (max 3 objects) |
//! | Max button title length | **20** characters | `action.buttons[].reply.title` |
//! | Max body text length | **1024** characters | `body.text` |
//! | Max list-message rows (across all sections) | **10** | `action.sections[].rows` |
//! | Max list-message row title length | **24** characters | `action.sections[].rows[].title` |
//!
//! This block's contract (`EN.8.A`) targets the tighter reply-buttons shape —
//! at most 3 named response options, each within the button title length —
//! per the spec's Invariant 3: "design against the narrowest target now." A
//! payload that cannot fit within these limits does not fall back to a list
//! message; per Invariant 2 it is rejected and the gate must declare the
//! `session` channel instead (`EN.8.A` tasks 3-4).
//!
//! If WhatsApp's platform limits change, re-confirm against the current docs
//! and update both the table above and the constants' doc comments with the
//! new date — do not silently bump the numbers without updating the record.

use serde::{Deserialize, Serialize};

/// WhatsApp's confirmed maximum number of interactive reply buttons per
/// message, as of 2026-08-12. See the module docs for the source and the
/// full confirmed-limits table.
pub const WHATSAPP_MAX_REPLY_BUTTONS: usize = 3;

/// WhatsApp's confirmed minimum number of interactive reply buttons that
/// still constitutes a real choice. Not a platform limit (WhatsApp permits a
/// single button) — this is `EN.8.A`'s own floor: a "decision, never a
/// task" payload (spec Invariant 1) offers the operator an actual choice, so
/// exactly one option is rejected the same as zero.
pub const OPERATOR_MIN_RESPONSE_OPTIONS: usize = 2;

/// WhatsApp's confirmed maximum reply-button label length in characters, as
/// of 2026-08-12. See the module docs for the source.
pub const WHATSAPP_MAX_BUTTON_LABEL_CHARS: usize = 20;

/// WhatsApp's confirmed maximum interactive-message body text length in
/// characters, as of 2026-08-12. See the module docs for the source.
pub const WHATSAPP_MAX_BODY_CHARS: usize = 1024;

/// The configurable limits an operator payload must satisfy before it may be
/// routed to the `notification` channel. Defaults to the confirmed WhatsApp
/// reply-buttons limits (see module docs); a deployment targeting a looser
/// channel (e.g. Telegram) may override these, but the built-in default
/// stays pinned to the narrowest target per the spec's Invariant 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct OperatorPayloadLimits {
    /// Maximum number of named response options a payload may offer.
    pub max_options: usize,
    /// Minimum number of named response options a payload must offer for
    /// the choice to be real (see [`OPERATOR_MIN_RESPONSE_OPTIONS`]).
    pub min_options: usize,
    /// Maximum character length of a single response option's label.
    pub max_label_chars: usize,
    /// Maximum character length of the inline rendered summary body.
    pub max_summary_chars: usize,
}

impl Default for OperatorPayloadLimits {
    /// The confirmed WhatsApp interactive reply-buttons limits, checked
    /// 2026-08-12 — see the module docs for the source and full table.
    fn default() -> Self {
        Self {
            max_options: WHATSAPP_MAX_REPLY_BUTTONS,
            min_options: OPERATOR_MIN_RESPONSE_OPTIONS,
            max_label_chars: WHATSAPP_MAX_BUTTON_LABEL_CHARS,
            max_summary_chars: WHATSAPP_MAX_BODY_CHARS,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_matches_confirmed_whatsapp_limits() {
        let limits = OperatorPayloadLimits::default();
        assert_eq!(limits.max_options, 3);
        assert_eq!(limits.min_options, 2);
        assert_eq!(limits.max_label_chars, 20);
        assert_eq!(limits.max_summary_chars, 1024);
    }

    #[test]
    fn limits_are_configurable_not_hardcoded() {
        let custom = OperatorPayloadLimits {
            max_options: 10,
            min_options: 1,
            max_label_chars: 60,
            max_summary_chars: 4096,
        };
        assert_ne!(custom, OperatorPayloadLimits::default());
    }

    #[test]
    fn round_trips_through_serde_json() {
        let limits = OperatorPayloadLimits::default();
        let json = serde_json::to_string(&limits).expect("serialize");
        let back: OperatorPayloadLimits = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(limits, back);
    }
}
