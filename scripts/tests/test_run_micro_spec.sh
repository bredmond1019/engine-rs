#!/usr/bin/env bash
#
# scripts/tests/test_run_micro_spec.sh
#
# EN.ticket.micro-spec-fixture-for-engine-seam-comparison task 5 — D64
# fixture-evidence for scripts/run_micro_spec.sh. The fixture (the
# micro-spec) is itself the test instrument for engine comparison, so what
# this gates is the RUNNER's harvest ORDERING, not the micro-spec's
# content: a runner that harvests in the wrong order still exits 0 and
# still goes green under the ordinary Rust suite (`fmt`/`clippy`/
# `nextest`/`build` never invoke this script at all), so that suite is not
# evidence for the harvest-before-next claim. This script is.
#
# WHAT IS STUBBED: `curl` is shimmed on PATH (scripts/tests/fixtures/
# micro_spec_fake_curl.py, installed as an executable named `curl`) so this
# test never touches a live `bastion serve`. The shim answers the two calls
# run_micro_spec.sh makes:
#   - POST .../events/   -> {"event_id": "<distinct id>"}, and as a SIDE
#                            EFFECT (standing in for what the real engine
#                            would do) writes/overwrites
#                            planning/<spec>/sdlc/sdlc-flow-state.json with
#                            that SAME event_id embedded in its content.
#   - GET  .../events/<id> -> {"status": "succeeded"} immediately.
#
# WHY CONTENT, NOT FILENAME, IS WHAT "DISTINCT" MEANS HERE: run_micro_spec.sh
# names each harvested file after the event_id from ITS OWN trigger
# response, so harvested FILENAMES are always distinct — 3 dispatches, 3
# names — even when the underlying shared state file was clobbered by a
# later run before an earlier run's copy was ever taken. That clobbering is
# exactly the defect harvest-before-next exists to prevent, and it is only
# visible by reading what is INSIDE each harvested file: this script counts
# distinct embedded event_ids across the harvested *.json files, not the
# count of files.
#
# Runs entirely under a disposable mktemp -d; run_micro_spec.sh is copied
# into a fresh scripts/ under that temp dir (never invoked from its real
# location) specifically so it cannot source the real, gitignored
# scripts/.env — hermeticity here does not depend on that file being
# absent or empty.
#
# POSITIVE CONTROL, OBSERVED (recorded per the verify_move_fidelity.sh /
# nextest-terminates-a-hang precedent — the numbers below are what this
# script itself printed on a real run against the tree at the time this
# task landed, not a claim that they were checked):
#   - Case 1 (-k 3, normal order):            3 distinct records -> PASS
#   - Case 2 (-k 3 --defer-harvest):           1 distinct record  -> PASS
#                                              (fewer than k=3, as expected)
#   - Case 3 (--clean removes leftover state): file absent after -> PASS
#   - Overall script exit code:                                     0
#
# Exit 0 — all three cases behaved as expected.
# Exit 1 — any case did not behave as expected (printed to stderr).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
FIXTURE_CURL="$REPO_ROOT/scripts/tests/fixtures/micro_spec_fake_curl.py"

if [ ! -f "$FIXTURE_CURL" ]; then
    echo "FAIL: missing fixture $FIXTURE_CURL" >&2
    exit 1
fi

TMPDIR_ROOT="$(mktemp -d -t test_run_micro_spec.XXXXXX)"
cleanup() { rm -rf "$TMPDIR_ROOT"; }
trap cleanup EXIT

# ── Isolated playground: copy the runner into a scripts/ dir with no .env ──

mkdir -p "$TMPDIR_ROOT/work/scripts"
cp "$REPO_ROOT/scripts/run_micro_spec.sh" "$TMPDIR_ROOT/work/scripts/run_micro_spec.sh"
chmod +x "$TMPDIR_ROOT/work/scripts/run_micro_spec.sh"

# ── Fake `curl` on PATH ─────────────────────────────────────────────────────

SHIM_BIN="$TMPDIR_ROOT/bin"
mkdir -p "$SHIM_BIN"
cat > "$SHIM_BIN/curl" <<SHIM
#!/usr/bin/env bash
exec python3 "$FIXTURE_CURL" "\$@"
SHIM
chmod +x "$SHIM_BIN/curl"

CURL_STATE_DIR="$TMPDIR_ROOT/curl-state"
mkdir -p "$CURL_STATE_DIR"

export PATH="$SHIM_BIN:$PATH"
export MICRO_SPEC_CURL_STATE_DIR="$CURL_STATE_DIR"
export BASTION_ENGINE_API_KEY="fake-key-for-test-run-micro-spec"
export BASTION_SERVE_ADDR="http://localhost:0"   # never actually dialed — curl is shimmed

