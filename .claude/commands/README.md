# Slash Commands

Custom Claude Code commands for projects scaffolded from `base-template/`. All commands are flat
— invoke with `/<name>` directly (e.g. `/prime`, `/plan`, `/implement`, `/commit`).

These drive **structured spec work**: a spec lives at `planning/<name>/tasks.md`, and
the pipeline takes it through implement → test → review → document → wrap-up, writing
predictably-named reports alongside it.

> **Project-agnostic harness.** The command set and `workflows/*.js` engines are fully
> stack-neutral. Validation commands, ports/routes, and the UI-test stage are all driven by
> each project's `planning/harness.json` — the engines carry no stack defaults. Copy a profile
> from `planning/harness.examples.md` to configure your project's stack.
> See `planning/decisions/D5-okf-phase-2-adopted.md` for the adoption record.

---

## Directory Layout

All commands live directly in `.claude/commands/` — no subdirectories (except `brain/`).
`sync-global-commands` installs all non-brain commands into `~/.claude/commands/`.

```
.claude/commands/
  README.md                        ← this file
  sync-global-commands.md          ← syncs all non-brain commands to ~/.claude/commands/
  e2e-templates-README.md          ← usage guide for the e2e test templates

  archive.md        capture.md       commit.md        handoff.md
  log-work.md       prime.md         session-recap.md status.md
  wrap-up.md        update-state.md

  breakdown.md      chore.md         generate-master-plan.md  generate-tasks.md
  plan.md           ticket.md

  close-out.md      conditional_docs.md  document.md      fix.md
  implement.md      patch.md             process-tasks.md review-PR.md
  review-task.md    review-workflow.md   test.md          update-docs.md
  update-task.md

  clean-worktree.md  init-worktree.md  merge-train.md  start-block.md

  test_auth_gate.md  test_crud_api.md  test_error_handling.md  test_ui_form.md

  brain/                           ← reference only; NEVER synced to ~/.claude/commands/
    (flat — same filenames as brain's own .claude/commands/)
```

### Command Summary

| Group | Commands |
|---|---|
| Session | `/prime`, `/session-recap`, `/status`, `/handoff`, `/wrap-up`, `/log-work`, `/archive`, `/capture` |
| State | `/update-state` — how to safely edit `planning/state.json` per `state-schema.md` |
| Planning | `/generate-master-plan`, `/generate-tasks`, `/plan`, `/ticket`, `/chore`, `/breakdown` |
| SDLC | `/implement`, `/test`, `/fix`, `/patch`, `/document`, `/update-docs`, `/conditional_docs`, `/process-tasks`, `/update-task`, `/review-task`, `/review-workflow`, `/review-PR`, `/close-out` |
| Git | `/commit`, `/init-worktree`, `/clean-worktree`, `/start-block`, `/merge-train` |
| E2E | `/test_auth_gate`, `/test_crud_api`, `/test_error_handling`, `/test_ui_form` |

### `brain/` — Reference Only

`brain/` contains a reference copy of all brain-level commands (flat — same filenames as the brain
repo's own `.claude/commands/`). It is **never** synced to `~/.claude/commands/` (the
`--exclude='brain/'` flag in `sync-global-commands` enforces this). Brain commands are managed by
the brain repo's own `sync-brain-commands` command.

### `sync-global-commands`

Run `/sync-global-commands` from base-template root to install (or update) all harness commands
into `~/.claude/commands/`. The command:
- Guards that it is running from the base-template root.
- Runs `rsync -av --delete --exclude='brain/' .claude/commands/ ~/.claude/commands/`.
- Verifies with a dry-run that nothing remains to sync.
- Reports file counts before and after and confirms `brain/` is absent from global.

---

## SDLC Pipeline

The complete development lifecycle for structured spec work. Each step runs in a fresh agent
context, starts with `/prime`, reads the prior step's output file, and writes a
predictably-named output file.

### Phase Table

