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

## The rule this whole file serves — if it is not in `state.json`, it does not exist

Everything that has to get done lands in `state.json` in **some** shape. A markdown file is where
work is *described*; the graph is where it is *held*. Prose gates nothing, sorts nowhere, and
surfaces on no board — so an item that lives only in a plan, a review, a handoff, an
`## Open questions` bullet or a `note` field is not deferred, it is lost. This is measured, not
theoretical: six drift tickets were filed on disk where the drift detector could not see them, and
30 of the fleet's 202 `carryover[]` entries are operator work parked where it gates nothing.

There is a container for every shape of "to be done." Route to one of them, always:

| The thing | Where it goes | Filed by |
|---|---|---|
| Work an agent can do | a block in `tracks[].blocks[]` + `planning/blocks/<ID>.json` | `/plan`, `/ticket`, `/chore` |
| Work only a human can do | `{"type": "operator", slug, exit, start}` in the gated block's `depends_on` | any command; driven by `/begin-session` |
| A single yes/no on a fixed payload | `{"type": "approval", slug, what, digest}` in `depends_on` | same |
| A dependency on another repo's work | `{"type": "block", repo, id}` in `depends_on` | same — and **file the other half** |
| A non-block dependency (hardware, a paid API, a manual step) | `{"type": "external", what}` in `depends_on` | same. Never for work in a fleet repo |
| A finding that will clear but is not ticketed yet | `carryover[]`, kind `defect` · `deferred` · `drift` · `env` | `/handoff`, `/wrap-up`, `/log-work` |
| A fact that is permanently true | `reference[]` | same |
| An idea not yet shaped into work | `backlog[]` (HQ brain) + a `/capture` note | `/backlog-ticket`, `/capture` |
| A multi-repo initiative | `epics[]` with a `plan` field naming its document (HQ brain) | `/generate-roadmap` |

The document still gets written — the narrative, the reasoning, the cut list are what a document is
for. But **every actionable item in it also has a row**, and where the two disagree the graph wins.

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

**These edges are how a chain sequences around a human, and they are the only mechanism that
actually blocks.** An operator session is a first-class member of a plan: file it as an edge and
the work behind it is held until the artifact exists, it inherits that work's effective priority,
it surfaces in `/next` as the reason nothing is moving, and `/begin-session <slug>` drives it to
its exit. Nothing else in the graph does that — the same item as prose, a `note`, or a
`carryover[]` entry gates nothing at all.

**But autonomy is the target, and every edge you file is a chain that stops until you are at the
keyboard.** So the test is narrow: *can only a human do this?* A credential only the operator
holds, a decision that is theirs to own, an outward-facing or irreversible action, a physical
machine, a judgement no evidence available to an agent can settle. If an agent could answer it by
reading the repo, running the gate, or following an existing decision — it is not an operator
edge, and filing one anyway converts autonomous work into a queue at the operator's desk.

Two design moves keep the count honest:

- **Shrink the gate.** Prefer one narrow decision with a named artifact over "review this
  initiative." A gate the operator can close in fifteen minutes closes; a vague one does not.
- **Bind it late.** Cut blocks so the operator edge sits on the last block that needs it rather
  than the first block of the chain, and everything ahead of it keeps running unattended.

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

**Extraction/port-shaped blocks must declare four gate-capability constraints (D68).** A block
that moves, copies, or forks existing code or data across a repo/crate/module boundary is
"extraction/port-shaped." For such a block, the acceptance-criteria set must name all four of the
following — each keyed on *where the evidence lives* and *what the gate is actually shown capable
of failing on*, never on how important or risky the move feels:

1. **Moved-asset content diff.** A content diff of moved non-source assets (`include_str!`/
   `include_bytes!` targets, fixtures, manifests) — not an existence check. A file present at the
   new path with different bytes must be able to fail the gate.
2. **Per-file test-count diff.** A per-file test-count diff measured from both trees at gate time,
   not a single aggregate total — two independently-wrong sub-counts must not be able to cancel
   into a plausible-looking sum.
3. **Source-tree-measured-at-gate-time baseline.** The baseline for (2) is always machine-measured
   from the source repo at gate time, never read from the extraction block's own planning
   inventory — a baseline copied into planning prose can be wrong before the block starts, and a
   gate that trusts it will agree with it anyway.
4. **Gate-shown-capable-of-failing-on-the-deliverable.** The stated validation command must
   actually compile/run the shipped code path, not merely declare it covered — e.g. a
   feature-gated or conditionally-compiled deliverable must be enabled by the validation command
   itself, not merely present in the crate.

See D68 for the EN.9.A/EN.9.B provenance this rule generalizes from.

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

## Step 7 — The initiative-wide consistency pass

**Run once, after every block in the batch is authored, and before registration closes.
Not per block.** For a single-block producer (`/ticket`, `/chore`) only C1 and C5 apply, and
they are cheap.

Every defect this step exists to catch was invisible to the agent that authored its block
correctly. In the run that motivated it, five authoring agents each produced a defensible row and
the initiative still registered with a dropped cross-repo half, an undetectable concurrency
hazard, inherited dependency edges, a known-oversized block, and four invented operator artifacts.
An agent working one row at a time cannot see a second repo, a concurrent lane, an inherited edge,
a prior sizing flag, or an ungrounded fact. Only a pass that reads every block of the initiative
at once can.

Load every block record in this initiative — `planning/blocks/*.json` filtered by `initiative`,
plus the other repos' records when the cut spans repos — and run all five checks. Each finding is
a defect, not a preference: fix it, or state in the plan/roadmap why it is deliberate.

### C1 — Two repos means two block records

