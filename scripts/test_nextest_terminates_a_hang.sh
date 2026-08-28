#!/usr/bin/env bash
# scripts/test_nextest_terminates_a_hang.sh
#
# EN.ticket.test-gate-must-terminate-a-hang-not-wedge task 3 — D64 fixture-evidence
# script. Proves that `.config/nextest.toml`'s `slow-timeout`/`terminate-after` bound
# actually kills a wedged test and reports it as TIMEOUT, rather than the gate hanging
# forever with no verdict — which is what the original (unreproducible) P1 report
# looked like.
#
# `cargo nextest run` is spawned as a CHILD PROCESS, so its verdict lives outside this
# repo's in-process test harness. This script must therefore bound its own wait: macOS
# has no `timeout` binary (CLAUDE.md HQ trap 5), so it backgrounds the nextest run,
# polls for completion with a wall-clock budget, and kills the process group if the
# budget is exceeded. A script that hangs waiting for a hang detector is exactly the
# failure mode this exists to catch — it must never itself block indefinitely.
#
# Checks the SOURCE TREE's `.config/nextest.toml` (nextest reads it automatically from
# the repo root) — there is no installed binary in this path, so no source-vs-installed
# divergence to declare.
#
# Exit 0  — the nextest run returned inside the wall-clock budget AND its output shows
#           a TIMEOUT verdict for the wedging fixture.
# Exit 1  — the run did not return inside the budget (killed by this script), or it
#           returned without a TIMEOUT verdict for the fixture.

set -u

cd "$(dirname "${BASH_SOURCE[0]}")/.." || exit 1

# Wall-clock budget for the whole nextest invocation. The fixture's own per-test
# override in .config/nextest.toml bounds it to a 5s period x 1 terminate-after (~5s),
# plus nextest startup/compile overhead — 60s leaves a wide margin without making a
# genuinely broken gate (terminate-after silently absent) wait anywhere near the
# fixture's own 3600s sleep.
BUDGET_SECS=60

OUT_FILE="$(mktemp -t nextest_gate_timeout_fixture.XXXXXX)"
trap 'rm -f "$OUT_FILE"' EXIT

# Run nextest in its own process group (setsid-less portable approach: background it
# and track the PID) so we can kill the whole tree if it overruns the budget.
cargo nextest run -p engine-core --test it --run-ignored only \
  -E 'test(deliberately_wedges)' >"$OUT_FILE" 2>&1 &
NEXTEST_PID=$!

elapsed=0
while kill -0 "$NEXTEST_PID" 2>/dev/null; do
  if [ "$elapsed" -ge "$BUDGET_SECS" ]; then
    echo "FAIL: cargo nextest run did not return within ${BUDGET_SECS}s — terminate-after did not fire (or is missing/misconfigured)." >&2
    # Kill the whole process group so a still-wedged child test doesn't outlive us.
    kill -TERM -- "-$NEXTEST_PID" 2>/dev/null
    kill -TERM "$NEXTEST_PID" 2>/dev/null
    sleep 1
    kill -KILL -- "-$NEXTEST_PID" 2>/dev/null
    kill -KILL "$NEXTEST_PID" 2>/dev/null
    wait "$NEXTEST_PID" 2>/dev/null
    echo "----- captured output before kill -----" >&2
    cat "$OUT_FILE" >&2
    exit 1
  fi
  sleep 1
  elapsed=$((elapsed + 1))
done

wait "$NEXTEST_PID"
NEXTEST_EXIT=$?

if grep -qi 'TIMEOUT' "$OUT_FILE"; then
  echo "PASS: cargo nextest run returned in <= ${elapsed}s and reported a TIMEOUT verdict (exit code ${NEXTEST_EXIT})."
  exit 0
else
  echo "FAIL: cargo nextest run returned in ${elapsed}s (exit code ${NEXTEST_EXIT}) but no TIMEOUT verdict was found in its output." >&2
  echo "----- captured output -----" >&2
  cat "$OUT_FILE" >&2
  exit 1
fi
