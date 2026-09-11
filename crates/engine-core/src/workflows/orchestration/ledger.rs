//! The D57 verification ledger — a per-`(repo x roadmap)` record of what a
//! chain shipped and how to verify it cold (`EN.15.L` Task 1).
//!
//! Schema authority: HQ `docs/sandbox/run-verification-ledger-prompt.md`
//! (as of `278fba407`, 2026-09-07). This module owns pure composition and
//! I/O helpers only — [`LedgerEntry`], its six-value [`LedgerStatus`], the
//! type-level `remediation`-only-on-`failed`/`blocked` rule, create-if-absent
//! for `verification-ledger.json`/`.md`, and a read-modify-write MERGE/append
//! writer. **Nothing here is wired to a caller yet** — that is `EN.15.L`
//! Task 2's job, threading a composer seam through `integrate.rs` and
//! `engine-serve`'s `journal.rs`.
//!
//! Two rules a writer against this file gets wrong, both enforced here:
//!
//! - **Append, never batch, and never overwrite an existing `id`.**
//!   [`merge_append_entries`] reads the file if present, skips any candidate
//!   whose `id` already exists, appends the rest, and writes the whole file
//!   back — the clobber class base-template
//!   `BT.ticket.verification-ledger-is-not-append-only` exists to catch.
//! - **The chain that built a capability writes `untested`, never
//!   `tested`/`failed`/etc.** [`LedgerEntry::compose`] is the stamping entry
//!   point a block-close writer calls: it ignores whatever status a
//!   candidate proposed and always writes `untested`, alongside a
//!   `<repo>-`-prefixed `id` and the closing block's own `block` id — see
//!   its own docs for why this means a freshly-composed entry can never
//!   carry a `remediation` (that requires `failed`/`blocked`, which this
//!   stamping path never produces). [`LedgerEntry::new`] is the general,
//!   non-stamping constructor used to build (or read back) an entry that
//!   already carries one of the other five statuses — e.g. one a later
//!   verifier pass marked `failed`/`blocked`, which is the only case a
//!   [`Remediation`] can attach to.

use std::path::Path;

use serde_json::{json, Map, Value};

/// The six values `docs/sandbox/run-verification-ledger-prompt.md` fixes as
/// `status_values` — the exact header every `verification-ledger.json`
/// carries, and the only statuses an entry's `status` field may hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LedgerStatus {
    #[default]
    Untested,
    Tested,
    Partial,
    Failed,
    Blocked,
    NotApplicable,
}

impl LedgerStatus {
    /// The exact wire value this status serializes to.
    #[must_use]
    pub fn as_wire_str(self) -> &'static str {
        match self {
            LedgerStatus::Untested => "untested",
            LedgerStatus::Tested => "tested",
            LedgerStatus::Partial => "partial",
            LedgerStatus::Failed => "failed",
            LedgerStatus::Blocked => "blocked",
            LedgerStatus::NotApplicable => "not_applicable",
        }
    }

    /// Whether a [`Remediation`] may attach to an entry holding this status
    /// — only `failed` ("ran and did not pass") or `blocked` ("could not
    /// be run at all").
    #[must_use]
    pub fn accepts_remediation(self) -> bool {
        matches!(self, LedgerStatus::Failed | LedgerStatus::Blocked)
    }

    /// The header array every ledger file's `status_values` field carries,
    /// in the schema's own fixed order.
    #[must_use]
    pub fn all_wire_values() -> [&'static str; 6] {
        [
            LedgerStatus::Untested.as_wire_str(),
            LedgerStatus::Tested.as_wire_str(),
            LedgerStatus::Partial.as_wire_str(),
            LedgerStatus::Failed.as_wire_str(),
            LedgerStatus::Blocked.as_wire_str(),
            LedgerStatus::NotApplicable.as_wire_str(),
        ]
    }
}

/// How well an automated test backs this capability — mirrors the schema's
/// `coverage` field. `Covered` MUST carry at least one [`LedgerEntry::covered_by`]
/// identifier; that pairing is enforced at construction, not merely documented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Coverage {
    Covered,
    Partial,
    #[default]
    Uncovered,
}

