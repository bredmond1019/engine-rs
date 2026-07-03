---
type: Guide
title: sync-downstream-harness — Pull the harness into every scaffolded repo
description: Runs scripts/sync_downstream_harness.py to copy changed .claude/commands/*.md + .claude/workflows/ files into every repo scaffolded from base-template, then reports what changed per repo so each can be reviewed and committed.
doc_id: sync-downstream-harness
layer: [factory]
project: base-template
status: active
keywords: [downstream sync, harness pull, template-version, workflows, propagation, D48]
related: [D48-downstream-harness-sync-script, sync-global-commands]
---

# sync-downstream-harness — Pull the harness into every scaffolded repo

Automates the manual "update loop" `docs/using-the-template.md` §6 describes: copies base-template's
current `.claude/commands/*.md` (flat root only — never `.claude/commands/brain/`) and
`.claude/workflows/` (the SDLC engines + `templates/`) into every repo discovered via `brain.toml`
that already has its own `.claude/workflows/` directory. Never deletes a repo's own customizations
(e.g. `feature.md`). See `planning/decisions/D48-downstream-harness-sync-script.md` for why this
exists — `.claude/workflows/*.js` has no equivalent of `/sync-global-commands`' global install path,
so a harness fix here doesn't reach a downstream repo until this runs.

## Variables

$ARGUMENTS — optional flags, space-separated:
- `--repo <slug>` — limit to one repo (repeatable: `--repo bastion --repo mev`). Default: all
  eligible repos.
- `--apply` — write changes. Default is dry-run (report only, nothing written).
- `--message "<text>"` — description recorded in each synced repo's `planning/.template-version`.
  Default: `"harness pull"`. Use something specific (e.g. the decision id driving the pull).

## Instructions

1. **Guard — confirm you are in the base-template root.**

   Run:
   ```bash
   test -f .claude/workflows/sdlc-run.js && echo "Guard: OK — running from base-template root" || echo "ABORT: .claude/workflows/sdlc-run.js not found. Run this command from the base-template root."
   ```

2. **Dry run first, always** — even if `$ARGUMENTS` includes `--apply`, run once without it first
   so the report is visible before anything is written:
   ```bash
   python3 scripts/sync_downstream_harness.py <$ARGUMENTS minus --apply, if present>
   ```
   Read the per-repo file list. If a repo shows an unexpectedly large diff (e.g. hundreds of lines
   in one engine file), that's very likely just a stale repo catching up several pulls at once —
   confirmed harmless in practice (D48's provenance) via `diff <target file> <base-template file>`
   showing the sync produces a byte-identical copy, not corruption. Flag it in the report either way.

3. **If `--apply` was requested, run it for real:**
   ```bash
   python3 scripts/sync_downstream_harness.py <$ARGUMENTS>
   ```
   This writes the changed files and updates each synced repo's `planning/.template-version`
   (`commit:` + `synced:` fields). It does **not** commit.

4. **Per repo, before committing:** check for pre-existing unrelated dirty state so it doesn't get
   swept into the harness-pull commit by accident:
   ```bash
   cd <repo_path> && git status --short | grep -v '\.claude/' | grep -v 'planning/\.template-version'
   ```
   If that prints anything, it's unrelated in-progress work in that repo — leave it out of the
   commit (stage `.claude/` and `planning/.template-version` explicitly, never `git add -A`).

5. **Commit in each repo that changed**, one commit per repo (matching the established convention
   already used historically — see `mev`/`bastion` git log for examples):
   ```bash
   cd <repo_path>
   git add .claude/ planning/.template-version
   git commit -m "chore(harness): pull base-template <short-hash> — <what changed, one line>"
   ```
   Portfolio-tier repos (`rag-engine-rs`, `workflow-engine-rs`, `claude-sdk-rs`) gitignore
   `.claude/`/`planning/` entirely by design (D8, published-repo hygiene) — the sync still updates
   those files locally (harmless), but there is nothing to commit there. Don't force-add them.

6. **If the pull touched `sdlc-flow.js` / the tasks.json contract specifically**, also check
   `core/orchestrator` — its `SDLC_FLOW` workflow (`app/schemas/sdlc_schema.py`,
   `app/workflows/sdlc_flow_workflow.py`) is a second, independent implementation of the same
   contract (see `core/orchestrator/docs/sdlc-flow-workflow.md`, and D45's provenance). This script
   does not touch orchestrator's Python code — flag for the user whether orchestrator's schema still
   matches, or whether a note is needed in orchestrator's own `planning/state.json` `carryover[]`.

7. **Sweep for already-broken specs** the fix should also repair: any repo's `planning/*/tasks.md`
   still using the old `### <prefix>.<n>.<n>` heading pattern with `**Status:** Not started` can be
   converted cleanly (write its `tasks.json`, trim `tasks.md`'s Step-by-Step Tasks section to a
   pointer). A spec already `In progress` or `Done` needs no touching — leave it. See
   `core/bastion/planning/13.1-persistent-agent-panel/` for a worked example of this conversion.

8. **Report:** which repos were synced, how many files each, which repos had nothing to sync, any
   repo skipped (no `.claude/workflows/`, or gitignored), and any spec found + fixed in step 7.

## Notes

- Run this after any change to `.claude/commands/*.md` (flat) or `.claude/workflows/` lands in
  base-template — the same trigger as `/sync-global-commands`, just for the repos that consume the
  harness as project source rather than as installed slash commands.
- The script never deletes. A file in a downstream repo's `.claude/` that doesn't exist in
  base-template (a repo's own command) is left untouched, always.
- `--repo` accepts the `slug` field from `brain.toml`'s `[[repos]]` entries, not the directory name
  (usually the same, but check `brain.toml` if unsure).
