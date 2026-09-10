//! Commander triage — `EN.15.F` task 4: the single gated `AgentCodeStep` in the whole
//! COMMANDER workflow.
//!
//! Ports the two judgement steps `/orchestration-commander`'s prompt still owns after this
//! block's `out_of_scope` cut: classifying the authored-orphan remainder (a dirty path left
//! after subtracting the `I_EMIT_WROTE` manifest — see [`super::emit_commit`]) and checking
//! each finding against `planning/open-work/index.md` before filing it fresh. Every OTHER
//! judgement step `/orchestration-commander` performs is explicitly out of scope for this
//! port (see the block record's `out_of_scope`) — this module must never grow a second
//! `AgentCodeStep`.
//!
//! ## Gated on `GatedAction::RunDrain`, never bypassed
//!
//! [`build_triage_step`] checks [`crate::policy::permission::decide`] before doing anything
//! else. Under [`PermissionProfile::Standard`] and [`PermissionProfile::Locked`], `RunDrain`
//! is `Deny` (`permission.rs:117`/`:354`), so the step is **skipped and recorded** as
//! [`TriageOutcome::SuppressedByProfile`] — the same "recorded, never dropped" discipline
//! [`crate::workflows::sweep::route`]'s `suppressed_by_profile` uses for a profile-denied
//! route. Only [`PermissionProfile::Unrestricted`] permits it (`:124`).
//!
//! The constructed [`Config`] never sets `dangerously_skip_permissions` — that is an
//! acceptance criterion, not a style note: this step is NOT invoked with
//! `--dangerously-skip-permissions` (the flag `claude-code-rs::Config::build_args` emits;
//! this codebase has no separate `--permission-mode` flag at all, so the criterion's
//! "not invoked with `--permission-mode bypassPermissions`" is satisfied by never opting
//! into the one bypass switch that does exist). [`triage_config`] is exposed and tested on
//! its own precisely so that guarantee is checked independently of whether the gate permits
//! construction at all.

use claude_code_rs::Config;

use crate::nodes::agent_code_step::AgentCodeStep;
use crate::policy::permission::{decide, Decision, GatedAction, PermissionProfile};

/// The commander triage step's stable system/task prompt — a colocated file per standing
/// rule 7 (`D24`), never an inline string literal, so `include_str!` resolves it at compile
/// time and the const's type/name/visibility stay unchanged regardless of prompt edits.
const TRIAGE_PROMPT: &str = include_str!("prompts/triage.md");

/// This step's stable `Node::name()` identity.
pub const TRIAGE_STEP_NAME: &str = "commander-triage";

/// Build the `claude_code_rs::Config` this step runs under. Exposed and tested standalone
/// (not only through [`build_triage_step`]) so "never bypasses permissions" is checked
/// independently of whether [`GatedAction::RunDrain`] happens to permit construction in a
/// given test.
///
/// `model` threads the workflow's own resolved model policy (per standing rule 6, model tier
/// is a knob, never hardcoded here) straight through; `None` leaves `claude_code_rs::execute`
/// to fall back to its own default.
#[must_use]
pub fn triage_config(model: Option<String>) -> Config {
    Config {
        model,
        // Never opt into the bypass switch for this gated step — see this module's doc
        // comment. `Config::default()` already leaves this `false`; set explicitly so the
        // intent survives a future field reordering or a copy-pasted builder.
        dangerously_skip_permissions: false,
        ..Config::default()
    }
}

/// The result of attempting to build the commander's single triage step for one drain pass.
#[derive(Debug)]
pub enum TriageOutcome {
    /// `GatedAction::RunDrain` permitted the step; here it is, ready to run. Boxed per
    /// clippy's `large_enum_variant` — `AgentCodeStep` is the far larger of the two
    /// variants and `SuppressedByProfile` carries no data at all.
    Step(Box<AgentCodeStep>),
    /// `GatedAction::RunDrain` denied the step under this profile. The step is skipped —
    /// this variant IS the record of that, mirroring `sweep::route`'s
    /// `suppressed_by_profile: true` discipline: a caller matching on this variant reports
    /// "triage suppressed by profile", it never silently proceeds as if nothing were asked
    /// for.
    SuppressedByProfile,
}

