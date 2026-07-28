//! The declared `WorkflowSchema` / `NodeRegistry` / `registry_for_policy`
//! (Local rewire) / `Workflow` assembly for `PROPOSAL_GENERATOR`.
//! `WORKFLOW_TYPE` lives here (mirrors `research_agent::graph` /
//! `diagnostic_intake::graph` / `sdlc_flow::graph`).
//!
//! Declared graph shape:
//!
//! ```text
//! ProposalCompanyResearchNode -> OpportunityIdentifierNode -> ProposalWriterNode
//!   -> ProposalReviewNode -> ProposalReviewRouterNode
//!   -> { PersistToBrainNode
//!      | ProposalReviseNode -> {loop guard/increment cluster}
//!          -> ProposalReviewNode (continue, capped) | PersistToBrainNode (cap reached) }
//! ```
//!
//! `ProposalReviewRouterNode` is a [`Router`]: it reads `ProposalReviewNode`'s
//! stored verdict off `ctx.nodes` and routes to whichever of
//! `PersistToBrainNode` (pass) / `ProposalReviseNode` (revise) matches — it
//! never mutates `ctx`, so (per the spec's Context Pointers) policy
//! resolution/telemetry live in the model nodes it routes between, not in
//! the router itself. `PersistToBrainNode` is the sole terminal node — no
//! forward connection.
//!
//! ## `EN.5.E` task 4 — the review/revise cluster rides the loop combinator
//!
//! Today's straight-line `ProposalReviseNode -> PersistToBrainNode`
//! fall-through is replaced by [`crate::loop_combinator::build_loop`]'s
//! `{guard router, increment node}` cluster: `ProposalReviseNode`'s single
//! declared connection now points at the cluster's guard, whose back-edge
//! (via the increment node) re-enters `ProposalReviewNode` for another
//! review pass, capped at [`REVISE_LOOP_MAX_ITERATIONS`]. Once
//! `ProposalReviewNode` re-runs, `ProposalReviewRouterNode` re-decides: a
//! `pass` verdict routes straight to `PersistToBrainNode` (bypassing the
//! loop cluster entirely — this is how the loop "exits on a pass verdict"),
//! while a further `revise` verdict re-enters the cluster, up to the cap.
//! The cluster's own [`crate::loop_combinator::LoopSpec::exit_predicate`]
//! is therefore a pure cap backstop (`false`, never short-circuits on its
//! own) — the verdict-based exit is already handled by
//! `ProposalReviewRouterNode` routing around the cluster on `pass`. The
//! guard/increment nodes are both real `Router`s (`as_router().is_some()`),
//! so `WorkflowValidator`'s DFS cycle check skips every edge out of them —
//! the back-edge is a runtime router edge, never a declared non-router
//! cycle (D42).

use std::collections::HashMap;
use std::sync::Arc;

use engine_contract::TaskContext;

use crate::loop_combinator::{build_loop, ExitPredicate, LoopCluster, LoopSpec};
use crate::node::NodeRegistry;
use crate::nodes::openai_compat_transport::openai_compat_transport_live;
use crate::schema::{NodeConfig, WorkflowSchema};
use crate::workflow::Workflow;
use crate::workflows::ModelTransport;

use super::company_research::ProposalCompanyResearchNode;
use super::opportunity_identifier::OpportunityIdentifierNode;
use super::persist_to_brain::PersistToBrainNode;
use super::policy::{ModelTier, ProposalGeneratorPolicy};
use super::review::ProposalReviewNode;
use super::review_router::ProposalReviewRouterNode;
use super::revise::ProposalReviseNode;
use super::writer::ProposalWriterNode;

/// Hard cap on how many times the review->revise cluster may bounce back
/// into `ProposalReviewNode` before it forces an exit straight to
/// `PersistToBrainNode`, regardless of verdict — the backstop against an
/// unbounded `revise` verdict loop.
const REVISE_LOOP_MAX_ITERATIONS: u32 = 3;

/// The identity prefix the review->revise loop cluster's guard/increment
/// nodes are derived from (`"{prefix}Guard"` / `"{prefix}Increment"`).
const REVISE_LOOP_IDENTITY_PREFIX: &str = "ProposalRevision";

