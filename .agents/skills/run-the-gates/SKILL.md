---
name: run-the-gates
description: >
  How to run this fleet's validation gates and get an answer you can trust — why validate-
  brain's flags must be one per invocation, why a piped command's exit code is the pipe's and
  not the command's, why a red gate is often another session's file rather than your change,
  and which checks deliberately do not gate. Use BEFORE running validate-brain, harness.json
  checks, or a push gate, and whenever a gate result looks wrong, unrelated to your change, or
  suspiciously green.
---

# Running the gates

> **Paths below are relative to the brain root** — the directory containing `brain.toml`, found by
> walking up from wherever you are. This skill is synced into every repo, so a repo-relative link
> would be wrong in most of them.

Most wasted gate time here is not a broken gate. It is a **true result to a question you did not
mean to ask.**

## 1. One flag per invocation — they do not compose

`validate-brain`'s mode flags dispatch through an if/else-if chain with a fixed precedence:

```
--links > --structure > --state > --graph > --sync > (base)
```

Pass two and the loser is **silently ignored** — you get a real, passing result for a check you
didn't run. Run them separately, always:

```bash
bastion validate-brain --structure   # index.md <-> directory coverage
bastion validate-brain --links       # dead markdown / file:// / [[wikilink]] targets
bastion validate-brain --graph       # related: edge integrity
bastion validate-brain --state       # state.json schema + cross-repo block graph
```

`mev validate-brain` has the same flags — `bastion` delegates to `mev`. HQ's `planning/harness.json`
gates on the `bastion` form; prefer it for consistency.

Other verbs do **not** share this shape: `bastion brain` / `bastion code` use a clap `ArgGroup`
(hard error), and `syn queries` uses an explicit `parser.error`. The silent-drop is specific to
`validate-brain`.

## 2. A piped command's `$?` is the pipe's, not the command's

```bash
mev conformance | tail -3     # prints failures, reports success
```

This has cost real runs. Redirect, then check:

```bash
mev conformance > /tmp/out.txt 2>&1; rc=$?; tail -3 /tmp/out.txt; echo "rc=$rc"
```

Same trap with `| head`, `| grep`, `| jq`. If you only need the tail of the output, still capture the
exit code separately.

## 3. A red gate is frequently not yours

Attribution is **by delta, not by path** (`docs/decisions/D64-push-gate-delta-attribution.md`): stage
1 of the push gate validates the whole corpus but blocks only on errors **new since this clone's last
successful push**. With several agents live, `--structure` in particular goes red because another
session added a file and has not written its `index.md` row yet.

**Read the error's path before assuming it is your change.** If the file is one you did not create,
the session that created it owns the fix — say so rather than racing its edit.

To ask the whole-corpus question deliberately, without pushing:

```bash
PREPUSH_STRICT=1 git push      # gate on everything, not just your delta
```

`./scripts/sync/validate_brain.sh` looks like the read-only equivalent but is not one — it ends in an
`emit-state --write`, and on a `primary` host that write is followed by a commit and a push (see
`derive-state-safely`). It is **banned** during a measurement embargo. For a read-only whole-corpus
answer, run the four `bastion validate-brain --<flag>` calls above instead, one per flag.

**Note the `sync/`.** There is no `./scripts/validate_brain.sh` — that path exits **127**, which
reads as a failed corpus gate rather than a missing file, so a lane that copies it concludes the
fleet is red. Measured 2026-08-28: four lanes hit this in one night, and one then reached for the
path that does exist and ran the writer as its closing check.

## 4. Know which checks do not gate

HQ's `planning/harness.json` is the authority (the table in `CLAUDE.md` is a convenience copy and has
drifted before — trust the JSON). Notably `conformance` is **non-gating**: its
`toolchain-freshness` check drifts whenever a source tree is ahead of the installed binary, which is
normal mid-flight. A red `conformance` is information, not a blocker — but if it names
`toolchain-freshness` and you are about to run any `--write` command, rebuild first.

## 5. A gate can be green or red for the wrong reason

- **Worktrees contaminate root-level globs.** While `git worktree list` shows more than one entry, a
  root `npm test` / `npm run lint` globs into `.claude/worktrees/` and reports another agent's
  in-flight failures as yours — once 1388 lint errors and 11 test failures that belonged to a
  different tree. Scope the command or check `git worktree list` first.
- **A `_`-prefixed file is invisible to the corpus checks.** Name a debug probe `_zz_test.md` and
  `validate-brain` will not see it; you will conclude detection is broken when it is working.
- **One bad frontmatter scalar fails all four flags at once.** A `: ` inside an unquoted
  `description:` breaks YAML parsing, and every flag loads the same frontmatter — so an unrelated
  change looks like four broken gates. See the `write-okf-markdown` skill.
- **`timeout` does not exist on this macOS shell.** A command that hangs will hang; do not wrap it in
  `timeout` and assume a bound.

## 5b. When `--graph` is red, look at the graph

`mev validate-brain --graph` names the broken edges one line at a time, which is hard to read when a
rename broke a cluster. Two non-gating instruments show the shape instead:

```bash
mev emit-graph --pretty | head -40        # nodes / related: edges / leaves as JSON — writes nothing
mev generate-graph                        # interactive HTML — WRITES to <root>/planning/doc-graph
mev generate-graph --out /tmp/docgraph    # ...send it somewhere disposable instead
```

**`generate-graph` writes files.** Its default output lands inside `planning/`, which this one HQ git
repo tracks — so an unscoped `git commit` afterwards sweeps the artifact in (see
`commit-in-this-fleet`). Pass `--out` to a scratch directory unless you actually want the artifact
committed. Neither verb is a gate, and neither is in `harness.json`; they are for reading a failure,
not for proving one is fixed — re-run `validate-brain --graph` for that.

For "what breaks if I change this doc", use the `check-blast-radius` skill: `bastion brain` answers
a different question (`[[wikilinks]]`, not `related:`) and reports an empty result with exit 0.

## 6. Report what actually ran

If a check was skipped, say so. If it failed, quote the real error text rather than summarising it.
A gate reported as passing when it was never executed is worse than a red one, because the next
session builds on it.

## Checklist

- [ ] One flag per `validate-brain` invocation
- [ ] Exit code captured without a pipe in between
- [ ] Red gate's path checked against what you actually changed
- [ ] `git worktree list` checked before trusting a root-level test/lint result
- [ ] Non-gating checks (`conformance`) not treated as blockers — and vice versa
- [ ] Any skipped check named explicitly in the report
