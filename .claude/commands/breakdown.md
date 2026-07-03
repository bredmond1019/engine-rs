# Breakdown — Decompose a task spec into agent-executable sub-steps.

Takes a task spec from `planning/` and produces a granular breakdown where every
sub-step names exact file paths, class/function names, and what to write or change —
precise enough for an agent (or a human) to execute without interpretation.

## Variables

$ARGUMENTS — path to the task spec to break down (e.g. `planning/<spec-slug>/tasks.md`).
             If omitted, default to the current block's spec identified via `planning/status.md`.
             If no spec exists for the current block, say so and suggest running `/next-task`.

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

5. Decompose each spec step into numbered sub-steps using the format `N.M`
   (e.g. step 2 → sub-steps 2.1, 2.2, 2.3). Each sub-step must be atomic:
   - One file to create or one specific change to one existing file.
   - If creating a file: state the full path and the complete structure (components, functions,
     fixtures, imports) — not "add a test file."
   - If editing a file: state the exact function or line to change and what to add or replace.
   - If running a command: write the exact command, not a description of what to run.

6. After each logical group of sub-steps (not only at the end), add an inline **Verify** check:
   a single command or observation that confirms the group succeeded before moving on.

   **Disjoint file ownership:** as you name exact paths, watch for the same existing file being
   edited under two different spec **steps** that could run as parallel tasks. If you find one, flag
   it in **Notes** — either the steps are sequentially dependent (say so) or the shared file should be
   append-only. An undeclared overlap between parallel tasks escalates the whole block at merge.

7. Write the breakdown to `planning/<block-dir>/breakdown.md` — same directory as the spec, named `breakdown.md`.

8. Commit the breakdown. Leave the working tree clean:
   ```bash
   git add planning/<block-dir>/breakdown.md
   git commit -m "planning: add breakdown for <spec-slug>"
   ```

9. Return only the path to the file created.

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
```

### State Refresh

Run `mev emit-state --write` to update the brain's focus derivation and state based on the new planning files.

## Report

Return only the path to the file created (e.g. `planning/<spec-slug>/breakdown.md`).
