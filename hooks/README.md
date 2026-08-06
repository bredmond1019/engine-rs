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
2026-08-04 under `HQ.chore.pre-push-gate-hook` — HQ (this repo), `learn-ai/`, `business/bastiel/`,
`client/brazilianportugui/`, `client/wild-trail-photo/`. The remaining ten were switched on
2026-08-06 during a quiet-fleet window: `core/orchestrator`, `core/mev`, `core/bastion`,
`core/bastion-ui`, `core/bella`, `core/engine-rs`, `core/claude-code-rs`, `core/bastion-web`,
`side/amistad`, `side/price-scout`.

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
`harness.json` — 15 of 19 real repos already carry one with real gated checks (orchestrator 7,
learn-ai 6, bastion 5, most others 4). Eleven of those have a working `harness.json` and no hook,
so stage 2 is one `git config` away for each.

Only one repo still needs a `harness.json`: **`core/okf-core`**, already ticketed as
`OK.ticket.harness-json-all-targets-clippy`.

**`okf-core`'s hooks were switched on 2026-08-06 anyway**, before that ticket lands. A missing
`harness.json` makes stage 2 skip with a notice rather than fail (see the graceful-skip cases at the
top of `hooks/pre-push`), so the repo gets the **stage 1 corpus gate immediately** — which is the
half that matters for a repo participating in the brain corpus — and picks up stage 2 for free when
the ticket ships. Verified by running the hook directly: stage 1 ran all five flags, stage 2 printed
`no planning/harness.json … skipping repo gate`.

These three are **deliberately out of scope** and should not be re-flagged by future sweeps:

| Repo | Why not |
|---|---|
| `portfolio/rag-engine-rs` | portfolio piece, not active development |
| `bastion-os` | pending `HQ.chore.bastion-os-to-portfolio` |
| `example-repo/qm` | a sample repo, not a real project |

## Hooks

| Hook | Fires | What it does |
|---|---|---|
| `pre-commit` | Before every commit | Parses the YAML frontmatter of every **staged** `.md` file and blocks the commit on a parse error (unquoted colon/`#`/em-dash clause inside a plain scalar). No-op for clean or absent frontmatter. |
| `post-commit` | After every commit | If the commit **deleted or renamed** a file: (1) prunes that file's stale rows from the Brain RAG vector store (`brain_documents`), and (2) appends the path(s) to `.brain-moves-pending` for integrity checking. No-op for ordinary edits. |
| `pre-push` | Before every push | Two stages, both run, either can block. **Stage 1:** the full 5-flag `validate-brain` suite — validates the whole corpus, but blocks only on errors **new since this clone's last successful push** (`PREPUSH_STRICT=1` gates on the total instead). **Stage 2:** this repo's own `planning/harness.json` `validation.checks[]` where `gates: true` (lint/types/test/build) — blocks on a real non-zero exit from any of them. |

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

```bash
bash hooks/test_pre-commit.sh   # exit 0 = all pass
```

10 cases: clean frontmatter passes, an unquoted colon blocks (and names the file:line),
the same value quoted passes, no-frontmatter passes, a non-`.md` staged file with
YAML-shaped content is ignored, an unstaged broken file is ignored, re-staging a broken
edit over a clean one blocks (proves it checks the staged blob, not the first `git add`),
no staged `.md` files at all is a silent no-op, and PyYAML being unimportable (isolated
PATH to the bare system `python3`, which has no PyYAML) degrades non-fatally.

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

The full 5-flag `validate-brain` suite already runs nightly on the Mac Mini
(`scripts/routine.sh` → `scripts/validate_brain.sh`) and exits non-zero on failure — but that is
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
- **The baseline ratchets down, never up.** `scripts/validate_brain.sh` rewrites it lower whenever
  the measured total drops; nothing raises it automatically. A stale-high baseline is merely
  permissive (safe); an auto-raising one would silently absorb exactly the drift this gate exists
  to catch.
- **The check flags do not compose** (same constraint as `scripts/validate_brain.sh`): `--sync
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
anything". `scripts/validate_brain.sh` answers the same question outside of a push, and runs nightly.

```bash
PREPUSH_STRICT=1 git push        # gate on the whole corpus
./scripts/validate_brain.sh      # same question, no push involved
```

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
- **Cost is whatever the repo's own gates cost** — measured on the four repos this shipped with:
  bastiel ~16s (lint+types+test+build), brazilianportugui ~12s, learn-ai ~40s (6 checks incl. a
  full `next build`), wild-trail-photo currently a no-op (unscaffolded, no `package.json` yet).
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
derived files across every repo in `brain.toml`, and both `/log-work` and `scripts/routine.sh`
invoke it from `PATH`. A stale install keeps writing with whatever derivation logic it was built
with, silently.

This is not hypothetical. On 2026-08-04 the append-only revision-history writer
(`MV.ticket.append-only-emit-state-writer`) shipped, merged, and closed — while `~/.cargo/bin/mev`
still held a pre-merge build. Every real `emit-state --write` for hours afterward ran *without* the
safety net the ticket had just added, and nothing surfaced it.

**Why it drifts on the machine doing the work.** `scripts/build_and_install.sh` reinstalls a binary
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
