---
type: Command
title: generate-roadmap — Author a multi-repo roadmap and its /orchestrate lane files
description: Turn a body of findings, open blocks and operator decisions into a roadmap document plus per-repo lane chain files that /begin-orchestration can drive concurrently, with the concurrency budget, cross-lane edges, operator gates and registration gate made explicit.
---
# Generate Roadmap — author a multi-repo run and the lanes that execute it

Produces the two things `/begin-orchestration` consumes: a **roadmap document** and one
**`lane-<name>.txt` chain file per lane**. It does not run anything.

A roadmap is not a list of what would be nice to do. It is a **concurrency plan** — an assignment of
work to parallel `/orchestrate` sessions that cannot step on each other, behind a definition of done
that can be checked by observation rather than asserted.

**Related:** `/generate-master-plan` authors *one repo's* canonical block definitions. This command
sits above it and spans repos. `/begin-orchestration` drives one lane of the result.

## Variables

`$ARGUMENTS` — a concept name or slug, plus optional flags.

| Flag | Required | Default | What it does |
|---|---|---|---|
| `<slug>` | **yes** | — | Roadmap slug. Becomes `planning/<slug>/`. Kebab-case, names the *outcome* not the date. |
| `--from <path ...>` | no | — | Source documents: a review, an audit, an action register, a previous roadmap. Repeatable. |
| `--supersedes <path>` | no | — | The roadmap this replaces. Adds the banner to both documents. |
| `--lanes <n>` | no | `4` | Target concurrent lanes. The real ceiling is operator capacity, not repo count. |
| `--dry-run` | no | off | Print the lane assignment and cut list; write nothing. |

Empty `$ARGUMENTS` → print usage and stop.

```
Usage: /generate-roadmap <slug> [--from <path> ...] [--supersedes <path>]
                         [--lanes <n>] [--dry-run]
```

---

## Step 1 — Resolve and scope

**A. `BRAIN_ROOT`** — walk up from cwd for `brain.toml`. This command runs at HQ. A roadmap spanning
repos cannot be authored from inside one of them.

**B. `roadmap_dir`** = `<BRAIN_ROOT>/planning/<slug>/`. If it exists and holds a `roadmap.md`, stop
and ask whether to supersede or amend — never overwrite one.

**C. Read every `--from` source in full.** Not summaries of them.

**D. Read the superseded roadmap's outcome**, if any. Two questions, both mandatory:
what did it *achieve*, and what did it *not* — the second is the reason this one exists and belongs
in the opening paragraph.

---

## Step 2 — Inventory, and re-verify before you plan on it

Collect candidate work from, in this order of trustworthiness:

1. **`state.json` across the fleet** — every `open`/`blocked` block. This is the graph and it is fact.
2. **Carryovers past their staleness threshold** — `mev carryover`, or `validate-brain --state`.
3. **Findings in the `--from` documents** that have no block. These become ★ items (Step 6).
4. **Operator decisions** named anywhere as gating something.

> **Re-verify the load-bearing claims before building on them.** In this fleet a formal review was
> wrong in six cited places within five days, and the follow-up built on it was wrong in three more.
> Nothing had been sloppy — the system moved. A roadmap inherits every stale claim in its sources
> and then multiplies it by four concurrent lanes.
>
> Cheap and mandatory: for each claim that determines a lane's shape, run the one command that
> checks it. "The CI is red" — look at the run list. "The name is available" — query the registry.
> "The gate is not wired" — read the `harness.json`. Record what changed; a killed claim is a
> finding, and the correction belongs in the roadmap's Wave 0 so nothing downstream cites it.

---

## Step 3 — Choose the outcomes, then cut everything else

**Three to five outcomes, each stated as something that becomes true**, not as an area of work.
"The demo is live and browser-verified" is an outcome. "Demo hardening" is a theme, and themes do
not terminate.

