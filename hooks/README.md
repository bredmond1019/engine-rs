---
type: Guide
title: Git Hooks
description: Tracked git hooks for the brain repo and how to enable them via core.hooksPath.
layer: [meta, brain]
status: active
keywords: [git-hooks, brain-rag, freshness, post-commit, pre-push, validate-brain]
related: [python-orchestration]
---

# Git Hooks

Tracked git hooks for the brain repo. They live here (not in `.git/hooks/`) so they
are version-controlled, reviewable, and survive a fresh clone. Git is pointed at this
directory with `core.hooksPath`.

## Enabling

Run once, from the brain repo root:

```bash
git config core.hooksPath hooks
```

This makes git look in `hooks/` instead of `.git/hooks/`. Confirm with:

```bash
git config --get core.hooksPath   # → hooks
```

> Note: setting `core.hooksPath` replaces `.git/hooks/` wholesale. The brain repo
> has no active hooks there, so nothing is lost. To revert: `git config --unset core.hooksPath`.

**Enabled per repo (as of 2026-08-06): all 15 eligible repos.** The first five were switched on
2026-08-04 under `HQ.chore.pre-push-gate-hook`; the remaining ten on 2026-08-06 during a
quiet-fleet window. The repos are deliberately not enumerated here: this file ships inside
public repositories, and the fleet's repo list includes client work. Read the live list from
`brain.toml`'s `[[repos]]` table instead, which is where it is authoritative anyway.

> **`base-template` is not eligible and never was.** It has no `hooks/` directory because
> `discover_targets()` in `base-template/scripts/sync_downstream_harness.py` explicitly skips
> base-template itself, so it never receives the synced hook files. Earlier notes that listed it
> among the pending targets were wrong.

**First watched run (2026-08-06): 6 green, 4 red.** Green through both stages: `bastion-ui`,
`claude-code-rs`, `bella`, `engine-rs`, `bastion`, `orchestrator`. Red on **stage 2 only**, each
from pre-existing repo-local breakage rather than hook wiring — `mev` (2 `brain_conformance`
test failures), `price-scout` (pytest collection: `price_scout` not installed in the venv),
`amistad` (2 `tsc` errors), `bastion-web` (1 test: real `sync-serve-types` drift against
`../bastion`). Hooks were left enabled in all four; `git push --no-verify` is the escape hatch.
Tracked by carryover `hq-hook-propagation-four-repos-red`.

**Rollout scope (settled 2026-08-04).** The remaining work is switching hooks on, *not* authoring
`harness.json` — 16 of 19 real repos already carry one with real gated checks (orchestrator 7,
learn-ai 6, bastion 5, most others 4). Eleven of those have a working `harness.json` and no hook,
so stage 2 is one `git config` away for each.

**No repo is still missing a `harness.json`.** `core/okf-core` was the last one; its
`OK.ticket.harness-json-all-targets-clippy` closed 2026-08-06 and added a four-gate manifest (`fmt`,
`clippy --all-targets`, `test`, `build`), so stage 2 now gates that repo instead of skipping.

`okf-core`'s hooks had been switched on 2026-08-06 *before* that ticket landed, which is why the
graceful-skip path mattered there: a missing `harness.json` makes stage 2 skip with a notice rather
than fail (see the cases at the top of `hooks/pre-push`), so the repo got the **stage 1 corpus gate
immediately** — the half that matters for a repo participating in the brain corpus — and picked up
stage 2 for free when the ticket shipped. Both halves are live there now.

These three are **deliberately out of scope** and should not be re-flagged by future sweeps:

| Repo | Why not |
|---|---|
| `portfolio/rag-engine-rs` | portfolio piece, not active development |
| `bastion-os` | pending `HQ.chore.bastion-os-to-portfolio` |
| `example-repo/qm` | a sample repo, not a real project |

## Hooks

