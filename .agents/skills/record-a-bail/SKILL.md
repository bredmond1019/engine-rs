---
name: record-a-bail
description: >
  How to classify a bail at the moment it happens so the record on disk can later be counted —
  the artifact that broke versus the check that noticed and why the artifact is the cluster key,
  self versus foreign ownership against the task's declared files[], when two differently-worded
  bails are the same defect and when they are not, and the no-artifact case. Use whenever a
  block/task bails (engine-driven or hand-driven via /orchestrate) and a `bails[]` entry must be
  written — before writing check_id, failing_artifact, ownership, bail_class or reason.
---

# Recording a bail

This is the judgement half of `BT.ticket.bails-must-be-append-only`. The engines
(`.claude/workflows/sdlc-task.js`, `sdlc-flow.js`) mechanically APPEND an entry to `state.bails[]`
every time a task bails — see the `APPEND-ONLY (BT.ticket.bails-must-be-append-only)` comments
around the `bail_reason` assignment sites in both files. What they cannot do mechanically is decide
*what the entry means* — that classification is this skill, and it is what a lane driving
`/orchestrate` by hand must reproduce so a hand-written record matches an engine-written one.

## Why this exists: the measured failure

2026-08-24, `planning/open-work/bail-signature-replay.md`: the `autonomous-foundation` run produced
nine bails across two repos sharing one root cause (a fleet-wide gate reading a file the failing
lane did not own). Only seven `bail_reason` strings survived the run window, four of them mev's, and
clustering the surviving prose split that ONE defect into three groups — because one entry phrased
the failing test name differently from the others. **A field cannot be phrased differently; prose
can.** That is the whole argument for the record shape below: classify onto fixed fields at write
time, not after the fact.

## The record shape

```json
{
  "occurred_at": "2026-08-24T18:03:11Z",
  "task_id": 3,
  "check_id": "pytest-fixtures",
  "failing_artifact": "scripts/test_bails_record.py",
  "ownership": "self",
  "bail_class": 1,
  "reason": "one sentence, the same text bail_reason carries",
  "resolution": null
}
```

`bail_reason` (the pre-existing top-level field) always mirrors the newest entry's `reason` — it is
a derived convenience, not a second source of truth. Never write to `bail_reason` directly; write
the entry, and let the mirror follow.

## Classifying each field

### `check_id` — the check that noticed, not the thing that broke

This is whichever gating check's output the bail cites — a harness check name
(`planning/harness.json`), a `task_validation_N` label, or `terminal-reconcile` for the
reconcile-stage bail that has no task to attribute to. It answers "which command failed," not
"what is actually wrong." Do not conflate it with `failing_artifact` — see the next section.

### `failing_artifact` — the thing that actually broke, and it is the CLUSTER KEY

**The artifact, not the detector, is what makes two bails the same defect or different ones.** A
check name recurring across bails tells you a gate keeps firing; the *artifact* it names tells you
whether that is one root cause or five unrelated ones surfacing through the same gate. Set
`failing_artifact` to the path the check's failure output actually named. If the check named none —
a generic "tests failed" with no path, an environment/credential failure, a structural bail with
nothing to point at — leave it `null`. **Never fabricate a path to fill the field.** A null here is
information (this bail could not be attributed to a file) and a guessed path corrupts every future
cluster that reads it. This case is expected and explicitly out of scope for this ticket to fix —
`BT.ticket.checks-must-name-their-failing-artifact` is the separate work that makes more checks name
one. Until it lands, `failing_artifact` will be null for any check that names only a test.

### `ownership` — self vs. foreign, against the task's declared `files[]`

`self` when `failing_artifact` intersects the current task's declared `files[]` (from `tasks.json`);
`foreign` when it does not. Compute this with the exact set-intersection `renderWorkAssertion()`
already uses (`sdlc-task.js:310` and its `sdlc-flow.js` counterpart) — do not reimplement the
comparison by hand, and in particular do not eyeball "this looks like my file." `ownership` stays
`null` alongside a `null` `failing_artifact`: there is nothing to intersect against.

Foreign-state bails are the ones this ticket exists to make visible. In the measured
`autonomous-foundation` run, **five of the ten base-template chain's six bails were foreign** — a
task bailing on a file it never declared, because some other lane's state had drifted underneath
it. A `self` bail is an ordinary failing assertion in the task's own work; a `foreign` bail is a
signal about something else entirely, and undercounting it is exactly what happened when the only
surviving record was a long-running agent's memory.

### `bail_class` — the immediate-bail reason number

The triage agent already returns one of five numbered immediate-bail reasons (see `BAIL_REASONS` in
`sdlc-task.js`, mirrored in `sdlc-flow.js`):

1. Missing/undefined upstream dependency or symbol the spec assumes exists.
2. Spec ambiguity/contradiction — intended behavior is genuinely undeterminable.
3. Environment/credential/auth/network failure (not a code defect).
4. Change would require a destructive or out-of-scope action.
5. Same failure twice with no progress (stuck), or a structural design flaw needing a re-plan.

Record that number verbatim as `bail_class`. When a bail happens outside triage — e.g. the terminal
reconcile-stage bail, which has no task and no triage call — leave `bail_class` `null` rather than
guessing which of the five it resembles.

### `reason` — the one sentence

The same one-sentence explanation that would otherwise have gone into `bail_reason` alone. Write it
onto the entry; the top-level field mirrors it.

### `resolution` — written later, never at bail time

Leave `resolution: null` when the entry is created. It is filled in on whichever LATER run clears
the bail:

- `resumed-clean` — a subsequent attempt of the same task passed.
- `respec` — the spec changed between attempts (the bail stopped applying, not because the code got
  fixed, but because the target moved).
- `abandoned` — the block closed without that task ever passing.

**Never delete or overwrite an existing entry to record a resolution.** Find the entry (by
`task_id` + `check_id`, or the newest open one for that task) and set its `resolution` field in
place. The array's length must never decrease — that is the exact defect this ticket fixes: a
successful retry used to erase the bail that preceded it, and eight of nine measured foreign-state
bails left no trace on disk because of it.

## Same artifact, different cause — the counter-case

Two bails naming the same `failing_artifact` are **not** automatically the same defect. A file can
break for two unrelated reasons on two different days — an environment failure today, a genuine
logic bug next week. Before treating two entries as one recurring defect, check that `bail_class`
and the substance of `reason` agree, not just the artifact path. The artifact is the cluster *key*,
not the cluster *proof* — it narrows the search, it does not end it. Clustering and counting over
these records is separate, later work (out of scope for `BT.ticket.bails-must-be-append-only`); this
skill only governs how a single entry gets classified at write time so that later work has something
trustworthy to read.

## Hand-driven runs (`/orchestrate`)

A lane driving a block by hand, without the engines doing the bookkeeping, must still append the
same shape onto the run's `bails[]` before reporting the block blocked — same eight fields, same
rules above. A hand-written record that skips `ownership` or fabricates `failing_artifact` is worse
than an engine-driven one that leaves the field honestly `null`.
