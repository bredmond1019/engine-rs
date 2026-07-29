//! The inbound event schema for `HARVEST_APPROVE` (`EN.7.C` task 6): the
//! pending-harvest record shape `crate::nodes::harvest_gate::
//! pending_harvest_record` stamps, `{artifact_id, url, payload, doc_paths}`.
//!
//! Mirrors `crate::workflows::opportunity_edit::schema`'s no-policy shape:
//! this workflow is deterministic and model-free, so there is no
//! `policy`/`profile` override pair to carry.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Inbound event schema for `HARVEST_APPROVE`: the pending-harvest record an
/// operator feeds back in to complete a deferred (`HarvestMode::Approval`)
/// harvest. `payload` is stored as an opaque `Value` — it is whatever
/// `build_learning_artifact_payload` produced when the harvest was deferred,
/// and this workflow POSTs it verbatim rather than re-deriving or
/// re-validating its shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HarvestApproveEvent {
    /// The artifact identifier the pending harvest was stamped for.
    pub artifact_id: String,
    /// The Synapse ingest endpoint URL the deferred push targets.
    pub url: String,
    /// The exact JSON body an `in_process` push would have POSTed to `url`.
    pub payload: Value,
    /// The materialized doc path(s) written before the harvest was
    /// deferred, carried through for operator visibility.
    #[serde(default)]
    pub doc_paths: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn harvest_approve_event_round_trips_through_serde_json() {
        let event = HarvestApproveEvent {
            artifact_id: "artifact-1".to_string(),
            url: "https://brain.example/ingest/learning".to_string(),
            payload: json!({"artifact_id": "artifact-1"}),
            doc_paths: vec!["brain/content/learning/artifact-1.md".to_string()],
        };
        let json = serde_json::to_string(&event).expect("serializes");
        let round_tripped: HarvestApproveEvent = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(event, round_tripped);
    }

    #[test]
    fn harvest_approve_event_deserializes_from_expected_payload_shape() {
        let json = json!({
            "artifact_id": "artifact-1",
            "url": "https://brain.example/ingest/learning",
            "payload": {"artifact_id": "artifact-1", "summary": "A digest."},
            "doc_paths": ["brain/content/learning/artifact-1.md"],
        });
        let event: HarvestApproveEvent = serde_json::from_value(json).expect("deserializes");
        assert_eq!(event.artifact_id, "artifact-1");
        assert_eq!(event.url, "https://brain.example/ingest/learning");
        assert_eq!(
            event.doc_paths,
            vec!["brain/content/learning/artifact-1.md"]
        );
    }

    #[test]
    fn harvest_approve_event_defaults_doc_paths_when_absent() {
        let json = json!({
            "artifact_id": "artifact-1",
            "url": "https://brain.example/ingest/learning",
            "payload": {"artifact_id": "artifact-1"},
        });
        let event: HarvestApproveEvent = serde_json::from_value(json).expect("deserializes");
        assert_eq!(event.doc_paths, Vec::<String>::new());
    }

    #[test]
    fn harvest_approve_event_missing_url_fails_to_deserialize() {
        let json = json!({
            "artifact_id": "artifact-1",
            "payload": {"artifact_id": "artifact-1"},
        });
        let result: Result<HarvestApproveEvent, _> = serde_json::from_value(json);
        assert!(result.is_err());
    }

    #[test]
    fn harvest_approve_event_missing_payload_fails_to_deserialize() {
        let json = json!({
            "artifact_id": "artifact-1",
            "url": "https://brain.example/ingest/learning",
        });
        let result: Result<HarvestApproveEvent, _> = serde_json::from_value(json);
        assert!(result.is_err());
    }

    #[test]
    fn harvest_approve_event_missing_artifact_id_fails_to_deserialize() {
        let json = json!({
            "url": "https://brain.example/ingest/learning",
            "payload": {"artifact_id": "artifact-1"},
        });
        let result: Result<HarvestApproveEvent, _> = serde_json::from_value(json);
        assert!(result.is_err());
    }
}
