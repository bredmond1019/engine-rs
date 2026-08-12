---
type: Reference
title: The Materialize -> Harvest Gate
description: HarvestMode/HarvestGate — the configurable off/in_process/approval gate fronting PersistToBrainNode's Synapse ingest POST, its four-layer resolution, the pending-harvest record, and the HARVEST_APPROVE hand-off
doc_id: harvest-gate
layer: [engine]
project: engine-rs
status: active
keywords: [harvest-gate, harvest-mode, persist-to-brain, harvest-approve, synapse-ingest, pending-harvest, D51, materialize-doc-node, boundary-test, operator-channel]
related: [architecture, docs-index, content-pipeline-workflow, materialize-doc-node, opportunity-edit-workflows, operator-payload-contract, brain:D51-brain-engine-boundary-and-synapse, brain:D53-engine-executes-mev-writes-brain-docs]
---

# The Materialize -> Harvest Gate

`EN.7.C` adds a generic, reusable materialize-\>harvest gate — `HarvestMode` / `HarvestGate`
(`crates/engine-core/src/nodes/harvest_gate.rs`) — that any pipeline terminating in a Synapse
ingest push can inherit. It sits in front of the ONE existing ingest POST
(`content_pipeline::persist_to_brain::PersistToBrainNode`), turning an unconditional push into a
policy-governed one, without adding a second route to the index.

## Operator channel (`EN.8.A`)

`HarvestGate` also carries a declared `OperatorChannel` (`crates/engine-core/src/operator/channel.rs`)
— `notification` (default) or `session-<slug>`, set via `HarvestGate::with_channel(...)` and read
via `HarvestGate::channel()`, readable off the gate definition without executing the workflow. This
is the same "declared at gate-definition time, never discovered or degraded at emit time" pattern
the gate already applies to `HarvestMode`. See
[operator-payload-contract.md](operator-payload-contract.md) for the full `OperatorPayload`/
`ValidatedOperatorPayload`/`OperatorChannel` contract this wires into.

## The three modes

`HarvestMode` has exactly three snake_case-serialized variants:

| Mode | Wire value | Behavior |
|---|---|---|
| Off (default) | `"off"` | No ingest POST. Indexing is left to the existing manifest / `index_brain` freshness reindex. This is **not** "no indexing" — it is "indexing via the standing path" rather than an explicit push. |
| In-process | `"in_process"` | POST synchronously, in-run, immediately after the artifact is built — the pre-`EN.7.C` behavior, for instant/curated indexing. |
| Approval | `"approval"` | No POST in-run. A `pending` record is stamped instead, for an operator to complete later via the `HARVEST_APPROVE` micro-workflow. |

