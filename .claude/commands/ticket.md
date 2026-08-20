# Ticket — Plan a small behavior-change with observable Acceptance Criteria.

## Variables

$ARGUMENTS — description of the bug fix, enhancement, or small behavior-change to implement.

## Purpose

Plan one small, well-scoped behavior-change — a bug fix or targeted enhancement that requires new
or modified tests. Output is a **block record** (`planning/blocks/<BlockID>.json`) plus a
**task list** (`planning/<BlockID>/tasks.json`), feeding directly into lean `/sdlc-task`.

A ticket is the one producer that writes both records at author time: it is a one-off with no
downstream block waiting on its code, so there is nothing to defer (D65).

> **Distinct from `/chore`:** chores are maintenance — no behavior change, tests incidental.
> Tickets are behavior-changing: tests required, Acceptance Criteria non-negotiable.
> For multi-block work use `/plan` instead.

## Instructions

1. If `$ARGUMENTS` is not provided, stop and ask the user to describe the bug or change.

2. **Plan-quality floor — clarify, don't fabricate.** If filling a load-bearing element (which
   files to change, what the observable correct behavior is, an Acceptance Criterion, or the
   **why**) would require *inventing* a fact you cannot ground in `$ARGUMENTS`, `CLAUDE.md`,
   `planning/context.md`, or the repo — **stop and ask a targeted question** rather than write a
   plausible-looking guess. An honest "I need X to write the AC" beats a confident invention.

3. Research the codebase: read `CLAUDE.md`, `planning/context.md`, then the files directly
   relevant to the change.

3a. **Reproduce it, or observe the current behaviour, before writing a single Acceptance
   Criterion.** This is the ticket-sized version of establishing ground truth and it is the
   highest-value minute in this command.
   - **Bug fix:** make it fail. Run the failing case and record the actual output or error text.
     A ticket written from a *described* bug fixes the description, not the bug — and the AC will
     describe a symptom nobody has seen.
   - **Enhancement:** run the current behaviour so the AC says what changes, not what you assume
     is there today.
   - **If you cannot reproduce it**, say so plainly and write that into the block record's `why`.
     Then either ask the user for the reproduction, or make the first task "reproduce and record
     the failing case" — do not write AC over an unobserved failure.

3b. **The floor, ticket-sized.** Two questions, a sentence each — this is the whole of the
   pre-plan discipline at this scale:
   - **Does this call anything it does not build?** If so: is that capability wired in
     production, or does it merely exist in source — behind a flag, with only test callers, never
     executed? Name the call site or say there isn't one. A "small fix" that turns out to depend
     on something half-built is not small, and this is the only cheap moment to find that out.
   - **What else touches the file(s) this changes?** One grep. If the answer is "a lot," the
     blast radius is the real scope.

4. THINK HARD about scope before writing:
   - A ticket is a **single coherent unit** — one logical change, one set of tests. If the fix
     touches more than 3–4 files or needs its own sub-phases, it belongs in `/plan`.
   - **Escalation trigger — size is not the only reason to escalate.** Stop and say so if step 3a
     could not reproduce the behaviour and nobody can supply a reproduction, or if step 3b's
     half-built question cannot be answered for something this change depends on. Both mean the
     work is not yet understood well enough to be a ticket. Recommend `/assess` on that area, or
     ask the user — do not write a plausible ticket over the gap. A ticket sized as a one-file fix
     that turns out to be a rewrite is the specific failure this trigger exists to catch, and it
     is not visible afterwards.
   - Choose the **SDLC workflow** (`none` | `patch` | `task` | `run` | `flow`) and the **model**
     (`sonnet` | `gemini-pro` | `gemini-flash` | `either`). Rule of thumb: Opus for
     reasoning/breakdown only; sonnet for high-risk or complex; gemini-pro intermediate;
     gemini-flash simple. Record the reasoning in `workflow_rationale`.
   - Every task in `tasks.json` must name ≥1 concrete file in `files[]` (the Validate task is
     exempt).
   - **Compilable task boundaries (outranks the file-based split when the two conflict).**
     `/ticket` only ever feeds `/sdlc-task` or `/sdlc-flow`, and both run every task
     **sequentially on one branch/worktree with no inter-task merge step**, gating the project's
     checks after **every single task**. A single breaking public-surface change (a renamed public
     type, a struct's changed fields, an altered trait/interface signature, and every call site
     each touches) must never be split across tasks such that an intermediate task leaves the
     repository non-compiling — put the whole change in **one** task, even if it then touches more
     files than usual. Unconditional: both engines are sequential.

