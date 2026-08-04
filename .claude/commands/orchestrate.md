# Orchestrate — Run an ordered chain of blocks through the SDLC engines, in one session.

Takes an ordered list of work blocks and drives each one end-to-end: spec → (breakdown) →
engine → integrate → verify state → next. The engines run as **background workflows**, so while
one block builds, this command prepares the specs for the blocks behind it.

One `/orchestrate` session drives one repo. Run several repos at once — that is the lane model.

## Variables

$ARGUMENTS — one of:
- **Inline list:** ordered block IDs or spec slugs, space- or comma-separated.
  `/orchestrate OK.3.A OK.3.B`
- **File path:** a file with one block ID or spec slug per line. `#` comments and blank lines are
  ignored; file order is execution order.
  `/orchestrate planning/bullet-proof-software/lane-okf-core.txt`
- **Flags:**
  - `--worktree` — **require** worktree isolation for every block in the chain. See step 5.
  - `--no-worktree` — force plain-branch/in-place for every block, overriding any per-repo default.
  - `--engine <task|flow>` — force one engine for every block, overriding recommendations.
  - `--dry-run` — resolve the chain, generate the specs, print the plan. Run no engines.
  - `--stop-on-fail` (default) / `--continue-on-fail`.

If `$ARGUMENTS` is empty, stop and print:
```
Usage: /orchestrate <block-id> [block-id ...]
       /orchestrate <path-to-list-file>
       Flags: --worktree --no-worktree --engine <task|flow> --dry-run --continue-on-fail
```

---

## Standing rules

Each of these exists because it has already caused a real failure in this fleet.

1. **Do NOT spawn subagents.** No `Task`/`Agent` calls. You invoke `/generate-tasks`, `/breakdown`,
   and the engine workflows **from this session**. The engines spawn their own internal agents —
   that is theirs to do, not yours.
2. **Only `sdlc-task` and `sdlc-flow`.** If `/generate-tasks` recommends `/sdlc-run` or
   `/sdlc-block`, stop and report — those have different isolation and merge semantics than this
   command handles.
3. **One repo per session, one engine run at a time.** Both engines take the repo's branch or
   working tree. Never launch a second engine workflow in the same repo before the first has
   completed and integrated.
4. **Never start a block with unmet dependencies.** See step 2.
5. **Verify every state write.** The engines' status bookkeeping is known-unreliable
   (`base-template:BT.ticket.sdlc-state-write-reliability` — agent-prompt-driven, skipped in
   worktree mode). Trust nothing; check it (step 8).
6. **Check downstream consumers after any block touching a shared crate's public surface** (step
   9). No lane can see a sibling lane running in a different repo — this is the only thing in this
   command that looks outside its own repo, and it exists because the alternative (nothing looked)
   has already broken two other repos mid-run.

---

## How the pipeline works

The engines are **background workflows**: launching one returns immediately with a task ID, and a
`<task-notification>` arrives when it finishes. That is the concurrency this command exploits.

```
 ├─ generate spec: block 1
 ├─ LAUNCH block 1 engine ──────────────────────────┐  (background)
 │   ├─ generate spec: block 2                      │
 │   ├─ generate spec: block 3                      │  ← you keep working
 │   └─ generate spec: block 4 …                    │
 ├─ ◄── task-notification: block 1 done ────────────┘
 ├─ integrate + verify state for block 1
 ├─ LAUNCH block 2 engine (spec already written) ───┐
 │   └─ generate spec: block 5 …                    │
 └─ …                                               ┘
```

**Engine runs are serial** (rule 3); **spec preparation overlaps them**. Aim to always be at least
two specs ahead of the running engine. If you run out of specs to write, wait for the notification
rather than launching anything.

---

## Steps

### 1. Parse the chain
Resolve `$ARGUMENTS` to an ordered list. For a file, read it and strip comments/blanks. Print the
chain with positions so the operator can confirm the order before anything runs.

### 2. Check readiness against the live graph
For each block, find it in the repo's `planning/state.json` `tracks[].blocks[]` and resolve every
`depends_on` target's `status`:
- Unmet dependency **inside this chain but later in it** → stop. The order is wrong; report the
  correct order.
- Unmet dependency **outside this chain** → mark `HELD`, drop from this run, and say so plainly:
  *"HELD: `<id>` needs `<dep>` (`<repo>`) — run after that lands."* Never silently skip.
