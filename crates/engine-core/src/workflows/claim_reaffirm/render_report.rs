//! `RenderReportNode` (`EN.6.L` task 3) — the queue-drain loop's drain
//! target. Renders the final [`super::schema::ClaimReaffirmState`] (every
//! claim, whether [`ClaimStatus::Judged`] with a recorded
//! [`super::schema::Verdict`] or [`ClaimStatus::Failed`] after exhausting
//! `max_attempts`) as one reviewable markdown proposal report, and writes
//! it through an injectable [`ReportFs`] seam to exactly one path.
//!
//! **The report is the only file this workflow writes** (this spec's
//! Context Pointers, load-bearing): unlike `EN.7.A`'s
//! `MaterializeDocNode`/`doc_materializer` seam — which materializes a full
//! `okf_core`/`mev` doc model into the Brain corpus as a tracked source
//! document — this node writes a plain markdown file at one fixed,
//! seam-scoped path, per this task's spec text's explicitly-allowed
//! simpler alternative ("a small `WriteFileNode` with an injectable fs
//! seam scoped to exactly one report path... **preferred if okf-core takes
//! it cheaply**, OR this"). The engine only ever *proposes* here (OR.K3,
//! inherited, load-bearing) — this report is read by a human, not written
//! back into `knowledge.md`/`memory.md` by anything in this workflow.
//!
//! # Seam shape
//!
//! Mirrors `load_claims::ClaimLaneFs` and `nodes::materialize_doc`'s
//! `with_brain_root` override exactly: [`ReportFs`] is the injectable
//! write seam (production [`RealReportFs`] touches real `std::fs`; tests
//! inject an in-memory [`StubReportFs`] so the gated suite never writes to
//! disk), and [`RenderReportNode::with_report_path`] pins the target path
//! outright — bypassing [`crate::brain_root::resolve_brain_root`] entirely
//! — for the same reason `MaterializeDocNode::with_brain_root` exists:
//! tests must not depend on the ambient `brain.toml` walk.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use engine_contract::TaskContext;
use serde_json::json;

use crate::node::{Node, NodeError};
use crate::workflows::put_result;

use super::queue_router::latest_state;
use super::schema::{ClaimItem, ClaimReaffirmState, ClaimStatus, VerdictAction};

/// The `Node::name()` identity `RenderReportNode` runs under, and the
/// `ctx.nodes` key its result (the written path + rendered byte count) is
/// stamped onto. Matches `queue_router`'s private `REPORT_NODE_TARGET`
/// string exactly — the two are never imported from one another (the same
/// discipline `save_verdict::NODE_NAME`/`queue_router::SAVE_VERDICT_NODE_NAME`
/// re-export could have used here too, but `REPORT_NODE_TARGET` is a
/// route-only literal, not a `Node::name()` this module owns).
pub const NODE_NAME: &str = "RenderReportNode";

/// The path segment (relative to the resolved brain root) the production
/// default report lands at. A **fixed** filename, not a per-run/timestamped
/// one — this is a standing "reviewable proposal report" a human re-reads
/// each run, not a log that accumulates; the next run's `RenderReportNode`
/// overwrites it. `with_report_path` overrides this outright for tests and
/// for a caller that wants a different location.
const DEFAULT_REPORT_RELATIVE_PATH: &str = "planning/artifacts/claim-reaffirm/report.md";

/// Injectable write seam. The production path ([`RealReportFs`]) writes a
/// real file (creating parent directories as needed); tests substitute an
/// in-memory stub so report rendering is exercised with zero real
/// filesystem access.
pub trait ReportFs: Send + Sync {
    /// Write `contents` to `path`, creating any missing parent directories.
    /// Returns a plain `String` error message on failure (mirrors
    /// `nodes::materialize_doc`'s error-surfacing style — no panics).
    fn write(&self, path: &Path, contents: &str) -> Result<(), String>;
}

/// The live [`ReportFs`] backed by real `std::fs` writes.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealReportFs;

impl ReportFs for RealReportFs {
    fn write(&self, path: &Path, contents: &str) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("failed to create {}: {err}", parent.display()))?;
        }
        std::fs::write(path, contents)
            .map_err(|err| format!("failed to write {}: {err}", path.display()))
    }
}

