//! `COMMANDER` — the drain as a workflow (`EN.15.F`).
//!
//! Ports `/orchestration-commander`'s drain loop as a registered engine workflow: discover
//! every lane's inbox under the fleet lock dir, route each message by `kind`, complete each
//! with a receipt, then (later tasks in this same block) emit scoped state under the
//! commander's own identity, commit only the manifest paths, and append the drain-log without
//! ever skipping.
//!
//! Submodules, one per pipeline stage — mirroring [`crate::workflows::sweep`]'s section split:
//! - [`drain`] — discover every queue, drain each inbox, route by kind, complete with receipts
//!   (task 1).
//! - [`emit_commit`] — the scoped `mev::emit_state_as` call and the manifest-ONLY commit,
//!   with the `git add -A` mutation test (task 2).
//! - [`drain_log`] — the drain-log append that never skips (a `roadmap: null` row when no
//!   roadmap resolves, never a skipped append) and the commander heartbeat in the pinned bare
//!   epoch-seconds format (task 3).
//! - [`triage`] — the commander's single gated `ClaudeCodeStep` (task 4, this task): orphan
//!   classification plus the `planning/open-work/index.md` check, gated on
//!   `GatedAction::RunDrain` and never invoked with a permissions bypass. Registration into
//!   the workflow graph itself is a later task of this same block.
//!
//! **THIS BLOCK'S CENTRAL SCAR, restated because it is the reason [`drain`] exists at all:**
//! the Python commander drained only its own inbox and reported "drained 0" for THIRTEEN
//! consecutive drains while three messages — one a P0 — sat unread. [`drain::discover_queues`]
//! walks the WHOLE `queue/` tree, mirroring `scripts/drain_log.py`'s `discover_queues`, so a
//! drain against any one lane still finds every other lane's mail.
//!
//! **RE-DERIVES, NEVER DETECTS:** every function here re-reads the tree from scratch on each
//! call rather than tracking a cursor or a "last seen" marker — the property that makes running
//! the same drain twice over an unchanged tree a safe no-op. Do not add a cache here.
//!
//! ## Registration (task 5, this task)
//!
//! [`schema`]/[`registry`] assemble the declared two-node `COMMANDER` graph —
//! [`CommanderDrainNode`] (start: discover-drain-route-complete, the scoped emit + manifest-only
//! commit, and the drain-log append + heartbeat stamp, all composed from the earlier tasks'
//! functions) feeding [`CommanderTriageNode`] (terminal: the block's one gated `ClaudeCodeStep`,
//! resolved fresh from `ctx.event`'s `profile`/`model` on every run so a `Deny`d profile is
//! recorded as [`triage::TriageOutcome::SuppressedByProfile`] rather than the step being baked
//! into the graph unconditionally). `crates/engine-serve/src/workflows.rs` registers this as a
//! dispatchable workflow; per Fork 4 (this block's `out_of_scope`), registering never schedules
//! it — `planning/harness.json`'s `schedule.entries` stays `[]`.

pub mod drain;
pub mod drain_log;
pub mod emit_commit;
pub mod triage;

use std::collections::HashMap;
use std::path::PathBuf;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use engine_contract::TaskContext;
use serde_json::Value;

use crate::node::{Node, NodeError, NodeRegistry};
use crate::policy::permission::{PermissionProfile, DEFAULT_PROFILE};
use crate::schema::{NodeConfig, WorkflowSchema};

/// The `COMMANDER` workflow's declared type identity.
pub const COMMANDER_WORKFLOW_TYPE: &str = "COMMANDER";

/// [`CommanderDrainNode`]'s registered identity — the graph's start node.
pub const COMMANDER_DRAIN_NODE_NAME: &str = "CommanderDrainNode";

/// Parse `ctx.event`'s `"profile"` field into a [`PermissionProfile`], mirroring
/// [`crate::workflows::sweep`]'s own `resolve_event_profile`: absent defaults to
/// [`DEFAULT_PROFILE`]; present but unrecognized is a loud event-shape error, never a silent
/// fallback.
fn resolve_event_profile(node_name: &str, event: &Value) -> Result<PermissionProfile, NodeError> {
    match event.get("profile") {
        None => Ok(DEFAULT_PROFILE),
        Some(raw) => serde_json::from_value(raw.clone()).map_err(|err| {
            NodeError::new(format!(
                "{node_name}: ctx.event's \"profile\" is not a recognized permission profile: \
                 {err}"
            ))
        }),
    }
}

