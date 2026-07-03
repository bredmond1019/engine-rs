# Archive — Retire a folder/file into `planning/archive/`, distilling its durable residue first.

This command is **`brain.toml`-driven and depth-agnostic**. It works unchanged at a brain root, inside a
tier sub-brain (`core/`, `portfolio/`, `side/`, `client/`), or standalone outside any brain. It is the
**manual archive-time gate** of the D35 memory-distillation loop: *nothing leaves the embedded corpus until
its durable residue has been promoted into `knowledge.md` / `memory.md` / `decisions/`.* Archival is a
**ratchet, not a drop**.

> The *automated* sweep (the `.brain-moves-pending`-triggered recurring loop in D35 §4) is out of scope —
> a future harness. This command is the deliberate, operator-invoked version.

## Variables

$ARGUMENTS — the path (relative to the cwd or absolute) of the folder or file to archive. Optionally
followed by a short reason / "what it was" note used for the archive registry row. Example:
`planning/some-finished-initiative "what it was / why it's done"`.

## Execution Model

Spawn a subagent (Agent tool) to execute all steps below; pass the resolved `$ARGUMENTS` and this whole
Instructions section in its prompt; return its result to the user. This command **promotes durable
knowledge** before deleting it from the live corpus — favour judgment over speed; use a capable model, not
the cheapest. **Never move anything before Step 2's distillation is written.**

## Instructions

### Step 0 — Resolve the manifest and the archive home

1. **Find the brain root.** From the cwd, walk **up** parent by parent for a `brain.toml` (its first line
   begins `# brain.toml`). Its directory is `BRAIN_ROOT`. If none is found, this is a **standalone repo** —
   everything below still applies *locally* (the file pack lives in the local `planning/`); just skip any
   cross-repo reasoning.
2. **Locate the owning `planning/`.** The archive target belongs to exactly one `planning/` — the nearest
   `planning/` directory at or above the target. Its `planning/archive/` is `ARCHIVE_DIR` (the warm files
   `planning/knowledge.md` / `planning/memory.md` / `planning/decisions/` are its siblings — the
   **promotion destinations**). If `ARCHIVE_DIR` doesn't exist, create it with an `index.md`
   (`type: Index`, `status: archived`, the "Folder · What it was · Status" table header).
3. **Confirm the target is genuinely cold.** If it looks active (recent `status: active`, in-flight blocks,
   referenced as "now"/"next" in a `status.md`), STOP and ask the operator to confirm before archiving.

### Step 1 — Read the target for residue

4. Read the target folder/file in full (plans, `log.md` sections, `decisions/`, reports, READMEs). Hold the
   question: **what durable residue here would be unretrievable once this leaves the corpus?**

### Step 2 — Distill (route, don't summarize) — write BEFORE moving

5. Classify each nugget per the **D35 routing table** and promote it into the owning `planning/`'s warm
   files. *Most of the value is routing experience into the right reusable asset — not preserving text.*

   | In the cold material | Promote to |
   |---|---|
   | How-it-works / a convention still true | `knowledge.md` (semantic) |
   | A decision made implicitly, never logged | `decisions/DXX` (next number; or a `knowledge.md` entry citing it) |
   | "We tried X, it failed because Y" (scar tissue) | `memory.md` (+ note a guardrail/eval candidate) |
   | A fact that changed | `memory.md` temporal entry with `supersedes` |
   | A trajectory that worked (sdlc report) | a `memory.md` note flagging a skill/workflow candidate |
   | Pure ephemera | nothing — leave it cold |

6. Each promoted entry uses the **D35 provenance format** verbatim — so it points back to the cold source:
   ```md
   - **<claim / fact / convention / lesson>**
     source: <path-relative-to-its-repo>.md · date: <ISO date of the cold material> · supersedes: <prior-entry | —> · freshness: <as-of date>
   ```
   Append entries under the right section (`## How it works` / `## Conventions` / `## Gotchas` in
   `knowledge.md`; `## Notes` / `## Preferences` in `memory.md`); never overwrite existing entries.
7. If the residue is genuinely pure ephemera (nothing durable), say so explicitly in the report — the
   ratchet allows an empty promotion only when consciously judged empty, never by skipping the pass.

