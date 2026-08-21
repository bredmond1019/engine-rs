//! `EN.11.J` Task 4 — the mev-parity fixture: evidence for the un-gateable AC "the
//! reader's answers match mev's for the same edges".
//!
//! # Why this cannot be a normal assertion
//!
//! `mev` is a separate repo (`core/mev`) and an **installed binary** — nothing in this
//! workspace can invoke it, and no `cargo` check here can observe its behaviour. This
//! module stands in for that AC the only way available: a checked-in `state.json`
//! fixture (built below, in Rust, exactly like `corpus_gates.rs`'s own test helpers)
//! plus mev's *actual, recorded* answers for the same edges, reproduced against
//! [`CorpusGates`]. If a future change to `CorpusGates`'s `wontfix`/operator/cross-repo
//! semantics silently diverges from mev's, this is the test that catches it.
//!
//! # Provenance (fill this in again if the fixture below ever changes)
//!
//! - **Binary**: the *installed* `mev` at `~/.cargo/bin/mev` (NOT a source build of
//!   `core/mev` run via `cargo run`) — `which mev` resolves there on this machine.
//! - **Version**: `mev --version` → `mev 0.1.0`.
//! - **Build stamp**: binary mtime `2026-08-20 16:20` (`ls -la ~/.cargo/bin/mev`);
//!   `core/mev`'s `git log -1 --oneline` at recording time was `2701bda
//!   fix(lane-segments): recognize RISK as an established non-directive header key` —
//!   informational only (the installed binary is not proven to be built from that exact
//!   commit, only that it is the closest available signal, per this module's own
//!   discipline that a recorded answer needs stated provenance).
//! - **Invocation**: `mev frontier <brain-root> --json`, run against the exact fixture
//!   tree [`fixture_brain_root`] builds below (a `brain.toml` naming `repo-a`/`repo-b`,
//!   each repo's `planning/state.json`, and one single-block lane file per case under
//!   `planning/roadmaps/parity/lane-<case>.txt` so each dependent block becomes its own
//!   frontier entry with its own `unmet_blocks`/`unmet_gates`). `mev frontier` is the
//!   CLI surface that walks the exact same `depends_on` -> met/unmet question this
//!   module's [`CorpusGates`] answers — `unmet_blocks` for `BlockedBy::Block` edges (a
//!   `repo:id` is unmet iff its authored `status` is not `"closed"`, so a `wontfix`
//!   target is unmet — the same "frontier" notion `CorpusGates::is_edge_met` documents,
//!   not `focus.next`'s `closed|wontfix`) and `unmet_gates` for
//!   `Operator`/`Approval`/`External` edges (always present while the edge exists, never
//!   evaluated). The recorded JSON for each lane is reproduced verbatim in this module's
//!   fixture-building tests below, and each recorded answer is cross-checked against
//!   `CorpusGates` for the same `(repo, block_id)`.
//!
//! # Coverage
//!
//! Five cases, one dependent block each, covering every edge shape the AC names:
//! `A.1` (closed target, same-repo — met), `A.2` (open target, same-repo — unmet),
//! `A.3` (closed target, **cross-repo** — met), `A.4` (operator edge — unmet, named),
//! `A.5` (`wontfix` target, same-repo — unmet, the frontier/`focus.next` divergence).

use std::sync::Arc;

use engine_core::repo_registry::RepoRegistry;
use engine_core::workflows::orchestration::corpus_gates::CorpusGates;
use engine_core::workflows::orchestration::gates::DependencyEdge;

