//! `MaterializeDocNode` (`EN.7.A` task 4) — the generic, reusable node every
//! pipeline appends to write a `BrainDocModel`-shaped artifact out of an
//! upstream node's `TaskContext` into the Brain corpus as a source `.md`
//! document, via the injectable [`crate::nodes::doc_materializer`] seam.
//!
//! Lives under `nodes/`, not under a `workflows::*` module, because it is
//! generic across pipelines (`EN.7.B` wires instances of it into specific
//! graphs — that wiring, and the `set-stage`/`add-action` micro-workflows,
//! are explicitly out of scope here). Modeled on
//! `crate::workflows::content_pipeline::persist_to_brain::PersistToBrainNode`'s
//! builder-seam shape: a `NODE_NAME` const, `put_result`/`get_result` for
//! `ctx.nodes` access, and node-name-prefixed `NodeError` messages.
//!
//! Brain-root resolution: an explicit [`MaterializeDocNode::with_brain_root`]
//! value wins; otherwise `process` calls
//! [`crate::brain_root::resolve_brain_root`] (which itself honours
//! `ENGINE_BRAIN_ROOT` before walking up from cwd for `brain.toml`).
//!
//! Result stamp: `process` writes `{"materialized", "skipped", "dry_run",
//! "model", "paths", "warnings"}` onto `ctx.nodes[self.name()]` — deliberately
//! `self.name()` rather than the bare `NODE_NAME` const, so a node wrapped in
//! `EN.5.E`'s `NodeExt::with_identity` lands its result under its own
//! override identity and two instances can co-exist in one graph without
//! colliding (exactly what `EN.7.B` needs).
//!
//! Enable/disable (`EN.7.D` task 1): [`MaterializeDocNode::with_enabled`]
//! defaults to `true` (every existing caller unaffected). When disabled,
//! `process` short-circuits before resolving the brain root or reading the
//! input artifact — nothing touches disk, the seam is never called, and an
//! unresolvable brain root raises no error — and stamps
//! `{"materialized": false, "skipped": true, ...}`, keeping the same key set
//! as an enabled run (`skipped: false` there) so consumers read one stable
//! shape.

use std::path::PathBuf;
use std::sync::Arc;

use engine_contract::TaskContext;
use serde_json::{json, Value};

use crate::brain_root::resolve_brain_root;
use crate::node::{Node, NodeError};
use crate::nodes::doc_materializer::{doc_materializer_live, DocMaterializer};
use crate::workflows::{get_result, put_result};

/// The `Node::name()` identity `MaterializeDocNode` runs under by default,
/// and the `ctx.nodes` key its result is stamped onto when no identity
/// override (`NodeExt::with_identity`) is applied.
pub const NODE_NAME: &str = "MaterializeDocNode";

/// The generic writer node: reads a `BrainDocModel`-shaped artifact out of
/// an upstream node's `TaskContext` (or the run's own `ctx.event` when no
/// upstream is configured) and calls the injectable [`DocMaterializer`] seam
/// to write it into the Brain corpus.
pub struct MaterializeDocNode {
    model: String,
    materializer: Arc<dyn DocMaterializer>,
    brain_root: Option<PathBuf>,
    source_nodes: Vec<String>,
    write: bool,
    enabled: bool,
}

impl MaterializeDocNode {
    /// Construct for `model` (one of `"opportunity"`, `"learning-artifact"`,
    /// `"proposal"` — validated by the seam, not here) with the live
    /// `mev`-backed materializer, `write = true`, no explicit brain root,
    /// and no explicit source node (reads `ctx.event`).
    #[must_use]
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            materializer: doc_materializer_live(),
            brain_root: None,
            source_nodes: Vec::new(),
            write: true,
            enabled: true,
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

    /// Read the input artifact from `upstream`'s stored `ctx.nodes` entry
    /// instead of `ctx.event`. Equivalent to
    /// `with_source_nodes([upstream])` — a one-element preference list.
    #[must_use]
    pub fn with_source_node(mut self, upstream: impl Into<String>) -> Self {
        self.source_nodes = vec![upstream.into()];
        self
    }

