//! Resend delivery/bounce webhook -> `EN.7.B`'s `AddOpportunityActionEvent`
//! (`EN.6.B` task 5).
//!
//! Correlation is the TAG ECHO and nothing else: `EmailChannelTransport`
//! stamps `opportunity_slug` as a Resend tag on send (task 2), Resend echoes
//! it back on the event, and this maps it to an add-action event. No address
//! lookup, no corpus scan — per THE BOUNDARY TEST this repo never reads the
//! index.
//!
//! `mev`'s `plan_add_action` is idempotent on an identical `{at, kind, note}`
//! triple, so a redelivered webhook plans zero actions — which is why the
//! mapping below must be a pure function of the payload, with no clock read.

use crate::workflows::opportunity_edit::schema::AddOpportunityActionEvent;
use serde_json::Value;

/// Event types this module maps, and the `kind` each produces on the
/// resulting [`AddOpportunityActionEvent`]. Any `type` not in this table
/// yields [`DeliveryMapping::Skipped`]`(`[`SkipReason::UnhandledEventType`]`)`.
const HANDLED: [(&str, &str); 3] = [
    ("email.delivered", "email-delivered"),
    ("email.bounced", "email-bounced"),
    ("email.complained", "email-complained"),
];

/// Why a payload mapped to no add-action event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// `payload["type"]` is not one of [`HANDLED`].
    UnhandledEventType,
    /// The event's echoed tags carry no `opportunity_slug` entry.
    NoOpportunitySlugTag,
}

impl SkipReason {
    /// A stable, machine-readable reason string for a JSON response body.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            SkipReason::UnhandledEventType => "unhandled_event_type",
            SkipReason::NoOpportunitySlugTag => "no_opportunity_slug_tag",
        }
    }
}

/// What [`map_delivery_event`] resolved to.
#[derive(Debug, Clone, PartialEq)]
pub enum DeliveryMapping {
    /// The payload maps to a well-formed add-action event.
    Action(AddOpportunityActionEvent),
    /// The payload was handled correctly but produces no action.
    Skipped(SkipReason),
}

/// Map a Resend delivery/bounce webhook payload to an
/// [`AddOpportunityActionEvent`], correlating solely via the echoed
/// `opportunity_slug` tag (see the module doc).
///
/// Mapping:
/// 1. `payload["type"]` must be a string, else `Err`.
/// 2. `kind` is looked up from [`HANDLED`]; an unrecognized type is
///    `Ok(DeliveryMapping::Skipped(UnhandledEventType))`.
/// 3. `slug` is read from `payload["data"]["tags"]`, accepting both shapes
///    Resend can produce: an array of `{"name","value"}` objects, or a flat
///    `{name: value}` object. Absent is
///    `Ok(DeliveryMapping::Skipped(NoOpportunitySlugTag))`.
/// 4. `at` comes from `payload["created_at"]`, falling back to
///    `payload["data"]["created_at"]`; absent is an `Err`. Never
///    `Utc::now()` — a clock read would break the idempotency the module
///    doc relies on.
/// 5. `note` is a short, factual line naming the event type and recipient
///    (`payload["data"]["to"]`, first entry if an array).
pub fn map_delivery_event(payload: &Value) -> Result<DeliveryMapping, String> {
    let event_type = payload
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| "email webhook: missing 'type'".to_string())?;

    let Some((_, kind)) = HANDLED.iter().find(|(name, _)| *name == event_type) else {
        return Ok(DeliveryMapping::Skipped(SkipReason::UnhandledEventType));
    };

    let Some(slug) = extract_opportunity_slug(payload) else {
        return Ok(DeliveryMapping::Skipped(SkipReason::NoOpportunitySlugTag));
    };

    let at = payload
        .get("created_at")
        .and_then(Value::as_str)
        .or_else(|| {
            payload
                .get("data")
                .and_then(|data| data.get("created_at"))
                .and_then(Value::as_str)
        })
        .ok_or_else(|| "email webhook: missing 'created_at'".to_string())?
        .to_string();

    let recipient = extract_recipient(payload).unwrap_or_else(|| "unknown recipient".to_string());
    let note = format!("Resend reported {event_type} for {recipient}");

    Ok(DeliveryMapping::Action(AddOpportunityActionEvent {
        slug,
        at,
        kind: (*kind).to_string(),
        note,
    }))
}

