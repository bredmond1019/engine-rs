---
name: write-repo-doc
description: Write or rewrite an internal doc (docs/*.md, a guide, a README section) so a reader who does not know the system can actually use it — quickstart first, plain-English section openers, vocabulary explained or linked, every named command/script/schema linked inline, and a diagram when the shape is not obvious from prose. Use BEFORE writing any doc under docs/, before rewriting one that reads as dense or reference-only, and when someone says a doc assumes too much or they could not find how to run something.
allowed-tools: Read, Write, Edit, Grep, Glob, Bash
---

# Writing an internal doc

> Governed by **D72** (`planning/decisions/D72-write-repo-doc-is-the-internal-docs-standard.md`).
> How the corpus gets converted: **D73** (`…/D73-docs-upgrade-incrementally-through-close-out.md`).

## Who you are writing for

**Brandon plans this system and approves every decision. He did not write the code — agents did.**
He cannot skim a hundred files at agent speed to reconstruct what a word means.

So the reader is: **smart, invested, and missing the context the author has.** They know the goal.
They do not know your vocabulary, your file layout, or which of the three things you named is the
one they should type.

A second reader matters just as much: **a fresh agent session with no context.** Every inline link
you write is an edge it can follow instead of guessing. Under-linking costs it a search; a wrong
link costs it a false belief.

> **The whole standard in one line:** a good teacher would never write the doc you are about to
> write by default. Write the one they would.

---

## Quickstart

1. **Read the doc as a newcomer.** Note every sentence you could not act on. That list is the work.
2. **Put a Quickstart at the top** — the literal commands, in order, that get someone from nothing
   to a result.
3. **Open every section with one plain-English sentence** saying what it is and why it exists,
   before any detail.
4. **Link every command, script, schema and decision the first time you name it.**
5. **Explain or link every term** a newcomer would not know.
6. **Add a diagram** if the shape is not obvious from prose.
7. **Run the checklist at the bottom**, then the gates.

---

## The seven rules

### 1. Quickstart first, always

The first thing after the title is **how to get started**, not what the system is.

- Real commands, copy-pasteable, in the order they are run.
- Say **where** each is typed — a Claude Code slash command and a shell command look identical on
  the page and are not interchangeable. `/begin-orchestration` goes in Claude Code. `python3 x.py`
  goes in a terminal. **Say which.**
- Then a short table of what must exist first, and what to do if it does not.

If the reader has to scroll past three paragraphs to find a command, the doc has failed regardless
of how correct those paragraphs are.

### 2. Name the runnable thing

**This rule exists because of a measured failure.** `orchestration.md` documented only
`./scripts/commander_drain.sh`. The reader had no idea `/orchestration-commander` was a slash
command he could type — and the script turned out to be a *wrapper* that reads that very command
file and feeds it to a Claude turn. The wrapper was also the dangerous path: no dry-run, always
writes to the shared lock directory.

So, for anything runnable:

- **List every way to run it**, in a table, with *when to use each*.
- Put the interactive/safe one first.
- If one path is destructive or has no dry-run, say so **at the command**, not in a later section.
- If two paths share an implementation, say that too — otherwise a reader assumes two things to
  keep in sync.

### 3. Plain English before detail

Every `##` opens with one or two sentences a newcomer can follow. Then the table, the flags, the
policy.

| Instead of | Write |
|---|---|
| "Heavy-gate repos register a slot with `fleet_concurrency_check.py`." | "Some repos are expensive to test — they launch browsers, or compile Rust. Running several at once will bury the machine. So those repos take a **slot** before starting and give it back when done, like a parking space." |
| "`base-template` always runs `--worktree`." | "A **worktree** is a second copy of the repo in a separate folder. `base-template` always uses one, because a lane there edits the very files that are running it." |

The detail is not the problem. The *missing first sentence* is.

### 4. Explain the vocabulary, or link where it is explained

Every term a newcomer would not know gets one of:

- a short definition **inline, at first use**, or
- a link to the vocabulary table or doc that defines it.

**Never both-and-neither** — a term used confidently and defined nowhere is the single most common
defect in these docs. If a repo has many such terms, build one vocabulary table (see
`base-template/docs/workflows/index.md`) and link every guide to it.

### 5. Link every named thing, inline, the first time

Naming a file the reader cannot reach is worse than not naming it: they cannot tell a live
reference from a stale one.

| Target | How to write it |
|---|---|
| A command in this repo | `[`/generate-roadmap`](../../.claude/commands/generate-roadmap.md)` |
| A script in this repo | `[`scripts/check_messages.py`](../../scripts/check_messages.py)` |
| A schema | `[`lane.schema.json`](../../.claude/workflows/lane.schema.json)` |
| A decision in this repo | `[D65](../../planning/decisions/D65-block-record-is-the-planning-unit.md)` |
| **Anything in another repo** | A **bare backticked path**, not a link — `` `agentic-portfolio/scripts/emit_state_write.sh` `` |

Three traps, all of which have fired here:

- **A relative link that climbs out of `planning/` breaks.** `planning/` is a symlink into the
  brain's vault, so `../..` leaves the *vault*, not the repo. See the `write-okf-markdown` skill.
- **Decision numbers collide across repos.** base-template's D43 is close-out integration; HQ's D43
  is the cross-domain priority graph. **Always say which repo** when citing a number.
- **Verify the target exists before you name it.** `scripts/emit_state_write.sh` was cited as local
  when it lives in HQ. One `ls` would have caught it, and the link gate would not — a bare path in
  backticks is not checked.

### 6. Give the reader the shape

If the system has moving parts, prose alone will not convey how they fit. Add a **mermaid diagram**
near the top, then **the same thing in numbered sentences underneath** — the diagram is for
orientation, the sentences are what a screen reader and a grepping agent get.

Then say plainly **which steps the reader personally does.** That single line is often the most
useful sentence in the document.

```mermaid
flowchart TD
    A["Thing you do"] --> B["Thing the system does"]
    B --> C["What you get"]
```

Diagrams render natively in this corpus. Keep them under ~12 nodes; past that, split the diagram.

### 7. Bullets and short sentences

- Prefer a table to a paragraph. Prefer a bullet to a sentence. Prefer a short sentence to a long one.
- One idea per bullet.
- **Bold the thing being decided or warned about**, so scanning works.
- Cut every clause that only tells the reader that what follows is important.
- Cite an authority instead of restating it: "field table: `<file>`" beats reproducing the table and
  letting it drift.

---

## Structure that works

```
Title
  One line: what this is. Link to the overview/vocabulary doc for newcomers.
## What this page is for      <- plain English, who it's for, what they'll be able to do
## Quickstart                 <- the commands, and what must exist first
## <Overview / diagram>       <- if the shape is non-obvious
## The <N> phases at a glance <- a table with links down into detail
## 1..N — the detail          <- each opening in plain English
## Troubleshooting            <- symptom -> likely cause -> what to check
## See also                   <- every related doc, command and decision
```

Not every doc needs every section. Every doc needs the Quickstart and the plain-English openers.

---

## Before you commit

- [ ] A newcomer can run something correctly within the first screen.
- [ ] Every runnable thing lists **all** the ways to run it, and says where each is typed.
- [ ] The destructive path is labelled at the command, not later.
- [ ] Every `##` opens with plain English before detail.
- [ ] Every unfamiliar term is defined inline or linked.
- [ ] Every command / script / schema / decision is linked on first mention.
- [ ] Cross-repo references are **bare qualified paths**, never relative links.
- [ ] Every named file was verified to exist (`ls` it — the gate will not catch a backticked path).
- [ ] Decision numbers say which repo.
- [ ] A diagram exists if the shape is non-obvious, with the same content in sentences beneath.
- [ ] Tables and bullets carry the weight; no paragraph runs past ~4 lines.
- [ ] OKF frontmatter is present and `keywords` has **3–7** entries — see `write-okf-markdown`.
- [ ] `bastion validate-brain --links`, `--structure` and `--graph` all clean, **one flag per run**.

## The check the gates cannot do

**No gate can tell whether a doc teaches.** `validate-brain` checks that links resolve and
frontmatter parses. It is silent on every rule above.

The substitute is a **fresh reader**: hand the doc to an agent with no context on this system and
ask it to perform the task the doc describes, reporting every point where it had to guess. What it
guessed at is the defect list. Do this for any doc someone will rely on without you in the room.

## Related

- `write-okf-markdown` — frontmatter, `index.md` rows, and the `planning/` symlink link trap.
- `base-template/docs/workflows/index.md` — a worked example: diagram, vocabulary table, and
  jump-links from every term.
- `base-template/docs/workflows/orchestration.md` and `lane-coordination.md` — worked examples of
  the quickstart-first, plain-English-opener shape.
- For **published** writing (blog posts, learning modules), the fuller voice standard is
  `learn-ai/content/blog/CLAUDE.md` § "Voice and tone" — same teacher identity, higher bar.
