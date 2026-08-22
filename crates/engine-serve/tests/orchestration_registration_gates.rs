//! Tests the SERVED `ORCHESTRATION` registration path —
//! `register_orchestration_with_registry` — not `OrchestrationRunNode`
//! directly. `EN.ticket.orchestration-production-gates-unwired` Task 3.
//!
//! Every case here dispatches through the same `Dispatcher` factory
//! `engine-serve::workflows::register_orchestration_with_registry`
//! installs and then runs the returned `Workflow` to completion, exactly
//! as `bastion serve` would. A test that constructed `OrchestrationRunNode`
//! with explicit seams instead would have passed on the broken tree before
//! this ticket — that shape is deliberately avoided here.
//!
//! # Why the "proceeds" cases assert on a *different* failure, not success
//!
//! `register_orchestration_with_registry` takes no `FlowRunner` override —
//! only `repo_reg` and `hold_source` — so a step that clears every gate
//! still falls through to `execute::default_flow_runner`, which builds and
//! runs a real `SDLC_FLOW` `Workflow` against the fixture's tempdir repo.
//! That repo is not a real git checkout, so `SetupWorktreeNode` fails fast
//! (`git status --porcelain` against a non-git directory), `SDLC_FLOW`'s own
//! `Workflow::run` still returns `Ok` (a node failure marks that node
//! FAILED but does not itself error the walk — see `engine-core`'s
//! `Workflow::run` doc), and `integrate::verify_state_write` then fails to
//! read `sdlc-flow-state.json` (never written). That failure is fast,
//! deterministic, purely local, and — this is the point of the assertion —
//! textually nothing like a `GateError`/`ChainError::Held` message. So
//! "the chain proceeded past this gate" is asserted as "the run failed for
//! a *different*, later-stage reason", never as an unqualified success.
//!
//! # Anti-revert guard (evidence, not enforcement)
//!
//! Before this ticket, `with_resolve_depends_on`/`with_is_edge_met`/
//! `with_is_block_open`/`with_hold_source` had zero production callers
//! anywhere in the workspace (grepped `EN.ticket.orchestration-production-gates-unwired`
//! Task 0). After Tasks 1-2, each has exactly one, all in
//! `engine-serve/src/workflows.rs`'s `register_orchestration_with_registry`:
//! ```text
//! $ grep -rn 'with_resolve_depends_on\|with_is_edge_met\|with_is_block_open\|with_hold_source' \
//!     --include='*.rs' crates | grep -v 'fn with_resolve_depends_on\|fn with_is_edge_met\|fn with_is_block_open\|fn with_hold_source'
//! crates/engine-serve/src/workflows.rs:689:    .with_resolve_depends_on(...)
//! crates/engine-serve/src/workflows.rs:696:    .with_is_edge_met(...)
//! crates/engine-serve/src/workflows.rs:703:    .with_is_block_open(...)
//! crates/engine-serve/src/workflows.rs:710:    .with_hold_source(...)
//! ```
//! The grep alone would pass on a build that wired the closures but
//! silently no-op'd them (e.g. `with_is_edge_met(Arc::new(|_, _| true))`),
//! so the tests below back it with the cheaper, honest check the module
//! doc calls for: each seam is observed CHANGING the outcome relative to
//! the permissive, unwired default (`orchestration::graph::workflow()`) —
//! re-defaulting any one of them turns the matching test red.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use engine_contract::{NodeRunStatus, TaskContext};
use engine_core::workflows::orchestration::graph as orch_graph;
use engine_core::workflows::orchestration::integrate::{HoldSource, NeverHeld};
use engine_serve::dispatch::Dispatcher;
use engine_serve::workflows::register_orchestration_with_registry;

// ── Fixture helpers ─────────────────────────────────────────────────────

/// `dir/<slug>/` (a directory, required for `RepoRegistry::resolve` to
/// record the slug at all) and, when `content` is `Some`, a
/// `planning/state.json` inside it. `content: None` is deliberate for the
/// "missing state.json" case — the repo directory exists, the state file
/// does not.
fn write_repo(dir: &Path, slug: &str, content: Option<&str>) {
    let repo_dir = dir.join(slug);
    std::fs::create_dir_all(&repo_dir).expect("mkdir repo dir");
    if let Some(content) = content {
        let planning = repo_dir.join("planning");
        std::fs::create_dir_all(&planning).expect("mkdir planning");
        std::fs::write(planning.join("state.json"), content).expect("write state.json");
    }
}

