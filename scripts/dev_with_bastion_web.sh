#!/usr/bin/env bash
#
# dev_with_bastion_web.sh
#
# Starts `bastion serve` with its cwd pinned to THIS repo (engine-rs), then
# starts bastion-web's `next dev` pointed at it — the exact pairing needed to
# trigger and watch an SDLC_FLOW run against engine-rs from bastion-web's
# QuickLaunch UI.
#
# Why cwd matters: bastion-web's QuickLaunch never sends the SDLC_FLOW
# event's `repo` field (a deliberate scope decision — see
# bastion-web/components/trigger/quick-launch.tsx's "Divergence 1" comment),
# so SDLC_FLOW's target-root resolution always falls back to bastion serve's
# own working directory (EN.3.K,
# crates/engine-core/src/workflows/sdlc_flow/setup.rs). bastion-web's own
# `scripts/dev-all.sh` starts bastion serve with cwd=core/bastion, which
# would silently target the WRONG repo for a trigger meant for engine-rs.
#
# This script exists to get that one detail right, plus two env-sourcing
# traps that are easy to hit by hand: DATABASE_URL lives in
# core/bastion/.env, NOT core/bastion-web/.env.local, and a server that
# boots without it comes up healthy on /health while quietly leaving every
# engine route unmounted (no error, no crash — just silently absent).
#
# Usage:
#   scripts/dev_with_bastion_web.sh                run preflight checks, then start both,
#                                                   foreground, Ctrl-C stops both
#   scripts/dev_with_bastion_web.sh --check-only   run preflight checks and exit; start nothing
#   scripts/dev_with_bastion_web.sh --rebuild      force a release rebuild of the bastion binary first
#   scripts/dev_with_bastion_web.sh --stop         kill any servers this script started (or left
#                                                   running on its ports), then exit — use this if
#                                                   Ctrl-C didn't get a chance to run (closed
#                                                   terminal, crashed shell, `kill -9` on the script)
#   scripts/dev_with_bastion_web.sh --help
#
# Shutdown: Ctrl-C (or a plain `kill` of the script's own PID) stops both
# servers AND their child processes — `npm run dev` spawns a separate
# `next-server` process that a plain `kill $NPM_PID` would orphan, so
# shutdown walks the process tree (see kill_tree below) rather than killing
# only the top-level PID.
#
# Env sourced (core/bastion/.env, then core/bastion-web/.env.local — the
# second wins on overlap): DATABASE_URL, BASTION_SERVE_TOKEN,
# BASTION_ENGINE_API_KEY, BASTION_SERVE_URL.
#
# Exit codes: 0 clean shutdown · 1 preflight failure · 2 a server failed to
# become healthy (or died) within its timeout.
set -euo pipefail

# ── Paths ────────────────────────────────────────────────────────────────────

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
CORE_DIR="$(cd "$ENGINE_DIR/.." && pwd)"
BASTION_DIR="$CORE_DIR/bastion"
WEB_DIR="$CORE_DIR/bastion-web"
BASTION_ENV="$BASTION_DIR/.env"
WEB_ENV="$WEB_DIR/.env.local"
BASTION_BIN="$BASTION_DIR/target/release/bastion"

RUN_DIR="$ENGINE_DIR/.dev-bastion-web"
BACKEND_LOG="$RUN_DIR/bastion-serve.log"
FRONTEND_LOG="$RUN_DIR/bastion-web.log"
BACKEND_PIDFILE="$RUN_DIR/bastion-serve.pid"
FRONTEND_PIDFILE="$RUN_DIR/bastion-web.pid"

# ── Flags ────────────────────────────────────────────────────────────────────

CHECK_ONLY="false"
REBUILD="false"
STOP_ONLY="false"

usage() {
  sed -n '2,42p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --check-only) CHECK_ONLY="true"; shift ;;
    --rebuild) REBUILD="true"; shift ;;
    --stop) STOP_ONLY="true"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unrecognized argument: $1" >&2; usage; exit 1 ;;
  esac
