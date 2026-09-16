//! `DecomposePlanNode` — the one model-calling stage of the `plan_authoring`
//! workflow (`EN.19.B` task 2). Composes `AgentCodeStep` and implements the
//! shared `TransportSlotted`/`Cancellable` traits (standing rule 11) rather
//! than hand-rolling a `TransportSlot` field or a bespoke local-vs-cloud
//! branch — see `crate::workflows::llm_node`'s own doc comment for why.
//!
//! This node needs no tool-calling: it is prose-in-prose-out (read the
//! gathered context, propose block boundaries, respond with structured
//! JSON), so its `Config` carries `allowed_tools: vec![]` — `Config`
//! derives `Default`, so an unset `allowed_tools` is already an empty
//! `Vec`; this node sets it explicitly so the "no tools at all" intent is
//! visible at the call site rather than an accident of the derive.
//!
//! The stable decomposition prompt (`.claude/commands/plan.md`'s step-5
//! rules, carried verbatim) lives in `prompts/decompose.md` and is pulled in
//! via `include_str!` per standing rule 7 / D24 — only the per-run body
//! (this run's gathered context, from [`super::gather_context::GatherPlanContextNode`])
//! is built at runtime, so the cache breakpoint over the stable prefix stays
//! run-invariant.
//!
//! # `_incomplete` candidates, not silently dropped fields
//!
//! `EN.19.B.json`'s acceptance criterion 3 requires that a candidate block
//! whose required field (`why`/`what`/`files`/`acceptance_criteria`/etc.)
//! the model could not fill from the given input is staged with an explicit
//! `_incomplete: true` marker and a `_missing_fields` array naming what's
//! absent — never silently staged as if complete. [`mark_incomplete`]
//! enforces this deterministically over every candidate the model returns,
//! independent of whether the model itself remembered to flag the gap.

use claude_code_rs::Config;
use engine_contract::TaskContext;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::cancellation::CancellationToken;
use crate::node::{Node, NodeError};
use crate::nodes::AgentCodeStep;
use crate::workflows::llm_node::{Cancellable, TransportSlotted};
use crate::workflows::sdlc_flow::carry_forward_billing;
use crate::workflows::{
    get_result, parse_structured_or_fenced, put_result, session_baseline, sessions_since,
    TransportSlot,
};

use super::check_existing::parse_slug;
use super::gather_context;

/// The `Node::name()` identity `DecomposePlanNode` is registered under, and
/// the `ctx.nodes` key its candidates (and the transport stamp
/// `AgentCodeStep::process` writes) land on. Block record acceptance
/// criterion 4 requires this exact identity so `GET /events/{run_id}`
/// surfaces the resolved model/tier under it.
pub const NODE_NAME: &str = "DecomposePlanNode";

/// The stable decomposition prompt — `.claude/commands/plan.md`'s step-5
/// rules (ships-alone test, name files by path, no stack specifics,
/// sequence by dependency and competence) carried verbatim, plus this
/// node's own output-shape contract. Compiled in via `include_str!`
/// (standing rule 7 / D24): a runtime file read would not survive a
/// deployed `bastion serve` that never has this source tree on disk.
const STABLE_DECOMPOSE_PROMPT: &str = include_str!("prompts/decompose.md");

/// The candidate-block fields this node treats as required — a SUBSET of
/// `.claude/workflows/block.schema.json`'s own required fields, per the
/// block record's `what` (`id_placeholder` is a staging-time mint, not
/// something the model must supply, so it is deliberately absent here).
const REQUIRED_CANDIDATE_FIELDS: [&str; 7] = [
    "title",
    "description",
    "what",
    "why",
    "files",
    "out_of_scope",
    "acceptance_criteria",
];

/// The JSON Schema `Config::json_schema` enforces on the model's reply —
/// `{"candidates": [<Candidate>, ...]}`, one entry per proposed block.
/// Deliberately does not mark any per-candidate field `required`: a model
/// that cannot fill one is expected to omit it (per the prompt's own
/// instruction), and [`mark_incomplete`] is what turns that omission into
/// an explicit, staged `_incomplete` marker rather than a schema rejection.
fn candidate_response_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "candidates": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "id_placeholder": { "type": "string" },
                        "title": { "type": "string" },
                        "description": { "type": "string" },
                        "what": { "type": "string" },
                        "why": { "type": "string" },
                        "files": { "type": "array", "items": { "type": "string" } },
                        "out_of_scope": { "type": "array", "items": { "type": "string" } },
                        "acceptance_criteria": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                    },
                },
            },
        },
        "required": ["candidates"],
    })
}

