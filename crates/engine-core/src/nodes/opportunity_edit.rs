//! `OpportunityEditNode` (`EN.7.B` task 3) — the generic node that drives one
//! [`crate::nodes::doc_materializer::OpportunityEdit`] operation
//! (`set-stage` | `add-action`) against the [`DocMaterializer`] seam's
//! [`DocMaterializer::edit_opportunity`] method.
//!
//! Lives under `nodes/`, not under a `workflows::*` module, for the same
//! reason `MaterializeDocNode` does (see `crate::nodes::materialize_doc`'s
//! module doc): it is generic machinery — the node is configured with WHICH
//! edit it performs (an [`OpportunityEditOp`]), and reads the edit's
//! ARGUMENTS off `ctx.event` at run time. `EN.7.B` task 5 wires two
//! `NodeExt::with_identity`-distinguished instances of it into the
//! `set-stage` / `add-action` single-node micro-workflows.
//!
//! Modeled directly on `crate::nodes::materialize_doc::MaterializeDocNode`:
//! a `NODE_NAME` const, the same `with_materializer` / `with_brain_root` /
//! `with_write` builder shape, `put_result` for the `ctx.nodes` stamp (under
//! `self.name()` so `with_identity` composes), and node-name-prefixed
//! `NodeError` messages.
//!
//! An invalid stage (`E_DOC_BAD_STAGE`) or an unknown slug both come back
//! from the seam as `Ok(MaterializeOutcome)` carrying an error-severity
//! diagnostic (see `doc_materializer`'s module doc) — this node is where
//! those diagnostics turn into a `NodeError` that fails the run.

use std::path::PathBuf;
use std::sync::Arc;

use engine_contract::TaskContext;
use serde_json::{json, Value};

use crate::brain_root::resolve_brain_root;
use crate::node::{Node, NodeError};
use crate::nodes::doc_materializer::{doc_materializer_live, DocMaterializer, OpportunityEdit};
use crate::workflows::opportunity_edit::schema::{
    AddOpportunityActionEvent, SetOpportunityStageEvent,
};
use crate::workflows::put_result;

/// The `Node::name()` identity `OpportunityEditNode` runs under by default,
/// and the `ctx.nodes` key its result is stamped onto when no identity
/// override (`NodeExt::with_identity`) is applied.
pub const NODE_NAME: &str = "OpportunityEditNode";

/// Which opportunity-edit operation a configured `OpportunityEditNode`
/// instance performs. Node CONFIGURATION, not event data — the two workflow
/// entry points `EN.7.B` task 5 declares are each a single node pinned to
/// one variant, so the workflow type alone is self-describing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpportunityEditOp {
    SetStage,
    AddAction,
}

impl OpportunityEditOp {
    /// The `op` value stamped into the node's result — mirrors
    /// `OpportunityEdit`'s `#[serde(tag = "op", rename_all = "kebab-case")]`
    /// spelling so the two never drift apart.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            OpportunityEditOp::SetStage => "set-stage",
            OpportunityEditOp::AddAction => "add-action",
        }
    }
}

/// Read a required string field off `event`, returning a `NodeError` naming
/// `node_name` and `field` when it is missing or not a string.
fn require_str(event: &Value, node_name: &str, field: &str) -> Result<String, NodeError> {
    event
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            NodeError::new(format!(
                "{node_name}: missing or non-string field '{field}' on event"
            ))
        })
}

/// The generic opportunity-edit node: configured with WHICH edit it
/// performs, reads the edit's arguments off `ctx.event`, and calls the
/// injectable [`DocMaterializer::edit_opportunity`] seam.
pub struct OpportunityEditNode {
    op: OpportunityEditOp,
    materializer: Arc<dyn DocMaterializer>,
    brain_root: Option<PathBuf>,
    write: bool,
}

