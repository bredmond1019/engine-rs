# /patch — Lightweight hotfix pipeline

> **Warning:** This workflow skips test, review, and document stages. Use only for low-risk
> hotfixes. If you are unsure whether a change is patch-scope, use `/sdlc-run` instead.

## What this is

A stripped-down pipeline for hotfixes and docs-only changes: implement → validate → commit.
No test report, no review stage, no document stage. Appropriate when:
- The change touches one file (or a tightly coupled pair).
- No new tests are needed (no new logic introduced).
- No API surface, schema, or interface changes.
- The fix is self-evidently correct and low blast-radius.

`/patch` is the **bottom rung of the pipeline ladder**. When a change is bigger than a hotfix,
Step 1 redirects you up the ladder rather than stretching `/patch` past its remit:

| Rung | When | Engine |
|---|---|---|
| `/patch` | trivial, self-evident hotfix or docs-only; no new test | this command |
| lean `/sdlc-task` | small, but it changes behaviour and needs a test (implement → fast-test → fix loop → commit) | `/sdlc-task <slug>` |
| `/sdlc-run` · `/sdlc-flow` | a whole spec (full lifecycle; `/sdlc-flow` adds a review + PR) | `/sdlc-run <slug>` · `/sdlc-flow <slug>` |

## Instructions

### Step 1 — Scope check (the ladder)

Before doing anything, confirm the change is patch-scope. Answer each question, and redirect up the
ladder at the first "yes":

1. Does the change touch more than two files? → If yes, stop and redirect (see below).
2. Does the change introduce new logic / behaviour that should be proven by a test? → If yes, stop and
   redirect to **lean `/sdlc-task`** (its fast-test→fix loop is exactly for small, tested changes).
3. Does the change affect an API endpoint, schema, or public interface, or span a whole spec? → If yes,
   stop and redirect to **`/sdlc-run`** (or `/sdlc-flow` if you want a consolidated review + a PR).

If you cannot answer all three with "no", output the matching redirect and stop:

```
SCOPE CHECK FAILED — this change is not patch-scope.
Redirect:
  - small but needs a test  →  /sdlc-task <spec-slug>      (lean: implement → fast-test → fix → commit)
  - a whole spec            →  /sdlc-run <spec-slug>        (full lifecycle, in place)
                               /sdlc-flow <spec-slug>       (full lifecycle + review + PR)
```

Pick the single rung that matches the change and name it in the redirect, then stop.

### Step 2 — Implement

Apply the fix. Read the affected file(s), make the targeted change, verify the edit looks correct.

Do not reorganize, refactor, or touch anything outside the minimal fix.

### Step 3 — Validate

Read `planning/harness.json`. Run every command listed under `validation.checks[]` whose
`kind` is `"command"` (the fast checks). Do not run the full test suite if it takes more than
30 seconds — the goal is a quick sanity check, not a full regression run.

If any check fails, fix it before proceeding. If the check failure is unrelated to the patch
and pre-existing, note it but do not block the commit.

### Step 4 — Commit

Run `/commit` with a `fix:` prefix describing the hotfix in one line.

### Step 5 — Done

Output a one-line summary:
```
Patch complete: <what was fixed> — <file(s) changed>
```

Remind the user: "Patch workflow skips test/review/document. For a small tested change run lean
/sdlc-task; for full pipeline coverage on a whole spec run /sdlc-run (or /sdlc-flow for a review + PR)."
