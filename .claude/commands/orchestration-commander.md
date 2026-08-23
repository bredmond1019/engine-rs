# Orchestration Commander — a stateless drain that re-derives and reports the remainder

One drain: a single Claude turn, woken by `scripts/commander_drain.sh` via `bastion ask`, that
routes this repo's message queue, re-derives the fleet's surfaces the only safe way (running the
derivation, never guessing which dirty files are derived), and reports whatever is left over that
a human or a lane needs to see. It never implements a block and never edits another lane's chain.

## Variables

None. A drain takes no arguments — everything it needs is already on disk: the queue, the lease
and registry records, and the repo's own `planning/state.json`. See "Stateless per drain" below for
why that is deliberate, not a limitation.

## The six steps, in order

### 1. Drain the queue
Use `scripts/check_messages.py`'s `drain_queue()` to move everything from this lane's
`inbox/` into `processing/`, then `complete_message()` per message once it has been triaged in
step 2. **Do not restate the queue layout or the receipts ledger here** — `BT.6.B` (shipped as
`scripts/check_messages.py`) owns both; read its module docstring if the layout is unclear.

### 2. Route and relay
For each drained message, decide where it goes and how urgently, then relay it (post a reply, file
a block, ping the owning lane — whatever the message's `kind` calls for; see the `ping-agent`
skill for the send/verify/four-verdict contract, which this step follows and does not restate).

- **Priority is assigned by this drain, from `planning/decisions/D43-cross-domain-priority-graph.md`
  (`doc_id: D43-cross-domain-priority-graph`) — cite it by doc_id in whatever you file.** Never read
  a priority off the message itself: `message.schema.json` has **no** `priority`/`urgency` field,
  deliberately — `check_messages.py` treats one appearing anywhere in the envelope as a named
  error, not an unknown key, because a sender-declared priority inflates to always-urgent and forks
  a second rubric alongside D43, which owns priority in this fleet.
- Only after a message is fully routed does it get `complete_message()`'d into `done/`. A message
  left in `processing/` across drains is not a bug — it is evidence something did not finish; the
  next drain picks it up from `processing/`, not `inbox/`, so nothing is drained twice.

### 3. Re-derive and commit — never detect
Run the brain's `scripts/emit_state_write.sh`. Do not stage or commit anything yourself beyond
what that script's own manifest names.

**THE COMMITTING RULE IS THE WHOLE POINT — read this in full before touching git.**

The commander **never** runs `git status` and guesses which dirty files "look derived." Derivation
is idempotent: running `emit_state_write.sh` (which runs `bastion emit-state --write`, then
`commit_routine_updates.sh`) gives a **positive, proven allowlist for free**, captured in
`$LOG_DIR/.emit_wrote` as the `I_EMIT_WROTE` manifest:

- A derived file a lane already committed → `emit-state` is idempotent, so this is a no-op; nothing
  new to stage.
- A derived file left dirty by a lane that finished but never re-derived → rewritten, named in the
  manifest, staged and committed by `commit_routine_updates.sh` as its own commit.
- An **authored** file — anything a human or an agent wrote by hand, not a pure function of
  `state.json` — never appears in the manifest, because `emit-state` never touches it. It is
  therefore never staged, never committed, by this drain. It becomes step 4's job.

