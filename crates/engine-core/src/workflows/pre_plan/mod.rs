//! `PRE_PLAN` — an inbound idea becomes a researched
//! `$BRAIN_ROOT/planning/open-work/pre-plan/<slug>/notes.md`, with no human
//! back-and-forth (`EN.19.A`).
//!
//! Composed of atomic, independently model-tier-routable nodes so a later
//! composing workflow (`EN.19.D`) can wire this same node set into a larger
//! graph rather than only reaching it through the standalone `PRE_PLAN`
//! workflow_type — no node here may assume it is the first node of a run.
//!
//! Module layout (source of truth for exact shapes: `planning/EN.19.A.json`
//! and `planning/EN.19.A/tasks.json`):
//! - `check_existing` — `CheckExistingNotesNode`, the idempotency guard: no
//!   model call, `exists()`-checks the target `notes.md` before any research
//!   runs and short-circuits unless `force_regenerate: true` (task 1).
//! - `intake` — `IntakeIdeaNode`, normalizes the dispatched event's idea
//!   text/slug/channel metadata into `TaskContext`; no model call (task 1).
//! - `research` — `ResearchCodebaseNode`, a read-only `AgentCodeStep`
//!   session scoped to Read/Grep/Glob (task 2).
//! - `write_notes` — `WriteNotesNode`, renders the research findings into
//!   `notes.md` matching `.claude/commands/capture.md`'s output shape
//!   (task 3).
//!
//! Graph assembly, `PrePlanNotesAlreadyExistsNode`'s short-circuit terminal,
//! the `PrePlanPolicy`/`PartialPrePlanPolicy` run-policy surface (research
//! node model tier + the PRE_PLAN kill switch, standing rule 12), and the
//! `WORKFLOW_TYPE`/`schema`/`registry`/`registry_for_policy` assembly land
//! here (task 4).
//!
//! Declared graph shape:
//!
//! ```text
//! CheckExistingNotesNode -> { PrePlanNotesAlreadyExistsNode | IntakeIdeaNode -> ResearchCodebaseNode -> WriteNotesNode }
//! ```
//!
//! `CheckExistingNotesNode` is the start node and a `Router` (see
//! `check_existing`'s own doc comment): it stamps its own verdict in
//! `process()`, then `route()` reads that verdict back and picks one of the
//! two declared connections. `PrePlanNotesAlreadyExistsNode` is a short,
//! no-model terminal that reads `CheckExistingNotesNode`'s stamped
//! `notes_path` and re-reports it under its own identity, so the webhook
//! route (task 5) has one place to read "the run short-circuited, here is
//! the existing path" regardless of which route fired.
//!
//! **Policy (standing rule 12 / rule 6).** `PrePlanPolicy` carries exactly
//! two knobs today: `enabled` (the newly-shipped externally-triggerable
//! route's kill switch — built-in default `false`, so a deployment that has
//! never set a `profile`/`policy` override on a dispatched event gets no
//! run at all) and `research_model_tier` (threaded onto the registered
//! `ResearchCodebaseNode` via `llm_node::resolve_meta_transport` + `wire`,
//! per standing rule 11 — never a hand-rolled transport field, and never a
//! rewire: the node stays registered under the same identity at every
//! tier). Resolution mirrors `research_agent::policy`/`profiles`: four
//! layers (event `policy` override > event `profile` > `harness.json`
//! `pre_plan.policy` defaults > built-in default) via the shared
//! `crate::policy` plumbing. `PRE_PLAN`'s inbound event has no dedicated
//! typed schema yet (task 1's `IntakeIdeaNode` validates only `idea`/
//! `slug`), so `resolve_policy_for_run_from` reads the optional
//! `profile`/`policy` fields directly off `ctx.event`'s raw JSON rather than
//! through a `ResearchAgentEventSchema`-shaped struct.

use std::collections::HashMap;

use engine_contract::TaskContext;
use serde::{Deserialize, Serialize};

