---
type: Reference
title: MaterializeDocNode and the DocMaterializer Seam
description: How the generic MaterializeDocNode writer node and its injectable DocMaterializer seam let any engine-rs workflow write a BrainDocModel-shaped artifact into the Brain corpus as a source .md document via mev/okf-core in-process
doc_id: materialize-doc-node
layer: [engine, factory]
project: engine-rs
status: active
keywords: [materialize-doc-node, doc-materializer, mev, okf-core, brain-root, opportunity, learning-artifact, proposal, dry-run, D53]
related: [architecture, docs-index, content-pipeline-workflow, proposal-generator-workflow, D53-engine-executes-mev-writes-brain-docs]
---

# MaterializeDocNode and the `DocMaterializer` Seam

`EN.7.A` adds a generic, reusable `Node` — `MaterializeDocNode` — plus its injectable
`DocMaterializer` seam, to `engine-core`. Together they let any workflow write a
`BrainDocModel`-shaped artifact (an opportunity, a learning artifact, a proposal) out of an
upstream node's `TaskContext` into the Brain corpus as a source `.md` document, calling
`mev`/`okf-core` in-process.

## Why this node exists (D53's fourth boundary-test channel)

`CLAUDE.md`'s THE BOUNDARY TEST names four channels for scoping new work between Synapse (the
Brain), engine-rs (the Engine), and mev/okf-core (Factory/Doc):

```
4. Does it serialize or write a repo-tracked source document
   (.md with OKF frontmatter)?                              YES -> mev / okf-core (via engine-rs)
```

Per `docs/decisions/D53-engine-executes-mev-writes-brain-docs.md`, **engine-rs executes, mev
writes**: this repo owns workflow orchestration and calls mev's document planners/writers
in-process rather than shelling out or duplicating mev's OKF-frontmatter logic. `mev` produces
the plan and applies it to disk; `MaterializeDocNode` is the workflow-facing wrapper that gets a
`BrainDocModel`-shaped payload from a graph node to mev's writer.

This is deliberately **not** a Synapse write. Synapse still owns the derived index — embeddings,
`brain_edges`, retrieval — over whatever `MaterializeDocNode` writes to the repo-tracked corpus.
Nothing in this node or seam touches pgvector, an embedding model, or Synapse's ingest endpoint;
see `crates/engine-core/src/workflows/content_pipeline/persist_to_brain.rs` and
`crates/engine-core/src/nodes/http_post.rs` for that separate, existing HTTP-POST-to-Synapse
boundary (`EN.4.C`), which this node does not use or replace.

**Not yet wired into any workflow.** `MaterializeDocNode` is generic and unattached — no graph,
registry, or `WorkflowSchema` references it yet. Wiring concrete instances into the
`RESEARCH_AGENT` terminal step and the `set-stage`/`add-action` micro-workflows is `EN.7.B`.

## The `DocMaterializer` seam

Source: `crates/engine-core/src/nodes/doc_materializer.rs`.

