# AGENTS.md — engine-rs

Bastion's native Rust execution engine — a graph-validated workflow runtime that embeds in `bastion serve`, holds live run state in-memory, and writes the data contract to Postgres as a durable record.

**This repo is the orchestrator.** Per brain **D50/D51** the Python repo (renamed **Synapse**, D52)
divested every execution workflow: all business workflows, artifact generation, eval, and the SDLC
harness are engine-rs's. Synapse keeps knowledge only — corpus, embeddings, structural graph, memory,
retrieval.

## Workflow engine telemetry

**After invoking `Workflow({name: 'sdlc-task'|'sdlc-flow', ...})`, load the `stamp-workflow-run-id`
skill.** The engine script can't read its own Workflow run id back — the Workflow script API has no
`runId` global and no filesystem access — so joining a run's `sdlc-task-state.json`/
`sdlc-flow-state.json` to the exact Claude Code session transcript for cost telemetry relies on the
*invoking* agent patching the id in after the call returns. Skip this and `workflow_run_id` simply
stays `null` — a normal, expected state, never a defect to chase.

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
- **Symlink traps:** `rg`/`grep`/`find` are symlink-blind by default — a search that must include `planning/` content needs `-L`/`--follow`. `git mv` fails through the symlink face ("source directory is empty") — move planning files via the real vault path (`.../_planning/<slug>/...`), never via `planning/...`. Planning changes are committed in the brain repo (`agentic-portfolio`) with an explicit pathspec, never in this repo.
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
7. **A node's stable prompt is a file, not a string literal.** Per **D24**
   (`planning/decisions/D24-node-prompts-live-in-colocated-files.md`), every stable-prompt `const`
   lives at `crates/engine-core/src/workflows/<workflow>/prompts/<node>.md` and is pulled in with
   `include_str!("prompts/<node>.md")` — colocated per workflow, never a global prompt tree, and
   never a runtime file read (`include_str!` resolves at compile time, so a deployed `bastion
   serve` never depends on finding the file on disk). This keeps the const's type, name, and
   visibility unchanged, so `policy::apply_prompt_cache`'s cache breakpoint stays run-invariant —
   the same discipline rule 6 requires for policy-varying text. Only the STABLE prefix moves:
   per-run body construction (`build_prompt(...)`, interpolating `format!`s) stays in Rust. A
   guard test at `crates/engine-core/tests/it/prompt_externalization.rs` (module of the shared
   integration binary, per rule 9) fails on any newly-introduced inline prompt literal — see
   [`docs/workflows/README.md`](docs/workflows/README.md) for where each workflow's prompts live.
