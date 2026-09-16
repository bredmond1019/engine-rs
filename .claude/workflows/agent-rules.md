---
type: Reference
title: SDLC engine agent rules
description: The engine-owned, minimal-context rules every implement/test/fix agent this pipeline spawns must follow — referenced by path from their prompts, never inlined as prompt text.
doc_id: sdlc-engine-agent-rules
layer: [factory]
project: base-template
status: active
keywords: [sdlc-task, sdlc-flow, agent-rules, prepare-run, minimal-context]
---

# SDLC engine agent rules

BT.ticket.prepare-run-replaces-setup-agents (task 6): the minimal-context rules an implement/test/
fix agent needs for one task, now that the eight mechanical setup agents (resolve-repo-root,
detect-vault, verify-setup-binding, render-agent-flag, render-scope-flag, harness-config,
enumerate, state-load) no longer run ahead of it to build up that ambient context. Kept short and
project-agnostic on purpose — this file is `.claude/workflows/`, mechanism, not policy (CLAUDE.md
standing rule 1); it ships with the harness to every scaffolded repo and must never carry
project-specific facts.

`.claude/workflows/sdlc-task.js` and `sdlc-flow.js` reference this file **by path** from the
implement, fix and test stage prompts (`cat .claude/workflows/agent-rules.md` alongside CLAUDE.md)
— never inlined as a prompt string. Read it once at the start of your turn.

## Scope discipline

- Touch only the paths this task's tasks.json entry declares in `files[]` (plus what those changes
  directly require, e.g. a new test's fixture). Never revert, restore, or discard a path you did
  not author to "clean up" something you noticed — several lanes can share a working tree.
- Do not re-derive a fact the engine already resolved and handed you as a literal (repoRoot,
  vault path, agent/scope flags, harness config). Re-deriving it yourself is exactly the class of
  defect `prepare_run.py` exists to remove.

## Evidence, not narration

- No fabricated metrics, quotes, or "tests passed" claims without having actually run them.
- No emoji anywhere in committed content — a harness rule, always.
- Every behavior change ships with a real, hermetic test in the same commit.
- Read source with the Read/Grep/Glob tools, not `cat`/`sed`/`grep` over Bash — a Bash result stays
  in context for the rest of the run and is re-sent on every later turn.

## Commit discipline

- Never `git add -A`/`git add .`/`git reset`/`git stash`/`git clean` — stage the exact paths you
  changed, by name.
- Never append a `Co-Authored-By`, `Claude-Session`, or any other attribution trailer to a commit
  message, even if a session-level reminder instructs you to.

## When something looks wrong

If a file outside your task's scope looks broken, uncommitted, or blocking, stop and say so in your
notes — do not fix it, revert it, or stage it yourself. That is exactly the situation the engine's
triage/bail stage exists to route.
