//! `COMMANDER` registration, real-fixture drain-log dedup replay, and the "call, not
//! capability" assertion (`EN.15.F` task 5).
//!
//! Fixtures live under `tests/fixtures/commander/drain-logs/` — verbatim copies of the fleet's
//! real drain logs (never referenced from `planning/` here: that path is a gitignored symlink
//! into the private HQ vault and would pass on this machine and fail on every CI runner, per
//! this block's own amended `notes`): `autonomous-foundation.jsonl` (105 lines),
//! `context-handling-between-nodes.jsonl` (127 lines), `coordination-layer-port.jsonl` (the
//! roadmap this very block ships under — grows as this lane's own drains run), and
//! `okf-core-fixture.jsonl` (the 4-line fixture `okf-core`'s own test suite already commits
//! to). The block record's `testing_strategy` originally named "the ONE existing 105-line
//! log" — amended 2026-09-08 as a FALSE PREMISE: there are three real roadmap logs plus this
//! fixture, and the count below is derived from the fixture directory itself, never frozen as
//! a literal.
//!
//! **THIS BLOCK'S SCAR TISSUE, restated as this file's second half:** a mechanism can be
//! schema'd, gated and documented while nothing ever calls it — the Python commander's own
//! drain-log writer was tested in isolation while the drain loop itself never invoked it.
//! [`a_full_commander_drain_pass_actually_invokes_the_drain_log_append_and_the_heartbeat_stamp`]
//! asserts the CALL: a real [`engine_core::workflows::commander::CommanderDrainNode::process`]
//! pass over a real temporary tree leaves a new line in the drain log and a heartbeat file on
//! disk — not merely that the writer functions work when invoked directly (that is already
//! covered by `drain_log.rs`'s own `#[cfg(test)]` module).

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use serde_json::json;

use engine_core::node::Node as _;
use engine_core::schema::WorkflowSchema;
use engine_core::workflow::Workflow;
use engine_core::workflows::commander::drain_log::{build_dedup_index, load_existing_lines};
use engine_core::workflows::commander::{schema, CommanderDrainNode, COMMANDER_WORKFLOW_TYPE};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/commander/drain-logs")
}

fn non_blank_line_count(path: &std::path::Path) -> usize {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count()
}

// --- registration: COMMANDER is a dispatchable, structurally sound graph -------------------

#[test]
fn commander_schema_declares_the_registered_workflow_type() {
    let schema: WorkflowSchema = schema();
    assert_eq!(schema.workflow_type, COMMANDER_WORKFLOW_TYPE);
}

#[test]
fn commander_registry_and_schema_assemble_into_a_validated_workflow() {
    // `Workflow::new_validated` fails loudly if the declared graph is not structurally
    // sound -- this is the engine-core-level half of "COMMANDER is registered and
    // dispatchable" (the dispatcher-registration half lives in
    // `engine-serve/src/workflows.rs`'s own `register_commander`/its test module, since
    // `engine-core` cannot depend on `engine-serve`).
    let workflow = Workflow::new_validated(
        engine_core::workflows::commander::registry(),
        engine_core::workflows::commander::schema(),
    );
    match workflow {
        Ok(_) => {}
        Err(err) => {
            panic!("COMMANDER's declared graph must pass WorkflowValidator::validate: {err}")
        }
    }
}

// --- dedup replay over EVERY real drain-log fixture, never a named/frozen one --------------

#[test]
fn dedup_replays_every_real_drain_log_fixture_with_a_directory_derived_count() {
    let dir = fixtures_dir();
    let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|err| panic!("fixtures dir {} must exist: {err}", dir.display()))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("jsonl"))
        .collect();
    entries.sort();

    // THE GUARD: an empty fixture directory must fail loudly. A test that silently passes
    // over zero files would "replay" nothing while reporting success -- indistinguishable
    // from a real replay, exactly the ambiguity THE DRAIN LOG NEVER SKIPS exists to close.
    assert!(
        !entries.is_empty(),
        "commander drain-log fixture directory {} must not be empty",
        dir.display()
    );

    let mut total_rows = 0usize;
    let mut total_keys = 0usize;

    for path in &entries {
        let expected = non_blank_line_count(path);
        let loaded = load_existing_lines(path);
        assert_eq!(
            loaded.len(),
            expected,
            "load_existing_lines must not drop or duplicate a single non-blank line in {}",
            path.display()
        );
        total_rows += loaded.len();

        let (receipt_keys, message_keys) = build_dedup_index(&loaded);
        total_keys += receipt_keys.len() + message_keys.len();

        // Replay determinism -- rebuilding the index from the IDENTICAL lines a second time
        // must produce identical key sets. This is exactly the property that makes running
        // the same drain twice over an unchanged log a safe no-op rather than a race.
        let (receipt_keys_2, message_keys_2) = build_dedup_index(&loaded);
        assert_eq!(
            receipt_keys,
            receipt_keys_2,
            "dedup index must be deterministic over {}",
            path.display()
        );
        assert_eq!(
            message_keys,
            message_keys_2,
            "dedup index must be deterministic over {}",
            path.display()
        );
    }

    // The count is asserted against the fixture directory's OWN file count, never a literal
    // like "404" or "105" -- exactly the rewrite the block record's amendment demanded.
    let directory_derived_total: usize = entries.iter().map(|p| non_blank_line_count(p)).sum();
    assert_eq!(
        total_rows, directory_derived_total,
        "the replayed row count must equal the fixture directory's own file count"
    );
    assert!(
        total_keys > 0,
        "the real fixture logs must contain at least one receipt/message row to dedup against"
    );
}

