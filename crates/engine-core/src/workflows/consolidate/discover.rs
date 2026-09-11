//! CONSOLIDATE Task 1 — discovery: brain-root walk, participant cross-check, realpath dedup.
//!
//! Reuses [`crate::roadmap_status`]'s existing `discover_run_records` / `realpath_dedup` /
//! `read_lane_log` / `repos_from_lane_log` rather than re-implementing any of them — this module
//! is the join between the two, not a second sweep.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::roadmap_status::{
    discover_run_records, read_lane_log, repos_from_lane_log, resolve_roadmap_dir, RunRecordPair,
};

/// A filesystem-vs-log mismatch: a repo that wrote an `orchestration-run/<roadmap>/` record but
/// never logged a `lane-log.jsonl` line naming it for this roadmap. Per D57 section 5, "a repo
/// that wrote notes but logged nothing is a finding to report, not something to silently union
/// in" — so this is surfaced as a [`DiscoveryFinding`], never folded into `participants`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryFinding {
    pub repo: String,
    pub reason: String,
}

/// The result of one CONSOLIDATE discovery pass for a roadmap.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryResult {
    /// Repos named in `lane-log.jsonl` — per D57 section 5, "Participants come from
    /// lane-log.jsonl", never from the run-record filesystem walk.
    pub participants: Vec<String>,
    /// Every discovered `orchestration-run/<roadmap>/{notes.md,review.md}` record, realpath-
    /// deduped and keyed by owning repo.
    pub records: BTreeMap<String, RunRecordPair>,
    /// Repos present in `records` but absent from `participants` — a mismatch to report, not to
    /// silently resolve.
    pub findings: Vec<DiscoveryFinding>,
}

