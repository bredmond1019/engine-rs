---
type: Command
title: sync-all — Sync all commands and skills across workspaces, sub-brains, and global configs
description: Run the python script to sync templates, workspace commands, global tools, and tiers in one command.
doc_id: command-sync-all
layer: [factory]
status: active
keywords: [sync, commands, skills, global config, automation]
related: [sync-skills, sync-global-commands, sync-global-skills, sync-brain-commands, sync-brain-skills]
---

# sync-all — Sync all commands and skills across workspaces, sub-brains, and global configs

Runs the comprehensive python sync utility `sync_all_skills_commands.py` to reconcile and distribute all agent commands and skills.

## Instructions

Run the sync script:
`python3 base-template/scripts/sync_all_skills_commands.py`

This will automatically execute the following steps:
1. Sync `base-template` commands/workflows to `base-template` skills.
2. Sync root workspace `.claude/commands/` to `.agents/skills/`.
3. Reconcile and copy `base-template` skills to global config `~/.gemini/config/skills/` (removing stale skills).
4. Reconcile and copy `base-template` commands (excluding `brain/`) to global config `~/.claude/commands/` (removing stale commands).
5. Distribute shared brain commands and skills to all sub-brain tiers (`core`, `portfolio`, `side`, `client`, and `business`) based on `brain.toml`.
6. Synchronize local commands to skills for each sub-brain tier.