use crate::node::{Node, NodeError, NodeRegistry};
use crate::policy::{
    merge_local, merge_opt, resolve as policy_resolve, AgentBackend, LocalConfig, ModelTier,
    PartialLocalConfig, Policy, PolicyConfigSource,
};
use crate::schema::{NodeConfig, WorkflowSchema};
use crate::workflow::Workflow;
use crate::workflows::llm_node::{resolve_meta_transport, wire};
use crate::workflows::{get_result, put_result};

pub mod check_existing;
pub mod intake;
pub mod research;
pub mod write_notes;

/// The `PRE_PLAN` workflow's declared identity/type name, used both to
/// register the workflow (`engine-serve`) and as `WorkflowSchema::workflow_type`.
pub const WORKFLOW_TYPE: &str = "PRE_PLAN";

/// The `harness.json` section key this workflow's policy/profiles live
/// under (`pre_plan.policy` / `pre_plan.profiles`).
const WORKFLOW_KEY: &str = "pre_plan";

/// Short, no-model terminal reached only via
/// `check_existing::CheckExistingNotesNode`'s [`check_existing::EXISTS_ROUTE`].
/// Reads back `CheckExistingNotesNode`'s stamped `notes_path` and re-reports
/// it under its own identity (== `check_existing::EXISTS_ROUTE`), so the
/// webhook route (task 5) always finds the reported path under this node's
/// name regardless of which branch fired.
pub struct PrePlanNotesAlreadyExistsNode;

impl PrePlanNotesAlreadyExistsNode {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for PrePlanNotesAlreadyExistsNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for PrePlanNotesAlreadyExistsNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let notes_path = get_result(&ctx, check_existing::NODE_NAME)
            .and_then(|value| value.get("notes_path"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                NodeError::new(format!(
                    "{}: {} has not run yet",
                    check_existing::EXISTS_ROUTE,
                    check_existing::NODE_NAME
                ))
            })?;

        put_result(
            &mut ctx,
            check_existing::EXISTS_ROUTE,
            serde_json::json!({
                "notes_path": notes_path,
                "already_exists": true,
            }),
        );

        Ok(ctx)
    }

    fn name(&self) -> &str {
        check_existing::EXISTS_ROUTE
    }
}

/// The fully-resolved, per-run `PRE_PLAN` policy — the merge of built-in
/// defaults, `harness.json`'s `pre_plan.policy` defaults, a named
/// `profile`, and any per-run event override, high->low precedence in that
/// order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrePlanPolicy {
    /// The PRE_PLAN kill switch (standing rule 12). `false` is the built-in
    /// default — a dispatched event with no `profile`/`policy` override
    /// gets no run at all, by design, until a profile or override turns
    /// this on.
    pub enabled: bool,
    /// The research node's cloud model tier, threaded through
    /// `llm_node::resolve_meta_transport` (standing rule 11) rather than a
    /// hand-rolled field on `ResearchCodebaseNode`.
    pub research_model_tier: ModelTier,
    /// Configuration for the `local` model tier — carried for API-shape
    /// parity with every other workflow's policy; `resolve_meta_transport`
    /// only consults it when `research_model_tier` resolves to
    /// `ModelTier::Local`.
    pub local: LocalConfig,
    /// The research node's [`AgentBackend`], threaded through
    /// `llm_node::resolve_meta_transport` (standing rule 11) rather than the
    /// hand-rolled `ClaudeCli`-literal `AgentBackend` value
    /// `registry_for_policy` used to pass directly. `ClaudeCli` (the
    /// built-in [`AgentBackend`] default) matches today's only-ever-behavior,
    /// so introducing this field is behavior-stable.
    pub research_backend: AgentBackend,
}

impl Default for PrePlanPolicy {
    /// Kill-switch default (standing rule 12): disabled until a profile or
    /// override explicitly turns PRE_PLAN on. `research_model_tier`
    /// defaults to `Sonnet`, matching `research::ResearchCodebaseNode`'s own
    /// pre-policy default model. `research_backend` defaults to
    /// [`AgentBackend`]'s own `ClaudeCli` default, matching today's
    /// only-ever-behavior.
    fn default() -> Self {
        Self {
            enabled: false,
            research_model_tier: ModelTier::Sonnet,
            local: LocalConfig::default(),
            research_backend: AgentBackend::default(),
        }
    }
}