impl Coverage {
    #[must_use]
    pub fn as_wire_str(self) -> &'static str {
        match self {
            Coverage::Covered => "covered",
            Coverage::Partial => "partial",
            Coverage::Uncovered => "uncovered",
        }
    }
}

/// Whether an end-to-end test exists, is needed, or does not apply — mirrors
/// `cross_repo.e2e`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CrossRepoE2e {
    Exists,
    Needed,
    #[default]
    NotApplicable,
}

impl CrossRepoE2e {
    #[must_use]
    pub fn as_wire_str(self) -> &'static str {
        match self {
            CrossRepoE2e::Exists => "exists",
            CrossRepoE2e::Needed => "needed",
            CrossRepoE2e::NotApplicable => "not-applicable",
        }
    }
}

/// Mirrors the schema's `cross_repo` object — present only when a
/// capability spans repos.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrossRepo {
    pub dependent: bool,
    pub repos: Vec<String>,
    pub e2e: CrossRepoE2e,
    pub note: String,
}

impl CrossRepo {
    fn to_json(&self) -> Value {
        json!({
            "dependent": self.dependent,
            "repos": self.repos,
            "e2e": self.e2e.as_wire_str(),
            "note": self.note,
        })
    }
}

/// The run-local remediation a **failing** entry may carry — `{block,
/// opened_at, note}`, and *only* those three fields. There is deliberately
/// no `finding` field on this type at all: minting the global HQ
/// `findings.md` integer is `/consolidate-run`'s single-writer job
/// (`docs/sandbox/run-verification-ledger-prompt.md`), and a caller holding
/// a [`Remediation`] value has no field to smuggle one into — the
/// unrepresentability is a compile-time property of this struct's shape,
/// not a runtime check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remediation {
    /// The block id that fixes the failing capability (the same `block`
    /// this remediation's entry already carries, or a distinct ticket).
    pub block: String,
    /// ISO-8601, when the ticket was filed.
    pub opened_at: String,
    /// What broke and which ticket now fixes it. Ticket status is recorded
    /// here as `filed`, never `fixed` — promotion to `fixed` is
    /// `/consolidate-run`'s job, out of scope for this writer.
    pub note: String,
}

impl Remediation {
    fn to_json(&self) -> Value {
        json!({
            "block": self.block,
            "opened_at": self.opened_at,
            "note": self.note,
        })
    }
}

/// Why a [`LedgerEntry`] failed to compose. Every variant means the entry
/// was refused before it could be written — there is no invalid
/// [`LedgerEntry`] value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerEntryError {
    /// A required string field was empty.
    EmptyField(&'static str),
    /// `coverage: covered` with an empty `covered_by` — the one shape the
    /// schema calls "always wrong".
    CoveredWithNoTests,
    /// `call_site` was missing entirely. The literal string `"NONE"` is a
    /// valid, present value and does NOT hit this — only an empty/absent
    /// `call_site` does.
    MissingCallSite,
    /// A `remediation` was attached to an entry whose `status` is not
    /// `failed` or `blocked`.
    RemediationOnNonFailingEntry { status: LedgerStatus },
}

impl std::fmt::Display for LedgerEntryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LedgerEntryError::EmptyField(field) => {
                write!(f, "ledger entry field `{field}` must not be empty")
            }
            LedgerEntryError::CoveredWithNoTests => write!(
                f,
                "ledger entry declares coverage: covered but covered_by is empty"
            ),
            LedgerEntryError::MissingCallSite => write!(
                f,
                "ledger entry is missing call_site (use the literal \"NONE\" if nothing calls it)"
            ),
            LedgerEntryError::RemediationOnNonFailingEntry { status } => write!(
                f,
                "ledger entry carries a remediation but status is {} — remediation is only valid on failed/blocked",
                status.as_wire_str()
            ),
        }
    }
}

impl std::error::Error for LedgerEntryError {}

