#!/usr/bin/env bash
#
# bench_local_models.sh
#
# Dispatch the SAME real ORCHESTRATION-driven SDLC_FLOW task set, once per
# local Ollama model (and per repeat), and harvest structured pass/fail data
# for each -- so the comparison is of the MODEL, not of varying tasks. Pure
# bash + curl + python3: no Claude agent drives this, so running it costs
# nothing beyond local compute.
#
# THREE FIXED DIFFICULTY TIERS (planning/local-model-bench/tiers/<tier>/),
# never combined into one run: easy (a floor check -- capable models should
# pass reliably), medium (real logic, no compilation), hard (genuinely
# difficult -- only strong local or frontier models should reliably pass).
# Pick one tier per invocation with --tier; run the script three times (once
# per tier) for a full sweep. This is deliberate, not a limitation: a single
# run's pass/fail must be attributable to ONE difficulty level.
#
# EVERY (model, rep) JOB GETS ITS OWN THROWAWAY SPEC DIRECTORY AND WORKTREE
# (planning/bench-<tier>-<model-slug>-r<rep>/, trees/sdlc/<same>). This is
# what makes parallel dispatch safe: SetupWorktreeNode keys a worktree and
# its shared sdlc-flow-state.json by block_id/spec_slug, so two concurrent
# dispatches sharing one block_id would race on the same state file. Giving
# every job a unique block_id sidesteps that entirely -- confirmed safe to
# dispatch WITHOUT a planning/state.json block registration (ORCHESTRATION
# only needs planning/<block_id>/tasks.json to exist on disk; verified
# 2026-09-14 with an unregistered probe block that dispatched and completed
# normally).
#
# LIGHT MODELS RUN IN PARALLEL, HEAVY MODELS RUN ONE AT A TIME. A model is
# "light" when its `ollama list` SIZE is <= --light-max-gb (default 6GB) --
# small/fast enough that running several at once is unlikely to starve the
# machine. Anything larger runs strictly sequentially, and the whole light
# pool is drained before any heavy model starts, so a heavy model never
# competes with a light one for RAM.
#
# WHY child_sdlc_flow_policy MUST NEST UNDER data.policy: OrchestrationEventSchema
# has no top-level child_sdlc_flow_policy field -- only `policy:
# Option<PartialOrchestrationPolicy>`, which carries `child_sdlc_flow_policy:
# Option<Option<serde_json::Value>>`. A payload with it at the top level (a
# sibling of `blocks`/`lane`) is silently accepted by serde (no
# deny_unknown_fields) and just as silently ignored, falling back to
# whatever real-Claude defaults the brain_root's own planning/harness.json
# configures. Measured 2026-09-13: a misplaced field here cost $1.38 in real
# Sonnet/Opus sessions on what was meant to be a free local-model smoke test.
#
# Usage:
#   bench_local_models.sh --tier <easy|medium|hard> --models <m1,m2,...> [options]
#   bench_local_models.sh --help
#
# Required:
#   --tier <easy|medium|hard>   Which canonical fixture under
#                               planning/local-model-bench/tiers/<tier>/ to
#                               run. Never mixed in one invocation.
#   --models <list>             Comma-separated Ollama model names, e.g.
#                               "qwen2.5:3b,qwen2.5-coder:7b,qwen2.5-coder:32b".
#                               Each must already be pulled (`ollama list`) --
#                               this script checks and records, never pulls
#                               one for you.
#
# Options:
#   --endpoint <url>       Ollama OpenAI-compatible base URL (default:
#                         http://localhost:11434).
#   --agent-backend <pi|aider>
#                         Which local ImplementTaskNode transport to use
#                         (default: pi).
#   --roadmap <slug>      An existing planning/roadmaps/<slug>/ directory
#                         (default: coordination-layer-port) -- ORCHESTRATION
#                         writes its lane-log/bails/escalations under this
#                         roadmap's directory regardless of block content.
#   --out <dir>           Directory harvested per-run JSON records and the
#                         tier's summary land in (default:
#                         planning/local-model-bench/results/<tier>).
#   --repeat <N>          Run each model N times (default: 1) -- separates
#                         flake from a model's genuine ceiling. Each rep gets
#                         its own throwaway spec dir, so reps of the SAME
#                         model are also safe to run in the parallel pool.
#   --light-max-gb <N>    A model at or under this many GB (from `ollama
#                         list`'s SIZE column) is dispatched in the parallel
#                         pool; anything larger runs alone (default: 6).
#   --max-parallel <N>    Concurrency cap for the light-model pool (default: 3).
#   --keep-work-dirs      Do not delete each job's throwaway
#                         planning/bench-.../ directory and worktree after
#                         harvesting (default: cleaned up immediately).
#   --help                 Show this help and exit.
#
# Environment (scripts/.env, gitignored, or already-exported):
#   BASTION_SERVE_ADDR                 Base URL of bastion serve (default:
#                                       http://localhost:4317)
#   BASTION_ENGINE_API_KEY             X-API-Key for the engine routes
#                                       (required)
#   BENCH_LOCAL_MODELS_POLL_INTERVAL   Seconds between polls (default: 5)
#   BENCH_LOCAL_MODELS_TIMEOUT_MINUTES Minutes before giving up on one live
#                                       run (default: 15)
#
# Per-run output: <out>/<model-slug>-r<rep>-<run-id>.json --
#   {model, tier, rep, run_id, status, wall_clock_seconds, chain_report,
#    bail_reason, tasks: [{task_id, title, status, attempt_count}],
#    backend_used, model_tier_used, total_cost_usd, total_attempts,
#    review_verdicts}
#
# Plus <out>/summary.md (human table) and <out>/summary.json (machine list),
# REGENERATED (not appended) from every *.json record present in <out> at
# the end of this run -- so re-running with different models/repeats
# accumulates one growing comparison for that tier, not a fresh one each
# time.
#
# Exit codes:
#   0   every job reached a terminal status (a fixture task genuinely
#       failing under a weak model is a RESULT, not a script error)
#   1   a run timed out, a dispatch failed, or a model was never pulled
#   3   usage error

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BRAIN_ROOT="$(cd "$REPO_DIR/../.." && pwd)"
cd "$REPO_DIR"