done

mkdir -p "$RUN_DIR"

# ── Process-tree kill helper ─────────────────────────────────────────────────
#
# `npm run dev` spawns npm, which spawns a separate `next-server` (Turbopack)
# child — killing only npm's PID leaves that child running and the port held.
# Walks children first (post-order) so a parent doesn't vanish out from under
# a `pgrep -P` lookup for its own children. `pgrep`/`pkill` are present on
# macOS by default; no `setsid`/process-group tricks needed.
kill_tree() {
  local pid="$1" sig="${2:-TERM}"
  [ -z "$pid" ] && return 0
  local child
  for child in $(pgrep -P "$pid" 2>/dev/null || true); do
    kill_tree "$child" "$sig"
  done
  kill -"$sig" "$pid" 2>/dev/null || true
}

port_in_use() { lsof -ti ":$1" >/dev/null 2>&1; }

# ── --stop: kill leftovers and exit, no preflight noise ─────────────────────
#
# Two independent strategies, both best-effort (a missing pidfile or a port
# nothing is listening on is not an error): pidfiles from a prior run of
# THIS script, and a port sweep as the guaranteed fallback for when the
# trap never ran at all (terminal closed, shell killed, `kill -9`). Mirrors
# bastion-web's own scripts/dev-stop.sh port-sweep approach.
if [ "$STOP_ONLY" = "true" ]; then
  echo "== Stopping dev_with_bastion_web.sh servers =="
  STOPPED_ANYTHING="false"

  for pidfile_pair in "$BACKEND_PIDFILE:bastion serve" "$FRONTEND_PIDFILE:bastion-web"; do
    pidfile="${pidfile_pair%%:*}"
    label="${pidfile_pair#*:}"
    if [ -f "$pidfile" ]; then
      pid=$(cat "$pidfile" 2>/dev/null || true)
      if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
        echo "Stopping $label (pid $pid, from pidfile)..."
        kill_tree "$pid" TERM
        STOPPED_ANYTHING="true"
      fi
      rm -f "$pidfile"
    fi
  done

  # Best-effort port resolution for the sweep — tolerate missing env files
  # entirely; --stop must work even if .env/.env.local were never set up.
  SWEEP_SERVE_PORT=""
  SWEEP_FRONTEND_PORT="${PORT:-3000}"
  if [ -f "$WEB_ENV" ]; then
    SWEEP_URL=$(grep -E '^BASTION_SERVE_URL=' "$WEB_ENV" 2>/dev/null | cut -d= -f2- | tr -d '"'"'"' \r')
    SWEEP_SERVE_PORT="${SWEEP_URL##*:}"
  fi
  SWEEP_SERVE_PORT="${SWEEP_SERVE_PORT:-4317}"

  for port in "$SWEEP_SERVE_PORT" "$SWEEP_FRONTEND_PORT"; do
    if port_in_use "$port"; then
      echo "Killing lingering process(es) on port $port..."
      for pid in $(lsof -ti ":$port" 2>/dev/null || true); do
        kill_tree "$pid" TERM
      done
      STOPPED_ANYTHING="true"
    fi
  done

  sleep 1
  for port in "$SWEEP_SERVE_PORT" "$SWEEP_FRONTEND_PORT"; do
    if port_in_use "$port"; then
      echo "Port $port still held — force killing..."
      for pid in $(lsof -ti ":$port" 2>/dev/null || true); do
        kill_tree "$pid" KILL
      done
    fi
  done

  if [ "$STOPPED_ANYTHING" = "true" ]; then
    echo "Done."
  else
    echo "Nothing appeared to be running."
  fi
  exit 0
fi

FAILED_CHECKS=()
fail_check() { FAILED_CHECKS+=("$1"); echo "  [FAIL] $1" >&2; }
pass_check() { echo "  [ OK ] $1"; }

# ── Preflight: repo layout sanity ───────────────────────────────────────────