/// The `COMMANDER` workflow's start node: run one full drain-and-route pass over EVERY queue
/// under the lock dir ([`drain::drain_all_queues`]), the scoped emit + manifest-ONLY commit
/// ([`emit_commit::run_scoped_emit_and_commit`]), then append the drain-log row that never
/// skips and stamp the heartbeat ([`drain_log::append_drain_log`] /
/// [`drain_log::stamp_commander_heartbeat`]) — all under the identity `ctx.event` names.
///
/// Reads off `ctx.event`: `root` and `repo` and `agent` (all required strings); `dir` (the
/// repo's own checkout directory passed to the scoped emit, defaulting to `root/<repo>`);
/// `lock_dir` (defaulting to `root/.fleet-locks`, mirroring `mev::brain::lease::resolve_lock_dir`'s
/// own default); `roadmap` (optional — `None` still produces a `roadmap: null` drain-log row,
/// per THE DRAIN LOG NEVER SKIPS); `drain_log_path` (defaulting to
/// `root/planning/roadmaps/<roadmap>/drain-log.jsonl` when `roadmap` resolves, else
/// `<lock_dir>/commander-drain-log.jsonl` — a lane with no roadmap yet still gets a durable,
/// discoverable log rather than silently having nowhere to write); `heartbeat_name` (defaulting
/// to `agent`); and `now` (an optional RFC3339 timestamp, defaulting to the real current
/// instant, driving both the drain-log row's `ts` and the heartbeat's epoch seconds).
pub struct CommanderDrainNode;

impl CommanderDrainNode {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    fn required_str<'a>(event: &'a Value, field: &str) -> Result<&'a str, NodeError> {
        event.get(field).and_then(Value::as_str).ok_or_else(|| {
            NodeError::new(format!(
                "{COMMANDER_DRAIN_NODE_NAME}: ctx.event missing required string \"{field}\""
            ))
        })
    }
}

impl Default for CommanderDrainNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Node for CommanderDrainNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let root = PathBuf::from(Self::required_str(&ctx.event, "root")?);
        let repo = Self::required_str(&ctx.event, "repo")?.to_string();
        let agent = Self::required_str(&ctx.event, "agent")?.to_string();

        let lock_dir = ctx
            .event
            .get("lock_dir")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join(".fleet-locks"));
        let dir = ctx
            .event
            .get("dir")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join(&repo));
        let roadmap = ctx
            .event
            .get("roadmap")
            .and_then(Value::as_str)
            .map(str::to_string);
        let drain_log_path = ctx
            .event
            .get("drain_log_path")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .unwrap_or_else(|| match &roadmap {
                Some(r) => root
                    .join("planning")
                    .join("roadmaps")
                    .join(r)
                    .join("drain-log.jsonl"),
                None => lock_dir.join("commander-drain-log.jsonl"),
            });
        let heartbeat_name = ctx
            .event
            .get("heartbeat_name")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| agent.clone());
        let now = match ctx.event.get("now").and_then(Value::as_str) {
            Some(raw) => DateTime::parse_from_rfc3339(raw)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|err| {
                    NodeError::new(format!(
                        "{COMMANDER_DRAIN_NODE_NAME}: ctx.event's \"now\" is not a valid \
                         RFC3339 timestamp: {err}"
                    ))
                })?,
            None => Utc::now(),
        };
        let now_iso = now.to_rfc3339();
        let now_epoch = now.timestamp();

        // 1. Drain EVERY queue under the lock dir — never only this lane's own inbox.
        let drain_results = drain::drain_all_queues(&lock_dir, &now_iso);
        let drained: u64 = drain_results.iter().map(|r| r.routed.len() as u64).sum();
        let completed: u64 = drain_results
            .iter()
            .flat_map(|r| r.routed.iter())
            .filter(|m| m.completed)
            .count() as u64;

        // 2. The scoped emit + manifest-ONLY commit. A foreign-lease refusal is captured in
        //    `emit_json` below and reported, never silently swallowed — and never retried
        //    (`run_scoped_emit_and_commit` makes exactly one `emit_state_as` call).
        let emit_outcome =
            emit_commit::run_scoped_emit_and_commit(&root, &repo, &agent, &dir, Some(&lock_dir));
        let manifest_paths: u64 = match &emit_outcome {
            emit_commit::EmitCommitOutcome::Committed { manifest, .. } => manifest.len() as u64,
            _ => 0,
        };
        let emit_json = match &emit_outcome {
            emit_commit::EmitCommitOutcome::Committed { manifest, commit } => serde_json::json!({
                "status": "committed",
                "manifest": manifest.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
                "committed": commit.committed.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
                "failed": commit.failed.len(),
            }),
            emit_commit::EmitCommitOutcome::NoOp => serde_json::json!({ "status": "noop" }),
            emit_commit::EmitCommitOutcome::Refused { reason } => serde_json::json!({
                "status": "refused",
                "reason": reason,
            }),
            emit_commit::EmitCommitOutcome::Failed { reason } => serde_json::json!({
                "status": "failed",
                "reason": reason,
            }),
        };

        // 3. THE DRAIN LOG NEVER SKIPS: append exactly one summary row, `roadmap: null` when
        //    unresolved, plus every not-yet-mirrored receipt/message row.
        let summary = drain_log::DrainSummary {
            roadmap: roadmap.clone(),
            drained,
            routed: drained,
            completed,
            manifest_paths,
            orphan_inbox: 0,
            orphan_processing: 0,
            orphan_receipts: 0,
        };
        let append_outcome =
            drain_log::append_drain_log(&drain_log_path, &lock_dir, &summary, &now_iso).map_err(
                |err| {
                    NodeError::new(format!(
                        "{COMMANDER_DRAIN_NODE_NAME}: failed to append drain log at {}: {err}",
                        drain_log_path.display()
                    ))
                },
            )?;

        // 4. The heartbeat, bare epoch seconds — `commander_drain.sh` stays and may stamp the
        //    same file; last-writer-wins by design.
        drain_log::stamp_commander_heartbeat(&lock_dir, &heartbeat_name, now_epoch).map_err(
            |err| {
                NodeError::new(format!(
                    "{COMMANDER_DRAIN_NODE_NAME}: failed to stamp the commander heartbeat: {err}"
                ))
            },
        )?;

        let mut ctx = ctx;
        ctx.nodes.insert(
            COMMANDER_DRAIN_NODE_NAME.to_string(),
            serde_json::json!({
                "queues_discovered": drain_results.len(),
                "drained": drained,
                "completed": completed,
                "emit": emit_json,
                "drain_log_path": drain_log_path.display().to_string(),
                "drain_log_receipts_added": append_outcome.receipts_added,
                "drain_log_messages_added": append_outcome.messages_added,
                "heartbeat_stamped": true,
                "heartbeat_epoch": now_epoch,
            }),
        );
        Ok(ctx)
    }

    fn name(&self) -> &str {
        COMMANDER_DRAIN_NODE_NAME
    }
}