/// Read `opportunity_slug` back off `payload["data"]["tags"]`, accepting
/// both the array-of-objects and flat-object shapes Resend can produce.
fn extract_opportunity_slug(payload: &Value) -> Option<String> {
    let tags = payload.get("data")?.get("tags")?;

    if let Some(array) = tags.as_array() {
        for entry in array {
            let name = entry.get("name").and_then(Value::as_str);
            if name == Some("opportunity_slug") {
                return entry
                    .get("value")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
        }
        return None;
    }

    if let Some(object) = tags.as_object() {
        return object
            .get("opportunity_slug")
            .and_then(Value::as_str)
            .map(str::to_string);
    }

    None
}

/// The recipient address, from `payload["data"]["to"]` — either a plain
/// string or the first entry of an array.
fn extract_recipient(payload: &Value) -> Option<String> {
    let to = payload.get("data")?.get("to")?;
    if let Some(s) = to.as_str() {
        return Some(s.to_string());
    }
    to.as_array()?.first()?.as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(event_type: &str, tags: Value) -> Value {
        json!({
            "type": event_type,
            "created_at": "2026-07-30T12:00:00Z",
            "data": {
                "tags": tags,
                "to": ["contact@acme-corp.example"],
            }
        })
    }

    #[test]
    fn bounce_with_a_slug_tag_maps_to_an_add_action_event() {
        let payload = event(
            "email.bounced",
            json!([{"name": "opportunity_slug", "value": "acme-corp"}]),
        );
        let mapping = map_delivery_event(&payload).unwrap();
        match mapping {
            DeliveryMapping::Action(action) => {
                assert_eq!(action.slug, "acme-corp");
                assert_eq!(action.kind, "email-bounced");
                assert_eq!(action.at, "2026-07-30T12:00:00Z");
                assert!(!action.note.is_empty());
                assert!(action.note.contains("contact@acme-corp.example"));
            }
            DeliveryMapping::Skipped(reason) => {
                panic!("expected an action, got skipped: {reason:?}")
            }
        }
    }

    #[test]
    fn delivery_maps_with_its_own_kind() {
        let payload = event(
            "email.delivered",
            json!([{"name": "opportunity_slug", "value": "acme-corp"}]),
        );
        let mapping = map_delivery_event(&payload).unwrap();
        match mapping {
            DeliveryMapping::Action(action) => assert_eq!(action.kind, "email-delivered"),
            other => panic!("expected an action, got {other:?}"),
        }
    }

    #[test]
    fn complaint_maps_with_its_own_kind() {
        let payload = event(
            "email.complained",
            json!([{"name": "opportunity_slug", "value": "acme-corp"}]),
        );
        let mapping = map_delivery_event(&payload).unwrap();
        match mapping {
            DeliveryMapping::Action(action) => assert_eq!(action.kind, "email-complained"),
            other => panic!("expected an action, got {other:?}"),
        }
    }

    #[test]
    fn tags_as_a_flat_object_are_also_read() {
        let payload = event("email.bounced", json!({"opportunity_slug": "acme-corp"}));
        let mapping = map_delivery_event(&payload).unwrap();
        match mapping {
            DeliveryMapping::Action(action) => assert_eq!(action.slug, "acme-corp"),
            other => panic!("expected an action, got {other:?}"),
        }
    }

    #[test]
    fn no_slug_tag_is_skipped_not_an_error() {
        let payload = event("email.bounced", json!([]));
        let mapping = map_delivery_event(&payload).unwrap();
        assert_eq!(
            mapping,
            DeliveryMapping::Skipped(SkipReason::NoOpportunitySlugTag)
        );
    }

    #[test]
    fn unknown_event_type_is_skipped() {
        let payload = event(
            "email.opened",
            json!([{"name": "opportunity_slug", "value": "acme-corp"}]),
        );
        let mapping = map_delivery_event(&payload).unwrap();
        assert_eq!(
            mapping,
            DeliveryMapping::Skipped(SkipReason::UnhandledEventType)
        );
    }

    #[test]
    fn missing_type_is_an_error() {
        let payload = json!({
            "created_at": "2026-07-30T12:00:00Z",
            "data": {"tags": [], "to": ["contact@acme-corp.example"]}
        });
        assert!(map_delivery_event(&payload).is_err());
    }

    #[test]
    fn missing_created_at_is_an_error() {
        let payload = json!({
            "type": "email.bounced",
            "data": {
                "tags": [{"name": "opportunity_slug", "value": "acme-corp"}],
                "to": ["contact@acme-corp.example"],
            }
        });
        assert!(map_delivery_event(&payload).is_err());
    }

    #[test]
    fn the_same_payload_maps_twice_to_an_identical_event() {
        let payload = event(
            "email.bounced",
            json!([{"name": "opportunity_slug", "value": "acme-corp"}]),
        );
        let first = map_delivery_event(&payload).unwrap();
        let second = map_delivery_event(&payload).unwrap();
        assert_eq!(first, second);
    }
}
