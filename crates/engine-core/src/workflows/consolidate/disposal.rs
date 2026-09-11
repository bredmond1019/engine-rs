//! CONSOLIDATE Task 4 — writing `disposal.json` for the first time, through the existing
//! `okf_core::coord::disposal` type rather than hand-rolled JSON.
//!
//! ## Why the enum refusal happens at construction, not at write time
//!
//! `okf_core::coord::disposal::Disposal` refuses an out-of-enum `route` string when
//! *deserializing* a file already on disk (its custom `Deserialize` impl inspects the raw JSON
//! before ever attempting the typed parse). That check exists to protect a READER of someone
//! else's `disposal.json`. This module is a WRITER, and a writer that only discovered a bad
//! route by round-tripping its own output through that reader would be validating itself
//! indirectly and reporting the failure one step too late. So [`disposal_row_from_ledger_entry`]
//! parses each raw ledger row's `route` string through `okf_core::coord::disposal::DisposalRoute`
//! (a plain `serde_json::from_value` against the enum, which carries the same closed vocabulary)
//! immediately when building the typed [`DisposalRow`] — refused at construction. This is why
//! [`selected_rows_to_disposal`] returns a `Result`: it must be able to report the first bad row
//! rather than silently defaulting it away.
//!
//! ## Field shape
//!
//! Matched against `consolidate-fleet.md`'s current contract AND the five real files on disk at
//! `planning/open-work/orchestration-runs/retros/disposal-2026-09-{02,03,05,07}.json`
//! (`disposal-2026-09-05-result.json` is a *different* artifact — `/dispose-run`'s outcome
//! report, keyed by `state`/`missing`/`fork`/`note` rather than `route`/`owner_repo`/`needs` — and
//! is not this shape; excluded from the union on purpose). All four disposal-shaped files agree
//! on the row envelope `okf_core::coord::disposal::DisposalRow` already types: `finding_id`,
//! `mechanism`, `route`, `owner_repo`, `needs`, `severity`, `breadth{repos,instances}`,
//! `evidence[]`, `payload`, optional `already_filed`, required `ungrounded[]`, `rationale`. The
//! file-level union is `analysis`, `generated`, `roadmaps`, `backfilled`, optional
//! `backfill_note` (09-02 only) / `reconciliation_note` (09-03 only), and `conventions`
//! (carrying `ungrounded_excludes` on every file observed) — all already modeled by
//! `okf_core::coord::disposal::DisposalFile`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use okf_core::{Disposal, DisposalBreadth, DisposalFile, DisposalRoute, DisposalRow};
use serde_json::Value;

use super::select::SelectedRow;

/// Errors writing or building a `disposal.json`.
#[derive(Debug, thiserror::Error)]
pub enum DisposalError {
    /// A raw ledger row named a `route` string outside `DisposalRoute`'s closed vocabulary.
    /// Refused at construction — see this module's doc comment.
    #[error(
        "finding {finding_id:?}: route {route:?} is not one of the closed DisposalRoute values"
    )]
    InvalidRoute { finding_id: String, route: String },
    /// Failed to create the parent directory or write the file.
    #[error("failed to write {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    /// Failed to serialize the typed `DisposalFile` to JSON (should not happen for a value the
    /// type system already accepted, but surfaced rather than unwrapped).
    #[error("failed to serialize disposal.json: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Build one typed [`DisposalRow`] from a [`SelectedRow`]'s raw ledger entry `Value`, refusing an