ENV_FILE="$SCRIPT_DIR/.env"
if [ -f "$ENV_FILE" ]; then
    # shellcheck source=/dev/null
    source "$ENV_FILE"
fi

BASTION_ADDR="${BASTION_SERVE_ADDR:-http://localhost:4317}"
POLL_INTERVAL="${BENCH_LOCAL_MODELS_POLL_INTERVAL:-5}"
TIMEOUT_MINUTES="${BENCH_LOCAL_MODELS_TIMEOUT_MINUTES:-15}"

TIER=""
MODELS=""
ENDPOINT="http://localhost:11434"
AGENT_BACKEND="pi"
ROADMAP="coordination-layer-port"
OUT_DIR=""
REPEAT=1
LIGHT_MAX_GB=6
MAX_PARALLEL=3
KEEP_WORK_DIRS=0

print_help() {
    sed -n '2,110p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

while [ $# -gt 0 ]; do
    case "$1" in
        --tier) TIER="${2:-}"; shift 2 ;;
        --models) MODELS="${2:-}"; shift 2 ;;
        --endpoint) ENDPOINT="${2:-}"; shift 2 ;;
        --agent-backend) AGENT_BACKEND="${2:-}"; shift 2 ;;
        --roadmap) ROADMAP="${2:-}"; shift 2 ;;
        --out) OUT_DIR="${2:-}"; shift 2 ;;
        --repeat) REPEAT="${2:-}"; shift 2 ;;
        --light-max-gb) LIGHT_MAX_GB="${2:-}"; shift 2 ;;
        --max-parallel) MAX_PARALLEL="${2:-}"; shift 2 ;;
        --keep-work-dirs) KEEP_WORK_DIRS=1; shift ;;
        --help|-h) print_help; exit 0 ;;
        *) echo "error: unrecognized argument: $1" >&2; print_help; exit 3 ;;
    esac