| SDLC Phase | Command | Role | Output |
|---|---|---|---|
| Session Start | `/session-recap` | Briefing: recent Log entries, where you left off, next step | chat only |
| Session Start | `/status` | Check current focus and what's in progress | chat only |
| Session Start | `/process-tasks` | Check which specs are eligible to start | chat only |
| Session End | `/wrap-up [note]` | Log work + commit; clean close without a handoff file | status.md, log.md, git |
| Session End | `/handoff [note]` | Write handoff + log work + commit; hands off to a fresh session | `planning/handoff.md`, status.md, log.md, git |
| Session End | `/close-out [--skip-coverage] [--no-review] [--review-level <level>] [--clean-worktree] [note]` | Verify coverage → review code → patch docs → clean worktree (opt.) → hand off; the quality-close pipeline | status.md, log.md, docs/, git |
| Block Setup | `/start-block [name]` | Flip a spec to `In progress` in status.md | status.md |
| **1 — Roadmap** | `/generate-master-plan [desc]` | Author the full roadmap as canonical block definitions | `planning/master-plan.md` |
| **1 — Plan** | `/generate-tasks <name>` · `/generate-tasks --from <path>` | Write the full task spec from a master-plan block, **or** from a standalone block file (`--from`) | `planning/<name>/tasks.md` |
| **1 — Plan (ad-hoc)** | `/chore` · `/ticket` · `/plan <desc>` | Plan ad-hoc work from a free-text description (not a master-plan block) | `planning/<prefix>-<slug>/{tasks,plan}.md` |
| **1 — Plan (opt.)** | `/breakdown <spec>` | Decompose spec into atomic, agent-executable sub-steps | `planning/<name>/breakdown.md` |
| **2 — Implement** | `/implement <spec> [N]` | Execute every task (or task N) in the spec | `planning/<name>/sdlc/reports/[taskN-]implement.md` |
| **2 — Hotfix** | `/patch` | Implement → validate → commit for low-risk single-file fixes; skips test/review/document | git history |
| **2 — Fix** | `/fix <spec> [N]` | Targeted fixes for FAIL/PARTIAL verdict; reads review report; overwrites implement report | `planning/<name>/sdlc/reports/[taskN-]implement.md` |
| **2 — Track** | `/update-task [name] <step> [note]` | Mark a step done and/or append a dated note mid-implementation | spec file (in-place) |
| **2 — Commit** | `/commit [hint]` | Stage + commit with a conventional message | git history |
| **3 — Test** | `/test <spec> [N]` | Run the project's validation suite; write snapshot | `planning/<name>/sdlc/reports/[taskN-]test.md` |
| **4 — Review** | `/review-task <spec> [N]` | Verify all criteria; run fresh tests; issue verdict | `planning/<name>/sdlc/reports/[taskN-]review.md` |
| **5 — Document** | `/document <spec> [N]` | Surgically patch `docs/`; gates on PASS verdict | `planning/<name>/sdlc/reports/[taskN-]document.md` |
| **6 — Wrap-up** | `/log-work [notes]` | Update status.md + append Log entry + sync company brain | status.md, log.md, brain `docs/projects/<slug>.md`, brain `README.md` |
| **7 — Verify run** | `/review-workflow <name> [N]` | Audit pipeline execution: reports, commits, Log, STATUS | `planning/<name>/sdlc/reports/[taskN-]workflow-review.md` |

### Pipeline Flow

```
SESSION START
  /status                  → read-only: current focus and what's next
  /process-tasks           → read-only: which specs are eligible

BLOCK SETUP
  /start-block <spec>      → status.md

PHASE 1 — PLAN
  /generate-tasks <spec>                 → planning/<spec>/tasks.md
        ↓  (optional)
  /breakdown planning/<spec>/tasks.md   → planning/<spec>/breakdown.md

PHASE 2 — IMPLEMENT
  /implement planning/<spec>/tasks.md [N]
        → planning/<spec>/sdlc/reports/[taskN-]implement.md
  (/update-task and /commit can be called any number of times during this phase)

PHASE 3 — TEST
  /test planning/<spec>/tasks.md [N]
        → planning/<spec>/sdlc/reports/[taskN-]test.md

PHASE 4 — REVIEW                   ← runs fresh tests; verdict gates next step
  /review-task planning/<spec>/tasks.md [N]
        → planning/<spec>/sdlc/reports/[taskN-]review.md

        if PASS → continue to PHASE 5 — DOCUMENT
        if FAIL/PARTIAL → PHASE 2 — FIX:
  /fix planning/<spec>/tasks.md [N]
        → planning/<spec>/sdlc/reports/[taskN-]implement.md  (overwritten)
  then repeat: /test [N] → /review-task [N] until PASS

PHASE 5 — DOCUMENT                 ← gates on PASS verdict
  /document planning/<spec>/tasks.md [N]
        → planning/<spec>/sdlc/reports/[taskN-]document.md

PHASE 6 — WRAP-UP
  /log-work [notes]        → status.md, log.md

(OPTIONAL) PHASE 7 — VERIFY RUN
  /review-workflow <spec> [N] → planning/<spec>/sdlc/reports/[taskN-]workflow-review.md
```

