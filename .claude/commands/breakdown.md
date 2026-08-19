# Breakdown — Decompose a task spec into agent-executable sub-steps.

Takes a task spec from `planning/` and produces a granular breakdown where every
sub-step names exact file paths, class/function names, and what to write or change —
precise enough for an agent (or a human) to execute without interpretation.

> **What consumes this.** `breakdown.md` is a **reading aid for the implementing context**, not an
> executable artifact. Every SDLC engine executes `planning/<spec-slug>/tasks.json` and nothing
> else — no engine parses `breakdown.md`. So a breakdown changes what an implementer *knows*, never
> what the engine *runs*.
>
> That has one consequence worth stating plainly: **if the decomposition should change what gets
> executed — different task boundaries, a task split in two, a new dependency edge — it must be
> written back into `tasks.json`**, not left in prose here (step 7a). A breakdown that quietly
> disagrees with `tasks.json` is worse than no breakdown, because the engine follows the JSON and
> the reviewer follows the prose.

## Variables

$ARGUMENTS — path to the task spec to break down (e.g. `planning/<spec-slug>/tasks.md`).
             If omitted, default to the current block's spec identified via `planning/status.md`.
             If no spec exists for the current block, say so and suggest running `/next` to find the current block, then `/generate-tasks <BlockID>` to write its spec.

## Instructions

1. Resolve the target spec:
   - If `$ARGUMENTS` is provided, read that file.
   - If omitted, read `planning/status.md` to find the current block, then read its spec.
   - If neither yields a file, stop and explain clearly.

2. Read the spec in full. Note:
   - Every step in **Step-by-Step Tasks**
   - The **Relevant Files** or **Context Pointers** section
   - The **Acceptance Criteria** and **Validation Commands** (copied verbatim into the breakdown)

3. Read `CLAUDE.md` for **the project's standing rules** (do not assume any stack, locale-parity,
   narrative, or content-layout rule unless written there; plus the universal harness rules — no
   fabricated metrics, no emoji, gated checks must pass). These constraints belong in the relevant
   sub-steps, not as a separate note.

4. **For each step in the spec, before writing its breakdown:** read the actual source files
   that step touches. This is not optional — the breakdown must name real things:
   - If a step says "unit test X" → read the module under test to get the actual function names
     and signatures before writing the test sub-steps.
   - If a step adds new code → read an existing sibling of the same kind to match the project's
     established pattern before writing the implementation sub-steps.
   - If a step edits content/docs → read the corresponding file(s), plus any companion files the
     project's conventions require, so the breakdown captures every artifact the change must touch.
   - Read only what is relevant to each step. Do not load the entire codebase.

4a. **Decide whether each step actually needs decomposing.** This command is not free and an
   already-atomic step gains nothing from being restated at greater length. Break a step down when
   any of these hold — otherwise carry it through as a single sub-step and say so:
   - it bundles separable concerns ("implement X **and** refactor Y **and** add Z")
   - it spans multiple layers (data model + API + UI)
   - it carries a large acceptance-criteria set over several independently-testable units
   - it touches more than `breakdown.complexityThreshold` distinct files
     (`planning/harness.json`; default 3) **and** those files are heterogeneous — different shapes
     or roles, or spanning more than one concern

   Do not flag on file count alone when the files are the same shape serving one concern. This is
   the same heuristic `/generate-tasks` applies at authoring time; applying it here keeps the two
   from disagreeing. If **no** step qualifies, stop and say the spec is already atomic rather than
   writing a breakdown nobody needs.

5. Decompose each spec step into numbered sub-steps using the format `N.M`
   (e.g. step 2 → sub-steps 2.1, 2.2, 2.3). Each sub-step must be atomic:
   - One file to create or one specific change to one existing file.
   - If creating a file: state the full path and the complete structure (components, functions,
     fixtures, imports) — not "add a test file."
   - If editing a file: state the exact function or line to change and what to add or replace.
   - If running a command: write the exact command, not a description of what to run.

6. After each logical group of sub-steps (not only at the end), add an inline **Verify** check:
   a single command or observation that confirms the group succeeded before moving on.

   **Disjoint file ownership (parallel-merge safety) — `/sdlc-block` only.** As you name exact
   paths, watch for the same existing file being edited under two different spec **steps** that
   could run as parallel tasks under `/sdlc-block`'s parallel-merge model. This does **not** apply
   when the spec will run under `/sdlc-flow` or `/sdlc-task`: those engines run every step
   sequentially on one branch/worktree with no inter-task merge, and `generate-tasks.md`'s
   compilable task boundaries rule governs step boundaries there instead — including when it
   requires two steps that would otherwise be file-disjoint to merge into one sub-step, because a
   breaking change (a renamed public type, a struct's changed fields, an altered trait/interface
   signature) cannot leave an intermediate step non-compiling. Under `/sdlc-block`: if you find an
   overlap, flag it in **Notes** — either the steps are sequentially dependent (say so) or the
   shared file should be append-only. An undeclared overlap between parallel tasks escalates the
   whole block at merge.

