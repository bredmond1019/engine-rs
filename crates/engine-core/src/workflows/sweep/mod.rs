//! `SWEEP` — the roadmap sweep as an engine workflow (`EN.15.E`).
//!
//! Mirrors `roadmap_sweep.py` (HQ root, 908 lines) field-for-field and decision-for-decision
//! — see that script's own module doc comment ("WHY THIS EXISTS" / "PIPELINE" / "THE STALENESS
//! RULE" / "RE-FIRE THRESHOLD") for the design this ports. **THE PYTHON STAYS THE ORACLE**
//! (Fork 2, `EN.15.E`'s block record `notes`): this module reproduces its diff and routing
//! decisions but never replaces, edits, or schedules it (Fork 4 — `planning/harness.json`'s
//! `schedule.entries` stays empty).
//!
//! Submodules, one per pipeline stage — the same section breaks the Python script itself uses:
//! - [`snapshot`] — `build_raw_snapshot` + `semantic_projection` (task 2)
//! - [`diff`] — `diff_snapshots` / `build_dedup_history` (task 3)
//! - [`route`] — `route_escalation` / `route_non_escalation_diff` + the three `GatedAction`
//!   integration (task 4, this task)
//!
//! `crates/engine-serve/src/workflows.rs` registers SWEEP as a dispatchable workflow (task 5,
//! this task): [`schema`]/[`registry`] assemble the declared single-node graph — [`SweepNode`]
//! is both start and terminal, mirroring `recall::graph`'s / `harvest_approve::graph`'s
//! micro-workflow shape — and [`run_sweep_pass`] is the composed pipeline itself,
//! `roadmap_sweep.py`'s `run_sweep` (:789) ported field-for-field: build a fresh raw snapshot,
//! diff it against the most recent snapshot already on disk (`prev = None` on a first sweep is
//! not an error), route every new escalation and any standing escalation past its re-fire
//! threshold, route a bare non-escalation drift at most once when nothing else routed, and write
//! the composed `{raw snapshot fields} + diff + routed` document to `sweeps/<ts>.json` — the
//! exact shape `roadmap_sweep.py`'s own `load_snapshot` reads back.
//!
//! **FORK 4 IS A HARD BOUNDARY, restated here because this is the task that makes SWEEP
//! drivable:** registering the workflow makes it dispatchable via
//! `Dispatcher::dispatch_with_event`; it does NOT schedule it anywhere.
//! `planning/harness.json`'s `schedule.entries` stays `[]`, `roadmap_sweep_cron.sh` stays
//! uninstalled, and `scripts/roadmap_sweep.py` is untouched and stays the oracle.
//!
//! **The two injectable seams have no production implementation in this crate yet** — matching
//! [`route`]'s own module doc ("A production impl over `crate::coord::write::send` is a later
//! task's concern"). [`NoopOperatorTransport`] and [`NoopLaneWake`] are this task's honest
//! placeholders: every route they touch is still recorded faithfully in the written snapshot
//! (`routed: false`, a `reason` naming the placeholder), never silently dropped — the same
//! "recorded, never dropped" discipline `suppressed_by_profile` uses for a profile-denied route.
//! [`registry_with`] is the explicit-seam entry point a later task (or a test) uses to inject a
//! real transport/waker once one exists; [`registry`] is the placeholder-backed default
//! `engine-serve`'s bare `register_sweep` uses.

pub mod diff;
pub mod route;
pub mod snapshot;

use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use engine_contract::TaskContext;
use serde_json::Value;

use crate::node::{Node, NodeError, NodeRegistry};
use crate::operator::transport::{
    DeliveredMessage, NotifyError, OperatorResponse, OperatorTransport, ResponseVerdict,
    UpdateCursor,
};
use crate::operator::ValidatedOperatorPayload;
use crate::policy::permission::PermissionProfile;
use crate::roadmap_status::RoadmapStatusError;
use crate::schema::{NodeConfig, WorkflowSchema};
use crate::workflow::Workflow;

