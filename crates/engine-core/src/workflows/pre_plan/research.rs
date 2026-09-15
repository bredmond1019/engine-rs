//! `ResearchCodebaseNode` — `PRE_PLAN`'s read-only research step (`EN.19.A`
//! task 2).
//!
//! Composes `crate::nodes::agent_code_step::AgentCodeStep` exactly as
//! `content_pipeline::summarize::SummarizeNode` does, but exposes its
//! transport override through the shared `super::super::llm_node::
//! {TransportSlotted, Cancellable}` traits (standing rule 11) rather than a
//! hand-rolled `with_transport`/`with_meta_transport` pair — this is a new
//! call site, not a port of a pre-consolidation node, so it goes straight to
//! the current shape.
//!
//! The composed `AgentCodeStep`'s `Config` is read-only by construction:
//! `allowed_tools` is exactly `[Read, Grep, Glob]` and `disallowed_tools`
//! includes `Write`/`Bash`/`Edit` — this is the acceptance-criteria-5/6
//! guarantee the block record's prompt-injection and secret-file-denial
//! tests exercise. The stable instructions live in
//! `prompts/research_codebase.md` (`include_str!`, per standing rule 7 /
//! D24); only the per-run idea text and slug are interpolated in the
//! Rust-side prompt body, via `AgentCodeStep::with_prompt_builder` so the
//! prompt is built fresh from whatever `TaskContext` `intake::IntakeIdeaNode`
//! stamped.

use claude_code_rs::Config;
use engine_contract::TaskContext;

use crate::cancellation::CancellationToken;
use crate::node::{Node, NodeError};
use crate::nodes::AgentCodeStep;
use crate::workflows::llm_node::{Cancellable, TransportSlotted};
use crate::workflows::{get_result, TransportSlot};

use super::intake;

/// The `Node::name()` identity this node registers under, and the
/// `ctx.nodes` key `AgentCodeStep::process` stamps its output onto (task 3's
/// `WriteNotesNode` reads `ctx.nodes[NODE_NAME]["content"]`).
pub const NODE_NAME: &str = "ResearchCodebaseNode";

/// The default model this node requests absent a resolved policy override —
/// `EN.19.A` task 4 is where a per-stage `ModelTier` knob gets wired through
/// `planning/harness.json`; task 2 only needs a working, safe-by-default
/// research session.
const DEFAULT_MODEL: &str = "claude-sonnet-4-5";

/// Stable, run-invariant research instructions used as the prompt's leading,
/// cacheable prefix — never interpolated with per-run text (standing rule 7 /
/// D24).
const STABLE_RESEARCH_PROMPT: &str = include_str!("prompts/research_codebase.md");

/// Build the `Config` this node composes its `AgentCodeStep` with: read-only
/// by construction (`Read`/`Grep`/`Glob` only; `Write`/`Bash`/`Edit`
/// explicitly disallowed), so a prompt-injected instruction in the idea text
/// has no tool available to act on it regardless of what the model does with
/// the text.
fn read_only_config() -> Config {
    Config {
        model: Some(DEFAULT_MODEL.to_string()),
        allowed_tools: vec!["Read".to_string(), "Grep".to_string(), "Glob".to_string()],
        disallowed_tools: vec!["Write".to_string(), "Bash".to_string(), "Edit".to_string()],
        ..Config::default()
    }
}