/// In-memory [`ReportFs`] stub for tests: records every write it was
/// handed (path + contents), performing no real filesystem I/O. Used by
/// this module's own tests and by the `EN.6.L` task 4 integration suite's
/// single-file-guarantee assertion.
#[derive(Debug, Clone, Default)]
pub struct StubReportFs {
    writes: Arc<Mutex<Vec<(PathBuf, String)>>>,
}

impl StubReportFs {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Every write this stub recorded, in call order.
    #[must_use]
    pub fn writes(&self) -> Vec<(PathBuf, String)> {
        self.writes.lock().expect("stub lock poisoned").clone()
    }
}

impl ReportFs for StubReportFs {
    fn write(&self, path: &Path, contents: &str) -> Result<(), String> {
        self.writes
            .lock()
            .expect("stub lock poisoned")
            .push((path.to_path_buf(), contents.to_string()));
        Ok(())
    }
}

/// Renders the final [`ClaimReaffirmState`] as one markdown proposal
/// report and writes it through [`ReportFs`] to exactly one path.
pub struct RenderReportNode {
    fs: Arc<dyn ReportFs>,
    /// Explicit target path, bypassing brain-root resolution entirely.
    /// `None` (the production default) resolves
    /// [`crate::brain_root::resolve_brain_root`] at `process` time and
    /// joins [`DEFAULT_REPORT_RELATIVE_PATH`].
    report_path: Option<PathBuf>,
}

impl RenderReportNode {
    /// Construct with the live filesystem seam and brain-root-resolved
    /// production default path — the behavior-stable default.
    #[must_use]
    pub fn new() -> Self {
        Self {
            fs: Arc::new(RealReportFs),
            report_path: None,
        }
    }

    /// Override the [`ReportFs`] seam. Tests inject a [`StubReportFs`] so
    /// the suite never touches a real filesystem.
    #[must_use]
    pub fn with_fs(mut self, fs: Arc<dyn ReportFs>) -> Self {
        self.fs = fs;
        self
    }

    /// Pin the report's target path outright, bypassing
    /// [`crate::brain_root::resolve_brain_root`] entirely. Tests use this
    /// (paired with a temp directory) so the single-file-guarantee
    /// assertion never depends on the ambient `brain.toml` walk.
    #[must_use]
    pub fn with_report_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.report_path = Some(path.into());
        self
    }

    /// Resolve the target path: [`Self::report_path`] when set, else
    /// `resolve_brain_root()` joined with [`DEFAULT_REPORT_RELATIVE_PATH`].
    fn resolve_report_path(&self) -> Result<PathBuf, NodeError> {
        if let Some(path) = &self.report_path {
            return Ok(path.clone());
        }
        let root = crate::brain_root::resolve_brain_root()
            .map_err(|err| NodeError::new(format!("{NODE_NAME}: {err}")))?;
        Ok(root.join(DEFAULT_REPORT_RELATIVE_PATH))
    }
}

impl Default for RenderReportNode {
    fn default() -> Self {
        Self::new()
    }
}

/// Render one claim's markdown section: action, evidence citations,
/// reasoning, transport — or an explicit "no verdict recorded" note for a
/// claim that never left [`ClaimStatus::Failed`]/[`ClaimStatus::Pending`]
/// (the latter should not reach a drained lane, but is rendered rather
/// than silently dropped if it somehow does).
fn render_claim_section(claim: &ClaimItem) -> String {
    let mut out = String::new();
    out.push_str(&format!("## {}\n\n", claim.id));
    out.push_str(&format!("- **Source:** `{}`\n", claim.source_doc_id));
    out.push_str(&format!("- **Claim:** {}\n", claim.claim_text));
    if let Some(freshness) = &claim.freshness_date {
        out.push_str(&format!("- **Freshness date:** {freshness}\n"));
    }
    match (&claim.status, &claim.verdict) {
        (ClaimStatus::Judged, Some(verdict)) => {
            out.push_str(&format!(
                "- **Verdict:** {}\n",
                verdict_action_label(verdict.action)
            ));
            out.push_str(&format!("- **Reasoning:** {}\n", verdict.reasoning));
            if let Some(transport) = &verdict.transport {
                out.push_str(&format!(
                    "- **Transport:** {} ({}{})\n",
                    transport.tier,
                    transport.model,
                    transport
                        .endpoint
                        .as_deref()
                        .map(|e| format!(", {e}"))
                        .unwrap_or_default()
                ));
            }
            if verdict.evidence.is_empty() {
                out.push_str("- **Evidence:** none\n");
            } else {
                out.push_str("- **Evidence:**\n");
                for citation in &verdict.evidence {
                    out.push_str(&format!(
                        "  - `{}`{}: {}\n",
                        citation.doc_id,
                        citation
                            .file_path
                            .as_deref()
                            .map(|f| format!(" ({f})"))
                            .unwrap_or_default(),
                        citation.snippet
                    ));
                }
            }
        }
        (ClaimStatus::Failed, _) => {
            out.push_str(&format!(
                "- **Verdict:** none — recall/judgment failed after {} attempt(s)\n",
                claim.attempt
            ));
        }
        _ => {
            out.push_str("- **Verdict:** none recorded\n");
        }
    }
    out.push('\n');
    out
}

