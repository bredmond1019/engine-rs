//! `LEAD_INGEST` (`EN.6.I`) — the minimal inbound-lead workflow: a bare
//! `MaterializeDocNode -> MergeContactsNode` two-node graph, zero new node
//! types, zero model calls, turning an inbound lead payload into a durable
//! opportunity document so a lead can no longer be lost to "the Resend
//! email notification is the only record" (the documented failure mode
//! that killed both 2026-06 leads — see `planning/en-6i-lead-ingest/tasks.md`).
//!
//! Declared graph shape (mirrors `opportunity_edit::graph`'s no-router,
//! no-policy shape, extended to two nodes):
//!
//! ```text
//! MaterializeDocNode (model = "opportunity")  ->  MergeContactsNode
//! ```
//!
//! Both nodes carry **empty `source_nodes`**, so each reads `ctx.event`
//! directly instead of a prior node's output — the same bare-ingest pattern
//! `crate::nodes::merge_contacts` documents in its own module doc, and the
//! one `content_pipeline`'s ingest shape already uses for `SourceRouterNode`.
//! `MaterializeDocNode` writes (or updates) the opportunity document from
//! `ctx.event`'s `company_name`/`contacts[]`; `MergeContactsNode` then reads
//! `ctx.event` again (not `MaterializeDocNode`'s stamped result) and merges
//! the same `contacts[]` into whatever document now exists on disk —
//! `plan_merge_contacts`'s match-on-`name`/union-fields conflict policy is
//! what actually reconciles the two nodes' overlapping view of `contacts[]`
//! into one merged set.
//!
//! **Failure mode:** a payload missing `company_name` cannot be shape-
//! detected by mev's `detect_kind` (`crates/mev/src/doc/opportunity.rs`),
//! so `plan_ingest` raises an error-severity `E_DOC_UNKNOWN_INPUT_SHAPE`
//! diagnostic, which `MaterializeDocNode::process` surfaces as a hard
//! `NodeError` — no partial document is ever written. Per this ticket's
//! Notes (`planning/en-6i-lead-ingest/tasks.md` § D3), this block does
//! **not** add slug-fallback logic for a missing `company_name`.
//!
//! **Idempotency:** `plan_ingest` already treats a slug whose opportunity
//! document exists as zero new-document actions (`doc_materializer.rs`'s
//! `MevDocMaterializer` tests exercise this for `plan_set_stage`/
//! `plan_add_action`; `plan_ingest` shares the same "no diff, no write"
//! contract) — so posting the same lead payload twice writes the document
//! once and only merges contacts on the second post, never duplicating it.
//!
//! **No policy module, no profiles module, no `harness.json` section** —
//! same rationale as `opportunity_edit::graph`'s module doc: neither node
//! calls a model, so there is no `ModelTier` to resolve and nothing for a
//! policy layer to override.

use std::collections::HashMap;

use crate::node::NodeRegistry;
use crate::nodes::materialize_doc::{self, MaterializeDocNode};
use crate::nodes::merge_contacts::{self, MergeContactsNode};
use crate::schema::{NodeConfig, WorkflowSchema};
use crate::workflow::Workflow;

/// The `model` string `MaterializeDocNode` is configured with — mev's
/// `Opportunity`-shaped model, matching `RESEARCH_AGENT`'s opportunity
/// instance and this ticket's Context Pointers.
const OPPORTUNITY_MODEL: &str = "opportunity";

/// `LEAD_INGEST`'s registered workflow type string.
pub const WORKFLOW_TYPE: &str = "LEAD_INGEST";

/// Build the declared `WorkflowSchema` for `LEAD_INGEST`: `MaterializeDocNode`
/// (start) forwards to `MergeContactsNode` (terminal) — the two nodes'
/// default `Node::name()` identities, unchanged by any `NodeExt::with_identity`
/// override since a single instance of each lives in this graph.
#[must_use]
pub fn schema() -> WorkflowSchema {
    let mut nodes = HashMap::new();
    nodes.insert(
        materialize_doc::NODE_NAME.to_string(),
        NodeConfig::new(
            materialize_doc::NODE_NAME,
            vec![merge_contacts::NODE_NAME.to_string()],
        ),
    );
    nodes.insert(
        merge_contacts::NODE_NAME.to_string(),
        NodeConfig::new(merge_contacts::NODE_NAME, vec![]),
    );
    WorkflowSchema::new(WORKFLOW_TYPE, materialize_doc::NODE_NAME, nodes)
}

