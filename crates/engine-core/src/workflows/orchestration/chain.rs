//! Lane chain resolution — `EN.10.B` Task 1.
//!
//! Turns a lane chain — either a `(roadmap, lane)` pair or an explicit list of
//! `(repo, block_id)` steps — into the ordered [`ChainStep`] list the rest of the
//! ORCHESTRATION workflow drives.
//!
//! # Structured directives, not prose
//!
//! A `(roadmap, lane)` chain is resolved by reading `planning/lane-segments.json`, the
//! corpus-wide derived artifact `mev` writes (`MV.ticket.lane-file-structured-directives`,
//! `mev::brain::lane_segments::plan_lane_segments`). This module never re-derives lane
//! segments or re-parses a `lane-*.txt` file itself — mev owns that computation, and this
//! module only reads its output. Each derived block position optionally carries the
//! owning lane's [`LaneDirectives`] — `HELD-UNTIL`, `BUDGET`, `EXCLUSIVE-REPOS` — which
//! were English prose in a lane-file comment header until that ticket landed. A human
//! driver honours prose by reading it; an engine fans out past it at machine speed, which
//! is why `HELD-UNTIL` is enforced here rather than merely surfaced.
//!
//! # Explicit block lists bypass lane-file parsing entirely
//!
//! [`resolve_explicit_chain`] never touches `lane-segments.json` and never resolves a
//! `HELD-UNTIL` directive — an explicit list is a caller's deliberate override of the lane
//! file, so it carries no lane directives at all ([`ChainStep::directives`] is always
//! `None`).
//!
//! # Malformed input is a loud error — but an unrecognised field is not (`EN.12.E`)
//!
//! [`resolve_lane_chain`] parses `planning/lane-segments.json` against the grammar `mev`
//! emits. A directive whose value does not match the grammar's *shape* (e.g. `budget`
//! present but not an object) still fails the whole parse loudly rather than silently
//! defaulting to "no directives". An *unrecognised key* on [`LaneDirectives`] or
//! [`LaneBudget`], though, now parses successfully and is dropped — `mev`'s
//! `brain::lane_segments::LaneDirectives` is hand-mirrored across two repos with no shared
//! dependency, so `#[serde(deny_unknown_fields)]` on this read side means mev shipping one
//! new field first hard-fails every lane segment on every un-rebuilt engine, not just the
//! new one (seam 15). Forward-compatibility beats strictness here on purpose; a malformed
//! *shape* is still a bug worth failing loudly on.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One block in a resolved chain: which repo it lives in, its block id, and the owning
/// lane's structured directives, if any.
///
/// [`resolve_explicit_chain`] always produces `directives: None` — an explicit block list
/// is a deliberate bypass of the lane file, not merely a lane with no directives declared.
///
/// `roadmap`/`lane`/`segment` (`EN.11.K` Task 2) identify which `(roadmap, lane, segment)`
/// tuple this step came from in `planning/lane-segments.json` — the same tuple mev's
/// `planning/lane-frontier.json` keys its entries on (see
/// [`gates::find_lane_head`](super::gates::find_lane_head)). [`resolve_lane_chain`] fills
/// all three; [`resolve_explicit_chain`] leaves them `None`, matching `directives: None`'s
/// same "deliberate bypass" contract — an explicit list names no lane segment at all.
///
/// `kind` (`EN.12.E` Task 1) is the step's engine: `block` keeps today's `EngineKind`-
/// selected SDLC invocation, `dispatch` runs a registered workflow instead (`EN.12.E` Task
/// 3+), and `command` is reserved vocabulary only — nothing executes it yet. Absent input
/// defaults to `StepKind::Block`, so a chain with no `kind` behaves exactly as before this
/// field existed. `EngineKind` itself (`orchestration/engine_kind.rs`) is untouched and
/// stays a closed two-variant enum — a `dispatch` step never selects one at all.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChainStep {
    pub repo: String,
    pub block_id: String,
    pub directives: Option<LaneDirectives>,
    pub roadmap: Option<String>,
    pub lane: Option<String>,
    pub segment: Option<usize>,
    pub kind: StepKind,
}

/// A chain step's engine, reserved by `EN.12.E` Task 1. Wire spellings are lowercase
/// (`block` / `dispatch` / `command`) to match `mev`'s eventual emission vocabulary
/// exactly — see [`ChainStep::kind`]. `command` is reserved only; nothing in this repo
/// executes it yet (out of scope for `EN.12.E`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepKind {
    #[default]
    Block,
    Dispatch,
    Command,
}

