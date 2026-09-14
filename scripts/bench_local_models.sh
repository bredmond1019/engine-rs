#!/usr/bin/env bash
#
# bench_local_models.sh
#
# Dispatch the SAME real ORCHESTRATION-driven SDLC_FLOW task set, once per
# local Ollama model (and per repeat), and harvest structured pass/fail data
# for each -- so the comparison is of the MODEL, not of varying tasks. Pure
# bash + curl + python3: no Claude agent drives this, so running it costs
# nothing beyond local compute. SEQUENTIAL ONLY (deliberately, for now --
# parallel dispatch is possible but was cut here to keep this script simple
# and reliable; see "Known follow-ups" below if reviving it).
#
# THREE FIXED DIFFICULTY TIERS (planning/local-model-bench/tiers/<tier>/),
# never combined into one run: easy (a floor check -- capable models should
# pass reliably), medium (real logic, no compilation), hard (genuinely
# difficult -- only strong local or frontier models should reliably pass).
# Pick one tier per invocation with --tier; run the script three times (once
# per tier) for a full sweep. This is deliberate, not a limitation: a single
# run's pass/fail must be attributable to ONE difficulty level.
#
# A REGISTERED BLOCK IS REQUIRED -- an unregistered block_id is not an
# error, it is a SILENT SKIP that still reports overall "succeeded" having
# run nothing at all (BlockPresence::NotInTracks in
# crates/engine-core/src/workflows/orchestration/integrate.rs; measured
# 2026-09-14, cost a full debugging cycle). This script registers ONE block
# per tier (bench-<tier>), once, via `mev create-block --write` -- reused
# across every model/rep, exactly like scripts/run_micro_spec.sh's own
# clean-before-each-dispatch pattern. Only planning/bench-<tier>/tasks.json
# is rewritten per job (it never changes -- it's a straight copy of the
# canonical tier fixture every time); the block record itself is written
# once and never touched again.
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
# THE tasks FIELD IN sdlc-flow-state.json IS A JSON OBJECT KEYED BY STRING
# TASK ID (`{"1": {...}, "2": {...}}`), NOT AN ARRAY. Iterating it as a list
# silently produces an empty per-task summary (measured 2026-09-14) -- this
# script's harvest_run reads it as a dict. Also: a bailed task's `status`
# stays "pending" forever (TriageRouterNode's MAJOR_BAIL arm bypasses
# UpdateTaskStatusNode, the only writer of "done"/"failed"), so pass/fail per
# task is derived from `attempt_count >= max_attempts` alongside `status`,
# not from `status` alone.
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
#   --endpoint <url>      Ollama OpenAI-compatible base URL (default:
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
#                         flake from a model's genuine ceiling.
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
#    bail_reason, tasks: [{task_id, title, status, attempt_count,
#    max_attempts, passed, attempts_exhausted}], backend_used,
#    model_tier_used, total_cost_usd, total_attempts, review_verdicts}
#
# Plus <out>/summary.md (human table) and <out>/summary.json (machine list),
# REGENERATED (not appended) from every *.json record present in <out> at
# the end of this run -- so re-running with different models/repeats
# accumulates one growing comparison for that tier, not a fresh one each
# time.
#
# Known follow-ups (deliberately not done here):
#   - Parallel dispatch of light/small models was prototyped and worked
#     (mkdir-based slot locks, one registered block per parallel slot) but
#     was cut for simplicity -- getting model comparison DATA matters more
#     right now than wall-clock speed. Revive by giving each concurrent job
#     its own registered block (bench-<tier>-slot-N) instead of the single
#     bench-<tier> block this script uses, since two dispatches sharing one
#     block_id race on the same sdlc-flow-state.json.
#   - Consider a Python rewrite if this keeps growing -- the JSON
#     construction/parsing here is already routed entirely through inline
#     python3 one-liners and heredocs because bash has no native JSON
#     support; a real script would drop the heredoc-quoting fragility (a
#     heredoc combined with `<<<` on the same command silently breaks --
#     hit once already, fixed) and the bash-3.2-on-macOS constraints
#     (no `declare -A`, no `wait -n`, no `mapfile`).
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

WORK_ID="bench-${TIER}"
STATE_FILE="planning/$WORK_ID/sdlc/sdlc-flow-state.json"

# ── Block registration (once per tier) ──────────────────────────────────────

block_registered() {
    local id="$1"
    python3 -c "
import json
d = json.load(open('planning/state.json'))
ids = {b['id'] for t in d.get('tracks', []) for b in t.get('blocks', [])}
exit(0 if '$id' in ids else 1)
" 2>/dev/null
}

