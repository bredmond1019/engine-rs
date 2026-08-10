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

1. **Never do block work yourself, and never delegate it to a subagent.** Every block in the chain
   goes through **`/sdlc-task` or `/sdlc-flow`** — those engines spawn their own internal agents,
   which is theirs to do. A block implemented by an ad-hoc subagent has no spec, no gate, no state
   write, and no review; it is indistinguishable from work that never happened, and the chain's
   verification in step 8 will not catch it because the state write will look fine.

   You may spawn a subagent for **exactly two things**: read-only exploration (finding files,
   answering a factual question about the codebase), and a long hotfix that has **no block of its
   own**. Everything else — `/generate-tasks`, `/breakdown`, integration, state verification,
   conflict resolution — runs **inline in this session**.

   If you find yourself about to write code for a block ID, stop: that is an engine's job.
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

7. **Commit immediately after any `mev` command or roadmap edit.** `mev emit-state --write`,
   `set-block-status`, `defer-epic`/`resume-epic`/`sync-epics`, and any roadmap or plan edit all
   mutate files that **sibling lanes read**. An uncommitted state change is invisible to them and
   will be clobbered by the next agent that writes the same file. Commit the `state.json` plus its
   regenerated surfaces as their own commit *before* launching the next engine — not batched at the
   end of the chain.

8. **Report progress where sibling lanes can see it.** After each block integrates, append one line
   to the run's lane log and commit it:

   ```
   {"ts":"<ISO-8601>","lane":"<lane-name>","repo":"<repo>","block":"<ID>","status":"closed|bailed|held","note":"<one line>"}
   ```

   The log lives beside the roadmap driving the run (e.g.
   `planning/demand-ready/lane-log.jsonl`); if the chain has no roadmap, skip it.

   **Do not hand-edit a roadmap's generated regions.** Run `mev emit-state --write` and let the
   sequence table regenerate from `state.json`, which is the authority. Four concurrent sessions
   editing one markdown file is the exact contention pattern this fleet has already been bitten by —
   the working rule is *each agent reports the state change it wants; one writer applies them
   centrally*. Per-repo `state.json` writes do not contend because they are different files; the log
   is append-only; the roadmap regenerates. That is the whole communication channel.

9. **Keep a running notes file — `planning/orchestration-run/notes.md` in this repo.** The lane log
   carries one line per block for *sibling lanes*; this file carries everything else, for the *next
   session in this repo*. Defects found in passing, deferred fixes, decisions you took, traps
   re-confirmed, whatever the roadmap got wrong. None of it survives the session transcript
   otherwise, and the next agent starts blind and rediscovers it the hard way.

   Create it on the first block if absent (OKF frontmatter, `type: Reference`; add a row to
   `planning/index.md`). **Append after every block — never rewrite.** Status every item so it can
   be triaged later: `OPEN` · `DONE` · `HELD` · `WONTFIX`. Commit it alongside the lane-log line
   (rule 7 timing: before the next engine launches).

   Keep it a *log*, not a second `status.md`. If an item turns into real work it becomes a ticket
   and the entry points at it.

10. **Resolve what you can; record the call.** A chain that halts at every ambiguity is worthless,
    and one that halts at none is dangerous. Decide the ordinary things inline — an imperfect spec
    slug, which plan file `--from` means, whether a surfaced defect is in scope, how to resolve a
    merge conflict — state the assumption, and keep the chain moving. **Every such decision goes in
    the notes file with its reasoning, in a line or two.** A decision nobody can find later is
    indistinguishable from a mistake.

    Still not yours to decide alone: a bailed block's fate under `--stop-on-fail`, two blocks that
    genuinely disagree about the same behaviour, a `BROKEN DOWNSTREAM` consumer (report, never fix),
    an operator gate, and anything requiring a spec slug you cannot resolve confidently (step 3
    says stop and ask — that still stands).

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