/// Build the exact tempdir brain root `mev frontier` was run against to produce the
/// recorded answers in the module doc above. Byte-identical fixture content to what
/// was on disk during that recording (state.json bodies + lane files), so re-running
/// `mev frontier <root> --json` against a freshly built copy of this same layout
/// reproduces the same JSON this module asserts against `CorpusGates`.
fn fixture_brain_root() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");

    let repo_a_planning = dir.path().join("repo-a").join("planning");
    let repo_b_planning = dir.path().join("repo-b").join("planning");
    std::fs::create_dir_all(&repo_a_planning).expect("mkdir repo-a/planning");
    std::fs::create_dir_all(&repo_b_planning).expect("mkdir repo-b/planning");

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

    std::fs::write(
        repo_a_planning.join("state.json"),
        r#"{
    "repo": "repo-a",
    "kind": "project",
    "updated": "2026-08-21",
    "tracks": [
        { "title": "wave 1", "blocks": [
            { "id": "A.CLOSED", "title": "closed target", "status": "closed" },
            { "id": "A.OPEN", "title": "open target", "status": "open" },
            { "id": "A.WONTFIX", "title": "wontfix target", "status": "wontfix" },
            { "id": "A.1", "title": "same-repo edge met", "status": "open",
              "depends_on": [ { "type": "block", "repo": "repo-a", "id": "A.CLOSED" } ] },
            { "id": "A.2", "title": "same-repo edge unmet", "status": "open",
              "depends_on": [ { "type": "block", "repo": "repo-a", "id": "A.OPEN" } ] },
            { "id": "A.3", "title": "cross-repo edge met", "status": "open",
              "depends_on": [ { "type": "block", "repo": "repo-b", "id": "B.1" } ] },
            { "id": "A.4", "title": "operator edge unmet", "status": "open",
              "depends_on": [ { "type": "operator", "slug": "op-visit",
                "exit": "operator confirms setup", "start": "engine flags the block ready" } ] },
            { "id": "A.5", "title": "wontfix target is not met (frontier notion)", "status": "open",
              "depends_on": [ { "type": "block", "repo": "repo-a", "id": "A.WONTFIX" } ] }
        ] }
    ]
}"#,
    )
    .expect("write repo-a/planning/state.json");

    std::fs::write(
        repo_b_planning.join("state.json"),
        r#"{
    "repo": "repo-b",
    "kind": "project",
    "updated": "2026-08-21",
    "tracks": [
        { "title": "wave 1", "blocks": [
            { "id": "B.1", "title": "b1", "status": "closed" }
        ] }
    ]
}"#,
    )
    .expect("write repo-b/planning/state.json");

    // One single-block lane per case so each dependent block heads its own frontier
    // segment — this is what makes `mev frontier`'s per-entry `unmet_blocks`/
    // `unmet_gates` answer exactly the same (repo, block_id) question `CorpusGates`
    // answers, rather than a multi-block segment head hiding the later cases.
    let roadmap = dir.path().join("planning").join("roadmaps").join("parity");
    std::fs::create_dir_all(&roadmap).expect("mkdir planning/roadmaps/parity");
    for (lane, block) in [
        ("a1", "A.1"),
        ("a2", "A.2"),
        ("a3", "A.3"),
        ("a4", "A.4"),
        ("a5", "A.5"),
    ] {
        std::fs::write(
            roadmap.join(format!("lane-{lane}.txt")),
            format!("{block}\n"),
        )
        .expect("write lane file");
    }

    dir
}

fn registry_for(dir: &tempfile::TempDir) -> Arc<RepoRegistry> {
    Arc::new(RepoRegistry::from_brain_root(dir.path()).expect("registry builds"))
}

/// Recorded from `mev frontier <fixture-root> --json` (see module doc for the exact
/// invocation and provenance). Reproduced here as the five per-case answers this test
/// asserts `CorpusGates` agrees with:
///
/// ```text
/// A.1  startable=true   unmet_blocks=[]                  unmet_gates=[]
/// A.2  startable=false  unmet_blocks=["repo-a:A.OPEN"]    unmet_gates=[]
/// A.3  startable=true   unmet_blocks=[]                  unmet_gates=[]
/// A.4  startable=false  unmet_blocks=[]                  unmet_gates=["operator:op-visit"]
/// A.5  startable=false  unmet_blocks=["repo-a:A.WONTFIX"] unmet_gates=[]
/// ```
#[test]
fn corpus_gates_matches_mev_recorded_answers_same_repo_closed_target_met() {
    let dir = fixture_brain_root();
    let gates = CorpusGates::new(registry_for(&dir));

    // mev: A.1 startable=true, unmet_blocks=[] — the sole edge (repo-a:A.CLOSED) is met.
    let edges = gates.resolve_depends_on("repo-a", "A.1");
    assert_eq!(
        edges,
        vec![DependencyEdge::Block {
            repo: "repo-a".to_string(),
            block_id: "A.CLOSED".to_string(),
        }]
    );
    assert!(
        gates.is_edge_met("repo-a", "A.CLOSED"),
        "mev reports A.1 startable — CorpusGates must agree its dependency is met"
    );
    assert!(gates.take_error().is_none());
}