pub use diff::{
    build_dedup_history, diff_snapshots, escalation_key, parse_iso, summarize_discovery_diff,
    DedupEntry, DiscoveryDiff, EscalationKey,
};
pub use route::{
    route_escalation, route_non_escalation_diff, Budget, LaneWake, RouteInputs, RouteOutcome,
    WakeOutcome, DEFAULT_REFIRE_HOURS,
};
pub use snapshot::{
    build_raw_snapshot, build_raw_snapshot_with, escalation_stale, git_head_sha,
    list_snapshot_files, load_snapshot, read_escalations, semantic_projection, snapshot_filename,
    sweeps_dir, utc_now_ts, ProjectedBlock, ProjectedLane, ProjectedLease, ProjectedMessageQueue,
    ProjectedOperatorGates, ProjectedRegistryEntry, ProjectedSdlcState, RawSnapshot,
    SemanticProjection,
};

/// `SWEEP`'s registered workflow type string — the wire spelling a dispatch `ChainStep`'s
/// `block_id` names, matching the existing screaming-snake registry keys (`RECALL`,
/// `TERMINAL_PROBE`).
pub const SWEEP_WORKFLOW_TYPE: &str = "SWEEP";

/// [`SweepNode`]'s registered identity — the single node in this declared graph, both start and
/// terminal.
pub const SWEEP_NODE_NAME: &str = "SweepNode";

