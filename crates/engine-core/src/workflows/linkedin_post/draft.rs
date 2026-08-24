//! `PostDraftNode` — the model node that proposes `PostCandidate`s from the
//! `Vec<WorkSource>` gathered by `WorkSourceNode`, gated by the traceability
//! invariant on `schema::PostCandidate` and the block's unsupported-claim
//! flagging requirement (`planning/EN.5.G/tasks.md` + `tasks.json` task 4).
//!
//! A `ClaudeCodeStep`-based model node modeled on
//! `research_agent::company_research::CompanyResearchNode`, with a
//! `with_transport(...)` seam so tests stub the model and the gated suite
//! never spawns a real `claude` subprocess. On `process`:
//! 1. read the resolved [`super::policy::LinkedInPostPolicy`] stamped onto
//!    `ctx` (`crate::policy::resolved_policy_strict`) — no per-node
//!    re-resolution;
//! 2. read [`super::work_source::WorkSourceNode`]'s `sources` from
//!    `ctx.nodes`;
//! 3. apply `draft`-stage model-tier shaping to the composed `Config`;
//! 4. await the (injectable) transport and parse its reply into candidates
//!    plus any `unsupported_claims`;
//! 5. **drop any candidate whose `sources` came back empty** — it can never
//!    become a [`super::schema::PostCandidate`] (the type rejects it at
//!    deserialization), and this block never emits an untraceable draft;
//! 6. stamp `{candidates, unsupported_claims}` onto `ctx.nodes`.
//!
//! `LinkedInPostPolicy` carries no `prompt_cache` knob (unlike
//! `research_agent`'s), so this node builds one plain per-run prompt rather
//! than splitting a `STABLE_SYSTEM_PROMPT` prefix from a policy-varying
//! body — there is no cache breakpoint to keep run-invariant here.

use engine_contract::TaskContext;
use serde::Deserialize;
use serde_json::json;

use crate::node::{Node, NodeError};
use crate::nodes::ClaudeCodeStep;
use crate::workflows::{get_result, parse_structured_or_fenced, put_result, ModelTransport};

use super::policy::LinkedInPostPolicy;
use super::schema::{LinkedInPostEventSchema, PostCandidate, WorkSource};
use super::work_source;

use claude_code_rs::Config;

/// The `Node::name()` identity `PostDraftNode` runs its composed
/// `ClaudeCodeStep` under, and the `ctx.nodes` key its output is stamped
/// onto.
pub const NODE_NAME: &str = "PostDraftNode";

/// Voice constraints from `agentic-portfolio/business/docs/brand.md`,
/// carried into the prompt per `tasks.md`'s Context Pointers: first person,
/// systems actually built, no metric that cannot be defended on a call, no
/// employer named, Bastion shown and never sold (D56 §4).
const VOICE_CONSTRAINTS: &str = "\
VOICE — write first person, about systems actually built (not hypothetical \
or generic). Never state a metric or number you could not defend if asked \
about it on a call. Never name an employer. Bastion (the practice's own \
agentic-engineering system) may be shown as work in progress, but never \
sold or pitched as a product.";

/// Directive covering the traceability + unsupported-claim-flagging
/// requirement: every candidate must draw only from the supplied
/// `WorkSource` list, and any claim the draft makes that isn't backed by
/// one of those sources must be named in `unsupported_claims` rather than
/// silently emitted or silently dropped.
const TRACEABILITY_DIRECTIVE: &str = "\
TRACEABILITY — every candidate's `sources` must be a non-empty subset of \
the WORK SOURCES listed below (reference each by its `id`). Do not invent \
a source. If a candidate's draft would need to assert something not \
backed by any of the supplied WORK SOURCES (a metric, a claim, a detail), \
do NOT invent or silently omit it — instead add a short description of \
the unsupported claim to the top-level `unsupported_claims` list and keep \
the draft to only what the sources support. A candidate whose `sources` \
would be empty must not be included in `candidates` at all.";

/// JSON schema for `PostDraftNode`'s model reply — a `candidates` array
/// (each with `angle`/`draft`/`sources`) plus a top-level
/// `unsupported_claims` array. `sources` has no schema-level `minItems`
/// floor here (the type-level invariant on [`PostCandidate`] and this
/// node's own filter enforce non-emptiness); the schema only shapes what
/// the model may return, not what this node accepts.
fn draft_response_json_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "candidates": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "angle": { "type": "string" },
                        "draft": { "type": "string" },
                        "sources": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "kind": { "type": "string" },
                                    "id": { "type": "string" },
                                    "summary": { "type": "string" },
                                },
                                "required": ["kind", "id", "summary"],
                            },
                        },
                    },
                    "required": ["angle", "draft", "sources"],
                },
            },
            "unsupported_claims": { "type": "array", "items": { "type": "string" } },
        },
        "required": ["candidates"],
    })
}