// --- ASSERT THE CALL, NOT THE CAPABILITY ----------------------------------------------------

/// A minimal brain root: one `[[repos]]` entry for `repo_slug` plus the `hq` entry
/// `BrainConfig::scope_dependencies` requires, and that repo's own `planning/state.json`.
/// Mirrors `emit_commit.rs`'s own `brain_fixture` test helper.
fn brain_fixture(repo_slug: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo_dir = dir.path().join(repo_slug);
    fs::create_dir_all(repo_dir.join("planning")).expect("mkdir");
    fs::write(
        dir.path().join("brain.toml"),
        format!(
            r#"[[repos]]
slug = "{repo_slug}"
tier = "core"
repo_path = "{repo_slug}"
status_file = "{repo_slug}/planning/status.md"
cache_doc = "docs/projects/{repo_slug}.md"

[[repos]]
slug = "hq"
repo_path = "."
status_file = "planning/status.md"
cache_doc = "docs/projects/hq.md"
"#
        ),
    )
    .expect("write brain.toml");
    fs::write(
        repo_dir.join("planning").join("state.json"),
        format!(
            r#"{{ "repo": "{repo_slug}", "kind": "project", "updated": "2026-08-20",
  "focus": {{ "now": [], "next": [], "blocked": [] }},
  "tracks": [{{ "title": "P1", "blocks": [] }}] }}"#
        ),
    )
    .expect("write state.json");
    (dir, repo_dir)
}

#[tokio::test]
async fn a_full_commander_drain_pass_actually_invokes_the_drain_log_append_and_the_heartbeat_stamp()
{
    let (root_dir, repo_dir) = brain_fixture("engine-rs");
    let root = root_dir.path();
    let lock_dir = root.join(".fleet-locks");
    let drain_log_path = lock_dir.join("commander-drain-log.jsonl");
    let heartbeat_path = lock_dir
        .join("commander-heartbeats")
        .join("call-assertion-test.heartbeat");

    // Before: neither artifact exists. The test proves the PASS is what produces them, not
    // some ambient state left over from setup.
    assert!(!drain_log_path.exists(), "drain log must not pre-exist");
    assert!(
        !heartbeat_path.exists(),
        "heartbeat file must not pre-exist"
    );

    let node = CommanderDrainNode::new();
    let ctx = engine_contract::TaskContext {
        event: json!({
            "root": root.display().to_string(),
            "repo": "engine-rs",
            "agent": "call-assertion-test",
            "dir": repo_dir.display().to_string(),
            "lock_dir": lock_dir.display().to_string(),
            "heartbeat_name": "call-assertion-test",
            "now": "2026-09-08T12:00:00Z",
        }),
        nodes: HashMap::new(),
        metadata: json!({}),
        node_runs: HashMap::new(),
    };

    let result = node
        .process(ctx)
        .await
        .expect("commander drain pass must succeed");

    // The CALL, not the capability: the drain-log writer and the heartbeat stamp are
    // themselves already unit-tested in `drain_log.rs` -- what this test proves is that a
    // full COMMANDER pass actually reaches them.
    assert!(
        drain_log_path.exists(),
        "a full commander pass must actually append the drain log, not merely be capable of it"
    );
    let drain_log_text = fs::read_to_string(&drain_log_path).expect("read drain log");
    assert_eq!(
        drain_log_text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count(),
        1,
        "exactly one drain-summary row must have been appended by this one pass"
    );
    let drain_row: serde_json::Value =
        serde_json::from_str(drain_log_text.lines().next().unwrap()).unwrap();
    assert_eq!(drain_row["record"], "drain");
    // No `roadmap` was supplied on the event -- THE DRAIN LOG NEVER SKIPS: the row is written
    // with `roadmap: null`, never omitted entirely.
    assert!(drain_row["roadmap"].is_null());

    assert!(
        heartbeat_path.exists(),
        "a full commander pass must actually stamp the heartbeat, not merely be capable of it"
    );
    let heartbeat_text = fs::read_to_string(&heartbeat_path).expect("read heartbeat");
    let heartbeat_trimmed = heartbeat_text.trim();
    assert!(
        heartbeat_trimmed.parse::<i64>().is_ok(),
        "heartbeat must be bare epoch seconds, not an ISO string: {heartbeat_trimmed:?}"
    );

    // The node's own reported outcome names both writes -- confirms the assertion above is
    // reading the SAME pass's result, not a coincidental leftover file.
    let reported = &result.nodes["CommanderDrainNode"];
    assert_eq!(reported["heartbeat_stamped"], true);
    assert_eq!(reported["drain_log_receipts_added"], 0);
    assert_eq!(reported["drain_log_messages_added"], 0);
}