/// The [`LoopSpec`] for the review->revise cluster: body entry is
/// `ProposalReviewNode` (the back-edge re-enters review, not revise
/// directly), exit is `PersistToBrainNode`. The predicate never fires on
/// its own — `ProposalReviewRouterNode` already routes a `pass` verdict
/// around the cluster entirely, so `max_iterations` is the cluster's only
/// forced exit.
fn revise_loop_spec() -> LoopSpec {
    let never_exits: ExitPredicate = Arc::new(|_ctx: &TaskContext| false);
    LoopSpec::new(
        REVISE_LOOP_IDENTITY_PREFIX,
        REVISE_LOOP_MAX_ITERATIONS,
        never_exits,
        "ProposalReviewNode",
        "PersistToBrainNode",
    )
}

/// Build the review->revise loop cluster from [`revise_loop_spec`]. `pub`
/// so a hermetic stand-in registry (e.g. `policy_dispatch_e2e.rs`'s
/// `build_hermetic_registry`) can register the same guard/increment nodes
/// [`registry`] does, keeping its node identity set a faithful match for
/// the real one.
#[must_use]
pub fn revise_loop_cluster() -> LoopCluster {
    build_loop(revise_loop_spec())
}

/// The `PROPOSAL_GENERATOR` workflow's declared identity/type name, used
/// both to register the workflow (`engine-serve`) and as
/// `WorkflowSchema::workflow_type`.
pub const WORKFLOW_TYPE: &str = "PROPOSAL_GENERATOR";

/// Build the declared `WorkflowSchema` for the `PROPOSAL_GENERATOR`
/// workflow.
#[must_use]
pub fn schema() -> WorkflowSchema {
    let mut nodes = HashMap::new();

    nodes.insert(
        "ProposalCompanyResearchNode".to_string(),
        NodeConfig::new(
            "ProposalCompanyResearchNode",
            vec!["OpportunityIdentifierNode".to_string()],
        ),
    );
    nodes.insert(
        "OpportunityIdentifierNode".to_string(),
        NodeConfig::new(
            "OpportunityIdentifierNode",
            vec!["ProposalWriterNode".to_string()],
        ),
    );
    nodes.insert(
        "ProposalWriterNode".to_string(),
        NodeConfig::new("ProposalWriterNode", vec!["ProposalReviewNode".to_string()]),
    );
    nodes.insert(
        "ProposalReviewNode".to_string(),
        NodeConfig::new(
            "ProposalReviewNode",
            vec!["ProposalReviewRouterNode".to_string()],
        ),
    );
    nodes.insert(
        "ProposalReviewRouterNode".to_string(),
        NodeConfig::new(
            "ProposalReviewRouterNode",
            vec![
                "PersistToBrainNode".to_string(),
                "ProposalReviseNode".to_string(),
            ],
        ),
    );
    let cluster = revise_loop_cluster();
    nodes.insert(
        "ProposalReviseNode".to_string(),
        NodeConfig::new("ProposalReviseNode", vec![cluster.guard_identity.clone()]),
    );
    nodes.extend(cluster.connections);
    nodes.insert(
        "PersistToBrainNode".to_string(),
        NodeConfig::new("PersistToBrainNode", vec![]),
    );

    WorkflowSchema::new(WORKFLOW_TYPE, "ProposalCompanyResearchNode", nodes)
}

/// Build a fresh `NodeRegistry` with every node identity in [`schema`]
/// registered, each with its default (real-transport/real-`HttpPost`)
/// configuration. Tests build their own registry with stubbed transports
/// instead of calling this directly.
#[must_use]
pub fn registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(ProposalCompanyResearchNode::new()));
    registry.register(Box::new(OpportunityIdentifierNode::new()));
    registry.register(Box::new(ProposalWriterNode::new()));
    registry.register(Box::new(ProposalReviewNode::new()));
    registry.register(Box::new(ProposalReviewRouterNode));
    registry.register(Box::new(ProposalReviseNode::new()));
    registry.register(Box::new(PersistToBrainNode::new()));

    for node in revise_loop_cluster().nodes {
        registry.register(node);
    }

    registry
}

/// The real `claude_code_rs::execute` transport — the cloud fallback a
/// `local`-tier stage's `openai_compat_transport` routes to when its local
/// endpoint is unavailable. Mirrors `sdlc_flow::graph::real_cloud_transport`.
fn real_cloud_transport() -> ModelTransport {
    std::sync::Arc::new(|config, prompt| {
        Box::pin(async move { claude_code_rs::execute(&config, &prompt).await })
    })
}

