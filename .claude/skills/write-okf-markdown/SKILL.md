---
name: write-okf-markdown
description: How to create or edit a markdown file in this brain without red-gating the fleet — whether the file needs OKF frontmatter at all, the four traps that break YAML parsing, the index.md row Standing Rule 7 requires, and the cross-repo `related:` prefix. Use BEFORE writing any new `.md` file anywhere in agentic-portfolio or a sub-repo, before adding frontmatter to an existing one, and when `bastion validate-brain` reports E_STRUCT_ORPHAN_FILE, E_GRAPH_DANGLING_RELATED, W_GRAPH_ISOLATED_NODE, or "mapping values are not allowed in this context".
allowed-tools: Bash(bastion:*) Bash(mev:*) Bash(python3:*) Bash(grep:*) Bash(ls:*) Bash(test:*)
---

# Writing markdown in this brain

> **Paths below are relative to the brain root** — the directory containing `brain.toml`, found by
> walking up from wherever you are. This skill is synced into every repo, so a repo-relative link
> would be wrong in most of them.

Three separate obligations, in this order. Most breakages come from doing #1 and skipping #2.

1. **Frontmatter** — if the file is a corpus member (Step 1 decides).
2. **An `index.md` row** — Standing Rule 7. Skipping this is the most common failure.
3. **Validation** — one flag per invocation; they do not compose.

The schema tables (every field, every controlled vocabulary, per-type examples) live in
`docs/okf-frontmatter.md`, governed by `docs/decisions/D27-enriched-okf-frontmatter.md`. **This skill is the procedure, not the
schema** — read that doc when you need to pick a `type`, a `layer`, or a `status`.

---

## Step 1 — Does this file need frontmatter at all?

Deterministic, from `core/mev/src/brain/crawl.rs::is_corpus_member`. A file is a corpus member iff,
**relative to its owning repo root**, it is:

- exactly `README.md`, `CLAUDE.md`, or `index.md` at the repo root, **or**
- anywhere under `planning/` or `docs/` (any depth).

> Those three filenames are matched **literally**. `GEMINI.md` and `AGENT.md` sit beside `CLAUDE.md`
> at the same repo roots and are **not** corpus members — they carry no frontmatter obligation and no
> index row, and editing one changes nothing a gate can see. Do not "correct" `CLAUDE.md` to another
> agent's filename anywhere in this document; the rule is the string, not the tool.

Everything else — `src/*.md`, a stray root-level `.md`, anything under an unregistered directory — is
**out of corpus**: no frontmatter obligation, no index row, no validation.

Then subtract the **ephemeral** names, from `crawl.rs::is_ephemeral`. These are excluded even inside
`planning/`:

`handoff.md` · `tasks.md` · `breakdown.md` · `worklog.md` · **any filename starting with `_`**

> **The `_` prefix is a debugging trap, not just a convention.** A probe named `_zz_test.md` is
> invisible to `validate-brain`. You will conclude detection is broken when it is working exactly as
> designed. If you are testing whether a check fires, do not name the fixture with a leading `_`.

`skip_dirs` in `brain.toml` removes whole trees regardless: `target`, `node_modules`, `.git`,
`.claude`, `.agent`, `archive`, `venv`, `.venv`, `sdlc`, `_planning`, `.mev-history`.

> **`_planning` is skipped, and every `planning/` is a symlink into it.** The corpus sees plan content
> *through* the symlink. This is why every cross-repo `rg`/`grep` sweep must pass `-L` — a sweep
> without it silently skips every leaf repo's plan content and reports a clean result that is a lie
> (Standing Rule 9).

**In corpus → continue. Out of corpus → write the file and stop.**

---

## Step 2 — Write the frontmatter

Three fields are required — `type`, `title`, `description`. Everything else is optional but earns its
place: `doc_id`, `layer`, `project`, `status`, `keywords`, `related` sharpen retrieval and create graph
edges.

```yaml
---
type: Reference
title: Human-readable title
description: One line saying what this file contains, written for a searcher.
doc_id: kebab-case-stable-id     # optional; defaults to the filename stem
layer: [meta]                    # controlled — see the schema doc
project: brain                   # controlled; OMIT for cross-cutting docs
status: active
keywords: [three, to, seven, concrete, terms]
related: [some-real-doc-id]
---
```

