---
name: epic
description: >
  Park, resume, reconcile and close an initiative (epic) in the HQ epics[] registry via mev's
  defer-epic / resume-epic / sync-epics / complete-epic verbs — dry-run-by-default semantics,
  the two lock-contention errors and how to respond to each, and why sync-epics never un-defers.
  Use before pausing or closing a multi-block initiative, before declaring one complete, or when
  a board shows an epic's status disagreeing with its member blocks' statuses.
allowed-tools: Bash(mev:*) Bash(cd:*)
---

# epic — park, resume, reconcile, close

`epics[]` is an **HQ-only** registry (brain-kind `state.json` — the HQ root or a tier sub-brain;
never a project-kind leaf repo's own `state.json`). These four verbs are the only sanctioned way to
change an epic's registry `status` or cascade that change onto its member blocks — never hand-edit
`epics[]` for this (see `edit-state-json` for hand-editing anything else in `state.json`).

## The four verbs, in one line each

| Verb | Does | Inverse of |
|---|---|---|
| `mev defer-epic <slug>` | Park: registry `status -> paused`, cascades `deferred` onto every **open** member block | `resume-epic` |
| `mev resume-epic <slug>` | Un-park: registry `status -> active`, returns every **deferred** member back to `open` | `defer-epic` |
| `mev sync-epics` | Reconcile **every** epic's registry status against its blocks, both directions, fleet-wide (no slug argument) | itself (idempotent) |
| `mev complete-epic <slug>` | Declare finished: registry `status -> complete`. Terminal, drops the epic off the board | nothing — one-way |

## Before you run any of them

- **All four are dry-run by default.** Nothing changes until you add `--write`. Run without it
  first and read the plan — this is not optional ceremony, it is how you catch "wrong slug" before
  it mutates anything.
- **`--write` also re-runs `emit-state --write`** in the same invocation, so `focus` and the boards
  regenerate together with the epic change — never a two-step "flip the field, then remember to
  derive" dance. Read the `derive-state-safely` skill before any `--write` call: it is a
  whole-corpus derived-surface rewrite, not a one-field poke, exactly like every other `mev`
  write verb.
- **`in_progress` blocks are never touched.** `defer-epic` deliberately skips them and reports
  `W_EPIC_SKIPPED_IN_PROGRESS` — parking work you're mid-block on is far more likely a mistake
  than an intent. If you actually mean to park mid-flight work, finish or explicitly close the
  block first; there is no flag to force it.
- **`sync-epics` never un-defers.** An `active` epic with *some* `deferred` blocks is a completely
  normal state (a partial pause via individual block edits, not the whole epic) — `sync-epics`
  only tightens the two disagreement cases (all-deferred-but-still-active -> `paused`;
  `paused`-but-has-open-members -> defer the open ones), never the reverse. Un-parking is always
  explicit, via `resume-epic`.
- **`complete-epic` is a judgement call, never inferred.** `mev` will not auto-flip an epic to
  `complete` when its last member block closes (`W_STATE_EPIC_ALL_CLOSED` is warn-only, by design
  — the last block closing is not the same as the initiative's actual goal being met). This verb
  is how you state that judgement explicitly. It touches **only** the epic's own registry
  `status` — no member block's status is ever touched, and the effect is one-way.

## The two lock-contention errors — do not conflate them

Every `--write` call checks a sibling lane's declared quiet window **first**, then takes the
ordinary emit lock:

| Error | Means | What to do |
|---|---|---|
| `E_QUIESCE_LEASE_HELD` | A sibling lane holds an exclusive fleet-wide lease and has declared a quiet window | **Do not retry blindly.** Wait for the window to lift, or pass `--agent <your-identity>` if you are the lane that is allowed to self-exempt |
| `E_EMIT_LOCK_HELD` | Another live process holds the ordinary `<root>/.mev-emit.lock` (names the holder's pid) | Wait for it to finish and retry. A **stale** lock (holder pid dead) is reclaimed automatically — you never need to delete the lock file by hand |

## Smoke-testing before you trust the guidance above

This skill's claims were verified against a disposable fixture brain — a scratch `brain.toml` +
`state.json` with one epic and two member blocks, never the real corpus — rather than experimenting
on a live initiative. Reproduce with (every field below is required; `mev validate-state` will name
the next missing one if you trim any):

```bash
TMP=$(mktemp -d) && cd "$TMP"
cat > brain.toml <<'EOF'
[[repos]]
slug = "fixture"
prefix = "ZZ"
tier = "_root"
repo_path = "."
status_file = "planning/status.md"
cache_doc = "README.md"
EOF
mkdir -p planning
cat > planning/state.json <<'EOF'
{
  "repo": "fixture",
  "kind": "brain",
  "updated": "2026-09-02",
  "epics": [{"slug": "fixture-epic", "title": "Fixture epic", "status": "active", "plan": "planning/fixture-epic/roadmap.md"}],
  "tracks": [{"title": "fixture track", "blocks": [
    {"id": "ZZ.1.A", "title": "member one", "status": "open", "depends_on": [], "epics": ["fixture-epic"], "repo": "fixture"},
    {"id": "ZZ.1.B", "title": "member two", "status": "in_progress", "depends_on": [], "epics": ["fixture-epic"], "repo": "fixture"}
  ]}]
}
EOF
mev validate-state planning/state.json    # 0 errors (some W_STATE_SDLC_WORKFLOW_MISSING/repos[] warnings expected — fixture-only noise)
mev defer-epic fixture-epic --write       # epic -> paused; ZZ.1.A -> deferred; ZZ.1.B untouched + W_EPIC_SKIPPED_IN_PROGRESS
mev resume-epic fixture-epic --write      # epic -> active; ZZ.1.A -> open again; ZZ.1.B still untouched
mev sync-epics                            # 0 warnings on the now-consistent fixture -- confirms "no drift"
rm -rf "$TMP"
```

Verified live 2026-09-02: all three transitions and the final `sync-epics` clean read matched
exactly. Confirm `mev validate-state planning/state.json` is clean after each `--write` before
trusting a result you didn't run yourself.

## Field table and full schema

`docs/state/state-schema.md`'s `epics[]` section is the ground truth for the registry's fields
(`slug`, `status`, `plan`). This skill covers the four verbs' behavior; it does not restate the
schema.
