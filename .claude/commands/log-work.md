# Log Work — Append a Log entry, sync status, and regenerate the freshness spine via `mev emit-state`.

This command is **`brain.toml`-driven and depth-agnostic**. It works unchanged whether this repo lives
at the brain root, inside a tier sub-brain (`core/`, `portfolio/`, `side/`, `client/`), or standalone
outside any brain — it resolves everything from the manifest at runtime. There are **no baked `../`
paths and no `{{SLUG}}` token**; if you find either, it is a stale copy.

## Variables

$ARGUMENTS — optional free-text explanation of what was done. May be brief or detailed. If provided, it
is the primary narrative for the Log entry — do not discard or summarise it down. If omitted, derive the
narrative from git history and the task spec.

## Execution Model

Spawn a subagent (Agent tool) to execute all steps below; pass the resolved `$ARGUMENTS` and this whole
Instructions section in its prompt; return its result to the user. This command **authors state**
(flips closed-block status, appends the Log entry, surgically updates `status.md`, offers to record a
decision) and then shells out to `mev emit-state --write`, which is what actually regenerates the
**freshness spine** (the watermarked caches + generated rollups that `mev validate-brain --sync`
checks) — so favour accuracy over speed in the authored parts, use a capable model, not the cheapest.

## Instructions

### Step 0 — Resolve the manifest (this is what replaced the baked paths)

1. **Find the brain root.** From the current working directory, walk **up** parent by parent looking for a
   `brain.toml` file (its first line begins `# brain.toml`). The directory containing it is `BRAIN_ROOT`.
   - **If no `brain.toml` is found**, this is a **standalone repo**. Skip the brain-sync Step 3
     (`mev emit-state --write` has nothing to resolve without a `brain.toml`): do only Steps 1–2
     (local `log.md` + `planning/status.md`), then go to Step 4 and say so in the report.