fn verdict_action_label(action: VerdictAction) -> &'static str {
    match action {
        VerdictAction::BumpFreshness => "bump-freshness",
        VerdictAction::Supersede => "supersede",
        VerdictAction::Archive => "archive",
        VerdictAction::NeedsHuman => "needs-human",
    }
}

/// Render the whole [`ClaimReaffirmState`] as one markdown report: a
/// summary header (claim count, per-action tally) followed by one section
/// per claim.
fn render_markdown(state: &ClaimReaffirmState) -> String {
    let mut out = String::new();
    out.push_str("# Claim Reaffirmation Report (EN.6.L)\n\n");
    out.push_str(
        "Engine-proposed verdicts over mev's stale-claim lane. The engine proposes; mev \
         writes; a human approves — no `knowledge.md`/`memory.md` edit in this repo comes \
         from this report directly.\n\n",
    );
    out.push_str(&format!("**Claims reviewed:** {}\n\n", state.claims.len()));

    if state.claims.is_empty() {
        out.push_str("_No stale claims found this run._\n");
        return out;
    }

    let mut tally: Vec<(&'static str, usize)> = vec![
        ("bump-freshness", 0),
        ("supersede", 0),
        ("archive", 0),
        ("needs-human", 0),
        ("failed (no verdict)", 0),
    ];
    for claim in &state.claims {
        match (&claim.status, &claim.verdict) {
            (ClaimStatus::Judged, Some(verdict)) => {
                let label = verdict_action_label(verdict.action);
                if let Some(entry) = tally.iter_mut().find(|(name, _)| *name == label) {
                    entry.1 += 1;
                }
            }
            (ClaimStatus::Failed, _) => {
                tally[4].1 += 1;
            }
            _ => {}
        }
    }
    out.push_str("| Action | Count |\n|---|---|\n");
    for (label, count) in &tally {
        out.push_str(&format!("| {label} | {count} |\n"));
    }
    out.push_str("\n---\n\n");

    for claim in &state.claims {
        out.push_str(&render_claim_section(claim));
    }
    out
}