/// A lane's structured directives, mirroring the shape `mev`'s
/// `brain::lane_segments::LaneDirectives` emits into `planning/lane-segments.json`. See
/// that module for the authoritative grammar — this is a reading-side mirror, not a
/// second definition of the format; the two must stay in lockstep by hand since this
/// crate does not depend on `mev`.
///
/// Deliberately **not** `deny_unknown_fields` (`EN.12.E` Task 1) — an unrecognised key
/// here parses and is dropped rather than hard-failing the whole lane-segments read. See
/// the module doc's "Malformed input is a loud error" section for why: this struct is
/// hand-mirrored with no shared dependency to enforce the mirror, so strict rejection
/// would let mev shipping one new field first break every un-rebuilt engine on every lane
/// segment, not just the one carrying the new field.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct LaneDirectives {
    /// The block ID or operator-gate slug the whole lane waits on before any block in it
    /// may start. Carried as opaque text, exactly as `mev` emits it — this module never
    /// resolves it against the corpus graph itself; the caller's `is_block_open` closure
    /// does that (see [`resolve_lane_chain`]).
    #[serde(default)]
    pub held_until: Option<String>,
    #[serde(default)]
    pub budget: Option<LaneBudget>,
    #[serde(default)]
    pub exclusive_repos: Option<Vec<String>>,
}

/// The `BUDGET` directive's parsed value — see [`LaneDirectives`]. Also not
/// `deny_unknown_fields`, for the same forward-compatibility reason.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LaneBudget {
    pub heavy: bool,
    #[serde(default)]
    pub not_with: Vec<String>,
}

/// One entry of `planning/lane-segments.json`'s `blocks` array, as `mev` emits it
/// (`mev::brain::lane_segments::DerivedBlockPosition`). Only the fields this module
/// actually uses are declared as meaningful; `line` and `origin_roadmap` are read and
/// kept (never silently dropped by an unknown-fields parse failure) but this module has
/// no use for either yet. `kind` (`EN.12.E`) is the `Deserialize` mirror of
/// [`ChainStep::kind`], adjacent to `directives` rather than nested inside it — absent
/// input defaults to [`StepKind::Block`].
#[derive(Debug, Clone, Deserialize)]
struct RawBlockPosition {
    roadmap: String,
    lane: String,
    repo: String,
    id: String,
    #[serde(default)]
    kind: StepKind,
    #[allow(dead_code)]
    #[serde(default)]
    line: usize,
    segment: usize,
    position: usize,
    #[allow(dead_code)]
    #[serde(default)]
    origin_roadmap: Option<String>,
    #[serde(default)]
    directives: Option<LaneDirectives>,
}

/// The whole `planning/lane-segments.json` payload — `mev`'s
/// `brain::lane_segments::LaneSegmentsArtifact`, read-only mirror.
#[derive(Debug, Deserialize)]
struct LaneSegmentsArtifact {
    blocks: Vec<RawBlockPosition>,
}

/// Everything that can go wrong resolving a `(roadmap, lane)` chain. Every variant names
/// the file and/or lane involved — this module never fails silently.
#[derive(Debug)]
pub enum ChainError {
    /// `planning/lane-segments.json` could not be read at all.
    ReadFailed {
        path: PathBuf,
        source: std::io::Error,
    },
    /// `planning/lane-segments.json` was read but did not parse under the expected
    /// shape — a malformed directive, per this module's doc comment, is exactly this
    /// variant, never a silent "treat as absent".
    ParseFailed {
        path: PathBuf,
        source: serde_json::Error,
    },
    /// No block in `planning/lane-segments.json` matches the requested `(roadmap, lane)`
    /// pair.
    LaneNotFound { roadmap: String, lane: String },
    /// The lane's `HELD-UNTIL` directive names a block that [`resolve_lane_chain`]'s
    /// `is_block_open` closure reports as still open. The run refuses to start — this is
    /// not a warning.
    Held {
        roadmap: String,
        lane: String,
        repo: String,
        held_until: String,
    },
}

