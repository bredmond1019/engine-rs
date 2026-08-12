---
type: Command
title: begin-orchestration — Open one lane of a multi-repo roadmap run and drive it through /orchestrate
description: Brief yourself from a roadmap and lane file, resolve engine/isolation policy, then drive this repo's chain through /orchestrate with the concurrency, reporting, and operator-gate rules enforced.
---
# Begin Orchestration — Open one lane of a multi-repo roadmap run

Wraps `/orchestrate` with the context a lane agent needs and the rules a concurrent run depends on.
`/orchestrate` knows how to drive a chain; it does not know *which* chain, *why*, what may not be
delegated, or who else is running right now. This command supplies that, then hands off.

**One `/begin-orchestration` session drives one repo.** Run several at once — that is the lane model.

## Variables

`$ARGUMENTS` — flags, any order. **`--roadmap` is required, plus one of `--lane` or `--blocks`.**
Everything else resolves to a default.

`--roadmap` is mandatory on purpose. An earlier version inferred it from whichever epic was
`focused`, which is correct during a single-initiative week and silently wrong the moment two
initiatives overlap — the case where a lane driven against the wrong roadmap is hardest to notice.
Naming it costs one flag and removes a hidden coupling to epic status.

| Flag | Required | Default | What it does |
|---|---|---|---|
| `--roadmap <path>` | **yes** | — | The roadmap this lane belongs to. Absolute, or relative to `BRAIN_ROOT`. |
| `--lane <path\|name>` | one of | — | Lane chain file. A bare name (`gtm`) resolves to `<roadmap-dir>/lane-<name>.txt`. |
| `--blocks <id ...>` | one of | — | Inline block IDs instead of a lane file. Space- or comma-separated. |
| `--repo <slug>` | no | inferred from cwd | Override only when inference is wrong. |
| `--isolation <worktree\|no-worktree\|auto>` | no | `auto` | `auto` applies the policy table below. |
| `--plan-file <path>` | no | — | Spec source for `/generate-tasks --from`, when the blocks are not in `master-plan.md`. |
| `--engine <task\|flow>` | no | per-block | Force one engine for the whole chain. |
| `--log <path>` | no | `<roadmap-dir>/lane-log.jsonl` | Where to report integrated blocks. `--log none` disables. |
| `--execute` | no | off | Skip the dry-run confirmation and start immediately. |
| `--continue-on-fail` | no | off | Passed through to `/orchestrate`. |

Empty `$ARGUMENTS` → print usage and stop:

```
Usage: /begin-orchestration --roadmap <path> --lane <path|name> [--repo <slug>]
                           [--isolation worktree|no-worktree|auto] [--plan-file <path>]
                           [--engine task|flow] [--log <path>] [--execute]
                           [--continue-on-fail]
       /begin-orchestration --roadmap <path> --blocks <id> [<id> ...] [same optional flags]
```

`--roadmap` missing → print usage and stop. Do **not** infer it, and do not offer to; if the
operator does not know which roadmap this lane belongs to, that is the thing to resolve first.

---

## Step 1 — Resolve

**A. `BRAIN_ROOT`** — walk up from cwd for `brain.toml`.

**B. The repo** — this repo's `planning/state.json` → `repo`. `--repo` overrides. If cwd *is*
`BRAIN_ROOT`, the repo is the brain (HQ).

**C. The roadmap** — `--roadmap`, resolved against `BRAIN_ROOT` if relative. It must exist and it
must be a roadmap; a path that resolves to a lane file or a `tasks.md` is an argument error, not
something to work around. **Never infer it.**

**D. `roadmap_dir`** = the roadmap's directory.