Then write the **cut list, and make it long.** Every substantial candidate that is not in an outcome
gets a row and a reason. This is the highest-value section of the document and the one most often
skipped: an unstated cut reads as an oversight, gets re-proposed next roadmap, and re-litigated.
A stated cut is a decision with a date on it.

Cut aggressively on this rule: **if no outcome depends on it, it is out, no matter how good it is.**

---

## Step 4 — Assign lanes

### The lane unit is the repo, never the wave

`/orchestrate` drives **one repo per session, engines serial inside it**. Two blocks in the same
repo can never run in parallel however a wave grid groups them. So:

- A repo holding 10 blocks **is the critical path**, regardless of what is scheduled beside it.
- "N concurrent agents" means N sessions in N *different* repos.
- Balance lanes by the *longest repo chain*, not by block count.

### The heavy budget is the real constraint

**At most two heavy-gate repos concurrently.** Heavy = its `planning/harness.json` gates include a
browser or a full production build (Playwright, `next build`) — or a very large native build.
Determine this by **reading each repo's `harness.json`**, not from memory.

If the work has three heavy repos, the third lane **opens on a light repo and reaches the heavy one
later**, and the roadmap says so in the lane table. Do not simply hope the operator sequences it.

> Nothing enforces this ceiling. It is prose in the roadmap and nowhere else — say so in the
> document rather than implying a machine checks it.

### Isolation is policy, not preference

| Repo | Isolation | Why |
|---|---|---|
| `base-template` | **`--worktree` always** | It owns `.claude/workflows/sdlc-*.js`; a chain there edits the engines while they execute it. |
| the brain root (HQ) | **`--no-worktree` always** | `validate-brain` in a worktree resolves the gitignored sub-repos against the worktree's own `brain.toml` — measured 64 structure / 601 state errors versus 0/0 in the main tree. |
| everything else | `--no-worktree` | Cheaper. Use `--worktree` when a change deserves quarantine. |

**If `base-template` is in the roadmap, decide its propagation timing explicitly.** Its work must
land early (every other lane runs on those engines) but `/sync-downstream-harness` must not run
while any lane is live — a mid-flight sync has already swapped a running lane's engine underneath
it. The resolution is always the same: **land in the worktree early, defer propagation to an
operator gate at the end.** Write both halves into the lane file.

---

## Step 5 — Find the cross-lane edges, and only those

A cross-lane edge is a place a lane must **wait on a different repo**. Everything else is sequential
inside one repo and needs no coordination.

For each edge, name: source block → target block, and **what breaks without it**. An edge whose
consequence you cannot state is usually not an edge.

Draw them as ASCII in the roadmap. Six to eight edges is normal for four lanes; twenty means the
lane split is wrong and should be redrawn.

**Operator gates are edges too.** A block waiting on a DNS record or a human read-through is
blocked exactly as hard as one waiting on a sibling repo — and unlike a code dependency, nothing in
the graph models it. Name every one in the lane file *and* in the operator table, with the block it
gates. The two gates that will actually bite are worth calling out by name; in practice they are
the ones that must happen mid-run and get deferred to deploy time instead.

---

## Step 6 — Wave 0, the registration gate

**`/orchestrate` resolves block IDs from `state.json`.** A lane file naming an ID that is not in the
graph does not degrade gracefully — the lane stops, or worse, improvises a spec.

So every ★ item from Step 2.3 must be **filed as a ticket and registered in its repo's
`state.json` before any lane launches.** Make that Wave 0 and say it is a hard gate.

Wave 0 also carries:
- Any **claim correction** from Step 2's re-verification, before a downstream lane cites it.
- The **operator ratifications** that gate a lane's first block.
- `mev emit-state --write`, then commit every touched `state.json` with an explicit pathspec.

A roadmap whose sequence table is empty before Wave 0 is correct and should say so. A *populated*
table is the signal the lanes may launch.

