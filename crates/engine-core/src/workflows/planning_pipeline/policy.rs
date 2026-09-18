//! `ApprovalGatePolicy` / `PartialApprovalGatePolicy` — the per-stage
//! approval knobs `PLANNING_PIPELINE`'s `ApprovalGateNode` (`EN.19.D` task
//! 4) reads, resolved through the standard four-layer precedence (per-run
//! event `policy` override > named `profile` bundle > `planning/
//! harness.json` `planning_pipeline.policy` defaults > built-in default)
//! per `CLAUDE.md` standing rule 6 — mirroring
//! [`crate::workflows::approve_and_run::policy`]'s exact shape rather than
//! inventing a new resolution pattern.
//!
//! Three knobs:
//!
//! - `approval` — the per-stage `{pre_plan, plan, generate_tasks, dispatch}
//!   -> auto | manual` map `ApprovalGateNode` reads for the stage that just
//!   completed, via [`ApprovalGatePolicy::mode_for_stage`]. A stage absent
//!   from the map resolves to [`ApprovalMode::Manual`] — **safe by
//!   default**: an event naming no `approval` map at all pauses after every
//!   requested stage, never silently auto-continuing (acceptance criterion:
//!   "Dispatching with an event naming no `approval` map at all pauses
//!   after EVERY requested stage").
//! - `approval_channel` — which [`crate::operator::channel::OperatorChannel`]
//!   variant a `manual` gate enqueues under: `notification` (headless) or
//!   `session` (interactive, the split `EN.19.E`'s terminal-nodes
//!   composition keys off of).
//! - `max_discussion_rounds` — the bound `EN.19.E`'s discuss-and-loop-back
//!   cycle (wired through `crate::loop_combinator::{build_loop, LoopSpec}`
//!   in task 7's graph, per the block record's 2026-09-15 amendment) feeds
//!   as `LoopSpec::max_iterations`, so a `discuss` verdict cannot bounce the
//!   gate forever.
//!
//! Like `ApproveAndRunPolicy`, this workflow drives no `AgentCodeStep`
//! directly from this policy, so there is no `ModelTiers`/`LocalConfig`
//! here.
//!
//! Every built-in default is behavior-stable in the only sense that applies
//! to a brand-new surface: an unset knob reproduces these exact values, so
//! introducing the knob changes nothing about a run that never overrides
//! it.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::policy::{merge_opt, Policy};

/// The `harness.json` section key this workflow's policy/profiles live
/// under (`planning_pipeline.policy` / `planning_pipeline.profiles`) —
/// mirrors `approve_and_run::profiles::WORKFLOW_KEY`.
pub const WORKFLOW_KEY: &str = "planning_pipeline";

/// Whether a completed stage's `ApprovalGateNode` pauses for an operator
/// tap (`Manual`, the safe-by-default) or passes straight through
/// (`Auto`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    Auto,
    /// Safe by default: a stage with no explicit entry in the `approval`
    /// map pauses rather than silently continuing.
    #[default]
    Manual,
}

/// Which `OperatorChannel` variant a `manual` gate enqueues its
/// `OperatorPayload` under — the headless/interactive split.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalChannel {
    #[default]
    Notification,
    Session,
}

/// The fully-resolved, per-run `PLANNING_PIPELINE` approval-gate policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalGatePolicy {
    /// Per-stage `approval_mode`, keyed by stage name (`"pre_plan"`,
    /// `"plan"`, `"generate_tasks"`, `"dispatch"`). A stage absent from
    /// this map resolves to [`ApprovalMode::Manual`] via
    /// [`Self::mode_for_stage`] — never treated as `Auto` by omission.
    pub approval: HashMap<String, ApprovalMode>,
    /// Which `OperatorChannel` a `manual` gate enqueues under.
    pub approval_channel: ApprovalChannel,
    /// Hard cap on `EN.19.E`'s discuss-and-loop-back cycle
    /// (`LoopSpec::max_iterations`), mirroring
    /// `proposal_generator::graph::REVISE_LOOP_MAX_ITERATIONS`'s precedent
    /// of `3`.
    pub max_discussion_rounds: u32,
}

impl ApprovalGatePolicy {
    /// The resolved [`ApprovalMode`] for `stage`, falling through to
    /// [`ApprovalMode::Manual`] when `stage` has no entry in `approval` —
    /// the safe-by-default lookup every `ApprovalGateNode` call goes
    /// through instead of indexing the map directly.
    #[must_use]
    pub fn mode_for_stage(&self, stage: &str) -> ApprovalMode {
        self.approval.get(stage).copied().unwrap_or_default()
    }
}

