# Prime — Deep orient to the current project at the start of a session.

## Instructions

1. Read each file listed in **Context / Files to Read** in order.
2. Run `git ls-files` to see the full tracked file tree.
3. Check for an active handoff: run `ls planning/handoff.md 2>/dev/null`.
   - If the file exists, read it. Lead your summary with an **Active Handoff** section
     (see format below) before everything else. This is the most important thing a new
     session agent needs to see.
   - If absent, skip this section silently.
3.5. **Freshness gate — `mev validate-brain --sync`.** Check whether this repo participates in a
   brain (walk up from the current directory looking for `brain.toml`, same resolution `/log-work`
   uses). Degrade gracefully rather than failing:
   - If `mev` is not found on `PATH`, say so in the Freshness line and continue — do not treat this
     as an error.
   - If no `brain.toml` is found (standalone repo), say so in the Freshness line and continue.
   - Otherwise run `mev validate-brain --sync` (read-only) from the brain root. If it reports every
     watermark in sync, note that. If it reports a drift (a project's source `timestamp` does not
     match its cache's `synced_from`, e.g. `E_SYNC_DRIFT` / `E_SYNC_WATERMARK_MISSING`), name the
     stale project(s) and **offer** to run `mev emit-state --write` to reconcile them — never run it
     without the user's explicit confirmation (D26: validation stays read-only by default). If the
     user declines or doesn't respond, proceed with the rest of `/prime` unchanged; the drift is
     informational, not a blocker.
3.6. **Warm memory — by budget, never by reflex** (brain decision **D67**). Run
   `wc -w planning/knowledge.md planning/memory.md` and branch on the **combined** count:
   - **≤ 2,500 words** → read both files in full.
   - **over 2,500** → read **only** the topic list
     (`grep '^### ' planning/knowledge.md planning/memory.md`), then open a named section on
     request and only then.

   These are D35-distilled entries — durable conventions and scar tissue harvested from archived
   plans, and the only retrieval path back to them once `planning/archive/` leaves the corpus.
   Never load both in full when over budget, and **never call `syn recall` here**: at session start
   there is no question yet, and a digest that ranks above its own sources makes retrieval worse,
   not better. Skip silently if the files are absent.

4. Summarize your understanding in plain prose:
   - **Active Handoff** (if `planning/handoff.md` exists) — surface first:
     ```
     ## Active Handoff — <title from handoff.md>
     <What's in flight and why (from the "What we're doing and why" section).>
     Remaining: <bullet list from "Remaining work">
     First command: `<command from "First command after /prime">`
     > Delete `planning/handoff.md` once this session has consumed it.
     ```
   - What this repo does (one paragraph).
   - Key directories and what lives in each.
   - Current project phase and focus (from context.md and status.md).
   - **Freshness** — one line summarizing the Step 3.5 gate result: in sync / drifted (name the
     stale project(s) and whether an emit was offered + the user's answer) / `mev` not found /
     standalone repo (no `brain.toml`). Place this near the Carryover section.
   - **Carryover** — if `planning/state.json` has any `carryover[]` entries, list the active ones
     (those whose `clears_when` is unresolved): slug, `kind` (constraint / known_issue / env /
     deferred), and a one-line gist. These are durable caveats/follow-ons the session must respect.
     Omit if there are none or the file is absent.
   - **Warm memory** — one line. Under budget: the distilled facts bearing on the current focus.
     Over budget: `N words across M topics — headings only`, plus the topics nearest the focus, and
     note a named section can be opened on request. Omit if the files are absent.
   - Anything flagged as standing rules worth keeping in mind.
5. Do not edit any file beyond the explicit, user-confirmed `mev emit-state --write` in Step 3.5.
   Everything else in this command is read-only.

## Context / Files to Read

- `README.md`
- `CLAUDE.md`
- `planning/context.md`
- `planning/status.md`
- `planning/state.json` (the `carryover[]` array, if present)
- `planning/handoff.md` (if present — check with ls first)
- `planning/knowledge.md` + `planning/memory.md` — **under the Step 3.6 budget**, never both in
  full when the combined `wc -w` exceeds 2,500 words
