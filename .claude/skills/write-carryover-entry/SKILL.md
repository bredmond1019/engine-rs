---
name: write-carryover-entry
description: How to write a carryover[] entry that can actually die — whether the finding belongs in carryover[] at all rather than an operator edge, a block, or reference[], how to author a clears_when predicate that will still fire in six weeks, how to write text that survives its author being wrong, and the finding_id discipline that makes one finding one fix. Use BEFORE adding any carryover[] entry, at every /handoff, /wrap-up, /log-work and lane close, before filing a finding an orchestration run turned up, and when a sweep reports an entry CLEARED that is plainly still live.
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

That is not the current state. **Measured across the fleet on 2026-09-01, over two audit rounds
covering 489 of 497 entries: 159 — 32% — were already dead.** Not deferred, not blocked. Done,
or void, sometimes for weeks, and still on the board. Both rounds hit ~32% independently, so that
is the steady-state rot rate, not an artifact of looking at the oldest ones first.

The cause is not neglect. It is that **only 30% of entries can be checked by a machine at all**
(136 of 409: 221 prose, 85 with no predicate), and a meaningful share of the rest carry a predicate
that *could never have fired*. Everything below is the difference between those two outcomes.

---

## Step 1 — Most findings do not belong in `carryover[]`

Ask these in order and stop at the first yes. Getting this wrong is the single most common error in
the container, and it is not cosmetic: **a carryover entry gates nothing.** It sorts onto no board's
critical path and blocks no work, so an item misfiled here is never forced — it just ages.

| Ask | If yes | Why not carryover |
|---|---|---|
| **Can only a human do this?** A decision, a credential, an approval, a physical act. | `{"type":"operator", slug, exit, start}` edge in `depends_on` on the block it gates | An operator edge inherits the effective priority of everything it gates and *blocks that work*. Drive it with `/begin-session <slug>`. |
| **Is it a unit of work an agent could be told to do?** | a block: `tracks[].blocks[]` + `planning/blocks/<ID>.json` | Carryover is a finding, not a task. A block is schedulable, startable, and closeable. |
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

### The six ways a predicate silently never fires

Each of these was found live on 2026-09-01. All six produce an entry that outlives its own fix.

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
- [ ] `clears_when` is **typed**, not prose, unless you genuinely cannot express the observable
- [ ] The predicate anchors to a block or a command where possible, not to a path that can move
- [ ] It is **not already satisfied** — confirmed with `mev carryover --repo <slug>`, not assumed
- [ ] A `file_contains` pattern is a literal substring, not a regex, and its file exists
- [ ] A `file_exists` on a missing path is deliberate, and `note` says what the artifact is
- [ ] Text leads with the observable, cites `file:line`, dates the measurement, and flags any
      untested cause
- [ ] `finding_id` set if this finding is true in another repo — and set on the sibling too
- [ ] `scope` has exactly one non-null key; `cross_repo` is a boolean
- [ ] Round-tripped byte-for-byte: `json.dump(data, f, indent=2, ensure_ascii=False)` + trailing
      newline, and `git diff --stat` shows the size of your edit
- [ ] `mev validate-state <path>` clean, then `bastion validate-brain --state` clean

## Related

`edit-state-json` carries the containers, the four `depends_on` edge shapes and the round-trip rule.
`derive-state-safely` covers what happens when a write verb re-derives the corpus.
