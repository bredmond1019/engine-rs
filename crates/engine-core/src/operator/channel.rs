//! The two-channel operator routing declaration (`EN.8.A` task 4).
//!
//! Per `planning/8.A-operator-payload-contract/tasks.md` spec Invariant 2:
//! "Two channels, declared at gate-definition time, never degraded."
//! [`OperatorChannel`] is that declaration — `notification` for a reducible
//! decision that fits [`crate::operator::OperatorPayloadLimits`], or
//! `session-<slug>` for anything irreducible (judgement, credential,
//! drafting, anything open-ended). It is attached to a gate's *definition*
//! (`crate::nodes::harvest_gate::HarvestGate`, `EN.7.C`'s generic
//! materialize -> harvest gate primitive that this contract sits on top of,
//! per the spec's "Prior art" pointer), so which channel a gate routes to is
//! readable by inspecting that definition — no execution required.
//!
//! This module does not itself enforce "a gate that cannot produce a
//! conforming payload must declare `session`" — that enforcement is the
//! type-level guarantee `EN.8.A` task 3 already built: only a
//! [`crate::operator::ValidatedOperatorPayload`] may reach the
//! `notification` channel, and there is no way to construct one except via
//! [`crate::operator::validate`] succeeding. What this module adds is the
//! other half: the channel a gate *intends* to use is declared on the gate
//! itself, up front, rather than discovered from whether validation happened
//! to pass at emit time.

use serde::{Deserialize, Serialize};

/// Which channel an operator-facing gate routes to, declared on the gate's
/// definition rather than decided at emit time (`EN.8.A` spec Invariant 2).
///
/// Wire form is snake_case via the `kind` tag: `{"kind": "notification"}` or
/// `{"kind": "session", "slug": "..."}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OperatorChannel {
    /// A reducible decision: an inline rendered summary plus 2-3 named
    /// response options that fit within the declared
    /// [`crate::operator::OperatorPayloadLimits`]. Only a
    /// [`crate::operator::ValidatedOperatorPayload`] may actually emit on
    /// this channel — see the module docs.
    Notification,
    /// An irreducible decision: judgement, a credential, drafting, or
    /// anything open-ended that cannot be packaged as a bounded set of named
    /// options. Names the operator session it routes to.
    Session {
        /// The slug of the operator session this gate hands off to, e.g.
        /// `"dev-to-sweep-review"`.
        slug: String,
    },
}

impl OperatorChannel {
    /// Construct a `session-<slug>` channel declaration.
    #[must_use]
    pub fn session(slug: impl Into<String>) -> Self {
        Self::Session { slug: slug.into() }
    }

    /// Whether this declaration routes to the `notification` channel.
    #[must_use]
    pub fn is_notification(&self) -> bool {
        matches!(self, Self::Notification)
    }

    /// Whether this declaration routes to the `session-<slug>` channel.
    #[must_use]
    pub fn is_session(&self) -> bool {
        matches!(self, Self::Session { .. })
    }

    /// The session slug this declaration names, or `None` if this is a
    /// `notification` declaration.
    #[must_use]
    pub fn session_slug(&self) -> Option<&str> {
        match self {
            Self::Notification => None,
            Self::Session { slug } => Some(slug.as_str()),
        }
    }
}

impl Default for OperatorChannel {
    /// Defaults to `notification`. A gate that needs the irreducible path
    /// must say so explicitly via [`OperatorChannel::session`] — silence
    /// does not imply the harder channel, but the type-level guarantee from
    /// `EN.8.A` task 3 means a gate defaulted to `notification` that never
    /// produces a validated payload simply never emits, rather than emitting
    /// something degraded.
    fn default() -> Self {
        Self::Notification
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_notification() {
        assert_eq!(OperatorChannel::default(), OperatorChannel::Notification);
        assert!(OperatorChannel::default().is_notification());
        assert!(!OperatorChannel::default().is_session());
    }

    #[test]
    fn session_channel_names_its_slug() {
        let channel = OperatorChannel::session("dev-to-sweep-review");
        assert!(channel.is_session());
        assert!(!channel.is_notification());
        assert_eq!(channel.session_slug(), Some("dev-to-sweep-review"));
    }

    #[test]
    fn notification_channel_has_no_session_slug() {
        assert_eq!(OperatorChannel::Notification.session_slug(), None);
    }

    #[test]
    fn round_trips_through_serde_json_with_kind_tag() {
        let notification = OperatorChannel::Notification;
        let json = serde_json::to_value(&notification).expect("serialize");
        assert_eq!(json, serde_json::json!({"kind": "notification"}));
        let back: OperatorChannel = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, notification);

        let session = OperatorChannel::session("gate-review");
        let json = serde_json::to_value(&session).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({"kind": "session", "slug": "gate-review"})
        );
        let back: OperatorChannel = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, session);
    }
}
