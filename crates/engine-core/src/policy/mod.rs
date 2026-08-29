//! `engine-core::policy` — the generic run-policy/observability framework
//! (EN.4.0), generalized out of `workflows::sdlc_flow`'s
//! `SdlcPolicy`/telemetry/aggregation machinery so any workflow can reuse
//! the same tier types, four-layer `resolve<P>` precedence, model-node
//! shaping helpers, telemetry harvest, and aggregation.
//!
//! `sdlc_flow` remains the sole owner of `CommandRunner`/
//! `default_command_runner` (subprocess execution) and its four concrete
//! named profiles — this module only generalizes the *mechanism*. The
//! non-overridable command-policy safety floor (`command_floor`) lives here
//! instead, lifted out of `workflows::sdlc_flow` (`EN.11.M` task 1) so a
//! second engine, and `nodes::terminal::send`, can reach it without
//! importing `sdlc_flow`.

pub mod aggregate;
pub mod command_floor;
pub mod emit_state;
pub mod overlay;
pub mod permission;
pub mod profiles;
pub mod resolve;
pub mod shaping;
pub mod telemetry;
pub mod tier;

pub use aggregate::{aggregate, aggregate_state_files, extract_policy_telemetry, PolicyAggregate};
pub use emit_state::{CommandOutputLike, EmitStateNode, Runner as EmitStateRunner};
pub use overlay::{merge_local, Overlay, PartialLocalConfig};
pub use permission::{
    decide, resolve_permission_profile, resolve_permission_profile_from_config, Decision,
    GatedAction, PermissionProfile, ProfileResolutionError, DEFAULT_PROFILE,
};
pub use profiles::{
    read_harness_policy_defaults, read_harness_policy_defaults_from, read_harness_profiles,
    read_harness_profiles_from, resolve_profile, resolve_profile_from, resolved_policy_strict,
    stamp_resolved_policy, PolicyConfigSource, RESOLVED_POLICY_IDENTITY,
};
pub use resolve::{merge_opt, resolve, Policy};
pub use shaping::{
    apply_call_timeout, apply_model_tier, apply_prompt_cache, apply_verbosity_directive,
};
pub use telemetry::{
    harvest as harvest_telemetry, observed_model_tiers, RunTelemetry, RunTelemetryInputs,
};
pub use tier::{model_tier_to_model_string, LocalConfig, ModelTier, OutputVerbosity};

/// Test-only helpers shared by this crate's policy-golden unit tests.
///
/// Gated on `#[cfg(test)]`, so it exists only in the crate's unit-test
/// build. That is sufficient for every current call site — both goldens
/// (`workflows::sdlc_flow::policy` and `workflows::sdlc_flow::schema`) are
/// *unit*-test modules compiled into `engine-core` itself. It is
/// deliberately NOT reachable from `tests/it/` integration binaries, which
/// link the non-test build of the lib; if an integration test ever needs
/// this, drop the `#[cfg(test)]` and add `#[allow(dead_code)]`.
#[cfg(test)]
pub(crate) mod test_support {
    use serde_json::Value;

    /// Assert that `expected` is a recursive *subset* of `actual`.
    ///
    /// Every key present in `expected` must be present in `actual` with an
    /// equal value; keys `actual` carries that `expected` lacks are ignored.
    /// This is what makes the policy goldens **additive-tolerant**: a new
    /// `SdlcPolicy` field shows up only in `actual` and is skipped, while
    /// every already-pinned value still has to match exactly.
    ///
    /// The tolerance is deliberately one-directional and shallow in exactly
    /// one respect:
    ///
    /// - **Direction matters.** `expected ⊆ actual`, never the reverse. A
    ///   field disappearing from the serialized shape is real drift and
    ///   still fails.
    /// - **Arrays are strict.** Equal length is required and elements are
    ///   compared by index — a policy list shrinking, growing, or reordering
    ///   IS drift.
    /// - **Scalars are strict.** Any leaf mismatch fails.
    /// - **Field order is not checked.** That is the one property given up
    ///   relative to a byte-identical comparison, and nothing depends on it.
    ///
    /// Panics name the diverging JSON path (e.g. `policy.model_tiers.review`)
    /// so a failure points straight at the offending knob.
    pub(crate) fn assert_json_subset(expected: &Value, actual: &Value) {
        let mut path = String::new();
        assert_subset_at(expected, actual, &mut path);
    }

