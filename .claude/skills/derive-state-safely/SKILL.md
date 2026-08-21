---
name: derive-state-safely
description: How to run `mev emit-state --write` without reverting generated boards or clobbering other lanes — why a stale installed binary silently rewrites surfaces in an old format, why installing (not merging or pushing) is the delivery boundary on this machine, the measurement embargo that bans the command outright, and the fact that it rewrites the whole corpus rather than your repo. Use BEFORE any emit-state --write, validate_brain.sh or routine.sh run, and when generated boards, focus lines or lane JSON look wrong or regressed.
allowed-tools: Bash(mev:*) Bash(bastion:*) Bash(cargo:*) Bash(git:*) Bash(ls:*) Bash(grep:*)
---

# Running `emit-state --write` safely

> **Paths below are relative to the brain root** — the directory containing `brain.toml`, found by
> walking up from wherever you are. This skill is synced into every repo, so a repo-relative link
> would be wrong in most of them.

This is the fleet's one **destructive** routine command. It rewrites generated surfaces across every
repo from authored state. Run it with a stale binary and it rewrites them in an **older format** —
that is a silent regression, not an error.

The same warning applies to anything that ends in it: `./scripts/validate_brain.sh` and
`scripts/routine.sh` both call an `emit-state --write`.

## Step 1 — Rebuild first. This is not optional.

```bash
mev conformance --check toolchain-freshness
```

Drifted output names the two commits and says *"rebuild before any --write run"*. Then:

```bash
cargo install --path core/mev
cargo install --path core/bastion    # bastion embeds mev as a path lib — a stale bastion carries a stale mev
```

**Why this matters more than it sounds.** Running `emit-state --write` from a stale binary has
already reverted the generated Attention lanes in two `status.md` files to an older format; both had
to be restored with `git checkout` (`base-template:installed-mev-and-bastion-are-stale`).

**The non-obvious rule behind it — on this machine the install, not the merge or the push, is the
delivery boundary.** A block can be `closed`, merged to local main, and still deliver nothing
(`mev:closed-but-uninstalled-reads-as-delivered-downstream`): `emit-state` cleared the closed block's
edges so a downstream board showed work as startable, while the installed binaries predated it,
`mev lanes` exited 2, and the lane JSON files did not exist on disk. Every `emit-state` run that day
used the stale binary, so the planners never ran.

**And the install trigger misses locally-authored work.** `~/.cargo/bin/mev` is a real installed copy
that only refreshes on an explicit `cargo install`; the build script runs it only when a *pull*
brought new commits — so the machine that **authors** commits never trips it and silently drifts,
while the Mac Mini self-heals on its next cron pull
(`mev:mev-install-trigger-misses-locally-authored-commits`). The pre-push advisory is non-blocking.
Manual fix: `cargo install --path core/mev --force`.

**`BRAIN_ROLE` gates two scripts, not this command.** Only `scripts/commit_routine_updates.sh` and
`scripts/validate_brain.sh` check it — `grep -rn BRAIN_ROLE scripts/*.sh` is the full consumer list.
Neither the `bastion` nor the `mev` **binary** checks it at all
(`grep -rn BRAIN_ROLE core/bastion/src core/mev/src` is empty), so calling `bastion emit-state --write`
or `mev set-block-status ... --write` directly — the interactive/agent path — is never gated by it,
regardless of the host's role or when it was last set. The gate exists for exactly one thing:
`scripts/routine.sh`'s **unattended nightly cron** run, where `validate_brain.sh` runs `emit-state`
read-only unless `BRAIN_ROLE=primary`, and `commit_routine_updates.sh` stages/commits/pushes nothing
unless `BRAIN_ROLE=primary`. Do not read a `replica` role as a reason an interactive session's
`--write` call was somehow unsafe or unauthorized — it wasn't gated either way.

## Step 2 — Check the embargo

While any **measurement** block is live, `syn refresh` / `syn ingest` / `syn prune` /
`emit-state --write` / `routine.sh` / `validate_brain.sh` are **banned** — corpus changes invalidate
a retrieval measurement in flight. The embargo is declared in `planning/close-the-loop/roadmap.md`
and `lane-substrate.json` (converted from `.txt` by HQ.8.A). Check the orchestrator's status before writing; if a measurement chain is
running, use read-only `bastion validate-brain --<flag>` instead.

## Step 3 — Author the state first; the sync is one-way

`emit-state` **never** infers completion from `status.md`. If a block closed, set its `status` to
`closed` in `tracks[].blocks[]` *before* running — that authored field is the input the derivation
reads. Skipping it leaves `focus` and every generated surface stale until someone reconciles by hand.