> **Registration is not optional bookkeeping.** Tickets filed on disk but absent from `state.json`
> are invisible to the board, to the generated sequence table, and to `/attention`. This has already
> happened once here: six tickets about drift, filed where the drift detector could not see them.

---

## Step 7 — Write the files

### `planning/<slug>/roadmap.md`

OKF frontmatter (`type: Plan`, `status: active`, `related:` pointing at the sources and the
superseded roadmap). Then, in order:

| Section | Content |
|---|---|
| Supersedes banner | What the previous roadmap achieved, what it did not, and **whether its folder may be archived** — if any of its documents are still referenced, say so explicitly so nobody archives them |
| The trade | Why this work, now. Lead with the finding that motivated it, with evidence |
| The outcomes | Three to five, each an observable statement |
| How to use this document | The generated table is authoritative; lane tables are execution order; ★ means filed in Wave 0 |
| Wave 0 | The gate. A table of registration, corrections and operator ratifications |
| Dependency graph | ASCII lane chains, then the cross-lane edges |
| The lanes | One table per lane: block, engine, and a **notes column that carries the evidence** — file:line, `AR-nn`, the trap, the thing the last run got wrong |
| Isolation and CPU budget | The policy table plus the two-heavy rule |
| Operator lane | Every gate, what it gates, and enough detail to act without re-reading a source doc |
| Coverage crosswalk | **Required whenever `--from` includes a runbook or action register.** One row per source item → where it lands. See below |
| What is cut, and why | Long. One row per cut candidate |
| Definition of done | See below — this is the section that decides whether the roadmap worked |
| Sequence | The generated region, between the markers |
| Live board · Lane log | Pointers |

**The coverage crosswalk is not documentation — it is the check.** A roadmap built from an action
register absorbs 30–60 discrete items and re-homes them into lanes. Items do not get dropped by
decision; they get dropped by *reorganisation*, and a prose roadmap gives you no way to notice.
Write one row per source item → its destination, then **verify it mechanically** before handing over:

```bash
C=$(cat roadmap.md lane-*.txt)
for ref in $(grep -o 'AR-[0-9A-Z]*' <source>.md | sort -u); do
  echo "$C" | grep -q "$ref" || echo "MISSING $ref"
done
```

A citation-style ref (`AR-nn`, `OPEN-n`) in the source makes this a one-liner, which is a good
reason to insist sources carry them. For items without a ref, grep a distinctive string from each.

**A row with no destination is a bug in the roadmap, not a decision.** If something should be
dropped, it goes in the cut list with a reason — that is a different row, and a deliberate one.
Real result of running this check on its first roadmap: four items had silently fallen out, two of
them operator infrastructure jobs that had been collapsed into a single link.

**Definition of done must be written as observations.** Not "block X closed" — a block closes when
its spec is satisfied, which is not the same as the capability working. Prefer a command and its
expected output:

```
✅  curl https://<site>/<path> returns the body AND `utm_source` in the HTML
✅  `npx playwright test` runs to completion
✅  `cargo add <crate>@<version>` compiles in a scratch project outside this fleet
❌  BW.10.F closed
❌  the funnel is instrumented
```

This is the single most valuable rule in this command. A previous roadmap closed 30 of 53 blocks and
still shipped a demo nobody had loaded in a browser, a funnel no lead had traversed, and six
capabilities wired into nothing — because every one of its DoD items was a block, not an observation.

**The generated region, verbatim, and never hand-edited afterwards:**

```markdown
<!-- BEGIN generated:epic-sequence -->
<!-- END generated:epic-sequence -->
```

`mev emit-state --write` fills it from `state.json`. Do **not** author a wave table beside it. The
last roadmap that did accumulated a second "Revised Wave Table" while the first was still marked
authoritative, so the document carried two contradictory plans plus a generated table that outranked
both. **A wave grid is a communication device, not a schedule.**

### `planning/<slug>/lane-<name>.txt`

One per lane. `<name>` must match what an operator would type after `--lane`.

