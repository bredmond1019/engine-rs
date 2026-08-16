# Chore — Plan a maintenance or housekeeping task.

## Variables

$ARGUMENTS — description of the chore to plan.

## Purpose

Plan one maintenance or housekeeping task — no behavior change, tests incidental. Output is a
**block record** (`planning/blocks/<BlockID>.json`) plus a **task list**
(`planning/<BlockID>/tasks.json`), feeding into lean `/sdlc-task`.

> **Distinct from `/ticket`:** tickets change behavior, so tests are required and Acceptance
> Criteria are non-negotiable. Chores do not — but they still state a **why** and a boundary.
> For multi-block work use `/plan` instead.

## Instructions

1. If `$ARGUMENTS` is not provided, stop and ask the user to describe the chore.

2. **Plan-quality floor — clarify, don't fabricate.** If filling a load-bearing element (which
   files to change, the scope boundary, or the **why**) would require *inventing* a fact you
   cannot ground in `$ARGUMENTS`, `CLAUDE.md`, `planning/context.md`, or the repo — **stop and ask
   a targeted question** rather than write a plausible-looking guess. A chore whose why is "it
   seemed untidy" is a chore nobody can evaluate six months later, which is exactly the gap D65
   exists to close.

3. Research the codebase: read `CLAUDE.md`, `planning/context.md`, then the files directly
   relevant to the chore.

4. THINK HARD about scope before writing:
   - Choose the **SDLC workflow** (`none` | `patch` | `task` | `run` | `flow`) and the **model**
     (`sonnet` | `gemini-pro` | `gemini-flash` | `either`). Rule of thumb: Opus for
     reasoning/breakdown only; sonnet for high-risk or complex; gemini-pro intermediate;
     gemini-flash simple. Record the reasoning in `workflow_rationale`.
   - **Compilable task boundaries (outranks the file-based split when the two conflict).**
     `/chore` only ever feeds `/sdlc-task` or `/sdlc-flow` — never `/sdlc-block`'s parallel-merge
     model — and both run every task **sequentially on one branch/worktree with no inter-task
     merge step**, gating the project's checks after **every single task**. A single breaking
     public-surface change (a renamed public type, a struct's changed fields, an altered
     trait/interface signature, and every call site each touches) must never be split across tasks
     such that an intermediate task leaves the repository non-compiling — put the whole change in
     **one** task, even if it then touches more files than usual. **Unconditional here**, with no
     `/sdlc-block` carve-out.
   - Acceptance criteria are lighter than a ticket's but still **observable** — "the check passes
     on a corpus sweep", not "the code is cleaner". End with the project's gating checks passing.

5. Choose a short descriptive slug (e.g. `remove-k8s-secret`, `update-stale-handles`). The Block ID
   is `<Prefix>.chore.<slug>`, and the spec directory equals it exactly.

6. **Write the block record and register it.** Read and follow
   `.claude/workflows/block-registration.md` — the canonical procedure for the block ID, the
   operator and cross-repo edge questions, the carryover read, the block record itself, and
   `state.json` registration. Do not restate it here or invent a variant.

   Set `kind` to `chore`. `testing_strategy` is optional for a chore — include it when the chore
   does touch test coverage, omit it when tests genuinely are incidental.

7. **Write `planning/<BlockID>/tasks.json`** — a **bare array**:
   ```json
   [
     { "task_id": 1, "title": "<First Task Name>", "description": "<specific action>", "acceptance_criteria": [], "validation_commands": [], "max_attempts": 3, "files": ["<path/to/file>"], "dependsOn": [] },
     { "task_id": 2, "title": "<Second Task Name>", "description": "<specific action>", "acceptance_criteria": [], "validation_commands": [], "max_attempts": 3, "files": ["<path/to/file>"], "dependsOn": [1] },
     { "task_id": 3, "title": "Validate", "description": "Run the block record's validation_commands and confirm all pass.", "acceptance_criteria": [], "validation_commands": [], "max_attempts": 3, "files": [], "dependsOn": [1, 2] }
   ]
   ```
   Populate `acceptance_criteria` and `validation_commands` per task — the empty arrays above are
   the *shape*, not the target. Fleet-wide, 36% of tasks shipped with empty `acceptance_criteria`
   and 53% with empty `validation_commands` because the template's empty array was read as a
   default.

8. **Render the spec view:** `python3 scripts/render_spec.py <BlockID>`. This writes
   `planning/<BlockID>/tasks.md` from the block record — the SDLC engines read it as the spec
   document. It is **generated**: never hand-edit it, edit the block record and re-render. Until
   D65 stage 2 lands this step is not optional; an engine run against a missing `tasks.md` has no
   spec to read.

9. **Property self-check (can fail).** Before reporting, confirm:
   - **`tasks.json` reads back off disk and parses** — run it, do not assert it:
     `python3 -c "import json;d=json.load(open('planning/<BlockID>/tasks.json'));assert isinstance(d,list) and d;print(len(d),'tasks')"`.
   - **The block record validates** against `.claude/workflows/block.schema.json`, with `why`,
     `description`, and `out_of_scope` non-empty.
   - **Compilable task boundaries — can fail.** Check whether any single breaking public-surface
     change is split across two or more tasks such that an intermediate task would leave the
     repository non-compiling under the per-task gate. If so this check **fails**: merge those
     tasks and re-run the self-check.
   - **`tasks.md` was rendered** and matches: `python3 scripts/render_spec.py <BlockID> --check`.

10. Report the paths created and the next step.

## Codebase Structure

- `CLAUDE.md` — standing rules, the SDLC pipeline, build/test/validate commands (start here)
- `planning/context.md` — why the project exists; `planning/status.md` — progress
- `planning/harness.json` — the project's validation commands + UI-test config
- `planning/blocks/` — block records; `planning/<BlockID>/` — spec directories
- `.claude/workflows/block.schema.json` — the block record field contract
- `.claude/workflows/block-registration.md` — the shared registration procedure

Read `CLAUDE.md` for the project's actual stack, directory layout, and conventions — do not assume
any framework, language, or directory structure that isn't written there.

## Standing rules to respect

Read `CLAUDE.md` and `planning/context.md` — internalize and enforce **the project's standing
rules**. CLAUDE.md is the authority; do not assume any stack, locale-parity, narrative, or
content-layout rule unless written there. Universal harness rules still apply: no fabricated
metrics or quotes, no emoji.

## Report

```
planning/blocks/<BlockID>.json     (block record)
planning/<BlockID>/tasks.json      (<N> tasks)
planning/<BlockID>/tasks.md        (generated view)
state.json: <created | already existed>, block registered

Next (implement + test loop):
  /sdlc-task <BlockID>
```
