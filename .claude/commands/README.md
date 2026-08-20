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

- [Slash Commands](#slash-commands)
  - [Directory Layout](#directory-layout)
    - [Command Summary](#command-summary)
    - [`brain/` — Reference Only](#brain--reference-only)
    - [`sync-global-commands`](#sync-global-commands)
  - [SDLC Pipeline](#sdlc-pipeline)
    - [Phase Table](#phase-table)
    - [Pipeline Flow](#pipeline-flow)
    - [Session boundaries and models](#session-boundaries-and-models)
    - [Argument Convention](#argument-convention)
    - [Directory Layout](#directory-layout-1)
    - [Run Artifacts](#run-artifacts)
  - [Automated \& Orchestrated Pipelines](#automated--orchestrated-pipelines)
    - [`/sdlc-block` — roadmap orchestration (branch train)](#sdlc-block--roadmap-orchestration-branch-train)
    - [`/review-PR <PR#> [plan-slug]`](#review-pr-pr-plan-slug)
    - [`/merge-train [plan-slug]`](#merge-train-plan-slug)
    - [`/orchestrate <block-id ...> | <list-file>`](#orchestrate-block-id---list-file)
    - [`/begin-orchestration --roadmap <path> (--lane <name|path> | --blocks <id ...>)`](#begin-orchestration---roadmap-path---lane-namepath----blocks-id-)
    - [`/begin-session <session-slug> [--roadmap <path>] [--dry-run]`](#begin-session-session-slug---roadmap-path---dry-run)
    - [`/consolidate-run <roadmap-slug> [--repo <slug>]`](#consolidate-run-roadmap-slug---repo-slug)
    - [`/roadmap-status --roadmap <slug>`](#roadmap-status---roadmap-slug)
  - [Session Orientation](#session-orientation)
    - [`/wrap-up [note]`](#wrap-up-note)
    - [`/handoff [note]`](#handoff-note)
    - [`/close-out [--base <ref>] [--gap-check-only] [--skip-coverage] [--clean-worktree | --merge-branch] [note]`](#close-out---base-ref---gap-check-only---skip-coverage---clean-worktree----merge-branch-note)
    - [`/session-recap`](#session-recap)
    - [`/update-state`](#update-state)
    - [`/conditional_docs [task-type]`](#conditional_docs-task-type)
    - [`/prime`](#prime)
    - [`/next`](#next)
    - [`/process-tasks`](#process-tasks)
  - [Phase 0 — Pre-plan](#phase-0--pre-plan)
    - [`/assess`](#assess)
    - [`/seams`](#seams)
    - [`/sequence`](#sequence)
    - [`/define-design-system` — greenfield UI](#define-design-system--greenfield-ui)
    - [`/define-polish-standard` — existing UI](#define-polish-standard--existing-ui)
  - [Phase 1 — Plan](#phase-1--plan)
    - [`/generate-roadmap <slug> [--from <path> ...] [--supersedes <path>]`](#generate-roadmap-slug---from-path----supersedes-path)
    - [`/generate-master-plan` — superseded (D65)](#generate-master-plan--superseded-d65)
    - [`/generate-tasks`](#generate-tasks)
    - [`/breakdown`](#breakdown)
    - [Pre-planning capture — `/capture`](#pre-planning-capture--capture)
    - [Ad-hoc planners — `/chore`, `/ticket`, `/plan`](#ad-hoc-planners--chore-ticket-plan)
  - [Phase 2 — Implement](#phase-2--implement)
    - [`/implement`](#implement)
    - [`/fix`](#fix)
    - [`/update-task`](#update-task)
    - [`/commit`](#commit)
  - [Phase 3 — Test](#phase-3--test)
    - [`/test`](#test)
  - [Phase 4 — Review](#phase-4--review)
    - [`/review-task`](#review-task)
  - [Phase 5 — Document](#phase-5--document)
    - [`/document`](#document)
  - [Phase 6 — Wrap-up](#phase-6--wrap-up)
    - [`/log-work`](#log-work)
  - [Block Setup \& Worktree Management](#block-setup--worktree-management)
    - [`/start-block`](#start-block)
    - [`/init-worktree` · `/clean-worktree`](#init-worktree--clean-worktree)
    - [`/update-docs [--patch] [--since <ref>]`](#update-docs---patch---since-ref)
  - [Company Brain Integration](#company-brain-integration)


## Directory Layout

All commands live directly in `.claude/commands/` — no subdirectories (except `brain/`).
`sync-global-commands` installs all non-brain commands into `~/.claude/commands/`.

```
.claude/commands/
  README.md                        ← this file
  sync-global-commands.md          ← syncs all non-brain commands to ~/.claude/commands/
  e2e-templates-README.md          ← usage guide for the e2e test templates

  archive.md        capture.md       commit.md        handoff.md
  log-work.md       prime.md         session-recap.md
  wrap-up.md        update-state.md  next.md

  assess.md         seams.md         sequence.md
  define-design-system.md            define-polish-standard.md

  breakdown.md      chore.md         generate-master-plan.md  generate-tasks.md
  generate-roadmap.md  plan.md       ticket.md

  close-out.md      conditional_docs.md  document.md      fix.md
  implement.md      patch.md             process-tasks.md review-PR.md
  review-task.md    test.md              update-docs.md
  update-task.md

  clean-worktree.md  init-worktree.md  merge-train.md  start-block.md

  test_auth_gate.md  test_crud_api.md  test_error_handling.md  test_ui_form.md

  brain/                           ← reference only; NEVER synced to ~/.claude/commands/
    (flat — same filenames as brain's own .claude/commands/)
```

### Command Summary

| Group | Commands |
|---|---|
| Session | `/prime`, `/session-recap`, `/next`, `/handoff`, `/wrap-up`, `/log-work`, `/archive`, `/capture` |
| State | `/update-state` — how to safely edit `planning/state.json` per `state-schema.md` |
| Pre-plan | `/assess`, `/seams`, `/sequence` |
| UI foundations | `/define-design-system` (greenfield), `/define-polish-standard` (existing UI) |
| Planning | `/generate-roadmap`, `/generate-tasks`, `/plan`, `/ticket`, `/chore`, `/breakdown` (`/generate-master-plan` is superseded by `/plan --founding`, D65) |
| SDLC | `/implement`, `/test`, `/fix`, `/patch`, `/document`, `/update-docs`, `/conditional_docs`, `/process-tasks`, `/update-task`, `/review-task`, `/review-PR`, `/close-out` |
| Git | `/commit`, `/init-worktree`, `/clean-worktree`, `/start-block`, `/merge-train` |
| Orchestration | `/orchestrate`, `/begin-orchestration`, `/begin-session`, `/consolidate-run`, `/roadmap-status` |
| E2E | `/test_auth_gate`, `/test_crud_api`, `/test_error_handling`, `/test_ui_form` |
| Backlog | `/backlog-ticket`, `/initial-research` |
| Distribution | `/sync-downstream-harness`, `/sync-all`, `/sync-global-commands`, `/sync-global-skills`, `/sync-brain-skills` |

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
| Session Start | `/process-tasks` | Check which specs are eligible to start | chat only |
| Session Start | `/next` | Briefing on what's up next, blocked, and recommend next action based on goals | chat only |
| Session End | `/wrap-up [note]` | Log work + commit; clean close without a handoff file | status.md, log.md, git |
| Session End | `/handoff [note]` | Write handoff + log work + commit; hands off to a fresh session | `planning/handoff.md`, status.md, log.md, git |
| Session End | `/close-out [--base <ref>] [--gap-check-only] [--skip-coverage] [--clean-worktree \| --merge-branch] [note]` | Resolve diff base (loud-fail if none) → verify coverage → patch docs → clean worktree/merge branch (opt.) → hand off; the quality-close pipeline | status.md, log.md, docs/, git |
| Block Setup | `/start-block [name]` | Flip a spec to `In progress` in status.md | status.md |
| **0 — Pre-plan** | `/assess <topic>` | Fan out fresh recon agents, then a second fresh set to re-check the load-bearing claims | `planning/<slug>/assessment.md` · `verification.md` · `evidence/` |
| **0 — Pre-plan** | `/seams <slug>` | Built/half-built/absent, attachment points with one writer per side, blast radius, one spike, the operator's forks | `planning/<slug>/seams.md` |
| **0 — Pre-plan** | `/sequence <slug>` | Cut into ordered blocks that each ship something usable; owning repo per block | `planning/<slug>/sequence.md` |
| **0 — UI (greenfield)** | `/define-design-system <desc> --surface <kind>` | Tokens, components and rules a new UI is built from; proved by building one real screen | `planning/<slug>/design-system.md` + emitted token/theme/component files |
| **0 — UI (existing)** | `/define-polish-standard <desc> --surface <kind>` | The falsifiable standard a UI is judged against, calibrated until two reviewers agree | `planning/<slug>/polish-standard.md` |
| **1 — Roadmap** | `/plan --founding` | A new project's founding roadmap — block records, not a hand-written master plan | `planning/founding/plan.md` + `planning/blocks/*.json` |
| **1 — Plan** | `/generate-tasks <name>` · `/generate-tasks --from <path>` | Write the full task spec from a master-plan block, **or** from a standalone block file (`--from`) | `planning/<name>/tasks.md` |
| **1 — Plan (ad-hoc)** | `/chore` · `/ticket` · `/plan <desc>` | Plan ad-hoc work from a free-text description (not a roadmap block). `/ticket` reproduces the failure first and orders the test before the fix; `/chore` takes a pre-change gate baseline | `planning/blocks/<BlockID>.json` + `planning/<BlockID>/tasks.json` — or `planning/<slug>/plan.md` for `/plan` |
| **1 — Plan (opt.)** | `/breakdown <spec>` | Decompose spec into atomic, agent-executable sub-steps | `planning/<name>/breakdown.md` |
| **2 — Implement** | `/implement <spec> [N]` | Execute every task (or task N) in the spec | `sdlc/state.json` + `sdlc/worklog.md` |
| **2 — Hotfix** | `/patch` | Implement → validate → commit for low-risk single-file fixes; skips test/review/document | git history |
| **2 — Fix** | `/fix <spec> [N]` | Targeted fixes for FAIL/PARTIAL verdict; reads review worklog entry; appends a fix-pass entry | `sdlc/state.json` + `sdlc/worklog.md` |
| **2 — Track** | `/update-task [name] <step> [note]` | Mark a step done and/or append a dated note mid-implementation | spec file (in-place) |
| **2 — Commit** | `/commit [hint]` | Stage + commit with a conventional message | git history |
| **3 — Test** | `/test <spec> [N]` | Run the project's validation suite; write snapshot | `sdlc/state.json` + `sdlc/worklog.md` |
| **4 — Review** | `/review-task <spec> [N]` | Verify all criteria; run fresh tests; issue verdict | `sdlc/state.json` + `sdlc/worklog.md` |
| **5 — Document** | `/document <spec> [N]` | Surgically patch `docs/`; gates on PASS verdict | `sdlc/state.json` + `sdlc/worklog.md` |
| **6 — Wrap-up** | `/log-work [notes]` | Update status.md + append Log entry + sync company brain | status.md, log.md, brain `docs/projects/<slug>.md`, brain `README.md` |

### Pipeline Flow

```
SESSION START
  /session-recap            → read-only: recent log, current focus, next action
  /process-tasks           → read-only: which specs are eligible

BLOCK SETUP
  /start-block <spec>      → status.md

UI FOUNDATIONS             ← only when the work has a visual surface. Beside the pipeline.
  new UI, nothing to read  → /define-design-system  → design-system.md + tokens/theme/components
                                                      (proved by building one real screen)
                                     ↓ emits
  existing UI              → /define-polish-standard → polish-standard.md
                                                      (calibrated: 2 agents, 1 screenshot, agree)
  A new page in an existing app needs neither — it inherits both.
        ↓ polish-standard.md is read by /assess's polish scout, or a UI /ticket's AC

PHASE 0 — PRE-PLAN         ← existing system, cut not obvious. Skip for a known block.
                           ← "|" = fresh session required.  "·" = same session continues.
  /assess <topic>          → planning/<slug>/assessment.md + verification.md + evidence/
      | fresh — /seams must be free to refute those classifications
  /seams <slug>            → planning/<slug>/seams.md      (+ the operator answers its forks)
      · same session
  /sequence <slug>         → planning/<slug>/sequence.md
      | fresh — a fresh reader of sequence.md IS the handoff test
        ↓
  one repo   → /plan             → planning/<slug>/plan.md + planning/blocks/*.json
  many repos → /generate-roadmap --from planning/<slug>/sequence.md
                                 → planning/roadmaps/<slug>/{roadmap.md,lane-*.txt,lane-log.jsonl}
      | fresh, ONE PER LANE, held open for that lane's whole chain
                                 → /begin-orchestration --roadmap ... --lane ... → /orchestrate

PHASE 1 — PLAN             ← fresh session, ONE PER BLOCK
  /generate-tasks <spec>                 → planning/<spec>/tasks.json (+ rendered tasks.md)
        ↓  (optional — only if a task is genuinely coarse; same session)
  /breakdown planning/<spec>/tasks.md   → planning/<spec>/breakdown.md
                                           (+ any executable correction written back to tasks.json)
      | fresh — the engine runs in its own session

PHASE 2 — IMPLEMENT
  /implement planning/<spec>/tasks.md [N]
        → planning/<spec>/sdlc/state.json + sdlc/worklog.md
  (/update-task and /commit can be called any number of times during this phase)

PHASE 3 — TEST
  /test planning/<spec>/tasks.md [N]
        → planning/<spec>/sdlc/state.json + sdlc/worklog.md

PHASE 4 — REVIEW                   ← runs fresh tests; verdict gates next step
  /review-task planning/<spec>/tasks.md [N]
        → planning/<spec>/sdlc/state.json + sdlc/worklog.md

        if PASS → continue to PHASE 5 — DOCUMENT
        if FAIL/PARTIAL → PHASE 2 — FIX:
  /fix planning/<spec>/tasks.md [N]
        → planning/<spec>/sdlc/state.json + sdlc/worklog.md  (appends a fix-pass entry)
  then repeat: /test [N] → /review-task [N] until PASS

PHASE 5 — DOCUMENT                 ← gates on PASS verdict
  /document planning/<spec>/tasks.md [N]
        → planning/<spec>/sdlc/state.json + sdlc/worklog.md

PHASE 6 — WRAP-UP
  /log-work [notes]        → status.md, log.md
```

### Session boundaries and models

Every command in the planning chain ends with a **Session boundary** section telling the agent what
to report to the operator on close. The map:

| Command | Session | Model |
|---|---|---|
| `/assess` | own; ends | Opus main · Sonnet scouts + verifiers |
| `/seams` | continues into `/sequence` | Opus · Opus red team |
| `/sequence` | own; **ends hard** | Opus · Opus red team |
| `/define-design-system` | own; the first feature runs fresh | Opus |
| `/define-polish-standard` | own; calibration subagents spawned from it | Opus |
| `/plan` | fresh; ends | Opus |
| `/generate-roadmap` | fresh; ends | Opus |
| `/generate-tasks` | fresh, **one per block** | Sonnet (Opus for breaking-surface blocks) |
| `/breakdown` | with `/generate-tasks` | Sonnet |
| `/chore` · `/ticket` | one session — authors **and** decomposes; the engine runs fresh | Sonnet; Opus for a subtle `/ticket` |
| `/capture` | inline in whatever session found the thing — **never a subagent** | whatever is already running |
| `/begin-orchestration` | fresh, **one per lane**, held open for the chain | Opus |
| `/sdlc-task` · `/sdlc-flow` | fresh | per-engine — the engines tier their own internal agents (Sonnet on mechanical stages, Opus escalation on hard retries) |

**Fresh** where the next step must be able to disagree with this one, or must prove an artifact
stands alone. **Continuous** where the work is one sustained act of judgement. The `/sequence` →
`/plan` \| `/generate-roadmap` break is the load-bearing one: a fresh session reading only
`sequence.md` *is* the handoff test, performed rather than imagined.

Model tier tracks **breadth held at once**, not importance — which is why `/generate-tasks`, the
most rule-dense command in the harness, is correctly Sonnet: it reads one block record and the files
that record names. Escalate it only when the block carries breaking public-surface changes or
several un-gateable criteria.

Full rationale: `docs/how-to-plan-with-agents.md` §11 in the brain repo.

### Argument Convention

Every step from Phase 2 onward takes the same form: `planning/<BlockID>/tasks.md [N]`

That path is what the engines hand their agents as the spec document (`sdlc-task.js` and
`sdlc-flow.js` both set `specFile = <blockDir>/tasks.md`). It is **not** what determines the work —
the task loop is enumerated from `tasks.json`, and a missing or unparseable one hard-aborts the run
(D16).

Split on the last space. Trailing number = task N (scope to that task only). No number = full
spec. Use the **same `N`** throughout the pipeline — under the hand-invoked Phase 2–5 commands
it determines which `tasks["<N>"]` entry in `sdlc/state.json` and which `## Task <N> — ...`
worklog section this run reads and writes (see Run Artifacts below).

### Directory Layout

Each block gets its own directory under `planning/`, named exactly for its block ID. The block
**record** lives separately in `planning/blocks/`, because it is the planning unit and outlives any
one run (D65).

```
planning/
  blocks/
    <BlockID>.json    <- the block record: what/why/files/out_of_scope/AC. The authored unit.
  <BlockID>/
    tasks.json        <- THE EXECUTED TASK LIST. A bare array. Every engine reads this.
    tasks.md          <- GENERATED from the record by scripts/render_spec.py. Never hand-edit.
    breakdown.md      <- optional (written by /breakdown)
    sdlc/
      sdlc-<engine>-state.json   <- authoritative run state for /sdlc-task or /sdlc-flow, committed
      state.json                 <- run state for the hand-invoked Phase 2-5 commands, write-only (not committed)
      worklog.md                 <- human-readable trail, one section per task (D31); shared by engines and hand-invoked commands
      reports/                   <- gate baselines only (<slug>-baseline.json, <slug>-skip-baseline.txt)
```

**The two files people confuse.** `tasks.json` is what runs — a bare array, never a
`{"tasks": [...]}` wrapper. `tasks.md` is a rendered view of the block record that the engines hand
their agents as the spec document; editing it by hand is silently discarded on the next render.
Change the record and re-render. `BT.ticket.engines-read-block-record` retires `tasks.md` entirely
once the engines read the record directly.

### Run Artifacts

**No command in the pipeline writes a per-step prose report.** D31 replaced the old 5xN report
files with one run-state file plus one worklog for `/sdlc-flow` and `/sdlc-task` — `sdlc-flow.js`'s
own header says so — and the five hand-invoked Phase 2-5 commands (`/implement`, `/fix`, `/test`,
`/review-task`, `/document`) now follow the same shape instead of the older `sdlc/reports/`
convention. `sdlc/reports/` survives only for gate baselines (`<slug>-baseline.json`,
`<slug>-skip-baseline.txt`), never step output.

Each of the five commands, on every call:

1. Reads `planning/<BlockID>/sdlc/state.json` (starts from `{}` if absent) and preserves every
   field it isn't updating — the file accumulates across `/implement` → `/test` → `/review-task` →
   `/fix` → `/document` calls on the same spec.
2. Updates that file's `tasks["<N>"]` entry (or the spec-wide fields, for a full run) with its
   outcome — status, attempts, files touched, commit hash, and (for `/review-task`) the verdict.
3. Appends one section to `planning/<BlockID>/sdlc/worklog.md` — `## Task <N> — IMPLEMENTED`,
   `TEST`, `REVIEW`, `FIX`, or `DOCUMENT` — a few key:value lines, never a narrative. `/fix` appends
   a new fix-pass section; it does not overwrite the prior one.

Both files are **write-only artifacts**: read back off disk by the next command in the chain,
never `git add`/`git commit`ed — `planning/` is a symlink into a brain vault in a vaulted repo, so
committing under it can fail "beyond a symbolic link" (same D46 reasoning the engines use). The
actual code/test changes still get a real git commit per the spec's own instructions.


## Automated & Orchestrated Pipelines

The manual Phase 1 → 7 commands above can be run end-to-end by automated workflows
(`workflows/*.js`). Invoke them like slash commands. Each runs the same pipeline stages, but
unattended.

| Workflow | Scope | Isolation |
|---|---|---|
| `/sdlc-task <name> N` | **one** task, parallel-safe | own git worktree; defers STATUS/Log to merge time |
| `/sdlc-flow <name> [range]` | a **full spec** on one shared branch, per-task test→fix loop, one end review, a PR | plain branch in the main tree (or `--worktree` for isolation); terminates in a PR |

> **Full reference with mermaid diagrams, per-stage detail, and token usage:**
> [`docs/workflows/`](../../docs/workflows/index.md) — one page per engine plus the manual lifecycle.

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

### `/orchestrate <block-id ...> | <list-file>`
Drives an **ordered chain of blocks** through the SDLC engines in one session: spec → (breakdown) →
`/sdlc-task` or `/sdlc-flow` → integrate → verify the state write → next. Engines run as background
workflows, so specs for later blocks are prepared while an earlier one builds. **One repo per session,
one engine run at a time** — several repos run concurrently as separate sessions (the lane model).
Ten standing rules, each from a real failure: never do block work yourself or via a subagent; verify
every state write (engine bookkeeping is known-unreliable); check downstream Cargo consumers after a
shared-crate change; commit after every `mev` command; report each block to the lane log (resolved
per `/begin-orchestration`'s rule — `planning/roadmaps/<slug>/`, else legacy `planning/<slug>/`); keep a
`planning/orchestration-run/notes.md` running tab; and resolve ordinary ambiguities inline while
recording the call. Flags: `--worktree` / `--no-worktree`, `--engine task|flow`, `--dry-run`,
`--continue-on-fail`.

### `/begin-orchestration --roadmap <path> (--lane <name|path> | --blocks <id ...>)`
Wraps `/orchestrate` with the context a lane agent needs and the rules a **concurrent** run depends
on: which chain, why, what may not be delegated, and who else is running. Resolves `BRAIN_ROOT`, the
repo, the roadmap and the lane file (cross-checking the lane's `# ROADMAP:` header against the one
given), then applies the isolation policy — `base-template` is always `--worktree` (a chain there
edits the engines running it), the brain root is always `--no-worktree` (corpus gates cannot pass in
a worktree) — before handing off. `--roadmap` is **required and never inferred**. Also enforces the
heavy-gate concurrency cap, operator gates, and the same notes-file and decision-recording rules.
At lane close it routes every still-`OPEN` lingering item to one of **three** homes rather than
sweeping them all into `carryover[]`: operator-only work becomes an `operator` edge on the block it
gates, permanently-true facts go to `reference[]`, and the rest becomes a `carryover[]` entry. This
is the fleet's highest-volume misfiling point — a closing lane promotes four to six items at once.

### `/begin-session <session-slug> [--roadmap <path>] [--dry-run]`
Drives one **operator session** — the unit for work an agent cannot do alone: a decision, a
credential, a judgement call. `/orchestrate` runs what an agent can do; this runs what it cannot.
Resolves the slug from `state.json` `depends_on` edges of type `operator` (the real home now that
`okf-core:OK.ticket.operator-edge-types` has landed — `{"type":"session"}` is a hard parse error),
else a roadmap's Wave 0 session table, else a `/capture` note — and reports **every** block the
session gates, with effective priority, since a session gating a P0 block *is* P0. Stops if you are
in the wrong repo, and groups every step needing another machine (the Mac Mini) into one sitting
rather than three. Closes **only** when the named exit artifact exists:
`mev close-operator-gate <slug> --exit-verified`, the operator asserting it — mev never infers it.
A session marked done without its artifact is worse than one never started, because the gate is
gone and the work is not.

### `/consolidate-run <roadmap-slug> [--repo <slug>]`
Gathers `orchestration-run/<roadmap-slug>/` records across the fleet (or one repo with `--repo`),
cross-checks them against `lane-log.jsonl` in both directions, selects on D57 section 3's two-axis
`origin_roadmap` rule, and emits `<roadmap-dir>/consolidated-review.md` — one proposed `carryover[]`
entry per finding, each carrying a `finding_id` for mev's cross-repo correlation. It implements no
dedup, similarity, ranking, or staleness logic of its own (that's mev's — `mev carryover`); it never
auto-merges, and it writes no `state.json` anywhere. `/generate-roadmap --from <consolidated-review.md>`
is the disposal path for what it proposes.

### `/roadmap-status --roadmap <slug>`
Read-only, mid-run view of one roadmap's live lanes across every repo — joins the roadmap's
`lane-log.jsonl`, `orchestration-run/<roadmap-slug>/{notes.md,review.md}`, per-spec
`sdlc/sdlc-*state.json`, and each repo's `state.json` (via `scripts/roadmap_status_discovery.py`),
then reports, in D57 section 6 order: what needs the operator now, what is running or recently
finished, what stopped and why. With `--roadmap` omitted it lists candidate roadmaps and stops
rather than guessing. Liveness comes from `updated_at` against a named staleness threshold, never
from the seven-value `status` field alone; unknown status values pass through verbatim. It
implements no ranking, dedup, or staleness scoring of its own (that's mev's, same discipline as
`/consolidate-run`) and writes nothing — no `state.json`, no record, no lock. Not `/attention`
(fleet-wide staleness triage), not `/next` (what to work on), not `/consolidate-run` (post-run
finding correlation) — this answers what a roadmap's lanes are doing right now.

---

## Session Orientation

### `/wrap-up [note]`
Clean session close without a handoff. Routes anything durable to one of **three** homes before
writing anything — operator-only work to an `operator` edge, permanently-true facts to
`reference[]`, and only what is left to `carryover[]` (full twelve-field shape — `slug`, `scope`,
`kind`, `text`, `related?`, `clears_when?`, `priority?`, `blocks?`, `finding_id?`, `created`,
`reviewed?`, `snoozed_until?`; `kind` is one of `defect` / `deferred` / `drift` / `env` per HQ D72,
and only entries with a typed `clears_when` predicate are machine-evaluable by `mev carryover`).
Then runs `/log-work` (syncs status.md + appends log entry) and `/commit`. Use this when you're done with a piece of work and don't need to hand off to a fresh
agent.

### `/handoff [note]`
Session end-of-context handoff. Writes `planning/handoff.md` (what's in flight, completed,
remaining, first command for the next agent), then invokes `/log-work` and `/commit`.
`/prime` in the next session detects the handoff file and surfaces it first. Delete
`planning/handoff.md` once the new session has consumed it. Drains durable caveats into
`carryover[]`'s full twelve-field shape (see `docs/state/state-schema.md`) — `priority`,
`blocks[]`, `finding_id`, and the four typed `clears_when` predicates
(`block_closed` / `file_exists` / `file_contains` / `command_exits_zero`) are what make an entry
rankable, cross-repo-correlatable, and machine-evaluable by `mev carryover`; a prose `clears_when`
lands it in the not-evaluable lane instead. `kind` is one of `defect` / `deferred` / `drift` / `env`
(HQ D72 — `constraint` and `known_issue` are retired). **Step 2b routes to three destinations, not
two: anything left for the operator to decide, review, approve, or judge is filed as an `operator`
(or `approval`) edge in `depends_on` — never as a `carryover[]` entry, and never written
into the handoff's `## Open questions / choices` section as prose** — that section now names the
slugs already filed, it does not hold decisions itself.

### `/close-out [--base <ref>] [--gap-check-only] [--skip-coverage] [--clean-worktree | --merge-branch] [note]`
Quality-close pipeline for the end of an `sdlc-flow` or `sdlc-task` session. Runs **(0.5)**
diff-base resolution before anything else: the emoji gate and the coverage sweep must scope to
the **same** base, resolved from real evidence — an explicit `--base <ref>`, else
`planning/harness.json`'s `flow.prBase`, else `origin/HEAD`, else a local `main`/`master` — never
a hard-coded literal. If `HEAD` **is** the resolved base (the default state after an in-place run,
a plain-branch run, or right after `--auto-merge`/`--merge-branch` land), a two-dot/three-dot diff
against it is empty by definition; close-out falls back to the merge commit's first parent
(`HEAD^1..HEAD`) when one exists, and otherwise **refuses to run** rather than report a vacuous
clean — it names the resolved base, tells you to pass `--base <ref>`, or to run before the branch
merges. Then runs four steps in sequence: **(1)** the full validation suite from
`planning/harness.json` — stops immediately if any gating check fails — then the emoji gate,
scoped to the resolved range; **(2)** coverage gap scan — reads changed source files from the
**same** resolved range, classifies gaps as adequate/non-blocking/blocking, writes minimal
targeted tests for blocking gaps and re-runs the suite to confirm; **(3)** `/update-docs --patch`;
**(4)** `/handoff` with the provided note (skips if `--gap-check-only` is set); **(5)**
`/clean-worktree` for the current branch to merge and remove the **worktree** (only when
explicitly requested via `--clean-worktree`); **(5b)** merge the current **plain branch** into
the base + `mev emit-state --write` (only via `--merge-branch` — the branch-mode `/sdlc-flow`
analogue, no worktree to remove; mutually exclusive with `--clean-worktree`). Non-blocking
gaps do not block the pipeline. Code review is **not** part of this pipeline — `/code-review`
cannot be invoked from a slash command; run it yourself when you want one.

### `/session-recap`
Start-of-session briefing: reads the three most recent Log entries, status.md, the current
spec's `tasks.md`, and the `reports/` directory listing; outputs a concise briefing (under 300
words) and the exact next command. Read-only.

### `/update-state`
The canonical workflow for hand-editing any repo's `planning/state.json`: the authored-vs-derived
field boundary, which `kind` (`project` / `brain` / `portfolio`) applies and what it requires, the
`<Prefix>.<Phase>.<Letter>` block-ID convention and what has to move in lockstep when an id is
renamed, and the edit → validate → `mev emit-state --write` → `mev validate-brain --state`
procedure. Points to `docs/state/state-schema.md` as the single source of truth for field
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

### `/next`
Show what's up next, what's blocked and by what, and recommend the next action based on local status and HQ/business/core goals. Read-only.

### `/process-tasks`
Reads `status.md`, applies sequential eligibility rules (a spec is ready only if all specs above
it are `Done`), and returns a status table. Read-only.

---

## Phase 0 — Pre-plan

For work on an **existing** system where the cut is not obvious. Three commands, each writing one
artifact that the next reads; `/sequence`'s output is the only input `/plan` or `/generate-roadmap`
needs. Skip all three when you already know the block — go straight to `/ticket`, `/chore`, or
`/generate-tasks`.

The method behind them is `docs/how-to-plan-with-agents.md` in the brain repo — §1 for the arc,
§8 for judging work a test suite cannot check, §11 for sessions and models. Its §1 states the
whole arc as **six phases named by the question each answers** — assess, seams, sequence, author,
decompose, evaluate — independent of these commands, which are one implementation of it. The rule
that makes the phases work is that **each is forbidden from doing the next one's job**: an
assessment may not propose a sequence, a seam map may not cut blocks, a sequence may not assign
concurrency, an author may not re-cut. The operator decides at exactly two points, both blocking:
the forks (after seams) and the cut (after sequence).

**Phase 0 is optional; its floor is not.** `/plan` and `/generate-roadmap` both run fine with no
pre-plan folder — short planning sessions should skip straight to them. But each carries a small
inline **floor** for when Phase 0 was skipped: classify every capability the work *calls* rather
than builds as built / half-built / absent, name the blast radius of each new attachment point, say
what is deleted first, and (for a roadmap) name the single writer of every artifact two lanes touch.
Answer them in proportion to the work — a sentence each for two blocks, a paragraph each for six.

Each floor carries an **escalation trigger**: if the half-built question cannot be answered for
something on the critical path, the command stops and recommends `/assess` rather than planning over
the gap. That failure is invisible afterwards — it surfaces as a block sized as wiring that turns
out to be a rewrite.

| Command | Writes | Answers |
|---|---|---|
| `/assess <topic> [--slug <name>] [--areas "..."] [--depth quick\|standard\|deep]` | `planning/<slug>/assessment.md` · `verification.md` · `evidence/` | What is actually there, with proof, and which claims survived re-checking |
| `/seams <slug> [--spike <n>]` | `planning/<slug>/seams.md` | Built / half-built / absent; what to reuse; what to delete; blast radius; the forks the operator must decide |
| `/sequence <slug> [--single-repo]` | `planning/<slug>/sequence.md` | The cut into blocks, each shipping something usable, with owning repo and cross-repo contract author |
| `/define-design-system <desc> --surface <kind>` | `planning/<slug>/design-system.md` + token/theme/component files | What a **new** UI is built from — tokens, a justified component inventory, the rules |
| `/define-polish-standard <desc> --surface <kind>` | `planning/<slug>/polish-standard.md` | What "polished" means for an **existing** product, in items that can be failed by looking |

### `/assess`
Establishes ground truth first — builds, runs the gated checks, and **runs the subsystem once** if
it can be run — then sweeps the fleet's own `carryover[]` / `decisions/` / `knowledge.md` /
`memory.md` / `backlog.md` before spawning any scout, so nothing already filed is re-discovered as
novel. Scouts get fresh context and narrow briefs, report **findings with `file:line` + symbol and
never recommendations**, and always include a reuse scout and a deletion scout. A second, fresh set
of agents then re-checks the load-bearing claims — **given claims, not conclusions** — and anything
refuted is corrected **in `assessment.md` itself**, not only noted in `verification.md`.

It produces evidence only. It may not propose a sequence, a wave, a block, or an estimate.

**Session:** own session, ends with `/handoff`. **Model:** Opus main, Sonnet scouts and verifiers.
`/seams` must run fresh so it is free to refute this document's classifications.

### `/seams`
The stage most often skipped and the one whose absence most reliably produces a plan that is
coherent on paper and unbuildable in practice. Classifies every capability the work depends on as
**built** (has a production call site), **half-built** (exists in source but has no caller, is
behind a disabled flag, or was never run), or **absent** — half-built is where plans die, and the
classification decides whether each is a wiring block or a rewrite. Then: the seam list with a
**single named writer per side**, a blast radius per seam, one spike on the riskiest assumption
(a smoke run of an existing path counts, and is always cheapest), the cross-cutting walk
(migrations · flags · observability · error paths · auth · perf · concurrency · **install/deploy
boundary** · rollback), and 2–4 forks stated as options plus a recommendation.

**Session:** normally continues into `/sequence` — one sustained act of judgement, and the red team
is fresh subagents anyway. Split only if the forks will take days to answer or the session is
already long. **Model:** Opus, Opus red team.

### `/sequence`
Cuts by **deliverable, not by layer**. Every block is tested against *"what can the operator do the
day this merges that they could not do the day before?"* — a block whose answer is "nothing yet" is
merged into the block that consumes it. Blocks that make later work observable outrank blocks that
add capability; deletions come before the extensions that would inherit them. Operator errands
(a credential, a machine visit, a decision) are first-class blocks, not prose asides. Fork answers
are recorded with dates before the cut, because an unresolved fork gets decided silently by
whichever agent hits it first.

**Session:** ends hard — `/plan` and `/generate-roadmap` always run fresh. This is the load-bearing
break in the chain: a fresh session reading only `sequence.md` *is* the handoff test, performed
rather than imagined. **Model:** Opus, Opus red team.

### `/define-design-system` — greenfield UI
For a UI that **does not exist yet**: a new client project, a new side project, a new app. Emits the
artifacts the first screen is built from — design tokens as real files, a Tailwind or `ThemeData`
config, a justified component inventory, the icon set — plus the rules that keep screen twenty
consistent with screen one.

Two things keep it honest. It **starts from the practice's settled stack** — Next 16 + React 19 +
Tailwind 4 with CSS variables + **shadcn/ui** (`style: base-nova`, `baseColor: neutral`) + `lucide`,
as built in `business/bastiel` and `client/jardins-fitness`, with the reference `components.json`
inlined so a new project starts identical. Hand-rolling is the departure and needs a reason.

Note *how* that precedent is chosen, because it generalises: **a pattern found in a codebase tells
you what happened, not what was intended.** `learn-ai` and `bastion-web` are the largest frontends
and are explicitly *not* the reference — one predates the practice's design-system discipline, the
other deferred the decision and retrofitted a system later. The command says to ask the operator
which projects are exemplary rather than infer a house standard from a majority, because inferring
propagates an old mistake with a survey as its evidence.

And **tokens come before components**, because a component that hardcodes a colour cannot be
retuned.

**The gate is building one real screen with it** — not a swatch page or a component gallery. Reaching
for a value that is not in the scale means the scale is wrong; needing a component that is not in the
inventory means the inventory is wrong or the screen is. What that forces gets folded back and
recorded. A system never used to build anything is a wish.

Component inventory is deliberately short: an entry earns its place only by appearing on ≥2 real
screens or being a state container. It ends by emitting a polish standard, so the review path
converges with the command below. **Escalation trigger:** if the product already has a discernible
system, it stops — codify it with `/define-polish-standard` instead; a second system is a rewrite.

> **A new page in an existing app needs neither command.** It inherits the system and is reviewed
> against the existing standard. Both of these are for the uncommon case.

### `/define-polish-standard` — existing UI
Runs **beside** the pipeline rather than in it, whenever the work involves a UI — web, mobile, TUI
or desktop. "Clean and polished" cannot be assessed against nothing: without a written standard you
get opinions, opinions differ between reviewers, and opinions do not become blocks of work.

It looks at the running product first (a standard written from general UI knowledge describes some
other product), **derives the existing system rather than inventing one** — codifying the spacing
and type scales the code already uses, cited to source — and writes items that can be **failed by
looking**, each with a verdict procedure. "Spacing is consistent" is not an item; "vertical gaps are
multiples of 8px, flag any that is not" is. It covers the five states where products are actually
unpolished — loading, empty, error, offline, permission-denied — and carries a real Out of Scope
section so a polish review cannot sprawl into a redesign.

**Then it calibrates, and that step is the gate:** two fresh agents, one screenshot, compared item
by item. Any item they read differently gets tightened and both re-run. An item that will not
converge after two rounds is taste, not a standard — it moves to Out of Scope or escalates to the
operator. **Escalation trigger:** if the product has no discernible existing system at all, it stops
and says so, because a standard written over that gap fails every screen, which is discouraging
rather than actionable — that is a design decision or a `/ticket`, not a polish pass.

Feed the result to `/assess`'s polish scout, or use it as the acceptance-criteria source for a UI
`/ticket`. **Session:** one session, calibration subagents spawned from it; whatever reviews the UI
runs fresh. **Model:** Opus — judging consistency across a whole product's screens is exactly the
breadth-held-at-once case.

---

## Phase 1 — Plan

### `/generate-roadmap <slug> [--from <path> ...] [--supersedes <path>]`
**Pre-plan input (Step 1b).** A `sequence.md` passed via `--from` is handled differently from every
other source: it is an **authored cut**, not a body of findings, so it is carried through rather
than re-derived — wave headings become the outcomes, wave exit lines become the Definition of done
verbatim, `candidate` blocks become Wave 0, cross-repo contracts become cross-lane edges, operator
errands become the operator lane, and `seams.md`'s blast radius lands in the lane files' `#`
comments where it is read at execution time. `SQ-nn` refs make the coverage crosswalk a one-liner.
Any departure from the authored cut must be stated with a reason, and the operator's fork answers
may not be silently re-decided. What this command still owns: lane assignment, the heavy budget,
isolation, Wave 0 mechanics and both crosswalks — `/sequence` decides *what* and *in what order*,
this decides *who runs it concurrently without colliding*.

**Session:** fresh (reading only `sequence.md`), and it ends without running anything. Each lane is
then **one fresh Opus session per repo, held open for that lane's whole chain** — the lane agent is
the single writer for its repo and carries block 1's lessons into block 7. Never two lanes in one
session. **Model:** Opus throughout.

Authors the two things `/begin-orchestration` consumes: a **roadmap document** and one
`lane-<name>.txt` chain file per lane, written to `planning/roadmaps/<slug>/` and registered as an
epic's `plan:` pointer. A roadmap is a *concurrency plan* — an assignment of work to
parallel `/orchestrate` sessions that cannot step on each other. Encodes the rules that have cost
real runs: the lane unit is the **repo, never the wave** (engines are serial inside a repo, so a
repo holding 10 blocks is the critical path regardless of scheduling); **at most two heavy-gate
repos concurrently**, read from each `harness.json` rather than memory; `base-template` lands early
in a worktree with propagation **deferred** to an operator gate; `[*]` blocks must be registered in
`state.json` in a hard **Wave 0** or the lane cannot resolve them; the generated `epic-sequence`
region is the only status surface and no wave table may be authored beside it; and the **Definition
of Done must be written as observations with commands, not as blocks closed** — the failure that
left a previous roadmap 30/53 closed with an undeployed demo and an unverified funnel. Authors only;
never runs `/orchestrate`. Sits above `/plan`, which scopes to one repo.

**Runs only from HQ (the brain root) — single-copy, does not sync downstream.** Step 1A resolves
`BRAIN_ROOT` and requires it: "a roadmap spanning repos cannot be authored from inside one of them."
`scripts/sync_downstream_harness.py`'s `EXCLUDED_COMMAND_FILENAMES` excludes `generate-roadmap.md`
from every sync target, HQ included, so it stays the one copy at `base-template/.claude/commands/`
rather than fanning out to all 17 leaf repos where it has no meaning. Decision + rationale recorded
in `planning/ticket-generate-roadmap-command/review.md`.

### `/generate-master-plan` — superseded (D65)
**Authors nothing.** `master-plan.md` is now **generated** from the block graph in `state.json`, not
hand-written, and a project's founding roadmap is authored by `/plan --founding`. The command file
remains only to redirect. Do not hand-write or hand-edit a `master-plan.md`; if one looks stale, run
`mev emit-state --write` and let it regenerate.

### `/generate-tasks`
Reads **`planning/blocks/<BlockID>.json`** — the authored block record (D65), *not* `master-plan.md`,
which is a generated view and gets you a stale summary. Writes the executable task list to
`planning/<name>/tasks.json`, renders the prose `tasks.md` from the record via
`scripts/render_spec.py` (generated — never hand-edit), and **commits** for a clean downstream tree.

**Before writing any task it reads the actual source the record names** — real function names,
signatures and sibling patterns. A file named as modified that does not exist means the record is
wrong, and the command stops rather than emitting a task against a path that is not there.

Three checks can **fail** the command and force a revision in place: **compilable task boundaries**
(under `/sdlc-flow` and `/sdlc-task` every task must leave the gating suite passing, so a breaking
public-surface change may never be split — the tasks merge, not the constraint); **un-gateable
acceptance criteria must be declared** (D64 — evidence living in another process, another repo, a
generated artifact, or an **installed** artefact needs a named failing command or a fixture-evidence
task); and **never fabricate a load-bearing fact** (ask interactively, abort with specifics in a
preflight context).

The commit lands **after** the self-check, the decomposition assessment and the pipeline
recommendation — all three can require a revision, so committing earlier means committing a draft.
The report names the engine command the recommendation actually chose; `/breakdown` appears only
when a task was flagged for it.

**`--from <path>` mode** decomposes a single standalone block file (e.g. a `/plan` output) instead
of a block record — for ad-hoc / experimental features kept out of the roadmap. Slug comes from the
file's parent directory.

**Session:** fresh, **one per block** — never decompose the next block in the same session, because
its tasks depend on this block's code, which does not exist yet (D65). **Model:** Sonnet; Opus when
the block carries breaking public-surface changes or several un-gateable criteria. The engine then
runs fresh again.

### `/breakdown`
Reads a task spec and the source files each step touches, then writes a granular
`breakdown.md` — every sub-step atomic (one file, one change, one command). Both `/implement`
and `/fix` auto-detect this file and use the matching `### Step N:` section as the primary
execution guide (HOW); `tasks.md` stays authoritative for scope (WHAT).

**Only `tasks.json` is executed.** No engine parses `breakdown.md`, so a breakdown changes what an
implementer *knows*, never what the engine *runs*. If the decomposition should change what gets
executed — a task split in two, a merge forced by a breaking public-surface change, a new
`dependsOn` edge, a wrong `files[]` — that correction must be **written back into `tasks.json`**
and recorded in the breakdown's `## tasks.json changes` section. A breakdown that quietly disagrees
with the JSON is worse than no breakdown: the engine follows the JSON and the reviewer follows the
prose.

It also **verifies every symbol it names actually exists** before committing (anything unresolved is
either explicitly marked as created by the sub-step, or a mistake), and applies a **coarseness
floor** — the same heuristic `/generate-tasks` uses at authoring time — so an already-atomic spec is
reported as such instead of being restated at greater length.

**Session:** runs inside `/generate-tasks`' session (same spec, same source, and it writes back to
`tasks.json` — one writer). **Model:** Sonnet. The engine runs fresh.

### Pre-planning capture — `/capture`

Before something is ready to plan, use `/capture` to park rich conversation notes without
losing them. Creates `planning/<slug>/notes.md` with a structured scaffold and adds a
pointer ticket to the brain's `planning/backlog.md`.

| Command | Use for | Writes to |
|---|---|---|
| `/capture <title>` | Rich pre-plan notes — detailed enough to need a file, not yet a plan | `planning/<slug>/notes.md` + brain backlog |

Captures are read weeks later with none of the originating context, and read as fact unless they
say otherwise — so every substantive claim is tagged **VERIFIED** (read in source or observed
running, with the symbol named) · **ASSUMED** (believed, not checked, with what would check it) ·
**SAID** (stated by someone, unconfirmed). A `## Provenance` block pins the date and each repo's
SHA, so a later reader can tell in one command whether the note still describes the system that
exists. Open Questions are each labelled READ · SPIKE · ASK · ASSESS, which is what decides where
the note goes next — an ASSESS entry means the next step is `/assess`, not a plan.

**A capture is not an assessment and must not be promoted as one.** It is cheap precisely because
it is mostly unverified; the tags are what keep that legible.

The notes file sections (What & Why · Context & Background · Key Information · Open Questions ·
Rough Scope · Provenance) are direct input to the commands below. **Where a note goes next is
decided by its Open Questions, not by its size:**

| Open Questions are mostly | Promote with |
|---|---|
| READ or ASK, and the unit of work is clear | `/ticket` or `/chore` |
| READ or ASK, several blocks in one repo | `/plan` |
| SPIKE — one assumption is load-bearing and unsettled | Settle the spike first, then promote |
| ASSESS, or "what does this call that it does not build" came back *unknown* | `/assess <topic>` — the shape of the work is not yet known |

**Session:** inline only, no subagent — a subagent cold-starts with none of the conversation the
capture exists to preserve. **Model:** whatever the session is already running.

### Ad-hoc planners — `/chore`, `/ticket`, `/plan`

Entry points into Phase 1 for work that **isn't** a master-plan block. Each takes a free-text
description, researches the codebase, and writes a spec into its own `planning/<dir>/` directory.
Output feeds the rest of the pipeline unchanged.

| Command | Use for | Writes to | Escalates to |
|---|---|---|---|
| `/chore <description>` | Maintenance / housekeeping — no behavior change | `planning/blocks/<Prefix>.chore.<slug>.json` + `planning/<BlockID>/tasks.json` (+ rendered `tasks.md`) | `/ticket`, if it turns out to change behavior |
| `/ticket <description>` | Bug fix or targeted enhancement requiring tests + observable AC | `planning/blocks/<Prefix>.ticket.<slug>.json` + `planning/<BlockID>/tasks.json` (+ rendered `tasks.md`) | `/plan` on size · `/assess` when the behavior can't be reproduced or the half-built question can't be answered |
| `/plan <description>` | Any ad-hoc or experimental feature — several blocks in one repo | `planning/<slug>/plan.md` + `planning/blocks/*.json` | `/generate-roadmap` if it spans repos · `/assess` per its own floor |

`/chore` and `/ticket` are the **one-session** commands: they author the block record *and* its
task list in the same pass, because a one-off has no downstream block waiting on its code and so
nothing to defer (D65). `tasks.md` is rendered from the record by `scripts/render_spec.py` — never
hand-written.

**`/chore` and `/ticket` carry the pre-plan floor at their own scale.** **`/ticket`** reproduces the failure before
writing a single Acceptance Criterion — a ticket written from a *described* bug fixes the
description — records the real error text in the block record, and orders the test **before** the
fix so the gate is shown capable of failing (D68, red-green). **`/chore`** takes a pre-change gate
baseline, since "no behaviour change" cannot be claimed against a baseline never taken, and must
carry at least one criterion that observes a *difference* — "the gates still pass" is true of doing
nothing. Both ask the half-built question (does this call something that merely exists in source?)
and both have an escalation trigger: `/ticket` stops when the behaviour cannot be reproduced or the
half-built question cannot be answered; `/chore` stops when the work turns out to change behaviour,
which makes it a ticket.

**Session:** both author *and* decompose in one session — that is the point of the lean lane, and
the work is small enough that the record-stands-alone property is cheap to satisfy directly. The
engine then runs fresh. **Model:** Opus for `/ticket` when the failure is subtle; Sonnet otherwise
and for most chores.

`/chore` and `/ticket` write a runnable `tasks.md` **directly** and route to lean `/sdlc-task`
(the fast path). `/plan` writes a `plan.md` in the **master-plan format** (phases/blocks/Quick
Reference table), so `/orchestrate` can drive it as a branch train or `/generate-tasks --from
planning/plan-<slug>/plan.md` can decompose a single block into a `tasks.md` → `/sdlc-flow`, all
**without** touching `master-plan.md`. See `planning/decisions/D34-adhoc-planning-seam.md`.

`/plan` reads **`planning/<slug>/sequence.md`** when Phase 0 has run, and carries its cut, wave
boundaries, `ships` lines, `depends_on` edges, named files and recorded fork answers through rather
than re-deriving them — a silent departure means the seam analysis was done and then ignored, so any
departure has to be stated in the Sequencing Rationale with a reason. Where `assessment.md` and
`verification.md` disagree, **verification wins**, and no claim it marked REFUTED may reach a block
record.

Its self-check carries three properties beyond the structural ones, each of which can **fail** the
plan: **ships alone** (a block record whose `why` reads "enables <later block>" gets merged into the
block that consumes it), **every block names the gate that proves it** plus — per D68 — how that gate
is shown capable of *failing*, and a **handoff test** on the first runnable block (could a fresh
agent start from that record alone, without asking a question?). A fresh-agent adversarial pass then
attacks the sequencing, because step 9 is self-review by the context that wrote the plan. `--no-redteam`
skips it for small, low-risk initiatives.

**Session:** fresh, and it ends. `/generate-tasks` reads **only** the target block's record — running
it in the planning session would let it lean on narrative context the record does not carry, and the
record's incompleteness would never surface. **Model:** Opus.

---

## Phase 2 — Implement

### `/implement`
Runs `/prime`, reads the plan file, executes every step (or task N) following CLAUDE.md
conventions, runs the relevant Validation Commands, and records the outcome in
`sdlc/state.json` + `sdlc/worklog.md` (see Run Artifacts above).

### `/fix`
Reads the review verdict's failing criteria from `sdlc/state.json` and `sdlc/worklog.md`, orients
via `/prime`, and applies targeted changes addressing only the failures. Appends a new fix-pass
worklog section rather than overwriting the implement one. Hard-errors if no review entry is
present; soft-stops if the verdict is already PASS.

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
also records the outcome in `sdlc/state.json` + `sdlc/worklog.md`.

> **Stack note:** the test stage runs the checks defined in `planning/harness.json`
> (`validation.checks[]`). The harness ships no stack defaults — define your project's actual
> validation commands there (copy a profile from `planning/harness.examples.md`). If the config
> is absent, the stage falls back to the spec's `## Validation Commands` section.

---

## Phase 4 — Review

### `/review-task`
Runs `/prime`, reads the implement/test worklog sections and `sdlc/state.json` as context, then
runs a **fresh test suite** as authoritative verification. Verdict is PASS only if all criteria
are MET **and** the fresh tests pass. Records the verdict in `sdlc/state.json` + `sdlc/worklog.md`.

---

## Phase 5 — Document

### `/document`
Gates strictly on the review verdict being PASS. Reads `sdlc/state.json`'s `files_changed` list
(cross-checked against the diff) to scope updates, then surgically patches only affected sections
of `docs/*.md`. Flags architecture-level changes as `NEEDS_REVIEW`. Never touches `planning/`,
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

## Block Setup & Worktree Management

### `/start-block`
Finds the target spec (defaulting to the first non-done spec), checks that all preceding specs
are `Done`, then flips it to `In progress` and updates Current focus + Last updated.

### `/init-worktree` · `/clean-worktree`
Manual entry points for the isolated-worktree lifecycle that `/sdlc-task` and `/orchestrate`
automate. `/init-worktree` derives a branch/worktree from the spec slug and creates an isolated
sparse checkout; `/clean-worktree` **merges before delete** — fast-forward-merges the branch
into `main`, applies deferred STATUS/Log updates, then removes the worktree.

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
