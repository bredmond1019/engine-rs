//! `EventsRow` — the `events` table row shape (contract §4).
//!
//! One row per workflow run. There are no relational `workflow_runs` / `node_states`
//! tables; all run/node state lives in `task_context` (see `task_context.rs`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::task_context::TaskContext;

/// Mirrors the `events` table (contract §4): `id`, `workflow_type` (registry key,
/// `varchar(150)`), `data` (the run input, verbatim), `task_context` (full execution
/// state), `created_at`, `updated_at`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventsRow {
    pub id: Uuid,
    pub workflow_type: String,
    pub data: serde_json::Value,
    pub task_context: TaskContext,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_context::{NodeRun, NodeRunStatus};
    use std::collections::HashMap;

    #[test]
    fn events_row_round_trips_through_serde_json() {
        let mut node_runs = HashMap::new();
        node_runs.insert(
            "DataIngestionNode".to_string(),
            NodeRun {
                status: NodeRunStatus::Pending,
                started_at: None,
                completed_at: None,
                error: None,
                input: None,
                usage: None,
            },
        );

        let row = EventsRow {
            id: Uuid::new_v4(),
            workflow_type: "sdlc-flow".to_string(),
            data: serde_json::json!({ "ticket_id": "T-1" }),
            task_context: TaskContext {
                event: serde_json::json!({ "ticket_id": "T-1" }),
                nodes: HashMap::new(),
                metadata: serde_json::json!({}),
                node_runs,
            },
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let json = serde_json::to_string(&row).unwrap();
        let round_tripped: EventsRow = serde_json::from_str(&json).unwrap();
        assert_eq!(round_tripped, row);
    }

    #[test]
    fn events_row_has_contract_top_level_fields() {
        let row = EventsRow {
            id: Uuid::new_v4(),
            workflow_type: "sdlc-flow".to_string(),
            data: serde_json::json!({}),
            task_context: TaskContext {
                event: serde_json::json!({}),
                nodes: HashMap::new(),
                metadata: serde_json::json!({}),
                node_runs: HashMap::new(),
            },
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let v = serde_json::to_value(&row).unwrap();
        for key in [
            "id",
            "workflow_type",
            "data",
            "task_context",
            "created_at",
            "updated_at",
        ] {
            assert!(v.get(key).is_some(), "missing top-level field: {key}");
        }
    }
}
