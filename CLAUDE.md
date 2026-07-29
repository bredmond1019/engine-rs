# CLAUDE.md — engine-rs

Bastion's native Rust execution engine — a graph-validated workflow runtime that embeds in `bastion serve`, holds live run state in-memory, and writes the data contract to Postgres as a durable record.

**This repo is the orchestrator.** Per brain **D50/D51** the Python repo (renamed **Synapse**, D52)
divested every execution workflow: all business workflows, artifact generation, eval, and the SDLC
harness are engine-rs's. Synapse keeps knowledge only — corpus, embeddings, structural graph, memory,
retrieval.

## THE BOUNDARY TEST — read this before scoping any new work

Brain (Synapse), Engine (engine-rs), or Factory/Doc (mev/okf-core)? Ask in order. Governed by brain **D51** & **D53**; this block is
byte-identical in `core/orchestrator/CLAUDE.md`.

```
THE BOUNDARY TEST — Brain (Synapse), Engine (engine-rs), or Factory/Doc (mev/okf-core)?  Ask in order.

1. Does it need IN-PROCESS access to embeddings, pgvector, brain_edges,
   or the memory tables?                                    YES -> Synapse
2. Does it produce a client- or repo-facing artifact
   (brief, proposal, PDF, PR, code)?                        YES -> engine-rs
3. Is it maintaining the corpus itself (freshness, validation,
   distillation, retrieval quality, scheduled chores)?      YES -> Synapse
4. Does it serialize or write a repo-tracked source document
   (.md with OKF frontmatter)?                              YES -> mev / okf-core (via engine-rs)

TIEBREAKER — if 1 and 2 are both YES, the work is a hybrid.
   SPLIT it at the ingest seam. Never let one repo own both halves.
       engine-rs workflow  --POST /ingest/*-->  Synapse
   engine-rs acquires and reasons; Synapse owns everything behind the endpoint
   (embedding, storage, retrieval, memory, decay).
```

**The practical consequence for this repo: no embedding, no pgvector, no corpus writes — ever.** A
workflow that produces something the Brain should remember uses a `PersistToBrainNode` over the
injectable `HttpPost` seam (`EN.4.C`) and POSTs to Synapse's ingest endpoint (Synapse block `OR.Q`).
If you find yourself reaching for an embedding model here, you are on the wrong side of the boundary.

**Cadence:** engine-rs and `bastion` schedule business runs; Synapse schedules only its own corpus
housekeeping. There is no global scheduler.

## Before you start

- **Strategic context:** `planning/context.md` (read first) → `planning/status.md` (current state)
- **Symlink warning:** the `planning/` directory is actually a local symlink pointing to the company brain repo's `_planning/` vault (e.g. `core/_planning/engine-rs/`). The brain repo is responsible for tracking all planning files under Git. Do not track `planning/` in this project's public Git repository (it is gitignored).
- **Plan:** `planning/master-plan.md` — the phase/block sequence
- **Pipeline config:** `planning/harness.json` — the validation commands + UI-test config the
  SDLC engines run (see `planning/harness.examples.md` for ready-made stack profiles)
- **Decisions log:** `planning/decisions/` (start at `planning/decisions/index.md`) — check
  before relitigating any settled choice

## Standing rules

1. **Every block/task ships with tests** covering its core functionality. No exceptions.
2. **Every new `.md` under `docs/` or `planning/` must open with OKF YAML frontmatter.**
   Required fields: `type` (e.g. Decision, Index, Reference, Plan, Log, ProjectStatus, LocalContext,
   Guide); `title` (human-readable); `description` (one-line summary for embedding).
   Optional but strongly encouraged: `doc_id` (kebab-case stable id, defaults to filename stem);
   `layer` (list from closed vocab: `factory` · `brain` · `engine` · `console` · `surface` ·
   `infra` · `business` · `content` · `meta`); `project` (the project's own slug — see
   `docs/okf-frontmatter.md` in the company brain for the controlled vocabulary); `status`
   (`active` · `draft` · `deprecated` · `superseded` · `archived`); `keywords` (3–7 topic
   terms); `related` (list of doc_ids). Canonical guide: `agentic-portfolio/docs/okf-frontmatter.md`
   (governed by brain decision D27).
   Adding a file to a directory requires updating that directory's `index.md` — propagate up
   the chain as needed.
