# Implement — Execute a plan file against the codebase.

## Variables

$ARGUMENTS — path to the plan file to implement, with an optional task number suffix.

Examples:
- `planning/<spec-slug>/tasks.md` — run all tasks in the plan
- `planning/<spec-slug>/tasks.md 1` — run only Task 1
- `planning/<spec-slug>/tasks.md 3` — run only Task 3

## Instructions

1. If `$ARGUMENTS` is not provided, stop and ask the user for the plan file path.
2. Parse `$ARGUMENTS`: split on the last space. If the trailing token is a number, treat it as the **task number** to run in isolation; the remainder is the plan file path. If no number is present, run all tasks.
3. Run `/prime` to orient to the codebase before touching any code.
4. Read the plan file in full.
5. THINK HARD about the plan: understand the goal, relevant files, and the target task(s) before writing anything.
6. **If a task number was given:** execute only that numbered task from the Step-by-Step Tasks section. Skip all others. After completing it, run only the validation checks directly relevant to that task (a subset of the project's checks in `planning/harness.json` / the spec's `## Validation Commands`). Do NOT run the full validation suite — that is reserved for when all tasks are complete.
7. **If no task number was given:** execute every Step-by-Step task in order, top to bottom.
   - Follow existing code patterns and conventions (see CLAUDE.md).
   - Read `CLAUDE.md` and `planning/context.md` and enforce **the project's standing rules** — do not assume any stack, locale-parity, narrative, or content-layout rule unless written there.
   - Universal harness rules: no fabricated metrics/quotes (verify model ids / package names via the `claude-api` skill, not memory), no emoji, every change ships with tests.
8. After all tasks are complete (full run only), run the spec's `## Validation Commands` exactly as written. If the spec has none, run the project's checks from `planning/harness.json` (`validation.checks[]`); if that is absent too, stop and ask the user for the validation commands.
9. If validation fails, fix the failure before reporting.
10. Record the completed work (see Record).

## Context / Files to Read

- `$ARGUMENTS` (the plan file)
- `CLAUDE.md` (standing rules)
- Files listed in the plan's Relevant Files section
- `planning/<spec-slug>/sdlc/state.json`, if present — prior task history for this spec (read, don't overwrite fields you didn't touch)

## Record (worklog + state, not a prose report)

No prose report file. Record this step's outcome the way `/sdlc-flow` and `/sdlc-task` do (D31): a
structured `sdlc/worklog.md` section, plus an update to `sdlc/state.json`. Both live under
`planning/<spec-slug>/sdlc/`, are **write-only artifacts** (never `git add`/`git commit` them — same
D46-vault reasoning the engines use: `planning/` is a symlink into a brain vault in a vaulted repo,
so committing anything under it can fail "beyond a symbolic link"; they're read back off disk, never
out of git history), and are separate from the git commit you still make for the actual code/test
changes per the spec's own instructions.

1. **Derive the spec dir:** `planning/<spec-slug>/tasks.md` → `planning/<spec-slug>/sdlc/`. Create it if it does not exist (`mkdir -p`).

2. **Read `sdlc/state.json`** if it exists (else start from `{}`). Preserve every field you are not
   updating — this file accumulates across `/implement`, `/test`, `/fix`, `/review-task`, `/document`
   calls on the same spec.

3. **Write `sdlc/state.json`** with these fields set/merged (object shape matches `sdlc-flow.js`'s
   in-memory `state`, adapted for a standalone run — `mode: "standalone"` instead of `branch`/`worktree_path`):
   ```json
   {
     "spec_slug": "<spec-slug>",
     "mode": "standalone",
     "status": "implemented",
     "current_task": <N or null for a full run>,
     "started_at": "<preserve from existing file, else NOW, UTC ISO8601>",
     "updated_at": "<NOW, UTC ISO8601>",
     "tasks": {
       "<N>": {
         "status": "implemented",
         "attempts": 1,
         "summary": "<one-line what was built/changed>",
         "issues": [],
         "fixes": [],
         "decisions": ["<any non-obvious choice, or omit if none>"],
         "files_changed": ["path/to/file", "..."],
         "commit": "<short hash of the commit you made for this task, or \"\" if none yet>",
         "validated": "<task-scoped | full | none>"
       }
     }
   }
   ```
   Only touch the `tasks["<N>"]` entry (or entries, for a full run) this call actually implemented —
   leave every other task's entry untouched.

4. **Append to `sdlc/worklog.md`** (create with header `# Worklog — <spec-slug>` + a blank line
   first, if the file doesn't exist yet). One section per task implemented this run, blank line
   before each:
   ```markdown
   ## Task <N> — IMPLEMENTED
   What: <one-line summary of what was built/changed>
   Files: <comma-joined file list, or omit if empty>
   Decisions: <non-obvious choices, semicolon-joined; omit line if none>
   Commit: <short hash, or omit if not yet committed>
   Validated: <task-scoped | full | none>
   ```
   (Omit any line whose value is empty — same convention `sdlc-flow.js` uses for its own worklog entries.)

5. Summarize the same information to the user in the chat.

Then output the pipeline next step:
```
Next: /test planning/<spec-slug>/tasks.md [N]
```
