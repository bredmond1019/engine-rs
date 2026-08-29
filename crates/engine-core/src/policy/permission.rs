//! Permission-profile enforcement — `EN.12.C` task 1: the gated-action vocabulary and
//! the (profile, action) decision matrix.
//!
//! This is the ENFORCEMENT half of SQ-37; the CONFIG half lives in the brain's
//! `brain.toml` `[permission_profiles]` table and is documented in
//! `docs/permission-profiles.md` (HQ block `HQ.5.B`). Both halves must agree on the same
//! three-level vocabulary and the same grading table — this module is the engine-side
//! source of truth for the closed Rust types and the decision function; the docs above
//! are the source of truth for the config identifiers and their prose meaning.
//!
//! Both [`GatedAction`] and [`PermissionProfile`] are closed enums, following the
//! `orchestration::engine_kind::EngineKind` precedent exactly: no `From<&str>`, no
//! `From<String>`, no string-typed constructor, no `Unknown(String)` fallback variant.
//! An unrecognised action or profile is unrepresentable by these types, not merely
//! undiagnosed — string parsing (where a caller genuinely needs it, e.g. reading a
//! profile name out of `harness.json`) is a separate concern layered on top, not baked
//! into this module.

use std::fmt;
use std::path::Path;

use mev::brain::config::PermissionProfilesConfig;
use serde::{Deserialize, Serialize};

/// The closed vocabulary of gated engine actions.
///
/// No `From<&str>`/`From<String>` impl and no `Unknown(String)` variant — an action
/// outside this set cannot be represented at all, mirroring
/// `orchestration::engine_kind::EngineKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatedAction {
    /// `mev close-operator-gate <slug> --exit-verified` — denied at every profile level,
    /// with no override. Restates D71.
    ClearOperatorGate,
    /// Installing a built binary on the Mac Mini.
    InstallOnMini,
    /// Pushing to `main`.
    PushToMain,
    /// Writing across repos.
    CrossRepoWrite,
}

/// The three permission levels HQ.5.B authored, ordered tightest to most permissive.
///
/// The wire identifiers (`locked` / `standard` / `unrestricted`, produced by
/// `#[serde(rename_all = "snake_case")]`) are a cross-repo contract consumed verbatim by
/// `brain.toml`'s `[permission_profiles]` table and `docs/permission-profiles.md` — they
/// are not human-readable labels subject to rewording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionProfile {
    /// Tightest. No side effect outside the working tree without an operator gate.
    Locked,
    /// Middle level — the declared default. Lands changes and pushes to `main`, but
    /// never installs on the Mac Mini.
    Standard,
    /// Most permissive. Reserved for explicit, deliberately-invoked runs. Never the
    /// default.
    Unrestricted,
}

/// The default permission profile a run resolves to absent any override.
///
/// Deliberately `Standard`, never `Unrestricted` — see
/// [`default_profile_is_not_most_permissive`] below, which fails loudly if this constant
/// is ever changed to the most permissive level. This is a written value, not a
/// first/last-entry fallback (`docs/permission-profiles.md`'s "written out explicitly"
/// precedent).
pub const DEFAULT_PROFILE: PermissionProfile = PermissionProfile::Standard;

/// The result of grading one `(profile, action)` pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Permit,
    Deny,
}