For every block, ask: does anything about it touch a repo other than its `repo` field? A Repo cell
naming two, a `files[]` path resolving into another repo's tree, an acceptance criterion whose
evidence lives in a sibling repo. If yes, that repo needs its own block record carrying its half
of the work. Nothing downstream catches this — the block closes green and the other half was
never filed.

**`{"type": "external"}` is not a placeholder for a fleet repo.** An agent that knows the other
half exists but has no ID for it is right to refuse to invent one — but `external` means a
*non-block* dependency: hardware, a paid-API budget, a manual step. If its `what` names a repo
listed in `brain.toml`, the edge is a `{"type": "block", "repo": ..., "id": ...}` and filing the
other half is the work that produces that ID. **Treat every `external` edge naming a fleet repo as
an unfiled block**, and file it before registration closes.

```bash
python3 - <<'PY'
import json, glob
for p in sorted(glob.glob("planning/blocks/*.json")):
    b = json.load(open(p))
    for d in b.get("depends_on", []):
        if d.get("type") == "external":
            print(f'{b["id"]:28} external: {d.get("what","")}')
PY
```

Check each printed `what` against `brain.toml`'s repo list by hand. A hit is an unfiled block.

### C2 — Concurrency is a files question, not a repo question

The lane model and `fleet_concurrency_check.py` reason about **repos in flight**. Blocks do not
respect that boundary: a `base-template` block that edits files under `core/mev/` collides with a
live `mev` lane, and the registry cannot see it because it was told two *different* repos are
running.

Build the path→block map for the whole initiative and look for two things:

- **A cross-tree writer** — a block whose `files[]` leave its own repo's tree. Legal, sometimes
  the right cut, but it must be *declared*: lane assignment downstream is built on the `repo`
  field and will otherwise schedule it beside the lane it collides with. Name the lane it may not
  run beside, in the plan and in the lane file.
- **Two writers, one file** — two blocks in different repos naming the same path. One of them is
  wrong; give the artifact a single named writer.

```bash
python3 - <<'PY'
import json, glob, collections
m = collections.defaultdict(list)
for p in glob.glob("planning/blocks/*.json"):
    b = json.load(open(p))
    for f in b.get("files", []):
        path = f if isinstance(f, str) else f.get("path", "")
        m[path].append((b["id"], b.get("repo")))
for path, owners in sorted(m.items()):
    if len({r for _, r in owners}) > 1:
        print("TWO WRITERS", path, owners)
PY
```

Cross-tree writers the script cannot judge for you: read each block's `files[]` against its own
`repo` and say, per block, whether every path is inside that repo's tree.

### C3 — A split row splits its edges too

When one row becomes two blocks — the config half and the enforcement half, the read side and the
write side — the halves inherit the row's `depends_on` wholesale. That is almost always wrong for
at least one of them: the config half does not need what the enforcement half needs, and the false
edge blocks it for the length of the run.

**Re-derive each half's edges from what that half actually needs**, rather than copying the
original row's. Two blocks tracing to the same source row with identical `depends_on` sets is the
signature — treat it as unverified until both have been re-derived.

### C4 — A sizing flag is a decision owed, not a note

If anything upstream says a block is oversized — a red-team pass, a `/sequence` note, an internal
decomposition listed in its own record ("T3–T9", "this is really four parts") — **registration is
where that gets decided**, not carried forward. Write the decision and its reason into the block
record: split it now, or defer with the trigger that would force the split later. A flag that is
neither acted on nor resolved ships the oversized block, which is exactly what happened.

### C5 — An `exit` artifact you cannot point at

Step 2's rule is already right: `exit` names the artifact whose existence ends the gate. The
failure mode is not ignorance of it — it is *satisfying* it with an artifact-shaped string. A
plist path stated nowhere, a rule file that does not exist, a config key nothing reads. It passes
every check available, because it looks exactly like a grounded answer.

**If you cannot point at the file, or at the block or command that creates it, do not write a
path.** Write what the operator must produce and leave the exit explicitly unresolved for them to
name. `"exit": "UNRESOLVED — operator names the artifact; this gate cannot close until they do"`
blocks visibly and gets fixed. An invented path closes the gate the moment anything with that name
appears, or never, and nobody can tell which.

```bash
python3 - <<'PY'
import json, glob
for p in sorted(glob.glob("planning/blocks/*.json")):
    b = json.load(open(p))
    for d in b.get("depends_on", []):
        if d.get("type") == "operator":
            print(f'{b["id"]:28} {d.get("slug","?"):34} exit: {d.get("exit","")}')
PY
```

For every row printed, point at the file on disk, or at the block or command that creates it.
Any you cannot point at gets rewritten as unresolved.

### C6 — Nothing actionable is left in prose

The initiative's own documents — `plan.md`, `roadmap.md`, `sequence.md`, the assessment, the notes,
the red-team output — accumulate real work in passing: an open question nobody closed, a "we should
also", a follow-up the red team raised and the author agreed with, a finding with no block. Every
one of them is invisible the moment the session ends.

Re-read them and route each actionable item to a container from the table at the top of this file —
a block, an operator or approval edge, a carryover, a reference, a backlog row — **or to the cut
list with a reason**. Those are the only two legitimate destinations. "Mentioned in the plan" is
neither.

```bash
grep -nEi 'TODO|follow[- ]?up|we should|still needs|open question|not covered|left for later'   planning/<slug>/*.md
```

Every hit is either already a row in `state.json`, or a cut-list line, or a defect to fix now.

### Report the pass

Say what it read and what it found: blocks scanned, defects per check (C1–C6), what was fixed, and
anything left standing as deliberate with its reason. **A pass that reports nothing on a
multi-repo initiative is a claim** — state it as one, so a reader can tell it ran.

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
