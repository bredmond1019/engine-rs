//! Commander scoped emit + manifest-ONLY commit (`EN.15.F` task 2).
//!
//! Runs `mev::emit_state_as` in-process — the exact function `mev emit-state --write`'s CLI
//! (and, per the block record, `bastion emit-state --write`) wraps — scoped to the running
//! repo's own derived surfaces (`--scope <repo>`) and self-exempting a lease this same
//! identity holds (`--agent <me>`), then commits ONLY the resulting `I_EMIT_WROTE` manifest
//! paths.
//!
//! **The commit half is deliberately NOT reimplemented here.** `super::super::sdlc_flow::
//! close_block::commit_write_manifest_with_runner` already realpath-resolves every manifest
//! path (a leaf repo's `planning/` is a symlink face; git tracks the vault target, so an
//! un-canonicalized pathspec silently matches nothing), groups paths by the git repository
//! each canonicalizes into, and stages + commits each group ONE PATH AT A TIME with an
//! explicit pathspec on both `git add` and `git commit -o` — never `-A`, never a bare
//! pathspec-less commit. That is the exact contract this block's acceptance criteria demand;
//! reusing it means this module inherits its existing coverage instead of duplicating (and
//! potentially drifting from) it.
//!
//! **`E_QUIESCE_LEASE_HELD` is a declared quiet window, not contention.** A drain whose scoped
//! emit lands on a repo under a FOREIGN lease is refused by `mev`'s own quiesce guard before
//! any write happens; this module surfaces that refusal as [`EmitCommitOutcome::Refused`] and
//! makes exactly one `emit_state_as` call — it never retries a refusal, mirroring the
//! `notify-operator`/`ping-agent` discipline that a declared quiet window is not an error to
//! recover from by trying again.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::workflows::sdlc_flow::close_block::{
    commit_write_manifest_with_runner, GitRunner, ManifestCommitResult, ProcessGitRunner,
};

/// The outcome of one commander scoped-emit-and-commit pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmitCommitOutcome {
    /// The emit produced a non-empty `I_EMIT_WROTE` manifest and the commit step ran over it.
    Committed {
        /// The manifest this pass emitted, in first-seen order, de-duplicated.
        manifest: Vec<PathBuf>,
        /// The staging/commit result over that manifest.
        commit: ManifestCommitResult,
    },
    /// The emit ran and produced no `I_EMIT_WROTE` diagnostic — a clean no-op. No `git`
    /// invocation happens on this path.
    NoOp,
    /// The emit was refused because a DIFFERENT identity holds this repo's quiesce lease
    /// (`E_QUIESCE_LEASE_HELD`). Reported, never retried.
    Refused {
        /// `mev`'s own refusal text, verbatim, so the drain-log/report carries the real
        /// reason rather than a re-paraphrased one.
        reason: String,
    },
    /// The emit failed for a reason OTHER than a foreign lease (an unknown `--scope` slug, a
    /// missing/unreadable `brain.toml`, or any other `mev::emit_state_as` error).
    Failed {
        /// The failure text, verbatim.
        reason: String,
    },
}

impl EmitCommitOutcome {
    /// Whether this pass was refused by a foreign lease (`E_QUIESCE_LEASE_HELD`) rather than
    /// completing (with or without work to do) or failing outright.
    #[must_use]
    pub fn is_refused(&self) -> bool {
        matches!(self, EmitCommitOutcome::Refused { .. })
    }
}

/// Collect the write manifest from a `mev::emit_state_as` [`mev::Report`]: every
/// `I_EMIT_WROTE` diagnostic's `file`, in first-seen order, de-duplicated.
///
/// Byte-identical logic to `close_block::write_manifest` (private there, over a
/// `set_block_status_as` report). Duplicated rather than exposed across modules because the
/// two callers read reports produced by different `mev` entry points and must stay
/// independently editable — a future change to one manifest's shape must not silently ripple
/// into the other's.
#[must_use]
pub fn write_manifest(report: &mev::Report) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut manifest = Vec::new();
    for d in &report.diagnostics {
        if d.locator == "I_EMIT_WROTE" && seen.insert(d.file.clone()) {
            manifest.push(d.file.clone());
        }
    }
    manifest
}

