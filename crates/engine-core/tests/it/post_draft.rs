//! Two guard tests for `EN.12.M` task 4, proven by construction rather than
//! asserted in passing (task 4's own testing strategy):
//!
//! 1. **The no-markdown-writer test.** engine-rs must write no `.md` file
//!    anywhere on the `POST_DRAFT` path — mev owns corpus writes, and a
//!    second writer is the exact duplication the mev-write-loop epic
//!    exists to prevent. Mirrors `prompt_externalization.rs`'s source-scan
//!    shape (a regex over the real source tree, plus a synthetic-source
//!    test proving the detector fires) rather than merely checking a file
//!    is absent, which would pass trivially even if the feature were
//!    broken.
//! 2. **The empty-draft refusal test**, at the `DebriefNode::process`
//!    integration level: a campaign whose journal rows carry nothing
//!    draft-worthy produces no draft — never an empty one — and dispatches
//!    it nowhere.
//!
//! Both were watched failing before being trusted (CLAUDE.md standing rule
//! 11 / task 4 AC): the no-markdown-writer guard was run against a
//! deliberately reintroduced `std::fs::write(..., "x.md", ...)` call
//! spliced into a copy of `post_draft.rs`'s source text (see
//! `guard_flags_a_direct_md_write_in_synthetic_source` below, which pins
//! that observation permanently rather than requiring it be repeated by
//! hand); the refusal test was watched failing by temporarily relaxing
//! `render_post_draft`'s bar to `Some(String::new())` and confirming
//! `post_draft_refusal_produces_no_draft_and_no_dispatch` went red before
//! the relaxation was reverted.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use regex::Regex;
use serde_json::{json, Value};
use uuid::Uuid;

use engine_contract::{JournalDecisionKind, JournalRow, TaskContext};
use engine_core::node::Node;
use engine_core::nodes::channel_transport::StubChannelTransport;
use engine_core::workflows::orchestration::debrief::{
    DebriefNode, StubJournalReader, DEBRIEF_NODE_NAME,
};

// ── (1) THE NO-MARKDOWN-WRITER TEST ─────────────────────────────────────

/// The files THIS block's `POST_DRAFT` path lives in (per the block
/// record's `files` — `post_draft.rs` new, `debrief.rs`/`graph.rs`
/// modified). Scoped deliberately to these rather than the whole
/// `orchestration/` directory: sibling modules on that path (e.g.
/// `integrate.rs`) legitimately write other `.md` files for unrelated,
/// already-shipped features, and this guard's job is "no *new* markdown
/// writer on the POST_DRAFT path", not "no engine-core code anywhere ever
/// writes markdown".
fn post_draft_path_files() -> Vec<PathBuf> {
    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set by cargo");
    let root = Path::new(&manifest_dir)
        .join("src")
        .join("workflows")
        .join("orchestration");
    vec![
        root.join("post_draft.rs"),
        root.join("debrief.rs"),
        root.join("graph.rs"),
    ]
}

