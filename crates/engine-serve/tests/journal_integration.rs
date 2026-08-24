//! Integration coverage for the durable run journal (EN.12.D), in the shape
//! of `dispatch_integration.rs` / `abort_integration.rs`. This file starts
//! with task 6's golden-rendering coverage; task 7 adds the route-level
//! coverage (bail-through-the-route, budget-halt-through-the-route,
//! repo-less addressability, no-`DATABASE_URL` self-skip) alongside it.
//!
//! ## The golden rendering (task 6)
//!
//! `engine_serve::journal::render_notes_md` / `render_review_md` produce the
//! D57 `notes.md`/`review.md` view. Two readers OUTSIDE this repo parse that
//! output and cannot be exercised here (D64 un-gateable criterion):
//!
//! - `base-template/scripts/roadmap_status_discovery.py`'s
//!   `discover_run_records` — reads the YAML frontmatter block via
//!   `_FRONTMATTER_RE` for `lifecycle`, `run_started`, `run_ended`, and
//!   counts `**OPEN**` / `**HELD**` occurrences in the body via
//!   `_OPEN_ROW_RE` / `_HELD_ROW_RE` (both case-insensitive, bold markdown).
//! - `/consolidate-run` (`base-template/.claude/commands/consolidate-run.md`,
//!   Step 4) selects records by their `roadmap:` frontmatter field matching
//!   the roadmap slug being consolidated.
//!
//! This test is the standing fixture for that un-gateable criterion: it
//! asserts, field-by-field, that the rendered output carries exactly the
//! shape those two readers require, so a rendering change that would break
//! either of them fails in THIS repo's own suite rather than silently
//! shipping.

use chrono::Utc;
use engine_contract::{JournalDecisionKind, JournalRow};
use engine_serve::journal::{render_notes_md, render_review_md, RunRecordMeta};
use uuid::Uuid;

fn sample_row(step: &str, kind: JournalDecisionKind, reason: &str) -> JournalRow {
    JournalRow {
        id: Uuid::new_v4(),
        campaign_id: "campaign-golden".to_string(),
        run_id: Uuid::new_v4(),
        step: step.to_string(),
        kind,
        reason: reason.to_string(),
        detail: serde_json::json!({}),
        created_at: Utc::now(),
    }
}

fn sample_meta(run_ended: Option<&str>) -> RunRecordMeta {
    RunRecordMeta {
        repo: "engine-rs".to_string(),
        roadmap: "orchestration-extensions".to_string(),
        lane: "engine".to_string(),
        run_started: "2026-08-24".to_string(),
        run_ended: run_ended.map(|s| s.to_string()),
    }
}

/// Extracts the YAML frontmatter block the same way
/// `roadmap_status_discovery.py`'s `_parse_frontmatter` does: a `key: value`
/// map from the region between the two `---` fences.
fn parse_frontmatter(text: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let mut lines = text.lines();
    assert_eq!(
        lines.next(),
        Some("---"),
        "rendered output must open with a '---' frontmatter fence, matching _FRONTMATTER_RE"
    );
    for line in lines {
        if line == "---" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            out.insert(
                key.trim().to_string(),
                value.trim().trim_matches('"').to_string(),
            );
        }
    }
    out
}

/// GOLDEN: `roadmap_status_discovery.py`'s `discover_run_records` requires
/// `lifecycle`, `run_started`, `run_ended` in frontmatter, and counts
/// `**OPEN**`/`**HELD**` markers in the body.
#[test]
fn render_notes_md_matches_roadmap_status_discovery_shape() {
    let campaign_id = Uuid::new_v4();
    let rows = vec![
        sample_row("build", JournalDecisionKind::StepIntegrated, "ok"),
        sample_row("deploy", JournalDecisionKind::StepBailed, "network timeout"),
        sample_row(
            "deploy",
            JournalDecisionKind::BudgetHalted,
            "daily cap exceeded",
        ),
    ];
    let meta = sample_meta(None);

    let notes = render_notes_md(&campaign_id, &rows, &meta);

    let fm = parse_frontmatter(&notes);
    // Field-by-field against roadmap_status_discovery.py's actual reads:
    // `fm.get("lifecycle", "<absent>")`, `fm.get("run_started", "<absent>")`,
    // `fm.get("run_ended", "<absent>")`.
    assert_eq!(fm.get("lifecycle").map(String::as_str), Some("active"));
    assert_eq!(
        fm.get("run_started").map(String::as_str),
        Some("2026-08-24")
    );
    // run_ended is None on this meta -> frontmatter value is empty, which
    // the Python parser treats as "" (present key, empty value), distinct
    // from "<absent>" (key missing entirely) — both read as falsy/absent by
    // that script's liveness logic, so an empty value is the correct render.
    assert_eq!(fm.get("run_ended").map(String::as_str), Some(""));
    assert_eq!(
        fm.get("roadmap").map(String::as_str),
        Some("orchestration-extensions"),
        "/consolidate-run Step 4 selects records by this exact field"
    );

    // _OPEN_ROW_RE / _HELD_ROW_RE: case-insensitive `**OPEN**` / `**HELD**`.
    let open_count = notes.matches("**OPEN**").count();
    let held_count = notes.matches("**HELD**").count();
    assert_eq!(open_count, 1, "one StepBailed row must render as **OPEN**");
    assert_eq!(
        held_count, 1,
        "one BudgetHalted row must render as **HELD**"
    );

    // No telemetry: out_of_scope in the block record, checked here because
    // both out-of-repo readers parse this exact text.
    for forbidden in ["token", "cost_usd", "attempt_count"] {
        assert!(
            !notes.to_lowercase().contains(forbidden),
            "rendered notes.md must carry no telemetry field: {forbidden}"
        );
    }
}

