//! `EN.15.L` task 5 — chain-level tests over the D57 verification ledger writer.
//!
//! Drives a real two-repo chain through
//! [`integrate_chain_with_run_record`] with a STUB [`ComposeLedgerEntriesFn`] (hermetic — no
//! live model call in the gated suite, matching this block's `testing_strategy`), against a
//! tempdir `roadmap_dir` — never a real, tracked roadmap. Covers every acceptance criterion
//! named on this block/task:
//!
//! 1. [`second_steps_bail_still_leaves_first_steps_ledger_entries_on_disk`] — THE LOAD-BEARING
//!    RED CASE: block 1's entries are on disk before block 2 even starts, proven by a chain
//!    whose second step bails.
//! 2. [`two_block_happy_path_leaves_ledger_entries_for_both`] — the happy path.
//! 3. [`create_ledger_if_absent`]'s header + `.md` wrapper: already unit-tested in
//!    `ledger.rs` task 1; re-asserted here end-to-end via
//!    [`chain_creates_ledger_with_header_and_md_wrapper_when_absent`].
//! 4. [`merging_into_an_existing_ledger_keeps_prior_entries_and_skips_duplicate_ids`] — the
//!    merge case, seeded with a prior-wave entry the stub composer re-proposes.
//! 5. [`validation_refusals_are_enforced_through_the_full_chain`] — covered/empty covered_by
//!    and missing call_site refused; `call_site: "NONE"` accepted AND recorded as a
//!    `GateRefused` journal-row finding.
//! 6. [`a_composer_error_never_fails_the_chain_and_is_recorded_as_a_journal_gap`] — a composer
//!    error/unparseable output never fails the chain; the gap is recorded as a `GateRefused`
//!    journal row (the accumulator `EN.15.G`'s `render_notes_md` renders `notes.md` from).
//!
//! Do NOT use `scripts/check_verification_ledger.py`'s exit code as evidence anywhere in this
//! file — it is WARN-only and always exits 0 by design (this block's own `testing_strategy`).

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use uuid::Uuid;

use engine_core::repo_registry::RepoRegistry;
use engine_core::workflows::orchestration::chain::resolve_explicit_chain;
use engine_core::workflows::orchestration::execute::{EngineKind, ExecutionOutcome, FlowRunner};
use engine_core::workflows::orchestration::gates::AdmissionGate;
use engine_core::workflows::orchestration::integrate::{
    integrate_chain_with_run_record, ComposeLedgerEntriesFn, NeverHeld, StepProgress,
};
use engine_core::workflows::orchestration::ledger::{Coverage, NewLedgerEntry};

// ── Shared fixtures — mirrors `orchestration.rs`'s own (private per-module, so
// duplicated in miniature here rather than reached across a test-binary module
// boundary that was never meant to be a shared library). ──────────────────────

fn two_repo_registry() -> (tempfile::TempDir, RepoRegistry) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("repo-a")).unwrap();
    std::fs::create_dir_all(dir.path().join("repo-b")).unwrap();
    std::fs::write(
        dir.path().join("brain.toml"),
        "[[repos]]\nslug = \"repo-a\"\nrepo_path = \"repo-a\"\n\
         [[repos]]\nslug = \"repo-b\"\nrepo_path = \"repo-b\"\n",
    )
    .unwrap();
    let registry = RepoRegistry::from_brain_root(dir.path()).expect("registry");
    (dir, registry)
}

fn write_state(repo_path: &Path, block_id: &str, status: &str) {
    let dir = repo_path.join("planning").join(block_id).join("sdlc");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("sdlc-flow-state.json"),
        serde_json::json!({"status": status}).to_string(),
    )
    .unwrap();
}

fn fixture_roadmap_dir(slug: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let planning_root = tempfile::tempdir().expect("tempdir");
    let roadmap_dir = planning_root.path().join("roadmaps").join(slug);
    std::fs::create_dir_all(&roadmap_dir).unwrap();
    (planning_root, roadmap_dir)
}