2. **Identify this repo in the manifest.** Compute this repo's path **relative to `BRAIN_ROOT`** (`.` if
   you are at the root). Read `brain.toml` and find the `[[repos]]` entry whose `repo_path` equals that
   relative path. From it read: `slug`, `tier`, `status_file`, `cache_doc`, `heading`.
   - If no entry matches (a repo not yet registered), STOP and report it — do not guess a cache path.
     (Registering a new repo is `/new-project` / `/generate-sub-brain`'s job.)
   - All `status_file` / `cache_doc` paths in the manifest are **relative to `BRAIN_ROOT`** — resolve
     them against `BRAIN_ROOT`, never against the current directory.

### Step 1 — Local `log.md`

3. Get today's date (`YYYY-MM-DD`) and the current ISO-8601 timestamp.
4. Open this repo's root `log.md` (`type: Log`; create it with OKF frontmatter if missing). Insert a dated
   entry — append a `### <title>` sub-entry under today's `## [YYYY-MM-DD]` section (create the date
   section at the top, just below `# Log`, if today's is absent):
   ```markdown
   ### <Short title of the session>
   - **What:** <what was built / changed / decided>
   - **Why:** <what prompted it — the problem, request, or insight>
   - **Refs:** <links to the driving plan / decision / tasks — else omit>
   ```
   **Why** is required; if `$ARGUMENTS` doesn't make the reason clear, ask one brief question first.
5. Update the `timestamp` field in `log.md`'s frontmatter to the current ISO-8601 time.

### Step 2 — Local `planning/status.md` (state + momentum)

6. **First, flip any block this session closed.** If a block completed (review PASS / merged), set its
   `status` to `closed` in `planning/state.json` `tracks[].blocks[]` **before** deriving — that authored
   field is the *input* the derivation reads; `emit-state` never infers completion from `status.md`
   (the sync is one-way by design). Skipping this leaves `focus` and every generated surface stale until
   a future session reconciles by hand (the engine-rs `state-json-block-status-stale` incident,
   2026-07-03).

   Then open this repo's `status_file` (`type: ProjectStatus`) and update **surgically** — only the
   parts `emit-state` does not derive:
   - the `timestamp` frontmatter field → current ISO-8601 time;
   - bump the `## Metrics` counters if present (cheap; skip if the file has none);
   - any hand-maintained prose (`## Momentum`, narrative callouts) that reflects this session's work.
   - If you are **not certain** what the new focus is, state your
     proposed change and ask before writing. (You do not need to hand-write the focus line itself —
     `emit-state` derives it from `tracks[]` in the next step.)

   Step 3 below shells out to `mev emit-state --write` to regenerate every derived surface from this
   authored state. Do NOT manually reimplement focus derivation, cache-watermark reconciliation, or
   rollup-table rendering here — that duplicates what the derivation engine now owns and risks
   drifting out of sync with it.
7. **Settled decision?** If a settled architectural choice was made, ask whether to record it in
   `planning/decisions/` (never auto-author). On yes: next number from `planning/decisions/index.md`,
   create `D{N+1}-<kebab-title>.md` with OKF frontmatter, append a row to the decisions index.
8. **Never edit `master-plan.md`** from this command.

> If `BRAIN_ROOT` was not found (standalone repo), stop here and report.

### Step 3 — Regenerate derived surfaces (`mev emit-state --write`)

9. Shell out to `mev emit-state --write` (it walks up from the current directory to find
   `brain.toml` itself — no need to `cd` to `BRAIN_ROOT` first). This is the **single derivation
   engine** and it regenerates, in place, every generated surface from the `tracks[]` you just
   authored in Step 2:
   - this repo's leaf `state.json` focus fields (`now` / `next` / `blocked`);
   - the brain `state.json`'s `repos[]` / `cross_repo[]` rollup and its own `focus`;
   - the per-project cache doc's focus headline + `synced_from` watermark
     (`docs/projects/<slug>.md`) — the one exception is `slug == "brain"`, whose cache is its own
     `README.md` `## Quick Status` subsection, handled manually in Step 3b below because that
     subsection carries no `generated:` sentinel;
   - the tier sub-brain's tier-rollup table (tiered repos only);
   - the HQ Operating Board;
   - `master-plan.md`'s wave/dependency tables.

   Do NOT manually reimplement any of the above — hand-editing a cache's `synced_from`, a tier
   rollup table, or a wave table now duplicates what `emit-state` owns and risks drifting out of
   sync with it. Confirm the run reported no `W_EMIT_NO_SENTINEL` warning against a target this repo
   feeds; if one lands, report it rather than hand-authoring the missing sentinel pair (sentinels
   are never invented into prose — that's a separate fix to the target doc).

   **Step 3b — `_root` repos' Quick Status (the one surviving manual edit).** For repos whose
   `tier == "_root"` (the brain itself, `learn-ai`, `base-template`), the brain root additionally
   keeps a hand-maintained `## Quick Status` in its `README.md` — `emit-state` does not generate
   this subsection (no `generated:` sentinel around it), so it stays a manual surgical edit: update
   only THIS repo's `###` subsection there (verify the heading matches `heading` before writing —
   never touch another project's subsection).

### Step 4 — Report

10. Show the user: the `log.md` entry added; the `status.md` fields changed; the `emit-state --write`
    summary (which surfaces it touched, and any `W_EMIT_NO_SENTINEL` warning); the `_root` Quick
    Status edit if applicable; and any decision recorded. If standalone, say the brain sync was
    skipped.

## Context / Files to Read

- `brain.toml` (at `BRAIN_ROOT` — the manifest; resolve everything from it)
- this repo's `log.md` and `status_file` (`planning/status.md`)
- the current `planning/<concept>/tasks.md`, if any
- `core/mev/docs/cli.md` (`emit-state` section) if unfamiliar with exactly what it regenerates
- `BRAIN_ROOT/README.md` (`## Quick Status` — the one manual target, `_root` repos only)
