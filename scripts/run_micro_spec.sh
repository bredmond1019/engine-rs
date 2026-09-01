#!/usr/bin/env bash
#
# run_micro_spec.sh
#
# EN.ticket.micro-spec-fixture-for-engine-seam-comparison task 4 — dispatch a
# named micro-spec (planning/micro-spec-small or planning/micro-spec-large) k
# times against a named engine and profile, HARVESTING each run's flow-state
# artifact BEFORE the next dispatch begins.
#
# Modelled on scripts/sdlc_smoke.sh: same scripts/.env sourcing, same
# BASTION_SERVE_ADDR default (http://localhost:4317), same
# BASTION_ENGINE_API_KEY requirement, same POST /events/ trigger + GET
# /events/{event_id} poll to a terminal status (succeeded | failed |
# cancelled | budget_halted; live: running | suspended). This script never
# opens the SSE streaming route.
#
# WHY HARVEST ORDER IS LOAD-BEARING: every leaf repo's planning/ is a
# gitignored symlink into ONE shared vault target, so k runs of the SAME
# spec all write the SAME planning/<spec>/sdlc/sdlc-flow-state.json — run 2
# overwrites run 1 in place. The harvested copy must be taken, and the state
# reset via --clean, before the next dispatch starts, or later runs silently
# erase earlier ones. This is measured, not theoretical (jynx JX.3.B
# criterion 6).
#
# WHY --clean MUST RUN BETWEEN EVERY PAIR OF DISPATCHES, not only before the
# first: SpecExistsRouterNode::route (crates/engine-core/src/workflows/
# sdlc_flow/setup.rs) routes to LoadTaskStateNode when
# planning/<spec>/sdlc/sdlc-flow-state.json OR tasks.json exists. A leftover
# state file from the previous run in the batch makes the NEXT dispatch
# RESUME a run already marked done — it "succeeds" instantly having executed
# nothing, and the harvested record for that run is a lie.
#
# --defer-harvest is a NEGATIVE CONTROL, not a feature: it runs all k
# dispatches first and harvests only at the end, so it is expected — and, in
# scripts/tests/test_run_micro_spec.sh, is DELIBERATELY SHOWN — to produce
# fewer than k distinct records, since later runs overwrite the shared state
# file before it is ever copied out.
#
# Usage:
#   run_micro_spec.sh --spec <slug> [options]
#   run_micro_spec.sh --help
#
# Required:
#   --spec <slug>        micro-spec-small | micro-spec-large
#
# Options:
#   -k <N>                Number of consecutive runs (default: 3).
#   --profile <name>      Policy profile passed through on the event body
#                          (default: cheap-fast).
#   --engine <rust|js>    Which engine dispatches the spec (default: rust).
#                          "rust" triggers the live SDLC_FLOW event via
#                          POST /events/. "js" shells out to the legacy JS
#                          engine. Whichever is not implemented EXITS 3
#                          naming what is missing — it never silently falls
#                          back to the other engine, because a silent
#                          fallback would make every comparison number a lie
#                          about which engine actually produced it.
#   --out <dir>            Directory harvested records land in (default:
#                          planning/micro-spec-runs/).
#   --defer-harvest       NEGATIVE CONTROL — see above. Run all k dispatches
#                          before harvesting anything, instead of harvesting
#                          before each next dispatch. Expected to produce
#                          FEWER than k distinct records; documented as a
#                          control, never as a normal mode.
#   --clean               Pre-run reset only (no dispatch, no watch),
#                          mirroring sdlc_smoke.sh --clean: removes
#                          trees/sdlc/<spec>, deletes branch sdlc/<spec>, and
#                          rm -rf's planning/<spec>/sdlc. Also invoked
#                          automatically between every pair of dispatches in
#                          a k-run batch (see above) — this flag standing
#                          alone is for a manual one-off reset.
#   --help                 Show this help and exit.
#
# On the last run of the batch (run k), after that run reaches its first
# terminal-status transition, this script re-attaches to the same event via
# a second GET /events/{event_id} poll — exercising the resume seam on
# every k-run batch rather than only when a human happens to interrupt one.
#
# Harvested filenames are keyed on the run's event_id (never a timestamp or
# a loop index), as <out>/<spec>-<engine>-<profile>-<event_id>.json, plus a
# sibling <event_id>.meta.json carrying wall-clock seconds and the observed
# terminal status.
#
# Environment (scripts/.env, gitignored, or already-exported):
#   BASTION_SERVE_ADDR         Base URL of bastion serve (default:
#                               http://localhost:4317)
#   BASTION_ENGINE_API_KEY     X-API-Key for the engine routes (required)
#   RUN_MICRO_SPEC_POLL_INTERVAL     Seconds between polls (default: 3)
#   RUN_MICRO_SPEC_TIMEOUT_MINUTES   Minutes before giving up on a live run
#                               (default: 20)
#
# Exit codes (matching sdlc_smoke.sh):
#   0   all k runs reached `succeeded`
#   1   any run terminal-failed (failed | cancelled | budget_halted)
#   2   timed out while a run was still live
#   3   usage error (bad flags, unknown --engine, unimplemented engine)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# ── Config ───────────────────────────────────────────────────────────────────

