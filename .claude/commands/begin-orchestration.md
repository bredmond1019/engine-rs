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


**Before writing down anything that is wrong, follow
[`.claude/workflows/finding-discipline.md`](../workflows/finding-discipline.md).** Evidence travels
with the finding or the finding does not exist; one occurrence is an instance, not a pattern; and an
odd-but-unexplained thing is recorded as an **observation** rather than inflated into a defect. The
cut list is part of the report — a pass that files everything it noticed has not filtered. Measured:
three independent audits found 32%/32%/26% of filed carryover already dead.

## Variables

`$ARGUMENTS` — flags, any order. **`--roadmap` is required, plus one of `--lane` or `--blocks`.**
Everything else resolves to a default.

`--roadmap` is mandatory on purpose. An earlier version inferred it from whichever epic was
`focused`, which is correct during a single-initiative week and silently wrong the moment two
initiatives overlap — the case where a lane driven against the wrong roadmap is hardest to notice.
Naming it costs one flag and removes a hidden coupling to epic status.

| Flag | Required | Default | What it does |
|---|---|---|---|
| `--roadmap <path\|slug>` | **yes** | — | The roadmap this lane belongs to. A path is absolute or relative to `BRAIN_ROOT` and is honoured as given; a bare slug is resolved per Step 1C. |
| `--lane <path\|name>` | one of | — | Lane record. A bare name (`gtm`) resolves to `<roadmap-dir>/lane-<name>.json`, authored against `.claude/workflows/lane.schema.json` (D71). |
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

**Roadmap slug resolution (the canonical rule — `/orchestrate` and `/consolidate-run` cite this
rather than restating it):** when `--roadmap` is given as a bare slug or resolves to a directory
rather than a `roadmap.md` file, resolve it in this fixed order:

1. `planning/roadmaps/<slug>/` — if it exists, that is `roadmap_dir`.
2. Otherwise, legacy `planning/<slug>/` — if it exists, that is `roadmap_dir`.
3. A slug present in **both** locations is an **error only when the legacy path is itself a
   roadmap** — that is, when `planning/<slug>/` holds `lane-log.jsonl` or `roadmap.md`. Then stop
   and report both paths; an ambiguous roadmap is how a lane appends to the wrong lane log.
   **Otherwise prefer `planning/roadmaps/<slug>/` silently and say nothing.** A `planning/<slug>/`
   holding `assessment.md` / `seams.md` / `sequence.md` / `evidence/` is PRE-PLAN RESIDUE, not a
   rival roadmap: `/assess`, `/seams` and `/sequence` all write there, and which successor consumes
   them is unknown until `/sequence` counts the repos in its cut — one repo goes to `/plan`, which
   stays in that same directory, several go to `/generate-roadmap`, which writes
   `planning/roadmaps/<slug>/`. So the same slug in both places is the NORMAL end state of the
   multi-repo path. Measured 2026-09-03: 0 of 31 roadmap directories and 0 directories under
   `planning/` carried a `lane-log.jsonl` or `roadmap.md`, so the unnarrowed rule's true-positive
   population was empty and it fired only on pre-plan residue — wedging every consolidation in the
   fleet (`lane_log_watermark.py` hard-exits) on a directory nobody was confused about. The
   mechanical form is `is_roadmap_dir()` in `scripts/lane_log_watermark.py`; keep the two in step.

An explicit path argument (one that already names a file or a full directory, e.g.
`planning/roadmaps/close-the-loop/roadmap.md` or `planning/demand-ready/`) is always **honoured as
given** — resolution applies only to a bare slug, never overriding an explicit path.

**D. `roadmap_dir`** = the roadmap's directory, per the resolution above.

**E. `run_record_dir`** = `planning/orchestration-run/<roadmap-slug>/` in **this repo**, where
`<roadmap-slug>` is `roadmap_dir`'s directory name (from D). Create the directory if absent; if it
already exists, **append** to its `notes.md` / `review.md` rather than creating new ones. **No
rotation, no archive move, no dated filenames, no crash window** — a record is addressed by
`(repo x roadmap)`, never by time. The required frontmatter (`roadmap`, `lane`, `run_started`,
`run_ended`, `lifecycle`), the `doc_id: <repo-slug>-orchestration-run-<roadmap-slug>` rule, and the
ledger's `origin_roadmap` column are specified once, in Rule 5 below, per
`planning/decisions/D57-orchestration-run-artifact-contract.md` — **cited as the deciding
authority; not paraphrased here.** Print `run_record_dir` alongside the other Step 1 resolutions.