/// All-optional mirror of [`PrePlanPolicy`] used by the override layers
/// (`harness.json`'s `pre_plan.policy`, a named `profile`, and a per-run
/// event's `policy` field). Every field left `None` falls through to the
/// next-lower-precedence layer.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialPrePlanPolicy {
    pub enabled: Option<bool>,
    pub research_model_tier: Option<ModelTier>,
    pub local: Option<PartialLocalConfig>,
    pub research_backend: Option<AgentBackend>,
}

impl Policy for PrePlanPolicy {
    type Partial = PartialPrePlanPolicy;

    /// Apply one override layer on top of `self`, field-by-field (`Some` in
    /// `over` wins, `None` falls through to `self`).
    fn apply(self, over: &PartialPrePlanPolicy) -> Self {
        let base = self;
        PrePlanPolicy {
            enabled: merge_opt(base.enabled, over.enabled),
            research_model_tier: merge_opt(base.research_model_tier, over.research_model_tier),
            local: match &over.local {
                Some(l) => merge_local(base.local, l),
                None => base.local,
            },
            research_backend: merge_opt(base.research_backend, over.research_backend),
        }
    }
}

/// The explicit, "normal operation" profile: PRE_PLAN turned on, Sonnet
/// research. Spelled out explicitly (rather than left all-`None`) so
/// selecting `profile: "baseline"` is a legible, self-documenting override
/// of the built-in disabled-by-default policy.
#[must_use]
pub fn baseline() -> PartialPrePlanPolicy {
    PartialPrePlanPolicy {
        enabled: Some(true),
        research_model_tier: Some(ModelTier::Sonnet),
        research_backend: Some(AgentBackend::default()),
        ..Default::default()
    }
}

/// Cheapest/fastest profile: PRE_PLAN on, Haiku research.
#[must_use]
pub fn cheap_fast() -> PartialPrePlanPolicy {
    PartialPrePlanPolicy {
        enabled: Some(true),
        research_model_tier: Some(ModelTier::Haiku),
        research_backend: Some(AgentBackend::default()),
        ..Default::default()
    }
}

/// Highest-quality profile: PRE_PLAN on, Opus research.
#[must_use]
pub fn thorough() -> PartialPrePlanPolicy {
    PartialPrePlanPolicy {
        enabled: Some(true),
        research_model_tier: Some(ModelTier::Opus),
        research_backend: Some(AgentBackend::default()),
        ..Default::default()
    }
}

/// Resolve a built-in profile bundle by its kebab-case name. Returns `None`
/// for any name that isn't one of the three canonical profiles.
#[must_use]
pub fn profile_by_name(name: &str) -> Option<PartialPrePlanPolicy> {
    match name {
        "baseline" => Some(baseline()),
        "cheap-fast" => Some(cheap_fast()),
        "thorough" => Some(thorough()),
        _ => None,
    }
}

/// Resolve the four policy layers into one concrete [`PrePlanPolicy`],
/// high->low precedence: the event's inline `policy` override beats its
/// `profile` beats `source`'s `pre_plan.policy` defaults beats the built-in
/// default. `PRE_PLAN`'s event carries no dedicated typed schema yet, so
/// `profile`/`policy` are read directly off `ctx.event`'s raw JSON.
pub fn resolve_policy_for_run_from(
    ctx: &TaskContext,
    source: &PolicyConfigSource,
) -> Result<PrePlanPolicy, NodeError> {
    let profile_name = ctx.event.get("profile").and_then(serde_json::Value::as_str);
    let event_override: Option<PartialPrePlanPolicy> = match ctx.event.get("policy") {
        Some(value) if !value.is_null() => {
            Some(serde_json::from_value(value.clone()).map_err(|err| {
                NodeError::new(format!("invalid PRE_PLAN policy override: {err}"))
            })?)
        }
        _ => None,
    };
    let harness_defaults = crate::policy::read_harness_policy_defaults_from(source, WORKFLOW_KEY)?;
    let profile =
        crate::policy::resolve_profile_from(profile_name, source, WORKFLOW_KEY, profile_by_name)?;
    Ok(policy_resolve(
        PrePlanPolicy::default(),
        harness_defaults.as_ref(),
        profile.as_ref(),
        event_override.as_ref(),
    ))
}

