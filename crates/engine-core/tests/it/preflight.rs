//! Integration tests for preflight (`EN.17.D` task 2): per-block claim
//! extraction and the per-program `argv` validator with no shell.
//!
//! Every test name is prefixed `preflight_` per the task's testing
//! strategy. The safety-boundary tests (`rg --pre`, `git --output`, the
//! ripgrep-config-env isolation, shell metacharacters, timeout) call
//! `preflight::test_support`'s claim builders and runners directly — they
//! do not need a judgment call at all, since the thing under test is the
//! argv validator and the no-shell runner, not claim extraction. The
//! higher-level tests (`SkippedNoRecord`, the claim cap, a false
//! load-bearing verdict) drive `PreflightRunner::run_for_block` end to end
//! with a stubbed judgment transport, so the gated suite never spawns a
//! real `claude` subprocess — the claim commands themselves
//! (`rg`/`git`/`test`/`ls`) still run for real.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use claude_code_rs::parse::Usage as SdkUsage;
use claude_code_rs::{Config, Outcome};
use engine_contract::TaskContext;
use engine_core::repo_registry::RepoRegistry;
use engine_core::workflows::orchestration::preflight::{
    test_support, PreflightConfig, PreflightOutcome, PreflightRunner,
};
use engine_core::workflows::ModelTransport;
use futures::future::BoxFuture;
use futures::FutureExt;
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Shared fixtures
// ---------------------------------------------------------------------------

fn empty_ctx() -> TaskContext {
    TaskContext {
        event: json!({}),
        nodes: HashMap::new(),
        metadata: json!({}),
        node_runs: HashMap::new(),
    }
}

/// A tempdir `brain.toml` + one real repo (`repo-a`), optionally carrying a
/// `planning/blocks/<id>.json` record.
struct FixtureRepo {
    _dir: tempfile::TempDir,
    registry: RepoRegistry,
}

fn fixture_repo_with_block(block_id: &str, what: &str, files: Value, ac: Value) -> FixtureRepo {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo_root = dir.path().join("repo-a");
    std::fs::create_dir_all(repo_root.join("planning").join("blocks")).unwrap();
    std::fs::write(
        dir.path().join("brain.toml"),
        "[[repos]]\nslug = \"repo-a\"\nrepo_path = \"repo-a\"\n",
    )
    .unwrap();
    let record = json!({
        "id": block_id,
        "what": what,
        "files": files,
        "acceptance_criteria": ac,
    });
    std::fs::write(
        repo_root
            .join("planning")
            .join("blocks")
            .join(format!("{block_id}.json")),
        serde_json::to_string_pretty(&record).unwrap(),
    )
    .unwrap();

    let registry = RepoRegistry::from_brain_root(dir.path()).expect("registry");
    FixtureRepo {
        _dir: dir,
        registry,
    }
}

/// A tempdir `brain.toml` + one real repo (`repo-a`) with NO block record —
/// for the `SkippedNoRecord` case.
fn fixture_repo_no_record() -> FixtureRepo {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("repo-a")).unwrap();
    std::fs::write(
        dir.path().join("brain.toml"),
        "[[repos]]\nslug = \"repo-a\"\nrepo_path = \"repo-a\"\n",
    )
    .unwrap();
    let registry = RepoRegistry::from_brain_root(dir.path()).expect("registry");
    FixtureRepo {
        _dir: dir,
        registry,
    }
}

