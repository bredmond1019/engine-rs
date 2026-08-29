//! Dependency gate + admission gate — `EN.10.B` Task 2.
//!
//! Two independent checks the ORCHESTRATION workflow runs before starting any
//! [`ChainStep`](super::chain::ChainStep):
//!
//! 1. [`check_dependencies`] — every `depends_on` edge of the block must be met before the
//!    block starts. A block with an unmet edge is **never** started, and the returned
//!    [`GateError::UnmetDependency`] names both the unmet edge and the repo it lives in.
//!    Readiness comes from the graph via an injectable closure, never from a hand-written
//!    wave table in a roadmap doc — the roadmap is a hand-authored snapshot and per the
//!    task spec it has already been wrong in this very lane. This module mirrors
//!    [`chain::resolve_lane_chain`](super::chain::resolve_lane_chain)'s `is_block_open`
//!    seam: it stays independent of *how* "met" is determined (a live corpus-graph query,
//!    `state.json`, or a test double) by taking the graph as a closure rather than owning
//!    a concrete graph type.
//! 2. [`AdmissionGate`] — consults `EN.9.F`'s [`AdmissionControl`](crate::nodes::terminal::admission::AdmissionControl)
//!    before every block. At capacity the caller's `acquire` simply awaits (it neither
//!    proceeds nor fails) until a permit frees, exactly the semantics
//!    `AdmissionControl::acquire` already gives terminal-node fan-out — this gate is a
//!    thin, orchestration-flavoured wrapper so callers here get a chain-step-shaped API
//!    instead of reaching into `nodes::terminal` directly.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::nodes::terminal::admission::{AdmissionControl, AdmissionPermit};
use crate::policy::permission::{self, Decision, GatedAction, PermissionProfile};

use super::chain::ChainStep;

// ── Dependency gate ─────────────────────────────────────────────────────

/// One `depends_on` edge a block declares. Widened (`EN.11.J` Task 1) beyond the
/// original block-only shape so an operator/approval/external edge is representable
/// too, instead of being structurally undroppable-but-unrepresentable — which is why
/// `corpus_gates.rs` used to drop them rather than pass them through.
///
/// Deliberately an enum, not a struct with optional fields: an operator/approval gate
/// is *targetless* (`OperatorDep`/`ApprovalDep` in okf-core carry no `repo`/`block_id`
/// at all, only a `slug`), so giving every edge a `repo`/`block_id` pair would invent
/// meaningless values for three of the four variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencyEdge {
    /// A dependency on another block, possibly cross-repo. Met when
    /// `is_edge_met(repo, block_id)` reports the target block as done.
    Block { repo: String, block_id: String },
    /// An operator working session that gates this block, named by its slug.
    ///
    /// Always treated as **unmet** while it appears in `resolve_depends_on`'s
    /// result — there is no runtime check that could make it "met" without an
    /// engine self-clearing an operator gate, which HQ D71 forbids at any
    /// priority. The gate clears by the edge being removed from the corpus'
    /// `state.json` (the mev CLI is the single writer), not by this reader
    /// re-evaluating anything.
    Operator { slug: String },
    /// A pending approval gating this block, named by its slug. Same
    /// always-unmet-while-present contract as `Operator`.
    Approval { slug: String },
    /// An external/environmental fact gating this block, described by `what`.
    /// Same always-unmet-while-present contract as `Operator`.
    External { what: String },
}

/// Everything that can go wrong gating a step's start. Every variant names the block and
/// repo involved — this module never fails silently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateError {
    /// `step` has at least one `depends_on` edge that is not yet met. Only the *first*
    /// unmet edge found is reported — the run reports which edge is unmet, not an
    /// exhaustive list, matching the acceptance criterion's singular "which edge".
    UnmetDependency {
        repo: String,
        block_id: String,
        edge: DependencyEdge,
    },
}

impl fmt::Display for GateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GateError::UnmetDependency {
                repo,
                block_id,
                edge,
            } => match edge {
                DependencyEdge::Block {
                    repo: dep_repo,
                    block_id: dep_id,
                } => write!(
                    f,
                    "block '{block_id}' (repo '{repo}') cannot start: dependency '{dep_id}' \
                     (repo '{dep_repo}') is not yet met"
                ),
                DependencyEdge::Operator { slug } => write!(
                    f,
                    "block '{block_id}' (repo '{repo}') cannot start: operator gate \
                     '{slug}' is not yet cleared"
                ),
                DependencyEdge::Approval { slug } => write!(
                    f,
                    "block '{block_id}' (repo '{repo}') cannot start: approval gate \
                     '{slug}' is not yet granted"
                ),
                DependencyEdge::External { what } => write!(
                    f,
                    "block '{block_id}' (repo '{repo}') cannot start: external \
                     dependency '{what}' is not yet met"
                ),
            },
        }
    }
}

impl std::error::Error for GateError {}

/// Check that every `depends_on` edge of `step` is met before it may start.
///
/// `resolve_depends_on(repo, block_id)` returns the block's declared dependency edges —
/// this module never derives them itself, since the block record (and the live graph it
/// lives in) is owned elsewhere. `is_edge_met(repo, block_id)` reports whether a given
/// **block** edge is satisfied (e.g. the target block's state is `done`/`closed`) —
/// again supplied by the caller so this module stays independent of how "met" is
/// determined. Operator/approval/external edges never reach `is_edge_met` at all: per
/// [`DependencyEdge`]'s doc, they are unmet for as long as they are present in
/// `resolve_depends_on`'s result, by construction.
///
/// Edges are checked in the order `resolve_depends_on` returns them; the first unmet edge
/// short-circuits the check and is the one named in the returned error. A block with no
/// declared edges (an empty `resolve_depends_on` result) is always ready.
pub fn check_dependencies(
    step: &ChainStep,
    resolve_depends_on: &dyn Fn(&str, &str) -> Vec<DependencyEdge>,
    is_edge_met: &dyn Fn(&str, &str) -> bool,
) -> Result<(), GateError> {
    let edges = resolve_depends_on(&step.repo, &step.block_id);
    for edge in edges {
        let met = match &edge {
            DependencyEdge::Block { repo, block_id } => is_edge_met(repo, block_id),
            DependencyEdge::Operator { .. }
            | DependencyEdge::Approval { .. }
            | DependencyEdge::External { .. } => false,
        };
        if !met {
            return Err(GateError::UnmetDependency {
                repo: step.repo.clone(),
                block_id: step.block_id.clone(),
                edge,
            });
        }
    }
    Ok(())
}

