//! `MergeContactsNode` (`EN.4.E` task 7) — the terminal node that collects
//! the contacts a `RESEARCH_AGENT` run surfaced and merges them into the
//! opportunity `MaterializeDocNode` just wrote, via
//! [`crate::nodes::doc_materializer::OpportunityEdit::MergeContacts`].
//!
//! Lives under `nodes/`, not under a `workflows::*` module, for the same
//! reason `MaterializeDocNode` and `OpportunityEditNode` do — it is generic
//! machinery, not `RESEARCH_AGENT`-specific wiring; the graph
//! (`crate::workflows::research_agent::graph`) wires one instance in after
//! `MaterializeDocNode` on both research branches.
//!
//! Modeled directly on `crate::nodes::materialize_doc::MaterializeDocNode`:
//! same `NODE_NAME` const, `with_materializer` / `with_brain_root` /
//! `with_source_nodes` / `with_write` builder shape, `put_result` for the
//! `ctx.nodes` stamp under `self.name()` (so `NodeExt::with_identity`
//! composes), and node-name-prefixed `NodeError` messages.
//!
//! **Shape detection mirrors mev's `detect_kind` exactly** (`company_name`
//! present -> company brief; otherwise `prospects`/`vertical` -> prospecting
//! result) so this node and okf-core's mapping never disagree about which
//! shape an artifact is.
//!
//! **"No contact found" is a success path**, not an error and not an empty
//! seam call — an empty collected contact list short-circuits to a stamped
//! no-op result before `edit_opportunity` is ever invoked. This is also the
//! normal path when the run's `contact_enrichment` policy resolved to
//! `off`, so a cost-capped run flows through cleanly (`EN.4.E` task 9 tests
//! that end to end).

use std::path::PathBuf;
use std::sync::Arc;

use engine_contract::TaskContext;
use serde_json::{json, Value};

use crate::brain_root::resolve_brain_root;
use crate::node::{Node, NodeError};
use crate::nodes::doc_materializer::{doc_materializer_live, DocMaterializer, OpportunityEdit};
use crate::workflows::{get_result, put_result};

/// The `Node::name()` identity `MergeContactsNode` runs under by default,
/// and the `ctx.nodes` key its result is stamped onto when no identity
/// override (`NodeExt::with_identity`) is applied.
pub const NODE_NAME: &str = "MergeContactsNode";

/// The terminal contact-merge node: reads the upstream research artifact
/// (company brief or prospecting result), collects its `contacts[]`, and
/// calls the injectable [`DocMaterializer::edit_opportunity`] seam with
/// [`OpportunityEdit::MergeContacts`] — unless the collected list is empty,
/// in which case it stamps a no-op result and makes no seam call.
pub struct MergeContactsNode {
    materializer: Arc<dyn DocMaterializer>,
    brain_root: Option<PathBuf>,
    source_nodes: Vec<String>,
    write: bool,
}

impl MergeContactsNode {
    /// Construct with the live `mev`-backed materializer, `write = true`,
    /// no explicit brain root, and no explicit source node (reads
    /// `ctx.event`).
    #[must_use]
    pub fn new() -> Self {
        Self {
            materializer: doc_materializer_live(),
            brain_root: None,
            source_nodes: Vec::new(),
            write: true,
        }
    }

    /// Override the [`DocMaterializer`] seam. Tests inject a
    /// `StubDocMaterializer` so the gated suite never touches disk.
    #[must_use]
    pub fn with_materializer(mut self, materializer: Arc<dyn DocMaterializer>) -> Self {
        self.materializer = materializer;
        self
    }

    /// Pin the brain root explicitly, bypassing `resolve_brain_root`. Tests
    /// use this to point at a `tempfile::tempdir()` corpus.
    #[must_use]
    pub fn with_brain_root(mut self, brain_root: impl Into<PathBuf>) -> Self {
        self.brain_root = Some(brain_root.into());
        self
    }

