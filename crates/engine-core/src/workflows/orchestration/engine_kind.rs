//! `EngineKind` — the closed, two-variant sanctioned-engine type — `EN.10.C` Task 1.
//!
//! `/orchestrate` standing rule 1 exists because a block implemented outside the two
//! sanctioned SDLC engines has no spec, no gate, no review and no honest state write, and
//! the chain's own verification does not catch it because the state write looks fine.
//! Translating orchestration into an engine makes that failure faster and harder to see,
//! so the rule has to survive as code, not as a convention an engine is asked to follow.
//!
//! [`EngineKind`] is that code: an enum with exactly two variants, [`EngineKind::Task`]
//! (`/sdlc-task`) and [`EngineKind::Flow`] (`/sdlc-flow`). There is no third variant and no
//! string-typed escape hatch anywhere in this module — a value the closed vocabulary does
//! not recognise cannot be represented as an `EngineKind` at all; it can only ever be an
//! [`UnsupportedSdlcWorkflow`] diagnostic. `sdlc-run` and `sdlc-block` stay unsupported by
//! orchestration, exactly as `/orchestrate` rule 2 says.

use std::fmt;

/// Which sanctioned SDLC engine runs a block.
///
/// Closed on purpose: this type cannot express any runner other than the two
/// `/orchestrate` sanctions. Constructing one from a block's authored `sdlc_workflow`
/// field goes through [`EngineKind::from_sdlc_workflow`], which is the only production
/// entry point — there is no `From<&str>`/`From<String>` impl and no constructor that
/// accepts an arbitrary command or runner name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineKind {
    /// `/sdlc-task`.
    Task,
    /// `/sdlc-flow`.
    Flow,
}

impl fmt::Display for EngineKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineKind::Task => write!(f, "task"),
            EngineKind::Flow => write!(f, "flow"),
        }
    }
}

/// The diagnostic produced when a block's authored `sdlc_workflow` field
/// (`okf_core::TrackBlock::sdlc_workflow`) is not one of the closed vocabulary
/// [`EngineKind`] represents.
///
/// Never a fallback and never a default: a block authored with e.g. `"sdlc-run"`,
/// `"sdlc-block"`, a typo, or no value at all produces this diagnostic and the caller
/// does not run the block — it does not silently fall through to [`EngineKind::Flow`]
/// or panic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedSdlcWorkflow {
    /// The authored value that failed to map, verbatim (`None` when the field itself
    /// was absent/null).
    pub value: Option<String>,
}

impl fmt::Display for UnsupportedSdlcWorkflow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.value {
            Some(v) => write!(
                f,
                "block declares sdlc_workflow '{v}', which is outside the sanctioned \
                 engine vocabulary {{task, flow}} — /orchestrate rule 2 leaves sdlc-run \
                 and sdlc-block (and anything else) unsupported here"
            ),
            None => write!(
                f,
                "block has no authored sdlc_workflow — the sanctioned engine vocabulary \
                 {{task, flow}} requires one to be set explicitly, never defaulted"
            ),
        }
    }
}

impl std::error::Error for UnsupportedSdlcWorkflow {}

impl EngineKind {
    /// Map a block's authored `sdlc_workflow` field
    /// (`okf_core::TrackBlock::sdlc_workflow`) onto the closed engine vocabulary.
    ///
    /// `Some("task")` -> [`EngineKind::Task`], `Some("flow")` -> [`EngineKind::Flow`].
    /// Every other value — any other string, `None` — is an [`UnsupportedSdlcWorkflow`]
    /// diagnostic, never a fallback.
    pub fn from_sdlc_workflow(value: Option<&str>) -> Result<EngineKind, UnsupportedSdlcWorkflow> {
        match value {
            Some("task") => Ok(EngineKind::Task),
            Some("flow") => Ok(EngineKind::Flow),
            other => Err(UnsupportedSdlcWorkflow {
                value: other.map(str::to_string),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_and_flow_map_to_their_variants() {
        assert_eq!(
            EngineKind::from_sdlc_workflow(Some("task")),
            Ok(EngineKind::Task)
        );
        assert_eq!(
            EngineKind::from_sdlc_workflow(Some("flow")),
            Ok(EngineKind::Flow)
        );
    }

    #[test]
    fn sdlc_run_and_sdlc_block_are_diagnostics_not_defaults() {
        // /orchestrate rule 2: sdlc-run and sdlc-block stay unsupported by orchestration.
        let err = EngineKind::from_sdlc_workflow(Some("sdlc-run")).unwrap_err();
        assert_eq!(err.value.as_deref(), Some("sdlc-run"));

        let err = EngineKind::from_sdlc_workflow(Some("sdlc-block")).unwrap_err();
        assert_eq!(err.value.as_deref(), Some("sdlc-block"));
    }

    #[test]
    fn arbitrary_and_missing_values_are_diagnostics_not_defaults() {
        let err = EngineKind::from_sdlc_workflow(Some("run-arbitrary-shell")).unwrap_err();
        assert_eq!(err.value.as_deref(), Some("run-arbitrary-shell"));

        let err = EngineKind::from_sdlc_workflow(None).unwrap_err();
        assert_eq!(err.value, None);
    }

    #[test]
    fn diagnostic_display_names_the_bad_value() {
        let err = EngineKind::from_sdlc_workflow(Some("sdlc-run")).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("sdlc-run"),
            "message should name the value: {msg}"
        );

        let err = EngineKind::from_sdlc_workflow(None).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("no authored sdlc_workflow"),
            "message should say no value was authored: {msg}"
        );
    }

    #[test]
    fn engine_kind_display_names_are_stable() {
        assert_eq!(EngineKind::Task.to_string(), "task");
        assert_eq!(EngineKind::Flow.to_string(), "flow");
    }
}