/// Like `orchestration.rs`'s `RecordingRunner`, minus the parts this suite never needs
/// (cwd/call-count assertions) — writes a `"status": "done"` state file per invocation unless
/// a block id is registered to bail (via [`RecordingRunner::fail_block`]), in which case it
/// writes an unparseable state file so `integrate_chain_with_run_record`'s state-write
/// verification fails and the chain bails on that step.
#[derive(Clone)]
struct RecordingRunner {
    failing_blocks: Arc<Mutex<std::collections::HashSet<String>>>,
}

impl RecordingRunner {
    fn new() -> Self {
        Self {
            failing_blocks: Arc::new(Mutex::new(std::collections::HashSet::new())),
        }
    }

    fn fail_block(&self, block_id: &str) {
        self.failing_blocks
            .lock()
            .unwrap()
            .insert(block_id.to_string());
    }

    fn into_runner(self) -> FlowRunner {
        Arc::new(move |invocation| {
            let this = self.clone();
            Box::pin(async move {
                if this
                    .failing_blocks
                    .lock()
                    .unwrap()
                    .contains(&invocation.block_id)
                {
                    // Not valid JSON at all -> `verify_state_write` fails loudly and the
                    // chain bails on this step, per `a_corrupted_state_write_fails_the_run_
                    // loudly` in `orchestration.rs` (the same fixture shape).
                    std::fs::create_dir_all(
                        invocation
                            .repo_path
                            .join("planning")
                            .join(&invocation.block_id)
                            .join("sdlc"),
                    )
                    .unwrap();
                    std::fs::write(
                        invocation
                            .repo_path
                            .join("planning")
                            .join(&invocation.block_id)
                            .join("sdlc")
                            .join("sdlc-flow-state.json"),
                        "not json",
                    )
                    .unwrap();
                } else {
                    write_state(&invocation.repo_path, &invocation.block_id, "done");
                }
                Ok(engine_contract::TaskContext {
                    event: serde_json::json!({}),
                    nodes: std::collections::HashMap::new(),
                    metadata: serde_json::json!({}),
                    node_runs: std::collections::HashMap::new(),
                })
            })
        })
    }
}

fn no_deps(
    _repo: &str,
    _id: &str,
) -> Vec<engine_core::workflows::orchestration::gates::DependencyEdge> {
    Vec::new()
}

fn always_met(_repo: &str, _id: &str) -> bool {
    true
}

fn always_flow(_repo: &str, _id: &str) -> EngineKind {
    EngineKind::Flow
}

fn noop_close_block(_repo: &str, _id: &str) {}

fn read_ledger(roadmap_dir: &Path) -> serde_json::Value {
    let raw = std::fs::read_to_string(roadmap_dir.join("verification-ledger.json"))
        .expect("verification-ledger.json must exist");
    serde_json::from_str(&raw).unwrap()
}

