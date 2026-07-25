//! Shared override-merge surface (EN.5.D task 1) that replaces the
//! per-workflow `merge_opt`/`merge_local`/`apply_override` trio duplicated
//! across `workflows::{sdlc_flow,research_agent,diagnostic_intake,
//! proposal_generator}::policy`. Nothing about merging a type's
//! all-optional `Partial` mirror onto itself should be written per workflow
//! again — this module is the one place that logic lives.
//!
//! This is the field-merge layer [`crate::policy::resolve::Policy::apply`]
//! composes for a concrete policy's fields; it is not a replacement for the
//! four-layer `builtin < harness < profile < event` precedence in
//! `policy::resolve`, which is unchanged and out of scope here.

use crate::policy::tier::LocalConfig;
use serde::{Deserialize, Serialize};

/// A type with an all-optional `Partial` mirror that can be merged onto a
/// base value of `Self`, field-by-field (`Some` in the override wins,
/// `None` falls through to `self`).
///
/// This is deliberately a separate trait from
/// [`crate::policy::resolve::Policy`] (which resolves the four-layer
/// precedence for a *whole* concrete policy type): `Overlay` is the smaller,
/// reusable "merge one partial onto one base" building block that a
/// `Policy::apply` impl composes per field, including for nested types like
/// [`LocalConfig`] that are not themselves a top-level resolvable policy.
pub trait Overlay: Sized {
    /// All-optional mirror of `Self` used by an override layer.
    type Partial;

    /// Merge one override layer onto `self`, field-by-field (`Some` in
    /// `over` wins, `None` falls through to `self`).
    #[must_use]
    fn overlay(self, over: &Self::Partial) -> Self;
}

/// Shared field-merge primitive: `higher` wins when present, otherwise
/// falls through to `lower`. Relocated from `policy::resolve::merge_opt`
/// (still re-exported there so no existing caller breaks) so every
/// `Overlay`/`Policy::apply` impl composes the same primitive.
#[must_use]
pub fn merge_opt<T>(lower: T, higher: Option<T>) -> T {
    higher.unwrap_or(lower)
}

/// All-optional mirror of [`LocalConfig`], used by the `local` override
/// layer in every workflow's policy. Byte-identical shape to the
/// per-workflow `PartialLocalConfig` types this replaces.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialLocalConfig {
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub constrained_json: Option<bool>,
}

/// Merge a `PartialLocalConfig` override onto a base [`LocalConfig`],
/// field-by-field. Free-function sibling of [`Overlay::overlay`] kept for
/// callers that want to merge a `LocalConfig` without importing the trait.
#[must_use]
pub fn merge_local(mut base: LocalConfig, over: &PartialLocalConfig) -> LocalConfig {
    if let Some(v) = &over.endpoint {
        base.endpoint = v.clone();
    }
    if let Some(v) = &over.model {
        base.model = v.clone();
    }
    if let Some(v) = over.constrained_json {
        base.constrained_json = v;
    }
    base
}

impl Overlay for LocalConfig {
    type Partial = PartialLocalConfig;

    fn overlay(self, over: &Self::Partial) -> Self {
        merge_local(self, over)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_opt_some_wins_over_lower() {
        assert_eq!(merge_opt(5, Some(9)), 9);
    }

    #[test]
    fn merge_opt_none_falls_through_to_lower() {
        assert_eq!(merge_opt(5, None), 5);
    }

    #[test]
    fn local_config_overlay_merges_field_by_field_without_clobbering_untouched_fields() {
        let base = LocalConfig {
            endpoint: "http://localhost:11434".to_string(),
            model: "qwen2.5-coder:7b".to_string(),
            constrained_json: false,
        };
        let over = PartialLocalConfig {
            endpoint: Some("http://localhost:8080".to_string()),
            model: None,
            constrained_json: Some(true),
        };

        let merged = base.clone().overlay(&over);

        assert_eq!(merged.endpoint, "http://localhost:8080");
        // Untouched field falls through to the base.
        assert_eq!(merged.model, base.model);
        assert!(merged.constrained_json);
    }

    #[test]
    fn local_config_overlay_with_all_none_partial_is_a_no_op() {
        let base = LocalConfig::default();
        let over = PartialLocalConfig::default();

        let merged = base.clone().overlay(&over);

        assert_eq!(merged, base);
    }

    #[test]
    fn merge_local_free_function_matches_overlay_trait_impl() {
        let base = LocalConfig::default();
        let over = PartialLocalConfig {
            model: Some("custom-model:1b".to_string()),
            ..Default::default()
        };

        let via_free_fn = merge_local(base.clone(), &over);
        let via_trait = base.overlay(&over);

        assert_eq!(via_free_fn, via_trait);
    }
}