/// GOLDEN: a repo-scoped run whose lane has closed renders `lifecycle:
/// lane-complete` with a populated `run_ended`, per D57 section 2.
#[test]
fn render_notes_md_lane_complete_when_run_ended_present() {
    let campaign_id = Uuid::new_v4();
    let rows = vec![sample_row(
        "build",
        JournalDecisionKind::StepIntegrated,
        "ok",
    )];
    let meta = sample_meta(Some("2026-08-25"));

    let notes = render_notes_md(&campaign_id, &rows, &meta);
    let fm = parse_frontmatter(&notes);

    assert_eq!(
        fm.get("lifecycle").map(String::as_str),
        Some("lane-complete")
    );
    assert_eq!(fm.get("run_ended").map(String::as_str), Some("2026-08-25"));
}

/// GOLDEN: `review.md` carries the same frontmatter contract as `notes.md`
/// (both readers glob `*.md` under the roadmap dir and parse either file
/// identically) plus a block ledger table.
#[test]
fn render_review_md_matches_roadmap_status_discovery_shape() {
    let campaign_id = Uuid::new_v4();
    let rows = vec![
        sample_row("build", JournalDecisionKind::StepIntegrated, "ok"),
        sample_row("gate", JournalDecisionKind::GateRefused, "admission denied"),
    ];
    let meta = sample_meta(Some("2026-08-25"));

    let review = render_review_md(&campaign_id, &rows, &meta);
    let fm = parse_frontmatter(&review);

    assert_eq!(
        fm.get("lifecycle").map(String::as_str),
        Some("lane-complete")
    );
    assert_eq!(
        fm.get("run_started").map(String::as_str),
        Some("2026-08-24")
    );
    assert_eq!(
        fm.get("roadmap").map(String::as_str),
        Some("orchestration-extensions")
    );

    assert_eq!(
        review.matches("**OPEN**").count(),
        1,
        "the GateRefused row must render as **OPEN**"
    );
    assert!(
        review.contains("| Step | Origin roadmap | Decision | Outcome |"),
        "review.md must carry the D57 block ledger table"
    );

    for forbidden in ["token", "cost_usd", "attempt_count"] {
        assert!(
            !review.to_lowercase().contains(forbidden),
            "rendered review.md must carry no telemetry field: {forbidden}"
        );
    }
}

/// A repo-less run has no `RunRecordMeta` at all — it is addressable purely
/// through the read route (task 5) and never goes through this renderer.
/// This is a documentation-only assertion that the renderer's inputs
/// (`RunRecordMeta`) are never optional/derived from `JournalRow` alone,
/// so a caller cannot accidentally fabricate repo/roadmap identity for a
/// repo-less campaign.
#[test]
fn renderer_requires_explicit_run_record_meta_not_derived_from_rows() {
    let campaign_id = Uuid::new_v4();
    let rows = vec![sample_row(
        "build",
        JournalDecisionKind::StepIntegrated,
        "ok",
    )];
    // JournalRow itself carries no repo/roadmap field to derive from.
    assert!(serde_json::to_value(&rows[0])
        .unwrap()
        .get("repo")
        .is_none());
    assert!(serde_json::to_value(&rows[0])
        .unwrap()
        .get("roadmap")
        .is_none());

    // The renderer is only reachable by supplying RunRecordMeta explicitly.
    let meta = sample_meta(None);
    let notes = render_notes_md(&campaign_id, &rows, &meta);
    assert!(notes.contains(&meta.repo));
    assert!(notes.contains(&meta.roadmap));
}