/// The caller-supplied shape a [`LedgerEntry`] is built from — whatever a
/// composer proposed, before [`LedgerEntry::compose`] or [`LedgerEntry::new`]
/// stamp/validate it. Not itself schema-valid; every field here is a raw
/// candidate value.
#[derive(Debug, Clone, Default)]
pub struct NewLedgerEntry {
    /// The composer's proposed id — a bare slug or an already-prefixed one.
    /// [`LedgerEntry::compose`] normalizes this; [`LedgerEntry::new`] takes
    /// it verbatim.
    pub id: String,
    pub capability: String,
    pub status: LedgerStatus,
    pub env: String,
    pub how_to_verify: String,
    pub call_site: String,
    pub evidence: String,
    pub coverage: Coverage,
    pub covered_by: Vec<String>,
    pub cross_repo: Option<CrossRepo>,
    pub remediation: Option<Remediation>,
}

/// One composed, always schema-valid ledger entry. There is no
/// [`LedgerEntry`] value that fails the rules below — both constructors
/// enforce them before a value can exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerEntry {
    pub id: String,
    pub block: String,
    pub capability: String,
    pub status: LedgerStatus,
    pub env: String,
    pub how_to_verify: String,
    pub call_site: String,
    pub evidence: String,
    pub coverage: Coverage,
    pub covered_by: Vec<String>,
    pub cross_repo: Option<CrossRepo>,
    pub remediation: Option<Remediation>,
}

impl LedgerEntry {
    /// Validate the rules every [`LedgerEntry`] must satisfy regardless of
    /// how it was built: non-empty `env`/`call_site`/`block`/`id`, the
    /// `coverage: covered` <-> non-empty `covered_by` pairing, and the
    /// `remediation` <-> `failed`/`blocked` status pairing.
    fn validate(id: &str, block: &str, args: &NewLedgerEntry) -> Result<(), LedgerEntryError> {
        if id.is_empty() {
            return Err(LedgerEntryError::EmptyField("id"));
        }
        if block.is_empty() {
            return Err(LedgerEntryError::EmptyField("block"));
        }
        if args.env.is_empty() {
            return Err(LedgerEntryError::EmptyField("env"));
        }
        if args.call_site.is_empty() {
            return Err(LedgerEntryError::MissingCallSite);
        }
        if args.coverage == Coverage::Covered && args.covered_by.is_empty() {
            return Err(LedgerEntryError::CoveredWithNoTests);
        }
        if args.remediation.is_some() && !args.status.accepts_remediation() {
            return Err(LedgerEntryError::RemediationOnNonFailingEntry {
                status: args.status,
            });
        }
        Ok(())
    }

    /// General, non-stamping constructor: builds an entry exactly from
    /// `args` plus an explicit `block`, taking `args.id` and `args.status`
    /// verbatim. This is the path for an entry that legitimately holds a
    /// status other than `untested` — e.g. reading one back off disk, or a
    /// later verifier pass recording `failed`/`blocked` plus a
    /// [`Remediation`]. Refuses on any of [`LedgerEntryError`]'s rules.
    pub fn new(block: impl Into<String>, args: NewLedgerEntry) -> Result<Self, LedgerEntryError> {
        let block = block.into();
        Self::validate(&args.id, &block, &args)?;
        Ok(Self {
            id: args.id,
            block,
            capability: args.capability,
            status: args.status,
            env: args.env,
            how_to_verify: args.how_to_verify,
            call_site: args.call_site,
            evidence: args.evidence,
            coverage: args.coverage,
            covered_by: args.covered_by,
            cross_repo: args.cross_repo,
            remediation: args.remediation,
        })
    }

    /// The stamping entry point a block-close writer calls: **ignores**
    /// whatever `args.status` and `args.id` a composer proposed and always
    /// writes `status: untested` plus an `id` prefixed `<repo>-` (stripping
    /// any pre-existing `<repo>-` prefix first, so a composer that already
    /// guessed the prefix is not double-stamped) and `block` set to the
    /// closing block's own id — never whatever `args` carried, if anything.
    ///
    /// Because the written status is always `untested`, a candidate's
    /// `remediation` (which requires `failed`/`blocked`) is never valid
    /// through this path — matching "the chain that built a capability
    /// writes untested and never marks its own work verified", remediation
    /// only ever attaches to an entry a later pass already marked failing.
    pub fn compose(
        repo: &str,
        block: impl Into<String>,
        mut args: NewLedgerEntry,
    ) -> Result<Self, LedgerEntryError> {
        let block = block.into();
        let prefix = format!("{repo}-");
        let bare_id = args.id.strip_prefix(&prefix).unwrap_or(&args.id);
        args.id = format!("{prefix}{bare_id}");
        args.status = LedgerStatus::Untested;
        Self::new(block, args)
    }