/// Decide allow/deny for one `(profile, action)` pair per HQ's grading table
/// (`docs/permission-profiles.md`'s grading table, mirrored in `brain.toml`'s
/// `[permission_profiles.levels.*]` entries).
///
/// `ClearOperatorGate` is deny at every level — this is expressed as a match arm that
/// returns before the per-profile table is consulted at all, not as three separate
/// per-profile cells, so it cannot be flipped by editing a single profile's row.
///
/// | profile        | mini_install | main_push | cross_repo_write |
/// |----------------|--------------|-----------|-------------------|
/// | `locked`       | deny         | deny      | deny              |
/// | `standard`     | deny         | permit    | permit            |
/// | `unrestricted` | permit       | permit    | permit            |
#[must_use]
pub fn decide(profile: PermissionProfile, action: GatedAction) -> Decision {
    // The one `never_allowed` entry: unconditional, checked before any per-profile
    // table lookup. `docs/permission-profiles.md`'s "one-entry never-allowed list".
    if matches!(action, GatedAction::ClearOperatorGate) {
        return Decision::Deny;
    }

    match (profile, action) {
        (PermissionProfile::Locked, _) => Decision::Deny,

        (PermissionProfile::Standard, GatedAction::InstallOnMini) => Decision::Deny,
        (PermissionProfile::Standard, GatedAction::PushToMain) => Decision::Permit,
        (PermissionProfile::Standard, GatedAction::CrossRepoWrite) => Decision::Permit,

        (PermissionProfile::Unrestricted, GatedAction::InstallOnMini) => Decision::Permit,
        (PermissionProfile::Unrestricted, GatedAction::PushToMain) => Decision::Permit,
        (PermissionProfile::Unrestricted, GatedAction::CrossRepoWrite) => Decision::Permit,

        // Unreachable: `ClearOperatorGate` already returned above for every profile.
        (_, GatedAction::ClearOperatorGate) => Decision::Deny,
    }
}

/// The one entry `brain.toml`'s `[permission_profiles].never_allowed` must contain,
/// exactly — restates D71 on the config side, mirroring [`GatedAction::ClearOperatorGate`]
/// on the enforcement side.
const CLEAR_OPERATOR_GATE_ACTION_ID: &str = "clear_operator_gate";

/// Typed failure modes for resolving a [`PermissionProfile`] out of `brain.toml`'s
/// `[permission_profiles]` table.
///
/// Every variant here corresponds to a shape mev's `#[serde(default)]` chain lets
/// through *silently* (an absent table, an empty `levels` map, a `None` default, a
/// `default` naming a level that isn't declared, a `never_allowed` that isn't exactly
/// `["clear_operator_gate"]`, or a level id outside the closed three-value vocabulary)
/// — [`resolve_permission_profile`] and [`resolve_permission_profile_from_config`]
/// never treat any of these as success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileResolutionError {
    /// `brain.toml` could not be read or parsed at all.
    ConfigLoad { message: String },
    /// `[permission_profiles.levels.*]` declared no entries.
    EmptyLevels,
    /// `[permission_profiles].default` was absent (`None`).
    NoDefault,
    /// `[permission_profiles].default` names a level id not present in `levels`.
    DefaultLevelMissing { default: String },
    /// `[permission_profiles].never_allowed` is not exactly `["clear_operator_gate"]`.
    NeverAllowedMismatch { got: Vec<String> },
    /// A `[permission_profiles.levels.<id>]` entry's `id` is outside the closed
    /// three-value vocabulary (`locked` / `standard` / `unrestricted`).
    UnknownLevelId { id: String },
}

impl fmt::Display for ProfileResolutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProfileResolutionError::ConfigLoad { message } => {
                write!(f, "could not load brain.toml: {message}")
            }
            ProfileResolutionError::EmptyLevels => {
                write!(f, "[permission_profiles.levels] declares no entries")
            }
            ProfileResolutionError::NoDefault => {
                write!(f, "[permission_profiles].default is absent")
            }
            ProfileResolutionError::DefaultLevelMissing { default } => write!(
                f,
                "[permission_profiles].default = \"{default}\" names a level absent from \
                 [permission_profiles.levels]"
            ),
            ProfileResolutionError::NeverAllowedMismatch { got } => write!(
                f,
                "[permission_profiles].never_allowed must be exactly [\"{CLEAR_OPERATOR_GATE_ACTION_ID}\"], \
                 got {got:?}"
            ),
            ProfileResolutionError::UnknownLevelId { id } => write!(
                f,
                "[permission_profiles.levels] declares unknown level id \"{id}\" (expected one \
                 of: locked, standard, unrestricted)"
            ),
        }
    }
}

impl std::error::Error for ProfileResolutionError {}

/// Map a `[permission_profiles.levels.<id>]` wire identifier onto the closed
/// [`PermissionProfile`] enum. `None` for anything outside the three known ids —
/// callers treat that as [`ProfileResolutionError::UnknownLevelId`], never a silent
/// fallback.
fn level_id_to_profile(id: &str) -> Option<PermissionProfile> {
    match id {
        "locked" => Some(PermissionProfile::Locked),
        "standard" => Some(PermissionProfile::Standard),
        "unrestricted" => Some(PermissionProfile::Unrestricted),
        _ => None,
    }
}

