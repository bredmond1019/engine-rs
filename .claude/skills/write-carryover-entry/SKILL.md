---
name: write-carryover-entry
description: How to write a carryover[] entry that can actually die — whether the finding belongs in carryover[] at all rather than an operator edge, a block, or reference[], the `needs` value that says what kind of work closes it, how to author a clears_when predicate that will still fire in six weeks, the eight measured ways a predicate silently never fires, how to write text that survives its author being wrong, and the finding_id discipline that makes one finding one fix. Use BEFORE adding any carryover[] entry, at every /handoff, /wrap-up, /log-work and lane close, before filing a finding an orchestration run turned up, and when a sweep reports an entry CLEARED that is plainly still live.
allowed-tools: Bash(mev:*) Bash(bastion:*) Bash(python3:*) Bash(git:*) Bash(grep:*) Bash(rg:*) Bash(ls:*) Bash(test:*)
---

# Writing a `carryover[]` entry

> **Paths below are relative to the brain root** — the directory containing `brain.toml`, found by
> walking up from wherever you are. This skill is synced into every repo, so a repo-relative link
> would be wrong in most of them.

**An entry's job is to die.** It exists to hold a finding until the finding stops being true, and
then to go away on its own. Judge every field by that: does this make the entry retirable by a
machine, or does it commit some future human to reading 400 words to find out the work landed a
month ago?

That is not the current state. **Three audit rounds on 2026-09-01/02 measured 32%, 32% and 26% of
their slices already dead** — not deferred, not blocked; done or void, sometimes for weeks, and still
on the board. Three independent measurements make ~30% the container's steady state, not an artifact
of sampling.

The cause is not neglect. It is that **only about a third of entries can be checked by a machine at
all** (measured after round 3: 161 prose, 39 with no predicate), and a share of the rest carry a
predicate that *could never have fired*. Retiring dead entries does not move that ratio, which is
exactly why ~30% is always dead. **The ratio is the disease; the dead entries are the symptom.**
Everything below is about authoring an entry that lands on the right side of it.

> **Some of this is now enforced.** As of 2026-09-02 `mev` warns on several of the failures below —
> `W_STATE_CARRYOVER_BROKEN_PREDICATE_UNREADABLE` / `_PATTERN`, `W_STATE_CARRYOVER_ALREADY_SATISFIED`,
> `W_STATE_FINDING_ID_ORPHAN`, `W_CARRYOVER_MISFILED`, `W_STATE_CARRYOVER_UNKNOWN_NEEDS`. Treat those
> as a safety net, not as the standard: three of the eight predicate failures below are still invisible
> to every gate, and the misfiling warning is blind unless you set `needs`.

---

## Step 1 — Most findings do not belong in `carryover[]`

Ask these in order and stop at the first yes. Getting this wrong is the single most common error in
the container, and it is not cosmetic: **a carryover entry gates nothing.** It sorts onto no board's
critical path and blocks no work, so an item misfiled here is never forced — it just ages.

| Ask | If yes | Why not carryover |
|---|---|---|
| **Can only a human do this?** A decision, a credential, an approval, a physical act. | `{"type":"operator", slug, exit, start}` edge in `depends_on` on the block it gates | An operator edge inherits the effective priority of everything it gates and *blocks that work*. Drive it with `/begin-session <slug>`. |
| **Is it a unit of work an agent could be told to do?** | a block — use **`mev create-block`**, which refuses bad input rather than guessing | Carryover is a finding, not a task. A block is schedulable, startable, and closeable. Until 2026-09-02 nothing could create one and every record in the fleet was hand-written; there is no longer an excuse to file work as a finding. |
| **Will it be true forever?** No `clears_when` is possible because nothing will ever make it stop being true. | `reference[]`, with a `class` of `trap` · `invariant` · `lesson` · `deliberate` | Per D72 a reference carries no `clears_when`, no `priority` and no `blocks[]` — nothing can gate on a permanent fact resolving. |
| **Is it an idea, not yet committed work?** | `backlog[]` + a `/capture` note | |
| Otherwise | `carryover[]`, `kind` ∈ `defect` · `deferred` · `drift` · `env` | |

**Measured:** 30 of 202 entries were operator work misfiled this way, against 46 correct operator
edges — and the 2026-09-01 audit found 7 more, including several where the only blocker was the
operator's own sign-off on work already shipped and live in both locales.

