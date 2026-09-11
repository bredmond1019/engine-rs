//! `CONSOLIDATE` workflow assembly — `EN.15.K` Task 6.
//!
//! Declared graph shape (single node, both start and terminal, no router — mirrors
//! `harvest_approve::graph`'s / `terminal_probe::graph`'s micro-workflow shape):
//!
//! ```text
//! ConsolidateRunNode
//! ```
//!
//! **No policy module, no profiles module, no `harness.json` section.** `ConsolidateRunNode`
//! calls no model — it drives the pure, already-tested Tasks 1-5 functions
//! (`discover::discover_participants`, `select::select_ledger_rows`/`since_filter`,
//! `disposal::selected_rows_to_disposal`/`write_disposal`, `remediation::promote_remediation`,
//! `watermark::advance_watermark`) over an event naming a brain root and a roadmap slug, in the
//! fixed order the module doc of [`super`] already declares: discover -> select -> disposal
//! write -> remediation promote (only for failing rows) -> watermark advance. There is no
//! `ModelTier` to resolve and nothing for a policy layer to override, so `engine-serve`'s
//! registration function for this workflow resolves no policy and seeds no policy stamp,
//! matching `register_harvest_approve`/`register_recall`/`register_terminal_probe`.
//!
//! # Gathering the full candidate row set (D57 §3)
//!
//! [`select::select_ledger_rows`]'s own doc notes that a row *adopted* from `roadmap_slug` into a
//! different driving lane's record (D57's own worked example: `close-the-loop` carrying two
//! `carryover-improvements` blocks) lives in a record discovered under *that other lane's own*
//! roadmap directory — so this node does not stop at `discover_participants(root, roadmap_slug)`
//! (which only walks `orchestration-run/<roadmap_slug>/` records). It also lists every other
//! roadmap directory the corpus has ([`list_roadmap_slugs`]) and runs `discover_run_records` +
//! `select_ledger_rows` against each, unioning the results — the "gathering the full candidate
//! set across every roadmap directory" this module's own doc comment assigns to `graph.rs`.
//!
//! `--since` scoping ([`select::since_filter`]) is applied only to a row whose *own* record is
//! native to `roadmap_slug` (i.e. its `record_roadmap` equals the target): an adopted row's
//! inclusion is governed by the driving lane's own activity, not by `roadmap_slug`'s log, which
//! may carry no lines for that lane's repo at all.
//!
//! # Injectable seams, sane defaults (CLAUDE.md standing rule 6)
//!
//! [`ConsolidateRunNode::new`] defaults every stage seam to the real, already-tested Tasks 1-5
//! function — `discover_participants`, `promote_remediation`, `advance_watermark`, and a real UTC
//! clock for the disposal file's `generated` timestamp — so an un-injected run behaves exactly as
//! calling those functions directly would. Each is overridable via a `with_*` builder, mirroring
//! `orchestration::graph::OrchestrationRunNode::new`'s established convention, so a test can stub
//! (or skip) the one stage that shells out to Python (`promote_remediation`) without touching the
//! others.
//!
//! Extraction and mechanism naming stay `ClaudeCodeStep`s per this block's own `out_of_scope` —
//! they are genuinely LLM work and are never part of this declared graph.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use engine_contract::TaskContext;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::node::{Node, NodeError, NodeRegistry};
use crate::roadmap_status::{discover_run_records, read_lane_log, resolve_roadmap_dir};
use crate::schema::{NodeConfig, WorkflowSchema};
use crate::workflow::Workflow;
use crate::workflows::orchestration::ledger::{Coverage, LedgerEntry, LedgerStatus, Remediation};
use crate::workflows::put_result;

use super::discover::{discover_participants, DiscoveryResult};
use super::disposal::{selected_rows_to_disposal, write_disposal, DisposalError};
use super::remediation::{promote_remediation, PromoteError, PromoteOutcome};
use super::select::{select_ledger_rows, since_filter, SelectedRow};
use super::watermark::{advance_watermark, AdvanceOutcome, WatermarkError};

/// `CONSOLIDATE`'s registered workflow type string.
pub const WORKFLOW_TYPE: &str = "CONSOLIDATE";

/// The identity `CONSOLIDATE`'s single node is registered under.
pub const NODE_NAME: &str = "ConsolidateRunNode";

/// Build the declared `WorkflowSchema` for `CONSOLIDATE`: a single node, both start and
/// terminal, with no forward connection.
#[must_use]
pub fn schema() -> WorkflowSchema {
    let mut nodes = HashMap::new();
    nodes.insert(NODE_NAME.to_string(), NodeConfig::new(NODE_NAME, vec![]));
    WorkflowSchema::new(WORKFLOW_TYPE, NODE_NAME, nodes)
}

