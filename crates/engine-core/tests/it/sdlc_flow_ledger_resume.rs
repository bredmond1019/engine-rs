//! EN.14.B task 3 — the headline integration test for ledger rehydration on
//! resume: a state file committed with two `claude_sessions` entries,
//! resumed, given one more billed call, and read back off disk with three
//! entries in order.
//!
//! Also carries the two controls the acceptance criteria call out by name:
//! a RESTART must not carry the old ledger forward (one entry, not three),
//! and a run that was never resumed must produce a ledger identical to
//! today's (absent, not an empty array).
//!
//! Do not assert on `ClaudeSession::model`/`started_at` — those fields
//! belong to EN.14.D and do not exist yet.

use std::collections::HashMap;
use std::path::Path;

use engine_contract::TaskContext;
use engine_core::node::Node;
use engine_core::sessions::{self, ClaudeSession};
use engine_core::workflows::sdlc_flow::policy::SdlcPolicy;
use engine_core::workflows::sdlc_flow::schema::{RunMeta, SDLCState};
use engine_core::workflows::sdlc_flow::setup::{LoadTaskStateNode, RESOLVED_POLICY_IDENTITY};
use serde_json::json;

fn empty_context(event: serde_json::Value) -> TaskContext {
    TaskContext {
        event,
        nodes: HashMap::new(),
        metadata: json!({}),
        node_runs: HashMap::new(),
    }
}

/// The shape a real walk hands `LoadTaskStateNode`: `SetupWorktreeNode`'s
/// `worktree_path` plus a stamped resolved policy, which the node reads
/// strictly.
fn ctx_with_worktree(worktree: &Path, event: serde_json::Value) -> TaskContext {
    let mut ctx = empty_context(event);
    ctx.nodes.insert(
        "SetupWorktreeNode".to_string(),
        json!({ "worktree_path": worktree.to_string_lossy() }),
    );
    ctx.nodes.insert(
        RESOLVED_POLICY_IDENTITY.to_string(),
        serde_json::to_value(SdlcPolicy::default()).expect("policy serializes"),
    );
    ctx
}

fn session(node: &str, id: &str, cost: f64) -> ClaudeSession {
    ClaudeSession {
        node: node.to_string(),
        session_id: Some(id.to_string()),
        ok: true,
        cost_usd: cost,
        input_tokens: 100,
        output_tokens: 10,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
    }
}

fn run_meta(worktree: &Path, claude_sessions: Vec<ClaudeSession>) -> RunMeta {
    RunMeta {
        branch: "sdlc/my-spec".to_string(),
        worktree_path: worktree.to_string_lossy().to_string(),
        started_at: "2026-08-01T00:00:00Z".to_string(),
        updated_at: "2026-08-01T00:00:00Z".to_string(),
        run_id: Some("run-aaaa".to_string()),
        claude_sessions,
    }
}

/// Write a committed state file at `<worktree>/planning/<slug>/sdlc/sdlc-flow-state.json`
/// carrying `claude_sessions` in its `RunMeta`, returning the `sdlc/` dir.
fn seed_committed_state(
    worktree: &Path,
    slug: &str,
    claude_sessions: Vec<ClaudeSession>,
) -> std::path::PathBuf {
    let dir = worktree.join("planning").join(slug);
    let sdlc_dir = dir.join("sdlc");
    std::fs::create_dir_all(&sdlc_dir).unwrap();
    // A restart (`resume: false`) needs `tasks.json` present too, or the
    // node falls back to "no state or tasks file found" — give every
    // fixture one pending task so the restart control has somewhere to
    // bootstrap from.
    std::fs::write(
        dir.join("tasks.json"),
        json!([{ "task_id": 1, "title": "One", "description": "d1" }]).to_string(),
    )
    .unwrap();

    let state = SDLCState::new(slug);
    let meta = run_meta(worktree, claude_sessions);
    let committed = state.to_committed_state_json(&meta, None, None, None, None, None);
    std::fs::write(
        sdlc_dir.join("sdlc-flow-state.json"),
        serde_json::to_string(&committed).unwrap(),
    )
    .unwrap();
    sdlc_dir
}

