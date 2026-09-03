//! `LoadClaimsNode` (`EN.6.L` task 1) — reads mev's stale-claim lane and
//! builds the initial [`ClaimReaffirmState`].
//!
//! # How mev exposes the lane (the finding this task's spec text asks for)
//!
//! mev has no `--json` subcommand purpose-built for this workflow. Two
//! candidates were checked:
//!
//! 1. **`mev attention-queue`** (`core/mev/src/main.rs`, the
//!    `AttentionQueue` command) emits every Attention-board item — across
//!    all four lanes plus backlog/capture/distilled — as a JSON array of
//!    `EN.8.A`-compatible operator payloads. It is real structured output,
//!    but its distilled-lane rows carry only a claim snippet truncated to
//!    **80 characters** (`attention_snippet(&claim, 80)`,
//!    `core/mev/src/brain/emit.rs:1928`) and no `source:` field at all —
//!    the payload type (`AttentionQueuePayload`,
//!    `core/mev/src/brain/attention_payload.rs`) is `pub(crate)` inside
//!    mev, built for an operator notification channel, not for recovering
//!    a claim's full text or its cited source document. Distinguishing a
//!    "distilled" row from a "backlog"/"capture" row in that JSON also
//!    requires text-matching the rendered `" DISTILLED "` label inside
//!    `rendered_summary`, since `lane: Option<TriageLane>` is `None` for
//!    all three non-carryover lanes alike. Unsuitable as this workflow's
//!    evidence-citation input.
//! 2. **mev as a library** — `mev` is already a workspace path-dependency
//!    of `engine-core` (`Cargo.toml`), and this crate already calls mev
//!    functions directly elsewhere (`crate::repo_registry`,
//!    `crate::brain_root`, `nodes::doc_materializer`) rather than shelling
//!    out via `CommandRunner`. `mev::brain::distill::{parse_distilled,
//!    distill_stale_age}` and `mev::brain::config::AttentionThresholds`
//!    are genuinely `pub fn`/`pub struct` (not `pub(crate)`), give the
//!    claim's **full** text (not an 80-char snippet), and are the exact
//!    predicate `/attention`'s own "Stale distilled knowledge" lane uses —
//!    so this loader's lane can never diverge from what the board shows.
//!
//! **Decision: option 2.** [`LoadClaimsNode`] calls mev's library
//! functions directly, through the injectable [`ClaimLaneFs`] seam below
//! (never raw `std::fs` — the `LoadTaskStateNode` precedent this task was
//! told not to copy). `mev::brain::distill::parse_distilled` does not
//! recover a claim's `source:` field (it only extracts `date:`/
//! `freshness:` off the same provenance line) — this module recovers it
//! with one extra, narrowly-scoped split of that exact line
//! ([`extract_source_field`]), mirroring `parse_distilled`'s own
//! `·`-delimited parsing rather than re-deriving the whole entry.
//!
//! # Discovery
//!
//! `LoadClaimsNode` resolves the brain root
//! ([`crate::brain_root::resolve_brain_root`]) and lists every registered
//! repo via [`crate::repo_registry::RepoRegistry`], then reads each repo's
//! `planning/knowledge.md` and `planning/memory.md` through [`ClaimLaneFs`],
//! applying the identical `distill_stale_age` staleness predicate the
//! Attention board uses. [`LoadClaimsNode::with_repo_roots`] overrides
//! discovery entirely with an explicit path list — the unit tests below use
//! this, paired with an in-memory [`ClaimLaneFs`] stub, so no real
//! filesystem or `brain.toml` is needed to exercise the parsing/staleness
//! logic. [`ClaimReaffirmInput::lane_source_override`] overrides discovery
//! at the *event* layer instead, for a caller that already knows the exact
//! claim set to re-run.
//!
//! An empty lane (no stale claims found, or a `lane_source_override` of
//! `Some(vec![])`) is not an error — [`ClaimReaffirmState`] with zero
//! claims is a valid, cheap outcome (the drain-router in task 2 exits
//! straight to the report).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::NaiveDate;
use engine_contract::TaskContext;
use sha2::{Digest, Sha256};