#[test]
fn corpus_gates_matches_mev_recorded_answers_same_repo_open_target_unmet() {
    let dir = fixture_brain_root();
    let gates = CorpusGates::new(registry_for(&dir));

    // mev: A.2 startable=false, unmet_blocks=["repo-a:A.OPEN"].
    let edges = gates.resolve_depends_on("repo-a", "A.2");
    assert_eq!(
        edges,
        vec![DependencyEdge::Block {
            repo: "repo-a".to_string(),
            block_id: "A.OPEN".to_string(),
        }]
    );
    assert!(
        !gates.is_edge_met("repo-a", "A.OPEN"),
        "mev reports A.2 blocked by repo-a:A.OPEN — CorpusGates must agree it is unmet"
    );
    assert!(gates.take_error().is_none());
}

#[test]
fn corpus_gates_matches_mev_recorded_answers_cross_repo_closed_target_met() {
    let dir = fixture_brain_root();
    let gates = CorpusGates::new(registry_for(&dir));

    // mev: A.3 startable=true, unmet_blocks=[] — its edge targets repo-b, not repo-a.
    let edges = gates.resolve_depends_on("repo-a", "A.3");
    assert_eq!(
        edges,
        vec![DependencyEdge::Block {
            repo: "repo-b".to_string(),
            block_id: "B.1".to_string(),
        }]
    );
    assert!(
        gates.is_edge_met("repo-b", "B.1"),
        "mev reports A.3 startable via its cross-repo edge — CorpusGates must resolve \
         repo-b, not just repo-a, and agree it is met"
    );
    assert!(gates.take_error().is_none());
}

#[test]
fn corpus_gates_matches_mev_recorded_answers_operator_edge_unmet_and_named() {
    let dir = fixture_brain_root();
    let gates = CorpusGates::new(registry_for(&dir));

    // mev: A.4 startable=false, unmet_gates=["operator:op-visit"].
    let edges = gates.resolve_depends_on("repo-a", "A.4");
    assert_eq!(
        edges,
        vec![DependencyEdge::Operator {
            slug: "op-visit".to_string(),
        }]
    );

    let is_edge_met = |repo: &str, block_id: &str| gates.is_edge_met(repo, block_id);
    let resolve_deps = |repo: &str, block_id: &str| gates.resolve_depends_on(repo, block_id);
    let step = engine_core::workflows::orchestration::chain::ChainStep {
        repo: "repo-a".to_string(),
        block_id: "A.4".to_string(),
        directives: None,
    };
    let err = engine_core::workflows::orchestration::gates::check_dependencies(
        &step,
        &resolve_deps,
        &is_edge_met,
    )
    .expect_err("mev reports A.4 blocked by operator:op-visit — CorpusGates must refuse too");
    let msg = err.to_string();
    assert!(
        msg.contains("op-visit"),
        "refusal must name the same gate slug mev's unmet_gates recorded: {msg}"
    );
    assert!(gates.take_error().is_none());
}

#[test]
fn corpus_gates_matches_mev_recorded_answers_wontfix_target_unmet_frontier_notion() {
    let dir = fixture_brain_root();
    let gates = CorpusGates::new(registry_for(&dir));

    // mev: A.5 startable=false, unmet_blocks=["repo-a:A.WONTFIX"] — mev's frontier
    // notion of "met" (only `closed`) treats `wontfix` as NOT met, exactly like
    // `CorpusGates::is_edge_met`'s documented divergence from `focus.next`.
    let edges = gates.resolve_depends_on("repo-a", "A.5");
    assert_eq!(
        edges,
        vec![DependencyEdge::Block {
            repo: "repo-a".to_string(),
            block_id: "A.WONTFIX".to_string(),
        }]
    );
    assert!(
        !gates.is_edge_met("repo-a", "A.WONTFIX"),
        "mev reports A.5 blocked by its wontfix target — CorpusGates must not conflate \
         wontfix with closed either"
    );
    assert!(gates.take_error().is_none());
}