**A second misfiling worth naming, because it destroyed information.** When a roadmap names a block
id that no `state.json` carries, the fix is to **register the block**, not to file a carryover about
it. On 2026-09-01, fourteen such ids (`BU.14.A`–`F`, `BT.3.A/B/F/H`, `HQ.6.A/B`, `EN.13.A`,
`BA.22.C`) existed only as carryover entries, because a lane-file format conversion had dropped
their HOLDS annotations and carryover was the last place the text survived. They read as startable
on every board while being impossible for `/generate-tasks` to touch. If you are about to write
"block X is named by a lane file but registered nowhere" — register it instead.

`constraint` and `known_issue` are **retired** kinds (D72). They still deserialize through
`CarryoverKind::Unknown(String)` so legacy entries round-trip, and they warn as
`W_STATE_LEGACY_KIND`. Never mint a new one.

---

## Step 1b — Set `needs`: what kind of work closes it

`kind` says *why the entry exists* (`defect`/`deferred`/`drift`/`env`). **`needs` says what closes
it** — `code` · `docs` · `state` · `operator` · `dedupe`. It is optional in the schema and you should
set it anyway, for two reasons.

**It routes.** The 2026-09-01 triage had to hand-sort 55 findings into those exact five buckets before
it could parallelize them — three doc agents running concurrently while one agent held `state.json`.
Without the field that sort is redone by hand every round, and "how much of this backlog is actually
engineering?" needs a ten-agent audit to answer.

**And `needs: operator` is a self-report of a misfiling.** `W_CARRYOVER_MISFILED` fires on exactly
that value: if only a human can do it, it belongs on the block it gates as an `operator` edge, where
it blocks something. **That warning is blind until the field is populated** — measured 2026-09-02,
8 of 275 entries carried a `needs` value, so a lint built for the fleet's most common misfiling was
firing zero times. Setting it is what turns the check on.

So: if you are about to write `needs: operator`, stop and re-read Step 1's first row. You have just
told yourself this is not a carryover entry.

---

## Step 2 — Write a predicate that will still fire in six weeks

`clears_when` is the whole ballgame. Prose is legal and it is what 221 entries carry, but a prose
predicate lands in the not-evaluable lane forever and the entry can only be retired by a human
reading it. **A typed predicate is the difference between an entry that dies on its own and one that
someone finds two months later, already fixed.**

The four typed forms. Note the shape: a **flat object with a `type` key**, never nested under its
own name — that is the most common authoring error and it fails the whole file with
`data did not match any variant of untagged enum ClearsWhen`.

```jsonc
{"type": "block_closed",       "repo": "bastion-web", "id": "BW.8.N"}
{"type": "file_exists",        "path": "planning/decision-rate-card.md"}
{"type": "file_contains",      "path": "docs/x.md", "pattern": "Folded into", "note": "…"}
{"type": "command_exits_zero", "command": "bastion validate-brain --links", "note": "…"}

{"command_exits_zero": {"command": "…"}}   // WRONG — nested under its own name, whole file fails
```

`note` is optional on all four. A `command_exits_zero` runs with **cwd = the entry's `scope.repo`
root**, and is never executed unless the sweep is invoked with `--allow-exec`.

### The eight ways a predicate silently never fires

Each was found live during the 2026-09-01/02 audits. All eight produce an entry that outlives its own
fix. **`mev` now warns on 1, 2, 4 and 5. It cannot see 3, 6, 7 or 8** — those are yours to avoid.

**1. It names a path that later moves.** This was the *dominant* failure of the whole audit — most
of the 65 round-2 RESOLVEDs were fixed weeks earlier and could not retire themselves because the
file had moved out from under the predicate: `scripts/` → `scripts/sync/`,
`planning/operator-surface/` → `planning/roadmaps/operator-surface/`, `serve-api.md` →
`docs/serve/serve-api.md`, `.claude/skills/record-a-bail/` → `.agents/skills/record-a-bail/`.

> **Prefer `block_closed` over a path whenever a block owns the work.** A block id is stable; a path
> is not. If a real block will close when this finding clears, name the block.

**2. It is already satisfied the moment you write it.** The entry retires on its first sweep while
the finding is still live — strictly worse than no predicate. Measured: of 5 entries a sweep
reported CLEARED, **3 were false**, and one live entry's predicate matches at line 33 of a document
whose *fix* is at line 208. **Check it, do not assume:**