/// Everything [`run_sweep_pass`] (or [`SweepNode::process`]) can fail on. `Discovery` covers
/// every way [`build_raw_snapshot`] itself can fail (bad/ambiguous roadmap slug —
/// [`RoadmapStatusError`]); `Write`/`Serialize` cover this task's own new step, persisting the
/// composed document.
#[derive(Debug, thiserror::Error)]
pub enum SweepRunError {
    #[error(transparent)]
    Discovery(#[from] RoadmapStatusError),
    #[error("sweep: failed to serialize the composed snapshot for '{roadmap}': {source}")]
    Serialize {
        roadmap: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("sweep: failed to write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Run one full SWEEP pass over `roadmap`, rooted at `root` — `roadmap_sweep.py`'s `run_sweep`
/// (:789), ported field-for-field (minus the Python's `dry_run` flag, out of scope here exactly
/// as [`route_escalation`]'s own doc notes for the CLI face, `BA.25.D`):
///
/// 1. [`build_raw_snapshot`] — a fresh measurement of the world.
/// 2. Load the most recently written snapshot in [`sweeps_dir`], if any ([`list_snapshot_files`]
///    is already sorted; its last entry is the most recent). `None` on a first sweep is a
///    legitimate input to [`diff_snapshots`], never an error.
/// 3. [`diff_snapshots`] against it, and [`build_dedup_history`] scanning every snapshot already
///    on disk (never a second state file).
/// 4. Route every escalation in `diff.new_escalations`, tracking the shared per-pass
///    [`Budget`] and feeding each successful route straight back into `dedup_history` so a
///    later escalation in the SAME pass sees it (mirrors the Python's own in-loop
///    `dedup_history[...] = ...` after each `classify_and_route` call).
/// 5. Route every STANDING (not-new) escalation already present in `dedup_history` past its
///    re-fire threshold — the Python's own second loop, guarded by `new_keys` so a just-routed
///    escalation is never routed twice in the same pass.
/// 6. If nothing was routed above and the diff carries a bare non-escalation drift, route that
///    drift once ([`route_non_escalation_diff`]).
/// 7. Compose `{raw snapshot fields} + diff + routed}` and write it to
///    `sweeps_dir/<ts>.json` — the exact shape `roadmap_sweep.py`'s own `load_snapshot` reads
///    back.
///
/// Returns the composed document as written.
#[allow(clippy::too_many_arguments)]
pub async fn run_sweep_pass(
    root: &Path,
    roadmap: &str,
    now: DateTime<Utc>,
    refire_hours: f64,
    profile: PermissionProfile,
    transport: &dyn OperatorTransport,
    waker: &dyn LaneWake,
) -> Result<Value, SweepRunError> {
    let raw = build_raw_snapshot(root, roadmap, now)?;

    let sweeps_directory = sweeps_dir(root, roadmap);
    let prev_files = list_snapshot_files(&sweeps_directory);
    let prev_snapshot = prev_files.last().and_then(|path| load_snapshot(path));

    let diff = diff_snapshots(prev_snapshot.as_ref(), &raw);
    let mut dedup_history = build_dedup_history(&sweeps_directory, None);

    let inputs = RouteInputs {
        now,
        current_sha: raw.git_sha.as_deref(),
        refire_hours,
        profile,
    };
    let mut budget = Budget::default();
    let mut routed: Vec<RouteOutcome> = Vec::new();

    // --- 1. every NEW escalation ----------------------------------------------------------
    for escalation in &diff.new_escalations {
        let result = route_escalation(
            escalation,
            &dedup_history,
            inputs,
            &mut budget,
            transport,
            waker,
        )
        .await;
        if result.routed {
            if let Some(gate_id) = escalation.get("gate_id").and_then(Value::as_str) {
                dedup_history.insert(
                    gate_id.to_string(),
                    DedupEntry {
                        ts: result.ts_routed.clone(),
                        severity: result.severity.clone(),
                    },
                );
            }
        }
        routed.push(result);
    }

    // --- 2. STANDING escalations past their re-fire threshold ------------------------------
    let new_keys: HashSet<EscalationKey> =
        diff.new_escalations.iter().map(escalation_key).collect();
    for escalation in &raw.escalations {
        if new_keys.contains(&escalation_key(escalation)) {
            continue;
        }
        let Some(gate_id) = escalation.get("gate_id").and_then(Value::as_str) else {
            continue;
        };
        if !dedup_history.contains_key(gate_id) {
            continue;
        }
        let result = route_escalation(
            escalation,
            &dedup_history,
            inputs,
            &mut budget,
            transport,
            waker,
        )
        .await;
        if result.action == "skip-dedup" {
            continue;
        }
        if result.routed {
            dedup_history.insert(
                gate_id.to_string(),
                DedupEntry {
                    ts: result.ts_routed.clone(),
                    severity: result.severity.clone(),
                },
            );
        }
        routed.push(result);
    }

    // --- 3. bare non-escalation drift, at most once, only if nothing else routed -----------
    if routed.is_empty() && diff.non_escalation_diff {
        routed.push(route_non_escalation_diff(
            &diff, roadmap, now, profile, waker,
        ));
    }

    let mut document = serde_json::to_value(&raw).map_err(|source| SweepRunError::Serialize {
        roadmap: roadmap.to_string(),
        source,
    })?;
    if let Value::Object(map) = &mut document {
        map.insert(
            "diff".to_string(),
            serde_json::to_value(&diff).map_err(|source| SweepRunError::Serialize {
                roadmap: roadmap.to_string(),
                source,
            })?,
        );
        map.insert(
            "routed".to_string(),
            serde_json::to_value(&routed).map_err(|source| SweepRunError::Serialize {
                roadmap: roadmap.to_string(),
                source,
            })?,
        );
    }

    fs::create_dir_all(&sweeps_directory).map_err(|source| SweepRunError::Write {
        path: sweeps_directory.clone(),
        source,
    })?;
    let out_path = sweeps_directory.join(snapshot_filename(&raw.ts_utc));
    let rendered =
        serde_json::to_string_pretty(&document).map_err(|source| SweepRunError::Serialize {
            roadmap: roadmap.to_string(),
            source,
        })?;
    fs::write(&out_path, format!("{rendered}\n")).map_err(|source| SweepRunError::Write {
        path: out_path,
        source,
    })?;

    Ok(document)
}

/// An honest placeholder [`OperatorTransport`] — no production Telegram/WhatsApp transport
/// exists anywhere in this crate yet (confirmed by grep: the only other `impl OperatorTransport`
/// is `operator::transport`'s own `#[cfg(test)] NoopTransport`, unreachable from production
/// code). `send` always fails with [`NotifyError::Transport`] naming itself as the reason, so a
/// `notify-ask` route is recorded truthfully as `routed: false` with that reason — never a false
/// "delivered". `poll_responses` returns an empty batch; `acknowledge` uses the trait's no-op
/// default. Wiring a real transport in is a later task's concern (this module's own doc
/// comment); [`registry_with`] is the seam that later task uses.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopOperatorTransport;

#[async_trait]
impl OperatorTransport for NoopOperatorTransport {
    async fn send(
        &self,
        _payload: &ValidatedOperatorPayload,
    ) -> Result<DeliveredMessage, NotifyError> {
        Err(NotifyError::Transport {
            reason: "NoopOperatorTransport: no production OperatorTransport wired into SWEEP yet"
                .to_string(),
        })
    }

    async fn poll_responses(
        &self,
        since: Option<UpdateCursor>,
    ) -> Result<(Vec<OperatorResponse>, Option<UpdateCursor>), NotifyError> {
        Ok((Vec::new(), since))
    }

    async fn acknowledge(
        &self,
        _response: &OperatorResponse,
        _verdict: &ResponseVerdict,
    ) -> Result<(), NotifyError> {
        Ok(())
    }
}

/// An honest placeholder [`LaneWake`] — a production impl over `crate::coord::write::send`
/// (building a well-formed `okf_core::MessageRecord` envelope) is a later task's concern, per
/// [`route`]'s own module doc. Every wake this drives is recorded as `invoked: false` with an
/// `error` naming the placeholder, never a false "invoked".
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopLaneWake;

impl LaneWake for NoopLaneWake {
    fn wake(&self, _repo: &str, _lane: &str, _reason: &str, _context: &Value) -> WakeOutcome {
        WakeOutcome {
            invoked: false,
            error: Some("NoopLaneWake: no production LaneWake wired into SWEEP yet".to_string()),
        }
    }
}

/// Parse `ctx.event`'s `"profile"` field into a [`PermissionProfile`] via its own
/// `#[serde(rename_all = "snake_case")]` `Deserialize` impl — never a hand-rolled `FromStr`
/// (`permission.rs` pins by test that no such impl exists for this closed enum). Absent
/// entirely, this defaults to [`crate::policy::permission::DEFAULT_PROFILE`]; present
/// but unrecognized is a loud event-shape error, never a silent fallback.
fn resolve_event_profile(event: &Value) -> Result<PermissionProfile, NodeError> {
    match event.get("profile") {
        None => Ok(crate::policy::permission::DEFAULT_PROFILE),
        Some(raw) => serde_json::from_value(raw.clone()).map_err(|err| {
            NodeError::new(format!(
                "{SWEEP_NODE_NAME}: ctx.event's \"profile\" is not a recognized permission \
                 profile: {err}"
            ))
        }),
    }
}

/// The `SWEEP` workflow's single node — both start and terminal. Reads `root` (a filesystem
/// path string) and `roadmap` (a slug string) off `ctx.event`, both required; `profile` is
/// optional (see [`resolve_event_profile`]); `now` is optional (an RFC3339 string, defaulting to
/// the real current instant — a fixed `now` lets a caller replay a specific instant
/// deterministically). Runs [`run_sweep_pass`] and stamps its composed document onto
/// `ctx.nodes["SweepNode"]`.
pub struct SweepNode {
    transport: Arc<dyn OperatorTransport>,
    waker: Arc<dyn LaneWake>,
    refire_hours: f64,
}

impl SweepNode {
    /// Build a `SweepNode` with the given transport/waker seams and
    /// [`DEFAULT_REFIRE_HOURS`].
    #[must_use]
    pub fn new(transport: Arc<dyn OperatorTransport>, waker: Arc<dyn LaneWake>) -> Self {
        Self {
            transport,
            waker,
            refire_hours: DEFAULT_REFIRE_HOURS,
        }
    }

    fn required_str<'a>(event: &'a Value, field: &str) -> Result<&'a str, NodeError> {
        event.get(field).and_then(Value::as_str).ok_or_else(|| {
            NodeError::new(format!(
                "{SWEEP_NODE_NAME}: ctx.event missing required string \"{field}\""
            ))
        })
    }
}

#[async_trait]
impl Node for SweepNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let root = PathBuf::from(Self::required_str(&ctx.event, "root")?);
        let roadmap = Self::required_str(&ctx.event, "roadmap")?.to_string();
        let profile = resolve_event_profile(&ctx.event)?;
        let now = match ctx.event.get("now").and_then(Value::as_str) {
            Some(raw) => DateTime::parse_from_rfc3339(raw)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|err| {
                    NodeError::new(format!(
                        "{SWEEP_NODE_NAME}: ctx.event's \"now\" is not a valid RFC3339 \
                         timestamp: {err}"
                    ))
                })?,
            None => Utc::now(),
        };