// ── Permission gate ──────────────────────────────────────────────────────
//
// `EN.12.C` Task 5 — the enforcement wiring for `crate::policy::permission`: consult
// [`permission::decide`] before a graded action, alongside the dependency and
// admission gates above. On denial, author a `{"type":"operator"}` edge (through the
// caller-supplied `author_operator_edge` seam, never an in-process `state.json` write —
// `state.json` has a single writer, `mev`, per `seams.md` seam 6) and refuse the step.
// A permitted action is a pure no-op: `Ok(())`, and `author_operator_edge` is never
// invoked at all.

/// One `{"type":"operator", slug, exit, start}` edge this gate asks its caller to
/// author on denial. Field names and meaning mirror the edit-state-json contract
/// exactly: `exit` names an ARTIFACT whose existence ends the gate (never a
/// description of the work itself), and `slug` is the join key — deliberately derived
/// from `action` alone (see [`permission_gate_slug`]), never from the denied step's
/// `repo`/`block_id`, so every block denied the SAME action under the SAME run shares
/// ONE gate rather than raising a fresh operator edge per block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorGateRequest {
    pub slug: String,
    pub exit: String,
    pub start: String,
}

/// Derive the stable `slug` for the operator gate a denied [`GatedAction`] raises.
/// Built from the action's own wire identifier (`GatedAction`'s
/// `#[serde(rename_all = "snake_case")]` contract, the same round-trip discipline
/// `resolved_permission_profile_identifier` in `integrate.rs` already follows for
/// [`PermissionProfile`]) rather than a hand-written string, so it cannot drift from
/// the closed action vocabulary `permission.rs` owns.
fn permission_gate_slug(action: GatedAction) -> String {
    let wire_id = serde_json::to_value(action)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown-action".to_string());
    format!("permission-{wire_id}")
}

/// Build the [`OperatorGateRequest`] a denial of `action` under `profile` raises.
/// `exit` names an artifact — a decision doc recording the operator's authorization —
/// never the work the denied action would itself have performed; `start` names the
/// existing `/begin-session <slug>` entry point every operator edge in this fleet
/// already uses (see `close_block.rs`'s fixture and `docs/state/state-schema.md`).
fn build_operator_gate_request(action: GatedAction, profile: PermissionProfile) -> OperatorGateRequest {
    let slug = permission_gate_slug(action);
    OperatorGateRequest {
        exit: format!(
            "planning/decisions/{slug}.md exists and records the operator's decision to \
             authorize {action:?} under permission profile {profile:?}"
        ),
        start: format!("/begin-session {slug}"),
        slug,
    }
}

/// Everything that can go wrong at the permission gate. Distinct from [`GateError`]
/// (the dependency gate's own error type) so the two concerns — an unmet
/// `depends_on` edge versus a denied graded action — stay independently readable at
/// the call site, exactly as `FrontierError` stays independent of both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionGateError {
    /// `action` is denied for `profile`; an operator-gate edge naming `edge.slug` was
    /// (or was attempted to be) authored via the caller's `author_operator_edge` seam.
    Denied {
        repo: String,
        block_id: String,
        action: GatedAction,
        profile: PermissionProfile,
        edge: OperatorGateRequest,
    },
    /// `action` was denied, but the caller's `author_operator_edge` seam itself
    /// returned an error while trying to raise the gate. The chain still stops —
    /// this is reported as a DISTINCT failure from [`PermissionGateError::Denied`]
    /// so a caller (and a test) can tell "the gate is raised, blocking as designed"
    /// from "the gate could not even be raised", which is a mev-CLI-availability
    /// problem, not a permission decision.
    EdgeAuthorFailed {
        repo: String,
        block_id: String,
        action: GatedAction,
        profile: PermissionProfile,
        reason: String,
    },
}

impl fmt::Display for PermissionGateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PermissionGateError::Denied {
                repo,
                block_id,
                action,
                profile,
                edge,
            } => write!(
                f,
                "block '{block_id}' (repo '{repo}') cannot proceed: {action:?} is denied \
                 at permission profile {profile:?}; operator gate '{}' raised, blocking \
                 the chain",
                edge.slug
            ),
            PermissionGateError::EdgeAuthorFailed {
                repo,
                block_id,
                action,
                profile,
                reason,
            } => write!(
                f,
                "block '{block_id}' (repo '{repo}') cannot proceed: {action:?} is denied \
                 at permission profile {profile:?}, and authoring the operator-gate edge \
                 failed: {reason}"
            ),
        }
    }
}

impl std::error::Error for PermissionGateError {}

