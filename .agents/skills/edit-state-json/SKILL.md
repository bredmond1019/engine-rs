---
name: edit-state-json
description: >
  How to write correct blocks[], carryover[], reference[] and depends_on edges in a repo's
  planning/state.json — which container an item belongs in, the four edge shapes, the
  authored-vs-derived status trap, the scope object's exactly-one-of rule, the two very
  different failures that share the code E_STATE_SCHEMA_MALFORMED_SCOPE, and the byte-for-byte
  round-trip that avoids 130 lines of conflict churn. Use BEFORE hand-editing any state.json,
  before adding or closing a block, and when validation reports E_STATE_MALFORMED_JSON,
  E_STATE_SCHEMA_MALFORMED_SCOPE, E_STATE_SCHEMA_BAD_BLOCKED_BY, E_BLOCK_BAD_KEY,
  E_BLOCK_BAD_STATUS, or W_STATE_LEGACY_KIND.
---

# Editing `state.json`

> **Paths below are relative to the brain root** — the directory containing `brain.toml`, found by
> walking up from wherever you are. This skill is synced into every repo, so a repo-relative link
> would be wrong in most of them.

**If it is not in `state.json`, it does not exist.** Prose gates nothing, sorts nowhere, and shows up
on no board. An item living only in a plan, a review, a handoff, or an `## Open questions` bullet is
**lost, not deferred**. Where a document and the graph disagree, the graph wins.

**Prefer the tools over a hand edit.** `/update-state` performs the edit per the canonical schema, and
`mev set-block-status <repo:id> <status>` moves exactly one block and nothing else. Hand-edit only
when no verb covers the field you need (`description`, `note`, a new `depends_on` edge) — and then
follow the round-trip rule in Step 4.

Field tables: `docs/state/state-schema.md` and `docs/state/reference-container-schema.md`.
Routing table: `base-template/.claude/workflows/block-registration.md`.

---

## Step 1 — Route the item before you write it

Ask in this order. Getting this wrong is not cosmetic: **30 of 202 `carryover[]` entries were measured
as operator work misfiled**, against 46 correct `operator` edges.

1. **Can only a human do this?** → an `{"type":"operator", slug, exit, start, what?}` edge in
   `depends_on` **on the block it gates** — *not* a `carryover[]` entry. An operator edge inherits the
   effective priority of everything it gates and blocks that work until done. A carryover entry gates
   nothing, so the item is never forced.
2. **Is it permanently true?** → `reference[]`. The signal is a finding with **no `clears_when`**,
   because nothing will ever make it stop being true.
3. **Otherwise** → `carryover[]`, with a kind from the four below.

Ideas that are not yet committed work go to `backlog[]`.

---

## Step 2 — Block records and `depends_on` edges

A block lives in `tracks[].blocks[]`. Field order convention:

```
id, title, status, depends_on, wave, origin, note, description,
priority, due, sdlc_workflow, model, epics
```

### `status` — authored values only

`open` · `in_progress` · `deferred` · `closed` · `wontfix` (`VALID_TRACK_BLOCK_STATUSES`).

> **`blocked` is NOT authorable.** It is a *derived* lane `emit-state` computes from unmet
> dependencies and stamps onto `focus.blocked[]`. Writing it onto a `tracks[]` block is
> `E_BLOCK_BAD_STATUS`, and mev says so explicitly. If you want a block to read as blocked, give it
> the dependency that actually blocks it — the lane follows.

### Block keys are `repo:id`, always

`mev set-block-status` takes `repo:id` (e.g. `mev:MV.10.A`). An unqualified id is `E_BLOCK_BAD_KEY`:
**block ids are only unique within a repo, so an unqualified id is ambiguous and is not guessed.**
Program blocks use letters (A–S) that do *not* match sub-repos' local block numbers.

### The four `depends_on` edge shapes

Tagged by `"type"` (`BlockedBy` in `core/okf-core/src/state.rs`). A bare string here is
`E_STATE_SCHEMA_BAD_BLOCKED_BY`.

```jsonc
// block — a dependency on another block, possibly cross-repo
{ "type": "block", "repo": "bastion", "id": "BA.15.8", "what": "optional gloss" }

// operator — a human working session that gates this block
{ "type": "operator",
  "slug":  "operator-readme-voice-pass",        // shared across every block this gate covers
  "exit":  "planning/<block>/voice-pass.md exists, listing all 9 repos with a dated approval line",
  "start": "/begin-session operator-readme-voice-pass",
  "what":  "optional: why THIS block is gated on it" }

// approval — one decision, approve or reject
{ "type": "approval", "slug": "...", "what": "one line stating the DECISION, not the context",
  "digest": "<digest of the exact payload reviewed>" }

// external — environmental, not a tracked block
{ "type": "external", "what": "human description" }
```