    /// Read the input artifact from the first of `upstreams` (in order)
    /// whose `ctx.nodes` entry is present. `RESEARCH_AGENT` has two producer
    /// branches (`CompanyResearchNode` | `ProspectingResearchNode`) and
    /// exactly one runs per event, so this ordered preference list is how a
    /// single `MaterializeDocNode` instance can sit downstream of both. An
    /// empty list (the default) keeps reading `ctx.event`.
    #[must_use]
    pub fn with_source_nodes(
        mut self,
        upstreams: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.source_nodes = upstreams.into_iter().map(Into::into).collect();
        self
    }

    /// Whether to actually write to disk (`true`, the default) or dry-run
    /// (`false` — nothing touches disk, but the stamped result still names
    /// the path(s) that would have been written).
    #[must_use]
    pub fn with_write(mut self, write: bool) -> Self {
        self.write = write;
        self
    }

    /// Enable/disable the node (default `true`, so every existing caller —
    /// `RESEARCH_AGENT`'s live instance included — is byte-for-byte
    /// unaffected). When `false`, `process` short-circuits before resolving
    /// the brain root or reading the input artifact: nothing touches disk,
    /// the seam is never called, and no error is raised even when the brain
    /// root would otherwise be unresolvable.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Resolve the input artifact: the first configured `source_nodes`
    /// identity present in `ctx.nodes` wins (first-listed preference), or
    /// `ctx.event` when no source nodes are configured. When a preference
    /// list is configured but none of its identities is present, names every
    /// identity tried in the error — mirrors
    /// `content_pipeline::persist_to_brain::read_source_ref`.
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

#[async_trait::async_trait]
impl Node for MaterializeDocNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        if !self.enabled {
            let mut ctx = ctx;
            put_result(
                &mut ctx,
                self.name(),
                json!({
                    "materialized": false,
                    "skipped": true,
                    "dry_run": !self.write,
                    "model": self.model,
                    "paths": Vec::<Value>::new(),
                    "warnings": Vec::<Value>::new(),
                }),
            );
            return Ok(ctx);
        }

        let root = self.resolve_root()?;
        let input = self.read_input(&ctx)?.clone();

        let outcome = self
            .materializer
            .materialize(&root, &self.model, &input, self.write)
            .await
            .map_err(|err| NodeError::new(format!("{}: {err}", self.name())))?;