impl OpportunityEditNode {
    /// Construct for `op` with the live `mev`-backed materializer,
    /// `write = true`, and no explicit brain root (resolved at run time via
    /// `crate::brain_root::resolve_brain_root`).
    #[must_use]
    pub fn new(op: OpportunityEditOp) -> Self {
        Self {
            op,
            materializer: doc_materializer_live(),
            brain_root: None,
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

    /// Whether to actually write to disk (`true`, the default) or dry-run
    /// (`false` — nothing touches disk, but the stamped result still names
    /// the path(s) that would have been written).
    #[must_use]
    pub fn with_write(mut self, write: bool) -> Self {
        self.write = write;
        self
    }

    /// Resolve the brain root: the explicit `with_brain_root` value when
    /// set, otherwise `crate::brain_root::resolve_brain_root()`. Mirrors
    /// `MaterializeDocNode::resolve_root`'s error mapping.
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

    /// Build the `OpportunityEdit` this node's configured op describes,
    /// reading its arguments off `ctx.event`. The fields read here are
    /// exactly the fields declared by `workflows::opportunity_edit::schema`'s
    /// `SetOpportunityStageEvent` / `AddOpportunityActionEvent` — those
    /// schema types are the single source of truth for "which fields does
    /// this event carry"; this builder assembles one of them field-by-field
    /// (rather than a single `serde_json::from_value`) purely so a missing
    /// or ill-typed field can be reported as a `NodeError` naming that
    /// field, instead of one opaque serde error.
    fn build_edit(&self, event: &Value) -> Result<OpportunityEdit, NodeError> {
        let node_name = self.name();
        let slug = require_str(event, node_name, "slug")?;

        match self.op {
            OpportunityEditOp::SetStage => {
                let stage = require_str(event, node_name, "stage")?;
                let event = SetOpportunityStageEvent { slug, stage };
                Ok(OpportunityEdit::SetStage {
                    slug: event.slug,
                    stage: event.stage,
                })
            }
            OpportunityEditOp::AddAction => {
                let at = require_str(event, node_name, "at")?;
                let kind = require_str(event, node_name, "kind")?;
                let note = require_str(event, node_name, "note")?;
                let event = AddOpportunityActionEvent {
                    slug,
                    at,
                    kind,
                    note,
                };
                Ok(OpportunityEdit::AddAction {
                    slug: event.slug,
                    at: event.at,
                    kind: event.kind,
                    note: event.note,
                })
            }
        }
    }
}

/// The `slug` argument every `OpportunityEdit` variant carries — read back
/// out for the result stamp without re-parsing `ctx.event`.
fn edit_slug(edit: &OpportunityEdit) -> &str {
    match edit {
        OpportunityEdit::SetStage { slug, .. }
        | OpportunityEdit::AddAction { slug, .. }
        | OpportunityEdit::MergeContacts { slug, .. } => slug,
    }
}

#[async_trait::async_trait]
impl Node for OpportunityEditNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let root = self.resolve_root()?;
        let edit = self.build_edit(&ctx.event)?;

        let outcome = self
            .materializer
            .edit_opportunity(&root, &edit, self.write)
            .await
            .map_err(|err| NodeError::new(format!("{}: {err}", self.name())))?;

        if let Some(error) = outcome.errors().first() {
            return Err(NodeError::new(format!(
                "{}: opportunity edit failed for '{}': {}",
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
                "edited": outcome.wrote,
                "dry_run": !self.write,
                "op": self.op.as_str(),
                "slug": edit_slug(&edit),
                "paths": paths,
                "warnings": warnings,
                "no_op": outcome.planned.is_empty(),
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
    async fn set_stage_op_passes_the_right_edit_through_to_the_seam() {
        let stub = StubDocMaterializer::succeeding(success_outcome());
        let node = OpportunityEditNode::new(OpportunityEditOp::SetStage)
            .with_materializer(Arc::new(stub.clone()))
            .with_brain_root("/tmp/brain");

        let ctx = empty_ctx(json!({"slug": "acme-corp", "stage": "contacted"}));
        node.process(ctx).await.expect("process should succeed");

        let call = stub.last_edit().expect("edit_opportunity should be called");
        assert_eq!(call.root, PathBuf::from("/tmp/brain"));
        assert!(call.write);
        assert_eq!(
            call.edit,
            OpportunityEdit::SetStage {
                slug: "acme-corp".to_string(),
                stage: "contacted".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn add_action_op_passes_the_right_edit_through_to_the_seam() {
        let stub = StubDocMaterializer::succeeding(success_outcome());
        let node = OpportunityEditNode::new(OpportunityEditOp::AddAction)
            .with_materializer(Arc::new(stub.clone()))
            .with_brain_root("/tmp/brain");

        let ctx = empty_ctx(json!({
            "slug": "acme-corp",
            "at": "2026-07-27",
            "kind": "email",
            "note": "Sent intro email",
        }));
        node.process(ctx).await.expect("process should succeed");

        let call = stub.last_edit().expect("edit_opportunity should be called");
        assert_eq!(
            call.edit,
            OpportunityEdit::AddAction {
                slug: "acme-corp".to_string(),
                at: "2026-07-27".to_string(),
                kind: "email".to_string(),
                note: "Sent intro email".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn seam_failure_becomes_a_node_error() {
        let stub = StubDocMaterializer::failing("disk full");
        let node = OpportunityEditNode::new(OpportunityEditOp::SetStage)
            .with_materializer(Arc::new(stub))
            .with_brain_root("/tmp/brain");

        let ctx = empty_ctx(json!({"slug": "acme-corp", "stage": "contacted"}));
        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("OpportunityEditNode"));
        assert!(err.message.contains("disk full"));
    }

    #[tokio::test]
    async fn an_error_severity_diagnostic_becomes_a_node_error_even_on_ok() {
        let outcome = MaterializeOutcome {
            wrote: false,
            planned: vec![],
            diagnostics: vec![MaterializeDiagnostic {
                severity: "error".to_string(),
                file: PathBuf::from("/tmp/brain/opportunities/acme-corp.md"),
                code: "E_DOC_BAD_STAGE".to_string(),
                message: "unknown stage 'nope' (expected one of: identified | researching)"
                    .to_string(),
            }],
        };
        let stub = StubDocMaterializer::succeeding(outcome);
        let node = OpportunityEditNode::new(OpportunityEditOp::SetStage)
            .with_materializer(Arc::new(stub))
            .with_brain_root("/tmp/brain");

        let ctx = empty_ctx(json!({"slug": "acme-corp", "stage": "nope"}));
        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("OpportunityEditNode"));
        assert!(err.message.contains("unknown stage"));
    }

    #[tokio::test]
    async fn missing_slug_field_is_a_node_error_naming_the_field() {
        let stub = StubDocMaterializer::succeeding(success_outcome());
        let node = OpportunityEditNode::new(OpportunityEditOp::SetStage)
            .with_materializer(Arc::new(stub))
            .with_brain_root("/tmp/brain");

        let ctx = empty_ctx(json!({"stage": "contacted"}));
        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("OpportunityEditNode"));
        assert!(err.message.contains("'slug'"));
    }

    #[tokio::test]
    async fn missing_stage_field_is_a_node_error_naming_the_field() {
        let stub = StubDocMaterializer::succeeding(success_outcome());
        let node = OpportunityEditNode::new(OpportunityEditOp::SetStage)
            .with_materializer(Arc::new(stub))
            .with_brain_root("/tmp/brain");

        let ctx = empty_ctx(json!({"slug": "acme-corp"}));
        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("'stage'"));
    }

    #[tokio::test]
    async fn missing_add_action_fields_are_node_errors_naming_the_field() {
        let stub = StubDocMaterializer::succeeding(success_outcome());

        for missing in ["at", "kind", "note"] {
            let mut event = json!({
                "slug": "acme-corp",
                "at": "2026-07-27",
                "kind": "email",
                "note": "Sent intro email",
            });
            event.as_object_mut().unwrap().remove(missing);

            let node = OpportunityEditNode::new(OpportunityEditOp::AddAction)
                .with_materializer(Arc::new(stub.clone()))
                .with_brain_root("/tmp/brain");

            let err = node
                .process(empty_ctx(event))
                .await
                .expect_err("should fail");
            assert!(
                err.message.contains(&format!("'{missing}'")),
                "expected error naming '{missing}', got: {}",
                err.message
            );
        }
    }

    #[tokio::test]
    async fn ill_typed_field_is_a_node_error_naming_the_field() {
        let stub = StubDocMaterializer::succeeding(success_outcome());
        let node = OpportunityEditNode::new(OpportunityEditOp::SetStage)
            .with_materializer(Arc::new(stub))
            .with_brain_root("/tmp/brain");

        let ctx = empty_ctx(json!({"slug": "acme-corp", "stage": 42}));
        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("'stage'"));
    }

    #[tokio::test]
    async fn zero_planned_action_outcome_stamps_no_op_true_without_erroring() {
        let stub = StubDocMaterializer::succeeding(MaterializeOutcome {
            wrote: false,
            planned: vec![],
            diagnostics: vec![],
        });
        let node = OpportunityEditNode::new(OpportunityEditOp::SetStage)
            .with_materializer(Arc::new(stub))
            .with_brain_root("/tmp/brain");

        let ctx = empty_ctx(json!({"slug": "acme-corp", "stage": "contacted"}));
        let ctx = node.process(ctx).await.expect("process should succeed");

        let result = &ctx.nodes[NODE_NAME];
        assert_eq!(result["no_op"], json!(true));
        assert_eq!(result["edited"], json!(false));
    }

    #[tokio::test]
    async fn with_write_false_stamps_dry_run_true() {
        let stub = StubDocMaterializer::succeeding(MaterializeOutcome {
            wrote: false,
            ..success_outcome()
        });
        let node = OpportunityEditNode::new(OpportunityEditOp::SetStage)
            .with_materializer(Arc::new(stub))
            .with_brain_root("/tmp/brain")
            .with_write(false);

        let ctx = empty_ctx(json!({"slug": "acme-corp", "stage": "contacted"}));
        let ctx = node.process(ctx).await.expect("process should succeed");

        let result = &ctx.nodes[NODE_NAME];
        assert_eq!(result["dry_run"], json!(true));
    }

    #[tokio::test]
    async fn result_stamp_lands_under_self_name_so_with_identity_works() {
        let stub = StubDocMaterializer::succeeding(success_outcome());
        let node = OpportunityEditNode::new(OpportunityEditOp::SetStage)
            .with_materializer(Arc::new(stub))
            .with_brain_root("/tmp/brain")
            .with_identity("SetOpportunityStageNode");

        let ctx = empty_ctx(json!({"slug": "acme-corp", "stage": "contacted"}));
        let ctx = node.process(ctx).await.expect("process should succeed");

        assert!(ctx.nodes.contains_key("SetOpportunityStageNode"));
        assert!(!ctx.nodes.contains_key(NODE_NAME));
    }

    #[tokio::test]
    async fn result_stamp_contains_op_and_slug() {
        let stub = StubDocMaterializer::succeeding(success_outcome());
        let node = OpportunityEditNode::new(OpportunityEditOp::AddAction)
            .with_materializer(Arc::new(stub))
            .with_brain_root("/tmp/brain");

        let ctx = empty_ctx(json!({
            "slug": "acme-corp",
            "at": "2026-07-27",
            "kind": "email",
            "note": "Sent intro email",
        }));
        let ctx = node.process(ctx).await.expect("process should succeed");

        let result = &ctx.nodes[NODE_NAME];
        assert_eq!(result["op"], json!("add-action"));
        assert_eq!(result["slug"], json!("acme-corp"));
    }
}