/// Build a fresh `NodeRegistry` for `CONSOLIDATE`: one [`ConsolidateRunNode`], registered under
/// [`NODE_NAME`] (its default `Node::name()` identity).
#[must_use]
pub fn registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(ConsolidateRunNode::new()));
    registry
}

/// Build the runnable `CONSOLIDATE` `Workflow`: [`registry`] paired with [`schema`], constructed
/// via `Workflow::new_validated` so assembly fails loudly if the declared graph is not
/// structurally sound.
///
/// # Panics
/// Panics if the declared graph fails `WorkflowValidator::validate` — this would be a
/// programming error in this module, not a runtime condition callers should recover from.
#[must_use]
pub fn workflow() -> Workflow {
    Workflow::new_validated(registry(), schema())
        .expect("CONSOLIDATE declared graph must pass WorkflowValidator::validate")
}

// ── Injectable seam types ───────────────────────────────────────────────

type DiscoverFn = Arc<dyn Fn(&Path, &str) -> DiscoveryResult + Send + Sync>;
type PromoteFn =
    Arc<dyn Fn(&Path, &LedgerEntry) -> Result<PromoteOutcome, PromoteError> + Send + Sync>;
type AdvanceFn =
    Arc<dyn Fn(&Path, &str, usize, &str) -> Result<AdvanceOutcome, WatermarkError> + Send + Sync>;
type ClockFn = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// List every roadmap slug the corpus under `root` has a directory for — both
/// `planning/roadmaps/<slug>/` and the legacy `planning/<slug>/` location (mirroring
/// [`resolve_roadmap_dir`]'s own two-location rule) — identified by the presence of a
/// `lane-log.jsonl` sibling, so a directory that happens to share a name with something else
/// under `planning/` is never mistaken for a roadmap. Order is alphabetical (`BTreeSet`) for
/// determinism; a missing `planning/` tree yields an empty list, never an error.
fn list_roadmap_slugs(root: &Path) -> Vec<String> {
    let mut slugs = BTreeSet::new();
    for base in ["planning/roadmaps", "planning"] {
        let dir = root.join(base);
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && path.join("lane-log.jsonl").is_file() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    slugs.insert(name.to_string());
                }
            }
        }
    }
    slugs.into_iter().collect()
}

/// Parse a wire-format `status` string the same way `docs/sandbox/run-verification-ledger-prompt.md`
/// fixes it — the exact inverse of [`LedgerStatus::as_wire_str`]. An unrecognized string yields
/// `None` rather than a default, since a row this module cannot even classify must never be
/// silently treated as non-failing.
fn parse_ledger_status(s: &str) -> Option<LedgerStatus> {
    Some(match s {
        "untested" => LedgerStatus::Untested,
        "tested" => LedgerStatus::Tested,
        "partial" => LedgerStatus::Partial,
        "failed" => LedgerStatus::Failed,
        "blocked" => LedgerStatus::Blocked,
        "not_applicable" => LedgerStatus::NotApplicable,
        _ => return None,
    })
}

fn parse_coverage(s: Option<&str>) -> Coverage {
    match s {
        Some("covered") => Coverage::Covered,
        Some("partial") => Coverage::Partial,
        _ => Coverage::Uncovered,
    }
}

/// Build a [`LedgerEntry`] from a [`SelectedRow`]'s raw ledger-entry JSON, for the sole purpose
/// of promoting it through [`promote_remediation`] — `None` when the row's `status` is absent,
/// unrecognized, or does not accept a remediation, or when it carries no `remediation` object at
/// all (the overwhelming majority of rows select for disposal but were never failing). This is a
/// READ of an already-persisted, already-validated `verification-ledger.json` entry (Task 2's
/// own `EN.15.L` writer enforces `LedgerEntry`'s construction rules before ever writing one to
/// disk), so it builds the struct directly from its already-`pub` fields rather than
/// re-running `LedgerEntry::new`'s validation a second time here.
fn ledger_entry_for_remediation(row: &Value) -> Option<LedgerEntry> {
    let get_str = |key: &str| row.get(key).and_then(Value::as_str);

    let status = parse_ledger_status(get_str("status")?)?;
    if !status.accepts_remediation() {
        return None;
    }
    let remediation_value = row.get("remediation")?;
    let remediation = Remediation {
        block: remediation_value
            .get("block")
            .and_then(Value::as_str)?
            .to_string(),
        opened_at: remediation_value
            .get("opened_at")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        note: remediation_value
            .get("note")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    };

    let id = get_str("id")?.to_string();
    let block = get_str("block").unwrap_or_default().to_string();
    let capability = get_str("capability").unwrap_or_default().to_string();
    let env = get_str("env").unwrap_or_default().to_string();
    let how_to_verify = get_str("how_to_verify").unwrap_or_default().to_string();
    let call_site = get_str("call_site").unwrap_or_default().to_string();
    let evidence = get_str("evidence").unwrap_or_default().to_string();
    let coverage = parse_coverage(get_str("coverage"));
    let covered_by = row
        .get("covered_by")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    Some(LedgerEntry {
        id,
        block,
        capability,
        status,
        env,
        how_to_verify,
        call_site,
        evidence,
        coverage,
        covered_by,
        cross_repo: None,
        remediation: Some(remediation),
    })
}

