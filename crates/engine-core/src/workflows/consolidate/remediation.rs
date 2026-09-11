//! CONSOLIDATE Task 5 — promoting a failing verification-ledger entry into HQ's
//! `docs/sandbox/remediation.json` + `docs/sandbox/findings.json`, as the single writer of the
//! global finding number.
//!
//! [`orchestration::ledger::Remediation`] is *deliberately* thin — `{block, opened_at, note}`,
//! with no `finding` field at all (see that type's own doc comment). Minting the global integer
//! and the `REM-NNN` id is this module's job, exercised through [`promote_remediation`].
//!
//! ## Idempotency
//!
//! [`promote_remediation`] is idempotent on the ledger entry's own `id`: a promoted remediation
//! entry's `origin_note` always embeds a `ledger-entry:<id>` marker, and a second call for the
//! same ledger entry finds that marker and returns [`PromoteOutcome::AlreadyPromoted`] without
//! touching either file or shelling out to any script.
//!
//! ## Write boundary (out of scope)
//!
//! This function writes NOTHING to any repo's `planning/state.json` — including the
//! `{type: remediation, slug: REM-NNN}` origin stamp a remediated block would otherwise carry.
//! It is returned on [`PromoteOutcome::Promoted`] as a [`ProposedOriginStamp`] instead, for a
//! caller (or the operator) to apply through a real state-editing verb.
//!
//! ## Un-gateable evidence (D64)
//!
//! `check_remediation.py` / `render_findings.py` / `render_remediation.py` resolve their own
//! root via `Path(__file__).resolve().parent.parent` — none takes a `--root`/CLI root argument —
//! so a test cannot point them at a fixture by passing a path. The test suite below instead
//! copies the scripts themselves alongside a minimal `docs/sandbox/*.json` fixture into a
//! `tempfile::tempdir()`, preserving the real `scripts/` + `docs/sandbox/` sibling layout, so
//! `__file__`-relative resolution lands inside the copy. The real HQ tree is never touched by a
//! test.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Map, Value};

use crate::workflows::orchestration::ledger::LedgerEntry;

/// Where the remediation ledger + findings register live, relative to an HQ root.
const REMEDIATION_REL: &str = "docs/sandbox/remediation.json";
const FINDINGS_REL: &str = "docs/sandbox/findings.json";

/// Errors promoting a ledger entry into the HQ remediation/findings pair.
#[derive(Debug, thiserror::Error)]
pub enum PromoteError {
    /// The ledger entry carries no [`Remediation`](super::super::orchestration::ledger::Remediation)
    /// — nothing to promote. `LedgerEntry`'s own construction rules already refuse a
    /// `remediation` on anything but a `failed`/`blocked` entry, so this only fires when the
    /// field is simply absent.
    #[error("ledger entry {id} carries no remediation to promote")]
    NotFailing { id: String },
    /// Failed to read or write a file.
    #[error("I/O error on {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// A file did not parse as JSON.
    #[error("{path} is not valid JSON: {source}")]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    /// A file parsed but was not the expected top-level shape.
    #[error("{path} is not the expected shape: {message}")]
    Malformed { path: PathBuf, message: String },
    /// A required promotion script could not be spawned at all (missing `python3`, missing
    /// script file, etc).
    #[error("failed to spawn {script}: {source}")]
    Spawn {
        script: PathBuf,
        source: std::io::Error,
    },
    /// `check_remediation.py` (run after writing) exited non-zero — surfaced instead of leaving
    /// an inconsistent remediation/findings pair on disk.
    #[error("check_remediation.py reported violations after promotion:\n{0}")]
    ValidationFailed(String),
}

/// The `{type: remediation, slug: REM-NNN}` origin stamp a remediated block would carry —
/// reported as a proposed edit only. This module never applies it (out of scope: no repo's
/// `state.json` is written here).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposedOriginStamp {
    /// The repo owning `block`, derived from the block id's fleet-prefix (see
    /// [`repo_for_block`]).
    pub repo: String,
    /// The block id the ledger entry's `remediation.block` names.
    pub block: String,
    /// Always `"remediation"` — `block.schema.json`'s `origin.type` enum value this stamp uses.
    pub origin_type: &'static str,
    /// The freshly-minted `REM-NNN` id.
    pub slug: String,
}