/// Build a `NodeRegistry` like [`registry`], but with whichever of the three
/// Local-eligible stages — `opportunity` (`OpportunityIdentifierNode`),
/// `review` (`ProposalReviewNode`), `revise` (`ProposalReviseNode`) —
/// `policy` resolves to [`ModelTier::Local`] rewired to route through
/// [`openai_compat_transport_live`] (falling back to the real `claude` CLI
/// transport on any local-endpoint failure). **Never** rewires `research`
/// (`ProposalCompanyResearchNode`) — it wraps `ClaudeCodeStep` with
/// WebSearch/WebFetch tools granted, which a local single-shot endpoint
/// cannot serve (spec Context Pointers). `writer` also never rewires — it
/// is cloud-default per the policy module's docs and carries no `Local`
/// dispatch here. This is the multi-stage analog of
/// `sdlc_flow::graph::registry_for_policy`.
#[must_use]
pub fn registry_for_policy(policy: &ProposalGeneratorPolicy) -> NodeRegistry {
    let mut registry = registry();

    if policy.model_tiers.opportunity == ModelTier::Local {
        registry.register(Box::new(OpportunityIdentifierNode::new().with_transport(
            openai_compat_transport_live(policy.local.clone(), real_cloud_transport()),
        )));
    }

    if policy.model_tiers.review == ModelTier::Local {
        registry.register(Box::new(ProposalReviewNode::new().with_transport(
            openai_compat_transport_live(policy.local.clone(), real_cloud_transport()),
        )));
    }

    if policy.model_tiers.revise == ModelTier::Local {
        registry.register(Box::new(ProposalReviseNode::new().with_transport(
            openai_compat_transport_live(policy.local.clone(), real_cloud_transport()),
        )));
    }

    registry
}