/// Gate `step`'s use of `action` under the permission `profile` in force for this run.
///
/// Consults [`permission::decide`] — the closed `(profile, action)` decision matrix
/// `EN.12.C` task 1 built — never re-derives a decision of its own. On
/// [`Decision::Permit`] this is a pure no-op: `Ok(())`, and `author_operator_edge` is
/// never called, so a run whose profile already permits `action` sees no new gate, no
/// new latency, and no behaviour change.
///
/// On [`Decision::Deny`] this calls `author_operator_edge` with the
/// [`OperatorGateRequest`] the denial raises, THEN refuses the step. Both the deny
/// decision and the authored edge happen unconditionally on denial — the step never
/// proceeds regardless of whether `author_operator_edge` itself succeeds (see
/// [`PermissionGateError::EdgeAuthorFailed`] for that distinct failure mode).
///
/// `author_operator_edge` is the seam through which the edge is actually written —
/// production wiring implements it as an out-of-process invocation of the `mev` CLI
/// (never an in-process `state.json` write; `state.json` has a single writer per
/// `seams.md` seam 6), mirroring how [`check_dependencies`] above takes
/// `resolve_depends_on`/`is_edge_met` as closures rather than owning a concrete graph
/// reader. This module contains, and calls, nothing capable of *clearing* an operator
/// gate — no reference to mev's gate-closing verb anywhere in this file (see the
/// `gates_rs_never_references_the_gate_closing_verb` test) — clearing a gate raised
/// here is a human decision routed through `mev close-operator-gate --exit-verified`,
/// exactly like every other operator edge in the fleet, never something this path can
/// do to itself.
pub fn check_permission_gate(
    step: &ChainStep,
    action: GatedAction,
    profile: PermissionProfile,
    author_operator_edge: &dyn Fn(&OperatorGateRequest) -> Result<(), String>,
) -> Result<(), PermissionGateError> {
    match permission::decide(profile, action) {
        Decision::Permit => Ok(()),
        Decision::Deny => {
            let edge = build_operator_gate_request(action, profile);
            if let Err(reason) = author_operator_edge(&edge) {
                return Err(PermissionGateError::EdgeAuthorFailed {
                    repo: step.repo.clone(),
                    block_id: step.block_id.clone(),
                    action,
                    profile,
                    reason,
                });
            }
            Err(PermissionGateError::Denied {
                repo: step.repo.clone(),
                block_id: step.block_id.clone(),
                action,
                profile,
                edge,
            })
        }
    }
}

// ── Frontier reader ─────────────────────────────────────────────────────
//
// `EN.11.K` Task 1 — a reader for mev's `planning/lane-frontier.json`, used ONLY to
// answer the narrow "is this lane's HEAD startable, and what are its `unmet_gates`"
// question. Per the block's `what`/`why`, this artifact holds ONE entry per
// `(roadmap, lane, segment)` — it cannot answer a per-edge `depends_on` question at
// all (that stays `corpus_gates.rs`'s live per-edge reader), so nothing in this
// section is wired into `check_dependencies`. Task 2 does that wiring, with the
// explicit constraint that a frontier `startable: true` must never short-circuit
// `check_dependencies`'s own refusal.

/// One lane's current head, as mev's `lane-frontier.json` records it. Field names and
/// types mirror `mev::brain::frontier::FrontierEntry` exactly — this module is a
/// reader, not a re-derivation, so it must accept the artifact byte-for-shape as mev
/// produces it rather than inventing a parallel schema.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct FrontierEntry {
    pub roadmap: String,
    pub lane: String,
    pub segment: usize,
    pub repo: String,
    /// Canonical `"repo:id"` key.
    pub key: String,
    pub id: String,
    pub title: String,
    pub status: String,
    /// Unmet `depends_on` edges of the `Block` kind, rendered `"repo:id"`.
    pub unmet_blocks: Vec<String>,
    /// Unmet `depends_on` edges of the `Operator`/`Approval`/`External` kind, rendered
    /// `"operator:<slug>"` / `"approval:<slug>"` / `"external:<what>"`.
    pub unmet_gates: Vec<String>,
    /// `true` iff both `unmet_blocks` and `unmet_gates` are empty. Per the block's
    /// `why`, this is NOT a licence to start — see the module-level note above.
    pub startable: bool,
}

/// One operator/approval gate's derived rank, as mev's `lane-frontier.json` records
/// it — consumed here as opaque data, never recomputed. mev owns `gate_rank`
/// derivation; a second implementation drifts, exactly like the two `digest_of`
/// implementations named in seams.md seam 7.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct GateRank {
    /// `"operator"` or `"approval"`.
    pub kind: String,
    pub slug: String,
    pub rank: u8,
    /// Every block this gate blocks, rendered `"repo:id"`.
    pub gates: Vec<String>,
}

/// The parsed shape of `planning/lane-frontier.json`: `{derived_at, entries[],
/// gate_ranks[]}`, matching `mev::brain::frontier::FrontierArtifact` exactly.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct FrontierArtifact {
    /// RFC 3339 timestamp of the mev derivation run that produced this artifact.
    pub derived_at: String,
    pub entries: Vec<FrontierEntry>,
    pub gate_ranks: Vec<GateRank>,
}

/// Everything that can go wrong reading `lane-frontier.json`. Every variant names the
/// path — this reader never fails silently, matching `GateError`'s own discipline. A
/// missing file and an unparsable file are kept distinct so a caller (and a test) can
/// tell "mev has never derived this artifact here" from "the artifact is present but
/// corrupt/stale-shaped".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrontierError {
    /// No file exists at `path` at all.
    Missing { path: PathBuf },
    /// A file exists at `path` but could not be parsed as a [`FrontierArtifact`].
    Unparsable { path: PathBuf, reason: String },
}

impl fmt::Display for FrontierError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrontierError::Missing { path } => write!(
                f,
                "lane frontier: no file at '{}' — mev has not derived it here \
                 (run `mev emit-state --write` or `mev frontier --json`)",
                path.display()
            ),
            FrontierError::Unparsable { path, reason } => write!(
                f,
                "lane frontier: '{}' could not be parsed as {{derived_at, entries[], \
                 gate_ranks[]}}: {reason}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for FrontierError {}

/// Read and parse `path` (typically `planning/lane-frontier.json`) as a
/// [`FrontierArtifact`]. Fails loudly and names the path on both a missing file and
/// an unparsable one — never returns a default/empty artifact, which would look like
/// an ordinary "nothing in the frontier" result instead of "this could not be read".
pub fn load_frontier(path: &Path) -> Result<FrontierArtifact, FrontierError> {
    let contents = fs::read_to_string(path).map_err(|_| FrontierError::Missing {
        path: path.to_path_buf(),
    })?;
    serde_json::from_str(&contents).map_err(|err| FrontierError::Unparsable {
        path: path.to_path_buf(),
        reason: err.to_string(),
    })
}

/// Find the frontier entry for `(roadmap, lane, segment)` — the ONLY key that is
/// unique across the artifact's entries. Matching on `lane` alone is ambiguous: a
/// lane name recurs once per roadmap (measured 2026-08-21: 24 entries, 3 for
/// engine-rs), so `lane == "engine-rs"` alone can hit more than one entry.
#[must_use]
pub fn find_lane_head<'a>(
    artifact: &'a FrontierArtifact,
    roadmap: &str,
    lane: &str,
    segment: usize,
) -> Option<&'a FrontierEntry> {
    artifact
        .entries
        .iter()
        .find(|e| e.roadmap == roadmap && e.lane == lane && e.segment == segment)
}

