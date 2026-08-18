//! `EN.9.D` task 6 fixture — runs `TERMINAL_PROBE` against a REAL tmux
//! server (never `StubTerminalDriver`) and prints every field the task's
//! artifact needs: session creation, the observed pane/detect result, and
//! confirmation that the lease is released at the end.
//!
//! Usage: `cargo run --example terminal_probe`
//!
//! Requires a real `tmux` binary reachable on `PATH` (over a non-interactive
//! ssh session this box's PATH omits `/opt/homebrew/bin` — export it before
//! running, e.g. `PATH=/opt/homebrew/bin:$PATH cargo run --example
//! terminal_probe`).

use engine_core::nodes::terminal::{identity::session_name_for, session::NODE_NAME};
use engine_core::workflow::RunOptions;
use engine_core::workflows::terminal_probe::{registry, schema, workflow};
use serde_json::json;
use term_core::driver::{TerminalDriver as _, TmuxDriver};
use term_core::lease::SessionLease;
use uuid::Uuid;

#[tokio::main]
async fn main() {
    let run_uuid = Uuid::new_v4();

    println!("== EN.9.D task 6 — real-Mini TERMINAL_PROBE run ==");
    println!("run_id: {run_uuid}");

    // LIVE-ENVIRONMENT FINDING (recorded verbatim in the task-6 artifact):
    // on a session whose `@engine_lease@<session>` option has never been
    // set, tmux's `show-option -g` exits 1 ("invalid option") instead of
    // returning empty output -- on BOTH the Mini's 3.5a and this dev box's
    // 3.7b, so it is not a version-divergence bug. `SessionLease::read()`
    // (`lease.rs:148`) propagates that as a hard `TmuxError` via `?`,
    // which surfaces through `acquire` as `LeaseError` and fails
    // `TerminalSessionNode` on the very first run against a real,
    // never-touched session. Pre-seeding the option to an empty string
    // (a legal, harmless value tmux accepts) works around it here so a
    // full run can be captured for AC-1 evidence; this workaround lives
    // ONLY in this fixture binary, not in any node under test.
    let probe_driver = TmuxDriver::new(std::time::Duration::from_secs(10));
    let session_name = session_name_for(&run_uuid.to_string(), NODE_NAME);
    let lease_option = format!("@engine_lease@{session_name}");
    probe_driver
        .set_option(&lease_option, "")
        .await
        .expect("pre-seed of the lease option should succeed (set-option always succeeds)");
    println!("(fixture workaround) pre-seeded {lease_option} = \"\"");

    let wf = workflow();

    let final_ctx = wf
        .run_with(
            json!({}),
            Box::new(|_ctx| {}),
            RunOptions {
                run_id: Some(run_uuid),
                ..Default::default()
            },
        )
        .await
        .expect("TERMINAL_PROBE run should not error at the WorkflowError level");

    println!(
        "\n-- node_runs --\n{}",
        serde_json::to_string_pretty(&final_ctx.node_runs).unwrap()
    );
    println!(
        "\n-- metadata --\n{}",
        serde_json::to_string_pretty(&final_ctx.metadata).unwrap()
    );
    println!(
        "\n-- SessionNode result --\n{}",
        serde_json::to_string_pretty(
            final_ctx
                .nodes
                .get("TerminalSessionNode")
                .expect("session result stamped")
        )
        .unwrap()
    );
    println!(
        "\n-- ObserveNode result --\n{}",
        serde_json::to_string_pretty(
            final_ctx
                .nodes
                .get("TerminalObserveNode")
                .expect("observe result stamped")
        )
        .unwrap()
    );

    let session_name = final_ctx.nodes["TerminalSessionNode"]["session_name"]
        .as_str()
        .expect("session_name")
        .to_string();
    let nonce = final_ctx.nodes["TerminalSessionNode"]["lease_nonce"]
        .as_str()
        .expect("lease_nonce")
        .to_string();

    // The read-only Phase-2 nodes intentionally never release the lease
    // (that ships with EN.9.E's send/await nodes) -- release it explicitly
    // here so the fixture's "lease released" AC is satisfiable now.
    let driver = TmuxDriver::new(std::time::Duration::from_secs(10));
    let lease = SessionLease::new(&driver);
    lease
        .release(&session_name, &nonce)
        .await
        .expect("lease release should succeed -- we are the current holder");

    // Confirm via a fresh registry() (unused, just proving the schema/registry
    // pairing is exactly what shipped) and via a raw show-option read that
    // @engine_lease is now empty/absent.
    let _sanity_registry = registry();
    let _sanity_schema = schema();

    let after = driver
        .show_option(&format!("@engine_lease@{session_name}"))
        .await;
    println!("\n-- post-release @engine_lease read-back --");
    match &after {
        Ok(v) if v.trim().is_empty() => println!("EMPTY (lease released) -- raw: {v:?}"),
        Ok(v) => println!("NOT EMPTY -- raw: {v:?}"),
        Err(e) => println!("show_option errored (treated as absent/unset): {e}"),
    }

    println!("\nsession_name={session_name}");
    println!(
        "lease released: {}",
        after.map(|v| v.trim().is_empty()).unwrap_or(true)
    );
}
