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

### 1. Validate and survey the whole queue tree, then drain this lane's inbox
`drain_queue()` and `complete_message()` are Python functions inside `scripts/check_messages.py`
— a drain is a Claude turn with shell access, not a Python process, so naming a function is not an
instruction a drain can execute. Everything below is a **shell command**, run as written.

**0. Read the open-work board FIRST — before the queue sweep in (a), before anything else in this
drain.** `planning/open-work/index.md` is the fleet's single durable listing of every named
recovery item and alert a past drain has already surfaced and left open. Read its open rows now,
so every later step in this drain already knows what has been found before, and a repeat can be
reported as an instance of an existing row (see steps 4-5) instead of being rediscovered from
scratch. Measured cost of skipping this: a finding written to this board at 05:45Z was
re-diagnosed from first principles five hours later by a different role, because that role never
read the board before it started. If `planning/open-work/index.md` does not exist yet, note that
and continue — step 5 creates it on the first drain that needs it.

**a. Validate every message record and layout invariant across every lane, not just this one's.**
```
python3 scripts/check_messages.py --quiet
```
`discover_queues()` walks `<lock_dir>/queue/<repo>/<lane>/` for every repo and lane under the
resolved lock dir — this single invocation already covers the whole tree, which is exactly what
thirteen consecutive drains never ran. Exit 0 means every record and receipt-backed transition in
every lane's `inbox/`/`processing/`/`done/` is well-formed; a nonzero exit means at least one is
broken, and its `FAIL <path>` lines name which. Resolve `<lock_dir>` the same way the script does
— `--lock-dir`, else `FLEET_LOCK_DIR`, else a `brain.toml` found by walking up from cwd — never
assume a repo-relative path (see `scripts/check_lane_agents.py`'s identical precedence).

**b. Age-check every inbox file, and report an absent queue directory and an empty one as
different findings.** `check_messages.py` validates message shape; it does not report how long a
file has waited, and it does not distinguish "this lane has never written a queue dir" from "this
lane's queue dir exists and is empty." Both distinctions matter — a count of undrained messages
reads as backlog, "10 hours old" reads as a failure; and "no queue dir for my lane" is why the
first thirteen drains read "nothing to drain" instead of "never checked."
```
for q in "$LOCK_DIR"/queue/*/*/; do
  q="${q%/}"
  inbox="$q/inbox"
  if [ ! -d "$inbox" ]; then
    echo "ABSENT  $inbox"
  elif [ -z "$(ls -A "$inbox" 2>/dev/null)" ]; then
    echo "EMPTY   $inbox"
  else
    for f in "$inbox"/*.json; do
      [ -e "$f" ] || continue
      now=$(date +%s)
      mtime=$(stat -f %m "$f" 2>/dev/null || stat -c %Y "$f")
      echo "MSG     $f  age=$(( (now - mtime) / 60 ))m"
    done
  fi
done
```
Run this over every `<lock_dir>/queue/<repo>/<lane>/` the tree contains — not only the drain's
own lane. Every `MSG` line surfaced for a lane other than this one is **reported, not acted on**
(step 4's board is where it lands) — routing another lane's message is that lane's call, never
this drain's (see "Out of scope" precedent in the block that shipped this step,
`BT.ticket.commander-must-validate-the-whole-queue-tree`).

**c. Drain this lane's own inbox for step 2 to triage.** Only this lane's messages get moved and
routed here; every other lane's `MSG`/`EMPTY`/`ABSENT` finding from (b) is report-only.
```
python3 -c "
import sys; sys.path.insert(0, 'scripts')
from check_messages import resolve_lock_dir, drain_queue
lock_dir = resolve_lock_dir()
queue_dir = lock_dir / 'queue' / '<this repo>' / '<this lane>'
for record in drain_queue(queue_dir):
    print(record['message_id'])
"
```
substituting this lane's actual `<repo>`/`<lane>` path segments. Each printed `message_id` is now
sitting in `processing/`, ready for step 2; `complete_message(queue_dir, message_id)` moves it to
`done/` the same way, once triaged. **Do not restate the queue layout or the receipts ledger
here** — `BT.6.B` (shipped as `scripts/check_messages.py`) owns both; read its module docstring
if the layout is unclear.

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
state, checking case 0 first, then falling through to exactly three more cases:

**Before filing any of the four cases below as a fresh finding, check it against the open-work
board read in step 1.** If this repo/cause already has an open row on `planning/open-work/index.md`,
report this occurrence as `instance N of <row>` and update that row's count — never append a new
row for a cause already on the board. Re-deriving a finding that is already written down is
costly and invisible, because it looks exactly like fresh work while producing nothing new.

0. **No lease on the repo, but a live lane elsewhere is a known cross-repo writer.** Before
   reaching for case 3's alert, check whether the file's dirty repo holds no lease *because* some
   other live lane — found the same way case 1/2 already find one, by joining `ListAgents` against
   lease/claim data — is a **known cross-repo writer** touching this repo as a side effect of its
   own work (e.g. a run like `mev graph-findings . --write` that legitimately dirties many repos
   from one lane). When that join identifies such a lane, **report once, attributed to that lane
   and covering every file it explains** — never once per file. This is a REPORT, not a
   suppression: the work stays fully visible, exactly as case 1/2's report-only outcomes do: it is
   simply one line naming the writing lane and its files, not N separate alerts for N files one
   lane legitimately touched. It introduces no new capability or data source — the `ListAgents`
   join is the same one case 1/2 already perform, just checked against a different repo than the
   one the lane holds a lease on. If no live lane can be identified as the writer for a given file,
   that file is NOT case 0 — it falls through to case 3 and is alerted, unchanged.
1. **Lease held, agent live** — the owning lane's `<lock_dir>/leases/lease-<repo>.json` names a
   claimant, and that agent name appears in `ListAgents`. **Silent.** The lane is mid-work; a dirty
   tree is exactly what mid-work looks like.
2. **Lease held, heartbeat stale** — the lease names a claimant, and its registry claim's
   `heartbeat` (or, absent that, the lease's own `acquired_at`) is older than
   `scripts/check_lane_agents.py`'s staleness threshold. Staleness alone is not the verdict — join
   it against the claimant's live state, ONE decision procedure with three outcomes:
   - **Stale + idle** (the claimant is missing from `ListAgents` entirely, or present but not
     actively working) — **Named recovery item**, a candidate abandoned lane: report the repo,
     lane, agent name and file(s), and a human decides its fate (this drain never reaps a lease
     itself — see out-of-scope below).
   - **Stale + busy** (the claimant is live in `ListAgents` and actively working — a long block,
     not an abandoned one) — **report-only**, one line noting the repo/lane/agent and that it is
     still busy. Never a named recovery item; nothing here needs a human decision.
   - **Stale + blocked on a human** (the claimant is live and its current block is gated on an
     unresolved `operator`/`approval` edge in that repo's `state.json`) — **report-only**, same as
     above. A lane blocked on an operator is the HEALTHIEST state a blocked lane can be in — the
     heartbeat goes stale precisely because the lane is correctly waiting, not because it died —
     and must never be reported as abandoned. Three false recovery items on a previous run came
     from exactly this conflation. If this drain's own report is the thing that should reach the
     operator (not merely note the healthy-blocked lane in the written report) — see the
     `notify-operator` skill for whether that rises to a real notification and which verb to use.

   This is one decision procedure, not two, and it stays that way deliberately. The branch above —
   `ListAgents` liveness joined against heartbeat staleness — is the FLOOR: it alone decides which
   of the three outcomes applies, and it must keep deciding that identically whether or not the
   claim carries `current_block`/`block_started_at`. **When those fields ARE present
   (`BT.ticket.lanes-do-not-record-their-current-block`), use `block_started_at`'s age only to
   ANNOTATE the outcome the floor already picked — never to move a file between outcomes:**
   - Outcome **Stale + busy**: report the block age alongside the existing one-liner (e.g. "still
     busy, 6m into `<current_block>`"). A recent `block_started_at` is the ordinary shape of this
     outcome — a long block naturally leaves the heartbeat stale between the boundary re-stamps
     that move both fields together — so it confirms "slow, not stuck" without changing the
     verdict, which was already report-only.
   - Outcome **Stale + idle**: report the block age alongside the named recovery item (e.g.
     "abandoned mid-`<current_block>`, block age 3h12m"). A large block age here is the strongest
     form of the same candidate signal `ListAgents` idleness already produced — it sharpens what
     the human is told, it does not create the candidate; `ListAgents` idleness alone already did.
   - Outcome **Stale + blocked on a human**: report the block age the same way, for context; the
     operator/approval gate is still what makes this report-only, unchanged.
   - **Fields absent on the claim**: no annotation is possible, and none is attempted — the three
     outcomes above are reached and reported exactly as they were before this refinement existed.

   **Walkthrough (recorded because a schema test cannot show this — see task 3 of
   `BT.ticket.lanes-do-not-record-their-current-block`):**
   1. *Fields absent.* Claimant live and busy, heartbeat stale, no `current_block`/
      `block_started_at` on the claim → outcome **Stale + busy**, report-only, one line, no block
      age mentioned — identical to this block's pre-refinement behaviour.
   2. *Fields present, block 3 minutes old.* Same claimant/heartbeat state, claim now carries
      `current_block`/`block_started_at` and the block started 3 minutes ago → outcome is still
      **Stale + busy** (the floor did not change), report-only, now annotated "6m busy, 3m into
      `<current_block>`" — read as *slow*, not stuck.
   3. *Fields present, block 3 hours old, heartbeat stale, session idle.* `ListAgents` shows the
      claimant absent or idle (the floor's own idle condition, unchanged) and the claim's block age
      is 3 hours → outcome **Stale + idle**, Named recovery item, annotated with the 3h block age —
      the candidate the human should look at first.
3. **No lease at all** on the repo the file lives in, and case 0 found no live cross-repo writer
   to attribute it to. **Alert** via the brain's `lib.sh` `send_alert()` — an authored file dirty
   with nothing holding the repo, and no lane explaining it, is unexplained by any lane this drain
   knows about. `send_alert()` is the automation-side alert path; reaching the human operator
   directly mid-run is a separate decision — see the `notify-operator` skill for when that is
   warranted and which verb to use.

`scripts/check_lane_agents.py` gives you the timestamp-age half of case 2 (a lease's `acquired_at`
or a registry claim's `heartbeat`) but **cannot call `ListAgents`** — by its own docstring, it
"never decides 'abandoned' vs. 'slow.'" It reports timestamp age only; no `ListAgents` logic
belongs in the script. Joining that timestamp signal against live `ListAgents` membership and state
to tell "agent absent or idle" (candidate abandoned lane) apart from "agent live and busy" or
"agent live and blocked on a human" (both report-only, never a candidate) is **this drain's job**,
not the script's.

**Walkthrough, case 0 (the measured scenario, `commander-retro.md` section E):** a single lane
runs `mev graph-findings . --write`, which legitimately dirties 24 repos as a side effect of one
piece of work; 8 of those repos are the lane's own held leases (case 1, silent) and 16 hold no
lease at all. Read literally without case 0, each of those 16 repos' dirty files is case 3 —
sixteen separate alerts for one lane's legitimate write. With case 0: the `ListAgents` join
identifies the same lane as live and as the writer touching all 16 repos, so those files are
reported **once**, attributed to that lane and listing all 16 repos/files together — visible on
the board same as before, just not sixteen alerts for one cause. **Walkthrough, the genuine
orphan (unchanged):** a file is dirty in a repo holding no lease, and no live lane in `ListAgents`
is a known cross-repo writer touching that repo. Case 0 does not match — nothing to attribute it
to — so it falls through to case 3 and is alerted exactly as before. A case that quiets both
scenarios would be a regression wearing a fix's clothes; case 0 quiets only the first.

### 5. Maintain the board
Update `planning/open-work/index.md` — a single durable listing of everything step 4 surfaced that
is still open, so a human scanning one file sees every named recovery item and alert across every
past drain, not just this one. Append/update rather than rewrite: an item closes only when a human
resolves it or a later drain observes it gone, not when a newer drain simply forgets to relist it.
Create the file (with OKF frontmatter — this repo's standing rule 5) and its `planning/index.md`
row on the first drain that needs it.

**A recurring cause updates its existing row instead of appending a new one.** When step 4 matched
this occurrence against a row already open on the board (per the check added there), write the
match back here as `instance N of <row>` on that same row — incrementing its count — rather than
adding a fresh row for the same cause. A board with fewer, denser rows that each carry an accurate
instance count is the point: anything reading this file afterward sees how many times a cause has
recurred instead of re-deriving that count from N separate rows.

**Forward-looking note.** Once `BT.ticket.bails-must-be-append-only`
(`planning/blocks/BT.ticket.bails-must-be-append-only.json`) lands, this step stops counting
instances by comparing against the board by hand — the instance count is READ from the append-only
bail records themselves, which become the source of truth for how many times a cause has recurred.
Until then, the board comparison above is the only mechanism.

### 6. Stamp the heartbeat
Write the last-drain heartbeat file (the same file `scripts/commander_drain.sh` checks for
staleness) with the current UTC timestamp, unconditionally — even a drain that did nothing in
steps 1-5 (empty inbox, nothing dirty, no orphans) still proves the drain ran by stamping this. A
missing or stale heartbeat is itself the signal that drains have stopped happening.

**Then append this drain's record to the durable evidence log.** Track three counts as you work
steps 1-2 — `DRAINED` (messages step 1c moved out of this lane's `inbox/`), `ROUTED` (messages
step 2 relayed), `COMPLETED` (messages step 2 moved into `done/`) — and pass them here:
```
python3 "<brain_root>/scripts/drain_log.py" record \
  --roadmap "<roadmap>" --lock-dir "$LOCK_DIR" --brain-root "<brain_root>" \
  --drained "$DRAINED" --routed "$ROUTED" --completed "$COMPLETED"
```
`<brain_root>` is the same walk-up-for-`brain.toml` resolution step 3 already uses — never a
repo-relative path. `<roadmap>` is this drain's roadmap slug, resolved by finding every
`planning/roadmaps/*/lane-<this repo>.json` under `<brain_root>` (step 1's own repo name) whose
`"lane"` matches this drain's lane: if exactly one such lane file exists, its `"roadmap"` field
names the roadmap. **Degrade-and-report, never abort, when a roadmap cannot be resolved this
way** — zero matching lane files (this repo/lane is not part of any roadmap run right now) or
more than one (this repo/lane is in-flight on two roadmaps at once, and this drain does not
guess which one the queue activity belongs to) both mean: skip the `drain_log.py` call, note
"no roadmap resolved — drain-log record skipped" in this drain's report line, and continue —
the heartbeat above has already been stamped, and steps 1-5's work is already done regardless.
`drain_log.py` itself exits 2 if `--roadmap` names a directory that does not exist; treat that
identically — log it, do not fail the drain over it.

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
  the heartbeat file and drain-log record (step 6). If a drain finds itself about to touch
  application code or a spec's `tasks.json`, stop — that is a lane's job, not this one's.

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
<UTC timestamp> · drained <n> · routed <n> · committed <manifest paths, or "none"> · orphans: <silent n / recovery n / alert n> · heartbeat stamped · drain-log: <recorded to <roadmap> | no roadmap resolved, skipped>
```

Then, only if non-empty:
- **Named recovery items** — repo, lane, agent, file(s), lease age.
- **Alerts sent** — repo, file(s), why no lease explains them.
- **Anything routed at P0** — message subject, D43 doc_id, where it was filed.

Silence on the empty lines is the normal case for most of the ~48+ drains a day; do not pad the
report to look busy.
