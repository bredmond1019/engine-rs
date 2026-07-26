//! The `Node` trait and node registry — the unit of work executed by the
//! `Workflow` runner (see `crate::workflow`, EN.1.A task 3).
//!
//! A `Node` only transforms the `TaskContext`; it does not touch its own
//! `NodeRun` status/timing — that envelope is framework-owned and stamped by
//! the runner around each call to `process` (contract §6).
//!
//! ## Composition primitives (`EN.5.E` task 1)
//!
//! Two additions here make nodes reusable across and within graphs:
//!
//! - **Instance identity (`with_identity`).** `NodeRegistry::register` keys
//!   on `node.name()`, so today the same node *type* can only ever appear
//!   once per graph (its identity is a per-type const). [`NodeExt::with_identity`]
//!   is a blanket extension method, implemented for every `Node`, that wraps
//!   the node in [`Identified`] — a delegating wrapper that overrides
//!   `name()` with an owned instance string while forwarding `process` and
//!   `as_router` unchanged. It is a **wrapper**, not a per-struct field,
//!   deliberately: it lets any of the 43 existing `impl Node` blocks (and
//!   the 20 more in `crates/*/tests/`) gain an overridable identity with
//!   zero edits to their source. Two `Identified` wrappers around the same
//!   underlying node type, constructed with different identity strings,
//!   register and resolve independently.
//! - **Input bindings (`InputBinding` / `with_input_from`).** [`InputBinding`]
//!   is the shared, reusable primitive a node holds (as a struct field,
//!   mirroring the existing `with_transport` / `with_runner` /
//!   `with_http_post` / `with_clock` builder convention) to know which
//!   upstream node's `ctx.nodes` entry to read, instead of a hardcoded
//!   `NODE_NAME` const imported from another module. `InputBinding::default()`
//!   is unbound; [`InputBinding::resolve`] falls back to a caller-supplied
//!   default in that case, so a node that never calls its own
//!   `with_input_from` builder behaves exactly as it did before this
//!   primitive existed. [`NodeExt::with_input_from`] additionally offers a
//!   generic, wrapper-based binding (via [`WithInput`]) for callers that
//!   only need to carry the binding alongside an existing node instance
//!   rather than author a bespoke per-struct builder.

use std::collections::HashMap;
use std::fmt;

use engine_contract::TaskContext;

use crate::routing::Router;

/// Error returned by a node's `process` when it fails. Carries a human-readable
/// message; the runner stores this in the node's `NodeRun.error` field and
/// halts the walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeError {
    pub message: String,
}

impl NodeError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for NodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for NodeError {}

/// A single unit of work in a workflow graph.
///
/// `process` transforms the `TaskContext` and returns the updated context (or
/// a `NodeError` on failure). `name` is the node's identity — a stable
/// type-name string used as the map key in both `TaskContext::nodes` and
/// `TaskContext::node_runs` (contract §1).
#[async_trait::async_trait]
pub trait Node: Send + Sync {
    /// Transform the context. The framework-owned envelope (see
    /// `crate::workflow::node_context`) handles `NodeRun` status/timing
    /// around this call — the node itself only does the work.
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError>;

    /// The node's identity — its type name, used as the registry/map key.
    fn name(&self) -> &str;

    /// Hook letting the runner/validator detect whether this node is a
    /// `Router` (chooses its next node at runtime) without downcasting.
    /// Defaults to `None`; router implementations override this to return
    /// `Some(self)`.
    fn as_router(&self) -> Option<&dyn Router> {
        None
    }
}

/// A delegating identity wrapper: overrides `Node::name()` with an owned
/// instance string while forwarding `process` and `as_router` to the
/// wrapped node unchanged.
///
/// Constructed via [`NodeExt::with_identity`]. This is the mechanism behind
/// instance-backed node identity — it exists so the same node *type* can be
/// registered more than once (under distinct identities) in one graph, or
/// reused across graphs, without editing that node type's `impl Node` block.
pub struct Identified<N> {
    inner: N,
    identity: String,
}

impl<N> Identified<N> {
    /// Wrap `inner`, overriding its reported identity with `identity`.
    pub fn new(inner: N, identity: impl Into<String>) -> Self {
        Self {
            inner,
            identity: identity.into(),
        }
    }

    /// Borrow the wrapped node.
    pub fn inner(&self) -> &N {
        &self.inner
    }
}