impl Default for ApprovalGatePolicy {
    /// Behavior-stable baseline: no stage pre-populated in `approval` (so
    /// every stage resolves to `Manual` via [`ApprovalGatePolicy::mode_for_stage`]),
    /// `notification` as the default channel (the headless case), and a
    /// discussion cap of `3` rounds.
    fn default() -> Self {
        Self {
            approval: HashMap::new(),
            approval_channel: ApprovalChannel::Notification,
            max_discussion_rounds: 3,
        }
    }
}

/// All-optional mirror of [`ApprovalGatePolicy`] used by the override
/// layers (`harness.json`'s `planning_pipeline.policy`, a named `profile`,
/// and a per-run event's `policy` field). Every field left `None` falls
/// through to the next-lower-precedence layer.
///
/// `approval` overrides as a whole map, not a per-key merge — the same
/// field-by-field (not deep-merge) semantics every other `Partial*Policy`
/// in this crate uses. A layer that wants to change one stage's mode
/// while keeping a lower layer's other stages must restate the whole map.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialApprovalGatePolicy {
    pub approval: Option<HashMap<String, ApprovalMode>>,
    pub approval_channel: Option<ApprovalChannel>,
    pub max_discussion_rounds: Option<u32>,
}

impl Policy for ApprovalGatePolicy {
    type Partial = PartialApprovalGatePolicy;

    /// Apply one override layer on top of `self`, field-by-field (`Some` in
    /// `over` wins, `None` falls through to `self`).
    fn apply(self, over: &PartialApprovalGatePolicy) -> Self {
        Self {
            approval: merge_opt(self.approval, over.approval.clone()),
            approval_channel: merge_opt(self.approval_channel, over.approval_channel),
            max_discussion_rounds: merge_opt(
                self.max_discussion_rounds,
                over.max_discussion_rounds,
            ),
        }
    }
}