ENV_FILE="$SCRIPT_DIR/.env"
if [ -f "$ENV_FILE" ]; then
    # shellcheck source=/dev/null
    source "$ENV_FILE"
fi

BASTION_ADDR="${BASTION_SERVE_ADDR:-http://localhost:4317}"
POLL_INTERVAL="${RUN_MICRO_SPEC_POLL_INTERVAL:-3}"
TIMEOUT_MINUTES="${RUN_MICRO_SPEC_TIMEOUT_MINUTES:-20}"

# ── Flags ────────────────────────────────────────────────────────────────────

SPEC=""
K=3
PROFILE="cheap-fast"
ENGINE="rust"
OUT_DIR="planning/micro-spec-runs"
DEFER_HARVEST=0
CLEAN_ONLY=0

print_help() {
    cat <<'EOF'
run_micro_spec.sh — dispatch a micro-spec k times, harvesting before each next run

Usage:
  run_micro_spec.sh --spec <slug> [options]
  run_micro_spec.sh --help

Required:
  --spec <slug>        micro-spec-small | micro-spec-large

Options:
  -k <N>                Number of consecutive runs (default: 3).
  --profile <name>      Policy profile passed through on the event body
                         (default: cheap-fast).
  --engine <rust|js>    Which engine dispatches the spec (default: rust).
                         "rust" triggers the live SDLC_FLOW event via
                         POST /events/. "js" shells out to the legacy JS
                         engine. Whichever is not implemented EXITS 3 naming
                         what is missing -- it never silently falls back to
                         the other engine, because a silent fallback would
                         make every comparison number a lie about which
                         engine actually produced it.
  --out <dir>            Directory harvested records land in (default:
                         planning/micro-spec-runs/).
  --defer-harvest       NEGATIVE CONTROL, not a feature: run all k
                         dispatches before harvesting anything, instead of
                         harvesting before each next dispatch. This is
                         expected -- and is deliberately shown in
                         scripts/tests/test_run_micro_spec.sh -- to produce
                         FEWER than k distinct records, because later runs
                         overwrite the shared planning/<spec>/sdlc state
                         file before it is ever copied out. Document as a
                         control, never treat as a normal mode.
  --clean               Pre-run reset only (no dispatch, no watch): removes
                         trees/sdlc/<spec>, deletes branch sdlc/<spec>, and
                         rm -rf's planning/<spec>/sdlc. Also invoked
                         automatically between every pair of dispatches in a
                         k-run batch -- this flag standing alone is for a
                         manual one-off reset.
  --help                 Show this help and exit.

Harvested filenames are keyed on the run's event_id (never a timestamp or a
loop index): <out>/<spec>-<engine>-<profile>-<event_id>.json, plus a
sibling <event_id>.meta.json with wall-clock seconds and terminal status.

On the last run of a k-run batch, after that run's first terminal-status
transition, this script re-attaches via a second GET /events/{event_id}
poll -- exercising the resume seam on every batch.

