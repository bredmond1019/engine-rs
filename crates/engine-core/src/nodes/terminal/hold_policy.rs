//! `hold_policy` (`EN.9.G` task 1) — the per-workflow operator-hold policy
//! surface over the `EN.9.B` lease/hold mechanism
//! (`term_core::lease::SessionLease`, `term_core::hold::OperatorHold`).
//!
//! # This is a policy surface, not the lease
//!
//! `lease.rs` and `hold.rs` already ship the mechanism (advisory
//! tmux-option lease, read-back arbitration, fail-closed steal refusal,
//! detach-grace hold). This module does not reimplement any of that — it
//! resolves the two knobs those modules already accept as call-site
//! overrides (`lease.rs`'s `AcquireRequest::steal_after`,
//! `hold.rs`'s `OperatorHold::with_grace`) into one policy value per
//! workflow, per CLAUDE.md standing rule 6.
//!
//! # Knobs
//!
//! - `grace_ms` — the operator-hold detach grace window. Default 60s
//!   (`hold.rs::DEFAULT_DETACH_GRACE`) — **this exact default is an
//!   acceptance criterion for `EN.9.G` task 1; do not change it.**
//! - `steal_after_ms` — the window past expiry after which a stale
//!   FOREIGN lease becomes acquirable. `None` (the built-in default)
//!   stays **fail-closed**: an expired-but-present lease is never
//!   acquired. That is `lease.rs`'s existing `LeaseError::NoStealWindow`
//!   contract; this surface only configures the knob feeding it, it does
//!   not soften the contract itself.
//!
//! # Resolution — four layers, per workflow
//!
//! High -> low precedence: per-run `ctx.event.policy` override, then a
//! named `ctx.event.profile` bundle, then `<workflow_key>.policy` /
//! `<workflow_key>.profiles` sections of `planning/harness.json` (or an
//! injected [`PolicyConfigSource`]), then [`HoldPolicy::default`].
//! `workflow_key` is supplied per call (mirroring
//! `workflows::sdlc_flow::setup::WORKFLOW_KEY`'s pattern, generalized via
//! `crate::policy::read_harness_policy_defaults_from` /
//! `crate::policy::resolve_profile_from`) — two workflows can each carry
//! their own hold policy defaults in the same `harness.json` without
//! colliding, because the section lookup is keyed by workflow, not global.
//!
//! [`HoldPolicy::default`] is behavior-stable: a run that opts into no
//! override, no profile, and finds no `harness.json` section gets exactly
//! `hold.rs`'s pre-`EN.9.G` constant (60s grace) and `lease.rs`'s
//! pre-`EN.9.G` fail-closed refusal (`steal_after: None`).

use engine_contract::TaskContext;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::node::{Node, NodeError};
use crate::policy::{
    merge_opt, read_harness_policy_defaults_from, resolve as resolve_policy_layers,
    resolve_profile_from, Policy, PolicyConfigSource,
};
use crate::workflows::put_result;

/// The `Node::name()` identity [`HoldPolicyNode`] runs under, and the
/// `ctx.nodes` key its output is stamped onto.
pub const NODE_NAME: &str = "HoldPolicyNode";

// ── Policy ───────────────────────────────────────────────────────────────

/// The fully-resolved, per-workflow operator-hold policy — see the module
/// doc for what each knob configures and why `steal_after_ms: None` stays
/// fail-closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HoldPolicy {
    pub grace_ms: u64,
    pub steal_after_ms: Option<u64>,
}

impl Default for HoldPolicy {
    /// The behavior-stable baseline: 60s detach grace
    /// (`term_core::hold::DEFAULT_DETACH_GRACE`), and `steal_after_ms:
    /// None` — an expired FOREIGN lease is never acquired
    /// (`term_core::lease::LeaseError::NoStealWindow`). This exact default
    /// is an `EN.9.G` task 1 acceptance criterion; do not change it.
    fn default() -> Self {
        Self {
            grace_ms: 60_000,
            steal_after_ms: None,
        }
    }
}