/// Serialize `policy` into the plain JSON object task 7's graph wiring
/// stamps into `ctx.nodes` via [`crate::policy::stamp_resolved_policy`] —
/// deliberately a thin, `cost_usd`-free serialization, mirroring
/// `approve_and_run::policy::policy_state`.
#[must_use]
pub fn policy_state(policy: &ApprovalGatePolicy) -> serde_json::Value {
    serde_json::to_value(policy).expect("ApprovalGatePolicy always serializes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_contract::TaskContext;

    #[test]
    fn builtin_default_is_behavior_stable_baseline() {
        let policy = ApprovalGatePolicy::default();
        assert!(policy.approval.is_empty());
        assert_eq!(policy.approval_channel, ApprovalChannel::Notification);
        assert_eq!(policy.max_discussion_rounds, 3);
    }

    #[test]
    fn resolve_with_no_overrides_returns_builtin() {
        let resolved = crate::policy::resolve(ApprovalGatePolicy::default(), None, None, None);
        assert_eq!(resolved, ApprovalGatePolicy::default());
    }

    /// Acceptance criterion: "An event naming no `approval` map resolves
    /// every stage to `manual`."
    #[test]
    fn absent_approval_map_resolves_every_stage_to_manual() {
        let resolved = crate::policy::resolve(ApprovalGatePolicy::default(), None, None, None);
        for stage in ["pre_plan", "plan", "generate_tasks", "dispatch"] {
            assert_eq!(resolved.mode_for_stage(stage), ApprovalMode::Manual);
        }
        // An entirely unknown stage name still resolves safely to Manual,
        // never Auto by omission.
        assert_eq!(
            resolved.mode_for_stage("not_a_real_stage"),
            ApprovalMode::Manual
        );
    }

    #[test]
    fn harness_default_overrides_builtin_for_approval_channel() {
        let harness = PartialApprovalGatePolicy {
            approval_channel: Some(ApprovalChannel::Session),
            ..Default::default()
        };
        let resolved =
            crate::policy::resolve(ApprovalGatePolicy::default(), Some(&harness), None, None);
        assert_eq!(resolved.approval_channel, ApprovalChannel::Session);
        // Untouched knobs still fall through to builtin.
        assert!(resolved.approval.is_empty());
        assert_eq!(resolved.max_discussion_rounds, 3);
    }

    #[test]
    fn profile_beats_harness_defaults_for_max_discussion_rounds() {
        let harness = PartialApprovalGatePolicy {
            max_discussion_rounds: Some(1),
            ..Default::default()
        };
        let profile = PartialApprovalGatePolicy {
            max_discussion_rounds: Some(5),
            ..Default::default()
        };
        let resolved = crate::policy::resolve(
            ApprovalGatePolicy::default(),
            Some(&harness),
            Some(&profile),
            None,
        );
        assert_eq!(resolved.max_discussion_rounds, 5);
    }

    #[test]
    fn event_override_beats_profile_for_approval_map() {
        let mut profile_map = HashMap::new();
        profile_map.insert("pre_plan".to_string(), ApprovalMode::Auto);
        let profile = PartialApprovalGatePolicy {
            approval: Some(profile_map),
            ..Default::default()
        };

        let mut event_map = HashMap::new();
        event_map.insert("pre_plan".to_string(), ApprovalMode::Manual);
        event_map.insert("plan".to_string(), ApprovalMode::Auto);
        let event = PartialApprovalGatePolicy {
            approval: Some(event_map),
            ..Default::default()
        };

        let resolved = crate::policy::resolve(
            ApprovalGatePolicy::default(),
            None,
            Some(&profile),
            Some(&event),
        );
        assert_eq!(resolved.mode_for_stage("pre_plan"), ApprovalMode::Manual);
        assert_eq!(resolved.mode_for_stage("plan"), ApprovalMode::Auto);
        // generate_tasks was in neither override's map — still Manual.
        assert_eq!(
            resolved.mode_for_stage("generate_tasks"),
            ApprovalMode::Manual
        );
    }

    #[test]
    fn deserializes_partial_policy_from_harness_json_shape() {
        let json = r#"{
            "approval": {"pre_plan": "auto", "plan": "manual"},
            "approval_channel": "session",
            "max_discussion_rounds": 4
        }"#;
        let partial: PartialApprovalGatePolicy =
            serde_json::from_str(json).expect("valid PartialApprovalGatePolicy JSON");
        assert_eq!(
            partial.approval.as_ref().and_then(|m| m.get("pre_plan")),
            Some(&ApprovalMode::Auto)
        );
        assert_eq!(partial.approval_channel, Some(ApprovalChannel::Session));
        assert_eq!(partial.max_discussion_rounds, Some(4));
    }

    #[test]
    fn partial_policy_round_trips_with_fields_absent() {
        let partial: PartialApprovalGatePolicy =
            serde_json::from_str("{}").expect("valid empty PartialApprovalGatePolicy JSON");
        assert_eq!(partial, PartialApprovalGatePolicy::default());
    }

    #[test]
    fn policy_state_round_trips_and_carries_no_cost_usd_key() {
        let policy = ApprovalGatePolicy::default();
        let state = policy_state(&policy);
        assert!(
            state.get("cost_usd").is_none(),
            "policy_state must never emit a cost_usd key"
        );
        let round_tripped: ApprovalGatePolicy =
            serde_json::from_value(state).expect("policy_state output deserializes back");
        assert_eq!(round_tripped, policy);
    }

    /// Acceptance criterion: "Resolved values are stamped into
    /// `ctx.nodes`." Exercises the same `stamp_resolved_policy` round-trip
    /// `sdlc_flow::policy`'s
    /// `resolved_policy_stamp_round_trips_the_payload_cap_via_invocations_reader`
    /// test proves for its own policy type.
    #[test]
    fn resolved_policy_stamps_into_ctx_nodes() {
        let mut approval = HashMap::new();
        approval.insert("pre_plan".to_string(), ApprovalMode::Auto);
        let event = PartialApprovalGatePolicy {
            approval: Some(approval),
            approval_channel: Some(ApprovalChannel::Session),
            max_discussion_rounds: Some(7),
        };
        let resolved =
            crate::policy::resolve(ApprovalGatePolicy::default(), None, None, Some(&event));

        let mut ctx = TaskContext {
            event: serde_json::json!({}),
            nodes: Default::default(),
            metadata: serde_json::json!({}),
            node_runs: Default::default(),
        };
        crate::policy::stamp_resolved_policy(&mut ctx, &resolved).expect("stamp should succeed");

        let stamped = ctx
            .nodes
            .get(crate::policy::RESOLVED_POLICY_IDENTITY)
            .and_then(serde_json::Value::as_object)
            .expect("ResolvedPolicy stamp is a JSON object");
        assert_eq!(
            stamped.get("approval_channel"),
            Some(&serde_json::json!("session"))
        );
        assert_eq!(
            stamped.get("max_discussion_rounds"),
            Some(&serde_json::json!(7))
        );

        let read_back: ApprovalGatePolicy = crate::policy::resolved_policy_strict(&ctx)
            .expect("resolved policy reads back through the strict reader");
        assert_eq!(read_back, resolved);
    }
}