/// Build the declared `WorkflowSchema` for the `PRE_PLAN` workflow.
#[must_use]
pub fn schema() -> WorkflowSchema {
    let mut nodes = HashMap::new();

    nodes.insert(
        check_existing::NODE_NAME.to_string(),
        NodeConfig::new(
            check_existing::NODE_NAME,
            vec![
                check_existing::EXISTS_ROUTE.to_string(),
                check_existing::CONTINUE_ROUTE.to_string(),
            ],
        ),
    );
    nodes.insert(
        check_existing::EXISTS_ROUTE.to_string(),
        NodeConfig::new(check_existing::EXISTS_ROUTE, vec![]),
    );
    nodes.insert(
        intake::NODE_NAME.to_string(),
        NodeConfig::new(intake::NODE_NAME, vec![research::NODE_NAME.to_string()]),
    );
    nodes.insert(
        research::NODE_NAME.to_string(),
        NodeConfig::new(
            research::NODE_NAME,
            vec![write_notes::NODE_NAME.to_string()],
        ),
    );
    nodes.insert(
        write_notes::NODE_NAME.to_string(),
        NodeConfig::new(write_notes::NODE_NAME, vec![]),
    );

    WorkflowSchema::new(WORKFLOW_TYPE, check_existing::NODE_NAME, nodes)
}

/// Build a fresh `NodeRegistry` with every node identity in [`schema`]
/// registered, each with its default (real-transport, built-in-model)
/// configuration. Tests/callers wanting a policy-rewired `ResearchCodebaseNode`
/// call [`registry_for_policy`] instead.
#[must_use]
pub fn registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(check_existing::CheckExistingNotesNode::new()));
    registry.register(Box::new(PrePlanNotesAlreadyExistsNode::new()));
    registry.register(Box::new(intake::IntakeIdeaNode::new()));
    registry.register(Box::new(research::ResearchCodebaseNode::new()));
    registry.register(Box::new(write_notes::WriteNotesNode::new()));
    registry
}

/// Build a `NodeRegistry` with [`registry`]'s node set, unchanged, for the
/// given resolved `policy`. Per standing rule 11, `ResearchCodebaseNode`'s
/// transport override is resolved through the shared
/// `llm_node::resolve_meta_transport`/`wire` seam rather than a hand-rolled
/// per-node model field — `policy.research_backend` at its `ClaudeCli`
/// default and any non-`Local` tier is a documented no-op (the node keeps
/// its own default cloud transport); `Pi`/`Aider` always route local
/// regardless of tier. The node SET is INVARIANT across every policy setting (standing
/// rule 6) — this never swaps which identities are registered, only
/// `ResearchCodebaseNode`'s own transport.
#[must_use]
pub fn registry_for_policy(policy: &PrePlanPolicy) -> NodeRegistry {
    let mut registry = registry();
    let transport = resolve_meta_transport(
        policy.research_model_tier,
        policy.research_backend,
        &policy.local,
        &crate::policy::PiConfig::default(),
    );
    registry.register(Box::new(wire(
        research::ResearchCodebaseNode::new(),
        transport,
        None,
    )));
    registry
}

