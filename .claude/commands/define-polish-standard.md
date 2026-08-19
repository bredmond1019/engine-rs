# Define Polish Standard — write the document a UI can actually be judged against.

## Variables

$ARGUMENTS — what the standard covers, plus flags.
             Example: "the BastionUI operator app --surface mobile --slug surfaces-e2e"

| Flag | What it does |
|---|---|
| `--surface <web\|mobile\|tui\|desktop>` | The surface kind. **Repeatable** — a product with a web app and a phone app gets one section per surface, because the rules differ |
| `--slug <name>` | Output goes to `planning/<name>/polish-standard.md`. Default: kebab-case of the description |
| `--from <path>` | An existing standard, style guide, or design doc to build on rather than replace |
| `--no-calibrate` | Skip the two-agent calibration. Only for a throwaway draft — never before an assessment reads it |

## Purpose

"Clean and polished" cannot be assessed against nothing. Without a written standard you get
opinions; opinions differ between reviewers; and opinions do not become blocks of work.

This command writes `planning/<slug>/polish-standard.md`: a **falsifiable** description of what
good looks like for this specific product, specific enough that two people shown the same
screenshot reach the same verdict.

It is the input a UI review reads — `/assess`'s polish scout, a `/ticket`'s acceptance criteria, a
reviewer's checklist. It is **not** a design system, a brand guide, or a redesign.

> **The test this document must pass:** hand it plus one screenshot to two fresh agents and ask
> "does this pass?" If they disagree, the standard is not finished. That is step 6, and it is the
> point of the command.

## Instructions

1. If `$ARGUMENTS` is empty, stop and ask what surface to write a standard for.

2. **Resolve the surfaces.** Take them from `--surface`, or infer from the repo and confirm with
   the user before writing. Each named surface gets its own section — the rules genuinely differ,
   and a merged section produces items that are meaningless for one of them (touch targets on a
   TUI, hover states on a phone).

3. **Look at the actual product before writing a single rule.** A standard written from general UI
   knowledge describes some other product. Run it and capture the current state:

   | Surface | How to look at it |
   |---|---|
   | web | Start the dev server, screenshot every main view at each breakpoint you intend to name |
   | mobile | Emulator or device; screenshot phone and tablet where both are supported |
   | tui | Run it in a real terminal; capture at 80x24 **and** at a wide window; capture both colour themes |
   | desktop | Screenshot at the smallest supported window and at full screen |

   If you cannot run it, say so and stop. A polish standard written without looking at the thing is
   the most confidently wrong document this harness can produce.

4. **Derive the existing system; do not invent one.** Read the code for what is already true — the
   spacing values actually used, the type sizes actually used, the colour tokens, the component
   library, the existing empty and error states. Name files and symbols.

   **The standard codifies what the product already does well and names where it is inconsistent.**
   Inventing a new scale turns a polish pass into a redesign, which is a different, larger, and
   differently-approved piece of work.

   - **Escalation trigger.** If there is no discernible existing system — spacing is ad hoc,
     there are eleven type sizes, no component is reused — then this is not a polish problem and a
     standard will not fix it. **Stop and say so.** Recommend a design decision from the operator,
     or a `/ticket` to establish the system first. Writing a standard over that gap produces a
     document every screen fails against, which is not actionable, it is just discouraging.

5. **Write the standard.** Use the Output Format below. The rule that governs every item:

   > **An item must be checkable by looking, without knowing the author's intent.**

   | Not this | This |
   |---|---|
   | "Spacing is consistent" | "Vertical gaps are multiples of 8px. Flag any that is not." |
   | "Errors are handled gracefully" | "Every failed request shows the failure, what was being attempted, and a retry control. A bare spinner or a blank pane fails." |
   | "The UI feels responsive" | "Any action over 300ms shows progress. Nothing blocks the whole view for a single pane's load." |
   | "Good contrast" | "Body text ≥ 4.5:1 against its background in both themes." |

   Every item needs a **verdict procedure** — what to look at and what makes it fail. An item you
   cannot fail is decoration.

   **Cover the states first, because they are where products are actually unpolished.** Loading,
   empty, error, offline, and permission-denied. The happy path is usually fine; those five are
   where the work is, and every review that skips them reports a clean bill of health on a broken
   product.

   Then per surface:

   | Surface | Items it needs that the others do not |
   |---|---|
   | **web** | Named breakpoints with exact widths · keyboard focus visibility and tab order · hover vs focus vs active · what reflows and what scrolls · print or none |
   | **mobile** | Minimum touch target · one-handed reach for primary actions · safe areas and notches · phone vs tablet layout · back-gesture behaviour · what happens on rotation |
   | **tui** | Minimum terminal size and what happens below it · behaviour when colour is unavailable (`NO_COLOR`, piped output) · what redraws vs appends · keybinding discoverability · how a long-running action reports progress without a cursor |
   | **desktop** | Minimum window size · resize behaviour · native menu and shortcut conventions · multi-window or single |

   **Write the Out of Scope section and make it real.** Brand, iconography, animation craft,
   copy-editing, accessibility beyond the items named. Without it a polish review sprawls into a
   redesign and nothing ships.