    /// Read the input artifact from the first of `upstreams` (in order)
    /// whose `ctx.nodes` entry is present — the same ordered
    /// first-present-wins preference `MaterializeDocNode::with_source_nodes`
    /// uses, so a single `MergeContactsNode` instance sits downstream of
    /// both `RESEARCH_AGENT` branches (`CompanyResearchNode` |
    /// `ProspectingResearchNode`). An empty list (the default) keeps
    /// reading `ctx.event`.
    #[must_use]
    pub fn with_source_nodes(
        mut self,
        upstreams: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.source_nodes = upstreams.into_iter().map(Into::into).collect();
        self
    }

    /// Whether to actually write to disk (`true`, the default) or dry-run
    /// (`false` — nothing touches disk, but the stamped result still
    /// reports `dry_run: true`).
    #[must_use]
    pub fn with_write(mut self, write: bool) -> Self {
        self.write = write;
        self
    }

    /// Resolve the input artifact: the first configured `source_nodes`
    /// identity present in `ctx.nodes` wins (first-listed preference), or
    /// `ctx.event` when no source nodes are configured. When a preference
    /// list is configured but none of its identities is present, names
    /// every identity tried in the error — mirrors
    /// `MaterializeDocNode::read_input`.
    fn read_input<'a>(&self, ctx: &'a TaskContext) -> Result<&'a Value, NodeError> {
        if self.source_nodes.is_empty() {
            return Ok(&ctx.event);
        }

        for upstream in &self.source_nodes {
            if let Some(stored) = get_result(ctx, upstream) {
                return Ok(stored);
            }
        }

        Err(NodeError::new(format!(
            "{}: no artifact stored by {}",
            self.name(),
            self.source_nodes.join(", ")
        )))
    }

    /// Resolve the brain root: the explicit `with_brain_root` value when
    /// set, otherwise `crate::brain_root::resolve_brain_root()`.
    fn resolve_root(&self) -> Result<PathBuf, NodeError> {
        match &self.brain_root {
            Some(root) => Ok(root.clone()),
            None => resolve_brain_root().map_err(|err| {
                NodeError::new(format!(
                    "{}: failed to resolve brain root (set ENGINE_BRAIN_ROOT to override): {err}",
                    self.name()
                ))
            }),
        }
    }
}

impl Default for MergeContactsNode {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether `input` is shaped like a company brief per mev's `detect_kind`
/// rule (`company_name` present, non-empty).
fn is_company_brief(input: &Value) -> bool {
    input
        .get("company_name")
        .and_then(Value::as_str)
        .map(|s| !s.is_empty())
        .unwrap_or(false)
}

/// The title to derive the opportunity slug from, mirroring the title each
/// `okf_core::Opportunity::from_*` constructor uses: `company_name` for a
/// company brief, `"{vertical} — Prospecting Sweep"` for a prospecting
/// result.
fn title_for(input: &Value) -> Option<String> {
    if is_company_brief(input) {
        return input
            .get("company_name")
            .and_then(Value::as_str)
            .map(str::to_string);
    }

    input
        .get("vertical")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(|vertical| format!("{vertical} — Prospecting Sweep"))
}

/// Collect the contacts to merge out of `input`: for a company brief, the
/// top-level `contacts[]`; for a prospecting result, every lead's
/// `contacts[]` flattened in lead order.
fn collect_contacts(input: &Value) -> Vec<Value> {
    if is_company_brief(input) {
        return input
            .get("contacts")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
    }

    input
        .get("prospects")
        .and_then(Value::as_array)
        .map(|prospects| {
            prospects
                .iter()
                .flat_map(|lead| {
                    lead.get("contacts")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default()
                })
                .collect()
        })
        .unwrap_or_default()
}

#[async_trait::async_trait]
impl Node for MergeContactsNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let input = self.read_input(&ctx)?.clone();

        let title = title_for(&input).ok_or_else(|| {
            NodeError::new(format!(
                "{}: could not derive a slug — missing 'company_name' (company brief) or \
                 'vertical' (prospecting result) on the upstream artifact",
                self.name()
            ))
        })?;
        let slug = okf_core::derive_slug(&title);
        if slug.is_empty() {
            return Err(NodeError::new(format!(
                "{}: derived slug from title '{title}' is empty",
                self.name()
            )));
        }