use crate::node::{Node, NodeError};
use crate::workflows::put_result;

use super::schema::{ClaimItem, ClaimReaffirmInput, ClaimReaffirmState, ClaimStatus};

/// The `Node::name()` identity `LoadClaimsNode` runs under by default, and
/// the `ctx.nodes` key its result (the initial [`ClaimReaffirmState`]) is
/// stamped onto.
pub const NODE_NAME: &str = "LoadClaimsNode";

/// The two D35-distilled file stems this loader scans per repo, matching
/// `mev::brain::emit::plan_attention_board`'s own `["knowledge", "memory"]`
/// scan exactly.
const DISTILL_STEMS: [&str; 2] = ["knowledge", "memory"];

/// Injectable file-read seam. The production path
/// ([`RealClaimLaneFs`]) reads real files; tests substitute an in-memory
/// stub so the parsing/staleness logic is exercised with zero real
/// filesystem access.
pub trait ClaimLaneFs: Send + Sync {
    /// Read `path`'s contents, or `None` when the file does not exist or
    /// cannot be read — mirrors `plan_attention_board`'s own
    /// `if let Ok(contents) = std::fs::read_to_string(&path)` tolerance (a
    /// repo with no `memory.md`, say, is not an error).
    fn read_to_string(&self, path: &Path) -> Option<String>;
}

/// The live [`ClaimLaneFs`] backed by real `std::fs` reads — the only
/// place in this module raw filesystem access happens, isolated behind the
/// seam rather than called inline from [`LoadClaimsNode::process`].
#[derive(Debug, Clone, Copy, Default)]
pub struct RealClaimLaneFs;

impl ClaimLaneFs for RealClaimLaneFs {
    fn read_to_string(&self, path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }
}

/// Reads mev's stale-claim lane (D35-distilled `knowledge.md`/`memory.md`
/// entries past their `freshness:` threshold, fleet-wide) and builds the
/// initial [`ClaimReaffirmState`].
pub struct LoadClaimsNode {
    fs: Arc<dyn ClaimLaneFs>,
    /// Explicit repo-root override. `None` (the default) resolves the
    /// brain root and every registered repo via
    /// [`crate::repo_registry::RepoRegistry`] at `process` time — the
    /// production path. `Some` bypasses both entirely; each path is
    /// treated as a repo root (its `planning/knowledge.md` /
    /// `planning/memory.md` are the two candidate files) — what the unit
    /// tests below use.
    repo_roots: Option<Vec<PathBuf>>,
    thresholds: mev::brain::config::AttentionThresholds,
    /// The "today" `distill_stale_age` ages entries against. `None` (the
    /// default) resolves `chrono::Utc::now().date_naive()` at `process`
    /// time; tests fix this so a fixture's staleness verdict is
    /// deterministic regardless of the day the suite happens to run.
    today: Option<NaiveDate>,
}

impl LoadClaimsNode {
    /// Construct with the live filesystem seam, fleet-wide discovery, the
    /// built-in `AttentionThresholds` default, and "today" resolved from
    /// the real clock at `process` time — the behavior-stable production
    /// default.
    #[must_use]
    pub fn new() -> Self {
        Self {
            fs: Arc::new(RealClaimLaneFs),
            repo_roots: None,
            thresholds: mev::brain::config::AttentionThresholds::default(),
            today: None,
        }
    }

    /// Override the [`ClaimLaneFs`] seam. Tests inject an in-memory stub so
    /// the suite never touches a real filesystem.
    #[must_use]
    pub fn with_fs(mut self, fs: Arc<dyn ClaimLaneFs>) -> Self {
        self.fs = fs;
        self
    }

    /// Override repo-root discovery with an explicit path list, bypassing
    /// [`crate::brain_root::resolve_brain_root`]/[`crate::repo_registry::RepoRegistry`]
    /// entirely.
    #[must_use]
    pub fn with_repo_roots(mut self, roots: Vec<PathBuf>) -> Self {
        self.repo_roots = Some(roots);
        self
    }

