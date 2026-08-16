---
type: Reference
title: Block Registration — the canonical procedure every planning producer follows
description: The single shared procedure for writing a block record, registering it in state.json, and refreshing derived state. Included by reference from /plan, /ticket, and /chore.
doc_id: block-registration
layer: [factory]
project: base-template
status: active
keywords: [block.json, state.json, registration, operator, carryover, producers]
related: [D65-block-record-is-the-planning-unit, state-schema, block-record-standardization]
---

# Block Registration

**Read and follow this file whenever a planning producer creates or revises a block.**
`/plan`, `/ticket`, and `/chore` all point here instead of carrying their own copy. Four
near-duplicate copies existed before D65 and had already drifted — only one of them created a
missing `state.json`, and the other three silently no-opped, which is how at least one repo's
block graph went dark for weeks.

Governed by [D65](../../planning/decisions/D65-block-record-is-the-planning-unit.md). The block
record's field contract is `block.schema.json`, next to this file.

---

## Step 1 — Resolve the block ID

Find this repo's `prefix` in `brain.toml` at the brain root (e.g. `MV`). Then:

| Producer | ID form | Example |
|---|---|---|
| `/plan` (roadmap block) | `<PFX>.<phase>.<block>` | `MV.3.A` |
| `/ticket` | `<PFX>.ticket.<slug>` | `MV.ticket.fix-null-deref` |
| `/chore` | `<PFX>.chore.<slug>` | `MV.chore.bump-deps` |

**Phase numbers are allocated per-repo, read from `state.json`** — never from `master-plan.md`,
`status.md`, or any other narrative file. Narrative files lag, and that lag is how one repo came
to carry two unrelated "Phase 4"s. Take the highest existing phase for this repo and go up.

**The spec directory equals the block ID exactly** — `planning/MV.3.A/`,
`planning/MV.ticket.fix-null-deref/`. No title suffix, ever. `/plan` does not create it;
`/generate-tasks` does, later. `/ticket` and `/chore` create it at author time because they
write `tasks.json` immediately.

---

## Step 2 — Ask the two questions that never surface on their own

Both of these are invisible unless asked explicitly. Skipping them is the single most common way
a block ends up blocked for days with nothing saying why.

**A. Does this block need the operator?** A decision, a credential, a judgement call, a
review only they can give. If yes, add to `depends_on`:

```json
{"type": "operator", "slug": "operator-<kebab>", "exit": "<the artifact whose existence ends the gate>", "start": "<paste-ready command>"}
```

`exit` names an **artifact**, never a description of the work — "the signed cert is at
`certs/prod.pem`", not "operator decides on certs". An operator edge inherits the effective
priority of everything it gates and surfaces in `/next` as the reason work cannot start. The same
item written as prose in a handoff, a `note`, or an `## Open questions` bullet surfaces nowhere.

For a single reducible yes/no on a fixed payload, use
`{"type": "approval", "slug", "what", "digest": "sha256:<hex>"}` instead.

**B. Does this block depend on work landing in another repo first?** A same-repo "Depends on"
line never reveals a cross-repo edge. If yes, resolve that repo's `slug` from `brain.toml` and
add `{"type": "block", "repo": "<other-slug>", "id": "<their-ID>"}`. If the dependency is
non-block — hardware, a paid-API budget, a manual step — use
`{"type": "external", "what": "<gloss>"}`.

Skip these questions only when the plan explicitly states the work is self-contained.

---

## Step 3 — Read the carryovers before you author

Load `carryover[]` from `planning/state.json` and filter to entries scoped to this repo. For any
entry whose files or subject overlap this block:

- If it is a **prerequisite** — the block cannot be correct until it is resolved — add it as a
  dependency edge, or link the carryover's `blocks[]` to this block.
- If it is a **constraint** — a known defect or drift the block must not re-break — record its
  slug in the block record's `carryover_context[]`.

A block authored blind to an open carryover on the same files is how the same defect gets
reintroduced by the next piece of work.

If this block is being filed **to resolve** a carryover, set
`"origin": {"type": "carryover", "slug": "<carryover-slug>"}` on the block record so the loop
visibly closes.

---

## Step 4 — Write the block record

Write `planning/blocks/<BlockID>.json`. Create `planning/blocks/` if absent; it needs no
`index.md` (D65 exempts it from standing rule 7 — a machine-written directory of JSON would churn
an index constantly).

Validate against `block.schema.json`. Required fields, and the reason each is required:

| Field | Why it is not optional |
|---|---|
| `description` | The line a surface renders when it has room for more than a title. Zero of 894 blocks carried one before D65. |
| `what` | Scope in implementation terms — what `/generate-tasks` decomposes. |
| `why` | The field whose absence forced intent to be re-derived from diffs, sometimes wrongly. |
| `files` | How `/generate-tasks` derives disjoint task ownership without guessing. |
| `out_of_scope` | At least one boundary. A block with none is under-specified, and the generator will over-scope it. |
| `acceptance_criteria` | Each true/false against a diff, ending with the project's gating checks passing. |