/// Resolve the [`FrontierEntry`] `step` refers to, by matching `step`'s own
/// `roadmap`/`lane`/`segment` (`chain.rs`'s `EN.11.K` Task 2 addition, filled by
/// [`resolve_lane_chain`](super::chain::resolve_lane_chain)) against `artifact`'s entries
/// via [`find_lane_head`]. Returns `None` when `step` carries no lane identity at all —
/// i.e. it came from [`resolve_explicit_chain`](super::chain::resolve_explicit_chain),
/// which deliberately bypasses the lane file and so names no `(roadmap, lane, segment)`
/// to look up.
#[must_use]
pub fn frontier_lane_head<'a>(
    step: &ChainStep,
    artifact: &'a FrontierArtifact,
) -> Option<&'a FrontierEntry> {
    let roadmap = step.roadmap.as_deref()?;
    let lane = step.lane.as_deref()?;
    let segment = step.segment?;
    find_lane_head(artifact, roadmap, lane, segment)
}

/// Gate a step's start using **both** signals, without ever letting the frontier's
/// `startable` substitute for the live per-edge check.
///
/// `check_dependencies` always runs, unconditionally — its result is computed first and
/// returned as-is; nothing about the frontier lookup can suppress or skip it. Per the
/// block's `why`: at spec time the frontier's own engine-rs head (`EN.11.E`) reports
/// `startable: true` with `unmet_gates: []` while being HELD on an operator decision the
/// graph cannot express, so treating `startable: true` as a licence to start would have
/// launched it. This function's shape makes that impossible structurally — the returned
/// [`FrontierEntry`] (when present) is advisory/telemetry only, never consulted to decide
/// the `Result`.
///
/// `frontier` is `Option` because a caller may be resolving an explicit chain (no
/// lane-file identity to look up) or may not have loaded a `lane-frontier.json` at all;
/// either way the per-edge gate still runs.
pub fn check_step_with_frontier_advice<'a>(
    step: &ChainStep,
    frontier: Option<&'a FrontierArtifact>,
    resolve_depends_on: &dyn Fn(&str, &str) -> Vec<DependencyEdge>,
    is_edge_met: &dyn Fn(&str, &str) -> bool,
) -> (Result<(), GateError>, Option<&'a FrontierEntry>) {
    let gate_result = check_dependencies(step, resolve_depends_on, is_edge_met);
    let head = frontier.and_then(|artifact| frontier_lane_head(step, artifact));
    (gate_result, head)
}

// ── Admission gate ──────────────────────────────────────────────────────

/// A thin, orchestration-flavoured wrapper over `EN.9.F`'s [`AdmissionControl`]: consult
/// admission before starting each [`ChainStep`], waiting (never proceeding, never
/// failing) when the configured concurrency ceiling is already saturated.
///
/// Cheaply cloneable — shares the same underlying semaphore as every clone, exactly like
/// [`AdmissionControl`] itself.
#[derive(Clone)]
pub struct AdmissionGate {
    control: AdmissionControl,
}

impl AdmissionGate {
    /// Wrap an existing, already-policy-resolved [`AdmissionControl`].
    #[must_use]
    pub fn new(control: AdmissionControl) -> Self {
        Self { control }
    }

    /// Build an admission gate under [`AdmissionPolicy::default`](crate::nodes::terminal::admission::AdmissionPolicy::default).
    #[must_use]
    pub fn with_default_policy() -> Self {
        Self::new(AdmissionControl::with_default_policy())
    }

    /// The number of admission slots currently free. Exposed for tests and telemetry.
    #[must_use]
    pub fn available_permits(&self) -> usize {
        self.control.available_permits()
    }

