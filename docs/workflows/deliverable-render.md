---
type: Reference
title: Deliverable Render Workflow
description: How the DELIVERABLE_RENDER workflow works — the two-node markdown-then-PDF render of an AutomationRoadmap, the authored_locale mismatch refusal, the typst CommandRunner seam, event schema, tunable DeliverableRenderPolicy, triggering, and reading outputs
doc_id: deliverable-render-workflow
layer: [engine]
project: engine-rs
status: active
keywords: [deliverable-render, workflow, typst, pdf, locale, automation-roadmap, command-runner, graph]
related: [architecture, proposal-generator-workflow, sdlc-flow-policy, data-contract]
---

# Deliverable Render Workflow

`DELIVERABLE_RENDER` (block `EN.4.D`) turns an already-written `AutomationRoadmap` (the
structured output of [`PROPOSAL_GENERATOR`](proposal-generator.md), `EN.4.C`) into the
client-facing deliverable: a locale-correct markdown file, then a PDF rendered from it by the
`typst` CLI. It does not write or research anything — the roadmap arrives inline on the event —
so both of its nodes are deterministic; no model runs on the default path.

Source: `crates/engine-core/src/workflows/deliverable_render/` (`mod.rs`, `schema.rs`,
`policy.rs`, `profiles.rs`, `render_markdown.rs`, `render_pdf.rs`, `graph.rs`), registered from
`crates/engine-serve/src/workflows.rs` (`register_deliverable_render` →
`register_builtin_workflows`).

## Quickstart

Trigger a run with `POST /events` (the same endpoint every workflow dispatches through) and a
`DELIVERABLE_RENDER` event body:

```bash
curl -X POST http://localhost:PORT/events \
  -H "Content-Type: application/json" \
  -d '{
    "workflow_type": "DELIVERABLE_RENDER",
    "event": {
      "roadmap": { "...": "an AutomationRoadmap, usually PROPOSAL_GENERATOR output" },
      "locale": "pt-BR",
      "output_dir": "/tmp/deliverables"
    }
  }'
```

What must exist first:

| Needs | Why |
|---|---|
| A finished `AutomationRoadmap` | This workflow renders one; it does not produce one. Get it from a `PROPOSAL_GENERATOR` run, or hand-build one for a test. |
| `roadmap.authored_locale == event.locale` | A mismatch fails the run before anything is written — see [Locale is total](#locale-is-total-and-the-authored_locale-refusal). |
| `typst` on `PATH`, for a real PDF | The `RenderPdfNode` subprocess call fails loudly (`NodeError` with the runner's stderr) if `typst` is missing or exits non-zero. Not installed on the dev host as of 2026-08-24 — tests stub the runner (see [The `typst` `CommandRunner` seam](#the-typst-commandrunner-seam)). |
| `event.output_dir` writable | Both `<company-slug>-roadmap.md` and `.pdf` land here. |

## Graph shape

```
RenderDeliverableNode -> RenderPdfNode
```

Two nodes, straight line, no branching. `RenderPdfNode` is the sole terminal node.

| Node | Kind | What it does |
|---|---|---|
| `RenderDeliverableNode` | Deterministic | Writes `<company-slug>-roadmap.md`: the four-section markdown described by `agentic-portfolio/business/docs/diagnostic/deliverable.md` §2, with every heading/label/currency format chosen from `event.locale`. Refuses (no file written) on an `authored_locale` mismatch. |
| `RenderPdfNode` | Deterministic subprocess | Invokes `typst compile <company-slug>-roadmap.md <company-slug>-roadmap.pdf` over the injectable `CommandRunner` seam. A non-zero exit or missing binary surfaces as a `NodeError` carrying the runner's stderr — never a silent success. |

Neither node currently has a policy-selected branch — `registry_for_policy` builds the same
registry `registry()` does regardless of the resolved policy, deliberately (`CLAUDE.md` standing
rule 6: the node set must stay invariant across all three named profiles). The optional
model-polish knob (below) has no wiring into either node yet; when it is wired, it must land as
an in-place no-op inside `RenderDeliverableNode`, not a conditional rewire of this graph.

## Output filenames: the `<company-slug>` basename

Both artifacts share one basename, `<company-slug>-roadmap.{md,pdf}`, derived by
`schema::deliverable_slug` from `roadmap.situation.company_name` — kebab-case, lowercased,
ASCII-folded (covers the `pt-BR` accented letters: `á/à/â/ã/ä`, `é/è/ê/ë`, `í/ì/î/ï`,
`ó/ò/ô/õ/ö`, `ú/ù/û/ü`, `ç`, `ñ`, and their uppercase forms). "Padaria São João" becomes
`padaria-sao-joao`.

Falls back to the constant `FALLBACK_COMPANY_SLUG` (`"deliverable"`) — never panics — when:
- `roadmap.situation` is absent, or
- the derived slug would otherwise be empty (an all-punctuation or empty `company_name`).

`RenderPdfNode` recomputes this slug itself from the event rather than reading
`RenderDeliverableNode`'s `ctx.nodes` entry, so it stays independently testable and not coupled
to the upstream node's output shape.

## Event schema (`DeliverableRenderEventSchema`)

```json
{
  "roadmap": { "...": "an AutomationRoadmap" },
  "locale": "pt-BR",
  "output_dir": "/tmp/deliverables",
  "policy": null,
  "profile": null
}
```

| Field | Notes |
|---|---|
| `roadmap` | Required. The `AutomationRoadmap` to render, passed inline — this workflow never looks a roadmap up itself. Re-exported from `workflows::proposal_generator::schema`. |
| `locale` | Optional `Locale` (`"pt-BR"` \| `"en-US"`), defaults to `"pt-BR"` when omitted. Must equal `roadmap.authored_locale` — see [Locale is total](#locale-is-total-and-the-authored_locale-refusal). |
| `output_dir` | Required. Directory both `<company-slug>-roadmap.md` and `.pdf` are written under. |
| `policy` | Optional per-run `PartialDeliverableRenderPolicy` override — highest-precedence layer. |
| `profile` | Optional name of a built-in or `harness.json`-defined policy profile bundle. |

## Locale is total, and the `authored_locale` refusal

Every piece of template chrome in the rendered markdown — section headings, table column
headers, tier labels, field labels — and the money format are selected from the run's `locale`
by an internal `Chrome` bundle built once per render. There is exactly one place per language
where wording is authored; a `pt-BR` run and an `en-US` run share zero literal chrome text.

`AutomationRoadmap` carries its own `authored_locale` — the locale its prose was actually
written in, stamped by `PROPOSAL_GENERATOR` (`EN.4.F`). When the event's requested `locale`
disagrees with `roadmap.authored_locale`, `RenderDeliverableNode::process` returns a `NodeError`
naming both locales and **writes no file** — emitting PT chrome over EN prose (or the reverse)
would hand a client a document that reads as broken. `RenderPdfNode` never dispatches in that
case (there is nothing for it to compile), so a mismatch produces zero output files, not a
half-written pair.

Money never converts: `format_money` renders a `crate::locale::MoneyRange` in its own currency
only — there is no cross-currency conversion path anywhere in this workflow.

## The `typst` `CommandRunner` seam

`RenderPdfNode` mirrors `sdlc_flow::end_review::EndReviewNode`'s `with_runner` builder pattern:
the default runner (`default_command_runner()`) shells out to the real `typst` binary, gated by
the non-overridable `crate::policy::command_floor::evaluate_command` org-floor denylist (recursive
`rm`, force push, destructive SQL, mkfs/fork-bomb, pipe-to-shell — a plain `typst compile <in>
<out>` clears all five). Tests substitute a stub via `RenderPdfNode::with_runner(..)` so the
gated `cargo nextest` suite never shells out — confirmed there is no `typst` on the dev host as
of 2026-08-24 (`command -v typst` → not found).

The exact argv is pinned separately in `typst_argv(markdown_path, pdf_path)` so a unit test can
assert the shape without driving the whole node, and so it can be printed verbatim as a
hand-verification command for the one criterion no hermetic test can cover: that a real `typst`
binary actually produces a valid PDF from the golden markdown fixtures (see
`crates/engine-core/tests/fixtures/deliverable_render_{pt_br,en_us}.md` and the NOT-RUN record in
`planning/orchestration-run/autonomous-foundation/notes.md`).

## Policy: the optional model-polish pass

This workflow is largely policy-free — both nodes are deterministic. The one real knob (per
`CLAUDE.md` standing rule 6) is an **optional model-polish pass** over the rendered markdown,
run behind a `with_transport` seam on `RenderDeliverableNode` (not yet wired to any node — see
[Graph shape](#graph-shape)). Resolution follows the same four layers every workflow's `Policy`
surface uses; mechanics: [sdlc-flow-policy.md](sdlc-flow-policy.md).

| Field | Meaning |
|---|---|
| `polish_enabled` | Whether the model-polish pass runs. `false` restores the plain deterministic render exactly. |
| `polish_model_tier` | Model tier the polish pass runs at when enabled. Still resolved (and discoverable) even while the pass is off. |

Built-in default: `polish_enabled: false` — adding this knob does not change what an existing
run produces. Three named profiles, in `harness.json`'s `deliverable_render.profiles` and
`profiles.rs`:

| Profile | `polish_enabled` | `polish_model_tier` |
|---|---|---|
| `baseline` | `false` | `sonnet` |
| `cheap-fast` | `false` | `sonnet` |
| `thorough` | `true` | `sonnet` |

Defaults live in `planning/harness.json`'s `deliverable_render` section, at the same
precedence rank every other workflow's `harness.json` defaults occupy (middle of the four
layers: event `policy` > event `profile` > `harness.json` defaults > built-in default).

## Triggering and reading outputs

Trigger the same way as any other workflow — `POST /events` with `"workflow_type":
"DELIVERABLE_RENDER"` and the event body from [Quickstart](#quickstart) above; dispatch resolves
policy once at dispatch time (`PolicyConfigSource::Builtin` — channel/API-shaped, no repo
checkout) and seeds it into `ctx.nodes` under `policy::RESOLVED_POLICY_IDENTITY`, same as every
other builtin registration (see [architecture.md](../architecture.md) § dispatch).

On success, `ctx.nodes["RenderDeliverableNode"]` and `ctx.nodes["RenderPdfNode"]` each carry
`{ "markdown_path", "pdf_path", "company_slug" }` (`RenderPdfNode`'s copy recomputed
independently, per [Output filenames](#output-filenames-the-company-slug-basename) above) — both
files exist on disk at those paths under `event.output_dir`.

## See also

- [proposal-generator-workflow.md](proposal-generator.md) — produces the
  `AutomationRoadmap` this workflow renders, including the `authored_locale` stamp this
  workflow's refusal checks against.
- [sdlc-flow-policy.md](sdlc-flow-policy.md) — the shared `Policy` framework (four-layer
  resolution, profile bundles) this workflow's policy surface is built on.
- [architecture.md](../architecture.md) — dispatch, `register_builtin_workflows`, and the
  `PolicyConfigSource::Builtin` vs. `Worktree` split.
- [testing.md](../testing.md) — the single-integration-test-binary layout this workflow's e2e
  suite (`tests/it/deliverable_render_e2e.rs`) follows.