/// What [`promote_remediation`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromoteOutcome {
    /// This ledger entry's id was already promoted — nothing written, no script invoked.
    AlreadyPromoted { rem_id: String },
    /// A new `remediation.json` + `findings.json` entry pair was written and validated.
    Promoted {
        rem_id: String,
        finding: u64,
        proposed_state_edit: ProposedOriginStamp,
    },
}

/// Map a block id's fleet prefix (the segment before its first `.`) to the repo slug that owns
/// it, mirroring `brain.toml`'s `[[repos]]` table. Only the prefixes this fleet's remediation
/// tooling has actually seen in `blocks[]`/residue references
/// (`scripts/check_remediation.py`'s own `block_ref` regex) are named; an unrecognized prefix
/// falls back to itself lower-cased rather than refusing, since a promotion should never be
/// blocked purely on an unregistered prefix.
fn repo_for_block(block_id: &str) -> String {
    let prefix = block_id.split('.').next().unwrap_or(block_id);
    match prefix {
        "HQ" => "brain",
        "EN" => "engine-rs",
        "OK" => "okf-core",
        "MV" => "mev",
        "BA" => "bastion",
        "SY" => "synapse",
        "BE" => "bella",
        "BT" => "base-template",
        "BW" => "bastion-web",
        "BU" => "bastion-ui",
        "PS" => "price-scout",
        "LA" => "learn-ai",
        other => return other.to_ascii_lowercase(),
    }
    .to_string()
}