| Hook | Fires | What it does |
|---|---|---|
| `pre-commit` | Before every commit | Two gates, both run, either can block. **Gate 1:** parses the YAML frontmatter of every **staged** `.md` file and blocks on a parse error (unquoted colon/`#`/em-dash clause inside a plain scalar). No-op for clean or absent frontmatter. **Gate 2:** the same new-errors-only `validate-brain` delta check pre-push uses (see below), scoped to the two cheap flags (`--graph`, `--structure`) and run on every commit, not just at push. |
| `post-commit` | After every commit | If the commit **deleted or renamed** a file: (1) prunes that file's stale rows from the Brain RAG vector store (`brain_documents`), and (2) appends the path(s) to `.brain-moves-pending` for integrity checking. No-op for ordinary edits. |
| `pre-push` | Before every push | Two stages, both run, either can block. **Stage 1:** the full 5-flag `validate-brain` suite — validates the whole corpus, but blocks only on errors **new since this clone's last successful push** (`PREPUSH_STRICT=1` gates on the total instead). **Stage 2:** this repo's own `planning/harness.json` `validation.checks[]` where `gates: true` (lint/types/test/build) — blocks on a real non-zero exit from any of them. |

`hooks/validate_brain_gate.sh` is the shared library behind both the `pre-commit` gate-2 delta
check and `pre-push` stage 1 — one `run_validate_brain_gate <label> <flag> [<flag> ...]`
function, sourced by both hooks, so the new-errors-only attribution logic is defined in
exactly one place. Not a hook itself — nothing invokes it directly.

### `pre-commit` — author-time OKF frontmatter YAML gate

A `: ` (colon-space), unquoted `#`, or an em-dash clause inside an unquoted plain scalar
in OKF frontmatter (typically `description:`/`title:`) breaks YAML parsing with
`mapping values are not allowed in this context`. Because `mev validate-brain`'s
`--structure`/`--links`/`--graph`/`--state` flags all load the same frontmatter, one bad
description fails all four simultaneously and looks like four broken gates for a change
that has nothing to do with any of them. This recurred three separate times on
2026-08-06 alone across three independent agent sessions (see `planning/state.json`
carryover `okf-frontmatter-unquoted-colon-trap`) — a day after already being fixed once
and re-filed as "only a gate prevents recurrence"
(`core/_planning/orchestrator/state.json` carryover
`frontmatter-colon-parse-failures-recur-fleet-wide`). This hook is that gate.

- **Staged `.md` files only**, checked against their **staged blob content**
  (`git show :<path>`), not the working tree — so a broken unstaged edit sitting in the
  same file is ignored, and re-staging a fix is what clears the block.
- **No staged `.md` files → exit 0, silent.** Ordinary commits (code, non-markdown docs)
  pay nothing.
- **Parses via `hooks/check_frontmatter.py`** (PyYAML `safe_load` over the
  `---`-delimited block) — a real YAML parser, not a colon-regex, so it does not false-
  positive on legitimately colon-containing *values* that are already quoted or on block
  scalars.
- **This is a parse gate, not a presence gate.** A file with no frontmatter at all, or an
  unterminated `---` block, passes silently — Standing Rule 6 (every new file needs OKF
  frontmatter) is a separate concern this hook does not enforce.
- **Degrades gracefully**, same spirit as `post-commit`/`pre-push`: no `python3` on PATH,
  PyYAML not importable, or `hooks/check_frontmatter.py` missing → warning on stderr,
  exit 0. The checker never blocks a commit by failing to run — only a real parse error
  in staged frontmatter does.
- **`git commit --no-verify` skips it**, same escape hatch as the other hooks.

#### The `created` / `updated` date gate — **ON** since 2026-08-31

`created` and `updated` were added to `okf_core::OkfFrontmatter` on 2026-08-29 (okf-core
block `OK.ticket.add-created-updated-frontmatter`). **This hook is the only thing in the
fleet that validates them**: okf-core models both as free strings, and mev's OKF validator
still has no rule for either — so outside a commit that runs this hook, a malformed or
impossible date remains invisible. `check_frontmatter.py` carries the gate, on by default:

```bash
OKF_DATE_GATE=0 git commit -m "..."     # gate off, for this commit only
```

A staged `.md` whose frontmatter parses is additionally checked for:

| Rejected | Why |
|---|---|
| `updated: soon` — any non-date value | Not a `YYYY-MM-DD` date |
| `created: "2026-13-40"` | Shaped like a date, is not one |
| `created: 2026-08-29T10:00:00Z` | A full timestamp; that shape belongs in `timestamp:` |
| `updated` earlier than `created` | A doc cannot be revised before it existed |

Absent fields pass — both are optional and the corpus is **not** backfilled. The trap the
implementation has to handle: PyYAML parses an unquoted `created: 2026-08-29` into a
`datetime.date`, not a `str`, so a plain regex over the value would reject the commonest
correct spelling; and `datetime.datetime` is a *subclass* of `date`, so the timestamp case
must be rejected before the date check, not after.

**Out of scope: code test fixtures.** The hook checks every staged `.md`, but the corpus
is only `docs/` + `planning/`. A markdown file under a source tree is test data meant to be
*parsed*, and may carry an odd field precisely because that is what a test asserts on — so
`is_code_fixture()` exempts any path containing `/fixtures/` from the **date** gate. The
YAML **parse** gate still applies to them: a fixture that will not parse is broken either way.

**Blast radius, re-measured 2026-08-31 before enabling** — over **7,425 tracked `.md` files
across every repo in the fleet** (the 2026-08-29 note below it measured 4,850 / 12 / 0 and is
superseded): **45 carry a `created:`/`updated:` field, and 5 would have been rejected** — all
five bastion fixtures under `src/**/fixtures/`, each carrying an RFC3339 `updated:` that no
bastion parser reads. With `is_code_fixture()` in place the count is **0 blocked**, verified by
re-running the same sweep. Re-measure before widening the gate's scope again.