/// Build the runnable `PRE_PLAN` `Workflow`: [`registry`] paired with
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
        .expect("PRE_PLAN declared graph must pass WorkflowValidator::validate")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap as StdHashMap;
    use std::path::Path;
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::validate::WorkflowValidator;

    // `ENGINE_BRAIN_ROOT` is process-global state — guard every test in this
    // module that touches it so it cannot race the rest of the suite (same
    // pattern as `check_existing`/`write_notes`'s own test modules).
    static ENV_GUARD: Mutex<()> = Mutex::new(());

    struct BrainRootGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        previous: Option<String>,
    }

    impl BrainRootGuard {
        fn set(root: &Path) -> Self {
            let lock = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
            let previous = std::env::var(crate::brain_root::ENGINE_BRAIN_ROOT_ENV).ok();
            std::env::set_var(crate::brain_root::ENGINE_BRAIN_ROOT_ENV, root);
            Self {
                _lock: lock,
                previous,
            }
        }
    }

    impl Drop for BrainRootGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(v) => std::env::set_var(crate::brain_root::ENGINE_BRAIN_ROOT_ENV, v),
                None => std::env::remove_var(crate::brain_root::ENGINE_BRAIN_ROOT_ENV),
            }
        }
    }

    fn empty_context(event: serde_json::Value) -> TaskContext {
        TaskContext {
            event,
            nodes: StdHashMap::new(),
            metadata: json!({}),
            node_runs: StdHashMap::new(),
        }
    }

    #[test]
    fn schema_passes_validation() {
        let schema = schema();
        let registry = registry();

        WorkflowValidator::validate(&registry, &schema).expect("declared graph should validate");
    }

    #[test]
    fn start_node_is_check_existing_notes() {
        assert_eq!(schema().start_node, check_existing::NODE_NAME);
    }

    #[test]
    fn workflow_type_is_pre_plan() {
        assert_eq!(schema().workflow_type, WORKFLOW_TYPE);
    }

    #[test]
    fn registry_contains_all_five_nodes() {
        let registry = registry();

        let expected = [
            check_existing::NODE_NAME,
            check_existing::EXISTS_ROUTE,
            intake::NODE_NAME,
            research::NODE_NAME,
            write_notes::NODE_NAME,
        ];

        for identity in expected {
            assert!(
                registry.contains(identity),
                "expected registry to contain '{identity}'"
            );
        }
        assert_eq!(registry.len(), expected.len());
    }

    #[test]
    fn exists_route_and_write_notes_are_the_only_terminal_identities() {
        let schema = schema();

        let mut terminal_identities: Vec<String> = schema
            .nodes
            .iter()
            .filter(|(_, config)| config.connections.is_empty())
            .map(|(identity, _)| identity.clone())
            .collect();
        terminal_identities.sort();

        let mut expected = vec![
            check_existing::EXISTS_ROUTE.to_string(),
            write_notes::NODE_NAME.to_string(),
        ];
        expected.sort();

        assert_eq!(terminal_identities, expected);
    }

    #[test]
    fn check_existing_notes_node_declares_both_routes_as_connections() {
        let schema = schema();

        let config = schema
            .nodes
            .get(check_existing::NODE_NAME)
            .expect("schema should declare CheckExistingNotesNode");
        assert_eq!(
            config.connections,
            vec![
                check_existing::EXISTS_ROUTE.to_string(),
                check_existing::CONTINUE_ROUTE.to_string(),
            ]
        );
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

    #[test]
    fn registry_for_policy_never_changes_node_set_vs_registry() {
        let default_registry = registry();
        let policy_registry = registry_for_policy(&PrePlanPolicy::default());

        assert_eq!(policy_registry.len(), default_registry.len());
        for identity in [
            check_existing::NODE_NAME,
            check_existing::EXISTS_ROUTE,
            intake::NODE_NAME,
            research::NODE_NAME,
            write_notes::NODE_NAME,
        ] {
            assert!(policy_registry.contains(identity));
        }
    }

    #[tokio::test]
    async fn already_exists_node_reports_the_stamped_path() {
        let mut ctx = empty_context(json!({}));
        put_result(
            &mut ctx,
            check_existing::NODE_NAME,
            json!({"notes_path": "/tmp/some/notes.md", "exists": true}),
        );

        let node = PrePlanNotesAlreadyExistsNode::new();
        let ctx = node.process(ctx).await.expect("process should succeed");
        let stored = ctx
            .nodes
            .get(check_existing::EXISTS_ROUTE)
            .expect("result stored");
        assert_eq!(
            stored.get("notes_path").and_then(|v| v.as_str()),
            Some("/tmp/some/notes.md")
        );
        assert_eq!(
            stored.get("already_exists").and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[tokio::test]
    async fn already_exists_node_errors_when_check_existing_has_not_run() {
        let ctx = empty_context(json!({}));
        let node = PrePlanNotesAlreadyExistsNode::new();
        let err = node
            .process(ctx)
            .await
            .expect_err("should fail without CheckExistingNotesNode having run");
        assert!(err.message.contains(check_existing::NODE_NAME));
    }

    #[test]
    fn builtin_default_is_disabled_kill_switch() {
        let policy = PrePlanPolicy::default();
        assert!(!policy.enabled);
        assert_eq!(policy.research_model_tier, ModelTier::Sonnet);
        assert_eq!(policy.research_backend, AgentBackend::ClaudeCli);
    }

    #[test]
    fn profile_by_name_resolves_all_three_canonical_names() {
        assert_eq!(profile_by_name("baseline"), Some(baseline()));
        assert_eq!(profile_by_name("cheap-fast"), Some(cheap_fast()));
        assert_eq!(profile_by_name("thorough"), Some(thorough()));
    }

    #[test]
    fn profile_by_name_returns_none_for_unknown_name() {
        assert_eq!(profile_by_name("nonexistent"), None);
    }

    #[test]
    fn every_canonical_profile_enables_pre_plan() {
        assert_eq!(baseline().enabled, Some(true));
        assert_eq!(cheap_fast().enabled, Some(true));
        assert_eq!(thorough().enabled, Some(true));
    }

    #[test]
    fn resolve_policy_for_run_from_builtin_source_with_no_overrides_stays_disabled() {
        let ctx = empty_context(json!({"idea": "build a widget", "slug": "widget-idea"}));
        let resolved = resolve_policy_for_run_from(&ctx, &PolicyConfigSource::Builtin)
            .expect("resolve should succeed with no filesystem access");
        assert_eq!(resolved, PrePlanPolicy::default());
        assert!(!resolved.enabled);
    }

    #[test]
    fn resolve_policy_for_run_from_named_profile_enables_and_sets_tier() {
        let ctx = empty_context(json!({
            "idea": "build a widget",
            "slug": "widget-idea",
            "profile": "cheap-fast",
        }));
        let resolved = resolve_policy_for_run_from(&ctx, &PolicyConfigSource::Builtin)
            .expect("resolve should succeed");
        assert!(resolved.enabled);
        assert_eq!(resolved.research_model_tier, ModelTier::Haiku);
    }

    #[test]
    fn resolve_policy_for_run_from_inline_policy_override_beats_profile() {
        let ctx = empty_context(json!({
            "idea": "build a widget",
            "slug": "widget-idea",
            "profile": "cheap-fast",
            "policy": {"research_model_tier": "opus"},
        }));
        let resolved = resolve_policy_for_run_from(&ctx, &PolicyConfigSource::Builtin)
            .expect("resolve should succeed");
        assert!(resolved.enabled);
        assert_eq!(resolved.research_model_tier, ModelTier::Opus);
    }

    #[test]
    fn resolve_policy_for_run_from_unknown_profile_name_errors() {
        let ctx = empty_context(json!({
            "idea": "build a widget",
            "slug": "widget-idea",
            "profile": "nonexistent",
        }));
        let err = resolve_policy_for_run_from(&ctx, &PolicyConfigSource::Builtin)
            .expect_err("should fail");
        assert!(err.message.contains("unknown profile"));
    }

    #[test]
    fn resolve_policy_for_run_reads_harness_json_pre_plan_policy_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _guard = BrainRootGuard::set(dir.path());
        std::fs::create_dir_all(dir.path().join("planning")).unwrap();
        std::fs::write(
            dir.path().join("planning").join("harness.json"),
            serde_json::json!({
                "pre_plan": {
                    "policy": {
                        "enabled": true,
                        "research_model_tier": "haiku"
                    }
                }
            })
            .to_string(),
        )
        .unwrap();

        let ctx = empty_context(json!({"idea": "build a widget", "slug": "widget-idea"}));
        let resolved = resolve_policy_for_run_from(
            &ctx,
            &PolicyConfigSource::Worktree(dir.path().to_path_buf()),
        )
        .expect("resolve should succeed");
        assert!(resolved.enabled);
        assert_eq!(resolved.research_model_tier, ModelTier::Haiku);
    }

    /// Writes an executable `/bin/sh` script with `body` — mirrors
    /// `sdlc_task::graph`'s private `write_fake_binary` test helper.
    #[cfg(unix)]
    fn write_fake_binary(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let script_path = dir.path().join("fake-binary.sh");
        let mut file = std::fs::File::create(&script_path).expect("create script");
        std::io::Write::write_all(&mut file, format!("#!/bin/sh\n{body}\n").as_bytes())
            .expect("write script");
        drop(file);
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod +x");
        (dir, script_path)
    }

    /// Serializes every test in this module that mutates the process-global
    /// `PI_BINARY` env var — `cargo nextest run` (standing rule 8) forks one
    /// process per test, so this only guards a stray plain `cargo test` run,
    /// mirroring `pi_transport.rs`'s own `PI_BINARY_ENV_LOCK`.
    #[cfg(unix)]
    static PI_BACKEND_ENV_LOCK: Mutex<()> = Mutex::new(());

    /// A `PrePlanPolicy` with `research_backend: AgentBackend::Pi`, resolved
    /// through `registry_for_policy`, must produce a registry whose
    /// `ResearchCodebaseNode` dispatches the Pi meta-transport — this is the
    /// exact bug task 8 fixes: before it, `registry_for_policy` always
    /// passed the literal `AgentBackend::ClaudeCli` to
    /// `resolve_meta_transport`, so a `research_backend: pi` policy was
    /// silently ignored. Would fail if that fix were reverted.
    #[cfg(unix)]
    #[tokio::test]
    async fn registry_for_policy_with_pi_backend_dispatches_pi_transport() {
        let _guard = PI_BACKEND_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let (_script_dir, script) = write_fake_binary(
            r#"printf '{"type":"agent_end","messages":[{"role":"assistant","content":[{"type":"text","text":"hi"}]}]}\n'
printf '{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"done"}],"usage":{"totalTokens":1}}}\n'
"#,
        );
        // SAFETY: single-threaded within this test's own process
        // (`cargo nextest run` forks one process per test), scoped to this
        // test and serialized via `PI_BACKEND_ENV_LOCK`.
        unsafe {
            std::env::set_var("PI_BINARY", &script);
        }

        let policy = PrePlanPolicy {
            research_backend: AgentBackend::Pi,
            ..PrePlanPolicy::default()
        };
        let registry = registry_for_policy(&policy);
        let node = registry
            .get(research::NODE_NAME)
            .expect("ResearchCodebaseNode registered");
        let ctx = context_with_intake_for_registry("build a widget", "widget-idea");
        let result = node.process(ctx).await;

        unsafe {
            std::env::remove_var("PI_BINARY");
        }

        let out = result.expect("process should succeed against the fake pi script");
        let backend = out.nodes[research::NODE_NAME]["transport"]["backend"].clone();
        assert_eq!(backend, json!("pi"));
    }

    #[cfg(unix)]
    fn context_with_intake_for_registry(idea: &str, slug: &str) -> TaskContext {
        let mut ctx = empty_context(json!({}));
        put_result(
            &mut ctx,
            intake::NODE_NAME,
            json!({"idea": idea, "slug": slug, "channel": null, "sender": null}),
        );
        ctx
    }
}
