# Update Docs — Documentation health sweep: find stale sections and create missing coverage.

Audits the entire documentation set against the current codebase and recent git history.
Produces a structured gap report (stale sections, missing coverage, confirmed-current).
Optionally fixes STALE sections and creates MISSING docs with `--patch`, or skips the audit
and creates all missing docs from scratch with `--bootstrap`.

This is the **ad-hoc maintenance** counterpart to `/document` (which gates on `sdlc/state.json`'s
recorded `review.verdict` being PASS, not a prose report — see `/document`'s Record section).
Use `/update-docs` for periodic doc health checks and bootstrapping outside the SDLC pipeline;
use `/document` inside it.

**Governed by the `write-repo-doc` skill** for anything this command writes or rewrites — quickstart
first, plain-English openers, vocabulary linked, every named command/script/schema linked inline.
If the target project also defines a `write-operating-doc` skill, prefer it instead for a doc whose
job is to be *acted on* (a checklist, an operating rhythm, a next-action board) rather than
understood. **Consult `write-okf-markdown`** (if the project has one) before creating or editing any
file under `docs/` or `planning/` — frontmatter requirements, the `index.md` row obligation, and
link-path traps are that skill's job, not this command's to restate.

## Variables

$ARGUMENTS — optional flags:
  - `--patch`        — after the audit, (1) apply surgical fixes for clear-cut STALE sections,
                       and (2) create new docs for MISSING capabilities flagged by the audit.
                       Conservative: only creates user-facing docs the audit confidently identifies.
  - `--bootstrap`    — skip the audit; create all missing project docs from scratch based on the
                       current codebase state. Use on new projects or after large blocks with no
                       prior doc coverage. Reads source, creates files, updates `docs/index.md`.
  - `--since <ref>`  — limit git history to commits after this ref (default: last 20 commits)
  - A bare git ref/range (e.g. `HEAD~10`, `main..HEAD`) also sets the history window

## Instructions

### Phase 1 — Git history snapshot

1. Run `git log --oneline -20` (or `--since <ref>` if provided) to understand what changed recently.
2. Run `git diff HEAD~10 --stat` (or the specified range) to see which source files shifted.
3. Note any new commands, engine flags, schema fields, or decision records added since the last
   doc update. These are the primary staleness signals.

### Phase 2 — Codebase inventory

Sweep the source of truth for each documentation area. For each area, note: what exists now vs.
what the docs claim.

| Area | Sweep target | What to check |
|---|---|---|
| Commands | `.claude/commands/*.md` (list files + read first line of each) | Count, names, purpose lines |
| Workflow engines | `.claude/workflows/*.js` (scan for exported flags, agent names, schema fields) | New flags (`--from`, `--tasks`, etc.), new agents, changed behavior |
| Harness schema | `.claude/workflows/harness.schema.json` | Fields, types, new keys |
| Scaffold | `scaffold/planning/harness.json`, `scaffold/planning/harness.examples.md` | Profiles match docs |
| Decisions | `planning/decisions/` (list files, read titles) | New ADRs not reflected in docs |

### Phase 3 — Documentation inventory

Read every file under `docs/` plus `.claude/commands/README.md`. For each doc, record:
- What it claims to cover (commands listed, flags documented, fields described, decisions cited)
- The last section or table that mentions the most recently-changed area

Build a coverage matrix: `{ doc → [capabilities it covers] }` and the inverse
`{ capability → doc that covers it (or "undocumented") }`.

### Phase 4 — Gap analysis

Run the five checks below **in this priority order** — earlier classes are structural (the reader
can't find anything) and dominate later ones (a wrong detail in a doc nobody can locate is moot).

**1. No capability catalogue.** Is there one page that answers "what can this thing do, and how do
I run it" — every command/workflow/capability, one plain-English line each, plus the invocation? An
`index.md` that just lists filenames does not satisfy this. If missing, this is the single highest-
priority MISSING item regardless of what else the sweep finds.

**2. Capabilities with no doc at all.** Diff the code's registered list (the actual dispatch table —
`.claude/commands/*.md` filenames, workflow engine names, CLI subcommands, whatever the project's
registry/router is) against the docs index. Never diff doc titles against doc titles — that only
proves the docs are self-consistent, not that they cover the code. Report the exact count on each
side (e.g. "2 of 15 registered workflows undocumented"); a diff that isn't sourced from code is not
this check.

**3. Detail-first docs.** A doc that opens with internals (`## Module layout`, `## Graph shape`)
before a `## Quickstart` and a plain-English one-line opener fails this even if every fact in it is
correct — the reader can't get started. Measure it, don't eyeball it:
````bash
grep -Lm1 "^## Quickstart" docs/*.md         # docs with NO quickstart section
grep -L '```mermaid' docs/*.md                # docs with no diagram, where the shape isn't obvious from prose
for f in docs/*.md; do echo "$f $(grep -m1 '^## ' "$f")"; done   # first heading = the tell
````
Report the fraction (e.g. "3 of 31 docs open with a Quickstart") — a bare pass/fail hides how bad it is.

**4. Index cells that restate whole docs.** An index row should be one scannable line pointing at
the authority, not a 5–10 line summary that will drift out of sync with the doc it's summarizing.
Flag any `index.md` row longer than ~2 lines and propose cutting it, grouping rows into task-
oriented sections instead of a flat alphabetical list.

**5. Links that 404 publicly.** Any relative link into a path this repo's `.gitignore` excludes
(a vaulted `planning/` symlink, a local-only cache dir) passes local validation but is dead on
GitHub. Cross-check every link target against `.gitignore` — if the target is excluded, replace the
link with a bare backticked path, per `write-okf-markdown`'s "linking out of `planning/`" section
where that skill exists.

Then classify each remaining discrepancy as one of:

**STALE** — doc references something that no longer matches source truth:
- A command count that's wrong (e.g., "22 commands" when there are 25)
- A flag or field listed in a doc that was removed, renamed, or changed behavior
- A decision number referenced that points to the wrong ADR
- An example or config snippet that no longer validates against the schema

**MISSING** — a capability exists in the codebase with no doc coverage at all:
- A new command added to `.claude/commands/` but not listed in README.md's table
- A new engine flag (e.g., `--from`, `--parallel-wave`) not documented anywhere
- A new `harness.json` field not in `docs/harness-json.md`
- A new workflow engine behavior not in `docs/architecture.md`

**NO-DOC** — code that was checked but deliberately should not get a doc:
- Internal implementation details (private functions, intermediate schema objects)
- ADR content — already captured in `planning/decisions/`, duplication would drift
- Scaffold template internals — those are the template, not user-facing docs

**CURRENT** — explicitly confirmed: doc section matches source truth.

### Phase 5 — Report

Output a structured report in this format:

```
## Documentation Health Report

### Defect classes (Phase 4 checks 1-5)
1. Capability catalogue: <present / missing — where>
2. Undocumented capabilities: <N of M registered capabilities have no doc coverage — list>
3. Detail-first docs: <fraction with a Quickstart / diagram — see measurement>
4. Index cells restating whole docs: <list of offending rows>
5. Links dead on GitHub: <list of gitignore-excluded targets>

### STALE — sections that need updating
- **<doc path>** § <section heading>
  What's wrong: <one line>
  Fix: <what the corrected text should say>

### MISSING — undocumented capabilities worth documenting
- **<capability>** (source: <file>)
  Suggested location: <which doc or "new doc: <name>">
  Why it warrants coverage: <one line>

### NO-DOC — checked, intentionally undocumented
- <thing> — <why no doc needed>

### CURRENT — confirmed up to date
- <doc path> — verified against <source files checked>
```

**Conservative threshold for MISSING:** only flag something as needing a new doc if:
- It is user-facing (a command, flag, config field, or workflow behavior)
- It is not already described in any existing doc, README, or inline comment visible from the
  docs/ inventory
- Adding a doc entry would reduce real confusion (not just increase coverage for its own sake)

### Phase 6 — Patch and Create (only if `--patch` or `--bootstrap` was passed)

If `--bootstrap` was passed, skip Phases 1–5 and go directly to Part B using the full
codebase inventory as the MISSING list — treat all undocumented user-facing capabilities
as MISSING.

**Part A — Fix STALE** (skip if `--bootstrap`):
For each **STALE** item where the fix is clear-cut (a count is wrong, a flag was renamed,
a field was added to a table):
- Apply the surgical edit described in the report.
- Leave the section marker `<!-- updated by /update-docs -->` if needed for auditability.
- Skip items marked as architecture-level changes — flag as `NEEDS_REVIEW` instead.
- Never touch `planning/` files, `log.md`, `status.md`, or `CLAUDE.md`.

**Part B — Create MISSING docs** (runs for both `--patch` and `--bootstrap`):
For each **MISSING** item from the Phase 4 report (or the full codebase scan for `--bootstrap`):
- Read the source file(s) that implement the capability before writing anything.
- Create the doc at the suggested location. Write real content — not stubs — based on what
  the source actually contains.
- Include OKF frontmatter: required fields `type`, `title`, `description`; encouraged:
  `doc_id`, `layer`, `project`, `status`, `keywords`, `related`.
- Do not create docs for NO-DOC items.
- After creating each doc, add an entry row to `docs/index.md`. Create `docs/index.md` if
  it does not exist.

After all changes, list every doc edited or created with the specific sections affected.

**When check 1 (no catalogue) fires and the fix is more than a few lines** — e.g. a whole capability
domain has grown undocumented — the shape that has worked well: a `docs/<domain>/` subdirectory with
a `README.md` (the capability catalogue: one line + invocation per capability, derived from source)
plus an `index.md` (the file listing, per Standing Rule 7 where the project has one), the parent
index grouped into task-oriented sections rather than a flat list, and one cross-cutting page for a
mechanism that was previously documented only inside a single feature's doc. Don't reach for this on
a small gap — it's for when the catalogue problem is structural, not a missing paragraph.

## Rules

- **Audit first, apply only on request.** Without `--patch` or `--bootstrap`, this command
  is read-only.
- **Source is authoritative.** If a doc and the source disagree, the source wins.
- **Conservative on MISSING.** Prefer fewer, high-value doc additions over comprehensive
  coverage of every internal detail. Three lines in an existing table beats a new standalone doc.
- **Surgical on STALE, generative on MISSING.** Fix STALE sections surgically; create MISSING
  docs from scratch. Never rewrite a STALE section beyond the identified fix.
- **Architecture-level changes → flag, don't edit.** Cross-cutting changes to existing docs
  go to `NEEDS_REVIEW`. Creating new architecture docs is fine.
- **Do not touch** `planning/` files, `log.md`, `status.md`, or `CLAUDE.md`.

### Non-negotiables

- **Verify every claim against source, not a standing rule or the ticket that prompted the sweep.**
  A rule saying "every X ships with Y" is not evidence that this project's X actually does — read
  the code. Two workflows in one sweep were assumed to match a documented convention because a rule
  said they should; they didn't. Conversely, reading the code (not the docs, not the rule) surfaced
  capabilities — three ready-made config profiles, in one pass — that the doc titles gave no hint of.
- **Don't hardcode a fact you can't confirm from source.** If a value has multiple plausible answers
  (e.g. two candidate ports/paths) and the code doesn't settle it, say so explicitly and flag it —
  a Quickstart with a variable and both candidates named beats a Quickstart with a confident guess.
- **If files moved, the project's own validation gate is the real check, not a manual link scan.**
  A hand grep for broken links can report clean while the project's structural/link validator (if
  it has one — e.g. `bastion validate-brain --structure`) finds real breakage outside `docs/`. Run
  the project's own gate, not just this command's inventory, after any file move.
- **Flags that gate the corpus don't compose.** Where the project's validator takes one check per
  invocation (links / structure / graph / state, or equivalent), run each separately — don't assume
  a single call covers all of them.

## Context / Files to Read

- `docs/` — all reference docs (architecture.md, using-the-template.md, harness-json.md, index.md)
- `.claude/commands/README.md` — the command reference
- `.claude/commands/*.md` — individual commands (list + first lines; full read only if flagged)
- `.claude/workflows/harness.schema.json` — schema source of truth
- `planning/decisions/` — ADR index (list files; read titles)
- `git log --oneline -20` and `git diff HEAD~10 --stat`