    /// Render this entry to the exact JSON object shape
    /// `docs/sandbox/run-verification-ledger-prompt.md` and
    /// `scripts/check_verification_ledger.py` expect. `remediation` and
    /// `cross_repo` are omitted entirely (not written as `null`) when absent.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut obj = Map::new();
        obj.insert("id".to_string(), json!(self.id));
        obj.insert("block".to_string(), json!(self.block));
        obj.insert("capability".to_string(), json!(self.capability));
        obj.insert("status".to_string(), json!(self.status.as_wire_str()));
        obj.insert("env".to_string(), json!(self.env));
        obj.insert("how_to_verify".to_string(), json!(self.how_to_verify));
        obj.insert("call_site".to_string(), json!(self.call_site));
        obj.insert("evidence".to_string(), json!(self.evidence));
        obj.insert("coverage".to_string(), json!(self.coverage.as_wire_str()));
        obj.insert("covered_by".to_string(), json!(self.covered_by));
        if let Some(cross_repo) = &self.cross_repo {
            obj.insert("cross_repo".to_string(), cross_repo.to_json());
        }
        if let Some(remediation) = &self.remediation {
            obj.insert("remediation".to_string(), remediation.to_json());
        }
        Value::Object(obj)
    }
}

/// Error building or writing a `verification-ledger.json`/`.md` pair.
#[derive(Debug)]
pub enum LedgerFileError {
    Io(std::io::Error),
    Json(serde_json::Error),
    /// The file on disk was not the object shape this module writes (e.g.
    /// missing `entries`, or `entries` was not an array).
    MalformedLedger(String),
}

impl std::fmt::Display for LedgerFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LedgerFileError::Io(err) => write!(f, "ledger file I/O error: {err}"),
            LedgerFileError::Json(err) => write!(f, "ledger file JSON error: {err}"),
            LedgerFileError::MalformedLedger(msg) => write!(f, "malformed ledger file: {msg}"),
        }
    }
}

impl std::error::Error for LedgerFileError {}

impl From<std::io::Error> for LedgerFileError {
    fn from(err: std::io::Error) -> Self {
        LedgerFileError::Io(err)
    }
}

impl From<serde_json::Error> for LedgerFileError {
    fn from(err: serde_json::Error) -> Self {
        LedgerFileError::Json(err)
    }
}

/// Create `verification-ledger.json` (and its sibling `verification-ledger.md`
/// OKF wrapper) at `dir` if, and only if, neither already exists — this is
/// **create-if-absent**, never an overwrite. Returns `Ok(false)` without
/// touching either file when `verification-ledger.json` is already present
/// (matching "an appended-to ledger addressed per (repo x roadmap) — a
/// later wave appends to it and never overwrites it").
pub fn create_ledger_if_absent(
    dir: &Path,
    roadmap: &str,
    repo: &str,
    lane: &str,
    created: &str,
) -> Result<bool, LedgerFileError> {
    let json_path = dir.join("verification-ledger.json");
    if json_path.exists() {
        return Ok(false);
    }
    std::fs::create_dir_all(dir)?;
    let header = json!({
        "roadmap": roadmap,
        "repo": repo,
        "lane": lane,
        "created": created,
        "status_values": LedgerStatus::all_wire_values(),
        "entries": [],
    });
    let rendered = serde_json::to_string_pretty(&header)?;
    std::fs::write(&json_path, format!("{rendered}\n"))?;

    let md_path = dir.join("verification-ledger.md");
    if !md_path.exists() {
        std::fs::write(&md_path, render_ledger_md(roadmap, repo, lane))?;
    }
    Ok(true)
}