7. Write the breakdown to `planning/<block-dir>/breakdown.md` — same directory as the spec, named `breakdown.md`.

7a. **Reconcile with `tasks.json` — the engines execute that, not this file.** If reading the
   source changed your view of the *executable* shape of the work, edit `tasks.json` too:
   - a step that must become two tasks, or two that must merge (a breaking public-surface change
     may never be split — the repository must compile at every task boundary)
   - a `dependsOn` edge the decomposition revealed
   - a `files[]` list that turned out wrong
   - an acceptance criterion the source shows is not observable as written

   Make the edit, and record it in **Notes** with one line per change and why. If nothing changed,
   say "no `tasks.json` changes" — an absent statement reads as an omission. Never leave a
   correction only in prose: the engine will run the JSON.

7b. **Verify every symbol you named actually exists.** The failure mode of this command is a
   confident sub-step referencing a function, type, or file that is not there. Before committing,
   grep each named symbol and path:
   ```bash
   rg -L --fixed-strings '<symbol>' <path>
   ```
   Anything that does not resolve is either a symbol the sub-step **creates** — mark it explicitly
   as new — or a mistake. Fix it. Report the count checked and any that were corrected.

8. Commit the breakdown. Leave the working tree clean:
   ```bash
   git add planning/<block-dir>/breakdown.md planning/<block-dir>/tasks.json
   git commit -m "planning: add breakdown for <spec-slug>"
   ```

9. Report:
   ```
   planning/<block-dir>/breakdown.md

   Steps decomposed: <n> of <total>   (<skipped as already atomic>)
   Symbols verified: <k> checked, <j> corrected, <m> marked as new
   tasks.json: <no changes | the edits made, one line each>
   ```

## What makes a sub-step unambiguous

Good sub-step:
> **2.3 Create `__tests__/lib/services/content-loader.test.ts`**
> File: `__tests__/lib/services/content-loader.test.ts`
> Suite: `describe("getPublishedPosts")`
> - `returns posts for a locale` — call `getPublishedPosts("en")`, assert the array is non-empty and every item has a `slug`
> - `handles empty input` — call `getPublishedPosts("")`, assert it returns an empty array (not an error)
> - `unknown slug returns null` — assert `getPostBySlug("missing", "en")` returns `null` (or throws — match actual behaviour in `lib/services/`)

Bad sub-step (too vague to execute without interpretation):
> - Add tests for the content loader

## Session boundary

Runs in `/generate-tasks`'s session, or its own if the spec already existed. **The engine runs
fresh** either way.

Close by telling the operator which engine command to run in a new session, and — if you edited
`tasks.json` — say so explicitly and in one line each. That edit changes what the engine executes,
and it is the one part of this command's output that will actually run.

```
Breakdown written: planning/<block-dir>/breakdown.md
tasks.json: <no changes | the edits, one line each>

Start a FRESH session and run:
  /sdlc-task <spec-slug>   |   /sdlc-flow <spec-slug>   — <reason>
```

## Context / Files to Read

- `$ARGUMENTS` (the spec file, or the current block's spec)
- `planning/status.md` (only if $ARGUMENTS is omitted)
- `CLAUDE.md`
- Source files relevant to each step (read per-step, not upfront)

## Output Format

```md
# Task Breakdown — <spec title>

## Source Spec
`<path to the spec file this was generated from>`

## Goal
<copied verbatim from the spec>

## How to Use
Work top to bottom. Each sub-step is a single atomic action. Run the inline **Verify**
checks as you go — do not batch them at the end. Each check must pass before continuing.

---

## Steps

### Step 1: <step name from spec>

#### 1.1 <atomic action — one file or one change>
**File:** `<exact relative path>`
**Action:** <create / add function / edit line / run command>
<precise content, structure, or change — not a description>

#### 1.2 <next atomic action>
...

**Verify:** `<exact command>` → <expected output or exit code>

---

### Step 2: <step name from spec>

#### 2.1 ...

...

**Verify:** `<exact command>` → <expected output>

---

<!-- repeat for every step in the spec -->

---

## Acceptance Criteria
<copied verbatim from the spec — do not paraphrase>

## Validation Commands
<copied verbatim from the spec — do not paraphrase>

## Notes
<any discoveries made while reading the codebase that affect execution — e.g. a function
 signature differs from what the spec implied, or a standing rule from CLAUDE.md applies>

## tasks.json changes
<one line per edit made to tasks.json and why — or "none". The engines execute tasks.json, so a
 correction recorded only in prose above will not reach the run.>
```

### State Refresh

Run `mev emit-state --write` to update the brain's focus derivation and state based on the new planning files.

## Report

The block in step 9 — the path, what was decomposed and what was skipped, the symbol-verification
result, and any `tasks.json` edits. The `tasks.json` line is the one that matters most: it is the
only part of this command's output an engine will act on.