Two things that make an `operator` edge work rather than rot:

- **`exit` is an artifact you can point at afterwards, not a description of the work.** "Decide the
  license" is not an exit; "`docs/decisions/D75-*.md` exists" is.
- **`slug` is a join key.** Share it across every block the same session gates, and tooling can find
  and clear them together. `approval` is digest-bound: if the payload changes, the approval is void
  and re-queues.

`operator` and `approval` are **targetless but identified** — no node, skipped by dangling/cycle/topo
logic. That is why they gate without creating a phantom block.

### `clears_when` predicates (carryover only)

Untagged: either free prose, or one of the typed predicates — `block_closed`, `file_exists`,
`file_contains`, `command_exits_zero`. Unknown `type` values are **rejected by serde** (there is no
`#[serde(other)]`), so a typo surfaces as a whole-file parse error, not a warning.

---

## Step 3 — The vocabularies are closed

From `core/mev/src/brain/state.rs`:

| Container | Field | Allowed values |
|---|---|---|
| `carryover[]` | `kind` | `defect` · `deferred` · `drift` · `env` (`VALID_CARRYOVER_KINDS`) |
| `reference[]` | `class` | `trap` · `invariant` · `lesson` · `deliberate` (`VALID_REFERENCE_CLASSES`) |

**`constraint` and `known_issue` are retired (D72).** They still *deserialize* — `okf-core`'s
`CarryoverKind::Unknown(String)` fallback round-trips any string — and they **warn rather than error**
(`W_STATE_LEGACY_KIND`) until the migration ticket re-kinds the live corpus. So a legacy value in a
file you are editing is expected; **minting a new one is not.** Never author one.

Per D72, `reference[]` entries carry no `clears_when` (no honest done-state exists), no `priority` (it
is consulted, not scheduled), and no `blocks[]` (nothing can gate on a permanent fact resolving). That
is a design rule stated in the decision — it is **not** mechanically gated, so nothing will stop you.

---

## Step 4 — The two failures that share one error code

`E_STATE_SCHEMA_MALFORMED_SCOPE` has **two trigger sites with very different blast radii**. Knowing
which one you are looking at tells you how much is broken.

### 4a. `scope` is not an object at all — `core/mev/src/lib.rs`

```
E_STATE_SCHEMA_MALFORMED_SCOPE — carryover item 'X' has 'scope' written as string,
not an object with repo/tier/cross_repo fields
```

### 4b. The exactly-one-of rule — `core/mev/src/brain/state.rs`

```rust
let scope_fields_set = item.scope.repo.is_some() as u8
    + item.scope.tier.is_some() as u8
    + item.scope.cross_repo.is_some() as u8;
if scope_fields_set != 1 { /* E_STATE_SCHEMA_MALFORMED_SCOPE */ }
```

**Exactly one of `repo` / `tier` / `cross_repo` must be set.** Zero fails. Two fails. `null` counts as
*not set*, which is why the canonical shape is one value and two explicit nulls:

```json
"scope": { "repo": "bastion", "tier": null, "cross_repo": null }
```

### The distinction that matters

`CarryoverScope` in `core/okf-core/src/state.rs` is `repo: Option<String>`, `tier: Option<String>`,
**`cross_repo: Option<bool>`** — so serde enforces the types.

| You wrote | What happens | How bad |
|---|---|---|
| **Wrong type** (`"cross_repo": "true"`) | `E_STATE_MALFORMED_JSON — invalid type: string "true", expected a boolean at line N column M`. The **whole file fails to deserialize**, so every state check dies on a parse error, not just that entry. | Loud, wide — but the message names the type *and* the line/column, so it is a fast fix. |
| **Two keys set** (`repo` *and* `cross_repo`) | The file parses fine; only the one semantic check fires. | Quiet, narrow — and **this is the one people write by accident**, so it is the recurring one. |

**The loud, scary-looking failure is the easy fix. The quiet one is the one that keeps coming back.**

A wrong type is worse than it first appears for a second reason: `mev` **refuses to write derived
views** when any discovered `state.json` fails to load —

