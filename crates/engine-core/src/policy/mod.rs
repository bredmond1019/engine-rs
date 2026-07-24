//! `engine-core::policy` — the generic run-policy/observability framework
//! (EN.4.0), generalized out of `workflows::sdlc_flow`'s
//! `SdlcPolicy`/telemetry/aggregation machinery so any workflow can reuse
//! the same tier types, four-layer `resolve<P>` precedence, model-node
//! shaping helpers, telemetry harvest, and aggregation.
//!
//! `sdlc_flow` remains the sole owner of `CommandRunner`/
//! `default_command_runner` (subprocess execution) and its four concrete
//! named profiles — this module only generalizes the *mechanism*.

pub mod resolve;
pub mod shaping;
pub mod tier;

pub use resolve::{merge_opt, resolve, Policy};
pub use shaping::{apply_model_tier, apply_prompt_cache, apply_verbosity_directive};
pub use tier::{model_tier_to_model_string, LocalConfig, ModelTier, OutputVerbosity};