fn promote_outcome_json(outcome: &PromoteOutcome) -> Value {
    match outcome {
        PromoteOutcome::AlreadyPromoted { rem_id } => json!({
            "already_promoted": true,
            "rem_id": rem_id,
        }),
        PromoteOutcome::Promoted {
            rem_id,
            finding,
            proposed_state_edit,
        } => json!({
            "already_promoted": false,
            "rem_id": rem_id,
            "finding": finding,
            "proposed_state_edit": {
                "repo": proposed_state_edit.repo,
                "block": proposed_state_edit.block,
                "origin_type": proposed_state_edit.origin_type,
                "slug": proposed_state_edit.slug,
            },
        }),
    }
}

/// The sole node in the `CONSOLIDATE` graph — see this module's doc comment for the full pipeline
/// shape and the seam defaults below.
pub struct ConsolidateRunNode {
    discover: DiscoverFn,
    promote: PromoteFn,
    advance: AdvanceFn,
    now: ClockFn,
}

impl ConsolidateRunNode {
    /// Every seam defaults to the real, already-tested Tasks 1-5 function — a behavior-stable
    /// default per CLAUDE.md standing rule 6.
    #[must_use]
    pub fn new() -> Self {
        Self {
            discover: Arc::new(discover_participants),
            promote: Arc::new(promote_remediation),
            advance: Arc::new(advance_watermark),
            now: Arc::new(Utc::now),
        }
    }

    /// Override the discovery seam. Tests use this to point at a fixture without needing a real
    /// `lane-log.jsonl` + run-record filesystem shape.
    #[must_use]
    pub fn with_discover(mut self, f: DiscoverFn) -> Self {
        self.discover = f;
        self
    }

    /// Override the remediation-promotion seam — the one stage that shells out to `python3`.
    /// Tests that want to exercise the graph's ordering without paying for that (or without a
    /// sibling HQ tree to copy the real scripts from) stub this; the "remediation promotion +
    /// idempotency" acceptance test wires the real [`promote_remediation`] instead, exactly the
    /// default this builder overrides.
    #[must_use]
    pub fn with_promote(mut self, f: PromoteFn) -> Self {
        self.promote = f;
        self
    }

    /// Override the watermark-advance seam.
    #[must_use]
    pub fn with_advance(mut self, f: AdvanceFn) -> Self {
        self.advance = f;
        self
    }

    /// Override the clock this node reads "now" through for `disposal.json`'s `generated` field.
    #[must_use]
    pub fn with_now(mut self, f: ClockFn) -> Self {
        self.now = f;
        self
    }

    fn required_str<'a>(&self, event: &'a Value, field: &str) -> Result<&'a str, NodeError> {
        event.get(field).and_then(Value::as_str).ok_or_else(|| {
            NodeError::new(format!(
                "{NODE_NAME}: CONSOLIDATE event missing required field `{field}`"
            ))
        })
    }
}

