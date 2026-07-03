---
type: Guide
title: sync-global-commands — Install harness commands into ~/.claude/commands/
description: Syncs all non-brain .claude/commands/ files to ~/.claude/commands/ using rsync, with guard, report, and dry-run verify.
doc_id: sync-global-commands
layer: [factory]
project: base-template
status: active
keywords: [global commands, sync, rsync, install, harness]
related: [gc-blockA-sync-command]
---

# sync-global-commands — Sync harness commands to global install

Installs all harness commands from `.claude/commands/` into `~/.claude/commands/`,
excluding the `brain/` reference directory.

## Instructions

1. **Guard — confirm you are in the base-template root.**

   Run:
   ```bash
   test -f .claude/workflows/sdlc-run.js && echo "Guard: OK — running from base-template root" || echo "ABORT: .claude/workflows/sdlc-run.js not found. Run this command from the base-template root."
   ```

   If the guard fails (file not found), **stop immediately** and tell the user:
   > "sync-global-commands must be run from the base-template root directory
   > (the directory that contains `.claude/workflows/sdlc-run.js`).
   > Please cd to that directory and try again."

2. **Count before — record current state of `~/.claude/commands/`.**

   Run:
   ```bash
   COUNT_BEFORE=$(find ~/.claude/commands -name "*.md" 2>/dev/null | wc -l | tr -d ' ')
   echo "Before: $COUNT_BEFORE .md files in ~/.claude/commands/"
   ```

3. **Sync — install commands globally.**

   Run:
   ```bash
   rsync -av --delete --exclude='brain/' .claude/commands/ ~/.claude/commands/
   ```

   This:
   - Copies the *contents* of `.claude/commands/` (trailing slash) into `~/.claude/commands/`.
   - `--delete` propagates removals and renames (stale files are removed from global).
   - `--exclude='brain/'` ensures brain-scoped reference commands are never installed globally.

4. **Verify — confirm sync is complete (dry run shows nothing to do).**

   Run:
   ```bash
   LEFTOVER=$(rsync -n --delete --exclude='brain/' .claude/commands/ ~/.claude/commands/ 2>&1)
   if echo "$LEFTOVER" | grep -qE '^(deleting |>f)'; then
     echo "WARN: dry run reports pending changes — sync may not be complete:"
     echo "$LEFTOVER"
   else
     echo "Verify: OK — dry run reports nothing to do"
   fi
   ```

5. **Report — show before/after counts and confirm brain/ is absent from global.**

   Run:
   ```bash
   COUNT_AFTER=$(find ~/.claude/commands -name "*.md" 2>/dev/null | wc -l | tr -d ' ')
   echo "After:  $COUNT_AFTER .md files in ~/.claude/commands/"

   if [ -d ~/.claude/commands/brain ]; then
     echo "FAIL: brain/ is present in ~/.claude/commands/ — it should not be there."
   else
     echo "brain/ not in global: OK"
   fi

   echo ""
   echo "Sync complete. Before: $COUNT_BEFORE  After: $COUNT_AFTER"
   ```

   Report the summary to the user in plain text, noting any discrepancies.

## Notes

- Run this command any time harness commands are added, moved, or renamed in base-template.
- `brain/` is a reference-only directory — it is never synced globally by design.
- After the sync, all commands are available globally by their filename: `/prime`, `/plan`,
  `/implement`, `/commit`, `/test_crud_api`, etc. No namespace prefix.
- Downstream project repos that have their own `.claude/commands/` override global commands
  of the same name — Claude Code's local-first walk-up takes precedence.