/// The model's raw structured reply — deliberately loose (`Vec<Value>`
/// rather than a strict per-field struct) so a candidate missing a required
/// field still deserializes instead of failing the whole response; the
/// missing-field handling happens in [`mark_incomplete`], not here.
#[derive(Debug, Deserialize)]
struct DecomposeResponse {
    #[serde(default)]
    candidates: Vec<Value>,
}

/// Stamp `_incomplete: true` + `_missing_fields: [...]` onto `candidate`
/// when any of [`REQUIRED_CANDIDATE_FIELDS`] is absent or JSON `null`;
/// otherwise return it unchanged (any stale `_incomplete`/`_missing_fields`
/// pair the model itself emitted is cleared, since this function is the
/// single source of truth for that marker, not the model).
fn mark_incomplete(mut candidate: Value) -> Value {
    let missing: Vec<Value> = REQUIRED_CANDIDATE_FIELDS
        .iter()
        .filter(|field| candidate.get(**field).map(Value::is_null).unwrap_or(true))
        .map(|field| Value::String((*field).to_string()))
        .collect();

    if let Some(obj) = candidate.as_object_mut() {
        if missing.is_empty() {
            obj.remove("_incomplete");
            obj.remove("_missing_fields");
        } else {
            obj.insert("_incomplete".to_string(), Value::Bool(true));
            obj.insert("_missing_fields".to_string(), Value::Array(missing));
        }
    }
    candidate
}

/// Build this run's per-run prompt body: the stable decomposition rules
/// (above) plus [`super::gather_context::GatherPlanContextNode`]'s gathered
/// context, interpolated fresh on every call — never folded into the
/// `STABLE_DECOMPOSE_PROMPT` constant, so the cache breakpoint over that
/// prefix stays run-invariant (standing rule 6).
fn build_prompt(ctx: &TaskContext) -> String {
    let slug = parse_slug(ctx).unwrap_or_default();
    let gathered = get_result(ctx, gather_context::NODE_NAME)
        .cloned()
        .unwrap_or_else(|| json!({}));
    let claude_md = gathered
        .get("claude_md")
        .and_then(Value::as_str)
        .unwrap_or("(not present)");
    let context_md = gathered
        .get("context_md")
        .and_then(Value::as_str)
        .unwrap_or("(not present)");
    let highest_wave = gathered
        .get("highest_wave")
        .and_then(Value::as_i64)
        .map_or_else(|| "(none)".to_string(), |wave| wave.to_string());
    let pre_plan_files = gathered
        .get("pre_plan_files")
        .cloned()
        .unwrap_or_else(|| json!({}));

    format!(
        "{STABLE_DECOMPOSE_PROMPT}\n\n\
         ## Slug\n{slug}\n\n\
         ## CLAUDE.md\n{claude_md}\n\n\
         ## planning/context.md\n{context_md}\n\n\
         ## Highest existing phase/wave for this repo\n{highest_wave}\n\n\
         ## Pre-plan folder inputs (notes.md / sequence.md / seams.md / assessment.md)\n\
         {pre_plan_files}\n"
    )
}

/// Proposes phase/block boundaries from the gathered pre-plan context,
/// following `.claude/commands/plan.md`'s decomposition rules — the one
/// model-calling stage in this workflow.
pub struct DecomposePlanNode {
    config: Config,
    transport: TransportSlot,
    /// Taken through this node's own builder (`with_cancellation_token`,
    /// via [`Cancellable`]), never inferred from context. `None` (the
    /// default `new()` sets) is behavior-stable: no token, no cancellation
    /// check.
    cancellation_token: Option<CancellationToken>,
}

impl DecomposePlanNode {
    /// The production node: no tool access (`allowed_tools: vec![]`), the
    /// real `claude` CLI transport (`AgentCodeStep`'s own default) unless
    /// overridden via [`TransportSlotted::with_transport`]/
    /// [`TransportSlotted::with_meta_transport`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: Config {
                allowed_tools: Vec::new(),
                ..Config::default()
            },
            transport: TransportSlot::default(),
            cancellation_token: None,
        }
    }
}

impl Default for DecomposePlanNode {
    fn default() -> Self {
        Self::new()
    }
}

/// See `llm_node::TransportSlotted`'s doc comment — `with_meta_transport`/
/// `with_transport` are default methods over this one field.
impl TransportSlotted for DecomposePlanNode {
    fn transport_slot_mut(&mut self) -> &mut TransportSlot {
        &mut self.transport
    }
}

/// See `llm_node::Cancellable`'s doc comment — `with_cancellation_token` is
/// a default method over this one field.
impl Cancellable for DecomposePlanNode {
    fn cancellation_token_mut(&mut self) -> &mut Option<CancellationToken> {
        &mut self.cancellation_token
    }
}