Environment (scripts/.env, gitignored, or already-exported):
  BASTION_SERVE_ADDR               Base URL of bastion serve (default:
                                    http://localhost:4317)
  BASTION_ENGINE_API_KEY           X-API-Key for the engine routes (required)
  RUN_MICRO_SPEC_POLL_INTERVAL     Seconds between polls (default: 3)
  RUN_MICRO_SPEC_TIMEOUT_MINUTES   Minutes before giving up on a live run
                                    (default: 20)

Exit codes:
  0   all k runs reached `succeeded`
  1   any run terminal-failed (failed | cancelled | budget_halted)
  2   timed out while a run was still live
  3   usage error (bad flags, unknown --engine, unimplemented engine)
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --spec)
            SPEC="${2:-}"
            if [ -z "$SPEC" ]; then
                echo "error: --spec requires a slug argument" >&2
                exit 3
            fi
            shift 2
            ;;
        -k)
            K="${2:-}"
            if [ -z "$K" ]; then
                echo "error: -k requires a numeric argument" >&2
                exit 3
            fi
            shift 2
            ;;
        --profile)
            PROFILE="${2:-}"
            if [ -z "$PROFILE" ]; then
                echo "error: --profile requires a name argument" >&2
                exit 3
            fi
            shift 2
            ;;
        --engine)
            ENGINE="${2:-}"
            if [ -z "$ENGINE" ]; then
                echo "error: --engine requires an argument (rust|js)" >&2
                exit 3
            fi
            shift 2
            ;;
        --out)
            OUT_DIR="${2:-}"
            if [ -z "$OUT_DIR" ]; then
                echo "error: --out requires a directory argument" >&2
                exit 3
            fi
            shift 2
            ;;
        --defer-harvest)
            DEFER_HARVEST=1
            shift
            ;;
        --clean)
            CLEAN_ONLY=1
            shift
            ;;
        --help|-h)
            print_help
            exit 0
            ;;
        *)
            echo "error: unrecognized argument: $1" >&2
            print_help
            exit 3
            ;;
    esac
done

case "$K" in
    ''|*[!0-9]*)
        echo "error: -k must be a positive integer, got '$K'" >&2
        exit 3
        ;;
esac

# ── Engine gate: never silently fall back ───────────────────────────────────

case "$ENGINE" in
    rust) : ;;
    js)
        echo "error: --engine js is not implemented by run_micro_spec.sh (no JS dispatch path is wired here) -- refusing to silently fall back to the rust engine, since that would make every comparison number a lie about which engine produced it" >&2
        exit 3
        ;;
    *)
        echo "error: unknown --engine '$ENGINE' (expected rust or js)" >&2
        exit 3
        ;;
esac

# ── Clean helper: reset one spec's worktree/branch/state ───────────────────
#
# Mirrors sdlc_smoke.sh --clean. The planning/<spec>/sdlc removal is the line
# that matters: SpecExistsRouterNode::route
# (crates/engine-core/src/workflows/sdlc_flow/setup.rs) routes to
# LoadTaskStateNode when sdlc/sdlc-flow-state.json OR tasks.json exists, so a
# leftover state file makes the NEXT dispatch RESUME a run already marked
# done and "succeed" instantly having executed nothing.

clean_spec() {
    local spec="$1"

    echo "Removing worktree trees/sdlc/$spec ..."
    git worktree remove --force "trees/sdlc/$spec" \
        && echo "  removed." \
        || echo "  not present (ok)."

    echo "Deleting branch sdlc/$spec ..."
    git branch -D "sdlc/$spec" \
        && echo "  deleted." \
        || echo "  not present (ok)."

    echo "Removing leftover flow state planning/$spec/sdlc ..."
    if [ -d "planning/$spec/sdlc" ]; then
        rm -rf "planning/$spec/sdlc"
        echo "  removed."
    else
        echo "  not present (ok)."
    fi
}

if [ "$CLEAN_ONLY" -eq 1 ] && [ -z "$SPEC" ]; then
    echo "error: --clean requires --spec <slug>" >&2
    exit 3