impl HoldPolicy {
    /// `grace_ms` as a [`Duration`], ready for
    /// `term_core::hold::OperatorHold::with_grace`.
    #[must_use]
    pub fn grace(&self) -> Duration {
        Duration::from_millis(self.grace_ms)
    }

    /// `steal_after_ms` as an `Option<Duration>`, ready for
    /// `term_core::lease::AcquireRequest::steal_after`. `None` preserves
    /// the fail-closed refusal.
    #[must_use]
    pub fn steal_after(&self) -> Option<Duration> {
        self.steal_after_ms.map(Duration::from_millis)
    }
}

/// All-optional mirror of [`HoldPolicy`] used by the override layers (a
/// `harness.json` `<workflow_key>.policy`/`profiles[name]` entry, and a
/// per-run `ctx.event.policy` override).
///
/// `steal_after_ms` is doubly-optional by design: the OUTER `Option`
/// means "this layer does not touch the knob at all" (falls through to
/// the next-lower-precedence layer, exactly like every other field here);
/// the INNER `Option<u64>` is the knob's own resolved shape — a layer can
/// explicitly set `steal_after_ms: null` to assert "stay fail-closed here"
/// (winning over a lower layer's `Some(ms)`), which a single-level
/// `Option<u64>` could never distinguish from "untouched".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialHoldPolicy {
    pub grace_ms: Option<u64>,
    #[serde(default, with = "steal_after_layer")]
    pub steal_after_ms: Option<Option<u64>>,
}

/// `serde` helper for [`PartialHoldPolicy::steal_after_ms`]'s
/// doubly-optional shape: absent JSON key -> `None` (layer untouched);
/// `"steal_after_ms": null` -> `Some(None)` (layer explicitly asserts
/// fail-closed); `"steal_after_ms": 30000` -> `Some(Some(30000))`.
mod steal_after_layer {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(
        value: &Option<Option<u64>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(inner) => inner.serialize(serializer),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Option<u64>>, D::Error> {
        Option::<u64>::deserialize(deserializer).map(Some)
    }
}

impl Policy for HoldPolicy {
    type Partial = PartialHoldPolicy;

    fn apply(self, over: &PartialHoldPolicy) -> Self {
        Self {
            grace_ms: merge_opt(self.grace_ms, over.grace_ms),
            steal_after_ms: merge_opt(self.steal_after_ms, over.steal_after_ms),
        }
    }
}

/// Resolve the four policy layers into one concrete [`HoldPolicy`],
/// high->low precedence: `event_override` beats `profile` beats
/// `harness_defaults` beats [`HoldPolicy::default`]. Delegates to the
/// generic `crate::policy::resolve`.
#[must_use]
pub fn resolve(
    harness_defaults: Option<&PartialHoldPolicy>,
    profile: Option<&PartialHoldPolicy>,
    event_override: Option<&PartialHoldPolicy>,
) -> HoldPolicy {
    resolve_policy_layers(
        HoldPolicy::default(),
        harness_defaults,
        profile,
        event_override,
    )
}

/// Resolve [`HoldPolicy`] for one workflow's run, reading
/// `<workflow_key>.policy` / `<workflow_key>.profiles` out of `source`
/// (per-workflow — a different `workflow_key` reads a different
/// `harness.json` section) and applying `event_override`/`profile_name` on
/// top, high->low precedence via [`resolve`]. [`PolicyConfigSource::Builtin`]
/// resolves successfully with no filesystem access (builtin + profile +
/// event layers only) — the case a worktree-free run needs.
pub fn resolve_for_workflow(
    workflow_key: &str,
    source: &PolicyConfigSource,
    profile_name: Option<&str>,
    event_override: Option<&PartialHoldPolicy>,
) -> Result<HoldPolicy, NodeError> {
    let harness_defaults =
        read_harness_policy_defaults_from::<PartialHoldPolicy>(source, workflow_key)?;
    let profile = resolve_profile_from::<PartialHoldPolicy>(
        profile_name,
        source,
        workflow_key,
        profiles::profile_by_name,
    )?;
    Ok(resolve(
        harness_defaults.as_ref(),
        profile.as_ref(),
        event_override,
    ))
}

/// Named [`PartialHoldPolicy`] bundles, per CLAUDE.md standing rule 6's
/// "every workflow ships the three named profiles" — `baseline` restates
/// the built-in default verbatim (a legible no-op), `cheap-fast` is the
/// cost/latency floor (short grace, aggressive reclaim of a stale lease so
/// a run doesn't sit queued behind an abandoned session), `thorough` is
/// the quality ceiling (long grace, still fail-closed — patient with a
/// slow-responding operator, and never guesses that a lease is safe to
/// steal).
pub mod profiles {
    use super::{HoldPolicy, PartialHoldPolicy};