        if let Some(error) = outcome.errors().first() {
            return Err(NodeError::new(format!(
                "{}: materialize failed for '{}': {}",
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

        let mut ctx = ctx;
        put_result(
            &mut ctx,
            self.name(),
            json!({
                "materialized": outcome.wrote,
                "skipped": false,
                "dry_run": !self.write,
                "model": self.model,
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
                path: PathBuf::from("/tmp/brain/opportunities/acme.md"),
                note: "created".to_string(),
            }],
            diagnostics: vec![],
        }
    }

    #[tokio::test]
    async fn process_passes_configured_model_root_and_write_flag_through_to_the_seam() {
        let stub = StubDocMaterializer::succeeding(success_outcome());
        let node = MaterializeDocNode::new("opportunity")
            .with_materializer(Arc::new(stub.clone()))
            .with_brain_root("/tmp/brain")
            .with_write(false);

        let ctx = empty_ctx(json!({"company_name": "Acme"}));
        node.process(ctx).await.expect("process should succeed");

        let call = stub
            .last_call()
            .expect("materialize should have been called");
        assert_eq!(call.root, PathBuf::from("/tmp/brain"));
        assert_eq!(call.model, "opportunity");
        assert_eq!(call.input, json!({"company_name": "Acme"}));
        assert!(!call.write);
    }

    #[tokio::test]
    async fn process_reads_input_from_configured_source_node() {
        let stub = StubDocMaterializer::succeeding(success_outcome());
        let node = MaterializeDocNode::new("opportunity")
            .with_materializer(Arc::new(stub.clone()))
            .with_brain_root("/tmp/brain")
            .with_source_node("UpstreamNode");

        let mut ctx = empty_ctx(json!({}));
        put_result(&mut ctx, "UpstreamNode", json!({"company_name": "Acme"}));

        node.process(ctx).await.expect("process should succeed");

        let call = stub
            .last_call()
            .expect("materialize should have been called");
        assert_eq!(call.input, json!({"company_name": "Acme"}));
    }

    #[tokio::test]
    async fn process_reads_input_from_first_listed_identity_when_both_present() {
        let stub = StubDocMaterializer::succeeding(success_outcome());
        let node = MaterializeDocNode::new("opportunity")
            .with_materializer(Arc::new(stub.clone()))
            .with_brain_root("/tmp/brain")
            .with_source_nodes(["CompanyResearchNode", "ProspectingResearchNode"]);

        let mut ctx = empty_ctx(json!({}));
        put_result(
            &mut ctx,
            "CompanyResearchNode",
            json!({"company_name": "Acme"}),
        );
        put_result(
            &mut ctx,
            "ProspectingResearchNode",
            json!({"vertical": "fintech"}),
        );

        node.process(ctx).await.expect("process should succeed");

        let call = stub
            .last_call()
            .expect("materialize should have been called");
        assert_eq!(call.input, json!({"company_name": "Acme"}));
    }

    #[tokio::test]
    async fn process_reads_input_from_second_listed_identity_when_only_it_is_present() {
        let stub = StubDocMaterializer::succeeding(success_outcome());
        let node = MaterializeDocNode::new("opportunity")
            .with_materializer(Arc::new(stub.clone()))
            .with_brain_root("/tmp/brain")
            .with_source_nodes(["CompanyResearchNode", "ProspectingResearchNode"]);

        let mut ctx = empty_ctx(json!({}));
        put_result(
            &mut ctx,
            "ProspectingResearchNode",
            json!({"vertical": "fintech"}),
        );

        node.process(ctx).await.expect("process should succeed");

        let call = stub
            .last_call()
            .expect("materialize should have been called");
        assert_eq!(call.input, json!({"vertical": "fintech"}));
    }

    #[tokio::test]
    async fn process_errors_naming_every_identity_when_none_of_a_preference_list_is_present() {
        let stub = StubDocMaterializer::succeeding(success_outcome());
        let node = MaterializeDocNode::new("opportunity")
            .with_materializer(Arc::new(stub))
            .with_brain_root("/tmp/brain")
            .with_source_nodes(["CompanyResearchNode", "ProspectingResearchNode"]);

        let ctx = empty_ctx(json!({}));
        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("MaterializeDocNode"));
        assert!(err.message.contains("CompanyResearchNode"));
        assert!(err.message.contains("ProspectingResearchNode"));
    }

    #[tokio::test]
    async fn process_errors_when_configured_source_node_is_missing() {
        let stub = StubDocMaterializer::succeeding(success_outcome());
        let node = MaterializeDocNode::new("opportunity")
            .with_materializer(Arc::new(stub))
            .with_brain_root("/tmp/brain")
            .with_source_node("NopeNode");

        let ctx = empty_ctx(json!({}));
        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("MaterializeDocNode"));
        assert!(err.message.contains("no artifact stored by NopeNode"));
    }

    #[tokio::test]
    async fn process_surfaces_a_seam_failure_as_a_node_error() {
        let stub = StubDocMaterializer::failing("unknown model 'not-a-model'");
        let node = MaterializeDocNode::new("not-a-model")
            .with_materializer(Arc::new(stub))
            .with_brain_root("/tmp/brain");

        let ctx = empty_ctx(json!({}));
        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("MaterializeDocNode"));
        assert!(err.message.contains("unknown model"));
    }