FAIL=0

# distinct_event_ids <dir> — count distinct embedded event_ids across every
# harvested *.json record in <dir> (excluding the *.meta.json siblings,
# which are metadata about the harvest, not the harvested payload).
distinct_event_ids() {
    local dir="$1"
    python3 - "$dir" <<'PYEOF'
import glob, json, os, sys
d = sys.argv[1]
ids = set()
for path in glob.glob(os.path.join(d, "*.json")):
    if path.endswith(".meta.json"):
        continue
    try:
        with open(path) as f:
            payload = json.load(f)
    except Exception:
        continue
    ids.add(payload.get("event_id"))
print(len(ids))
PYEOF
}

cd "$TMPDIR_ROOT/work"

# ── Case 1: -k 3, normal harvest order -> 3 distinct records ────────────────

OUT1="$TMPDIR_ROOT/work/out-normal"
set +e
bash scripts/run_micro_spec.sh --spec micro-spec-small -k 3 --out "$OUT1" >"$TMPDIR_ROOT/case1.log" 2>&1
CASE1_RC=$?
set -e
CASE1_COUNT="$(distinct_event_ids "$OUT1")"
echo "case 1: exit=$CASE1_RC distinct_records=$CASE1_COUNT (log: $TMPDIR_ROOT/case1.log)"
if [ "$CASE1_RC" -ne 0 ]; then
    echo "FAIL: case 1 (normal harvest) exited $CASE1_RC, expected 0" >&2
    tail -n 40 "$TMPDIR_ROOT/case1.log" >&2
    FAIL=1
fi
if [ "$CASE1_COUNT" -ne 3 ]; then
    echo "FAIL: case 1 (normal harvest) produced $CASE1_COUNT distinct records, expected 3" >&2
    FAIL=1
fi

# ── Case 2: -k 3 --defer-harvest -> POSITIVE CONTROL, fewer than 3 ─────────
#
# A green case 1 alone does not prove ordering is load-bearing — this case
# must be OBSERVED producing fewer than k distinct records, not merely
# assumed to.

OUT2="$TMPDIR_ROOT/work/out-deferred"
set +e
bash scripts/run_micro_spec.sh --spec micro-spec-small -k 3 --defer-harvest --out "$OUT2" >"$TMPDIR_ROOT/case2.log" 2>&1
CASE2_RC=$?
set -e
CASE2_COUNT="$(distinct_event_ids "$OUT2")"
echo "case 2: exit=$CASE2_RC distinct_records=$CASE2_COUNT (log: $TMPDIR_ROOT/case2.log)"
if [ "$CASE2_RC" -ne 0 ]; then
    echo "FAIL: case 2 (--defer-harvest) exited $CASE2_RC, expected 0" >&2
    tail -n 40 "$TMPDIR_ROOT/case2.log" >&2
    FAIL=1
fi
if [ "$CASE2_COUNT" -ge 3 ]; then
    echo "FAIL: case 2 (--defer-harvest) produced $CASE2_COUNT distinct records, expected FEWER than 3 — the positive control did not fire" >&2
    FAIL=1
fi

# ── Case 3: --clean removes the leftover state file before the next dispatch ─

STATE_FILE="planning/micro-spec-small/sdlc/sdlc-flow-state.json"
mkdir -p "$(dirname "$STATE_FILE")"
echo '{"leftover": true}' > "$STATE_FILE"
if [ ! -f "$STATE_FILE" ]; then
    echo "FAIL: could not seed the leftover state file for case 3" >&2
    FAIL=1
fi

set +e
bash scripts/run_micro_spec.sh --spec micro-spec-small --clean >"$TMPDIR_ROOT/case3.log" 2>&1
CASE3_RC=$?
set -e
echo "case 3: exit=$CASE3_RC state_file_present_after=$([ -f "$STATE_FILE" ] && echo yes || echo no) (log: $TMPDIR_ROOT/case3.log)"
if [ "$CASE3_RC" -ne 0 ]; then
    echo "FAIL: case 3 (--clean) exited $CASE3_RC, expected 0" >&2
    tail -n 40 "$TMPDIR_ROOT/case3.log" >&2
    FAIL=1
fi
if [ -f "$STATE_FILE" ]; then
    echo "FAIL: case 3 (--clean) left $STATE_FILE in place — a leftover state file is exactly what makes the next dispatch RESUME a run already marked done" >&2
    FAIL=1
fi

if [ "$FAIL" -ne 0 ]; then
    echo "test_run_micro_spec.sh: FAIL" >&2
    exit 1
fi

echo "test_run_micro_spec.sh: PASS (case1=$CASE1_COUNT/3 distinct, case2=$CASE2_COUNT/3 distinct (control), case3=clean ok)"
exit 0