#[async_trait::async_trait]
impl<N: Node> Node for Identified<N> {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        // `ctx.node_runs` stamping is entirely framework-owned (the runner's
        // `node_context` envelope calls `name()` on whatever it resolved
        // from the registry, so an overridden identity already lands there
        // for free). `ctx.nodes` stamping, by contrast, is done by each
        // node's own `process` body against its *own* notion of its
        // identity (`self.name()` or a hardcoded `NODE_NAME` const) — that
        // executes with `self` bound to the wrapped node, not this wrapper,
        // so it always writes under the wrapped node's original/default
        // identity. Relabel that single entry (if produced) onto the
        // override identity so two `Identified` instances of the same node
        // type stamp independent `ctx.nodes` entries.
        let default_identity = self.inner.name().to_string();
        let mut out = self.inner.process(ctx).await?;
        if default_identity != self.identity {
            if let Some(value) = out.nodes.remove(&default_identity) {
                out.nodes.insert(self.identity.clone(), value);
            }
        }
        Ok(out)
    }

    fn name(&self) -> &str {
        &self.identity
    }

    fn as_router(&self) -> Option<&dyn Router> {
        // Forward to the wrapped node so identity overrides never hide a
        // router from `WorkflowValidator::is_router` (validate.rs classifies
        // purely off this hook, and that file is not expected to change).
        self.inner.as_router()
    }
}

/// A declarative binding from a node to the identity of the upstream node
/// whose `ctx.nodes` entry it should read — the shared primitive that
/// replaces a hardcoded `NODE_NAME` const imported from another module.
///
/// Node authors hold this as a struct field (following the existing
/// `with_transport` / `with_runner` / `with_http_post` / `with_clock`
/// builder convention: a `#[must_use]`, `self`-consuming `with_input_from`
/// method that sets the field) and call [`InputBinding::resolve`] inside
/// `process` to get the upstream identity to look up in `ctx.nodes`.
///
/// `InputBinding::default()` is unbound and `resolve` falls back to the
/// caller-supplied default in that case, so a node that never calls its own
/// `with_input_from` behaves exactly as it did before this primitive
/// existed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InputBinding(Option<String>);

impl InputBinding {
    /// An explicitly unbound binding. Equivalent to `InputBinding::default()`.
    #[must_use]
    pub fn unbound() -> Self {
        Self(None)
    }

    /// A binding pinned to `upstream`'s identity.
    #[must_use]
    pub fn bound(upstream: impl Into<String>) -> Self {
        Self(Some(upstream.into()))
    }

    /// Whether this binding was explicitly set via [`InputBinding::bound`].
    #[must_use]
    pub fn is_bound(&self) -> bool {
        self.0.is_some()
    }

    /// Resolve to the bound upstream identity, or `default` when unbound.
    #[must_use]
    pub fn resolve<'a>(&'a self, default: &'a str) -> &'a str {
        self.0.as_deref().unwrap_or(default)
    }
}

/// A generic, wrapper-based input binding for callers that want to carry an
/// [`InputBinding`] alongside an existing node instance without authoring a
/// bespoke per-struct `with_input_from` builder. Constructed via
/// [`NodeExt::with_input_from`]; forwards `process`/`name`/`as_router` to the
/// wrapped node unchanged (the binding itself is inert here — it is metadata
/// the wrapping/assembly code reads via [`WithInput::binding`], not
/// something the wrapped node's own `process` can see).
pub struct WithInput<N> {
    inner: N,
    binding: InputBinding,
}

impl<N> WithInput<N> {
    /// Wrap `inner`, binding its declared upstream to `upstream`'s identity.
    pub fn new(inner: N, upstream: impl Into<String>) -> Self {
        Self {
            inner,
            binding: InputBinding::bound(upstream),
        }
    }

    /// The binding this wrapper carries.
    #[must_use]
    pub fn binding(&self) -> &InputBinding {
        &self.binding
    }

    /// Borrow the wrapped node.
    pub fn inner(&self) -> &N {
        &self.inner
    }
}

#[async_trait::async_trait]
impl<N: Node> Node for WithInput<N> {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        self.inner.process(ctx).await
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn as_router(&self) -> Option<&dyn Router> {
        self.inner.as_router()
    }
}

/// Blanket extension trait adding the builder-style composition primitives
/// on top of any `Node`, with zero changes required to existing `impl Node`
/// blocks.
pub trait NodeExt: Node + Sized {
    /// Override this node's identity — the string returned by `name()` and
    /// used as the `NodeRegistry` key. See [`Identified`].
    #[must_use]
    fn with_identity(self, identity: impl Into<String>) -> Identified<Self> {
        Identified::new(self, identity)
    }

