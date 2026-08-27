---
name: fleet-push-discipline
description: How to push this fleet to GitHub — and get a repo's own hosted CI green — without breaking a sibling's CI, wasting 30+ minutes on a redundant per-push gate, blocking every repo's push on one stale doc, or chasing a CI failure that has nothing to do with your code. Covers the Cargo dependency graph, git_push.sh's dependency ordering + ci-blocked gate, why pre-push hooks are disabled fleet-wide, the bash-3.2/python-f-string traps that bit this exact work, and the recurring "public CI checkout can't see the private HQ vault" failure class (checkout-depth bugs, stale CI mock fixtures, floating toolchain channels, missing CI service setup). Use BEFORE running scripts/git_push.sh, before pushing any core/* repo directly, when a push fails on E_SYNC_DRIFT or a corpus error unrelated to your change, and when a push (or a repo's hosted CI) is red for a reason you can't immediately explain.
allowed-tools: Bash(git:*) Bash(bash:*) Bash(python3:*) Bash(gh:*) Bash(grep:*) Bash(cargo:*)
---

# Fleet push discipline

> **Paths below are relative to the brain root** — the directory containing `brain.toml`, found by
> walking up from wherever you are. This skill is synced into every repo, so a repo-relative link
> would be wrong in most of them. Remember its **rule 1** from inside any sub-repo.

## 1. Never `git push` a `core/*` repo directly

Every `core/*` repo's own `CLAUDE.md` carries a standing rule saying so (added 2026-08-24). Always
push through `scripts/git_push.sh` from HQ root instead:

```bash
./scripts/git_push.sh --all          # whole fleet, dependency order
./scripts/git_push.sh core/mev core/engine-rs core/bastion   # explicit subset, YOUR order
```

Committing, branching, and merging PRs to `main` **locally** inside a sub-repo are all fine — it's
only the final `git push` of `main` to `origin` that must go through the script.

## 2. Why order matters: the Cargo dependency graph

```
bastion    -> bella, engine-rs, mev, okf-core
engine-rs  -> claude-code-rs, mev, okf-core
mev        -> okf-core
```

Every Rust repo's CI clones its sibling path-deps at their **unpinned default branch**
(`gh repo clone ... --depth 1`, in base-template's `gate-rust.yml`). Push a dependent before its
dependency and CI builds the OLD dependency and fails on code that is perfectly fine locally. This
happened for real on 2026-08-18: `bastion` red with `cannot find function lanes_brain in crate mev`
purely because `mev` sat 23 commits behind on its remote.

`git_push.sh` (default and `--all` scans, not an explicit repo list) now reorders to
`scripts/preflight_status.py --plan-json`'s dependency-first push order automatically. Run
`./scripts/preflight.sh status` first if you want to see the plan before running it for real — it
writes and pushes nothing, ~6s for the whole fleet.

## 3. The `ci-blocked` gate — and what it does NOT catch

`git_push.sh` skips (does not push) a repo whose Cargo dependency is **currently red on GitHub with
nothing queued in this batch to fix it** — pushing on top just wastes a push/CI cycle on a
combination already known to fail. Override with `--ignore-ci-blocked` if you really mean to push
anyway. This is distinct from:

- **`will-break-on-push`** (both sides unpushed) — ordering alone fixes it, not gated, just reordered.
- **A repo's own CI being red** — pushing more of ITS OWN commits is often exactly the fix, so
  self-red never blocks that repo's own push.

`git_sync.sh` (routine.sh Step 1) has the mirror-image guard: it skips **pulling** any Cargo-chain
repo whose latest GitHub CI is red, so the Mini never hands `build_and_install.sh` (Step 2) a
known-broken sibling to compile against every single night until a human pushes a fix.

Both gates fail OPEN on any `python3`/`gh`/network hiccup — never let a lookup failure block a
whole sync or push run.

## 4. `hooks/pre-push` is disabled fleet-wide (as of 2026-08-24) — know why before re-enabling it

Every repo's `hooks/pre-push` is `chmod -x`'d (git silently skips a non-executable hook; check with
`git config core.hooksPath` — it's still set to `hooks`, just the file itself is inert). This was an
explicit, repeated operator request, not an accident. The reason: the hook is shared fleet-wide via
`core.hooksPath`, so **every single `git push`, in every repo, re-ran a corpus-wide `validate-brain`
gate (5 checks, ~15-20s) plus that repo's own full `fmt`/`clippy`/`test`/`build` suite** — for a
whole-fleet retry push, this stacked into 35+ *minutes* stalled on one repo's `cargo test` (mev, via
an apparent network hiccup fetching over SSH) and 9 outright failures from one unrelated corpus
error (see §5) before the real work even started.

If you ever need the safety net back: `chmod +x hooks/pre-push` per repo (the script and its 44-case
test suite, `hooks/test_pre_push.sh`, are untouched). Prefer running the checks explicitly instead —
`./scripts/preflight.sh repo <slug>` runs one repo's own gated checks on demand, and
`./scripts/preflight.sh corpus --state` (etc.) runs the corpus checks once for the whole fleet
instead of once per push.

## 5. One stale doc can block the ENTIRE fleet's pushes

`bastion validate-brain --sync` — part of the (now-optional) pre-push corpus gate, and something
`bastion validate-brain --sync` on its own will always still tell you — compares each `[[repos]]`
entry's `status_file` `timestamp` against its `cache_doc`'s `synced_from` in `brain.toml`. If a
sub-repo's status timestamp moves (e.g. from its own push) and the brain's cached project doc
(`docs/projects/<repo>.md`) isn't re-synced, **every repo's push** — not just that one repo's — hits
`E_SYNC_DRIFT` and fails, because the gate is corpus-wide, not repo-scoped. Fix:
`/sync-status <repo-slug>` (updates `synced_from` to match verbatim), then re-run
`bastion validate-brain --sync` to confirm 0 errors before retrying pushes. This is exactly what ate
9 of the first 18 pushes in the 2026-08-24 incident this skill documents.

## 6. Two bash/python traps this exact work hit — don't reintroduce them

- **No `declare -A` in `git_push.sh`/`git_sync.sh`.** The Mac Mini's `/bin/bash` is macOS's stock
  3.2 (pre-4.0) — `declare -A` fails with `declare: -A: invalid option` and silently degrades
  whatever depended on it to a no-op the first time the script runs unattended. Use a
  `printf '%s\n' "$@" | grep -Fxq -- "$needle"` linear-scan helper instead (see `list_has()` in
  `git_push.sh`) — the same posture `build_and_install.sh`'s `repo_dependents_of()`/`seen_in()`
  already document at length.
- **No `\"` inside an f-string's `{...}` in an embedded `python3 -c '...'` block.** `f"{d[\"k\"]}"`
  is a `SyntaxError: unexpected character after line continuation character` even on Python 3.14 —
  it looks like it should work (PEP 701) but doesn't in this exact shape. Assign the dict value to a
  plain variable first (`v = d["k"]`) and interpolate that, or use string concatenation
  (`d["k"] + "\t" + v`) instead of an f-string with a nested same-quote subscript.

## 7. Build caching — the other half of "why is this slow"

`build_and_install.sh` (routine.sh Step 2) skips `cargo check` entirely for a Rust repo that's
outside the run's reverse-dependency closure over `.changed_repos` AND already passed check at the
exact commit it's still sitting at (`logs/.last_good_check`, one line per repo, not truncated
between runs). If a "why is this rebuilding everything" complaint resurfaces, check that file exists
and is being read/written (`git status`/`cat logs/.last_good_check` from HQ root) before assuming
the closure logic itself is broken — a missing/corrupt file fails OPEN (checks everything), which
looks identical to the skip logic never having run at all.

## 8. "My CI is red and it's not my code" — the public-checkout-can't-see-the-vault class

Every `core/*` repo's `planning/` is a gitignored symlink into this private brain repo's vault
(standing rule everywhere: "Planning symlinks", each repo's `CLAUDE.md`). A repo's own **hosted
CI** clones only that one public repo — no vault, no HQ tree, no sibling brain checkout unless a
workflow explicitly adds one. Any test or CI step that assumes the vault (or the wider fleet) is
present will fail in hosted CI for reasons that have nothing to do with the code just pushed. This
bit three different repos on 2026-08-24, three different ways — recognize the shape before assuming
you introduced a regression:

- **A test reads a real file that only exists locally.** `engine-rs`'s
  `harness_json_sdlc_task_policy_key_set_matches_partial_sdlc_task_policy_fields` read
  `planning/harness.json` off disk unconditionally and panicked in CI where it's absent. Its own
  sibling test in the same file already had the right pattern —
  `let Ok(raw) = std::fs::read_to_string(path) else { return };` (skip, don't fail, "a guard for
  this working tree, not a hard dependency") — the fix was just making the broken test match its
  own neighbor. **Before building a CI-side mock/fixture pipeline for this, check whether a
  sibling test already solved it by skipping** — that's usually the right answer, not a bigger
  fixture-provisioning mechanism (an earlier draft of this exact fix over-built one before
  noticing the simpler sibling pattern).
- **A test computes an "HQ root" by counting parent directories.** `orchestrator`'s
  `test_expect_docs_paths_exist_on_disk` computed `_HQ_ROOT` as
  `Path(__file__).resolve().parents[4]`, mirroring the real fleet's `HQ_ROOT/core/orchestrator/…`
  depth — correct in the real repo, but CI's checkout put the repo directly at the workspace root
  (one level shallower), so `_HQ_ROOT` resolved to an unrelated runner directory and the test was
  structurally guaranteed to fail regardless of fixture content. Fixed in
  `base-template/.github/workflows/gate-python-uv.yml` by checking the repo out at a nested path
  (`mock-hq/core/orchestrator`) that reproduces the real depth, with
  `defaults.run.working-directory` pointing every step there. **When a "resolve N parents up" test
  fails only in CI, recompute what that resolves to under CI's actual checkout layout before
  assuming the fixture content is wrong.**
- **A CI service container is missing an extension/schema the app needs.** `orchestrator`'s
  Postgres service started empty (no migrations ever ran against it — added an `alembic upgrade
  head` step), and once migrations ran they immediately hit `FeatureNotSupported: extension
  "vector" is not available` because the plain `postgres:15` image has no pgvector. **Verify a
  replacement service image locally with `docker run` before trusting a docs mention of it** — this
  repo's own docs pointed at `supabase/postgres:15.8.1`, which turned out to need Supabase's full
  bootstrap stack (a `supabase_admin` role, JWT secrets) and crash-loops on a bare
  `POSTGRES_USER/PASSWORD/DB` service container; `pgvector/pgvector:pg15` (the extension project's
  own minimal image) was the one that actually worked, confirmed by running the real migration
  chain and a real test file against it before pushing.
- **CI runs a floating toolchain channel that has moved past local dev.** `bastion-ui`'s CI installs
  Flutter's `stable` channel unpinned (`subosito/flutter-action`, no `flutter-version` input) — it
  had drifted to 3.47.1 while local dev sat on 3.44.4, and `dart format`'s output differs enough
  across that gap that 3 files were clean locally but failed CI's `--set-exit-if-changed` check on
  every push. `flutter --version` in the failed run's log named the exact version; `flutter
  upgrade` locally to match it, then reformat, is the fix — not hand-editing the 3 files to guess
  at a newer formatter's opinion.

## Quick checklist before a fleet push

1. `./scripts/preflight.sh status` — read the plan, note anything `ci-blocked` or `ci RED`.
2. `bastion validate-brain --sync` (and the other four flags, one at a time) — 0 errors, or you
   know exactly which pre-existing errors are not yours.
3. `./scripts/git_push.sh --all` (or an explicit ordered subset) from HQ root — never a bare
   `git push` inside a `core/*` repo.
4. If something fails on a corpus error you didn't cause: fix it (§5 is the most common shape),
   commit, re-run — don't `--no-verify` past it or chase it repo-by-repo.
5. If a repo's own hosted CI is red for a reason your local run never sees: check §8 before
   assuming it's a real regression — verify what CI's actual checkout/service setup gives the
   failing test, locally, before touching the test or the fixture.