fi

if [ "$CLEAN_ONLY" -eq 1 ]; then
    clean_spec "$SPEC"
    exit 0
fi

# ── Everything past this point dispatches, so --spec is mandatory ──────────

if [ -z "$SPEC" ]; then
    echo "error: --spec is required (micro-spec-small | micro-spec-large)" >&2
    print_help
    exit 3
fi

if [ -z "${BASTION_ENGINE_API_KEY:-}" ]; then
    echo "error: BASTION_ENGINE_API_KEY is not set (scripts/.env or environment)" >&2
    exit 1
fi

mkdir -p "$OUT_DIR"

STATE_FILE="planning/$SPEC/sdlc/sdlc-flow-state.json"

# ── Poll one event to a terminal status; echoes "<status> <elapsed_seconds>" ─
#
# Never opens the SSE streaming route -- polls GET /events/{event_id}, the
# simpler contract for a script whose only job is a terminal verdict.

poll_event() {
    local event_id="$1"
    local start_ts
    start_ts="$(date +%s)"
    local last_status=""
    local deadline=$(( start_ts + TIMEOUT_MINUTES * 60 ))

    while true; do
        local now
        now="$(date +%s)"
        if [ "$now" -ge "$deadline" ]; then
            echo "error: timed out after ${TIMEOUT_MINUTES}m while still '$last_status' (event $event_id)" >&2
            echo "timeout $(( now - start_ts ))"
            return 2
        fi

        local response status
        response=$(curl -sf -H "X-API-Key: $BASTION_ENGINE_API_KEY" "$BASTION_ADDR/events/$event_id" 2>/dev/null || true)
        status=$(echo "$response" | python3 -c "import json,sys; print(json.load(sys.stdin).get('status','unknown'))" 2>/dev/null || echo "unknown")

        if [ "$status" != "$last_status" ]; then
            echo "$(date '+%H:%M:%S') status: $status (event $event_id)" >&2
            last_status="$status"
        fi

        case "$status" in
            succeeded)
                echo "succeeded $(( $(date +%s) - start_ts ))"
                return 0
                ;;
            failed|cancelled|budget_halted)
                echo "$status $(( $(date +%s) - start_ts ))"
                return 1
                ;;
            running|suspended)
                : # still live -- keep polling
                ;;
            *)
                : # unknown/transient read -- keep polling until timeout
                ;;
        esac

        sleep "$POLL_INTERVAL"
    done
}

# ── Harvest: copy the shared flow-state file out, keyed on event_id ────────

harvest_run() {
    local event_id="$1"
    local status="$2"
    local elapsed="$3"

    local dest="$OUT_DIR/${SPEC}-${ENGINE}-${PROFILE}-${event_id}.json"
    if [ -f "$STATE_FILE" ]; then
        cp "$STATE_FILE" "$dest"
    else
        echo "warning: no state file at $STATE_FILE to harvest for event $event_id" >&2
        echo "{}" > "$dest"
    fi

    local meta="$OUT_DIR/${SPEC}-${ENGINE}-${PROFILE}-${event_id}.meta.json"
    python3 - "$meta" "$event_id" "$status" "$elapsed" "$SPEC" "$ENGINE" "$PROFILE" <<'PYEOF'
import json, sys
path, event_id, status, elapsed, spec, engine, profile = sys.argv[1:8]
with open(path, "w") as f:
    json.dump({
        "event_id": event_id,
        "status": status,
        "wall_clock_seconds": int(elapsed),
        "spec": spec,
        "engine": engine,
        "profile": profile,
    }, f, indent=2)
    f.write("\n")
PYEOF

    echo "Harvested event $event_id -> $dest (status=$status, ${elapsed}s)"
}

# ── Dispatch one run: trigger, poll, return "<event_id> <status> <elapsed>" ─

