---
name: sdlc-flow
description: >
  Run a spec sequentially on one branch (or --worktree) with a per-task test→fix loop, one end review, a docs patch, and a PR
---

=============================================================================
 sdlc-flow — single-branch, single-review, PR-terminating SDLC engine
 =============================================================================

 The default engine for non-trivial feature work. Runs one spec's tasks
 SEQUENTIALLY on a SINGLE shared branch (so there are no inter-task merges to
 conflict), with a per-task test→fix loop, ONE
 consolidated review at the end, a docs patch, and a PR as the terminal step.

 ISOLATION MODE
   Default: a plain branch (<spec>-flow) checked out IN THE MAIN WORKING TREE. No
   sparse-checkout worktree, so a relative planning/ symlink (brain-vaulted repos)
   stays intact. main is left on the branch until the PR merges.
   --worktree: the isolated sparse-checkout worktree under trees/<spec>-flow/ —
   opt in when you need true isolation.

 A compact, COMMITTED, AUTHORITATIVE state.json + one worklog.md replace the 5×N
 per-stage report files: resume + review + wrap-up read a structured index instead
 of re-reading verbose prose. This inverts the harness's usual "committed report
 files are authoritative, state JSON is gitignored" rule on purpose (see D31).

 USAGE
   /sdlc-flow <spec-slug>                  run every task in the spec, open a PR, stop
   /sdlc-flow <spec-slug> 1-3              scope to a task range (1-3, 1,3,5, 5)
   /sdlc-flow <spec-slug> --auto-merge     merge the PR + clean up on success
   /sdlc-flow <spec-slug> --no-pr          stop after wrap-up; do not create a PR
   /sdlc-flow <spec-slug> --worktree       run in an isolated worktree (default: plain branch)
   /sdlc-flow <spec-slug> --resume         re-attach the branch/worktree, resume from state.json
   /sdlc-flow <spec-slug> --test-depth full  run the FULL gating suite per task (default: fast)

 PIPELINE
   worktree-setup → enumerate (D16 lint) → [resume load] → per-task loop
     → end-review → docs (gated on PASS) → wrap-up(PR)

   Per-task loop (sequential, on the one branch):
     implement → fast-test → (triage → fix/​bail) ×≤3
     One state-commit per task. A triage MAJOR / immediate-bail reason breaks
     straight to wrap-up (draft PR) — it does NOT burn three attempts.
     Triage's "same failure, no progress" must be measured THIS attempt: a work-assertion,
     vault-commit or removed-literal-scan failure precedes the test stage, so gate_results/issues
     may be carried over from an earlier attempt (DATA FRESHNESS WARNING) and never alone justify
     sameFailureAsBefore=true.

   End-review: ONE review over the integrated tree, fed state.json as the index but
   reading `git diff <prBase>..HEAD` + tasks.md criteria directly + the AGGREGATED
   gate_results already recorded by every task's own fast-test stage (latest entry
   per check_id wins) instead of re-running the full gating suite from scratch — a
   check with no recorded entry is run directly, and on the FINAL review pass every
   still-failing check is re-run fresh as the authoritative last word before a bail.
   PASS → docs; FAIL/PARTIAL → a `gapKind` of 'design'/'operator' stops the loop
   immediately (no further review/suite pass); otherwise triage findings:
   small/localized → bounded fix→test→review (≤2, Opus last); broad → bail.

 COMMIT STRATEGY (crash recovery — everything lands on the branch)
   feat: implement <stem> task N      implement agent (per task)
   fix:  fix pass P for <stem> task N  fix agent (per pass)
   chore: flow state — <label>         state-writer (state.json + worklog.md + checkbox)
   docs: update docs for <spec>        docs agent
   chore: wrap up <spec>               wrap-up agent (status/log/amendment-log)

 MODEL TIERING (the token lever — see the MODEL map below)
   haiku : setup, enumerate, scout/state-load, test, state-writer
   sonnet: implement, fix, review, triage, docs, wrap-up
   opus  : ESCALATION on the FINAL per-task fix pass and the FINAL review attempt

 STATE  (committed — NOT gitignored — at planning/<spec>/sdlc/)
   sdlc-flow-state.json   the authoritative run index (per-task summary/issues/fixes/commit)
   worklog.md             the human-readable trail — one short section per task
 =============================================================================

