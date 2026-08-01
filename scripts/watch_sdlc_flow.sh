#!/usr/bin/env bash
#
# watch_sdlc_flow.sh — attach to an already-triggered SDLC_FLOW run and watch
# it to a terminal state, showing per-node progress (not just the coarse
# overall status).
#
# This is the watch half of core/scripts/run-sdlc-flow.sh (per-node polling
# via bastion's bearer-token `/api/runs/{id}`, the on-disk state file,
# macOS notifications on transitions) combined with sdlc_smoke.sh's
# X-API-Key `/events/{id}` readback and scriptable exit codes — built for
# the case where triggering happens elsewhere (bastion-web's QuickLaunch)
# and this script's only job is watching. Unlike run-sdlc-flow.sh, it does
# NOT need `--repo`/spec_slug arguments — it resolves the spec_slug and
# state-file path itself from the triggered event's own task_context.
#
# Usage:
#   scripts/watch_sdlc_flow.sh <run_id> [--poll-interval N] [--timeout-minutes N]
#   scripts/watch_sdlc_flow.sh --help
#
# Env sourced (core/bastion/.env, then scripts/.env — the second wins on
# overlap): BASTION_SERVE_TOKEN (bearer, for bastion's /api/runs/{id}
# per-node view), BASTION_ENGINE_API_KEY (X-API-Key, for the engine's
# /events/{id} readback). Only ONE of the two is strictly required — with
# just BASTION_ENGINE_API_KEY you get overall status + the on-disk state
# file; with just BASTION_SERVE_TOKEN you get the per-node table.
#   BASTION_SERVE_ADDR   default: http://localhost:4317
#
# Terminal statuses: succeeded | failed | cancelled | budget_halted
# Live statuses:      running | suspended
#
# Exit codes: 0 succeeded · 1 failed/cancelled/budget_halted · 2 timed out
# while still live · 3 usage error.
#
# KNOWN RESIDUAL (deliberately out of scope — do not "fix" piecemeal):
# the on-disk state file is resolved under the engine-rs checkout this script
# lives in (ENGINE_DIR="$SCRIPT_DIR/..") plus the event's spec_slug. That path
# ignores both the event's own `task_context.event.repo` and the
# `trees/<branch>/` worktree location SaveStateNode actually writes under, so
# for `--repo` runs and for any run executing in a worktree the `[state]` line
# is simply absent (the HTTP status polling below is unaffected and remains
# authoritative). Resolving the real per-run state path is a separate, larger
# change. What this script DOES guarantee is that it never attributes another
# run's state file to the watched run — see the run_id guard in the poll loop.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BASTION_ENV="$ENGINE_DIR/../bastion/.env"
LOCAL_ENV="$SCRIPT_DIR/.env"

usage() {
  sed -n '2,29p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

if [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ]; then
  usage
  exit 0
fi

if [ $# -eq 0 ]; then
  usage
  exit 3
fi

RUN_ID="$1"
shift

POLL_INTERVAL=3
TIMEOUT_MINUTES=20

while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --poll-interval) POLL_INTERVAL="$2"; shift 2 ;;
    --timeout-minutes) TIMEOUT_MINUTES="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unrecognized argument: $1" >&2; usage; exit 3 ;;
  esac
done

set -a
[ -f "$BASTION_ENV" ] && source "$BASTION_ENV"
[ -f "$LOCAL_ENV" ] && source "$LOCAL_ENV"
set +a

BASTION_ADDR="${BASTION_SERVE_ADDR:-http://localhost:4317}"

if [ -z "${BASTION_SERVE_TOKEN:-}" ] && [ -z "${BASTION_ENGINE_API_KEY:-}" ]; then
  echo "error: neither BASTION_SERVE_TOKEN nor BASTION_ENGINE_API_KEY is set (expected in $BASTION_ENV or $LOCAL_ENV)" >&2
  exit 3
fi

# ── macOS desktop notifications (best-effort, silent no-op elsewhere) ──────