### Argument Convention

Every step from Phase 2 onward takes the same form: `planning/<name>/tasks.md [N]`

Split on the last space. Trailing number = task N (scope to that task only). No number = full
spec. Use the **same `N`** throughout the pipeline — it determines all report filenames at
every step.

### Directory Layout

Each spec gets its own directory under `planning/`. All reports live in a `reports/`
subdirectory alongside the spec:

```
planning/
  <spec>/
    tasks.md          ← spec (written by /generate-tasks)
    breakdown.md      ← optional (written by /breakdown)
    sdlc/
      reports/
        implement.md         ← or task3-implement.md for task-scoped
        test.md              ← or task3-test.md
        review.md            ← or task3-review.md
        document.md          ← or task3-document.md
        workflow.md          ← or task3-workflow.md (written by /sdlc-run)
        workflow-review.md   ← or task3-workflow-review.md (written by /review-workflow)
```

### Report File Naming

Pattern: `[taskN-]{step}.md` inside `planning/<name>/sdlc/reports/`

| Step | Full-spec | Task-scoped |
|---|---|---|
| implement | `implement.md` | `task3-implement.md` |
| fix | *(overwrites implement slot)* | *(overwrites implement slot)* |
| test | `test.md` | `task3-test.md` |
| review | `review.md` | `task3-review.md` |
| document | `document.md` | `task3-document.md` |
| workflow (sdlc-run) | `workflow.md` | `task3-workflow.md` |
| workflow-review | `workflow-review.md` | `task3-workflow-review.md` |

> **Note:** `/fix` writes to the same `implement.md` slot as `/implement` — it represents the
> current state of Phase 2 work. Git history preserves prior versions.

---

## Automated & Orchestrated Pipelines

The manual Phase 1 → 7 commands above can be run end-to-end by automated workflows
(`workflows/*.js`). Invoke them like slash commands. Each runs the same pipeline stages, but
unattended.

| Workflow | Scope | Isolation |
|---|---|---|
| `/sdlc-run <name> [N]` | one task or a **full spec**, sequential | none — runs on the current branch, updates STATUS/Log directly |
| `/sdlc-task <name> N` | **one** task, parallel-safe | own git worktree; defers STATUS/Log to merge time |
| `/sdlc-flow <name> [range]` | a **full spec** as one shared worktree, per-task test→fix loop, one end review, a PR | own worktree; terminates in a PR |
| `/sdlc-block [plan-file]` | a **whole roadmap** (master-plan) as a branch train — one `/sdlc-flow` per independent block, in dependency-ordered waves | each block its own worktree + PR; orchestrator owns the train branch and merges in dependency order |

> **Full reference with mermaid diagrams, per-stage detail, and token usage:**
> [`docs/workflows/`](../../docs/workflows/index.md) — one page per engine plus the manual lifecycle.

### `/sdlc-block` — roadmap orchestration (branch train)

**Drive a whole master-plan roadmap to completion in one invocation.** Fans out **one `/sdlc-flow` per
independent block** over dependency-ordered waves, producing a **branch train of reviewable PRs**.
Blocks in a wave are independent *by construction* (the master-plan's per-block **Files** + **Out of
scope** contract). A **pre-flight** guarantees a clean tree with the plan committed and sets up the train
branch off the base; **enumerate** parses the `## Phase N` / `### Block X` sections into blocks + a
dependency graph. Per wave it ensures each block's `tasks.md`, fans out the child flows (each `--no-pr`),
runs a **per-block close-out gap-check** (scoped to the whole block, `<train>...HEAD`), then opens the PR
(default) or merges into the base (`--auto-merge`), advancing the train in dependency order. A final
`/close-out --gap-check-only` runs over the full train. See
[D34](../../planning/decisions/D34-adhoc-planning-seam.md).

| Arg | Meaning | Default |
|---|---|---|
| `[plan-file]` | Optional 1st positional — a master-plan-format path, or a slug → `planning/<slug>/plan.md`. | `planning/master-plan.md` |
| `--base <branch>` | Base branch the train forks from / merges into. | `main` |
| `--auto-merge` | Merge each block into `<base>` in dependency order (no PRs). | off |
| `--no-pr` | Branch train only — no PRs anywhere. | off |
| `--max-parallel-blocks N` | Max `/sdlc-flow` runs in flight per wave (default from `harness.json` `block.maxParallelBlocks`). | `3` |
| `--blocks <sel>` | Phase selection: `0`, `0-1`, `0,2` — only those phases' blocks run. | all phases |
| `--resume` | Re-read `block-orchestration-state.json`, skip done blocks, continue. | — |

