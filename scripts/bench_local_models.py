#!/usr/bin/env python3
"""
bench_local_models.py

Dispatch the SAME real SDLC_FLOW task set once per
(tier, local Ollama model, agent backend, repeat), and harvest structured
pass/fail data for each -- so the comparison is of the MODEL and BACKEND, not
of varying tasks. Pure stdlib: no Claude agent drives this, so a sweep costs
nothing beyond local compute.

Built for an unattended overnight sweep:

- **One job can never abort the sweep.** Every dispatch runs inside its own
  try/except and always leaves a structured JSON record, pass or fail.
- **Resumable.** Records land at <out>/<run-name>/<tier>/<backend>/<model>-r<rep>.json.
  Re-running the same command skips every job that already has a record,
  except infrastructure failures (exception / dispatch_error / infra_error),
  which are retried.
- **Balanced if cut short.** Jobs run rep -> tier -> model -> backend, so if a
  --deadline or a crash stops the sweep, every model has data for the early
  tiers rather than half the models having everything and half nothing.
- **Stops rather than corrupts.** Before each job it waits (--infra-wait-minutes)
  for `bastion serve` and Ollama; if either stays down it stops with exit 2.
  A job that exceeds --timeout-minutes is aborted via POST /events/{id}/abort;
  if the abort is not confirmed the sweep stops, because cleaning the shared
  block while its run is still alive would corrupt the next job.
- **Model-attributable classification.** After each run the tier's own
  validation_commands are re-run against the final worktree. That separates
  "the model did not do it" (no_change / wrong_path / check_failed) from "the
  engine rejected correct work" -- the class of engine defect this bench has
  already found twice.
- **Evidence kept.** Each job's SDLC state, run event, aider chat
  history and git log/diff are copied to <run>/artifacts/<job>/ before cleanup.
- **Enough context for Pi.** Ollama's default context silently truncated Pi's
  ~9k-token engine prompt to ~2k tokens, so the model never saw its task. Preflight
  creates a `<model>-ctx<N>` variant per model (--ollama-num-ctx, default 16384)
  and BOTH backends use it, so the comparison stays like-for-like.
- **Reports regenerated after every job**: <run>/leaderboard.md (with OKF
  frontmatter, it lives in the corpus) and <run>/summary.json.

WORKTREES ARE BASED ON origin/main, NOT LOCAL HEAD (SetupWorktreeNode,
crates/engine-core/src/workflows/sdlc_flow/setup.rs). A tier may only
reference repo files already pushed; preflight checks this. Checkers that are
not on origin/main live in planning/local-model-bench/checkers/ and are reached
through the worktree's planning/ symlink -- which also keeps them out of
aider's repo map. The bench's own docs/tiers/checkers/results moved 2026-09-14
to the HQ vault at planning/open-work/local-models/local-model-bench/ (they
outgrew this repo's own planning/); a compat symlink at
core/_planning/engine-rs/local-model-bench -> that new location keeps this
literal "planning/local-model-bench/..." checker path (baked into every tier's
tasks.json validation_commands, and into missing_fixture_paths()'s preflight
existence check below) resolving unchanged through the worktree's planning/
symlink. BENCH_DIR itself now points straight at the new HQ location.

CONFIG-FIRST FIXTURE GENERATION: each tier's harness.json is DERIVED from that
tier's tasks.json validation_commands at dispatch time (build_harness_from_tasks),
never hand-authored a second time -- a hand-duplicated copy once drifted and
failed every dispatch regardless of what the model produced.

SDLC_FLOW IS DISPATCHED DIRECTLY, WITH NO block_id. Through ORCHESTRATION a
passing job CLOSES its block (measured 2026-09-14: the close ran a fleet-wide
emit-state, rewrote HQ state/status/lane files, and every later job was skipped
with "block status is 'closed'; not dispatched"). CloseBlockNode no-ops on a run
with no block_id, so a direct run touches no block state and writes nothing into
any roadmap's lane log. Every job reuses one spec directory (--spec-slug) and
overwrites its tasks.json and harness.json.

The model and backend travel in the event's `policy` field; a misplaced policy is
silently ignored and falls back to real-Claude defaults (measured: $1.38 on a
run meant to be free), so tests pin the event shape.

sdlc-flow-state.json's `tasks` is an object keyed by task id, and a bailed
task's status stays "pending", so engine-side pass/fail is `status == "done"`.

Usage:
  bench_local_models.py --models all --dry-run
  bench_local_models.py --models all --agent-backends aider,pi --repeat 2 --deadline 07:30
"""
from __future__ import annotations

import argparse
import dataclasses
import json
import os
import re
import shutil
import statistics
import subprocess
import sys
import time
import traceback
import urllib.error
import urllib.request
from collections import Counter
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_DIR = SCRIPT_DIR.parent
HQ_ROOT = REPO_DIR.parent.parent  # core/engine-rs -> core -> agentic-portfolio
# Moved 2026-09-14 out of this repo's own planning/ vault into the HQ-level
# open-work tree (it outgrew a single-repo home). A compat symlink at
# core/_planning/engine-rs/local-model-bench -> this same directory keeps the
# literal "planning/local-model-bench/..." checker paths baked into every
# tier's tasks.json (and checked by missing_fixture_paths() below) resolving
# through the worktree's own planning/ symlink -- do not remove that symlink.
BENCH_DIR = HQ_ROOT / "planning" / "open-work" / "local-models" / "local-model-bench"
TIERS_DIR = BENCH_DIR / "tiers"

DEFAULT_TIERS = "easy,edit,medium,hard,rust"
BACKENDS = ("aider", "pi")
# Backends that drive a coding agent and therefore always send Ollama tool
# definitions on every call. Ollama's OpenAI-compat endpoint hard-rejects a
# tools-incapable model with an instant HTTP 400 before generation starts --
# a guaranteed, uninformative failure, confirmed 2026-09-14 for phi3.5:3.8b
# and codestral:22b (see docs/local-model-bench.md pitfall table).
CODING_AGENT_BACKENDS = frozenset({"aider", "pi"})
CAPABILITY_CHOICES = ("auto", "tools", "completion")
REVIEW_MODES = ("per_task", "trivial_skip", "end_only")
TERMINAL_STATUSES = ("succeeded", "failed", "cancelled", "budget_halted")
RETRYABLE_OUTCOMES = ("exception", "dispatch_error", "infra_error")
ENGINE_ANOMALIES = ("engine_rejected_correct_work", "engine_accepted_failing_work", "abort_unconfirmed")
NOISE_PATH_PREFIXES = (".aider", ".gitignore")
ARTIFACT_TEXT_CAP_BYTES = 200_000
FINAL_CHECK_TIMEOUT_SECONDS = 180
REPO_PATH_RE = re.compile(r"(?<![\w/.-])((?:scripts|crates|docs)/[\w./-]*\w)")
PLANNING_PATH_RE = re.compile(r"(?<![\w/.-])(planning/[\w./-]*\w)")
CTX_VARIANT_RE = re.compile(r"-ctx\d+$")


def load_env_file(path: Path) -> None:
    """Best-effort KEY=VALUE loader for scripts/.env; never overrides an exported variable."""
    if not path.is_file():
        return
    for raw_line in path.read_text().splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, _, value = line.partition("=")
        os.environ.setdefault(key.strip(), value.strip().strip('"').strip("'"))