/// The thin OKF-frontmattered `verification-ledger.md` wrapper — mirrors the
/// shape every existing run-record ledger in this fleet already carries
/// (see e.g. `core/_planning/bella/orchestration-run/operator-console/verification-ledger.md`).
fn render_ledger_md(roadmap: &str, repo: &str, lane: &str) -> String {
    format!(
        "---\n\
type: Reference\n\
title: \"{repo} — {roadmap} run verification ledger\"\n\
description: \"Thin wrapper over verification-ledger.json — the {repo} {lane} lane's per-capability record of what shipped and how to verify it cold.\"\n\
doc_id: {repo}-verification-ledger-{roadmap}\n\
layer: [engine, factory]\n\
project: {repo}\n\
status: active\n\
keywords: [verification, ledger, orchestration, {lane}, {roadmap}]\n\
related: [{repo}-orchestration-run-{roadmap}]\n\
roadmap: {roadmap}\n\
lane: {lane}\n\
---\n\
\n\
# Verification ledger — {repo}, `{lane}` lane, {roadmap}\n\
\n\
The machine-readable record lives in [`verification-ledger.json`](verification-ledger.json).\n\
\n\
**One entry per capability, not per block.** Entries are appended **as each block closes**,\n\
never batched at the end, so a bailed run still leaves a usable ledger.\n\
\n\
**Every entry defaults to `status: \"untested\"`.** The chain that built a capability does not\n\
get to mark its own work verified; moving an entry to `tested` is the post-run verifier's job.\n\
\n\
`call_site` is mandatory and names the **production** caller, or the literal `NONE` — and `NONE`\n\
is a finding.\n\
\n\
Full field contract: `docs/sandbox/run-verification-ledger-prompt.md` in HQ.\n\
\n\
This ledger is appended to, never overwritten, by later waves against this same\n\
(repo x roadmap) directory.\n",
    )
}