ensure_block_registered() {
    if block_registered "$WORK_ID"; then
        return 0
    fi
    echo "Registering bench block: $WORK_ID ..." >&2
    local payload
    payload=$(mktemp).json
    python3 -c "
import json
json.dump({
    'id': '$WORK_ID',
    'title': 'local-model-bench $TIER tier -- reusable dispatch target',
    'kind': 'chore',
    'sdlc_workflow': 'flow',
    'repo': 'engine-rs',
    'spec_dir': 'planning/$WORK_ID/',
    'model': 'sonnet',
    'what': 'A reusable local-model-bench dispatch target -- scripts/bench_local_models.sh overwrites planning/$WORK_ID/tasks.json fresh (from planning/local-model-bench/tiers/$TIER/) before every dispatch. The block record itself never changes.',
    'why': 'ORCHESTRATION refuses to dispatch a block_id absent from planning/state.json tracks[] (a silent skip, not an error), so a reusable target must be registered once.',
    'description': 'Infrastructure for the local-model-bench sweep. Never closed/wontfixed -- reused indefinitely as a dispatch target.',
    'acceptance_criteria': ['N/A -- infrastructure target, not a unit of work with a completion criterion.'],
    'out_of_scope': ['Any real implementation work -- this block id is never actually implemented, only dispatched through.'],
    'testing_strategy': 'Each dispatch through this block is its own test, harvested by the calling script.',
    'forward_looking': False,
    'epics': ['unattended-runs'],
}, open('$payload', 'w'), indent=2)
"
    mev create-block --write --scope engine-rs --from "$payload" >/dev/null 2>&1 || true
    rm -f "$payload"
    if ! block_registered "$WORK_ID"; then
        echo "error: failed to register block $WORK_ID -- run \`mev create-block --write --scope engine-rs --from <payload>\` by hand to see the real error" >&2
        exit 1
    fi
}

# ── Clean helper: reset the block's worktree/branch/state (same trap
# run_micro_spec.sh guards against -- a leftover sdlc-flow-state.json makes
# the NEXT dispatch resume a run already marked done) ───────────────────────

clean_block() {
    git worktree remove --force "trees/sdlc/$WORK_ID" >/dev/null 2>&1 || true
    git branch -D "sdlc/$WORK_ID" >/dev/null 2>&1 || true
    rm -rf "planning/$WORK_ID/sdlc"
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
            echo "$(date '+%H:%M:%S') status: $status (run $run_id)" >&2
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
# report into one structured per-run record ─────────────────────────────────

harvest_run() {
    local model="$1" rep="$2" run_id="$3" status="$4" elapsed="$5"
    local slug dest orch_record_file
    slug=$(echo "$model" | tr -c 'A-Za-z0-9._-' '-')
    dest="$OUT_DIR/${slug}-r${rep}-${run_id}.json"

    orch_record_file=$(mktemp)
    curl -sf -H "X-API-Key: $BASTION_ENGINE_API_KEY" "$BASTION_ADDR/events/$run_id" 2>/dev/null > "$orch_record_file" || echo '{}' > "$orch_record_file"

    python3 - "$dest" "$model" "$TIER" "$rep" "$run_id" "$status" "$elapsed" "$STATE_FILE" "$orch_record_file" <<'PYEOF'
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

# ── Dispatch one run; echoes "<run_id>" ─────────────────────────────────────

dispatch_run() {
    local model="$1" lane="$2"
    local event_body trigger run_id

    event_body=$(python3 - "$BRAIN_ROOT" "$ROADMAP" "$lane" "$WORK_ID" "$AGENT_BACKEND" "$ENDPOINT" "$model" <<'PYEOF'
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
        echo "error: POST /events/ did not return a run_id ($trigger)" >&2
        return 1
    fi
    echo "$run_id"
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

# ── Main sweep: sequential, every model x every rep ─────────────────────────

ensure_block_registered

IFS=',' read -r -a MODEL_ARRAY <<< "$MODELS"
OVERALL_RC=0

for model in "${MODEL_ARRAY[@]}"; do
    rep=1
    while [ "$rep" -le "$REPEAT" ]; do
        echo "== model=$model rep=$rep/$REPEAT =="
        clean_block
        mkdir -p "planning/$WORK_ID"
        cp "$CANONICAL_DIR/tasks.json" "planning/$WORK_ID/tasks.json"
        cp "$CANONICAL_DIR/harness.json" "planning/$WORK_ID/harness.json"

        lane="bench-$(echo "$model" | tr -c 'A-Za-z0-9._-' '-')-r${rep}-$(date +%s)"

        set +e
        RUN_ID=$(dispatch_run "$model" "$lane")
        DISPATCH_RC=$?
        set -e

        if [ -z "$RUN_ID" ]; then
            echo "error: model=$model rep=$rep produced no run_id" >&2
            OVERALL_RC=1
            rep=$((rep + 1))
            continue
        fi

        set +e
        POLL_OUT=$(poll_run "$RUN_ID")
        POLL_RC=$?
        set -e

        RUN_STATUS=$(echo "$POLL_OUT" | tail -n1 | awk '{print $1}')
        RUN_ELAPSED=$(echo "$POLL_OUT" | tail -n1 | awk '{print $2}')

        harvest_run "$model" "$rep" "$RUN_ID" "$RUN_STATUS" "$RUN_ELAPSED"

        if [ "$DISPATCH_RC" -ne 0 ] || [ "$POLL_RC" -ne 0 ]; then
            OVERALL_RC=1
        fi

        rep=$((rep + 1))
    done
done

clean_block

echo ""
echo "=== Comparison summary: tier=$TIER ($OUT_DIR/summary.md, $OUT_DIR/summary.json) ==="
regenerate_summary

echo ""
echo "Completed local-model bench tier=$TIER; overall exit=$OVERALL_RC"
exit "$OVERALL_RC"