3. **Sequence, not calendar** — work the order in `master-plan.md`; pick up where you left off.
4. **Decisions are append-only** — never edit a settled decision; supersede it with a new
   atomic file in `planning/decisions/` and link back.
5. **Verified identity / handles:** none — treat these as the only authoritative
   identities/URLs; flag any other handle or profile link as unverified before publishing it.
6. **Nodes are configurable, not hardcoded.** Anything that trades cost, latency, or quality —
   model tier, verbosity, prompt caching, loop/retry bounds, fetch or tool budgets, local-vs-cloud
   routing, how much telemetry a stage emits, whether an optional enrichment step runs at all —
   belongs on that workflow's `Policy` surface (`EN.4.0`), resolving through the four layers
   (per-run event `policy` override > named `profile` bundle > `planning/harness.json` defaults >
   built-in default). **Never bake such a value into a prompt string, a node constructor, or a
   `const`.** The test: *if a future run might reasonably want a different value for cost, speed, or
   quality reasons, it is a knob.* Practical consequences:
   - **Give every knob a built-in default that is behavior-stable** — adding the knob must not
     change what an existing run does.
   - **Every workflow ships the three named profiles** (`baseline` = explicit no-op, `cheap-fast` =
     the cost/latency floor, `thorough` = the quality ceiling) and sets the new knob in each. A knob
     absent from the profile bundles is a knob nobody will find.
   - **Keep the shape invariant across settings.** A policy knob may change a *prompt*, a bound, or
     a tier; it must not change an emitted JSON schema, a declared graph's node set, or a data
     contract. Cost control belongs in the prompt and in a node's own no-op path, not in a
     conditional rewire — one graph, validated once, is what makes runs comparable.
   - **Keep cache breakpoints run-invariant.** Policy-varying text goes in the per-run prompt body,
     never in a `STABLE_SYSTEM_PROMPT` prefix.
   - **Stamp the resolved value** into the node's `ctx.nodes` result so `RunTelemetry` /
     `PolicyAggregate` can attribute observed cost to the setting that caused it.
   - **Document it in `planning/harness.json`** alongside the existing no-op defaults, so the knob is
     discoverable without reading the Rust.
   - *Where feasible* is a real qualifier: a value fixed by an external contract (a wire format, a
     required header, an interface another repo pins) is not a knob. Say so in a comment rather than
     leaving the next reader to wonder.
7. <!-- Add further project-specific standing rules here (prompt handling, registries, deployment
   boundaries, code style, etc.). -->

## Known bugs

None known at initialization.

## Build / test / run

```bash
cargo build
cargo test --lib --workspace   # fast — use this, not plain `cargo test`
cargo run
```

> **Always prefer `cargo test --lib --workspace` over plain `cargo test` in this repo.** The
> full `cargo test` (unit + integration + doctests across the workspace) takes several minutes;
> `--lib --workspace` runs only the unit tests and is dramatically faster for the same-signal
> inner loop. This is already wired as the `fastCommand` on the `test` check in
> `planning/harness.json`, which the SDLC engines use for per-task (`testDepth: "fast"`) runs —
> but reach for it manually too, any time you're iterating and don't need the full gate (e.g.
> quick sanity checks between edits, or triage loops outside the harness).
>
> **Scope even narrower while mid-task.** While iterating inside a single task, prefer
> `cargo test -p <crate> --lib <module::path>` — just the touched crate and module — over even
> the workspace-wide fast command. Only the task(s) explicitly designated to own full-suite
> validation for the spec should run the workspace-wide `fastCommand` or the full
> `cargo test` / `cargo build --release` gates; every other task should stay scoped to what it
> touched and defer the broad run.
>
> The SDLC pipeline reads its validation suite from `planning/harness.json` (not from this
> block). Keep the commands here in sync with that file's `validation.checks[]` so humans and
> the pipeline run the same thing.

## Directory map

```
engine-rs/
├── .claude/        ← Claude Code commands + SDLC workflow engines
├── planning/       ← context, status (+Momentum/Metrics), master-plan, knowledge, memory,
│                     artifacts/, harness.json, decisions/, <concept>/
└── <source dirs>   ← add as the project grows
```

