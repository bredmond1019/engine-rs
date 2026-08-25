---
name: notify-operator
description: How to reach the human operator mid-run over `bastion notify send|ask` without recreating the noise that got the old Stop/Notification hook retired — when NOT to notify (restraint first), which verb to use, the durable-home rule inherited from ping-agent, the outcome table with an action per status, payload composition limits, and the multi-bot contract. Use BEFORE calling `bastion notify` for any reason, and BEFORE deciding a lane is blocked and the operator should be told.
allowed-tools: Bash(bastion:*) Bash(mev:*) Bash(git:*) Bash(cat:*)
---

# Notifying the operator without training them to mute it

> **Paths below are relative to the brain root** — the directory containing `brain.toml`, found by
> walking up from wherever you are. This skill is synced into every repo, so a repo-relative link
> would be wrong in most of them.

## When NOT to notify

This section comes first because it is the load-bearing half. The predecessor to this skill was a
Claude Code `Stop`/`Notification` hook that fired on every session stop — it was retired for noise,
and `~/.claude/settings.json`'s `hooks` is `{}` today because of it. A skill that makes
notification easy without making restraint explicit just rebuilds the same failure with better
plumbing: a channel that fires too often trains its audience to stop reading it, and the second
retirement is much harder than the first.

**Notify only for one of these three:**

1. A decision only the operator can make, and the lane is blocked on it.
2. A run-stopping failure the lane cannot recover from.
3. A long chain reaching its terminal state.

**Do NOT notify for any of these** — an agent that reaches for `bastion notify` on one of these is
misusing the channel:

- A block closing successfully.
- A gate going green.
- Routine progress ("task 3 of 6 done").
- A question the agent could answer itself by reading the repo.
- A retryable failure the lane can recover from on its own.

If what you have is not one of the three qualifying triggers, do not send anything — write it to
the durable home (below) and move on.

## Which verb

- **`bastion notify ask`** — when the answer changes what happens next. Holds the per-bot ask lock
  and blocks the lane for up to its timeout, so never use it rhetorically or for something you are
  not actually going to wait on.
- **`bastion notify send`** — a terminal FYI that needs no reply. Outbound-only, never blocks.

## The durable-home rule

Every `ask` is ALSO written to a durable home — an `operator` edge in `depends_on`, a
`carryover[]` entry, or an orchestration-run's `notes.md` — before or alongside the send. This is
inherited from [[ping-agent]]: a Telegram message gates nothing, sorts nowhere, and appears on no
board. The fast channel supplements the durable one and never replaces it. A timeout must leave a
record that outlives the process — if the ask times out, the durable entry is what the next agent
or the operator finds; the chat message alone is gone the moment the process exits.

## The outcome contract

`ask` prints one JSON object with a `status` field. Act on it like this — never merely read what the
status *means*, do what it tells you to *do*:

| `status` | Do this | Exit code |
|---|---|---|
| `answered` | Proceed on the tapped `option_key` (the JSON also carries `gate_id`, `decided_at`) | 0 |
| `timeout` | Record and stop. **NEVER read a timeout as approval.** | 2 |
| `stale_digest` | The payload was re-rendered since it was posted — re-ask, never execute against the stale one | 3 |
| `busy` | Another lane holds this bot's ask lock — back off and retry, or fall through to the durable home alone | 4 |
| (n/a — `send`, or `ask` usage error) | Unconfigured slug, unknown bot, or a usage error — stop | 1 |

A `timeout` is not a "no" either — it is an absence of an answer, and must be treated as neither yes
nor no. Record it and stop; do not infer intent from silence.

## Composition limits

The CLI enforces these; know them so you compose a valid payload the first time rather than by
rejection:

- At most **3 options**.
- Each option label **≤20 characters**.
- Summary **≤1024 characters**.
- `--gate-id` scoped to the lane and roadmap, e.g. `<roadmap>/<repo>/<block>` — this is what
  prevents a stale tap on an old keyboard from resolving a different lane's question.

## Which bot

`bastion notify` is multi-bot: `--bot <slug>` is available on both `send` and `ask`, defaulting to
`lane`. Credentials resolve by rule — `BASTION_<SLUG_UPPER>_BOT_TOKEN` /
`BASTION_<SLUG_UPPER>_CHAT_ID` — so a fourth bot needs an env pair and no code change.

Three rules, each because the wrong-looking-right alternative is tempting:

1. **Never fall back to another bot** when the requested slug is unconfigured. The CLI exits 1 and
   the agent stops. Silently reaching a different operator channel is worse than reaching none — no
   example in this file demonstrates a fallback, and none should ever be added.
2. `ask --bot telegram` or `--bot codesessions` **warns on stderr** that the CLI will compete with
   `bastion serve`'s poller for that token's update stream. This is allowed, expected, and **not a
   failure** — do not treat a non-empty stderr on that path as an error.
3. `send` never warns and is safe against any bot at any time, because it is outbound-only.

## Example usage

```bash
# Terminal FYI, default bot
bastion notify send --text "roadmap X finished: 6/6 blocks landed"

# A real decision the operator must make, durable home written first
bastion notify ask \
  --gate-id "<roadmap>/<repo>/<block>" \
  --summary "Migration touches prod schema — proceed or hold?" \
  --option "go:Proceed" \
  --option "hold:Hold"
```

## What this wraps

This skill is prose over `bastion notify send|ask` — the one place that verb is named. It ships no
script, no wrapper, and no token handling: `bastion notify` reads its own credentials from the
environment. If `bastion notify` is ever superseded (e.g. by engine-rs's operator seam), re-point
this one line rather than sweeping every example.

## Out of scope here

- Implementing or changing `bastion notify` itself — that is bastion's own CLI, a hard dependency
  of this skill.
- The queue/registry mechanics for peer-to-peer lane messages — [[ping-agent]] owns those; this
  skill is about the human, not another lane.
- Any credential path or token handling — `bastion notify` resolves its own env pair.