- Already `closed` → drop with a note.
- Block belongs to another repo → stop. It is a different lane.

### 3. Resolve block IDs to spec slugs
The spec lives at `planning/<spec-slug>/tasks.md`. Read `planning/` to learn the repo's actual
convention rather than assuming:
- `XX.ticket.<slug>` → `ticket-<slug>` · `XX.chore.<slug>` → `chore-<slug>`
- `XX.<phase>.<letter>` → the master-plan slug, usually `<phase>.<letter>-<kebab-title>`

If a directory already matches, use it verbatim. If you cannot resolve a slug confidently, **stop
and ask** — a wrong slug writes a spec to the wrong place.

### 4. Prepare the first spec, then start the pipeline
If `planning/<spec-slug>/tasks.md` is missing for block 1, run **`/generate-tasks <spec-slug>`**
(or `--from <plan-path> phaseN-blockX` for a standalone plan file). Capture from its output:
- whether it flagged any task as a **`/breakdown` candidate**, and
- its **pipeline recommendation** (its step 11).

Run **`/breakdown planning/<spec-slug>/tasks.md`** *only* when it flagged that spec. Never break
down on your own judgment — an unnecessary breakdown multiplies engine runs for no benefit.

### 5. Decide engine and isolation

**Engine** — take `/generate-tasks`' recommendation unless you have a concrete reason not to:
- **`sdlc-task <spec-slug>`** — one small unit of behaviour change (a `/ticket` or `/chore`
  output, a handful of files). Cheapest rung. In place, no review, no PR.
- **`sdlc-flow <spec-slug>`** — a whole spec wanting a consolidated review, a docs pass, and a PR.
  The default for anything not clearly small.
- Recommends `sdlc-run`/`sdlc-block` → stop and report (rule 2).

**Isolation.** Both engines default to plain-branch/in-place; `--worktree` opts into an isolated
sparse-checkout worktree. Worktrees are **safe in brain-vaulted repos** — the engines detect a
symlinked `planning/` and resolve it (D46), and `/init-worktree` was fixed to match
(`BT.ticket.init-worktree-symlink-repair`, closed). Plain branch is simply *cheaper*, not safer.

Use `--worktree` when:
- **The repo owns the engines it is running — `base-template` ALWAYS.** A chain there edits
  `.claude/workflows/sdlc-*.js` *while those engines are executing the chain*. Without isolation a
  block's edits land in the working tree the next block's engine loads from, so a mid-chain change
  silently alters how the rest of the chain runs. The worktree keeps each block's engine edits
  quarantined until you merge them deliberately.
- The change is risky enough to want quarantined until reviewed.
- A `.env` or other untracked file is needed at runtime → check it copied; if not, prefer plain
  branch for that block.

`--worktree` / `--no-worktree` on the command line overrides all of the above.

### 6. Launch the engine — do not wait idly
Invoke the workflow **in this session**:
- `sdlc-task <spec-slug> [--worktree]`
- `sdlc-flow <spec-slug> --auto-merge [--worktree]` — prefer `--auto-merge` in a chain so an open
  PR does not block the next block. Drop it when the change deserves a look first.

It returns a task ID immediately. **Now go back to step 4 for the next un-specced blocks** and keep
generating specs until either the notification arrives or you are out of blocks to prepare.

### 7. On the completion notification
If the engine **bailed** (triage MAJOR, immediate-bail, review FAIL after its bounded retries):
- `--stop-on-fail` (default) → stop the chain. Report which block, why, and the remaining chain.
- `--continue-on-fail` → record it, leave the block `open`, continue. **Never mark a bailed block
  closed.**

### 8. Integrate, then verify the state write

**Integrate:**
- In-place `sdlc-task` → nothing to merge.
- `sdlc-flow --auto-merge` → confirm the PR actually merged and the branch is gone.
- `sdlc-flow` without it → merge the PR, then delete the branch.
- Any `--worktree` run → **`/clean-worktree <spec-slug>`** (or the literal worktree name the engine
  printed, including any `-2`/`-3` suffix).
- **Merge conflicts are yours.** Resolve toward the incoming block's intent, re-run the gating
  suite, and record what you resolved. If two blocks genuinely disagree about the same behaviour,
  stop the chain and report — even under `--continue-on-fail`.