def now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def slugify(s: str) -> str:
    return "".join(c if c.isalnum() or c in "._-" else "-" for c in s)


def log(msg: str) -> None:
    print(f"[{datetime.now():%H:%M:%S}] {msg}", file=sys.stderr, flush=True)


# ── Config-first fixture generation ─────────────────────────────────────────


def build_harness_from_tasks(tasks: list[dict]) -> dict:
    """Derive a tier's harness.json from its own tasks.json validation_commands."""
    checks = []
    for task in tasks:
        commands = task.get("validation_commands") or []
        if not commands:
            continue
        checks.append(
            {
                "name": f"task-{task['task_id']}",
                "command": " && ".join(commands),
                "purpose": task.get("title", f"Task {task['task_id']} validation."),
                "gates": True,
            }
        )
    return {
        "$schema": "../../.claude/workflows/harness.schema.json",
        "stack": "fixture",
        "_comment": (
            "Generated by scripts/bench_local_models.py:build_harness_from_tasks "
            "from this tier's own tasks.json validation_commands -- do not hand-edit. "
            "Without this override, a dispatch resolves to the repo's own "
            "planning/harness.json and gates on engine-rs's Rust suite."
        ),
        "validation": {"checks": checks},
        "uiTest": {"enabled": False},
    }


def missing_fixture_paths(tasks: list[dict]) -> list[str]:
    """Repo paths a tier references must exist on origin/main (worktrees are
    based there); planning/ paths must exist in the vault."""
    text = " ".join(
        " ".join(t.get("validation_commands") or []) + " " + str(t.get("description", "")) for t in tasks
    )
    missing = []
    for path in sorted(set(REPO_PATH_RE.findall(text))):
        probe = subprocess.run(
            ["git", "cat-file", "-e", f"origin/main:{path}"], cwd=REPO_DIR, capture_output=True, check=False
        )
        if probe.returncode != 0:
            missing.append(f"{path} (not on origin/main)")
    for path in sorted(set(PLANNING_PATH_RE.findall(text))):
        if not (REPO_DIR / path).exists():
            missing.append(f"{path} (not in the vault)")
    return missing


# ── HTTP helpers (stdlib only) ──────────────────────────────────────────────


def _headers(api_key: str | None) -> dict:
    return {"X-API-Key": api_key} if api_key else {}