    /// Acquire one admission slot for `step`, awaiting (queueing) until one is free when
    /// the ceiling is already saturated. The returned [`AdmissionPermit`] holds the slot
    /// until dropped — drop it once the block's `SDLC_FLOW` run completes to admit the
    /// next queued block.
    ///
    /// `step` is accepted (rather than this being a bare `acquire()`) purely to keep the
    /// call site self-documenting at every gate call — the permit itself carries no
    /// per-block identity, matching `EN.9.F`'s uniform terminal-run semaphore.
    pub async fn acquire_for(&self, step: &ChainStep) -> AdmissionPermit {
        let _ = step; // identity is documentation-only; the semaphore is uniform.
        self.control.acquire().await
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::super::chain::StepKind;
    use super::*;
    use crate::nodes::terminal::admission::AdmissionPolicy;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    fn step(repo: &str, block_id: &str) -> ChainStep {
        ChainStep {
            repo: repo.to_string(),
            block_id: block_id.to_string(),
            directives: None,
            ..Default::default()
        }
    }

    #[test]
    fn unmet_edge_blocks_the_start_and_names_the_edge_and_repo() {
        let s = step("engine-rs", "EN.10.B");
        let resolve_deps = |_repo: &str, _id: &str| {
            vec![DependencyEdge::Block {
                repo: "engine-rs".to_string(),
                block_id: "EN.9.F".to_string(),
            }]
        };
        // Nothing is ever met.
        let is_met = |_repo: &str, _id: &str| false;

        let err = check_dependencies(&s, &resolve_deps, &is_met).unwrap_err();
        match &err {
            GateError::UnmetDependency {
                repo,
                block_id,
                edge,
            } => {
                assert_eq!(repo, "engine-rs");
                assert_eq!(block_id, "EN.10.B");
                match edge {
                    DependencyEdge::Block { repo, block_id } => {
                        assert_eq!(repo, "engine-rs");
                        assert_eq!(block_id, "EN.9.F");
                    }
                    other => panic!("expected a Block edge, got {other:?}"),
                }
            }
        }
        let msg = err.to_string();
        assert!(
            msg.contains("EN.9.F"),
            "message should name the edge: {msg}"
        );
        assert!(
            msg.contains("engine-rs"),
            "message should name the repo: {msg}"
        );
    }

    #[test]
    fn met_dependency_block_starts_immediately() {
        let s = step("engine-rs", "EN.10.B");
        let resolve_deps = |_repo: &str, _id: &str| {
            vec![DependencyEdge::Block {
                repo: "engine-rs".to_string(),
                block_id: "EN.9.F".to_string(),
            }]
        };
        let is_met = |_repo: &str, _id: &str| true;

        assert!(check_dependencies(&s, &resolve_deps, &is_met).is_ok());
    }

    #[test]
    fn block_with_no_declared_edges_is_always_ready() {
        let s = step("engine-rs", "EN.10.B");
        let resolve_deps = |_repo: &str, _id: &str| Vec::new();
        let is_met = |_repo: &str, _id: &str| false;

        assert!(check_dependencies(&s, &resolve_deps, &is_met).is_ok());
    }

    #[test]
    fn first_unmet_edge_short_circuits_the_check() {
        let s = step("engine-rs", "EN.10.B");
        let resolve_deps = |_repo: &str, _id: &str| {
            vec![
                DependencyEdge::Block {
                    repo: "engine-rs".to_string(),
                    block_id: "FIRST".to_string(),
                },
                DependencyEdge::Block {
                    repo: "engine-rs".to_string(),
                    block_id: "SECOND".to_string(),
                },
            ]
        };
        // Only FIRST is unmet.
        let is_met = |_repo: &str, id: &str| id != "FIRST";

        let err = check_dependencies(&s, &resolve_deps, &is_met).unwrap_err();
        match err {
            GateError::UnmetDependency { edge, .. } => match edge {
                DependencyEdge::Block { block_id, .. } => assert_eq!(block_id, "FIRST"),
                other => panic!("expected a Block edge, got {other:?}"),
            },
        }
    }

    #[test]
    fn unmet_operator_edge_is_refused_and_names_the_slug() {
        let s = step("engine-rs", "EN.11.K");
        let resolve_deps = |_repo: &str, _id: &str| {
            vec![DependencyEdge::Operator {
                slug: "operator-mac-mini-visit".to_string(),
            }]
        };
        // is_edge_met must never even be consulted for a non-Block edge.
        let is_met =
            |_repo: &str, _id: &str| panic!("is_edge_met must not be called for an operator edge");

        let err = check_dependencies(&s, &resolve_deps, &is_met).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("operator-mac-mini-visit"),
            "message should name the operator gate slug: {msg}"
        );
    }

