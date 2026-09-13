//! `EN.17.E` task 5 — the un-gateable acceptance criterion (D64): "A reply written by the
//! chain passes base-template's message check." Its evidence lives in a sibling repo
//! (`base-template/scripts/check_messages.py`), so this drives one real `EDGE_RELEASED`
//! message through `inbox_triage::process_drained_message` (task 2's entry point) against a
//! temp lock dir, then shells out to the real oracle script against that same directory and
//! asserts exit 0 — mirroring `coord_parity.rs`'s own "shell to the real sibling script, skip
//! loudly when it isn't checked out" convention (`find_brain_root` / oracle-path resolution
//! duplicated from that module, per its own stated precedent of one copy per file rather than
//! cross-test-module coupling).
//!
//! FIXTURE EVIDENCE (recorded per this task's own acceptance criteria): the literal command
//! this test runs is
//!
//! ```text
//! python3 <brain_root>/base-template/scripts/check_messages.py --quiet --lock-dir <temp lock dir>
//! ```
//!
//! and on a real fleet checkout (sibling `base-template/` present) it exits `0` against the
//! queue tree this test writes — one `EDGE_RELEASED` delivered to `repo-a`/`engine-rs`'s inbox,
//! drained (`inbox->processing` receipt), routed through `process_drained_message` (which
//! completes it — `processing->done` receipt — and writes exactly one reply into
//! `bastion`/`types`'s inbox), leaving a queue tree with two lanes each carrying one message
//! whose location is fully justified by receipts, and no `priority`/`urgency` key.

use std::path::{Path, PathBuf};
use std::process::Command;

use okf_core::{
    DurableHomeChannel, MessageDurableHome, MessageKind, MessageRecord, MessageSender,
    MessageSubject,
};

use engine_core::workflows::orchestration::coord_lane::CoordHandle;
use engine_core::workflows::orchestration::inbox_triage::{
    process_drained_message, EscalationContext, InboxTriageConfig, InboxTriageRunner,
};
use engine_core::workflows::ModelTransport;

/// Walk up from `start` looking for a `brain.toml` — the marker of the company-brain vault
/// root that houses this repo's sibling `base-template/`. Duplicated from `coord_parity.rs`
/// per that module's own stated convention (one copy per file, not cross-test-module coupling).
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

/// `<brain_root>/base-template/scripts/check_messages.py`.
fn oracle_script_path(brain_root: &Path) -> PathBuf {
    brain_root
        .join("base-template")
        .join("scripts")
        .join("check_messages.py")
}

/// Resolve the real oracle script, or `None` with a loud `eprintln!` when this checkout has no
/// sibling `base-template` to find it in (an isolated CI clone of just this repo) — skip
/// loudly, never silently pass.
fn find_oracle_script() -> Option<PathBuf> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let Some(brain_root) = find_brain_root(manifest_dir) else {
        eprintln!(
            "SKIPPING inbox_triage_message_check test: no brain.toml found walking up from {} \
             (this checkout has no sibling base-template to locate the oracle script in)",
            manifest_dir.display()
        );
        return None;
    };
    let script = oracle_script_path(&brain_root);
    if !script.is_file() {
        eprintln!(
            "SKIPPING inbox_triage_message_check test: brain root found at {} but {} does not exist",
            brain_root.display(),
            script.display()
        );
        return None;
    }
    Some(script)
}

/// A transport that panics if ever invoked — an `EDGE_RELEASED` is deterministic
/// (`inbox_triage::handle_edge_released`) and must never bill a `JudgmentNode` session.
fn panicking_transport() -> ModelTransport {
    std::sync::Arc::new(|_config: claude_code_rs::Config, _prompt: String| {
        Box::pin(async { panic!("JudgmentNode must never be called for an EDGE_RELEASED") })
    })
}