def http_post_json(url: str, api_key: str | None, payload: dict, timeout: float = 30.0) -> dict:
    req = urllib.request.Request(
        url,
        data=json.dumps(payload).encode("utf-8"),
        method="POST",
        headers={**_headers(api_key), "Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        body = resp.read().decode("utf-8")
        return json.loads(body) if body.strip() else {}


def http_get_json(url: str, api_key: str | None, timeout: float = 30.0) -> dict:
    req = urllib.request.Request(url, method="GET", headers=_headers(api_key))
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read().decode("utf-8"))


def http_status(url: str, api_key: str | None = None, timeout: float = 10.0) -> int | None:
    req = urllib.request.Request(url, method="GET", headers=_headers(api_key))
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return resp.status
    except urllib.error.HTTPError as e:
        return e.code
    except (urllib.error.URLError, TimeoutError, OSError):
        return None


# ── Infrastructure checks ───────────────────────────────────────────────────


def infra_ready(bastion_addr: str, endpoint: str) -> tuple[bool, str]:
    status = http_status(f"{bastion_addr}/health")
    if status != 200:
        return False, f"bastion serve {bastion_addr}/health -> {status}"
    status = http_status(f"{endpoint}/api/tags")
    if status != 200:
        return False, f"ollama {endpoint}/api/tags -> {status}"
    return True, "ok"


def wait_for_infra(bastion_addr: str, endpoint: str, wait_minutes: float) -> tuple[bool, str]:
    deadline = time.monotonic() + wait_minutes * 60
    while True:
        ok, why = infra_ready(bastion_addr, endpoint)
        if ok or time.monotonic() >= deadline:
            return ok, why
        log(f"waiting for infrastructure: {why}")
        time.sleep(15)


def resolve_required_capability(choice: str, backends: list[str]) -> str:
    """Resolve --require-capability's `auto` into a concrete floor: `tools`
    whenever any selected backend always sends tool definitions (aider, pi),
    else `completion` (a future non-agentic backend needs no tool-calling)."""
    if choice != "auto":
        return choice
    return "tools" if any(b in CODING_AGENT_BACKENDS for b in backends) else "completion"


def resolve_models(models_arg: str, endpoint: str) -> list[str]:
    if models_arg.strip() == "all":
        tags = http_get_json(f"{endpoint}/api/tags", None).get("models", [])
        return [
            m["name"]
            for m in sorted(tags, key=lambda m: m.get("size", 0))
            if not CTX_VARIANT_RE.search(m["name"])
        ]
    return [m.strip() for m in models_arg.split(",") if m.strip()]


def commit_subject_collisions(tier_tasks: dict[str, list[dict]]) -> list[str]:
    """LoadTaskStateNode marks a task done, without running it, when
    `feat(sdlc): <id> — <title>` is already in git history (setup.rs). A bench
    branch merged into main once put two bench titles there, so those tasks were
    silently skipped on every later run. Every task title must be absent."""
    subjects = set(
        subprocess.run(
            ["git", "log", "origin/main", "--format=%s", "--fixed-strings", "--grep=feat(sdlc):"],
            cwd=REPO_DIR, capture_output=True, text=True, check=False,
        ).stdout.splitlines()
    )
    problems = []
    for tier, tasks in tier_tasks.items():
        for task in tasks:
            subject = f"feat(sdlc): {task.get('task_id')} — {task.get('title')}"
            if subject in subjects:
                problems.append(
                    f"tier {tier}: origin/main already has a commit '{subject}', so the engine "
                    f"would mark task {task.get('task_id')} done without running it -- retitle it"
                )
    return problems


def tasks_already_satisfied(tier_tasks: dict[str, list[dict]]) -> list[str]:
    """Run every task's checks in a pristine detached worktree of origin/main.
    A check that already passes there scores a model for doing nothing (measured
    2026-09-14: a merged bench branch put the easy tier's outputs on origin/main).
    Returns one problem string per already-satisfied task."""
    scratch = Path(os.environ.get("TMPDIR", "/tmp")) / f"bench-pristine-{os.getpid()}"
    shutil.rmtree(scratch, ignore_errors=True)
    added = subprocess.run(
        ["git", "worktree", "add", "--detach", str(scratch), "origin/main"],
        cwd=REPO_DIR, capture_output=True, text=True, check=False,
    )
    if added.returncode != 0:
        return [f"could not create a pristine origin/main worktree: {added.stderr.strip()}"]
    problems = []
    try:
        planning = scratch / "planning"
        if not planning.exists():
            planning.symlink_to((REPO_DIR / "planning").resolve())
        for tier, tasks in tier_tasks.items():
            for result in run_final_checks(tasks, scratch):
                if result["passed"]:
                    problems.append(
                        f"tier {tier}: task {result['task_id']}'s checks already pass on a pristine "
                        f"origin/main -- the task is already done there"
                    )
    finally:
        subprocess.run(["git", "worktree", "remove", "--force", str(scratch)], cwd=REPO_DIR, capture_output=True, check=False)
        shutil.rmtree(scratch, ignore_errors=True)
        subprocess.run(["git", "worktree", "prune"], cwd=REPO_DIR, capture_output=True, check=False)
    return problems


def ctx_variant_name(model: str, num_ctx: int) -> str:
    """The Ollama model name carrying a raised num_ctx, e.g. qwen2.5:7b-instruct-ctx16384."""
    return f"{model}-ctx{num_ctx}" if ":" in model else f"{model}:latest-ctx{num_ctx}"


def ensure_ctx_variant(model: str, num_ctx: int, endpoint: str, create: bool) -> tuple[str, bool]:
    """Returns (variant_name, existed). Ollama's default context silently truncated
    Pi's ~9k-token engine prompt to ~2k tokens (measured 2026-09-14): the model lost
    the task and tool definitions. Pi sends no num_ctx, so the context has to live
    in the model itself. A variant reuses the base weights; `ollama rm` removes it."""
    variant = ctx_variant_name(model, num_ctx)
    try:
        http_post_json(f"{endpoint}/api/show", None, {"model": variant})
        return variant, True
    except Exception:  # noqa: BLE001 -- absent variant
        pass
    if not create:
        return variant, False
    modelfile = Path(os.environ.get("TMPDIR", "/tmp")) / f"bench-modelfile-{slugify(variant)}"
    modelfile.write_text(f"FROM {model}\nPARAMETER num_ctx {num_ctx}\n")
    try:
        result = subprocess.run(
            ["ollama", "create", variant, "-f", str(modelfile)],
            capture_output=True, text=True, check=False,
            env={**os.environ, "OLLAMA_HOST": endpoint},
        )
    finally:
        modelfile.unlink(missing_ok=True)
    if result.returncode != 0:
        raise RuntimeError(f"ollama create {variant} failed: {result.stderr.strip()[-300:]}")
    return variant, False


def preflight(
    *, tiers: list[str], models_arg: str, backends: list[str], endpoint: str, bastion_addr: str, api_key: str,
    num_ctx: int = 0, create_variants: bool = False, require_capability: str = "completion",
) -> tuple[list[str], list[str], list[dict], list[str], dict, list[dict]]:
    """Returns (problems, warnings, excluded, runnable_models, model_info, all_model_caps).
    Any problem blocks the sweep. `excluded` is models filtered by capability
    (distinct from `warnings`, which covers other preflight notices); `all_model_caps`
    is the capability record for every resolved model, runnable or not, for the
    durable capability report."""
    problems: list[str] = []
    warnings: list[str] = []
    excluded: list[dict] = []
    all_model_caps: list[dict] = []
    tier_tasks: dict[str, list[dict]] = {}
    for tier in tiers:
        tasks_path = TIERS_DIR / tier / "tasks.json"
        if not tasks_path.is_file():
            problems.append(f"tier {tier}: {tasks_path} not found")
            continue
        try:
            tasks = json.loads(tasks_path.read_text())
        except json.JSONDecodeError as e:
            problems.append(f"tier {tier}: tasks.json does not parse ({e})")
            continue
        missing = missing_fixture_paths(tasks)
        for path in missing:
            problems.append(f"tier {tier}: {path}")
        if not missing:
            tier_tasks[tier] = tasks
    problems.extend(commit_subject_collisions(tier_tasks))
    problems.extend(tasks_already_satisfied(tier_tasks))
    for backend in backends:
        if shutil.which(backend) is None:
            problems.append(f"backend {backend}: `{backend}` is not on PATH")

    health = http_status(f"{bastion_addr}/health")
    if health != 200:
        problems.append(f"bastion serve not healthy at {bastion_addr} (/health -> {health})")
    elif http_status(f"{bastion_addr}/events/suspended", api_key) in (401, 403):
        problems.append("BASTION_ENGINE_API_KEY was rejected by bastion serve (scripts/.env must match the running server)")

    if http_status(f"{endpoint}/api/tags") != 200:
        problems.append(f"ollama not reachable at {endpoint}")
        return problems, warnings, excluded, [], {}, all_model_caps

    runnable: list[str] = []
    info: dict = {}
    for model in resolve_models(models_arg, endpoint):
        try:
            # Capability comes from /api/show, NOT the /api/tags list used by
            # resolve_models(): measured 2026-09-14, /api/tags reports
            # deepseek-r1:14b/32b as `[completion, thinking]` (no `tools`),
            # while /api/show for the same model returns `[tools, thinking,
            # completion]`. /api/tags is unreliable for capability filtering.
            show = http_post_json(f"{endpoint}/api/show", None, {"model": model})
        except Exception as e:  # noqa: BLE001
            problems.append(f"model {model}: not available in ollama ({e})")
            continue
        caps = show.get("capabilities") or []
        details = show.get("details") or {}
        cap_record = {
            "model": model,
            "capabilities": caps,
            "parameter_size": details.get("parameter_size"),
            "quantization_level": details.get("quantization_level"),
            "family": details.get("family"),
        }
        all_model_caps.append(cap_record)
        if "completion" not in caps:
            excluded.append({**cap_record, "reason": f"no completion capability {caps}"})
            continue
        if require_capability == "tools" and "tools" not in caps:
            excluded.append(
                {
                    **cap_record,
                    "reason": (
                        f"no tools capability {caps} -- required because backend(s) "
                        f"{sorted(set(backends) & CODING_AGENT_BACKENDS)} always send tool definitions "
                        "and Ollama's OpenAI-compat endpoint hard-rejects a tools-incapable model with "
                        "HTTP 400 before generation starts"
                    ),
                }
            )
            continue
        ollama_model = model
        if num_ctx:
            try:
                ollama_model, existed = ensure_ctx_variant(model, num_ctx, endpoint, create_variants)
            except Exception as e:  # noqa: BLE001
                problems.append(f"model {model}: {e}")
                continue
            if not existed:
                verb = "created" if create_variants else "will create"
                warnings.append(f"model {model}: {verb} context variant {ollama_model}")
        runnable.append(model)
        info[model] = {
            "capabilities": caps,
            "parameter_size": details.get("parameter_size"),
            "quantization_level": details.get("quantization_level"),
            "family": details.get("family"),
            "ollama_model": ollama_model,
            "num_ctx": num_ctx or None,
        }
    if not runnable:
        problems.append("no runnable models")
    return problems, warnings, excluded, runnable, info, all_model_caps


# ── Block registration / cleanup ────────────────────────────────────────────


def block_id_registered(spec_slug: str) -> bool:
    """Guard: the spec slug must not name a registered block, or the run would close it."""
    try:
        state = json.loads((REPO_DIR / "planning" / "state.json").read_text())
    except (OSError, json.JSONDecodeError):
        return False
    return spec_slug in {b.get("id") for t in state.get("tracks", []) for b in t.get("blocks", [])}


def worktree_dir(work_id: str) -> Path:
    return REPO_DIR / "trees" / "sdlc" / work_id


def clean_block(work_id: str) -> None:
    def git(*args: str) -> None:
        subprocess.run(["git", *args], cwd=REPO_DIR, capture_output=True, check=False)

    git("worktree", "remove", "--force", f"trees/sdlc/{work_id}")
    wt = worktree_dir(work_id)
    if wt.exists():
        shutil.rmtree(wt, ignore_errors=True)
    git("worktree", "prune")
    git("branch", "-D", f"sdlc/{work_id}")
    shutil.rmtree(REPO_DIR / "planning" / work_id / "sdlc", ignore_errors=True)


# ── Job plan ────────────────────────────────────────────────────────────────


@dataclasses.dataclass(frozen=True)
class JobSpec:
    tier: str
    model: str
    backend: str
    rep: int

    @property
    def slug(self) -> str:
        return f"{self.tier}__{self.backend}__{slugify(self.model)}__r{self.rep}"

    def record_path(self, run_dir: Path) -> Path:
        return run_dir / self.tier / self.backend / f"{slugify(self.model)}-r{self.rep}.json"


def plan_jobs(tiers: list[str], models: list[str], backends: list[str], repeat: int) -> list[JobSpec]:
    return [
        JobSpec(tier, model, backend, rep)
        for rep in range(1, repeat + 1)
        for tier in tiers
        for model in models
        for backend in backends
    ]


def completed_record(path: Path) -> dict | None:
    """A record that should NOT be re-run: present, parseable, and not an infrastructure failure."""
    if not path.is_file():
        return None
    try:
        record = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError):
        return None
    return None if record.get("outcome") in RETRYABLE_OUTCOMES else record