notify_mac() {
  local title="$1" message="$2" sound="${3:-Glass}"
  [ "$(uname)" = "Darwin" ] || return 0
  local esc_title="${title//\"/\\\"}" esc_message="${message//\"/\\\"}"
  osascript -e "display notification \"$esc_message\" with title \"$esc_title\" sound name \"$sound\"" >/dev/null 2>&1 || true
}

# Pure string logic, no I/O — same contract as run-sdlc-flow.sh's helpers.
detect_failed_node_transition() {
  local prev="$1" curr="$2" tok name status prev_status ptok
  for tok in $curr; do
    name="${tok%%=*}"; status="${tok#*=}"
    [ "$status" = "failed" ] || continue
    prev_status=""
    for ptok in $prev; do
      if [ "${ptok%%=*}" = "$name" ]; then prev_status="${ptok#*=}"; break; fi
    done
    if [ "$prev_status" != "failed" ]; then echo "$name"; return 0; fi
  done
  return 0
}

# ── One-time: resolve spec_slug from the event, to locate the state file ───

EVENT_URL="$BASTION_ADDR/events/$RUN_ID"
SPEC_SLUG=""
STATE_FILE=""

if [ -n "${BASTION_ENGINE_API_KEY:-}" ]; then
  SPEC_SLUG=$(curl -sf -H "X-API-Key: $BASTION_ENGINE_API_KEY" "$EVENT_URL" 2>/dev/null \
    | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('task_context',{}).get('event',{}).get('spec_slug',''))" 2>/dev/null || true)
fi

if [ -n "$SPEC_SLUG" ]; then
  STATE_FILE="$ENGINE_DIR/planning/$SPEC_SLUG/sdlc/sdlc-flow-state.json"
  echo "Watching run $RUN_ID (spec_slug=$SPEC_SLUG) at $BASTION_ADDR"
else
  echo "Watching run $RUN_ID at $BASTION_ADDR (spec_slug not resolved yet — no X-API-Key, or event not found; will keep trying)"
fi

# ── Poll loop ────────────────────────────────────────────────────────────────

RUNS_URL="$BASTION_ADDR/api/runs/$RUN_ID"
LAST_NODE_SUMMARY=""
LAST_TASK_SUMMARY=""
LAST_STATUS=""
DEADLINE=$(( $(date +%s) + TIMEOUT_MINUTES * 60 ))

