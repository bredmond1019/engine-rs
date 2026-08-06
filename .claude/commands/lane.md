---
type: Command
title: lane — Open one lane of a multi-repo roadmap run and drive it through /orchestrate
description: Brief yourself from a roadmap and lane file, resolve engine/isolation policy, then drive this repo's chain through /orchestrate with the concurrency, reporting, and operator-gate rules enforced.
---
# Lane — Open one lane of a multi-repo roadmap run

Wraps `/orchestrate` with the context a lane agent needs and the rules a concurrent run depends on.
`/orchestrate` knows how to drive a chain; it does not know *which* chain, *why*, what may not be
delegated, or who else is running right now. This command supplies that, then hands off.

**One `/lane` session drives one repo.** Run several at once — that is the lane model.

## Variables

`$ARGUMENTS` — flags, any order. **One of `--lane` or `--blocks` is required; everything else is
optional and resolves to a default.**

| Flag | Required | Default | What it does |
|---|---|---|---|
| `--lane <path\|name>` | one of | — | Lane chain file. A bare name (`gtm`) resolves to `<roadmap-dir>/lane-<name>.txt` — see step 1, which finds the roadmap *first*. |
| `--blocks <id ...>` | one of | — | Inline block IDs instead of a lane file. Space- or comma-separated. |
| `--roadmap <path>` | no | see *Resolving the roadmap* | The roadmap this lane belongs to. |
| `--repo <slug>` | no | inferred from cwd | Override only when inference is wrong. |
| `--isolation <worktree\|no-worktree\|auto>` | no | `auto` | `auto` applies the policy table below. |
| `--plan-file <path>` | no | — | Spec source for `/generate-tasks --from`, when the blocks are not in `master-plan.md`. |
| `--engine <task\|flow>` | no | per-block | Force one engine for the whole chain. |
| `--log <path>` | no | `<roadmap-dir>/lane-log.jsonl` | Where to report integrated blocks. `--log none` disables. |
| `--execute` | no | off | Skip the dry-run confirmation and start immediately. |
| `--continue-on-fail` | no | off | Passed through to `/orchestrate`. |

Empty `$ARGUMENTS` → print usage and stop:

```
Usage: /lane --lane <path|name> [--roadmap <path>] [--repo <slug>]
             [--isolation worktree|no-worktree|auto] [--plan-file <path>]
             [--engine task|flow] [--log <path>] [--execute] [--continue-on-fail]
       /lane --blocks <id> [<id> ...] [same optional flags]
```

---

## Step 1 — Resolve

Resolve in this order. It is deliberately non-circular: **the roadmap is found before the lane
file**, because a bare `--lane` name can only be located relative to the roadmap's directory.

**A. `BRAIN_ROOT`** — walk up from cwd for `brain.toml`.

**B. The repo** — this repo's `planning/state.json` → `repo`. `--repo` overrides. If cwd *is*
`BRAIN_ROOT`, the repo is the brain (HQ).

**C. The roadmap** — first hit wins:
1. `--roadmap <path>`.
2. If `--lane` was given as a **path that exists**, read its `# ROADMAP:` header.
3. `<BRAIN_ROOT>/planning/state.json` → `epics[]` → the `plan` pointer of the **single `focused`**
   epic, resolved relative to `BRAIN_ROOT`. Exactly one `focused` epic → use it. **Zero or more than
   one → stop and ask.** Never pick.

   *This is the path a bare `--lane <name>` takes*, since rule 2 needs a file you cannot locate yet.
   It is also the fragile one: it infers the roadmap from what the fleet is currently focused on,
   which is right during a focused run and wrong the moment two initiatives overlap. Pass
   `--roadmap` explicitly whenever the run is not the focused epic.

**D. `roadmap_dir`** = the roadmap's directory.

**E. The chain** — `--blocks` verbatim, or the lane file:
- `--lane <path>` → that path.
- `--lane <name>` (bare) → `<roadmap_dir>/lane-<name>.txt`. Missing → stop and list what
  `lane-*.txt` files do exist there.

**F. Cross-check** — if the resolved lane file carries a `# ROADMAP:` header and it disagrees with
the roadmap resolved in C, **stop and report both.** That mismatch means the lane belongs to a
different run than the one you inferred, and it is the failure this ordering exists to catch.

Read the lane file with `#` comments and blanks stripped; file order is execution order. Lane files
may cover several repos in one running order — **take only your repo's section.** If the file has
section markers for other repos and you cannot tell which is yours, stop and ask.

Print what you resolved and **which rule produced it**. A lane driven against the wrong roadmap is
worse than one driven against none.

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
/lane --lane gtm --isolation no-worktree
/lane --blocks MV.12.A MV.12.B MV.12.C
/lane --lane lane-bastion-web.txt --plan-file planning/bastion-web-demo/plan.md --execute
```