/// Render the supplied `WorkSource`s as a compact JSON block for the
/// prompt body.
fn render_sources(sources: &[WorkSource]) -> String {
    serde_json::to_string_pretty(sources).unwrap_or_else(|_| "[]".to_string())
}

/// Build the draft prompt from the gathered `sources` and the requested
/// `candidate_count`.
fn build_prompt(sources: &[WorkSource], candidate_count: u32) -> String {
    format!(
        "You are drafting {candidate_count} LinkedIn post candidates for a solo AI & \
         Automations Engineer, based ONLY on the real work items below.\n\n\
         WORK SOURCES:\n{}\n\n\
         {VOICE_CONSTRAINTS}\n\n\
         {TRACEABILITY_DIRECTIVE}\n\n\
         Respond with strict JSON matching this shape: {{\"candidates\": \
         [{{\"angle\": str, \"draft\": str, \"sources\": [{{\"kind\": str, \
         \"id\": str, \"summary\": str}}]}}], \"unsupported_claims\": [str]}}.",
        render_sources(sources)
    )
}

/// Shadow of the model's reply, parsed before the traceability filter is
/// applied. `sources` is allowed to be empty here (unlike
/// [`PostCandidate`]'s own strict `Deserialize`) so a model reply that
/// violates the directive is filtered out in [`PostDraftNode::process`]
/// rather than failing the whole node.
#[derive(Debug, Clone, Deserialize)]
struct RawCandidate {
    angle: String,
    draft: String,
    #[serde(default)]
    sources: Vec<WorkSource>,
}

#[derive(Debug, Clone, Deserialize)]
struct DraftModelResponse {
    #[serde(default)]
    candidates: Vec<RawCandidate>,
    #[serde(default)]
    unsupported_claims: Vec<String>,
}

/// Read [`work_source::WorkSourceNode`]'s prior result off `ctx.nodes`.
/// Absent (never run, or an empty range) yields an empty `Vec` rather than
/// an error — a run with no sources still completes, it just proposes no
/// candidates.
fn upstream_sources(ctx: &TaskContext) -> Vec<WorkSource> {
    get_result(ctx, work_source::NODE_NAME)
        .and_then(|value| value.get("sources"))
        .and_then(|value| serde_json::from_value::<Vec<WorkSource>>(value.clone()).ok())
        .unwrap_or_default()
}

/// The model node that proposes traceable `PostCandidate`s from the
/// event's gathered `WorkSource`s.
pub struct PostDraftNode {
    config: Config,
    transport: Option<ModelTransport>,
}

impl PostDraftNode {
    /// Construct with the draft `json_schema` set; `process` overwrites
    /// `model` per the resolved `draft`-stage tier.
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: Config {
                json_schema: Some(draft_response_json_schema()),
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
}

impl Default for PostDraftNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for PostDraftNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let event: LinkedInPostEventSchema = serde_json::from_value(ctx.event.clone())
            .map_err(|err| NodeError::new(format!("{NODE_NAME}: invalid event: {err}")))?;
        let policy: LinkedInPostPolicy = crate::policy::resolved_policy_strict(&ctx)?;
        let sources = upstream_sources(&ctx);

        let mut config = self.config.clone();
        config =
            crate::policy::apply_model_tier(config, policy.model_tiers.draft, &policy.local.model);

        let prompt = build_prompt(&sources, event.candidate_count);

        let mut step = ClaudeCodeStep::new(NODE_NAME, config, prompt);
        if let Some(transport) = self.transport.clone() {
            step = step.with_transport(move |config, prompt| (transport)(config, prompt));
        }

        let mut ctx = step.process(ctx).await?;

        let content = ctx
            .nodes
            .get(NODE_NAME)
            .and_then(|value| value.get("content"))
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();

        let parsed: DraftModelResponse = parse_structured_or_fenced(&ctx, NODE_NAME, &content)
            .map_err(|err| {
                NodeError::new(format!(
                    "{NODE_NAME}: failed to parse a draft response from the model's reply: {err}"
                ))
            })?;

        // The traceability invariant, enforced here rather than trusted
        // from the model: a candidate whose sources came back empty is
        // dropped, never emitted.
        let candidates: Vec<PostCandidate> = parsed
            .candidates
            .into_iter()
            .filter(|raw| !raw.sources.is_empty())
            .map(|raw| PostCandidate {
                angle: raw.angle,
                draft: raw.draft,
                sources: raw.sources,
            })
            .collect();