/// Read `{repo}`'s `status` for `block_id` out of its own `planning/state.json`, mirroring
/// `check_remediation.py`'s `load_state_blocks` candidate order: `<root>/core/<repo>/planning/
/// state.json`, then `<root>/<repo>/planning/state.json`, then (for `repo == "brain"`)
/// `<root>/planning/state.json`. Returns `None` when no candidate exists or parses, and never
/// assumes a status for a block it could not find.
fn read_block_status(hq_root: &Path, repo: &str, block_id: &str) -> Option<String> {
    let mut candidates = vec![
        hq_root.join("core").join(repo).join("planning/state.json"),
        hq_root.join(repo).join("planning/state.json"),
    ];
    if repo == "brain" {
        candidates.push(hq_root.join("planning/state.json"));
    }
    for path in candidates {
        if !path.is_file() {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(doc) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let tracks = doc.get("tracks").and_then(Value::as_array);
        if let Some(tracks) = tracks {
            for track in tracks {
                let Some(blocks) = track.get("blocks").and_then(Value::as_array) else {
                    continue;
                };
                for block in blocks {
                    if block.get("id").and_then(Value::as_str) == Some(block_id) {
                        return block
                            .get("status")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                    }
                }
            }
        }
        // The file existed and parsed but the block was not found in it — a real answer
        // ("unknown to this repo's state"), not a reason to fall through to another candidate.
        return None;
    }
    None
}

fn read_json_object(path: &Path) -> Result<Map<String, Value>, PromoteError> {
    let text = fs::read_to_string(path).map_err(|source| PromoteError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let value: Value = serde_json::from_str(&text).map_err(|source| PromoteError::Json {
        path: path.to_path_buf(),
        source,
    })?;
    match value {
        Value::Object(map) => Ok(map),
        _ => Err(PromoteError::Malformed {
            path: path.to_path_buf(),
            message: "top level is not a JSON object".to_string(),
        }),
    }
}

fn write_json_object(path: &Path, obj: &Map<String, Value>) -> Result<(), PromoteError> {
    let rendered = serde_json::to_string_pretty(&Value::Object(obj.clone())).map_err(|source| {
        PromoteError::Json {
            path: path.to_path_buf(),
            source,
        }
    })?;
    fs::write(path, format!("{rendered}\n")).map_err(|source| PromoteError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// The `ledger-entry:<id>` marker a promoted `remediation.json` row's `origin_note` always
/// embeds — the cross-reference [`promote_remediation`] checks to stay idempotent.
fn ledger_marker(ledger_id: &str) -> String {
    format!("ledger-entry:{ledger_id}")
}

fn already_promoted(remediations: &[Value], ledger_id: &str) -> Option<String> {
    let marker = ledger_marker(ledger_id);
    remediations.iter().find_map(|rem| {
        let note = rem.get("origin_note").and_then(Value::as_str)?;
        if note.contains(&marker) {
            rem.get("id").and_then(Value::as_str).map(str::to_string)
        } else {
            None
        }
    })
}

fn next_finding(remediations: &[Value], findings: &[Value]) -> u64 {
    let from_rems = remediations
        .iter()
        .filter_map(|r| r.get("finding").and_then(Value::as_u64))
        .max();
    let from_findings = findings
        .iter()
        .filter_map(|f| f.get("id").and_then(Value::as_u64))
        .max();
    from_rems
        .into_iter()
        .chain(from_findings)
        .max()
        .map_or(1, |m| m + 1)
}

fn next_rem_id(remediations: &[Value]) -> (String, u64) {
    let max = remediations
        .iter()
        .filter_map(|r| r.get("id").and_then(Value::as_str))
        .filter_map(|id| id.strip_prefix("REM-"))
        .filter_map(|n| n.parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    let next = max + 1;
    (format!("REM-{next:03}"), next)
}

/// Promote a failing (`status: failed`/`blocked`, `remediation: Some(_)`) [`LedgerEntry`] into
/// `<hq_root>/docs/sandbox/remediation.json` and `<hq_root>/docs/sandbox/findings.json`.
///
/// Idempotent on `entry.id` (see the module doc comment). On a fresh promotion:
/// 1. computes the next unused `finding` integer across both files, and the next unused
///    `REM-NNN` id across `remediation.json`'s own entries;
/// 2. appends a `findings.json` entry (`status: "open"`) and a `remediation.json` entry
///    (`status: "filed"` for an open fixing block, `"fixed"` only when the block is already
///    `closed` in ITS OWNING REPO's real `planning/state.json` — never assumed);
/// 3. regenerates `findings.md` / `remediation.md` via the corresponding `render_*.py` scripts;
/// 4. runs `check_remediation.py` and surfaces a non-zero exit as
///    [`PromoteError::ValidationFailed`] rather than leaving an inconsistent pair on disk.
///
/// Writes nothing to any repo's `planning/state.json` — see [`ProposedOriginStamp`].
pub fn promote_remediation(
    hq_root: &Path,
    entry: &LedgerEntry,
) -> Result<PromoteOutcome, PromoteError> {
    let Some(remediation) = &entry.remediation else {
        return Err(PromoteError::NotFailing {
            id: entry.id.clone(),
        });
    };

    let remediation_path = hq_root.join(REMEDIATION_REL);
    let findings_path = hq_root.join(FINDINGS_REL);

    let mut rem_doc = read_json_object(&remediation_path)?;
    let mut findings_doc = read_json_object(&findings_path)?;

    let rems = rem_doc
        .get("remediations")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| PromoteError::Malformed {
            path: remediation_path.clone(),
            message: "`remediations` is missing or not an array".to_string(),
        })?;
    let findings = findings_doc
        .get("findings")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| PromoteError::Malformed {
            path: findings_path.clone(),
            message: "`findings` is missing or not an array".to_string(),
        })?;

    if let Some(rem_id) = already_promoted(&rems, &entry.id) {
        return Ok(PromoteOutcome::AlreadyPromoted { rem_id });
    }

    let finding = next_finding(&rems, &findings);
    let (rem_id, _) = next_rem_id(&rems);

    let repo = repo_for_block(&remediation.block);
    let block_status = read_block_status(hq_root, &repo, &remediation.block);
    let status = if block_status.as_deref() == Some("closed") {
        "fixed"
    } else {
        "filed"
    };

    let coverage: Vec<Value> = if entry.covered_by.is_empty() {
        Vec::new()
    } else {
        let env = &entry.env;
        entry
            .covered_by
            .iter()
            .map(|test_id| json!({"test_id": test_id, "env": env}))
            .collect()
    };

    let command = if entry.how_to_verify.trim().is_empty() {
        format!("no how_to_verify recorded on ledger entry {}", entry.id)
    } else {
        entry.how_to_verify.clone()
    };
    let before = if entry.evidence.trim().is_empty() {
        format!("no evidence recorded on ledger entry {}", entry.id)
    } else {
        entry.evidence.clone()
    };

    let new_rem = json!({
        "id": rem_id.clone(),
        "finding": finding,
        "repo": repo.clone(),
        "origin_note": format!("{} — promoted from consolidation ledger entry {}", ledger_marker(&entry.id), entry.id),
        "status": status,
        "title": entry.capability.clone(),
        "coverage": coverage,
        "blocks": [remediation.block.clone()],
        "commits": Vec::<String>::new(),
        "issue": remediation.note.clone(),
        "change": "",
        "verify": {
            "command": command,
            "before": before,
            "expect": format!("ledger entry {} passes", entry.id),
        },
        "residue": Vec::<String>::new(),
    });

    let new_finding = json!({
        "id": finding,
        "title": entry.capability.clone(),
        "repo": repo.clone(),
        "observed_in": entry.env.clone(),
        "evidence": entry.evidence.clone(),
        "status": "open",
        "remediation": rem_id.clone(),
    });

    let mut rems = rems;
    rems.push(new_rem);
    rem_doc.insert("remediations".to_string(), Value::Array(rems));

    let mut findings = findings;
    findings.push(new_finding);
    findings_doc.insert("findings".to_string(), Value::Array(findings));

    write_json_object(&remediation_path, &rem_doc)?;
    write_json_object(&findings_path, &findings_doc)?;

    run_render_script(hq_root, "render_findings.py")?;
    run_render_script(hq_root, "render_remediation.py")?;

    let check_script = hq_root.join("scripts").join("check_remediation.py");
    let output = Command::new("python3")
        .arg(&check_script)
        .current_dir(hq_root)
        .output()
        .map_err(|source| PromoteError::Spawn {
            script: check_script.clone(),
            source,
        })?;
    if !output.status.success() {
        let mut msg = String::from_utf8_lossy(&output.stderr).into_owned();
        msg.push_str(&String::from_utf8_lossy(&output.stdout));
        return Err(PromoteError::ValidationFailed(msg));
    }

    Ok(PromoteOutcome::Promoted {
        rem_id: rem_id.clone(),
        finding,
        proposed_state_edit: ProposedOriginStamp {
            repo,
            block: remediation.block.clone(),
            origin_type: "remediation",
            slug: rem_id,
        },
    })
}

fn run_render_script(hq_root: &Path, name: &str) -> Result<(), PromoteError> {
    let script = hq_root.join("scripts").join(name);
    let output = Command::new("python3")
        .arg(&script)
        .current_dir(hq_root)
        .output()
        .map_err(|source| PromoteError::Spawn {
            script: script.clone(),
            source,
        })?;
    if !output.status.success() {
        let mut msg = String::from_utf8_lossy(&output.stderr).into_owned();
        msg.push_str(&String::from_utf8_lossy(&output.stdout));
        return Err(PromoteError::ValidationFailed(format!(
            "{} failed: {msg}",
            script.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::orchestration::ledger::{
        Coverage, LedgerEntry, LedgerStatus, NewLedgerEntry, Remediation,
    };

    /// Walk up from `start` looking for `brain.toml` — mirrors `coord/write.rs`'s
    /// `find_brain_root` test helper. `None` means this checkout has no sibling HQ tree to copy
    /// the real scripts from, and every test below skips loudly rather than fabricating one.
    fn find_brain_root(start: &Path) -> Option<PathBuf> {
        let mut dir = Some(start);
        while let Some(d) = dir {
            if d.join("brain.toml").is_file() {
                return Some(d.to_path_buf());
            }
            dir = d.parent();
        }
        None
    }

    fn python3_available() -> bool {
        match Command::new("python3").arg("--version").output() {
            Ok(output) => output.status.success(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => panic!("python3 --version failed unexpectedly: {e}"),
        }
    }

    /// Build a hermetic `tempdir` carrying the REAL `check_remediation.py` / `render_findings.py`
    /// / `render_remediation.py` (copied byte-for-byte from the real HQ tree, never
    /// reimplemented) alongside a minimal `docs/sandbox/{remediation,findings,test-catalogue}.json`
    /// fixture and a `core/engine-rs/planning/state.json` fixture — preserving the real
    /// `scripts/` + `docs/sandbox/` sibling layout so each script's `Path(__file__).resolve()
    /// .parent.parent` lands inside the copy, never the real HQ tree.
    fn hermetic_hq(brain_root: &Path, block_status: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();

        fs::create_dir_all(root.join("scripts")).unwrap();
        fs::create_dir_all(root.join("docs/sandbox/results")).unwrap();
        fs::create_dir_all(root.join("core/engine-rs/planning")).unwrap();

        for name in [
            "check_remediation.py",
            "render_findings.py",
            "render_remediation.py",
        ] {
            fs::copy(
                brain_root.join("scripts").join(name),
                root.join("scripts").join(name),
            )
            .unwrap_or_else(|e| panic!("copying {name}: {e}"));
        }

        fs::write(
            root.join("docs/sandbox/remediation.json"),
            serde_json::to_string_pretty(&json!({
                "schema_version": 1,
                "updated": "2026-09-10",
                "status_values": {
                    "fixed": "Shipped and verified.",
                    "partial": "Some blocks closed, named residue still open.",
                    "filed": "Block(s) exist and are open; no code has shipped yet.",
                    "wontfix": "Deliberately not fixing."
                },
                "remediations": []
            }))
            .unwrap(),
        )
        .unwrap();

        fs::write(
            root.join("docs/sandbox/findings.json"),
            serde_json::to_string_pretty(&json!({
                "schema_version": 1,
                "updated": "2026-09-10",
                "status_values": {
                    "open": "Not yet ticketed.",
                    "ticketed": "A block exists but has not shipped.",
                    "closed": "Ticketed and fixed.",
                    "fixed": "Fixed and verified.",
                    "partial": "Some blocks closed, named residue still open."
                },
                "findings": []
            }))
            .unwrap(),
        )
        .unwrap();

        fs::write(
            root.join("docs/sandbox/test-catalogue.json"),
            serde_json::to_string_pretty(&json!({
                "schema_version": 1,
                "tests": []
            }))
            .unwrap(),
        )
        .unwrap();

        fs::write(
            root.join("core/engine-rs/planning/state.json"),
            serde_json::to_string_pretty(&json!({
                "tracks": [
                    {
                        "blocks": [
                            {"id": "EN.99.Z", "status": block_status}
                        ]
                    }
                ]
            }))
            .unwrap(),
        )
        .unwrap();

        dir
    }

    fn failing_ledger_entry(id: &str, block: &str) -> LedgerEntry {
        LedgerEntry::new(
            block,
            NewLedgerEntry {
                id: id.to_string(),
                capability: "widget renders under load".to_string(),
                status: LedgerStatus::Failed,
                env: "v1".to_string(),
                how_to_verify: "cargo nextest run -p engine-core widget::renders_under_load"
                    .to_string(),
                call_site: "crates/engine-core/src/widget.rs:42".to_string(),
                evidence: "panicked at widget.rs:42: index out of bounds".to_string(),
                coverage: Coverage::Uncovered,
                covered_by: Vec::new(),
                cross_repo: None,
                remediation: Some(Remediation {
                    block: block.to_string(),
                    opened_at: "2026-09-10T00:00:00Z".to_string(),
                    note: "filed EN.99.Z to fix the OOB panic".to_string(),
                }),
            },
        )
        .expect("valid ledger entry")
    }

    #[test]
    fn a_non_failing_entry_has_nothing_to_promote() {
        let entry = LedgerEntry::new(
            "EN.99.Z",
            NewLedgerEntry {
                id: "engine-rs-1".to_string(),
                capability: "cap".to_string(),
                status: LedgerStatus::Untested,
                env: "v1".to_string(),
                call_site: "src/lib.rs:1".to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        let err = promote_remediation(Path::new("/nonexistent"), &entry).unwrap_err();
        assert!(matches!(err, PromoteError::NotFailing { .. }));
    }

    #[test]
    fn promotion_writes_both_files_and_check_remediation_exits_0() {
        let Some(brain_root) = find_brain_root(Path::new(env!("CARGO_MANIFEST_DIR"))) else {
            eprintln!("SKIPPING: no sibling brain.toml found to copy the real scripts from");
            return;
        };
        if !python3_available() {
            eprintln!("SKIPPING: python3 not on PATH");
            return;
        }

        let hq = hermetic_hq(&brain_root, "open");
        let entry = failing_ledger_entry("engine-rs-widget-oob", "EN.99.Z");

        let outcome = promote_remediation(hq.path(), &entry).expect("promotion succeeds");
        let (rem_id, finding) = match outcome {
            PromoteOutcome::Promoted {
                rem_id,
                finding,
                proposed_state_edit,
            } => {
                assert_eq!(proposed_state_edit.repo, "engine-rs");
                assert_eq!(proposed_state_edit.block, "EN.99.Z");
                assert_eq!(proposed_state_edit.origin_type, "remediation");
                assert_eq!(proposed_state_edit.slug, rem_id);
                (rem_id, finding)
            }
            other => panic!("expected Promoted, got {other:?}"),
        };
        assert_eq!(rem_id, "REM-001");
        assert_eq!(finding, 1);

        let rem_doc: Value = serde_json::from_str(
            &fs::read_to_string(hq.path().join("docs/sandbox/remediation.json")).unwrap(),
        )
        .unwrap();
        let rems = rem_doc["remediations"].as_array().unwrap();
        assert_eq!(rems.len(), 1);
        assert_eq!(rems[0]["status"], "filed");
        assert_eq!(rems[0]["finding"], 1);

        let findings_doc: Value = serde_json::from_str(
            &fs::read_to_string(hq.path().join("docs/sandbox/findings.json")).unwrap(),
        )
        .unwrap();
        let findings = findings_doc["findings"].as_array().unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0]["remediation"], "REM-001");

        assert!(
            hq.path().join("docs/sandbox/remediation.md").is_file(),
            "render_remediation.py should have regenerated remediation.md"
        );
        assert!(
            hq.path().join("docs/sandbox/findings.md").is_file(),
            "render_findings.py should have regenerated findings.md"
        );
    }

    #[test]
    fn promotion_of_an_already_closed_block_is_status_fixed() {
        let Some(brain_root) = find_brain_root(Path::new(env!("CARGO_MANIFEST_DIR"))) else {
            eprintln!("SKIPPING: no sibling brain.toml found to copy the real scripts from");
            return;
        };
        if !python3_available() {
            eprintln!("SKIPPING: python3 not on PATH");
            return;
        }

        let hq = hermetic_hq(&brain_root, "closed");
        let entry = failing_ledger_entry("engine-rs-widget-oob-2", "EN.99.Z");

        let outcome = promote_remediation(hq.path(), &entry).expect("promotion succeeds");
        assert!(matches!(outcome, PromoteOutcome::Promoted { .. }));

        let rem_doc: Value = serde_json::from_str(
            &fs::read_to_string(hq.path().join("docs/sandbox/remediation.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(rem_doc["remediations"][0]["status"], "fixed");
    }

    #[test]
    fn second_promotion_of_the_same_ledger_entry_is_a_no_op() {
        let Some(brain_root) = find_brain_root(Path::new(env!("CARGO_MANIFEST_DIR"))) else {
            eprintln!("SKIPPING: no sibling brain.toml found to copy the real scripts from");
            return;
        };
        if !python3_available() {
            eprintln!("SKIPPING: python3 not on PATH");
            return;
        }

        let hq = hermetic_hq(&brain_root, "open");
        let entry = failing_ledger_entry("engine-rs-widget-oob-3", "EN.99.Z");

        let first = promote_remediation(hq.path(), &entry).expect("first promotion succeeds");
        let rem_id = match first {
            PromoteOutcome::Promoted { rem_id, .. } => rem_id,
            other => panic!("expected Promoted, got {other:?}"),
        };

        let rem_mtime_before = fs::metadata(hq.path().join("docs/sandbox/remediation.json"))
            .unwrap()
            .modified()
            .unwrap();

        let second = promote_remediation(hq.path(), &entry).expect("second call does not error");
        assert_eq!(second, PromoteOutcome::AlreadyPromoted { rem_id });

        let rem_mtime_after = fs::metadata(hq.path().join("docs/sandbox/remediation.json"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(
            rem_mtime_before, rem_mtime_after,
            "an already-promoted entry must not rewrite remediation.json"
        );

        let rem_doc: Value = serde_json::from_str(
            &fs::read_to_string(hq.path().join("docs/sandbox/remediation.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            rem_doc["remediations"].as_array().unwrap().len(),
            1,
            "a second promotion must not append a duplicate entry"
        );
    }

    #[test]
    fn repo_for_block_maps_known_fleet_prefixes() {
        assert_eq!(repo_for_block("EN.15.K"), "engine-rs");
        assert_eq!(repo_for_block("OK.6.A"), "okf-core");
        assert_eq!(repo_for_block("BT.ticket.something"), "base-template");
        assert_eq!(repo_for_block("HQ.1.A"), "brain");
    }
}