done

case "$AGENT_BACKEND" in
    pi|aider) : ;;
    *) echo "error: --agent-backend must be pi or aider, got '$AGENT_BACKEND'" >&2; exit 3 ;;
esac

case "$TIER" in
    easy|medium|hard) : ;;
    *) echo "error: --tier must be easy, medium, or hard, got '$TIER'" >&2; exit 3 ;;
esac

case "$REPEAT" in
    ''|*[!0-9]*|0) echo "error: --repeat must be a positive integer, got '$REPEAT'" >&2; exit 3 ;;
esac

case "$MAX_PARALLEL" in
    ''|*[!0-9]*|0) echo "error: --max-parallel must be a positive integer, got '$MAX_PARALLEL'" >&2; exit 3 ;;
esac

if [ -z "$MODELS" ]; then
    echo "error: --models is required (comma-separated)" >&2
    print_help
    exit 3
fi

if [ -z "${BASTION_ENGINE_API_KEY:-}" ]; then
    echo "error: BASTION_ENGINE_API_KEY is not set (scripts/.env or environment)" >&2
    exit 1
fi

CANONICAL_DIR="planning/local-model-bench/tiers/$TIER"
if [ ! -f "$CANONICAL_DIR/tasks.json" ]; then
    echo "error: $CANONICAL_DIR/tasks.json not found" >&2
    exit 3
fi

if [ -z "$OUT_DIR" ]; then
    OUT_DIR="planning/local-model-bench/results/$TIER"
fi
mkdir -p "$OUT_DIR"

# ── Model classification: record, never pull ────────────────────────────────

model_size_gb() {
    local model="$1"
    curl -sf "$ENDPOINT/api/tags" 2>/dev/null | python3 -c "
import json, sys
model = sys.argv[1]
try:
    tags = json.load(sys.stdin)
except Exception:
    sys.exit(1)
for m in tags.get('models', []):
    if m.get('name') == model:
        print(m.get('size', 0) / (1024**3))
        sys.exit(0)
sys.exit(1)
" "$model"
}

# ── Poll one run to a terminal status; echoes "<status> <elapsed_seconds>" ──

poll_run() {
    local run_id="$1"
    local start_ts last_status deadline
    start_ts="$(date +%s)"
    last_status=""
    deadline=$(( start_ts + TIMEOUT_MINUTES * 60 ))

    while true; do
        local now response status
        now="$(date +%s)"
        if [ "$now" -ge "$deadline" ]; then
            echo "error: timed out after ${TIMEOUT_MINUTES}m while still '$last_status' (run $run_id)" >&2
            echo "timeout $(( now - start_ts ))"
            return 2
        fi

        response=$(curl -sf -H "X-API-Key: $BASTION_ENGINE_API_KEY" "$BASTION_ADDR/events/$run_id" 2>/dev/null || true)
        status=$(echo "$response" | python3 -c "import json,sys; print(json.load(sys.stdin).get('status','unknown'))" 2>/dev/null || echo "unknown")

        if [ "$status" != "$last_status" ]; then
            echo "$(date '+%H:%M:%S') [$run_id] status: $status" >&2
            last_status="$status"
        fi

        case "$status" in
            succeeded|failed|cancelled|budget_halted)
                echo "$status $(( $(date +%s) - start_ts ))"
                return 0
                ;;
            running|suspended) : ;;
            *) : ;;
        esac

        sleep "$POLL_INTERVAL"
    done
}

