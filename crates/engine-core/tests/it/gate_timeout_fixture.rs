//! Deliberately-wedging fixture for `EN.ticket.test-gate-must-terminate-a-hang-not-wedge`.
//!
//! This is the ONE test in the suite that never returns. It exists solely so
//! `scripts/test_nextest_terminates_a_hang.sh` can prove that `.config/nextest.toml`'s
//! `slow-timeout`/`terminate-after` bound actually kills a wedged test and reports it as
//! TIMEOUT rather than letting it hang the gate forever.
//!
//! `#[ignore]` keeps it out of every normal run (`cargo nextest run --workspace
//! --all-features`) — it only ever runs via `--run-ignored only -E 'test(deliberately_wedges)'`,
//! which is exactly what the fixture script does.

#[test]
#[ignore]
fn deliberately_wedges_to_prove_the_gate_terminates_it() {
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}
