//! `HarvestMode`/`HarvestGate` (`EN.7.C` task 1) — the generic materialize→harvest
//! gate primitive every pipeline inherits.
//!
//! Lives under `nodes/`, not under `workflows::content_pipeline`, because the
//! gate is workflow-agnostic: `EN.7.C` wires it into `PersistToBrainNode`
//! (task 4) and a future `HARVEST_APPROVE` micro-workflow (task 6) reuses the
//! same [`HarvestMode`]/pending-record shape, but any pipeline that
//! materializes a doc and then pushes it to Synapse's ingest endpoint can
//! reuse this module without depending on `content_pipeline` at all.
//!
//! **The default is `off`.** Per `planning/EN.7.C-materialize-harvest-gate/tasks.md`,
//! this is a deliberate behavior change, not a side effect: the default path
//! relies on the existing manifest / `index_brain` freshness reindex to index
//! a materialized doc, and an explicit Synapse `POST /ingest/*` is reserved
//! for instant/curated indexing (`in_process`) or a human-approval hand-off
//! (`approval`).
//!
//! `HarvestGate::decide()` is the single place a node asks "what do I do with
//! this harvest push right now?" — `Post` (push now), `Skip` (do nothing; the
//! freshness reindex will pick it up), or `Defer` (build a pending record for
//! a later `HARVEST_APPROVE` run to complete). [`pending_harvest_record`] is
//! the one constructor for that pending-record shape, shared by the deferring
//! node (`PersistToBrainNode`, task 4) and the approval node
//! (`HarvestApproveNode`, task 6) so the two can never drift out of sync on
//! what fields the record carries.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The three ways a materialized doc can (or cannot) reach Synapse's ingest
/// endpoint. Wire form is snake_case: `"off" | "in_process" | "approval"`.
///
/// `Default` is `Off` — the existing manifest / `index_brain` freshness
/// reindex already indexes the written `.md`; an explicit ingest POST is only
/// for instant/curated indexing (`InProcess`) or a deferred, human-approved
/// push (`Approval`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum HarvestMode {
    /// No ingest POST. Indexing is left to the existing manifest /
    /// `index_brain` freshness reindex. This is the built-in default.
    #[default]
    Off,
    /// POST the finished artifact to Synapse's ingest endpoint in-process,
    /// during the run — instant/curated indexing.
    InProcess,
    /// No ingest POST during the run. Instead, a pending record
    /// ([`pending_harvest_record`]) is stamped so an operator can complete
    /// the harvest later by feeding that record to the `HARVEST_APPROVE`
    /// micro-workflow, which POSTs the exact same payload to the same URL.
    Approval,
}

/// What [`HarvestGate::decide`] tells a node to do with an about-to-happen
/// ingest push.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarvestDecision {
    /// Push now, over the injectable `HttpPost` seam.
    Post,
    /// Do nothing; the existing freshness reindex will pick this up.
    Skip,
    /// Do not push now; build a pending record for a later `HARVEST_APPROVE`
    /// run to complete.
    Defer,
}

/// The resolved per-run gate a node holds: just the resolved [`HarvestMode`],
/// with the decision logic factored onto [`HarvestGate::decide`] so a node
/// never has to match on `HarvestMode` itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HarvestGate {
    mode: HarvestMode,
}

impl HarvestGate {
    /// Construct a gate resolved to `mode`.
    #[must_use]
    pub fn new(mode: HarvestMode) -> Self {
        Self { mode }
    }

    /// The resolved mode this gate holds.
    #[must_use]
    pub fn mode(&self) -> HarvestMode {
        self.mode
    }

    /// What a node should do given this gate's resolved mode.
    #[must_use]
    pub fn decide(&self) -> HarvestDecision {
        match self.mode {
            HarvestMode::Off => HarvestDecision::Skip,
            HarvestMode::InProcess => HarvestDecision::Post,
            HarvestMode::Approval => HarvestDecision::Defer,
        }
    }
}

/// The single constructor for the deferred-harvest pending-record shape:
/// `{artifact_id, url, payload, doc_paths}`. Shared by the node that defers a
/// harvest (`PersistToBrainNode` under `HarvestMode::Approval`, task 4) and
/// the node that completes one (`HarvestApproveNode`, task 6) so the two can
/// never drift on what fields the record carries.
///
/// `payload` is stored verbatim — the exact JSON body an `in_process` push
/// would have POSTed to `url` — so the eventual `HARVEST_APPROVE` push is
/// byte-identical to what the in-process path would have sent.
#[must_use]
pub fn pending_harvest_record(
    artifact_id: impl Into<String>,
    url: impl Into<String>,
    payload: Value,
    doc_paths: Vec<String>,
) -> Value {
    json!({
        "artifact_id": artifact_id.into(),
        "url": url.into(),
        "payload": payload,
        "doc_paths": doc_paths,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harvest_mode_round_trips_through_serde_snake_case() {
        for (mode, wire) in [
            (HarvestMode::Off, "\"off\""),
            (HarvestMode::InProcess, "\"in_process\""),
            (HarvestMode::Approval, "\"approval\""),
        ] {
            let serialized = serde_json::to_string(&mode).unwrap();
            assert_eq!(serialized, wire);
            let deserialized: HarvestMode = serde_json::from_str(wire).unwrap();
            assert_eq!(deserialized, mode);
        }
    }

    #[test]
    fn harvest_mode_default_is_off() {
        assert_eq!(HarvestMode::default(), HarvestMode::Off);
    }

    #[test]
    fn harvest_gate_default_is_off() {
        assert_eq!(HarvestGate::default().mode(), HarvestMode::Off);
    }

    #[test]
    fn decide_maps_each_mode_to_its_decision() {
        assert_eq!(
            HarvestGate::new(HarvestMode::Off).decide(),
            HarvestDecision::Skip
        );
        assert_eq!(
            HarvestGate::new(HarvestMode::InProcess).decide(),
            HarvestDecision::Post
        );
        assert_eq!(
            HarvestGate::new(HarvestMode::Approval).decide(),
            HarvestDecision::Defer
        );
    }

    #[test]
    fn pending_harvest_record_has_exactly_the_four_documented_keys() {
        let payload = json!({"title": "some artifact", "body": "content"});
        let record = pending_harvest_record(
            "artifact-123",
            "https://synapse.example/ingest/learning-artifact",
            payload.clone(),
            vec!["docs/foo.md".to_string()],
        );
        let obj = record.as_object().expect("record is a JSON object");
        assert_eq!(obj.len(), 4);
        assert_eq!(obj.get("artifact_id").unwrap(), "artifact-123");
        assert_eq!(
            obj.get("url").unwrap(),
            "https://synapse.example/ingest/learning-artifact"
        );
        assert_eq!(obj.get("payload").unwrap(), &payload);
        assert_eq!(obj.get("doc_paths").unwrap(), &json!(["docs/foo.md"]));
    }

    #[test]
    fn pending_harvest_record_preserves_payload_verbatim() {
        let payload = json!({
            "nested": {"a": 1, "b": [1, 2, 3]},
            "unicode": "café",
        });
        let record = pending_harvest_record("id", "url", payload.clone(), vec![]);
        assert_eq!(record["payload"], payload);
        assert_eq!(record["doc_paths"], json!([]));
    }
}