```bash
mev carryover --repo <slug>          # your new entry must NOT be in the CLEARED lane
mev carryover --repo <slug> --allow-exec   # only this executes command_exits_zero
```

**3. It is a literal that is always true.** One live entry's `clears_when` is `true #…`, which
reports clear on any exec-enabled sweep.

**4. `file_contains` with a regex-shaped pattern.** The evaluator does **literal substring matching
only** and adds no regex dependency. A pattern like `ChatAbout .*live` can therefore never match.
mev reports this distinctly as `pattern-not-literal` — but only in the sweep, where nobody looks.

**5. `file_contains` on a file that cannot be read.** Missing, oversized, non-UTF-8, or ambiguous
across the two roots. Reported as `file-unreadable`, never as a genuine negative.

**6. `file_exists` on an artifact that gets created under a different name.** The predicate is
mechanically fine and semantically dead. Live case: an entry waited on
`BT.ticket.extraction-gate-constraints.json`; the work shipped as
`BT.ticket.extraction-port-gate-cannot-lie.json`, so the entry was permanently open and permanently
done at the same time.

> **`file_exists` on a path that does not exist yet is CORRECT** when the missing file *is* the open
> work — 15 live entries are legitimately waiting for an artifact to appear. The failure is only
> when the artifact arrives under another name. Say in `note` what the artifact is, so a reader can
> tell the two apart.

**7. `block_closed` on a block that gets CUT.** The skill recommends `block_closed` over a path, and
that advice is right — but a block can stop being work without ever becoming `closed`.
`unmet_carryover_block_keys`' contract treats a `wontfix` target as **unmet by design**, so an entry
anchored to a cut block is permanently open and permanently undoable at once. Live case:
`bastion-web:docs-graph-and-related-chips-blocked-upstream` on `BW.8.N`, which is `wontfix`. **When you
author a `block_closed`, and whenever you audit one, check the target's status is `closed` — not
`wontfix`, not absent.**

