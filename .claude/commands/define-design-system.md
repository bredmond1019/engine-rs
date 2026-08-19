# Define Design System — establish the tokens, components and rules a new UI is built from.

## Variables

$ARGUMENTS — what is being built, plus flags.
             Example: "client booking portal --surface web --slug acme-portal"

| Flag | What it does |
|---|---|
| `--surface <web\|mobile\|tui\|desktop>` | Repeatable. Each gets its own stack and component inventory |
| `--slug <name>` | Output to `planning/<name>/design-system.md` + the artifacts it names |
| `--from <path>` | A brand guide, a client's existing site, a reference product to derive from |
| `--house` | Adopt the practice's default stack without re-deciding it (step 2) |

## Purpose

For a UI that **does not exist yet** — a new client project, a new side project, a new app.

Produces the thing the first screen is built from: design tokens, a Tailwind/theme config, a
justified component inventory, the icon set, and the rules that keep screen twenty consistent with
screen one. Then it **proves the system by building one real screen with it**, and emits a polish
standard so later work can be reviewed.

> **This is the greenfield twin of `/define-polish-standard`.** That command *reads* a running
> product and writes a rubric to judge it. This one has nothing to read and writes *artifacts* to
> build from. Do not merge them: their inputs, outputs, failure modes and proof steps all differ.
>
> **Escalation trigger — check first.** If the product already has a discernible system (a spacing
> scale in use, components being reused, a theme file), **stop and use `/define-polish-standard`
> instead.** Codify what exists; do not impose a second system beside it. Establishing a design
> system on a product that has one is a rewrite and needs approving as one.
>
> **A new page or screen in an existing app needs neither command** — it inherits the system and is
> reviewed against the existing polish standard. That is the common case; both commands are for the
> uncommon one.

## Instructions

1. If `$ARGUMENTS` is empty, stop and ask what is being built and for which surfaces.

2. **Check what the practice already settled, before choosing anything.** A new project that picks a
   different stack for no reason costs every future context the ability to move between projects.
   Read the sibling repos' `package.json` / `pubspec.yaml` and record what is actually in use.

   > **Survey what exists, but ask which examples are deliberate.** A pattern found in a codebase
   > tells you what happened, not what was intended — some of it is a decision and some is residue
   > from before the decision was made. **Ask the operator which projects are exemplary** rather
   > than inferring a house standard from a majority. Getting this backwards propagates an old
   > mistake into every new project, with the survey as its evidence.
   >
   > The web table below is the corrected version: `learn-ai` and `bastion-web` are *not* the
   > precedent, despite being the largest. `learn-ai` predates the practice's design-system
   > discipline, and `bastion-web` was built fast with the decision deferred and a basic system
   > retrofitted later. The newer frontends are the reference.

   The practice's web standard, verified 2026-08-19 in `business/bastiel` and
   `client/jardins-fitness` — the two most recent frontends, and the two built the way the operator
   intends. Adopt this; depart only with a stated reason:

   | Piece | Standard | Notes |
   |---|---|---|
   | Framework | Next 16, React 19 | RSC on |
   | Styling | Tailwind 4, CSS variables | `cssVariables: true` — semantic tokens, not literals |
   | Components | **shadcn/ui** — `components.json` present | `style: base-nova`, `baseColor: neutral` |
   | Icons | `lucide` via `lucide-react` | one set, one size scale |
   | Variants | `class-variance-authority` | |
   | Class merging | `clsx` + `tailwind-merge` | |
   | Aliases | `@/components`, `@/components/ui`, `@/lib`, `@/lib/utils`, `@/hooks` | identical in both |

   The reference `components.json`, byte-shared between both projects:

   ```json
   {
     "$schema": "https://ui.shadcn.com/schema.json",
     "style": "base-nova", "rsc": true, "tsx": true,
     "tailwind": { "config": "", "css": "<app>/globals.css",
                   "baseColor": "neutral", "cssVariables": true, "prefix": "" },
     "iconLibrary": "lucide", "rtl": false,
     "aliases": { "components": "@/components", "utils": "@/lib/utils",
                  "ui": "@/components/ui", "lib": "@/lib", "hooks": "@/hooks" }
   }
   ```

   **Start every greenfield web project with `npx shadcn@latest init` against that config.** It is
   settled — do not re-open it per project. Hand-rolling is now the *departure* and needs a reason:
   it re-solves focus management, dismissal and ARIA on every interactive component, which is
   exactly the cost `bastion-web` paid and had to go back and fix. `--house` adopts this without
   asking.

   Note what shadcn does **not** give you: it installs primitives, not a design system. The tokens
   in step 4 and the inventory in step 5 are still yours to decide — and `bastiel` and
   `jardins-fitness` each ship only one or two `ui/` components today, which is the correct starting
   size, not an oversight.

   For **mobile** the equivalent is Flutter `ThemeData` + Material 3, which `bastion-ui` uses. For
   **TUI** it is the widget library's theme primitives. Read the actual repo before asserting
   either — and apply the same residue-vs-decision question to what you find there.