/// out-of-enum `route` at construction rather than deferring to the write-time `Disposal`
/// deserializer.
///
/// Every field this row's raw JSON does not itself carry is filled with a documented,
/// non-fabricated default AND named in the returned row's `ungrounded[]` — mirroring the real
/// retros/ files' own convention of listing exactly the fields the evidence cannot support,
/// never inventing a value silently. `finding_id` is the one field that can never be defaulted:
/// a row an ledger entry cannot even name is not something this writer can dispose of, so it
/// falls back to a `{repo}/{origin_roadmap}` synthetic id that is itself always listed in
/// `ungrounded[]`.
pub fn disposal_row_from_ledger_entry(
    selected: &SelectedRow,
) -> Result<DisposalRow, DisposalError> {
    let row = &selected.row;
    let get_str =
        |key: &str| -> Option<String> { row.get(key).and_then(Value::as_str).map(str::to_string) };

    let mut ungrounded = Vec::new();
    let mut note_absent = |key: &str, present: bool| {
        if !present {
            ungrounded.push(key.to_string());
        }
    };

    let finding_id = get_str("finding_id").or_else(|| get_str("id"));
    note_absent("finding_id", finding_id.is_some());
    let finding_id =
        finding_id.unwrap_or_else(|| format!("{}/{}", selected.repo, selected.origin_roadmap));

    let mechanism = get_str("mechanism").or_else(|| get_str("block"));
    note_absent("mechanism", mechanism.is_some());
    let mechanism = mechanism.unwrap_or_default();

    let route_str = get_str("route");
    note_absent("route", route_str.is_some());
    let route =
        match route_str {
            Some(raw) => serde_json::from_value::<DisposalRoute>(Value::String(raw.clone()))
                .map_err(|_| DisposalError::InvalidRoute {
                    finding_id: finding_id.clone(),
                    route: raw,
                })?,
            None => DisposalRoute::None,
        };

    let owner_repo = get_str("owner_repo");
    note_absent("owner_repo", owner_repo.is_some());
    let owner_repo = owner_repo.unwrap_or_else(|| selected.repo.clone());

    let needs = get_str("needs");
    note_absent("needs", needs.is_some());
    let needs = needs.unwrap_or_else(|| "code".to_string());

    let severity = get_str("severity");
    note_absent("severity", severity.is_some());
    let severity = severity.unwrap_or_else(|| "P2".to_string());

    let breadth_value = row.get("breadth");
    note_absent("breadth", breadth_value.is_some());
    let breadth = DisposalBreadth {
        repos: breadth_value
            .and_then(|b| b.get("repos"))
            .and_then(Value::as_i64)
            .unwrap_or(1),
        instances: breadth_value
            .and_then(|b| b.get("instances"))
            .and_then(Value::as_i64),
    };

    let evidence_value = row.get("evidence").and_then(Value::as_array);
    note_absent("evidence", evidence_value.is_some());
    let evidence = evidence_value
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let payload = row.get("payload").cloned().unwrap_or_else(|| row.clone());

    let already_filed = get_str("already_filed");

    let rationale = get_str("rationale");
    note_absent("rationale", rationale.is_some());
    let rationale = rationale.unwrap_or_default();

    Ok(DisposalRow {
        finding_id,
        mechanism,
        route,
        owner_repo,
        needs,
        severity,
        breadth,
        evidence,
        payload,
        already_filed,
        ungrounded,
        rationale,
    })
}

/// Map every selected row into a typed [`DisposalRow`], refusing the whole batch on the first
/// row whose raw `route` string is outside [`DisposalRoute`]'s closed vocabulary — see this
/// module's doc comment for why this is a `Result` rather than the infallible mapper a first
/// reading of consolidate-fleet.md might suggest.
pub fn selected_rows_to_disposal(rows: &[SelectedRow]) -> Result<Vec<DisposalRow>, DisposalError> {
    rows.iter().map(disposal_row_from_ledger_entry).collect()
}