### The four traps that break YAML parsing

`hooks/pre-commit` exists solely because of the first one. It recurred **three times on 2026-08-06
alone, across three independent agent sessions**, after already being fixed and re-filed as a systemic
gate the day before.

| # | Trap | Why it breaks |
|---|---|---|
| 1 | **A `: ` (colon-space) inside an unquoted scalar** | YAML reads it as a nested mapping → `mapping values are not allowed in this context`. Most often in `description:` or `title:`. |
| 2 | **An unquoted `#`** | Starts a comment; the rest of your line vanishes. |
| 3 | **An em-dash clause in an unquoted plain scalar** | Combined with a colon or `#`, same failure class. Em dashes alone are fine — this is about what surrounds them. |
| 4 | **A date-only `timestamp`** | `timestamp: 2026-08-19` where full ISO-8601 with timezone is required. This had the pre-push gate **red fleet-wide** on 2026-08-14. |

**The fix is always the same: quote the scalar.**

```yaml
# Wrong — the ": " makes this a nested mapping and fails all four gates at once
description: The trap: a colon inside an unquoted scalar

# Right
description: "The trap: a colon inside an unquoted scalar"
```

**Why this is worse than it looks:** `--structure`, `--links`, `--graph` and `--state` all load the
same frontmatter. One bad `description:` fails **all four simultaneously**, and looks like four broken
gates for a change unrelated to any of them.

### `related:` — the cross-repo prefix

A bare `doc_id` resolves against the **authoring file's own scope**. Correct only when the target
lives in that same scope. Anything else must be written `<scope>:<doc_id>`.

**The prefix is not always the repo slug, and this part is not guessable:**

- A doc under a sub-brain tier's own path (`core/docs/...`) resolves by **tier** → `core:`
- A doc under a repo's vaulted planning tree (`core/_planning/engine-rs/...`) resolves by **repo** →
  `engine-rs:`, *not* `core:`, even though the path sits under `core/`

```yaml
# Wrong — silently resolves to the local scope, which has no such doc_id
related: [sequence-orchestration-extensions]
# Right
related: [engine-rs:sequence-orchestration-extensions]
```

Getting it wrong raises `E_GRAPH_DANGLING_RELATED`, and **the blast radius is corpus-wide** — a
`--graph` error red-gates every concurrent orchestration lane across the fleet, not just the repo that
authored the bad edge.

A `doc_id`-bearing file with **zero** outbound edges is an isolated graph node
(`W_GRAPH_ISOLATED_NODE`). If you set a `doc_id`, populate `related` with at least one real target.

---

## Step 3 — Add the `index.md` row (Standing Rule 7)

**This is the step that gets skipped, and it red-gates the fleet.** From
`core/mev/src/brain/structure.rs`: every corpus file in a directory must be referenced by **that
directory's** `index.md`, and every index row must point at a file that exists.

- **Direct children only.** Subdirectories are covered by their own `index.md`; a parent index does not
  cover a child directory's files.
- **A directory with no `index.md` has no coverage obligation** — no index, no orphan flags. Adding the
  first `index.md` to a directory therefore obliges you to list *every* sibling in it at once.
- The reference must be a **markdown or `file://` link**. `[[wikilinks]]`, external `http(s)://` URLs,
  and targets outside the corpus root are ignored by the check and will not satisfy it.
- If the new file changes the scope of a parent directory's `index.md`, update that too — propagate up.

| Diagnostic | Meaning |
|---|---|
| `E_STRUCT_ORPHAN_FILE` | A corpus file exists that its directory's `index.md` does not reference. Located at the orphan file. |
| `E_STRUCT_DANGLING_ROW` | An index row points at a file that is not on disk. Located at the `index.md`. |
| `W_STRUCT_DANGLING_ROW_EPHEMERAL` | A row points at a known-ephemeral name (`handoff.md`, `tasks.md`). Expected, not drift — the standard "delete after consuming" handoff row. |

---

## Step 4 — Validate

**One flag per invocation.** `validate-brain`'s flags do **not** compose — the dispatch is an
if/else-if chain with a fixed precedence (`links > structure > state > graph > sync > base`), so a
second flag is silently ignored and you get a false green on the check you thought you ran.

