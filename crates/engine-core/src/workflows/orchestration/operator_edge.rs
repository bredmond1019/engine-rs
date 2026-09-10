//! `EN.15.J` Task 1 — the production `author_operator_edge` closure for
//! [`gates::check_permission_gate`](super::gates::check_permission_gate).
//!
//! [`check_permission_gate`](super::gates::check_permission_gate) has zero production
//! callers today: every existing call site is a test-stub closure in `gates.rs`'s own
//! `#[cfg(test)]` module. This module gives it its first real one —
//! [`make_author_operator_edge`] builds an `Fn(&OperatorGateRequest) -> Result<(), String>`
//! backed by `mev::add_operator_edge_as`, the QUIESCE-REFUSING guarded verb (never the
//! plain `mev::add_operator_edge`, which only downgrades a quiesce refusal to a warning).
//!
//! ## Always through the CLI-guarded library call, never an in-process `state.json` write
//!
//! `state.json` has a single writer — `mev` — per `seams.md` seam 6. This module calls
//! `mev::add_operator_edge_as` as an in-process **library** call (the same convention
//! `coord::write`'s tests already exercise for `mev::set_block_status_as`; `mev` is a
//! workspace path-dependency of `engine-core`, see `Cargo.toml`), which is not the same
//! as shelling out to the `mev` binary, but it IS the guarded entry point: it takes the
//! quiesce lock, and its `write: true` path chains into the identical validation
//! `bastion validate-brain --state` runs. Nothing here reimplements any part of mev's
//! graph-write or duplicate-slug logic.
//!
//! ## The failure mode this exists to prevent
//!
//! `mev::add_operator_edge_as` reports a refused write (a duplicate slug on the same
//! block, a malformed corpus) as an error-severity [`mev::Diagnostic`] inside an `Ok`
//! [`mev::Report`], not as an `Err` — only a lock/quiesce/I-O failure surfaces as `Err`.
//! [`author_operator_edge`] checks `report.is_failure()` on every call and maps EITHER
//! failure shape to `Err(String)`. Mapping only the `Err` case and treating a
//! diagnostic-carrying `Ok` as success is exactly the silent-skip bug this block exists
//! to close — see `check_permission_gate`'s own doc comment on why the step never
//! proceeds regardless of what this closure returns, and why a caller that misreports
//! success here defeats that guarantee anyway by feeding a false green upstream.

use std::path::{Path, PathBuf};

use okf_core::OperatorDep;

use super::escalate::{
    append_escalation_line, EscalationChannel, EscalationKind, EscalationOption, EscalationRecord,
    EscalationSeverity, NewEscalation, SUMMARY_MAX_CHARS,
};
use super::gates::OperatorGateRequest;

/// Everything [`make_author_operator_edge`] needs to author an edge and (best-effort)
/// enqueue the notification escalation that carries it to the operator.
///
/// `roadmap_dir` is `Option` because not every chain runs under a roadmap (a standalone
/// `/sdlc-flow` invocation has none) — when absent, the edge is still authored, just with
/// no escalation composed, since there is no `escalations.jsonl` to enqueue it onto.
#[derive(Debug, Clone)]
pub struct OperatorEdgeAuthorConfig {
    /// The brain root `mev::add_operator_edge_as` resolves `brain.toml` from.
    pub root: PathBuf,
    /// The repo half of the `<repo>:<block_id>` key the edge is authored on — the
    /// gating step's own block, per `check_permission_gate`'s doc: the edge blocks
    /// exactly the block that hit the gate.
    pub repo: String,
    /// The block-id half of that key.
    pub block_id: String,
    /// The subject repo's working directory — passed through to
    /// `mev::add_operator_edge_as`'s own `dir` parameter (its quiesce-identity /
    /// lease-resolution scope) and reused to resolve `verified_at_sha` for the
    /// escalation.
    pub dir: PathBuf,
    /// Quiesce identity for the guarded write — the same `agent` convention
    /// `mev::set_block_status_as` callers already pass.
    pub agent: Option<String>,
    /// Explicit lock-dir override; `None` resolves to `mev`'s own default
    /// (`<root>/.fleet-locks`).
    pub lock_dir: Option<PathBuf>,
    /// The roadmap this chain runs under, if any — folded into the escalation's
    /// `gate_id` and `lane` fallback.
    pub roadmap: Option<String>,
    /// The lane this chain runs under, if any — falls back to `repo` when absent,
    /// matching `integrate.rs`'s `record_bail_escalation` convention.
    pub lane: Option<String>,
    /// `<roadmap>/escalations.jsonl`'s parent directory. `None` skips escalation
    /// composition entirely (see struct docs).
    pub roadmap_dir: Option<PathBuf>,
}