Note the hook is still only *live* where `core.hooksPath` is set (HQ is; `pre-push` remains
`chmod -x`'d fleet-wide), so enabling this is a change to author-time behaviour in HQ and in
every repo that opts in, not a fleet-wide flag day.

### Gate 2 (corpus graph/structure): off-and-back-on, 2026-09-01 → 2026-09-02

The frontmatter gate above (gate 1) stays on throughout; only gate 2 (the corpus
graph/structure delta check) went through this cycle.

**Shipped 2026-09-01, broke same day.** The gate scores the WHOLE corpus by design — that is
what lets it catch the break class a path-scoped gate misses, where deleting a doc surfaces the
error on a different file. But the corpus is one shared vault written by several concurrent
sessions, and the FIRST version of the gate attributed a new error to "the repo currently
committing" using only the physical git repo boundary. That is too coarse: a commit scoped to
`core/_planning/mev/` was blocked by `core/bella/planning/ide-layout/sequence.md`, an
**untracked** file another session had written 40 minutes earlier — different sub-repos'
planning vaults, but the SAME physical HQ git repo (every `planning/` is a symlink into
`core/_planning/<slug>/`, CLAUDE.md standing rule 10), so the repo-boundary check could not
tell them apart. Switched off behind `BRAIN_GRAPH_GATE=1` (opt-in) the same day.

**Re-enabled 2026-09-02** after the real fix: `hooks/validate_brain_gate.sh`'s
`classify_new_errors` now scopes blocking by **lane**, not merely by physical repo — a
`core/<slug>/planning/...` or `core/_planning/<slug>/...` path (both shapes a file can appear
under are unified to the same lane token) is its own lane, distinct from the rest of the repo.
A commit in `core/_planning/mev/` no longer blocks on a break in `core/_planning/bella/`; it
still blocks on a break inside its own vault, or in genuinely shared top-level content
(`docs/`, `hooks/`, `scripts/`). Verified live against the exact reported scenario, both
directions, before re-enabling. Gate 2 is **ON by default** again; `BRAIN_GRAPH_GATE=0` is
kept as an opt-out escape hatch (inverted from the 2026-09-01 default) in case a lane-scoping
edge case surfaces before this has been proven at scale.

```bash
BRAIN_GRAPH_GATE=0 git commit -m "..."   # skip gate 2 for one commit
```

`hooks/pre-push` stage 1 uses the same shared gate script throughout this whole cycle and was
never affected by either the break or the fix — see `hooks/validate_brain_gate.sh`'s header for
why (pre-push has nothing staged post-commit, so it always used the whole-repo fallback, which
this lane fix leaves untouched).

```bash
bash hooks/test_pre-commit.sh   # exit 0 = all pass
```

47 checks across ~30 scenarios — clean frontmatter passes, an unquoted colon blocks (and names the file:line),
the same value quoted passes, no-frontmatter passes, a non-`.md` staged file with
YAML-shaped content is ignored, an unstaged broken file is ignored, re-staging a broken
edit over a clean one blocks (proves it checks the staged blob, not the first `git add`),
no staged `.md` files at all is a silent no-op, and PyYAML being unimportable (isolated
PATH to the bare system `python3`, which has no PyYAML) degrades non-fatally.

The date-gate cases pin both halves of the default: one proves a plainly bad date is now
**blocked** with no env var set, one proves `OKF_DATE_GATE=0` still lets it through, one
proves a `src/**/fixtures/*.md` file is exempt from the date gate while the parse gate still
blocks an unquoted colon in it, and the rest set `OKF_DATE_GATE=1` explicitly and cover the unquoted
date, the quoted date, a non-date value, a full timestamp, reversed ordering, an
impossible date, and both fields absent. The env var passes through `git commit` into the
hook and on into the checker, so they exercise the real path rather than calling the
function directly.

Gate 2's own cases (`new_gated_repo()`, a `bastion` shim over `--graph`/`--structure` only)
prove: the gate is ON with no env var at all, and `BRAIN_GRAPH_GATE=0` opts back out; the gate
visibly runs rather than silently skipping when `brain.toml` + `bastion` are both present; a
fresh fixture with no `.git/validate-last-good.json` yet blocks on a newly introduced error,
falling back to the (absent, so zero) tracked baseline; the SAME error pre-recorded in
`.git/validate-last-good.json` does **not** block — the fairness property gate 2 exists for;
`bastion` missing from PATH degrades gracefully; a nested git repo's error is advisory-only for
the outer repo's commit while the outer repo's own error still blocks (`new_nested_repo()`);
and — the 2026-09-01→02 fix's own regression test — two DIFFERENT sub-repo planning vaults
inside the SAME physical git repo are correctly kept apart (`new_planning_vaults_repo()`),
including proving the real-vault-path and symlinked-face path SHAPES for the same vault map to
the identical lane. Every other existing case's fixture has no `brain.toml` at all, so gate 2
always skips gracefully for them ("no brain.toml found") — proving the gate is additive and
does not change gate 1's existing behavior.

#### `pre-commit` gate 2 — corpus graph/structure delta gate (added 2026-09-01)

Gate 1 above only catches YAML *parse* errors. It says nothing about a `related:` entry
naming a `doc_id` that doesn't exist, a missing cross-repo prefix (`seams-foo` instead of
`engine-rs:seams-foo`), or an `index.md` row pointing at a file that's been renamed or
deleted — those are `bastion validate-brain --graph`/`--structure` errors, and until this
gate existed they were caught only at **push** time (`pre-push` stage 1, or `preflight.sh`
inside `push_routine.sh`) — often a whole session after the edit that caused them, discovered
only when a push is blocked with no clue which commit is at fault.

- **Reuses `pre-push` stage 1's exact new-errors-only attribution** via the shared
  `hooks/validate_brain_gate.sh` (`run_validate_brain_gate`) — see that file's header for
  the full rationale. In short: it diffs the corpus's current error *set* against
  `.git/validate-last-good.json`'s known set, so a commit is blocked only for an error it
  itself introduces, never for a pre-existing one another session already left unresolved
  (unless that session bypassed the gate with `--no-verify`).
- **Scoped by LANE, not merely by physical repo** (the 2026-09-02 fix). A new error blocks
  only if it is owned by the SAME lane as what this commit is staging — a separate git
  repo's own lane (unchanged since 2026-09-01), OR a specific `core/<slug>/planning/...` /
  `core/_planning/<slug>/...` sub-repo vault (both path shapes unified), OR genuinely shared
  top-level content (`docs/`, `hooks/`, `scripts/`) as its own catch-all lane. A commit
  scoped to one sub-repo's vault is never blocked by a break in a different sub-repo's
  vault, even though both are tracked by the SAME physical HQ git repo.