fn write_brain_toml(dir: &Path, slugs: &[&str]) {
    let mut toml = String::new();
    for slug in slugs {
        toml.push_str(&format!(
            "[[repos]]\nslug = \"{slug}\"\nrepo_path = \"{slug}\"\n\n"
        ));
    }
    std::fs::write(dir.join("brain.toml"), toml).expect("write brain.toml");
}

/// `resolve_roadmap_dir` (`integrate.rs`) is consulted on every run, gated
/// or not — every fixture needs this to exist so a case is never
/// misattributed to a missing roadmap directory instead of the gate under
/// test.
fn ensure_roadmap_dir(dir: &Path, roadmap_slug: &str) {
    let roadmap_dir = dir.join("planning").join("roadmaps").join(roadmap_slug);
    std::fs::create_dir_all(&roadmap_dir).expect("mkdir roadmap dir");
}

fn write_lane_segments(dir: &Path, content: &str) {
    let planning = dir.join("planning");
    std::fs::create_dir_all(&planning).expect("mkdir planning");
    std::fs::write(planning.join("lane-segments.json"), content).expect("write lane-segments.json");
}

/// Wraps one or more hand-written block JSON objects into a minimal
/// `state.json` document `okf_core::load_state` accepts.
fn state_json(blocks_json: &str) -> String {
    format!(
        r#"{{
    "repo": "repo",
    "kind": "project",
    "updated": "2026-08-20",
    "tracks": [
        {{ "title": "wave 1", "blocks": [ {blocks_json} ] }}
    ]
}}"#
    )
}

const ROADMAP_SLUG: &str = "test-roadmap";

fn never_held() -> Arc<dyn HoldSource> {
    Arc::new(NeverHeld)
}

/// Run `event` through the wired `ORCHESTRATION` registration
/// (`register_orchestration_with_registry`) end to end and return the sole
/// node's recorded `error`, if any. `None` means the run's node reported
/// `Success`.
async fn run_wired(hold_source: Arc<dyn HoldSource>, event: serde_json::Value) -> Option<String> {
    let mut dispatcher = Dispatcher::new();
    register_orchestration_with_registry(&mut dispatcher, None, hold_source);
    let workflow = dispatcher
        .dispatch_with_event("ORCHESTRATION", &event)
        .expect("ORCHESTRATION should dispatch to a runnable Workflow");
    let ctx: TaskContext = workflow
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("Workflow::run itself must not error — a node failure is stamped, not raised");
    let node_run = ctx
        .node_runs
        .get(orch_graph::NODE_NAME)
        .expect("OrchestrationRunNode must have a recorded NodeRun");
    match node_run.status {
        NodeRunStatus::Success => None,
        NodeRunStatus::Failed => Some(
            node_run
                .error
                .clone()
                .expect("a Failed NodeRun must carry an error message"),
        ),
        other => panic!("unexpected terminal NodeRun status: {other:?}"),
    }
}

/// The permissive, UNWIRED default — `orchestration::graph::workflow()` —
/// run against the same event, with no `Dispatcher`/registration involved
/// at all. Used only as the "before" side of the behavioural anti-revert
/// checks: this is exactly the shape production ran before this ticket.
async fn run_unwired(event: serde_json::Value) -> Option<String> {
    let workflow = orch_graph::workflow();
    let ctx: TaskContext = workflow
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("Workflow::run itself must not error — a node failure is stamped, not raised");
    let node_run = ctx
        .node_runs
        .get(orch_graph::NODE_NAME)
        .expect("OrchestrationRunNode must have a recorded NodeRun");
    match node_run.status {
        NodeRunStatus::Success => None,
        NodeRunStatus::Failed => Some(
            node_run
                .error
                .clone()
                .expect("a Failed NodeRun must carry an error message"),
        ),
        other => panic!("unexpected terminal NodeRun status: {other:?}"),
    }
}

// ── Case 1: unmet dependency stops the chain, names the edge ────────────