/// Build the runnable `PROPOSAL_GENERATOR` `Workflow`: [`registry`] paired
/// with [`schema`], constructed via `Workflow::new_validated` so assembly
/// fails loudly if the declared graph is not structurally sound.
///
/// # Panics
/// Panics if the declared graph fails `WorkflowValidator::validate` — this
/// would be a programming error in this module, not a runtime condition
/// callers should recover from.
#[must_use]
pub fn workflow() -> Workflow {
    Workflow::new_validated(registry(), schema())
        .expect("PROPOSAL_GENERATOR declared graph must pass WorkflowValidator::validate")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap as StdHashMap;

    use engine_contract::TaskContext;
    use serde_json::json;

    use super::*;
    use crate::routing::Router;
    use crate::validate::WorkflowValidator;
    use crate::workflows::put_result;

    #[test]
    fn schema_passes_validation() {
        let schema = schema();
        let registry = registry();

        WorkflowValidator::validate(&registry, &schema).expect("declared graph should validate");
    }

    #[test]
    fn start_node_is_company_research() {
        assert_eq!(schema().start_node, "ProposalCompanyResearchNode");
    }

    #[test]
    fn workflow_type_is_proposal_generator() {
        assert_eq!(schema().workflow_type, WORKFLOW_TYPE);
    }

    #[test]
    fn registry_contains_all_seven_nodes_plus_the_loop_cluster() {
        let registry = registry();
        let cluster = revise_loop_cluster();

        let expected = [
            "ProposalCompanyResearchNode",
            "OpportunityIdentifierNode",
            "ProposalWriterNode",
            "ProposalReviewNode",
            "ProposalReviewRouterNode",
            "ProposalReviseNode",
            "PersistToBrainNode",
            cluster.guard_identity.as_str(),
            cluster.increment_identity.as_str(),
        ];

        for identity in expected {
            assert!(
                registry.contains(identity),
                "expected registry to contain '{identity}'"
            );
        }
        assert_eq!(registry.len(), expected.len());
    }

    fn ctx_with_verdict(verdict: &str) -> TaskContext {
        let mut ctx = TaskContext {
            event: json!({}),
            nodes: StdHashMap::new(),
            metadata: json!({}),
            node_runs: StdHashMap::new(),
        };
        put_result(
            &mut ctx,
            "ProposalReviewNode",
            json!({ "verdict": verdict }),
        );
        ctx
    }

    #[test]
    fn router_pass_branch_routes_to_persist_to_brain() {
        let router = ProposalReviewRouterNode;
        let ctx = ctx_with_verdict("pass");
        assert_eq!(router.route(&ctx), Some("PersistToBrainNode".to_string()));
    }

    #[test]
    fn router_revise_branch_routes_to_revise_node() {
        let router = ProposalReviewRouterNode;
        let ctx = ctx_with_verdict("revise");
        assert_eq!(router.route(&ctx), Some("ProposalReviseNode".to_string()));
    }

    #[test]
    fn both_router_branches_are_declared_and_registered() {
        let schema = schema();
        let registry = registry();

        let router_config = &schema.nodes["ProposalReviewRouterNode"];
        for branch in ["PersistToBrainNode", "ProposalReviseNode"] {
            assert!(
                router_config.connections.contains(&branch.to_string()),
                "router should declare a connection to '{branch}'"
            );
            assert!(
                registry.contains(branch),
                "branch target '{branch}' should be registered"
            );
        }
    }

    #[test]
    fn registry_for_policy_with_default_policy_matches_plain_registry() {
        let default_registry = registry();
        let policy_registry = registry_for_policy(&ProposalGeneratorPolicy::default());

        assert_eq!(policy_registry.len(), default_registry.len());
        assert!(policy_registry.contains("OpportunityIdentifierNode"));
        assert!(policy_registry.contains("ProposalReviewNode"));
        assert!(policy_registry.contains("ProposalReviseNode"));
    }

    #[test]
    fn registry_for_policy_with_local_tiers_rewires_judgment_stages_but_never_research() {
        let mut policy = ProposalGeneratorPolicy::default();
        policy.model_tiers.opportunity = ModelTier::Local;
        policy.model_tiers.review = ModelTier::Local;
        policy.model_tiers.revise = ModelTier::Local;
        // research is deliberately left at its default (Sonnet) tier — the
        // policy layer never resolves it to Local (spec Context Pointers),
        // and even if it were set to Local here, registry_for_policy has no
        // branch that would rewire ProposalCompanyResearchNode.

        let registry = registry_for_policy(&policy);

        // Rewiring must not change the registry's node count or identity
        // set — only the transport those nodes' composed `ClaudeCodeStep`
        // uses.
        assert_eq!(registry.len(), super::registry().len());
        assert!(registry.contains("OpportunityIdentifierNode"));
        assert!(registry.contains("ProposalReviewNode"));
        assert!(registry.contains("ProposalReviseNode"));
        assert!(registry.contains("ProposalCompanyResearchNode"));
    }

    #[test]
    fn registry_for_policy_never_rewires_research_even_when_every_tier_is_local() {
        // ProposalCompanyResearchNode has no `model_tiers.research == Local`
        // branch in `registry_for_policy` at all — WebSearch/WebFetch tools
        // cannot be served by a local single-shot endpoint (spec Context
        // Pointers). This test documents that by construction: a policy
        // with every tier set to Local, including research, still leaves
        // registry_for_policy with only the three judgment-stage branches
        // to rewire.
        let mut policy = ProposalGeneratorPolicy::default();
        policy.model_tiers.research = ModelTier::Local;
        policy.model_tiers.opportunity = ModelTier::Local;
        policy.model_tiers.review = ModelTier::Local;
        policy.model_tiers.revise = ModelTier::Local;

        let registry = registry_for_policy(&policy);
        assert!(registry.contains("ProposalCompanyResearchNode"));
        assert_eq!(registry.len(), super::registry().len());
    }

    #[test]
    fn declared_graph_has_no_dangling_or_unregistered_identity() {
        let schema = schema();
        let registry = registry();

        for (identity, config) in &schema.nodes {
            assert!(
                registry.contains(identity),
                "declared node '{identity}' is not registered"
            );
            for connection in &config.connections {
                assert!(
                    schema.nodes.contains_key(connection),
                    "'{identity}' declares a connection to unregistered/undeclared '{connection}'"
                );
            }
        }
    }

    #[test]
    fn workflow_builds_without_panicking() {
        let _workflow = workflow();
    }

    // -- EN.5.E task 4: the combinator-backed review->revise loop, driven
    // -- through a real `Workflow::run` walk (not the hand-driven sequence
    // -- `proposal_generator_e2e.rs` uses).

    mod loop_behavior {
        use std::collections::BTreeMap;
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc as StdArc;

        use claude_code_rs::parse::Usage as SdkUsage;
        use claude_code_rs::{Config as ClaudeConfig, Outcome};
        use futures::FutureExt;

        use super::*;
        use crate::nodes::http_post::StubHttpPost;
        use crate::policy::RESOLVED_POLICY_IDENTITY;

        fn outcome(structured: serde_json::Value) -> Outcome {
            Outcome {
                text: serde_json::to_string(&structured).unwrap(),
                cost_usd: 0.0,
                usage: SdkUsage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
                model_usage: BTreeMap::new(),
                structured_output: Some(structured),
                is_error: false,
                api_error_status: None,
            }
        }

        fn constant_stub(structured: serde_json::Value) -> ModelTransport {
            StdArc::new(move |_config: ClaudeConfig, _prompt: String| {
                let structured = structured.clone();
                async move { Ok(outcome(structured)) }.boxed()
            })
        }

        /// A transport that counts its calls (via `counter`) and replies
        /// with `structured`, regardless of prompt/config.
        fn counting_stub(
            structured: serde_json::Value,
            counter: StdArc<AtomicU32>,
        ) -> ModelTransport {
            StdArc::new(move |_config: ClaudeConfig, _prompt: String| {
                counter.fetch_add(1, Ordering::SeqCst);
                let structured = structured.clone();
                async move { Ok(outcome(structured)) }.boxed()
            })
        }

        /// A transport that replies with the verdict at `verdicts[call_index]`,
        /// clamped to the last entry once exhausted — lets a test script a
        /// `revise, revise, ..., pass` (or all-`revise`) sequence across
        /// repeated `ProposalReviewNode` visits.
        fn scripted_verdict_stub(
            verdicts: Vec<&'static str>,
            counter: StdArc<AtomicU32>,
        ) -> ModelTransport {
            let verdicts: StdArc<Vec<String>> =
                StdArc::new(verdicts.into_iter().map(str::to_string).collect());
            StdArc::new(move |_config: ClaudeConfig, _prompt: String| {
                let idx = counter.fetch_add(1, Ordering::SeqCst) as usize;
                let verdict = verdicts
                    .get(idx)
                    .cloned()
                    .unwrap_or_else(|| verdicts.last().cloned().unwrap());
                async move { Ok(outcome(json!({ "verdict": verdict, "notes": "loop test" }))) }
                    .boxed()
            })
        }

        fn company_brief_json() -> serde_json::Value {
            json!({
                "company_name": "Loja da Ana",
                "summary": "Retail SMB tracking orders manually over WhatsApp.",
                "recent_developments": [],
                "pain_points": [],
                "outreach_hooks": [],
                "sources": [],
            })
        }

        fn scored_candidates_json() -> serde_json::Value {
            json!({
                "candidates": [
                    {
                        "name": "WhatsApp order tracking",
                        "frequency": 5.0,
                        "time_cost": 4.0,
                        "buildability": 4.0,
                        "rationale": "Happens daily and is fully manual today.",
                    }
                ]
            })
        }

        fn roadmap_json() -> serde_json::Value {
            json!({
                "situation": {
                    "company_name": "Loja da Ana",
                    "business_type": "retail SMB",
                    "team_size": 4,
                    "painful_workflow_summary": "Orders tracked by scrolling WhatsApp threads.",
                    "candidate_count": 1,
                },
                "candidates": [
                    {
                        "name": "WhatsApp order tracking",
                        "frequency": 5.0,
                        "time_cost": 4.0,
                        "buildability": 4.0,
                        "composite": 4.35,
                        "tier": "quick_win",
                        "rationale": "Happens daily and is fully manual today.",
                    }
                ],
                "top_profiles": [
                    {
                        "name": "WhatsApp order tracking",
                        "today": "Manually scrolled.",
                        "proposed_solution": "Automated bot with human approval gate.",
                        "stack": "WhatsApp Business API + small service.",
                        "rough_scope": "2-3 weeks.",
                        "expected_roi": "Saves ~5 hrs/week.",
                    }
                ],
                "recommendation": {
                    "start_with": "WhatsApp order tracking",
                    "phase_1_scope": ["Order intake bot"],
                    "how_it_works": "Connects to WhatsApp Business API.",
                    "call_to_action": "Book a call to proceed.",
                },
            })
        }

        /// Assembles the real `PROPOSAL_GENERATOR` `Workflow` — [`schema`] +
        /// [`registry`]'s shape, but with every model node's transport
        /// stubbed and `PersistToBrainNode`'s `HttpPost` stubbed — and seeds
        /// the resolved policy the way `engine-serve`'s dispatch factory
        /// would (EN.5.D task 8), so `Workflow::run` can drive it directly.
        #[allow(clippy::too_many_arguments)]
        fn build_workflow(
            review_transport: ModelTransport,
            revise_transport: ModelTransport,
            http_post: StubHttpPost,
        ) -> Workflow {
            let mut registry = NodeRegistry::new();
            registry.register(Box::new(
                ProposalCompanyResearchNode::new()
                    .with_transport(constant_stub(company_brief_json())),
            ));
            registry.register(Box::new(
                OpportunityIdentifierNode::new()
                    .with_transport(constant_stub(scored_candidates_json())),
            ));
            registry.register(Box::new(
                ProposalWriterNode::new().with_transport(constant_stub(roadmap_json())),
            ));
            registry.register(Box::new(
                ProposalReviewNode::new().with_transport(review_transport),
            ));
            registry.register(Box::new(ProposalReviewRouterNode));
            registry.register(Box::new(
                ProposalReviseNode::new().with_transport(revise_transport),
            ));
            registry.register(Box::new(
                PersistToBrainNode::new()
                    .with_http_post(StdArc::new(http_post))
                    .with_url("https://brain.example/ingest/proposal-generator-loop-test"),
            ));
            for node in revise_loop_cluster().nodes {
                registry.register(node);
            }

            let workflow = Workflow::new_validated(registry, schema())
                .expect("assembled review->revise cluster should validate");

            let mut seeded = StdHashMap::new();
            seeded.insert(
                RESOLVED_POLICY_IDENTITY.to_string(),
                serde_json::to_value(ProposalGeneratorPolicy::default())
                    .expect("policy serializes"),
            );
            workflow.with_seeded_nodes(seeded)
        }

        #[tokio::test]
        async fn revise_loop_stops_at_max_iterations_when_verdict_never_passes() {
            let review_calls = StdArc::new(AtomicU32::new(0));
            let review_transport = scripted_verdict_stub(vec!["revise"], review_calls.clone());
            let revise_calls = StdArc::new(AtomicU32::new(0));
            let revise_transport = counting_stub(roadmap_json(), revise_calls.clone());
            let http_post = StubHttpPost::succeeding(json!({ "ok": true }));

            let workflow = build_workflow(review_transport, revise_transport, http_post.clone());

            let ctx = workflow
                .run(json!({ "company_name": "Loja da Ana" }), Box::new(|_| {}))
                .await
                .expect("run should succeed");

            // Every visit says "revise", so the guard only ever exits via
            // the cap — `ProposalReviewNode`/`ProposalReviseNode` each run
            // exactly `REVISE_LOOP_MAX_ITERATIONS` times before the guard
            // forces the exit to `PersistToBrainNode`.
            assert_eq!(
                review_calls.load(Ordering::SeqCst),
                REVISE_LOOP_MAX_ITERATIONS
            );
            assert_eq!(
                revise_calls.load(Ordering::SeqCst),
                REVISE_LOOP_MAX_ITERATIONS
            );
            assert!(ctx.nodes.contains_key("PersistToBrainNode"));
            assert!(http_post.last_call().is_some());
        }

        #[tokio::test]
        async fn revise_loop_exits_early_on_a_pass_verdict_before_the_cap() {
            let review_calls = StdArc::new(AtomicU32::new(0));
            // "revise" on the first review, "pass" on the second — the loop
            // must exit as soon as the router sees "pass" (well before
            // `REVISE_LOOP_MAX_ITERATIONS`), routing straight to
            // `PersistToBrainNode` and never revisiting the guard again.
            let review_transport =
                scripted_verdict_stub(vec!["revise", "pass"], review_calls.clone());
            let revise_calls = StdArc::new(AtomicU32::new(0));
            let revise_transport = counting_stub(roadmap_json(), revise_calls.clone());
            let http_post = StubHttpPost::succeeding(json!({ "ok": true }));

            let workflow = build_workflow(review_transport, revise_transport, http_post.clone());

            let ctx = workflow
                .run(json!({ "company_name": "Loja da Ana" }), Box::new(|_| {}))
                .await
                .expect("run should succeed");

            assert_eq!(review_calls.load(Ordering::SeqCst), 2);
            assert_eq!(revise_calls.load(Ordering::SeqCst), 1);
            assert!(ctx.nodes.contains_key("PersistToBrainNode"));
            const { assert!(REVISE_LOOP_MAX_ITERATIONS > 2, "cap should not be hit") };
        }
    }
}
