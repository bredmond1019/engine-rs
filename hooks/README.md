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

## Hooks

| Hook | Fires | What it does |
|---|---|---|
| `post-commit` | After every commit | If the commit **deleted or renamed** a file: (1) prunes that file's stale rows from the Brain RAG vector store (`brain_documents`), and (2) appends the path(s) to `.brain-moves-pending` for integrity checking. No-op for ordinary edits. |
| `pre-push` | Before every push | Runs the full 5-flag `validate-brain` suite and **blocks the push** if the corpus-wide error count would exceed the tracked baseline (`hooks/validate-baseline.json`). |

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

- **Blocks on new errors only, against a tracked baseline** (`hooks/validate-baseline.json`) —
  never on any error. A block-on-any gate would be red from day one in `core/engine-rs` (it
  carries committed `E_GRAPH_DANGLING_RELATED` errors as of this writing) and would just get
  bypassed with `--no-verify` forever.
- **The baseline is corpus-wide and singular, not per-repo.** `validate-brain` resolves the brain
  root by walking *up* from wherever it runs and always validates the **entire corpus** regardless
  of cwd — so a push from `core/engine-rs` and a push from HQ see the identical error set. The
  baseline file lives in HQ only and is read read-only from every sub-repo via the resolved brain
  root.
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
no real `bastion`/`mev` binary, database, or network. It covers under/at/over-baseline,
`bastion`-absent, no-`brain.toml`, missing-baseline-treated-as-0, and warnings-never-block.

```bash
bash hooks/test_pre_push.sh   # exit 0 = all pass
```