/// A call that writes a `.md` file: `fs::write`/`std::fs::write` (or
/// `tokio::fs::write`) whose second-or-later argument text mentions a
/// `.md` path literal, OR any `write!`/`writeln!` into something bound to
/// an `md`-suffixed variable. Kept deliberately simple and over-inclusive
/// (regex, not a full Rust parse) — a false positive here just means a
/// legitimate write has to name itself more carefully; a false negative is
/// the failure mode this guard exists to prevent.
fn md_write_pattern() -> Regex {
    Regex::new(r#"(?:::)?fs::write\s*\([^)]*\.md"#).expect("static regex is valid")
}

/// One `.md`-write violation: a file and the matched snippet.
struct Violation {
    file: PathBuf,
    snippet: String,
}

/// Scan `contents` for a direct `.md` write. Its own named function (not
/// inlined into the test) so [`guard_flags_a_direct_md_write_in_synthetic_source`]
/// can attack the detector directly, mirroring
/// `prompt_externalization::find_violations_in_source`.
fn find_md_write_violations(file: &Path, contents: &str, pattern: &Regex) -> Vec<Violation> {
    pattern
        .find_iter(contents)
        .map(|m| Violation {
            file: file.to_path_buf(),
            snippet: m.as_str().to_string(),
        })
        .collect()
}

/// engine-rs writes no `.md` anywhere on the `POST_DRAFT` path
/// (`post_draft.rs`, `debrief.rs`, `graph.rs`). `docs/content/drafts/` is
/// mev's to write (`mev doc materialize --model learning-artifact`), never
/// this crate's (block AC: "engine-rs writes no `.md` file anywhere on
/// this path - asserted by a test that fails if `fs::write` to a `.md`
/// target appears in the module").
#[test]
fn post_draft_path_writes_no_markdown_file() {
    let files = post_draft_path_files();
    for file in &files {
        assert!(
            file.is_file(),
            "expected {} to exist - guard cannot have scanned it",
            file.display()
        );
    }

    let pattern = md_write_pattern();
    let mut violations = Vec::new();
    for file in &files {
        let contents = fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", file.display()));
        violations.extend(find_md_write_violations(file, &contents, &pattern));
    }

    assert!(
        violations.is_empty(),
        "found {} direct .md write(s) on the POST_DRAFT path - engine-rs must write no \
         markdown on this path; mev owns corpus writes (EN.12.M task 4):\n{}",
        violations.len(),
        violations
            .iter()
            .map(|v| format!("  {} :: {}", v.file.display(), v.snippet))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Prove the detector actually fires on a direct `.md` write, without
/// touching any real source file — the guard against a guard that would
/// pass even if the module started writing markdown directly (task 4:
/// "must fail if someone 'simplifies' by writing the file directly").
/// This is the deliberate-break-and-watch-it-fail observation, pinned as a
/// permanent test rather than a one-off manual step.
#[test]
fn guard_flags_a_direct_md_write_in_synthetic_source() {
    let pattern = md_write_pattern();

    let offending = r#"
        fn simplify(draft: &str) {
            std::fs::write("docs/content/drafts/campaign.md", draft).unwrap();
        }
    "#;
    let violations = find_md_write_violations(Path::new("synthetic.rs"), offending, &pattern);
    assert_eq!(
        violations.len(),
        1,
        "guard must flag a direct .md fs::write - if this fails, the detector itself is broken \
         and the guard test above proves nothing"
    );
    assert!(violations[0].snippet.contains(".md"));

    let clean = r#"
        fn propose(draft: &str) -> serde_json::Value {
            serde_json::json!({ "would_write": "docs/content/drafts/campaign.md", "digest_markdown": draft })
        }
    "#;
    let violations = find_md_write_violations(Path::new("synthetic.rs"), clean, &pattern);
    assert!(
        violations.is_empty(),
        "guard must not flag a .md path that is merely named in a proposed-intent payload, \
         never passed to fs::write"
    );
}

// ── (2) THE EMPTY-DRAFT REFUSAL, AT THE NODE LEVEL ──────────────────────

fn base_ctx(event: Value) -> TaskContext {
    TaskContext {
        event,
        nodes: Default::default(),
        metadata: serde_json::json!({}),
        node_runs: Default::default(),
    }
}

/// A journal row that carries neither a measured number nor an evidence
/// path — plain prose only. On its own, or in any combination with other
/// such rows, this must never clear the draft bar.
fn undraftworthy_row(campaign_id: &str, offset_secs: i64) -> JournalRow {
    JournalRow {
        id: Uuid::new_v4(),
        campaign_id: campaign_id.to_string(),
        run_id: Uuid::new_v4(),
        step: "build".to_string(),
        kind: JournalDecisionKind::StepIntegrated,
        reason: "everything looks fine, nothing further to say".to_string(),
        detail: json!({ "note": "no numbers, no paths, just prose" }),
        created_at: chrono::Utc::now() + chrono::Duration::seconds(offset_secs),
    }
}

/// A campaign whose journal has rows, but none draft-worthy, produces NO
/// draft at the full `DebriefNode::process` level: no `LearningArtifact`
/// payload, no second dispatch call, and the ops digest is unaffected.
/// This is the refusal the block exists to enforce — a queue that fills up
/// regardless of whether a run had anything to say trains the operator to
/// ignore it (task 4's "empty-draft refusal", proven at construction: see
/// the module doc for how this was watched failing).
#[tokio::test]
async fn post_draft_refusal_produces_no_draft_and_no_dispatch() {
    let campaign_id = Uuid::new_v4();
    let rows = vec![
        undraftworthy_row(&campaign_id.to_string(), 0),
        undraftworthy_row(&campaign_id.to_string(), 1),
    ];
    let reader = Arc::new(StubJournalReader::succeeding(rows));
    let transport = Arc::new(StubChannelTransport::succeeding());
    let written: Arc<Mutex<Vec<JournalRow>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_written = written.clone();
    let sink: Arc<engine_core::workflows::orchestration::integrate::JournalSinkFn> =
        Arc::new(move |row| sink_written.lock().unwrap().push(row));

    let node = DebriefNode::new(reader, transport.clone()).with_journal_sink(sink);
    let ctx = base_ctx(Value::String(campaign_id.to_string()));

    let result_ctx = node
        .process(ctx)
        .await
        .expect("an unworthy-but-valid campaign must not fail the node");

    let node_result = result_ctx
        .nodes
        .get(DEBRIEF_NODE_NAME)
        .expect("DebriefNode result present");
    let post_draft = &node_result["post_draft"];

    assert_eq!(
        post_draft["rendered"],
        json!(false),
        "a campaign with no measured number and no evidence path must not render a draft"
    );
    assert!(
        post_draft["payload"].is_null(),
        "refused draft must carry no payload - Some(empty draft) is exactly the failure this \
         guard exists to prevent"
    );
    assert!(
        post_draft["reason"].as_str().is_some_and(|r| !r.is_empty()),
        "a refusal must name why, never a silent pass"
    );
    assert!(
        post_draft["materialize_intent"].is_null(),
        "nothing to materialize when no draft was produced"
    );

    // Exactly one dispatch call - the ops digest - never a second for the
    // refused draft.
    assert_eq!(
        transport.calls().len(),
        1,
        "a refused draft must not be dispatched"
    );

    // The ops digest itself is still produced and still written back -
    // the refusal must not weaken the first output (block AC7).
    assert!(node_result["brief"].as_str().is_some_and(|b| !b.is_empty()));
    let written_rows = written.lock().unwrap();
    assert_eq!(
        written_rows.len(),
        1,
        "only the ops digest is journalled when the draft is refused"
    );
    assert_eq!(written_rows[0].step, DEBRIEF_NODE_NAME);
}
