//! The `task_context` JSON shape (contract §5–§6) — `TaskContext`, `NodeRun`,
//! `NodeRunStatus`, and `Usage`.
//!
//! Source of truth: `orchestrator/docs/data-contract.md` v1.0.1. Node identity is the
//! node's Python class name (contract §1) and is used as the map key in both
//! `TaskContext::nodes` and `TaskContext::node_runs`.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Per-node execution lifecycle status (contract §6). Serializes/deserializes as the
/// lowercase strings `pending` | `running` | `success` | `failed` — **no other casing
/// is contract-compliant.**
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeRunStatus {
    Pending,
    Running,
    Success,
    Failed,
}

/// Token/model usage for LLM nodes (contract §6). `None` on the containing `NodeRun`
/// serializes as JSON `null`; non-LLM nodes carry no usage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub model: String,
}

/// Per-node execution envelope, keyed by class name in `TaskContext::node_runs`
/// (contract §6). `started_at`/`completed_at` are ISO-8601 UTC timestamps, set on
/// entry / on success-or-failure respectively (`null` while pending).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeRun {
    pub status: NodeRunStatus,
    /// `null` on the wire when unset — the field is always present (contract §6),
    /// so this does not use `skip_serializing_if`.
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub input: Option<serde_json::Value>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

/// The JSON serialization of the orchestrator's `TaskContext` (contract §5):
/// `{ event, nodes: {<ClassName>: output}, metadata, node_runs: {<ClassName>: NodeRun} }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskContext {
    pub event: serde_json::Value,
    pub nodes: HashMap<String, serde_json::Value>,
    pub metadata: serde_json::Value,
    pub node_runs: HashMap<String, NodeRun>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_run_status_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&NodeRunStatus::Pending).unwrap(),
            "\"pending\""
        );
        assert_eq!(
            serde_json::to_string(&NodeRunStatus::Running).unwrap(),
            "\"running\""
        );
        assert_eq!(
            serde_json::to_string(&NodeRunStatus::Success).unwrap(),
            "\"success\""
        );
        assert_eq!(
            serde_json::to_string(&NodeRunStatus::Failed).unwrap(),
            "\"failed\""
        );
    }

    #[test]
    fn node_run_status_deserializes_lowercase() {
        assert_eq!(
            serde_json::from_str::<NodeRunStatus>("\"pending\"").unwrap(),
            NodeRunStatus::Pending
        );
        assert_eq!(
            serde_json::from_str::<NodeRunStatus>("\"running\"").unwrap(),
            NodeRunStatus::Running
        );
        assert_eq!(
            serde_json::from_str::<NodeRunStatus>("\"success\"").unwrap(),
            NodeRunStatus::Success
        );
        assert_eq!(
            serde_json::from_str::<NodeRunStatus>("\"failed\"").unwrap(),
            NodeRunStatus::Failed
        );
    }

    #[test]
    fn node_run_status_rejects_unknown_casing() {
        assert!(serde_json::from_str::<NodeRunStatus>("\"Pending\"").is_err());
        assert!(serde_json::from_str::<NodeRunStatus>("\"PENDING\"").is_err());
    }

    #[test]
    fn usage_serializes_object_when_present() {
        let run = NodeRun {
            status: NodeRunStatus::Success,
            started_at: None,
            completed_at: None,
            error: None,
            input: None,
            usage: Some(Usage {
                input_tokens: Some(512),
                output_tokens: Some(0),
                model: "text-embedding-3-small".to_string(),
            }),
        };
        let v = serde_json::to_value(&run).unwrap();
        assert_eq!(
            v["usage"],
            serde_json::json!({
                "input_tokens": 512,
                "output_tokens": 0,
                "model": "text-embedding-3-small",
            })
        );
    }

    #[test]
    fn usage_serializes_null_when_absent() {
        let run = NodeRun {
            status: NodeRunStatus::Pending,
            started_at: None,
            completed_at: None,
            error: None,
            input: None,
            usage: None,
        };
        let v = serde_json::to_value(&run).unwrap();
        assert!(v["usage"].is_null());
    }

    #[test]
    fn usage_deserializes_null_to_none() {
        let json = serde_json::json!({
            "status": "pending",
            "started_at": null,
            "completed_at": null,
            "error": null,
            "input": null,
            "usage": null,
        });
        let run: NodeRun = serde_json::from_value(json).unwrap();
        assert!(run.usage.is_none());
    }

    #[test]
    fn task_context_round_trips_shape() {
        let mut node_runs = HashMap::new();
        node_runs.insert(
            "DataIngestionNode".to_string(),
            NodeRun {
                status: NodeRunStatus::Success,
                started_at: Some(Utc::now()),
                completed_at: Some(Utc::now()),
                error: None,
                input: None,
                usage: None,
            },
        );
        let mut nodes = HashMap::new();
        nodes.insert(
            "DataIngestionNode".to_string(),
            serde_json::json!({ "output": { "documents_loaded": 3 } }),
        );

        let tc = TaskContext {
            event: serde_json::json!({ "ticket_id": "T-1" }),
            nodes,
            metadata: serde_json::json!({ "workflow": "sdlc-flow" }),
            node_runs,
        };

        let v = serde_json::to_value(&tc).unwrap();
        assert!(v.get("event").is_some());
        assert!(v.get("nodes").is_some());
        assert!(v.get("metadata").is_some());
        assert!(v.get("node_runs").is_some());

        let round_tripped: TaskContext = serde_json::from_value(v).unwrap();
        assert_eq!(round_tripped, tc);
    }
}