6. **Calibrate — this step is the gate.** A standard nobody has tested is a wish.

   - Pick **one** screenshot, ideally a middling one rather than the best or worst.
   - Spawn **two fresh agents**, each given only the standard and that screenshot, each asked:
     *"Does this pass? Cite the specific items it passes and fails."* They must not see each other's
     answer.
   - **Compare the verdicts item by item, not just the overall pass/fail.** Two agents can agree on
     "fails" for different reasons, which is still a calibration failure.
   - For every item they disagreed on: the item is too vague. Rewrite it with a sharper verdict
     procedure and re-run **both** agents on the same screenshot.
   - Repeat until they agree item by item. Record how many rounds it took and what changed — the
     items that needed tightening are the ones a human reviewer will also read differently.

   If an item cannot be made to converge after two rounds, it is a matter of taste rather than a
   standard. **Move it to Out of Scope and say so**, or escalate it to the operator as a design
   decision. Do not leave an unconvergent item in the document; it will generate contradictory
   findings forever.

7. **Property self-check.** Revise in place until all hold:
   - Every item has a verdict procedure — what to look at, what makes it fail.
   - No item uses an unmeasurable adjective (clean, modern, polished, intuitive, professional) as
     its criterion.
   - The five states — loading, empty, error, offline, permission-denied — each have an item, or
     are explicitly excluded with a reason.
   - Every named surface has its own section, and no item is applied to a surface it cannot mean
     anything for.
   - Numbers are the product's real numbers, cited to a file or token, not invented.
   - Out of Scope is non-empty.
   - Calibration ran and its result is recorded, including the rounds and what was tightened.
   - Frontmatter `related:` carries ≥1 real `doc_id`. **A target outside this file's own scope must
     be qualified `<scope>:<doc_id>`** — the scope is the tier for a sub-brain doc, the repo for a
     repo-vaulted one. Unqualified, it resolves locally and raises a corpus-wide
     `E_GRAPH_DANGLING_RELATED` that red-gates every concurrent lane.

8. Commit with an explicit pathspec. Report.

## Session boundary

Runs in one session; the calibration subagents are spawned from it.

**Whatever reads this standard runs fresh** — a polish review, `/assess`'s polish scout, a
`/ticket`'s acceptance criteria. The reviewer must meet the document the way any reviewer would,
with none of the reasoning that produced it. If a reviewer has to ask what an item means, that is a
defect in the standard, and it comes back here rather than being answered in the review.

Close by telling the operator:

```
Polish standard: planning/<slug>/polish-standard.md
Surfaces: <list>   Items: <n>   Out of scope: <n>

Calibration: AGREED after <k> round(s) — <what was tightened>
             | NOT CONVERGENT on <items> — moved to Out of Scope | needs your call

Use it:
  /assess "<...> with a UI meeting planning/<slug>/polish-standard.md" ...
  or as the acceptance criteria source for a UI /ticket

<If escalation triggered:>
  I did not write a standard. <Which surface> has no discernible existing system
  — <what is inconsistent>. This is a design decision, not a polish pass.
```

## Output Format

~~~md
---
type: Guideline
title: "<Product> — UI polish standard"
description: <One line: what "polished" means for this product, and what it deliberately does not cover.>
doc_id: polish-standard-<slug>
layer: [<surface|console|...>]
project: <repo slug>
status: active
keywords: [polish, ui, standard, <surface kinds>]
related: [<≥1 real doc_id, qualified <scope>:<doc_id> if outside this scope>]
---

# <Product> — UI polish standard

*Written <DATE> against `<repo>@<sha>`, from screenshots of the running product. Every number here
is the product's own, cited. Calibrated <k> round(s) — see the bottom.*

## How to use this

Judge one screen at a time against the items below. **Cite the item ID in every verdict** — a
finding that does not name an item is an opinion and should be discarded. An item either passes or
fails by looking; if you find yourself reasoning about intent, the item is defective — report that
rather than guessing.

## Universal — the five states

| ID | Item | Fails when |
|---|---|---|
| S1 | Loading | <what to look at> | <what makes it fail> |
| S2 | Empty | | |
| S3 | Error | | |
| S4 | Offline / disconnected | | |
| S5 | Permission denied | | |

## Universal — foundations

| ID | Item | Fails when |
|---|---|---|
| F1 | Spacing scale — <the real values, cited> | |
| F2 | Type scale — <the real sizes and weights, cited> | |
| F3 | Colour and contrast, both themes | |
| F4 | Latency and progress | |
| F5 | Destructive actions | |

## <Surface> — <web | mobile | tui | desktop>

*Breakpoints / sizes judged at: <exact values>.*

| ID | Item | Fails when |
|---|---|---|

<!-- repeat one section per surface -->

## Out of scope

| Not judged | Why |
|---|---|

## Calibration record

| Round | Disagreed on | What was tightened |
|---|---|---|

*Screenshot used: `<path>`. Final round: both reviewers agreed item by item.*
~~~

## Report

```
planning/<slug>/polish-standard.md

Surfaces: <list>
Items: <n> (<s> state · <f> foundation · <p> per-surface)
Out of scope: <n>
Numbers derived from source: <n> cited | <n> invented (must be 0)

Calibration: AGREED after <k> round(s)
  Round 1 disagreements: <items>
  Tightened: <what changed>

Next: hand this to whatever reviews the UI — /assess's polish scout, or a UI /ticket's AC.
```