    /// Restates [`HoldPolicy::default`] verbatim — selecting `"baseline"`
    /// must not silently change behavior.
    #[must_use]
    pub fn baseline() -> PartialHoldPolicy {
        let default = HoldPolicy::default();
        PartialHoldPolicy {
            grace_ms: Some(default.grace_ms),
            steal_after_ms: Some(default.steal_after_ms),
        }
    }

    /// The cost/latency floor: a 15s grace and a 15s steal window — reclaim
    /// a stale lease quickly rather than let cheap/fast runs queue behind
    /// an abandoned session.
    #[must_use]
    pub fn cheap_fast() -> PartialHoldPolicy {
        PartialHoldPolicy {
            grace_ms: Some(15_000),
            steal_after_ms: Some(Some(15_000)),
        }
    }

    /// The quality ceiling: a 5-minute grace, and still `steal_after_ms:
    /// None` — patient with a slow operator, never softening the
    /// fail-closed steal refusal just to move a queue along faster.
    #[must_use]
    pub fn thorough() -> PartialHoldPolicy {
        PartialHoldPolicy {
            grace_ms: Some(300_000),
            steal_after_ms: Some(None),
        }
    }

    /// Look up one of the three canonical profile names. `None` for any
    /// other name — callers decide whether an unknown name is an error.
    #[must_use]
    pub fn profile_by_name(name: &str) -> Option<PartialHoldPolicy> {
        match name {
            "baseline" => Some(baseline()),
            "cheap-fast" => Some(cheap_fast()),
            "thorough" => Some(thorough()),
            _ => None,
        }
    }
}

// ── The node ─────────────────────────────────────────────────────────────

/// Resolves this run's [`HoldPolicy`] for one `workflow_key` and stamps
/// the resolved values into its own `ctx.nodes` result — the read surface
/// a hold/lease-consuming node (or an operator-facing status endpoint)
/// reads instead of re-deriving the four-layer merge itself.
///
/// Deliberately does not touch `term_core::lease`/`term_core::hold` at
/// all — wiring the resolved policy into an actual lease acquisition or
/// hold guard is the job of whichever node consumes
/// [`HoldPolicyNode`]'s stamped result, not this one (mirrors
/// `admission.rs`'s "supplies the policy and the control, wiring a `Node`
/// around it into the send/lease path is out of this task's scope").
pub struct HoldPolicyNode {
    /// The `harness.json` section (`<workflow_key>.policy`/`.profiles`)
    /// this instance reads — set once per workflow at construction, which
    /// is what makes the surface genuinely per-workflow rather than one
    /// shared global.
    workflow_key: String,
    /// Where to read `<workflow_key>.policy`/`.profiles` from. Defaults to
    /// [`PolicyConfigSource::Builtin`] (builtin + profile + event layers
    /// only, no filesystem access) unless overridden.
    source: PolicyConfigSource,
}

impl HoldPolicyNode {
    /// Construct for `workflow_key`, reading no `harness.json` section
    /// ([`PolicyConfigSource::Builtin`]) unless [`Self::with_source`] is
    /// called.
    #[must_use]
    pub fn new(workflow_key: impl Into<String>) -> Self {
        Self {
            workflow_key: workflow_key.into(),
            source: PolicyConfigSource::Builtin,
        }
    }

    /// Read `<workflow_key>.policy`/`.profiles` from `source` instead of
    /// the [`PolicyConfigSource::Builtin`] default.
    #[must_use]
    pub fn with_source(mut self, source: PolicyConfigSource) -> Self {
        self.source = source;
        self
    }