impl TriageOutcome {
    /// `true` only for [`TriageOutcome::SuppressedByProfile`] — a small reader so call sites
    /// don't have to match on the variant just to log the suppression.
    #[must_use]
    pub fn suppressed_by_profile(&self) -> bool {
        matches!(self, TriageOutcome::SuppressedByProfile)
    }
}

/// Build the commander's one and only `AgentCodeStep` — orphan classification plus the
/// `planning/open-work/index.md` check — gated on `GatedAction::RunDrain` under `profile`.
///
/// This is the single call site in the COMMANDER workflow that constructs a `AgentCodeStep`;
/// see this module's doc comment for why a second one would be a scope violation, not a bug
/// fix.
#[must_use]
pub fn build_triage_step(profile: PermissionProfile, model: Option<String>) -> TriageOutcome {
    match decide(profile, GatedAction::RunDrain) {
        Decision::Deny => TriageOutcome::SuppressedByProfile,
        Decision::Permit => {
            let config = triage_config(model);
            TriageOutcome::Step(Box::new(AgentCodeStep::new(
                TRIAGE_STEP_NAME,
                config,
                TRIAGE_PROMPT,
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::Node as _;

    // --- triage_config: never bypasses permissions --------------------------------------

    #[test]
    fn triage_config_never_sets_dangerously_skip_permissions() {
        let config = triage_config(Some("sonnet".to_string()));
        assert!(!config.dangerously_skip_permissions);
    }

    #[test]
    fn triage_config_build_args_never_carries_a_bypass_or_permission_mode_flag() {
        let config = triage_config(None);
        let args = config.build_args("prompt");
        assert!(
            !args.iter().any(|a| a.contains("permission-mode")),
            "triage step must never be invoked with --permission-mode: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "--dangerously-skip-permissions"),
            "triage step must never be invoked with --dangerously-skip-permissions: {args:?}"
        );
    }

    #[test]
    fn triage_config_threads_the_model_policy_through() {
        let config = triage_config(Some("haiku".to_string()));
        assert_eq!(config.model.as_deref(), Some("haiku"));
    }

    // --- build_triage_step: gated on GatedAction::RunDrain -------------------------------

    #[test]
    fn standard_profile_suppresses_triage_and_records_it() {
        let outcome = build_triage_step(PermissionProfile::Standard, None);
        assert!(outcome.suppressed_by_profile());
        assert!(matches!(outcome, TriageOutcome::SuppressedByProfile));
    }

    #[test]
    fn locked_profile_suppresses_triage() {
        let outcome = build_triage_step(PermissionProfile::Locked, None);
        assert!(outcome.suppressed_by_profile());
    }

    #[test]
    fn unrestricted_profile_permits_triage_and_builds_the_one_step() {
        let outcome =
            build_triage_step(PermissionProfile::Unrestricted, Some("sonnet".to_string()));
        match outcome {
            TriageOutcome::Step(step) => {
                assert_eq!(step.name(), TRIAGE_STEP_NAME);
            }
            TriageOutcome::SuppressedByProfile => {
                panic!("Unrestricted must permit GatedAction::RunDrain")
            }
        }
        assert!(!TriageOutcome::Step(Box::new(AgentCodeStep::new(
            TRIAGE_STEP_NAME,
            Config::default(),
            "x"
        )))
        .suppressed_by_profile());
    }

    #[test]
    fn the_prompt_covers_both_named_judgement_tasks_and_nothing_else_is_claimed() {
        // Guards against the scope creeping back toward the rest of
        // `/orchestration-commander`'s judgement steps, which this block's `out_of_scope`
        // explicitly excludes.
        assert!(TRIAGE_PROMPT.to_lowercase().contains("orphan"));
        assert!(TRIAGE_PROMPT.contains("open-work/index.md"));
    }
}