def parse_deadline(value: str | None, now: datetime | None = None) -> datetime | None:
    """HH:MM in local time; a time already past today means tomorrow."""
    if not value:
        return None
    now = now or datetime.now()
    hour, minute = (int(part) for part in value.split(":"))
    deadline = now.replace(hour=hour, minute=minute, second=0, microsecond=0)
    return deadline + timedelta(days=1) if deadline <= now else deadline


# ── Job record: who / what / where / when / why ────────────────────────────


@dataclasses.dataclass
class JobRecord:
    # who
    model: str
    backend: str
    model_info: dict = dataclasses.field(default_factory=dict)
    backend_used: Any = None
    model_tier_used: Any = None
    engine_build_sha: str | None = None
    # what
    tier: str = ""
    rep: int = 0
    review_mode: str = ""
    outcome: str = "unknown"  # passed | task_failed | timeout | exception | dispatch_error | infra_error
    failure_category: str = "unknown"
    run_status: str | None = None
    timed_out: bool = False
    fatal: bool = False
    tasks: list[dict] = dataclasses.field(default_factory=list)
    tasks_total: int = 0
    tasks_passed_final: int = 0
    final_checks: list[dict] = dataclasses.field(default_factory=list)
    changed_paths: list[str] = dataclasses.field(default_factory=list)
    undeclared_paths: list[str] = dataclasses.field(default_factory=list)
    total_cost_usd: float | None = None
    total_attempts: int | None = None
    review_verdicts: Any = None
    # where
    run_id: str | None = None
    endpoint: str = ""
    artifacts_dir: str | None = None
    # when
    started_at: str = ""
    ended_at: str = ""
    wall_clock_seconds: float = 0.0
    # why
    bail_reason: str | None = None
    error: str | None = None
    error_traceback: str | None = None

    def to_json(self) -> dict:
        return dataclasses.asdict(self)


@dataclasses.dataclass
class SweepConfig:
    bastion_addr: str
    api_key: str
    endpoint: str
    work_id: str
    review_mode: str
    call_timeout_seconds: int | None
    poll_interval: float
    timeout_minutes: float
    abort_grace_minutes: float
    run_dir: Path
    test_dispatch: str = "inline"
    model_info: dict = dataclasses.field(default_factory=dict)


def build_event_body(spec: JobSpec, cfg: SweepConfig) -> dict:
    policy: dict = {
        "agent_backend": spec.backend,
        # Every non-agentic SDLC_FLOW model stage this bench dispatches must
        # resolve to the local model under test — "triage"/"review" alone
        # left `PatchDocsNode`/`GenerateTasksNode` silently calling the real
        # `claude` CLI with sonnet/opus (measured 2026-09-14; see
        # `docs/local-model-bench.md` § Pitfalls). `implement` is
        # deliberately absent: the agentic implement stage routes through
        # `agent_backend` (aider/pi), not a bare model-tier swap.
        "model_tiers": {
            "triage": "local",
            "review": "local",
            "docs": "local",
            "generate": "local",
        },
        "local": {
            "endpoint": cfg.endpoint,
            "model": (cfg.model_info.get(spec.model) or {}).get("ollama_model") or spec.model,
            "constrained_json": False,
        },
        "review_mode": cfg.review_mode,
        # Inline, not HQ's default queue_park: a queued check's resume path
        # reported every task failed (job record `passed: null`, measured
        # 2026-09-14), and the queue's free-memory floor can stall behind a
        # loaded 32B model. These checks take milliseconds.
        "test_dispatch": cfg.test_dispatch,
    }
    if cfg.call_timeout_seconds:
        seconds = cfg.call_timeout_seconds
        policy["timeouts"] = {
            "implement": seconds,
            "triage": seconds,
            "review": seconds,
            "docs": seconds,
            "generate": seconds,
        }
    return {
        "workflow_type": "SDLC_FLOW",
        "data": {
            "spec_slug": cfg.work_id,
            "repo": "engine-rs",
            "use_worktree": True,
            "auto_pr": False,
            "resume": False,
            "llm_triage": False,
            "policy": policy,
        },
    }


# ── Poll / abort ────────────────────────────────────────────────────────────


def poll_run(
    bastion_addr: str, api_key: str, run_id: str, poll_interval: float, timeout_minutes: float
) -> tuple[str, float]:
    start = time.monotonic()
    deadline = start + timeout_minutes * 60
    last_status = None
    while True:
        now = time.monotonic()
        if now >= deadline:
            return "timeout", now - start
        try:
            status = http_get_json(f"{bastion_addr}/events/{run_id}", api_key).get("status", "unknown")
        except (urllib.error.URLError, TimeoutError, json.JSONDecodeError, OSError) as e:
            log(f"  poll error (continuing): {e}")
            status = last_status or "unknown"
        if status != last_status:
            log(f"  status: {status} (run {run_id})")
            last_status = status
        if status in TERMINAL_STATUSES:
            return status, time.monotonic() - start
        time.sleep(poll_interval)


def abort_run(bastion_addr: str, api_key: str, run_id: str) -> None:
    try:
        http_post_json(f"{bastion_addr}/events/{run_id}/abort", api_key, {})
    except Exception as e:  # noqa: BLE001
        log(f"  abort request failed (will still wait for a terminal status): {e}")


# ── Harvest + final-tree evidence ───────────────────────────────────────────


def harvest_state(record: JobRecord, state_file: Path) -> None:
    flow_state: dict = {}
    if state_file.is_file():
        try:
            flow_state = json.loads(state_file.read_text())
        except (OSError, json.JSONDecodeError):
            flow_state = {}

    outcomes = flow_state.get("outcomes") or {}
    record.bail_reason = flow_state.get("bail_reason")
    record.engine_build_sha = flow_state.get("engine_build_sha")
    record.backend_used = outcomes.get("backend_used")
    record.model_tier_used = outcomes.get("model_tier_used")
    record.total_cost_usd = outcomes.get("total_cost_usd")
    record.total_attempts = outcomes.get("total_attempts")
    record.review_verdicts = outcomes.get("review_verdicts")

    raw_tasks = flow_state.get("tasks") or {}
    summaries = []
    if isinstance(raw_tasks, dict):
        for key, task in sorted(raw_tasks.items(), key=lambda kv: int(kv[0]) if str(kv[0]).isdigit() else 0):
            if not isinstance(task, dict):
                continue
            status = task.get("status")
            attempts = task.get("attempt_count")
            max_attempts = task.get("max_attempts")
            summaries.append(
                {
                    "task_id": task.get("task_id"),
                    "title": task.get("title"),
                    "status": status,
                    "attempt_count": attempts,
                    "max_attempts": max_attempts,
                    "passed": status == "done",
                    "attempts_exhausted": status != "done"
                    and isinstance(attempts, int)
                    and isinstance(max_attempts, int)
                    and attempts >= max_attempts,
                }
            )
    record.tasks = summaries