```bash
bastion validate-brain --structure   # index.md <-> directory coverage
bastion validate-brain --links       # dead markdown / file:// / [[wikilink]] targets
bastion validate-brain --graph       # related: edge integrity
bastion validate-brain --state       # state.json schema + block graph
```

> **A piped command's `$?` is the pipe's, not the command's.** `bastion validate-brain --graph | tail`
> reports success while the command itself exits 1. Redirect to a file, then check `$?`.

Both `mev validate-brain` and `bastion validate-brain` exist with the same flags — `bastion` delegates
to `mev`. HQ's `planning/harness.json` gates on the `bastion` form; use it for consistency.

---

## Deleting a doc — the error lands on OTHER files, in two waves

Deleting a `.md` is not the inverse of adding one. **Every error it causes appears somewhere else,
so a path-scoped check on what you deleted reports clean.** It arrives in two waves, and fixing the
first surfaces the second — expect to run the gates at least three times.

**Wave 1 — dead links (`--links`), on every file that pointed AT the deleted doc:**
- `[text](path/to/deleted.md)` markdown links
- `file:///abs/path/to/deleted.md` URIs, including in archived or superseded documents nobody
  thinks of as live
- `[[wikilinks]]`

**Wave 2 — dangling edges (`--graph`), on every file naming its `doc_id` in `related:`.** These do
**not** appear until wave 1 is clean, because `--links` and `--graph` are separate flags and you
only run the second after the first goes green.

Measured 2026-08-20 retiring `/sdlc-block` and `/sdlc-run`: deleting 12 doc pages produced **0**
errors on the deleted paths, then **58** `E_LINK_DEAD_MARKDOWN` across `index.md` and `commands.md`
in six repos, then **13** `E_GRAPH_DANGLING_RELATED` on the *surviving* `sdlc-flow.md` /
`sdlc-task.md` pages once links were clean.

**Do this before deleting, not after:**

```bash
# 1. Who links to it? (all three link forms, symlink- and gitignore-blind by default -> -L -uu)
rg -L -uu -l 'deleted-file\.md|\[\[deleted-doc-id\]\]' .

# 2. Who names its doc_id in related:?  Use the doc_id, NOT the filename - they differ.
rg -L -uu -l 'related:.*deleted-doc-id' .

# 3. Which index.md lists it? (Standing Rule 7 in reverse - the row must go too)
rg -L -uu -l 'deleted-file\.md' --glob 'index.md'
```

Fix all three sets **in the same change as the deletion**. Then run `--links` and `--graph`
separately, in that order, and re-run both after each fix.

**Two traps:**
- **`related:` holds `doc_id`s, not filenames.** Grepping the filename finds the links and misses
  every graph edge. `doc_id` defaults to the filename stem but is often set explicitly to something
  else — read the deleted file's frontmatter before you delete it, and note the `doc_id` down.
- **A retired doc's *content* usually outlives its links.** De-linking a reference to a deleted
  engine leaves prose that still describes it as live. Removing the link makes the gate green; it
  does not make the sentence true. Say what changed, or delete the sentence.

---

## Before you commit

- [ ] In corpus? (Step 1) — if not, none of this applies
- [ ] Frontmatter present, with `type` / `title` / `description`
- [ ] Every scalar containing `: ` or `#` is **quoted**
- [ ] `timestamp` (Log / ProjectStatus only) is full ISO-8601 **with timezone**
- [ ] Controlled fields (`layer` / `project` / `status`) use real vocabulary values — check the schema doc
- [ ] Cross-scope `related:` targets carry a `<scope>:` prefix
- [ ] A row exists in the directory's `index.md`
- [ ] `--structure` and `--graph` both run clean

`hooks/pre-commit` catches trap #1 at commit time — but only if hooks are enabled
(`git config core.hooksPath hooks`), and it **degrades silently to a pass** when `python3` or PyYAML is
missing. Do not treat a green commit as proof the frontmatter parses.

---

## When a gate is red and it is not yours

Errors are attributed **by delta, not by path** (`docs/decisions/D64-push-gate-delta-attribution.md`).
With concurrent agents, `--structure` frequently goes red because another session added a file and has
not written its index row yet. Check the error's path before assuming it is your change: if the file is
one you did not create, the session that created it owns the fix — say so rather than racing its
index edit.