    /// This node's configured `workflow_key`.
    #[must_use]
    pub fn workflow_key(&self) -> &str {
        &self.workflow_key
    }

    /// Read this node's own `policy`/`profile` arguments off `ctx.event`,
    /// mirroring `TerminalAwaitNode::read_event_args`'s convention: a
    /// top-level `ctx.event.policy` object is the event-layer override, a
    /// top-level `ctx.event.profile` string names a profile bundle. Both
    /// absent is not an error — it just means no override at either
    /// layer.
    fn read_event_args(
        &self,
        ctx: &TaskContext,
    ) -> Result<(Option<PartialHoldPolicy>, Option<String>), NodeError> {
        let event_override = match ctx.event.get("policy") {
            Some(value) => Some(
                serde_json::from_value::<PartialHoldPolicy>(value.clone()).map_err(|err| {
                    NodeError::new(format!("{NODE_NAME}: invalid ctx.event.policy: {err}"))
                })?,
            ),
            None => None,
        };
        let profile_name = ctx
            .event
            .get("profile")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        Ok((event_override, profile_name))
    }
}

#[async_trait::async_trait]
impl Node for HoldPolicyNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let (event_override, profile_name) = self.read_event_args(&ctx)?;
        let policy = resolve_for_workflow(
            &self.workflow_key,
            &self.source,
            profile_name.as_deref(),
            event_override.as_ref(),
        )?;