def run_final_checks(tasks: list[dict], worktree: Path) -> list[dict]:
    results = []
    for task in tasks:
        commands = task.get("validation_commands") or []
        entry = {"task_id": task.get("task_id"), "passed": False, "output": ""}
        if not worktree.is_dir():
            entry["output"] = "worktree missing"
        else:
            try:
                proc = subprocess.run(
                    ["bash", "-c", " && ".join(commands) or "true"],
                    cwd=worktree,
                    capture_output=True,
                    text=True,
                    timeout=FINAL_CHECK_TIMEOUT_SECONDS,
                    check=False,
                )
                entry["passed"] = proc.returncode == 0
                entry["output"] = (proc.stdout + proc.stderr)[-2000:]
            except subprocess.TimeoutExpired:
                entry["output"] = f"timed out after {FINAL_CHECK_TIMEOUT_SECONDS}s"
        results.append(entry)
    return results


def _git_text(worktree: Path, *args: str) -> str:
    proc = subprocess.run(["git", *args], cwd=worktree, capture_output=True, text=True, check=False)
    return proc.stdout


def changed_paths(worktree: Path) -> list[str]:
    if not worktree.is_dir():
        return []
    paths = set(_git_text(worktree, "diff", "--name-only", "origin/main").split())
    paths.update(_git_text(worktree, "ls-files", "--others", "--exclude-standard").split())
    return sorted(p for p in paths if not p.startswith(NOISE_PATH_PREFIXES))


def classify(*, engine_tasks: list[dict], final_checks: list[dict], changed: list[str], declared: set[str]) -> str:
    engine_tasks_passed = bool(engine_tasks) and all(t.get("passed") for t in engine_tasks)
    checks_ok = bool(final_checks) and all(c.get("passed") for c in final_checks)
    if engine_tasks_passed and checks_ok:
        return "passed"
    if checks_ok:
        return "engine_rejected_correct_work"
    # A later task may never have run because an earlier, correct one was
    # rejected -- that is still the engine's failure, not the model's.
    passing_ids = {c.get("task_id") for c in final_checks if c.get("passed")}
    if any(t.get("attempts_exhausted") and t.get("task_id") in passing_ids for t in engine_tasks):
        return "engine_rejected_correct_work"
    if engine_tasks_passed:
        return "engine_accepted_failing_work"
    if not changed:
        return "no_change"
    if declared and not declared.intersection(changed):
        return "wrong_path"
    return "check_failed"


def capture_evidence(dest: Path, worktree: Path, work_id: str, orch_event: dict, final_checks: list[dict]) -> Path:
    dest.mkdir(parents=True, exist_ok=True)
    (dest / "run-event.json").write_text(json.dumps(orch_event, indent=2) + "\n")
    (dest / "final-checks.json").write_text(json.dumps(final_checks, indent=2) + "\n")
    sdlc_dir = REPO_DIR / "planning" / work_id / "sdlc"
    if sdlc_dir.is_dir():
        shutil.copytree(sdlc_dir, dest / "sdlc", dirs_exist_ok=True)
    if worktree.is_dir():
        history = worktree / ".aider.chat.history.md"
        if history.is_file():
            (dest / "aider.chat.history.txt").write_text(history.read_text(errors="replace")[-ARTIFACT_TEXT_CAP_BYTES:])
        git_text = (
            "## git log --stat origin/main..HEAD\n"
            + _git_text(worktree, "log", "--stat", "origin/main..HEAD")
            + "\n## git diff origin/main\n"
            + _git_text(worktree, "diff", "origin/main")
            + "\n## untracked\n"
            + _git_text(worktree, "ls-files", "--others", "--exclude-standard")
        )
        (dest / "git.txt").write_text(git_text[:ARTIFACT_TEXT_CAP_BYTES])
    return dest


# ── One job: dispatch + poll + harvest, wrapped so nothing propagates ───────


def run_one_job(spec: JobSpec, cfg: SweepConfig) -> JobRecord:
    record = JobRecord(
        model=spec.model,
        backend=spec.backend,
        model_info=cfg.model_info.get(spec.model, {}),
        tier=spec.tier,
        rep=spec.rep,
        review_mode=cfg.review_mode,
        endpoint=cfg.endpoint,
        started_at=now_iso(),
    )
    start = time.monotonic()
    worktree = worktree_dir(cfg.work_id)
    orch_event: dict = {}

    try:
        clean_block(cfg.work_id)
        work_dir = REPO_DIR / "planning" / cfg.work_id
        work_dir.mkdir(parents=True, exist_ok=True)
        tasks = json.loads((TIERS_DIR / spec.tier / "tasks.json").read_text())
        (work_dir / "tasks.json").write_text(json.dumps(tasks, indent=2) + "\n")
        (work_dir / "harness.json").write_text(json.dumps(build_harness_from_tasks(tasks), indent=2) + "\n")

        trigger = http_post_json(f"{cfg.bastion_addr}/events/", cfg.api_key, build_event_body(spec, cfg))
        run_id = trigger.get("run_id")
        if not run_id:
            record.outcome = "dispatch_error"
            record.failure_category = "infra"
            record.error = f"POST /events/ did not return a run_id: {trigger}"
            return record
        record.run_id = run_id

        status, _ = poll_run(cfg.bastion_addr, cfg.api_key, run_id, cfg.poll_interval, cfg.timeout_minutes)
        if status == "timeout":
            record.timed_out = True
            log(f"  job exceeded {cfg.timeout_minutes}m, aborting run {run_id}")
            abort_run(cfg.bastion_addr, cfg.api_key, run_id)
            status, _ = poll_run(cfg.bastion_addr, cfg.api_key, run_id, cfg.poll_interval, cfg.abort_grace_minutes)
        record.run_status = status
        record.wall_clock_seconds = round(time.monotonic() - start, 1)

        if status == "timeout":
            record.outcome = "infra_error"
            record.failure_category = "abort_unconfirmed"
            record.fatal = True
            record.error = f"run {run_id} still not terminal {cfg.abort_grace_minutes}m after abort"
            return record

        try:
            orch_event = http_get_json(f"{cfg.bastion_addr}/events/{run_id}", cfg.api_key)
        except Exception:  # noqa: BLE001
            orch_event = {}

        harvest_state(record, work_dir / "sdlc" / "sdlc-flow-state.json")
        record.final_checks = run_final_checks(tasks, worktree)
        record.changed_paths = changed_paths(worktree)
        declared = {f for t in tasks for f in t.get("files") or []}
        record.undeclared_paths = [p for p in record.changed_paths if p not in declared]
        record.tasks_total = len(tasks)
        record.tasks_passed_final = sum(1 for c in record.final_checks if c["passed"])
        record.failure_category = classify(
            engine_tasks=record.tasks,
            final_checks=record.final_checks,
            changed=record.changed_paths,
            declared=declared,
        )
        if record.timed_out:
            record.outcome = "timeout"
        elif record.failure_category == "passed":
            record.outcome = "passed"
        else:
            record.outcome = "task_failed"

    except Exception as e:  # noqa: BLE001 -- deliberate: a job must never kill the sweep
        record.outcome = "exception"
        record.failure_category = "infra"
        record.error = str(e)
        record.error_traceback = traceback.format_exc()
        log(f"  !! job exception (recorded, continuing): {e}")
    finally:
        record.ended_at = now_iso()
        if not record.wall_clock_seconds:
            record.wall_clock_seconds = round(time.monotonic() - start, 1)
        try:
            dest = cfg.run_dir / "artifacts" / spec.slug
            record.artifacts_dir = str(capture_evidence(dest, worktree, cfg.work_id, orch_event, record.final_checks))
        except Exception as err:  # noqa: BLE001
            log(f"  !! evidence capture error (non-fatal): {err}")
        if not record.fatal:
            try:
                clean_block(cfg.work_id)
            except Exception as err:  # noqa: BLE001
                log(f"  !! cleanup error (non-fatal): {err}")

    return record