**E. `run_record_dir`** = `planning/orchestration-run/<roadmap-slug>/` in **this repo**, where
`<roadmap-slug>` is `roadmap_dir`'s directory name (from D). Create the directory if absent; if it
already exists, **append** to its `notes.md` / `review.md` rather than creating new ones. **No
rotation, no archive move, no dated filenames, no crash window** — a record is addressed by
`(repo x roadmap)`, never by time. The required frontmatter (`roadmap`, `lane`, `run_started`,
`run_ended`, `lifecycle`), the `doc_id: <repo-slug>-orchestration-run-<roadmap-slug>` rule, and the
ledger's `origin_roadmap` column are specified once, in Rule 5 below, per
`planning/decisions/D57-orchestration-run-artifact-contract.md` — **cited as the deciding
authority; not paraphrased here.** Print `run_record_dir` alongside the other Step 1 resolutions.

**F. The chain** — `--blocks` verbatim, or the lane file:
- `--lane <path>` → that path.
- `--lane <name>` (bare) → `<roadmap_dir>/lane-<name>.txt`. Missing → stop and list what
  `lane-*.txt` files do exist there.

**G. Cross-check** — if the resolved lane file carries a `# ROADMAP:` header and it disagrees with
the roadmap resolved in C, **stop and report both.** That mismatch means the lane belongs to a
different run than the one you were told, and it is the cheapest available check that `--roadmap`
was typed correctly.

Read the lane file with `#` comments and blanks stripped **when extracting the chain / block list**;
file order is execution order. Lane files may cover several repos in one running order — **take only
your repo's section.** If the file has section markers for other repos and you cannot tell which is
yours, stop and ask.

**Comments are binding context for your own section, not decoration.** Stripping applies only to
parsing the block list out of the file. The prose in your section's `#` comments — isolation
rationale, prior-run gotchas, "do not touch X" — is the per-repo briefing for this run and must be
honored exactly like the block list itself. `carryover-improvements`' lane files carry their entire
per-repo briefing this way; an agent that strips comments before reading drops all of it.

Print what you resolved. A lane driven against the wrong roadmap is worse than one driven against
none.

## Step 2 — Isolation policy

`--isolation auto` resolves as:

| Repo | Isolation | Why |
|---|---|---|
| `base-template` | **`--worktree`, always** | A chain there edits `.claude/workflows/sdlc-*.js` *while those engines are running it*. |
| the brain root (HQ) | **`--no-worktree`, always** | `validate-brain` inside a worktree resolves the gitignored sub-repos against the worktree's own `brain.toml` and they are absent from any checkout. Measured: 64 structure / 601 state errors versus 0/0 in the main tree. Worktree creation is clean — it is the corpus gates that cannot pass. |
| anything else | `--no-worktree` | Cheaper, and worktrees are safe but rarely needed. Use `--worktree` when a change deserves quarantine. |

An explicit `--isolation` that contradicts either of the first two rows → **stop and report.** Do
not run a chain whose gates cannot pass.

**Re-verify the caveat before you plan on it.** The isolation table above isn't policy handed down
once — it's
a measurement ("64 structure / 601 state errors versus 0/0 in the main tree") that can go stale the
moment the corpus or the worktree machinery changes. The same applies to any carryover caveat this
lane inherits from a prior run or a sibling lane's notes file. Before treating either as fact: run
the one command that checks it (`./scripts/validate_brain.sh` in a scratch worktree for the isolation
row; whatever the carryover names for a carried-forward one) and record the result. An orchestration
run inherited at least one caveat that had since changed and planned a block on it — this is the
same failure class `/generate-roadmap`'s Step 2 exists to close ("Inventory, and re-verify before you
plan on it"); this step is its equivalent for isolation decisions and carryovers.

## Step 3 — Concurrency

**At most two heavy-gate repos may run concurrently** — anything whose `planning/harness.json` gates
include a browser or full production build (Playwright, `next build`). Determine whether this repo
is heavy mechanically, never by memory:
`python3 <path-to-base-template>/scripts/fleet_concurrency_check.py is-heavy --repo-path <this-repo>`
(exit 0 = heavy).