    #[tokio::test]
    async fn process_surfaces_an_error_severity_diagnostic_as_a_node_error_even_on_success() {
        let outcome = MaterializeOutcome {
            wrote: false,
            planned: vec![MaterializedFile {
                path: PathBuf::from("/tmp/brain/opportunities/acme.md"),
                note: "created".to_string(),
            }],
            diagnostics: vec![MaterializeDiagnostic {
                severity: "error".to_string(),
                file: PathBuf::from("/tmp/brain/opportunities/acme.md"),
                code: "E_EMIT_WRITE_FAILED".to_string(),
                message: "permission denied".to_string(),
            }],
        };
        let stub = StubDocMaterializer::succeeding(outcome);
        let node = MaterializeDocNode::new("opportunity")
            .with_materializer(Arc::new(stub))
            .with_brain_root("/tmp/brain");

        let ctx = empty_ctx(json!({}));
        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("MaterializeDocNode"));
        assert!(err.message.contains("permission denied"));
    }

    #[tokio::test]
    async fn process_stamps_the_documented_result_keys() {
        let stub = StubDocMaterializer::succeeding(success_outcome());
        let node = MaterializeDocNode::new("opportunity")
            .with_materializer(Arc::new(stub))
            .with_brain_root("/tmp/brain");

        let ctx = empty_ctx(json!({}));
        let ctx = node.process(ctx).await.expect("process should succeed");

        let result = &ctx.nodes[NODE_NAME];
        assert_eq!(result["materialized"], json!(true));
        assert_eq!(result["skipped"], json!(false));
        assert_eq!(result["dry_run"], json!(false));
        assert_eq!(result["model"], json!("opportunity"));
        assert_eq!(result["paths"], json!(["/tmp/brain/opportunities/acme.md"]));
        assert_eq!(result["warnings"], json!([]));
    }

    #[tokio::test]
    async fn disabled_node_never_calls_the_seam_and_stamps_the_skip() {
        let stub = StubDocMaterializer::succeeding(success_outcome());
        let node = MaterializeDocNode::new("opportunity")
            .with_materializer(Arc::new(stub.clone()))
            .with_brain_root("/tmp/brain")
            .with_enabled(false);

        let ctx = empty_ctx(json!({}));
        let ctx = node.process(ctx).await.expect("process should succeed");

        assert!(
            stub.last_call().is_none(),
            "disabled node must never call the seam"
        );

        let result = &ctx.nodes[NODE_NAME];
        assert_eq!(result["materialized"], json!(false));
        assert_eq!(result["skipped"], json!(true));
        assert_eq!(result["dry_run"], json!(false));
        assert_eq!(result["model"], json!("opportunity"));
        assert_eq!(result["paths"], json!([]));
        assert_eq!(result["warnings"], json!([]));
    }

    #[tokio::test]
    async fn disabled_node_succeeds_even_with_an_unresolvable_brain_root() {
        let stub = StubDocMaterializer::succeeding(success_outcome());
        // No `with_brain_root` set, so an enabled run would fall through to
        // `resolve_brain_root()` — which may fail depending on cwd/env. The
        // disabled path must succeed regardless, since it never resolves
        // the root at all.
        let node = MaterializeDocNode::new("opportunity")
            .with_materializer(Arc::new(stub.clone()))
            .with_enabled(false);

        let ctx = empty_ctx(json!({}));
        let ctx = node
            .process(ctx)
            .await
            .expect("disabled process should always succeed");

        assert!(stub.last_call().is_none());
        let result = &ctx.nodes[NODE_NAME];
        assert_eq!(result["skipped"], json!(true));
        assert_eq!(result["materialized"], json!(false));
    }

    #[tokio::test]
    async fn with_enabled_defaults_to_true_and_leaves_enabled_behavior_unchanged() {
        let stub = StubDocMaterializer::succeeding(success_outcome());
        let node = MaterializeDocNode::new("opportunity")
            .with_materializer(Arc::new(stub.clone()))
            .with_brain_root("/tmp/brain");

        let ctx = empty_ctx(json!({}));
        let ctx = node.process(ctx).await.expect("process should succeed");

        assert!(
            stub.last_call().is_some(),
            "enabled node must call the seam"
        );
        let result = &ctx.nodes[NODE_NAME];
        assert_eq!(result["skipped"], json!(false));
        assert_eq!(result["materialized"], json!(true));
    }

    #[tokio::test]
    async fn with_write_false_stamps_dry_run_true() {
        let stub = StubDocMaterializer::succeeding(MaterializeOutcome {
            wrote: false,
            ..success_outcome()
        });
        let node = MaterializeDocNode::new("opportunity")
            .with_materializer(Arc::new(stub))
            .with_brain_root("/tmp/brain")
            .with_write(false);

        let ctx = empty_ctx(json!({}));
        let ctx = node.process(ctx).await.expect("process should succeed");

        let result = &ctx.nodes[NODE_NAME];
        assert_eq!(result["dry_run"], json!(true));
        assert_eq!(result["materialized"], json!(false));
    }

    #[tokio::test]
    async fn identity_override_stamps_result_under_the_overridden_identity() {
        let stub = StubDocMaterializer::succeeding(success_outcome());
        let node = MaterializeDocNode::new("opportunity")
            .with_materializer(Arc::new(stub))
            .with_brain_root("/tmp/brain")
            .with_identity("MaterializeOpportunityDoc");

        let ctx = empty_ctx(json!({}));
        let ctx = node.process(ctx).await.expect("process should succeed");

        assert!(ctx.nodes.contains_key("MaterializeOpportunityDoc"));
        assert!(!ctx.nodes.contains_key(NODE_NAME));
    }
}