5. **Un-gateable acceptance criteria must be declared, not just written down (D64).** This repo's
   checks are in-repo and in-language, and structurally cannot observe evidence outside that
   boundary. For **each** Acceptance Criterion apply this mechanical test, keyed on *where the
   criterion's evidence lives* — never on how important or risky it feels:

   | Evidence location | Verdict |
   |---|---|
   | this repo, this language, observable in-process | **gated** — say nothing further |
   | another process (an external CLI such as `gh`), another repo, a generated artifact, or an **installed artefact** (the binary the fleet runs, as opposed to the source tree the checks compile) | **declare it** |

   A criterion in the second row is written in the object form —
   `{"criterion": "...", "gateable": false, "evidence": "<the concrete fixture that stands in>"}`
   — and gets a dedicated fixture-evidence task in `tasks.json`. **`tasksPassed` is evidence of
   gate agreement, not correctness**; a green suite is never itself the evidence for an
   un-gateable criterion.

   Ordinary criteria ("the function returns X", "the diagnostic fires") resolve to the first row
   instantly and get no added ceremony — this rule must stay quiet on the common case or it
   destroys the lean lane.

5a. **The test must be shown failing before the fix lands (D68, ticket-sized).** A ticket's gate is
   its new test, and a test that has never gone red is not evidence — it may be asserting something
   that was already true. Order the tasks so the test is written and observed failing *before* the
   change that makes it pass, and say so in that task's acceptance criteria: *"`<command>` fails
   with `<the real error from step 3a>` before task N, passes after."*

   Where the test genuinely cannot precede the fix, name in the task how the gate was otherwise
   shown capable of failing — a retro-fixture against the known-bad input, or the reproduction
   recorded in 3a. `tasksPassed` is agreement between gates, never proof of correctness.

   **Corollary for any verification step that shells out to an installed binary** (`mev`,
   `bastion`, or similar): state explicitly whether it checks **source** or **installed**
   behaviour. The two diverge, and the divergence is invisible unless named.

6. Choose a short descriptive slug (e.g. `fix-null-deref`, `add-rate-limit`). The Block ID is
   `<Prefix>.ticket.<slug>`, and the spec directory equals it exactly.

7. **Write the block record and register it.** Read and follow
   `.claude/workflows/block-registration.md` — the canonical procedure for the block ID, the
   operator and cross-repo edge questions, the carryover read, the block record itself, and
   `state.json` registration. Do not restate it here or invent a variant.

8. **Write `planning/<BlockID>/tasks.json`** — a **bare array** matching the `SDLCTask` shape plus
   the two additive fields this template's tooling uses:
   ```json
   [
     { "task_id": 1, "title": "<First Task Name>", "description": "<specific action>", "acceptance_criteria": [], "validation_commands": [], "max_attempts": 3, "files": ["<file this task touches>"], "dependsOn": [] },
     { "task_id": 2, "title": "<Second Task Name>", "description": "<specific action>", "acceptance_criteria": [], "validation_commands": [], "max_attempts": 3, "files": ["<file this task touches>"], "dependsOn": [1] },
     { "task_id": 3, "title": "Validate", "description": "Run the block record's validation_commands and confirm all pass.", "acceptance_criteria": [], "validation_commands": [], "max_attempts": 3, "files": [], "dependsOn": [1, 2] }
   ]
   ```
   Populate `acceptance_criteria` and `validation_commands` per task — the empty arrays above are
   the *shape*, not the target. Fleet-wide, 36% of tasks shipped with empty
   `acceptance_criteria` and 53% with empty `validation_commands` because the template's empty
   array was read as a default.