fn entry_ids(ledger: &serde_json::Value) -> Vec<String> {
    ledger["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_string())
        .collect()
}

/// A fixed one-candidate-per-block stub composer: proposes a single, always-valid
/// candidate named after the step's own `block_id`, so each integrated step's ledger
/// entry is distinguishable by id (`<repo>-<block_id>-cap`, after `LedgerEntry::compose`'s
/// own `<repo>-` stamping).
fn fixed_candidate_composer() -> Box<ComposeLedgerEntriesFn> {
    Box::new(move |outcome: &ExecutionOutcome| {
        let block_id = outcome.block_id.clone();
        Box::pin(async move {
            Ok(vec![NewLedgerEntry {
                id: format!("{block_id}-cap"),
                capability: format!("{block_id} shipped"),
                call_site: format!("src/lib.rs:{block_id}"),
                env: "fleet-main".to_string(),
                how_to_verify: "cargo nextest run".to_string(),
                evidence: "exit 0".to_string(),
                coverage: Coverage::Uncovered,
                covered_by: vec![],
                cross_repo: None,
                ..Default::default()
            }])
        })
    })
}

// ── 1. THE LOAD-BEARING RED CASE ────────────────────────────────────────────

/// A chain whose SECOND step bails still leaves the FIRST step's ledger entries on disk —
/// the per-close-not-batched property this whole block exists for. Gate-scope guard
/// (`gate-scope-must-be-shown-capable-of-failing`): perturbed `integrate_chain_impl_inner`
/// (`integrate.rs`) by commenting out the `compose_and_append_ledger_entries(...).await;`
/// call entirely — simulating a writer that never appends per-step at all. Observed failure:
/// `panicked at crates/engine-core/tests/it/ledger.rs:164:10: verification-ledger.json must
/// exist: Os { code: 2, kind: NotFound, message: "No such file or directory" }` — with no
/// per-step writer, block A.1's entry is never written before B.1 bails, so even the file
/// itself never gets created. Reverted with `git checkout --` before this task's commit;
/// `git status --porcelain` on `crates/engine-core/src/` is empty and no production source
/// differs from HEAD.
#[tokio::test]
async fn second_steps_bail_still_leaves_first_steps_ledger_entries_on_disk() {
    let (_repos_dir, registry) = two_repo_registry();
    let (_planning_root, roadmap_dir) = fixture_roadmap_dir("ledger-bail");
    let runner = RecordingRunner::new();
    runner.fail_block("B.1");
    let flow_runner = runner.into_runner();
    let admission = AdmissionGate::with_default_policy();
    let composer = fixed_candidate_composer();

    let chain = resolve_explicit_chain(vec![
        ("repo-a".to_string(), "A.1".to_string()),
        ("repo-b".to_string(), "B.1".to_string()),
    ]);

    let err = integrate_chain_with_run_record(
        &chain,
        &no_deps,
        &always_met,
        &admission,
        &NeverHeld,
        Duration::from_millis(5),
        None,
        None,
        None,
        &always_flow,
        &registry,
        &flow_runner,
        &roadmap_dir,
        None,
        &|_: &StepProgress| {},
        false,
        true,
        Uuid::new_v4(),
        &noop_close_block,
        None,
        None,
        None,
        Some(composer.as_ref()),
    )
    .await
    .expect_err("the second step's corrupted state write must bail the chain");
    let _ = err;

    let ledger = read_ledger(&roadmap_dir);
    let ids = entry_ids(&ledger);
    assert_eq!(
        ids,
        vec!["repo-a-A.1-cap".to_string()],
        "block A.1's entry must be on disk even though block B.1 bailed"
    );
}

// ── 2. Happy path ────────────────────────────────────────────────────────

#[tokio::test]
async fn two_block_happy_path_leaves_ledger_entries_for_both() {
    let (_repos_dir, registry) = two_repo_registry();
    let (_planning_root, roadmap_dir) = fixture_roadmap_dir("ledger-happy");
    let runner = RecordingRunner::new();
    let flow_runner = runner.into_runner();
    let admission = AdmissionGate::with_default_policy();

    // Records the ledger's entry ids at the moment each step is observed integrating, so we
    // can assert block 1's entry exists strictly before block 2's step observer fires for
    // block 2 (the observer runs after that step's own ledger append — see the loop's own
    // ordering doc — so "seen at index 2" already implies block 1's write happened first).
    let seen_at_step_two: Arc<Mutex<Option<Vec<String>>>> = Arc::new(Mutex::new(None));
    let seen_at_step_two_write = seen_at_step_two.clone();
    let roadmap_dir_for_observer = roadmap_dir.clone();
    let composer = fixed_candidate_composer();

    let chain = resolve_explicit_chain(vec![
        ("repo-a".to_string(), "A.1".to_string()),
        ("repo-b".to_string(), "B.1".to_string()),
    ]);

    let observer_hits = AtomicUsize::new(0);
    let outcomes = integrate_chain_with_run_record(
        &chain,
        &no_deps,
        &always_met,
        &admission,
        &NeverHeld,
        Duration::from_millis(5),
        None,
        None,
        None,
        &always_flow,
        &registry,
        &flow_runner,
        &roadmap_dir,
        None,
        &move |progress: &StepProgress| {
            let hit = observer_hits.fetch_add(1, Ordering::SeqCst) + 1;
            if hit == 2 {
                let _ = progress;
                let ledger = read_ledger(&roadmap_dir_for_observer);
                *seen_at_step_two_write.lock().unwrap() = Some(entry_ids(&ledger));
            }
        },
        false,
        true,
        Uuid::new_v4(),
        &noop_close_block,
        None,
        None,
        None,
        Some(composer.as_ref()),
    )
    .await
    .expect("two-block happy path must integrate cleanly");
    assert_eq!(outcomes.len(), 2);

    let at_step_two = seen_at_step_two
        .lock()
        .unwrap()
        .clone()
        .expect("observer must have fired for step 2");
    assert!(
        at_step_two.contains(&"repo-a-A.1-cap".to_string()),
        "block A.1's entry must already be on disk by the time block B.1's step observer fires: {at_step_two:?}"
    );

    let ledger = read_ledger(&roadmap_dir);
    let mut ids = entry_ids(&ledger);
    ids.sort();
    assert_eq!(
        ids,
        vec!["repo-a-A.1-cap".to_string(), "repo-b-B.1-cap".to_string()],
        "both blocks' entries must be present"
    );
}

// ── 3. Ledger creation when absent ──────────────────────────────────────

#[tokio::test]
async fn chain_creates_ledger_with_header_and_md_wrapper_when_absent() {
    let (_repos_dir, registry) = two_repo_registry();
    let (_planning_root, roadmap_dir) = fixture_roadmap_dir("ledger-create");
    let runner = RecordingRunner::new();
    let flow_runner = runner.into_runner();
    let admission = AdmissionGate::with_default_policy();
    let composer = fixed_candidate_composer();

    assert!(!roadmap_dir.join("verification-ledger.json").exists());

    let chain = resolve_explicit_chain(vec![("repo-a".to_string(), "A.1".to_string())]);

    integrate_chain_with_run_record(
        &chain,
        &no_deps,
        &always_met,
        &admission,
        &NeverHeld,
        Duration::from_millis(5),
        None,
        None,
        None,
        &always_flow,
        &registry,
        &flow_runner,
        &roadmap_dir,
        Some("unattended"),
        &|_: &StepProgress| {},
        false,
        true,
        Uuid::new_v4(),
        &noop_close_block,
        None,
        None,
        None,
        Some(composer.as_ref()),
    )
    .await
    .expect("single-block chain must integrate cleanly");

    let ledger = read_ledger(&roadmap_dir);
    assert_eq!(ledger["repo"], serde_json::json!("repo-a"));
    assert_eq!(ledger["lane"], serde_json::json!("unattended"));
    assert!(ledger["created"].as_str().is_some_and(|s| !s.is_empty()));
    assert_eq!(
        ledger["status_values"],
        serde_json::json!([
            "untested",
            "tested",
            "partial",
            "failed",
            "blocked",
            "not_applicable"
        ])
    );

    let md_path = roadmap_dir.join("verification-ledger.md");
    assert!(md_path.exists());
    let md = std::fs::read_to_string(&md_path).unwrap();
    assert!(md.starts_with("---\n"));
    assert!(md.contains("type: Reference"));
}

// ── 4. Merge case ────────────────────────────────────────────────────────

#[tokio::test]
async fn merging_into_an_existing_ledger_keeps_prior_entries_and_skips_duplicate_ids() {
    use engine_core::workflows::orchestration::ledger::{
        create_ledger_if_absent, merge_append_entries, LedgerEntry,
    };

    let (_repos_dir, registry) = two_repo_registry();
    let (_planning_root, roadmap_dir) = fixture_roadmap_dir("ledger-merge");
    create_ledger_if_absent(
        &roadmap_dir,
        "ledger-merge",
        "repo-a",
        "unattended",
        "2026-09-01T00:00:00Z",
    )
    .unwrap();
    let prior = LedgerEntry::compose(
        "repo-a",
        "EARLIER.1",
        NewLedgerEntry {
            id: "A.1-cap".to_string(),
            capability: "an earlier wave's capability".to_string(),
            env: "fleet-main".to_string(),
            call_site: "src/lib.rs:1".to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    merge_append_entries(
        &roadmap_dir.join("verification-ledger.json"),
        std::slice::from_ref(&prior),
    )
    .unwrap();

    // The stub composer for this run re-proposes the SAME id the prior wave already holds
    // (`A.1-cap`, unprefixed — `LedgerEntry::compose` will stamp it to `repo-a-A.1-cap`,
    // colliding with `prior`'s already-stamped id).
    let composer: Box<ComposeLedgerEntriesFn> = Box::new(move |_outcome: &ExecutionOutcome| {
        Box::pin(async move {
            Ok(vec![NewLedgerEntry {
                id: "A.1-cap".to_string(),
                capability: "a DIFFERENT capability text from the second wave".to_string(),
                env: "fleet-main".to_string(),
                call_site: "src/lib.rs:99".to_string(),
                ..Default::default()
            }])
        })
    });

    let runner = RecordingRunner::new();
    let flow_runner = runner.into_runner();
    let admission = AdmissionGate::with_default_policy();
    let chain = resolve_explicit_chain(vec![("repo-a".to_string(), "A.1".to_string())]);

    integrate_chain_with_run_record(
        &chain,
        &no_deps,
        &always_met,
        &admission,
        &NeverHeld,
        Duration::from_millis(5),
        None,
        None,
        None,
        &always_flow,
        &registry,
        &flow_runner,
        &roadmap_dir,
        None,
        &|_: &StepProgress| {},
        false,
        true,
        Uuid::new_v4(),
        &noop_close_block,
        None,
        None,
        None,
        Some(composer.as_ref()),
    )
    .await
    .expect("chain must integrate cleanly");

    let ledger = read_ledger(&roadmap_dir);
    let entries = ledger["entries"].as_array().unwrap();
    assert_eq!(
        entries.len(),
        1,
        "the colliding id must be skipped, not duplicated: {entries:?}"
    );
    assert_eq!(
        entries[0]["capability"],
        serde_json::json!("an earlier wave's capability"),
        "the ORIGINAL prior-wave entry must win, not the re-proposed candidate"
    );
}

// ── 5. Validation refusals through the full chain ───────────────────────

#[tokio::test]
async fn validation_refusals_are_enforced_through_the_full_chain() {
    let (_repos_dir, registry) = two_repo_registry();
    let (_planning_root, roadmap_dir) = fixture_roadmap_dir("ledger-refusals");
    let runner = RecordingRunner::new();
    let flow_runner = runner.into_runner();
    let admission = AdmissionGate::with_default_policy();

    // Three candidates from one step: (1) covered with empty covered_by -> refused,
    // (2) missing call_site -> refused, (3) call_site: "NONE" -> accepted.
    let composer: Box<ComposeLedgerEntriesFn> = Box::new(move |_outcome: &ExecutionOutcome| {
        Box::pin(async move {
            Ok(vec![
                NewLedgerEntry {
                    id: "covered-no-tests".to_string(),
                    capability: "bad: covered with no tests".to_string(),
                    env: "fleet-main".to_string(),
                    call_site: "src/lib.rs:1".to_string(),
                    coverage: Coverage::Covered,
                    covered_by: vec![],
                    ..Default::default()
                },
                NewLedgerEntry {
                    id: "missing-call-site".to_string(),
                    capability: "bad: no call_site at all".to_string(),
                    env: "fleet-main".to_string(),
                    call_site: String::new(),
                    ..Default::default()
                },
                NewLedgerEntry {
                    id: "none-call-site".to_string(),
                    capability: "ok: literal NONE call_site".to_string(),
                    env: "fleet-main".to_string(),
                    call_site: "NONE".to_string(),
                    ..Default::default()
                },
            ])
        })
    });

    let rows: Arc<Mutex<Vec<engine_contract::JournalRow>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_rows = rows.clone();
    let journal_sink: Arc<dyn Fn(engine_contract::JournalRow) + Send + Sync> =
        Arc::new(move |row: engine_contract::JournalRow| sink_rows.lock().unwrap().push(row));

    let chain = resolve_explicit_chain(vec![("repo-a".to_string(), "A.1".to_string())]);
    integrate_chain_with_run_record(
        &chain,
        &no_deps,
        &always_met,
        &admission,
        &NeverHeld,
        Duration::from_millis(5),
        None,
        None,
        None,
        &always_flow,
        &registry,
        &flow_runner,
        &roadmap_dir,
        None,
        &|_: &StepProgress| {},
        false,
        true,
        Uuid::new_v4(),
        &noop_close_block,
        Some(journal_sink.as_ref()),
        None,
        None,
        Some(composer.as_ref()),
    )
    .await
    .expect("a refused candidate must never fail the chain");

    let ledger = read_ledger(&roadmap_dir);
    let ids = entry_ids(&ledger);
    assert_eq!(
        ids,
        vec!["repo-a-none-call-site".to_string()],
        "only the call_site:NONE candidate should survive composition: {ids:?}"
    );

    // The accepted call_site:NONE entry must still surface as a tracked finding — the
    // composer prompt's own promise ("it becomes a tracked finding downstream") is kept
    // by recording a GateRefused journal row alongside writing the entry, not instead of
    // it.
    let recorded = rows.lock().unwrap();
    let finding_row = recorded
        .iter()
        .find(|r| r.kind == engine_contract::JournalDecisionKind::GateRefused)
        .expect(
            "an accepted call_site:NONE entry must be recorded as a GateRefused journal \
             row — the same accumulator EN.15.G's render_notes_md renders notes.md's \
             findings from",
        );
    assert!(finding_row.reason.contains("call_site: NONE"));
    assert_eq!(
        finding_row.detail["gap"],
        serde_json::json!("ledger_entry_call_site_none")
    );
    assert_eq!(
        finding_row.detail["entry_id"],
        serde_json::json!("repo-a-none-call-site")
    );
}

// ── 6. Composer error is a non-fatal, recorded gap ──────────────────────

#[tokio::test]
async fn a_composer_error_never_fails_the_chain_and_is_recorded_as_a_journal_gap() {
    let (_repos_dir, registry) = two_repo_registry();
    let (_planning_root, roadmap_dir) = fixture_roadmap_dir("ledger-composer-error");
    let runner = RecordingRunner::new();
    let flow_runner = runner.into_runner();
    let admission = AdmissionGate::with_default_policy();

    let composer: Box<ComposeLedgerEntriesFn> = Box::new(move |_outcome: &ExecutionOutcome| {
        Box::pin(async move { Err("model returned unparseable output".to_string()) })
    });

    let rows: Arc<Mutex<Vec<engine_contract::JournalRow>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_rows = rows.clone();
    let journal_sink: Arc<dyn Fn(engine_contract::JournalRow) + Send + Sync> =
        Arc::new(move |row: engine_contract::JournalRow| sink_rows.lock().unwrap().push(row));

    let chain = resolve_explicit_chain(vec![("repo-a".to_string(), "A.1".to_string())]);
    integrate_chain_with_run_record(
        &chain,
        &no_deps,
        &always_met,
        &admission,
        &NeverHeld,
        Duration::from_millis(5),
        None,
        None,
        None,
        &always_flow,
        &registry,
        &flow_runner,
        &roadmap_dir,
        None,
        &|_: &StepProgress| {},
        false,
        true,
        Uuid::new_v4(),
        &noop_close_block,
        Some(journal_sink.as_ref()),
        None,
        None,
        Some(composer.as_ref()),
    )
    .await
    .expect("a composer error must never fail the chain");

    // No ledger file at all: an empty/errored composition never even creates one (matches
    // `compose_and_append_ledger_entries`'s own doc — a composer error returns before
    // `create_ledger_if_absent` is ever called).
    assert!(!roadmap_dir.join("verification-ledger.json").exists());

    let recorded = rows.lock().unwrap();
    let gap_row = recorded
        .iter()
        .find(|r| r.kind == engine_contract::JournalDecisionKind::GateRefused)
        .expect(
            "the composer error must be recorded as a GateRefused journal row — the same \
             accumulator EN.15.G's render_notes_md renders notes.md's findings from",
        );
    assert!(gap_row.reason.contains("verification-ledger composer"));
    assert_eq!(
        gap_row.detail["gap"],
        serde_json::json!("ledger_composer_error")
    );
}