3. **Ask what this product is, before choosing tokens.** Four questions whose answers change the
   system materially — if `$ARGUMENTS` does not settle them, ask:
   - **Who uses it, in what conditions?** An operator tool used one-handed on a phone over a flaky
     tunnel is not a marketing site. Density, contrast and offline behaviour all follow.
   - **Is there a brand to honour?** A client's colours and type are constraints, not suggestions.
     Without one, say so — a neutral system is a legitimate and better outcome than an invented
     personality.
   - **Light, dark, or both?** Decide now. Retrofitting a second theme onto hardcoded values is most
     of a rebuild.
   - **Widest and narrowest realistic viewport?** These become the breakpoints. Inventing five when
     the product has two states is how a system gets abandoned.

4. **Tokens before components — this order is not negotiable.** A component that hardcodes a colour
   or a gap cannot be retuned; you end up editing forty files to change one radius.

   Write the token set, small and complete:
   - **Spacing:** one scale, ~6 steps. Everything is a multiple.
   - **Type:** ~5 sizes, 2 weights. Line heights bound to sizes, not chosen per use.
   - **Colour:** semantic, never literal — `surface`, `surface-muted`, `border`, `text`,
     `text-muted`, `accent`, `danger`, `warning`, `success`. **Never `blue-500` in a component.**
     Define the full light palette on `:root`, then redefine only the tokens for dark. The semantic
     layer is what makes the second theme a config change instead of a sweep.
   - **Radius, border, shadow:** 2–3 values each. More is noise.
   - **Motion:** one duration and one easing, or none.

   Emit them as real files, not prose — the CSS custom properties, the Tailwind theme config, the
   Flutter `ThemeData`. **Name the file paths in the document.**

5. **Component inventory — justify every entry, keep it short.** The failure mode is forty
   components nobody uses, built before anyone knew what the product needed.

   **A component earns its place only if it appears on ≥2 screens of the actual planned product, or
   it is a state container every screen needs.** Anything else is speculative and waits.

   Start from the states, because they are what gets skipped and retrofitted badly: `EmptyState` ·
   `ErrorState` · `LoadingState` · `OfflineBanner`. Then the small set the product genuinely needs —
   typically `Button`, `Card`, `PageHeader`, `Nav`, `Chip`/`Badge`, `Field`.

   For each: name, variants (few), states (default/hover/focus/active/disabled/loading), which
   tokens it consumes, and the exact command or file that creates it. **A component with no
   focus-visible state is not finished** — that is the item that gets skipped and is expensive later.

6. **The rules — what keeps screen twenty consistent with screen one.** Short and enforceable:
   - Which values may never appear literally in a component (colours, spacings, radii).
   - When to compose existing components vs add a new one, and who decides.
   - The icon set and its single size scale — one set, never two.
   - How a new screen starts (from which layout primitive).
   - What is deliberately **not** systematised, so the system does not sprawl.

7. **Build one real screen with it. This is the proof step and it is the gate.**

   A design system never used to build anything is a wish. Pick the most representative real screen
   of the product — not a swatch page, not a component gallery — and build it using only the tokens
   and components defined above.

   | What happens | What it means |
   |---|---|
   | You reach for a value not in the scale | The scale is wrong, or the screen is. Fix one, say which |
   | You need a component not in the inventory | Either it belongs there, or the screen is doing something the product should not |
   | A state has nowhere to live | The state containers are incomplete — the most common miss |
   | It builds cleanly with no new values | The system holds. Record that |

   Fold every finding back into the tokens and inventory, then say what changed. A system that
   survived its first real screen unchanged is either very good or was not tested honestly — say
   which you think it is.