# ── Harvest: combine the child SDLC_FLOW state + the ORCHESTRATION chain
# report into one structured per-run record. `tasks` in sdlc-flow-state.json
# is a JSON OBJECT keyed by string task_id ({"1": {...}, "2": {...}}), not an
# array -- a real trap hit building this script (2026-09-14): iterating it
# as a list silently produced an empty per-task summary. Also: a bailed
# task's status stays "pending" (TriageRouterNode's MAJOR_BAIL arm bypasses
# UpdateTaskStatusNode, the only writer of "done"/"failed"), so pass/fail
# per task is read from attempt_count reaching max_attempts, not from status
# alone. ──────────────────────────────────────────────────────────────────

harvest_run() {
    local model="$1" tier="$2" rep="$3" run_id="$4" status="$5" elapsed="$6" state_file="$7"
    local slug dest orch_record_file
    slug=$(echo "$model" | tr -c 'A-Za-z0-9._-' '-')
    dest="$OUT_DIR/${slug}-r${rep}-${run_id}.json"

    orch_record_file=$(mktemp)
    curl -sf -H "X-API-Key: $BASTION_ENGINE_API_KEY" "$BASTION_ADDR/events/$run_id" 2>/dev/null > "$orch_record_file" || echo '{}' > "$orch_record_file"

    python3 - "$dest" "$model" "$tier" "$rep" "$run_id" "$status" "$elapsed" "$state_file" "$orch_record_file" <<'PYEOF'
import json, sys, os

dest, model, tier, rep, run_id, status, elapsed, state_file, orch_record_file = sys.argv[1:10]
with open(orch_record_file) as f:
    orch_record = json.load(f)

chain_report = (
    orch_record.get("task_context", {})
    .get("nodes", {})
    .get("OrchestrationRunNode", {})
    .get("chain_report", {})
)

flow_state = {}
if os.path.isfile(state_file):
    try:
        with open(state_file) as f:
            flow_state = json.load(f)
    except Exception:
        flow_state = {}

outcomes = flow_state.get("outcomes", {}) or {}
raw_tasks = flow_state.get("tasks", {})
task_summaries = []
if isinstance(raw_tasks, dict):
    for _, t in sorted(raw_tasks.items(), key=lambda kv: int(kv[0]) if kv[0].isdigit() else kv[0]):
        if not isinstance(t, dict):
            continue
        status_v = t.get("status")
        attempt_count = t.get("attempt_count")
        max_attempts = t.get("max_attempts")
        passed = status_v == "done"
        exhausted = (
            status_v != "done"
            and isinstance(attempt_count, int)
            and isinstance(max_attempts, int)
            and attempt_count >= max_attempts
        )
        task_summaries.append({
            "task_id": t.get("task_id"),
            "title": t.get("title"),
            "status": status_v,
            "attempt_count": attempt_count,
            "max_attempts": max_attempts,
            "passed": passed,
            "attempts_exhausted": exhausted,
        })
elif isinstance(raw_tasks, list):
    for t in raw_tasks:
        if isinstance(t, dict):
            task_summaries.append({
                "task_id": t.get("task_id"),
                "title": t.get("title"),
                "status": t.get("status"),
                "attempt_count": t.get("attempt_count"),
            })

record = {
    "model": model,
    "tier": tier,
    "rep": int(rep),
    "run_id": run_id,
    "status": status,
    "wall_clock_seconds": int(elapsed),
    "chain_report": chain_report,
    "bail_reason": flow_state.get("bail_reason"),
    "tasks": task_summaries,
    "backend_used": outcomes.get("backend_used"),
    "model_tier_used": outcomes.get("model_tier_used"),
    "total_cost_usd": outcomes.get("total_cost_usd"),
    "total_attempts": outcomes.get("total_attempts"),
    "review_verdicts": outcomes.get("review_verdicts"),
}

with open(dest, "w") as f:
    json.dump(record, f, indent=2)
    f.write("\n")

print(f"Harvested run {run_id} ({model}, tier={tier}, rep={rep}) -> {dest} (status={status}, {elapsed}s)")
PYEOF
    rm -f "$orch_record_file"
}