/// Run `mev::emit_state_as(root, write: true, scope: <repo_slug>, agent: <agent>, ...)`
/// in-process, then commit ONLY the resulting manifest via the production [`ProcessGitRunner`].
///
/// `dir` is the specific repo's own checkout directory (not `root`) — the same argument
/// `mev`'s own `emit-state` CLI passes as the quiesce guard's "who is asking, from where" seam
/// (`main.rs`'s `EmitState` arm passes its own `path`; `--agent` self-exempts a lease this
/// identity holds while a DIFFERENT identity is still refused).
#[must_use]
pub fn run_scoped_emit_and_commit(
    root: &Path,
    repo_slug: &str,
    agent: &str,
    dir: &Path,
    lock_dir: Option<&Path>,
) -> EmitCommitOutcome {
    let mut runner = ProcessGitRunner;
    run_scoped_emit_and_commit_with(root, repo_slug, agent, dir, lock_dir, &mut runner)
}

/// [`run_scoped_emit_and_commit`] with an injectable [`GitRunner`] — the seam tests use to
/// assert on constructed argv and to drive a real temporary git repository without touching
/// this crate's own working tree.
#[must_use]
pub fn run_scoped_emit_and_commit_with(
    root: &Path,
    repo_slug: &str,
    agent: &str,
    dir: &Path,
    lock_dir: Option<&Path>,
    git_runner: &mut dyn GitRunner,
) -> EmitCommitOutcome {
    let config = match mev::brain::config::load_brain_config(&root.join("brain.toml")) {
        Ok(cfg) => cfg,
        Err(e) => {
            return EmitCommitOutcome::Failed {
                reason: format!("brain.toml not found or unreadable: {e}"),
            }
        }
    };
    let scope = match config.scope_dependencies(repo_slug) {
        Ok(deps) => deps,
        Err(e) => {
            return EmitCommitOutcome::Failed {
                reason: format!("E_EMIT_UNKNOWN_SCOPE: {e}"),
            }
        }
    };

    // Exactly ONE call — no retry loop of any kind around this. A refusal below is reported,
    // not retried; a genuine failure is reported too.
    let report = match mev::emit_state_as(root, true, Some(&scope), Some(agent), lock_dir, dir) {
        Ok(report) => report,
        Err(err) => {
            let message = format!("{err:#}");
            if message.contains(mev::E_QUIESCE_LEASE_HELD) {
                return EmitCommitOutcome::Refused { reason: message };
            }
            return EmitCommitOutcome::Failed { reason: message };
        }
    };

    let manifest = write_manifest(&report);
    if manifest.is_empty() {
        return EmitCommitOutcome::NoOp;
    }

    let commit = commit_write_manifest_with_runner(&manifest, git_runner);
    EmitCommitOutcome::Committed { manifest, commit }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    // --- fixture builders --------------------------------------------------------------

    /// A minimal brain root valid enough for `BrainConfig::scope_dependencies` to resolve
    /// `repo_slug`: one `[[repos]]` entry for `repo_slug` itself, plus the HQ root entry
    /// `scope_dependencies` requires (`repo_path == "."`) for its `hq_board_status_file`.
    fn brain_fixture(repo_slug: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo_dir = dir.path().join(repo_slug);
        std::fs::create_dir_all(repo_dir.join("planning")).expect("mkdir");
        std::fs::write(
            dir.path().join("brain.toml"),
            format!(
                r#"[[repos]]
slug = "{repo_slug}"
tier = "core"
repo_path = "{repo_slug}"
status_file = "{repo_slug}/planning/status.md"
cache_doc = "docs/projects/{repo_slug}.md"

[[repos]]
slug = "hq"
repo_path = "."
status_file = "planning/status.md"
cache_doc = "docs/projects/hq.md"
"#
            ),
        )
        .expect("write brain.toml");
        std::fs::write(
            repo_dir.join("planning").join("state.json"),
            format!(
                r#"{{ "repo": "{repo_slug}", "kind": "project", "updated": "2026-08-20",
  "focus": {{ "now": [], "next": [], "blocked": [] }},
  "tracks": [{{ "title": "P1", "blocks": [] }}] }}"#
            ),
        )
        .expect("write state.json");
        (dir, repo_dir)
    }

    /// Write one live, `exclusive`, `scope: repo` lease file under
    /// `<root>/.fleet-locks/leases/` — the on-disk shape `mev::brain::lease::check_quiesce`
    /// reads. Mirrors `close_block`'s own `write_lease` test helper
    /// (`EN.15.B` task 2's real-`.fleet-locks`-tree testing strategy: drive the guard through
    /// the actual files it reads, rather than mocking `mev`).
    fn write_lease(root: &Path, repo: &str, agent: &str) {
        let leases_dir = root.join(".fleet-locks").join("leases");
        std::fs::create_dir_all(&leases_dir).expect("mkdir .fleet-locks/leases");
        let acquired_at = chrono::Utc::now().to_rfc3339();
        let raw = format!(
            r#"{{"repo": "{repo}", "lane": "test-lane", "agent": "{agent}", "acquired_at": "{acquired_at}", "kind": "exclusive", "scope": "repo"}}"#
        );
        std::fs::write(leases_dir.join("lease-test.json"), raw).expect("write lease file");
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(dir)
            .args(args)
            .status()
            .expect("failed to run git");
        assert!(status.success(), "git {args:?} failed in {}", dir.display());
    }

    fn init_git_repo(dir: &Path) {
        git(dir, &["init", "-q"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        git(dir, &["config", "user.name", "Test"]);
    }

    fn last_commit_stat(dir: &Path) -> String {
        let output = Command::new("git")
            .current_dir(dir)
            .args(["log", "-1", "--stat", "--format=%s"])
            .output()
            .expect("git log -1 --stat");
        assert!(output.status.success(), "git log -1 --stat failed");
        String::from_utf8_lossy(&output.stdout).to_string()
    }

    // --- write_manifest ------------------------------------------------------------------

    #[test]
    fn write_manifest_collects_i_emit_wrote_files_deduplicated_in_first_seen_order() {
        let report = mev::Report {
            diagnostics: vec![
                mev::Diagnostic::warning("state.json", "I_EMIT_WROTE", "wrote: state"),
                mev::Diagnostic::warning("status.md", "I_EMIT_WROTE", "wrote: status"),
                // A repeat of the first path must not duplicate in the manifest.
                mev::Diagnostic::warning("state.json", "I_EMIT_WROTE", "wrote: state again"),
                // A non-I_EMIT_WROTE diagnostic must never leak into the manifest.
                mev::Diagnostic::warning("status.md", "W_EMIT_DRY_RUN", "dry run"),
            ],
        };
        let manifest = write_manifest(&report);
        assert_eq!(
            manifest,
            vec![PathBuf::from("state.json"), PathBuf::from("status.md")]
        );
    }

    #[test]
    fn write_manifest_is_empty_for_a_report_with_no_i_emit_wrote_diagnostics() {
        let report = mev::Report::default();
        assert!(write_manifest(&report).is_empty());
    }

    // --- the manifest-ONLY commit, and the mutation test that proves it matters ----------

    /// **AC 1 & 3.** After a drain, `git log -1 --stat` names ONLY the manifest paths, even
    /// with a dirty AUTHORED file (never in the manifest) sitting beside a derived one in the
    /// tree — and the manifest path is realpath'd through a `planning/`-style symlink face
    /// before being handed to git, so the symlink cannot silently make the pathspec match
    /// nothing.
    #[test]
    fn commit_step_commits_only_the_manifest_path_resolved_through_a_symlink_face() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        init_git_repo(root);
        // Baseline commit so `git log -1` below has something to diff against.
        std::fs::write(root.join("README.md"), "baseline\n").expect("write baseline");
        git(root, &["add", "README.md"]);
        git(root, &["commit", "-q", "-m", "init"]);

        // The real target a leaf repo's `planning/` symlink face points at.
        std::fs::create_dir_all(root.join("_planning").join("engine-rs")).expect("mkdir vault");
        std::fs::write(
            root.join("_planning").join("engine-rs").join("state.json"),
            "{}",
        )
        .expect("write derived file");
        // The symlink FACE the manifest path names — mirrors a leaf repo's `planning/`.
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            root.join("_planning").join("engine-rs"),
            root.join("planning"),
        )
        .expect("symlink planning -> _planning/engine-rs");

        // A dirty AUTHORED file, never named by the manifest, sitting beside the derived one.
        std::fs::write(root.join("authored.rs"), "// authored change\n")
            .expect("write authored file");

        let manifest = vec![root.join("planning").join("state.json")];
        let mut runner = ProcessGitRunner;
        let result = commit_write_manifest_with_runner(&manifest, &mut runner);

        assert!(
            result.failed.is_empty(),
            "unexpected failures: {:?}",
            result.failed
        );
        assert_eq!(result.committed, manifest);

        let stat = last_commit_stat(root);
        assert!(
            stat.contains("state.json"),
            "commit must name the manifest's derived file: {stat}"
        );
        assert!(
            !stat.contains("authored.rs"),
            "commit must NOT name the undeclared authored file: {stat}"
        );

        // The authored file is still dirty afterwards — never swept in.
        let status = Command::new("git")
            .current_dir(root)
            .args(["status", "--porcelain"])
            .output()
            .expect("git status");
        let status_text = String::from_utf8_lossy(&status.stdout);
        assert!(
            status_text.contains("authored.rs"),
            "authored.rs must remain uncommitted (dirty): {status_text}"
        );
    }

    /// **AC 2, the load-bearing mutation test.** Substituting the manifest-only commit with
    /// `workflows::commit_all`'s `git add -A` makes the SAME assertion — "the commit names
    /// only the manifest's declared file" — go RED, over the identical starting tree. Both
    /// directions are asserted in this one test, at runtime, by swapping which commit
    /// function is exercised; no `expect_red`, no harness check is named.
    #[test]
    fn substituting_commit_all_add_dash_a_makes_the_manifest_only_assertion_go_red() {
        // --- direction 1: the real seam (manifest-only, explicit pathspec) is CLEAN -----
        let clean_dir = tempfile::tempdir().expect("tempdir");
        let clean_root = clean_dir.path();
        init_git_repo(clean_root);
        std::fs::write(clean_root.join("README.md"), "baseline\n").expect("write baseline");
        git(clean_root, &["add", "README.md"]);
        git(clean_root, &["commit", "-q", "-m", "init"]);

        std::fs::write(clean_root.join("derived.json"), "{}").expect("write derived");
        std::fs::write(clean_root.join("authored.rs"), "// authored\n").expect("write authored");

        let manifest = vec![clean_root.join("derived.json")];
        let mut runner = ProcessGitRunner;
        let clean_result = commit_write_manifest_with_runner(&manifest, &mut runner);
        assert!(clean_result.failed.is_empty());

        let clean_stat = last_commit_stat(clean_root);
        let manifest_only_is_clean =
            clean_stat.contains("derived.json") && !clean_stat.contains("authored.rs");
        assert!(
            manifest_only_is_clean,
            "the real seam must commit only the manifest file: {clean_stat}"
        );

        // --- direction 2: an IDENTICAL starting tree, but committed via `commit_all` ----
        // (`workflows::mod::commit_all`, which really does `git add -A` then `git commit`)
        // must fail the exact same assertion — proving the assertion is sensitive to the
        // pathspec discipline, not merely to something happening.
        let dirty_dir = tempfile::tempdir().expect("tempdir");
        let dirty_root = dirty_dir.path();
        init_git_repo(dirty_root);
        std::fs::write(dirty_root.join("README.md"), "baseline\n").expect("write baseline");
        git(dirty_root, &["add", "README.md"]);
        git(dirty_root, &["commit", "-q", "-m", "init"]);

        std::fs::write(dirty_root.join("derived.json"), "{}").expect("write derived");
        std::fs::write(dirty_root.join("authored.rs"), "// authored\n").expect("write authored");

        let command_runner = crate::workflows::default_command_runner();
        let outcome = crate::workflows::commit_all(&command_runner, dirty_root, "chore: mutation");
        assert!(matches!(
            outcome,
            crate::workflows::CommitOutcome::Committed
        ));

        let dirty_stat = last_commit_stat(dirty_root);
        let manifest_only_is_clean_under_add_all =
            dirty_stat.contains("derived.json") && !dirty_stat.contains("authored.rs");
        assert!(
            !manifest_only_is_clean_under_add_all,
            "MUTATION CHECK: substituting `commit_all`'s `git add -A` must swallow the \
             undeclared authored file too, which is exactly what makes the manifest-only \
             assertion go RED here: {dirty_stat}"
        );
    }

    // --- the refusal path: E_QUIESCE_LEASE_HELD is reported, never retried ---------------

    /// **AC 4.** An emit refused because a DIFFERENT identity holds the repo's quiesce lease
    /// is REPORTED by the drain (`EmitCommitOutcome::Refused`, carrying `mev`'s own
    /// `E_QUIESCE_LEASE_HELD` text), never swallowed, and this function makes exactly one
    /// `emit_state_as` call to reach that verdict — there is no retry loop to have retried it.
    #[test]
    fn emit_refused_by_a_foreign_lease_is_reported_not_swallowed_and_never_retried() {
        let (dir, repo_dir) = brain_fixture("engine-rs");
        let root = dir.path();

        write_lease(root, "engine-rs", "some-other-lane-holder");

        let mut runner = ProcessGitRunner;
        let outcome = run_scoped_emit_and_commit_with(
            root,
            "engine-rs",
            "this-drain-identity",
            &repo_dir,
            None,
            &mut runner,
        );

        match &outcome {
            EmitCommitOutcome::Refused { reason } => {
                assert!(
                    reason.contains(mev::E_QUIESCE_LEASE_HELD),
                    "refusal reason must carry mev's own E_QUIESCE_LEASE_HELD text: {reason}"
                );
            }
            other => panic!("expected Refused, got {other:?}"),
        }
        assert!(outcome.is_refused());
    }

    /// An unknown `--scope` slug fails cleanly (`E_EMIT_UNKNOWN_SCOPE`) rather than panicking
    /// or silently proceeding against the wrong repo's surfaces.
    #[test]
    fn an_unknown_scope_slug_fails_with_e_emit_unknown_scope() {
        let (dir, repo_dir) = brain_fixture("engine-rs");
        let root = dir.path();

        let mut runner = ProcessGitRunner;
        let outcome = run_scoped_emit_and_commit_with(
            root,
            "not-a-registered-repo",
            "this-drain-identity",
            &repo_dir,
            None,
            &mut runner,
        );

        match outcome {
            EmitCommitOutcome::Failed { reason } => {
                assert!(
                    reason.contains("E_EMIT_UNKNOWN_SCOPE"),
                    "expected E_EMIT_UNKNOWN_SCOPE in the failure reason: {reason}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }
}