**F. The chain** — `--blocks` verbatim, or the lane record:
- `--lane <path>` → that path.
- `--lane <name>` (bare) → `<roadmap_dir>/lane-<name>.json`. Missing → stop and list what
  `lane-*.json` files do exist there.

**G. Cross-check** — parse the resolved `lane-<name>.json` and compare its top-level `roadmap`
field against the roadmap resolved in C. If they disagree, **stop and report both.** That mismatch
means the lane belongs to a different run than the one you were told, and it is the cheapest
available check that `--roadmap` was typed correctly. Keep this check even though the format
changed — the schema makes `roadmap` a required field, so the cross-check is now against structured
data instead of an optional header comment, but the stop-and-report behaviour on mismatch is
unchanged.

Read the chain from the lane record's `blocks[]` array — **array order is chain order.** Each entry
carries its own required `repo`; a lane is not single-repo in this corpus, so **take only the
entries whose `repo` matches the repo resolved in B.** If you cannot tell which entries are yours,
stop and ask.

**There is no prose in a lane record — the JSON schema has no field for it.** Everything that used
to live in a lane file's `#` comments now lives in a container that is actually read by the thing
that needs it, and this command must use those containers instead of looking for binding text in
the lane file itself:
- **Per-block briefing** (isolation rationale, prior-run gotchas, "do not touch X") — read from
  each block's own record (`why` / `notes`), not from the lane. Neither SDLC engine has ever
  opened a lane file; the block record is what they actually read.
- **Holds and intra-lane dependencies** — read from `state.json` `depends_on` on the block, not
  from a HOLDS section.
- **Operator gates** — read from an `operator` edge in `depends_on`, not from a TRAPS section or
  free-text warning.
- **Cross-roadmap adoption** — each `blocks[]` entry's own `origin_roadmap` field names the
  roadmap the block was originally allocated under. Use it wherever this command used to read an
  `# ORIGIN: <roadmap path>` comment above an adopted block; a block entry whose `origin_roadmap`
  differs from the lane record's own top-level `roadmap` field is an adopted block, and the
  per-block ledger row in Rule 5 below records that `origin_roadmap`, not the lane's.

Print what you resolved. A lane driven against the wrong roadmap is worse than one driven against
none.

## Step 1B — Premise re-derivation

**Before task generation, re-derive every quantitative claim and every named live artifact in each
block about to run — never trust the record's own numbers.** A block record's premises rot faster
than the block runs: a count, a line number, an "N of M", a named file/symbol/command/registered
check, or a "three sites" claim in the block's `description`, `what`, `why`, or
`acceptance_criteria` can be stale the day the block executes, and a wrong premise ships working
code doing the wrong thing (M4, `pattern-analysis-2026-09-02`).

For each block in the resolved chain:

1. **Extract** every quantitative claim and every named live artifact from that block's record —
   `description`, `what`, `why`, `acceptance_criteria`. A "quantitative claim" is a count, a line
   number, an "N of M", or any number a reader could check against the live system. A "named live
   artifact" is a file, a symbol, a command, or a registered check the record names as existing or
   behaving a specific way right now.
2. **Run one command per claim.** Re-reading the record is not re-derivation — the record is the
   thing under test, so reading it again only confirms what it already says. Run the actual check
   (`grep`, `wc -l`, the named script, `bastion validate-brain`, whatever answers that specific
   claim) against the live corpus.
3. **Amend the record in place** with the re-measured value plus the date, per D18 — never leave
   the stale number sitting next to the fresh one. A block record cannot carry a separate
   `amendments` array (`.claude/workflows/block.schema.json` sets `additionalProperties: false` and
   defines no such field), so the amendment goes in the record's existing `notes` field, with the
   evidence (the command run and its output) captured in the orchestration-run notes.
4. **Rewrite a moved criterion to measure at run time**, not to compare against a newly-frozen
   number — an acceptance criterion re-pinned to today's count just rots again on the same clock
   that broke it the first time.
5. **A premise that survives re-derivation unchanged is still a result, not a no-op.** Record that
   it was checked and held — a lane that finds nothing on every block should not conclude the step
   is ceremony and stop running it; the control case (a record checked the same way that held on
   every premise) is what proves the step isn't a formality that always finds something.

**Do not fold this into the isolation policy step's re-verification below** — that step re-verifies
the *isolation* caveat (worktree exposure, a carried-forward isolation override); this step
re-derives *block record* premises. They are different questions in different steps, and
`BT.ticket.begin-orchestration-step2-reverify-rules` (closed) already owns the isolation one —
do not re-litigate it here.

## Step 2 — Isolation policy

`--isolation auto` resolves as:

`--worktree` was suspended fleet-wide from 2026-08-23 to 2026-08-28 (`D81-worktree-moratorium`);
it was lifted after `BT.ticket.worktree-smoke-fixture` verified a real `--worktree` run end to end.
The table below records the current isolation recommendation per repo, D81 history noted where it
still bears on the *why*:

| Repo | Isolation | Why |
|---|---|---|
| `base-template` | **`--no-worktree`** (default) | The Workflow harness executes a launch-time **copy** of the engine, so a chain editing `.claude/workflows/sdlc-*.js` does not change the engine already executing it, in either isolation mode — a worktree never protected a running chain. The residual exposure is narrower and *between* blocks, not within one: a block's engine edit lands in the working tree before the *next* block's launch snapshots it. Mitigate by sequencing engine edits to a chain boundary, not with `--worktree`. |
| the brain root (HQ) | **`--no-worktree`, always** | `validate-brain` inside a worktree resolves the gitignored sub-repos against the worktree's own `brain.toml` and they are absent from any checkout. Measured: 64 structure / 601 state errors versus 0/0 in the main tree. Worktree creation is clean — it is the corpus gates that cannot pass. |
| anything else | `--no-worktree` | Cheaper. Use `--worktree` when a change deserves quarantine — available again fleet-wide since the D81 lift. |
| any repo that already has another session live in it | **`--worktree`** | The first three rows assume one session per repo. Two chains sharing a working tree share one git index, so each sees the other's uncommitted files: a tree-wide `validation_command` bails on a sibling's edits, `git status` reads as dirty for reasons you did not cause, and a `git checkout`/branch switch by either one moves the other's tree underneath it. Cheap detection before Step 4: `git -C <repo> status --short` showing edits you did not make, a branch you did not create (`git -C <repo> branch --show-current`), or the roadmap's lane records naming another live lane in this repo. When in doubt, take the worktree — except for the two rows above, where a worktree cannot pass the gates at all; there, do not start a second concurrent chain. |

An explicit `--isolation` that contradicts either of the first two rows → **stop and report.** Do
not run a chain whose gates cannot pass.

**Re-verify the caveat before you plan on it.** The isolation table above isn't policy handed down
once — it's
a measurement ("64 structure / 601 state errors versus 0/0 in the main tree") that can go stale the
moment the corpus or the worktree machinery changes. The same applies to any carryover caveat this
lane inherits from a prior run or a sibling lane's notes file. Before treating either as fact:
re-derive it — the HQ row from a fresh `bastion validate-brain --structure` / `--state` inside a
real worktree; `base-template`'s row by confirming the launch-time-snapshot argument it rests on is
still live — list the real `sdlc-*-wf_*.js` engine snapshot copies under
`~/.claude/projects/<proj>/<session>/workflows/scripts/`. Finding none is the expected state before
this session's first engine launch, not a failure — it is copies accumulated by *prior* sessions
that make the control positive, by showing the executed engine already lives outside any worktree
so a worktree could never have protected a running chain. And re-derive any carried-forward caveat
from whatever the carryover names — and record the result. An orchestration run inherited at least
one caveat that had since changed and planned a block on it — this is the same failure class
`/generate-roadmap`'s Step 2 exists to close ("Inventory, and re-verify before you plan on it");
this step is its equivalent for isolation decisions and carryovers.

**A re-verification control must exercise the same code path the system actually takes**, not an
equivalent-looking one — the rule is HQ `CLAUDE.md` standing rule 11 as amended by
`BT.ticket.positive-control-must-take-the-same-code-path`, and its enforceable form is
`scripts/check_command_hazards.py`'s `H1_FIX` ("a control must run the SAME tool as the claim it
licenses, not a substitute that happens to sound related"). It is cited here, not restated. The
measured failure that earned it: five lanes ran a positive control on 2026-08-23 and all five ran
it down a path the engine never takes.

**A lane record's own `isolation` field is not exempt from re-verification.** When a lane record
carries an `isolation` value that differs from the table's row for this repo, the lane must do one
of two things before planning on it — a bare override is never honoured:

- **record the basis it checked and the command it ran to check it** (e.g. the engine-snapshot
  listing above, or the fresh `validate-brain` re-run for HQ); or
- **fall back to the table's value** when no such basis can be produced.

Naming a reason is not the same as checking one: `engine-rs-37` inherited a `--worktree` override
and justified it as "sibling native-build lanes hold the shared index" — a single command,
`git -C core/engine-rs rev-parse --git-common-dir`, returns `.git`, a separate repo with a separate
index, disproving the stated basis outright. A stated-but-unchecked basis is worse than a silent
pass-through, because it makes an unexamined value look examined.