fn coord_now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn edge_released_message() -> MessageRecord {
    MessageRecord {
        message_id: "bbbbbbbb-2222-4e21-9f10-000000000099".to_string(),
        sender: MessageSender {
            agent_name: "peer-lane".to_string(),
            repo: "bastion".to_string(),
            lane: "types".to_string(),
            roadmap: "coordination-layer-port".to_string(),
        },
        sent_at: "2026-09-12T00:00:00Z".to_string(),
        kind: MessageKind::EdgeReleased,
        subject: MessageSubject {
            repo: "repo-a".to_string(),
            block: Some("DEP.1".to_string()),
        },
        body: "released".to_string(),
        durable_home: MessageDurableHome {
            channel: DurableHomeChannel::LaneLog,
            reference: "lane-log.jsonl#1".to_string(),
        },
        verified_by: "test fixture".to_string(),
        host: None,
    }
}

#[tokio::test]
async fn inbox_triage_message_check_reply_passes_check_messages_py() {
    let Some(oracle) = find_oracle_script() else {
        return;
    };

    let lock_dir = tempfile::tempdir().expect("tempdir");

    // Deliver one EDGE_RELEASED into repo-a/engine-rs's own inbox — standing in for a
    // sibling lane's `coord::write::send`.
    let coord = CoordHandle::new(
        lock_dir.path().to_path_buf(),
        "repo-a",
        "engine-rs",
        "engine-rs-1",
        coord_now_iso,
    );
    let message = edge_released_message();
    let envelope = serde_json::to_value(&message).expect("serialize envelope");
    engine_core::coord::write::send(lock_dir.path(), "repo-a", "engine-rs", envelope, None)
        .expect("send must succeed");

    // Drain it — writes the `inbox->processing` receipt `check_messages.py` requires for a
    // file sitting in `processing/`.
    let drained = coord.drain().expect("drain must succeed");
    assert_eq!(drained.len(), 1, "exactly one message must be drained");
    let record = drained[0]
        .record
        .as_ref()
        .expect("drained message must parse as a MessageRecord");

    // Route it through the real inbox_triage entry point — this is what writes exactly one
    // reply into the sender's inbox and completes the original message (`processing->done`).
    let runner =
        InboxTriageRunner::new(InboxTriageConfig::default()).with_transport(panicking_transport());
    let ctx = engine_contract::TaskContext {
        event: serde_json::json!({}),
        nodes: std::collections::HashMap::new(),
        metadata: serde_json::json!({}),
        node_runs: std::collections::HashMap::new(),
    };
    let resolve_depends_on = |_repo: &str, _id: &str| Vec::new();
    let is_edge_met = |_repo: &str, _id: &str| true;
    let escalation_ctx = EscalationContext {
        roadmap: "coordination-layer-port",
        verified_at_sha: "unknown",
        now_iso: || chrono::Utc::now().to_rfc3339(),
    };
    let (processed, _requeue_step, _escalation) = process_drained_message(
        &runner,
        &ctx,
        &coord,
        record,
        &[],
        &resolve_depends_on,
        &is_edge_met,
        "no chain",
        &escalation_ctx,
    )
    .await;
    assert!(
        processed.reply_path.is_some(),
        "the message must have produced exactly one reply"
    );

    // Now the real oracle, live, against the exact tree the chain just wrote.
    let output = Command::new("python3")
        .arg(&oracle)
        .arg("--quiet")
        .arg("--lock-dir")
        .arg(lock_dir.path())
        .output()
        .unwrap_or_else(|err| panic!("failed to spawn python3 {}: {err}", oracle.display()));

    eprintln!(
        "FIXTURE EVIDENCE (EN.17.E task 5): ran `python3 {} --quiet --lock-dir {}` -> exit {}",
        oracle.display(),
        lock_dir.path().display(),
        output.status.code().map_or(-1, |c| c),
    );

    assert!(
        output.status.success(),
        "check_messages.py must exit 0 against a chain-written reply; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