    /// Override the staleness thresholds `distill_stale_age` ages entries
    /// against. Defaults to `AttentionThresholds::default()`.
    #[must_use]
    pub fn with_thresholds(mut self, thresholds: mev::brain::config::AttentionThresholds) -> Self {
        self.thresholds = thresholds;
        self
    }

    /// Fix "today" for deterministic staleness verdicts in tests.
    #[must_use]
    pub fn with_today(mut self, today: NaiveDate) -> Self {
        self.today = Some(today);
        self
    }

    /// Resolve the set of repo roots to scan: [`Self::repo_roots`] when
    /// set, else every repo [`crate::repo_registry::RepoRegistry`]
    /// currently resolves from `brain.toml`.
    fn resolve_repo_roots(&self) -> Result<Vec<PathBuf>, NodeError> {
        if let Some(roots) = &self.repo_roots {
            return Ok(roots.clone());
        }
        let registry = crate::repo_registry::RepoRegistry::from_env()
            .map_err(|err| NodeError::new(format!("{NODE_NAME}: {err}")))?;
        let mut roots = Vec::new();
        for slug in registry.known_slugs() {
            if let Ok(root) = registry.resolve(&slug) {
                roots.push(root);
            }
        }
        Ok(roots)
    }

    /// Scan one repo root's `knowledge.md`/`memory.md` for stale D35
    /// entries, converting each into a [`ClaimItem`].
    fn scan_repo(&self, repo_root: &Path, today: NaiveDate) -> Vec<ClaimItem> {
        let mut items = Vec::new();
        for stem in DISTILL_STEMS {
            let path = repo_root.join("planning").join(format!("{stem}.md"));
            let Some(contents) = self.fs.read_to_string(&path) else {
                continue;
            };
            for entry in mev::brain::distill::parse_distilled(&contents) {
                if mev::brain::distill::distill_stale_age(&entry, today, &self.thresholds, stem)
                    .is_none()
                {
                    continue;
                }
                let source_doc_id =
                    extract_source_field(&contents, entry.line).unwrap_or_else(|| {
                        // No `source:` path recovered from this entry's
                        // provenance line (e.g. `- source:` amistad-scope
                        // variant without a leading path token, or a
                        // malformed line) — fall back to the file the claim
                        // itself lives in, so `source_doc_id` is never
                        // empty.
                        path.to_string_lossy().to_string()
                    });
                items.push(ClaimItem {
                    id: claim_id(&source_doc_id, stem, entry.line),
                    source_doc_id,
                    claim_text: entry.claim.clone(),
                    freshness_date: entry.freshness.map(|d| d.to_string()),
                    status: ClaimStatus::Pending,
                    attempt: 0,
                    verdict: None,
                });
            }
        }
        items
    }
}

impl Default for LoadClaimsNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for LoadClaimsNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let input: ClaimReaffirmInput = serde_json::from_value(ctx.event.clone())
            .map_err(|err| NodeError::new(format!("invalid CLAIM_REAFFIRM event: {err}")))?;

        let claims = if let Some(override_claims) = input.lane_source_override {
            override_claims
        } else {
            let today = self
                .today
                .unwrap_or_else(|| chrono::Utc::now().date_naive());
            let repo_roots = self.resolve_repo_roots()?;
            let mut claims = Vec::new();
            for repo_root in &repo_roots {
                claims.extend(self.scan_repo(repo_root, today));
            }
            claims
        };

        let state = ClaimReaffirmState { claims };
        let mut ctx = ctx;
        put_result(
            &mut ctx,
            self.name(),
            serde_json::to_value(&state).map_err(|err| {
                NodeError::new(format!("{NODE_NAME}: failed to serialize state: {err}"))
            })?,
        );
        Ok(ctx)
    }

    fn name(&self) -> &str {
        NODE_NAME
    }
}