After the train is built, review each PR with **`/review-PR <PR#>`** and land them bottom-up with
**`/merge-train`** (below).

### `/review-PR <PR#> [plan-slug]`
Spec-aware review for a branch-train PR. Locates the block's `block-orchestration-state.json`, checks
out the PR, runs the project's gating suite (from `harness.json`, falling back to the spec's
`## Validation Commands`) + the emoji gate (merge-base scoped), reviews the diff against the block's
Acceptance Criteria, and posts an APPROVE / REQUEST_CHANGES / COMMENT verdict via `gh pr review`. Restores
the original branch when done.

### `/merge-train [plan-slug]`
Merges the block-train PRs into the base in the recorded `merge_order` (dependency order), halting on the
first unresolved conflict. Pre-flights a clean tree + synced base, classifies each block
(ready / already-merged / needs-approval / has-conflicts / escalated), stops before any merge if any PR
is `CONFLICTING`, confirms with you, then merges each via `gh pr merge --merge --delete-branch`. Exits
early for `--auto-merge` / `--no-pr` runs. Resume-safe — already-merged blocks are auto-detected on re-run.

---

## Session Orientation

### `/wrap-up [note]`
Clean session close without a handoff. Runs `/log-work` (syncs status.md + appends log entry)
then `/commit`. Use this when you're done with a piece of work and don't need to hand off
to a fresh agent.

### `/handoff [note]`
Session end-of-context handoff. Writes `planning/handoff.md` (what's in flight, completed,
remaining, open questions, first command for the next agent), then invokes `/log-work` and
`/commit`. `/prime` in the next session detects the handoff file and surfaces it first.
Delete `planning/handoff.md` once the new session has consumed it.

### `/close-out [--gap-check-only] [--skip-coverage] [--no-review] [--review-level <level>] [--clean-worktree] [note]`
Quality-close pipeline for the end of an `sdlc-run` or `sdlc-flow` session. Runs five
steps in sequence: **(1)** the full validation suite from `planning/harness.json` — stops
immediately if any gating check fails; **(2)** coverage gap scan — reads changed source
files, classifies gaps as adequate/non-blocking/blocking, writes minimal targeted tests for
blocking gaps and re-runs the suite to confirm; **(2.5)** code review — runs a light,
low-overhead `/code-review` check (defaults to `low`, overridden via `--review-level <level>`,
or skipped via `--no-review`); **(3)** `/update-docs --patch`; **(4)** `/handoff` with the
provided note (skips if `--gap-check-only` is set); **(5)** `/clean-worktree` for the current
branch to merge and remove the worktree (only when explicitly requested via `--clean-worktree`).
Non-blocking gaps/findings do not block the pipeline unless critical errors occur.

### `/session-recap`
Start-of-session briefing: reads the three most recent Log entries, status.md, the current
spec's `tasks.md`, and the `reports/` directory listing; outputs a concise briefing (under 300
words) and the exact next command. Read-only.

### `/update-state`
The canonical workflow for hand-editing any repo's `planning/state.json`: the authored-vs-derived
field boundary, which `kind` (`project` / `brain` / `portfolio`) applies and what it requires, the
`<Prefix>.<Phase>.<Letter>` block-ID convention and what has to move in lockstep when an id is
renamed, and the edit → validate → `mev emit-state --write` → `mev validate-brain --state`
procedure. Points to `core/planning/state-schema.md` as the single source of truth for field
shapes rather than duplicating them. Use before any non-trivial `state.json` edit, or when another
command's instructions say "update state.json" without repeating the mechanics.

### `/conditional_docs [task-type]`
Routes the agent to the documentation most relevant to the current task type (feature, bug/fix,
api/endpoint, test/testing, docs/documentation). Reduces CLAUDE.md overload by surfacing only
the files needed for the task at hand. Takes an optional argument; defaults to reading
`planning/context.md` + `planning/status.md` + `planning/harness.json`.

### `/prime`
Orient to this repo at session start: reads `README.md`, `CLAUDE.md`, `planning/context.md`,
`planning/status.md`; runs `git ls-files`; surfaces an active `planning/handoff.md` first if
present; runs a read-only `mev validate-brain --sync` freshness gate (if this repo participates
in a brain) and offers — never auto-runs — `mev emit-state --write` on drift; summarizes the
codebase, layout, focus, carryover, and standing rules. Read-only except for that one
user-confirmed emit. Embedded in every pipeline command.