        let contacts = collect_contacts(&input);

        let mut ctx = ctx;

        if contacts.is_empty() {
            put_result(
                &mut ctx,
                self.name(),
                json!({
                    "merged": false,
                    "contacts": 0,
                    "slug": slug,
                    "dry_run": !self.write,
                }),
            );
            return Ok(ctx);
        }

        let root = self.resolve_root()?;
        let contacts_count = contacts.len();
        let edit = OpportunityEdit::MergeContacts {
            slug: slug.clone(),
            contacts: Value::Array(contacts),
        };

        let outcome = self
            .materializer
            .edit_opportunity(&root, &edit, self.write)
            .await
            .map_err(|err| NodeError::new(format!("{}: {err}", self.name())))?;

        if let Some(error) = outcome.errors().first() {
            return Err(NodeError::new(format!(
                "{}: merge-contacts failed for '{}': {}",
                self.name(),
                error.file.display(),
                error.message
            )));
        }

        let paths: Vec<Value> = outcome
            .planned
            .iter()
            .map(|file| json!(file.path.display().to_string()))
            .collect();
        let warnings: Vec<Value> = outcome
            .warnings()
            .iter()
            .map(|diag| json!(diag.message.clone()))
            .collect();

        put_result(
            &mut ctx,
            self.name(),
            json!({
                "merged": outcome.wrote,
                "contacts": contacts_count,
                "slug": slug,
                "dry_run": !self.write,
                "paths": paths,
                "warnings": warnings,
            }),
        );