8. **Use `cargo nextest run`, never plain `cargo test`, for any test run you invoke yourself
   during a task** (scoped to a module: `cargo nextest run -p <crate> <module::path>`; workspace-
   wide fast check: `cargo nextest run --lib --workspace`). A `PreToolUse` hook in
   `.claude/settings.json` enforces this. Beyond the parallelism, nextest's process-per-test model
   is what makes the single-integration-test-binary layout (rule 9) safe — the two go together.
   The one exception is the task explicitly designated to own full-suite validation for a spec —
   that task runs `planning/harness.json`'s authoritative `command` (`cargo nextest run
   --workspace` + `cargo build --release`), not `fastCommand`.
9. **One integration-test binary per crate.** New integration suites go in
   `crates/engine-core/tests/it/<name>.rs` with a `mod <name>;` line in `tests/it/main.rs` — never
   a new `crates/engine-core/tests/*.rs` file, which cargo builds as a separate binary that
   statically re-links the whole ~345-crate graph. See [`docs/testing.md`](docs/testing.md) and
   "Build / test / run" below.
10. **Never `git push` this repo directly from inside it.** This repo sits in the fleet's Cargo
   path-dependency graph (`engine-rs` -> `claude-code-rs`, `mev`, `okf-core`; and `bastion` ->
   `engine-rs`), and every Rust repo's CI clones its sibling path-deps at their unpinned default
   branch — pushing out of order breaks a sibling's CI on code that was actually fine (the
   2026-08-18 outage: `bastion` red with `cannot find function lanes_brain in crate mev` purely
   because `mev` sat 23 commits unpushed). Route every push through the company-brain's
   `agentic-portfolio/scripts/git_push.sh --all`, which pushes the whole fleet in dependency
   order and skips a repo flagged `ci-blocked` (a Cargo dependency is red on GitHub with nothing
   queued to fix it). Branching, committing, and opening/reviewing/merging PRs to `main` locally
   are all fine from inside this repo — only the final `git push` of `main` to `origin` must go
   through that script.

## Known bugs

None known at initialization.

## Build / test / run

```bash
cargo build
cargo nextest run --lib --workspace   # fast — use this, not plain `cargo test`
cargo run
```

> **`cargo nextest run`, never plain `cargo test`** (standing rule 7, enforced by a `PreToolUse`
> hook in `.claude/settings.json`). `nextest` runs each test in its own process in parallel,
> rather than libtest's serial-per-binary model.
>
> **The cost in this repo is LINKING, not testing.** Measured 2026-07-29: running the full
> workspace suite takes ~2s; everything else was compile and link. Full detail in
> [`docs/testing.md`](docs/testing.md); the cross-project playbook is
> `base-template/docs/rust-sdlc-iteration-speed.md` (governed by brain decision **D57**). Three
> fixes came out of that measurement, and the numbers below are why they must not be casually undone:
>
> | | before | after |
> |---|---|---|
> | Per-task tripwire (`--lib --workspace`, after an `engine-core` edit) | 2m44s | **6.4s** |
> | Full suite build (after a one-line `engine-core` edit) | 2m24s | **5.3s** |
> | Full suite run | 58s | **2.2s** |
> | Full suite, nothing changed | minutes | **2.8s** |
>
> 1. **One integration-test binary per crate, not one per file.** cargo builds a separate binary
>    for every `tests/*.rs`, each statically linking the crate plus its ~345-crate dependency
>    graph — 25 binaries x ~20MB of linking on every full run. All of `engine-core`'s integration
>    tests are now modules of a single binary: **add a new one at `crates/engine-core/tests/it/<name>.rs`
>    and declare `mod <name>;` in `crates/engine-core/tests/it/main.rs`.** Do NOT add a new
>    `crates/engine-core/tests/*.rs` file — that silently reintroduces a second binary. Per-test
>    isolation is unaffected because nextest forks a process per test regardless; this collapse
>    would NOT be safe under plain `cargo test`.
> 2. **No `sccache`.** It was wired in `6ccbcce` and measured doing literally nothing —
>    `sccache --show-stats` reported 25 compile requests and **0 executed, 0 hits, 0 misses**,
>    because it refuses to cache incremental compilations and cargo passes `-C incremental` for
>    the test profile. Incremental compilation is the right trade for a loop that re-edits one
>    crate 10-30 times; see `.cargo/config.toml` for the full rationale before re-adding it.
> 3. **`[profile.dev]` link-time settings in `Cargo.toml`** (`debug = "line-tables-only"`,
>    `split-debuginfo = "unpacked"`) — keep backtraces, drop the expensive DWARF/dsymutil work.
> 4. **Keep `target/` clean.** Cargo never garbage-collects incremental state. This tree had rotted
>    to **40GB** (930,599 files), and clearing it was the single largest lever of all — a cold
>    from-scratch build (48.9s) beat the bloated tree's *incremental* build (2m24s) by ~3x. Run
>    `du -sh target` when the loop feels slow; `cargo clean` when it passes a few GB.
>
> **Scope even narrower while mid-task.** While iterating inside a single task, prefer
> `cargo nextest run -p <crate> <module::path>` — just the touched crate and module — over even
> the workspace-wide fast command. Only the task(s) explicitly designated to own full-suite
> validation for the spec should run the workspace-wide `fastCommand` or the full
> `cargo test` / `cargo build --release` gates; every other task should stay scoped to what it
> touched and defer the broad run.
>
> **A task that cannot break the build should not pay for one.** `tasks.json`'s
> `validation_commands` is honoured by `/sdlc-flow` and `/sdlc-task`: a task declaring a
> non-empty array runs those commands **in addition to** this project's `gates: true` harness
> checks (in their `fastCommand` form) — they **AUGMENT** the harness list, they do **not**
> replace it. See `base-template/planning/decisions/D63-per-task-validation-commands-augment-gating.md`
> (note: this is base-template's D63, not the brain's D63, which is an unrelated decision), and
> the engine's own log line at `.claude/workflows/sdlc-task.js:2000`.
>
> **The consequence, which is the opposite of what this section said until 2026-09-04:** scoping a
> task's `validation_commands` narrowly does **not** buy it a cheaper run — the project's gating
> checks still run on top. The only thing that removes a project check from per-task runs is
> **`perTask: false` on that check in `planning/harness.json`**; a task's own `validation_commands`
> can never suppress one. Two corollaries: a docs-only task still pays for whatever the harness
> gates unless those rows are `perTask: false`, and `expect_red` can never invert a `gates: true`
> harness check — so in a repo whose suite-wide runner gates, a task whose deliverable is a test
> observed failing must use a runtime inversion or be merged into its fix task. The end review
> still runs the full harness suite over the integrated tree, so nothing escapes validation. Leave
> the field `[]` for any task that touches `.rs` files.
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

## SDLC pipeline

This project carries the curated SDLC harness. Run `/prime` to orient, then drive
structured work through:
`/generate-tasks → /implement → /test → /review-task → /document → /log-work`.

> **Stack note:** the SDLC engines carry no stack defaults. Point them at this project's stack
> by filling `planning/harness.json` (validation commands + optional UI-test config). Copy a
> ready-made profile from `planning/harness.examples.md` (Rust / Python / Next.js). Do **not**
> edit the `workflows/*.js` engines for stack reasons — that's what `harness.json` is for.

<!-- BEGIN:response-style -->
## Response Style

You are read by an operator scanning several concurrent agent sessions. Long prose is the failure
mode, not thoroughness.

1. **First line = the outcome** — what happened, and whether it needs them.
2. **Then the specifics** — bullets, one line each, max ~6. Facts, not narration.
3. **Last line = the ask**, if there is one. One question, answerable in a word.

**Ceiling: 10 lines for a normal turn, 20 for an end-of-run report.** Only depth the operator
explicitly asked for may exceed it.

Durable detail goes to disk — the commands already require that. **Link the path; do not restate
the file.** Lead with failures, blocks, and anything that did not match the ask, in plain words with
the real error text. Cut reasoning narration, unasked-for next steps, and self-assessment.

Full rationale, the complete cut-list, and worked before/after examples: the
**`report-to-the-operator`** skill.
<!-- END:response-style -->

<!-- BEGIN:session-continuity -->
## Stopping, continuing, and handing off

**Run to completion. Never stop, clear, or hand off because context is getting large.** There is no
token band, no percentage, and no "the next block would be cleaner in a fresh session." A chain runs
every block it was given; a lane that stops after one block and waits to be relaunched by hand
defeats the entire point of the run and puts the operator back in the loop after every block. If
context genuinely runs out, the harness summarizes and you keep going — that is its job, not yours.

There is exactly **one** reason to end a session early, and it is about correctness, not cost:
**something the running session depends on changed underneath it** — an engine, command file,
installed binary (`mev`, `bastion`), hook or `settings.json` edited this session, or a `CLAUDE.md`
you already read. The running session is a launch-time snapshot (base-template standing rule 10), so
it keeps producing pre-change results, which read as an unreliable agent rather than a stale
snapshot. **Name the trigger, finish the unit of work in flight, and say plainly that a fresh
session is needed.** Do not present it as a context-budget decision, and do not go looking for the
trigger as an excuse to stop.

Whenever you do hand off, write the entry point first — `status.md`, `handoff.md`, a spec's
`tasks.json`, or an orchestration-run `notes.md` — so the next agent starts from an artifact instead
of from your memory.
<!-- END:session-continuity -->