/// Recover the `source:` field off a D35 entry's provenance line — the
/// same `·`-delimited line `mev::brain::distill::parse_distilled` already
/// parsed for `date:`/`freshness:`, at the 1-indexed line number it
/// recorded (`DistilledEntry::line`). `parse_distilled` itself discards
/// this field (it only needed `date:`/`freshness:`); this helper re-splits
/// the identical line rather than re-deriving the whole entry, so it stays
/// in lockstep with mev's own parsing convention (`- source:` and
/// `source:` prefixes, `·`-delimited segments) without depending on any
/// `pub(crate)` mev internals.
///
/// Returns `None` when the line has no `source:`/`- source:` segment, or
/// when `line` is out of range.
fn extract_source_field(contents: &str, line: usize) -> Option<String> {
    let raw_line = contents.lines().nth(line.checked_sub(1)?)?;
    for part in raw_line.split('·') {
        let part = part.trim();
        let Some(value) = part
            .strip_prefix("source:")
            .or_else(|| part.strip_prefix("- source:"))
        else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            return None;
        }
        return Some(value.to_string());
    }
    None
}

/// Derive a stable claim `id` from its identity fields
/// (`source_doc_id`/`stem`/`line`) — never from mutable content (the claim
/// text itself), mirroring `mev::brain::attention_payload::item_id_for`'s
/// own identity-not-content discipline. A re-trigger against an unchanged
/// corpus reproduces the same id for the same claim, which is what task
/// 2's "a re-trigger skips already-judged claims" acceptance criterion
/// needs a stable key for.
fn claim_id(source_doc_id: &str, stem: &str, line: usize) -> String {
    let mut hasher = Sha256::new();
    hasher.update(source_doc_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(stem.as_bytes());
    hasher.update(b"\0");
    hasher.update(line.to_le_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use engine_contract::TaskContext;

    use super::*;

    /// An in-memory [`ClaimLaneFs`] stub — no real filesystem access at
    /// all. Built from a fixed `path -> contents` map.
    struct FakeClaimLaneFs {
        files: HashMap<PathBuf, String>,
    }

    impl FakeClaimLaneFs {
        fn new(files: Vec<(&str, &str)>) -> Self {
            Self {
                files: files
                    .into_iter()
                    .map(|(p, c)| (PathBuf::from(p), c.to_string()))
                    .collect(),
            }
        }
    }

    impl ClaimLaneFs for FakeClaimLaneFs {
        fn read_to_string(&self, path: &Path) -> Option<String> {
            self.files.get(path).cloned()
        }
    }

    fn empty_ctx() -> TaskContext {
        TaskContext {
            event: serde_json::json!({}),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        }
    }

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, 3).expect("valid date")
    }

    /// A stale `knowledge.md` fixture with 3 entries: two stale entries
    /// (mev's `parse_distilled` only recognizes a provenance line starting
    /// with `source:`/`- source:`, so `source_doc_id` is always
    /// recoverable for anything mev itself parses as an entry at all —
    /// [`extract_source_field_none_when_absent_or_out_of_range`] and
    /// friends separately cover the helper's own defensive `None` paths),
    /// each citing a different source document, and one fresh entry (not
    /// yet past threshold — must be excluded).
    fn fixture_knowledge_md() -> &'static str {
        "\
- **Claim A is durably true.**
  source: planning/status.md · date: 2025-01-01 · supersedes: — · freshness: 2025-01-01

- **Claim B cites a different source document.**
  source: planning/context.md · date: 2025-01-01 · supersedes: — · freshness: 2025-01-01

- **Claim C was just reaffirmed and is still fresh.**
  source: planning/status.md · date: 2025-01-01 · supersedes: — · freshness: 2026-09-01
"
    }

    #[tokio::test]
    async fn loads_stale_claims_from_a_fixture_lane_with_source_field_recovered() {
        let fs = FakeClaimLaneFs::new(vec![(
            "/repo/planning/knowledge.md",
            fixture_knowledge_md(),
        )]);
        let node = LoadClaimsNode::new()
            .with_fs(Arc::new(fs))
            .with_repo_roots(vec![PathBuf::from("/repo")])
            .with_today(today());

        let ctx = node.process(empty_ctx()).await.expect("process succeeds");
        let result = ctx.nodes.get(NODE_NAME).expect("result stamped");
        let state: ClaimReaffirmState =
            serde_json::from_value(result.clone()).expect("valid ClaimReaffirmState");

        // Claim C (fresh) is excluded; A and B (stale) are included.
        assert_eq!(state.claims.len(), 2);

        let claim_a = state
            .claims
            .iter()
            .find(|c| c.claim_text.contains("Claim A"))
            .expect("claim A present");
        assert_eq!(claim_a.source_doc_id, "planning/status.md");
        assert_eq!(claim_a.status, ClaimStatus::Pending);
        assert_eq!(claim_a.attempt, 0);
        assert!(claim_a.verdict.is_none());
        assert_eq!(claim_a.freshness_date.as_deref(), Some("2025-01-01"));

        let claim_b = state
            .claims
            .iter()
            .find(|c| c.claim_text.contains("Claim B"))
            .expect("claim B present");
        assert_eq!(claim_b.source_doc_id, "planning/context.md");
        assert_ne!(
            claim_a.id, claim_b.id,
            "distinct entries get distinct stable ids"
        );
    }

    #[tokio::test]
    async fn empty_lane_yields_zero_claims_not_an_error() {
        let fs = FakeClaimLaneFs::new(vec![]);
        let node = LoadClaimsNode::new()
            .with_fs(Arc::new(fs))
            .with_repo_roots(vec![PathBuf::from("/repo")])
            .with_today(today());

        let ctx = node.process(empty_ctx()).await.expect("process succeeds");
        let result = ctx.nodes.get(NODE_NAME).expect("result stamped");
        let state: ClaimReaffirmState =
            serde_json::from_value(result.clone()).expect("valid ClaimReaffirmState");
        assert!(state.claims.is_empty());
    }

    #[tokio::test]
    async fn lane_source_override_bypasses_discovery_entirely() {
        // repo_roots deliberately points somewhere with no fixture data —
        // if the override did not take effect, this would yield 0 claims.
        let fs = FakeClaimLaneFs::new(vec![]);
        let node = LoadClaimsNode::new()
            .with_fs(Arc::new(fs))
            .with_repo_roots(vec![PathBuf::from("/nowhere")])
            .with_today(today());

        let override_claim = ClaimItem {
            id: "explicit-1".to_string(),
            source_doc_id: "repo/planning/knowledge.md".to_string(),
            claim_text: "an explicitly supplied claim".to_string(),
            freshness_date: None,
            status: ClaimStatus::Pending,
            attempt: 0,
            verdict: None,
        };
        let mut ctx = empty_ctx();
        ctx.event = serde_json::json!({
            "lane_source_override": [override_claim],
        });

        let ctx = node.process(ctx).await.expect("process succeeds");
        let result = ctx.nodes.get(NODE_NAME).expect("result stamped");
        let state: ClaimReaffirmState =
            serde_json::from_value(result.clone()).expect("valid ClaimReaffirmState");
        assert_eq!(state.claims.len(), 1);
        assert_eq!(state.claims[0].id, "explicit-1");
    }

    #[tokio::test]
    async fn no_evidence_designed_claim_still_loads_with_recoverable_source() {
        // The spec's own AC calls for "one designed to have no corpus
        // evidence" in the fixture lane — that is a `RecallNode`/judge
        // concern (task 2), not this loader's, but the loader must still
        // load such a claim with a usable `source_doc_id` so task 2's
        // `RecallNode` query has something to anchor on.
        let contents = "\
- **A claim about a document that no longer exists.**
  source: planning/deleted-doc.md · date: 2025-01-01 · supersedes: — · freshness: 2025-01-01
";
        let fs = FakeClaimLaneFs::new(vec![("/repo/planning/memory.md", contents)]);
        let node = LoadClaimsNode::new()
            .with_fs(Arc::new(fs))
            .with_repo_roots(vec![PathBuf::from("/repo")])
            .with_today(today());

        let ctx = node.process(empty_ctx()).await.expect("process succeeds");
        let result = ctx.nodes.get(NODE_NAME).expect("result stamped");
        let state: ClaimReaffirmState =
            serde_json::from_value(result.clone()).expect("valid ClaimReaffirmState");
        assert_eq!(state.claims.len(), 1);
        assert_eq!(state.claims[0].source_doc_id, "planning/deleted-doc.md");
    }

    #[test]
    fn extract_source_field_recovers_plain_and_amistad_variants() {
        let plain = "  source: planning/x.md · date: 2025-01-01 · freshness: 2025-01-01";
        let contents = format!("- **claim**\n{plain}\n");
        assert_eq!(
            extract_source_field(&contents, 2),
            Some("planning/x.md".to_string())
        );

        let amistad = "  - source: planning/y.md · date: 2025-01-01 · freshness: 2025-01-01";
        let contents = format!("- **claim**\n{amistad}\n");
        assert_eq!(
            extract_source_field(&contents, 2),
            Some("planning/y.md".to_string())
        );
    }

    #[test]
    fn extract_source_field_finds_source_when_not_the_first_segment() {
        // Regression guard: the loop over `·`-delimited segments must
        // actually scan every segment, not just the first.
        let contents =
            "- **claim**\n  date: 2025-01-01 · source: planning/z.md · freshness: 2025-01-01\n";
        assert_eq!(
            extract_source_field(contents, 2),
            Some("planning/z.md".to_string())
        );
    }

    #[test]
    fn extract_source_field_none_when_absent_or_out_of_range() {
        let contents = "- **claim**\n  date: 2025-01-01 · freshness: 2025-01-01\n";
        assert_eq!(extract_source_field(contents, 2), None);
        assert_eq!(extract_source_field(contents, 999), None);
        assert_eq!(extract_source_field(contents, 0), None);
    }

    #[test]
    fn claim_id_is_stable_across_calls_and_distinct_across_lines() {
        let id_a = claim_id("repo/planning/knowledge.md", "knowledge", 3);
        let id_a_again = claim_id("repo/planning/knowledge.md", "knowledge", 3);
        let id_b = claim_id("repo/planning/knowledge.md", "knowledge", 10);
        assert_eq!(id_a, id_a_again, "same identity fields -> same id");
        assert_ne!(id_a, id_b, "different line -> different id");
    }

    /// Guards the [`ClaimLaneFs`] seam itself: [`RealClaimLaneFs`] reads a
    /// real file it does not know exists and returns `None`, never a
    /// panic — the not-found tolerance `plan_attention_board` relies on
    /// for repos with no `memory.md`.
    #[test]
    fn real_claim_lane_fs_returns_none_for_a_missing_path() {
        let fs = RealClaimLaneFs;
        assert_eq!(
            fs.read_to_string(Path::new("/definitely/does/not/exist/knowledge.md")),
            None
        );
    }

    /// One process-global mutex guarding tests that touch
    /// `ENGINE_REPO_ALLOWLIST`/`ENGINE_BRAIN_ROOT` indirectly via
    /// `resolve_repo_roots`'s fallback path — this suite otherwise never
    /// exercises that path (every test above sets `with_repo_roots`), so
    /// this guard exists only so a future test added here inherits the
    /// same discipline `brain_root.rs`'s own tests document.
    static ENV_GUARD: Mutex<()> = Mutex::new(());

    #[test]
    fn resolve_repo_roots_uses_explicit_override_when_set() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let node =
            LoadClaimsNode::new().with_repo_roots(vec![PathBuf::from("/a"), PathBuf::from("/b")]);
        let roots = node.resolve_repo_roots().expect("override never errors");
        assert_eq!(roots, vec![PathBuf::from("/a"), PathBuf::from("/b")]);
    }
}
