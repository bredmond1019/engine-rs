---
type: Reference
title: Opportunity Edit Workflows
description: The OPPORTUNITY_SET_STAGE and OPPORTUNITY_ADD_ACTION micro-workflows — event payloads, the OpportunityEdit seam operation, OpportunityEditNode, mev's stage vocabulary, idempotency, and the error surface
doc_id: opportunity-edit-workflows
layer: [engine, factory]
project: engine-rs
status: active
keywords: [opportunity-edit, set-stage, add-action, opportunity-edit-node, doc-materializer, mev, idempotency, E_DOC_BAD_STAGE, ENGINE_BRAIN_ROOT, D53]
related: [materialize-doc-node, research-agent-workflow, architecture, data-contract, D53-engine-executes-mev-writes-brain-docs, harvest-gate]
---

# Opportunity Edit Workflows

`EN.7.B` adds two single-node micro-workflows that let an outside caller edit an existing
Opportunity document already written into the Brain corpus: **`OPPORTUNITY_SET_STAGE`** (change
an opportunity's pipeline `stage`) and **`OPPORTUNITY_ADD_ACTION`** (append one action-log entry
to an opportunity's `actions[]`). Both are the write half of the edits `BW.7.A`'s bastion-web UI
will trigger — this block builds the `POST /events/` entry points, not the browser-facing UI.

Source: `crates/engine-core/src/nodes/opportunity_edit.rs` (the node),
`crates/engine-core/src/workflows/opportunity_edit/` (`mod.rs`, `schema.rs`, `graph.rs` — the
two declared graphs), registered from `crates/engine-serve/src/workflows.rs`
(`register_opportunity_set_stage` / `register_opportunity_add_action` →
`register_builtin_workflows`).

## The `OpportunityEdit` seam operation

`crates/engine-core/src/nodes/doc_materializer.rs`'s `DocMaterializer` trait — the same
injectable seam `MaterializeDocNode` uses (see
[materialize-doc-node.md](materialize-doc-node.md)) — gains a second method:

```rust
async fn edit_opportunity(
    &self,
    root: &Path,
    edit: &OpportunityEdit,
    write: bool,
) -> Result<MaterializeOutcome, String>;
```

`OpportunityEdit` has exactly two variants — `merge-contacts` is deliberately not exposed yet;
its natural driver is contact enrichment (`EN.4.E`), which is not shipped:

```rust
pub enum OpportunityEdit {
    SetStage { slug: String, stage: String },
    AddAction { slug: String, at: String, kind: String, note: String },
}
```

`MevDocMaterializer` (the live implementation) dispatches `SetStage` to
`mev::doc::opportunity::plan_set_stage(slug, stage, root)` and `AddAction` to
`mev::doc::opportunity::plan_add_action(slug, at, kind, note, root)`, then applies the resulting
plan via `mev::apply_plan` inside `tokio::task::spawn_blocking` — the same plan-then-apply,
diagnostics-mapped shape `materialize` uses, returning the same engine-owned `MaterializeOutcome`
type (no new result type). `StubDocMaterializer` records edit calls via `last_edit()` /
`RecordedEditCall { root, edit, write }`, mirroring `last_call()` for `materialize`.

## Valid stages

`plan_set_stage` validates `stage` against mev's own vocabulary,
`mev::doc::opportunity::VALID_STAGES` (`core/mev/src/doc/opportunity.rs`) — this repo does not
fork that list into a Rust enum or duplicate it in prose; read the const at the source of truth.
An unrecognized `stage` string raises mev's `E_DOC_BAD_STAGE` diagnostic naming the valid stages,
and plans nothing.

## `OpportunityEditNode`

Source: `crates/engine-core/src/nodes/opportunity_edit.rs`. Lives under `nodes/`, not under a
`workflows::*` module, for the same reason `MaterializeDocNode` does — it is generic machinery.

```rust
OpportunityEditNode::new(op: OpportunityEditOp)   // OpportunityEditOp::SetStage | AddAction
    .with_materializer(materializer: Arc<dyn DocMaterializer>)
    .with_brain_root(root: impl Into<PathBuf>)
    .with_write(write: bool)
```

- The node is configured with **which** edit it performs (`op` is node configuration); it reads
  the edit's **arguments** off `ctx.event` at run time, using the same field-by-field reads as
  `workflows::opportunity_edit::schema`'s two event types (`SetOpportunityStageEvent` /
  `AddOpportunityActionEvent` — the single source of truth for each event's field list).
- `new(op)` defaults to the live seam (`doc_materializer_live()`), `write = true`, and no explicit
  brain root (resolved at run time via `crate::brain_root::resolve_brain_root()` — see
  [materialize-doc-node.md § Brain-root resolution](materialize-doc-node.md#brain-root-resolution-engine_brain_root)
  for the `ENGINE_BRAIN_ROOT` precedence).
- A missing or ill-typed `slug` / `stage` / `at` / `kind` / `note` field is a `NodeError` naming
  that field.
- A seam `Err`, or any error-severity diagnostic in an otherwise-successful `MaterializeOutcome`
  (this is how `E_DOC_BAD_STAGE` and an unknown slug become run failures), maps to a `NodeError`
  naming the node and the underlying mev message.
- Result stamp lands under `self.name()` (not the bare `NODE_NAME` const), so
  `NodeExt::with_identity` composes and the two configured instances below can coexist in one
  process:

```json
{
  "edited": true,
  "dry_run": false,
  "op": "set-stage",
  "slug": "acme-corp",
  "paths": ["/abs/path/to/business/docs/opportunities/acme-corp.md"],
  "warnings": [],
  "no_op": false
}
```

`no_op: true` (with `edited: false`) is the successful, zero-diagnostic shape of an idempotent
repeat — see Idempotency below.

## The two workflow types

Both are deterministic, model-free, single-node graphs — no policy module, no profiles module,
no `harness.json` section (`OpportunityEditNode` calls no model, so there is no `ModelTier` to
resolve). Each identity wraps a distinctly-configured `OpportunityEditNode` via
`NodeExt::with_identity`, and each graph is both its own start node and its own terminal node
(mirrors `diagnostic_intake`'s single-node shape).

| Workflow type | Node identity | Event schema |
|---|---|---|
| `OPPORTUNITY_SET_STAGE` | `SetOpportunityStageNode` | `SetOpportunityStageEvent { slug, stage }` |
| `OPPORTUNITY_ADD_ACTION` | `AddOpportunityActionNode` | `AddOpportunityActionEvent { slug, at, kind, note }` |

### `POST /events/` payloads

```
POST /events/
X-API-Key: <BASTION_ENGINE_API_KEY>
Content-Type: application/json

{ "workflow_type": "OPPORTUNITY_SET_STAGE", "data": { "slug": "acme-corp", "stage": "contacted" } }
```

```
POST /events/
X-API-Key: <BASTION_ENGINE_API_KEY>
Content-Type: application/json

{
  "workflow_type": "OPPORTUNITY_ADD_ACTION",
  "data": { "slug": "acme-corp", "at": "2026-07-27", "kind": "email", "note": "Sent intro email" }
}
```

Same HTTP surface as every other `engine-serve` workflow (`docs/cli.md`; see
[sdlc-flow-workflow.md § How to trigger a run](sdlc-flow-workflow.md#how-to-trigger-a-run) for the
full auth/mounting story). `crates/engine-serve/src/workflows.rs`'s
`register_opportunity_set_stage` / `register_opportunity_add_action` are the first
`register_builtin_workflows` entries whose `WorkflowFactory` resolves no policy and seeds no
policy stamp — say so explicitly in each fn's doc comment so a future reader does not "restore"
the missing policy hop.

## Idempotency

Both edit operations are idempotent no-ops on a repeat with identical arguments — this is mev's
own planner behaviour, not something `OpportunityEditNode` layers on top:

- **Set-stage**: re-running with the same `stage` the document already has plans zero actions.
  The file's bytes are unchanged, and the node's result stamps `no_op: true`, `edited: false` —
  it does **not** error.
- **Add-action**: an identical `{at, kind, note}` triple already present in `actions[]` is not
  re-appended. A repeat plans zero actions and the file's bytes are unchanged.

## Error surface

| Condition | Outcome |
|---|---|
| Unknown/invalid `stage` (outside `VALID_STAGES`) | `NodeError` naming the valid stages (mev's `E_DOC_BAD_STAGE`); nothing is written. |
| Unknown `slug` (no opportunity file for it) | `NodeError`; mev's planners load the existing doc first, so an unknown slug is an error-severity diagnostic, not a newly-created file. |
| Missing/ill-typed event field | `NodeError` naming the field, raised before the seam is ever called. |
| Seam-level failure (join/IO error) | `NodeError` naming the node and the underlying message. |

## `ENGINE_BRAIN_ROOT` requirement

Neither workflow accepts a brain-root override in its event — the node resolves the corpus root
via `crate::brain_root::resolve_brain_root()` exactly like `MaterializeDocNode`: the
`ENGINE_BRAIN_ROOT` env var when set, otherwise walking up from the process cwd for a
`brain.toml`. A served run with no resolvable brain root fails loudly with a `NodeError` — this is
intentional, the same "a run now fails loudly rather than silently no-opping" posture
`research-agent-workflow.md` documents for `RESEARCH_AGENT`'s own terminal write.

## Out of scope

- **`merge-contacts`** — mev's `plan_merge_contacts` exists but its driver is contact enrichment
  (`EN.4.E`), which is not shipped. Adding a third `OpportunityEdit` variant later is a small
  additive change, not a redesign.
- The bastion-web trigger UI / BFF that will actually POST these payloads — `BW.7.A`.

## See also — the pattern this block's micro-workflows established

`EN.7.C`'s `HARVEST_APPROVE` micro-workflow (the human-approval completion hop for a deferred
harvest) copies this block's single-node, both-start-and-terminal, no-router shape verbatim: no
`policy` module, no `profiles` module, no `harness.json` section — `HarvestApproveNode`, like
`OpportunityEditNode`, calls no model and reads no policy layer, so `register_harvest_approve`
resolves no policy and seeds no policy stamp, exactly like `register_opportunity_set_stage` /
`register_opportunity_add_action` above. See [harvest-gate.md](harvest-gate.md) for the full
gate and the `HARVEST_APPROVE` hand-off.
