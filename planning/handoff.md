---
type: Handoff
created: 2026-07-03
---

# Handoff — EN.2.A paused: review a Claude Code Rust SDK before deciding transport

> **For the next agent:** Read this immediately after `/prime`. Delete this file once consumed.

## What we're doing and why

`engine-rs` is porting the Python `orchestrator` engine core to Rust (the parallel-pilot
rewrite, D42). **Phase 1 is fully done** — EN.1.C merged, `bastion serve` can trigger workflows
over HTTP, hold live run state in memory, and durably record runs to Postgres. The next block is
**EN.2.A — Claude Code step node** (`ClaudeCodeStep`, the shared Claude-Code primitive Phase 3's
SDLC-flow port leans on).

We started `/generate-tasks EN.2.A` but **paused before writing the spec** because the block hinges
on an unresolved, load-bearing decision: **which transport `ClaudeCodeStep` uses to invoke Claude
Code** — the native `claude-sdk-rs` launcher (`execute_claude` + `Config`, post its repair pass) vs.
the tmux/file-drop `bastion ask` seam (parity with `orchestrator/app/services/claude_code/bastion_backend.py:128`).
The master-plan explicitly frames transport primacy as an open question to "decide here." Two facts
block a clean decision (both recorded as `carryover[]`): the `claude-sdk-rs` repo **is not on disk**
here, so its native path can't be built/verified; and **the user wants to first review a Claude Code
Rust SDK on GitHub and compare it against the existing `claude-sdk-rs`** before choosing. So the
immediate task is that review/comparison, not writing the spec.

## Completed this session

- **EN.1.C fully landed** (from the prior sub-session, already merged): `/sdlc-flow` PASS across 6
  tasks, PR #1 opened, merged `--ff-only` to `main` (`2248d5a`), worktree cleaned, `state.json`
  reconciled. Phase 1 Done. (Detail preserved in `log.md`.)
- Authored the **EN.1.C spec + breakdown** earlier this session (`planning/EN.1.C-.../tasks.md`,
  `tasks.json`, `breakdown.md`) and switched its HTTP framework from axum to **actix-web** at the
  user's request — recorded as **D3** (`planning/decisions/D3-http-framework-choice.md`), linked in
  the decisions index. (These are what EN.1.C then implemented.)
- Started `/generate-tasks EN.2.A`; **stopped at the transport-decision clarify gate** (no spec
  written — working tree was clean of EN.2.A files). Surfaced two blockers → recorded as
  `carryover[]` (see below).

## Remaining work

1. **Review the Claude Code Rust SDK on GitHub the user wants to compare.** Ask the user for the
   repo URL (not yet provided). Compare it against the existing `claude-sdk-rs`
   (`agentic-portfolio/claude-sdk-rs/`, its brain cache: `docs/projects/claude-sdk-rs.md`) —
   feature surface, session/launcher API, cost/token reporting, cancellation (kill-on-drop),
   maintenance/health. Goal: decide whether to adopt/reuse it, keep `claude-sdk-rs`, or continue
   with the tmux/file-drop seam.
2. **Decide `ClaudeCodeStep`'s transport** (blocked on #1). Candidate framing offered this session:
   a `ClaudeTransport` trait with the buildable `bastion ask` file-drop seam as the default *now*
   and a native SDK impl behind the trait for when it's available — but the user has not chosen; do
   not assume.
3. **Then** resume `/generate-tasks EN.2.A`. When writing it: honor the two `carryover[]` constraints —
   **use D4, not D3**, for the transport decision file (`carryover: transport-decision-uses-d4-not-d3`),
   and account for the native path possibly being unbuildable here
   (`carryover: claude-sdk-rs-not-on-disk`). Block files per master-plan:
   `crates/engine-core/src/nodes/claude_code_step.rs` (new — no `nodes/` dir exists yet),
   the transport decision file (→ **D4**), `crates/engine-core/tests/claude_code_step.rs`.
   Out of scope (EN.2.B): cancellation-token plumbing through the run loop, abort endpoint, budget gate.
4. **Housekeeping (optional):** PR #1 (EN.1.C) is open on GitHub but `main` already has the local
   fast-forward merge — decide whether to close it as already-merged and push `main`, or reconcile
   GitHub's view. (Carried from the prior handoff; still open.)

## Durable State Updates

- `planning/state.json` `carryover[]` — two new entries:
  - `transport-decision-uses-d4-not-d3` (`kind: constraint`) — the EN.2.A transport decision file
    must be **D4**, since D3 is now the HTTP-framework decision.
  - `claude-sdk-rs-not-on-disk` (`kind: known_issue`) — native transport unbuildable/unverifiable
    here; tmux/file-drop seam is the fallback; transport primacy deliberately deferred pending the
    GitHub-SDK review.
- Ran `mev emit-state --write` after editing `carryover[]`.
- `focus.next` already points at `EN.2.A` (status `open`); no block `tasks.json` created/changed.

## Open questions / choices

- **The GitHub Claude Code Rust SDK URL** — the user referenced it but hasn't pasted the link yet.
  Ask for it first thing.
- **Transport primacy** — unresolved by design; it's the output of the review in remaining-work #1.

## Context the next agent needs

The transport decision is not a blind pick — the user is actively evaluating an external SDK against
their own `claude-sdk-rs` and wants that comparison done before committing. Lead with the review, not
the spec. Everything durable is in `carryover[]`; this file just points at it.

## First command after `/prime`

Ask the user for the GitHub URL of the Claude Code Rust SDK they want reviewed, then compare it
against `claude-sdk-rs` (start from the brain cache `docs/projects/claude-sdk-rs.md`). Resume
`/generate-tasks EN.2.A` only after the transport is decided.