**The only commit pathspec that ever exists in this procedure is the `I_EMIT_WROTE` manifest** (plus
the one directory `commit_routine_updates.sh` also legitimately owns,
`planning/carryover-follow-up-routine/` — see that script's own header). Never widen it, never add
a second pathspec, never fall back to a blanket `add`.

**Why this is not optional caution — it is the incident this design retires.** A `git status`
sweep committing "whatever looks dirty" IS the four-incident mechanism this whole lane-coordination
set exists to close out. At the brain root, **one** git repo tracks every repo's `planning/`
directory in the fleet (CLAUDE.md standing rule 10) — a sweep run there does not commit one lane's
work, it commits **four lanes' in-flight work at once**, because their staged-but-uncommitted edits
all live in the same index. `emit_state_write.sh`/`commit_routine_updates.sh` already encode this
the hard way (see their headers: BRAIN_ROLE-gated, one path staged at a time, the resolved-symlink
fix for the `beyond a symbolic link` failure) — this drain's whole obligation on this step is to
call that machinery and trust its manifest, not to reinvent detection.

Cross-repo path note: `scripts/emit_state_write.sh`, `scripts/lib.sh` and
`scripts/commit_routine_updates.sh` live in the **brain repo**, not in `base-template`. Resolve
`<brain_root>` by walking up for `brain.toml` — the same resolution `scripts/check_lane_agents.py`
and `scripts/check_messages.py` already use for `<lock_dir>` — never assume a repo-relative path.

**Not the exclusive writer.** Running `emit_state_write.sh` here makes the commander *an*
idempotent writer of derived state — not the exclusive one. The SDLC engines' own bookkeep stage
still shells out to
`mev emit-state --write` directly, and this drain does not change that. Making the commander the
sole writer is Scope B of this initiative — blocked on `mev set-block-status --no-emit`, a
`harness.json` post-emit hook, and engine changes across three prompt sites — and is **deliberately
not attempted here**.

### 4. Compute and report the authored-orphan remainder
```
remainder = git status --porcelain (this run's snapshot) − I_EMIT_WROTE manifest paths
```
Every path left in the remainder is **authored** — a human or an agent wrote it and it is not a
pure function of `state.json` — so it is **surfaced, never touched**. Route each one by lease
state, into exactly three cases:

1. **Lease held, agent live** — the owning lane's `<lock_dir>/leases/lease-<repo>.json` names a
   claimant, and that agent name appears in `ListAgents`. **Silent.** The lane is mid-work; a dirty
   tree is exactly what mid-work looks like.
2. **Lease held, agent absent or heartbeat stale** — the lease names a claimant, but that agent is
   either missing from `ListAgents` or its registry claim's `heartbeat` is older than
   `scripts/check_lane_agents.py`'s staleness threshold. **Named recovery item** — report the repo,
   lane, agent name and file(s); this is a candidate abandoned lane, and a human decides its fate
   (this drain never reaps a lease itself — see out-of-scope below).
3. **No lease at all** on the repo the file lives in. **Alert** via the brain's `lib.sh`
   `send_alert()` — an authored file dirty with nothing holding the repo is unexplained by any
   lane this drain knows about.

`scripts/check_lane_agents.py` gives you the timestamp-age half of case 2 (a lease's `acquired_at`
or a registry claim's `heartbeat`) but **cannot call `ListAgents`** — by its own docstring, it
"never decides 'abandoned' vs. 'slow.'" Joining that timestamp signal against live `ListAgents`
membership to tell "agent absent" (case 2) apart from "agent live but heartbeat merely old" (still
case 1) is **this drain's job**, not the script's.

### 5. Maintain the board
Update `planning/open-work/index.md` — a single durable listing of everything step 4 surfaced that
is still open, so a human scanning one file sees every named recovery item and alert across every
past drain, not just this one. Append/update rather than rewrite: an item closes only when a human
resolves it or a later drain observes it gone, not when a newer drain simply forgets to relist it.
Create the file (with OKF frontmatter — this repo's standing rule 5) and its `planning/index.md`
row on the first drain that needs it.

### 6. Stamp the heartbeat
Write the last-drain heartbeat file (the same file `scripts/commander_drain.sh` checks for
staleness) with the current UTC timestamp, unconditionally — even a drain that did nothing in
steps 1-5 (empty inbox, nothing dirty, no orphans) still proves the drain ran by stamping this. A
missing or stale heartbeat is itself the signal that drains have stopped happening.

## Stateless per drain

Nothing is carried between drains except what is already on disk: the queue directories, the
lease/registry records, `planning/open-work/index.md`, and the heartbeat file. Each drain reads
that state fresh and writes back only what changed. This is deliberate, not an oversight — a
stateless drain never accumulates context across ~48+ runs a day, so the cost of a drain does not
grow with how long the commander has been running, and a drain that crashes mid-way loses nothing
that the next drain cannot reconstruct from disk.

## Rules that keep the commander from becoming the problem

- **Lanes never block on the commander.** A lane enqueues a message and continues; it does not wait
  for a drain. Derived state sitting twenty minutes stale is harmless. A lane stalled waiting on
  the commander is not — that would make the commander exactly the bottleneck this design exists to
  avoid.
- **Requeue is by message, never by editing a running lane's `lane-<name>.json`.** `/orchestrate`
  parses a lane file's `blocks[]` once, at step 1 — a running lane's chain is a launch-time
  snapshot, exactly as base-template standing rule 10 describes for the SDLC engines themselves.
  Editing that file under a lane already running it produces a silent divergence between what the
  lane believes its chain is and what the file now says, which reads as an agent ignoring
  instructions rather than as a stale snapshot. To change a running lane's work, send it a message
  (step 2's routing) and let it pick the change up at its own next block boundary — never touch its
  lane file directly.
- **The commander never runs an SDLC engine and never implements a block.** Its only writes are:
  queue-directory transitions (step 1), relayed messages (step 2), whatever
  `emit_state_write.sh` derives and commits (step 3), `planning/open-work/index.md` (step 5), and
  the heartbeat file (step 6). If a drain finds itself about to touch application code or a spec's
  `tasks.json`, stop — that is a lane's job, not this one's.

## Out of scope (do not attempt here)

- **Exclusive emit-state ownership.** Covered above under step 3 — this is Scope B, blocked on
  `mev`, and deliberately not this block.
- **Adding a cron entry.** The brain's `routine.sh` already shells to `bastion emit-state --write`
  nightly; that standing arrangement is untouched by this command.
- **Running any SDLC engine, or implementing any block.**
- **Editing another repo's authored `state.json`,** or committing in a repo whose lease another
  lane currently holds.
- **Reaping a stale lease.** Case 2 above reports the abandoned lane as a named recovery item; a
  human, or the owning repo's own next session, decides whether and how to reclaim it. This drain
  never releases a lease it does not itself hold.

## Traps

- A piped command's `$?` reports the pipe's exit code, not the command's — never pipe
  `emit_state_write.sh` or `bastion emit-state` output into something else and check `$?`
  afterward; redirect to a file and check the command's own exit status.
- `rg`/`find` are symlink-blind and every `planning/` is a symlink into the brain vault — pass `-L`
  when scanning for authored orphans or reading queue/lease state that lives under `planning/`.
- Do not confuse "no lease on this repo" (case 3, an alert) with "lease held by a lane not in this
  drain's own repo" — a lease is scoped per repo; check the lease file for *this* repo specifically,
  not whether any lease exists anywhere.
- `commit_routine_updates.sh` already resolves each `I_EMIT_WROTE` path with `realpath` and stages
  one path at a time specifically because a single symlinked path in a batched `git add` fails the
  *whole* call silently — do not "simplify" this drain's own path-staging by reverting to a batched
  add if you ever touch that script; that regression has already happened once (measured
  2026-08-21, 21 dirty files reported clean).

## Report

One line per drain, appended to the running log the wrapper maintains (never a new file per
drain — see "Stateless per drain"):

```
<UTC timestamp> · drained <n> · routed <n> · committed <manifest paths, or "none"> · orphans: <silent n / recovery n / alert n> · heartbeat stamped
```

Then, only if non-empty:
- **Named recovery items** — repo, lane, agent, file(s), lease age.
- **Alerts sent** — repo, file(s), why no lease explains them.
- **Anything routed at P0** — message subject, D43 doc_id, where it was filed.

Silence on the empty lines is the normal case for most of the ~48+ drains a day; do not pad the
report to look busy.
