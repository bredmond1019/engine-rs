//! `PatchDocsNode` — the docs-patching model node (bottom-half, EN.3.B).
//!
//! Ported from `orchestrator/app/workflows/sdlc_flow_workflow_nodes/patch_docs_node.py`:
//! a Sonnet-tier model node (real judgment, not deterministic — per the
//! spec's Context Pointers economics classification) that reads the most
//! recent `ImplementTaskNode` output's `modified_files`, asks the model to
//! find + patch stale `docs/` references to those files/symbols, and stamps
//! `{summary, files_patched}` under its own identity. This node does not
//! touch the filesystem itself — the model performs any doc edits via its
//! own tool use / the harness that runs it; this node's job is to build the
//! prompt and record what came back (mirrors the Python docstring).

use claude_code_rs::Config;
use engine_contract::TaskContext;
use serde::Deserialize;
use serde_json::json;

use crate::node::{Node, NodeError};
use crate::nodes::ClaudeCodeStep;

use super::ModelTransport;

/// Model output shape `PatchDocsNode` expects (strict JSON reply).
#[derive(Debug, Deserialize)]
struct PatchDocsOutput {
    summary: String,
    #[serde(default)]
    files_patched: Vec<String>,
}

/// Model node (Sonnet): patches documentation referencing the task's
/// modified files. Composes a `ClaudeCodeStep` under the `PatchDocsNode`
/// identity so it can post-process the model's JSON output.
pub struct PatchDocsNode {
    config: Config,
    transport: Option<ModelTransport>,
}

impl PatchDocsNode {
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: Config {
                model: Some("claude-sonnet-4-5".to_string()),
                ..Config::default()
            },
            transport: None,
        }
    }

    /// Override the transport used by the composed `ClaudeCodeStep`. Tests
    /// use this to stub a real subprocess call with a canned `Outcome`, so
    /// the gated suite never spawns a real `claude`.
    #[must_use]
    pub fn with_transport(mut self, transport: ModelTransport) -> Self {
        self.transport = Some(transport);
        self
    }

    /// Return the `modified_files` reported by the most recent
    /// `ImplementTaskNode` pass, or an empty list if it hasn't run.
    /// `TaskContext.nodes` stores one entry per node *name*, so across a
    /// retry loop only the latest `ImplementTaskNode` run is available here
    /// — this is that latest pass's reported `modified_files`. Mirrors
    /// `PatchDocsNode._collect_modified_files` in Python.
    fn collect_modified_files(ctx: &TaskContext) -> Vec<String> {
        ctx.nodes
            .get("ImplementTaskNode")
            .and_then(|value| value.get("modified_files"))
            .and_then(|value| value.as_array())
            .map(|values| {
                values
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl Default for PatchDocsNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for PatchDocsNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let mut step = ClaudeCodeStep::with_prompt_builder(
            "PatchDocsNode",
            self.config.clone(),
            |ctx: &TaskContext| {
                let modified_files = Self::collect_modified_files(ctx);
                format!(
                    "Search docs/ for stale references to the following \
                     modified files/symbols and patch them. Respond with \
                     strict JSON of the shape {{\"summary\": str, \
                     \"files_patched\": [str]}}.\n\nModified files: {}",
                    json!(modified_files)
                )
            },
        );
        if let Some(transport) = self.transport.clone() {
            step = step.with_transport(move |config, prompt| (transport)(config, prompt));
        }

        let mut ctx = step.process(ctx).await?;

        let content = ctx
            .nodes
            .get("PatchDocsNode")
            .and_then(|value| value.get("content"))
            .and_then(|value| value.as_str())
            .ok_or_else(|| NodeError::new("PatchDocsNode: model returned no content"))?
            .to_string();

        let parsed: PatchDocsOutput = serde_json::from_str(super::strip_json_fence(&content))
            .map_err(|err| {
                NodeError::new(format!(
                    "PatchDocsNode: failed to parse model output as JSON: {err}"
                ))
            })?;

        super::put_result(
            &mut ctx,
            "PatchDocsNode",
            json!({
                "summary": parsed.summary,
                "files_patched": parsed.files_patched,
            }),
        );

        Ok(ctx)
    }

    fn name(&self) -> &str {
        "PatchDocsNode"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_code_rs::Outcome;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn empty_context(event: serde_json::Value) -> TaskContext {
        TaskContext {
            event,
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        }
    }

    fn stub_transport(reply: serde_json::Value) -> ModelTransport {
        Arc::new(move |_config, _prompt| {
            let outcome = Outcome {
                cost_usd: 0.0,
                usage: claude_code_rs::parse::Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
                model_usage: std::collections::BTreeMap::new(),
                text: reply.to_string(),
                is_error: false,
                api_error_status: None,
                structured_output: None,
            };
            Box::pin(async move { Ok(outcome) })
        })
    }

    #[tokio::test]
    async fn stamps_summary_and_files_patched_from_stub_transport() {
        let mut ctx = empty_context(json!({}));
        ctx.nodes.insert(
            "ImplementTaskNode".to_string(),
            json!({
                "summary": "did the thing",
                "modified_files": ["src/foo.rs", "src/bar.rs"],
                "tests_added": [],
            }),
        );

        let node = PatchDocsNode::new().with_transport(stub_transport(json!({
            "summary": "patched stale references",
            "files_patched": ["docs/foo.md"],
        })));

        let out = node.process(ctx).await.expect("process should succeed");
        let result = out.nodes.get("PatchDocsNode").expect("output present");
        assert_eq!(result["summary"], "patched stale references");
        assert_eq!(result["files_patched"], json!(["docs/foo.md"]));
    }

    #[tokio::test]
    async fn collects_modified_files_from_latest_implement_task_node() {
        let mut ctx = empty_context(json!({}));
        ctx.nodes.insert(
            "ImplementTaskNode".to_string(),
            json!({
                "summary": "s",
                "modified_files": ["a.rs", "b.rs"],
                "tests_added": [],
            }),
        );
        assert_eq!(
            PatchDocsNode::collect_modified_files(&ctx),
            vec!["a.rs".to_string(), "b.rs".to_string()]
        );
    }

    #[tokio::test]
    async fn collects_empty_when_implement_task_node_absent() {
        let ctx = empty_context(json!({}));
        assert_eq!(
            PatchDocsNode::collect_modified_files(&ctx),
            Vec::<String>::new()
        );
    }

    #[tokio::test]
    async fn errors_on_non_json_model_reply() {
        let ctx = empty_context(json!({}));
        let node = PatchDocsNode::new().with_transport(Arc::new(|_config, _prompt| {
            let outcome = Outcome {
                cost_usd: 0.0,
                usage: claude_code_rs::parse::Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
                model_usage: std::collections::BTreeMap::new(),
                text: "not json".to_string(),
                is_error: false,
                api_error_status: None,
                structured_output: None,
            };
            Box::pin(async move { Ok(outcome) })
        }));

        let result = node.process(ctx).await;
        assert!(result.is_err());
    }
}