echo "== Preflight: repo layout =="

if ! grep -q '"crates/engine-core"' "$ENGINE_DIR/Cargo.toml" 2>/dev/null; then
  fail_check "$ENGINE_DIR does not look like the engine-rs workspace root (Cargo.toml missing crates/engine-core member) — this script must live in engine-rs/scripts/"
else
  pass_check "engine-rs workspace root resolved: $ENGINE_DIR"
fi

if [ ! -d "$BASTION_DIR" ]; then
  fail_check "sibling repo not found: $BASTION_DIR"
else
  pass_check "bastion repo found: $BASTION_DIR"
fi

if [ ! -d "$WEB_DIR" ]; then
  fail_check "sibling repo not found: $WEB_DIR"
else
  pass_check "bastion-web repo found: $WEB_DIR"
fi

if [ ${#FAILED_CHECKS[@]} -gt 0 ]; then
  echo "Preflight failed — fix the above before continuing." >&2
  exit 1
fi

# ── Preflight: env files present ────────────────────────────────────────────

echo "== Preflight: env files =="

if [ ! -f "$BASTION_ENV" ]; then
  fail_check "$BASTION_ENV not found — this is where DATABASE_URL lives; create it before running this script"
else
  pass_check "found $BASTION_ENV"
fi

if [ ! -f "$WEB_ENV" ]; then
  fail_check "$WEB_ENV not found — copy $WEB_DIR/.env.example to .env.local and fill it in"
else
  pass_check "found $WEB_ENV"
fi

if [ ${#FAILED_CHECKS[@]} -gt 0 ]; then
  echo "Preflight failed — fix the above before continuing." >&2
  exit 1
fi

# core/bastion/.env first, core/bastion-web/.env.local second (wins on overlap —
# matches bastion-web's own scripts/dev-all.sh sourcing order intent, except
# we ALSO need bastion/.env for DATABASE_URL, which dev-all.sh never sources).
set -a
# shellcheck source=/dev/null
source "$BASTION_ENV"
# shellcheck source=/dev/null
source "$WEB_ENV"
set +a

# ── Preflight: required env vars ────────────────────────────────────────────

echo "== Preflight: required env vars =="

check_var() {
  local name="$1"
  local value="${!name:-}"
  if [ -z "$value" ]; then
    fail_check "$name is not set (expected in $BASTION_ENV or $WEB_ENV)"
  else
    pass_check "$name is set"
  fi
}

check_var DATABASE_URL
check_var BASTION_SERVE_TOKEN
check_var BASTION_ENGINE_API_KEY
check_var BASTION_SERVE_URL

if [ ${#FAILED_CHECKS[@]} -gt 0 ]; then
  echo "Preflight failed — fix the above before continuing." >&2
  exit 1
fi

SERVE_ADDR="${BASTION_SERVE_URL#http://}"
SERVE_ADDR="${SERVE_ADDR#https://}"
SERVE_PORT="${SERVE_ADDR##*:}"
FRONTEND_PORT="${PORT:-3000}"

# ── Preflight: Postgres reachable ───────────────────────────────────────────

echo "== Preflight: Postgres reachability =="

if command -v pg_isready >/dev/null 2>&1; then
  if pg_isready -d "$DATABASE_URL" >/dev/null 2>&1; then
    pass_check "Postgres accepting connections (pg_isready)"
  else
    fail_check "pg_isready could not reach DATABASE_URL — start Postgres before running this script"
  fi
else
  echo "  [SKIP] pg_isready not on PATH — cannot pre-verify Postgres; the engine mount check below will catch it anyway"
fi

if [ ${#FAILED_CHECKS[@]} -gt 0 ]; then
  echo "Preflight failed — fix the above before continuing." >&2
  exit 1
fi

# ── Preflight: ports free ───────────────────────────────────────────────────

echo "== Preflight: ports free =="

if port_in_use "$SERVE_PORT"; then
  fail_check "port $SERVE_PORT (bastion serve) is already in use — run scripts/dev_with_bastion_web.sh --stop, or: lsof -ti :$SERVE_PORT | xargs kill"
else
  pass_check "port $SERVE_PORT free"
fi

if port_in_use "$FRONTEND_PORT"; then
  fail_check "port $FRONTEND_PORT (bastion-web) is already in use — run scripts/dev_with_bastion_web.sh --stop, or: lsof -ti :$FRONTEND_PORT | xargs kill"
else
  pass_check "port $FRONTEND_PORT free"
fi

if [ ${#FAILED_CHECKS[@]} -gt 0 ]; then
  echo "Preflight failed — fix the above before continuing." >&2
  exit 1
fi

echo
echo "All preflight checks passed."

if [ "$CHECK_ONLY" = "true" ]; then
  echo "(--check-only: not starting any servers)"
  exit 0
fi

# ── Build bastion binary if missing or --rebuild ────────────────────────────

if [ "$REBUILD" = "true" ] || [ ! -x "$BASTION_BIN" ]; then
  echo
  echo "Building bastion release binary..."
  (cd "$BASTION_DIR" && cargo build --release --bin bastion)
fi

# ── Cleanup ──────────────────────────────────────────────────────────────────

BACKEND_PID=""
FRONTEND_PID=""
TAIL_PID=""

cleanup() {
  trap - INT TERM EXIT
  echo
  echo "Shutting down..."

  [ -n "$TAIL_PID" ] && kill "$TAIL_PID" 2>/dev/null || true

  if [ -n "$FRONTEND_PID" ] && kill -0 "$FRONTEND_PID" 2>/dev/null; then
    echo "Stopping bastion-web (pid $FRONTEND_PID, and its child processes)..."
    kill_tree "$FRONTEND_PID" TERM
  fi
  if [ -n "$BACKEND_PID" ] && kill -0 "$BACKEND_PID" 2>/dev/null; then
    echo "Stopping bastion serve (pid $BACKEND_PID)..."
    kill_tree "$BACKEND_PID" TERM
  fi

  # Grace period, then force-kill anything that ignored TERM (e.g. a
  # Turbopack worker mid-compile) so this never hangs waiting.
  sleep 1
  if [ -n "$FRONTEND_PID" ] && kill -0 "$FRONTEND_PID" 2>/dev/null; then
    kill_tree "$FRONTEND_PID" KILL
  fi
  if [ -n "$BACKEND_PID" ] && kill -0 "$BACKEND_PID" 2>/dev/null; then
    kill_tree "$BACKEND_PID" KILL
  fi

  rm -f "$BACKEND_PIDFILE" "$FRONTEND_PIDFILE"
  wait 2>/dev/null || true
  echo "Stopped."
}
trap cleanup INT TERM EXIT

# ── Start bastion serve — cwd = engine-rs, THIS is the fix ──────────────────

echo
echo "Starting bastion serve — cwd=$ENGINE_DIR, addr=$SERVE_ADDR, log=$BACKEND_LOG"
(
  cd "$ENGINE_DIR"
  exec "$BASTION_BIN" serve --addr "$SERVE_ADDR" --token "$BASTION_SERVE_TOKEN"
) >"$BACKEND_LOG" 2>&1 &
BACKEND_PID=$!
echo "$BACKEND_PID" >"$BACKEND_PIDFILE"

HEALTH_URL="http://$SERVE_ADDR/health"
WORKFLOWS_URL="http://$SERVE_ADDR/workflows"

echo -n "Waiting for /health"
BACKEND_UP="false"
for _ in $(seq 1 30); do
  if curl -sf "$HEALTH_URL" >/dev/null 2>&1; then
    BACKEND_UP="true"
    break
  fi
  echo -n "."
  sleep 1
done
echo

if [ "$BACKEND_UP" != "true" ]; then
  echo "error: bastion serve did not answer /health within 30s — see $BACKEND_LOG" >&2
  tail -n 40 "$BACKEND_LOG" >&2 || true
  exit 2
fi
echo "bastion serve is up (pid $BACKEND_PID)"

# GET /workflows is unauthenticated and 404s specifically when the engine's
# route table isn't mounted (decide_engine_mount in bastion/src/serve/mod.rs) —
# distinct from /health, which answers 200 whether or not the engine mounted.
echo -n "Confirming engine routes are mounted (GET /workflows)"
ENGINE_MOUNTED="false"
for _ in $(seq 1 15); do
  STATUS=$(curl -s -o /dev/null -w "%{http_code}" "$WORKFLOWS_URL" 2>/dev/null || echo "000")
  if [ "$STATUS" = "200" ]; then
    ENGINE_MOUNTED="true"
    break
  fi
  echo -n "."
  sleep 1
done
echo

if [ "$ENGINE_MOUNTED" != "true" ]; then
  echo "error: engine routes are not mounted (GET /workflows did not return 200) — check DATABASE_URL/BASTION_ENGINE_API_KEY; see $BACKEND_LOG" >&2
  grep -i "engine routes" "$BACKEND_LOG" >&2 || tail -n 40 "$BACKEND_LOG" >&2 || true
  exit 2
fi
echo "engine routes mounted."

# ── Start bastion-web ────────────────────────────────────────────────────────

echo
echo "Starting bastion-web — cwd=$WEB_DIR, port=$FRONTEND_PORT, log=$FRONTEND_LOG"
(
  cd "$WEB_DIR"
  exec npm run dev
) >"$FRONTEND_LOG" 2>&1 &
FRONTEND_PID=$!
echo "$FRONTEND_PID" >"$FRONTEND_PIDFILE"

FRONTEND_URL="http://localhost:$FRONTEND_PORT"
echo -n "Waiting for $FRONTEND_URL"
FRONTEND_UP="false"
for _ in $(seq 1 60); do
  if curl -sf "$FRONTEND_URL" >/dev/null 2>&1; then
    FRONTEND_UP="true"
    break
  fi
  echo -n "."
  sleep 1
done
echo

if [ "$FRONTEND_UP" != "true" ]; then
  echo "error: bastion-web did not answer on $FRONTEND_URL within 60s — see $FRONTEND_LOG" >&2
  tail -n 40 "$FRONTEND_LOG" >&2 || true
  exit 2
fi
echo "bastion-web is up (pid $FRONTEND_PID)"

# ── Ready ────────────────────────────────────────────────────────────────────

cat <<EOF

──────────────────────────────────────────────────────────────────────────
Ready.

  bastion serve : $HEALTH_URL  (cwd=$ENGINE_DIR, so SDLC_FLOW's cwd-fallback
                  targeting resolves to THIS repo — QuickLaunch never sends
                  a "repo" field)
  bastion-web   : $FRONTEND_URL

QuickLaunch settings for a worktree-based trial run:
  - Use worktree : On (true)
  - Auto PR      : Off (false)
  - Profile      : leave blank (uses this repo's harness.json sonnet/fast defaults)

Watch a triggered run from another terminal:
  cd $ENGINE_DIR
  BASTION_SERVE_ADDR=$BASTION_SERVE_URL ./scripts/sdlc_smoke.sh --watch <run_id>

Logs:
  backend  $BACKEND_LOG
  frontend $FRONTEND_LOG

Stop:
  Ctrl-C here stops both cleanly (backend, frontend, and its next-server
  child). If this terminal gets closed instead, run from another shell:
    $ENGINE_DIR/scripts/dev_with_bastion_web.sh --stop
──────────────────────────────────────────────────────────────────────────

EOF

tail -f "$BACKEND_LOG" "$FRONTEND_LOG" &
TAIL_PID=$!

while kill -0 "$BACKEND_PID" 2>/dev/null && kill -0 "$FRONTEND_PID" 2>/dev/null; do
  sleep 1
done

echo
echo "One of the servers exited on its own — see logs above."
exit 2