**Plan-quality floor — clarify, don't fabricate.** If filling any load-bearing field would
require inventing a fact you cannot ground in the user's input, `CLAUDE.md`,
`planning/context.md`, the repo, or an existing plan — **stop and ask a targeted question**.
An honest "I need X to state the why for this block" beats a confident invention that multiplies
through every downstream task. In a non-interactive context (invoked by an engine to
auto-generate a missing record), **abort naming exactly what is missing** rather than guessing.

**Un-gateable acceptance criteria must be declared (D64).** For each criterion, ask only *where
its evidence lives* — never how important it feels. Evidence in this repo, this language,
observable in-process: gated, say nothing. Evidence in another process, another repo, a generated
artifact, or an installed artefact (the distributed binary rather than the source tree): mark it
`{"criterion": "...", "gateable": false, "evidence": "<the fixture that stands in>"}` and give it
a dedicated fixture-evidence task. A green suite is evidence of gate agreement, not correctness.

Write the file with `json.dump(..., indent=2, ensure_ascii=False)` plus a trailing newline.

---

## Step 5 — Register in `state.json`

**If `planning/state.json` does not exist, create it first.** Nothing else does — the scaffold
ships none and there is no `mev init`. Skipping this makes every later `mev emit-state --write`
silently derive from nothing: the block graph never surfaces and the repo's cross-repo visibility
stays dark until a human notices. This has happened more than once. Script it; never free-hand
the JSON:

```bash
python3 - <<'PYEOF'
import json
skeleton = {
    "repo": "<repo-slug>",
    "kind": "project",
    "updated": "<YYYY-MM-DD>",
    "focus": {"now": [], "next": [], "blocked": []},
    "tracks": [],
    "carryover": []
}
with open("planning/state.json", "w") as f:
    json.dump(skeleton, f, indent=2, ensure_ascii=False)
    f.write("\n")
PYEOF
```

Substitute the real slug and today's date. Never overwrite a populated `state.json`.

Then find or create the `tracks[]` entry — the phase name for a roadmap block, `"Tickets"` for a
ticket, `"Chores"` for a chore — and add an entry to its `blocks[]` if one does not already exist
(match by `id`):

- `id`, `title` — as resolved in Step 1.
- `description` — the same one-liner as the block record. This is what the surfaces read.
- `status` — `"open"`. Never hand-set `"blocked"`; blocked-ness is derived from unmet
  `depends_on`.
- `sdlc_workflow`, `model` — the block's choices.
- `wave` — for a roadmap block, `10 * <phase>`. For a ticket or chore, the next multiple of ten
  past this repo's highest existing wave, so one-offs queue behind roadmap work and stay on the
  same lattice. Ask before assigning an earlier wave. **Do not use `max + 1`** — that lands
  inside the lattice and silently interleaves chores with roadmap phases.
- `depends_on` — the edges from Step 2. Omit or `[]` when there are none. Never encode the
  implicit phase-sequential default as an edge; `wave` already expresses it.
- `created` — today, `YYYY-MM-DD`.
- `origin` — only when promoted from a backlog item, a carryover, or a capture note.

**Do not hand-author `tasks`** — it is a derived pointer and status summary
(`{file, generated, counts}`), regenerated by `mev emit-state --write` from the block's
`tasks.json`. Do not overwrite an existing block's `status` or `tasks`; only add missing blocks,
or update `title` / `description` / `wave` / `depends_on` on ones still `open`.

`depends_on` is authored in both the block record and `state.json` until `mev` learns to derive
the scheduling fields from `blocks/*.json`. Mirror them exactly; they must not disagree.

---

## Step 6 — Verify and refresh

```bash
python3 -c "import json;json.load(open('planning/state.json'))"
```

This is **parse-only**. It cannot catch a shape mismatch — a struct-typed field like `origin`
written as a scalar parses fine as JSON and only fails `mev`'s typed deserialization. For real
confidence:

```bash
mev validate-brain --state
```

Then run `mev emit-state --write` to refresh the derived focus, wave tables, and project caches.

---

## Traps

- **`planning/state.json` round-trips with `ensure_ascii=False`** plus a trailing newline. The
  default escapes every em dash and turns a three-field edit into ~130 lines of churn, plus noisy
  conflicts with concurrent agents.
- **Concurrent agents contend on `state.json`.** If several are running, report the state change
  you want and let one writer apply them centrally.
- **Every `planning/` is a symlink into the brain's `_planning/` vault.** `git mv` fails through
  it ("source directory is empty") — move via the real vault path. `rg`/`find` are symlink-blind;
  pass `-L`.
- **Commit with an explicit pathspec** (`git commit -o <paths>`). One git index backs every
  repo's `planning/`, so a bare commit sweeps other sessions' staged work into yours.
- **`timeout` does not exist on this macOS shell.**
- **A piped command's `$?` is the pipe's, not the command's.** Redirect to a file, then check.