#[async_trait::async_trait]
impl Node for RenderReportNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let state = latest_state(&ctx)?;
        let markdown = render_markdown(&state);
        let path = self.resolve_report_path()?;

        self.fs
            .write(&path, &markdown)
            .map_err(|err| NodeError::new(format!("{NODE_NAME}: {err}")))?;

        let mut ctx = ctx;
        put_result(
            &mut ctx,
            NODE_NAME,
            json!({
                "report_path": path.to_string_lossy(),
                "claim_count": state.claims.len(),
                "bytes_written": markdown.len(),
            }),
        );
        Ok(ctx)
    }

    fn name(&self) -> &str {
        NODE_NAME
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::super::queue_router::SAVE_VERDICT_NODE_NAME;
    use super::super::schema::{Citation, TransportInfo, Verdict};
    use super::*;
    use crate::workflows::put_result as put;

    fn empty_ctx() -> TaskContext {
        TaskContext {
            event: json!({}),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        }
    }

    fn ctx_with_state(claims: Vec<ClaimItem>) -> TaskContext {
        let mut ctx = empty_ctx();
        put(
            &mut ctx,
            SAVE_VERDICT_NODE_NAME,
            serde_json::to_value(ClaimReaffirmState { claims }).unwrap(),
        );
        ctx
    }

    fn judged_claim(id: &str, action: VerdictAction, evidence: Vec<Citation>) -> ClaimItem {
        ClaimItem {
            id: id.to_string(),
            source_doc_id: "engine-rs/planning/knowledge.md".to_string(),
            claim_text: format!("claim {id}"),
            freshness_date: Some("2025-01-01".to_string()),
            status: ClaimStatus::Judged,
            attempt: 0,
            verdict: Some(Verdict {
                action,
                evidence,
                reasoning: "because reasons".to_string(),
                transport: Some(TransportInfo {
                    tier: "cloud".to_string(),
                    model: "claude-sonnet-4-5".to_string(),
                    endpoint: None,
                }),
            }),
        }
    }

    fn failed_claim(id: &str, attempt: u32) -> ClaimItem {
        ClaimItem {
            id: id.to_string(),
            source_doc_id: "engine-rs/planning/memory.md".to_string(),
            claim_text: format!("claim {id}"),
            freshness_date: None,
            status: ClaimStatus::Failed,
            attempt,
            verdict: None,
        }
    }

    #[tokio::test]
    async fn writes_exactly_one_file_at_the_pinned_path() {
        let ctx = ctx_with_state(vec![judged_claim(
            "a",
            VerdictAction::BumpFreshness,
            vec![Citation {
                doc_id: "engine-rs/planning/status.md".to_string(),
                file_path: Some("planning/status.md".to_string()),
                snippet: "still true".to_string(),
            }],
        )]);
        let stub = Arc::new(StubReportFs::new());
        let node = RenderReportNode::new()
            .with_fs(stub.clone())
            .with_report_path("/tmp/does-not-matter/report.md");

        let ctx = node.process(ctx).await.expect("process succeeds");

        let writes = stub.writes();
        assert_eq!(writes.len(), 1, "exactly one file written");
        assert_eq!(writes[0].0, PathBuf::from("/tmp/does-not-matter/report.md"));
        assert!(writes[0].1.contains("Claim Reaffirmation Report"));
        assert!(writes[0].1.contains("bump-freshness"));
        assert!(writes[0].1.contains("still true"));

        let stamp = ctx.nodes.get(NODE_NAME).expect("stamped");
        assert_eq!(stamp.get("claim_count").and_then(|v| v.as_u64()), Some(1));
    }

    #[tokio::test]
    async fn renders_every_action_and_a_failed_claim() {
        let ctx = ctx_with_state(vec![
            judged_claim("a", VerdictAction::BumpFreshness, vec![]),
            judged_claim("b", VerdictAction::Supersede, vec![]),
            judged_claim("c", VerdictAction::Archive, vec![]),
            judged_claim("d", VerdictAction::NeedsHuman, vec![]),
            failed_claim("e", 3),
        ]);
        let stub = Arc::new(StubReportFs::new());
        let node = RenderReportNode::new()
            .with_fs(stub.clone())
            .with_report_path("/tmp/x/report.md");

        node.process(ctx).await.expect("process succeeds");

        let contents = &stub.writes()[0].1;
        assert!(contents.contains("## a"));
        assert!(contents.contains("## b"));
        assert!(contents.contains("## c"));
        assert!(contents.contains("## d"));
        assert!(contents.contains("## e"));
        assert!(contents.contains("recall/judgment failed after 3 attempt(s)"));
    }

    #[tokio::test]
    async fn empty_lane_renders_a_valid_cheap_report() {
        let ctx = ctx_with_state(vec![]);
        let stub = Arc::new(StubReportFs::new());
        let node = RenderReportNode::new()
            .with_fs(stub.clone())
            .with_report_path("/tmp/x/report.md");

        let ctx = node.process(ctx).await.expect("process succeeds");

        let contents = &stub.writes()[0].1;
        assert!(contents.contains("No stale claims found"));
        let stamp = ctx.nodes.get(NODE_NAME).expect("stamped");
        assert_eq!(stamp.get("claim_count").and_then(|v| v.as_u64()), Some(0));
    }

    #[test]
    fn default_report_path_is_scoped_under_planning_artifacts() {
        assert!(DEFAULT_REPORT_RELATIVE_PATH.starts_with("planning/artifacts/claim-reaffirm/"));
    }

    #[test]
    fn real_report_fs_writes_and_creates_parent_dirs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("report.md");
        let fs = RealReportFs;
        fs.write(&path, "hello").expect("write succeeds");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
    }
}