# ── Reports ─────────────────────────────────────────────────────────────────


def load_records(run_dir: Path) -> list[dict]:
    records = []
    if not run_dir.is_dir():
        return records
    for tier_dir in sorted(p for p in run_dir.iterdir() if p.is_dir() and p.name != "artifacts"):
        for path in sorted(tier_dir.glob("*/*.json")):
            try:
                record = json.loads(path.read_text())
            except (OSError, json.JSONDecodeError):
                continue
            if isinstance(record, dict) and "model" in record and "tier" in record:
                records.append(record)
    return records


def _cell(text: Any, limit: int = 100) -> str:
    return str(text if text is not None else "-").replace("|", "/").replace("\n", " ")[:limit]


# `bail_reason` routinely wraps the actually-useful diagnostic (e.g. a real
# check's stdout/traceback) behind a fixed, verbose "how to recover" template
# -- naively truncating from the front (as the Jobs table's Reason column
# used to) cut the reason off exactly before the useful part on every
# "max attempts reached" bail. Prefer the tail after a known marker.
_DIAGNOSTIC_MARKERS = ("Failing check detail:", "check_id:", "reason:")


def _diagnostic_reason(text: Any) -> str:
    """The actionable tail of a bail/error reason, not its boilerplate head."""
    s = str(text if text is not None else "-")
    for marker in _DIAGNOSTIC_MARKERS:
        idx = s.find(marker)
        if idx != -1:
            return s[idx:]
    return s


def render_reports(run_dir: Path, run_name: str) -> str:
    records = load_records(run_dir)
    run_dir.mkdir(parents=True, exist_ok=True)
    (run_dir / "summary.json").write_text(json.dumps(records, indent=2) + "\n")

    known = DEFAULT_TIERS.split(",")
    tiers = sorted({r["tier"] for r in records}, key=lambda t: (known.index(t) if t in known else len(known), t))
    groups: dict[tuple[str, str], list[dict]] = {}
    for r in records:
        groups.setdefault((r["model"], r["backend"]), []).append(r)

    rows = []
    for (model, backend), recs in groups.items():
        scored = [r for r in recs if r.get("outcome") not in RETRYABLE_OUTCOMES]
        passed = [r for r in scored if r.get("outcome") == "passed"]
        cells = []
        for tier in tiers:
            tier_scored = [r for r in scored if r["tier"] == tier]
            tier_passed = sum(1 for r in tier_scored if r.get("outcome") == "passed")
            cells.append(f"{tier_passed}/{len(tier_scored)}" if tier_scored else "-")
        misses = Counter(r.get("failure_category") for r in scored if r.get("outcome") != "passed")
        times = [r.get("wall_clock_seconds") or 0 for r in passed]
        rows.append(
            {
                "model": model,
                "backend": backend,
                "size": (recs[0].get("model_info") or {}).get("parameter_size") or "-",
                "cells": cells,
                "rate": len(passed) / len(scored) if scored else 0.0,
                "total": f"{len(passed)}/{len(scored)}",
                "median": round(statistics.median(times)) if times else None,
                "top_miss": ", ".join(f"{k} x{v}" for k, v in misses.most_common(2)) or "-",
                "infra": len(recs) - len(scored),
            }
        )
    rows.sort(key=lambda row: (-row["rate"], row["median"] if row["median"] is not None else float("inf")))

    lines = [
        "---",
        "type: Reference",
        f'title: "local-model-bench results -- {run_name}"',
        (
            f"description: Generated leaderboard and per-job table for local-model-bench run {run_name}, "
            "regenerated after every job by scripts/bench_local_models.py -- do not hand-edit."
        ),
        f"doc_id: local-model-bench-results-{slugify(run_name).lower()}",
        "layer: [engine]",
        "project: engine-rs",
        "status: active",
        "keywords: [local model bench, results, leaderboard, ollama]",
        "related: [local-model-bench-index]",
        "---",
        "",
        f"# local-model-bench results — {run_name}",
        "",
        f"{len(records)} job records. Pass cells are passed/scored jobs; infrastructure failures are not scored.",
        "Categories: `no_change`, `wrong_path`, `check_failed` are the model's; `engine_*` and "
        "`abort_unconfirmed` are engine anomalies worth a bug report.",
        "",
        "## Leaderboard",
        "",
        "| Model | Size | Backend | " + " | ".join(tiers) + " | Total | Median pass (s) | Main misses | Infra |",
        "|---|---|---|" + "---|" * len(tiers) + "---|---|---|---|",
    ]
    for row in rows:
        lines.append(
            f"| {row['model']} | {row['size']} | {row['backend']} | " + " | ".join(row["cells"])
            + f" | {row['total']} | {row['median'] if row['median'] is not None else '-'} | {row['top_miss']} | {row['infra']} |"
        )

    anomalies = [r for r in records if r.get("failure_category") in ENGINE_ANOMALIES]
    lines += ["", "## Engine anomalies", ""]
    if anomalies:
        for r in anomalies:
            lines.append(
                f"- {r['tier']} / {r['backend']} / {r['model']} r{r['rep']}: `{r['failure_category']}` "
                f"-- evidence `{r.get('artifacts_dir')}`"
            )
    else:
        lines.append("None.")

    lines += [
        "",
        "## Jobs",
        "",
        "| Tier | Backend | Model | Rep | Outcome | Category | Final checks | Attempts | Run status | Wall (s) | Reason |",
        "|---|---|---|---|---|---|---|---|---|---|---|",
    ]
    for r in sorted(records, key=lambda r: (r.get("rep", 0), r["tier"], r["model"], r["backend"])):
        reason = r.get("error") or r.get("bail_reason")
        lines.append(
            f"| {r['tier']} | {r['backend']} | {r['model']} | {r.get('rep')} | {r.get('outcome')} | "
            f"{r.get('failure_category')} | {r.get('tasks_passed_final', 0)}/{r.get('tasks_total', 0)} | "
            f"{_cell(r.get('total_attempts'))} | {_cell(r.get('run_status'))} | {_cell(r.get('wall_clock_seconds'))} | "
            f"{_cell(_diagnostic_reason(reason), limit=200)} |"
        )

    # Full, untruncated reason per non-passing job -- the table above is for
    # scanning; this is for reading. Grouped so the same recurring failure
    # text (a systemic bug) is visually obvious rather than scattered.
    failing = [r for r in records if r.get("outcome") != "passed" or r.get("run_status") == "failed"]
    lines += ["", "## Failure detail (full, untruncated)", ""]
    if failing:
        by_reason: dict[str, list[dict]] = {}
        for r in failing:
            reason = str(r.get("error") or r.get("bail_reason") or "(no reason recorded)")
            by_reason.setdefault(reason, []).append(r)
        # Recurring reasons first -- these are the ones worth investigating
        # as an engine/tool bug rather than N independent model mistakes.
        for reason, recs in sorted(by_reason.items(), key=lambda kv: -len(kv[1])):
            who = ", ".join(f"{r['tier']}/{r['backend']}/{r['model']}r{r.get('rep')}" for r in recs)
            count_flag = f" **(seen {len(recs)}x -- check for a systemic cause before blaming the model)**" if len(recs) > 1 else ""
            lines.append(f"### {who}{count_flag}")
            lines.append("")
            lines.append(f"```\n{reason}\n```")
            lines.append("")
    else:
        lines.append("None.")

    text = "\n".join(lines) + "\n"
    (run_dir / "leaderboard.md").write_text(text)
    return text


