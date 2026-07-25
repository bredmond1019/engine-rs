//! `engine-core::policy` — the generic run-policy/observability framework
//! (EN.4.0), generalized out of `workflows::sdlc_flow`'s
//! `SdlcPolicy`/telemetry/aggregation machinery so any workflow can reuse
//! the same tier types, four-layer `resolve<P>` precedence, model-node
//! shaping helpers, telemetry harvest, and aggregation.
//!
//! `sdlc_flow` remains the sole owner of `CommandRunner`/
//! `default_command_runner` (subprocess execution) and its four concrete
//! named profiles — this module only generalizes the *mechanism*.

pub mod aggregate;
pub mod emit_state;
pub mod overlay;
pub mod profiles;
pub mod resolve;
pub mod shaping;
pub mod telemetry;
pub mod tier;

pub use aggregate::{aggregate, aggregate_state_files, extract_policy_telemetry, PolicyAggregate};
pub use emit_state::{CommandOutputLike, EmitStateNode, Runner as EmitStateRunner};
pub use overlay::{merge_local, Overlay, PartialLocalConfig};
pub use profiles::{
    read_harness_policy_defaults, read_harness_policy_defaults_from, read_harness_profiles,
    read_harness_profiles_from, resolve_profile, resolve_profile_from, resolved_policy,
    resolved_policy_strict, stamp_resolved_policy, PolicyConfigSource, RESOLVED_POLICY_IDENTITY,
};
pub use resolve::{merge_opt, resolve, Policy};
pub use shaping::{apply_model_tier, apply_prompt_cache, apply_verbosity_directive};
pub use telemetry::{harvest as harvest_telemetry, RunTelemetry, RunTelemetryInputs};
pub use tier::{model_tier_to_model_string, LocalConfig, ModelTier, OutputVerbosity};