fn stub_outcome_structured(structured: Value) -> Outcome {
    Outcome {
        cost_usd: 0.01,
        usage: SdkUsage {
            input_tokens: 10,
            output_tokens: 5,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
        model_usage: BTreeMap::new(),
        text: structured.to_string(),
        is_error: false,
        api_error_status: None,
        session_id: Some("sess-preflight".to_string()),
        structured_output: Some(structured),
    }
}

/// A stub transport that always returns the same canned `claims` reply.
fn stub_claims(claims: Value) -> ModelTransport {
    let outcome = stub_outcome_structured(json!({ "claims": claims }));
    Arc::new(move |_config: Config, _prompt: String| {
        let outcome = outcome.clone();
        async move { Ok(outcome) }.boxed() as BoxFuture<'static, claude_code_rs::Result<Outcome>>
    })
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

// ---------------------------------------------------------------------------
// SkippedNoRecord
// ---------------------------------------------------------------------------

#[tokio::test]
async fn preflight_missing_record_is_skipped() {
    let fixture = fixture_repo_no_record();
    let runner = PreflightRunner::new(PreflightConfig::default());
    let result = runner
        .run_for_block(&empty_ctx(), &fixture.registry, "repo-a", "NO.SUCH.BLOCK")
        .await;
    assert!(matches!(result.outcome, PreflightOutcome::SkippedNoRecord));
    assert!(result.claims.is_empty());
    assert_eq!(result.claims_dropped, 0);
}

// ---------------------------------------------------------------------------
// A false load-bearing claim / a non-load-bearing false claim
// ---------------------------------------------------------------------------

#[tokio::test]
async fn preflight_false_load_bearing_claim_yields_false_verdict() {
    let fixture = fixture_repo_with_block(
        "EX.1",
        "Adds a file that does not actually exist.",
        json!({ "new": [{ "path": "definitely-not-here.rs" }] }),
        json!(["definitely-not-here.rs exists"]),
    );
    let claims = json!([{
        "claim": "definitely-not-here.rs exists",
        "load_bearing": true,
        "argv": ["test", "-e", "definitely-not-here.rs"],
        "expect": "exit_zero",
        "needle": null,
    }]);
    let runner =
        PreflightRunner::new(PreflightConfig::default()).with_transport(stub_claims(claims));

    let result = runner
        .run_for_block(&empty_ctx(), &fixture.registry, "repo-a", "EX.1")
        .await;

    match result.outcome {
        PreflightOutcome::Judged { claims } => {
            assert_eq!(claims.len(), 1);
            assert!(claims[0].load_bearing);
            assert!(test_support::verdict_is_false(&claims[0]));
        }
        other => panic!("expected Judged, got {other:?}"),
    }
}

#[tokio::test]
async fn preflight_non_load_bearing_false_claim_is_recorded() {
    let fixture = fixture_repo_with_block(
        "EX.2",
        "A record with one non-load-bearing aside that happens to be false.",
        json!({}),
        json!([]),
    );
    let claims = json!([{
        "claim": "an incidental, non-load-bearing claim that is false",
        "load_bearing": false,
        "argv": ["test", "-e", "still-not-here.rs"],
        "expect": "exit_zero",
        "needle": null,
    }]);
    let runner =
        PreflightRunner::new(PreflightConfig::default()).with_transport(stub_claims(claims));

    let result = runner
        .run_for_block(&empty_ctx(), &fixture.registry, "repo-a", "EX.2")
        .await;

    match result.outcome {
        PreflightOutcome::Judged { claims } => {
            assert_eq!(claims.len(), 1);
            assert!(!claims[0].load_bearing);
            assert!(test_support::verdict_is_false(&claims[0]));
        }
        other => panic!("expected Judged, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Claim cap
// ---------------------------------------------------------------------------

#[tokio::test]
async fn preflight_claim_cap_is_enforced() {
    let fixture = fixture_repo_with_block("EX.3", "many claims", json!({}), json!([]));
    let many_claims: Vec<Value> = (0..5)
        .map(|i| {
            json!({
                "claim": format!("claim {i}"),
                "load_bearing": false,
                "argv": ["ls", "-1"],
                "expect": "exit_zero",
                "needle": null,
            })
        })
        .collect();
    let mut config = PreflightConfig::default();
    config.max_claims = 2;
    let runner = PreflightRunner::new(config).with_transport(stub_claims(json!(many_claims)));

    let result = runner
        .run_for_block(&empty_ctx(), &fixture.registry, "repo-a", "EX.3")
        .await;

    assert_eq!(result.claims.len(), 2);
    assert_eq!(result.claims_dropped, 3);
}

// ---------------------------------------------------------------------------
// The safety boundary — direct validator/runner tests, no judgment call.
// ---------------------------------------------------------------------------

#[test]
fn preflight_rg_pre_is_refused_and_never_executes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("marker");
    let script = dir.path().join("make_marker.sh");
    std::fs::write(
        &script,
        format!("#!/bin/sh\ntouch \"{}\"\n", marker.display()),
    )
    .unwrap();
    let mut perms = std::fs::metadata(&script).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&script, perms).unwrap();

    // POSITIVE CONTROL: the script really does create the marker when run
    // directly.
    let status = std::process::Command::new(&script)
        .status()
        .expect("run script directly");
    assert!(status.success());
    assert!(marker.exists(), "positive control: marker must exist");
    std::fs::remove_file(&marker).unwrap();

    for pre_argv in [
        argv(&["rg", "--pre", script.to_str().unwrap(), "x", "."]),
        argv(&[
            "rg",
            &format!("--pre={}", script.to_str().unwrap()),
            "x",
            ".",
        ]),
        argv(&["rg", "--pre-glob", script.to_str().unwrap(), "x", "."]),
    ] {
        let claim = test_support::claim("rg --pre variant", true, pre_argv, None);
        let result =
            test_support::run_one_claim(&claim, dir.path(), None, Duration::from_millis(2000));
        assert!(
            test_support::verdict_is_unverifiable(&result),
            "rg --pre variant must be unverifiable"
        );
        assert!(
            !marker.exists(),
            "rg --pre must never actually run the script"
        );
    }
}

#[test]
fn preflight_git_output_and_global_options_are_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    // A minimal git repo so `git log` would otherwise succeed.
    let status = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .expect("git init");
    assert!(status.success());

    let out_path = dir.path().join("git-output-target");
    let claims: Vec<(Vec<String>, &str)> = vec![
        (
            argv(&["git", "log", &format!("--output={}", out_path.display())]),
            "output",
        ),
        (argv(&["git", "-c", "core.pager=x", "log"]), "global option"),
        (argv(&["git", "push"]), "push"),
        (argv(&["/usr/bin/rg", "x"]), "path program"),
    ];

    for (claim_argv, label) in claims {
        let claim = test_support::claim(label, true, claim_argv, None);
        let result =
            test_support::run_one_claim(&claim, dir.path(), None, Duration::from_millis(2000));
        assert!(
            test_support::verdict_is_unverifiable(&result),
            "{label} must be unverifiable"
        );
    }
    assert!(
        !out_path.exists(),
        "git log --output must never actually write its target"
    );

    let temp_file = dir.path().join("do-not-remove-me");
    std::fs::write(&temp_file, "keep me").unwrap();
    let rm_claim =
        test_support::claim("rm", true, argv(&["rm", temp_file.to_str().unwrap()]), None);
    let rm_result =
        test_support::run_one_claim(&rm_claim, dir.path(), None, Duration::from_millis(2000));
    assert!(test_support::verdict_is_unverifiable(&rm_result));
    assert!(temp_file.exists(), "rm must never actually run");
}

#[test]
fn preflight_ripgrep_config_env_is_not_inherited() {
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("ripgrep-config-marker");
    let script = dir.path().join("marker.sh");
    std::fs::write(
        &script,
        format!("#!/bin/sh\ntouch \"{}\"\n", marker.display()),
    )
    .unwrap();
    let mut perms = std::fs::metadata(&script).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&script, perms).unwrap();

    let rg_config = dir.path().join("ripgreprc");
    std::fs::write(&rg_config, format!("--pre={}\n", script.display())).unwrap();

    std::fs::write(dir.path().join("haystack.txt"), "hello world\n").unwrap();

    // SAFETY: test-process-local env var mutation, guarded by not running
    // concurrently with anything else that reads RIPGREP_CONFIG_PATH in this
    // process.
    std::env::set_var("RIPGREP_CONFIG_PATH", &rg_config);
    let claim = test_support::claim(
        "an allowed rg claim",
        false,
        argv(&["rg", "-n", "hello", "haystack.txt"]),
        Some("hello"),
    );
    let result = test_support::run_one_claim(&claim, dir.path(), None, Duration::from_millis(2000));
    std::env::remove_var("RIPGREP_CONFIG_PATH");

    assert!(
        !marker.exists(),
        "RIPGREP_CONFIG_PATH must not be inherited by the cleared-environment runner"
    );
    assert!(
        test_support::verdict_is_held(&result),
        "the allowed rg claim itself should still succeed: {result:?}"
    );
}

#[test]
fn preflight_no_shell_metacharacters_are_interpreted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let redirect_target = dir.path().join("should-not-be-created");

    let claim = test_support::claim_exit_nonzero(
        "a `>` positional argument must never be shell-redirected",
        false,
        argv(&["ls", ">", "should-not-be-created"]),
    );
    let _ = test_support::run_one_claim(&claim, dir.path(), None, Duration::from_millis(2000));

    assert!(
        !redirect_target.exists(),
        "no shell means `>` is just a literal argument, never a redirect"
    );
}

#[test]
fn preflight_command_timeout_is_unverifiable_with_timeout_reason() {
    let dir = tempfile::tempdir().expect("tempdir");
    let claim = test_support::claim(
        "a command given no time to run",
        true,
        argv(&["ls", "-1"]),
        None,
    );
    // A zero-duration budget forces the deadline check to fire on (or
    // before) the very first poll, regardless of how fast `ls` itself is.
    let result = test_support::run_one_claim(&claim, dir.path(), None, Duration::from_millis(0));
    assert!(test_support::verdict_is_unverifiable(&result));
    assert_eq!(result.reason.as_deref(), Some("timeout"));
}