**This is enforced, not prose-only.** If this repo is heavy, register before starting:
`python3 <path-to-base-template>/scripts/fleet_concurrency_check.py register --repo <this-repo-name>`.
Exit code `3` (`"allowed": false`) means the fleet is already at capacity (two heavy repos live) —
stop and report rather than starting a third; wait or swap in a cheap-gate block instead. Release
the slot once this repo's chain finishes:
`... release --repo <this-repo-name>`. A lane killed mid-run does not block the fleet forever — its
entry expires automatically (dead process, or past a fixed TTL) on the next registration. If the
lock store itself is unavailable, the script degrades to `"allowed": true, "degraded": true` — the
same advisory behavior this replaces, never a hard failure. Full design:
`planning/decisions/D61-fleet-concurrency-enforcement.md` (in `base-template`).

## Step 4 — Confirm

Print, and stop for confirmation unless `--execute`:

- repo · roadmap · lane file (and section) · resolved chain in order
- isolation, and whether it was forced by policy or chosen
- per-block: engine, spec status (`tasks.md` present, or which `/generate-tasks` invocation will
  create it), and any `--from` plan file
- **readiness against the live graph** — any block with an unmet `depends_on`, named with its
  blocker and that blocker's repo
- **operator gates** — any block the roadmap marks as waiting on a human, with which item
- the log path

Then run `/orchestrate <chain> <isolation-flag> [--engine ...] [--continue-on-fail]`.

Everything below is what you enforce *around* `/orchestrate` — it does not supersede that command's
own standing rules, it adds to them.

---

## The six rules

Each has already cost a real run in this fleet.

1. **Never implement a block yourself, and never delegate one to a subagent.** Every block goes
   through `/sdlc-task` or `/sdlc-flow`; those engines spawn their own internal agents, which is
   theirs to do. A subagent is permitted **only** for read-only exploration, or a hotfix with no
   block of its own. Everything else — `/generate-tasks`, `/breakdown`, integration, verification,
   conflict resolution — runs inline in this session. A block built by an ad-hoc subagent has no
   spec, no gate, no state write and no review, and the chain's own verification will still look
   fine, so nothing catches it.

   **Two authoring-time rules for any spec or OKF frontmatter `/generate-tasks` (or you, editing
   one by hand) produces** — generalized from a lane that hit both the same day: a `related:`
   target must resolve to a real `doc_id` on a document that has actually been crawled, never a
   carryover slug or an invented id — one bad edge red-gates the whole corpus for every concurrent
   lane when `--graph` gates, not just the authoring one. And a `validation_command` must be
   scoped to the task's own changes, never the whole working tree (e.g. never a
   working-tree-wide `git diff | grep` guard) — it can never pass in a shared index with
   concurrent lanes and bails the block on an unrelated lane's uncommitted files.

2. **Commit after every `mev` command and every roadmap or plan edit**, before launching the next
   engine. Sibling lanes read those files; an uncommitted state change is invisible to them and gets
   clobbered.

3. **Report each integrated block** — append one line to the log and commit it:
   ```
   {"ts":"<ISO-8601>","lane":"<repo>","repo":"<repo>","block":"<id>","status":"closed|bailed|held","note":"<one line>"}
   ```
   **Never hand-edit a roadmap's generated regions.** Run `mev emit-state --write` and let the
   sequence table regenerate from `state.json`, which is the authority. Four sessions editing one
   markdown file is the contention pattern this structure exists to avoid.

4. **Never start a block showing `blocked`.** If the next one is HELD on a sibling lane, say so
   plainly — `HELD: <id> needs <dep> (<repo>)` — and pull the next `open` block in this repo rather
   than idling or improvising. Never skip silently.

