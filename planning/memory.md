---
type: Reference
title: engine-rs Memory
description: Repo-scoped durable memory for engine-rs — episodic notes, preferences, superseded facts. Committed and portable.
doc_id: memory
layer: [factory]
status: active
keywords: [memory, episodic, preferences, durable, portable]
related: [knowledge, context, status, planning-index]
---

# Memory — engine-rs

Repo-scoped **durable memory**: episodic notes, operator preferences, and superseded facts that
must survive a handoff and travel with the repo. Committed and portable — distinct from the global
`~/.claude/.../memory/` auto-memory (which is operator-level and stays on one machine).

Use this for project facts worth remembering across sessions. Promote durable "how it works"
knowledge to `knowledge.md`; promote settled choices to `decisions/`. Do not duplicate the global
auto-memory here.

## Notes

_Dated episodic entries — what was tried, what was decided in-flight, what to remember next time._

## Preferences

_Project-specific preferences (tooling, style, workflow) the operator has expressed._

---

*Episodic + portable. For durable "how it works" knowledge see `knowledge.md`.*
