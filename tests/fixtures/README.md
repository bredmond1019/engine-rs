# `task_context` fixture — provenance

`research_agent_task_context.json` is a **checked-in copy** of a real, code-path-captured
`task_context` value. The orchestrator repo owns and captures it
(`orchestrator/scripts/emit_task_context_fixture.py`, output of an actual `ResearchAgentWorkflow`
run); this crate keeps a copy so it stays clonable and testable standalone, without a hard
cross-repo path dependency.

> **Why this replaced `python_task_context.json`.** That file was hand-authored by this crate
> during EN.0.B — hand-typed UUID, round-number timestamps, and a `ticket_id`/`title` naming EN.0.B
> itself (the block that created it). `round_trip.rs` therefore proved this crate self-consistent,
> never that it matched the orchestrator. See
> `orchestrator/planning/task-context-fixture/notes.md` for the full finding and
> `orchestrator/tests/fixtures/task_context/README.md` for the emission-side provenance record
> (what was redacted, how to re-emit).

## Divergence check

`crates/engine-contract/tests/round_trip.rs::fixture_matches_orchestrator_owned_original_when_sibling_checkout_present`
compares this copy against `../orchestrator/tests/fixtures/task_context/research_agent_task_context.json`
(a sibling checkout under the same parent directory — the common layout for this practice's repos)
byte-for-byte, and skips silently if no sibling checkout is present. If it fails, the copy here is
stale — recopy it:

```bash
cp ../orchestrator/tests/fixtures/task_context/research_agent_task_context.json \
   tests/fixtures/research_agent_task_context.json
```

Then re-run `cargo test -p engine-contract` and confirm `fixture_round_trips_with_no_field_or_casing_drift`
still passes — if it doesn't, the orchestrator's `task_context` shape changed and this crate's
`TaskContext`/`NodeRun`/`Usage` types (`src/task_context.rs`) need updating to match, not the other
way around. The orchestrator owns this contract (D30); this crate is the consumer.