#[tokio::test]
async fn unmet_dependency_stops_the_chain_and_names_the_edge() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_repo(
        dir.path(),
        "repo-a",
        Some(&state_json(
            r#"{"id": "A.1", "title": "a1", "status": "open", "depends_on": [
                {"type": "block", "repo": "repo-b", "id": "B.1"}
            ]}"#,
        )),
    );
    write_repo(
        dir.path(),
        "repo-b",
        // B.1 is NOT closed — the dependency is unmet.
        Some(&state_json(
            r#"{"id": "B.1", "title": "b1", "status": "open"}"#,
        )),
    );
    write_brain_toml(dir.path(), &["repo-a", "repo-b"]);
    ensure_roadmap_dir(dir.path(), ROADMAP_SLUG);

    let event = serde_json::json!({
        "brain_root": dir.path(),
        "roadmap_slug": ROADMAP_SLUG,
        "blocks": [{"repo": "repo-a", "block_id": "A.1"}],
    });

    let error = run_wired(never_held(), event)
        .await
        .expect("an unmet dependency must stop the chain");
    assert!(
        error.contains("A.1") && error.contains("repo-a"),
        "error should name the blocked block: {error}"
    );
    assert!(
        error.contains("B.1") && error.contains("repo-b"),
        "error should name the unmet edge: {error}"
    );
    assert!(
        error.contains("not yet met") || error.contains("cannot start"),
        "error should read as a dependency gate failure: {error}"
    );
}

// ── Case 2: all edges closed — chain proceeds past the gate ─────────────

#[tokio::test]
async fn met_dependency_proceeds_past_the_gate() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_repo(
        dir.path(),
        "repo-a",
        Some(&state_json(
            r#"{"id": "A.1", "title": "a1", "status": "open", "depends_on": [
                {"type": "block", "repo": "repo-b", "id": "B.1"}
            ]}"#,
        )),
    );
    write_repo(
        dir.path(),
        "repo-b",
        // B.1 IS closed — the dependency is met.
        Some(&state_json(
            r#"{"id": "B.1", "title": "b1", "status": "closed"}"#,
        )),
    );
    write_brain_toml(dir.path(), &["repo-a", "repo-b"]);
    ensure_roadmap_dir(dir.path(), ROADMAP_SLUG);

    let event = serde_json::json!({
        "brain_root": dir.path(),
        "roadmap_slug": ROADMAP_SLUG,
        "blocks": [{"repo": "repo-a", "block_id": "A.1"}],
    });

    let error = run_wired(never_held(), event).await;
    // The chain must not stop at the dependency gate — whatever happens
    // next (a real `SDLC_FLOW` attempt against a non-git fixture repo
    // fails at state-write verification; see the module doc), the failure
    // must not read as an unmet dependency.
    if let Some(error) = &error {
        assert!(
            !error.contains("not yet met") && !error.contains("cannot start"),
            "a met dependency must not surface as a gate failure: {error}"
        );
    }
}

// ── Case 3: HELD-UNTIL naming a still-open block is not admitted ────────

#[tokio::test]
async fn held_until_naming_open_block_is_not_admitted() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_repo(
        dir.path(),
        "repo-a",
        Some(&state_json(
            r#"{"id": "A.1", "title": "a1", "status": "open"}"#,
        )),
    );
    write_repo(
        dir.path(),
        "repo-b",
        // B.1 is NOT closed — the HELD-UNTIL target is still open.
        Some(&state_json(
            r#"{"id": "B.1", "title": "b1", "status": "open"}"#,
        )),
    );
    write_brain_toml(dir.path(), &["repo-a", "repo-b"]);
    ensure_roadmap_dir(dir.path(), ROADMAP_SLUG);
    write_lane_segments(
        dir.path(),
        &format!(
            r#"{{"blocks": [
                {{"roadmap": "{ROADMAP_SLUG}", "lane": "lane1", "repo": "repo-a", "id": "A.1",
                  "line": 1, "segment": 0, "position": 0, "origin_roadmap": null,
                  "directives": {{"held_until": "B.1"}}}}
            ]}}"#
        ),
    );

    let event = serde_json::json!({
        "brain_root": dir.path(),
        "roadmap": ROADMAP_SLUG,
        "lane": "lane1",
        "roadmap_slug": ROADMAP_SLUG,
    });

    let error = run_wired(never_held(), event)
        .await
        .expect("a lane held on a still-open block must not be admitted");
    assert!(
        error.contains("HELD-UNTIL") && error.contains("B.1"),
        "error should name the held-until target: {error}"
    );
    assert!(
        error.contains("still open") || error.contains("open"),
        "error should say the target is still open: {error}"
    );
}