        Ok(ctx)
    }

    fn name(&self) -> &str {
        NODE_NAME
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use serde_json::json;

    use crate::node::NodeExt;
    use crate::nodes::doc_materializer::{
        MaterializeDiagnostic, MaterializeOutcome, MaterializedFile, StubDocMaterializer,
    };

    use super::*;

    fn empty_ctx(event: Value) -> TaskContext {
        TaskContext {
            event,
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        }
    }

    fn success_outcome() -> MaterializeOutcome {
        MaterializeOutcome {
            wrote: true,
            planned: vec![MaterializedFile {
                path: PathBuf::from("/tmp/brain/opportunities/acme-corp.md"),
                note: "updated".to_string(),
            }],
            diagnostics: vec![],
        }
    }

    #[tokio::test]
    async fn company_shape_merges_top_level_contacts_under_the_company_name_slug() {
        let stub = StubDocMaterializer::succeeding(success_outcome());
        let node = MergeContactsNode::new()
            .with_materializer(Arc::new(stub.clone()))
            .with_brain_root("/tmp/brain");

        let ctx = empty_ctx(json!({
            "company_name": "Acme Corp",
            "summary": "Widgets.",
            "contacts": [{"name": "Jane Doe", "emails": ["jane@acme.com"]}],
        }));

        let ctx = node.process(ctx).await.expect("process should succeed");

        let call = stub.last_edit().expect("edit_opportunity should be called");
        assert_eq!(call.root, PathBuf::from("/tmp/brain"));
        match call.edit {
            OpportunityEdit::MergeContacts { slug, contacts } => {
                assert_eq!(slug, "acme-corp");
                assert_eq!(
                    contacts,
                    json!([{"name": "Jane Doe", "emails": ["jane@acme.com"]}])
                );
            }
            other => panic!("expected MergeContacts, got {other:?}"),
        }
        assert!(call.write);

        let result = &ctx.nodes[NODE_NAME];
        assert_eq!(result["merged"], json!(true));
        assert_eq!(result["contacts"], json!(1));
        assert_eq!(result["slug"], json!("acme-corp"));
        assert_eq!(result["dry_run"], json!(false));
    }

    #[tokio::test]
    async fn prospecting_shape_flattens_per_lead_contacts_in_order_under_the_vertical_slug() {
        let stub = StubDocMaterializer::succeeding(success_outcome());
        let node = MergeContactsNode::new()
            .with_materializer(Arc::new(stub.clone()))
            .with_brain_root("/tmp/brain");

        let ctx = empty_ctx(json!({
            "vertical": "fintech",
            "prospects": [
                {
                    "name": "Lead One",
                    "pillar": "automation",
                    "contacts": [{"name": "", "emails": ["contato@one.example"]}],
                },
                {
                    "name": "Lead Two",
                    "pillar": "automation",
                    "contacts": [{"name": "Bob", "phones": ["+55 11 5555-0000"]}],
                },
            ],
        }));

        node.process(ctx).await.expect("process should succeed");

        let call = stub.last_edit().expect("edit_opportunity should be called");
        match call.edit {
            OpportunityEdit::MergeContacts { slug, contacts } => {
                assert_eq!(slug, "fintech-prospecting-sweep");
                assert_eq!(
                    contacts,
                    json!([
                        {"name": "", "emails": ["contato@one.example"]},
                        {"name": "Bob", "phones": ["+55 11 5555-0000"]},
                    ])
                );
            }
            other => panic!("expected MergeContacts, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn empty_contact_list_stamps_a_no_op_result_and_makes_zero_seam_calls() {
        let stub = StubDocMaterializer::succeeding(success_outcome());
        let node = MergeContactsNode::new()
            .with_materializer(Arc::new(stub.clone()))
            .with_brain_root("/tmp/brain");

        let ctx = empty_ctx(json!({
            "company_name": "Acme Corp",
            "summary": "Widgets.",
            "contacts": [],
        }));

        let ctx = node.process(ctx).await.expect("process should succeed");

        assert!(
            stub.last_edit().is_none(),
            "no contacts should mean zero seam calls"
        );

        let result = &ctx.nodes[NODE_NAME];
        assert_eq!(result["merged"], json!(false));
        assert_eq!(result["contacts"], json!(0));
        assert_eq!(result["slug"], json!("acme-corp"));
        assert_eq!(result["dry_run"], json!(false));
    }

    #[tokio::test]
    async fn missing_contacts_field_is_also_a_clean_no_op() {
        let stub = StubDocMaterializer::succeeding(success_outcome());
        let node = MergeContactsNode::new()
            .with_materializer(Arc::new(stub.clone()))
            .with_brain_root("/tmp/brain");

        let ctx = empty_ctx(json!({
            "company_name": "Acme Corp",
            "summary": "Widgets.",
        }));

        node.process(ctx).await.expect("process should succeed");

        assert!(stub.last_edit().is_none());
    }

    #[tokio::test]
    async fn process_reads_input_from_first_listed_identity_when_both_present() {
        let stub = StubDocMaterializer::succeeding(success_outcome());
        let node = MergeContactsNode::new()
            .with_materializer(Arc::new(stub.clone()))
            .with_brain_root("/tmp/brain")
            .with_source_nodes(["CompanyResearchNode", "ProspectingResearchNode"]);

        let mut ctx = empty_ctx(json!({}));
        put_result(
            &mut ctx,
            "CompanyResearchNode",
            json!({
                "company_name": "Acme Corp",
                "contacts": [{"name": "Jane"}],
            }),
        );
        put_result(
            &mut ctx,
            "ProspectingResearchNode",
            json!({"vertical": "fintech", "prospects": []}),
        );

        node.process(ctx).await.expect("process should succeed");

        let call = stub.last_edit().expect("edit_opportunity should be called");
        match call.edit {
            OpportunityEdit::MergeContacts { slug, .. } => assert_eq!(slug, "acme-corp"),
            other => panic!("expected MergeContacts, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn process_errors_naming_every_identity_when_none_of_a_preference_list_is_present() {
        let stub = StubDocMaterializer::succeeding(success_outcome());
        let node = MergeContactsNode::new()
            .with_materializer(Arc::new(stub))
            .with_brain_root("/tmp/brain")
            .with_source_nodes(["CompanyResearchNode", "ProspectingResearchNode"]);

        let ctx = empty_ctx(json!({}));
        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("MergeContactsNode"));
        assert!(err.message.contains("CompanyResearchNode"));
        assert!(err.message.contains("ProspectingResearchNode"));
    }

    #[tokio::test]
    async fn unresolvable_slug_is_a_node_error() {
        let stub = StubDocMaterializer::succeeding(success_outcome());
        let node = MergeContactsNode::new()
            .with_materializer(Arc::new(stub))
            .with_brain_root("/tmp/brain");

        // Neither `company_name` nor `vertical` present.
        let ctx = empty_ctx(json!({"summary": "no title here"}));
        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("MergeContactsNode"));
    }

    #[tokio::test]
    async fn empty_derived_slug_is_a_node_error() {
        let stub = StubDocMaterializer::succeeding(success_outcome());
        let node = MergeContactsNode::new()
            .with_materializer(Arc::new(stub))
            .with_brain_root("/tmp/brain");

        // `company_name` present but punctuation-only -> derives to "".
        let ctx = empty_ctx(json!({"company_name": "!!!", "contacts": [{"name": "Jane"}]}));
        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("MergeContactsNode"));
        assert!(err.message.contains("empty"));
    }

    #[tokio::test]
    async fn with_write_false_stamps_dry_run_true() {
        let stub = StubDocMaterializer::succeeding(MaterializeOutcome {
            wrote: false,
            ..success_outcome()
        });
        let node = MergeContactsNode::new()
            .with_materializer(Arc::new(stub.clone()))
            .with_brain_root("/tmp/brain")
            .with_write(false);

        let ctx = empty_ctx(json!({
            "company_name": "Acme Corp",
            "contacts": [{"name": "Jane"}],
        }));

        let ctx = node.process(ctx).await.expect("process should succeed");

        let call = stub.last_edit().expect("edit_opportunity should be called");
        assert!(!call.write);

        let result = &ctx.nodes[NODE_NAME];
        assert_eq!(result["dry_run"], json!(true));
        assert_eq!(result["merged"], json!(false));
    }

    #[tokio::test]
    async fn identity_override_stamps_result_under_the_overridden_identity() {
        let stub = StubDocMaterializer::succeeding(success_outcome());
        let node = MergeContactsNode::new()
            .with_materializer(Arc::new(stub))
            .with_brain_root("/tmp/brain")
            .with_identity("MergeContactsOverride");

        let ctx = empty_ctx(json!({
            "company_name": "Acme Corp",
            "contacts": [{"name": "Jane"}],
        }));

        let ctx = node.process(ctx).await.expect("process should succeed");

        assert!(ctx.nodes.contains_key("MergeContactsOverride"));
        assert!(!ctx.nodes.contains_key(NODE_NAME));
    }

    #[tokio::test]
    async fn seam_failure_surfaces_as_a_node_error() {
        let stub = StubDocMaterializer::failing("boom");
        let node = MergeContactsNode::new()
            .with_materializer(Arc::new(stub))
            .with_brain_root("/tmp/brain");

        let ctx = empty_ctx(json!({
            "company_name": "Acme Corp",
            "contacts": [{"name": "Jane"}],
        }));

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("MergeContactsNode"));
        assert!(err.message.contains("boom"));
    }

    #[tokio::test]
    async fn error_severity_diagnostic_surfaces_as_a_node_error_even_on_ok() {
        let outcome = MaterializeOutcome {
            wrote: false,
            planned: vec![],
            diagnostics: vec![MaterializeDiagnostic {
                severity: "error".to_string(),
                file: PathBuf::from("/tmp/brain/opportunities/no-such-opportunity.md"),
                code: "E_DOC_UNKNOWN_SLUG".to_string(),
                message: "unknown opportunity slug".to_string(),
            }],
        };
        let stub = StubDocMaterializer::succeeding(outcome);
        let node = MergeContactsNode::new()
            .with_materializer(Arc::new(stub))
            .with_brain_root("/tmp/brain");

        let ctx = empty_ctx(json!({
            "company_name": "Acme Corp",
            "contacts": [{"name": "Jane"}],
        }));

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("MergeContactsNode"));
        assert!(err.message.contains("unknown opportunity slug"));
    }
}