**Two authoring-time rules for any spec or OKF frontmatter this step produces or edits** —
generalized from a lane that hit both in one day: a `related:` target must resolve to a real
`doc_id` on a document that has actually been crawled, never a carryover slug or an invented id
— an unresolved edge red-gates the whole corpus for every concurrent lane when `--graph` gates,
not just the authoring one. And a `validation_command` must be scoped to the task's own changes,
never the whole working tree (e.g. never a working-tree-wide `git diff | grep` guard) — a
tree-wide guard can never pass in a shared index with concurrent lanes and bails the block on an
unrelated lane's uncommitted files.

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

**Two repos have a non-negotiable answer. Encode them, do not re-derive them per run:**

| Repo | Isolation | Why |
|---|---|---|
| `base-template` | **`--worktree` ALWAYS** | See above — a chain there edits the engines running it. |
| the brain root (HQ) | **`--no-worktree` ALWAYS** | Carryover `hq-specs-cannot-run-in-a-worktree`. Measured 2026-08-04 inside a real branch worktree: `validate-brain --structure` gave **64 errors** and `--state` **601**, against 0/0 in the main tree. `validate-brain` walks up to the worktree's own `brain.toml` and resolves the 17 sub-repos relative to it — and every sub-repo is gitignored, so absent from any checkout. Worktree creation itself is clean; it is specifically the corpus gates that cannot pass. Same root cause as the CI exclusion in D65. |

`--worktree` / `--no-worktree` on the command line overrides all of the above **except those two** —
if a flag contradicts the table, stop and report rather than running a chain whose gates cannot pass.

**Concurrency across sessions is enforced mechanically, not by human memory.** Rule 3 governs one
repo; nothing stops four sessions launching `playwright` and `next build` simultaneously on their
own. `scripts/fleet_concurrency_check.py` lives in the `base-template` checkout (the fleet's shared
harness source, typically a sibling directory at the brain root, e.g. `../base-template` — resolve
its actual path for this machine rather than assuming). Before starting a heavy repo
(browser/production-build checks — determine this by reading the target repo's own
`planning/harness.json`, never from memory:
`python3 <path-to-base-template>/scripts/fleet_concurrency_check.py is-heavy --repo-path <target-repo>`),
register it:
`python3 <path-to-base-template>/scripts/fleet_concurrency_check.py register --repo <name>`.
Exit code `3` (or `"allowed": false` in the JSON output) means the fleet is already at capacity
(`MAX_HEAVY_LANES = 2`) — put this repo on a cheap-gate block instead, or wait. Release the slot
when the heavy repo's chain finishes: `... release --repo <name>`. A stale entry (a killed lane, or
one past the TTL) expires automatically on the next registration, so a dead lane never blocks the
fleet permanently. If the lock store itself is unavailable (no brain root found, unwritable), the
script reports `"degraded": true, "allowed": true` — same as today's unenforced-prose behavior, not
a new way to fail. See `planning/decisions/D61-fleet-concurrency-enforcement.md` for the full design.

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

If the engine did **not** bail but `sdlc-flow`'s return has `stranded: true` — a `PASS` verdict
that ended with no PR opened and (under `--auto-merge`) no merge, because the PR stage was
attempted and either errored or could not be independently verified via `gh pr view` — **treat it
the same as a bail for chain purposes**: it is a completed-looking run whose work never actually
landed anywhere the next block can build on.
- `--stop-on-fail` (default) → stop the chain. Report the block, `prOutcome` (`'failed'`) and
  `state.pr`/the branch name so the operator can open the PR manually, and the remaining chain.
- `--continue-on-fail` → record it, leave the block `open`, continue — same as a bail. **Never
  treat a `stranded: true` run as integrated;** the next block would be building on a base missing
  this one's work.
- `prOutcome: 'impossible'` (no `gh` / no remote) is **not** `stranded` and needs no special
  handling here — that is the standalone-repo degradation path working as intended; the branch is
  intact and ready for a manual PR whenever the operator wants one.

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

**Then check the corpus, then commit, then report** (rules 7, 8 and 9):

```
./scripts/validate_brain.sh          # from the brain root — delta against the last good push
```