while true; do
  NOW=$(date +%s)
  if [ "$NOW" -ge "$DEADLINE" ]; then
    echo "error: timed out after ${TIMEOUT_MINUTES}m while still '$LAST_STATUS'" >&2
    exit 2
  fi

  # Per-node table (bearer token) — the richer view; skipped silently if no token.
  if [ -n "${BASTION_SERVE_TOKEN:-}" ]; then
    SNAPSHOT=$(curl -sf -H "Authorization: Bearer $BASTION_SERVE_TOKEN" "$RUNS_URL" 2>/dev/null || true)
    if [ -n "$SNAPSHOT" ]; then
      SUMMARY=$(echo "$SNAPSHOT" | python3 -c "
import json,sys
try:
    d=json.load(sys.stdin)
    print(' '.join(f\"{n['node']}={n['status']}\" for n in d.get('nodes', [])))
except Exception:
    pass
" 2>/dev/null || true)
      if [ -n "$SUMMARY" ] && [ "$SUMMARY" != "$LAST_NODE_SUMMARY" ]; then
        echo "$(date '+%H:%M:%S') [nodes] $SUMMARY"
        FAILED_NODE=$(detect_failed_node_transition "$LAST_NODE_SUMMARY" "$SUMMARY")
        [ -n "$FAILED_NODE" ] && notify_mac "SDLC Flow: node failed" "$RUN_ID: $FAILED_NODE failed" "Basso"
        LAST_NODE_SUMMARY="$SUMMARY"
      fi
    fi
  fi

  # On-disk task-loop state — appears once the first task attempt lands.
  if [ -n "$STATE_FILE" ] && [ -f "$STATE_FILE" ]; then
    # The path is spec_slug-scoped, NOT run-scoped, so a second run of the same
    # spec (or a stale corpse from a previous one) lands on the exact same file.
    # EN.6.J stamps run_id into it precisely so a reader can tell: if it does not
    # match the run we were asked to watch, say so loudly and display nothing —
    # attributing another run's task state to this one is worse than silence.
    # Repeated polls collapse to a single printed warning via LAST_TASK_SUMMARY.
    TASK_SUMMARY=$(python3 -c "
import json
try:
    d=json.load(open('$STATE_FILE'))
    file_run_id=d.get('run_id')
    watching='$RUN_ID'
    if not file_run_id:
        print(f\"STALE (state file carries no run_id — pre-EN.6.J or JS-engine write; \"
              f\"cannot attribute it to {watching}) — skipping state display\")
    elif file_run_id != watching:
        print(f\"STALE (run_id mismatch: file={file_run_id} watching={watching}) \"
              f\"— skipping state display\")
    else:
        tasks=d.get('tasks', {})
        print(f\"status={d.get('status')} current_task={d.get('current_task')} tasks=[\" +
              ','.join(f\"{k}:{v.get('status')}\" for k,v in tasks.items()) + ']')
except Exception:
    pass
" 2>/dev/null || true)
    if [ -n "$TASK_SUMMARY" ] && [ "$TASK_SUMMARY" != "$LAST_TASK_SUMMARY" ]; then
      echo "$(date '+%H:%M:%S') [state] $TASK_SUMMARY"
      LAST_TASK_SUMMARY="$TASK_SUMMARY"
    fi
  fi

  # Overall status + spec_slug resolution retry (X-API-Key).
  if [ -n "${BASTION_ENGINE_API_KEY:-}" ]; then
    EVENT_BODY=$(curl -sf -H "X-API-Key: $BASTION_ENGINE_API_KEY" "$EVENT_URL" 2>/dev/null || true)
    if [ -n "$EVENT_BODY" ]; then
      if [ -z "$SPEC_SLUG" ]; then
        SPEC_SLUG=$(echo "$EVENT_BODY" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('task_context',{}).get('event',{}).get('spec_slug',''))" 2>/dev/null || true)
        if [ -n "$SPEC_SLUG" ]; then
          STATE_FILE="$ENGINE_DIR/planning/$SPEC_SLUG/sdlc/sdlc-flow-state.json"
          echo "resolved spec_slug=$SPEC_SLUG"
        fi
      fi

      STATUS=$(echo "$EVENT_BODY" | python3 -c "import json,sys; print(json.load(sys.stdin).get('status','unknown'))" 2>/dev/null || echo "unknown")
      if [ "$STATUS" != "$LAST_STATUS" ]; then
        echo "$(date '+%H:%M:%S') status: $STATUS"
        LAST_STATUS="$STATUS"
      fi

      case "$STATUS" in
        succeeded)
          echo "SDLC_FLOW run $RUN_ID succeeded."
          exit 0
          ;;
        failed|cancelled|budget_halted)
          notify_mac "SDLC Flow: $STATUS" "$RUN_ID ended $STATUS" "Basso"
          echo "SDLC_FLOW run $RUN_ID ended in terminal state: $STATUS" >&2
          # Surface the failing node's own error message directly — no
          # separate manual curl+grep needed to find e.g. "claude call
          # timed out".
          echo "$EVENT_BODY" | python3 -c "
import json,sys
try:
    d=json.load(sys.stdin)
    for name, run in d.get('task_context', {}).get('node_runs', {}).items():
        if run.get('status') == 'failed':
            print(f\"  {name}: {run.get('error')}\", file=sys.stderr)
except Exception:
    pass
" || true
          exit 1
          ;;
      esac
    fi
  fi

  sleep "$POLL_INTERVAL"
done