dispatch_run() {
    local event_body
    event_body=$(python3 -c "
import json
print(json.dumps({
    'workflow_type': 'SDLC_FLOW',
    'data': {
        'spec_slug': '$SPEC',
        'use_worktree': True,
        'auto_pr': False,
        'profile': '$PROFILE',
    },
}))
")

    echo "Triggering SDLC_FLOW (spec_slug=$SPEC, profile=$PROFILE, engine=$ENGINE) at $BASTION_ADDR ..." >&2

    local trigger event_id
    trigger=$(curl -sf -X POST "$BASTION_ADDR/events/" \
        -H "X-API-Key: $BASTION_ENGINE_API_KEY" \
        -H "Content-Type: application/json" \
        -d "$event_body")

    event_id=$(echo "$trigger" | python3 -c "import json,sys; print(json.load(sys.stdin).get('event_id',''))" 2>/dev/null)

    if [ -z "$event_id" ]; then
        echo "error: POST /events/ did not return an event_id ($trigger)" >&2
        return 1
    fi

    echo "Triggered run_id/event_id: $event_id" >&2

    local poll_out poll_rc status_and_elapsed
    poll_out=$(poll_event "$event_id")
    poll_rc=$?
    status_and_elapsed=$(echo "$poll_out" | tail -n1)

    echo "$event_id $status_and_elapsed"
    return "$poll_rc"
}

# ── Main k-run loop ──────────────────────────────────────────────────────────

RUN_RESULTS=()   # each entry: "event_id status elapsed"
OVERALL_RC=0
RESUME_DONE=0

run_index=1
while [ "$run_index" -le "$K" ]; do
    echo "== run $run_index/$K =="

    clean_spec "$SPEC" >/dev/null 2>&1 || true

    set +e
    DISPATCH_OUT=$(dispatch_run)
    DISPATCH_RC=$?
    set -e

    EVENT_ID=$(echo "$DISPATCH_OUT" | awk '{print $1}')
    RUN_STATUS=$(echo "$DISPATCH_OUT" | awk '{print $2}')
    RUN_ELAPSED=$(echo "$DISPATCH_OUT" | awk '{print $3}')

    if [ -z "$EVENT_ID" ]; then
        echo "error: run $run_index produced no event_id" >&2
        OVERALL_RC=1
        run_index=$((run_index + 1))
        continue
    fi

    RUN_RESULTS+=("$EVENT_ID $RUN_STATUS $RUN_ELAPSED")

    # Resume seam: on the LAST run of the batch, after its first terminal
    # transition, re-attach via a second GET /events/{event_id} poll so
    # resume is exercised on every k-run batch, not only by accident.
    if [ "$run_index" -eq "$K" ] && [ "$RESUME_DONE" -eq 0 ]; then
        echo "Re-attaching to event $EVENT_ID to exercise the resume seam ..." >&2
        poll_event "$EVENT_ID" >/dev/null || true
        RESUME_DONE=1
    fi

    if [ "$DEFER_HARVEST" -eq 0 ]; then
        harvest_run "$EVENT_ID" "$RUN_STATUS" "$RUN_ELAPSED"
    fi

    if [ "$DISPATCH_RC" -eq 2 ]; then
        OVERALL_RC=2
    elif [ "$DISPATCH_RC" -ne 0 ] && [ "$OVERALL_RC" -eq 0 ]; then
        OVERALL_RC=1
    fi

    run_index=$((run_index + 1))
done

# --defer-harvest: harvest only now, at the end -- by which point every run
# in the batch has already overwritten the SAME shared state file, so this
# deliberately produces fewer than k distinct records. That is the point:
# it is the negative control for harvest-before-next.
if [ "$DEFER_HARVEST" -eq 1 ]; then
    for entry in "${RUN_RESULTS[@]}"; do
        entry_event_id=$(echo "$entry" | awk '{print $1}')
        entry_status=$(echo "$entry" | awk '{print $2}')
        entry_elapsed=$(echo "$entry" | awk '{print $3}')
        harvest_run "$entry_event_id" "$entry_status" "$entry_elapsed"
    done
fi

echo "Completed $K run(s) of $SPEC on engine=$ENGINE profile=$PROFILE; overall exit=$OVERALL_RC"
exit "$OVERALL_RC"