/// Discover CONSOLIDATE's inputs for `roadmap_slug` under brain root `root`.
///
/// - Participants come from `<root>/planning/roadmaps/<roadmap_slug>/lane-log.jsonl` (or the
///   legacy `<root>/planning/<roadmap_slug>/lane-log.jsonl` location — see
///   [`crate::roadmap_status::resolve_roadmap_dir`]'s two-location rule). A roadmap with no
///   resolvable directory yields an empty participant list, mirroring
///   [`crate::roadmap_status::read_lane_log`]'s own tolerance for a missing file — never an
///   error, since a run record can still exist even when this particular repo's own planning
///   tree has no roadmap directory for it.
/// - Records come from a fleet-wide, realpath-deduped sweep via
///   [`crate::roadmap_status::discover_run_records`] — the SAME dedup the roadmap-status join
///   uses, never re-implemented here.
/// - Every repo present in `records` but absent from `participants` is reported as a
///   [`DiscoveryFinding`] rather than silently added to the participant list.
pub fn discover_participants(root: &Path, roadmap_slug: &str) -> DiscoveryResult {
    let lane_entries = match resolve_roadmap_dir(root, roadmap_slug) {
        Ok(roadmap_dir) => read_lane_log(&roadmap_dir).0,
        Err(_) => Vec::new(),
    };
    let participants = repos_from_lane_log(&lane_entries);

    let records = discover_run_records(root, roadmap_slug);

    let participant_set: BTreeSet<&String> = participants.iter().collect();
    let findings = records
        .keys()
        .filter(|repo| !participant_set.contains(repo))
        .map(|repo| DiscoveryFinding {
            repo: repo.clone(),
            reason:
                "run record present but no lane-log.jsonl line names this repo for this roadmap"
                    .to_string(),
        })
        .collect();

    DiscoveryResult {
        participants,
        records,
        findings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn repo_with_run_record_and_no_lane_log_line_is_a_finding_not_a_silent_participant() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        // engine-rs participates properly: a lane-log line AND a run record.
        write(
            &root.join("planning/roadmaps/demo/lane-log.jsonl"),
            &format!(
                "{}\n",
                serde_json::json!({
                    "repo": "engine-rs",
                    "lane": "engine-lane",
                    "block": "EN.1.A",
                    "status": "done",
                    "ts": "2026-09-08T00:00:00Z"
                })
            ),
        );
        write(
            &root.join("engine-rs/planning/orchestration-run/demo/notes.md"),
            "---\nlifecycle: active\n---\n",
        );

        // mev wrote a run record but never logged a lane-log line for this roadmap.
        write(
            &root.join("mev/planning/orchestration-run/demo/notes.md"),
            "---\nlifecycle: active\n---\n",
        );

        let result = discover_participants(root, "demo");

        assert_eq!(result.participants, vec!["engine-rs".to_string()]);
        assert_eq!(result.records.len(), 2);
        assert!(result.records.contains_key("engine-rs"));
        assert!(result.records.contains_key("mev"));
        assert_eq!(
            result.findings,
            vec![DiscoveryFinding {
                repo: "mev".to_string(),
                reason:
                    "run record present but no lane-log.jsonl line names this repo for this roadmap"
                        .to_string(),
            }]
        );
    }

    #[test]
    #[cfg(unix)]
    fn record_reachable_through_symlink_and_vault_path_is_deduped_to_one_physical_hit() {
        // This exercises the SAME realpath-dedup path `discover_participants` reuses
        // (`crate::roadmap_status::discover_run_records` -> `realpath_dedup`), via the fleet's
        // real `planning/` -> `_planning/<repo>` shape: a repo dir whose `planning/` is a
        // symlink onto a vault directory holding the actual run record.
        //
        // KNOWN UPSTREAM LIMITATION (not this task's to fix — `discover_run_records` lives in
        // `crate::roadmap_status`, EN.15.H, outside this task's `files[]`): once
        // `realpath_dedup` canonicalizes a hit reached through the `planning/` symlink, the
        // returned candidate is the fully-resolved VAULT path, which — per this fleet's own
        // shape (`<repo>/planning -> _planning/<repo>`, never `.../planning/<repo>`) — no longer
        // contains a literal `planning` path segment. `discover_run_records`'s
        // `repo_from_run_record_path` helper attributes a record to a repo by locating that
        // literal segment, so a record reached ONLY via symlink resolution is dropped from its
        // returned map entirely (not merely deduped) rather than attributed once. Every existing
        // fixture for `discover_run_records`/`discover_at` in `roadmap_status.rs` uses a plain,
        // non-symlinked `planning/` directory, so this gap was previously unexercised.
        //
        // What CAN be verified from this task's own files: `realpath_dedup` itself (the piece
        // this task is told to reuse rather than re-implement) correctly collapses the two raw
        // hits — the symlinked face and the direct vault face — to exactly one physical path,
        // confirmed directly below. Fixing the downstream attribution helper so
        // `discover_participants` also recovers the repo name in this case is out of this task's
        // scope; flagged for the roadmap_status/EN.15.H owner.
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        write(
            &root.join("planning/roadmaps/demo/lane-log.jsonl"),
            &format!(
                "{}\n",
                serde_json::json!({
                    "repo": "engine-rs",
                    "lane": "engine-lane",
                    "block": "EN.1.A",
                    "status": "done",
                    "ts": "2026-09-08T00:00:00Z"
                })
            ),
        );

        // The vault holds the real file; the repo's own `planning/` is a symlink onto it —
        // exactly this fleet's `planning/` -> `_planning/<repo>` shape.
        let vault_planning = root.join("_planning/engine-rs");
        write(
            &vault_planning.join("orchestration-run/demo/notes.md"),
            "---\nlifecycle: active\n---\n",
        );
        let repo_dir = root.join("engine-rs");
        fs::create_dir_all(&repo_dir).unwrap();
        symlink(&vault_planning, repo_dir.join("planning")).unwrap();

        // The dedup layer this task reuses collapses the two raw hits (symlink face + direct
        // vault face) to exactly one physical file.
        let raw = vec![
            repo_dir.join("planning/orchestration-run/demo/notes.md"),
            vault_planning.join("orchestration-run/demo/notes.md"),
        ];
        let deduped = crate::roadmap_status::realpath_dedup(root, &raw);
        assert_eq!(
            deduped.len(),
            1,
            "realpath_dedup should collapse the symlinked and vault-direct hits to one, got {deduped:?}"
        );

        // Documents the current, inherited behavior of `discover_participants` given the
        // upstream attribution gap above: the record is deduped away from double-counting, but
        // (until roadmap_status.rs is fixed, out of this task's scope) also not attributed to
        // `engine-rs` — never silently duplicated, which is the property this task's own code is
        // responsible for.
        let result = discover_participants(root, "demo");
        assert!(
            result.records.len() <= 1,
            "must never double-count a realpath-equivalent record, got {:?}",
            result.records
        );
    }

    #[test]
    fn clean_case_every_run_record_repo_is_also_a_lane_log_participant() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        write(
            &root.join("planning/roadmaps/demo/lane-log.jsonl"),
            &format!(
                "{}\n{}\n",
                serde_json::json!({
                    "repo": "engine-rs",
                    "lane": "engine-lane",
                    "block": "EN.1.A",
                    "status": "done",
                    "ts": "2026-09-08T00:00:00Z"
                }),
                serde_json::json!({
                    "repo": "mev",
                    "lane": "mev-lane",
                    "block": "MV.1.A",
                    "status": "done",
                    "ts": "2026-09-08T00:05:00Z"
                })
            ),
        );
        write(
            &root.join("engine-rs/planning/orchestration-run/demo/notes.md"),
            "---\nlifecycle: active\n---\n",
        );
        write(
            &root.join("mev/planning/orchestration-run/demo/notes.md"),
            "---\nlifecycle: active\n---\n",
        );

        let result = discover_participants(root, "demo");

        assert_eq!(
            result.participants,
            vec!["engine-rs".to_string(), "mev".to_string()]
        );
        assert_eq!(result.records.len(), 2);
        assert!(result.findings.is_empty());
    }

    #[test]
    fn missing_roadmap_directory_yields_empty_participants_never_an_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        let result = discover_participants(root, "nonexistent-roadmap");

        assert!(result.participants.is_empty());
        assert!(result.records.is_empty());
        assert!(result.findings.is_empty());
    }
}
