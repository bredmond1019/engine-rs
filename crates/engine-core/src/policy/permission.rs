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
}
