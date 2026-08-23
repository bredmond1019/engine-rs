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

    /// EN.10.C Task 2 (widened by `EN.ticket.sanctioned-engine-guard-covers-the-execute-seam`
    /// Task 1) — the unreachability test.
    ///
    /// `from_sdlc_workflow` is the ONE sanctioned entry point allowed to take a
    /// string-shaped runner value; it exists precisely to turn an arbitrary string into
    /// either a closed [`EngineKind`] variant or an [`UnsupportedSdlcWorkflow`]
    /// diagnostic — never a raw runner. This test scans EVERY `.rs` file under this
    /// module's own directory (`crates/engine-core/src/workflows/orchestration/`, read
    /// at runtime via `std::fs::read_dir` rooted at `env!("CARGO_MANIFEST_DIR")` — never
    /// `git diff`, so it is immune to concurrent lanes' unrelated uncommitted files
    /// elsewhere in the working tree, and never any directory outside this module) for
    /// every `pub`/`pub(crate)` fn signature that accepts a `&str` or `String`
    /// parameter, and asserts each one is on the per-file sanctioned allowlist below.
    ///
    /// A working-tree-wide `git diff | grep` guard is explicitly the wrong shape here —
    /// it can never pass in a shared index with concurrent lanes and would bail the
    /// block on an unrelated lane's uncommitted files. Scoping the read to this
    /// module's own directory keeps the check honest without that failure mode.
    ///
    /// Reintroducing a sibling escape hatch anywhere in the module — e.g.
    /// `pub fn run_raw(cmd: &str)` in `execute.rs`, the block-execution seam — adds a
    /// `pub fn ...(&str ...)` signature line not on the allowlist for that file and
    /// makes this test fail, naming both the offending fn and its file. Demonstrated
    /// 2026-08-18: before this widening, an identical injection into `execute.rs` left
    /// the whole orchestration suite GREEN with zero failures — the original,
    /// file-scoped guard only ever saw its own source. See the task notes for the
    /// adversarial-injection transcript proving the widened guard now catches it.
    #[test]
    fn no_string_typed_runner_escape_besides_the_one_sanctioned_mapping_fn() {
        // Per-file allowlist: (file name, sanctioned string-taking `pub`/`pub(crate)` fn
        // names in that file). A fn not listed here, in a file scanned below, fails the
        // guard. Every entry here is a legitimate existing string-taking entry point as
        // of this widening — verified by grepping the module for
        // `^\s*pub(\(crate\))? fn .*(&str|: String)` on 2026-08-18.
        const SANCTIONED_STRING_TAKING_FNS: &[(&str, &[&str])] = &[
            ("chain.rs", &[]),
            // `resolve_depends_on`/`is_edge_met` take a repo slug + block id,
            // `is_block_open` takes an opaque HELD-UNTIL token — corpus lookups keyed
            // by identifier, not a runner-name escape hatch.
            (
                "corpus_gates.rs",
                &["resolve_depends_on", "is_edge_met", "is_block_open"],
            ),
            ("engine_kind.rs", &["from_sdlc_workflow"]),
            ("execute.rs", &[]),
            ("gates.rs", &[]),
            // `profile_by_name` resolves a named policy profile (e.g. "cheap-fast") to
            // its `PartialOrchestrationPolicy` bundle — a config lookup, not a runner.
            // `resolve_campaign_id` (`EN.11.F` task 2 follow-up) parses/mints a
            // campaign `Uuid` from an event's optional `campaign_id` string field —
            // exposed so `engine-serve`'s factory can resolve the SAME id up front
            // and register it for campaign-scoped abort. A UUID parse, not a runner
            // selector.
            ("graph.rs", &["profile_by_name", "resolve_campaign_id"]),
            // `resolve_roadmap_dir` resolves a roadmap slug to its planning directory —
            // a path lookup, not a runner. `closed`/`bailed`/`cancelled`/`budget_halted`
            // are `LaneLogEntry` constructors taking a `lane: &str` and a
            // `note: impl Into<String>` (`EN.11.F` task 4 adds the latter two, same
            // shape as the existing pair) — building a log line, not selecting or
            // invoking a runner.
            (
                "integrate.rs",
                &[
                    "resolve_roadmap_dir",
                    "closed",
                    "bailed",
                    "cancelled",
                    "budget_halted",
                ],
            ),
            ("mod.rs", &[]),
        ];

        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/workflows/orchestration");

        let mut offending: Vec<String> = Vec::new();
        let mut scanned_files: Vec<&str> = Vec::new();

        for &(file_name, sanctioned) in SANCTIONED_STRING_TAKING_FNS {
            let path = dir.join(file_name);
            let source = std::fs::read_to_string(&path).unwrap_or_else(|err| {
                panic!("failed to read {path:?} for the sanctioned-engine guard scan: {err}")
            });
            scanned_files.push(file_name);

            for line in source.lines() {
                let trimmed = line.trim_start();
                let is_pub_fn =
                    trimmed.starts_with("pub fn ") || trimmed.starts_with("pub(crate) fn ");
                if !is_pub_fn {
                    continue;
                }
                // Every `pub fn` signature in this module is written on a single line
                // (enforced by `cargo fmt --check` in the harness); a signature-line
                // scan is therefore sufficient to catch a reintroduced string-typed
                // runner param without needing a full parser.
                let takes_string = trimmed.contains("&str") || trimmed.contains(": String");
                if !takes_string {
                    continue;
                }

                let after_fn = trimmed
                    .strip_prefix("pub(crate) fn ")
                    .or_else(|| trimmed.strip_prefix("pub fn "))
                    .unwrap();
                let name = after_fn.split(['(', '<']).next().unwrap_or("").trim();

                if !sanctioned.contains(&name) {
                    offending.push(format!("{name} (in {file_name})"));
                }
            }
        }

        // Fail loudly, not silently, if this test's own file list drifts from the
        // directory's actual contents — a new file added to the module without being
        // added to `SANCTIONED_STRING_TAKING_FNS` above would otherwise go unscanned.
        let actual_rs_files: std::collections::BTreeSet<String> = std::fs::read_dir(&dir)
            .unwrap_or_else(|err| panic!("failed to read {dir:?}: {err}"))
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                name.ends_with(".rs").then_some(name)
            })
            .collect();
        let listed_files: std::collections::BTreeSet<String> =
            scanned_files.iter().map(|s| s.to_string()).collect();
        assert_eq!(
            actual_rs_files, listed_files,
            "the orchestration module's .rs files have drifted from the guard's scan \
             list — add the new file to SANCTIONED_STRING_TAKING_FNS in \
             no_string_typed_runner_escape_besides_the_one_sanctioned_mapping_fn so it \
             is not silently left unscanned"
        );

        assert!(
            offending.is_empty(),
            "extra string-typed pub fn(s) found in crates/engine-core/src/workflows/\
             orchestration/ beyond the sanctioned allowlist: {offending:?} — a \
             string-typed runner escape was reintroduced in the orchestration module"
        );
    }

    /// Companion guard: the enum itself stays exhaustively matched with no wildcard arm,
    /// so adding a third `EngineKind` variant fails to COMPILE this module rather than
    /// silently falling through an `_ => ...` arm anywhere the type is consumed.
    #[test]
    fn engine_kind_match_stays_exhaustive_no_wildcard() {
        fn describe(kind: EngineKind) -> &'static str {
            match kind {
                EngineKind::Task => "task",
                EngineKind::Flow => "flow",
                // Deliberately NO `_ =>` arm: a third variant added to `EngineKind`
                // makes this `match` non-exhaustive and fails the build, rather than
                // silently falling through.
            }
        }
        assert_eq!(describe(EngineKind::Task), "task");
        assert_eq!(describe(EngineKind::Flow), "flow");
    }
}