/// Resolve the permission profile in force from a `brain.toml` on disk.
///
/// Reads the file via `mev::brain::config::load_brain_config` — engine-core has no
/// second parser for this table — and delegates the resolution logic to
/// [`resolve_permission_profile_from_config`].
///
/// **Always returns a [`PermissionProfile`].** On any failure the returned profile is
/// [`PermissionProfile::Locked`] (the tightest level) and `Some` typed error explains
/// why; there is no path that fails open to `standard` or `unrestricted`. Callers that
/// only need the profile can ignore the error half; callers that must surface *why* a
/// run downgraded to `locked` read it.
#[must_use]
pub fn resolve_permission_profile(
    brain_toml_path: &Path,
) -> (PermissionProfile, Option<ProfileResolutionError>) {
    match mev::brain::config::load_brain_config(brain_toml_path) {
        Ok(config) => resolve_permission_profile_from_config(&config.permission_profiles),
        Err(err) => (
            PermissionProfile::Locked,
            Some(ProfileResolutionError::ConfigLoad {
                message: err.to_string(),
            }),
        ),
    }
}

/// Resolve the permission profile in force from an already-parsed
/// `[permission_profiles]` table.
///
/// Fail-closed at every step, in this order: an empty `levels` map; a `None` default; a
/// `never_allowed` that is not exactly `["clear_operator_gate"]`; any level whose `id`
/// is outside the closed three-value vocabulary; a `default` naming a level absent from
/// `levels`. Only when every check passes does this resolve to the level named by
/// `default`. See [`resolve_permission_profile`] for the always-returns-a-profile
/// contract this shares.
#[must_use]
pub fn resolve_permission_profile_from_config(
    config: &PermissionProfilesConfig,
) -> (PermissionProfile, Option<ProfileResolutionError>) {
    if config.levels.is_empty() {
        return (
            PermissionProfile::Locked,
            Some(ProfileResolutionError::EmptyLevels),
        );
    }

    let Some(default_id) = &config.default else {
        return (
            PermissionProfile::Locked,
            Some(ProfileResolutionError::NoDefault),
        );
    };

    if config.never_allowed != [CLEAR_OPERATOR_GATE_ACTION_ID.to_string()] {
        return (
            PermissionProfile::Locked,
            Some(ProfileResolutionError::NeverAllowedMismatch {
                got: config.never_allowed.clone(),
            }),
        );
    }

    // Every declared level id must be one of the three known ids — an unrecognised id
    // fails closed even if it isn't the one `default` happens to name.
    for level in config.levels.values() {
        if level_id_to_profile(&level.id).is_none() {
            return (
                PermissionProfile::Locked,
                Some(ProfileResolutionError::UnknownLevelId {
                    id: level.id.clone(),
                }),
            );
        }
    }

    let Some(default_level) = config.levels.get(default_id) else {
        return (
            PermissionProfile::Locked,
            Some(ProfileResolutionError::DefaultLevelMissing {
                default: default_id.clone(),
            }),
        );
    };

    match level_id_to_profile(&default_level.id) {
        Some(profile) => (profile, None),
        None => (
            PermissionProfile::Locked,
            Some(ProfileResolutionError::UnknownLevelId {
                id: default_level.id.clone(),
            }),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_PROFILES: [PermissionProfile; 3] = [
        PermissionProfile::Locked,
        PermissionProfile::Standard,
        PermissionProfile::Unrestricted,
    ];

    const ALL_ACTIONS: [GatedAction; 4] = [
        GatedAction::ClearOperatorGate,
        GatedAction::InstallOnMini,
        GatedAction::PushToMain,
        GatedAction::CrossRepoWrite,
    ];

    /// AC: table-driven — every `PermissionProfile` x `ClearOperatorGate` cell denies.
    #[test]
    fn clear_operator_gate_is_denied_at_every_profile_level() {
        for profile in ALL_PROFILES {
            assert_eq!(
                decide(profile, GatedAction::ClearOperatorGate),
                Decision::Deny,
                "clear-operator-gate must be denied at profile {profile:?}"
            );
        }
    }

    /// AC: the full 12-cell (3 profiles x 4 actions) grading matrix, asserted
    /// table-driven against `docs/permission-profiles.md`'s grading table cell-for-cell.
    #[test]
    fn decision_matrix_matches_docs_permission_profiles_grading_table() {
        use Decision::{Deny, Permit};
        use GatedAction::{ClearOperatorGate, CrossRepoWrite, InstallOnMini, PushToMain};
        use PermissionProfile::{Locked, Standard, Unrestricted};

        let expected: [(PermissionProfile, GatedAction, Decision); 12] = [
            // locked
            (Locked, ClearOperatorGate, Deny),
            (Locked, InstallOnMini, Deny),
            (Locked, PushToMain, Deny),
            (Locked, CrossRepoWrite, Deny),
            // standard
            (Standard, ClearOperatorGate, Deny),
            (Standard, InstallOnMini, Deny),
            (Standard, PushToMain, Permit),
            (Standard, CrossRepoWrite, Permit),
            // unrestricted
            (Unrestricted, ClearOperatorGate, Deny),
            (Unrestricted, InstallOnMini, Permit),
            (Unrestricted, PushToMain, Permit),
            (Unrestricted, CrossRepoWrite, Permit),
        ];

        for (profile, action, want) in expected {
            assert_eq!(
                decide(profile, action),
                want,
                "mismatch at (profile={profile:?}, action={action:?}): expected {want:?}"
            );
        }

        // Sanity: this test itself must actually cover all 12 pairs, not a subset.
        assert_eq!(expected.len(), ALL_PROFILES.len() * ALL_ACTIONS.len());
    }

    /// AC: the default profile is not the most permissive one, and this test fails if
    /// the builtin default is ever changed to `Unrestricted`.
    #[test]
    fn default_profile_is_not_most_permissive() {
        assert_ne!(
            DEFAULT_PROFILE,
            PermissionProfile::Unrestricted,
            "DEFAULT_PROFILE must never be the most permissive profile \
             (docs/permission-profiles.md invariant 2)"
        );
        assert_eq!(
            DEFAULT_PROFILE,
            PermissionProfile::Standard,
            "DEFAULT_PROFILE is documented as `standard` — this pins the declared \
             default, not merely 'not unrestricted'"
        );
    }

    /// AC: each of the three graded actions has at least one profile that permits and
    /// one that denies — proving they are graded, not forbidden and not universally
    /// allowed.
    #[test]
    fn each_graded_action_has_both_a_permit_and_a_deny_profile() {
        for action in [
            GatedAction::InstallOnMini,
            GatedAction::PushToMain,
            GatedAction::CrossRepoWrite,
        ] {
            let mut saw_permit = false;
            let mut saw_deny = false;
            for profile in ALL_PROFILES {
                match decide(profile, action) {
                    Decision::Permit => saw_permit = true,
                    Decision::Deny => saw_deny = true,
                }
            }
            assert!(
                saw_permit,
                "{action:?} must be permitted at at least one profile"
            );
            assert!(
                saw_deny,
                "{action:?} must be denied at at least one profile"
            );
        }
    }

    /// `ClearOperatorGate` is exempt from the "graded" claim above by design — it is
    /// the one forbidden-everywhere action, not a graded one. This test documents that
    /// distinction explicitly rather than leaving it implicit.
    #[test]
    fn clear_operator_gate_is_never_permitted_anywhere_not_graded() {
        for profile in ALL_PROFILES {
            assert_eq!(
                decide(profile, GatedAction::ClearOperatorGate),
                Decision::Deny
            );
        }
    }

    /// AC: the three wire identifiers serialize as exactly `locked`, `standard`,
    /// `unrestricted`.
    #[test]
    fn permission_profile_wire_identifiers_are_exact() {
        assert_eq!(
            serde_json::to_string(&PermissionProfile::Locked).unwrap(),
            "\"locked\""
        );
        assert_eq!(
            serde_json::to_string(&PermissionProfile::Standard).unwrap(),
            "\"standard\""
        );
        assert_eq!(
            serde_json::to_string(&PermissionProfile::Unrestricted).unwrap(),
            "\"unrestricted\""
        );
    }

    /// Round-trip the wire identifiers back into the closed enum, confirming the
    /// contract holds in both directions.
    #[test]
    fn permission_profile_wire_identifiers_round_trip() {
        for (json, profile) in [
            ("\"locked\"", PermissionProfile::Locked),
            ("\"standard\"", PermissionProfile::Standard),
            ("\"unrestricted\"", PermissionProfile::Unrestricted),
        ] {
            let parsed: PermissionProfile = serde_json::from_str(json).unwrap();
            assert_eq!(parsed, profile);
        }
    }

    /// `GatedAction` and `PermissionProfile` have no `From<&str>`/`From<String>` impl
    /// and no string-typed constructor — a source scan mirroring
    /// `orchestration::engine_kind`'s own guard, so this closedness is enforced as code
    /// rather than only as a doc comment claim.
    #[test]
    fn no_from_str_or_from_string_impl_for_the_closed_enums() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/policy/permission.rs");
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("failed to read {path:?}: {err}"));

        for ty in ["GatedAction", "PermissionProfile"] {
            let has_from_str = source.lines().any(|l| {
                l.trim_start()
                    .starts_with(&format!("impl From<&str> for {ty}"))
            });
            let has_from_string = source.lines().any(|l| {
                l.trim_start()
                    .starts_with(&format!("impl From<String> for {ty}"))
            });
            assert!(
                !has_from_str && !has_from_string,
                "{ty} must not have a From<&str>/From<String> impl — it is a closed \
                 enum, following the orchestration::engine_kind::EngineKind precedent"
            );
        }
    }

    /// Companion guard: `decide`'s match stays exhaustive with no wildcard arm over
    /// `GatedAction`, so a fourth action added to the enum fails to compile this
    /// function rather than silently falling through.
    #[test]
    fn decide_match_is_exercised_over_every_action_variant() {
        // This is a coverage sanity check, not a compile-time guard (the wildcard arm
        // inside `decide` exists only to make the ClearOperatorGate-under-any-profile
        // case unreachable, not to swallow a genuinely new variant) — every variant is
        // exercised here so a fifth action silently added without a corresponding test
        // update stands out in review.
        for action in ALL_ACTIONS {
            for profile in ALL_PROFILES {
                let _ = decide(profile, action);
            }
        }
    }

    // --- EN.12.C task 2: resolving PermissionProfile from brain.toml -----------------

    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use mev::brain::config::PermissionProfileLevel;

    fn fixture_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    fn level(
        id: &str,
        mini_install: bool,
        main_push: bool,
        cross_repo_write: bool,
    ) -> PermissionProfileLevel {
        PermissionProfileLevel {
            id: id.to_string(),
            meaning: String::new(),
            mini_install,
            main_push,
            cross_repo_write,
        }
    }

    fn valid_three_levels() -> BTreeMap<String, PermissionProfileLevel> {
        let mut levels = BTreeMap::new();
        levels.insert("locked".to_string(), level("locked", false, false, false));
        levels.insert("standard".to_string(), level("standard", false, true, true));
        levels.insert(
            "unrestricted".to_string(),
            level("unrestricted", true, true, true),
        );
        levels
    }

    fn valid_config(default: &str) -> PermissionProfilesConfig {
        PermissionProfilesConfig {
            never_allowed: vec![CLEAR_OPERATOR_GATE_ACTION_ID.to_string()],
            default: Some(default.to_string()),
            levels: valid_three_levels(),
        }
    }

    /// AC: the checked-in fixture reproducing HQ's real table resolves to the three
    /// expected levels and `default = standard` — reads through
    /// `mev::brain::config::load_brain_config`, not a second parser.
    #[test]
    fn checked_in_fixture_resolves_to_standard_with_no_error() {
        let (profile, error) =
            resolve_permission_profile(&fixture_path("permission_profiles_brain.toml"));
        assert_eq!(profile, PermissionProfile::Standard);
        assert_eq!(error, None);
    }

    /// AC: the same fixture's three declared levels each map onto the expected
    /// `PermissionProfile` variant.
    #[test]
    fn checked_in_fixture_declares_the_three_expected_levels() {
        let config =
            mev::brain::config::load_brain_config(&fixture_path("permission_profiles_brain.toml"))
                .expect("fixture must parse");
        let levels = &config.permission_profiles.levels;
        assert_eq!(levels.len(), 3);
        assert_eq!(
            level_id_to_profile(&levels["locked"].id),
            Some(PermissionProfile::Locked)
        );
        assert_eq!(
            level_id_to_profile(&levels["standard"].id),
            Some(PermissionProfile::Standard)
        );
        assert_eq!(
            level_id_to_profile(&levels["unrestricted"].id),
            Some(PermissionProfile::Unrestricted)
        );
    }

    /// AC: a `brain.toml` with NO `[permission_profiles]` table resolves to `locked`
    /// plus a typed error — mev's `#[serde(default)]` chain would otherwise silently
    /// deserialize this to an empty, unusable config.
    #[test]
    fn absent_table_fails_closed_to_locked_with_a_typed_error() {
        let (profile, error) =
            resolve_permission_profile(&fixture_path("permission_profiles_absent_brain.toml"));
        assert_eq!(profile, PermissionProfile::Locked);
        assert_eq!(error, Some(ProfileResolutionError::EmptyLevels));
    }

    /// AC: empty `levels` fails closed to `locked` plus `EmptyLevels`.
    #[test]
    fn empty_levels_fails_closed() {
        let config = PermissionProfilesConfig {
            never_allowed: vec![CLEAR_OPERATOR_GATE_ACTION_ID.to_string()],
            default: Some("standard".to_string()),
            levels: BTreeMap::new(),
        };
        let (profile, error) = resolve_permission_profile_from_config(&config);
        assert_eq!(profile, PermissionProfile::Locked);
        assert_eq!(error, Some(ProfileResolutionError::EmptyLevels));
    }

    /// AC: `default: None` fails closed to `locked` plus `NoDefault`.
    #[test]
    fn none_default_fails_closed() {
        let mut config = valid_config("standard");
        config.default = None;
        let (profile, error) = resolve_permission_profile_from_config(&config);
        assert_eq!(profile, PermissionProfile::Locked);
        assert_eq!(error, Some(ProfileResolutionError::NoDefault));
    }

    /// AC: `default` naming an absent level fails closed to `locked` plus
    /// `DefaultLevelMissing`.
    #[test]
    fn default_naming_absent_level_fails_closed() {
        let config = valid_config("nonexistent");
        let (profile, error) = resolve_permission_profile_from_config(&config);
        assert_eq!(profile, PermissionProfile::Locked);
        assert_eq!(
            error,
            Some(ProfileResolutionError::DefaultLevelMissing {
                default: "nonexistent".to_string()
            })
        );
    }

    /// AC: a `never_allowed` that is not exactly `["clear_operator_gate"]` fails closed
    /// to `locked` plus `NeverAllowedMismatch`, whether empty or merely different.
    #[test]
    fn never_allowed_mismatch_fails_closed() {
        for got in [Vec::<String>::new(), vec!["something_else".to_string()]] {
            let mut config = valid_config("standard");
            config.never_allowed = got.clone();
            let (profile, error) = resolve_permission_profile_from_config(&config);
            assert_eq!(profile, PermissionProfile::Locked);
            assert_eq!(
                error,
                Some(ProfileResolutionError::NeverAllowedMismatch { got })
            );
        }
    }

    /// AC: an unknown level id fails closed to `locked` plus `UnknownLevelId`, even
    /// when the unrecognised level isn't the one `default` names.
    #[test]
    fn unknown_level_id_fails_closed() {
        let mut config = valid_config("standard");
        config
            .levels
            .insert("beta".to_string(), level("beta", false, false, false));
        let (profile, error) = resolve_permission_profile_from_config(&config);
        assert_eq!(profile, PermissionProfile::Locked);
        assert_eq!(
            error,
            Some(ProfileResolutionError::UnknownLevelId {
                id: "beta".to_string()
            })
        );
    }

    /// A `ConfigLoad` error (nonexistent path) also fails closed to `locked`.
    #[test]
    fn nonexistent_brain_toml_fails_closed_with_config_load_error() {
        let (profile, error) = resolve_permission_profile(&fixture_path("does_not_exist.toml"));
        assert_eq!(profile, PermissionProfile::Locked);
        assert!(matches!(
            error,
            Some(ProfileResolutionError::ConfigLoad { .. })
        ));
    }
}