## Step 3 — Concurrency

**Heavy-gate repos are capped per category, not fleet-wide** (D66) — a browser-automation repo
(Playwright, `next build`, ...) and a native-build repo (`cargo build --release`) draw from separate
pools, because they have different cost shapes: browser-automation tooling stays CPU-expensive for a
lane's whole run, while a native build is expensive only once per lane (at the end/reconcile), not
per task. Determine whether this repo is heavy, and which category, mechanically, never by memory:
`python3 <path-to-base-template>/scripts/fleet_concurrency_check.py is-heavy --repo-path <this-repo>`
(exit 0 = heavy; the JSON `category` field is `"browser-automation"` or `"native-build"`).

**This is enforced, not prose-only.** If this repo is heavy, register before starting, passing the
category from the `is-heavy` output:
`python3 <path-to-base-template>/scripts/fleet_concurrency_check.py register --repo <this-repo-name> --category <category> --agent <this lane's agent identity>`.
Exit code `3` (`"allowed": false`) means either that category's pool is already at capacity (2
browser-automation lanes, or 4 native-build lanes) **or** that a held exclusive lease is blocking
this registration — the two are distinguishable from the JSON output's now-populated
`exclusive_leases` array (non-empty means a lease, not capacity, is the cause) versus `active`.
Either way: stop and report rather than starting another; wait or swap in a cheap-gate block
instead.

**Do not pass `--pid`.** The process running `register` is this short-lived command invocation —
it exits as soon as this step returns, so its own pid is never a valid liveness signal for a later
process to check. Leave `pid_source` at its default (`"self"`); the entry is then held by **TTL
(90 minutes) plus explicit release only**, never by pid liveness. **Pass `--agent <this lane's
agent identity>`** on every `register` and `release` call — the entry is keyed on that identity,
not on the caller's pid, which is what lets a `release` run from a different process than the one
that registered actually free the slot. If this chain runs longer than that, re-register
periodically as a heartbeat (repeat the same `register --repo <this-repo-name> --category
<category> --agent <this lane's agent identity>` call): a repeat register for the SAME agent
refreshes `started_at` on the existing entry in place rather than consuming a second slot.

**The old release → register → re-take workaround is superseded by this heartbeat.** Before the entry
was keyed on `--agent`, the only way to refresh a long-running heavy lane's slot was to `release`
it and `register` again — which genuinely gave the slot up and let another lane claim it
mid-chain. Do not do that any more: repeat the `register` in place. (The *repo lease*
release/drain/re-take at the block boundary is a different mechanism and is still required.)

**Release the slot when this repo's chain finishes — this is required, not optional:**
`... release --repo <this-repo-name> --agent <this lane's agent identity>`, on success, failure,
or abandonment. A lane killed mid-run
without releasing does not block the fleet forever — its entry expires automatically once it is
past the TTL, or (for an entry with an *explicitly*-supplied `--pid`) once that pid has died — but
that is the fallback, not a substitute for releasing on exit. If the lock store itself is
unavailable, the script degrades to `"allowed": true, "degraded": true` — the same advisory
behavior this replaces, never a hard failure. Full design:
`planning/decisions/D61-fleet-concurrency-enforcement.md` and
`planning/decisions/D66-tiered-heavy-lane-concurrency.md` (in `base-template`).

**At lane close, release both the repo lease and the registry claim taken in Step 4, alongside
this fleet-concurrency slot release** — delete `<lock_dir>/leases/lease-<repo>.json` and
`<lock_dir>/lane-agents/agent-<agent_name>.json`. All three releases happen together, on success,
failure, or abandonment, so a reader looking for "what does this lane give back on exit" finds it
in one place.