## Antigravity Execution Guide

When the user asks you to run `/sdlc-flow <spec-slug> [range]`, do NOT run `sdlc-flow.js`. Instead, perform the flow execution yourself:

1. **Setup — plain branch, or isolated worktree with `--worktree`**:
   - **Since `BT.ticket.prepare-run-replaces-setup-agents`, the real engine's setup phase runs ONE
     `prepare-run` agent** (whose whole job is `python3 .claude/workflows/bin/prepare_run.py
     --spec-slug <spec-slug>`, returning its JSON — repo root, vault detection, rendered flags,
     harness config, task enumeration, a lint/probe verdict) **before, at most, a second agent
     that creates/locates the worktree.** If `prepare_run.py` exists in this repo, run it directly
     with your own bash tools instead of hand-deriving these facts — you have exactly the shell
     access it needs. A `refused: true` result stops the whole run right here, before any branch
     or worktree work, reporting `reason` verbatim. `--resume` reads this same JSON back from the
     run's meta bundle (`sdlc-flow-state.json`'s `setup` field) rather than re-running it. If
     `prepare_run.py` does not exist yet in this repo, fall back to the manual steps below.
   - **Without `--worktree` (default):** check out branch `<spec-slug>-flow` IN THE MAIN WORKING
     TREE — no sparse-checkout worktree, so a relative `planning/` symlink (brain-vaulted repos) stays
     intact. `main` stays on the branch until the PR merges; refuse to start on a dirty working tree —
     **except** at a brain root (`brain.toml` present at `repoRoot`), where dirt confined entirely to a
     sibling repo's vaulted `_planning/<repo>/` path (`git status --porcelain | grep -v '_planning/'`
     comes back empty) does not count, since that dirt belongs to another lane's in-flight work, not
     this run — dirt outside `_planning/`, and any dirt at a non-brain root, still blocks exactly as
     before.
   - **With `--worktree`:** create (or, with `--resume`, reuse/re-attach) an isolated sparse-checkout
     worktree, mirroring `/sdlc-task`'s Steps 1b/1c (see `.agents/skills/sdlc-task/SKILL.md`) with the
     branch name `<spec-slug>-flow` instead of `<blockId>-task`:
     - `--resume`: try to reuse first — `git worktree list | grep "trees/<spec-slug>-flow"` and
       `git branch --list "<spec-slug>-flow"`. Worktree exists → reuse verbatim. Branch exists but
       worktree missing (orphaned) → re-attach with `git worktree add --no-checkout` (no `-b`). Neither
       exists → fall through to a fresh create.
     - Fresh create:
       ```
       mkdir -p trees
       git worktree add --no-checkout trees/<spec-slug>-flow -b <spec-slug>-flow
       git -C trees/<spec-slug>-flow sparse-checkout init --cone
       git -C trees/<spec-slug>-flow sparse-checkout set $(git ls-tree HEAD --name-only -d | tr '\n' ' ')
       git -C trees/<spec-slug>-flow checkout
       ```
       Then seed gitignored `.env`/`.env.*` files (same recipe as `/sdlc-task` Step 1b(f)) and commit
       `chore: init worktree <spec-slug>-flow --allow-empty`.
     - Repair the `planning/` symlink inside the worktree if the repo is brain-vaulted (same recipe as
       `/sdlc-task` Step 1c — absolute symlink to the same vault target, never relative, never a real
       directory).
     - `runDir = repoRoot/trees/<spec-slug>-flow`.
   - **Spec location.** Paths are `planning/<spec-slug>/...` at the git root by default. If no spec
     exists there, ALSO check `<invoking-dir-relative-to-root>/planning/<spec-slug>/...` — a
     sub-brain tier (e.g. `business/`) has its own `planning/` without being its own git repo. The
     ROOT always wins when a spec exists at both locations. If found at neither, abort and name BOTH
     paths you searched, not just one.
   - **Binding / brain-root / population guards** (BT.ticket.worktree-setup-can-adopt-the-brain-root-as-repo-root)
     — run these BEFORE the D16 preflight lint below and before any task work, comparing against the
     `repoRoot` you resolved at the top of this step (never re-derive it):
     - **BINDING GUARD.** `runGitCommonDir = git -C <runDir> rev-parse --path-format=absolute
       --git-common-dir`. If it does not resolve under `repoRoot`, abort — `Setup binding guard
       failed`, naming both `runGitCommonDir` and `repoRoot`. This is the check that catches the
       run silently adopting a different repo (e.g. the brain root) than the one it resolved.
     - **BRAIN-ROOT GUARD.** If `<runDir>/brain.toml` exists but a `brain.toml` did NOT exist at the
       invocation root, abort — `Setup binding guard failed`, naming both paths. Never identify a
       brain root by counting harness checks or by a hardcoded path — brain.toml presence at the two
       roots is the only signal.
     - **POPULATION GUARD (worktree mode only).** Every path in `git -C <runDir> ls-files` must exist on disk at `<runDir>/<path>`; if
       any are missing, abort — `Setup binding guard failed`, naming the missing count and up to
       five example paths.
     Log each guard's verdict, pass or fail — the transcript must show the check ran, not merely
     that nothing exploded.
   - Capture the **emoji-gate diff base**: `baseSha = git -C <runDir> rev-parse --short HEAD` — the
     HEAD sha as it stands right now, in the worktree/branch you just created/reused/re-attached,
     BEFORE any task commit. Persist it as `state.base_sha` (mirrors `/sdlc-task`'s Step 1d capture —
     see `.agents/skills/sdlc-task/SKILL.md`). Never persist `prBase`/`diffBase` (the PR base branch
     name, e.g. `main`) into this field — it is a branch tip, not a sha pinned to this run's start.