**8. The predicate becomes satisfied AFTER authoring, by unrelated work, while the finding stays
live.** This is the nastiest, because the predicate is typed, well-formed, and genuinely passing —
none of the other seven classes flags it, and no gate catches it. Trap 2 is checkable at authoring
time; this one arrives later by a route authoring-time checking cannot see. Found in three repos
independently: a bella render bug whose scene check went green from a *different* fix while the
original keystroke finding stayed unexplained (its own text warned "do not assume the tape fix closed
the keystroke one" — and the predicate then assumed exactly that); a mev prototype-retirement entry
whose block closed while the prototype it exists to retire is still on disk; and a bastiel entry whose
`file_contains "npm install"` matched a *comment* explaining why `npm ci` is used instead — the right
verdict reached by luck.

> **The defence is in the text, not the predicate.** Write the predicate so it names the observable
> the finding is *about*, and write the text so a reader re-checking a CLEARED entry can tell whether
> the finding actually went away. This is why the CLEARED lane is a candidate list and never a delete
> list.

### Anchor the predicate to the thing that actually changes

The point of failure in cases 1 and 6 is that the predicate names an *incidental* fact (where a file
lives today, what a block record is called) rather than the *observable* the finding is about. Prefer,
in order: the block that owns the fix → a command whose exit code encodes the condition → a path.

---

## Step 3 — Write text that survives you being wrong

**Assume the next reader is an agent six weeks from now who will believe you.** Measured on
2026-09-01, entry text was wrong often enough that every audit prompt had to carry a warning about
it: entries blamed the wrong file, understated their own defect (a stub was described as listing
"5 of 9" registered workflow types when the real number was 17), asserted a premise that had since
inverted, and — worst — **explained away a real failure with a false cause** (a flaky test was
recorded as "only fails under full-suite load"; it reproduced on an idle machine in isolation).

So:

- **Lead with the observable, then the diagnosis.** "`X` returns 5 where the registry has 17
  (`workflows.rs:1255`)" survives. "The stub registry is out of date" does not.
- **Cite `file:line` and say when you measured it.** Line numbers move; a date lets the next reader
  know how much to trust it.
- **State a cause only if you tested it.** If you did not reproduce it, write what you observed and
  say the cause is unverified. A confident wrong cause stops the next person from looking.
- **Say what you checked and what you did not.** An entry that names its own blind spot is worth
  more than one that sounds finished.
- **Do not describe the fix in the text and nowhere else.** Prose gates nothing. If there is work to
  do, it needs a block or an edge — the entry records the finding.

---

## Step 4 — `finding_id`: one finding, one fix

If this same finding is true in more than one repo, give **every** copy the same `finding_id`.
`mev carryover` groups them into one cluster so it is fixed once.

**It is effectively unused.** Measured 2026-09-01: 71 of 409 entries carry a `finding_id`, forming
68 clusters, of which **exactly one spans more than one repo**. Meanwhile the same audit found three
duplicate pairs by hand — two mev-lease entries in jynx, two quick-launch flake entries in
bastion-web, and six `BU.14` drift entries duplicating four briefings — **none of which carried a
`finding_id` at all.**

- Set it when you file the *second* copy, and go back and set it on the first.
- Use a readable slug (`ticket-specs-missing-tasks-json`), not a hash. The 33 hash-shaped ids in the
  corpus are machine-emitted by `mev graph-findings`; do not imitate them by hand.
- A `finding_id` on a single entry groups nothing. That is fine when you expect a sibling — it is
  noise when you do not.
- Do **not** reach for `mev carryover`'s SUGGESTED DUPLICATES to find these. It is a token-overlap
  heuristic over entries with no `finding_id` and currently emits **544 pairs**.

---

## Step 5 — `scope` has exactly one non-null key

The most frequently repeated `state.json` error in the fleet, with one specific cause: every entry
on disk carries all three keys with two nulled, so copying a neighbour and editing it leaves the old
key set.

```jsonc
"scope": { "repo": "bastion", "tier": null, "cross_repo": null }   // exactly one non-null
"scope": { "repo": "bastion", "tier": null, "cross_repo": true }   // two set — fails
"scope": { "repo": null, "tier": null, "cross_repo": false }       // false COUNTS AS SET
```

`repo` and `tier` are strings; **`cross_repo` is a boolean.** A string `"true"` is a different and
much worse failure — serde rejects the entire file, so every state check dies on a parse error.

**Ask who fixes it, not what it touches.** An item is `cross_repo` only when no single repo can
close it. A bastion change *triggered* by another repo's release is still `repo: bastion`.

---

## Before you commit the entry

- [ ] Ran Step 1's routing questions — it is not operator work, a block, a permanent fact, or an idea
- [ ] `kind` is one of `defect` · `deferred` · `drift` · `env` — no retired value minted
- [ ] **`needs` is set** (`code`/`docs`/`state`/`operator`/`dedupe`) — and if it is `operator`, you
      went back to Step 1 instead of filing it here
- [ ] `clears_when` is **typed**, not prose, unless you genuinely cannot express the observable
- [ ] The predicate anchors to a block or a command where possible, not to a path that can move
- [ ] It is **not already satisfied** — confirmed with `mev carryover --repo <slug>`, not assumed
- [ ] A `file_contains` pattern is a literal substring, not a regex, and its file exists
- [ ] A `file_exists` on a missing path is deliberate, and `note` says what the artifact is
- [ ] A `block_closed` target's status is `closed` — **not `wontfix`, not absent** (trap 7)
- [ ] The text would let a future reader re-checking a CLEARED entry tell whether the finding
      actually went away, not just whether the predicate passes (trap 8)
- [ ] Text leads with the observable, cites `file:line`, dates the measurement, and flags any
      untested cause
- [ ] `finding_id` set if this finding is true in another repo — and set on the sibling too
- [ ] `scope` has exactly one non-null key; `cross_repo` is a boolean
- [ ] Round-tripped byte-for-byte: `json.dump(data, f, indent=2, ensure_ascii=False)` + trailing
      newline, and `git diff --stat` shows the size of your edit
- [ ] `mev validate-state <path>` clean, then `bastion validate-brain --state` clean — and read the
      new `W_STATE_CARRYOVER_*` / `W_CARRYOVER_*` warnings on your own entry before moving on
- [ ] If the change touched docs or `related:` edges, `--graph` **and** `--links` were both run.
      They check different things; a dangling `related:` has shipped behind a green `--links`

## Related

`edit-state-json` carries the containers, the four `depends_on` edge shapes and the round-trip rule.
`derive-state-safely` covers what happens when a write verb re-derives the corpus.