#[async_trait::async_trait]
impl Node for DecomposePlanNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let mut config = self.config.clone();
        config.json_schema = Some(candidate_response_schema());
        let prompt = build_prompt(&ctx);

        let mut step = self
            .transport
            .apply(AgentCodeStep::new(NODE_NAME, config, prompt));
        if let Some(token) = self.cancellation_token.clone() {
            step = step.with_cancellation_token(token);
        }

        let baseline = session_baseline(&ctx);
        let mut ctx = step.process(ctx).await?;

        let content = ctx
            .nodes
            .get(NODE_NAME)
            .and_then(|value| value.get("content"))
            .and_then(|value| value.as_str())
            .ok_or_else(|| {
                NodeError::new(format!("{NODE_NAME}: model returned no content"))
                    .with_sessions(sessions_since(&ctx, baseline))
            })?
            .to_string();

        let parsed: DecomposeResponse = parse_structured_or_fenced(&ctx, NODE_NAME, &content)
            .map_err(|err| {
                NodeError::new(format!(
                    "{NODE_NAME}: failed to parse model output as JSON: {err}"
                ))
                .with_sessions(sessions_since(&ctx, baseline))
            })?;

        let candidates: Vec<Value> = parsed.candidates.into_iter().map(mark_incomplete).collect();

        // `carry_forward_billing` reads the PRIOR `ctx.nodes[NODE_NAME]`
        // entry (the one `AgentCodeStep::process` above just stamped — the
        // "transport" tier stamp, cost, cache channels) before this
        // `put_result` overwrites it with the candidates payload, so the
        // resolved model/tier stays readable via `GET /events/{run_id}`
        // (block record acceptance criterion 4).
        let mut result = json!({ "candidates": candidates });
        carry_forward_billing(&ctx, NODE_NAME, &mut result);
        put_result(&mut ctx, NODE_NAME, result);

        Ok(ctx)
    }

    fn name(&self) -> &str {
        NODE_NAME
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use claude_code_rs::Outcome;
    use futures::future::BoxFuture;
    use serde_json::json;

    use super::*;
    use crate::workflows::ModelTransport;

    fn ctx_with_event(event: Value) -> TaskContext {
        TaskContext {
            event,
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        }
    }

    fn stub_outcome_with_text(text: &str) -> Outcome {
        Outcome {
            cost_usd: 0.0,
            usage: claude_code_rs::parse::Usage {
                input_tokens: 1,
                output_tokens: 1,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
            model_usage: std::collections::BTreeMap::new(),
            text: text.to_string(),
            is_error: false,
            api_error_status: None,
            session_id: None,
            structured_output: None,
        }
    }

    fn stub_outcome_with_structured(text: &str, structured: Value) -> Outcome {
        Outcome {
            structured_output: Some(structured),
            ..stub_outcome_with_text(text)
        }
    }

    fn stub_transport_returning(outcome: Outcome) -> ModelTransport {
        Arc::new(move |_config, _prompt| {
            let outcome = outcome.clone();
            Box::pin(async move { Ok(outcome) })
                as BoxFuture<'static, claude_code_rs::Result<Outcome>>
        })
    }

    #[test]
    fn mark_incomplete_leaves_a_fully_populated_candidate_untouched() {
        let candidate = json!({
            "id_placeholder": "EN.99.A",
            "title": "t",
            "description": "d",
            "what": "w",
            "why": "why",
            "files": ["a.rs"],
            "out_of_scope": ["x"],
            "acceptance_criteria": ["y"],
        });
        let out = mark_incomplete(candidate.clone());
        assert_eq!(out, candidate);
        assert!(out.get("_incomplete").is_none());
    }

    #[test]
    fn mark_incomplete_flags_a_candidate_missing_why_and_files() {
        let candidate = json!({
            "title": "t",
            "description": "d",
            "what": "w",
            "out_of_scope": ["x"],
            "acceptance_criteria": ["y"],
        });
        let out = mark_incomplete(candidate);
        assert_eq!(out.get("_incomplete"), Some(&json!(true)));
        let missing = out
            .get("_missing_fields")
            .and_then(Value::as_array)
            .expect("missing fields array present");
        let missing: Vec<&str> = missing.iter().filter_map(Value::as_str).collect();
        assert!(missing.contains(&"why"));
        assert!(missing.contains(&"files"));
        assert_eq!(missing.len(), 2);
    }

    #[test]
    fn mark_incomplete_treats_a_null_field_the_same_as_an_absent_one() {
        let candidate = json!({
            "title": "t",
            "description": "d",
            "what": "w",
            "why": null,
            "files": ["a.rs"],
            "out_of_scope": ["x"],
            "acceptance_criteria": ["y"],
        });
        let out = mark_incomplete(candidate);
        assert_eq!(out.get("_incomplete"), Some(&json!(true)));
        let missing: Vec<&str> = out
            .get("_missing_fields")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(missing, vec!["why"]);
    }

    #[tokio::test]
    async fn config_carries_no_allowed_tools() {
        let node = DecomposePlanNode::new();
        assert!(
            node.config.allowed_tools.is_empty(),
            "DecomposePlanNode must make no tool calls"
        );
    }

    #[tokio::test]
    async fn a_candidate_missing_a_required_field_is_surfaced_as_incomplete_not_dropped() {
        let structured = json!({
            "candidates": [
                {
                    "title": "Partial block",
                    "description": "d",
                    "what": "w",
                    // "why" deliberately absent — the model could not fill it.
                    "files": ["crates/x.rs"],
                    "out_of_scope": ["nothing"],
                    "acceptance_criteria": ["passes"],
                }
            ]
        });
        let outcome = stub_outcome_with_structured("ignored", structured);
        let transport = stub_transport_returning(outcome);

        let node = DecomposePlanNode::new().with_transport(transport);
        let ctx = ctx_with_event(json!({ "slug": "test-slug" }));

        let out = node.process(ctx).await.expect("process should succeed");
        let stored = get_result(&out, NODE_NAME).expect("result stored");
        let candidates = stored
            .get("candidates")
            .and_then(Value::as_array)
            .expect("candidates array present");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].get("_incomplete"), Some(&json!(true)));
        let missing: Vec<&str> = candidates[0]
            .get("_missing_fields")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(missing, vec!["why"]);
    }

    #[tokio::test]
    async fn a_fully_populated_candidate_is_staged_without_the_incomplete_marker() {
        let structured = json!({
            "candidates": [
                {
                    "id_placeholder": "EN.99.A",
                    "title": "Full block",
                    "description": "d",
                    "what": "w",
                    "why": "why",
                    "files": ["crates/x.rs"],
                    "out_of_scope": ["nothing"],
                    "acceptance_criteria": ["passes"],
                }
            ]
        });
        let outcome = stub_outcome_with_structured("ignored", structured);
        let transport = stub_transport_returning(outcome);

        let node = DecomposePlanNode::new().with_transport(transport);
        let ctx = ctx_with_event(json!({ "slug": "test-slug" }));

        let out = node.process(ctx).await.expect("process should succeed");
        let stored = get_result(&out, NODE_NAME).expect("result stored");
        let candidates = stored.get("candidates").and_then(Value::as_array).unwrap();
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].get("_incomplete").is_none());
    }

    #[tokio::test]
    async fn transport_tier_stamp_survives_the_result_overwrite() {
        // Regression guard for the exact bug `carry_forward_billing`'s own
        // doc comment describes: a wrapper node that builds a fresh
        // `result` object and calls `put_result` must not silently drop
        // the inner `AgentCodeStep::process`'s "transport" stamp.
        let structured = json!({ "candidates": [] });
        let outcome = stub_outcome_with_structured("ignored", structured);
        let transport = stub_transport_returning(outcome);

        let node = DecomposePlanNode::new().with_transport(transport);
        let ctx = ctx_with_event(json!({ "slug": "test-slug" }));

        let out = node.process(ctx).await.expect("process should succeed");
        let stored = get_result(&out, NODE_NAME).expect("result stored");
        assert_eq!(
            stored.get("transport").and_then(|t| t.get("tier")),
            Some(&json!("cloud")),
            "resolved transport tier must be readable via ctx.nodes[DecomposePlanNode] \
             for GET /events/{{run_id}} (acceptance criterion 4)"
        );
    }

    #[tokio::test]
    async fn empty_candidates_response_stages_nothing_without_erroring() {
        let structured = json!({ "candidates": [] });
        let outcome = stub_outcome_with_structured("ignored", structured);
        let transport = stub_transport_returning(outcome);

        let node = DecomposePlanNode::new().with_transport(transport);
        let ctx = ctx_with_event(json!({ "slug": "test-slug" }));

        let out = node.process(ctx).await.expect("process should succeed");
        let stored = get_result(&out, NODE_NAME).expect("result stored");
        assert_eq!(stored.get("candidates"), Some(&json!([])));
    }

    #[test]
    fn as_router_is_none() {
        // DecomposePlanNode has no branching decision of its own — it always
        // hands off to StageCandidateBlocksNode (task 3's graph wiring).
        let node = DecomposePlanNode::new();
        assert!(node.as_router().is_none());
    }

    #[test]
    fn node_name_is_stable() {
        assert_eq!(DecomposePlanNode::new().name(), NODE_NAME);
    }
}