2. **D16 preflight lint — do not guess the task structure.**
   - If the spec's `tasks.json` already exists, skip to task execution.
   - If it is missing but `tasks.md` has derivable step content, derive a FRESH `tasks.json` from
     `tasks.md`'s step list plus its Acceptance Criteria / Validation Commands sections (a real
     decomposition, never a verbatim copy of the prose). Write it as a BARE ARRAY (D45 shape — not
     the superseded `{"tasks": [...]}` wrapper), each entry `{ task_id, title, description,
     acceptance_criteria, validation_commands, max_attempts, files, dependsOn }` — `task_id` a
     1-indexed integer in dependency order with no gaps, `max_attempts: 3`, and never author
     `status`/`attempt_count` (engine-owned). Commit it on the current branch with an explicit
     pathspec: `git add <tasksJsonFile>`, then run the COMMIT-SAFETY GUARD above and `git commit -m
     "chore: derive tasks.json from tasks.md (D16 fallback)"` as one `&&`-joined call. Log a
     distinct line — `Derived tasks.json from tasks.md (D16
     derive-from-tasks.md fallback) — <N> task(s), commit <hash>.`
   - **Per-task `validation_commands` scoping** — follow the convention documented at
     `.claude/commands/generate-tasks.md` (search it for "validation_commands"); do not restate the
     rubric in your own words, just apply it: `validation_commands` is `[]` for any task that
     touches source the project's checks compile or lint — those tasks fall back to the
     project-wide harness checks, which are authoritative for them. Set it ONLY for a task that
     CANNOT break the build (docs-only, config-only, fixture-only), with cheap commands that
     actually verify that task (file exists, frontmatter present, index updated). If you DO author
     an override that runs tests, it MUST target that task's own tests specifically — never a
     bare/positional filter that could silently match zero or the wrong tests — and a command
     matching nothing must fail rather than pass. Never hardcode a stack-specific command into
     this; that judgment belongs to whoever derives or authors the task at run time. Match the
     intent of the parallel generator in `sdlc-block.js` ("acceptance_criteria/validation_commands
     can stay `[]` per task").
   - **D63 — pure substitute, unchanged (this engine only, unlike `sdlc-task`'s augment-gating
     semantics):** a task whose `validation_commands` is a non-empty array runs ONLY those commands
     on its per-task tripwire — zero `planning/harness.json` `gates:true` checks — and the end
     review's full gating suite is the backstop that still runs everything at the end.
   - **`expect_red` (D68) — an INVERTED verdict, easy to get backwards.** A task may carry
     `"expect_red": ["<command>", ...]`, and every command listed there must also appear in that
     same task's own `validation_commands`. Each named command **passes on a NON-ZERO exit and
     fails on exit 0** — the opposite of every other check. This is for a task whose deliverable IS
     a test observed failing; a zero exit means the deliverable is missing, not that the task
     succeeded. Every other check on that task's list is judged normally. If an `expect_red` entry
     names a command that is NOT in that task's `validation_commands`, that is a hard spec error:
     abort with `ABORTED (spec error)` — never silently ignore it, and never invert a project-wide
     `gates:true` harness check, which `expect_red` can never reach.
   - **The derive source follows `specSource`.** If the spec came from an authored block record
     (`planning/blocks/<BlockID>.json`), derive `tasks.json` by decomposing its `what`, `why`,
     `files`, `acceptance_criteria`, `testing_strategy` and `validation_commands`. Otherwise derive
     from `tasks.md`'s step list. Either way it is a real decomposition, never a verbatim copy.
   - Only if that source is also missing, or has no derivable content, abort: report `ABORTED
     (D16)` and tell the user to run `/generate-tasks <blockId>` to author `tasks.json`, commit,
     then re-run. Deriving from an authored block record or `tasks.md` is not guessing the task
     structure; fabricating one from nothing is what D16 still refuses to do.
3. **Execute Tasks sequentially in the worktree**:
   - For each task in the specified range (or all if not specified):
     - Run `/update-task` to flip status to `In progress` in the worklog and local files.
     - **STEP 0 — started marker (implement attempt only, `BT.ticket.per-task-state-write-before-implement`)**:
       the same marker as `/sdlc-task` (see `.agents/skills/sdlc-task/SKILL.md`'s Step 3), written
       against `sdlc-flow-state.json` instead of `sdlc-task-state.json`. On attempt 1 with no
       `start_sha` recorded for this task, before reading, editing, or committing anything, merge
       `tasks["<taskNum>"] = {status: "running", start_sha: <git rev-parse --short HEAD>,
       marker_at: <UTC ISO now>}` into the state file, preserving every other key; a fix attempt
       never re-stamps `start_sha`. A task found at `running` on entry (a resumed crashed attempt)
       keeps its original `start_sha`, re-enters at implement attempt 1, and gets the same
       crashed-prior-attempt note as `/sdlc-task`. `/sdlc-flow`'s own work-assertion range is
       UNCHANGED by this — it stays `HEAD~1` with no `prevSha`, unlike `/sdlc-task`.
     - Implement the task following instructions.
     - Run fast validation tests. **Record `gate_results`** — one entry per check actually run
       this turn (same set, same order as the checklist): `{check_id, status: 'pass'|'fail',
       failing_ids}`, deriving `failing_ids` from structured runner output (nextest JUnit XML,
       pytest `--junitxml`/`-rf`) where available, else a single-element array holding `check_id`.
       This is what drives triage-only-on-red (triage is called ONLY from the failure branch — a
       green task spawns no triage agent) and the end-review stage's aggregated read below.
     - **Also re-stamp this lane's claim+lease heartbeat now** (best-effort, NEVER gating —
       `BT.ticket.lane-heartbeat-goes-stale-mid-block`, task 4; mirrors `/sdlc-task`'s identical
       per-task step, see `.agents/skills/sdlc-task/SKILL.md`): from the repo root, run
       `python3 scripts/lane_heartbeat.py --agent <this lane's agent identity> --repo <this
       repo's slug in brain.toml> --current-block <blockId> || true`. Same identity this lane
       already used to claim its lease — never a second, invented identity source. A lane driven
       by hand never reaches `/orchestrate`'s release-and-re-take, so this is the ONLY heartbeat
       re-stamp such a lane gets between block boundaries. If this lane holds no live claim or
       lease, the call fails harmlessly (`|| true`) — its exit code must never affect the task's
       own pass/fail verdict.
     - **Removed-literal scan (BT.ticket.failure-attribution-and-gate-cache, task 4)** — after the
       commit (and any vault commit) lands, before deciding whether to fix/retry: this task's own
       commit(s) may have REMOVED a string literal or identifier a test OUTSIDE this task's own
       `files[]` still references. Diff this task's commit range against `HEAD` (`--unified=0`),
       collect REMOVED lines, extract candidate literals — quoted strings ≥8 chars (config knob
       `minLiteralLen`) and bare identifiers (any with an underscore, or ≥12 chars underscore-free;
       config knob `identifierMinLen`) — then search every file matching the test-file convention
       regex (config knob `testGlobRegex`; all three via `planning/harness.json`'s optional
       `removedLiteralScan` object). No candidate test file at all → the scan is inconclusive
       (never read as "no hits"); proceed normally. A hit inside this task's own `files[]` is never
       reported. A hit outside `files[]` is **IN-SPEC DEBT, `failure_class: fixable`** — run it
       through the same triage/fix loop as any other failure, with the next fix pass's writable set
       WIDENED to include the hit file(s) in addition to this task's own `files[]`.
     - **Attribution look-back (BT.ticket.failure-attribution-and-gate-cache, task 3)** — before
       triaging a red gating check, ONLY when exactly one check is red this attempt and a real
       `check_id` resolved off this attempt's own `gate_results` (a mixed multi-check failure falls
       straight through to ordinary triage, the safe default): look back through this RUN's own
       recorded `gate_results` for the earliest earlier task that already ran this `check_id`. Green
       there → `ownership: self, failure_class: fixable` (introduced after that task — today's
       ordinary fix loop, UNCHANGED; the regression control). Red there (never fixed) → **IN-SPEC
       DEBT**: `ownership: self, failure_class: fixable`, declared by that earlier task; widen the
       next fix pass's writable set to that check's `failing_ids` plus this task's own `files[]`. No
       record in this run yet → consult the lazy cross-lane gate cache,
       `.claude/workflows/bin/gate_cache.py lookup --repo <slug> --base-sha <baseSha> --check-ids
       <check_id>`: a cache hit answers directly; a miss with a known command re-runs ONLY that one
       check id at `base_sha` in an isolated, throwaway `git worktree add --detach` (removed again
       immediately — never touches this run's own branch/tree), then `warm`s the cache with the
       result. Red at `base_sha` → `ownership: foreign` — **carried and recorded, no attempt burned,
       no bail**; treat this task's gating as clean and skip triage/fix entirely for this check.
       Green at `base_sha` (no history record either) → in-spec debt, `declaredByTask` unknown.
       Nothing decides (no history, cache unreachable) → fall straight through to ordinary triage,
       unchanged. Verdict vocabulary is `.claude/workflows/sdlc-state-vocab.json`'s `ownership`
       (`self`|`foreign`) / `failure_class` (`fixable`|`escalate` — debt still open at terminal
       review escalates) — never a third spelling.
     - Fix failures (up to 3 triage/fix attempts) — skipped entirely for a `foreign` verdict above
       (the check is already treated as clean); for an in-spec-debt verdict, the fix agent's
       writable set is the widened one from the attribution step.
     - Run the COMMIT-SAFETY GUARD above, `&&`-joined with the commit itself, then commit the task
       state on the branch (`feat: implement <slug> task N`). If a vault commit is also needed
       (D46), run the same guard against `git -C <vault path>` before that commit too.
     - **If this task's fix loop ended in a triage MAJOR or an exhausted-attempts bail
       (BT.ticket.bails-must-be-append-only):** append one fully-populated entry to the committed
       `state.json`'s top-level `bails` array — `{occurred_at, task_id, check_id, check_id_raw,
       failing_artifact, ownership, bail_class, reason, resolution: null}` — never overwrite or
       truncate the array; a second bail in the same run appends a second entry, the first stays
       byte-identical. **`check_id`** is resolved, not copied verbatim: take the last `status:
       'fail'` entry from this task's own recorded `gate_results` (falling back to this task's
       last `issues[]` entry only when no `gate_results` entry exists at all), then validate that
       candidate against `planning/harness.json`'s check names — a match is stored verbatim; a
       non-match stores `check_id: null` with the raw candidate preserved in `check_id_raw`, never
       silently dropped. The end-review bail site follows the identical rule but always candidates
       the literal `'review'` (its own pipeline-stage name, never a real harness check), so it
       always resolves to `check_id: null, check_id_raw: 'review'` by construction.
       `bail_reason` is still set too, as a plain mirror of the newest entry's `reason`. On
       `--resume`, read `state.json`'s prior `bails` array and carry it forward verbatim before
       appending anything new — re-initialising it instead of merging silently deletes a bail that
       was later retried successfully, which is the exact defect this record exists to prevent.
     - Immediately after that commit, run the POST-COMMIT WORK ASSERTION above (`&&`-joined onto
       the commit). A `WORK_ASSERTION_ABORT` means the commit did not actually contain the task's
       declared work — treat the task as failed and fix/re-commit before proceeding.
4. **Consolidated End-Review**:
   - Once all tasks are complete, read the aggregated `gate_results` already recorded by every
     task's own fast-test stage (latest entry per `check_id` wins) instead of re-running the full
     suite from scratch; run directly any check with no recorded entry, and on the FINAL review
     pass re-run every still-failing check fresh as the authoritative last word.
   - Run the acceptance criteria check.
   - If PASS -> proceed to docs. If FAIL/PARTIAL: a `gapKind` of 'design' or 'operator' stops the
     loop immediately (no further review/suite pass); otherwise run a targeted fix loop
     (capped at 2 review passes total, Opus on the last).
   - **`baseline-diff` checks fail CLOSED here** (BT.ticket.failure-attribution-and-gate-cache, task
     5 — D64: by delta, never path-scoped) — this end-review's aggregated read (and the FINAL
     review's fresh re-run of any still-failing check) is where a `perTask:false` baseline-diff
     check gets its one authoritative run. Net-new items absent from the baseline FAIL, naming them;
     pre-existing items never fail. Every one of the following ALSO fails, naming its own specific
     cause rather than being silently treated as zero items or skipped: the check's command output
     is empty, unparseable, or not a JSON array; the baseline file is missing; the baseline's sidecar
     `<baselinePath>.sha` is missing (`"baseline predates base-SHA stamping"`); or the sidecar's SHA
     disagrees with this run's own `state.base_sha` (names both SHAs). The sidecar is written
     alongside a NEWLY-created baseline snapshot only (never backfilled onto an existing one) at
     setup time, stamped with this run's own `base_sha`.
5. **Docs & Wrap-up**:
   - If PASS, run `/update-docs --patch` to update documentation, running the COMMIT-SAFETY GUARD
     `&&`-joined before the docs commit (and its vault counterpart, if any patched/created doc lives
     under `planning/`).
   - Update `planning/status.md` (surgical Edit). Before this edit, load the `write-okf-markdown`
     skill — this step edits an EXISTING file's YAML frontmatter, and that skill carries the
     frontmatter-must-start-at-line-1 rule and the insert-point trap (a row inserted after the
     OPENING `---` instead of the CLOSING one destroys the block, per E_SYNC_WATERMARK_MALFORMED).
     HQ standing rule 6 and base-template standing rule 11 both require this before writing or
     editing any `.md`. "Current focus" is APPEND-ONLY narrative — never delete or rewrite an
     existing line under it. Refresh the BODY line `**Last updated:**` — run: `date +%Y-%m-%d`. This
     is the only "Last updated" field this step touches. The YAML FRONTMATTER block at the top of
     status.md (the `timestamp:` field, an RFC3339 value) is NOT this step's to write — no field in
     that frontmatter block is — because `mev emit-state --write` regenerates the whole frontmatter
     block later in this same stage. Hand-editing `timestamp` here and then having emit-state
     rewrite it afterward is how the two go out of step with the HQ cache doc's `synced_from`
     (E_SYNC_DRIFT) — never touch `timestamp` in this step.
   - Update the log. In-place (non-worktree) only, after `mev emit-state --write`
     succeeds: if `planning/harness.json` declares an OPTIONAL `postEmitCommitCommand`
     (BT.ticket.bookkeep-leaves-derived-output-uncommitted), run it — this is project policy, never
     an engine default, so an absent key is a silent no-op. A hook failure must be reported, never
     swallowed, and never blocks the wrap-up commit below (the hook owns its own transaction).
   - Run the COMMIT-SAFETY GUARD `&&`-joined before the wrap-up commit — both the repo-local one and,
     in a vaulted repo, the vault one (`git -C <vault path>`) — then commit. The heredoc you write is
     the COMPLETE commit message, verbatim: never append a Co-Authored-By, Claude-Session, or any
     other attribution trailer, even if a session-level reminder instructs otherwise — this repo's
     AGENTS.md standing rule 5 and the user's own global CLAUDE.md forbid it categorically.
   - Create a pull request (PR) using git CLI or GitHub CLI (unless `--no-pr` is specified).
