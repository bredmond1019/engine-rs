# sync-global-skills — Sync harness skills to global install

**Two targets, two audiences — both are installed by this command:**

| Target | Source | Read by |
|---|---|---|
| `~/.claude/skills/` | `.claude/skills/` (17 hand-authored) | Claude Code, in every repo and outside any repo |
| `~/.gemini/config/skills/` | `.agents/skills/` (67 = 50 command mirrors + the 17) | the Antigravity surface |

The `~/.claude/skills/` half was added by BT.chore.skills-go-global (2026-09-03). Before it, the 17
fleet skills were copied into all 19 repos — 323 byte-identical files, zero divergence — and are now
installed once here instead. **Per-repo copies of a fleet skill name are not just redundant, they are
unreachable:** skills offer no picker, and a same-named repo-local skill loses to the global one
(measured by body, not by listing). A repo needing different behaviour must use a DISTINCT name.

Freshness of the `~/.claude/skills/` half is reported by `global-skills-fresh` in
`planning/harness.json` — registered **non-gating**, because a CI checkout and a fresh clone have no
global install at all.

Installs all harness skills from `.agents/skills/` into `~/.gemini/config/skills/`, mirroring what
`/sync-global-commands` does for `.claude/commands/` → `~/.claude/commands/`. `.agents/skills/` is
the vendor-neutral skill directory other agent tools (Gemini, per `GEMINI.md`) read; Claude Code
itself reads `.claude/commands/` and does not need this sync.

## Instructions

1. **Guard — confirm you are in the base-template root.**

   Run:
   ```bash
   test -f .agents/skills/README.md && echo "Guard: OK — running from base-template root" || echo "ABORT: .agents/skills/README.md not found. Run this command from the base-template root."
   ```

2. **Count before — record current state of `~/.gemini/config/skills/`.**

   Run:
   ```bash
   COUNT_BEFORE=$(find ~/.gemini/config/skills -name "SKILL.md" 2>/dev/null | wc -l | tr -d ' ')
   echo "Before: $COUNT_BEFORE SKILL.md files in ~/.gemini/config/skills/"
   ```

3. **Sync — install skills globally.**

   Run:
   ```bash
   rsync -av --delete .claude/skills/ ~/.claude/skills/
   rsync -av --delete .agents/skills/ ~/.gemini/config/skills/
   ```

4. **Verify — confirm sync is complete (dry run shows nothing to do).**

   Run:
   ```bash
   LEFTOVER=$(rsync -n --delete .agents/skills/ ~/.gemini/config/skills/ 2>&1)
   if echo "$LEFTOVER" | grep -qE '^(deleting |>f)'; then
     echo "WARN: dry run reports pending changes — sync may not be complete:"
     echo "$LEFTOVER"
   else
     echo "Verify: OK — dry run reports nothing to do"
   fi
   ```

5. **Report — show before/after counts.**

   Run:
   ```bash
   COUNT_AFTER=$(find ~/.gemini/config/skills -name "SKILL.md" 2>/dev/null | wc -l | tr -d ' ')
   echo "After:  $COUNT_AFTER SKILL.md files in ~/.gemini/config/skills/"
   echo ""
   echo "Sync complete. Before: $COUNT_BEFORE  After: $COUNT_AFTER"
   ```

   Report the summary to the user in plain text.

## Notes

- Run this command any time harness skills are added, moved, or renamed in base-template.
- Unlike `/sync-global-commands`, there is no `brain/` reference directory to exclude under
  `.agents/skills/` — the full tree syncs.