        put_result(
            &mut ctx,
            self.name(),
            serde_json::json!({
                "workflow_key": self.workflow_key,
                "grace_ms": policy.grace_ms,
                "steal_after_ms": policy.steal_after_ms,
            }),
        );
        Ok(ctx)
    }

    fn name(&self) -> &str {
        NODE_NAME
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::get_result;

    fn temp_dir() -> std::path::PathBuf {
        tempfile::Builder::new()
            .prefix("engine-core-hold-policy-test-")
            .tempdir()
            .expect("create temp dir")
            .keep()
    }

    fn write_harness_json(dir: &std::path::Path, contents: &str) {
        std::fs::create_dir_all(dir.join("planning")).expect("create planning dir");
        std::fs::write(dir.join("planning").join("harness.json"), contents)
            .expect("write harness.json");
    }

    #[test]
    fn default_grace_is_60s_and_steal_after_is_fail_closed() {
        let default = HoldPolicy::default();
        assert_eq!(default.grace_ms, 60_000);
        assert_eq!(default.grace(), Duration::from_secs(60));
        assert_eq!(default.steal_after_ms, None);
        assert_eq!(default.steal_after(), None);
    }

    #[test]
    fn resolve_with_no_overrides_returns_default() {
        assert_eq!(resolve(None, None, None), HoldPolicy::default());
    }

    #[test]
    fn resolve_precedence_event_beats_profile_beats_harness_beats_default() {
        let harness = PartialHoldPolicy {
            grace_ms: Some(10_000),
            steal_after_ms: Some(Some(10_000)),
        };
        let profile = PartialHoldPolicy {
            grace_ms: Some(20_000),
            steal_after_ms: None,
        };
        let event = PartialHoldPolicy {
            grace_ms: Some(30_000),
            steal_after_ms: None,
        };

        assert_eq!(
            resolve(Some(&harness), None, None).grace_ms,
            10_000,
            "harness beats default"
        );
        assert_eq!(
            resolve(Some(&harness), Some(&profile), None).grace_ms,
            20_000,
            "profile beats harness"
        );
        assert_eq!(
            resolve(Some(&harness), Some(&profile), Some(&event)).grace_ms,
            30_000,
            "event beats profile"
        );
        // Neither `profile` nor `event` touched `steal_after_ms`, so
        // harness's value survives through to the top.
        assert_eq!(
            resolve(Some(&harness), Some(&profile), Some(&event)).steal_after_ms,
            Some(10_000)
        );
    }

    #[test]
    fn a_configured_steal_after_overrides_the_default() {
        let event = PartialHoldPolicy {
            grace_ms: None,
            steal_after_ms: Some(Some(90_000)),
        };
        let resolved = resolve(None, None, Some(&event));
        assert_eq!(resolved.steal_after_ms, Some(90_000));
        assert_eq!(resolved.steal_after(), Some(Duration::from_secs(90)));
    }

    #[test]
    fn steal_after_none_stays_fail_closed_even_over_a_lower_layer_value() {
        let harness = PartialHoldPolicy {
            grace_ms: None,
            steal_after_ms: Some(Some(60_000)),
        };
        // The event layer explicitly asserts "stay fail-closed" —
        // `Some(None)`, not the untouched `None` — and it must win over
        // harness's `Some(Some(60_000))`.
        let event = PartialHoldPolicy {
            grace_ms: None,
            steal_after_ms: Some(None),
        };
        let resolved = resolve(Some(&harness), None, Some(&event));
        assert_eq!(resolved.steal_after_ms, None);
        assert_eq!(resolved.steal_after(), None);
    }

    #[test]
    fn steal_after_untouched_layer_falls_through() {
        let harness = PartialHoldPolicy {
            grace_ms: None,
            steal_after_ms: Some(Some(45_000)),
        };
        let event = PartialHoldPolicy {
            grace_ms: Some(5_000),
            steal_after_ms: None, // untouched — falls through to harness
        };
        let resolved = resolve(Some(&harness), None, Some(&event));
        assert_eq!(resolved.steal_after_ms, Some(45_000));
        assert_eq!(resolved.grace_ms, 5_000);
    }

    #[test]
    fn partial_hold_policy_steal_after_layer_round_trips_through_json() {
        let untouched = PartialHoldPolicy {
            grace_ms: Some(1),
            steal_after_ms: None,
        };
        let value = serde_json::to_value(&untouched).unwrap();
        assert!(value.get("steal_after_ms").is_none() || value["steal_after_ms"].is_null());
        let parsed: PartialHoldPolicy =
            serde_json::from_value(serde_json::json!({"grace_ms": 1})).unwrap();
        assert_eq!(parsed.steal_after_ms, None);

        let explicit_none: PartialHoldPolicy =
            serde_json::from_value(serde_json::json!({"steal_after_ms": null})).unwrap();
        assert_eq!(explicit_none.steal_after_ms, Some(None));

        let explicit_some: PartialHoldPolicy =
            serde_json::from_value(serde_json::json!({"steal_after_ms": 5000})).unwrap();
        assert_eq!(explicit_some.steal_after_ms, Some(Some(5000)));
    }

    #[test]
    fn profiles_set_both_knobs_explicitly() {
        let default = HoldPolicy::default();
        assert_eq!(profiles::baseline().grace_ms, Some(default.grace_ms));
        assert_eq!(
            profiles::baseline().steal_after_ms,
            Some(default.steal_after_ms)
        );

        assert_eq!(profiles::cheap_fast().grace_ms, Some(15_000));
        assert_eq!(profiles::cheap_fast().steal_after_ms, Some(Some(15_000)));

        assert_eq!(profiles::thorough().grace_ms, Some(300_000));
        assert_eq!(profiles::thorough().steal_after_ms, Some(None));

        assert!(profiles::profile_by_name("nonexistent").is_none());
    }

    #[test]
    fn baseline_profile_is_a_genuine_no_op() {
        // Selecting "baseline" must resolve to byte-identical behaviour as
        // no profile at all.
        let baseline = profiles::baseline();
        assert_eq!(
            resolve(None, Some(&baseline), None),
            resolve(None, None, None)
        );
    }

    #[test]
    fn resolve_for_workflow_reads_the_workflow_keyed_harness_section() {
        let dir = temp_dir();
        write_harness_json(
            &dir,
            r#"{
              "hold_policy_workflow_a": { "policy": { "grace_ms": 12000 } },
              "hold_policy_workflow_b": { "policy": { "grace_ms": 99000 } }
            }"#,
        );
        let source = PolicyConfigSource::Worktree(dir.clone());

        let resolved_a =
            resolve_for_workflow("hold_policy_workflow_a", &source, None, None).unwrap();
        let resolved_b =
            resolve_for_workflow("hold_policy_workflow_b", &source, None, None).unwrap();
        // A workflow key with no section at all falls through to default.
        let resolved_c =
            resolve_for_workflow("hold_policy_workflow_c", &source, None, None).unwrap();

        assert_eq!(resolved_a.grace_ms, 12_000);
        assert_eq!(resolved_b.grace_ms, 99_000);
        assert_eq!(resolved_c.grace_ms, HoldPolicy::default().grace_ms);
    }

    #[test]
    fn resolve_for_workflow_named_profile_beats_harness_defaults() {
        let dir = temp_dir();
        write_harness_json(
            &dir,
            r#"{
              "hold_policy_workflow_a": {
                "policy": { "grace_ms": 12000 },
                "profiles": { "cheap-fast": { "grace_ms": 7000, "steal_after_ms": 7000 } }
              }
            }"#,
        );
        let source = PolicyConfigSource::Worktree(dir.clone());

        let resolved =
            resolve_for_workflow("hold_policy_workflow_a", &source, Some("cheap-fast"), None)
                .unwrap();
        assert_eq!(resolved.grace_ms, 7_000);
        assert_eq!(resolved.steal_after_ms, Some(7_000));

        // A profile name absent from harness.json's own `profiles` map
        // falls back to the built-in bundle of the same name.
        let resolved_builtin = resolve_for_workflow(
            "hold_policy_workflow_missing",
            &source,
            Some("thorough"),
            None,
        )
        .unwrap();
        assert_eq!(resolved_builtin.grace_ms, 300_000);
    }

    #[test]
    fn resolve_for_workflow_builtin_source_resolves_with_no_filesystem_access() {
        let resolved =
            resolve_for_workflow("any-workflow", &PolicyConfigSource::Builtin, None, None).unwrap();
        assert_eq!(resolved, HoldPolicy::default());
    }

    #[tokio::test]
    async fn node_stamps_the_resolved_values_under_its_own_result() {
        let node = HoldPolicyNode::new("hold_policy_test_workflow");
        let ctx = TaskContext {
            event: serde_json::json!({}),
            nodes: std::collections::HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: std::collections::HashMap::new(),
        };

        let result = node.process(ctx).await.expect("process should succeed");
        let stamped = get_result(&result, NODE_NAME).expect("result should be stamped");
        assert_eq!(stamped["workflow_key"], "hold_policy_test_workflow");
        assert_eq!(stamped["grace_ms"], HoldPolicy::default().grace_ms);
        assert!(stamped["steal_after_ms"].is_null());
    }

    #[tokio::test]
    async fn node_applies_event_override_and_named_profile() {
        let node = HoldPolicyNode::new("hold_policy_test_workflow");
        let ctx = TaskContext {
            event: serde_json::json!({
                "profile": "cheap-fast",
                "policy": { "grace_ms": 1234 },
            }),
            nodes: std::collections::HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: std::collections::HashMap::new(),
        };

        let result = node.process(ctx).await.expect("process should succeed");
        let stamped = get_result(&result, NODE_NAME).expect("result should be stamped");
        // event.policy.grace_ms (1234) beats the cheap-fast profile's
        // grace_ms (15000); cheap-fast's steal_after_ms (15000) survives
        // since the event override never touched that field.
        assert_eq!(stamped["grace_ms"], 1234);
        assert_eq!(stamped["steal_after_ms"], 15_000);
    }

    #[tokio::test]
    async fn node_reports_an_invalid_event_policy_override_as_an_error() {
        let node = HoldPolicyNode::new("hold_policy_test_workflow");
        let ctx = TaskContext {
            event: serde_json::json!({ "policy": { "grace_ms": "not-a-number" } }),
            nodes: std::collections::HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: std::collections::HashMap::new(),
        };

        let err = node
            .process(ctx)
            .await
            .expect_err("an invalid ctx.event.policy must error, not silently default");
        assert!(err.to_string().contains("invalid ctx.event.policy"));
    }
}
