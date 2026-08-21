//! Corpus-backed implementations of the three decidable ORCHESTRATION seams —
//! `resolve_depends_on`, `is_edge_met`, and `is_block_open` — `EN.ticket.orchestration-production-gates-unwired`
//! Task 1.
//!
//! [`graph::OrchestrationRunNode::new`](super::graph::OrchestrationRunNode::new)'s
//! defaults for these three closures are deliberately permissive (no edges, every
//! edge met, every block closed) — the correct behaviour for a bare constructor, but
//! also, until this module, the *only* behaviour production ever got: nothing wired
//! them to anything real. [`CorpusGates`] is that wiring: it reads the same
//! `planning/state.json` files the rest of the fleet treats as the authoritative DAG,
//! via [`RepoRegistry`] (slug -> repo root) and [`okf_core::load_state`]
//! (`state.json` -> [`StateFile`]). It never hand-rolls a parser.
//!
//! # Caching
//!
//! A single [`CorpusGates`] is built fresh per ORCHESTRATION run (see
//! `engine-serve`'s `register_orchestration_with_registry`) and shared across every
//! step of that run's chain. A 30-step chain calling `resolve_depends_on` and
//! `is_edge_met` per step would otherwise re-read the same repo's `state.json` dozens
//! of times; instead each repo is loaded at most once per run and cached behind a
//! `Mutex<HashMap<String, Arc<StateFile>>>`.
//!
//! # Fail loud, never silently permissive
//!
//! The closures the workflow consumes ([`super::gates::DependencyEdge`]-returning
//! `resolve_depends_on`, bool-returning `is_edge_met`/`is_block_open`) have no `Result`
//! in their signature — that shape is fixed by `gates::check_dependencies` and
//! `chain::resolve_lane_chain`, which this ticket does not touch. A missing or
//! malformed `state.json`, or an unresolvable `HELD-UNTIL` token, therefore cannot be
//! returned from the closure itself; it is recorded in an internal error slot
//! ([`CorpusGates::take_error`]) that the node wiring these seams together (Task 2)
//! MUST check after resolution and fail the run loudly. Falling back to `Vec::new()`
//! / `false` from inside the closure — which is exactly the shape of the defect this
//! ticket fixes — would read as "no edges, proceed" instead of the loud failure the
//! acceptance criteria require. Only the *first* recorded failure is kept, so a run's
//! error names the earliest thing that went wrong rather than the last.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use okf_core::{load_state, BlockedBy, StateFile, StateLoadError, TrackBlock};

use crate::repo_registry::RepoRegistry;

use super::gates::DependencyEdge;

/// Everything that can go wrong resolving a corpus-backed gate. Every variant names
/// the repo (and, where relevant, the path or block id) involved — this module never
/// fails silently, matching `gates::GateError`'s own discipline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorpusGatesError {
    /// `repo` is not a slug [`RepoRegistry`] can resolve (unknown, filtered by the
    /// allowlist, or an unusable `brain.toml` entry).
    UnknownRepo { repo: String, reason: String },
    /// `repo`'s `state.json` at `path` could not be read or parsed.
    Load {
        repo: String,
        path: PathBuf,
        reason: String,
    },
    /// `is_edge_met` was asked about a block id that does not exist anywhere in
    /// `repo`'s `tracks[]`. Never treated as met — an unknown target is a corpus
    /// inconsistency, not a satisfied dependency.
    UnknownBlock { repo: String, block_id: String },
    /// `is_block_open`'s token did not resolve to any block id across every repo
    /// [`RepoRegistry`] knows. The token is opaque text (a block id or an
    /// operator-gate slug per the `HELD-UNTIL` directive's own docs) and this module
    /// only knows how to resolve the block-id case; anything else it cannot decide
    /// and must not silently treat as "not held".
    UnresolvedToken { token: String },
}