> "Derived views (focus, `repos[]`/`cross_repo[]`, project caches, tier rollups, HQ/unified/epic
> boards, master-plan and epic sequence tables) are cross-repo unions — regenerating them from a
> partial corpus would silently erase the missing repo(s) from every surface."

So one malformed file in one repo stops `emit-state` regenerating **every** board in the fleet.

Sibling code, same family: `E_STATE_SCHEMA_BAD_BLOCKED_BY` — a `related[]` or `depends_on[]` entry
written as a bare string instead of a dependency object (`{"type": "block", "repo": ..., "id": ...}`).

---

## Step 5 — Round-trip byte-for-byte

`state.json` is written with **`ensure_ascii=False`**. Reserialize it any other way and every em dash
becomes an escape — **~130 lines of churn for a 3-field edit**, and noisy conflicts with concurrent
agents.

```python
with open(path, 'w', encoding='utf-8') as f:
    json.dump(data, f, indent=2, ensure_ascii=False)
    f.write("\n")          # the trailing newline is part of the format
```

**Verify the diff is the size of your edit.** `git diff --stat` should show roughly the lines you
meant to change. A 3-field edit that reports 130+ changed lines means the round-trip is wrong — fix
the serialization, do not commit it.

Preserve the existing field order when adding keys to a block. The convention is:
`id, title, status, depends_on, wave, origin, note, description, priority, due, sdlc_workflow, model, epics`.

---

## Step 6 — Two rules that are easy to violate and expensive to undo

**Never author a `clears_when` predicate that is already satisfied.** It retires the entry on its first
`mev carryover` sweep while the finding is still live — worse than no predicate at all. Measured case:
3 of 5 entries reported CLEARED were still live, because an unanchored `file_contains` matched its own
target file's prose and a proxy string landed while the finding got worse.

**Flip block status *before* deriving.** `emit-state` never infers completion from `status.md` — the
sync is one-way by design. Set `status` to `closed` in `tracks[].blocks[]` first; that authored field
is the *input* the derivation reads. Skipping it leaves `focus` and every generated surface stale
until a future session reconciles by hand.

---

## Step 7 — Validate, then derive

```bash
mev validate-state <path>          # one file, no brain.toml needed — works from anywhere
bastion validate-brain --state     # the whole corpus, cross-repo block graph included
```

`validate-state` is one of only two mev verbs that run without a `brain.toml` above the cwd, so it is
the fast inner-loop check while editing.

Then regenerate derived surfaces — but **rebuild the binary first**:

```bash
mev conformance --check toolchain-freshness   # "rebuild before any --write run" is a real warning
cargo install --path core/mev                 # if drifted
mev emit-state --write
```

A stale installed `mev` **reverts** generated boards to an older format. This has happened: an
`emit-state --write` regressed the Attention lanes in two `status.md` files and both had to be
restored with `git checkout`.

---

## Concurrency — the pattern that actually works

Every `planning/` dir, including every `core/<repo>/planning`, is tracked by the **one HQ git repo**.
Two consequences:

- **Concurrent agents contend on `state.json`.** The working pattern: each agent *reports* the state
  change it wants; **one writer applies them centrally**. Do not have several agents write the same
  file.
- **Always commit with an explicit pathspec** — `git commit -o <path1> <path2>`. Never `git add -A`,
  `git add .`, `git reset`, or `git stash` here: a bare commit sweeps another session's staged work
  into yours. This has happened multiple times.
- `git mv` fails through the `planning/` symlink face with "source directory is empty" — move against
  the real path (`core/_planning/<slug>/...`), not the symlinked one.

## Before you commit

- [ ] Routed correctly — operator edge, `reference[]`, `carryover[]`, or `backlog[]` (Step 1)
- [ ] Block `status` is an authored value — **never `blocked`**, which is derived
- [ ] Block keys are `repo:id`; every `depends_on` entry is a tagged object, not a bare string
- [ ] Any `operator` edge's `exit` names an **artifact**, not the work
- [ ] `kind` / `class` from the closed vocabulary; no newly-minted legacy value
- [ ] `scope` is an object with **exactly one** of `repo` / `tier` / `cross_repo` set
- [ ] `cross_repo` is a **boolean**, not the string `"true"`
- [ ] Any `clears_when` is **not** already satisfied
- [ ] `git diff --stat` shows the size of your edit, not a whole-file reserialization
- [ ] `mev validate-state <path>` clean, then `bastion validate-brain --state` clean
- [ ] Explicit pathspec on the commit
