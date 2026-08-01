---
type: Index
title: crates/engine-core/planning Archive Index
description: Registry of archived, cold planning folders that used this crate-local planning root.
doc_id: archive-index
layer: [engine]
project: engine-rs
status: archived
keywords: [archive, index, engine-core, crate-local-planning]
related: [knowledge, memory]
---

# Archive Index — crates/engine-core/planning

Retired planning folders from this crate-local planning root, residue distilled into
`../knowledge.md`/`../memory.md` before the move (D35 gate). See `../memory.md` for why this
crate-local planning root exists at all.

| Folder | What it was | Status |
|---|---|---|
| `EN.5.D-policy-dispatch-seam/` | Policy/telemetry productionization — policy-aware `WorkflowFactory`, resolve-once, shared `Overlay` merge surface, transport-stamped model-tier telemetry | Complete — residue distilled 2026-08-01 |
| `EN.5.F-async-run-lifecycle/` | Async run lifecycle — non-blocking `POST /events/`, `GET /events/{event_id}` readback, SSE progress stream, bounded terminal-run retention | Complete — residue distilled 2026-08-01 |
| `EN.6.A-egress-dispatch/` | Egress seam — `ChannelTransport` + `ActionDispatchNode` + `WorkflowTriggerDispatch` fire-and-forget chain dispatch | Complete — residue distilled 2026-08-01 |