### Step 3 — Move and mark archived

8. **Capture the graph baseline (brain only, before moving).** If a `BRAIN_ROOT` was found in Step 0 **and**
   `mev` is available (a `mev` on `PATH`, the brain's `core/mev/target/release/mev`, or `cargo run -q --` in
   the mev checkout), run `mev validate-brain --graph <BRAIN_ROOT>` **now, before the move**, and save the
   full diagnostic set (the `[E_GRAPH_*]`/`[W_GRAPH_*]` lines) as the BEFORE baseline. Skip silently if
   standalone or `mev` isn't available.
9. `git mv` the target into `ARCHIVE_DIR/<name>/` (preserve history; plain `mv` only if not git-tracked).
10. Set `status: archived` in the moved content's OKF frontmatter — the top-level file and any nested `.md`
    that carried `status: active`. Leave a one-line "Archived <date> — residue distilled into
    `knowledge.md`/`memory.md`" note at the top of its index/README if it has one.

### Step 4 — Index propagation

11. Add a registry row to `ARCHIVE_DIR/index.md`: `| <name>/ | <what it was — from $ARGUMENTS or inferred> |
    <Status — e.g. "Complete — residue distilled <date>"> |`.
12. Update the **parent** `planning/index.md` (and any chain doc) that listed the now-moved folder — remove
    or repoint its row. Propagate up as scope changes.

### Step 4.5 — Verify graph integrity (brain only)

13. **Re-run the graph check and diff against the baseline.** If Step 3 captured a BEFORE baseline, re-run
    `mev validate-brain --graph <BRAIN_ROOT>` and compute the **net-new** diagnostics (present in AFTER but
    not BEFORE). Archiving can only introduce a graph error when a doc **still in the corpus** carries a
    `related:` / `[[wikilink]]` / index link pointing at a `doc_id` that just left the corpus.
    - **Net-new = 0** → report "graph clean, 0 new errors" (pre-existing diagnostics are **not** archive
      failures — do not attribute them to this archive).
    - **Net-new > 0** → surface each loudly. For each, the referrer must be repointed (fix its `related:`/link)
      or the archive of that node reconsidered. Offer to fix the dangling referrers; do not silently leave a
      newly-broken graph.
    - If `mev`/brain was unavailable, state "graph check skipped (no brain graph available)".

### Step 5 — Report

14. Show: the entries promoted and to which warm file (with their provenance lines); the move
    (`<old> → <archive path>`); frontmatter set to `archived`; both index files updated; and the **graph
    verdict** from Step 4.5 (net-new error count, or "skipped"). If standalone, note no cross-repo sync was
    needed. If you judged the residue empty, say so and why.

## Notes

- **Never archive empty-handed.** Step 2 runs before Step 3 — always. A conscious "nothing durable" verdict
  is allowed; a skipped pass is not.
- This command does **not** embed or re-index anything — archives stay out of the corpus (`brain.toml`
  `[crawl].skip_dirs`); the promoted warm entries are what restores retrievability.
- **The post-archive graph check (Step 4.5) is a diff, not an absolute pass.** A brain with pre-existing
  dangling `related:` edges will still show errors after a clean archive — that's fine. The command only
  fails the archive on **net-new** diagnostics it introduced. Pre-existing brain-content issues are a
  separate cleanup, not an archive blocker.
- Governed by D35 (the memory-distillation loop) and D30 (the `knowledge.md`/`memory.md` file pack it
  promotes into) — see `agentic-portfolio/docs/decisions/` in the company brain.

## Context / Files to Read

- `brain.toml` (at `BRAIN_ROOT` — only to confirm depth-agnostic resolution; archiving is local to one `planning/`)
- the archive target (read in full — it is the cold source)
- the owning `planning/{knowledge,memory}.md` + `planning/decisions/` (promotion destinations)
- `ARCHIVE_DIR/index.md` and the parent `planning/index.md` (index propagation)
- `mev validate-brain --graph <BRAIN_ROOT>` (Step 3 baseline + Step 4.5 verify — brain only, if `mev` is available)