# ── One (model, rep) job: unique throwaway spec dir + worktree, dispatch,
# poll, harvest, clean up ───────────────────────────────────────────────────

run_one() {
    local model="$1" rep="$2"
    local slug work_id lane event_body trigger run_id state_file

    slug=$(echo "$model" | tr -c 'A-Za-z0-9._-' '-')
    work_id="bench-${TIER}-${slug}-r${rep}"
    state_file="planning/$work_id/sdlc/sdlc-flow-state.json"

    echo "== [$work_id] preparing =="
    rm -rf "planning/$work_id"
    mkdir -p "planning/$work_id"
    cp "$CANONICAL_DIR/tasks.json" "planning/$work_id/tasks.json"
    cp "$CANONICAL_DIR/harness.json" "planning/$work_id/harness.json"

    lane="bench-${slug}-r${rep}-$(date +%s)"
    event_body=$(python3 - "$BRAIN_ROOT" "$ROADMAP" "$lane" "$work_id" "$AGENT_BACKEND" "$ENDPOINT" "$model" <<'PYEOF'
import json, sys
brain_root, roadmap, lane, block_id, agent_backend, endpoint, model = sys.argv[1:8]
print(json.dumps({
    "workflow_type": "ORCHESTRATION",
    "data": {
        "brain_root": brain_root,
        "roadmap_slug": roadmap,
        "lane": lane,
        "blocks": [{"repo": "engine-rs", "block_id": block_id}],
        "policy": {
            "default_auto_pr": False,
            "child_sdlc_flow_policy": {
                "agent_backend": agent_backend,
                "model_tiers": {"triage": "local", "review": "local"},
                "local": {"endpoint": endpoint, "model": model, "constrained_json": False},
            },
        },
    },
}))
PYEOF
)

    trigger=$(curl -sf -X POST "$BASTION_ADDR/events/" \
        -H "X-API-Key: $BASTION_ENGINE_API_KEY" \
        -H "Content-Type: application/json" \
        -d "$event_body")
    run_id=$(echo "$trigger" | python3 -c "import json,sys; print(json.load(sys.stdin).get('run_id',''))" 2>/dev/null)

    if [ -z "$run_id" ]; then
        echo "error: [$work_id] POST /events/ did not return a run_id ($trigger)" >&2
        [ "$KEEP_WORK_DIRS" -eq 1 ] || rm -rf "planning/$work_id"
        return 1
    fi

    echo "[$work_id] run_id=$run_id"

    local poll_out poll_rc run_status run_elapsed
    set +e
    poll_out=$(poll_run "$run_id")
    poll_rc=$?
    set -e
    run_status=$(echo "$poll_out" | tail -n1 | awk '{print $1}')
    run_elapsed=$(echo "$poll_out" | tail -n1 | awk '{print $2}')

    harvest_run "$model" "$TIER" "$rep" "$run_id" "$run_status" "$run_elapsed" "$state_file"

    if [ "$KEEP_WORK_DIRS" -eq 0 ]; then
        git worktree remove --force "trees/sdlc/$work_id" >/dev/null 2>&1 || true
        git branch -D "sdlc/$work_id" >/dev/null 2>&1 || true
        rm -rf "planning/$work_id"
    fi

    return "$poll_rc"
}

# ── Regenerate the comparison summary from every record in OUT_DIR ──────────