impl fmt::Display for ChainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChainError::ReadFailed { path, source } => {
                write!(f, "failed to read {}: {source}", path.display())
            }
            ChainError::ParseFailed { path, source } => {
                write!(
                    f,
                    "malformed lane-segments artifact at {}: {source}",
                    path.display()
                )
            }
            ChainError::LaneNotFound { roadmap, lane } => {
                write!(
                    f,
                    "no lane '{lane}' found for roadmap '{roadmap}' in the lane-segments artifact"
                )
            }
            ChainError::Held {
                roadmap,
                lane,
                repo,
                held_until,
            } => {
                write!(
                    f,
                    "lane '{lane}' of roadmap '{roadmap}' (repo '{repo}') refuses to start: \
                     HELD-UNTIL '{held_until}' is still open"
                )
            }
        }
    }
}

impl std::error::Error for ChainError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ChainError::ReadFailed { source, .. } => Some(source),
            ChainError::ParseFailed { source, .. } => Some(source),
            ChainError::LaneNotFound { .. } | ChainError::Held { .. } => None,
        }
    }
}

/// Resolve a `(roadmap, lane)` chain by reading `lane_segments_path` (the corpus-wide
/// `planning/lane-segments.json` `mev` writes) and returning that lane's blocks in file
/// order — `(segment, position)` ascending, mirroring `mev`'s own emission order exactly.
///
/// Before returning, checks the lane's `HELD-UNTIL` directive (if any — every matching
/// block carries the same lane-level directives, so the first one found is authoritative)
/// against `is_block_open`: a still-open held-until target refuses the whole chain via
/// [`ChainError::Held`], naming the held block and the lane's own repo/roadmap/lane. This
/// module never resolves `HELD-UNTIL`'s target against the corpus graph itself — that is
/// the caller's job, kept as an injectable closure exactly like `mev`'s own
/// `resolve_owner` seam, so this module stays independent of how "open" is determined
/// (state.json, a live graph query, or a test double).
pub fn resolve_lane_chain(
    lane_segments_path: &Path,
    roadmap: &str,
    lane: &str,
    is_block_open: &dyn Fn(&str) -> bool,
) -> Result<Vec<ChainStep>, ChainError> {
    let raw =
        std::fs::read_to_string(lane_segments_path).map_err(|source| ChainError::ReadFailed {
            path: lane_segments_path.to_path_buf(),
            source,
        })?;
    let artifact: LaneSegmentsArtifact =
        serde_json::from_str(&raw).map_err(|source| ChainError::ParseFailed {
            path: lane_segments_path.to_path_buf(),
            source,
        })?;

    let mut matching: Vec<&RawBlockPosition> = artifact
        .blocks
        .iter()
        .filter(|b| b.roadmap == roadmap && b.lane == lane)
        .collect();
    if matching.is_empty() {
        return Err(ChainError::LaneNotFound {
            roadmap: roadmap.to_string(),
            lane: lane.to_string(),
        });
    }
    matching.sort_by_key(|b| (b.segment, b.position));

    if let Some(held_until) = matching
        .iter()
        .find_map(|b| b.directives.as_ref().and_then(|d| d.held_until.clone()))
    {
        if is_block_open(&held_until) {
            return Err(ChainError::Held {
                roadmap: roadmap.to_string(),
                lane: lane.to_string(),
                repo: matching[0].repo.clone(),
                held_until,
            });
        }
    }

    Ok(matching
        .into_iter()
        .map(|b| ChainStep {
            repo: b.repo.clone(),
            block_id: b.id.clone(),
            directives: b.directives.clone(),
            roadmap: Some(b.roadmap.clone()),
            lane: Some(b.lane.clone()),
            segment: Some(b.segment),
            kind: b.kind,
        })
        .collect())
}