/// Read-modify-write MERGE: append `new_entries` into the `entries` array of
/// `path`'s ledger, refusing to overwrite any entry whose `id` already
/// exists (the existing one wins; the candidate is skipped) — a true merge,
/// never a truncate-and-replace. Returns the number of entries actually
/// appended. `path` must already exist (create it first with
/// [`create_ledger_if_absent`]).
pub fn merge_append_entries(
    path: &Path,
    new_entries: &[LedgerEntry],
) -> Result<usize, LedgerFileError> {
    let raw = std::fs::read_to_string(path)?;
    let mut doc: Value = serde_json::from_str(&raw)?;

    let entries = doc
        .get_mut("entries")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| {
            LedgerFileError::MalformedLedger(format!("{} has no `entries` array", path.display()))
        })?;

    let existing_ids: std::collections::HashSet<String> = entries
        .iter()
        .filter_map(|entry| entry.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .collect();

    let mut appended = 0usize;
    for entry in new_entries {
        if existing_ids.contains(&entry.id) {
            continue;
        }
        entries.push(entry.to_json());
        appended += 1;
    }

    let rendered = serde_json::to_string_pretty(&doc)?;
    std::fs::write(path, format!("{rendered}\n"))?;
    Ok(appended)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_candidate() -> NewLedgerEntry {
        NewLedgerEntry {
            id: "widget-flag".to_string(),
            capability: "does the thing".to_string(),
            status: LedgerStatus::Untested,
            env: "fleet-main".to_string(),
            how_to_verify: "cargo test".to_string(),
            call_site: "src/main.rs:10".to_string(),
            evidence: "exit 0".to_string(),
            coverage: Coverage::Uncovered,
            covered_by: vec![],
            cross_repo: None,
            remediation: None,
        }
    }

    // --- Acceptance criterion 1: compose() stamps id/block/status ---

    #[test]
    fn compose_stamps_repo_prefixed_id_block_and_untested_status_regardless_of_candidate() {
        let mut candidate = valid_candidate();
        candidate.id = "bare-slug".to_string();
        candidate.status = LedgerStatus::Tested; // composer proposes something else
        let entry = LedgerEntry::compose("engine-rs", "EN.15.L", candidate).unwrap();
        assert_eq!(entry.id, "engine-rs-bare-slug");
        assert_eq!(entry.block, "EN.15.L");
        assert_eq!(entry.status, LedgerStatus::Untested);
    }

    #[test]
    fn compose_does_not_double_prefix_an_already_prefixed_id() {
        let mut candidate = valid_candidate();
        candidate.id = "engine-rs-already-prefixed".to_string();
        let entry = LedgerEntry::compose("engine-rs", "EN.15.L", candidate).unwrap();
        assert_eq!(entry.id, "engine-rs-already-prefixed");
    }

    #[test]
    fn compose_requires_non_empty_env() {
        let mut candidate = valid_candidate();
        candidate.env = String::new();
        let err = LedgerEntry::compose("engine-rs", "EN.15.L", candidate).unwrap_err();
        assert_eq!(err, LedgerEntryError::EmptyField("env"));
    }

    // --- Acceptance criterion 2: coverage/call_site validation refusals ---

    #[test]
    fn covered_with_empty_covered_by_is_refused() {
        let mut candidate = valid_candidate();
        candidate.coverage = Coverage::Covered;
        candidate.covered_by = vec![];
        let err = LedgerEntry::new("EN.15.L", candidate).unwrap_err();
        assert_eq!(err, LedgerEntryError::CoveredWithNoTests);
    }

    #[test]
    fn covered_with_non_empty_covered_by_is_accepted() {
        let mut candidate = valid_candidate();
        candidate.coverage = Coverage::Covered;
        candidate.covered_by = vec!["crate::mod::test_fn".to_string()];
        let entry = LedgerEntry::new("EN.15.L", candidate).unwrap();
        assert_eq!(entry.coverage, Coverage::Covered);
    }

    #[test]
    fn missing_call_site_is_refused() {
        let mut candidate = valid_candidate();
        candidate.call_site = String::new();
        let err = LedgerEntry::new("EN.15.L", candidate).unwrap_err();
        assert_eq!(err, LedgerEntryError::MissingCallSite);
    }

    #[test]
    fn call_site_literal_none_is_accepted() {
        let mut candidate = valid_candidate();
        candidate.call_site = "NONE".to_string();
        let entry = LedgerEntry::new("EN.15.L", candidate).unwrap();
        assert_eq!(entry.call_site, "NONE");
    }

    // --- Acceptance criterion 3: remediation only on failed/blocked ---

    #[test]
    fn remediation_on_untested_entry_is_refused() {
        let mut candidate = valid_candidate();
        candidate.status = LedgerStatus::Untested;
        candidate.remediation = Some(Remediation {
            block: "EN.15.L".to_string(),
            opened_at: "2026-09-10T00:00:00Z".to_string(),
            note: "filed BT.ticket.x".to_string(),
        });
        let err = LedgerEntry::new("EN.15.L", candidate).unwrap_err();
        assert_eq!(
            err,
            LedgerEntryError::RemediationOnNonFailingEntry {
                status: LedgerStatus::Untested
            }
        );
    }

    #[test]
    fn remediation_on_failed_entry_is_accepted() {
        let mut candidate = valid_candidate();
        candidate.status = LedgerStatus::Failed;
        candidate.remediation = Some(Remediation {
            block: "EN.15.L".to_string(),
            opened_at: "2026-09-10T00:00:00Z".to_string(),
            note: "filed BT.ticket.x, status: filed".to_string(),
        });
        let entry = LedgerEntry::new("EN.15.L", candidate).unwrap();
        assert!(entry.remediation.is_some());
    }

    #[test]
    fn remediation_on_blocked_entry_is_accepted() {
        let mut candidate = valid_candidate();
        candidate.status = LedgerStatus::Blocked;
        candidate.remediation = Some(Remediation {
            block: "EN.15.L".to_string(),
            opened_at: "2026-09-10T00:00:00Z".to_string(),
            note: "no environment to run this yet".to_string(),
        });
        let entry = LedgerEntry::new("EN.15.L", candidate).unwrap();
        assert!(entry.remediation.is_some());
    }

    #[test]
    fn remediation_json_never_carries_a_finding_field() {
        // Compile-time property: Remediation has no `finding` field at all,
        // so its rendered JSON object can never contain one either.
        let remediation = Remediation {
            block: "EN.15.L".to_string(),
            opened_at: "2026-09-10T00:00:00Z".to_string(),
            note: "note".to_string(),
        };
        let json = remediation.to_json();
        assert!(json.get("finding").is_none());
        assert_eq!(
            json.as_object().unwrap().keys().collect::<Vec<_>>().len(),
            3
        );
    }

    // --- Acceptance criterion 4: create-if-absent + merge/append ---

    #[test]
    fn create_ledger_if_absent_writes_header_and_md_wrapper_once() {
        let dir = tempfile::tempdir().unwrap();
        let created = create_ledger_if_absent(
            dir.path(),
            "coordination-layer-port",
            "engine-rs",
            "unattended",
            "2026-09-10T00:00:00Z",
        )
        .unwrap();
        assert!(created);
        let json_path = dir.path().join("verification-ledger.json");
        let md_path = dir.path().join("verification-ledger.md");
        assert!(json_path.exists());
        assert!(md_path.exists());

        let doc: Value =
            serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).unwrap();
        assert_eq!(doc["roadmap"], json!("coordination-layer-port"));
        assert_eq!(doc["repo"], json!("engine-rs"));
        assert_eq!(doc["lane"], json!("unattended"));
        assert_eq!(doc["created"], json!("2026-09-10T00:00:00Z"));
        assert_eq!(
            doc["status_values"],
            json!([
                "untested",
                "tested",
                "partial",
                "failed",
                "blocked",
                "not_applicable"
            ])
        );
        assert_eq!(doc["entries"], json!([]));

        let md = std::fs::read_to_string(&md_path).unwrap();
        assert!(md.starts_with("---\n"));
        assert!(md.contains("doc_id: engine-rs-verification-ledger-coordination-layer-port"));

        // Second call: create-if-absent must not touch either file.
        std::fs::write(&md_path, "MUTATED").unwrap();
        let created_again = create_ledger_if_absent(
            dir.path(),
            "coordination-layer-port",
            "engine-rs",
            "unattended",
            "2026-09-10T00:00:00Z",
        )
        .unwrap();
        assert!(!created_again);
        assert_eq!(std::fs::read_to_string(&md_path).unwrap(), "MUTATED");
    }

    #[test]
    fn merge_append_leaves_prior_entries_and_skips_duplicate_ids() {
        let dir = tempfile::tempdir().unwrap();
        create_ledger_if_absent(
            dir.path(),
            "coordination-layer-port",
            "engine-rs",
            "unattended",
            "2026-09-10T00:00:00Z",
        )
        .unwrap();
        let json_path = dir.path().join("verification-ledger.json");

        let mut first_wave_candidate = valid_candidate();
        first_wave_candidate.id = "first".to_string();
        let first_entry =
            LedgerEntry::compose("engine-rs", "EN.15.K", first_wave_candidate).unwrap();
        let appended =
            merge_append_entries(&json_path, std::slice::from_ref(&first_entry)).unwrap();
        assert_eq!(appended, 1);

        // Second wave: one brand-new id, one that collides with the first wave's.
        let mut new_candidate = valid_candidate();
        new_candidate.id = "second".to_string();
        let new_entry = LedgerEntry::compose("engine-rs", "EN.15.L", new_candidate).unwrap();

        let mut colliding_candidate = valid_candidate();
        colliding_candidate.id = "first".to_string();
        colliding_candidate.capability = "a DIFFERENT capability text".to_string();
        let colliding_entry =
            LedgerEntry::compose("engine-rs", "EN.15.L", colliding_candidate).unwrap();

        let appended = merge_append_entries(&json_path, &[new_entry, colliding_entry]).unwrap();
        assert_eq!(
            appended, 1,
            "the colliding id must be skipped, not overwritten"
        );

        let doc: Value =
            serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).unwrap();
        let entries = doc["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2, "N=1 prior + 1 new = 2, collision skipped");
        let first_still = entries
            .iter()
            .find(|e| e["id"] == json!("engine-rs-first"))
            .expect("prior id must survive the merge");
        assert_eq!(
            first_still["capability"],
            json!(first_entry.capability),
            "the ORIGINAL entry must win, not the colliding candidate"
        );
        assert!(entries.iter().any(|e| e["id"] == json!("engine-rs-second")));
    }
}