### `/status`
Reads only `planning/status.md` and reports the Current focus line, what's In progress, and
what's Next. Read-only.

### `/process-tasks`
Reads `status.md`, applies sequential eligibility rules (a spec is ready only if all specs above
it are `Done`), and returns a status table. Read-only.

---

## Phase 1 — Plan

### `/generate-master-plan`
Authors (or revises) `planning/master-plan.md` — the roadmap source of truth — as a sequence of
canonical **block definitions** (`## Phase N` → `### Block X`, each with What / Why / Build notes /
Acceptance criteria) whose phase/block headers `/generate-tasks` can parse directly. Turns a
free-form planning session into the structure the rest of Phase 1 expects. `/new-project` should call
this as its post-scaffold roadmap step. See `planning/decisions/D34-adhoc-planning-seam.md`.

### `/generate-tasks`
Reads the relevant section of `planning/master-plan.md`, writes a full task spec to
`planning/<name>/tasks.md`, and **commits it** (clean tree for downstream `/sdlc-block`).
Each spec carries a **Validation Commands** block and ends with a Validate task.

**`--from <path>` mode** decomposes a single **standalone block file** (e.g. a `/plan` output)
instead of a master-plan block — for ad-hoc / experimental features kept out of the roadmap. It
derives the slug from the file's parent directory and writes `tasks.md` beside the source, then runs
the identical decomposition / pipeline-recommendation logic. The default master-plan slug mode is
unchanged.

### `/breakdown`
Reads a task spec and the source files each step touches, then writes a granular
`breakdown.md` — every sub-step atomic (one file, one change, one command). Both `/implement`
and `/fix` auto-detect this file and use the matching `### Step N:` section as the primary
execution guide (HOW); `tasks.md` stays authoritative for scope (WHAT).

### Pre-planning capture — `/capture`

Before something is ready to plan, use `/capture` to park rich conversation notes without
losing them. Creates `planning/<slug>/notes.md` with a structured scaffold and adds a
pointer ticket to the brain's `planning/backlog.md`.

| Command | Use for | Writes to |
|---|---|---|
| `/capture <title>` | Rich pre-plan notes — detailed enough to need a file, not yet a plan | `planning/<slug>/notes.md` + brain backlog |

The notes file sections (What & Why · Context & Background · Key Information · Open Questions ·
Rough Scope) are designed as direct input to the planning commands below — paste conversation
content in, then promote with `/plan`, `/chore`, or `/generate-master-plan` when ready.

### Ad-hoc planners — `/chore`, `/ticket`, `/plan`

Entry points into Phase 1 for work that **isn't** a master-plan block. Each takes a free-text
description, researches the codebase, and writes a spec into its own `planning/<dir>/` directory.
Output feeds the rest of the pipeline unchanged.

| Command | Use for | Writes to |
|---|---|---|
| `/chore <description>` | Maintenance / housekeeping (no behavior change) | `planning/chore-<slug>/tasks.md` |
| `/ticket <description>` | Bug fix or targeted enhancement that requires tests + observable AC | `planning/ticket-<slug>/tasks.md` |
| `/plan <description>` | Any ad-hoc or experimental feature — mini-roadmap format | `planning/plan-<slug>/plan.md` |

`/chore` and `/ticket` write a runnable `tasks.md` **directly** and route to lean `/sdlc-task`
(the fast path). `/plan` writes a `plan.md` in the **master-plan format** (phases/blocks/Quick
Reference table), so `/sdlc-block` can orchestrate it as a branch train or `/generate-tasks --from
planning/plan-<slug>/plan.md` can decompose a single block into a `tasks.md` → `/sdlc-flow`, all
**without** touching `master-plan.md`. See `planning/decisions/D34-adhoc-planning-seam.md`.

---

## Phase 2 — Implement

### `/implement`
Runs `/prime`, reads the plan file, executes every step (or task N) following CLAUDE.md
conventions, runs the relevant Validation Commands, and writes
`planning/<name>/sdlc/reports/[taskN-]implement.md`.

### `/fix`
Reads the review report to extract every failing criterion, orients via `/prime`, and applies
targeted changes addressing only the failures. Overwrites the `implement.md` slot. Hard-errors
if the review report is absent; soft-stops if the verdict is already PASS.