**Why the default is `off`.** Before this block, `CONTENT_PIPELINE` POSTed to Synapse
unconditionally on every run. The operator's direction and the block's prose agree that this was
too eager: the corpus already has a freshness reindex (mev's manifest / `index_brain`) that picks
up every materialized `.md`, so an explicit, synchronous ingest POST should be reserved for cases
that need instant or curated indexing, not fired on every run by default. This is a deliberate
behavior change, not a side effect smuggled in under CLAUDE.md rule 6's usual
behavior-stability requirement — it is called out loudly here, in `planning/harness.json`'s
`_harvest_comment`, and in the migration note below.

**Migration note — restoring the old always-push behavior.** A deployment that wants the
pre-`EN.7.C` behavior (push on every run) sets
`content_pipeline.policy.harvest.mode = "in_process"` in `planning/harness.json`, or triggers runs
with `"profile": "curated-harvest"` — the one built-in named profile that resolves `in_process`.

## Resolution — four layers

`harvest` resolves through the same four layers as every other `ContentPipelinePolicy` knob
(`crates/engine-core/src/workflows/content_pipeline/policy.rs`), highest precedence first:

1. Per-run event `policy.harvest` override
2. Named `profile:` bundle (`profiles.rs::profile_by_name`)
3. `planning/harness.json`'s `content_pipeline.policy.harvest` defaults
4. Built-in default (`HarvestConfig::default()` -\> `HarvestMode::Off`)

Every named profile states `harvest` explicitly — a knob absent from the profile bundles is a
knob nobody will find (CLAUDE.md rule 6):

| Profile | `harvest.mode` |
|---|---|
| `baseline` | `off` |
| `local-drafting` | `off` |
| `fast-summarize` | `off` |
| `curated-harvest` | `in_process` |

`curated-harvest` is the new profile this block adds: the one built-in bundle that opts into an
explicit, synchronous ingest push.

## Shape invariance

The declared `CONTENT_PIPELINE` node set (`schema()`, sixteen nodes) is identical across every
harvest mode — the gate changes what `PersistToBrainNode` *does*, never what nodes the graph
declares. `WorkflowValidator::validate` passes for every mode, and `registry_for_policy`
re-registers the same node identity with different configuration, never a different node set.

`PersistToBrainNode::process` stamps **one stable key set** in all three modes:

```json
{
  "posted": false,
  "skipped": true,
  "harvest_mode": "off",
  "status": null,
  "artifact_id": "...",
  "response": null,
  "pending": null
}
```

`status`/`response`/`pending` are `null` where they do not apply to the resolved mode. Every
result carries `harvest_mode` so `RunTelemetry`/`PolicyAggregate` can attribute observed cost to
the setting that caused it (CLAUDE.md rule 6). The payload itself
(`super::learning_artifact::build_learning_artifact_payload`) is always built first and
unconditionally, before the mode branch — payload derivation never forks by mode, which is what
keeps a deferred (`approval`) push byte-identical to what an `in_process` push would have sent.

## The pending-harvest record and the `HARVEST_APPROVE` hand-off

Under `HarvestMode::Approval`, `PersistToBrainNode` makes no POST. Instead it calls
`crate::nodes::harvest_gate::pending_harvest_record` — the single constructor for the
pending-record shape, shared by the node that defers a harvest (`PersistToBrainNode`) and the
node that completes one (`HarvestApproveNode`) so the two can never drift:

```json
{
  "artifact_id": "artifact-1",
  "url": "http://localhost:8000/ingest/learning",
  "payload": { "...": "the exact body an in_process push would have POSTed" },
  "doc_paths": ["brain/content/learning/artifact-1.md"]
}
```

- `artifact_id` — the harvest this record is for.
- `url` — the same Synapse ingest endpoint an `in_process` push targets.
- `payload` — the verbatim `LearningArtifact` JSON body `build_learning_artifact_payload`
  produced; stored as-is, never re-derived or re-validated at approval time.
- `doc_paths` — the materialized `.md` path(s) written upstream by `MaterializeDocNode`, read
  from its `paths` result field for operator visibility. An absent/skipped upstream materialize
  stamp yields an empty list, not an error.

**How an operator observes and completes a pending harvest.** The `pending` record lands on
`ctx.nodes["PersistToBrainNode"].pending`, which is exactly what `EN.5.F`'s run readback
(`GET /events/{event_id}`, or the SSE stream) surfaces to a caller polling or watching that run —
there is no separate pending-harvest queue or table (per the block's non-goals: no new
engine-side persistence for this). To complete the harvest, an operator feeds that same record
back in as the triggering event for `HARVEST_APPROVE`:

```
POST /events/
X-API-Key: <BASTION_ENGINE_API_KEY>
Content-Type: application/json

{
  "workflow_type": "HARVEST_APPROVE",
  "data": {
    "artifact_id": "artifact-1",
    "url": "http://localhost:8000/ingest/learning",
    "payload": { "...": "..." },
    "doc_paths": ["brain/content/learning/artifact-1.md"]
  }
}
```

`HarvestApproveNode` (`crates/engine-core/src/nodes/harvest_approve.rs`) reads `artifact_id`,
`url`, and `payload` off `ctx.event` and POSTs `payload` to `url` over the same injectable
`HttpPost` seam `PersistToBrainNode` uses — the eventual push is byte-identical to what the
`in_process` path would have sent at materialize time, which is the no-double-write-inconsistency
guarantee between the written `.md` and the index. A malformed/absent pending record (missing
`artifact_id`/`url`/`payload`) is a `NodeError` naming the missing field; a non-2xx/transport
failure from the push is a `NodeError` too — an *attempted* harvest, whether in-process or
approved, is never a silent drop.

`HARVEST_APPROVE` is a single-node, model-free micro-workflow — both start and terminal node, no
router, no `policy`/`profiles` module, no `harness.json` section — the same pattern
`OPPORTUNITY_SET_STAGE`/`OPPORTUNITY_ADD_ACTION` use (see
[opportunity-edit-workflows.md](opportunity-edit-workflows.md)). It is registered by
`register_harvest_approve` (`crates/engine-serve/src/workflows.rs`), which
`register_builtin_workflows` calls alongside every other builtin.

## No second indexing path

Per THE BOUNDARY TEST (`CLAUDE.md`, D51/D53): there is exactly one index and Synapse owns it. This
gate adds a *gate* in front of the one existing ingest POST — it does not add another route to the
index. Concretely:

- `HarvestApproveNode` POSTs to the *same* `url` the deferred record carries, which is the *same*
  endpoint an `in_process` push would have targeted.
- The payload is derived exactly once, by `build_learning_artifact_payload`, regardless of mode.
- No engine-side persistence table or queue is added for pending harvests — a pending harvest is
  observable purely through `EN.5.F`'s existing run readback and completed by re-POSTing it.
- Nothing in this block touches mev's manifest / `index_brain` freshness reindex or Synapse's
  ingest internals.
- Per D51, engine-rs only POSTs — no embedding, no `pgvector`, no corpus-index write happens in
  this repo, in any harvest mode. What happens behind the ingest endpoint is entirely Synapse's
  concern.

## See also

- [content-pipeline-workflow.md](content-pipeline-workflow.md) — the `CONTENT_PIPELINE` graph
  `PersistToBrainNode` terminates (as of `EN.6.A`, `ActionDispatchNode` runs after it), and the
  `LearningArtifact` payload shape in full.
- [materialize-doc-node.md](materialize-doc-node.md) — the upstream `MaterializeDocNode` write
  that always happens identically regardless of harvest mode, and the `DocMaterializer` seam
  pattern `HarvestApproveNode`'s model-free, single-node shape mirrors.
- [opportunity-edit-workflows.md](opportunity-edit-workflows.md) — the micro-workflow pattern
  (`OPPORTUNITY_SET_STAGE`/`OPPORTUNITY_ADD_ACTION`) `HARVEST_APPROVE` copies: no policy module,
  no profiles module, no `harness.json` section.
- [architecture.md](architecture.md#injectable-seams) — the `http_post.rs` seam row, now noting
  the harvest gate, and the materialize-\>harvest ordering guarantee.
- [operator-payload-contract.md](operator-payload-contract.md) — the `OperatorPayload`/
  `ValidatedOperatorPayload`/`OperatorChannel` contract (`EN.8.A`) this gate's `channel` field
  declares.