/// Resolve an explicit `(repo, block_id)` list into a chain, bypassing lane-file parsing
/// entirely — no `planning/lane-segments.json` read, no `HELD-UNTIL` check. Order is
/// preserved exactly as given; every produced [`ChainStep`] carries `directives: None` and
/// `roadmap`/`lane`/`segment: None` — an explicit list names no lane segment, so it cannot
/// be matched against a `lane-frontier.json` entry either.
///
/// This is also the exact function `CONDUCTOR`'s
/// [`super::conductor::propose_chain`] (`EN.12.F` Task 4) calls to turn its
/// finalised, `git log -S`-surviving candidate list into a chain — a
/// conductor-produced chain is therefore never a parallel shape, only ever
/// this function's own output, so nothing downstream of chain resolution can
/// tell a proposed chain from an authored one. See
/// `conductor_produced_chain_step_shape_matches_resolve_explicit_chain`
/// below for a test proving the two calls converge byte-for-byte on the
/// same input.
#[must_use]
pub fn resolve_explicit_chain(blocks: Vec<(String, String)>) -> Vec<ChainStep> {
    blocks
        .into_iter()
        .map(|(repo, block_id)| ChainStep {
            repo,
            block_id,
            directives: None,
            roadmap: None,
            lane: None,
            segment: None,
            kind: StepKind::Block,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Writes `content` to a fresh temp file and returns its path. The file (and its
    /// containing dir) is left on disk under the OS temp dir with a unique name per call
    /// — this repo's other integration fixtures follow the same pattern rather than
    /// wiring in a tempdir crate dependency here.
    fn write_fixture(name: &str, content: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "engine-rs-chain-test-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("lane-segments.json");
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn two_repo_chain_resolves_in_file_order() {
        // Deliberately out of (segment, position) order in the source array — resolution
        // must sort by (segment, position), not array order.
        let json = r#"{
            "blocks": [
                {"roadmap": "r", "lane": "l", "repo": "repo-b", "id": "B.2", "line": 2, "segment": 0, "position": 1, "origin_roadmap": null},
                {"roadmap": "r", "lane": "l", "repo": "repo-a", "id": "A.1", "line": 1, "segment": 0, "position": 0, "origin_roadmap": null},
                {"roadmap": "other", "lane": "l", "repo": "repo-c", "id": "C.1", "line": 1, "segment": 0, "position": 0, "origin_roadmap": null}
            ]
        }"#;
        let path = write_fixture("two-repo", json);
        let steps = resolve_lane_chain(&path, "r", "l", &|_| false).unwrap();
        assert_eq!(
            steps,
            vec![
                ChainStep {
                    repo: "repo-a".into(),
                    block_id: "A.1".into(),
                    directives: None,
                    roadmap: Some("r".into()),
                    lane: Some("l".into()),
                    segment: Some(0),
                    kind: StepKind::Block,
                },
                ChainStep {
                    repo: "repo-b".into(),
                    block_id: "B.2".into(),
                    directives: None,
                    roadmap: Some("r".into()),
                    lane: Some("l".into()),
                    segment: Some(0),
                    kind: StepKind::Block,
                },
            ]
        );
    }

    #[test]
    fn held_until_naming_open_block_refuses_and_names_it() {
        let json = r#"{
            "blocks": [
                {"roadmap": "r", "lane": "l", "repo": "repo-a", "id": "A.1", "line": 1, "segment": 0, "position": 0, "origin_roadmap": null, "directives": {"held_until": "OTHER.1"}}
            ]
        }"#;
        let path = write_fixture("held-open", json);
        let err = resolve_lane_chain(&path, "r", "l", &|token| token == "OTHER.1").unwrap_err();
        match &err {
            ChainError::Held {
                held_until, repo, ..
            } => {
                assert_eq!(held_until, "OTHER.1");
                assert_eq!(repo, "repo-a");
            }
            other => panic!("expected Held, got {other:?}"),
        }
        // The Display message names both the held block and the repo.
        let msg = err.to_string();
        assert!(
            msg.contains("OTHER.1"),
            "message should name the held block: {msg}"
        );
        assert!(
            msg.contains("repo-a"),
            "message should name the repo: {msg}"
        );
    }

    #[test]
    fn held_until_naming_closed_block_proceeds() {
        let json = r#"{
            "blocks": [
                {"roadmap": "r", "lane": "l", "repo": "repo-a", "id": "A.1", "line": 1, "segment": 0, "position": 0, "origin_roadmap": null, "directives": {"held_until": "OTHER.1"}}
            ]
        }"#;
        let path = write_fixture("held-closed", json);
        let steps = resolve_lane_chain(&path, "r", "l", &|_| false).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].block_id, "A.1");
    }

    #[test]
    fn explicit_block_list_bypasses_lane_file_parsing() {
        // A path that does not exist on disk — if this were touched, the call would fail.
        let bogus_path = PathBuf::from("/nonexistent/does-not-exist/lane-segments.json");
        assert!(!bogus_path.exists());
        let steps = resolve_explicit_chain(vec![
            ("repo-a".to_string(), "A.1".to_string()),
            ("repo-b".to_string(), "B.2".to_string()),
        ]);
        assert_eq!(
            steps,
            vec![
                ChainStep {
                    repo: "repo-a".into(),
                    block_id: "A.1".into(),
                    directives: None,
                    ..Default::default()
                },
                ChainStep {
                    repo: "repo-b".into(),
                    block_id: "B.2".into(),
                    directives: None,
                    ..Default::default()
                },
            ]
        );
    }

    #[test]
    fn malformed_directive_shape_is_a_loud_error() {
        // `budget` must be an object per the grammar; a bare string is malformed and must
        // fail the whole parse rather than silently becoming "no directives".
        let json = r#"{
            "blocks": [
                {"roadmap": "r", "lane": "l", "repo": "repo-a", "id": "A.1", "line": 1, "segment": 0, "position": 0, "origin_roadmap": null, "directives": {"budget": "HEAVY"}}
            ]
        }"#;
        let path = write_fixture("malformed-budget", json);
        let err = resolve_lane_chain(&path, "r", "l", &|_| false).unwrap_err();
        assert!(matches!(err, ChainError::ParseFailed { .. }), "got {err:?}");
    }

    /// `EN.12.E` Task 1's forward-compatibility positive control (seam 15).
    ///
    /// **Observed FAILING** with `#[serde(deny_unknown_fields)]` temporarily re-added to
    /// `LaneDirectives` (the pre-`EN.12.E` state), via
    /// `cargo nextest run -p engine-core chain::tests::unrecognised_directive_key_is_tolerated_forward_compat`:
    ///
    /// ```text
    /// thread 'workflows::orchestration::chain::tests::unrecognised_directive_key_is_tolerated_forward_compat' (330542126) panicked at crates/engine-core/src/workflows/orchestration/chain.rs:492:69:
    /// called `Result::unwrap()` on an `Err` value: ParseFailed { path: "/var/folders/.../lane-segments.json", source: Error("unknown field `future_field`, expected one of `held_until`, `budget`, `exclusive_repos`", line: 3, column: 195) }
    /// test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 2248 filtered out
    /// ```
    ///
    /// **Observed PASSING** after removing `deny_unknown_fields` again (this commit's
    /// state): `cargo nextest run -p engine-core chain::tests::unrecognised_directive_key_is_tolerated_forward_compat`
    /// reports `1 passed`.
    #[test]
    fn unrecognised_directive_key_is_tolerated_forward_compat() {
        let json = r#"{
            "blocks": [
                {"roadmap": "r", "lane": "l", "repo": "repo-a", "id": "A.1", "line": 1, "segment": 0, "position": 0, "origin_roadmap": null, "directives": {"held_until": "OTHER.1", "future_field": "some-forward-compatible-value"}}
            ]
        }"#;
        let path = write_fixture("unrecognised-directive-key", json);
        let steps = resolve_lane_chain(&path, "r", "l", &|_| false).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(
            steps[0]
                .directives
                .as_ref()
                .and_then(|d| d.held_until.clone()),
            Some("OTHER.1".to_string()),
            "the recognised fields alongside the unknown one must still parse correctly"
        );
    }

    #[test]
    fn lane_not_found_is_reported() {
        let json = r#"{"blocks": []}"#;
        let path = write_fixture("empty", json);
        let err = resolve_lane_chain(&path, "r", "l", &|_| false).unwrap_err();
        assert!(
            matches!(err, ChainError::LaneNotFound { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn missing_file_is_reported() {
        let path = PathBuf::from("/nonexistent/does-not-exist/lane-segments.json");
        let err = resolve_lane_chain(&path, "r", "l", &|_| false).unwrap_err();
        assert!(matches!(err, ChainError::ReadFailed { .. }), "got {err:?}");
    }

    #[test]
    fn chain_step_with_no_kind_field_defaults_to_block() {
        let json = r#"{
            "blocks": [
                {"roadmap": "r", "lane": "l", "repo": "repo-a", "id": "A.1", "line": 1, "segment": 0, "position": 0, "origin_roadmap": null}
            ]
        }"#;
        let path = write_fixture("no-kind", json);
        let steps = resolve_lane_chain(&path, "r", "l", &|_| false).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].kind, StepKind::Block);
    }

    #[test]
    fn explicit_chain_steps_default_to_block_kind() {
        let steps = resolve_explicit_chain(vec![("repo-a".to_string(), "A.1".to_string())]);
        assert_eq!(steps[0].kind, StepKind::Block);
    }

    #[test]
    fn step_kind_round_trips_through_serde_with_lowercase_wire_spellings() {
        for (kind, wire) in [
            (StepKind::Block, "\"block\""),
            (StepKind::Dispatch, "\"dispatch\""),
            (StepKind::Command, "\"command\""),
        ] {
            let serialized = serde_json::to_string(&kind).unwrap();
            assert_eq!(serialized, wire, "unexpected wire spelling for {kind:?}");
            let deserialized: StepKind = serde_json::from_str(wire).unwrap();
            assert_eq!(deserialized, kind, "round-trip mismatch for {wire}");
        }
    }

    // ── `CONDUCTOR` chain-shape equivalence (`EN.12.F` Task 4) ──────────

    /// A `git`/`mev` [`crate::policy::emit_state::Runner`] stub that reports
    /// no matching commits for any `git log -S` pickaxe search — every
    /// candidate survives the pre-flight untouched.
    struct NoHistoryOutput {
        stdout: String,
    }

    impl crate::policy::emit_state::CommandOutputLike for NoHistoryOutput {
        fn status(&self) -> i32 {
            0
        }
        fn stdout(&self) -> &str {
            &self.stdout
        }
        fn stderr(&self) -> &str {
            ""
        }
    }

    #[test]
    fn conductor_produced_chain_step_shape_matches_resolve_explicit_chain() {
        use super::super::conductor::{propose_chain, ConductorConfig};

        let dir = tempfile::tempdir().expect("tempdir");
        let objective_path = dir.path().join("objective.md");
        fs::write(&objective_path, "Ship EN.12.F.\n").unwrap();
        let config = ConductorConfig::new().with_objective_path(&objective_path);

        let slate_json = r#"{
            "derived_at": "2026-09-03T00:00:00-07:00",
            "entries": [
                {
                    "roadmap": "r", "lane": "l", "segment": 0, "repo": "repo-a",
                    "key": "repo-a:A.1", "id": "A.1", "title": "t", "status": "open",
                    "unmet_blocks": [], "unmet_gates": [], "startable": true
                },
                {
                    "roadmap": "r", "lane": "l", "segment": 0, "repo": "repo-b",
                    "key": "repo-b:B.2", "id": "B.2", "title": "t", "status": "open",
                    "unmet_blocks": [], "unmet_gates": [], "startable": true
                }
            ],
            "gate_ranks": []
        }"#;
        let slate: crate::workflows::orchestration::gates::FrontierArtifact =
            serde_json::from_str(slate_json).expect("well-formed frontier fixture");

        let proposed = vec![
            ("repo-a".to_string(), "A.1".to_string()),
            ("repo-b".to_string(), "B.2".to_string()),
        ];
        let tasks_json_exists: super::super::conductor::TasksJsonChecker =
            std::sync::Arc::new(|_repo: &str, _block_id: &str| true);
        let git_runner: crate::policy::emit_state::Runner<NoHistoryOutput> =
            std::sync::Arc::new(|_program, _args, _cwd| {
                Ok(NoHistoryOutput {
                    stdout: String::new(),
                })
            });

        let outcome = propose_chain(&config, &proposed, &slate, &tasks_json_exists, &git_runner)
            .expect("subset + tasks.json + pre-flight all pass");

        let directly_resolved = resolve_explicit_chain(proposed);
        assert_eq!(
            outcome.chain, directly_resolved,
            "a conductor-produced chain must be byte-identical to \
             `resolve_explicit_chain`'s own output for the same survivors"
        );
    }

    #[test]
    fn raw_block_position_with_explicit_kind_is_carried_onto_chain_step() {
        let json = r#"{
            "blocks": [
                {"roadmap": "r", "lane": "l", "repo": "repo-a", "id": "A.1", "line": 1, "segment": 0, "position": 0, "origin_roadmap": null, "kind": "dispatch"}
            ]
        }"#;
        let path = write_fixture("explicit-dispatch-kind", json);
        let steps = resolve_lane_chain(&path, "r", "l", &|_| false).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].kind, StepKind::Dispatch);
    }
}