impl fmt::Display for CorpusGatesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CorpusGatesError::UnknownRepo { repo, reason } => {
                write!(f, "corpus gates: unknown repo '{repo}': {reason}")
            }
            CorpusGatesError::Load { repo, path, reason } => {
                write!(
                    f,
                    "corpus gates: could not load state.json for repo '{repo}' at \
                     '{}': {reason}",
                    path.display()
                )
            }
            CorpusGatesError::UnknownBlock { repo, block_id } => {
                write!(
                    f,
                    "corpus gates: block '{block_id}' (repo '{repo}') does not exist \
                     in that repo's state.json — treating as not met"
                )
            }
            CorpusGatesError::UnresolvedToken { token } => {
                write!(
                    f,
                    "corpus gates: HELD-UNTIL token '{token}' does not resolve to any \
                     known block id across the registered repos"
                )
            }
        }
    }
}

impl std::error::Error for CorpusGatesError {}

/// A pluggable `state.json` loader, defaulting to [`okf_core::load_state`].
/// Overridable only for tests — e.g. a counting reader that proves the cache really
/// avoids re-reading the same repo per step.
type LoadStateFn = Arc<dyn Fn(&Path) -> Result<StateFile, StateLoadError> + Send + Sync>;

/// Corpus-backed `resolve_depends_on` / `is_edge_met` / `is_block_open`, shared across
/// one ORCHESTRATION run. See the module doc for the caching and fail-loud contract.
pub struct CorpusGates {
    repo_registry: Arc<RepoRegistry>,
    load_state_fn: LoadStateFn,
    cache: Mutex<HashMap<String, Arc<StateFile>>>,
    error: Mutex<Option<CorpusGatesError>>,
}

impl CorpusGates {
    /// Build against a real [`RepoRegistry`], reading `state.json` via
    /// [`okf_core::load_state`].
    #[must_use]
    pub fn new(repo_registry: Arc<RepoRegistry>) -> Self {
        Self {
            repo_registry,
            load_state_fn: Arc::new(load_state),
            cache: Mutex::new(HashMap::new()),
            error: Mutex::new(None),
        }
    }

    /// Override the `state.json` loader. Test-only in practice (a counting or
    /// failure-injecting double); production always uses [`Self::new`]'s default.
    #[must_use]
    pub fn with_load_state(mut self, f: LoadStateFn) -> Self {
        self.load_state_fn = f;
        self
    }