5. **Keep a running notes file — `planning/orchestration-run/<roadmap-slug>/notes.md` in this
   repo** (the `run_record_dir` resolved in Step 1E). A lane run surfaces far more than the lane
   log's one line per block: defects found in passing, deferred fixes, cross-lane blockers, traps
   re-confirmed, things the roadmap got wrong. All of it dies in the session transcript unless it
   is written down, and the next session starts blind.

   Create it on the first block if absent (OKF frontmatter, `type: Reference`; add the row to
   `planning/index.md` per standing rule 7). Then **append after every block** — never rewrite, and
   never let it become a second status.md. Give every item a status so it can be triaged later:
   `OPEN` · `DONE` · `HELD` · `WONTFIX`. Commit it with the lane-log line.

   The lane log is the *cross-lane* channel and stays one line per block; this file is the *local*
   one and holds the detail. Anything that needs a ticket later, or that the next agent would
   otherwise rediscover the hard way, belongs here.

   **Required frontmatter**, per
   `planning/decisions/D57-orchestration-run-artifact-contract.md` (cited as the deciding
   authority — do not paraphrase its reasoning here):

   ```yaml
   roadmap: <driving-roadmap-slug>    # the --roadmap value resolved in Step 1C
   lane: <lane-name>
   run_started: YYYY-MM-DD
   run_ended: YYYY-MM-DD              # stamped at lane close
   lifecycle: active | lane-complete | consolidated
   ```

   `doc_id: <repo-slug>-orchestration-run-<roadmap-slug>` — unqualified ids collide corpus-wide and
   a corpus-wide `--graph` error red-gates every concurrent lane, not just this one. `lifecycle`
   replaces `status: archived`: `active` while the lane is running, `lane-complete` once this
   repo's part is done (the roadmap itself may still be open), `consolidated` once a consolidation
   run has consumed the record.

   **The per-block ledger table in the run record carries an `origin_roadmap` column**, defaulting
   to the record's own `roadmap` and set explicitly only when a block was adopted from a different
   roadmap (D57 section 3) — the driving roadmap is not always the block's own.

   **Unresolved items do not carry into a successor run record.** There is no "next" file to carry
   them into — each `(repo, roadmap)` pair has exactly one record, addressed, never rotated (D57
   section 1). At lane close, promote any item still `OPEN` into `state.json` `carryover[]` (D57
   section 4); do not copy it into another `notes.md`.

6. **Resolve what you can; record the call.** A lane that stops at every ambiguity is worthless,
   and one that stops at none is dangerous. Decide the ordinary things yourself — a spec slug that
   does not quite match convention, which of two plausible `--from` plan files is meant, whether a
   surfaced defect is in scope, how to resolve a merge conflict. Say what you assumed and keep
   moving.

   **Write every such decision into the notes file with its reasoning**, in one or two lines. A
   decision nobody can find later is indistinguishable from a mistake, and the next agent will
   re-litigate it or quietly reverse it.

   What you still must **not** decide alone: an operator gate (below), a bailed block's fate, two
   blocks that genuinely disagree about the same behaviour, and anything that would edit another
   lane's repo. Those stop and get reported.

## Operator gates

Some blocks wait on work only the operator can do: a DNS record, a hosting project, a written brief,
a human read-through of generated content. **Stop and name the item and the block waiting on it.**
Do not stub it, fake it, or route around it. A lane that invents its way past a human gate produces
work that has to be redone.

## Traps

- A piped command's exit code is the **pipe's**, not the command's — `mev conformance | tail`
  reports success while the command exits 1. Redirect to a file, then check `$?`.
- `mev validate-brain`'s flags **do not compose** (`main.rs` is an if/else-if chain, first flag
  wins). One invocation per flag.
- Every `planning/` is a symlink into a `_planning/` vault. Any `rg`/`find` sweep that must be
  exhaustive needs `-L`; one reporting "clean" without it is not trustworthy. **At the brain root,
  also pass `-uu`** — every sub-repo is gitignored there, so `rg` skips them all even with `-L`
  alone, and a sweep missing `-uu` reports a false clean over the whole fleet.
- `planning/state.json` is written with `ensure_ascii=False`. Script edits must round-trip with
  `json.dump(..., indent=2, ensure_ascii=False)` plus a trailing newline — the default escapes every
  em dash and turns a 3-field edit into ~130 lines of churn, and a conflict for every sibling lane.
- A leading `_` excludes a file from the corpus, so `_zz_*.md` debug probes are invisible to
  `validate-brain`.
