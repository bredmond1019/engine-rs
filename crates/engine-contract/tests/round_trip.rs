//! Byte-for-byte (semantic JSON) round-trip guard against the orchestrator data
//! contract (`orchestrator/docs/data-contract.md` v1.0.1). Any drift here breaks
//! `bastion` (engine-rs context.md governing principle 4).
//!
//! `FIXTURE` is a **captured, code-path-produced** `task_context` — the output of a
//! real `ResearchAgentWorkflow` run in the orchestrator repo
//! (`orchestrator/scripts/emit_task_context_fixture.py`), not hand-authored here. See
//! `tests/fixtures/README.md` for full provenance and the `python_task_context.json`
//! it replaced (a fixture this crate had written about itself during EN.0.B — see
//! `orchestrator/planning/task-context-fixture/notes.md` for the finding).

use engine_contract::{EventsRow, TaskContext};

const FIXTURE: &str = include_str!("../../../tests/fixtures/research_agent_task_context.json");

/// (a) Deserialize the captured orchestrator-shaped fixture into `TaskContext` and
/// re-serialize; assert semantic JSON equality (parse both sides to
/// `serde_json::Value`, field order doesn't matter) with no field/casing/type drift.
///
/// This asserts against `TaskContext` (contract §5), not `EventsRow` (contract §4):
/// the orchestrator emits and owns a `task_context` value, not a full `events` row —
/// the surrounding row fields (`id`, `created_at`, ...) are engine-rs/Postgres's own
/// concern and are exercised separately by `rust_constructed_events_row_matches_contract_shape`.
#[test]
fn fixture_round_trips_with_no_field_or_casing_drift() {
    let original: serde_json::Value = serde_json::from_str(FIXTURE).expect("fixture is valid JSON");

    let task_context: TaskContext =
        serde_json::from_str(FIXTURE).expect("fixture deserializes into TaskContext");
    let re_serialized =
        serde_json::to_value(&task_context).expect("TaskContext serializes back to JSON");

    assert_eq!(
        re_serialized, original,
        "re-serialized TaskContext must be semantically identical to the captured fixture"
    );
}

/// Divergence guard for the "owner-local + checked-in copy" fixture-sharing pattern
/// (see `tests/fixtures/README.md`): if a sibling `orchestrator` checkout is present
/// (both repos cloned under the same parent, the common layout), assert this crate's
/// copy of the fixture is byte-identical to the orchestrator-owned original. Skips
/// silently when the sibling isn't present — this crate must stay standalone-clonable.
#[test]
fn fixture_matches_orchestrator_owned_original_when_sibling_checkout_present() {
    let sibling = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../orchestrator/tests/fixtures/task_context/research_agent_task_context.json");

    let Ok(canonical) = sibling.canonicalize() else {
        eprintln!(
            "skipping: no sibling orchestrator checkout at {}",
            sibling.display()
        );
        return;
    };

    let owned = std::fs::read_to_string(&canonical)
        .expect("sibling orchestrator fixture exists but could not be read");
    let owned_value: serde_json::Value =
        serde_json::from_str(&owned).expect("orchestrator fixture is valid JSON");
    let copy_value: serde_json::Value =
        serde_json::from_str(FIXTURE).expect("checked-in copy is valid JSON");

    assert_eq!(
        copy_value,
        owned_value,
        "engine-rs's checked-in fixture copy has diverged from the orchestrator-owned \
         original at {} — re-copy it and re-run `cargo test`",
        canonical.display()
    );
}

/// (b) Construct an equivalent `EventsRow`/`TaskContext` in Rust from scratch and
/// assert its emitted JSON matches the contract shape (top-level keys, `node_runs`
/// entry shape, lowercase status, `usage` null-or-object).
#[test]
fn rust_constructed_events_row_matches_contract_shape() {
    use chrono::{TimeZone, Utc};
    use engine_contract::{NodeRun, NodeRunStatus, TaskContext, Usage};
    use std::collections::HashMap;
    use uuid::Uuid;

    let mut node_runs = HashMap::new();
    node_runs.insert(
        "DataIngestionNode".to_string(),
        NodeRun {
            status: NodeRunStatus::Success,
            started_at: Some(Utc.with_ymd_and_hms(2026, 6, 20, 9, 0, 0).unwrap()),
            completed_at: Some(Utc.with_ymd_and_hms(2026, 6, 20, 9, 0, 3).unwrap()),
            error: None,
            input: None,
            usage: None,
        },
    );
    node_runs.insert(
        "EmbeddingNode".to_string(),
        NodeRun {
            status: NodeRunStatus::Success,
            started_at: Some(Utc.with_ymd_and_hms(2026, 6, 20, 9, 0, 4).unwrap()),
            completed_at: Some(Utc.with_ymd_and_hms(2026, 6, 20, 9, 0, 8).unwrap()),
            error: None,
            input: Some(serde_json::json!({ "model": "text-embedding-3-small" })),
            usage: Some(Usage {
                input_tokens: Some(512),
                output_tokens: Some(0),
                model: "text-embedding-3-small".to_string(),
            }),
        },
    );

    let mut nodes = HashMap::new();
    nodes.insert(
        "DataIngestionNode".to_string(),
        serde_json::json!({ "output": { "documents_loaded": 3 } }),
    );

    let row = EventsRow {
        id: Uuid::parse_str("5b1f9a4c-6e2d-4b3a-8b8b-1e2f3a4b5c6d").unwrap(),
        workflow_type: "sdlc-flow".to_string(),
        data: serde_json::json!({ "ticket_id": "T-142" }),
        task_context: TaskContext {
            event: serde_json::json!({ "ticket_id": "T-142" }),
            nodes,
            metadata: serde_json::json!({ "workflow": "sdlc-flow" }),
            node_runs,
        },
        created_at: Utc.with_ymd_and_hms(2026, 6, 20, 9, 0, 0).unwrap(),
        updated_at: Utc.with_ymd_and_hms(2026, 6, 20, 9, 0, 8).unwrap(),
    };

    let v = serde_json::to_value(&row).expect("EventsRow serializes");

    // Top-level events-row shape (contract §4).
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

    // task_context shape (contract §5).
    let tc = &v["task_context"];
    for key in ["event", "nodes", "metadata", "node_runs"] {
        assert!(tc.get(key).is_some(), "task_context missing field: {key}");
    }

    // node_runs entry shape + lowercase status (contract §6).
    let embedding_run = &tc["node_runs"]["EmbeddingNode"];
    assert_eq!(embedding_run["status"], "success");
    assert_eq!(
        embedding_run["usage"],
        serde_json::json!({
            "input_tokens": 512,
            "output_tokens": 0,
            "model": "text-embedding-3-small",
        })
    );

    let ingestion_run = &tc["node_runs"]["DataIngestionNode"];
    assert!(
        ingestion_run["usage"].is_null(),
        "usage must serialize as null when absent"
    );
}