Concurrent lanes pushing into one corpus is exactly the condition that accumulated 32
`validate-brain` errors across four lanes on 2026-08-04 and blocked `git push` fleet-wide. Rule 6
checks downstream *code* consumers; nothing else checks the *corpus*, so this belongs here.

Commit the `state.json` and its regenerated surfaces as their own commit, then append **both** the
lane-log line and this block's `planning/orchestration-run/notes.md` entries (rule 9 — including any
decision you took under rule 10) and commit those together. **Only then** launch the next engine.

> **`planning/state.json` is written with `ensure_ascii=False`.** If you edit it with a script,
> round-trip with `json.dump(..., indent=2, ensure_ascii=False)` plus a trailing newline. Using the
> default `ensure_ascii=True` escapes every em dash and turns a 3-field edit into ~130 lines of
> churn — which becomes a conflict for every sibling lane.

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
CARGO_TARGET_DIR=$(mktemp -d) cargo nextest run --no-run --locked \
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
- **`cargo nextest run --no-run`, never `cargo build`, never plain `cargo test`** — the entire
  `E0063` class (missing struct fields) is invisible to `build`; only test code constructs the
  affected literals, so a compile-only test build is still required. Plain `cargo test` is
  **denied fleet-wide by a `PreToolUse` hook** — see `core/mev/.claude/settings.json`, which
  matches `cargo\s+test(\s|$)` on any Bash command and returns `permissionDecision: deny` unless
  the command contains `cargo nextest` or is prefixed `NEXTEST_POLICY_OVERRIDE=1`.
  `cargo nextest run --no-run` compiles the same test targets and is not denied.

**Report only. Never fix another lane's repo** and never run this against a repo with an active
worktree lane of its own — a plain `cargo build`/`cargo nextest run` in a repo mid-chain can mutate
its `Cargo.lock` out from under that lane. If a consumer fails, add it to the final report as a new
**BROKEN DOWNSTREAM** line (repo, error class, one-line fix estimate) — do not open a fix block for
it yourself; that is the operator's call, same as a `HELD` block.

**Concurrent cargo runs in sibling repos can contaminate captured output.** Observed once during
the audit: a `mev` build capture returned `engine-rs`'s test summary — another lane's build was
writing to the terminal or a shared capture at the same time. A surprising result (an unexpected
PASS or an unexpected failure class) from this step is not trustworthy on its own when other lanes
are active concurrently. Mitigation: if the result looks surprising, re-run the capture in
isolation (no other lane's cargo command in flight) before reporting it as **BROKEN DOWNSTREAM** or
as a clean pass.

### 10. Re-check the next block's dependencies, then launch it
Cheap, and it catches anything that changed outside the chain. Then return to step 6.

### 11. Repeat until the chain is done or stopped.

---

## Traps

- `rg`/`find` are symlink-blind and every `planning/` is a symlink into a `_planning/` vault — pass
  `-L`. At the brain root every sub-repo is also **gitignored**, so `-L` alone still skips them all
  — pass `-uu` too. A sweep reporting "clean" without both is not trustworthy. See
  `begin-orchestration.md`'s Traps section for the same rule stated for that command.

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
- **Decisions you took** under rule 10, each with its one-line reasoning — and confirmation they
  are in `planning/orchestration-run/notes.md`, not only in this report.
- **Open items** the run surfaced but did not fix, as recorded in the notes file (defects found in
  passing, deferred propagation, anything needing its own ticket).
- **The remaining chain** if you stopped early — as a paste-ready `/orchestrate` invocation.
- A **terminal `planning/orchestration-run/review.md`** — required, not optional. It is a
  plain-English summary of what this chain changed plus the hand-verification recipes an operator
  would run to confirm it. Every recipe in it must have been **executed at least once by this
  session before the file is written**, and the file must say so explicitly (e.g. "ran, output:
  ...") — an authored-but-unrun recipe reads as verification while being a guess, which is worse
  than no recipe at all. Naming, frontmatter, and lifecycle follow
  `planning/decisions/D57-orchestration-run-artifact-contract.md`; do not restate that contract
  here.
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
