# /conditional_docs — Task-type documentation router

Routes the agent to the documentation most relevant to the current task type. Use this at the
start of a task to avoid reading unrelated files. Takes an optional `$ARGUMENTS` describing
what kind of task you are about to do.

## Instructions

Read `$ARGUMENTS`. Match it to the closest category below and direct yourself (or the user) to
read the listed files. If the argument is absent or does not match, use the **default** set.

---

### new feature / feature

You are building something new — a capability, endpoint, or component that does not exist yet.

Read in this order:
1. `planning/master-plan.md` — understand the project's overall direction and where this feature fits.
2. `planning/context.md` — confirm the architectural constraints.
3. `planning/harness.json` — check validation commands and any stack-specific config.
4. The relevant spec in `planning/<concept>/tasks.md` if one already exists.

Then use `/generate-tasks` (or `/plan`) to plan the work before starting.

---

### bug / fix / hotfix

You are fixing something broken.

Read in this order:
1. `planning/context.md` — orient to the codebase.
2. `planning/harness.json` — know the validation commands you will run after fixing.

Then:
- If the fix touches one or two files with no new tests needed → use `/patch`.
- If the fix is multi-file or requires new tests → use `/sdlc-run`.

---

### api / endpoint

You are adding or modifying an API endpoint.

Read in this order:
1. The project's `CLAUDE.md` — look for API conventions (naming, auth, error format).
2. `planning/harness.json` — check if there are API-specific validation checks.
3. `docs/api-reference.md` if it exists — avoid duplicating or breaking existing patterns.

---

### test / testing

You are writing or running tests.

Read in this order:
1. `planning/harness.json` — `validation.checks[]` defines what the harness runs; understand
   the check kinds (`command`, `baseline-diff`, `count-delta`, etc.).
2. `.claude/commands/e2e/README.md` — available E2E test templates and how to use them.
3. The relevant spec's `## Validation Commands` section if you are writing task-specific tests.

---

### docs / documentation

You are updating or writing documentation.

Read in this order:
1. `planning/master-plan.md` — understand what the project is and its current phase.
2. `planning/context.md` — confirm scope and governing rules.
3. `docs/` index if it exists (`docs/index.md`) — see what docs already exist.

Then use `/document` (after a PASS review) or `/update-docs` for a standalone doc health sweep.

---

### default (no argument or unrecognized)

Read the minimum useful orientation set:
1. `planning/context.md`
2. `planning/status.md`
3. `planning/harness.json`

Then describe what you are trying to do and the agent will route appropriately.