    fn assert_subset_at(expected: &Value, actual: &Value, path: &mut String) {
        let here: String = if path.is_empty() {
            "<root>".to_string()
        } else {
            path.clone()
        };
        match (expected, actual) {
            (Value::Object(exp), Value::Object(act)) => {
                for (key, exp_val) in exp {
                    let Some(act_val) = act.get(key) else {
                        panic!(
                            "JSON subset mismatch at `{}`: key `{key}` missing from actual \
                             (actual keys: {:?})",
                            here,
                            act.keys().collect::<Vec<_>>()
                        );
                    };
                    let restore = path.len();
                    if !path.is_empty() {
                        path.push('.');
                    }
                    path.push_str(key);
                    assert_subset_at(exp_val, act_val, path);
                    path.truncate(restore);
                }
            }
            (Value::Array(exp), Value::Array(act)) => {
                assert_eq!(
                    exp.len(),
                    act.len(),
                    "JSON subset mismatch at `{here}`: array length differs \
                     (expected {}, actual {})",
                    exp.len(),
                    act.len()
                );
                for (idx, (exp_val, act_val)) in exp.iter().zip(act.iter()).enumerate() {
                    let restore = path.len();
                    path.push_str(&format!("[{idx}]"));
                    assert_subset_at(exp_val, act_val, path);
                    path.truncate(restore);
                }
            }
            (exp, act) => {
                assert_eq!(
                    exp, act,
                    "JSON subset mismatch at `{here}`: expected {exp}, actual {act}"
                );
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::assert_json_subset;
        use serde_json::json;

        #[test]
        fn equal_objects_pass() {
            let v = json!({"a": 1, "b": {"c": "x"}, "d": [1, 2]});
            assert_json_subset(&v, &v.clone());
        }

        #[test]
        fn extra_keys_in_actual_pass() {
            let expected = json!({"a": 1, "nested": {"c": "x"}});
            let actual = json!({"a": 1, "z": true, "nested": {"c": "x", "new_knob": null}});
            assert_json_subset(&expected, &actual);
        }

        #[test]
        #[should_panic(expected = "at `nested`: key `c` missing")]
        fn missing_key_panics_with_path() {
            let expected = json!({"nested": {"c": "x"}});
            let actual = json!({"nested": {"other": 1}});
            assert_json_subset(&expected, &actual);
        }

        #[test]
        #[should_panic(expected = "at `<root>`: key `a` missing")]
        fn missing_root_key_panics_with_root_path() {
            assert_json_subset(&json!({"a": 1}), &json!({"b": 1}));
        }

        #[test]
        #[should_panic(expected = "at `policy.model_tiers.review`")]
        fn differing_leaf_panics_with_path() {
            let expected = json!({"policy": {"model_tiers": {"review": "local"}}});
            let actual = json!({"policy": {"model_tiers": {"review": "sonnet"}}});
            assert_json_subset(&expected, &actual);
        }

        #[test]
        #[should_panic(expected = "at `list`: array length differs")]
        fn array_length_mismatch_panics() {
            assert_json_subset(&json!({"list": [1, 2, 3]}), &json!({"list": [1, 2]}));
        }

        #[test]
        #[should_panic(expected = "at `list[1].k`")]
        fn array_element_mismatch_panics_with_index_path() {
            let expected = json!({"list": [{"k": 1}, {"k": 2}]});
            let actual = json!({"list": [{"k": 1}, {"k": 99}]});
            assert_json_subset(&expected, &actual);
        }

        #[test]
        #[should_panic(expected = "array length differs")]
        fn array_growth_is_drift_not_tolerated() {
            assert_json_subset(&json!([1]), &json!([1, 2]));
        }

        #[test]
        #[should_panic(expected = "at `a`")]
        fn type_change_panics() {
            assert_json_subset(&json!({"a": {"b": 1}}), &json!({"a": 5}));
        }
    }
}