regenerate_summary() {
    python3 - "$OUT_DIR" <<'PYEOF'
import json, glob, os, sys

out_dir = sys.argv[1]
records = []
for path in sorted(glob.glob(os.path.join(out_dir, "*.json"))):
    if os.path.basename(path) == "summary.json":
        continue
    try:
        with open(path) as f:
            records.append(json.load(f))
    except Exception:
        continue

with open(os.path.join(out_dir, "summary.json"), "w") as f:
    json.dump(records, f, indent=2)
    f.write("\n")

lines = [
    "| Model | Rep | Status | Tasks passed | First failing task | Bail reason | Attempts | Cost (USD) | Wall clock (s) |",
    "|---|---|---|---|---|---|---|---|---|",
]
for r in records:
    tasks = r.get("tasks") or []
    total = len(tasks)
    passed = sum(1 for t in tasks if t.get("passed") or t.get("status") == "done")
    first_fail = next(
        (f"#{t.get('task_id')} {t.get('title')}" for t in tasks if not (t.get("passed") or t.get("status") == "done")),
        "-",
    )
    bail = (r.get("bail_reason") or "-").split(".")[0]
    lines.append(
        f"| {r.get('model')} | {r.get('rep')} | {r.get('status')} | {passed}/{total} | {first_fail} | {bail} | "
        f"{r.get('total_attempts')} | {r.get('total_cost_usd')} | {r.get('wall_clock_seconds')} |"
    )

with open(os.path.join(out_dir, "summary.md"), "w") as f:
    f.write("\n".join(lines) + "\n")

print("\n".join(lines))
PYEOF
}

# ── Classify models into light (parallel pool) / heavy (sequential) ────────

LIGHT_MODELS=()
HEAVY_MODELS=()
IFS=',' read -r -a MODEL_ARRAY <<< "$MODELS"
OVERALL_RC=0

for model in "${MODEL_ARRAY[@]}"; do
    size_gb=$(model_size_gb "$model" || true)
    if [ -z "$size_gb" ]; then
        echo "SKIPPING $model: not found in \`ollama list\` at $ENDPOINT -- pull it first (\`ollama pull $model\`)" >&2
        OVERALL_RC=1
        continue
    fi
    is_light=$(python3 -c "print(1 if $size_gb <= $LIGHT_MAX_GB else 0)")
    if [ "$is_light" -eq 1 ]; then
        LIGHT_MODELS+=("$model")
        echo "$model classified LIGHT (${size_gb}GB <= ${LIGHT_MAX_GB}GB) -> parallel pool"
    else
        HEAVY_MODELS+=("$model")
        echo "$model classified HEAVY (${size_gb}GB > ${LIGHT_MAX_GB}GB) -> sequential"
    fi
done

# ── Phase 1: light models, parallel pool (bounded by --max-parallel) ────────

PIDS=()
for model in "${LIGHT_MODELS[@]:-}"; do
    [ -z "$model" ] && continue
    rep=1
    while [ "$rep" -le "$REPEAT" ]; do
        while [ "${#PIDS[@]}" -ge "$MAX_PARALLEL" ]; do
            NEW_PIDS=()
            for pid in "${PIDS[@]}"; do
                if kill -0 "$pid" 2>/dev/null; then
                    NEW_PIDS+=("$pid")
                fi
            done
            PIDS=("${NEW_PIDS[@]:-}")
            [ "${#PIDS[@]}" -ge "$MAX_PARALLEL" ] && sleep 3
        done
        run_one "$model" "$rep" &
        PIDS+=("$!")
        rep=$((rep + 1))
    done
done

for pid in "${PIDS[@]:-}"; do
    [ -z "$pid" ] && continue
    wait "$pid" || OVERALL_RC=1
done

# ── Phase 2: heavy models, strictly sequential, after the light pool drains ─

for model in "${HEAVY_MODELS[@]:-}"; do
    [ -z "$model" ] && continue
    rep=1
    while [ "$rep" -le "$REPEAT" ]; do
        run_one "$model" "$rep" || OVERALL_RC=1
        rep=$((rep + 1))
    done
done

echo ""
echo "=== Comparison summary: tier=$TIER ($OUT_DIR/summary.md, $OUT_DIR/summary.json) ==="
regenerate_summary

echo ""
echo "Completed local-model bench tier=$TIER; overall exit=$OVERALL_RC"
exit "$OVERALL_RC"