    #[test]
    fn unmet_approval_edge_is_refused_and_names_the_slug() {
        let s = step("engine-rs", "EN.11.K");
        let resolve_deps = |_repo: &str, _id: &str| {
            vec![DependencyEdge::Approval {
                slug: "approve-release".to_string(),
            }]
        };
        let is_met =
            |_repo: &str, _id: &str| panic!("is_edge_met must not be called for an approval edge");

        let err = check_dependencies(&s, &resolve_deps, &is_met).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("approve-release"),
            "message should name the approval gate slug: {msg}"
        );
    }

    #[test]
    fn unmet_external_edge_is_refused_and_names_what() {
        let s = step("engine-rs", "EN.11.K");
        let resolve_deps = |_repo: &str, _id: &str| {
            vec![DependencyEdge::External {
                what: "DNS cutover complete".to_string(),
            }]
        };
        let is_met =
            |_repo: &str, _id: &str| panic!("is_edge_met must not be called for an external edge");

        let err = check_dependencies(&s, &resolve_deps, &is_met).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("DNS cutover complete"),
            "message should name the external dependency: {msg}"
        );
    }

    #[test]
    fn clearing_the_operator_gate_by_no_longer_returning_it_makes_the_block_admissible() {
        // The gate clears when the edge is removed from `resolve_depends_on`'s
        // result (mirroring mev removing it from `state.json`), not via any
        // runtime re-check of the operator edge itself.
        let s = step("engine-rs", "EN.11.K");
        let resolve_deps = |_repo: &str, _id: &str| Vec::new();
        let is_met = |_repo: &str, _id: &str| false;

        assert!(check_dependencies(&s, &resolve_deps, &is_met).is_ok());
    }

    #[tokio::test]
    async fn met_dependency_block_and_free_admission_starts_immediately() {
        let gate = AdmissionGate::with_default_policy();
        let s = step("engine-rs", "EN.10.B");
        let permit = tokio::time::timeout(Duration::from_millis(200), gate.acquire_for(&s))
            .await
            .expect("admission under the default limit must not block");
        drop(permit);
    }

    #[tokio::test]
    async fn at_capacity_the_run_waits_rather_than_proceeding_or_failing() {
        let gate = AdmissionGate::new(AdmissionControl::new(AdmissionPolicy {
            max_concurrent_terminal_runs: 1,
        }));
        let s1 = step("engine-rs", "EN.10.B");
        let s2 = step("mev", "MV.ticket.foo");

        let first = gate.acquire_for(&s1).await;
        assert_eq!(gate.available_permits(), 0);

        let gate2 = gate.clone();
        let admitted = Arc::new(AtomicUsize::new(0));
        let admitted_writer = admitted.clone();
        let waiter = tokio::spawn(async move {
            let _permit = gate2.acquire_for(&s2).await;
            admitted_writer.store(1, Ordering::SeqCst);
        });

        // Give the waiter every chance to (wrongly) proceed before release.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            admitted.load(Ordering::SeqCst),
            0,
            "a block at capacity must wait, not start"
        );
        assert!(!waiter.is_finished(), "waiter must not have failed either");

        drop(first);
        waiter
            .await
            .expect("queued block should proceed once a permit frees");
        assert_eq!(admitted.load(Ordering::SeqCst), 1);
    }

    // ── Frontier reader tests ───────────────────────────────────────────

    const SAMPLE_FRONTIER: &str = r#"{
        "derived_at": "2026-08-21T00:00:00-07:00",
        "entries": [
            {
                "roadmap": "orchestration-extensions",
                "lane": "engine-rs",
                "segment": 0,
                "repo": "engine-rs",
                "key": "engine-rs:EN.11.E",
                "id": "EN.11.E",
                "title": "Example head",
                "status": "open",
                "unmet_blocks": [],
                "unmet_gates": [],
                "startable": true
            },
            {
                "roadmap": "orchestration-extensions",
                "lane": "mev",
                "segment": 0,
                "repo": "mev",
                "key": "mev:MV.13.B",
                "id": "MV.13.B",
                "title": "Frontier compute",
                "status": "open",
                "unmet_blocks": ["engine-rs:EN.11.A"],
                "unmet_gates": ["operator:some-slug"],
                "startable": false
            },
            {
                "roadmap": "another-roadmap",
                "lane": "engine-rs",
                "segment": 0,
                "repo": "engine-rs",
                "key": "engine-rs:EN.12.A",
                "id": "EN.12.A",
                "title": "Same lane name, different roadmap",
                "status": "open",
                "unmet_blocks": [],
                "unmet_gates": [],
                "startable": true
            }
        ],
        "gate_ranks": [
            {
                "kind": "operator",
                "slug": "some-slug",
                "rank": 1,
                "gates": ["mev:MV.13.B"]
            }
        ]
    }"#;

    #[test]
    fn load_frontier_parses_the_real_artifact_shape() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("lane-frontier.json");
        std::fs::write(&path, SAMPLE_FRONTIER).expect("write fixture");

        let artifact = load_frontier(&path).expect("must parse");
        assert_eq!(artifact.derived_at, "2026-08-21T00:00:00-07:00");
        assert_eq!(artifact.entries.len(), 3);
        assert_eq!(artifact.gate_ranks.len(), 1);
        assert_eq!(artifact.gate_ranks[0].slug, "some-slug");
        assert_eq!(artifact.gate_ranks[0].gates, vec!["mev:MV.13.B"]);
    }

    #[test]
    fn find_lane_head_matches_on_roadmap_lane_segment_not_lane_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("lane-frontier.json");
        std::fs::write(&path, SAMPLE_FRONTIER).expect("write fixture");
        let artifact = load_frontier(&path).expect("must parse");

        // Two entries share lane "engine-rs" but differ by roadmap — matching on the
        // full tuple must disambiguate them.
        let head = find_lane_head(&artifact, "orchestration-extensions", "engine-rs", 0)
            .expect("must find the entry for this roadmap");
        assert_eq!(head.id, "EN.11.E");
        assert!(head.startable);

        let other_head = find_lane_head(&artifact, "another-roadmap", "engine-rs", 0)
            .expect("must find the entry for the other roadmap");
        assert_eq!(other_head.id, "EN.12.A");

        assert!(find_lane_head(&artifact, "orchestration-extensions", "engine-rs", 99).is_none());
    }

    #[test]
    fn find_lane_head_passes_through_unmet_gates_and_blocks_verbatim() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("lane-frontier.json");
        std::fs::write(&path, SAMPLE_FRONTIER).expect("write fixture");
        let artifact = load_frontier(&path).expect("must parse");

        let head = find_lane_head(&artifact, "orchestration-extensions", "mev", 0)
            .expect("must find mev's head");
        assert!(!head.startable);
        assert_eq!(head.unmet_blocks, vec!["engine-rs:EN.11.A"]);
        assert_eq!(head.unmet_gates, vec!["operator:some-slug"]);
    }

    #[test]
    fn missing_frontier_file_fails_loudly_naming_the_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("does-not-exist.json");

        let err = load_frontier(&path).unwrap_err();
        match &err {
            FrontierError::Missing { path: p } => assert_eq!(p, &path),
            other => panic!("expected Missing, got {other:?}"),
        }
        let msg = err.to_string();
        assert!(
            msg.contains(&path.display().to_string()),
            "message must name the path: {msg}"
        );
    }

    #[test]
    fn unparsable_frontier_file_fails_loudly_naming_the_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("lane-frontier.json");
        std::fs::write(&path, "{ not valid json").expect("write garbage");

        let err = load_frontier(&path).unwrap_err();
        match &err {
            FrontierError::Unparsable { path: p, .. } => assert_eq!(p, &path),
            other => panic!("expected Unparsable, got {other:?}"),
        }
        let msg = err.to_string();
        assert!(
            msg.contains(&path.display().to_string()),
            "message must name the path: {msg}"
        );
    }

    #[test]
    fn gate_ranks_are_consumed_as_opaque_data_not_recomputed() {
        // This reader has no rank-derivation logic at all — it only deserializes
        // gate_ranks[] as given. Pinning the field values round-trip is the test
        // that a future change hasn't quietly added a re-derivation.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("lane-frontier.json");
        std::fs::write(&path, SAMPLE_FRONTIER).expect("write fixture");
        let artifact = load_frontier(&path).expect("must parse");

        assert_eq!(artifact.gate_ranks[0].kind, "operator");
        assert_eq!(artifact.gate_ranks[0].rank, 1);
    }

    // ── Task 2: wiring the frontier into gates.rs without letting it override ──

    fn lane_step(
        roadmap: &str,
        lane: &str,
        segment: usize,
        repo: &str,
        block_id: &str,
    ) -> ChainStep {
        ChainStep {
            repo: repo.to_string(),
            block_id: block_id.to_string(),
            directives: None,
            roadmap: Some(roadmap.to_string()),
            lane: Some(lane.to_string()),
            segment: Some(segment),
            kind: StepKind::Block,
        }
    }

    #[test]
    fn frontier_lane_head_matches_a_step_from_resolve_lane_chain() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("lane-frontier.json");
        std::fs::write(&path, SAMPLE_FRONTIER).expect("write fixture");
        let artifact = load_frontier(&path).expect("must parse");

        let step = lane_step(
            "orchestration-extensions",
            "engine-rs",
            0,
            "engine-rs",
            "EN.11.E",
        );
        let head = frontier_lane_head(&step, &artifact).expect("must find the matching entry");
        assert_eq!(head.id, "EN.11.E");
        assert!(head.startable);
    }

    #[test]
    fn frontier_lane_head_is_none_for_an_explicit_chain_step() {
        // resolve_explicit_chain leaves roadmap/lane/segment all None — there is no lane
        // identity to look up, by construction.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("lane-frontier.json");
        std::fs::write(&path, SAMPLE_FRONTIER).expect("write fixture");
        let artifact = load_frontier(&path).expect("must parse");

        let step = step("engine-rs", "EN.11.E");
        assert!(step.roadmap.is_none());
        assert!(frontier_lane_head(&step, &artifact).is_none());
    }

    #[test]
    fn frontier_startable_true_does_not_short_circuit_check_dependencies() {
        // This is the exact live condition named in the block's Why: the frontier's own
        // engine-rs head is startable:true with unmet_gates:[] while a per-edge check
        // (standing in for the HELD-UNTIL operator decision the graph cannot express)
        // still refuses. check_step_with_frontier_advice must surface the refusal, not
        // the frontier's optimism.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("lane-frontier.json");
        std::fs::write(&path, SAMPLE_FRONTIER).expect("write fixture");
        let artifact = load_frontier(&path).expect("must parse");

        let s = lane_step(
            "orchestration-extensions",
            "engine-rs",
            0,
            "engine-rs",
            "EN.11.E",
        );
        let resolve_deps = |_repo: &str, _block_id: &str| {
            vec![DependencyEdge::Operator {
                slug: "some-other-gate".to_string(),
            }]
        };
        let is_edge_met = |_repo: &str, _block_id: &str| true;

        let (gate_result, head) =
            check_step_with_frontier_advice(&s, Some(&artifact), &resolve_deps, &is_edge_met);

        let head = head.expect("frontier entry must still be found");
        assert!(
            head.startable,
            "the frontier itself must say startable:true"
        );
        assert!(head.unmet_gates.is_empty());
        assert!(
            gate_result.is_err(),
            "check_dependencies must still refuse despite frontier startable:true"
        );
        match gate_result.unwrap_err() {
            GateError::UnmetDependency {
                edge: DependencyEdge::Operator { slug },
                ..
            } => assert_eq!(slug, "some-other-gate"),
            other => panic!("expected an Operator UnmetDependency, got {other:?}"),
        }
    }

    #[test]
    fn frontier_advice_is_none_when_no_artifact_is_supplied() {
        let s = lane_step(
            "orchestration-extensions",
            "engine-rs",
            0,
            "engine-rs",
            "EN.11.E",
        );
        let resolve_deps = |_repo: &str, _block_id: &str| Vec::new();
        let is_edge_met = |_repo: &str, _block_id: &str| true;

        let (gate_result, head) =
            check_step_with_frontier_advice(&s, None, &resolve_deps, &is_edge_met);
        assert!(gate_result.is_ok());
        assert!(head.is_none());
    }

    #[test]
    fn check_dependencies_still_passes_when_frontier_agrees() {
        // The agreement path: frontier says startable, and the live per-edge check has
        // nothing outstanding either. Both signals concur, and the gate's own Ok(()) is
        // what actually permits the start.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("lane-frontier.json");
        std::fs::write(&path, SAMPLE_FRONTIER).expect("write fixture");
        let artifact = load_frontier(&path).expect("must parse");

        let s = lane_step(
            "orchestration-extensions",
            "engine-rs",
            0,
            "engine-rs",
            "EN.11.E",
        );
        let resolve_deps = |_repo: &str, _block_id: &str| Vec::new();
        let is_edge_met = |_repo: &str, _block_id: &str| true;

        let (gate_result, head) =
            check_step_with_frontier_advice(&s, Some(&artifact), &resolve_deps, &is_edge_met);
        assert!(gate_result.is_ok());
        assert!(head.expect("must find entry").startable);
    }

    #[tokio::test]
    async fn releasing_admits_the_next_queued_block_in_order() {
        let gate = AdmissionGate::new(AdmissionControl::new(AdmissionPolicy {
            max_concurrent_terminal_runs: 1,
        }));
        let s = step("engine-rs", "EN.10.B");

        let first = gate.acquire_for(&s).await;
        let gate2 = gate.clone();
        let s2 = s.clone();
        let waiting = tokio::spawn(async move { gate2.acquire_for(&s2).await });

        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!waiting.is_finished());

        drop(first);
        let second = tokio::time::timeout(Duration::from_millis(200), waiting)
            .await
            .expect("second acquire should complete promptly after release")
            .expect("join should succeed");
        assert_eq!(gate.available_permits(), 0);
        drop(second);
        assert_eq!(gate.available_permits(), 1);
    }

    // ── Task 5: the permission gate ─────────────────────────────────────

    /// A closure double that records every [`OperatorGateRequest`] it is called
    /// with, so a test can assert both "called exactly once with this shape" and
    /// "never called at all".
    fn recording_author() -> (
        impl Fn(&OperatorGateRequest) -> Result<(), String>,
        Arc<std::sync::Mutex<Vec<OperatorGateRequest>>>,
    ) {
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = calls.clone();
        let author = move |edge: &OperatorGateRequest| {
            recorder.lock().unwrap().push(edge.clone());
            Ok(())
        };
        (author, calls)
    }

    /// AC "A permitted graded action proceeds with no behaviour change": `standard`
    /// permits `PushToMain` (permission.rs's own grading table) — `check_permission_gate`
    /// returns `Ok(())` and never invokes `author_operator_edge` at all.
    #[test]
    fn a_permitted_graded_action_proceeds_and_never_authors_an_edge() {
        let s = step("engine-rs", "EN.12.C");
        let (author, calls) = recording_author();

        let result =
            check_permission_gate(&s, GatedAction::PushToMain, PermissionProfile::Standard, &author);

        assert!(result.is_ok(), "a permitted action must proceed: {result:?}");
        assert!(
            calls.lock().unwrap().is_empty(),
            "author_operator_edge must not be called for a permitted action"
        );
    }

    /// AC "A denied graded action authors a {"type":"operator"} edge and STOPS the
    /// chain — it does not proceed": `locked` denies `InstallOnMini` — the gate
    /// authors an edge and returns `Err`.
    #[test]
    fn a_denied_graded_action_authors_an_edge_and_stops_the_chain() {
        let s = step("engine-rs", "EN.12.C");
        let (author, calls) = recording_author();

        let result = check_permission_gate(
            &s,
            GatedAction::InstallOnMini,
            PermissionProfile::Locked,
            &author,
        );

        let err = result.expect_err("a denied action must stop the chain, not proceed");
        match err {
            PermissionGateError::Denied {
                repo,
                block_id,
                action,
                profile,
                edge,
            } => {
                assert_eq!(repo, "engine-rs");
                assert_eq!(block_id, "EN.12.C");
                assert_eq!(action, GatedAction::InstallOnMini);
                assert_eq!(profile, PermissionProfile::Locked);
                assert_eq!(edge.slug, "permission-install_on_mini");
            }
            other => panic!("expected Denied, got {other:?}"),
        }

        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 1, "exactly one edge must be authored");
        assert_eq!(recorded[0].slug, "permission-install_on_mini");
    }

    /// AC "The authored edge's `exit` names an artifact ... its `slug` is stable
    /// across every block the same denial gates": two different blocks, both denied
    /// the same action, author edges sharing the identical `slug` — the join key
    /// mev's operator-gate machinery groups on — and `exit` reads as an artifact
    /// path/existence claim, never a restatement of the work.
    #[test]
    fn the_authored_edges_slug_is_stable_across_every_block_the_same_denial_gates() {
        let s1 = step("engine-rs", "EN.12.C");
        let s2 = step("mev", "MV.ticket.unrelated");
        let (author, calls) = recording_author();

        let _ = check_permission_gate(
            &s1,
            GatedAction::CrossRepoWrite,
            PermissionProfile::Locked,
            &author,
        );
        let _ = check_permission_gate(
            &s2,
            GatedAction::CrossRepoWrite,
            PermissionProfile::Locked,
            &author,
        );

        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 2);
        assert_eq!(
            recorded[0].slug, recorded[1].slug,
            "the same denied action must raise the SAME operator-gate slug regardless \
             of which block hit it, so mev's clustering treats them as one gate"
        );
        assert!(
            recorded[0].exit.contains("exists"),
            "exit must name an artifact whose existence ends the gate: {}",
            recorded[0].exit
        );
        assert!(
            !recorded[0].exit.contains("CrossRepoWrite is complete"),
            "exit must never restate the denied work itself"
        );
    }

    /// AC "`clear_operator_gate` is unreachable from this path at EVERY profile
    /// including `unrestricted` — asserted, not assumed": drive `check_permission_gate`
    /// itself (not merely `permission::decide` in isolation) with `ClearOperatorGate`
    /// across all three profiles and confirm every one denies (raises the gate rather
    /// than permitting).
    #[test]
    fn clear_operator_gate_is_denied_through_this_gate_at_every_profile() {
        for profile in [
            PermissionProfile::Locked,
            PermissionProfile::Standard,
            PermissionProfile::Unrestricted,
        ] {
            let s = step("engine-rs", "EN.12.C");
            let (author, _calls) = recording_author();
            let result =
                check_permission_gate(&s, GatedAction::ClearOperatorGate, profile, &author);
            assert!(
                result.is_err(),
                "ClearOperatorGate must never be permitted through this gate at profile \
                 {profile:?}"
            );
        }
    }

    /// Companion source-scan guard: this module never references mev's
    /// gate-closing verb (snake_case or CamelCase form) at all — the concrete proof
    /// that clearing a gate is not merely denied by `decide()` but structurally
    /// unreachable from this file, since it contains no call capable of doing it.
    #[test]
    fn gates_rs_never_references_the_gate_closing_verb() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/workflows/orchestration/gates.rs");
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("failed to read {path:?}: {err}"));
        // Built via `concat!` rather than as string literals so this assertion's OWN
        // banned-substring text (necessarily naming what it forbids) doesn't trip
        // itself when scanning this very file — the same discipline
        // `permission.rs`'s `no_from_str_or_from_string_impl_for_the_closed_enums`
        // guard and `close_block.rs`'s `never_writes_state_json_directly` guard both
        // already follow.
        let snake_needle = ["close", "_operator", "_gate"].concat();
        let camel_needle = ["Close", "OperatorGate"].concat();
        assert!(
            !source.contains(&snake_needle) && !source.contains(&camel_needle),
            "gates.rs must never reference mev's operator-gate-closing verb — \
             clearing an operator gate is a human decision routed through the mev \
             CLI directly, never something this file can invoke"
        );
    }

    /// Companion guard: this file contains no in-process `state.json` write of its
    /// own (no `fs::write`/`std::fs::write` call anywhere in production code) — the
    /// authored edge always goes through the injected `author_operator_edge` seam,
    /// mirroring `close_block.rs`'s `never_writes_state_json_directly` guard.
    #[test]
    fn gates_rs_never_writes_state_json_in_process() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/workflows/orchestration/gates.rs");
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("failed to read {path:?}: {err}"));
        let production_code = source
            .split_once("\n#[cfg(test)]\n")
            .map(|(before, _)| before)
            .expect("this module has a #[cfg(test)] boundary");
        assert!(
            !production_code.contains("fs::write"),
            "gates.rs's production code must never write state.json in-process — the \
             authored operator edge must go through author_operator_edge, mirroring \
             close_block.rs's own never_writes_state_json_directly guard"
        );
    }

    /// AC "on a denied action ... the caller's `author_operator_edge` seam ... failing
    /// is a distinct, reported failure": when the injected closure itself errors, the
    /// chain still stops, and the failure is reported as
    /// [`PermissionGateError::EdgeAuthorFailed`], not silently swallowed or conflated
    /// with an ordinary [`PermissionGateError::Denied`].
    #[test]
    fn a_failing_author_seam_still_stops_the_chain_with_a_distinct_error() {
        let s = step("engine-rs", "EN.12.C");
        let failing_author =
            |_edge: &OperatorGateRequest| Err("mev CLI not found on PATH".to_string());

        let result = check_permission_gate(
            &s,
            GatedAction::PushToMain,
            PermissionProfile::Locked,
            &failing_author,
        );

        match result {
            Err(PermissionGateError::EdgeAuthorFailed { reason, .. }) => {
                assert_eq!(reason, "mev CLI not found on PATH");
            }
            other => panic!("expected EdgeAuthorFailed, got {other:?}"),
        }
    }
}