Mirrors the shape of `crate::nodes::http_post`'s `HttpPost` seam and `crate::nodes::
channel_transport`'s `ChannelTransport` seam: a trait, a live implementation backed by the real
dependency, and a recording test stub — so production code reaches for the real writer while the
gated `cargo test` suite injects a stub and never touches the filesystem unless a test
deliberately drives the live impl against its own `tempfile::tempdir()`.

```rust
#[async_trait::async_trait]
pub trait DocMaterializer: Send + Sync {
    async fn materialize(
        &self,
        root: &Path,
        model: &str,
        input: &Value,
        write: bool,
    ) -> Result<MaterializeOutcome, String>;
}
```

- **`MevDocMaterializer`** (live) — dispatches `model` to the matching mev planner (`plan_ingest`
  for `"opportunity"`, `plan_document` over an `okf_core::LearningArtifact`/`okf_core::Proposal`
  for `"learning-artifact"`/`"proposal"`), builds the `EmitPlan` first so the target paths are
  known before anything is applied, then calls `mev::brain::emit::apply_plan(&plan, write)`. mev's
  planners and `apply_plan` are synchronous filesystem work, so the live impl runs them inside
  `tokio::task::spawn_blocking` and awaits the join handle — the same pattern `EN.6.A`'s
  `channel_transport` used for non-`Send` work. `doc_materializer_live()` returns an
  `Arc<dyn DocMaterializer>` wrapping it.
- **`StubDocMaterializer`** (test) — records the last `(root, model, input, write)` call
  (`last_call()`) and returns a configurable `Ok(MaterializeOutcome)` / `Err(String)`, same
  `Arc<Mutex<..>>` interior-mutability shape as `StubHttpPost`.

No `mev`/`okf-core` type appears in the seam's public signature. Engine-owned result types stand
in for mev's:

- `MaterializedFile { path, note }` — one file a call planned (or wrote); mirrors
  `mev::brain::emit::EmitAction`'s `path`/`note` without exposing the mev type.
- `MaterializeDiagnostic { severity, file, code, message }` — mapped from `mev::Diagnostic`;
  `code` carries mev's `Diagnostic.locator` field (e.g. `I_EMIT_WROTE`, `W_EMIT_DRY_RUN`,
  `E_EMIT_WRITE_FAILED`, `E_DOC_UNKNOWN_MODEL`).
- `MaterializeOutcome { wrote, planned, diagnostics }` with `errors()`/`warnings()` helpers
  filtering diagnostics by severity.

### Supported `model` values

`MevDocMaterializer` dispatches on exactly three `model` strings — anything else is an `Err`
naming this list, never a panic:

| `model` | Planner |
|---|---|
| `"opportunity"` | `mev::doc::opportunity::plan_ingest` (kind auto-detected from the payload shape) |
| `"learning-artifact"` | `mev::doc::plan_document` over an `okf_core::LearningArtifact::from_payload`-built model |
| `"proposal"` | `mev::doc::plan_document` over an `okf_core::Proposal::from_automation_roadmap` model, reading `company_name`/`roadmap` off the input payload |

This dispatch is a deliberate, documented ~20-line duplicate of `mev::doc_materialize`'s own
dispatch (see the module doc in `doc_materializer.rs`) — planning first, rather than calling
`mev::doc_materialize` directly, is what exposes the target paths before `apply_plan` runs.

## Brain-root resolution (`ENGINE_BRAIN_ROOT`)

Source: `crates/engine-core/src/brain_root.rs` (`EN.7.A` task 2).

`resolve_brain_root() -> Result<PathBuf, BrainRootError>` resolves the target corpus root with
this precedence:

1. The `ENGINE_BRAIN_ROOT` env var, when set and non-empty — following the repo's existing
   env-var convention (`ENGINE_EVENTS_URL`, `ENGINE_RUN_MAX_COST_USD`, `ENGINE_RUN_MAX_TOKENS`).
2. `mev::brain::config::find_brain_root(&std::env::current_dir()?)`, walking up from the process
   cwd for a `brain.toml`.

Every failure path (an `ENGINE_BRAIN_ROOT` pointing at a path that does not exist or is not a
directory, no `brain.toml` found walking up, an unreadable cwd) returns a typed `BrainRootError`
— never a panic, never a silent `.` default. `resolve_brain_root_from(start: &Path)` resolves from
an explicit directory without touching process-global env state, for tests and callers that
already know their root.

## `MaterializeDocNode`

Source: `crates/engine-core/src/nodes/materialize_doc.rs`.

Lives under `nodes/`, not under a `workflows::*` module, because it is generic — every future
pipeline appends it rather than each pipeline growing its own writer node. Modeled on
`persist_to_brain::PersistToBrainNode`'s builder-seam shape: a `NODE_NAME` const
(`"MaterializeDocNode"`), `put_result`/`get_result` for `ctx.nodes` access, and node-name-prefixed
`NodeError` messages.

### Constructor and builders

```rust
MaterializeDocNode::new(model: impl Into<String>)
    .with_materializer(materializer: Arc<dyn DocMaterializer>)
    .with_brain_root(root: impl Into<PathBuf>)
    .with_source_node(upstream: impl Into<String>)
    .with_write(write: bool)
```

- `new(model)` defaults to the live seam (`doc_materializer_live()`), `write = true`, no explicit
  brain root (falls through to `resolve_brain_root()`), and no explicit source node (reads
  `ctx.event` directly).
- `with_materializer` swaps in a `StubDocMaterializer` for tests.
- `with_brain_root` pins the corpus root explicitly, bypassing `resolve_brain_root` — tests point
  this at a `tempfile::tempdir()`.
- `with_source_node(upstream)` reads the input artifact from `upstream`'s stored `ctx.nodes` entry
  instead of `ctx.event`. A configured-but-absent upstream is a `NodeError` naming the missing
  identity (`"{node}: no artifact stored by {upstream}"`), matching `persist_to_brain`'s message
  style.
- `with_write(false)` is dry-run: nothing is written to disk, but the result stamp still names the
  path(s) that would have been written and reports `dry_run: true`.

### Errors

`process()` maps every failure mode into a `NodeError` naming the node — never a panic:

- brain-root resolution failure (`resolve_brain_root`'s `BrainRootError`, with a hint to set
  `ENGINE_BRAIN_ROOT`);
- a missing configured source node;
- the seam returning `Err(String)` (including an unknown `model` value, which names the three
  valid models);
- any error-severity diagnostic in an otherwise-successful `MaterializeOutcome`.

### Result stamp

`process()` stamps its result under `ctx.nodes[self.name()]` — deliberately `self.name()` rather
than the bare `NODE_NAME` const, so a node wrapped in `EN.5.E`'s `NodeExt::with_identity` lands
its result under its own override identity and two `MaterializeDocNode` instances can coexist in
one graph without colliding (what `EN.7.B` needs when it wires several instances into one
workflow):

```json
{
  "materialized": true,
  "dry_run": false,
  "model": "opportunity",
  "paths": ["/abs/path/to/business/docs/opportunities/acme-corp.md"],
  "warnings": []
}
```

## Not yet wired into any workflow

`MaterializeDocNode` is a standalone `Node` implementation. No `WorkflowSchema`, node registry, or
graph references it as of `EN.7.A`. Wiring concrete instances into `RESEARCH_AGENT`'s terminal
step and the `set-stage`/`add-action` micro-workflows is `EN.7.B` — see
`agentic-portfolio/core/planning/mev-write-loop-master-plan.md` § Phase 2.