### `/update-task`
Optionally marks a step done (prepends `[done]`) and/or appends a dated note to the spec's `## Notes`
section. Auto-detects the current spec from status.md if not given. Does not touch status.md.

### `/commit`
Inspects `git status`/`git diff --stat`, chooses a commit strategy (code-only, docs-only, or
both → two commits), drafts a conventional message, and confirms before committing. Never
pushes, never `--no-verify`, never `git add -A`.

---

## Phase 3 — Test

### `/test`
Runs `/prime`, then the project's validation suite (lint, type-check, tests, build, and any
project-specific gates), returning results as a JSON array sorted failed-first. With a spec path,
also writes `planning/<name>/sdlc/reports/[taskN-]test.md`.

> **Stack note:** the test stage runs the checks defined in `planning/harness.json`
> (`validation.checks[]`). The harness ships no stack defaults — define your project's actual
> validation commands there (copy a profile from `planning/harness.examples.md`). If the config
> is absent, the stage falls back to the spec's `## Validation Commands` section.

---

## Phase 4 — Review

### `/review-task`
Runs `/prime`, reads the `implement.md`/`test.md` reports as context, then runs a **fresh test
suite** as authoritative verification. Verdict is PASS only if all criteria are MET **and** the
fresh tests pass. Writes a review report.

---

## Phase 5 — Document

### `/document`
Gates strictly on the review verdict being PASS. Reads the implement report's **Files Created
or Modified** table to scope updates, then surgically patches only affected sections of
`docs/*.md`. Flags architecture-level changes as `NEEDS_REVIEW`. Never touches `planning/`,
`log.md`, `status.md`, or `CLAUDE.md`.

---

## Phase 6 — Wrap-up

### `/log-work`
Reads `status.md`, the current spec, and `log.md`; runs `git diff --stat`. Updates
`status.md` and appends a `log.md` entry. Prompts you to add settled choices to
`planning/decisions/` — never edits decisions directly. Then shells out to
`mev emit-state --write`, the single derivation engine that regenerates every generated
surface from the authored state: this repo's `state.json` focus fields, the brain rollup,
the per-project cache doc's `synced_from` watermark, the tier rollup table, the HQ Operating
Board, and `master-plan.md`'s wave tables. `brain.toml`-driven and depth-agnostic — resolves
the brain root and this repo's manifest entry at runtime, no baked paths. Standalone repos
(no `brain.toml`) skip the brain-sync step entirely.

---

## Phase 7 — Verify Run (Optional)

### `/review-workflow`
Audits a completed `/sdlc-run` pipeline execution — not the implementation, but the mechanics:
report files present and well-formed, the Test stage ran the suite, commits follow conventional
format, Log/STATUS reflect the outcome. Issues PASS/PARTIAL/FAIL and writes
`workflow-review.md`. Does **not** re-run tests — use `/review-task` for that.

---

## Block Setup & Worktree Management

### `/start-block`
Finds the target spec (defaulting to the first non-done spec), checks that all preceding specs
are `Done`, then flips it to `In progress` and updates Current focus + Last updated.

### `/init-worktree` · `/clean-worktree`
Manual entry points for the isolated-worktree lifecycle that `/sdlc-task` and `/sdlc-block`
automate. `/init-worktree` derives a branch/worktree from the spec slug and creates an isolated
sparse checkout; `/clean-worktree` **merges before delete** — fast-forward-merges the branch
into `main`, applies deferred STATUS/Log updates, then removes the worktree. Do **not** run
`/clean-worktree` for `/sdlc-block` tasks — that orchestrator merges each wave for you.

### `/update-docs [--patch] [--since <ref>]`
Documentation health sweep — audits all `docs/` files and `.claude/commands/README.md` against
the current codebase (commands, engine flags, schema fields, new decisions) and recent git
history. Produces a structured gap report: **STALE** sections, **MISSING** coverage, **NO-DOC**
(intentionally undocumented), and **CURRENT** (confirmed). Add `--patch` to apply surgical
fixes for clear-cut stale sections; without it the command is read-only. The un-gated complement
to `/document` — use for periodic doc health checks outside the pipeline.

---

## Company Brain Integration

`/log-work` resolves the brain root from `brain.toml` and shells out to `mev emit-state
--write`, which regenerates this repo's per-project cache doc (`docs/projects/<slug>.md`)
and rollup entries in the parent `agentic-portfolio/` company brain. To run brain-level
commands (briefing, sync-status, log-decision, add-project, log-correspondence), open
Claude Code in the `agentic-portfolio/` root.