## What NOT to touch

<!-- Reference-only code, generated files, migration history, etc. List them as they appear. -->

---

## Available Commands

All harness commands are installed globally in `~/.claude/commands/` via `/sync-global-commands`
(run from base-template). Invoke them with `/<name>` directly. Project-specific commands (if any)
live in `.claude/commands/` and take precedence over global commands on name conflict.

### Session

| Command | What it does |
|---|---|
| `/prime` (global) | Deep session start — reads key docs and summarizes state |
| `/session-recap` (global) | Start-of-session briefing: recent log, current focus, next action |
| `/handoff` (global) | Write handoff.md + log work + commit; hands off to a fresh agent |
| `/wrap-up` (global) | Log work + commit; clean session close without a handoff file |
| `/status` (global) | Quick status snapshot of current focus and momentum |
| `/log-work` (global) | Log a completed work session and update status.md |
| `/archive` (global) | Retire a folder/file — distill durable residue first (D35 gate) |
| `/capture` (global) | Scaffold planning/<slug>/notes.md for pre-plan ideas; adds backlog ticket to brain |

### Planning

| Command | What it does |
|---|---|
| `/plan` (global) | Author a mini-roadmap (phases/blocks) into planning/plan-<slug>/plan.md |
| `/ticket` (global) | Single-block behavior-change spec with observable AC + testing strategy |
| `/chore` (global) | Plan a maintenance or housekeeping task |
| `/breakdown` (global) | Decompose a task spec into agent-executable sub-steps |
| `/generate-tasks` (global) | Generate a task spec for a specified phase and block |
| `/generate-master-plan` (global) | Author the project roadmap as canonical block definitions |

### SDLC

| Command | What it does |
|---|---|
| `/implement` (global) | Execute a plan file against the codebase |
| `/test` (global) | Application validation test suite |
| `/fix` (global) | Make targeted fixes for a FAIL or PARTIAL review verdict |
| `/patch` (global) | Hotfix ladder: small targeted fix routed to lean /sdlc-task |
| `/document` (global) | Update docs to reflect a completed, reviewed implementation |
| `/update-docs` (global) | Documentation health sweep: find stale sections and create missing coverage |
| `/conditional_docs` (global) | Task-type documentation router |
| `/process-tasks` (global) | Process a task list sequentially |
| `/update-task` (global) | Update a task spec after a deviation or completion |
| `/review-task` (global) | Verify a completed task against its spec and acceptance criteria |
| `/review-workflow` (global) | Verify that a completed pipeline executed correctly |
| `/review-PR` (global) | Review a PR against its block spec; post structured verdict |
| `/close-out` (global) | Verify test coverage, patch docs, and hand off cleanly |

### Git

| Command | What it does |
|---|---|
| `/commit` (global) | Stage and commit changes with a conventional message |
| `/init-worktree` (global) | Initialize a new git worktree for isolated work |
| `/clean-worktree` (global) | Merge a completed worktree branch into main and remove it |
| `/start-block` (global) | Start a new spec block: branch, initial commit, worktree setup |
| `/merge-train` (global) | Merge all approved block PRs in dependency order |

### E2E

| Command | What it does |
|---|---|
| `/test_auth_gate` (global) | E2E test template: authentication gate |
| `/test_crud_api` (global) | E2E test template: CRUD API |
| `/test_error_handling` (global) | E2E test template: error handling |
| `/test_ui_form` (global) | E2E test template: UI form |

> `/sync-global-commands` (global) is available in base-template only — it syncs
> these commands to `~/.claude/commands/` and aborts if run outside the base-template root.

## SDLC pipeline

This project carries the curated SDLC harness. Run `/prime` to orient, then drive
structured work through:
`/generate-tasks → /implement → /test → /review-task → /document → /log-work`.

> **Stack note:** the SDLC engines carry no stack defaults. Point them at this project's stack
> by filling `planning/harness.json` (validation commands + optional UI-test config). Copy a
> ready-made profile from `planning/harness.examples.md` (Rust / Python / Next.js). Do **not**
> edit the `workflows/*.js` engines for stack reasons — that's what `harness.json` is for.
