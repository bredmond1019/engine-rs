//! Integration suite for `EN.9.F` — coverage unit tests cannot exercise on
//! their own: an on-disk manifest reload picked up mid-run with no rebuild,
//! and semaphore-bounded queueing under a real concurrent burst.
//!
//! Every manifest fixture here is written to a `tempfile::tempdir()` — never
//! into `crates/term-core/src/detect/manifests/`, which is the exact pattern
//! the tracked `diagnostic-intake-state-json-rewritten-by-test-suite` defect
//! covers and which is still open.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use engine_core::nodes::terminal::{
    AdmissionControl, AdmissionPolicy, ManifestOrigin, ManifestSource, NoMatchAlarmPolicy,
    NoMatchAlarmTracker,
};
use term_core::detect::{detect, AgentState};

fn write_manifest(
    dir: &Path,
    name: &str,
    rule_name: &str,
    needle: &str,
    state: &str,
) -> std::path::PathBuf {
    let toml = format!(
        r#"
name = "{rule_name}"

[[rules]]
state = "{state}"
gate = {{ contains = "{needle}" }}
"#
    );
    let path = dir.join(name);
    fs::write(&path, toml).expect("write tempdir manifest fixture");
    path
}

/// Some macOS/CI filesystems round mtime to ~1s resolution — sleep past
/// that before rewriting so the reload path actually observes a change,
/// matching `manifest_source.rs`'s own unit-test precedent
/// (`touch_later`).
fn rewrite_manifest_later(
    dir: &Path,
    name: &str,
    rule_name: &str,
    needle: &str,
    state: &str,
) -> std::path::PathBuf {
    thread::sleep(Duration::from_millis(1100));
    write_manifest(dir, name, rule_name, needle, state)
}

/// An on-disk manifest edit takes effect on the next capture with no
/// rebuild and no process restart.
#[test]
fn on_disk_manifest_edit_is_picked_up_by_the_next_capture_with_no_rebuild() {
    let dir = tempfile::tempdir().expect("tempdir for manifest fixture");
    let path = write_manifest(
        dir.path(),
        "claude.toml",
        "v1-manifest",
        "V1-MARKER",
        "working",
    );

    let source = ManifestSource::new(Some(path.clone()));

    // First capture: screen carries the v1 marker, classifies under the v1
    // manifest's rule.
    let resolved_v1 = source.resolve();
    assert_eq!(resolved_v1.origin, ManifestOrigin::Override(path.clone()));
    let detection_v1 = detect("...V1-MARKER on screen...", &resolved_v1.manifest);
    assert_eq!(detection_v1.state, AgentState::Working);

    // A screen carrying only the (not-yet-existing) v2 marker does not
    // match the v1 manifest.
    let pre_edit_probe = detect("...V2-MARKER on screen...", &resolved_v1.manifest);
    assert_eq!(pre_edit_probe.state, AgentState::Unknown);

    // Edit the manifest on disk mid-"run" — no process restart, no rebuild.
    rewrite_manifest_later(
        dir.path(),
        "claude.toml",
        "v2-manifest",
        "V2-MARKER",
        "idle",
    );

    // Next capture resolves the EDITED manifest and classifies accordingly.
    let resolved_v2 = source.resolve();
    assert_ne!(resolved_v2.digest, resolved_v1.digest);
    let detection_v2 = detect("...V2-MARKER on screen...", &resolved_v2.manifest);
    assert_eq!(detection_v2.state, AgentState::Idle);

    // And the OLD marker no longer matches the new manifest — proof this
    // is a real reload, not a stale cache serving the v1 rule alongside.
    let old_marker_after_edit = detect("...V1-MARKER on screen...", &resolved_v2.manifest);
    assert_eq!(old_marker_after_edit.state, AgentState::Unknown);
}

/// The no-match alarm names the override manifest's digest across a
/// reload, end to end through `ManifestSource` + `NoMatchAlarmTracker`
/// together (task 1 + task 2 wired as a real caller would use them).
#[test]
fn reload_then_no_match_alarm_names_the_currently_active_override_digest() {
    let dir = tempfile::tempdir().expect("tempdir for manifest fixture");
    let path = write_manifest(dir.path(), "claude.toml", "v1", "MATCH-V1", "working");
    let source = ManifestSource::new(Some(path));

    let tracker = NoMatchAlarmTracker::new(NoMatchAlarmPolicy {
        consecutive_unmatched_threshold: 2,
    });

    let resolved = source.resolve();
    let detection = detect("no marker present here", &resolved.manifest);
    assert_eq!(detection.state, AgentState::Unknown);
    assert_eq!(
        tracker.record_state(&resolved.manifest.name, &resolved.digest, detection.state),
        None
    );

    let resolved_again = source.resolve();
    let detection_again = detect("still no marker present", &resolved_again.manifest);
    let alarm = tracker
        .record_state(
            &resolved_again.manifest.name,
            &resolved_again.digest,
            detection_again.state,
        )
        .expect("2nd consecutive unmatched capture should raise");

    assert_eq!(alarm.manifest_digest, resolved_again.digest);
    assert_eq!(alarm.manifest_digest, resolved.digest); // unchanged manifest, same digest
}

/// A burst of terminal-node "runs" beyond the semaphore limit queues:
/// observed concurrency never exceeds the configured limit, and every run
/// eventually completes.
#[tokio::test]
async fn burst_beyond_the_semaphore_limit_queues_and_every_run_completes() {
    const LIMIT: usize = 4;
    const BURST: usize = 40;

    let control = AdmissionControl::new(AdmissionPolicy {
        max_concurrent_terminal_runs: LIMIT as u32,
    });

    let current = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::with_capacity(BURST);
    for _ in 0..BURST {
        let control = control.clone();
        let current = current.clone();
        let peak = peak.clone();
        let completed = completed.clone();
        handles.push(tokio::spawn(async move {
            // Simulates a terminal-node run: acquire admission, do work,
            // release on drop.
            let _permit = control.acquire().await;
            let now = current.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(5)).await;
            current.fetch_sub(1, Ordering::SeqCst);
            completed.fetch_add(1, Ordering::SeqCst);
        }));
    }

    for handle in handles {
        handle
            .await
            .expect("every queued terminal-node run should complete");
    }

    assert!(
        peak.load(Ordering::SeqCst) <= LIMIT,
        "observed concurrency {} exceeded the configured limit {}",
        peak.load(Ordering::SeqCst),
        LIMIT
    );
    assert_eq!(completed.load(Ordering::SeqCst), BURST);
    assert_eq!(control.available_permits(), LIMIT);
}

/// Runs beyond the limit specifically QUEUE rather than fail: a run
/// submitted while the control is saturated is still pending (not yet
/// admitted, not errored) until an in-flight run releases its permit.
#[tokio::test]
async fn a_run_submitted_while_saturated_queues_instead_of_failing() {
    let control = AdmissionControl::new(AdmissionPolicy {
        max_concurrent_terminal_runs: 1,
    });

    let first = control.acquire().await;
    assert_eq!(control.available_permits(), 0);

    let control2 = control.clone();
    let queued = tokio::spawn(async move { control2.acquire().await });

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !queued.is_finished(),
        "a run beyond the limit must still be queued, not admitted"
    );

    drop(first);

    let permit = tokio::time::timeout(Duration::from_millis(500), queued)
        .await
        .expect("queued run should be admitted promptly once a permit frees up")
        .expect("queued task should join without panicking");
    assert_eq!(control.available_permits(), 0);
    drop(permit);
    assert_eq!(control.available_permits(), 1);
}