/// Build the production `author_operator_edge` closure `check_permission_gate` takes.
///
/// A pure constructor: no I/O happens until the returned closure is actually called by
/// a denied [`super::gates::check_permission_gate`].
pub fn make_author_operator_edge(
    config: OperatorEdgeAuthorConfig,
) -> impl Fn(&OperatorGateRequest) -> Result<(), String> {
    move |req: &OperatorGateRequest| author_operator_edge(&config, req)
}

/// The closure body — split out from [`make_author_operator_edge`] so it can be unit
/// tested directly against a real temp brain fixture without going through a `dyn Fn`.
fn author_operator_edge(
    config: &OperatorEdgeAuthorConfig,
    req: &OperatorGateRequest,
) -> Result<(), String> {
    let key = format!("{}:{}", config.repo, config.block_id);
    let edge = OperatorDep {
        slug: req.slug.clone(),
        exit: req.exit.clone(),
        start: req.start.clone(),
        what: None,
    };

    let report = mev::add_operator_edge_as(
        &config.root,
        &key,
        &edge,
        true,
        None,
        config.agent.as_deref(),
        config.lock_dir.as_deref(),
        &config.dir,
    )
    .map_err(|err| {
        format!(
            "mev add-operator-edge failed for gate '{}' on '{key}': {err}",
            req.slug
        )
    })?;

    if report.is_failure() {
        let messages: Vec<String> = report
            .diagnostics
            .iter()
            .filter(|d| d.severity == mev::Severity::Error)
            .map(|d| format!("{} [{}]: {}", d.file.display(), d.locator, d.message))
            .collect();
        return Err(format!(
            "mev add-operator-edge refused to author operator gate '{}' on '{key}': {}",
            req.slug,
            messages.join("; ")
        ));
    }

    // Best-effort, matching `integrate.rs`'s `record_bail_escalation` convention: a
    // notification that cannot be composed must never turn an already-successful
    // edge-author into a failure — the gate is real either way.
    if let Err(reason) = compose_and_enqueue_notification(config, req) {
        tracing::warn!(
            slug = %req.slug,
            reason = %reason,
            "EN.15.J: operator edge authored, but the notification escalation could not be \
             composed/enqueued"
        );
    }

    Ok(())
}

/// Compose a schema-valid `operator-gate` escalation naming `req.slug` and append it to
/// `<roadmap_dir>/escalations.jsonl`, reusing `EN.15.G`'s own
/// `NewEscalation`/`EscalationRecord`/`append_escalation_line` machinery rather than
/// building a parallel mechanism. Never asserts or implies actual phone delivery —
/// `OperatorTransport` (bastion) owns that; this only composes and enqueues the record.
fn compose_and_enqueue_notification(
    config: &OperatorEdgeAuthorConfig,
    req: &OperatorGateRequest,
) -> Result<(), String> {
    let Some(roadmap_dir) = config.roadmap_dir.as_deref() else {
        return Ok(());
    };
    let sha = subject_repo_short_sha(&config.dir)
        .ok_or_else(|| "could not resolve subject repo short SHA".to_string())?;
    let roadmap = config
        .roadmap
        .clone()
        .unwrap_or_else(|| "no-roadmap".to_string());
    let lane = config.lane.clone().unwrap_or_else(|| config.repo.clone());
    let gate_id = format!("{roadmap}/{}/{}", config.repo, config.block_id);

    let options = vec![
        EscalationOption::new("clear", "Clear gate").map_err(|e| e.to_string())?,
        EscalationOption::new("view", "View gate").map_err(|e| e.to_string())?,
    ];
    let channel = EscalationChannel::notification(options).map_err(|e| e.to_string())?;

    let summary: String = req.exit.chars().take(SUMMARY_MAX_CHARS).collect();

    let record = EscalationRecord::new(NewEscalation {
        ts_utc: chrono::Utc::now().to_rfc3339(),
        repo: config.repo.clone(),
        lane,
        kind: EscalationKind::OperatorGate,
        severity: EscalationSeverity::Blocking,
        channel,
        block: Some(config.block_id.clone()),
        gate_id,
        summary,
        verified_by: format!(
            "UNVERIFIED: check_permission_gate authored operator edge '{}' via \
             author_operator_edge",
            req.slug
        ),
        durable_home: serde_json::json!({
            "channel": "state",
            "ref": format!("{}/planning/state.json#operator={}", config.repo, req.slug),
        }),
        verified_at_sha: sha,
        clears_when: Some(format!(
            "state.json no longer carries the operator edge '{}' on {}:{}",
            req.slug, config.repo, config.block_id
        )),
        host: None,
    })
    .map_err(|e| e.to_string())?;

    let path = roadmap_dir.join("escalations.jsonl");
    append_escalation_line(&path, &record).map_err(|e| e.to_string())
}