/// The `COMMANDER` workflow's terminal node: the block's ONE gated `ClaudeCodeStep`. Resolves
/// `profile`/`model` fresh from `ctx.event` on every run (never baked into the graph at
/// registration time) and delegates to [`triage::build_triage_step`] — a `Deny`d profile is
/// recorded onto `ctx.nodes[triage::TRIAGE_STEP_NAME]` as `{"suppressed_by_profile": true}`
/// (the same "recorded, never dropped" discipline [`crate::workflows::sweep::route`]'s
/// `suppressed_by_profile` uses), a `Permit`d one actually runs the one constructed
/// `ClaudeCodeStep`. This node's registered name IS [`triage::TRIAGE_STEP_NAME`] — there is
/// exactly one `ClaudeCodeStep`-shaped node anywhere in this graph.
pub struct CommanderTriageNode;

impl CommanderTriageNode {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for CommanderTriageNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Node for CommanderTriageNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let profile = resolve_event_profile(triage::TRIAGE_STEP_NAME, &ctx.event)?;
        let model = ctx
            .event
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string);

        match triage::build_triage_step(profile, model) {
            triage::TriageOutcome::SuppressedByProfile => {
                let mut ctx = ctx;
                ctx.nodes.insert(
                    triage::TRIAGE_STEP_NAME.to_string(),
                    serde_json::json!({ "suppressed_by_profile": true }),
                );
                Ok(ctx)
            }
            triage::TriageOutcome::Step(step) => step.process(ctx).await,
        }
    }

    fn name(&self) -> &str {
        triage::TRIAGE_STEP_NAME
    }
}

/// Build the declared `WorkflowSchema` for `COMMANDER`: [`CommanderDrainNode`] (start) feeding
/// [`CommanderTriageNode`] (terminal) — the exactly-one-`ClaudeCodeStep` graph this block's
/// acceptance criteria require.
#[must_use]
pub fn schema() -> WorkflowSchema {
    let mut nodes = HashMap::new();
    nodes.insert(
        COMMANDER_DRAIN_NODE_NAME.to_string(),
        NodeConfig::new(
            COMMANDER_DRAIN_NODE_NAME,
            vec![triage::TRIAGE_STEP_NAME.to_string()],
        ),
    );
    nodes.insert(
        triage::TRIAGE_STEP_NAME.to_string(),
        NodeConfig::new(triage::TRIAGE_STEP_NAME, vec![]),
    );
    WorkflowSchema::new(COMMANDER_WORKFLOW_TYPE, COMMANDER_DRAIN_NODE_NAME, nodes)
}

/// Build a fresh `NodeRegistry` for `COMMANDER` — `engine-serve`'s `register_commander` uses
/// this directly; there is no separate seam-injecting variant because, unlike `SWEEP`, neither
/// node here takes an injectable placeholder transport/waker.
#[must_use]
pub fn registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(CommanderDrainNode::new()));
    registry.register(Box::new(CommanderTriageNode::new()));
    registry
}
