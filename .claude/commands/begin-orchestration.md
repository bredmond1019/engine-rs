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

**E. The chain** — `--blocks` verbatim, or the lane file:
- `--lane <path>` → that path.
- `--lane <name>` (bare) → `<roadmap_dir>/lane-<name>.txt`. Missing → stop and list what
  `lane-*.txt` files do exist there.

**F. Cross-check** — if the resolved lane file carries a `# ROADMAP:` header and it disagrees with
the roadmap resolved in C, **stop and report both.** That mismatch means the lane belongs to a
different run than the one you were told, and it is the cheapest available check that `--roadmap`
was typed correctly.

Read the lane file with `#` comments and blanks stripped; file order is execution order. Lane files
may cover several repos in one running order — **take only your repo's section.** If the file has
section markers for other repos and you cannot tell which is yours, stop and ask.

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

## Step 3 — Concurrency

**At most two heavy-gate repos may run concurrently** — anything whose `planning/harness.json` gates
include a browser or full production build (Playwright, `next build`). Determine whether this repo
is heavy by reading its `harness.json`, not by memory.

Nothing enforces this. If this repo is heavy, say so and ask before starting; the operator knows
what else is live.

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

## The four rules

Each has already cost a real run in this fleet.

1. **Never implement a block yourself, and never delegate one to a subagent.** Every block goes
   through `/sdlc-task` or `/sdlc-flow`; those engines spawn their own internal agents, which is
   theirs to do. A subagent is permitted **only** for read-only exploration, or a hotfix with no
   block of its own. Everything else — `/generate-tasks`, `/breakdown`, integration, verification,
   conflict resolution — runs inline in this session. A block built by an ad-hoc subagent has no
   spec, no gate, no state write and no review, and the chain's own verification will still look
   fine, so nothing catches it.

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
  exhaustive needs `-L`; one reporting "clean" without it is not trustworthy.
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

**Report:** blocks closed · blocks HELD and on what · operator gates hit · anything the roadmap got
wrong. The last one matters most — the roadmap is a hand-authored snapshot and the graph is the
fact.

## Files

- Reads: the roadmap, the lane file, `planning/state.json`, `planning/harness.json`, `brain.toml`
- Writes: the lane log (append-only), plus whatever `/orchestrate` and the engines write
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