Do not hand-write anything `emit-state` owns: focus scalars, cache `synced_from` watermarks, tier
rollup tables, the HQ boards, master-plan wave tables, the lane JSONs. Editing those by hand
duplicates the derivation engine and drifts from it.

## Step 4 — Know the blast radius before you commit

**It regenerates the whole corpus spine, not the repo you ran it from.** One run has modified
`core/_planning/bastion/state.json`, `core/_planning/engine-rs/state.json`, `README.md`,
`client/_planning/brazilianportugui/status.md` and the tier/HQ rollups in a single pass —
several carrying **other sessions' uncommitted work**
(`bastion-web:emit-state-rewrites-sibling-repos`).

**Why another repo's file changed even though you never touched that repo.** This is not a
mismatch being reconciled — `focus`, `tasks`, brain `repos[]`/`cross_repo[]` and the master-plan
wave tables are a **materialized view of the fleet-wide dependency graph, stored per-repo**
(`docs/state/state-schema.md`'s "Authored vs derived" table — those fields are explicitly caches,
kept on disk "for human readability and the future UI," never hand-edited). `depends_on` edges
cross repos, so closing a block in one repo can flip `focus.next` in several others — their
*authored* data never changed, only their *derived* cache did. That is expected, not a bug to chase.

**Don't hand-craft the commit pathspec from `git status`, and don't call `bastion emit-state
--write` directly.** Call `./scripts/emit_state_write.sh` instead — it's the one place the
write-then-commit sequence is defined. It runs `emit-state` (write on primary, read-only on
replica, same `BRAIN_ROLE` gate as always), writes every touched path to `$LOG_DIR/.emit_wrote`,
and on a primary immediately calls `commit_routine_updates.sh` to stage and commit **exactly**
that manifest — nothing else, never `git add -A`. `validate_brain.sh` delegates to this same
script for its own emit-state step, so the two are identical here; use `emit_state_write.sh`
directly when you only need the write-and-commit, without a full validate-brain pass first.

**`commit_routine_updates.sh` resolves every manifest path with `realpath` before staging, and
stages one path at a time.** `bastion`'s `I_EMIT_WROTE` lines report a repo's path through its
`planning/` symlink face (e.g. `base-template/planning/state.json`), and `git add` cannot cross
that boundary — worse, one such path in a single batched `git add` call fails the **whole** call,
so *nothing* gets staged and the script reports "clean" while everything sits dirty. Measured
2026-08-21: one symlinked path in an 8-entry manifest silently blocked all 8. If you ever see
`emit-state`/`validate_brain.sh` report success but `git status` still shows derived files dirty
afterward, check the log for a `[FAILED TO STAGE]` line before assuming the run did nothing.

Reading `git status` and building a pathspec yourself is the fallback only for the rare case you
ran `bastion emit-state --write` directly (bypassing the wrapper) — prefer the script.

**A tempting-sounding wrong fix: merging everything into one `state.json`.** The instinct is half
right — the actual problem is that derived caches are stored *inside* the authored files, which is
what lets one repo's emit-state run dirty a sibling's file. But merging authored state across repos
would make it worse: concurrent lanes would serialize onto one file and conflict on every write, and
you'd lose `mev validate-state <path>` working standalone with no `brain.toml` needed, plus any
per-repo `--scope` filtering. The fix that actually helps is what's already in Step 4 above:
authored stays per-repo, derived caches are a **convenience copy** regenerated on demand — nothing
depends on them being co-located, so keeping them synced automatically (as above) is enough.

## Step 5 — Read the run's warnings

- `I_EMIT_WROTE` — informational, one per surface written. This is your blast-radius list; read it.
- `W_EMIT_NO_SENTINEL` — a target has no `<!-- BEGIN generated:… -->` sentinel pair, so the emit was
  skipped. Most of these are long-standing (`master-plan.md` files fleet-wide). **Never hand-author
  a missing sentinel into prose** to make the warning go away — that is a separate fix to the target
  doc.

## Checklist

- [ ] `mev conformance --check toolchain-freshness` passes, or both binaries were reinstalled
- [ ] No measurement block is live
- [ ] Block statuses authored **before** the run, not after
- [ ] Blast radius read from the `I_EMIT_WROTE` lines
- [ ] Ran via `./scripts/emit_state_write.sh` (or `validate_brain.sh`, which delegates to it) —
      never `bastion emit-state --write` directly — so the touched paths are committed
      automatically, not left dirty for a later manual sweep
- [ ] If the run reported success but files are still dirty afterward, checked the log for a
      `[FAILED TO STAGE]` line rather than assuming nothing happened
- [ ] If committing by hand instead: pathspec scoped to `.emit_wrote`'s contents, each path
      resolved through `realpath` first, other lanes' files left alone, never `git add -A`
- [ ] Generated boards spot-checked — a format regression looks like a successful run