- `timeout` does not exist on this macOS shell.
- Invoke `/sdlc-flow` and `/sdlc-task` from the **main session** — the `Workflow` runtime behind
  `.claude/workflows/` is unavailable to delegated subagents.

## Before finishing

Run this repo's own gates from `planning/harness.json`, then the corpus gate from `BRAIN_ROOT`:

```
./scripts/validate_brain.sh
```

Concurrent lanes pushing into one corpus is the exact condition that accumulated 32
`validate-brain` errors across four lanes and blocked pushes fleet-wide.

**Report:** blocks closed · blocks HELD and on what · operator gates hit · decisions you took and
why · anything the roadmap got wrong. The last one matters most — the roadmap is a hand-authored
snapshot and the graph is the fact.

Everything in that report should already be in `planning/orchestration-run/<roadmap-slug>/notes.md`
(rule 5). The report is the summary; the notes file is the record that survives the session.

**A terminal `planning/orchestration-run/<roadmap-slug>/review.md` is required, not optional.** It is a
plain-English summary of what this lane changed plus the hand-verification recipes an operator
would run to confirm it. Every recipe in it must have been **executed at least once by this lane
before the file is written**, and the file must say so explicitly — a recipe that was only
authored, never run, reads as verification while being a guess, which is worse than handing the
operator nothing. Naming, frontmatter, and lifecycle follow
`planning/decisions/D57-orchestration-run-artifact-contract.md`; do not restate that contract
here.

## Standing operator convention

This is the universal end-of-run convention every program used to re-author for itself in a
per-program `orchestrate-prompt.md` (`close-the-loop`'s and `carryover-improvements`' copies were
near-duplicates of each other, pasted by the operator because neither command knew they existed).
**It lives here now, once, and nowhere else — `/generate-roadmap` must stop being a place this
convention gets authored per-program.** A program-specific rule (a repo's own "no `blocking:
bool`"-style constraint) still belongs in that program's lane file; only the universal shape below
moves here.

At the end of every lane, alongside the report and `review.md`:

- **Order every lingering item P0–P3 per `planning/decisions/D43-cross-domain-priority-graph.md`
  (cite it by doc_id).** Do not restate or paraphrase its rubric here — the priority a lane assigns
  comes from reading D43 fresh each time, not from a copy of its definition drifting in this file.
- **Name the owning repo for each lingering item.** Most of a repo's carryover belongs to
  `base-template` or HQ, not the repo the lane just ran in — a per-repo file that skips this always
  accumulates other repos' work.
- **For any lingering item this repo owns, write its next step into `handoff.md`**, ordered by a
  mix of priority (per D43) and quick-wins, so a fresh session can pick it up without replaying
  this lane's context.
- **A lingering item that is still `OPEN` when the lane closes promotes to `state.json`
  `carryover[]` (D57 section 4) — it never moves into a successor run record.** Each
  `(repo, roadmap)` pair has exactly one record, addressed rather than rotated (Step 1E, rule 5),
  so there is no successor file to move it into.
- **Close the lane with a terminal `/close-out`.**

## Files

- Reads: the roadmap, the lane file, `planning/state.json`, `planning/harness.json`, `brain.toml`
- Writes: the lane log (append-only), `planning/orchestration-run/<roadmap-slug>/notes.md` (append-only),
  `handoff.md` (per the standing operator convention above), plus whatever `/orchestrate` and the
  engines write
- Never writes: a roadmap's `<!-- BEGIN generated:* -->` regions

## Example

```
/begin-orchestration --roadmap planning/demand-ready/roadmap.md --lane bastion-web

/begin-orchestration --roadmap planning/demand-ready/roadmap.md --lane gtm --isolation no-worktree

/begin-orchestration --roadmap planning/demand-ready/roadmap.md --blocks MV.12.A MV.12.B MV.12.C

/begin-orchestration --roadmap planning/demand-ready/roadmap.md --lane bastion-web \
      --plan-file planning/bastion-web-demo/plan.md --execute
```

`--roadmap` resolves against `BRAIN_ROOT` when relative, so the same string works from every repo
regardless of how deep it sits.
