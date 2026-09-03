---
type: Reference
title: Finding discipline — what earns a written record, and what a written record must carry
description: The shared rule every producer of findings follows — lanes, the commander, and the consolidators. Evidence travels with the finding or the finding does not exist; one observation is not a pattern; and an unexplained thing is recordable without being inflated into a defect.
doc_id: finding-discipline
layer: [factory]
project: base-template
status: active
keywords: [findings, evidence, provenance, corroboration, carryover, observation]
related: [write-carryover-entry, block-registration, ping-agent, D43-cross-domain-priority-graph]
---

# Finding discipline

**Read and follow this whenever you are about to write down something that is wrong.**
`/orchestrate`, `/begin-orchestration`, `/orchestration-commander`, `/consolidate-run` and
`/consolidate-fleet` all point here instead of carrying their own copy.

`write-carryover-entry` tells you how to author an entry once you have decided it deserves to
exist. **This file is about that decision**, and about what the record must carry so the decision
can be re-checked by someone who was not there.

---

## The problem this exists to solve

**An agent asked to find problems will find problems.** That is not dishonesty, it is what the
instruction selects for — and the fleet has the numbers. Three independent carryover audits
measured **32%, 32% and 26% dead** on different slices: roughly a third of everything ever filed
was not a live finding by the time anyone looked. One two-day triage took the pool from ~450
entries to ~200.

So the cost of a written finding is not zero and it is not paid by the author. It is paid by
whoever audits it weeks later, and a third of the time they pay it for nothing.

**The fix is not a stricter filter downstream.** By the time a finding reaches disposal, the
evidence for it is in a session that has ended. The filter has to be at the moment of writing,
where the evidence is still in hand.

---

## Rule 1 — Evidence travels with the finding, or the finding did not happen

**A finding is written into the same artifact as the evidence for it.** Not a pointer to a session,
not "I checked", not "confirmed" — the thing a later reader can re-run or re-read:

| Acceptable evidence | Not evidence |
|---|---|
| `path:line` a reader can open | "the engine does this" |
| The command **and its output**, pasted | "verified", "measured", "confirmed" |
| A commit sha, PR number, run id, message id | "as discussed", "per the earlier finding" |
| A count, with the command that produced it | "several", "repeatedly", "often" |

**A bare adjective is the failure mode.** `escalations.jsonl`'s own schema rejects `verified_by`
values that are not a command-plus-output block, for exactly this reason — and 18 of 25 live
records still fail it.

If you cannot produce evidence in the artifact, you have two honest options: go get it, or write it
as an **observation** (Rule 4). Neither of those is "file it anyway and let disposal sort it out."

## Rule 2 — Every finding carries its provenance, and the tag is not decoration

Tag every finding with how you know it:

- **`verified`** — you ran it or read it yourself, and the artifact shows the command or the
  citation.
- **`relayed`** — another lane or agent told you. Their claim is not yours until you check it
  (`ping-agent`). A relayed finding you did not verify says so.
- **`assumed`** — you inferred it. Legitimate, and it must be labelled, because the next reader
  will otherwise treat it as measured.
- **`operator-stated`** — the operator said it. Do **not** harden this into a diagnosis: on
  2026-09-02 a commander turned an operator statement into a claim that a lane had been paused and
  resumed, and the lane disputed it with evidence.

The hand-written pattern analysis that this discipline generalises from was useful *because* every
claim carried one of these tags — a reviewer could tell in one pass what to re-check.

## Rule 3 — One observation is not a pattern, and the instance count is a field

**A single occurrence is an instance. A pattern is a counted set.** Where you assert a pattern,
state the count and how you counted it:

```
breadth: 7 repos, 17 instances   ← countable, re-checkable
breadth: "widespread"            ← not a finding, an impression
```

**This is the load-bearing filter.** Five records independently describing the same failure is a
mechanism worth a block. One record describing something that *could* generalise is one record.
An uncounted claim of breadth is the single most common way a small thing becomes a program.

Corollary: **if you cannot count it, say `instances: unknown` rather than omitting the field.** An
absent count reads as "not applicable"; an explicit unknown reads as what it is.

## Rule 4 — The unexplained observation is a real category. Use it instead of inflating

Some things genuinely cannot be explained when you see them — a run that behaved oddly, a number
that does not reconcile, a gate that passed when you expected red. **These are worth recording and
they are not defects.**

Record them as an **observation**: what you saw, the evidence, and explicitly *that you do not know
why*. An observation has no owner, no `clears_when`, and no route — it is a note for whoever sees
the second instance.

**Why this rule exists:** without a category for "odd, unexplained, real", the only way to write it
down is to dress it as a defect — which mints an owner, a predicate and a repo that were all
invented. That is the inflation this whole file is trying to stop. Make the honest record cheap and
the dishonest one unnecessary.

Two or more observations that match are the point at which you have a finding, and Rule 3's count
is already there to support it.

## Rule 5 — Route by what the evidence supports, not by how it feels

| The evidence supports | Route |
|---|---|
| A counted pattern, and the change is specified | a **block** |
| A real defect, but nobody has specified the fix | `carryover[]` with a `clears_when` that can fail |
| A permanently-true fact | `reference[]` |
| Something only a human can decide or do | an `operator` edge on the block it gates |
| Something odd, real, unexplained | an **observation** (Rule 4) |
| A thing you think might be true | nothing. Go get evidence, or let it go |

**A block whose `files[]` and `acceptance_criteria` have to be invented is not a block yet.**
Measured 2026-09-02: of eleven mechanisms proposed for filing, **ten** could not ground
`acceptance_criteria`, `out_of_scope`, `sdlc_workflow` or `files.new` from the analysis — the
filing agent derived them. A finding whose work cannot be specified from its own evidence is a
carryover entry until someone specifies it. Filing it as a block does not make it ready; it makes
it look ready.

## Rule 6 — The cut is part of the output

**Say what you did not file, and why.** A pass that reports only what it found has not filtered;
it has collected. Every producer's report names:

- what was written down, by route;
- what was **considered and cut**, with the reason (no evidence · single instance · already filed ·
  not ours);
- anything recorded as an observation rather than a finding.

A run that cuts nothing is reporting a fact about its instructions, not about the system.

---

## The one-line test

> **Could a reader who was not there re-check this from what I wrote, and would they find the same
> thing?**

If no — get the evidence, downgrade it to an observation, or drop it. Those are the three honest
outcomes, and "file it and let someone else decide" is not among them.
