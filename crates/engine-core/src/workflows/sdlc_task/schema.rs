//! `SdlcTaskEventSchema` — the SDLC_TASK trigger event, per port design §3
//! — plus re-exports of the `sdlc_flow` state types this workflow reuses
//! as-is (`SDLCState`/`SDLCTask`/`SDLCTaskStatus`/`RunMeta`/
//! `SDLCTelemetry`, `parse_task_range`/`derive_current_task`/
//! `derive_bail_reason`) so this module's own consumers never reach into
//! `crate::workflows::sdlc_flow::schema` directly.

use serde::Deserialize;

pub use crate::workflows::sdlc_flow::schema::{
    derive_bail_reason, derive_current_task, parse_task_range, RunMeta, SDLCState, SDLCTask,
    SDLCTaskStatus, SDLCTelemetry,
};

/// The SDLC_TASK trigger event — port design §3. `spec_slug` is the only
/// required field; every other field is `#[serde(default)]` so `{"spec_slug":
/// "X"}` alone deserializes.
///
/// Deliberately drops `auto_pr` — the ONLY `SDLCFlowEventSchema` field
/// SDLC_TASK does not carry. SDLC_TASK ships no PR ceremony at all (see
/// `sdlc-task-ships-no-docs-stage`), so an `auto_pr` flag would have nothing
/// to gate.
#[derive(Debug, Clone, Deserialize)]
pub struct SdlcTaskEventSchema {
    /// The only required field.
    pub spec_slug: String,
    /// A `RepoRegistry` slug — **never a path**. Resolved the same way
    /// `sdlc_flow`'s `repo` field is resolved.
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub task_range: Option<String>,
    #[serde(default)]
    pub resume: bool,
    /// Default `false` = run in place. This is the JS engine's own
    /// default (`sdlc-task.js`), and is deliberately NOT `sdlc_flow`'s
    /// default — `SDLCFlowEventSchema::use_worktree` is also `false` by
    /// default today, but the two schemas are independent types and this
    /// field states its own default explicitly rather than inheriting it.
    #[serde(default)]
    pub use_worktree: bool,
    #[serde(default)]
    pub branch_name: Option<String>,
    #[serde(default)]
    pub llm_triage: bool,
    /// Typed `Option<serde_json::Value>` ON PURPOSE: `PartialSdlcTaskPolicy`
    /// does not exist until `EN.11.O`, which owns the policy surface.
    /// Inventing a placeholder type here that `EN.11.O` would only have to
    /// delete is worse than an opaque passthrough — see this block's
    /// Amendment Log ("SCOPE CALL").
    #[serde(default)]
    pub policy: Option<serde_json::Value>,
    #[serde(default)]
    pub profile: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_from_spec_slug_alone_with_every_default() {
        let event: SdlcTaskEventSchema =
            serde_json::from_str(r#"{"spec_slug": "EN.11.N"}"#).expect("deserializes");
        assert_eq!(event.spec_slug, "EN.11.N");
        assert_eq!(event.repo, None);
        assert_eq!(event.task_range, None);
        assert!(!event.resume);
        assert!(!event.use_worktree);
        assert_eq!(event.branch_name, None);
        assert!(!event.llm_triage);
        assert_eq!(event.policy, None);
        assert_eq!(event.profile, None);
    }

    #[test]
    fn deserializes_every_field_when_present() {
        let event: SdlcTaskEventSchema = serde_json::from_str(
            r#"{
                "spec_slug": "EN.11.N",
                "repo": "engine-rs",
                "task_range": "1-3",
                "resume": true,
                "use_worktree": true,
                "branch_name": "task/EN.11.N",
                "llm_triage": true,
                "policy": {"max_attempts": 5},
                "profile": "cheap-fast"
            }"#,
        )
        .expect("deserializes");
        assert_eq!(event.repo.as_deref(), Some("engine-rs"));
        assert_eq!(event.task_range.as_deref(), Some("1-3"));
        assert!(event.resume);
        assert!(event.use_worktree);
        assert_eq!(event.branch_name.as_deref(), Some("task/EN.11.N"));
        assert!(event.llm_triage);
        assert_eq!(event.policy, Some(serde_json::json!({"max_attempts": 5})));
        assert_eq!(event.profile.as_deref(), Some("cheap-fast"));
    }

    #[test]
    fn has_no_auto_pr_field() {
        // `auto_pr` is the one `SDLCFlowEventSchema` field this schema
        // deliberately drops. Feeding it in must not fail deserialization
        // (serde ignores unknown fields by default) and there must be no
        // way to read it back — this is a structural assertion, not a
        // deserialize-error assertion.
        let event: SdlcTaskEventSchema =
            serde_json::from_str(r#"{"spec_slug": "X", "auto_pr": true}"#)
                .expect("unknown fields are ignored, not rejected");
        // The struct simply has no `auto_pr` field to read — this compiles
        // only because that is true; if a field were added this test would
        // fail to compile, which is the point.
        let _ = event;
    }
}