8. **Emit the polish standard.** The system says what to build from; a review still needs something
   to judge against. Run `/define-polish-standard` with this system as its input — the items derive
   from the tokens rather than being invented, and its calibration step still applies.

9. **Property self-check.** Revise in place until all hold:
   - Every token set is emitted as a real file at a named path, not described in prose.
   - No component hardcodes a colour, spacing or radius.
   - Colour tokens are semantic; no literal palette name appears in a component.
   - Every component names its focus-visible state.
   - Every component is justified against the ≥2-screens-or-state-container rule.
   - The state containers exist: empty, error, loading, offline.
   - Both themes are defined, or one is chosen with a stated reason.
   - **The proof screen was actually built**, and what it changed is recorded.
   - The stack matches the standard in step 2, or departs with a stated reason. For web that means
     `components.json` exists and matches the reference config.
   - Any precedent cited from another repo was confirmed with the operator as deliberate, not
     inferred from a survey.
   - Frontmatter `related:` carries ≥1 real `doc_id`; a target outside this file's scope is
     qualified `<scope>:<doc_id>`.

10. Commit with an explicit pathspec. Report.

## Session boundary

One session — steps 2–3 need the operator, and the proof screen needs the same context that wrote
the tokens.

**The first real feature runs fresh**, built by a session reading only `design-system.md` and the
emitted files. If it has to ask what a token means, that is a defect in the document, not a
question — the same handoff test as everywhere else.

Close by telling the operator:

```
Design system: planning/<slug>/design-system.md
Emitted: <token file> · <theme config> · <n> components
Stack: <named>, <matching the fleet | departing because ...>

Proof screen: <which> — built | FAILED
  Changed as a result: <what, or "nothing — see the note">

Decisions you took: <shadcn vs hand-rolled, themes, brand>

Next:
  /define-polish-standard "<product>" --surface <kind> --slug <slug>
  then build the first feature in a FRESH session, from the document alone.

<If escalation triggered:>
  This product already has a system — <the evidence>. Use /define-polish-standard
  to codify it; a second system here would be a rewrite.
```

## Output Format

~~~md
---
type: Guideline
title: "<Product> — design system"
description: <One line: the tokens, components and rules this product is built from.>
doc_id: design-system-<slug>
layer: [<surface|...>]
project: <repo slug>
status: active
keywords: [design system, tokens, components, <surface kinds>]
related: [<≥1 real doc_id>]
---

# <Product> — design system

*Established <DATE>. Proved by building `<screen>`. Tokens in `<path>`; components in `<path>`.*

## What this product is
<Who uses it, in what conditions, and the two or three consequences that follow for the system.>

## Stack

| Piece | Choice | Why / how it differs from the fleet default |
|---|---|---|

## Tokens

*Emitted to `<path>`. Nothing below may be written literally in a component.*

### Spacing · Type · Colour · Radius · Motion
<The real values. Semantic colour names only.>

## Components

| Component | Variants | States | Tokens used | Justified by | Created via |
|---|---|---|---|---|---|

*State containers — `EmptyState`, `ErrorState`, `LoadingState`, `OfflineBanner` — are not optional.*

## Rules

<Short, enforceable. What may never be literal, when to add vs compose, the icon set, how a screen
starts, and what is deliberately not systematised.>

## Proof

**Screen built:** `<path>`
**Reached for something missing:** <what, or none>
**Changed as a result:** <the token or component edits this forced>

## Out of scope
<What this system does not cover, so it does not sprawl.>
~~~

## Report

```
planning/<slug>/design-system.md
Emitted: <files>
Components: <n> (<s> state containers · <c> product components)
Stack: <fleet-matching | departures listed>

Proof screen: <which> — <what it changed>

Next: /define-polish-standard "<product>" --surface <kind> --slug <slug>
```
