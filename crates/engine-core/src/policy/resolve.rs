//! The generic `Policy` trait + four-layer `resolve<P>` merge, lifted from
//! `workflows::sdlc_flow::policy::resolve` (EN.4.0 task 1). Any workflow's
//! concrete policy type implements [`Policy`] (its `Partial` mirror +
//! field-by-field `apply`) to get the same
//! `builtin < harness_defaults < profile < event_override` precedence
//! `SdlcPolicy` already had.

/// A resolvable run policy: a concrete type `P` plus an all-optional
/// `Partial` mirror used by override layers, and an `apply` that merges one
/// override layer onto a base policy (`Some` in the override wins, `None`
/// falls through to `self`).
pub trait Policy: Sized {
    /// All-optional mirror of `Self` used by the override layers
    /// (`harness_defaults`, `profile`, `event_override`).
    type Partial;

    /// Apply one override layer on top of `self`, field-by-field.
    #[must_use]
    fn apply(self, over: &Self::Partial) -> Self;
}

/// Resolve the four policy layers into one concrete `P`, high->low
/// precedence: `event_override` > `profile` > `harness_defaults` >
/// `builtin`.
///
/// `builtin` is normally `P::default()`, taken as a parameter so
/// callers/tests can exercise the merge against any base. `profile` is a
/// named bundle resolved by the caller before this function runs.
#[must_use]
pub fn resolve<P: Policy>(
    builtin: P,
    harness_defaults: Option<&P::Partial>,
    profile: Option<&P::Partial>,
    event_override: Option<&P::Partial>,
) -> P {
    let mut resolved = builtin;
    if let Some(harness) = harness_defaults {
        resolved = resolved.apply(harness);
    }
    if let Some(profile) = profile {
        resolved = resolved.apply(profile);
    }
    if let Some(event) = event_override {
        resolved = resolved.apply(event);
    }
    resolved
}

/// Shared field-merge primitive: `higher` wins when present, otherwise
/// falls through to `lower`. The building block every `Policy::apply` impl
/// composes for its scalar fields.
#[must_use]
pub fn merge_opt<T>(lower: T, higher: Option<T>) -> T {
    higher.unwrap_or(lower)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    struct TestPolicy {
        retries: u32,
        verbose: bool,
    }

    #[derive(Debug, Clone, Default)]
    struct PartialTestPolicy {
        retries: Option<u32>,
        verbose: Option<bool>,
    }

    impl Policy for TestPolicy {
        type Partial = PartialTestPolicy;

        fn apply(self, over: &Self::Partial) -> Self {
            Self {
                retries: merge_opt(self.retries, over.retries),
                verbose: merge_opt(self.verbose, over.verbose),
            }
        }
    }

    #[test]
    fn resolve_with_no_overrides_returns_builtin() {
        let resolved = resolve(TestPolicy::default(), None, None, None);
        assert_eq!(resolved, TestPolicy::default());
    }

    #[test]
    fn resolve_precedence_event_beats_profile_beats_harness_beats_builtin() {
        let harness = PartialTestPolicy {
            retries: Some(1),
            verbose: Some(true),
        };
        let profile = PartialTestPolicy {
            retries: Some(2),
            verbose: None,
        };
        let event = PartialTestPolicy {
            retries: Some(3),
            verbose: None,
        };
        let resolved = resolve(
            TestPolicy::default(),
            Some(&harness),
            Some(&profile),
            Some(&event),
        );
        // event wins for `retries`.
        assert_eq!(resolved.retries, 3);
        // profile and event left `verbose` untouched, so harness's value
        // survives.
        assert!(resolved.verbose);
    }

    #[test]
    fn merge_opt_falls_through_on_none() {
        assert_eq!(merge_opt(5, None), 5);
        assert_eq!(merge_opt(5, Some(9)), 9);
    }
}