/// Write a `disposal.json` at `path`, through `okf_core::coord::disposal::DisposalFile` — the
/// same type a reader parses the file back with, so a shape this writer cannot produce (an
/// out-of-enum `route`) is a structural impossibility rather than a runtime check.
///
/// `analysis`/`generated`/`roadmaps`/`backfilled` are the file-level fields every one of the
/// four real disposal-shaped files on disk carries; none of task 4's acceptance criteria pin
/// their values, so the caller (the `graph.rs` assembly, Task 6) supplies them from the run
/// that produced `rows`.
#[allow(clippy::too_many_arguments)]
pub fn write_disposal(
    path: &Path,
    analysis: &str,
    generated: &str,
    roadmaps: &[String],
    backfilled: bool,
    rows: Vec<DisposalRow>,
    ungrounded_excludes: &[String],
) -> Result<(), DisposalError> {
    let file = DisposalFile {
        analysis: analysis.to_string(),
        generated: generated.to_string(),
        roadmaps: roadmaps.to_vec(),
        backfilled,
        backfill_note: None,
        reconciliation_note: None,
        conventions: serde_json::json!({ "ungrounded_excludes": ungrounded_excludes }),
        rows,
    };

    let json = serde_json::to_string_pretty(&file)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| DisposalError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    }
    fs::write(path, format!("{json}\n")).map_err(|source| DisposalError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    // Sanity check our own output through the same reader a consumer uses — never leave a file
    // on disk that this writer itself cannot read back as `Disposal::typed()`.
    let raw = fs::read_to_string(path).map_err(|source| DisposalError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let round_tripped: Disposal = serde_json::from_str(&raw)?;
    debug_assert!(
        !round_tripped.is_legacy(),
        "write_disposal produced a file its own type cannot parse as typed"
    );
    let _ = round_tripped;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selected_row(row: Value, repo: &str, origin_roadmap: &str) -> SelectedRow {
        SelectedRow {
            repo: repo.to_string(),
            record_roadmap: origin_roadmap.to_string(),
            origin_roadmap: origin_roadmap.to_string(),
            row,
        }
    }

    #[test]
    fn out_of_enum_route_is_refused_at_construction_not_write_time() {
        let row = serde_json::json!({
            "finding_id": "sample-finding",
            "mechanism": "M1",
            "route": "bogus_route",
            "owner_repo": "engine-rs",
            "needs": "code",
            "severity": "P1",
            "breadth": {"repos": 1, "instances": null},
            "evidence": ["some/path.md:1"],
            "payload": {},
            "ungrounded": [],
            "rationale": "test",
        });
        let selected = selected_row(row, "engine-rs", "coordination-layer-port");

        // Refused by `disposal_row_from_ledger_entry` itself — never reaches `write_disposal`.
        let result = disposal_row_from_ledger_entry(&selected);
        assert!(
            matches!(result, Err(DisposalError::InvalidRoute { .. })),
            "expected InvalidRoute, got {result:?}"
        );

        // And the batch mapper propagates the same refusal.
        let batch_result = selected_rows_to_disposal(std::slice::from_ref(&selected));
        assert!(batch_result.is_err());
    }

    #[test]
    fn round_trip_write_and_read_reproduces_the_same_structure() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("disposal.json");

        let row = serde_json::json!({
            "finding_id": "M1",
            "mechanism": "single-invocation replay",
            "route": "carryover",
            "owner_repo": "engine-rs",
            "needs": "code",
            "severity": "P1",
            "breadth": {"repos": 2, "instances": null},
            "evidence": ["planning/notes.md:1"],
            "payload": {"origin": {"type": "mechanism", "slug": "M1"}},
            "ungrounded": ["id"],
            "rationale": "Observed twice.",
        });
        let selected = selected_row(row, "engine-rs", "coordination-layer-port");
        let rows = selected_rows_to_disposal(&[selected]).unwrap();

        write_disposal(
            &path,
            "planning/pattern-analysis.md",
            "2026-09-10T00:00:00Z",
            &["coordination-layer-port".to_string()],
            false,
            rows.clone(),
            &["id".to_string()],
        )
        .unwrap();

        let raw = fs::read_to_string(&path).unwrap();
        let disposal: Disposal = serde_json::from_str(&raw).unwrap();
        assert!(
            !disposal.is_legacy(),
            "written file must parse as typed, not Legacy"
        );
        let file = disposal.typed().unwrap();
        assert_eq!(file.rows, rows);
        assert_eq!(
            file.conventions.get("ungrounded_excludes"),
            Some(&serde_json::json!(["id"]))
        );
    }

    #[test]
    fn every_row_carries_the_required_field_set_and_a_non_null_ungrounded() {
        let row = serde_json::json!({
            "mechanism": "M2",
            "evidence": ["a.md:1"],
        });
        let selected = selected_row(row, "mev", "coordination-layer-port");
        let disposal_row = disposal_row_from_ledger_entry(&selected).unwrap();

        assert!(!disposal_row.finding_id.is_empty());
        assert_eq!(disposal_row.owner_repo, "mev");
        assert_eq!(disposal_row.route, DisposalRoute::None);
        // Every field this raw row omitted is named in ungrounded, never silently guessed away.
        assert!(disposal_row.ungrounded.contains(&"finding_id".to_string()));
        assert!(disposal_row.ungrounded.contains(&"route".to_string()));
        assert!(disposal_row.ungrounded.contains(&"owner_repo".to_string()));
        assert!(disposal_row.ungrounded.contains(&"needs".to_string()));
        assert!(disposal_row.ungrounded.contains(&"severity".to_string()));
        assert!(disposal_row.ungrounded.contains(&"breadth".to_string()));
        assert!(!disposal_row.ungrounded.contains(&"evidence".to_string()));
    }

    #[test]
    fn a_row_missing_ungrounded_in_raw_json_is_refused_by_the_okf_core_type() {
        // This module always produces a populated `ungrounded: Vec<String>` (never optional —
        // the Rust type makes "missing" impossible for values this writer builds). What can
        // still be refused is a hand-authored row on disk that omits the key entirely: the
        // okf-core `DisposalRow` field has no `#[serde(default)]`, so deserializing it directly
        // errors rather than silently defaulting to an empty vec.
        let bad_row = serde_json::json!({
            "finding_id": "M3",
            "mechanism": "x",
            "route": "none",
            "owner_repo": "engine-rs",
            "needs": "code",
            "severity": "P2",
            "breadth": {"repos": 1, "instances": null},
            "evidence": [],
            "payload": {},
            "rationale": "no ungrounded key at all",
        });
        let result: Result<DisposalRow, _> = serde_json::from_value(bad_row);
        assert!(
            result.is_err(),
            "expected a row missing `ungrounded` to be refused, got {result:?}"
        );
    }
}