# ── Model capability report ─────────────────────────────────────────────────

# content_pipeline nodes confirmed 2026-09-14 (crates/engine-core/src/workflows/
# content_pipeline/{summarize,self_critic,revise,translate}.rs) to parse plain
# model text via parse_structured_or_fenced rather than driving a tool-calling
# loop -- so a completion-only model is architecturally usable there, though
# never benchmarked: passing this bench's tool-driven coding tasks says nothing
# about summarization/critique/translation quality. See planning/backlog.md.
COMPLETION_ONLY_CANDIDATE_NODES = (
    "SummarizeNode",
    "SelfCriticNode",
    "ReviseNode",
    "TranslateNode",
)


def render_capability_report(
    all_model_caps: list[dict], excluded: list[dict], require_capability: str, backends: list[str]
) -> str:
    """Durable record of every resolved model's Ollama capability set, written to
    planning/local-model-bench/ on every preflight (dry-run or real) so a model
    pulled next month shows up here with zero code changes. do not hand-edit."""
    BENCH_DIR.mkdir(parents=True, exist_ok=True)
    (BENCH_DIR / "model-capabilities.json").write_text(json.dumps(all_model_caps, indent=2) + "\n")

    excluded_models = {e["model"] for e in excluded}
    tools_capable = sorted(c["model"] for c in all_model_caps if "tools" in (c["capabilities"] or []))
    completion_only = sorted(
        c["model"] for c in all_model_caps if "completion" in (c["capabilities"] or []) and "tools" not in (c["capabilities"] or [])
    )

    lines = [
        "---",
        "type: Reference",
        'title: "local-model-bench model capabilities"',
        (
            "description: Ollama capability set (tools vs completion-only) for every model resolved by "
            "scripts/bench_local_models.py, regenerated on every preflight -- do not hand-edit."
        ),
        "doc_id: local-model-bench-model-capabilities",
        "layer: [engine]",
        "project: engine-rs",
        "status: active",
        "keywords: [local model bench, ollama, capabilities, tools, completion]",
        "related: [local-model-bench-index]",
        f"updated: {datetime.now():%Y-%m-%d}",
        "---",
        "",
        "# local-model-bench model capabilities",
        "",
        f"Generated by `scripts/bench_local_models.py:render_capability_report`. Current selection floor: "
        f"`--require-capability {require_capability}` (backends: {', '.join(backends) or '-'}).",
        "",
        "## Tools-capable (usable by aider/pi coding-agent backends)",
        "",
    ]
    lines += [f"- `{m}`" for m in tools_capable] or ["None."]
    lines += ["", "## Completion-only (excluded from aider/pi dispatch; see opportunity note below)", ""]
    lines += [f"- `{m}`" for m in completion_only] or ["None."]
    lines += ["", "## Excluded this run", ""]
    if excluded:
        lines += [f"- `{e['model']}`: {e['reason']}" for e in excluded]
    else:
        lines.append("None.")
    lines += [
        "",
        "## Opportunity: completion-only models for non-coding-agent nodes (not yet evaluated)",
        "",
        (
            "Models with no `tools` capability (e.g. `phi3.5:3.8b`, `codestral:22b`) can never run through "
            "aider/pi, but several content_pipeline nodes call a local model for plain text generation, not "
            "tool-calling, and could plausibly use them: "
            + ", ".join(f"`{n}`" for n in COMPLETION_ONLY_CANDIDATE_NODES)
            + " (crates/engine-core/src/workflows/content_pipeline/, each parses the reply via "
            "parse_structured_or_fenced). This is a hypothesis, not a result -- passing this bench's "
            "tool-driven coding tasks says nothing about summarization/critique/translation quality, and no "
            "such run has been attempted. Tracked as a backlog idea, not committed work: see "
            "`planning/backlog.md` (repo:engine-rs, `local-model-bench-completion-only-models-for-content-pipeline`)."
        ),
        "",
    ]
    text = "\n".join(lines) + "\n"
    (BENCH_DIR / "model-capabilities.md").write_text(text)
    return text


# ── CLI ──────────────────────────────────────────────────────────────────────