impl Default for ConsolidateRunNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for ConsolidateRunNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let event = ctx.event.clone();

        let brain_root = PathBuf::from(self.required_str(&event, "brain_root")?);
        let roadmap_slug = self.required_str(&event, "roadmap_slug")?.to_string();
        let hq_root = event
            .get("hq_root")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .unwrap_or_else(|| brain_root.clone());
        let since = event
            .get("since")
            .and_then(Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc));
        let run_id = event
            .get("run_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                ctx.metadata
                    .get("run_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| Uuid::new_v4().to_string());

        // 1. DISCOVER — this roadmap's own participants + run records.
        let discovery = (self.discover)(&brain_root, &roadmap_slug);
        let roadmap_dir = resolve_roadmap_dir(&brain_root, &roadmap_slug).ok();

        let (included_participants, excluded_participants) = match &roadmap_dir {
            Some(dir) => since_filter(dir, &discovery.participants, since),
            None => (discovery.participants.clone(), Vec::new()),
        };
        let included: BTreeSet<&String> = included_participants.iter().collect();

        // 2. SELECT — this roadmap's own native rows, plus every adopted row living in another
        // roadmap directory's records (D57 §3; see this module's doc comment).
        let mut selected: Vec<SelectedRow> = select_ledger_rows(&discovery.records, &roadmap_slug);
        for other_slug in list_roadmap_slugs(&brain_root) {
            if other_slug == roadmap_slug {
                continue;
            }
            let other_records = discover_run_records(&brain_root, &other_slug);
            selected.extend(select_ledger_rows(&other_records, &roadmap_slug));
        }
        // `--since` scoping only ever governs a row NATIVE to `roadmap_slug`'s own log — an
        // adopted row's inclusion is the driving lane's own business, not this roadmap's.
        selected.retain(|row| row.record_roadmap != roadmap_slug || included.contains(&row.repo));

        // 3. DISPOSAL — write disposal.json for the first time.
        let disposal_rows =
            selected_rows_to_disposal(&selected).map_err(|err: DisposalError| {
                NodeError::new(format!("{NODE_NAME}: building disposal rows: {err}"))
            })?;
        let disposal_path = event
            .get("disposal_path")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .unwrap_or_else(|| match &roadmap_dir {
                Some(dir) => dir.join("disposal.json"),
                None => brain_root
                    .join("planning/open-work/orchestration-runs")
                    .join(format!("disposal-{roadmap_slug}.json")),
            });
        let analysis = event
            .get("analysis")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("CONSOLIDATE run for {roadmap_slug}"));
        let generated = (self.now)().to_rfc3339();
        let backfilled = event
            .get("backfilled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let ungrounded_excludes: Vec<String> = event
            .get("ungrounded_excludes")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        write_disposal(
            &disposal_path,
            &analysis,
            &generated,
            std::slice::from_ref(&roadmap_slug),
            backfilled,
            disposal_rows.clone(),
            &ungrounded_excludes,
        )
        .map_err(|err| NodeError::new(format!("{NODE_NAME}: writing disposal.json: {err}")))?;

        // 4. REMEDIATION PROMOTE — only for a row whose raw ledger entry is failing/blocked and
        // carries a `remediation` object. Every other row (the overwhelming majority) is skipped
        // silently — this is not an error path, most disposal rows were never a failing test.
        let mut promotions = Vec::new();
        for row in &selected {
            let Some(entry) = ledger_entry_for_remediation(&row.row) else {
                continue;
            };
            let outcome = (self.promote)(&hq_root, &entry).map_err(|err| {
                NodeError::new(format!(
                    "{NODE_NAME}: promoting remediation for ledger entry {}: {err}",
                    entry.id
                ))
            })?;
            promotions.push(promote_outcome_json(&outcome));
        }

        // 5. WATERMARK ADVANCE — this roadmap's own log, to its current end (or an explicit
        // `watermark_to_line` override). Skipped entirely when the roadmap directory does not
        // resolve — nothing to advance a cursor over.
        let watermark_outcome = if let Some(dir) = &roadmap_dir {
            let (entries, malformed) = read_lane_log(dir);
            let to_line = event
                .get("watermark_to_line")
                .and_then(Value::as_u64)
                .map(|n| n as usize)
                .unwrap_or(entries.len() + malformed.len());
            let outcome =
                (self.advance)(&brain_root, &roadmap_slug, to_line, &run_id).map_err(|err| {
                    NodeError::new(format!("{NODE_NAME}: advancing watermark: {err}"))
                })?;
            Some(json!({
                "roadmap": outcome.roadmap,
                "advanced_from": outcome.advanced_from,
                "advanced_to": outcome.advanced_to,
                "consumed": outcome.consumed,
                "malformed_lines": outcome.malformed_lines,
            }))
        } else {
            None
        };

        put_result(
            &mut ctx,
            self.name(),
            json!({
                "roadmap": roadmap_slug,
                "run_id": run_id,
                "participants": discovery.participants,
                "included_participants": included_participants,
                "excluded_participants": excluded_participants,
                "discovery_findings": discovery.findings,
                "selected_row_count": selected.len(),
                "disposal_path": disposal_path,
                "disposal_row_count": disposal_rows.len(),
                "promotions": promotions,
                "watermark": watermark_outcome,
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
    use super::*;
    use crate::validate::WorkflowValidator;

    #[test]
    fn schema_passes_validation() {
        let schema = schema();
        let registry = registry();
        WorkflowValidator::validate(&registry, &schema).expect("declared graph should validate");
    }

    #[test]
    fn workflow_type_matches_schema() {
        assert_eq!(schema().workflow_type, WORKFLOW_TYPE);
    }

    #[test]
    fn schema_declares_exactly_one_node_that_is_both_start_and_terminal() {
        let schema = schema();
        assert_eq!(schema.nodes.len(), 1);
        let config = schema
            .nodes
            .get(&schema.start_node)
            .expect("start node declared");
        assert!(config.connections.is_empty());
    }

    #[test]
    fn registry_contains_exactly_one_node() {
        let registry = registry();
        assert!(registry.contains(NODE_NAME));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn workflow_builds_without_panicking() {
        let _workflow = workflow();
    }
}