**Taking any exclusive lease quiesces this repo's `mev` write verbs for the length of the chain.** While a `kind: exclusive` lease is held — whether the ordinary per-lane lease Step 4 takes for every real lane, or an additional lease taken here for a repo named in `exclusive_repos` — every `mev` write verb for that leased repo (`set-block-status --write`, `emit-state --write`, and the other write verbs) is refused with `E_QUIESCE_LEASE_HELD`. This is a declared quiet window, distinct from `E_EMIT_LOCK_HELD` contention: do NOT retry — either wait for the lease to be released, or, if this lane is the holder, pass `--agent <this lane's agent identity>` as the self-exemption on the write verb, exactly as `register`/`release` already require it above. The lease Step 4 makes this lane take is one of the leases this section's own `register` check consults — `--agent` is what lets the holder's own `register`/`release`/write calls through without being refused by its own lease. (The holder's own re-register is never refused: both `fleet_concurrency_check.py` and `mev` skip a lease whose `agent` matches the requester.)

**Fleet-exclusive lanes (`exclusive_repos`).** If the lane record's `exclusive_repos` array is
non-empty, before the first block starts, write an additional `kind: exclusive` lease at
`<lock_dir>/leases/lease-<repo>.json` for **each** repo named in `exclusive_repos` — same shape as
any other lease record (`repo`, `lane`, `agent`, `acquired_at`, `kind: exclusive`; no new field).
`scope` absent defaults to `repo`: a repo-scoped exclusive lease refuses `register` (and the
`mev` write verbs above) only for a requester whose own repo the lease names — not every other
agent. Only `scope: fleet` quiesces the whole fleet; the leases this paragraph writes for each
`exclusive_repos` entry are repo-scoped, one per named repo, so together they close
registration on exactly those repos. This is admission control only, never pre-emption of a
lane already running. Remove every lease written this way at lane close — success, failure, or
abandonment — alongside the ordinary lease and registry releases. `exclusive_repos` is read only
here; no new field is added to `.claude/workflows/lane.schema.json` or to the lease record.

## Step 4 — Confirm

Print, and stop for confirmation unless `--execute`:

- repo · roadmap · lane record · resolved chain in order (this repo's `blocks[]` entries only)
- isolation, and whether it was forced by policy or chosen
- per-block: engine, spec status (`tasks.md` present, or which `/generate-tasks` invocation will
  create it), and any `--from` plan file
- **readiness against the live graph** — any block with an unmet `depends_on`, named with its
  blocker and that blocker's repo
- **operator gates** — any block the roadmap marks as waiting on a human, with which item
- the log path

**Before any block runs**, claim this lane's identity in the registry and take the repo lease:

- Resolve `<lock_dir>` exactly as `scripts/check_lane_agents.py` does — `--lock-dir`, else
  `FLEET_LOCK_DIR`, else a `brain.toml` walk-up joined with `.fleet-locks` — see that script for
  the precedence rather than re-deriving it here.
- Write a lane-agent registry record, per `.claude/workflows/lane-agent.schema.json`
  (`agent_name`, `repo`, `lane`, `roadmap`, `started_at`, `heartbeat`), to
  `<lock_dir>/lane-agents/agent-<agent_name>.json`.
- Take the repo lease, per `.claude/workflows/lease.schema.json` (`repo`, `lane`, `agent`,
  `acquired_at`, `heartbeat`, `kind`), at `<lock_dir>/leases/lease-<repo>.json`. `kind` is
  `exclusive` for a lane that will commit — every real lane. `acquired_at` is IMMUTABLE, set once
  here and never re-stamped; `heartbeat` is the field that gets re-stamped to prove liveness — a
  lease written without it falls back to judging staleness on `acquired_at` and can never recover
  once that ages past the threshold, however live its holder actually is.
- Both writes happen before the first block launches. If either write fails, stop; do not start
  the chain holding only one of the two.

**Agent identity comes from the transport, never from self-report.** `agent_name` is the
ListAgents nickname this session is currently reachable at, taken from the transport-stamped
identity — never typed, assumed, or copied from a listing row. The reason is not that an agent
cannot know its own name; that claim is false and a reviewer will disprove it in one tool call.
The reason is that a **self-reported identity is unverifiable at the reader**: a lease whose
primary key is supplied by its own holder is ambiguous by construction, regardless of whether the
self-reporter happened to get it right. Two facts measured 2026-08-21, stronger one first:

1. **The self-identity capability is not uniform across sessions.** `engine-rs-6d`'s `ListAgents`
   output opens with a header naming the calling session ("This session is engine-rs-6d [871821]
   ... it is not listed below"). The identical call in a base-template session that same evening,
   checked three separate times, carries no such header — the output begins directly with "Peer
   sessions (N):" and the caller never appears in it. A registry primary key must not depend on a
   capability that some sessions have and others lack.
2. A base-template session wrote "base-template-b6 here" into two consecutive cross-session
   messages while the routing envelope carried "base-template-46" — it had read a `ListAgents`
   listing and taken a neighbour's row.

**If no transport-stamped identity is reachable at claim time, the claim fails loudly and the
lane does not start.** Never record a guessed name.

**What `check_lane_agents.py` does not do:** it cannot call `ListAgents`, so it can only report a
claim's or lease's timestamp age and agent name. It never decides "abandoned" versus "merely
slow" — joining staleness against live `ListAgents` output to make that call is the caller's job,
not the checker's.

No field is added to `.claude/workflows/lane.schema.json` by this claim — the registry and lease
records are separate files, not lane-record fields.

**Re-stamp both heartbeats at every block boundary, once `/orchestrate` is running.** The claim
written here carries `heartbeat`; the lease written here does too. `/orchestrate` rule 10 re-stamps
both — the claim's `heartbeat` and the lease's `heartbeat` — each time it releases and re-takes the
lease at a block boundary (do not restate that mechanism here). **At that same re-stamp, if the
claim carries the optional `current_block` and `block_started_at` fields, update them too** — set
`current_block` to the id of the block about to start and `block_started_at` to the current time,
alongside the `heartbeat` write, not as a separate pass. Both fields are optional; a claim without
them is unaffected. **Leave `started_at` (on the claim) and `acquired_at` (on the lease) alone at
every re-stamp** — those are acquisition timestamps set once, here, at first claim; re-stamping them
on a later heartbeat destroys the record of when the claim or lease was actually taken. **This is a
different clock from Step 3's fleet-concurrency re-registration above** — that heartbeat refreshes
the separate `<lock_dir>/fleet-concurrency/...` entry (a different file, on its own TTL clock) and
does nothing to the claim's or lease's `heartbeat`; the two must not be conflated.

Then run `/orchestrate <chain> <isolation-flag> [--engine ...] [--continue-on-fail]`.

Everything below is what you enforce *around* `/orchestrate` — it does not supersede that command's
own standing rules, it adds to them.

---

## The seven rules

Each has already cost a real run in this fleet.

1. **Never implement a block yourself, and never delegate one to a subagent.** Every block goes
   through `/sdlc-task` or `/sdlc-flow`; those engines spawn their own internal agents, which is
   theirs to do. A subagent is permitted **only** for read-only exploration, or a hotfix with no
   block of its own. Everything else — `/generate-tasks`, `/breakdown`, integration, verification,
   conflict resolution — runs inline in this session. A block built by an ad-hoc subagent has no
   spec, no gate, no state write and no review, and the chain's own verification will still look
   fine, so nothing catches it.

   **Two authoring-time rules for any spec or OKF frontmatter this step (or `/generate-tasks`,
   or hand-editing) produces or edits** — generalized from a lane that hit both in one day: a
   `related:` target must resolve to a real `doc_id` on a document that has actually been crawled,
   never a carryover slug or an invented id — an unresolved edge red-gates the whole corpus for
   every concurrent lane when `--graph` gates, not just the authoring one. A cross-repo target must
   be qualified `<repo>:<doc_id>` (e.g. `base-template:D48-downstream-harness-sync-script`); a bare
   `doc_id` resolves only within the authoring repo and is treated as unresolved everywhere else —
   see `docs/okf-frontmatter.md` for the full syntax. And a `validation_command` must be scoped to
   the task's own changes, never the whole working tree (e.g. never a working-tree-wide `git diff |
   grep` guard) — a tree-wide guard can never pass in a shared index with concurrent lanes and bails
   the block on an unrelated lane's uncommitted files.

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

   **Dual-write for anything that would qualify under Rule 6's "must not decide alone" list.** In
   addition to the notes.md entry, append one line to `planning/roadmaps/<roadmap>/escalations.jsonl`
   per `scripted-liaison-sweep-design` section 3, for every such item. Write **both**, never one
   instead of the other: the notes.md prose carries the reasoning for a human or a fresh agent to
   read; the `escalations.jsonl` line is what a script can diff without parsing prose. Dropping
   either regresses to the failure mode this record exists to fix from the opposite direction —
   losing either the reasoning trail or the machine-diffable signal.

   **The record shape** — every field `scripts/escalation.schema.json` requires, so the sweep
   script can validate without parsing prose. This is background to the schema, not a substitute
   for it; if the two ever disagree, the schema wins:

   ```json
   {
     "ts_utc": "2026-09-05T14:22:07Z",
     "repo": "engine-rs",
     "lane": "engine-rs-lane-a",
     "kind": "bail",
     "severity": "blocking",
     "channel": "notification",
     "gate_id": "roadmap-slug/engine-rs/BA.21.A",
     "summary": "BA.21.A bailed: spec's validation_command references a file deleted upstream.",
     "verified_by": "grep -n validation_command planning/BA.21.A/spec.md\nvalidation_command: pytest tests/removed_module_test.py",
     "durable_home": {"channel": "run-record", "ref": "engine-rs/planning/orchestration-run/roadmap-slug/notes.md#ba-21-a-bail"},
     "verified_at_sha": "a1b2c3d",
     "options": [
       {"key": "skip", "label": "Skip block"},
       {"key": "retarget", "label": "Retarget test"}
     ]
   }
   ```

   Notes on the fields most often gotten wrong:
   - `verified_by` MUST be a real command line followed by its real output on the next line —
     never a prose description like "measured" or "confirmed"; that shape fails validation.
     The only other accepted shape is `UNVERIFIED: <who claimed it>`, for a claim taken on
     someone's word rather than checked directly.
   - `verified_at_sha` is the short git SHA of the SUBJECT repo — the repo the `verified_by`
     command actually ran in — never the brain root's SHA. A brain SHA makes every cross-repo
     escalation look permanently stale.
   - `gate_id` is `<roadmap>/<repo>/<block>`, matching `notify-operator`'s `--gate-id` scoping.
   - `channel` is `notification` (only for a reducible yes/no already declared as such — requires
     `options`, 2-3 entries, each `label` at most 20 chars) or `session:<slug>` for anything
     richer (never carries `options`).
   - `summary` is capped at 1024 chars — trim to the substance and point at `durable_home` for
     the full text rather than truncating mid-thought.
   - `block` (the block id this escalation is scoped to) and `clears_when` (a predicate for when
     it resolves) are optional; omit them rather than writing a null placeholder if unused.

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

   **Verify what you just wrote, before continuing.** After every write or append to `notes.md`
   (and after writing the terminal `review.md`), run
   `python3 <path-to-base-template>/scripts/test_orchestration_run_contract.py` and confirm it
   exits 0. This is the writer's job, not some other repo's: the corpus is one shared vault, so an
   unvalidated record surfaces as a failure in a different repo's gate, blocking a lane that never
   touched it (the brain root `CLAUDE.md` rule — "any generator writing into the corpus must
   validate its own output"). On a violation attributable to the record just written, **fix it**
   (correct the `doc_id`, `roadmap:`, or `lifecycle` field per Rule 5 above) and re-run the checker
   — do not proceed with a known violation. **Deleting or emptying the record is never an
   acceptable way to make the check pass**; the record is the run's evidence.

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
   lane's repo. Those stop and get reported. Each case here is also a `kind` value in the
   escalation schema (`operator-gate`, `bail`, `disagreement`, `cross-repo-edit`) — write both the
   notes.md entry and the `escalations.jsonl` line per Rule 5 above.

7. **Urgent-item adoption.** Nothing before this rule let a P0 raised mid-run jump a chain — the
   2026-08-21 empty-tree P0 was adopted into a live lane ad hoc, with no defined procedure, because
   none existed. Adoption is three steps, always in this order, and never fewer:

   1. **File the block.** A real block record plus a `state.json` entry — never prose in a message,
      a note, or this session's transcript. An adopted item that only exists as a ping is lost the
      moment the session that received the ping ends.
   2. **Write the ledger row at adoption time, not at lane close**, with `origin_roadmap` set
      **explicitly** to the roadmap the block was originally allocated under — this is not a new
      rule, it is Rule 5's existing ledger contract (per
      `planning/decisions/D57-orchestration-run-artifact-contract.md`) applied the moment adoption
      happens rather than deferred to lane close. Do not re-derive the ledger schema here.
   3. **Ping the owning lane**, via the `ping-agent` skill, and write the same item to its durable
      home (the block record and ledger row from steps 1–2). The ping accelerates the durable
      channel; it never replaces it — a ping with no durable write behind it disappears the moment
      the receiving session clears.

   **What adoption is not.** It is not a licence to reorder this chain for anything below P0.
   Priority comes from `planning/decisions/D43-cross-domain-priority-graph.md`, cited by doc_id,
   never from the sender's own claim of urgency — the Standing operator convention below already
   states this for lingering items at lane close; adoption is the same rule applied mid-run. No
   field was added to `.claude/workflows/lane.schema.json` for this, and no role enum was
   introduced anywhere in this fleet — every lane agent does the same job in a different repo; the
   only distinct role is the commander (`BT.6.D`).

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
  alone, and a sweep missing `-uu` reports a false clean over the whole fleet. **`-uu` also disables
  `.gitignore`'s protection against `target/`/`node_modules/`, so pair it with
  `--glob '!**/target/**' --glob '!**/node_modules/**' --glob '!**/.git/**'`** — this fleet's repos
  carry ~43GB of Rust `target/` dirs, and an unscoped `-uu` sweep walking them pegs 350–500% CPU for
  minutes or hits a Bash timeout, reading as a hang rather than a slow search (measured 2026-09-03,
  `check-blast-radius`).
- `planning/state.json` is written with `ensure_ascii=False`. Script edits must round-trip with
  `json.dump(..., indent=2, ensure_ascii=False)` plus a trailing newline — the default escapes every
  em dash and turns a 3-field edit into ~130 lines of churn, and a conflict for every sibling lane.
- A leading `_` excludes a file from the corpus, so `_zz_*.md` debug probes are invisible to
  `validate-brain`.
- `timeout` does not exist on this macOS shell.
- **Check `git status --short <file>` before acting on a corpus error.** A validation error on a
  file that is *uncommitted or untracked* belongs to whoever is editing it right now — very likely a
  concurrent lane, not you — and "fixing" it overwrites work in flight; leave it, and say in the
  report which lane's file it was. A committed error belongs to nobody in particular and is yours to
  fix if it blocks the gate. The two cases look identical in `validate-brain` output, so the git
  check is the only thing separating them. Positive control: run the same `git status --short` on a
  file you know you just edited, and confirm it prints a line.
- Invoke `/sdlc-flow` and `/sdlc-task` from the **main session** — the `Workflow` runtime behind
  `.claude/workflows/` is unavailable to delegated subagents.
- The `Workflow` tool **inherits the session cwd**, so an engine launched from the wrong tree runs
  against another repo's `.claude/workflows/` without complaining. The tell is the script path in
  the launch result — check it names the repo you meant to drive before letting the engine proceed.

## Before finishing

Run this repo's own gates from `planning/harness.json`, then the corpus gate from `BRAIN_ROOT`.
Use the four read-only checks, **one invocation per flag** — `validate-brain`'s flags do not
compose (`main.rs` is an if/else-if chain, first flag wins; passing more than one silently runs
only the highest-precedence one and reports a real, passing result for a check that never ran):

```
bastion validate-brain --state
bastion validate-brain --graph
bastion validate-brain --links
bastion validate-brain --structure
```

`./scripts/sync/validate_brain.sh` is **not** this check — on a `primary` host it ends in an
`emit-state --write`, a commit, and a `git push` (see `derive-state-safely`), so using it as a
closing verification commits and pushes whatever the shared index holds, not just this lane's work.

**Note the `sync/`.** There is no `./scripts/validate_brain.sh` — that path exits **127**, which
reads as a failed corpus gate rather than a missing file, so a lane that copies it concludes the
fleet is red. Measured 2026-08-28: four lanes hit this in one night, and one then reached for the
path that does exist and ran the writer as its closing check.

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
bool`"-style constraint) still belongs in that program's roadmap document or the affected block's
own record — a lane record has no free-text field for it (per its schema); only the universal
shape below moves here.

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
- **A lingering item that is still `OPEN` when the lane closes promotes to a durable home — and
  `carryover[]` is only one of three.** It never moves into a successor run record: each
  `(repo, roadmap)` pair has exactly one record, addressed rather than rotated (Step 1E, rule 5),
  so there is no successor file to move it into. Route each item at promotion time:
  1. **Only a human can do it** — a decision, a credential, a judgement call, a thing the operator
     must look at → an `{"type":"operator", slug, exit, start, what?}` edge on the block it gates,
     **not** a carryover entry. This is the highest-volume misfiling point in the fleet: a lane
     closing promotes four to six items at once, and a carryover entry gates nothing, so operator
     work parked there is never forced while an operator edge blocks the work behind it. Measured
     2026-08-19 — **30 of the fleet's 202 `carryover[]` entries are operator work misfiled this
     way.** Entry form: `docs/state/state-schema.md`.
  2. **Permanently true** — a gotcha still true next month, a deliberate non-fix, a load-bearing
     measured number → `reference[]`. The signal is having no `clears_when` because nothing will
     ever make it stop being true.
  3. **Everything else** → `carryover[]` (D57 section 4), `kind` one of `defect` / `deferred` /
     `drift` / `env` (HQ D72; `constraint` and `known_issue` are retired). Prefer a typed
     `clears_when` — but never author one that is **already satisfied**, which retires the entry on
     its first sweep while the finding is still live. When no honest predicate exists, write prose
     and say why.
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

## Report

**<= 10 lines.** First line: outcome + whether it needs the operator. Then <= 6 one-line
bullets. Link paths; never restate a file. See the `report-to-the-operator` skill.

```
Lane <name> briefed: <n> blocks, engine <task|flow>, isolation <worktree|in-place>
- Held/blocked: <ids + what they wait on>, or "none"
- Concurrency: registered <repo> (<category>) | not heavy | at capacity, waiting
Starting: <first block id>
```