Required header — `/begin-orchestration` **cross-checks the `# ROADMAP:` line against its own
`--roadmap` flag and stops if they disagree**, which is the cheapest available check that the lane
was pointed at the right run:

```
# Lane <X> · <repo-or-theme> — <one line on what this lane is for>
# ROADMAP: <absolute path to roadmap.md>
# LOG:     <absolute path to lane-log.jsonl>
#
# RUN FROM <dir> :
#   /begin-orchestration --roadmap <rel path> --lane <name>
#
# ISOLATION: <flag> — <why>
# BUDGET: <heavy/light, and what it may not run beside>
```

Then the traps, holds and spec sources as comments, then **bare block IDs, one per line, in
execution order**. Blank lines and `#` comments are stripped by the reader.

Three things belong in these comments and nowhere else, because they are read at the moment of
execution rather than at planning time:

- **Every HELD block**, with the exact sibling block it waits on and why.
- **Every spec source that is not master-plan slug mode** — `/generate-tasks --from <path>`. A lane
  that cannot resolve a spec improvises one.
- **The traps that have cost a real run in that repo.** Not general advice; specific, cited, and
  ideally with the failure it caused.

A lane file covering several repos uses section markers and says **"take only your repo's section"**
at the top — the reader stops and asks if it cannot tell which section is its own.

### `planning/<slug>/lane-log.jsonl`

Create it empty. Append-only, one line per integrated block. Four sessions editing one markdown
file is the contention pattern this structure exists to avoid.

### `planning/index.md`

Add the folder (standing rule 7). If superseding, mark the old folder's row — and if its documents
are still referenced, write **"NOT archived"** on that row with the reason.

---

## Step 8 — Verify before handing over

```bash
bastion validate-brain --okf-structure   # one invocation per flag; they do not compose
bastion validate-brain --links
bastion validate-brain --state
```

Then check by hand:

- [ ] Every block ID in every lane file exists in a `state.json`, **or** is marked ★ and appears in Wave 0.
- [ ] No lane has more than one heavy repo live at a time, given the stated ordering.
- [ ] Every cross-lane edge in the ASCII appears in the lane file of the *waiting* lane.
- [ ] Every operator gate names the block it gates, in both the operator table and the lane file.
- [ ] **The crosswalk check above runs clean** — every ref in every `--from` source appears in the
      roadmap or a lane file, or has a cut-list row.
- [ ] **No multi-step operator sequence is collapsed into a single link.** A runbook referenced as
      one row loses its steps. Break it out; two of its items probably touch live traffic.
- [ ] Every Definition-of-done item is an observation with a command, not a block ID.
- [ ] The `# ROADMAP:` line in each lane file resolves to this roadmap.
- [ ] The cut list is longer than you are comfortable with.

Report the lane assignment, the Wave 0 item count, and the cut list. **Do not run `/orchestrate`** —
this command authors; `/begin-orchestration` executes.

---

## Traps

- A piped command's exit code is the **pipe's**, not the command's. Redirect, then check `$?`.
- `rg`/`find` are symlink-blind and every `planning/` is a symlink into a `_planning/` vault — pass
  `-L`, and `-uu` to reach gitignored sub-repos. An inventory sweep without them is not trustworthy.
- `planning/state.json` round-trips with `json.dump(..., indent=2, ensure_ascii=False)` plus a
  trailing newline. The default escapes every em dash and turns a small edit into ~130 lines of churn.
- **HQ commits need an explicit pathspec** — `git commit -o <paths>`. Every repo's `planning/` is a
  symlink into the one HQ git index, so a bare commit sweeps other sessions' staged work in.
- `timeout` does not exist on this macOS shell.
- Block IDs must be allocated by reading the canonical `state.json`, not `status.md` or
  `master-plan.md` — narrative files lag and produce ID collisions. One repo already carries two
  unrelated "Phase 4"s from exactly this.
