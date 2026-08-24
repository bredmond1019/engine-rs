//! `DeliverableRenderPolicy` / `PartialDeliverableRenderPolicy` and the
//! `Policy` trait delegation to `crate::policy::resolve::resolve`.
//!
//! **Resolution — four layers, high-to-low precedence:** per-run event
//! `policy` override, then a named `profile:` bundle, then
//! `planning/harness.json` `deliverable_render.policy` defaults, then
//! built-in defaults.
//!
//! This workflow is largely policy-free — `RenderDeliverableNode`'s
//! four-section render and `RenderPdfNode`'s `typst` invocation are both
//! deterministic. The one real knob (CLAUDE.md standing rule 6) is the
//! **optional model-polish pass** over the rendered markdown, run behind a
//! `with_transport` seam on `RenderDeliverableNode` (mirroring
//! `sdlc_flow`'s injectable-transport pattern). The built-in
//! [`DeliverableRenderPolicy::default`] keeps the polish pass **off** —
//! adding this knob must not change what an existing run produces.

use serde::{Deserialize, Serialize};

use crate::policy::merge_opt;
pub use crate::policy::tier::ModelTier;

/// The fully-resolved, per-run Deliverable Render policy — the merge of
/// built-in defaults, `harness.json`'s `deliverable_render.policy`
/// defaults, a named `profile`, and any per-run event override, high->low
/// precedence in that order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliverableRenderPolicy {
    /// Whether the optional model-polish pass runs over the rendered
    /// markdown before it is written to disk. `false` restores the plain
    /// deterministic render exactly.
    pub polish_enabled: bool,
    /// Model tier the polish pass runs at, when `polish_enabled` is `true`.
    /// Unused (but still resolved, so it is always discoverable) when the
    /// polish pass is off.
    pub polish_model_tier: ModelTier,
}

impl Default for DeliverableRenderPolicy {
    /// The safe, behavior-stable default: the polish pass is off, so a run
    /// under the built-in default produces exactly the deterministic render.
    fn default() -> Self {
        Self {
            polish_enabled: false,
            polish_model_tier: ModelTier::Sonnet,
        }
    }
}

/// All-optional mirror of [`DeliverableRenderPolicy`] used by the override
/// layers (`harness.json`'s `deliverable_render.policy`, a named `profile`,
/// and a per-run event's `policy` field). Every field left `None` falls
/// through to the next-lower-precedence layer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialDeliverableRenderPolicy {
    pub polish_enabled: Option<bool>,
    pub polish_model_tier: Option<ModelTier>,
}

impl crate::policy::Policy for DeliverableRenderPolicy {
    type Partial = PartialDeliverableRenderPolicy;

    /// Apply one override layer on top of `self`, field-by-field (`Some` in
    /// `over` wins, `None` falls through to `self`).
    fn apply(self, over: &PartialDeliverableRenderPolicy) -> Self {
        DeliverableRenderPolicy {
            polish_enabled: merge_opt(self.polish_enabled, over.polish_enabled),
            polish_model_tier: merge_opt(self.polish_model_tier, over.polish_model_tier),
        }
    }
}

/// Resolve the four policy layers into one concrete
/// [`DeliverableRenderPolicy`], high->low precedence: `event_override` beats
/// `profile` beats `harness_defaults` beats `builtin`. Delegates to the
/// generic `crate::policy::resolve::resolve` now that
/// `DeliverableRenderPolicy` implements `crate::policy::Policy`.
///
/// `builtin` is normally `DeliverableRenderPolicy::default()`, taken as a
/// parameter so callers/tests can exercise the merge against any base.
/// `profile` is a named bundle (see `profiles::profile_by_name` and
/// `harness.json`'s `deliverable_render.profiles`) resolved by the caller
/// before this function runs.
#[must_use]
pub fn resolve(
    builtin: DeliverableRenderPolicy,
    harness_defaults: Option<&PartialDeliverableRenderPolicy>,
    profile: Option<&PartialDeliverableRenderPolicy>,
    event_override: Option<&PartialDeliverableRenderPolicy>,
) -> DeliverableRenderPolicy {
    crate::policy::resolve::resolve(builtin, harness_defaults, profile, event_override)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_default_has_polish_disabled() {
        let policy = DeliverableRenderPolicy::default();
        assert!(!policy.polish_enabled);
        assert_eq!(policy.polish_model_tier, ModelTier::Sonnet);
    }

    #[test]
    fn resolve_with_no_overrides_returns_builtin() {
        let resolved = resolve(DeliverableRenderPolicy::default(), None, None, None);
        assert_eq!(resolved, DeliverableRenderPolicy::default());
    }

    #[test]
    fn harness_default_overrides_builtin_for_polish_model_tier() {
        let harness = PartialDeliverableRenderPolicy {
            polish_model_tier: Some(ModelTier::Haiku),
            ..Default::default()
        };
        let resolved = resolve(
            DeliverableRenderPolicy::default(),
            Some(&harness),
            None,
            None,
        );
        assert_eq!(resolved.polish_model_tier, ModelTier::Haiku);
        // Untouched knob still falls through to builtin.
        assert!(!resolved.polish_enabled);
    }

    #[test]
    fn profile_beats_harness_defaults_for_polish_enabled() {
        let harness = PartialDeliverableRenderPolicy {
            polish_enabled: Some(false),
            ..Default::default()
        };
        let profile = PartialDeliverableRenderPolicy {
            polish_enabled: Some(true),
            ..Default::default()
        };
        let resolved = resolve(
            DeliverableRenderPolicy::default(),
            Some(&harness),
            Some(&profile),
            None,
        );
        assert!(resolved.polish_enabled);
    }

    #[test]
    fn event_override_beats_profile_for_polish_enabled() {
        let profile = PartialDeliverableRenderPolicy {
            polish_enabled: Some(true),
            ..Default::default()
        };
        let event = PartialDeliverableRenderPolicy {
            polish_enabled: Some(false),
            ..Default::default()
        };
        let resolved = resolve(
            DeliverableRenderPolicy::default(),
            None,
            Some(&profile),
            Some(&event),
        );
        assert!(!resolved.polish_enabled);
    }

    #[test]
    fn deserializes_partial_policy_from_harness_json_shape() {
        let json = r#"{ "polish_enabled": true, "polish_model_tier": "haiku" }"#;
        let partial: PartialDeliverableRenderPolicy =
            serde_json::from_str(json).expect("valid PartialDeliverableRenderPolicy JSON");
        assert_eq!(partial.polish_enabled, Some(true));
        assert_eq!(partial.polish_model_tier, Some(ModelTier::Haiku));
    }
}