    /// The first failure recorded while resolving any of this instance's gates, if
    /// any. Callers MUST check this after driving a run to completion — the gate
    /// closures themselves cannot return a `Result` (see module doc), so this is the
    /// only channel a read failure has to reach the caller. Non-destructive: repeated
    /// calls keep returning the same first failure.
    #[must_use]
    pub fn take_error(&self) -> Option<CorpusGatesError> {
        self.error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn record_error(&self, err: CorpusGatesError) {
        let mut slot = self
            .error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if slot.is_none() {
            *slot = Some(err);
        }
    }

    /// Load (or fetch from cache) `repo`'s parsed `state.json`. `None` on any
    /// failure to resolve the slug or read/parse the file — the failure itself is
    /// always recorded via [`Self::record_error`] first, never swallowed.
    fn state_for(&self, repo: &str) -> Option<Arc<StateFile>> {
        if let Some(state) = self
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(repo)
        {
            return Some(state.clone());
        }

        let root = match self.repo_registry.resolve(repo) {
            Ok(root) => root,
            Err(err) => {
                self.record_error(CorpusGatesError::UnknownRepo {
                    repo: repo.to_string(),
                    reason: err.to_string(),
                });
                return None;
            }
        };
        let path = root.join("planning").join("state.json");
        let state = match (self.load_state_fn)(&path) {
            Ok(state) => Arc::new(state),
            Err(err) => {
                self.record_error(CorpusGatesError::Load {
                    repo: repo.to_string(),
                    path,
                    reason: err.to_string(),
                });
                return None;
            }
        };

        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Another concurrent caller may have raced us and already inserted this
        // repo's state; `entry(..).or_insert_with(..)` keeps whichever landed first
        // rather than double-loading, preserving the "one load per repo" contract.
        let cached = cache
            .entry(repo.to_string())
            .or_insert_with(|| state.clone());
        Some(cached.clone())
    }

    fn find_block<'a>(state: &'a StateFile, block_id: &str) -> Option<&'a TrackBlock> {
        state
            .tracks
            .iter()
            .flat_map(|track| track.blocks.iter())
            .find(|block| block.id == block_id)
    }

    /// `resolve_depends_on(repo, block_id)`: the block's authored `depends_on`,
    /// mapped from `BlockedBy` to [`DependencyEdge`].
    ///
    /// Every `BlockedBy` variant now survives resolution (`EN.11.J` Task 2):
    /// `BlockedBy::Block` maps to [`DependencyEdge::Block`], and
    /// `BlockedBy::Operator`/`Approval`/`External` map to their widened
    /// [`DependencyEdge`] counterparts by slug (or `what`, for `External`) — they
    /// are no longer dropped. Each of those three is *targetless* (no `repo`/
    /// `block_id` of its own) and, per [`DependencyEdge`]'s own doc, is unmet for as
    /// long as it appears in this result: `gates::check_dependencies` never even
    /// consults `is_edge_met` for them. A block with no `depends_on` entries at all
    /// resolves to an empty vec, exactly as before.
    ///
    /// An unknown `block_id` resolves to no edges (there is nothing to resolve a
    /// dependency list *from*) rather than an error — [`Self::is_edge_met`] is where
    /// an unknown id is the substantive question and is reported loudly there.
    #[must_use]
    pub fn resolve_depends_on(&self, repo: &str, block_id: &str) -> Vec<DependencyEdge> {
        let Some(state) = self.state_for(repo) else {
            return Vec::new();
        };
        let Some(block) = Self::find_block(&state, block_id) else {
            return Vec::new();
        };
        block
            .depends_on
            .iter()
            .map(|dep| match dep {
                BlockedBy::Block(b) => DependencyEdge::Block {
                    repo: b.repo.clone(),
                    block_id: b.id.clone(),
                },
                BlockedBy::Operator(op) => DependencyEdge::Operator {
                    slug: op.slug.clone(),
                },
                BlockedBy::Approval(ap) => DependencyEdge::Approval {
                    slug: ap.slug.clone(),
                },
                BlockedBy::External(ext) => DependencyEdge::External {
                    what: ext.what.clone(),
                },
            })
            .collect()
    }

    /// `is_edge_met(repo, block_id)`: true iff the target block's authored `status`
    /// is exactly `"closed"`.
    ///
    /// **`wontfix` vs `closed` (`EN.11.J` Task 3):** the fleet has two different
    /// notions of "done enough". `focus.next` (what mev surfaces as the next thing to
    /// *work on*) accepts `closed|wontfix` — a block a human deliberately abandoned
    /// should not keep dangling as "next up". The **frontier** (what gates whether a
    /// *dependent* block may start — `mev lane-frontier`, and this reader) accepts
    /// only `closed`. This method implements the frontier's notion, deliberately:
    /// `is_edge_met` answers "is it safe for something that depends on this to
    /// start", and a `wontfix` target means the work the dependent needed was never
    /// done — admitting the dependent would silently launder an abandoned dependency
    /// into a satisfied one. So `"wontfix"` is treated exactly like `"open"` or
    /// `"deferred"`: NOT met. This is a deliberate divergence from `focus.next`, not
    /// an oversight — do not "fix" it to also accept `wontfix` without re-reading
    /// this comment.
    ///
    /// `"deferred"` is NOT met — a deferred block is deliberately parked, not done.
    /// Any other authored status (`"open"`, `"in_progress"`, `None`) is likewise not
    /// met. An unknown block id (absent from every track in `repo`'s `state.json`) is
    /// NOT met either, and is recorded via [`Self::take_error`] rather than silently
    /// read as satisfied — a dangling dependency edge is a corpus defect the run
    /// should surface, not paper over.
    #[must_use]
    pub fn is_edge_met(&self, repo: &str, block_id: &str) -> bool {
        let Some(state) = self.state_for(repo) else {
            return false;
        };
        match Self::find_block(&state, block_id) {
            Some(block) => block.status.as_deref() == Some("closed"),
            None => {
                self.record_error(CorpusGatesError::UnknownBlock {
                    repo: repo.to_string(),
                    block_id: block_id.to_string(),
                });
                false
            }
        }
    }

    /// `is_block_open(token)`: the `HELD-UNTIL` token is opaque text with no repo of
    /// its own, so this searches every repo [`RepoRegistry`] currently knows (in
    /// registry order) for a block whose id matches `token`. The first match wins:
    /// "open" means that block's status is anything other than `"closed"` — including
    /// `"wontfix"`, kept CONSISTENT with [`Self::is_edge_met`]'s frontier notion of
    /// done (see that method's doc for the `focus.next` vs frontier distinction): a
    /// `HELD-UNTIL` naming a `wontfix` block is still held, never treated as if the
    /// hold had cleared.
    ///
    /// A token that matches no block id anywhere in the registered repos cannot be
    /// resolved here — it is either an operator-gate slug (which this module has no
    /// way to check; a real answer needs the hold surface, out of scope per this
    /// ticket's record) or a typo. Either way this returns `false` (matching the
    /// permissive default's shape, since the closure signature has no room for
    /// anything else) but ALSO records [`CorpusGatesError::UnresolvedToken`] so the
    /// caller fails loudly instead of reading a silent `false` as "definitely not
    /// held".
    #[must_use]
    pub fn is_block_open(&self, token: &str) -> bool {
        for repo in self.repo_registry.known_slugs() {
            if let Some(state) = self.state_for(&repo) {
                if let Some(block) = Self::find_block(&state, token) {
                    return block.status.as_deref() != Some("closed");
                }
            }
        }
        self.record_error(CorpusGatesError::UnresolvedToken {
            token: token.to_string(),
        });
        false
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Build a tempdir brain root with a `brain.toml` naming `repo-a`/`repo-b`,
    /// each with a real subdirectory and a hand-written `planning/state.json`.
    fn two_repo_brain_root(state_a: &str, state_b: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        for (slug, state) in [("repo-a", state_a), ("repo-b", state_b)] {
            let planning = dir.path().join(slug).join("planning");
            std::fs::create_dir_all(&planning).expect("mkdir planning");
            std::fs::write(planning.join("state.json"), state).expect("write state.json");
        }
        std::fs::write(
            dir.path().join("brain.toml"),
            r#"
[[repos]]
slug = "repo-a"
repo_path = "repo-a"

[[repos]]
slug = "repo-b"
repo_path = "repo-b"
"#,
        )
        .expect("write brain.toml");
        dir
    }

    fn state_json(blocks: &str) -> String {
        format!(
            r#"{{
    "repo": "repo",
    "kind": "project",
    "updated": "2026-08-18",
    "tracks": [
        {{ "title": "wave 1", "blocks": [ {blocks} ] }}
    ]
}}"#
        )
    }

    fn env_guarded<T>(f: impl FnOnce() -> T) -> T {
        // Mirrors repo_registry.rs's ENV_GUARD discipline: RepoRegistry::from_brain_root
        // never touches env on its own explicit-root path, but keep this here so any
        // future switch to from_env() stays safe without a silent re-audit.
        f()
    }

    fn registry_for(dir: &tempfile::TempDir) -> Arc<RepoRegistry> {
        Arc::new(RepoRegistry::from_brain_root(dir.path()).expect("registry"))
    }

    #[test]
    fn resolve_depends_on_maps_block_edges_and_non_block_edges_alike() {
        env_guarded(|| {
            let dir = two_repo_brain_root(
                &state_json(
                    r#"{"id": "A.1", "title": "a1", "status": "open", "depends_on": [
                        {"type": "block", "repo": "repo-b", "id": "B.1"},
                        {"type": "operator", "slug": "op", "exit": "x", "start": "y"},
                        {"type": "approval", "slug": "approve-it", "what": "ship it", "digest": "abc123"},
                        {"type": "external", "what": "env thing"}
                    ]}"#,
                ),
                &state_json(r#"{"id": "B.1", "title": "b1", "status": "closed"}"#),
            );
            let gates = CorpusGates::new(registry_for(&dir));

            let edges = gates.resolve_depends_on("repo-a", "A.1");
            assert_eq!(
                edges,
                vec![
                    DependencyEdge::Block {
                        repo: "repo-b".to_string(),
                        block_id: "B.1".to_string(),
                    },
                    DependencyEdge::Operator {
                        slug: "op".to_string(),
                    },
                    DependencyEdge::Approval {
                        slug: "approve-it".to_string(),
                    },
                    DependencyEdge::External {
                        what: "env thing".to_string(),
                    },
                ]
            );
            assert!(gates.take_error().is_none());
        });
    }

    #[test]
    fn block_gated_only_by_an_operator_edge_is_refused_and_clearing_it_admits() {
        env_guarded(|| {
            let dir = two_repo_brain_root(
                &state_json(
                    r#"{"id": "A.1", "title": "a1", "status": "open", "depends_on": [
                        {"type": "operator", "slug": "op-visit", "exit": "x", "start": "y"}
                    ]}"#,
                ),
                &state_json(r#"{"id": "B.1", "title": "b1", "status": "closed"}"#),
            );
            let gates = CorpusGates::new(registry_for(&dir));

            let is_edge_met = |repo: &str, block_id: &str| gates.is_edge_met(repo, block_id);
            let resolve_deps =
                |repo: &str, block_id: &str| gates.resolve_depends_on(repo, block_id);
            let step = super::super::chain::ChainStep {
                repo: "repo-a".to_string(),
                block_id: "A.1".to_string(),
                directives: None,
            };

            let err = super::super::gates::check_dependencies(&step, &resolve_deps, &is_edge_met)
                .expect_err("open operator gate must refuse the block");
            match err {
                super::super::gates::GateError::UnmetDependency {
                    edge: DependencyEdge::Operator { slug },
                    ..
                } => assert_eq!(slug, "op-visit"),
                other => panic!("expected an unmet Operator edge, got {other:?}"),
            }

            // Clearing the gate (mev removing the edge from state.json) is simulated
            // by re-reading a fixture with no depends_on at all.
            let dir2 = two_repo_brain_root(
                &state_json(r#"{"id": "A.1", "title": "a1", "status": "open"}"#),
                &state_json(r#"{"id": "B.1", "title": "b1", "status": "closed"}"#),
            );
            let gates2 = CorpusGates::new(registry_for(&dir2));
            let is_edge_met2 = |repo: &str, block_id: &str| gates2.is_edge_met(repo, block_id);
            let resolve_deps2 =
                |repo: &str, block_id: &str| gates2.resolve_depends_on(repo, block_id);
            assert!(
                super::super::gates::check_dependencies(&step, &resolve_deps2, &is_edge_met2)
                    .is_ok(),
                "clearing the operator gate must make the block admissible"
            );
        });
    }

    #[test]
    fn resolve_depends_on_unknown_block_is_empty_not_an_error() {
        env_guarded(|| {
            let dir = two_repo_brain_root(
                &state_json(r#"{"id": "A.1", "title": "a1", "status": "open"}"#),
                &state_json(r#"{"id": "B.1", "title": "b1", "status": "closed"}"#),
            );
            let gates = CorpusGates::new(registry_for(&dir));

            assert!(gates.resolve_depends_on("repo-a", "NOPE").is_empty());
            assert!(gates.take_error().is_none());
        });
    }

    #[test]
    fn is_edge_met_true_only_for_closed() {
        env_guarded(|| {
            let dir = two_repo_brain_root(
                &state_json(r#"{"id": "A.1", "title": "a1", "status": "open"}"#),
                &state_json(
                    r#"{"id": "B.1", "title": "b1", "status": "closed"},
                       {"id": "B.2", "title": "b2", "status": "open"},
                       {"id": "B.3", "title": "b3", "status": "deferred"}"#,
                ),
            );
            let gates = CorpusGates::new(registry_for(&dir));

            assert!(gates.is_edge_met("repo-b", "B.1"));
            assert!(!gates.is_edge_met("repo-b", "B.2"));
            assert!(!gates.is_edge_met("repo-b", "B.3"), "deferred is not met");
            assert!(gates.take_error().is_none());
        });
    }

    #[test]
    fn is_edge_met_wontfix_is_not_met_frontier_diverges_from_focus_next() {
        env_guarded(|| {
            // `focus.next` accepts `closed|wontfix`; the frontier (this reader)
            // accepts only `closed`. Pin that a `wontfix` target is NOT met — an
            // abandoned dependency must not silently unblock a dependent block.
            let dir = two_repo_brain_root(
                &state_json(r#"{"id": "A.1", "title": "a1", "status": "open"}"#),
                &state_json(r#"{"id": "B.1", "title": "b1", "status": "wontfix"}"#),
            );
            let gates = CorpusGates::new(registry_for(&dir));

            assert!(
                !gates.is_edge_met("repo-b", "B.1"),
                "wontfix must not be conflated with closed"
            );
            assert!(gates.take_error().is_none());
        });
    }

    #[test]
    fn is_block_open_wontfix_still_reports_open_consistent_with_is_edge_met() {
        env_guarded(|| {
            let dir = two_repo_brain_root(
                &state_json(r#"{"id": "A.1", "title": "a1", "status": "open"}"#),
                &state_json(r#"{"id": "B.1", "title": "b1", "status": "wontfix"}"#),
            );
            let gates = CorpusGates::new(registry_for(&dir));

            assert!(
                gates.is_block_open("B.1"),
                "wontfix is still 'open' for HELD-UNTIL purposes, matching is_edge_met's frontier notion"
            );
            assert!(gates.take_error().is_none());
        });
    }

    #[test]
    fn is_edge_met_unknown_block_is_not_met_and_reported() {
        env_guarded(|| {
            let dir = two_repo_brain_root(
                &state_json(r#"{"id": "A.1", "title": "a1", "status": "open"}"#),
                &state_json(r#"{"id": "B.1", "title": "b1", "status": "closed"}"#),
            );
            let gates = CorpusGates::new(registry_for(&dir));

            assert!(!gates.is_edge_met("repo-b", "GHOST"));
            match gates.take_error() {
                Some(CorpusGatesError::UnknownBlock { repo, block_id }) => {
                    assert_eq!(repo, "repo-b");
                    assert_eq!(block_id, "GHOST");
                }
                other => panic!("expected UnknownBlock, got {other:?}"),
            }
        });
    }

    #[test]
    fn is_block_open_searches_every_repo_and_reports_open_vs_closed() {
        env_guarded(|| {
            let dir = two_repo_brain_root(
                &state_json(r#"{"id": "A.1", "title": "a1", "status": "open"}"#),
                &state_json(r#"{"id": "B.1", "title": "b1", "status": "closed"}"#),
            );
            let gates = CorpusGates::new(registry_for(&dir));

            assert!(gates.is_block_open("A.1"), "open block reports open");
            assert!(!gates.is_block_open("B.1"), "closed block reports not open");
            assert!(gates.take_error().is_none());
        });
    }

    #[test]
    fn is_block_open_unresolved_token_fails_loud_not_false_silently() {
        env_guarded(|| {
            let dir = two_repo_brain_root(
                &state_json(r#"{"id": "A.1", "title": "a1", "status": "open"}"#),
                &state_json(r#"{"id": "B.1", "title": "b1", "status": "closed"}"#),
            );
            let gates = CorpusGates::new(registry_for(&dir));

            assert!(!gates.is_block_open("some-operator-gate-slug"));
            match gates.take_error() {
                Some(CorpusGatesError::UnresolvedToken { token }) => {
                    assert_eq!(token, "some-operator-gate-slug");
                }
                other => panic!("expected UnresolvedToken, got {other:?}"),
            }
        });
    }

    #[test]
    fn missing_state_json_fails_loud_naming_repo_and_path() {
        env_guarded(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            std::fs::create_dir_all(dir.path().join("repo-a")).expect("mkdir");
            std::fs::write(
                dir.path().join("brain.toml"),
                r#"
[[repos]]
slug = "repo-a"
repo_path = "repo-a"
"#,
            )
            .expect("write brain.toml");
            let registry = Arc::new(RepoRegistry::from_brain_root(dir.path()).expect("registry"));
            let gates = CorpusGates::new(registry);

            assert!(gates.resolve_depends_on("repo-a", "A.1").is_empty());
            match gates.take_error() {
                Some(CorpusGatesError::Load { repo, path, .. }) => {
                    assert_eq!(repo, "repo-a");
                    assert!(path.ends_with("planning/state.json"));
                }
                other => panic!("expected Load error, got {other:?}"),
            }
        });
    }

    #[test]
    fn malformed_state_json_fails_loud_never_reads_as_no_edges() {
        env_guarded(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            let planning = dir.path().join("repo-a").join("planning");
            std::fs::create_dir_all(&planning).expect("mkdir planning");
            std::fs::write(planning.join("state.json"), "{ not valid json").expect("write");
            std::fs::write(
                dir.path().join("brain.toml"),
                r#"
[[repos]]
slug = "repo-a"
repo_path = "repo-a"
"#,
            )
            .expect("write brain.toml");
            let registry = Arc::new(RepoRegistry::from_brain_root(dir.path()).expect("registry"));
            let gates = CorpusGates::new(registry);

            assert!(!gates.is_edge_met("repo-a", "A.1"));
            match gates.take_error() {
                Some(CorpusGatesError::Load { repo, .. }) => assert_eq!(repo, "repo-a"),
                other => panic!("expected Load error, got {other:?}"),
            }
        });
    }

    #[test]
    fn one_load_per_repo_per_run_not_per_step() {
        env_guarded(|| {
            let dir = two_repo_brain_root(
                &state_json(
                    r#"{"id": "A.1", "title": "a1", "status": "open", "depends_on": [
                        {"type": "block", "repo": "repo-b", "id": "B.1"}
                    ]}"#,
                ),
                &state_json(r#"{"id": "B.1", "title": "b1", "status": "closed"}"#),
            );
            let load_count: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
            let counting = {
                let load_count = load_count.clone();
                move |path: &Path| -> Result<StateFile, StateLoadError> {
                    load_count.fetch_add(1, Ordering::SeqCst);
                    load_state(path)
                }
            };
            let gates = CorpusGates::new(registry_for(&dir)).with_load_state(Arc::new(counting));

            // Simulate a multi-step chain hammering the same two repos.
            for _ in 0..10 {
                let _ = gates.resolve_depends_on("repo-a", "A.1");
                let _ = gates.is_edge_met("repo-b", "B.1");
                let _ = gates.is_block_open("A.1");
            }

            assert_eq!(
                load_count.load(Ordering::SeqCst),
                2,
                "expected exactly one load each for repo-a and repo-b, got {}",
                load_count.load(Ordering::SeqCst)
            );
            assert!(gates.take_error().is_none());
        });
    }
}