- **Only `--graph` and `--structure`** (~1s each, corpus-wide). `--links` (~11s) and
  `--state`/`--sync` stay at push time — too slow to pay on every commit.
- **Unconditional** — unlike gate 1, this does NOT skip when no `.md` file is staged. A
  commit that only touches `planning/state.json` (the common shape for
  `emit_state_write.sh`, since every sub-repo's `planning/` is a symlink into this HQ git —
  CLAUDE.md standing rule 10) still pays for it: `validate-brain` always scores the whole
  corpus regardless of what the commit touched, because the errors worth catching are
  relational — an edit to one file can dangle a `related:` edge or `index.md` row in a
  completely different, untouched file. The LANE check above only narrows which of those
  errors can *block*, never what is *checked*.
- **Degrades gracefully**: no `brain.toml` walking up → skip, notice only; `bastion` not on
  PATH → skip, warning only; `hooks/validate_brain_gate.sh` missing → skip, warning only.
- **`.git/validate-last-good.json` is per-clone, per-PHYSICAL-repo, and untracked** — HQ's
  is a different file from `core/mev`'s own, even once every repo carries this gate; it only
  ever advances past a commit that did NOT block, so the fairness property holds within one
  repo as long as nobody routes around the gate with `--no-verify`.
- **`BRAIN_GRAPH_GATE=0`** turns gate 2 off for one commit (it is ON by default). Separately,
  **`VALIDATE_BRAIN_STRICT=1`** (or the older `PREPUSH_STRICT=1`, kept as an alias) forces
  the whole-corpus, every-lane test instead of the delta/lane scoping, same escape hatch
  `pre-push` stage 1 has.

### `post-commit` — Brain RAG delete/rename freshness

The brain indexer (`core/orchestrator/scripts/index_brain.py`) upserts vector rows keyed on
`file_path + section`. When a file is **deleted or renamed away**, the incremental indexer
never revisits the old path, so its rows linger and surface as stale retrieval hits. This
hook calls `syn prune` — `app/brain/ops.py::prune_paths` (a row delete, no re-embedding, no
embedding-API cost) — for exactly those paths.

After pruning, the hook also **appends a log line to `.brain-moves-pending`** at the repo
root. Format: `<ISO-date> <rel-path1> [rel-path2 ...]` — one line per commit that had
deletes or renames. This file is gitignored and ephemeral; it is the input source for
`bastion validate --integrity` (future Block K), which will walk `related:` edges across
the corpus and flag any references that now point at a deleted/renamed path.

- **Cheap by default.** It runs a single `git diff` on every commit and exits in
  milliseconds unless that commit actually deleted or renamed something.
- **`git mv`-independent.** Git infers renames by content similarity, so a rename is
  caught whether or not `git mv` was used; the old path is pruned and logged.
- **Non-fatal.** `post-commit` runs after the commit has landed, so it can never block a
  commit; a prune failure only prints a warning.
- **Degrades gracefully.** If the orchestration engine isn't checked out, the hook exits 0
  without writing the log (no engine = no vector store to prune; no log needed).

**`.brain-moves-pending` format:**
```
2026-06-26 planning/bastion-product/architecture.md planning/bastion-product/ownership.md
2026-06-27 docs/some-old-doc.md
```
Each line is a space-separated record: ISO date followed by one or more repo-relative paths
that were deleted or renamed in that commit. `bastion validate --integrity` will consume
this file and clear lines it has processed.

**Scope / limits:** this catches **file-level** orphans only. A section renamed or removed
*inside* a still-existing file is not caught here — run `index_brain.py --rebuild` after
structural edits within files. This hook is the delete/rename slice of the broader
**Block J** freshness loop (auto-reindex on commit), which is otherwise deferred.

### `pre-push` — validate-brain drift gate

