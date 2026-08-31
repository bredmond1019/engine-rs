---
name: pick-the-next-block
description: Answer "what should I work on next" from the block graph instead of by reading status.md prose — the three mev query verbs, the three different meanings of "ready" they each report (dependency-clear vs. lane-available vs. an engine can actually start it), the two unrelated leverage numbers, and the flag combinations that are usage errors. Use when choosing the next block in any repo, when filling a free lane mid-orchestration, when asked what unblocks the most work, and before claiming a block is startable.
allowed-tools: Bash(mev:*) Bash(jq:*) Bash(python3:*) Bash(grep:*)
---

# Picking the next block

`status.md` prose and the focus queues are snapshots. The block graph is live, and three read-only
`mev` verbs query it. **They disagree with each other on purpose** — each answers a different
question, and taking the first one's word for it is how a lane gets started on a block another
session is already running.

| Verb | Question it answers | Scope |
|---|---|---|
| `mev frontier` | Which lane heads are **dependency-clear**? | one line per roadmap lane segment |
| `mev lanes` | Which lane segments are **actually available right now**? | same segments, six-state availability + reason |
| `mev blocks` | Which **individual blocks** match my filters, and can an engine start one? | every block in the corpus, filterable |

All three are read-only, recompute live (they never read the on-disk artifacts), and never write a
file. Exit 0 on success, 1 on `brain.toml` not found or a truncated graph they refuse to degrade on.

## The trap: `startable` does not mean available, and does not mean runnable

Measured corpus-wide, 2026-08-31:

```
mev frontier | grep -c startable          →  36   dependency-clear lane heads
mev lanes    | grep -c ' — startable'     →  16   ...of which this many are actually free
mev blocks --startable            | wc -l → 121   dependency-clear blocks
mev blocks --startable --runnable | wc -l →  11   ...of which an engine can start this many
```

Two independent narrowings, and both are invisible in the verb above them:

- **`frontier` → `lanes`.** `frontier` printed `autonomous-foundation/orchestrator#0
  synapse:OR.3.A — startable`. `lanes` printed the same segment as `held-repo-busy (repo synapse is
  live on plan-brain-rag-quality)`. `frontier` knows dependencies only; it does not know a lane is
  already running. **Never launch off `frontier` alone.**
- **`startable` → `runnable`.** `startable` is dependency-clear. `runnable` means the block has
  **both** a block record and a `tasks.json` on disk — the precondition for `/sdlc-task` or
  `/sdlc-flow` to have anything to execute. Most startable blocks are not runnable; they need
  `/generate-tasks` first, and that is the real next action for them.

## The six availability states

`done` > `held-block` > `held-operator` > `held-repo-busy` > `held-slot` > `startable`
(fixed precedence, highest first). Each `mev lanes` line carries the reason:

```
{roadmap}/{lane}#{segment} {repo}:{head} — {availability} ({reason}) frees N lane(s)
```

- `held-operator` means a human gate (`OP.<slug>`) — drive it with `/begin-session`, not by
  starting the block.
- `held-repo-busy` derives from exactly one source: the per-`(repo, roadmap)` orchestration-run
  record's `lifecycle:` frontmatter (`planning/orchestration-run/<roadmap>/notes.md`) — **not**
  `lane-log.jsonl` and **not** `.fleet-locks`. If it looks wrong, that file is what to check.
- `frees N lane(s)` on a `done` segment is historical, not actionable — those lanes are already free.

## The two leverage numbers are not the same number

| Number | Verb | Means |
|---|---|---|
| `frees N lane(s)` | `mev lanes` | how many **lane segments** this segment's head unblocks |
| `leverage: N live, M parked` | `mev blocks --leverage` | the block's **transitive downstream block cone**; only live members rank it, parked ones are reported but never counted |

Do not compare or add them. For "what unblocks the most work", ask for the cone:

```bash
mev blocks --startable --leverage --limit 10
#   price-scout:PS.7.B (startable=true record=true tasks=true runnable=true)
#     leverage: 7 live, 0 parked
```

`--chain` instead reports the longest same-repo run reachable from each block — the right question
when you want one lane to keep going without a cross-repo handoff.

## Usage errors (all exit 1, none of them warn)

- `--leverage` with `--chain` — pick one derivation per invocation.
- `--startable` with `--blocked`.
- `--runnable` with `--not-runnable`.

## `--repo` here filters; on `emit-block-graph` it silently does not

`mev blocks --repo <slug>` narrows on its own. On `mev emit-block-graph`, a bare `--repo` without
`--scope repo` is **ignored**, and the whole corpus comes back looking like a filtered result — a
clean-looking wrong answer. `mev blocks` has no `--scope` flag to forget; prefer it for any
ad-hoc question.

## The recipe

```bash
# 1. Is this repo's lane even free, and what is holding the others?
mev lanes | grep -v ' — done'

# 2. What can an engine actually start here, hottest leverage first?
mev blocks --repo <slug> --startable --runnable --leverage

# 3. Nothing runnable? Then the next action is authoring, not implementing:
mev blocks --repo <slug> --startable --not-runnable     # these need /generate-tasks
```

Filters worth knowing: `--roadmap <slug>` (resolves via `origin_roadmap`, D57, falling back to the
scheduled roadmap), `--max-priority N` (inclusive; a block with no resolvable priority never
matches), `--limit N`, `--json` for this verb's own `QueryReport` shape.

## What these verbs do **not** answer

- **Operator work.** `held-operator` names the gate but not what to do about it — `mev
  attention-queue` and `/attention` own the human queue; `/begin-session` works one gate.
- **Priority across the whole board.** Only `--max-priority` filters; nothing here sorts by
  effective priority. The Attention board does that.
- **Whether the work is still worth doing.** A startable block with a stale premise is still
  reported as startable.

## Do not "refresh" the artifacts to answer these questions

`planning/lane-frontier.json` and `planning/lane-availability.json` are the same shapes, written
**only** by `mev emit-state --write` — which rewrites generated boards across the whole corpus, not
just yours. The three verbs above recompute live and write nothing, so never run a write verb just
to get a fresh read. If you do need to write, load `derive-state-safely` first.

## See also

- `derive-state-safely` — before any `emit-state --write` / `set-block-status`.
- `edit-state-json` — the `depends_on` edge shapes that produce `held-block` / `held-operator`.
- `run-the-gates` — a piped command's exit code is the pipe's, not the command's.
- `core/mev/docs/cli/lanes.md` — the six states, precedence, and JSON shapes in full.