/// Build a fresh `NodeRegistry` for `LEAD_INGEST`: a live-materializer
/// `MaterializeDocNode` configured for `model = "opportunity"` chained into
/// a live-materializer `MergeContactsNode` — both with empty `source_nodes`
/// (the default), so both read `ctx.event` directly. Tests build their own
/// registry with stubbed/tempdir-pinned nodes instead of calling this
/// directly (mirrors `opportunity_edit::graph::set_stage_registry`'s
/// production-defaults convention).
#[must_use]
pub fn registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(
        MaterializeDocNode::new(OPPORTUNITY_MODEL).with_source_nodes(Vec::<String>::new()),
    ));
    registry.register(Box::new(
        MergeContactsNode::new().with_source_nodes(Vec::<String>::new()),
    ));
    registry
}

/// Build the runnable `LEAD_INGEST` `Workflow`: [`registry`] paired with
/// [`schema`], constructed via `Workflow::new_validated` so assembly fails
/// loudly if the declared graph is not structurally sound.
///
/// # Panics
/// Panics if the declared graph fails `WorkflowValidator::validate` — this
/// would be a programming error in this module, not a runtime condition
/// callers should recover from.
#[must_use]
pub fn workflow() -> Workflow {
    Workflow::new_validated(registry(), schema())
        .expect("LEAD_INGEST declared graph must pass WorkflowValidator::validate")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::validate::WorkflowValidator;

    /// Build a `LEAD_INGEST` registry pinned at `root` (a `tempfile::tempdir()`),
    /// with real `mev`-backed nodes — no stub — so these tests exercise the
    /// live idempotency/failure contract this ticket's acceptance criteria
    /// require ("a posted lead payload writes a valid opportunity doc",
    /// "a duplicate post does not create a second document", "a malformed
    /// payload fails loudly").
    fn live_registry_at(root: &std::path::Path) -> NodeRegistry {
        let mut registry = NodeRegistry::new();
        registry.register(Box::new(
            MaterializeDocNode::new(OPPORTUNITY_MODEL)
                .with_source_nodes(Vec::<String>::new())
                .with_brain_root(root),
        ));
        registry.register(Box::new(
            MergeContactsNode::new()
                .with_source_nodes(Vec::<String>::new())
                .with_brain_root(root),
        ));
        registry
    }

    fn lead_payload() -> serde_json::Value {
        json!({
            "company_name": "Acme Corp",
            "summary": "Widget manufacturer expanding into SaaS.",
            "contacts": [{"name": "Jane Doe", "emails": ["jane@acme.com"]}],
        })
    }

    #[test]
    fn schema_passes_validation() {
        let schema = schema();
        let registry = registry();
        WorkflowValidator::validate(&registry, &schema).expect("declared graph should validate");
    }

    #[test]
    fn workflow_type_matches_schema() {
        assert_eq!(schema().workflow_type, WORKFLOW_TYPE);
    }

    #[test]
    fn start_node_is_materialize_doc_node_forwarding_to_merge_contacts_node() {
        let schema = schema();
        assert_eq!(schema.start_node, materialize_doc::NODE_NAME);
        let start_config = schema
            .nodes
            .get(materialize_doc::NODE_NAME)
            .expect("start node should be declared");
        assert_eq!(
            start_config.connections,
            vec![merge_contacts::NODE_NAME.to_string()]
        );
        let terminal_config = schema
            .nodes
            .get(merge_contacts::NODE_NAME)
            .expect("terminal node should be declared");
        assert!(terminal_config.connections.is_empty());
    }

    #[test]
    fn registry_contains_exactly_the_two_expected_identities() {
        let registry = registry();
        assert!(registry.contains(materialize_doc::NODE_NAME));
        assert!(registry.contains(merge_contacts::NODE_NAME));
        assert_eq!(registry.len(), 2);
    }

    #[test]
    fn workflow_builds_without_panicking() {
        let _workflow = workflow();
    }

    #[tokio::test]
    async fn well_formed_payload_writes_an_opportunity_doc_with_contacts_populated() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("business/docs/opportunities"))
            .expect("create opportunities dir");

        let workflow_registry = live_registry_at(dir.path());
        let workflow = Workflow::new_validated(workflow_registry, schema())
            .expect("declared graph should validate");

        let ctx = workflow
            .run(lead_payload(), Box::new(|_ctx| {}))
            .await
            .expect("well-formed lead payload should run to completion");

        assert_eq!(
            ctx.node_runs[materialize_doc::NODE_NAME].status,
            engine_contract::NodeRunStatus::Success
        );
        assert_eq!(
            ctx.node_runs[merge_contacts::NODE_NAME].status,
            engine_contract::NodeRunStatus::Success
        );

        let path = dir.path().join("business/docs/opportunities/acme-corp.md");
        assert!(path.exists(), "expected opportunity doc to be written");
        let contents = std::fs::read_to_string(&path).expect("read opportunity doc");
        assert!(contents.contains("Jane Doe"));
        assert!(contents.contains("jane@acme.com"));

        let materialize_result = &ctx.nodes[materialize_doc::NODE_NAME];
        assert_eq!(materialize_result["materialized"], json!(true));
        let merge_result = &ctx.nodes[merge_contacts::NODE_NAME];
        assert_eq!(merge_result["merged"], json!(true));
        assert_eq!(merge_result["contacts"], json!(1));
    }

    #[tokio::test]
    async fn payload_missing_company_name_fails_loudly_and_writes_no_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("business/docs/opportunities"))
            .expect("create opportunities dir");

        let workflow_registry = live_registry_at(dir.path());
        let workflow = Workflow::new_validated(workflow_registry, schema())
            .expect("declared graph should validate");

        let malformed_payload = json!({
            "summary": "No company_name on this payload.",
            "contacts": [{"name": "Jane Doe", "emails": ["jane@acme.com"]}],
        });

        let ctx = workflow
            .run(malformed_payload, Box::new(|_ctx| {}))
            .await
            .expect("run itself should not error — the failure is a stamped NodeRun");

        let materialize_run = &ctx.node_runs[materialize_doc::NODE_NAME];
        assert_eq!(
            materialize_run.status,
            engine_contract::NodeRunStatus::Failed
        );
        let error_message = materialize_run
            .error
            .as_ref()
            .expect("failed node run should carry an error message");
        assert!(
            error_message.contains("company_name") && error_message.contains("cannot infer"),
            "expected a shape-detection failure naming 'company_name', got: {error_message}"
        );
        assert!(
            !ctx.node_runs.contains_key(merge_contacts::NODE_NAME)
                || ctx.node_runs[merge_contacts::NODE_NAME].status
                    == engine_contract::NodeRunStatus::Pending,
            "MergeContactsNode must never run after MaterializeDocNode fails loudly"
        );

        let entries = std::fs::read_dir(dir.path().join("business/docs/opportunities"))
            .expect("read opportunities dir")
            .count();
        assert_eq!(entries, 0, "malformed payload must write no file");
    }

    #[tokio::test]
    async fn duplicate_post_does_not_create_a_second_document() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("business/docs/opportunities"))
            .expect("create opportunities dir");

        let payload = lead_payload();
        let path = dir.path().join("business/docs/opportunities/acme-corp.md");

        // First post: writes the document.
        {
            let workflow = Workflow::new_validated(live_registry_at(dir.path()), schema())
                .expect("declared graph should validate");
            let ctx = workflow
                .run(payload.clone(), Box::new(|_ctx| {}))
                .await
                .expect("first post should run cleanly");
            assert_eq!(
                ctx.node_runs[materialize_doc::NODE_NAME].status,
                engine_contract::NodeRunStatus::Success
            );
        }
        assert!(path.exists());
        let entries_after_first = std::fs::read_dir(dir.path().join("business/docs/opportunities"))
            .expect("read opportunities dir")
            .filter(|e| !e.as_ref().unwrap().file_name().to_string_lossy().starts_with('.'))
            .count();
        assert_eq!(entries_after_first, 1);

        // Second post of the identical payload: no new document, only a
        // (no-op) contacts merge.
        {
            let workflow = Workflow::new_validated(live_registry_at(dir.path()), schema())
                .expect("declared graph should validate");
            let ctx = workflow
                .run(payload, Box::new(|_ctx| {}))
                .await
                .expect("duplicate post should run cleanly, not error");
            assert_eq!(
                ctx.node_runs[materialize_doc::NODE_NAME].status,
                engine_contract::NodeRunStatus::Success
            );
        }

        let entries_after_second =
            std::fs::read_dir(dir.path().join("business/docs/opportunities"))
                .expect("read opportunities dir")
                .filter(|e| !e.as_ref().unwrap().file_name().to_string_lossy().starts_with('.'))
                .count();
        assert_eq!(
            entries_after_second, 1,
            "duplicate post must not create a second document"
        );
    }
}