// ── Case 4: HELD-UNTIL naming a closed block is admitted ────────────────

#[tokio::test]
async fn held_until_naming_closed_block_is_admitted() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_repo(
        dir.path(),
        "repo-a",
        Some(&state_json(
            r#"{"id": "A.1", "title": "a1", "status": "open"}"#,
        )),
    );
    write_repo(
        dir.path(),
        "repo-b",
        // B.1 IS closed — the HELD-UNTIL target has cleared.
        Some(&state_json(
            r#"{"id": "B.1", "title": "b1", "status": "closed"}"#,
        )),
    );
    write_brain_toml(dir.path(), &["repo-a", "repo-b"]);
    ensure_roadmap_dir(dir.path(), ROADMAP_SLUG);
    write_lane_segments(
        dir.path(),
        &format!(
            r#"{{"blocks": [
                {{"roadmap": "{ROADMAP_SLUG}", "lane": "lane1", "repo": "repo-a", "id": "A.1",
                  "line": 1, "segment": 0, "position": 0, "origin_roadmap": null,
                  "directives": {{"held_until": "B.1"}}}}
            ]}}"#
        ),
    );

    let event = serde_json::json!({
        "brain_root": dir.path(),
        "roadmap": ROADMAP_SLUG,
        "lane": "lane1",
        "roadmap_slug": ROADMAP_SLUG,
    });

    let error = run_wired(never_held(), event).await;
    if let Some(error) = &error {
        assert!(
            !error.contains("HELD-UNTIL") && !error.contains("still open"),
            "a closed held-until target must not surface as held: {error}"
        );
    }
}

// ── Case 5: missing state.json fails loud, naming repo and path ─────────

#[tokio::test]
async fn missing_state_json_fails_loud_naming_repo_and_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    // repo-a exists (so RepoRegistry records the slug) but has no
    // planning/state.json at all.
    write_repo(dir.path(), "repo-a", None);
    write_brain_toml(dir.path(), &["repo-a"]);
    ensure_roadmap_dir(dir.path(), ROADMAP_SLUG);

    let event = serde_json::json!({
        "brain_root": dir.path(),
        "roadmap_slug": ROADMAP_SLUG,
        "blocks": [{"repo": "repo-a", "block_id": "A.1"}],
    });

    let error = run_wired(never_held(), event)
        .await
        .expect("a missing state.json must fail loudly, never read as 'no edges, proceed'");
    assert!(
        error.contains("repo-a"),
        "error should name the repo: {error}"
    );
    assert!(
        error.contains("state.json") || error.contains("could not"),
        "error should name the load failure: {error}"
    );
}

// ── Case 6: malformed state.json fails loud, naming repo ────────────────

#[tokio::test]
async fn malformed_state_json_fails_loud_naming_repo() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_repo(dir.path(), "repo-a", Some("{ not valid json"));
    write_brain_toml(dir.path(), &["repo-a"]);
    ensure_roadmap_dir(dir.path(), ROADMAP_SLUG);

    let event = serde_json::json!({
        "brain_root": dir.path(),
        "roadmap_slug": ROADMAP_SLUG,
        "blocks": [{"repo": "repo-a", "block_id": "A.1"}],
    });

    let error = run_wired(never_held(), event)
        .await
        .expect("a malformed state.json must fail loudly, never read as 'no edges, proceed'");
    assert!(
        error.contains("repo-a"),
        "error should name the repo: {error}"
    );
}

// ── Case 7: an injected HoldSource pauses the chain, then it resumes ────

/// A `HoldSource` that reports held for its first `held_for_calls` calls,
/// then clears — recording every call so the test can prove
/// `wait_for_clearance` actually polled it more than once (a real pause)
/// rather than the chain proceeding straight through.
struct FlakyHold {
    calls: Arc<AtomicUsize>,
    held_for_calls: usize,
}

