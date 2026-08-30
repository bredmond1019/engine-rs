//! Integration test for `commit_write_manifest`
//! (`EN.ticket.close-block-node-leaves-derived-output-uncommitted`, task
//! 2) against a REAL temporary git repository with a symlinked
//! `planning/` face — mirroring this fleet's actual shape, where every
//! sub-repo's `planning/` is a symlink into a vault directory tracked by a
//! single HQ git repo (CLAUDE.md, "Planning symlinks").
//!
//! Uses the real `git` binary via [`ProcessGitRunner`] (through the public
//! `commit_write_manifest` entry point) rather than a fake — this is
//! exactly the seam a fake would rubber-stamp: whether `git add`/`git
//! commit` actually resolve a manifest path through a symlink face onto
//! real repo content, and whether an explicit-pathspec commit really
//! excludes everything else that happens to be dirty in the same tree.

use std::path::PathBuf;
use std::process::Command;

use engine_core::workflows::sdlc_flow::close_block::commit_write_manifest;

fn run_git(dir: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(dir)
        .args(args)
        .status()
        .unwrap_or_else(|e| panic!("failed to run git {args:?} in {}: {e}", dir.display()));
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

fn output_git(dir: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to run git {args:?} in {}: {e}", dir.display()));
    assert!(
        out.status.success(),
        "git {args:?} failed in {}",
        dir.display()
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Build a fresh temp git repo (`git init`, an initial commit so `HEAD`
/// exists), with a `_planning_vault/` real directory and a `planning`
/// symlink pointing at it — the exact shape `CLAUDE.md`'s "Planning
/// symlinks" section describes for this fleet.
fn init_repo_with_symlinked_planning() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "engine-rs-close-block-it-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();

    run_git(&dir, &["init", "-q"]);
    run_git(&dir, &["config", "user.email", "test@example.com"]);
    run_git(&dir, &["config", "user.name", "Test"]);

    std::fs::create_dir_all(dir.join("_planning_vault")).unwrap();
    std::fs::write(dir.join("_planning_vault/.gitkeep"), "").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(dir.join("_planning_vault"), dir.join("planning")).unwrap();

    std::fs::write(dir.join("README.md"), "init\n").unwrap();
    run_git(&dir, &["add", "README.md", "planning", "_planning_vault"]);
    run_git(&dir, &["commit", "-q", "-m", "init"]);

    dir
}

#[test]
fn a_manifest_path_behind_the_planning_symlink_is_committed_and_a_decoy_is_not() {
    let dir = init_repo_with_symlinked_planning();

    // The derived file `set_block_status` regenerated, addressed through
    // the symlink face exactly as the real manifest would carry it.
    let manifest_path = dir.join("planning/master-plan.md");
    std::fs::write(&manifest_path, "regenerated content\n").unwrap();

    // An unrelated dirty file NOT in the manifest — must stay out of the
    // commit even though it is dirty in the same working tree.
    let decoy_path = dir.join("README.md");
    std::fs::write(&decoy_path, "decoy edit, must not be committed\n").unwrap();

    let before_head = output_git(&dir, &["rev-parse", "HEAD"]);

    let result = commit_write_manifest(std::slice::from_ref(&manifest_path));

    assert!(
        result.failed.is_empty(),
        "expected no staging failures, got: {:?}",
        result.failed
    );
    assert_eq!(result.committed, vec![manifest_path.clone()]);

    let after_head = output_git(&dir, &["rev-parse", "HEAD"]);
    assert_ne!(before_head, after_head, "a new commit must have been made");

    let changed_files = output_git(&dir, &["show", "--name-only", "--pretty=format:", "HEAD"]);
    let changed: Vec<&str> = changed_files.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        changed,
        vec!["_planning_vault/master-plan.md"],
        "the commit must contain ONLY the manifest path (resolved through the symlink), \
         never the decoy README.md edit"
    );

    // The decoy is still dirty on disk — proving it was never staged/committed.
    let status = output_git(&dir, &["status", "--porcelain"]);
    assert!(
        status.contains("README.md"),
        "decoy file must remain uncommitted/dirty: {status}"
    );
    assert!(
        !status.contains("master-plan.md"),
        "the manifest path must be clean (committed): {status}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn one_unstageable_path_does_not_block_the_rest_of_the_manifest() {
    let dir = init_repo_with_symlinked_planning();

    let good_path = dir.join("planning/master-plan.md");
    std::fs::write(&good_path, "regenerated content\n").unwrap();

    // Not a real file on disk — `canonicalize` fails, so this path must be
    // reported as a failure without blocking the good path from landing.
    let bad_path = dir.join("planning/does-not-exist.md");

    let result = commit_write_manifest(&[bad_path.clone(), good_path.clone()]);

    assert_eq!(result.committed, vec![good_path.clone()]);
    assert_eq!(result.failed.len(), 1);
    assert_eq!(result.failed[0].0, bad_path);

    let status = output_git(&dir, &["status", "--porcelain"]);
    assert!(
        !status.contains("master-plan.md"),
        "the good path must still have committed: {status}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_empty_manifest_leaves_the_repo_untouched() {
    let dir = init_repo_with_symlinked_planning();
    let before_head = output_git(&dir, &["rev-parse", "HEAD"]);

    let result = commit_write_manifest(&[]);

    assert!(result.committed.is_empty());
    assert!(result.failed.is_empty());
    let after_head = output_git(&dir, &["rev-parse", "HEAD"]);
    assert_eq!(
        before_head, after_head,
        "no commit must be made for an empty manifest"
    );

    std::fs::remove_dir_all(&dir).ok();
}
