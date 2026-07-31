#!/usr/bin/env bash

# sdlc_smoke.sh
#
# Trigger a minimal SDLC_FLOW run against a live `bastion serve` instance and
# watch it to a terminal state — the smoke-test harness for EN.3.J: proving
# the Rust SDLC_FLOW works end to end (trigger -> dispatch -> real worktree
# write -> terminal state on disk) before .claude/workflows/sdlc-flow.js is
# retired. It never opens the SSE streaming route — it polls the plain event
# readback endpoint instead.
#
# Usage:
#   scripts/sdlc_smoke.sh              trigger a fresh SDLC_FLOW run and watch it
#   scripts/sdlc_smoke.sh --watch ID   attach to an existing run_id/event_id and watch it
#   scripts/sdlc_smoke.sh --clean      cleanup only (see task 3) — no trigger, no watch
#   scripts/sdlc_smoke.sh --help       show this help
#
# Secrets: sourced from scripts/.env if present (gitignored), else read from
# the environment. BASTION_SERVE_ADDR defaults to http://localhost:4317.
# BASTION_ENGINE_API_KEY has no default — required for the trigger/watch path.
#
# Exit codes:
#   0   run reached `succeeded`
#   1   run reached `failed`, `cancelled`, or `budget_halted`, or a hard error
#   2   timed out while still `running`/`suspended`
#   3   usage error (bad flags)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# ── Config ───────────────────────────────────────────────────────────────────

ENV_FILE="$SCRIPT_DIR/.env"
if [ -f "$ENV_FILE" ]; then
    # shellcheck source=/dev/null
    source "$ENV_FILE"
fi

BASTION_ADDR="${BASTION_SERVE_ADDR:-http://localhost:4317}"
POLL_INTERVAL="${SDLC_SMOKE_POLL_INTERVAL:-3}"
TIMEOUT_MINUTES="${SDLC_SMOKE_TIMEOUT_MINUTES:-20}"

# ── Flags ────────────────────────────────────────────────────────────────────

MODE="trigger"
WATCH_ID=""

print_help() {
    cat <<'EOF'
sdlc_smoke.sh — trigger and watch an SDLC_FLOW smoke run

Usage:
  sdlc_smoke.sh              Trigger a fresh SDLC_FLOW run (spec_slug
                              "smoke-sdlc-flow", use_worktree, no auto_pr,
                              profile cheap-fast) and poll it to a terminal
                              state.
  sdlc_smoke.sh --watch ID    Skip the trigger; attach to and poll an
                              existing run_id/event_id.
  sdlc_smoke.sh --clean       Cleanup only — no trigger, no watch.
  sdlc_smoke.sh --help        Show this help and exit.

Environment (scripts/.env, gitignored, or already-exported):
  BASTION_SERVE_ADDR        Base URL of bastion serve (default:
                             http://localhost:4317)
  BASTION_ENGINE_API_KEY    X-API-Key for the engine routes (required for
                             trigger/watch)
  SDLC_SMOKE_POLL_INTERVAL  Seconds between polls (default: 3)
  SDLC_SMOKE_TIMEOUT_MINUTES  Minutes before giving up on a live run
                             (default: 20)

Terminal statuses: succeeded | failed | cancelled | budget_halted
Live statuses:      running | suspended
(There is no "completed" — see derive_terminal_status in
crates/engine-serve/src/http.rs.)

This script never opens the SSE streaming endpoint — it polls
GET /events/{event_id} instead, which is the simpler contract for a script
whose only job is a terminal verdict.
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --watch)
            MODE="watch"
            WATCH_ID="${2:-}"
            if [ -z "$WATCH_ID" ]; then
                echo "error: --watch requires an event_id argument" >&2
                exit 3
            fi
            shift 2
            ;;
        --clean)
            MODE="clean"
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

# ── Cleanup-only mode (task 3 owns the actual cleanup logic) ────────────────

if [ "$MODE" = "clean" ]; then
    echo "Cleanup is implemented by task 3 of EN.3.J-sdlc-flow-smoke." >&2
    exit 0
fi

# ── Require the API key for trigger/watch ───────────────────────────────────

if [ -z "${BASTION_ENGINE_API_KEY:-}" ]; then
    echo "error: BASTION_ENGINE_API_KEY is not set (scripts/.env or environment)" >&2
    exit 1
fi

# ── Trigger ──────────────────────────────────────────────────────────────────

EVENT_ID=""

if [ "$MODE" = "trigger" ]; then
    echo "Triggering SDLC_FLOW (spec_slug=smoke-sdlc-flow, use_worktree=true, auto_pr=false, profile=cheap-fast) at $BASTION_ADDR ..."

    TRIGGER=$(curl -sf -X POST "$BASTION_ADDR/events/" \
        -H "X-API-Key: $BASTION_ENGINE_API_KEY" \
        -H "Content-Type: application/json" \
        -d '{"workflow_type":"SDLC_FLOW","data":{"spec_slug":"smoke-sdlc-flow","use_worktree":true,"auto_pr":false,"profile":"cheap-fast"}}')

    EVENT_ID=$(echo "$TRIGGER" | python3 -c "import json,sys; print(json.load(sys.stdin).get('event_id',''))" 2>/dev/null)

    if [ -z "$EVENT_ID" ]; then
        echo "error: POST /events/ did not return an event_id ($TRIGGER)" >&2
        exit 1
    fi

    echo "Triggered run_id/event_id: $EVENT_ID"
else
    EVENT_ID="$WATCH_ID"
    echo "Attaching to existing run $EVENT_ID at $BASTION_ADDR ..."
fi

# ── Watch: poll GET /events/{event_id}, print transitions, exit on terminal ─

LAST_STATUS=""
DEADLINE=$(( $(date +%s) + TIMEOUT_MINUTES * 60 ))

while true; do
    NOW=$(date +%s)
    if [ "$NOW" -ge "$DEADLINE" ]; then
        echo "error: timed out after ${TIMEOUT_MINUTES}m while still '$LAST_STATUS'" >&2
        exit 2
    fi

    RESPONSE=$(curl -sf -H "X-API-Key: $BASTION_ENGINE_API_KEY" "$BASTION_ADDR/events/$EVENT_ID" 2>/dev/null || true)
    STATUS=$(echo "$RESPONSE" | python3 -c "import json,sys; print(json.load(sys.stdin).get('status','unknown'))" 2>/dev/null || echo "unknown")

    if [ "$STATUS" != "$LAST_STATUS" ]; then
        echo "$(date '+%H:%M:%S') status: $STATUS"
        LAST_STATUS="$STATUS"
    fi

    case "$STATUS" in
        succeeded)
            echo "SDLC_FLOW run $EVENT_ID succeeded."
            exit 0
            ;;
        failed|cancelled|budget_halted)
            echo "SDLC_FLOW run $EVENT_ID ended in terminal state: $STATUS" >&2
            exit 1
            ;;
        running|suspended)
            : # still live — keep polling
            ;;
        *)
            : # unknown/transient read — keep polling until timeout
            ;;
    esac

    sleep "$POLL_INTERVAL"
done