/// Build the per-run prompt: the stable instructions, followed by the idea
/// text and slug `intake::IntakeIdeaNode` stamped onto `ctx`. Built fresh
/// from the live context on every call (`AgentCodeStep::with_prompt_builder`)
/// rather than captured once, so a re-run of this node against an updated
/// context always reflects what `IntakeIdeaNode` actually stored.
fn build_prompt(ctx: &TaskContext) -> String {
    let intake = get_result(ctx, intake::NODE_NAME);
    let idea = intake
        .and_then(|value| value.get("idea"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let slug = intake
        .and_then(|value| value.get("slug"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();

    format!("{STABLE_RESEARCH_PROMPT}\n\nSlug: {slug}\nIdea: {idea}\n")
}

/// Read-only `AgentCodeStep`-composing research step. No hand-rolled
/// transport field or builder pair — `transport`/`cancellation_token` are
/// exposed via [`TransportSlotted`]/[`Cancellable`], whose default methods
/// give this struct `with_meta_transport`/`with_transport`/
/// `with_cancellation_token` for free.
pub struct ResearchCodebaseNode {
    config: Config,
    transport: TransportSlot,
    cancellation_token: Option<CancellationToken>,
}

impl ResearchCodebaseNode {
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: read_only_config(),
            transport: TransportSlot::default(),
            cancellation_token: None,
        }
    }
}

impl Default for ResearchCodebaseNode {
    fn default() -> Self {
        Self::new()
    }
}

/// See `llm_node::TransportSlotted`'s doc comment — `with_meta_transport`/
/// `with_transport` are default methods over this one field.
impl TransportSlotted for ResearchCodebaseNode {
    fn transport_slot_mut(&mut self) -> &mut TransportSlot {
        &mut self.transport
    }
}

/// See `llm_node::Cancellable`'s doc comment — `with_cancellation_token` is a
/// default method over this one field. `None` (the default `new()` sets) is
/// behavior-stable: no token, no cancellation check.
impl Cancellable for ResearchCodebaseNode {
    fn cancellation_token_mut(&mut self) -> &mut Option<CancellationToken> {
        &mut self.cancellation_token
    }
}

#[async_trait::async_trait]
impl Node for ResearchCodebaseNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        // Fail fast, naming the missing dependency, rather than composing a
        // step whose prompt would silently interpolate empty idea/slug
        // strings — mirrors `TriageTaskNode`'s
        // `get_result(...).ok_or_else(...)?` shape for a required
        // upstream node.
        if get_result(&ctx, intake::NODE_NAME).is_none() {
            return Err(NodeError::new(format!(
                "{NODE_NAME}: {} has not run yet",
                intake::NODE_NAME
            )));
        }

        let mut step = self.transport.apply(AgentCodeStep::with_prompt_builder(
            NODE_NAME,
            self.config.clone(),
            build_prompt,
        ));
        if let Some(token) = self.cancellation_token.clone() {
            step = step.with_cancellation_token(token);
        }

        // `AgentCodeStep::process` stamps `ctx.nodes[NODE_NAME]` itself
        // (content/transport/cost/etc.) since it was constructed with this
        // node's own `NODE_NAME` — nothing further to stamp here.
        step.process(ctx).await
    }

    fn name(&self) -> &str {
        NODE_NAME
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::collections::HashMap;
    use std::sync::Arc;

    use claude_code_rs::Outcome;
    use futures::future::BoxFuture;
    use serde_json::json;

    use crate::nodes::{MetaTransport, TransportInfo};
    use crate::workflows::ModelTransport;

    use super::*;

    fn empty_context(event: serde_json::Value) -> TaskContext {
        TaskContext {
            event,
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        }
    }

    fn context_with_intake(idea: &str, slug: &str) -> TaskContext {
        let mut ctx = empty_context(json!({}));
        crate::workflows::put_result(
            &mut ctx,
            intake::NODE_NAME,
            json!({"idea": idea, "slug": slug, "channel": null, "sender": null}),
        );
        ctx
    }

    fn canned_outcome(text: String) -> Outcome {
        Outcome {
            cost_usd: 0.0,
            usage: claude_code_rs::parse::Usage {
                input_tokens: 1,
                output_tokens: 1,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
            model_usage: BTreeMap::new(),
            text,
            is_error: false,
            api_error_status: None,
            session_id: None,
            structured_output: None,
        }
    }

    fn stub_plain_transport(text: &'static str) -> ModelTransport {
        Arc::new(move |_config, _prompt| {
            let outcome = canned_outcome(text.to_string());
            Box::pin(async move { Ok(outcome) })
                as BoxFuture<'static, claude_code_rs::Result<Outcome>>
        })
    }

    /// Like [`stub_plain_transport`], but yields before resolving — used by
    /// the cancellation test so an *already*-cancelled token deterministically
    /// wins `tokio::select!`'s race instead of depending on which of two
    /// simultaneously-ready branches the runtime happens to poll first.
    fn slow_stub_plain_transport(text: &'static str) -> ModelTransport {
        Arc::new(move |_config, _prompt| {
            let outcome = canned_outcome(text.to_string());
            Box::pin(async move {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                Ok(outcome)
            }) as BoxFuture<'static, claude_code_rs::Result<Outcome>>
        })
    }

    fn stub_meta_transport(tier: &'static str) -> MetaTransport {
        Arc::new(move |_config, _prompt| {
            let outcome = canned_outcome("meta findings".to_string());
            let info = TransportInfo {
                tier: tier.to_string(),
                model: "stub-model".to_string(),
                endpoint: None,
                backend: "claude_cli".to_string(),
                cost_known: true,
                extra: BTreeMap::new(),
            };
            Box::pin(async move { Ok((outcome, info)) })
                as BoxFuture<'static, claude_code_rs::Result<(Outcome, TransportInfo)>>
        })
    }

    #[test]
    fn config_is_read_only_by_construction() {
        let node = ResearchCodebaseNode::new();
        assert_eq!(
            node.config.allowed_tools,
            vec!["Read".to_string(), "Grep".to_string(), "Glob".to_string()]
        );
        assert!(node.config.disallowed_tools.contains(&"Write".to_string()));
        assert!(node.config.disallowed_tools.contains(&"Bash".to_string()));
        assert!(node.config.disallowed_tools.contains(&"Edit".to_string()));
    }

    #[test]
    fn stable_prompt_is_pulled_from_the_prompts_file() {
        // Guards against the prompt regressing to an inline literal —
        // standing rule 7 / D24. A non-trivial length plus a phrase unique
        // to the file is enough to prove this is the real included text,
        // not an empty/placeholder string.
        assert!(STABLE_RESEARCH_PROMPT.len() > 200);
        assert!(STABLE_RESEARCH_PROMPT.contains("read-only"));
        assert!(STABLE_RESEARCH_PROMPT.contains("VERIFIED"));
    }

    #[test]
    fn build_prompt_interpolates_idea_and_slug_after_the_stable_prefix() {
        let ctx = context_with_intake("build a widget", "widget-idea");
        let prompt = build_prompt(&ctx);
        assert!(prompt.starts_with(STABLE_RESEARCH_PROMPT));
        assert!(prompt.contains("Idea: build a widget"));
        assert!(prompt.contains("Slug: widget-idea"));
    }

    #[test]
    fn build_prompt_tolerates_missing_intake_with_empty_fields() {
        let ctx = empty_context(json!({}));
        let prompt = build_prompt(&ctx);
        assert!(prompt.starts_with(STABLE_RESEARCH_PROMPT));
        assert!(prompt.contains("Idea: \n"));
    }

    #[tokio::test]
    async fn process_errors_when_intake_has_not_run() {
        let node = ResearchCodebaseNode::new();
        let ctx = empty_context(json!({}));

        let err = node
            .process(ctx)
            .await
            .expect_err("should fail without IntakeIdeaNode having run");
        assert!(err.message.contains(intake::NODE_NAME));
    }

    #[tokio::test]
    async fn process_stamps_content_from_the_plain_transport() {
        let node = ResearchCodebaseNode::new().with_transport(stub_plain_transport("findings"));
        let ctx = context_with_intake("build a widget", "widget-idea");

        let ctx = node.process(ctx).await.expect("process should succeed");
        let stored = ctx.nodes.get(NODE_NAME).expect("result stored");
        assert_eq!(
            stored.get("content").and_then(|v| v.as_str()),
            Some("findings")
        );
    }

    #[tokio::test]
    async fn process_meta_transport_wins_over_plain_transport() {
        let node = ResearchCodebaseNode::new()
            .with_transport(stub_plain_transport("plain"))
            .with_meta_transport(stub_meta_transport("local"));
        let ctx = context_with_intake("build a widget", "widget-idea");

        let ctx = node.process(ctx).await.expect("process should succeed");
        let stored = ctx.nodes.get(NODE_NAME).expect("result stored");
        assert_eq!(
            stored
                .get("transport")
                .and_then(|t| t.get("tier"))
                .and_then(|v| v.as_str()),
            Some("local")
        );
    }

    #[tokio::test]
    async fn process_with_cancellation_token_cancelled_returns_ctx_unchanged() {
        let token = CancellationToken::new();
        token.cancel();
        let node = ResearchCodebaseNode::new()
            .with_transport(slow_stub_plain_transport("findings"))
            .with_cancellation_token(token);
        let ctx = context_with_intake("build a widget", "widget-idea");

        let ctx = node
            .process(ctx)
            .await
            .expect("a cancel win returns Ok, not Err");
        assert!(ctx.nodes.get(NODE_NAME).is_none());
    }

    #[test]
    fn name_matches_node_name_const() {
        assert_eq!(ResearchCodebaseNode::new().name(), NODE_NAME);
    }
}