        let document = run_sweep_pass(
            &root,
            &roadmap,
            now,
            self.refire_hours,
            profile,
            self.transport.as_ref(),
            self.waker.as_ref(),
        )
        .await
        .map_err(|err| NodeError::new(format!("{SWEEP_NODE_NAME}: {err}")))?;

        let mut ctx = ctx;
        ctx.nodes.insert(SWEEP_NODE_NAME.to_string(), document);
        Ok(ctx)
    }

    fn name(&self) -> &str {
        SWEEP_NODE_NAME
    }
}

/// Build the declared `WorkflowSchema` for `SWEEP`: a single node, both start and terminal, with
/// no forward connection — mirrors `recall::graph::schema`'s shape verbatim.
#[must_use]
pub fn schema() -> WorkflowSchema {
    let mut nodes = HashMap::new();
    nodes.insert(
        SWEEP_NODE_NAME.to_string(),
        NodeConfig::new(SWEEP_NODE_NAME, vec![]),
    );
    WorkflowSchema::new(SWEEP_WORKFLOW_TYPE, SWEEP_NODE_NAME, nodes)
}

/// Build a fresh `NodeRegistry` for `SWEEP` with the placeholder seams ([`NoopOperatorTransport`],
/// [`NoopLaneWake`]) — `engine-serve`'s bare `register_sweep` uses this. See [`registry_with`]
/// for the explicit-seam entry point.
#[must_use]
pub fn registry() -> NodeRegistry {
    registry_with(Arc::new(NoopOperatorTransport), Arc::new(NoopLaneWake))
}