    /// Bind this node's declared upstream to `upstream`'s identity. See
    /// [`WithInput`].
    #[must_use]
    fn with_input_from(self, upstream: impl Into<String>) -> WithInput<Self> {
        WithInput::new(self, upstream)
    }
}

impl<N: Node + Sized> NodeExt for N {}

/// Maps a node's identity string to its boxed `Node` instance, so the runner
/// can resolve the next node to execute by name.
#[derive(Default)]
pub struct NodeRegistry {
    nodes: HashMap<String, Box<dyn Node>>,
}

impl NodeRegistry {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
        }
    }

    /// Register a node under its own identity (`Node::name`).
    pub fn register(&mut self, node: Box<dyn Node>) {
        let key = node.name().to_string();
        self.nodes.insert(key, node);
    }

    /// Look up a node by identity string.
    pub fn get(&self, name: &str) -> Option<&dyn Node> {
        self.nodes.get(name).map(|boxed| boxed.as_ref())
    }

    /// Whether a node with this identity is registered.
    pub fn contains(&self, name: &str) -> bool {
        self.nodes.contains_key(name)
    }

    /// Number of registered nodes.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trivial node that stamps a marker into `TaskContext::nodes` under
    /// its own identity.
    struct MarkerNode;

    #[async_trait::async_trait]
    impl Node for MarkerNode {
        async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
            ctx.nodes
                .insert(self.name().to_string(), serde_json::json!({ "ran": true }));
            Ok(ctx)
        }

        fn name(&self) -> &str {
            "MarkerNode"
        }
    }

    fn empty_context() -> TaskContext {
        TaskContext {
            event: serde_json::json!({}),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn node_process_transforms_task_context() {
        let node = MarkerNode;
        let ctx = empty_context();

        let out = node.process(ctx).await.expect("process should succeed");

        assert_eq!(
            out.nodes.get("MarkerNode"),
            Some(&serde_json::json!({ "ran": true }))
        );
    }

    #[test]
    fn node_identity_matches_registry_key() {
        let mut registry = NodeRegistry::new();
        let node = MarkerNode;
        let identity = node.name().to_string();

        registry.register(Box::new(node));

        assert!(registry.contains(&identity));
        let looked_up = registry.get(&identity).expect("node should be registered");
        assert_eq!(looked_up.name(), identity);
    }

    #[test]
    fn registry_lookup_missing_returns_none() {
        let registry = NodeRegistry::new();
        assert!(registry.get("Nonexistent").is_none());
    }

    // -- with_identity ---------------------------------------------------

    #[test]
    fn default_identity_equals_type_const() {
        let node = MarkerNode;
        assert_eq!(node.name(), "MarkerNode");
    }

    #[test]
    fn with_identity_changes_name_and_registry_key() {
        let node = MarkerNode.with_identity("MarkerNode#1");

        assert_eq!(node.name(), "MarkerNode#1");

        let mut registry = NodeRegistry::new();
        registry.register(Box::new(node));

        assert!(registry.contains("MarkerNode#1"));
        assert!(!registry.contains("MarkerNode"));
    }

    #[test]
    fn registry_holds_two_instances_of_one_type_under_distinct_identities() {
        let mut registry = NodeRegistry::new();
        registry.register(Box::new(MarkerNode.with_identity("MarkerNode#1")));
        registry.register(Box::new(MarkerNode.with_identity("MarkerNode#2")));

        assert_eq!(registry.len(), 2);
        assert_eq!(
            registry.get("MarkerNode#1").expect("first instance").name(),
            "MarkerNode#1"
        );
        assert_eq!(
            registry
                .get("MarkerNode#2")
                .expect("second instance")
                .name(),
            "MarkerNode#2"
        );
    }

    #[tokio::test]
    async fn two_identified_instances_run_with_independent_inputs() {
        let mut registry = NodeRegistry::new();
        registry.register(Box::new(MarkerNode.with_identity("MarkerNode#1")));
        registry.register(Box::new(MarkerNode.with_identity("MarkerNode#2")));

        let ctx = empty_context();
        let ctx = registry
            .get("MarkerNode#1")
            .expect("first instance")
            .process(ctx)
            .await
            .expect("first process should succeed");
        let ctx = registry
            .get("MarkerNode#2")
            .expect("second instance")
            .process(ctx)
            .await
            .expect("second process should succeed");

        assert_eq!(
            ctx.nodes.get("MarkerNode#1"),
            Some(&serde_json::json!({ "ran": true }))
        );
        assert_eq!(
            ctx.nodes.get("MarkerNode#2"),
            Some(&serde_json::json!({ "ran": true }))
        );
    }

    /// A trivial router used to prove `as_router()` survives identity
    /// overrides.
    struct StubRouter;

    #[async_trait::async_trait]
    impl Node for StubRouter {
        async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
            Ok(ctx)
        }

        fn name(&self) -> &str {
            "StubRouter"
        }

        fn as_router(&self) -> Option<&dyn Router> {
            Some(self)
        }
    }

    impl Router for StubRouter {
        fn route(&self, _ctx: &TaskContext) -> Option<String> {
            Some("Next".to_string())
        }
    }

    #[test]
    fn identity_overridden_router_still_reports_as_router() {
        let node = StubRouter.with_identity("StubRouter#1");

        assert_eq!(node.name(), "StubRouter#1");
        assert!(node.as_router().is_some());
    }

    // -- InputBinding / with_input_from ----------------------------------

    #[test]
    fn input_binding_unbound_resolves_to_default() {
        let binding = InputBinding::default();
        assert!(!binding.is_bound());
        assert_eq!(binding.resolve("Default"), "Default");
    }

    #[test]
    fn input_binding_bound_resolves_to_bound_value() {
        let binding = InputBinding::bound("Upstream");
        assert!(binding.is_bound());
        assert_eq!(binding.resolve("Default"), "Upstream");
    }

    /// A node whose per-struct `with_input_from` builder holds an
    /// `InputBinding`, mirroring the `with_transport` / `with_http_post`
    /// convention real nodes (e.g. `proposal_generator::revise`) follow. It
    /// reads its upstream's `ctx.nodes` entry through `InputBinding::resolve`
    /// instead of a hardcoded `NODE_NAME` const, falling back to
    /// `DEFAULT_UPSTREAM` when unbound.
    struct ReaderNode {
        input: InputBinding,
    }

    impl ReaderNode {
        const DEFAULT_UPSTREAM: &'static str = "DefaultUpstream";

        fn new() -> Self {
            Self {
                input: InputBinding::default(),
            }
        }

        #[must_use]
        fn with_input_from(mut self, upstream: impl Into<String>) -> Self {
            self.input = InputBinding::bound(upstream);
            self
        }
    }

    #[async_trait::async_trait]
    impl Node for ReaderNode {
        async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
            let upstream = self.input.resolve(Self::DEFAULT_UPSTREAM);
            let value = ctx
                .nodes
                .get(upstream)
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            ctx.nodes.insert("ReaderNode".to_string(), value);
            Ok(ctx)
        }

        fn name(&self) -> &str {
            "ReaderNode"
        }
    }

    #[tokio::test]
    async fn bound_reader_node_reads_its_bound_upstream() {
        let node = ReaderNode::new().with_input_from("Upstream");

        let mut ctx = empty_context();
        ctx.nodes
            .insert("Upstream".to_string(), serde_json::json!({ "value": 1 }));
        ctx.nodes.insert(
            ReaderNode::DEFAULT_UPSTREAM.to_string(),
            serde_json::json!({ "value": 99 }),
        );

        let ctx = node.process(ctx).await.expect("process should succeed");

        assert_eq!(
            ctx.nodes.get("ReaderNode"),
            Some(&serde_json::json!({ "value": 1 }))
        );
    }

    #[tokio::test]
    async fn unbound_reader_node_falls_back_to_default_upstream() {
        let node = ReaderNode::new();

        let mut ctx = empty_context();
        ctx.nodes.insert(
            ReaderNode::DEFAULT_UPSTREAM.to_string(),
            serde_json::json!({ "value": 99 }),
        );

        let ctx = node.process(ctx).await.expect("process should succeed");

        assert_eq!(
            ctx.nodes.get("ReaderNode"),
            Some(&serde_json::json!({ "value": 99 }))
        );
    }

    // -- WithInput (generic wrapper) --------------------------------------

    #[test]
    fn with_input_from_wrapper_carries_binding_and_forwards_identity() {
        let node = MarkerNode.with_input_from("Upstream");

        assert_eq!(node.binding().resolve("Default"), "Upstream");
        assert_eq!(node.name(), "MarkerNode");
        assert!(node.as_router().is_none());
    }

    #[tokio::test]
    async fn with_input_wrapper_process_matches_inner_unwrapped_behavior() {
        let wrapped = MarkerNode.with_input_from("Upstream");
        let plain = MarkerNode;

        let wrapped_out = wrapped
            .process(empty_context())
            .await
            .expect("wrapped process should succeed");
        let plain_out = plain
            .process(empty_context())
            .await
            .expect("plain process should succeed");

        assert_eq!(wrapped_out.nodes, plain_out.nodes);
    }
}
