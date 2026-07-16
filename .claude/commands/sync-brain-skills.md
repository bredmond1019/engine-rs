# sync-brain-skills — Distribute shared skills to all sub-brain tiers

Reads `brain.toml` to discover all sub-brain tier directories, then rsyncs the shared skills into
each tier's `.agents/skills/`. Mirrors `.claude/commands/sync-brain-commands.md`'s job for the
brain's own commands, but this one runs from `base-template/` and distributes base-template's own
generic session/planning skills — not any brain-specific command.

## Instructions

### Step 1 — Guards

1. Verify `brain.toml` exists in the current working directory. If not, abort:
   ```
   ERROR: brain.toml not found. Run sync-brain-skills from the brain root (agentic-portfolio/).
   ```

### Step 2 — Discover tiers

2. Read `brain.toml`. Extract unique `tier` values from all `[[repos]]` entries, excluding
   `_root`. These are the sub-brain tier directories to sync into.

### Step 3 — Sync

3. For each discovered tier, sync the shared skills from `base-template/.agents/skills/` into
   `<tier>/.agents/skills/`. The include list is base-template's own generic session + planning
   skills only — **not** the brain's own `log-decision`/`sync-status`/`update-progress` (those are
   brain-specific commands that don't exist in `base-template/.agents/skills/` at all; an earlier
   version of this list copied them in by mistake and they silently matched nothing).

   ```bash
   rsync -av --delete \
     --include="archive/***"\
     --include="capture/***"\
     --include="commit/***"\
     --include="handoff/***"\
     --include="log-work/***"\
     --include="prime/***"\
     --include="session-recap/***"\
     --include="wrap-up/***"\
     --include="backlog-ticket/***"\
     --include="generate-master-plan/***"\
     --include="next/***"\
     --exclude="*" \
     base-template/.agents/skills/ <tier>/.agents/skills/
   ```

### Step 4 — Report

4. For each tier, output:
   - Number of files synced / already up-to-date
   - Any errors or missing directories

5. Summarize: N tiers updated, total files synced.