def parse_args(argv: list[str]) -> argparse.Namespace:
    env = os.environ.get
    p = argparse.ArgumentParser(
        description="Dispatch local Ollama models through real SDLC_FLOW runs across fixed tiers.",
    )
    p.add_argument("--models", required=True, help="Comma-separated Ollama model names, or `all` (every pulled model, smallest first, base variants only -- excluded by --require-capability, see that flag).")
    p.add_argument("--tiers", default=env("BENCH_LOCAL_MODELS_TIERS", DEFAULT_TIERS), help=f"Comma-separated tier directories under planning/local-model-bench/tiers/ (default: {DEFAULT_TIERS}).")
    p.add_argument("--agent-backends", default=env("BENCH_LOCAL_MODELS_AGENT_BACKENDS", "aider,pi"), help="Comma-separated local ImplementTaskNode transports to compare (aider, pi).")
    p.add_argument("--repeat", type=int, default=int(env("BENCH_LOCAL_MODELS_REPEAT", "1")), help="Repeats per (tier, model, backend); all of rep 1 runs before any of rep 2.")
    p.add_argument("--run-name", default=env("BENCH_LOCAL_MODELS_RUN_NAME", datetime.now().strftime("%Y-%m-%d")), help="Results subdirectory; re-running with the same name resumes (default: today's date).")
    p.add_argument("--out", default=None, help="Base results directory (default: planning/local-model-bench/results).")
    p.add_argument("--deadline", default=env("BENCH_LOCAL_MODELS_DEADLINE"), help="Local HH:MM after which no new job starts (a past time means tomorrow).")
    p.add_argument("--timeout-minutes", type=float, default=float(env("BENCH_LOCAL_MODELS_TIMEOUT_MINUTES", "40")), help="Per-job limit before the run is aborted and recorded as outcome=timeout.")
    p.add_argument("--abort-grace-minutes", type=float, default=float(env("BENCH_LOCAL_MODELS_ABORT_GRACE_MINUTES", "10")), help="How long to wait for an aborted run to stop before halting the sweep.")
    p.add_argument("--call-timeout-seconds", type=int, default=int(env("BENCH_LOCAL_MODELS_CALL_TIMEOUT_SECONDS", "0")) or None, help="Optional per-model-call timeout for implement/triage/review (child policy `timeouts`); unset keeps the engine default.")
    p.add_argument("--review-mode", choices=REVIEW_MODES, default=env("BENCH_LOCAL_MODELS_REVIEW_MODE", "end_only"), help="Child SDLC_FLOW review_mode (default end_only: one local-model review of the whole run's diff against its base). per_task/trivial_skip review `git diff HEAD`, which is empty after aider's auto-commit, so the reviewer fails correct aider work (measured 2026-09-14). end_only needs an engine build with EndReviewNode on the local tier.")
    p.add_argument("--ollama-num-ctx", type=int, default=int(env("BENCH_LOCAL_MODELS_OLLAMA_NUM_CTX", "16384")), help="Context window baked into a per-model Ollama variant (<model>-ctx<N>) used by both backends; 0 uses each model as-is. Pi sends no num_ctx, and Ollama's default truncated its ~9k-token engine prompt.")
    p.add_argument("--require-capability", choices=CAPABILITY_CHOICES, default=env("BENCH_LOCAL_MODELS_REQUIRE_CAPABILITY", "auto"), help="Ollama capability floor for --models all selection, queried live from `POST /api/show` (never a hardcoded model-name list). `tools` excludes any model lacking Ollama's `tools` capability -- coding-agent backends (aider, pi) always send tool definitions, and Ollama's OpenAI-compat endpoint hard-rejects a tools-incapable model with HTTP 400 before generation starts. `completion` allows any completion-capable model. `auto` (default) resolves to `tools` whenever --agent-backends includes aider or pi, else `completion` -- so the aider/pi default is safe with no flag needed.")
    p.add_argument("--test-dispatch",choices=("inline", "queue_park"), default=env("BENCH_LOCAL_MODELS_TEST_DISPATCH", "inline"), help="Child SDLC_FLOW test_dispatch (default inline; see build_event_body).")
    p.add_argument("--endpoint", default=env("BENCH_LOCAL_MODELS_ENDPOINT", "http://localhost:11434"), help="Ollama base URL.")
    p.add_argument("--spec-slug", default=env("BENCH_LOCAL_MODELS_SPEC_SLUG", "local-model-bench-run"), help="planning/<slug>/ spec directory every job reuses; its worktree is trees/sdlc/<slug>. Never a registered block id.")
    p.add_argument("--poll-interval", type=float, default=float(env("BENCH_LOCAL_MODELS_POLL_INTERVAL", "5")), help="Seconds between status polls.")
    p.add_argument("--infra-wait-minutes", type=float, default=float(env("BENCH_LOCAL_MODELS_INFRA_WAIT_MINUTES", "10")), help="How long to wait for bastion serve/Ollama before stopping the sweep.")
    p.add_argument("--dry-run", action="store_true", help="Run preflight and print the job plan without dispatching anything.")
    return p.parse_args(argv)


def main(argv: list[str]) -> int:
    load_env_file(SCRIPT_DIR / ".env")
    args = parse_args(argv)

    tiers = [t.strip() for t in args.tiers.split(",") if t.strip()]
    backends = [b.strip() for b in args.agent_backends.split(",") if b.strip()]
    bad_backends = [b for b in backends if b not in BACKENDS]
    if not tiers or not backends or bad_backends or args.repeat < 1:
        print(f"error: need tiers, backends from {BACKENDS} (bad: {bad_backends}), and --repeat >= 1", file=sys.stderr)
        return 3
    try:
        deadline = parse_deadline(args.deadline)
    except ValueError:
        print(f"error: --deadline must be HH:MM, got {args.deadline!r}", file=sys.stderr)
        return 3

    api_key = os.environ.get("BASTION_ENGINE_API_KEY")
    if not api_key:
        print("error: BASTION_ENGINE_API_KEY is not set (scripts/.env or environment)", file=sys.stderr)
        return 1
    bastion_addr = os.environ.get("BASTION_SERVE_ADDR", "http://localhost:4317")
    run_dir = (Path(args.out) if args.out else BENCH_DIR / "results") / args.run_name
    require_capability = resolve_required_capability(args.require_capability, backends)

    problems, warnings, excluded, models, model_info, all_model_caps = preflight(
        tiers=tiers, models_arg=args.models, backends=backends,
        endpoint=args.endpoint, bastion_addr=bastion_addr, api_key=api_key,
        num_ctx=args.ollama_num_ctx, create_variants=not args.dry_run,
        require_capability=require_capability,
    )
    render_capability_report(all_model_caps, excluded, require_capability, backends)
    for w in warnings:
        log(f"warning: {w}")
    for e in excluded:
        log(f"excluded (capability): {e['model']} -- {e['reason']}")
    for problem in problems:
        log(f"PREFLIGHT FAILED: {problem}")
    if problems:
        return 2

    jobs = plan_jobs(tiers, models, backends, args.repeat)
    pending = [job for job in jobs if completed_record(job.record_path(run_dir)) is None]
    log(f"run {run_dir}: {len(jobs)} jobs planned, {len(jobs) - len(pending)} already recorded, {len(pending)} to run")
    log(f"models: {', '.join(models)}")
    log(f"tiers: {', '.join(tiers)}; backends: {', '.join(backends)}; deadline: {deadline or 'none'}")
    log(f"require-capability: {require_capability} ({len(excluded)} model(s) excluded, see {BENCH_DIR / 'model-capabilities.md'})")
    if args.dry_run:
        if excluded:
            print(f"# excluded (capability floor: {require_capability}):")
            for e in excluded:
                print(f"#   {e['model']}: {e['reason']}")
        for job in pending:
            print(job.slug)
        return 0

    if block_id_registered(args.spec_slug):
        log(f"error: --spec-slug {args.spec_slug} is a registered block id; a passing run would close it")
        return 3
    cfg = SweepConfig(
        bastion_addr=bastion_addr, api_key=api_key, endpoint=args.endpoint,
        work_id=args.spec_slug, review_mode=args.review_mode, call_timeout_seconds=args.call_timeout_seconds,
        poll_interval=args.poll_interval, timeout_minutes=args.timeout_minutes,
        abort_grace_minutes=args.abort_grace_minutes, run_dir=run_dir, model_info=model_info,
        test_dispatch=args.test_dispatch,
    )

    stopped = None
    for index, spec in enumerate(pending, start=1):
        if deadline and datetime.now() >= deadline:
            stopped = f"deadline {args.deadline} reached"
            break
        ok, why = wait_for_infra(bastion_addr, args.endpoint, args.infra_wait_minutes)
        if not ok:
            stopped = f"infrastructure unavailable: {why}"
            break
        log(f"== [{index}/{len(pending)}] {spec.slug} ==")
        record = run_one_job(spec, cfg)
        path = spec.record_path(run_dir)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(record.to_json(), indent=2) + "\n")
        log(f"  -> {record.outcome} / {record.failure_category} in {record.wall_clock_seconds}s")
        render_reports(run_dir, args.run_name)
        if record.fatal:
            stopped = f"fatal job {spec.slug}: {record.error}"
            break

    render_reports(run_dir, args.run_name)
    if stopped:
        log(f"STOPPED: {stopped}. Re-run the same command to resume.")
        return 2
    log(f"done: {run_dir / 'leaderboard.md'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