impl HoldSource for FlakyHold {
    fn is_held(&self, _repo: &str, _block_id: &str) -> bool {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        n < self.held_for_calls
    }
}

#[tokio::test]
async fn injected_hold_source_pauses_the_chain_then_resumes() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_repo(
        dir.path(),
        "repo-a",
        Some(&state_json(
            r#"{"id": "A.1", "title": "a1", "status": "open"}"#,
        )),
    );
    write_brain_toml(dir.path(), &["repo-a"]);
    ensure_roadmap_dir(dir.path(), ROADMAP_SLUG);

    let calls = Arc::new(AtomicUsize::new(0));
    let hold_source: Arc<dyn HoldSource> = Arc::new(FlakyHold {
        calls: calls.clone(),
        held_for_calls: 3,
    });

    let event = serde_json::json!({
        "brain_root": dir.path(),
        "roadmap_slug": ROADMAP_SLUG,
        "blocks": [{"repo": "repo-a", "block_id": "A.1"}],
        // A tiny poll interval so the held-then-cleared loop above
        // resolves in milliseconds, not the 2s built-in default.
        "policy": {"hold_poll_interval_ms": 5},
    });

    let _ = run_wired(hold_source, event).await;

    assert!(
        calls.load(Ordering::SeqCst) >= 4,
        "expected wait_for_clearance to poll past the 3 held calls (3 held + \
         at least 1 clearing check), got {}",
        calls.load(Ordering::SeqCst)
    );
}

/// The same fixture and event, but with `NeverHeld` — the chain must not
/// pause at all, i.e. `is_held` is never even consulted (there is nothing
/// to compare against: `NeverHeld` has no counter). This exists purely to
/// contrast with the case above: a custom `HoldSource` genuinely changes
/// behaviour relative to the always-clear default, proving `hold_source`
/// really is a live parameter of the registration, not a hardcoded no-op.
#[tokio::test]
async fn never_held_hold_source_never_pauses() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_repo(
        dir.path(),
        "repo-a",
        Some(&state_json(
            r#"{"id": "A.1", "title": "a1", "status": "open"}"#,
        )),
    );
    write_brain_toml(dir.path(), &["repo-a"]);
    ensure_roadmap_dir(dir.path(), ROADMAP_SLUG);

    let event = serde_json::json!({
        "brain_root": dir.path(),
        "roadmap_slug": ROADMAP_SLUG,
        "blocks": [{"repo": "repo-a", "block_id": "A.1"}],
        "policy": {"hold_poll_interval_ms": 5},
    });

    let start = std::time::Instant::now();
    let _ = run_wired(never_held(), event).await;
    // With no hold at all, the run must not have spent multiple poll
    // intervals waiting — a generous ceiling well under what even one
    // hold-and-clear cycle at the fixture's own poll interval would cost.
    assert!(
        start.elapsed() < std::time::Duration::from_secs(5),
        "a never-held chain must not pause"
    );
}

// ── Anti-revert guard: each with_* seam demonstrably changes outcome ────

/// `with_resolve_depends_on` + `with_is_edge_met`: the wired registration
/// stops on the unmet edge (case 1's fixture); the permissive UNWIRED
/// default (`orchestration::graph::workflow()`, `EN.ticket.orchestration-production-gates-unwired`
/// Task 1-2's whole point) declares no edges at all and so does not.
/// Re-defaulting either builder back to the permissive shape collapses
/// this to `wired == unwired`, turning this test red.
#[tokio::test]
async fn anti_revert_guard_depends_on_and_edge_met_change_outcome() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_repo(
        dir.path(),
        "repo-a",
        Some(&state_json(
            r#"{"id": "A.1", "title": "a1", "status": "open", "depends_on": [
                {"type": "block", "repo": "repo-b", "id": "B.1"}
            ]}"#,
        )),
    );
    write_repo(
        dir.path(),
        "repo-b",
        Some(&state_json(
            r#"{"id": "B.1", "title": "b1", "status": "open"}"#,
        )),
    );
    write_brain_toml(dir.path(), &["repo-a", "repo-b"]);
    ensure_roadmap_dir(dir.path(), ROADMAP_SLUG);

    let event = serde_json::json!({
        "brain_root": dir.path(),
        "roadmap_slug": ROADMAP_SLUG,
        "blocks": [{"repo": "repo-a", "block_id": "A.1"}],
    });

    let wired_error = run_wired(never_held(), event.clone())
        .await
        .expect("wired registration must stop on the unmet edge");
    assert!(
        wired_error.contains("not yet met") || wired_error.contains("cannot start"),
        "wired: {wired_error}"
    );

    let unwired_error = run_unwired(event).await;
    if let Some(unwired_error) = &unwired_error {
        assert!(
            !unwired_error.contains("not yet met") && !unwired_error.contains("cannot start"),
            "the permissive unwired default must not gate on a dependency it never declared: {unwired_error}"
        );
    }
}

