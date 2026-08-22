---
name: stop-or-continue
description: How to decide whether to keep working in this session or hand off to a fresh one, and what to write before you do — the correctness triggers that override token count entirely, the block-boundary rule for orchestration lanes, the context thresholds, and the test for whether an artifact is good enough to clear on. Use when the operator asks whether to /clear, when context is getting large, when a natural stopping point arrives, and before recommending a fresh session for any reason.
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

## 3. Only now, the token count

The useful signal is not the number, it is **what fraction of context is finished tool output versus
active understanding.** Workflow result payloads, file dumps and long test logs are the least reusable
context you hold — a single `TaskOutput` dump can run 8k tokens. When most of the window is transcript
rather than judgement, clearing costs almost nothing regardless of the total.

With that as the frame, rough thresholds:

| Context | Posture |
|---|---|
| under ~100k | Do not raise it. |
| 100–200k | Keep going. Make sure the durable record is current at each natural boundary. |
| 200–300k | Finish the unit in flight, then suggest clearing. **Do not start a new one.** |
| over ~300k | Suggest clearing at the next boundary regardless. |

These are prompts to *raise it*, not permission to stop mid-task. Never abandon work in flight to
respect a threshold — reach the next boundary first.

### Orchestration lanes: the rule is structural, not numeric

**Clear at block boundaries; never mid-block.** A lane is built to resume at block granularity —
that is what `lane-log.jsonl` and the `planning/orchestration-run/<roadmap>/` record are for — so a
boundary is the only cheap exit. Budget roughly **20–40k of context per block** (spec reads plus the
engine's result payload) and pick the boundary that keeps you under the band above.

At every boundary a lane already writes its own handoff: the lane-log line, the `notes.md` append,
and the block's `state.json` status. If those are current, the session is disposable by construction.

---

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