**Verify the state write — never skip this** (rule 5). Check:
- `planning/state.json` → the block's `status` is `closed`
- `planning/<spec-slug>/tasks.md` → checkboxes match what ran
- `planning/status.md` → regenerated, not stale

If any is wrong: set `status` to `closed`, then run **`mev emit-state --write`** and
**`mev validate-brain --state`** (expect 0 errors). **Record every repair** — a pattern of them is
evidence for that open ticket.

### 9. Check downstream consumers — only for blocks that touched a shared crate's public types

A lane cannot see the sibling lanes running in other repos. Twice now that has caused a real
cross-repo break mid-run: okf-core's `OK.3.B` added a non-`Option` field to six shared structs and
broke both `mev` (101 sites) and `bastion` (31 sites) in test code that `cargo build` cannot see;
mev's D58 removed a public constant and broke `engine-rs`'s workspace compile. This step exists to
catch that class **before** the next block in a *different* lane hits it, not to police every
block in this one.

**Fires only when** the block just integrated changed a public type, field, or removed/renamed a
public symbol in a crate other repos depend on via a `path = "../..."` Cargo dependency (e.g.
`okf-core`, `mev`, `engine-rs`'s `engine-contract`, `claude-code-rs`). Skip silently for
non-Rust blocks, blocks with no public-surface change, and blocks in a repo nothing else path-depends
on — the cost is a cold build per consumer, so do not run it for every block.

Find consumers by grepping the fleet for `path = "../<this-repo>"` (or the specific crate name) in
every other repo's `Cargo.toml`. For each one found:

```
git -C <consumer> status --porcelain          # non-empty → SKIP, report SKIPPED-DIRTY
CARGO_TARGET_DIR=$(mktemp -d) cargo test --no-run --locked \
    --manifest-path <consumer>/Cargo.toml
```

Each flag earns its place — do not simplify this away:
- **`--locked`** — refuses to rewrite the consumer's `Cargo.lock`, turning a silent mutation into a
  useful error instead of leaving an uncommitted diff in a repo you don't own. This exact mutation
  happened during manual verification on 2026-08-04.
- **`CARGO_TARGET_DIR=$(mktemp -d)`** — no `target/` lock contention with whatever else might be
  building in that repo, no incremental-cache churn. Costs a cold build; that is the price of not
  interfering.
- **dirty check first** — never blame your shared-crate change for someone else's half-written
  code; a dirty consumer is not evidence of anything.
- **`cargo test --no-run`, never `cargo build`** — the entire `E0063` class (missing struct fields)
  is invisible to `build`; only test code constructs the affected literals.

**Report only. Never fix another lane's repo** and never run this against a repo with an active
worktree lane of its own — a plain `cargo build`/`cargo test` in a repo mid-chain can mutate its
`Cargo.lock` out from under that lane. If a consumer fails, add it to the final report as a new
**BROKEN DOWNSTREAM** line (repo, error class, one-line fix estimate) — do not open a fix block for
it yourself; that is the operator's call, same as a `HELD` block.

### 10. Re-check the next block's dependencies, then launch it
Cheap, and it catches anything that changed outside the chain. Then return to step 6.

### 11. Repeat until the chain is done or stopped.

---

## Final report

A table, one row per block: `position · block ID · spec slug · engine · isolation · outcome ·
state verified (clean / repaired) · commit or PR`.

Then explicitly:
- **HELD** blocks and what each waits on.
- **State repairs** you made, and where.
- **Merge conflicts** you resolved, and how.
- **BROKEN DOWNSTREAM** — any consumer repo step 9 found broken by this chain's changes (repo,
  error class, one-line fix estimate). Empty is the expected case; say so rather than omitting
  the line.
- **The remaining chain** if you stopped early — as a paste-ready `/orchestrate` invocation.
- A reminder to run **`/log-work`**: `sdlc-task`'s bookkeep is deliberately lean and writes no
  `log.md` entry, so a chain of tasks leaves no narrative history without it.

## Notes

- **This command does not decide what to work on.** Order comes from the operator or the roadmap;
  readiness comes from the `depends_on` graph. If the order looks wrong, say so — do not silently
  reorder.
- **The generated board is the authority on readiness**, not any hand-written wave table. A block
  showing `blocked` in `planning/status.md` is not startable, whatever a roadmap says.
- **Lanes only interact through cross-repo `depends_on` edges**, which step 2 already checks. Run up
  to four repos at once.