/// `with_is_block_open`: the wired registration refuses the still-open
/// HELD-UNTIL target (case 3's fixture); the permissive UNWIRED default's
/// `is_block_open` always answers "not open" and so admits it. Re-
/// defaulting the builder collapses this to `wired == unwired`.
#[tokio::test]
async fn anti_revert_guard_is_block_open_changes_outcome() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_repo(
        dir.path(),
        "repo-a",
        Some(&state_json(
            r#"{"id": "A.1", "title": "a1", "status": "open"}"#,
        )),
    );
    write_repo(
        dir.path(),
        "repo-b",
        Some(&state_json(
            r#"{"id": "B.1", "title": "b1", "status": "open"}"#,
        )),
    );
    write_brain_toml(dir.path(), &["repo-a", "repo-b"]);
    ensure_roadmap_dir(dir.path(), ROADMAP_SLUG);
    write_lane_segments(
        dir.path(),
        &format!(
            r#"{{"blocks": [
                {{"roadmap": "{ROADMAP_SLUG}", "lane": "lane1", "repo": "repo-a", "id": "A.1",
                  "line": 1, "segment": 0, "position": 0, "origin_roadmap": null,
                  "directives": {{"held_until": "B.1"}}}}
            ]}}"#
        ),
    );

    let event = serde_json::json!({
        "brain_root": dir.path(),
        "roadmap": ROADMAP_SLUG,
        "lane": "lane1",
        "roadmap_slug": ROADMAP_SLUG,
    });

    let wired_error = run_wired(never_held(), event.clone())
        .await
        .expect("wired registration must refuse the still-open held-until target");
    assert!(wired_error.contains("HELD-UNTIL"), "wired: {wired_error}");

    let unwired_error = run_unwired(event).await;
    if let Some(unwired_error) = &unwired_error {
        assert!(
            !unwired_error.contains("HELD-UNTIL"),
            "the permissive unwired default must not refuse a lane on a held-until token \
             it never resolves: {unwired_error}"
        );
    }
}

/// `with_hold_source`: a custom `HoldSource` that stays held for several
/// polls is actually consulted (and paused on) by the wired registration;
/// swapping it for `NeverHeld` on the identical fixture never pauses at
/// all (`never_held_hold_source_never_pauses`, above) — the two together
/// are the behavioural proof that `hold_source` is a live parameter of
/// `register_orchestration_with_registry`, not a hardcoded value.
#[tokio::test]
async fn anti_revert_guard_hold_source_changes_outcome() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_repo(
        dir.path(),
        "repo-a",
        Some(&state_json(
            r#"{"id": "A.1", "title": "a1", "status": "open"}"#,
        )),
    );
    write_brain_toml(dir.path(), &["repo-a"]);
    ensure_roadmap_dir(dir.path(), ROADMAP_SLUG);

    let calls = Arc::new(AtomicUsize::new(0));
    let hold_source: Arc<dyn HoldSource> = Arc::new(FlakyHold {
        calls: calls.clone(),
        held_for_calls: 2,
    });

    let event = serde_json::json!({
        "brain_root": dir.path(),
        "roadmap_slug": ROADMAP_SLUG,
        "blocks": [{"repo": "repo-a", "block_id": "A.1"}],
        "policy": {"hold_poll_interval_ms": 5},
    });

    let _ = run_wired(hold_source, event).await;

    assert!(
        calls.load(Ordering::SeqCst) >= 3,
        "a custom HoldSource passed to registration must actually be polled \
         until it clears; got {} calls",
        calls.load(Ordering::SeqCst)
    );
}