        put_result(
            &mut ctx,
            NODE_NAME,
            json!({
                "candidates": candidates,
                "unsupported_claims": parsed.unsupported_claims,
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
    use std::collections::{BTreeMap, HashMap};

    use claude_code_rs::parse::{ModelUsage as SdkModelUsage, Usage as SdkUsage};
    use claude_code_rs::Outcome;
    use futures::FutureExt;

    use super::super::policy::LinkedInPostPolicy;
    use super::super::schema::WorkSourceKind;
    use super::*;

    fn event() -> LinkedInPostEventSchema {
        LinkedInPostEventSchema {
            since: "2026-08-17".to_string(),
            until: "2026-08-24".to_string(),
            repos: None,
            candidate_count: 3,
            policy: None,
            profile: None,
        }
    }

    fn fixture_sources() -> Vec<WorkSource> {
        vec![
            WorkSource {
                kind: WorkSourceKind::Commit,
                id: "abc123".to_string(),
                summary: "implemented WorkSourceNode".to_string(),
            },
            WorkSource {
                kind: WorkSourceKind::LogEntry,
                id: "2026-08-20-1".to_string(),
                summary: "shipped the brand critic".to_string(),
            },
        ]
    }

    fn ctx_with_sources(event: LinkedInPostEventSchema, sources: Vec<WorkSource>) -> TaskContext {
        let mut ctx = TaskContext {
            event: serde_json::to_value(event).unwrap(),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        };
        ctx.nodes.insert(
            crate::policy::RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(LinkedInPostPolicy::default()).expect("policy serializes"),
        );
        ctx.nodes.insert(
            work_source::NODE_NAME.to_string(),
            json!({ "sources": sources, "message": null }),
        );
        ctx
    }

    fn stub_transport(structured: serde_json::Value) -> ModelTransport {
        std::sync::Arc::new(move |_config: Config, _prompt: String| {
            let structured = structured.clone();
            async move {
                Ok(Outcome {
                    text: serde_json::to_string(&structured).unwrap(),
                    cost_usd: 0.01,
                    usage: SdkUsage {
                        input_tokens: 10,
                        output_tokens: 5,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::from([(
                        "claude-sonnet-4-5".to_string(),
                        SdkModelUsage {
                            input_tokens: 10,
                            output_tokens: 5,
                            cache_read_input_tokens: 0,
                            cache_creation_input_tokens: 0,
                            cost_usd: 0.01,
                        },
                    )]),
                    structured_output: Some(structured),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        })
    }

    fn candidate_json(angle: &str, sources: serde_json::Value) -> serde_json::Value {
        json!({
            "angle": angle,
            "draft": format!("Draft about {angle}"),
            "sources": sources,
        })
    }

    #[tokio::test]
    async fn three_stubbed_candidates_yield_three_candidates_each_with_sources() {
        let src = json!([{"kind": "commit", "id": "abc123", "summary": "did a thing"}]);
        let response = json!({
            "candidates": [
                candidate_json("angle-1", src.clone()),
                candidate_json("angle-2", src.clone()),
                candidate_json("angle-3", src),
            ],
            "unsupported_claims": [],
        });

        let node = PostDraftNode::new().with_transport(stub_transport(response));
        let ctx = ctx_with_sources(event(), fixture_sources());
        let ctx = node.process(ctx).await.expect("process should succeed");

        let candidates = ctx.nodes[NODE_NAME]["candidates"]
            .as_array()
            .expect("candidates array");
        assert_eq!(candidates.len(), 3);
        for candidate in candidates {
            let sources = candidate["sources"].as_array().expect("sources array");
            assert!(!sources.is_empty());
        }
    }

    #[tokio::test]
    async fn model_flagged_unsupported_claim_is_populated() {
        let src = json!([{"kind": "commit", "id": "abc123", "summary": "did a thing"}]);
        let response = json!({
            "candidates": [candidate_json("angle-1", src)],
            "unsupported_claims": ["cut latency by 40% (no supporting WorkSource)"],
        });

        let node = PostDraftNode::new().with_transport(stub_transport(response));
        let ctx = ctx_with_sources(event(), fixture_sources());
        let ctx = node.process(ctx).await.expect("process should succeed");

        let claims = ctx.nodes[NODE_NAME]["unsupported_claims"]
            .as_array()
            .expect("unsupported_claims array");
        assert_eq!(claims.len(), 1);
        assert!(claims[0].as_str().unwrap().contains("cut latency by 40%"));
    }

    #[tokio::test]
    async fn candidate_with_empty_sources_is_dropped_not_emitted() {
        let good_src = json!([{"kind": "commit", "id": "abc123", "summary": "did a thing"}]);
        let response = json!({
            "candidates": [
                candidate_json("good-angle", good_src),
                candidate_json("bad-angle", json!([])),
            ],
            "unsupported_claims": [],
        });

        let node = PostDraftNode::new().with_transport(stub_transport(response));
        let ctx = ctx_with_sources(event(), fixture_sources());
        let ctx = node.process(ctx).await.expect("process should succeed");

        let candidates = ctx.nodes[NODE_NAME]["candidates"]
            .as_array()
            .expect("candidates array");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0]["angle"], json!("good-angle"));
    }

    #[tokio::test]
    async fn all_candidates_dropped_yields_empty_candidates_vec_not_an_error() {
        let response = json!({
            "candidates": [candidate_json("bad-angle", json!([]))],
            "unsupported_claims": [],
        });

        let node = PostDraftNode::new().with_transport(stub_transport(response));
        let ctx = ctx_with_sources(event(), fixture_sources());
        let ctx = node.process(ctx).await.expect("process should succeed");

        let candidates = ctx.nodes[NODE_NAME]["candidates"]
            .as_array()
            .expect("candidates array");
        assert!(candidates.is_empty());
    }

    #[tokio::test]
    async fn applies_draft_stage_model_tier() {
        let policy = LinkedInPostPolicy {
            model_tiers: super::super::policy::ModelTiers {
                draft: super::super::policy::ModelTier::Opus,
                ..LinkedInPostPolicy::default().model_tiers
            },
            ..LinkedInPostPolicy::default()
        };

        let captured: std::sync::Arc<std::sync::Mutex<Option<Config>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured_clone = captured.clone();
        let response = json!({
            "candidates": [candidate_json(
                "angle-1",
                json!([{"kind": "commit", "id": "abc123", "summary": "did a thing"}])
            )],
            "unsupported_claims": [],
        });
        let response_clone = response.clone();
        let transport: ModelTransport = std::sync::Arc::new(move |config, _prompt| {
            *captured_clone.lock().unwrap() = Some(config);
            let response = response_clone.clone();
            async move {
                Ok(Outcome {
                    text: serde_json::to_string(&response).unwrap(),
                    cost_usd: 0.0,
                    usage: SdkUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::new(),
                    structured_output: Some(response),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        });

        let node = PostDraftNode::new().with_transport(transport);
        let mut ctx = ctx_with_sources(event(), fixture_sources());
        ctx.nodes.insert(
            crate::policy::RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(&policy).expect("policy serializes"),
        );
        node.process(ctx).await.expect("process should succeed");

        let config = captured.lock().unwrap().take().expect("transport called");
        assert_eq!(config.model.as_deref(), Some("claude-opus-4-8"));
    }

    #[tokio::test]
    async fn prompt_includes_work_sources_and_voice_constraints() {
        let captured: std::sync::Arc<std::sync::Mutex<Option<String>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured_clone = captured.clone();
        let response = json!({ "candidates": [], "unsupported_claims": [] });
        let response_clone = response.clone();
        let transport: ModelTransport = std::sync::Arc::new(move |_config, prompt| {
            *captured_clone.lock().unwrap() = Some(prompt);
            let response = response_clone.clone();
            async move {
                Ok(Outcome {
                    text: serde_json::to_string(&response).unwrap(),
                    cost_usd: 0.0,
                    usage: SdkUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::new(),
                    structured_output: Some(response),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        });

        let node = PostDraftNode::new().with_transport(transport);
        let ctx = ctx_with_sources(event(), fixture_sources());
        node.process(ctx).await.expect("process should succeed");

        let prompt = captured.lock().unwrap().take().expect("transport called");
        assert!(prompt.contains("abc123"));
        assert!(prompt.contains("first person"));
        assert!(prompt.contains("Never name an employer"));
        assert!(prompt.contains("TRACEABILITY"));
    }

    #[tokio::test]
    async fn no_upstream_sources_yields_empty_candidates_without_error() {
        let response = json!({ "candidates": [], "unsupported_claims": [] });
        let node = PostDraftNode::new().with_transport(stub_transport(response));
        let ctx = ctx_with_sources(event(), Vec::new());
        let ctx = node.process(ctx).await.expect("process should succeed");

        let candidates = ctx.nodes[NODE_NAME]["candidates"]
            .as_array()
            .expect("candidates array");
        assert!(candidates.is_empty());
    }

    #[tokio::test]
    async fn process_errors_when_event_is_invalid() {
        let node = PostDraftNode::new().with_transport(stub_transport(json!({})));
        let ctx = TaskContext {
            event: json!({ "since": 1, "until": 2 }),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("invalid event"));
    }
}
