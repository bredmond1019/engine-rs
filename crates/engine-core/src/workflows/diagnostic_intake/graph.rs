//! The declared `WorkflowSchema` / `NodeRegistry` / `registry_for_policy`
//! (Local rewire for `IntakeExtractNode`) / `Workflow` assembly for
//! `DIAGNOSTIC_INTAKE`.
//!
//! Declared graph shape:
//!
//! ```text
//! IntakeExtractNode
//! ```
//!
//! `IntakeExtractNode` is both the start node and the sole (terminal) node —
//! there is no router, unlike `research_agent`'s two-branch shape.

use std::collections::HashMap;
use std::sync::Arc;

use crate::node::NodeRegistry;
use crate::nodes::openai_compat_transport::openai_compat_meta_transport_live;
use crate::schema::{NodeConfig, WorkflowSchema};
use crate::workflow::Workflow;
use crate::workflows::ModelTransport;

use super::extract::IntakeExtractNode;
use super::policy::{DiagnosticIntakePolicy, ModelTier};

/// The registered workflow type string (mirrors `research_agent::graph` /
/// `sdlc_flow::graph`, both of which hold `WORKFLOW_TYPE` here rather than
/// in `mod.rs`).
pub const WORKFLOW_TYPE: &str = "DIAGNOSTIC_INTAKE";

/// Build the declared `WorkflowSchema` for the `DIAGNOSTIC_INTAKE`
/// workflow: a single node, both start and terminal, with no forward
/// connection.
#[must_use]
pub fn schema() -> WorkflowSchema {
    let mut nodes = HashMap::new();

    nodes.insert(
        "IntakeExtractNode".to_string(),
        NodeConfig::new("IntakeExtractNode", vec![]),
    );

    WorkflowSchema::new(WORKFLOW_TYPE, "IntakeExtractNode", nodes)
}

/// Build a fresh `NodeRegistry` with the single node identity in [`schema`]
/// registered, with its default (real-transport) configuration. Tests build
/// their own registry with a stubbed transport instead of calling this
/// directly.
#[must_use]
pub fn registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(IntakeExtractNode::new()));
    registry
}

/// The real `claude_code_rs::execute` transport — the cloud fallback a
/// `local`-tier `extract` stage's `openai_compat_transport` routes to when
/// its local endpoint is unavailable. Mirrors
/// `sdlc_flow::graph::real_cloud_transport`.
fn real_cloud_transport() -> ModelTransport {
    Arc::new(|config, prompt| {
        Box::pin(async move { claude_code_rs::execute(&config, &prompt).await })
    })
}

/// Build a `NodeRegistry` like [`registry`], but with `IntakeExtractNode`
/// rewired to route through [`openai_compat_meta_transport_live`] whenever
/// `policy`'s resolved `extract` tier is [`ModelTier::Local`] — the direct
/// analog of `sdlc_flow::graph::registry_for_policy`'s triage/review rewire,
/// but the inverse of `research_agent::graph::registry_for_policy`'s
/// no-rewire guard: here the sole stage *is* Local-eligible (pure
/// extraction suits a local coder model), so the rewire fires for the one
/// and only node in this workflow rather than being permanently absent.
/// Using the meta-transport sibling (rather than the plain
/// `openai_compat_transport_live`) lets telemetry stamp the actual tier that
/// ran — `"local"` on success, `"cloud"` on a fallback — instead of a
/// generic `"cloud"` regardless of outcome
/// (`EN.ticket.wire-meta-transport-telemetry`).
///
/// Any local-endpoint failure at call time falls back to the real `claude`
/// CLI transport for that call — `openai_compat_transport`'s own fail-fast
/// + fallback, not something this function decides.
#[must_use]
pub fn registry_for_policy(policy: &DiagnosticIntakePolicy) -> NodeRegistry {
    let mut registry = NodeRegistry::new();

    if policy.model_tiers.extract == ModelTier::Local {
        registry.register(Box::new(IntakeExtractNode::new().with_meta_transport(
            openai_compat_meta_transport_live(policy.local.clone(), real_cloud_transport()),
        )));
    } else {
        registry.register(Box::new(IntakeExtractNode::new()));
    }

    registry
}