/// The subject repo's own short git SHA — never the brain root's. Mirrors
/// `integrate.rs`'s private `subject_repo_short_sha` by hand (that one is private to its
/// own module, so it cannot be imported); keep the two in lockstep if either changes.
fn subject_repo_short_sha(repo_path: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .current_dir(repo_path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8(output.stdout).ok()?;
    let sha = sha.trim();
    if sha.is_empty() {
        None
    } else {
        Some(sha.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A minimal brain root: `brain.toml` naming one repo, plus that repo's own
    /// `planning/state.json` carrying exactly one open block — the block the edge gets
    /// authored on. Mirrors `coord/write.rs`'s own `brain_fixture` helper (same
    /// brain-fixture pattern `close_block.rs`'s `EN.15.B` tests established), but this
    /// one needs a real block present since `add_operator_edge_as` looks the key up.
    fn brain_fixture_with_block(repo: &str, block_id: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo_dir = dir.path().join(repo);
        fs::create_dir_all(repo_dir.join("planning")).expect("mkdir");
        fs::write(
            dir.path().join("brain.toml"),
            format!("[[repos]]\nslug = \"{repo}\"\nrepo_path = \"{repo}\"\n"),
        )
        .expect("write brain.toml");
        fs::write(
            repo_dir.join("planning").join("state.json"),
            format!(
                r#"{{ "repo": "{repo}", "kind": "project", "updated": "2026-08-20",
  "focus": {{ "now": [], "next": [], "blocked": [] }},
  "tracks": [{{ "title": "P1", "blocks": [
    {{ "id": "{block_id}", "title": "T", "status": "open", "depends_on": [] }}
  ] }}] }}"#
            ),
        )
        .expect("write state.json");
        (dir, repo_dir)
    }

    fn req(slug: &str) -> OperatorGateRequest {
        OperatorGateRequest {
            slug: slug.to_string(),
            exit: format!("planning/decisions/{slug}.md exists"),
            start: format!("/begin-session {slug}"),
        }
    }

    fn config(root: &Path, repo: &str, block_id: &str, dir: &Path) -> OperatorEdgeAuthorConfig {
        OperatorEdgeAuthorConfig {
            root: root.to_path_buf(),
            repo: repo.to_string(),
            block_id: block_id.to_string(),
            dir: dir.to_path_buf(),
            agent: Some("test-agent".to_string()),
            lock_dir: None,
            roadmap: None,
            lane: None,
            // No roadmap dir: proves the edge is authored even with escalation
            // composition skipped entirely (see `escalation_composition_is_skipped...`
            // for the `Some` case).
            roadmap_dir: None,
        }
    }

    /// The headline case: a real closure built by `make_author_operator_edge`, called
    /// against a real temp `state.json`, actually authors the `{"type":"operator",...}`
    /// edge — proven by reading the file back, not by inspecting a returned struct.
    #[test]
    fn make_author_operator_edge_writes_a_real_operator_edge_to_state_json() {
        let (dir, repo_dir) = brain_fixture_with_block("engine-rs", "EN.99.NOPE");
        let root = dir.path();
        let closure = make_author_operator_edge(config(root, "engine-rs", "EN.99.NOPE", &repo_dir));

        let result = closure(&req("permission-push-to-main"));
        assert!(result.is_ok(), "expected Ok, got {result:?}");

        let written = fs::read_to_string(repo_dir.join("planning").join("state.json"))
            .expect("read state.json back");
        let value: serde_json::Value = serde_json::from_str(&written).expect("valid JSON");
        let block = value["tracks"][0]["blocks"][0]["depends_on"]
            .as_array()
            .expect("depends_on array");
        assert!(
            block.iter().any(|edge| edge["type"] == "operator"
                && edge["slug"] == "permission-push-to-main"),
            "expected an authored operator edge in depends_on, got: {block:?}"
        );
    }

    /// A non-zero/error result from `mev::add_operator_edge_as` — here, a duplicate
    /// slug on the same block, which the underlying verb reports as an error-severity
    /// diagnostic inside an `Ok(Report)`, not an `Err` — is propagated as `Err`, never
    /// mapped to `Ok` or swallowed. This is the failure mode the block exists to close:
    /// a caller that reads `Ok(Report{is_failure: true})` as success silently skips the
    /// gate.
    #[test]
    fn a_duplicate_slug_refusal_is_propagated_as_err_never_swallowed() {
        let (dir, repo_dir) = brain_fixture_with_block("engine-rs", "EN.99.NOPE");
        let root = dir.path();
        let closure = make_author_operator_edge(config(root, "engine-rs", "EN.99.NOPE", &repo_dir));

        let first = closure(&req("permission-push-to-main"));
        assert!(first.is_ok(), "first author must succeed: {first:?}");

        let second = closure(&req("permission-push-to-main"));
        let err = second.expect_err("a duplicate slug on the same block must be refused");
        assert!(
            err.contains("E_OPERATOR_EDGE_DUPLICATE_SLUG") || err.contains("duplicate"),
            "expected the duplicate-slug refusal in the propagated error, got: {err}"
        );
    }

    /// A quiesce refusal — an I/O-level `Err` from `mev::add_operator_edge_as` itself,
    /// distinct from the diagnostic-carrying-`Ok` shape above — is ALSO propagated as
    /// `Err`, proving both failure shapes are mapped, not just one.
    #[test]
    fn a_quiesce_refusal_is_also_propagated_as_err() {
        use crate::coord::write::{lease, LeaseRequest};

        let (dir, repo_dir) = brain_fixture_with_block("engine-rs", "EN.99.NOPE");
        let root = dir.path();
        let lock_dir = root.join(".fleet-locks");
        let now = chrono::Utc::now().to_rfc3339();
        let no_blocks: Vec<String> = Vec::new();

        // A DIFFERENT identity ("someone-else") holds the lease — this closure's own
        // `agent` ("test-agent") must be refused while it is live.
        lease(
            &lock_dir,
            &LeaseRequest {
                repo: "engine-rs",
                lane: "some-other-lane",
                agent: "someone-else",
                kind: okf_core::LeaseKind::Exclusive,
                scope: Some(okf_core::LeaseScope::Repo),
                host: None,
                now_iso: &now,
                window: None,
                lane_blocks: &no_blocks,
            },
        )
        .expect("seed a held lease for a different identity");

        let mut cfg = config(root, "engine-rs", "EN.99.NOPE", &repo_dir);
        cfg.lock_dir = Some(lock_dir);
        cfg.agent = Some("test-agent".to_string());
        let closure = make_author_operator_edge(cfg);

        let result = closure(&req("permission-push-to-main"));
        let err = result.expect_err("a live lease held by a different identity must refuse");
        assert!(
            err.contains("E_QUIESCE_LEASE_HELD") || err.contains("quiesce"),
            "expected the quiesce refusal in the propagated error, got: {err}"
        );
    }

    /// With no `roadmap_dir` configured, the edge is still authored — escalation
    /// composition is a best-effort ADD-ON, never a precondition for the edge write
    /// this block's real job is.
    #[test]
    fn edge_is_authored_even_with_no_roadmap_dir_to_enqueue_an_escalation_onto() {
        let (dir, repo_dir) = brain_fixture_with_block("engine-rs", "EN.99.NOPE");
        let root = dir.path();
        let mut cfg = config(root, "engine-rs", "EN.99.NOPE", &repo_dir);
        cfg.roadmap_dir = None;
        let closure = make_author_operator_edge(cfg);

        assert!(closure(&req("permission-install-on-mini")).is_ok());
    }

    /// With a `roadmap_dir` configured (and the subject repo dir a real git checkout,
    /// which the `engine-rs` workspace itself is), a successful edge-author also
    /// composes and enqueues a schema-valid `operator-gate` notification escalation —
    /// the in-repo half `evidence` names for the un-gateable "reaches the phone"
    /// criterion. Never asserts delivery, only composition + enqueue.
    #[test]
    fn a_successful_edge_author_also_enqueues_a_notification_escalation() {
        let (dir, _repo_dir) = brain_fixture_with_block("engine-rs", "EN.99.NOPE");
        let root = dir.path();
        let roadmap_dir = dir.path().join("roadmap");
        fs::create_dir_all(&roadmap_dir).expect("mkdir roadmap dir");

        let mut cfg = config(
            root,
            "engine-rs",
            "EN.99.NOPE",
            // Use the REAL engine-rs checkout as the subject repo dir so `git
            // rev-parse` resolves a real SHA — the fixture's synthetic `repo_dir`
            // is not a git checkout at all.
            &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")),
        );
        cfg.roadmap_dir = Some(roadmap_dir.clone());
        cfg.roadmap = Some("coordination-layer-port".to_string());
        cfg.lane = Some("engine-rs".to_string());
        let closure = make_author_operator_edge(cfg);

        let result = closure(&req("permission-cross-repo-write"));
        assert!(result.is_ok(), "expected Ok, got {result:?}");

        let escalations_path = roadmap_dir.join("escalations.jsonl");
        let contents = fs::read_to_string(&escalations_path).expect("read escalations.jsonl");
        let line = contents.lines().next().expect("at least one line");
        let value: serde_json::Value = serde_json::from_str(line).expect("valid JSON line");
        assert_eq!(value["kind"], "operator-gate");
        assert_eq!(value["channel"], "notification");
        assert_eq!(
            value["gate_id"],
            "coordination-layer-port/engine-rs/EN.99.NOPE"
        );
        assert!(
            value["options"].as_array().map(|a| a.len()).unwrap_or(0) >= 2,
            "notification channel must carry its options: {value:?}"
        );
    }
}
