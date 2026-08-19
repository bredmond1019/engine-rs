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

## Step 2 — Check the embargo

While any **measurement** block is live, `syn refresh` / `syn ingest` / `syn prune` /
`emit-state --write` / `routine.sh` / `validate_brain.sh` are **banned** — corpus changes invalidate
a retrieval measurement in flight. The embargo is declared in `planning/close-the-loop/roadmap.md`
and `lane-substrate.txt`. Check the orchestrator's status before writing; if a measurement chain is
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

So: read `git status` afterwards, commit with a pathspec scoped to what you own, and leave the
rollups unstaged for whoever owns them. Never `git add -A` after this command.

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
- [ ] Commit scoped by explicit pathspec; other lanes' files left alone
- [ ] Generated boards spot-checked — a format regression looks like a successful run