/// Build a fresh `NodeRegistry` for `SWEEP` with explicit transport/waker seams — the entry
/// point a later task (once a production `OperatorTransport`/`LaneWake` exists) or a test
/// injects through, mirroring `terminal_probe::graph::registry_with`'s shape.
#[must_use]
pub fn registry_with(
    transport: Arc<dyn OperatorTransport>,
    waker: Arc<dyn LaneWake>,
) -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(SweepNode::new(transport, waker)));
    registry
}

/// Build the runnable `SWEEP` `Workflow` with the placeholder seams: [`registry`] paired with
/// [`schema`], constructed via `Workflow::new_validated` so assembly fails loudly if the
/// declared graph is not structurally sound.
///
/// # Panics
/// Panics if the declared graph fails `WorkflowValidator::validate` — this would be a
/// programming error in this module, not a runtime condition callers should recover from.
#[must_use]
pub fn workflow() -> Workflow {
    Workflow::new_validated(registry(), schema())
        .expect("SWEEP declared graph must pass WorkflowValidator::validate")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate::WorkflowValidator;
    use std::fs;

    fn write_lane_log(roadmap_dir: &Path, lines: &[&str]) {
        fs::write(roadmap_dir.join("lane-log.jsonl"), lines.join("\n") + "\n").unwrap();
    }

    fn fixture_root() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn make_roadmap(root: &Path, roadmap: &str) -> PathBuf {
        let roadmap_dir = root.join("planning/roadmaps").join(roadmap);
        fs::create_dir_all(&roadmap_dir).unwrap();
        write_lane_log(&roadmap_dir, &[]);
        roadmap_dir
    }

    // -----------------------------------------------------------------------------------------
    // schema / registry / workflow
    // -----------------------------------------------------------------------------------------

    #[test]
    fn schema_passes_validation() {
        let schema = schema();
        let registry = registry();
        WorkflowValidator::validate(&registry, &schema).expect("declared graph should validate");
    }

    #[test]
    fn workflow_type_matches_schema() {
        assert_eq!(schema().workflow_type, SWEEP_WORKFLOW_TYPE);
        assert_eq!(SWEEP_WORKFLOW_TYPE, "SWEEP");
    }

    #[test]
    fn declared_graph_is_a_single_start_and_terminal_node() {
        let schema = schema();
        assert_eq!(schema.start_node, SWEEP_NODE_NAME);
        assert_eq!(schema.nodes.len(), 1);
        let config = schema
            .nodes
            .get(SWEEP_NODE_NAME)
            .expect("SweepNode should be declared");
        assert!(
            config.connections.is_empty(),
            "SweepNode should be terminal"
        );
    }

    #[test]
    fn registry_contains_exactly_sweep_node() {
        let registry = registry();
        assert!(registry.contains(SWEEP_NODE_NAME));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn workflow_builds_without_panicking() {
        let _workflow = workflow();
    }

    // -----------------------------------------------------------------------------------------
    // NoopOperatorTransport / NoopLaneWake — honest placeholders, never a false "delivered"
    // -----------------------------------------------------------------------------------------

    #[tokio::test]
    async fn noop_operator_transport_reports_failure_never_a_false_delivery() {
        let payload = crate::operator::validate(
            crate::operator::payload::OperatorPayload::new(
                "gate-1",
                "summary",
                vec![
                    crate::operator::payload::OperatorResponseOption::new("y", "Yes"),
                    crate::operator::payload::OperatorResponseOption::new("n", "No"),
                ],
            ),
            &crate::operator::OperatorPayloadLimits::default(),
        )
        .unwrap();
        let err = NoopOperatorTransport
            .send(&payload)
            .await
            .expect_err("placeholder transport must never report success");
        assert!(matches!(err, NotifyError::Transport { .. }));
    }

    #[test]
    fn noop_lane_wake_reports_not_invoked_never_a_false_invocation() {
        let outcome = NoopLaneWake.wake("repo", "lane", "reason", &serde_json::json!({}));
        assert!(!outcome.invoked);
        assert!(outcome.error.is_some());
    }

    // -----------------------------------------------------------------------------------------
    // resolve_event_profile
    // -----------------------------------------------------------------------------------------

    #[test]
    fn resolve_event_profile_defaults_to_standard_when_absent() {
        let profile = resolve_event_profile(&serde_json::json!({})).unwrap();
        assert_eq!(profile, PermissionProfile::Standard);
    }

    #[test]
    fn resolve_event_profile_parses_the_wire_spelling() {
        let profile =
            resolve_event_profile(&serde_json::json!({"profile": "unrestricted"})).unwrap();
        assert_eq!(profile, PermissionProfile::Unrestricted);
    }

    #[test]
    fn resolve_event_profile_rejects_an_unrecognized_value() {
        let err = resolve_event_profile(&serde_json::json!({"profile": "bogus"}))
            .expect_err("an unrecognized profile string must be a loud error");
        assert!(err.to_string().contains("profile"));
    }

    // -----------------------------------------------------------------------------------------
    // run_sweep_pass — first sweep, and a round-trip a second pass can load back
    // -----------------------------------------------------------------------------------------

    #[tokio::test]
    async fn first_sweep_writes_a_snapshot_loadable_by_load_snapshot() {
        let root = fixture_root();
        make_roadmap(root.path(), "demo-roadmap");

        let now = DateTime::parse_from_rfc3339("2026-09-08T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let document = run_sweep_pass(
            root.path(),
            "demo-roadmap",
            now,
            DEFAULT_REFIRE_HOURS,
            PermissionProfile::Standard,
            &NoopOperatorTransport,
            &NoopLaneWake,
        )
        .await
        .expect("first sweep pass should succeed");

        assert_eq!(document["roadmap"], "demo-roadmap");
        assert_eq!(document["diff"]["first_sweep"], true);
        assert_eq!(document["routed"], serde_json::json!([]));

        let sweeps_directory = sweeps_dir(root.path(), "demo-roadmap");
        let files = list_snapshot_files(&sweeps_directory);
        assert_eq!(files.len(), 1);

        let loaded = load_snapshot(&files[0]).expect("the written snapshot must load back");
        assert_eq!(loaded.roadmap, "demo-roadmap");
        assert_eq!(loaded.ts_utc, document["ts_utc"].as_str().unwrap());
    }

    #[tokio::test]
    async fn second_sweep_with_no_changes_reports_unchanged_and_writes_a_second_file() {
        let root = fixture_root();
        make_roadmap(root.path(), "demo-roadmap");

        let now = DateTime::parse_from_rfc3339("2026-09-08T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        run_sweep_pass(
            root.path(),
            "demo-roadmap",
            now,
            DEFAULT_REFIRE_HOURS,
            PermissionProfile::Standard,
            &NoopOperatorTransport,
            &NoopLaneWake,
        )
        .await
        .expect("first sweep pass should succeed");

        let later = now + chrono::Duration::seconds(1);
        let document = run_sweep_pass(
            root.path(),
            "demo-roadmap",
            later,
            DEFAULT_REFIRE_HOURS,
            PermissionProfile::Standard,
            &NoopOperatorTransport,
            &NoopLaneWake,
        )
        .await
        .expect("second sweep pass should succeed");

        assert_eq!(document["diff"]["changed"], false);
        assert_eq!(document["diff"]["first_sweep"], false);

        let sweeps_directory = sweeps_dir(root.path(), "demo-roadmap");
        assert_eq!(list_snapshot_files(&sweeps_directory).len(), 2);
    }

    #[tokio::test]
    async fn a_new_escalation_routes_and_is_recorded_even_when_the_placeholder_transport_fails() {
        let root = fixture_root();
        let roadmap_dir = make_roadmap(root.path(), "demo-roadmap");
        fs::write(
            roadmap_dir.join("escalations.jsonl"),
            serde_json::json!({
                "gate_id": "g1",
                "kind": "advisory",
                "channel": "session:lane-a",
                "severity": "advisory",
                "summary": "something happened",
            })
            .to_string()
                + "\n",
        )
        .unwrap();

        let now = DateTime::parse_from_rfc3339("2026-09-08T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let document = run_sweep_pass(
            root.path(),
            "demo-roadmap",
            now,
            DEFAULT_REFIRE_HOURS,
            PermissionProfile::Standard,
            &NoopOperatorTransport,
            &NoopLaneWake,
        )
        .await
        .expect("sweep pass should succeed even though the placeholder waker never invokes");

        let routed = document["routed"]
            .as_array()
            .expect("routed should be an array");
        assert_eq!(routed.len(), 1);
        assert_eq!(routed[0]["gate_id"], "g1");
        assert_eq!(routed[0]["action"], "wake-session");
        assert_eq!(routed[0]["routed"], false);
    }

    // -----------------------------------------------------------------------------------------
    // SweepNode — event validation and end-to-end dispatch
    // -----------------------------------------------------------------------------------------

    fn base_ctx(event: Value) -> TaskContext {
        TaskContext {
            event,
            nodes: Default::default(),
            metadata: serde_json::json!({}),
            node_runs: Default::default(),
        }
    }

    #[tokio::test]
    async fn sweep_node_requires_root_and_roadmap() {
        let node = SweepNode::new(Arc::new(NoopOperatorTransport), Arc::new(NoopLaneWake));

        let err = node
            .process(base_ctx(serde_json::json!({})))
            .await
            .expect_err("missing root/roadmap must be a loud error");
        assert!(err.to_string().contains("root"));

        let err = node
            .process(base_ctx(serde_json::json!({"root": "/tmp"})))
            .await
            .expect_err("missing roadmap must be a loud error");
        assert!(err.to_string().contains("roadmap"));
    }

    #[tokio::test]
    async fn sweep_node_rejects_a_malformed_now() {
        let node = SweepNode::new(Arc::new(NoopOperatorTransport), Arc::new(NoopLaneWake));
        let err = node
            .process(base_ctx(serde_json::json!({
                "root": "/tmp", "roadmap": "demo", "now": "not-a-timestamp",
            })))
            .await
            .expect_err("a malformed now must be a loud error");
        assert!(err.to_string().contains("now"));
    }

    #[tokio::test]
    async fn sweep_node_end_to_end_stamps_the_composed_document_onto_ctx_nodes() {
        let root = fixture_root();
        make_roadmap(root.path(), "demo-roadmap");

        let node = SweepNode::new(Arc::new(NoopOperatorTransport), Arc::new(NoopLaneWake));
        let ctx = node
            .process(base_ctx(serde_json::json!({
                "root": root.path().to_string_lossy(),
                "roadmap": "demo-roadmap",
                "now": "2026-09-08T00:00:00Z",
            })))
            .await
            .expect("SweepNode should process a well-formed event");

        let stamped = ctx
            .nodes
            .get(SWEEP_NODE_NAME)
            .expect("SweepNode should stamp its result onto ctx.nodes");
        assert_eq!(stamped["roadmap"], "demo-roadmap");
    }
}