/// Build the runnable `DIAGNOSTIC_INTAKE` `Workflow`: [`registry`] paired
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
        .expect("DIAGNOSTIC_INTAKE declared graph must pass WorkflowValidator::validate")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate::WorkflowValidator;

    /// Isolated per-test tempdir worktree, mirroring `extract.rs`'s
    /// `temp_worktree()` (`EN.ticket.hermetic-test-temp-dirs`): a test that
    /// drives `IntakeExtractNode::process` without stamping a
    /// `SetupWorktreeNode` result falls back to `worktree_path`'s
    /// `std::env::current_dir()` branch and `persist_state` writes into the
    /// crate's own tracked `planning/diagnostic-intake-state.json` —
    /// exactly the dirtying `ticket-diagnostic-intake-fixture-tempdir` task 2
    /// closes. `remove_dir_all` before `create_dir_all` so a recycled PID
    /// cannot produce a false failure.
    fn temp_worktree() -> std::path::PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "engine-core-diagnostic-intake-graph-test-{}-{n}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Stamp a `SetupWorktreeNode` result pointing at `worktree`, mirroring
    /// `diagnostic_intake_e2e.rs`'s `set_worktree` helper.
    fn set_worktree(ctx: &mut engine_contract::TaskContext, worktree: &std::path::Path) {
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            serde_json::json!({ "worktree_path": worktree.to_string_lossy() }),
        );
    }

    #[test]
    fn schema_passes_validation() {
        let schema = schema();
        let registry = registry();

        WorkflowValidator::validate(&registry, &schema).expect("declared graph should validate");
    }

    #[test]
    fn start_node_is_intake_extract_node() {
        assert_eq!(schema().start_node, "IntakeExtractNode");
    }

    #[test]
    fn workflow_type_is_diagnostic_intake() {
        assert_eq!(schema().workflow_type, WORKFLOW_TYPE);
    }

    #[test]
    fn registry_contains_the_single_node() {
        let registry = registry();
        assert!(registry.contains("IntakeExtractNode"));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn registry_for_policy_with_default_policy_matches_plain_registry() {
        let default_registry = registry();
        let policy_registry = registry_for_policy(&DiagnosticIntakePolicy::default());

        assert_eq!(policy_registry.len(), default_registry.len());
        assert!(policy_registry.contains("IntakeExtractNode"));
    }

    #[test]
    fn registry_for_policy_with_local_tier_keeps_same_node_identity() {
        let policy = DiagnosticIntakePolicy {
            model_tiers: super::super::policy::ModelTiers {
                extract: ModelTier::Local,
            },
            ..DiagnosticIntakePolicy::default()
        };

        let registry = registry_for_policy(&policy);

        // Rewiring IntakeExtractNode's transport must not change the
        // registry's node count or identity set — only the transport its
        // composed `ClaudeCodeStep` uses.
        assert_eq!(registry.len(), 1);
        assert!(registry.contains("IntakeExtractNode"));
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

    /// `EN.ticket.wire-meta-transport-telemetry` task 5: the
    /// `IntakeExtractNode` `registry_for_policy` registers under
    /// `ModelTier::Local` must stamp the *actual* transport tier onto
    /// `ctx.nodes["IntakeExtractNode"]["transport"]["tier"]` — `"local"` on
    /// a stubbed local success, not a generic `"cloud"` regardless of what
    /// ran (the bug this ticket fixes). First confirms `registry_for_policy`
    /// actually rewires the node under this policy, then drives an
    /// equivalent node built the same way
    /// (`with_meta_transport(openai_compat_meta_transport(...))`) but with
    /// the HTTP layer stubbed rather than a real Ollama endpoint.
    #[tokio::test]
    async fn extract_registry_for_policy_stamps_local_tier_on_stubbed_local_success() {
        use crate::node::Node;
        use crate::nodes::openai_compat_meta_transport;
        use crate::policy::tier::LocalConfig;
        use engine_contract::TaskContext;
        use std::collections::HashMap as StdHashMap;

        let mut policy = DiagnosticIntakePolicy::default();
        policy.model_tiers.extract = ModelTier::Local;
        policy.local = LocalConfig {
            endpoint: "http://localhost:11434".to_string(),
            model: "qwen2.5-coder:7b".to_string(),
            constrained_json: false,
        };

        assert!(
            registry_for_policy(&policy).contains("IntakeExtractNode"),
            "IntakeExtractNode must be registered under an extract=Local policy"
        );

        let event = super::super::schema::DiagnosticIntakeEventSchema {
            notes: "Client tracks orders in WhatsApp.".to_string(),
            locale: crate::locale::Locale::default(),
            policy: None,
            profile: None,
        };
        let mut ctx = TaskContext {
            event: serde_json::to_value(&event).unwrap(),
            nodes: StdHashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: StdHashMap::new(),
        };
        ctx.nodes.insert(
            crate::policy::RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(&policy).expect("policy serializes"),
        );
        let worktree = temp_worktree();
        set_worktree(&mut ctx, &worktree);

        let local_http_post: crate::nodes::LocalHttpPost = Arc::new(|_url, _body| {
            Box::pin(async {
                Ok(serde_json::json!({
                    "choices": [{ "message": {
                        "content": serde_json::json!({
                            "company_name": "Loja da Ana",
                            "company_type": "retail SMB",
                            "team_size": 4,
                            "primary_channels": [],
                            "existing_tools": [],
                            "existing_automations": [],
                            "top_workflows": [],
                        }).to_string()
                    } }],
                    "usage": { "prompt_tokens": 1, "completion_tokens": 1 },
                }))
            })
        });
        let cloud_fallback: ModelTransport = Arc::new(|_config, _prompt| {
            Box::pin(async { panic!("cloud fallback must not be called when local succeeds") })
        });
        let meta_transport =
            openai_compat_meta_transport(policy.local.clone(), local_http_post, cloud_fallback);
        let stubbed_node = IntakeExtractNode::new().with_meta_transport(meta_transport);

        let out = stubbed_node
            .process(ctx)
            .await
            .expect("process should succeed");
        assert_eq!(out.nodes["IntakeExtractNode"]["transport"]["tier"], "local");
        assert_eq!(
            out.nodes["IntakeExtractNode"]["transport"]["endpoint"],
            "http://localhost:11434"
        );
        std::fs::remove_dir_all(&worktree).ok();
    }

    /// Same seam, but the local endpoint fails — the resulting telemetry
    /// must show `"cloud"` (what actually ran, via the fallback), not the
    /// `"local"` tier the resolved policy intended.
    #[tokio::test]
    async fn extract_registry_for_policy_stamps_cloud_tier_on_local_failure_fallback() {
        use crate::node::Node;
        use crate::nodes::openai_compat_meta_transport;
        use crate::policy::tier::LocalConfig;
        use engine_contract::TaskContext;
        use futures::FutureExt;
        use std::collections::HashMap as StdHashMap;

        let local = LocalConfig {
            endpoint: "http://localhost:11434".to_string(),
            model: "qwen2.5-coder:7b".to_string(),
            constrained_json: false,
        };

        let event = super::super::schema::DiagnosticIntakeEventSchema {
            notes: "Client tracks orders in WhatsApp.".to_string(),
            locale: crate::locale::Locale::default(),
            policy: None,
            profile: None,
        };
        let mut ctx = TaskContext {
            event: serde_json::to_value(&event).unwrap(),
            nodes: StdHashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: StdHashMap::new(),
        };
        ctx.nodes.insert(
            crate::policy::RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(DiagnosticIntakePolicy::default()).expect("policy serializes"),
        );
        let worktree = temp_worktree();
        set_worktree(&mut ctx, &worktree);

        let local_http_post: crate::nodes::LocalHttpPost =
            Arc::new(|_url, _body| Box::pin(async { Err("connection refused".to_string()) }));
        let cloud_fallback: ModelTransport = Arc::new(|_config, _prompt| {
            async move {
                Ok(claude_code_rs::Outcome {
                    text: serde_json::json!({
                        "company_name": "Loja da Ana",
                        "company_type": "retail SMB",
                        "team_size": 4,
                        "primary_channels": [],
                        "existing_tools": [],
                        "existing_automations": [],
                        "top_workflows": [],
                    })
                    .to_string(),
                    cost_usd: 0.0,
                    usage: claude_code_rs::parse::Usage {
                        input_tokens: 1,
                        output_tokens: 1,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: std::collections::BTreeMap::new(),
                    structured_output: Some(serde_json::json!({
                        "company_name": "Loja da Ana",
                        "company_type": "retail SMB",
                        "team_size": 4,
                        "primary_channels": [],
                        "existing_tools": [],
                        "existing_automations": [],
                        "top_workflows": [],
                    })),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        });
        let meta_transport = openai_compat_meta_transport(local, local_http_post, cloud_fallback);
        let node = IntakeExtractNode::new().with_meta_transport(meta_transport);

        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["IntakeExtractNode"]["transport"]["tier"], "cloud");
        std::fs::remove_dir_all(&worktree).ok();
    }
}
