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

To ask the whole-corpus question deliberately:

```bash
PREPUSH_STRICT=1 git push      # gate on everything, not just your delta
./scripts/validate_brain.sh    # same question without pushing — but see below
```

> `validate_brain.sh` ends in an `emit-state --write`. It is **not** read-only, and it is banned
> during a measurement embargo. For a read-only answer use the `validate-brain --<flag>` calls above.

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