9. **Property self-check.** Re-read what you wrote and **revise in place** until every property
    holds, then re-check:
    - **`tasks.json` must be read BACK off disk and parsed** — a verification you *perform*, not
      an assertion you write. Run it:
      `python3 -c "import json;d=json.load(open('planning/<BlockID>/tasks.json'));assert isinstance(d,list) and d,'must be a non-empty bare array';print(len(d),'tasks')"`.
      If that errors or you skip it, the check has not been performed. Reporting the spec path
      without `tasks.json` on disk is a **failed run** — every engine enumerates its task loop
      from `tasks.json`, and a missing or unparseable file hard-aborts the next step (D16).
    - **Reaffirm the D45 shape**: a bare array (never a `{"tasks": [...]}` wrapper — that is D44,
      superseded), 1-indexed integer `task_id`, `description` as one string, and no `status` or
      `attempt_count` key authored by you (engine-owned).
    - **The block record validates** against `.claude/workflows/block.schema.json`, and `why`,
      `description`, and `out_of_scope` are all non-empty.
    - **Every task names ≥1 concrete file** in `files[]` (Validate exempt).
    - **Compilable task boundaries — can fail.** Check whether any single breaking public-surface
      change is split across two or more tasks such that an intermediate task would leave the
      repository non-compiling under the per-task gate. If so this check **fails**: merge those
      tasks and re-run the self-check.
    - **Un-gateable criteria are declared (D64) — can fail.** Re-apply step 5's evidence-location
      table to every criterion. Any criterion whose evidence lives outside this repo's gates and
      that carries neither a named failing command nor a dedicated fixture-evidence task fails
      this check. Judge purely on where the evidence lives.
    - **Testing Strategy is non-empty** — names the test file(s) and what each must cover. The
      schema requires it for `kind: ticket`.
    - **The reproduction is recorded** — the block record's `why` carries the observed failure
      (real output or error text), or states plainly that it could not be reproduced and what the
      first task does about that.
    - **The gate is shown capable of failing** — a task orders the test before the fix, or names
      the fixture standing in for that.
    - **Nothing this ticket depends on is unclassified** as built / half-built / absent.

10. Report the paths and next step.

## Session boundary

`/ticket` authors and decomposes in one session — that is the point of the lean lane, and the work
is small enough that the record-stands-alone property is cheap to satisfy directly.

**The engine runs fresh.** `/sdlc-task` spawns its own agent stack; carrying an authoring context
into it buys nothing.

Close by telling the operator:

```
Ticket authored: planning/blocks/<BlockID>.json + planning/<BlockID>/tasks.json
Reproduced: <the observed failure> | NOT REPRODUCED — <what task 1 does about it>

Start a FRESH session and run:
  /sdlc-task <BlockID>

<If escalation triggered:>
  I did not write this ticket. <Which question could not be answered.>
  Recommend: /assess <area>  — or answer the question and re-run /ticket.
```

## Codebase Structure

- `CLAUDE.md` — standing rules, stack, build/test/validate commands (start here)
- `planning/context.md` — why the project exists; `planning/status.md` — current state
- `planning/harness.json` — validation commands + UI-test config
- `planning/blocks/` — block records; `planning/<BlockID>/` — spec directories
- `.claude/workflows/block.schema.json` — the block record field contract
- `.claude/workflows/block-registration.md` — the shared registration procedure

Read `CLAUDE.md` for the project's actual stack and conventions — do not assume any framework,
language, or directory structure that isn't written there.

## Standing rules to respect

Read `CLAUDE.md` and `planning/context.md` and enforce **the project's standing rules**. CLAUDE.md
is the authority. Universal harness rules apply: no fabricated metrics or quotes, no emoji, every
ticket ships with tests.

## Report

```
planning/blocks/<BlockID>.json     (block record)
planning/<BlockID>/tasks.json      (<N> tasks)
state.json: <created | already existed>, block registered

Next (implement + test loop):
  /sdlc-task <BlockID>
```
