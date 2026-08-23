---
name: stop-or-continue
description: How to decide whether a session must restart for CORRECTNESS reasons, and what to write down before any handoff — the trigger table for an engine, binary, hook or CLAUDE.md changing under a running session, and the test for whether an artifact is good enough to hand off on. Context size is NOT a reason to stop; this skill says so explicitly. Use when something the session depends on changed mid-run, when the operator asks whether to /clear, and before recommending a fresh session for any reason.
allowed-tools: Bash(grep:*) Bash(ls:*) Bash(git:*)
---

# Deciding to stop, continue, or write something down first

> **Paths below are relative to the brain root** — the directory containing `brain.toml`, found by
> walking up from wherever you are. This skill is synced into every repo, so a repo-relative link
> would be wrong in most of them.

Three questions, **in this order**. Only the third one is about tokens, and most of the time you
never reach it.

---

## 1. Is there a correctness reason to restart? — overrides everything

These are not efficiency calls. They hold at 40k context as firmly as at 400k, and continuing
produces **wrong results that look like ordinary failures**, which is what makes them worth checking
first.

| Trigger | Why continuing is wrong |
|---|---|
| **An engine, command or binary changed this session** | Standing rule 10: the Workflow harness copies each engine `.js` at launch and runs *that* copy; `.claude/commands/*.md` behave the same way. A fix on `main` does not change the running session. A stale engine emits the pre-fix command, the pre-fix failure returns, and it reads as an unreliable agent rather than a stale snapshot. Measured 2026-08-19: one block re-run **four times** against an engine that never changed. |
| **A locally-installed binary was rebuilt** (`mev`, `bastion`) | Same shape one layer down, plus `emit-state --write` from a stale binary **reverts** generated boards to an older format. See `derive-state-safely`. |
| **`settings.json`, hooks, or MCP config changed** | The harness reads these at startup. |
| **The operator changed a `CLAUDE.md` you already read** | You are working from the superseded text. A mid-session diff notice is enough; a full rewrite is not. |

If any of these fired, **say so and name the trigger.** Do not present it as a token decision — the
operator should know the session is now producing untrustworthy evidence, not merely an expensive one.

---

## 2. Does the next chunk of work have a written entry point?

**The gate is the artifact, not the number.** If the next agent can start from a file, clearing is
nearly free. If it cannot, the token count is the wrong thing to be looking at — write the artifact,
*then* decide.

**Clearing is cheap when** the next step is named in `planning/status.md`, a `handoff.md`, a spec's
`tasks.json`, an orchestration-run `notes.md`, or a block record. It is cheap in this fleet *by
design*: `/handoff`, `/wrap-up`, `/close-out` and `/begin-orchestration`'s run record all exist to
make the session disposable.

**Never clear** while the un-writable context is the valuable part:

- **Mid-debug.** Ruled-out hypotheses and a half-formed pattern do not survive a write-up. Finish the
  diagnosis, write the finding, then clear.
- **Mid-block, mid-task, mid-review.** You would lose the bail reason, the triage verdict, and which
  fix was already tried.
- **While the operator is mid-decision** with you. Their reasoning is in the conversation, not on disk.

**If clearing feels expensive, that is a signal about the artifacts, not a reason to stay.** The
correct move is to fix the handoff, not to keep the session alive to compensate for it. Ask: *what do
I know that the next agent would have to rediscover?* Every answer belongs in a file — then clearing
costs nothing.

---

## 3. There is no third question — context size is not a reason to stop

**Removed 2026-08-23 by operator decision.** This section used to carry token bands (100-200k keep
going, 200-300k finish and suggest clearing, over 300k clear at the next boundary) and a structural
"clear at block boundaries, never mid-block" rule for orchestration lanes.

Both are gone, and nothing replaces them. **Do not reintroduce a numeric threshold, a percentage, or
a per-block context budget.** The measured cost of the old guidance was that lanes ended
orchestration runs after a single block and waited to be relaunched by hand, which put the operator
back in the loop after every block and defeated the purpose of running a chain at all. A chain runs
every block it was given. If context genuinely runs out, the harness summarizes and the session
continues — that is the harness's job, not a decision for the agent to pre-empt.

The correctness triggers in section 1 still stand and still override everything. They are about the
session producing **wrong** results, not expensive ones. When one fires, say which one — never dress
it up as a context-budget call, and never go looking for one as a pretext to stop.

## What to actually say

One or two lines, per `report-to-the-operator`. Lead with the recommendation and the *reason*, not
the arithmetic:

> Fresh session — `sdlc-task.js` changed this run, so this session's engine snapshot is stale
> (standing rule 10). `handoff.md` has the next step.

> Worth clearing at this boundary — ~310k, and most of it is finished workflow output. `notes.md`
> is current; nothing would be lost.

> Let's not clear yet — the repro is only in this conversation. Give me one more pass to land the
> finding in `knowledge.md`, then it is a clean break.

**Do not** recommend a fresh session without naming where the next agent starts. **Do not** dress an
efficiency call up as a correctness one — if it is only about cost, say it is only about cost. And
when the harness itself suggests `/clear`, treat that as a generic size heuristic that knows nothing
about whether the task is finished; answer question 1 and 2 before agreeing with it.

## Related

- `report-to-the-operator` — the shape of the reply this produces
- `derive-state-safely` — the stale-binary case in question 1, in full
- `run-the-gates` — what to run before calling a boundary clean