**The delta-attribution logic below (block on new errors only) is defined once, in
`hooks/validate_brain_gate.sh`, and shared with `pre-commit` gate 2** (a cheaper,
`--graph`/`--structure`-only subset of the same 5 flags, run at commit time instead of only
at push time — see that section above). This section describes the mechanism; both hooks
use it identically.

The full 5-flag `validate-brain` suite already runs nightly on the Mac Mini
(`scripts/sync/routine.sh` → `scripts/sync/validate_brain.sh`) and exits non-zero on failure — but that is
detection only: nightly, remote, log-only, and it gates nothing. A bad commit made on any machine
can be pushed immediately, and the drift is only discovered a day later in a log file nobody is
looking at. This hook moves the same check to the push boundary, where it can actually stop a
regression from landing.

- **Blocks on what YOUR push introduced, not on the corpus total** (changed 2026-08-04). The
  whole corpus is still *validated* — only the *blocking decision* narrows. See
  [Attribution](#attribution-what-blocks-you) below, which is the most important thing to
  understand about this gate.
- **The baseline is corpus-wide and singular, not per-repo.** `validate-brain` resolves the brain
  root by walking *up* from wherever it runs and always validates the **entire corpus** regardless
  of cwd — so a push from `core/engine-rs` and a push from HQ see the identical error set. The
  baseline file lives in HQ only and is read read-only from every sub-repo via the resolved brain
  root. It remains the fallback when no per-clone history exists yet.
- **The baseline ratchets down, never up.** `scripts/sync/validate_brain.sh` rewrites it lower whenever
  the measured total drops; nothing raises it automatically. A stale-high baseline is merely
  permissive (safe); an auto-raising one would silently absorb exactly the drift this gate exists
  to catch.
- **The check flags do not compose** (same constraint as `scripts/sync/validate_brain.sh`): `--sync
  --graph --state --links --structure` need five separate invocations, ~1.6s each (~8s total,
  measured). This lives at pre-push, not pre-commit, on purpose — that cost is fine once per push,
  not once per commit.
- **Degrades gracefully**, same spirit as `post-commit`: exits 0 with a notice if no `brain.toml`
  is found walking up (standalone checkout), and exits 0 with a warning if `bastion` isn't on
  PATH. Warnings never block a push — only a measured, parsed error total over the baseline does.
- **`--no-verify` exists and that is fine.** `git push --no-verify` skips the hook entirely. This
  is a speed bump against silent drift, not a security control.
- **Distributed downstream** (see `base-template/scripts/sync_downstream_harness.py`), but
  copying the file alone does nothing — **enabling is per-repo and manual**. Each downstream repo
  must run `git config core.hooksPath hooks` once (see Enabling, above) before its pre-push hook
  actually fires; the sync script prints a per-repo reminder whenever it syncs the hook into a
  repo where this isn't already set.

#### Attribution: what blocks you

The baseline is corpus-wide and singular. A plain `total > baseline` test therefore blocks **every**
hooked repo whenever any file anywhere is bad — including files that repo never touched, written by
another session or by an unattended routine. That is precisely how a gate gets muted with
`--no-verify` forever, so blocking is attributed instead:

| Situation | Result |
|---|---|
| Errors **new** since this clone's last successful push | **blocked**, and only the new ones are printed |
| Errors that were already there | reported, not blocking |
| `PREPUSH_STRICT=1` set | blocked on the **whole-corpus total** vs the baseline |
| No `.git/validate-last-good.json` yet (fresh clone) | falls back to the tracked baseline — the pre-2026-08-04 behaviour |
| That file corrupt or unreadable | falls back to the baseline; never fails open |

**Attribution is by delta, never by path.** This matters more than it looks. Delete `docs/foo.md`
and the resulting error surfaces on `bar.md` — a file your push never touched. A path-scoped gate
would wave that straight through; a delta-scoped one still blocks, because the error is new. There
is a test for exactly this case (`delta: blocks on an error in an untouched file`).

`.git/validate-last-good.json` records the error set present at the last push this clone let
through. It lives under `.git/` deliberately: untracked, per-clone, never committed, so it cannot
drift into the corpus or be shared between machines.

**When to use `PREPUSH_STRICT=1`:** before a deploy, after a large merge, when enabling the hook in
a new repo, or any time the real question is "is *all* of it correct" rather than "did I break
anything". `scripts/sync/validate_brain.sh` answers the same question outside of a push, and runs nightly.

```bash
PREPUSH_STRICT=1 git push        # gate on the whole corpus
./scripts/sync/validate_brain.sh      # same question, no push involved
```

> **Warning: running `hooks/pre-push` by hand rewrites the baseline.** Invoking the script directly
> (rather than through an actual `git push`) still writes `.git/validate-last-good.json` on success,
> silently advancing this clone's delta baseline. Every error present at that moment becomes
> "pre-existing" and stops blocking future pushes — the gate quietly weakens and nothing reports it.
> If the intent is only to **look**, run `./scripts/sync/validate_brain.sh` instead — it answers the same
> question without touching the baseline.

### `pre-push` — stage 2: repo-native gate (lint/types/test/build)

Stage 1 only ever measures brain-corpus drift — it says nothing about whether the repo's own
code still works. The solo-operator failure mode this closes is "I forgot to run the gate"
before pushing: stage 2 re-runs the repo's own `planning/harness.json` at the push boundary, the
same policy file the SDLC engines (`/test`, `/review-task`) already read for Test/Review. No new
config format — this is the existing checks manifest, re-run at a new trigger point.

- **Reads `planning/harness.json`** (repo-local, not the brain root) → `validation.checks[]` →
  runs every entry with `gates: true`, in file order, each as `sh -c "<command>"` from the repo
  root. Non-gated checks are never run here (`gates` is not implicitly true).
- **Blocks only on a real non-zero exit** from a gated check — never on a warning-only tool
  (e.g. ESLint warnings without `--max-warnings 0` still exit 0).
- **Degrades gracefully**, same spirit as stage 1 and `post-commit`:
  - no `planning/harness.json` in this repo → skip, notice only (most repos don't carry the
    harness yet — this is not an error)
  - `python3` not on PATH (needed to parse the JSON) → skip, warning only
  - `harness.json` unparseable, or has no `gates: true` checks → skip, warning/notice only
  - **stack declared but not yet scaffolded** — if `harness.json` names a `stack` (`nextjs`,
    `rust`, `python`, …) but that stack's marker file isn't present in the repo root
    (`package.json`, `Cargo.toml`, `pyproject.toml`) → skip, notice only. A placeholder
    `harness.json` committed ahead of `create-next-app`/`cargo new` is not a real failure.
  - an individual gated check's command isn't on PATH (e.g. a tool nobody installed on this
    machine) → that one check is skipped, warning only; the rest still run
- **Cost is whatever the repo's own gates cost** — measured on the four repos this shipped
  with: roughly 12-16s for a small Next.js app (lint+types+test+build), ~40s for a larger one
  (6 checks including a full `next build`), and a no-op for a repo not yet scaffolded (no
  `package.json`, so nothing to run).
  Same rationale as stage 1 for living at pre-push and not pre-commit.
- **`--no-verify` skips both stages.** There is no way to skip stage 2 alone short of temporarily
  removing/editing `planning/harness.json`'s `gates` flags — that's intentional; if a check is
  genuinely not push-worthy, un-gate it in `harness.json` (the same file the SDLC pipeline reads),
  don't special-case the hook.

**Enabling stage 2 in a given repo requires two things**, both one-time and manual: the repo must
already carry a real `planning/harness.json` with `gates: true` checks (the SDLC pipeline's
`/generate-tasks` scaffolds this, or copy a profile from `planning/harness.examples.md`), and the
repo's git must be pointed at `hooks/` (`git config core.hooksPath hooks`, same as stage 1 —
there's only one `pre-push` file, one switch enables both stages together).

### `pre-push` — advisory: is the installed `mev` binary stale?

After both stages, the hook prints a **notice** (never a block) when the `mev` on `PATH` was built
from a different commit than its source tree's current `HEAD`.

**Why this is worth the noise.** `mev` is the fleet's *writer* — `mev emit-state --write` rewrites
derived files across every repo in `brain.toml`, and both `/log-work` and `scripts/sync/routine.sh`
invoke it from `PATH`. A stale install keeps writing with whatever derivation logic it was built
with, silently.

This is not hypothetical. On 2026-08-04 the append-only revision-history writer
(`MV.ticket.append-only-emit-state-writer`) shipped, merged, and closed — while `~/.cargo/bin/mev`
still held a pre-merge build. Every real `emit-state --write` for hours afterward ran *without* the
safety net the ticket had just added, and nothing surfaced it.

**Why it drifts on the machine doing the work.** `scripts/sync/build_and_install.sh` reinstalls a binary
only when `git_sync.sh` **pulled** new commits for its repo. Commits **authored** locally never trip
that condition — so the authoring machine is exactly the one that goes stale, while the Mac Mini
self-heals on its next cron pull.

Detection is free: mev's own `toolchain-freshness` conformance check already compares its
compiled-in `MEV_BUILD_GIT_SHA` against the live tree's `HEAD` (~50ms). Nothing was acting on the
result; this just says it out loud, once per push. The fix it prints:

```bash
cargo install --path core/mev --force
```

Advisory only, and it degrades like everything else here: `mev` not installed → silent; `mev`
present but failing → silent; binary current → silent. It also prints on the **blocked** path, since
a stale writer is worth knowing about either way. Note this covers `mev` and not `bastion`:
`~/.local/bin/bastion` is a symlink into `core/bastion/target/release/`, so it auto-tracks every
release build, and bastion exposes no equivalent self-check.

## Testing

Both hooks have self-contained regression tests — throwaway git repos, no real database, no
network, safe to run anywhere.

`test_post_commit.sh` builds throwaway git repos, installs the real hook, and shadows `uv` with
a shim that records the prune args — so it needs no `uv`, Python, or database. It covers the
no-op (ordinary edit / root commit), delete, rename-without-`git mv`, and engine-absent cases.

```bash
bash hooks/test_post_commit.sh   # exit 0 = all pass
```

The `prune_paths` op the hook calls is unit-tested separately in the orchestrator repo
(`tests/brain/test_ops.py::TestPrunePaths`, `tests/brain/test_cli_ops.py::TestPruneDispatch`);
the underlying `--prune-paths` indexer mode has its own coverage in
`tests/test_index_brain.py::TestPrunePaths`.

`test_pre_push.sh` builds throwaway git repos, installs the real hook, and shadows `bastion`
with a shim that emits canned `validated <path>: N error(s), M warning(s)` lines — so it needs
no real `bastion`/`mev` binary, database, or network. 44 cases total: stage 1 covers
under/at/over-baseline, `bastion`-absent, no-`brain.toml`, missing-baseline-treated-as-0, and
warnings-never-block; **attribution** covers a pre-existing error not blocking, a newly
introduced one blocking, the block report listing only what is new, an error in a file the push
never touched still blocking (the delete-breaks-another-file case), `PREPUSH_STRICT` blocking
what delta mode allows, last-good being written after a successful push, and a corrupt last-good
falling back to the baseline rather than failing open; stage 2 (repo-native gate) covers no-`harness.json`, a passing gated check,
a failing gated check (with its output surfaced and the block message naming stage 2), an
unscaffolded stack (marker file missing → skip), a gated check whose command isn't on PATH
(warn + skip), a `harness.json` with only non-gated checks (skip), and a combined case proving
stage 1 alone still blocks even when stage 2 would pass (both stages always run and report,
regardless of the other's outcome).

```bash
bash hooks/test_pre_push.sh   # exit 0 = all pass
```

The suite `unset`s `PREPUSH_STRICT` at the top. It is itself a gated check in HQ's
`planning/harness.json`, so `PREPUSH_STRICT=1 git push` runs it with that variable inherited —
which silently put every non-strict case into strict mode and failed one on 2026-08-04. Cases
that want strict mode set it per-invocation. Keep it that way: a test suite whose result depends
on the caller's environment is worse than no suite.