/// THE HEADLINE TEST. Two committed entries, resumed, one more invocation
/// appended, persisted, and read back with three entries in order, the
/// first two byte-identical to what was committed.
#[tokio::test]
async fn resume_rehydrates_the_ledger_so_a_third_invocation_lands_as_the_third_entry() {
    let worktree = tempfile::tempdir().expect("tempdir");
    let seeded = vec![
        session("ImplementNode", "sess-1", 0.10),
        session("TestNode", "sess-2", 0.20),
    ];
    seed_committed_state(worktree.path(), "my-spec", seeded.clone());

    let mut ctx = ctx_with_worktree(
        worktree.path(),
        json!({ "spec_slug": "my-spec", "resume": true }),
    );

    let out = LoadTaskStateNode::new()
        .process(ctx.clone())
        .await
        .expect("load should succeed");
    ctx = out;

    // Rehydration must have landed in ctx.metadata BEFORE any new
    // invocation appends to it.
    let rehydrated = sessions::read_sessions(&ctx.metadata);
    assert_eq!(
        rehydrated.len(),
        2,
        "resume must rehydrate the committed ledger into ctx.metadata"
    );

    // One more billed call this segment.
    sessions::append_session(&mut ctx.metadata, session("ReviewNode", "sess-3", 0.30));

    // Persist exactly as a real SaveStateNode would: pull the ledger back
    // out of ctx.metadata and write it to disk via a fresh RunMeta.
    let state = SDLCState::new("my-spec");
    let meta = run_meta(worktree.path(), sessions::read_sessions(&ctx.metadata));
    let committed = state.to_committed_state_json(&meta, None, None, None, None, None);
    let state_path = worktree
        .path()
        .join("planning")
        .join("my-spec")
        .join("sdlc")
        .join("sdlc-flow-state.json");
    std::fs::write(&state_path, serde_json::to_string(&committed).unwrap()).unwrap();

    // Read back off disk.
    let on_disk: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    let entries = on_disk["claude_sessions"].as_array().expect("array");
    assert_eq!(
        entries.len(),
        3,
        "expected three entries on disk, got: {on_disk:#}"
    );
    assert_eq!(entries[0]["session_id"], "sess-1");
    assert_eq!(entries[0]["cost_usd"], 0.10);
    assert_eq!(entries[1]["session_id"], "sess-2");
    assert_eq!(entries[1]["cost_usd"], 0.20);
    assert_eq!(entries[2]["session_id"], "sess-3");
    assert_eq!(entries[2]["cost_usd"], 0.30);

    // SHOWN CAPABLE OF FAILING (gate-scope-must-be-shown-capable-of-failing):
    // commenting out the `crate::sessions::seed_sessions(&mut ctx.metadata,
    // state.claude_sessions.clone());` call on the main resume branch of
    // `LoadTaskStateNode::process` (setup.rs) makes the `rehydrated.len()`
    // assertion above go red, because there is nothing to seed the two
    // committed entries from before the new invocation appends. Observed
    // manually while authoring this test:
    //   red:   `assertion `left == right` failed: resume must rehydrate
    //           the committed ledger into ctx.metadata\n  left: 0\n right: 2`
    //   green: restoring the line returns all three tests in this file to
    //          PASS, as asserted above.
}

/// THE RESTART CONTROL: a restart (`resume: false`, existing state,
/// `tasks.json` present) must NOT carry the old ledger forward — its
/// rehydrated ledger must be empty, so a subsequent append lands as entry
/// one, not entry three. Without this, a rehydration that fires on every
/// path would pass the headline test while silently billing each restart
/// for its predecessor's spend.
#[tokio::test]
async fn restart_does_not_carry_the_old_ledger_forward() {
    let worktree = tempfile::tempdir().expect("tempdir");
    let seeded = vec![
        session("ImplementNode", "sess-1", 0.10),
        session("TestNode", "sess-2", 0.20),
    ];
    seed_committed_state(worktree.path(), "my-spec", seeded);

    // resume defaults to false: this is a restart, not a resume.
    let ctx = ctx_with_worktree(worktree.path(), json!({ "spec_slug": "my-spec" }));

    let out = LoadTaskStateNode::new()
        .process(ctx)
        .await
        .expect("load should succeed");

    let rehydrated = sessions::read_sessions(&out.metadata);
    assert!(
        rehydrated.is_empty(),
        "a restart must not carry the old ledger forward, got: {rehydrated:?}"
    );

    // A subsequent append must land as the FIRST entry, not the third.
    let mut metadata = out.metadata;
    sessions::append_session(&mut metadata, session("ImplementNode", "sess-new", 0.05));
    let after = sessions::read_sessions(&metadata);
    assert_eq!(
        after.len(),
        1,
        "restart's first invocation must be entry one"
    );
}

/// THE BEHAVIOUR-STABILITY CONTROL: a run that was never resumed (fresh
/// bootstrap from `tasks.json`, no committed state file at all) produces
/// the ledger it produces today — absent from `ctx.metadata`, not an empty
/// array.
#[tokio::test]
async fn a_fresh_run_has_no_ledger_seeded_into_metadata() {
    let worktree = tempfile::tempdir().expect("tempdir");
    let dir = worktree.path().join("planning").join("my-spec");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("tasks.json"),
        json!([{ "task_id": 1, "title": "One", "description": "d1" }]).to_string(),
    )
    .unwrap();

    let ctx = ctx_with_worktree(worktree.path(), json!({ "spec_slug": "my-spec" }));

    let out = LoadTaskStateNode::new()
        .process(ctx)
        .await
        .expect("load should succeed");

    assert!(
        out.metadata.get(sessions::SESSIONS_METADATA_KEY).is_none(),
        "a fresh run must not have a ledger key seeded into metadata at all, got: {:?}",
        out.metadata
    );
    assert!(sessions::read_sessions(&out.metadata).is_empty());
}
