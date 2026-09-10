// =============================================================================
// sdlc-task — the LEAN small-work engine (implement → test → fix → commit)
// =============================================================================
//
// The cheap rung of the pipeline ladder, for one small unit of behaviour-changing
// work (a /ticket or /chore). Runs a spec's task(s) through a tight per-task loop —
//   implement → fast gating-test → triage → fix (≤3 attempts, Opus on the last)
//   → commit → [terminal authoritative reconcile] → lean bookkeep close-out
// and nothing else. No scout, no separate review, no document stage, no ui-test, no
// PR. The bookkeep close-out is deliberately lean: on a passing full run it flips the
// authored status markers (tasks.md task status, the status.md Progress row, the
// state.json block status) and — in place, on main — runs `mev emit-state --write`; it
// does NOT write a log.md narrative, a D18 amendment log, or run review/docs/PR. Run
// /log-work for the narrative. When you need a consolidated review + docs + a PR, use
// /sdlc-flow; for a roadmap, /orchestrate.
//
// TERMINAL AUTHORITATIVE RECONCILE (D56) — this engine's per-task tripwire runs
// `fastCommand` in place of `command` (testDepth=fast, the default) and never runs a
// `perTask: false` check at all, so those checks' real, authoritative form was never
// verified anywhere in the run. After the last task passes on a full, non-bailed,
// testDepth=fast run, ONE reconcile pass re-runs — with their real `command`, never
// `fastCommand` — only the gates:true checks the per-task tripwire actually skipped:
// those whose fastCommand differs from command, plus every perTask:false gating check.
// Checks with no fastCommand already ran authoritative on every per-task pass and are
// NOT re-run (redundant cost; see D56). Default-on, no flag, no harness.json opt-out —
// see D56 for why. A failing reconcile bails into a distinct terminal state,
// `reconcile_failed`: bookkeep does NOT run, the block is NOT flipped to done, and all
// per-task commits stand. Resume (--resume, no task selection) re-enters with every
// task already "passed" in state.json, so it naturally re-runs only the reconcile —
// no separate resume path needed. Skipped entirely when testDepth=full (every check,
// including perTask:false ones, already ran authoritative on every per-task pass — see
// renderCheckList) or on a partial task-subset run (the existing fullRun guard).
//
// ISOLATION
//   Default: IN PLACE on the current branch (no worktree) — cheapest.
//   --worktree: run in an isolated git worktree on its own branch (you integrate the
//   branch yourself when ready). Opt-in only.
//
// USAGE
//   /sdlc-task <spec-slug>                 run every task in the spec, in place
//   /sdlc-task <spec-slug> 2               run only task 2
//   /sdlc-task <spec-slug> 1-3             run a task range (1-3, 1,3,5, 5)
//   /sdlc-task <spec-slug> 2 --worktree    run task 2 in an isolated worktree/branch
//   /sdlc-task <spec-slug> --resume        resume from the committed state file
//   /sdlc-task <spec-slug> --test-depth full  full gating suite per task (default: fast)
//
// PIPELINE
//   setup (locate repo / create worktree) → enumerate (D16 lint) → [resume load]
//     → per-task loop → [terminal authoritative reconcile, D56] → lean bookkeep
//     close-out (on pass) → final state commit
//
//   Per-task loop (sequential):
//     implement → fast-test → (triage → fix/bail) ×≤3 → one state write per task
//   A triage MAJOR / immediate-bail reason breaks straight out (does NOT burn the
//   remaining attempts); the run stops and reports for human pickup.
//
//   Terminal reconcile (D56, after every task passes on a full, testDepth=fast run):
//     re-run, with their authoritative `command`, only the checks the fast tripwire
//     substituted (fastCommand) or skipped (perTask:false) → on failure, status
//     "reconcile_failed" — bookkeep is skipped, the block is NOT flipped to done.
//
// STATE (NOT gitignored, but deliberately never committed — at planning/<spec>/sdlc/)
//   sdlc-task-state.json   the authoritative run index (per-task summary/issues/fixes/commit +
//                          the Block-A `tokens` block, plus `base_sha` — the pre-task HEAD this
//                          run's own emoji gate diffs from). Written to disk after every task and
//                          again at the end (cat-visible for crash inspection); read back off
//                          disk only, by --resume, and by /close-out's Step 0.5 in-place fallback
//                          (base_sha lets close-out scope its diff even when this run committed
//                          straight to the base branch) — never out of git — so it is disk-only,
//                          never committed (D46: planning/ may be a vaulted symlink into the brain
//                          repo, where a plain `git add planning/...` fails).
//
// COMMIT STRATEGY
//   feat: implement <stem>         implement agent (per task)
//   fix:  fix pass P for <stem>    fix agent (per pass)
//   chore: sdlc-task bookkeep — <…>  bookkeep close-out (on a passing run)
//
// MODEL TIERING (the token lever — see the MODEL map below)
//   haiku : setup, enumerate, state-load, test, state-writer, bookkeep
//   sonnet: implement, fix, triage
//   opus  : ESCALATION on the FINAL per-task fix pass
//
// IMPLEMENTATION RULE: engines are self-contained — lift, don't import. No cross-engine
// require. Validation is downstream only; never run this against base-template itself.
// =============================================================================

export const meta = {
  name: 'sdlc-task',
  description: 'Lean single-unit SDLC engine — implement → fast-test → fix → commit, in place or in a worktree',
  whenToUse: 'For one small unit of behaviour-changing work (a /ticket or /chore). No review/docs/PR — use /sdlc-flow for those. Usage: /sdlc-task <spec-slug> [task|range] [--worktree] [--resume]',
  phases: [
    { title: 'Setup', detail: 'Locate the repo root (or create an isolated worktree under --worktree)' },
    { title: 'Plan',  detail: 'Enumerate tasks from tasks.json (D16 lint) + load resume state' },
    { title: 'Tasks', detail: 'Per task: implement → fast-test → (triage → fix/bail), then a state write' },
  ]
}

// GIT_REPO_ENV_VARS (BT.ticket.worktree-run-can-commit-an-empty-tree, half (a)) — git exports these
// nine repository-scoping variables to the hooks it runs, and a hook-spawned process (this project's
// hooksPath = hooks) inherits them; they OVERRIDE `-C` and cwd, so a later `git commit` can silently
// build its tree from a stale/foreign index (e.g. a hook's GIT_INDEX_FILE) instead of the one the
// recipe actually staged, and commit a tree that deletes every tracked file behind a green PASS.
// Ported verbatim (name, order) from core/mev/src/shared.rs GIT_REPO_ENV_VARS/git_command() — do not
// re-derive this list. Every executable git invocation in this file's recipes must go through ${GIT},
// never a bare `git`; prose mentions of git (descriptions, prohibitions) are left alone. Kept
// byte-identical with sdlc-flow.js's copy — the two engines share no module, so this is duplicated on
// purpose (see scripts/test_git_env_strip.py's cross-engine agreement check).
// <<shared:GIT>>
const GIT = 'env -u GIT_DIR -u GIT_COMMON_DIR -u GIT_WORK_TREE -u GIT_INDEX_FILE -u GIT_OBJECT_DIRECTORY -u GIT_ALTERNATE_OBJECT_DIRECTORIES -u GIT_NAMESPACE -u GIT_PREFIX -u GIT_CEILING_DIRECTORIES git'
// <</shared:GIT>>

// ----------------------------------------------------------------
// Parse args: "<spec-slug> [task|range] [--worktree] [--resume] [--test-depth fast|full]"
// ----------------------------------------------------------------
const rawArgs = typeof args === 'string' ? args.trim() : ''
if (!rawArgs) {
  log('ERROR: No spec name provided.')
  log('Usage: /sdlc-task <spec-slug> [task|range] [--worktree] [--resume] [--test-depth fast|full]')
  return { error: 'Missing required argument: spec name (e.g. "<spec-slug>" or "<spec-slug> 2")' }
}

const tokens = rawArgs.split(/\s+/)
const blockId = tokens[0]

// <<shared:hasFlag>>
function hasFlag(name) { return tokens.includes(name) }
// <</shared:hasFlag>>
// <<shared:flagStr>>
function flagStr(name) {
  const i = tokens.indexOf(name)
  return (i === -1 || i + 1 >= tokens.length) ? null : tokens[i + 1]
}
// <</shared:flagStr>>
// Parse a task selection like "1-7", "1,3,5", "1-3,7", or "5" into a sorted int array.
function parseRange(spec) {
  const out = new Set()
  for (const part of spec.split(',')) {
    const m = part.trim().match(/^(\d+)(?:-(\d+))?$/)
    if (!m) return null
    const a = parseInt(m[1], 10), b = m[2] ? parseInt(m[2], 10) : a
    for (let i = Math.min(a, b); i <= Math.max(a, b); i++) out.add(i)
  }
  return [...out].sort((x, y) => x - y)
}

const useWorktree = hasFlag('--worktree')
const resumeMode  = hasFlag('--resume')

const VALID_TEST_DEPTHS = ['fast', 'full']
const testDepthFlag = flagStr('--test-depth')
if (testDepthFlag && !VALID_TEST_DEPTHS.includes(testDepthFlag)) {
  log(`ERROR: unknown --test-depth "${testDepthFlag}". Valid values: ${VALID_TEST_DEPTHS.join(', ')}.`)
  return { error: 'Invalid --test-depth', testDepthFlag, blockId }
}

// Optional task selection: `--tasks 1-7` OR a positional range/number as the 2nd token.
const rangeSpec = flagStr('--tasks') || (tokens[1] && !tokens[1].startsWith('--') ? tokens[1] : null)
let selectedTasks = null
if (rangeSpec) {
  const parsed = parseRange(rangeSpec)
  if (!parsed || parsed.length === 0) {
    log(`ERROR: could not parse task selection "${rangeSpec}". Use forms like 2, 1-7, 1,3,5, or 1-3,7.`)
    return { error: 'Invalid task selection', rangeSpec, blockId }
  }
  selectedTasks = new Set(parsed)
}

// Resolved against the git root by default; re-derived under a tier prefix (e.g. "business/")
// once setup reports where the spec actually lives (see setupResult.tierPrefix below) — `let`,
// not `const`, following the same pattern specFile already uses for its own reassignment.
let blockDir       = `planning/${blockId}`
let blockRecordFile = `planning/blocks/${blockId}.json`   // D65: the authored block record — preferred spec source
let specFile         = `${blockDir}/tasks.md`                // legacy fallback for a spec with no block record (reassigned once setup reports which source exists)
let tasksJsonFile = `${blockDir}/tasks.json`
let breakdownFile = `${blockDir}/breakdown.md`
let reportsDir    = `${blockDir}/sdlc/reports`
let stateFile     = `${blockDir}/sdlc/sdlc-task-state.json`   // COMMITTED authoritative run index (Block A)
const baseBranchName = `${blockId}-task`.toLowerCase().replace(/[^a-z0-9.-]/g, '-')  // worktree branch base

const MAX_TASK_ATTEMPTS = 3   // implement→test→fix attempts per task before bail (final on Opus)

// D46: a vaulted repo's planning/ is a relative symlink into a brain-owned vault
// (e.g. planning -> ../_planning/<repo>), so a plain `git add planning/...` from the
// repo root fails with "pathspec is beyond a symbolic link". Given the invoking repo
// root, this reports whether planning/ is such a symlink and resolves where its bytes
// actually live, so state-writing steps can stage through the real path instead of
// the link (and never "repair" the failure by checking out/committing in the vault
// repo). The Workflow runtime has no filesystem/Node API access (no fs, no process,
// no require, and `import` declarations don't even parse) — so this shells out via a
// cheap Haiku agent instead of calling fs.lstatSync/realpathSync in-process, exactly
// like every other filesystem check in this engine. Returns { vaulted, planningPath }
// where planningPath is always the absolute resolved directory: the vault's realpath
// when vaulted, the plain planning/ directory otherwise. (Duplicated from sdlc-flow.js:
// the engines are deliberately standalone files with no shared import.)
// <<shared:VAULT_DETECT_SCHEMA>>
const VAULT_DETECT_SCHEMA = {
  type: 'object',
  required: ['vaulted', 'planningPath'],
  properties: {
    vaulted:      { type: 'boolean', description: 'true iff planning/ is a symlink' },
    planningPath: { type: 'string', description: 'the resolved absolute real path of planning/' }
  }
}
// <</shared:VAULT_DETECT_SCHEMA>>
// <<shared:detectPlanningVault>>
async function detectPlanningVault(repoRoot) {
  const result = await agent(`
Determine whether planning/ in this repo is a symlink (a brain-vaulted repo) or a plain directory.
Run exactly this ONE Bash call (from the repo root, ${repoRoot}):
  cd ${repoRoot} && { [ -L planning ] && echo "SYMLINK" || echo "PLAIN"; } && python3 -c "import os; print(os.path.realpath('planning'))"
The first line is SYMLINK or PLAIN. The second line is the resolved absolute real path (this works
for both cases — realpath of a plain directory is itself).
Return via StructuredOutput: vaulted (true iff the first line is SYMLINK), planningPath (the
resolved absolute path from the second line).
`, { label: 'detect-vault', schema: VAULT_DETECT_SCHEMA, model: 'haiku' })
  if (!result) return { vaulted: false, planningPath: `${repoRoot}/planning` }
  return result
}
// <</shared:detectPlanningVault>>

// BT.ticket.worktree-setup-can-adopt-the-brain-root-as-repo-root — resolve repoRoot ONCE, here in
// the engine, and hand it to every later prompt as a GIVEN literal instead of asking the setup agent
// to derive-and-substitute a repoRoot placeholder token itself. That hand-substitution was the measured
// mechanism of a real misbinding: an agent read the correct root on command 1, then voluntarily cd'd
// to the brain root and re-derived REPO_ROOT there — nothing instructed it to, but nothing forbade it
// either. Removing the agent's discretion (one fixed command, no substitution to perform) removes the
// class, not just the one observed instance. IN-PLACE MODE is not exempt: this engine's in-place branch
// sets runDir = repoRoot from the same value, so a mis-derived root sends in-place reads/commits into
// the wrong repo just as effectively as a worktree misbinding — the guard below runs in both modes.
// Same shape as detectPlanningVault() immediately above, for the same reason: the Workflow runtime
// has no fs/process/require and `import` declarations don't even parse, so this shells out via a
// cheap Haiku agent turn instead of resolving the path in-process. Returns null on failure (unlike
// detectPlanningVault's safe fallback) — a wrong repoRoot silently accepted here is exactly the
// defect this ticket exists to remove, so the caller must abort rather than guess.
// <<shared:RESOLVE_REPO_ROOT_SCHEMA>>
const RESOLVE_REPO_ROOT_SCHEMA = {
  type: 'object',
  required: ['repoRoot', 'gitCommonDir', 'tierPrefix', 'brainTomlAtRoot'],
  properties: {
    repoRoot:        { type: 'string', description: 'Absolute repo root from the REPO_ROOT: line' },
    gitCommonDir:    { type: 'string', description: 'Absolute --git-common-dir from the GIT_COMMON_DIR: line' },
    tierPrefix:      { type: 'string', description: 'The invoking directory\'s path relative to repoRoot, with a trailing slash (e.g. "business/"), or "" at the repo root, from the TIER_PREFIX: line' },
    brainTomlAtRoot: { type: 'boolean', description: 'true iff the BRAIN_TOML: line reads "yes" — a brain.toml exists at repoRoot' }
  }
}
// <</shared:RESOLVE_REPO_ROOT_SCHEMA>>
// <<shared:resolveRepoRoot>>
async function resolveRepoRoot() {
  const result = await agent(`
Resolve this repo's root and related mechanical facts ONCE, before anything else runs.
Run exactly this ONE Bash call, from the invoking directory — do not cd anywhere first, do not
substitute or re-derive any value, and do not run any other command:
  REPO_ROOT=$(${GIT} rev-parse --show-toplevel) && echo "REPO_ROOT:$REPO_ROOT" && echo "GIT_COMMON_DIR:$(${GIT} rev-parse --path-format=absolute --git-common-dir)" && echo "TIER_PREFIX:$(python3 -c "import os; r=os.path.relpath(os.getcwd(), '$REPO_ROOT'); print('' if r=='.' else r+'/')")" && { [ -f "$REPO_ROOT/brain.toml" ] && echo "BRAIN_TOML:yes" || echo "BRAIN_TOML:no"; }
Four labelled lines come back — REPO_ROOT:, GIT_COMMON_DIR:, TIER_PREFIX:, BRAIN_TOML: (yes/no).
Return via StructuredOutput: repoRoot (the REPO_ROOT: value), gitCommonDir (the GIT_COMMON_DIR:
value), tierPrefix (the TIER_PREFIX: value, "" when invoking at the repo root), brainTomlAtRoot
(true iff BRAIN_TOML: is yes).
`, { label: 'resolve-repo-root', schema: RESOLVE_REPO_ROOT_SCHEMA, model: 'haiku' })
  return result || null
}
// <</shared:resolveRepoRoot>>

// WORKTREE-LIST GROUND TRUTH (BT.ticket.sdlc-task-worktree-flag-is-intermittently-ignored, task 2) —
// parses `git worktree list --porcelain` stdout entirely IN JS, never trusting the setup agent's own
// parsed conclusion about which entry is "its" worktree. Porcelain format is blocks of lines
// separated by a blank line, each block starting with "worktree <path>" and (for a non-detached
// worktree) containing a "branch refs/heads/<name>" line. Returns [{ path, branch }, ...] with
// branch === null for a detached entry (no "branch" line in its block) — callers treat that as "no
// branch", never as a match for any expected name.
function parseWorktreeListPorcelain(porcelain) {
  const entries = []
  const blocks = String(porcelain || '').split(/\r?\n\r?\n/)
  for (const block of blocks) {
    const lines = block.split(/\r?\n/)
    const worktreeLine = lines.find(l => l.startsWith('worktree '))
    if (!worktreeLine) continue
    const path = worktreeLine.slice('worktree '.length).trim()
    const branchLine = lines.find(l => l.startsWith('branch '))
    // branch lines report "refs/heads/<name>" — strip the prefix; a bare/unexpected form is kept
    // verbatim rather than dropped, so a mismatch is still visible in a bail message.
    const branch = branchLine
      ? branchLine.slice('branch '.length).trim().replace(/^refs\/heads\//, '')
      : null
    entries.push({ path, branch })
  }
  return entries
}

// BINDING / BRAIN-ROOT / POPULATION checks (BT.ticket.worktree-setup-can-adopt-the-brain-root-as-repo-root,
// task 4) — run immediately after the setup agent returns and BEFORE the enumerate/per-task stages, so a
// misbound or unpopulated checkout is caught before any task's implement stage touches it. Covers BOTH the
// worktree branch and the in-place branch: in-place sets runDir = repoRoot from the same engine-resolved
// value, so it is subject to the same binding and brain-root checks (population is worktree-only — see
// below). Same shape as VAULT_VERIFY_SCHEMA's verification agent above: the agent runs ONE fixed script and
// transcribes its already-labelled output; the abort DECISION is made HERE IN JS, never left to the model's
// own reasoning over labelled lines — a cheap model following multi-branch conditional prose reliably skips
// the else branch (measured live, documented on sdlc-flow.js's verifyVaultCommit). Neither this function nor
// its callers know what a "brain root" IS in any project-specific sense: the BRAIN-ROOT GUARD below compares
// two mechanical facts (brain.toml presence at two paths, both ordinary filesystem facts), never a
// harness-check count and never a hardcoded path (see out_of_scope on the ticket).
// <<shared:SETUP_GUARD_SCHEMA>>
const SETUP_GUARD_SCHEMA = {
  type: 'object',
  required: ['gitCommonDir', 'brainTomlAtRun'],
  properties: {
    gitCommonDir:   { type: 'string', description: 'Absolute --git-common-dir from the GIT_COMMON_DIR: line' },
    brainTomlAtRun: { type: 'boolean', description: 'true iff the BRAIN_TOML_AT_RUN: line reads "yes"' },
    missingCount:   { type: 'integer', description: 'Worktree mode only: the MISSING_COUNT: integer (0 when the script was not asked to check population)' },
    missingSample:  { type: 'array', items: { type: 'string' }, description: 'Worktree mode only: up to 5 example missing paths, split from the MISSING_SAMPLE: line on "|" with empty entries dropped' },
    notes:          { type: 'string' }
  }
}
// <</shared:SETUP_GUARD_SCHEMA>>
async function verifySetupBinding(runDir, useWorktreeMode) {
  // Population check only runs in worktree mode — an in-place run has no separate checkout to
  // under-populate (runDir === repoRoot, already fully checked out).
  const populationCmd = useWorktreeMode
    ? ` && MISSING=$(${GIT} -C ${runDir} ls-files | while read -r p; do [ -e "${runDir}/$p" ] || echo "$p"; done); echo "MISSING_COUNT:$(printf '%s\\n' "$MISSING" | grep -c . || true)" && echo "MISSING_SAMPLE:$(printf '%s\\n' "$MISSING" | head -5 | tr '\\n' '|')"`
    : ''
  const script = `${GIT} -C ${runDir} rev-parse --path-format=absolute --git-common-dir | sed 's/^/GIT_COMMON_DIR:/' && { [ -f "${runDir}/brain.toml" ] && echo "BRAIN_TOML_AT_RUN:yes" || echo "BRAIN_TOML_AT_RUN:no"; }${populationCmd}`
  const result = await agent(`
Run this exact script from ${runDir} with Bash, verbatim, and transcribe its labelled output —
do not reason about binding, brain roots, or population yourself, the script already produced the facts:
\`\`\`
${script}
\`\`\`
Return via StructuredOutput: gitCommonDir (the GIT_COMMON_DIR: value), brainTomlAtRun (true iff
BRAIN_TOML_AT_RUN: is yes)${useWorktreeMode ? ', missingCount (the MISSING_COUNT: integer), missingSample (the MISSING_SAMPLE: value split on "|", empty entries dropped)' : ', missingCount (0), missingSample ([])'}.
`, { label: 'verify-setup-binding', schema: SETUP_GUARD_SCHEMA, model: 'haiku' })
  return result || null
}

// Vault-aware task commits (extends D46): the per-task implement/fix stage below is instructed to
// stage + commit any planning/ paths it wrote THROUGH the vault repo (git -C <vault.planningPath>),
// reusing detectPlanningVault's real path exactly like the bookkeep/wrap-up recipe already does —
// never a second detection idiom. But that instruction is self-reported: the amendment log on this
// ticket recorded a live run where a stage returned a perfectly valid commitHash that covered ONLY
// the source half of a task, with the vault half silently uncommitted. So a valid commitHash proves
// nothing about the vault half, and this check never keys on it — it independently re-verifies, for
// every filesModified path that resolves under the vault, that the path is BOTH tracked and free of
// any staged/unstaged diff in the vault repo (i.e. actually landed in a commit there), via a cheap
// Haiku agent turn rather than trusting the implementer's own report.
// <<shared:VAULT_VERIFY_SCHEMA>>
const VAULT_VERIFY_SCHEMA = {
  type: 'object',
  required: ['allCommitted'],
  properties: {
    allCommitted:     { type: 'boolean', description: 'true iff every given path is tracked+committed either in THIS repo\'s vault, or (BRAIN_ROOT case) in the brain root repo directly' },
    uncommittedPaths: { type: 'array', items: { type: 'string' }, description: 'the subset (vault-relative) not committed anywhere — a real failure' },
    brainRootExempt:  { type: 'array', items: { type: 'string' }, description: 'the subset that does not exist under this repo\'s own vault at all, but IS committed directly in the brain root repo — a legitimate cross-repo write (e.g. /generate-roadmap authoring at HQ), not a vault-commit failure' },
    notes:            { type: 'string' }
  }
}
// <</shared:VAULT_VERIFY_SCHEMA>>
// <<shared:verifyVaultCommit>>
async function verifyVaultCommit(runDir, vault, vaultRelPaths) {
  if (!vault.vaulted || !vaultRelPaths.length) return { allCommitted: true, uncommittedPaths: [], brainRootExempt: [] }
  // The classification logic runs entirely IN THE SCRIPT, not in the model's own reasoning — a cheap
  // model following multi-branch conditional prose reliably skips the "else" branch (observed live:
  // Haiku checked only the vault path for 4/6 paths and never attempted the brain-root fallback for
  // any of them, silently treating a path that simply doesn't exist in the vault as UNCOMMITTED
  // instead of trying the brain root). The agent's only job now is to run ONE script and transcribe
  // its already-classified output lines — no per-path decision-making left to delegate.
  const script = `set -e
BRAIN_ROOT=$(cd "${vault.planningPath}" && while [ ! -f brain.toml ] && [ "$PWD" != "/" ]; do cd ..; done; pwd)
for p in ${vaultRelPaths.map(p => JSON.stringify(p)).join(' ')}; do
  if [ -e "${vault.planningPath}/$p" ]; then
    if [ -z "$(${GIT} -C ${vault.planningPath} status --porcelain -- "$p")" ] && ${GIT} -C ${vault.planningPath} ls-files --error-unmatch -- "$p" >/dev/null 2>&1; then
      echo "VAULT_OK:$p"
    else
      echo "UNCOMMITTED:$p"
    fi
  elif [ -e "$BRAIN_ROOT/planning/$p" ]; then
    if [ -z "$(${GIT} -C "$BRAIN_ROOT/planning" status --porcelain -- "$p")" ] && ${GIT} -C "$BRAIN_ROOT/planning" ls-files --error-unmatch -- "$p" >/dev/null 2>&1; then
      echo "BRAIN_ROOT_OK:$p"
    else
      echo "UNCOMMITTED:$p"
    fi
  else
    echo "UNCOMMITTED:$p"
  fi
done`
  const result = await agent(`
Run this exact script from ${runDir} with Bash, verbatim, and transcribe its output — do not
reason about vault vs. brain-root yourself, the script already decided it:
\`\`\`
${script}
\`\`\`
Each output line is "<BUCKET>:<path>". Return via StructuredOutput: allCommitted (true only if
every line's bucket is VAULT_OK or BRAIN_ROOT_OK — false if any line is UNCOMMITTED, or if the
script produced fewer lines than paths given, or errored), uncommittedPaths (the paths from every
UNCOMMITTED line), brainRootExempt (the paths from every BRAIN_ROOT_OK line — not a failure, just a
different repo), notes (paste the raw script output).
`, { label: 'verify-vault-commit', schema: VAULT_VERIFY_SCHEMA, model: 'haiku' })
  if (!result) return { allCommitted: false, uncommittedPaths: vaultRelPaths, brainRootExempt: [], notes: 'verification agent returned null' }
  if (!Array.isArray(result.brainRootExempt)) result.brainRootExempt = []
  return result
}
// <</shared:verifyVaultCommit>>

// COMMIT-SAFETY GUARD (BT.ticket.worktree-run-can-commit-an-empty-tree) — the cause-independent
// backstop. Joined to a `git commit` with `&&` in the SAME Bash call as the commit itself: a
// separate preceding call runs in a different process whose inherited git environment may differ,
// which is the whole failure mode this guards against. Signal is an EMPTY INDEX against a non-empty
// HEAD tree — deliberately NOT core.bare, which stays false in every reproduction of the data loss.
// `gitCmd` lets a vault-repo commit run the same guard via `git -C <vault path>` against the vault's
// own HEAD/index rather than the worktree's; the default 'git' reproduces the exact snippet verbatim.
// Kept byte-identical with sdlc-flow.js's copy — the two engines share no module, so this is
// duplicated on purpose (see scripts/test_commit_safety_guard.py's cross-engine agreement check).
// <<shared:renderCommitSafetyGuard>>
function renderCommitSafetyGuard(gitCmd = 'git') {
  return `if ${gitCmd} rev-parse --verify -q HEAD >/dev/null; then TRACKED=$(${gitCmd} ls-tree -r HEAD --name-only | wc -l | tr -d ' '); STAGED=$(${gitCmd} ls-files -s | wc -l | tr -d ' '); if [ "$TRACKED" -gt 0 ] && [ "$STAGED" -eq 0 ]; then echo "COMMIT_GUARD_ABORT: index holds 0 entries but HEAD tracks $TRACKED files - refusing to commit a tree that deletes everything (BT.ticket.worktree-run-can-commit-an-empty-tree)"; exit 1; fi; fi`
}
// <</shared:renderCommitSafetyGuard>>

// Post-commit work assertion (D81 lift condition 2 — BT.ticket.a-run-must-prove-its-commits-contain-the-work).
// renderCommitSafetyGuard() above fires only on a TOTALLY empty index (TRACKED>0 && STAGED==0); EN.11.O had a
// NON-EMPTY index full of deletions (443 files, 177,867 deletions, zero insertions) and sailed straight through
// it. This is the complement: it runs AFTER the task's own work commit (never before — HEAD~1 must exist), reads
// tasksJsonPath itself at RUN TIME (never a JS-side static file list — the engine never parses tasks.json's
// `files[]`, only the agent does) to get task `taskNum`'s declared files[], then aborts when ANY of:
//   (1) the commit's diff (git diff --name-status HEAD~1 HEAD) is EMPTY;
//   (2) NO changed path matches any declared file — condition (2);
//   (3) the commit DELETES ("D" status) a path that is NOT a declared file — the EN.11.O shape, condition (3).
// Deletion is not itself the signal: a task that deletes a file it DECLARED passes condition (3) cleanly, since
// that path is matched in WA_DECLARED. Diagnostic names the task and the failing condition, plus both path sets.
// EXEMPT (by simply never being called at their commit sites, same idiom renderCommitSafetyGuard already uses
// for the worktree-init commit): the worktree-init commit, the D16 `chore: derive tasks.json ...` fallback
// commits, and the vault commit (step 7b) — the vault commits into a DIFFERENT repo where HEAD is the vault's
// own and files[] entries are `planning/`-prefixed; comparing that commit's diff would need a second, foreign
// HEAD~1 that may not exist yet in a freshly-adopted vault checkout and that other concurrent lanes also write
// to, so a false WORK_ASSERTION_ABORT there would block an honest vault commit on a shared repo it does not
// fully control. Exempted outright rather than compared.
// <<shared:renderWorkAssertion>>
function renderWorkAssertion(gitCmd = 'git', taskNum, tasksJsonPath) {
  return `NAME_STATUS=$(${gitCmd} diff --name-status HEAD~1 HEAD); if [ -z "$NAME_STATUS" ]; then echo "WORK_ASSERTION_ABORT: task ${taskNum} commit diff is EMPTY (condition 1) - no work was committed"; exit 1; fi; WA_DECLARED=$(python3 -c "
import json
d = json.load(open('${tasksJsonPath}'))
t = [x for x in d if x.get('task_id') == ${taskNum}]
print(chr(10).join(t[0].get('files', []) if t else []))
"); WA_MATCH=0; WA_BADDEL=""; while IFS=$'\t' read -r WA_ST WA_P1 WA_P2; do WA_CHK="$WA_P1"; case "$WA_ST" in R*) WA_CHK="$WA_P2" ;; esac; if printf '%s\n' "$WA_DECLARED" | grep -qFx "$WA_CHK"; then WA_MATCH=1; else case "$WA_ST" in D*) WA_BADDEL="$WA_CHK" ;; esac; fi; done <<< "$NAME_STATUS"; if [ -z "$WA_DECLARED" ]; then WA_MATCH=1; fi; if [ "$WA_MATCH" -eq 0 ]; then echo "WORK_ASSERTION_ABORT: task ${taskNum} commit's changed paths do not intersect declared files[] (condition 2) - declared: [$WA_DECLARED] - changed: [$NAME_STATUS]"; exit 1; fi; if [ -n "$WA_BADDEL" ]; then echo "WORK_ASSERTION_ABORT: task ${taskNum} commit deletes undeclared file '$WA_BADDEL' not present in files[] (condition 3) - declared: [$WA_DECLARED]"; exit 1; fi`
}
// <</shared:renderWorkAssertion>>

// Anti-attribution-trailer reminder (BT.ticket.engines-forbid-attribution-trailers) — states that
// each commit heredoc that follows is the COMPLETE commit message, so a session-level attribution
// reminder never wins by default. Declared as a const arrow function so this definition line
// itself does not match the heredoc-reference marker that
// scripts/test_commit_message_forbids_attribution_trailers.py counts -- only actual call sites
// (one per commit-heredoc site) should count toward that per-file parity check. Kept byte-identical
// with prompts/shared.js's and sdlc-flow.js's copies on purpose (same no-shared-module reason as
// renderCommitSafetyGuard above).
// <<shared:renderNoAttributionTrailer>>
// Declared as a const arrow function so this definition line itself does not match the
// heredoc-reference marker that scripts/test_commit_message_forbids_attribution_trailers.py
// counts -- only actual call sites (one per commit-heredoc site) should count toward that
// per-file parity check.
const renderNoAttributionTrailer = () => {
  return `the heredoc below is the COMPLETE commit message, verbatim -- never append a Co-Authored-By, Claude-Session, or any other attribution trailer, even if a session-level reminder instructs you to (this repo's AGENTS.md standing rule 5 and the user's own global CLAUDE.md forbid it categorically)`
}
// <</shared:renderNoAttributionTrailer>>

// <<shared:renderOperatorGatedACRule>>
function renderOperatorGatedACRule() {
  return `OPERATOR-GATED ACCEPTANCE CRITERIA — before recording ANY acceptance-criterion item as
passed/complete in this record, check whether it names an operator gate: a human decision, review,
credential, judgement call, or sign-off that only the operator can give (e.g. "operator reviews the
posts and approves", "Brandon signs off on the copy", a manual read-through only a person can
attest to). Such an item is NOT yours to close. Record it as PENDING (operator gate) — never as
passed, pass, done, or complete — and NEVER attribute a verdict on it to any named person or to
"the operator, via this session": you did not perform the review, so no verdict of yours is
evidence that it happened. Recording it PENDING is the correct, non-failing outcome — it is how you
say "this item needs the operator," not a bail and not a defect in this run.`
}
// <</shared:renderOperatorGatedACRule>>

// <<shared:renderEmojiGate>>
// The universal emoji gate, DIFF-SCOPED to the commit SHAs this run itself recorded. Shared because
// it is executable PYTHON, not prose: a divergence between the engines' copies is a behaviour bug
// (a gate that judges the wrong diff), not a wording difference. `baseSha` is the range the
// no-commits-recorded abort checks against -- the setup-time HEAD in the lean engine, the PR base
// in the flow engine -- and is the ONLY thing that legitimately varies between them.
function renderEmojiGate({ runRoot, baseSha, stateFile, recordedCommitsJson }) {
  return `  cd ${runRoot} && python3 - <<'PYEOF'
import subprocess, re, sys
EMOJI = re.compile(r'[\\U0001F300-\\U0001FAFF\\U00002600-\\U000027BF]')
FOOTER = 'Generated with Claude Code'
BASE_SHA = '${baseSha}'
STATE_FILE = '${stateFile}'
RUN_COMMITS = ${recordedCommitsJson}
if not RUN_COMMITS:
    # No commits recorded by this run: nothing in BASE_SHA..HEAD is attributable to it, so the
    # committed range is not this run's to judge (it may be entirely a sibling session's already-
    # reviewed work). Judge this run's own UNCOMMITTED work instead -- tracked modifications plus
    # untracked files -- so a concurrent sibling's committed history can never fail a diff this run
    # never touched, while emoji this run itself is actively writing still fails closed.
    hits = []
    diff = subprocess.run(['git','diff','HEAD','-M','-U0','--','*.md','*.mdx'], capture_output=True, text=True).stdout.splitlines()
    cur_file = None
    cur_line = None
    for line in diff:
        if line.startswith('diff --git '):
            cur_file = None; cur_line = None
        elif line.startswith('+++ '):
            p = line[4:]
            cur_file = None if p == '/dev/null' else (p[2:] if p.startswith('b/') else p)
        elif line.startswith('@@'):
            m = re.match(r'@@ -\\d+(?:,\\d+)? \\+(\\d+)(?:,\\d+)? @@', line)
            cur_line = int(m.group(1)) if m else None
        elif cur_file and cur_line is not None and line.startswith('+') and not line.startswith('+++'):
            content = line[1:]
            if EMOJI.search(content) and FOOTER not in content:
                hits.append(f'{cur_file}:{cur_line}: {content.rstrip()[:100]}')
            cur_line += 1
    status = subprocess.run(['git','status','--porcelain','--','*.md','*.mdx'], capture_output=True, text=True).stdout.splitlines()
    for line in status:
        if not line.startswith('??'):
            continue
        untracked_path = line[3:]
        try:
            with open(untracked_path, encoding='utf-8') as fh:
                for lineno, content in enumerate(fh, start=1):
                    if EMOJI.search(content) and FOOTER not in content:
                        hits.append(f'{untracked_path}:{lineno}: {content.rstrip()[:100]}')
        except (OSError, UnicodeDecodeError):
            pass
    if hits:
        print(f'EMOJI CHECK FAIL (uncommitted work by this run -- no commits recorded yet in the run-state, {STATE_FILE}):')
        [print(h) for h in hits[:25]]
        sys.exit(1)
    print('EMOJI CHECK: OK'); sys.exit(0)
hits = []
for commit in RUN_COMMITS:
    diff = subprocess.run(['git','diff','-M','-U0',f'{commit}^..{commit}','--','*.md','*.mdx'], capture_output=True, text=True).stdout.splitlines()
    cur_file = None
    cur_line = None
    for line in diff:
        if line.startswith('diff --git '):
            cur_file = None; cur_line = None
        elif line.startswith('+++ '):
            p = line[4:]
            cur_file = None if p == '/dev/null' else (p[2:] if p.startswith('b/') else p)
        elif line.startswith('@@'):
            m = re.match(r'@@ -\\d+(?:,\\d+)? \\+(\\d+)(?:,\\d+)? @@', line)
            cur_line = int(m.group(1)) if m else None
        elif cur_file and cur_line is not None and line.startswith('+') and not line.startswith('+++'):
            content = line[1:]
            if EMOJI.search(content) and FOOTER not in content:
                hits.append(f'{cur_file}:{cur_line}: {content.rstrip()[:100]}')
            cur_line += 1
if hits:
    print('EMOJI CHECK FAIL:'); [print(h) for h in hits[:25]]; sys.exit(1)
print('EMOJI CHECK: OK'); sys.exit(0)
PYEOF`
}
// <</shared:renderEmojiGate>>

// <<shared:renderStateFlipScript>>
// The deterministic block-status flip for planning/state.json's authored block status
// (BT.ticket.sdlc-bookkeep-writes-block-status-deterministically). Outside a linked git worktree,
// with `mev` on PATH and this repo resolvable in brain.toml's [[repos]] table, the rendered script
// calls `mev set-block-status <repo>:<id> closed --write` and derives success/failure from ITS OWN
// subprocess exit code -- never from an agent-authored payload field. That `--write` call always
// carries the SAME `--agent <lane>` flag the adjacent `mev emit-state --write` call site already
// uses (renderAgentFlag(), resolved once here at prompt-GENERATION time, exactly as that call site
// does) -- reused rather than a second identity resolver. `<repo>` is resolved the same way
// renderScopeFlag() resolves its `--scope` slug (the brain.toml [[repos]] walk-up matching cwd to
// a registered repo_path); both are computed once, at generation time, and baked into the script
// as literals, matching this file's existing convention for those two flags.
//
// WORKTREE-MODE DECISION (made here, not left implicit, per this ticket's task 1): a successful
// `mev set-block-status --write` ALWAYS chains `emit-state --write` internally -- there is no flag
// to suppress it -- and `emit-state` refuses to run inside a linked git worktree. So inside a
// worktree this script NEVER calls `mev set-block-status` at all: the caller passes
// `runningInWorktree: true` and the script falls straight to the SAME validated hand-edit this
// region has always used (validated via `mev validate-brain --state` when `mev` is on PATH,
// degraded json.load-only when it is not), with `stateWriteValidated` reflecting that distinction
// exactly as before. Rewriting emit-state's own worktree-deferral behavior is out of scope for this
// ticket; this decision only says which route THIS script takes.
//
// Repo UNREGISTERED in brain.toml (no repo slug resolves) but mev IS on PATH still DEGRADES, NEVER
// BAILS: falls to the same validated hand-edit the worktree case uses (validated via
// `mev validate-brain --state` before/after diagnostics -- see the adjacent `emit-state` call
// site's identical contract).
//
// mev ABSENT is different (D86 / BT.ticket.sdlc-state-status-vocabulary task 3): this fallback
// used to silently write an UNVALIDATED status straight into state.json's JSON whenever `mev`
// could not be found on PATH, with no signal beyond an easily-missed "UNVALIDATED:" output line --
// bypassing `mev set-block-status`'s validation at the moment of write entirely. It now REFUSES
// instead of writing: "FLIP_REFUSED: <id>" followed by "MEV_OUTPUT: <line>" (exit 1) -- the SAME
// refusal contract the deterministic path above already uses for a failed `mev set-block-status`
// call, so a caller that already handles that contract needs no new branch. These engines still
// ship to downstream repos with no `mev` on PATH (D5, standing rule 1: mechanism, never stack
// defaults); a block closed through this fallback route in such a repo now requires installing
// `mev` rather than landing an unvalidated write.
//
// Machine-readable result lines a caller's bookkeep prompt copies verbatim, never re-derives:
//   deterministic path  -- "FLIPPED: <repo>:<id>" (exit 0) or "FLIP_REFUSED: <repo>:<id>" followed
//                          by "MEV_OUTPUT: <line>" lines (exit 1) -- both read from mev's own exit
//                          code, never from mev's stdout wording.
//   hand-edit fallback  -- "NOT_FOUND" (exit 0); "FLIPPED:<id>" (exit 0, mev on PATH: validated via
//                          `mev validate-brain --state` before/after diagnostics); "REJECTED:<id>"
//                          with "NET_NEW:" lines (exit 1, net-new validate-brain errors); or, when
//                          `mev` is not on PATH, "FLIP_REFUSED:<id>" with "MEV_OUTPUT:" lines
//                          (exit 1, D86 -- no more silent unvalidated write).
//
// `indent` exists only because the two prompts nest it at different depths.
async function renderStateFlipScript({ runRoot, indent, runningInWorktree = false }) {
  const agentFlag = await renderAgentFlag()
  const scopeFlagRaw = await renderScopeFlag()
  const scopeMatch = scopeFlagRaw.match(/--scope\s+(\S+)/)
  const repoSlug = scopeMatch ? scopeMatch[1] : null
  const useDeterministic = !runningInWorktree && !!repoSlug

  const agentTrim = agentFlag.trim()
  const agentArgsPy = agentTrim
    ? '[' + agentTrim.split(/\s+/).map(a => `'${a}'`).join(', ') + ']'
    : '[]'

  return `${indent}cd ${runRoot} && python3 -c "
import json, subprocess, sys, shutil

path = 'planning/state.json'
bid = sys.argv[1]
USE_DETERMINISTIC = ${useDeterministic ? 'True' : 'False'}
REPO_SLUG = '${repoSlug || ''}'

mev_available = shutil.which('mev') is not None

if USE_DETERMINISTIC and mev_available:
    key = REPO_SLUG + ':' + bid
    cmd = ['mev', 'set-block-status', key, 'closed', '--write'] + ${agentArgsPy}
    r = subprocess.run(cmd, capture_output=True, text=True)
    if r.returncode == 0:
        print('FLIPPED: ' + key)
        sys.exit(0)
    print('FLIP_REFUSED: ' + key)
    for line in (r.stdout + r.stderr).splitlines():
        print('MEV_OUTPUT: ' + line)
    sys.exit(1)

# Fallback: mev is not on PATH, this repo has no resolvable brain.toml slug, or this run is inside
# a linked git worktree (set-block-status's unconditional chained emit-state --write would trip
# emit-state's own worktree refusal) -- degrade to the validated hand edit rather than bail.
with open(path, 'rb') as fh:
    pre_bytes = fh.read()

data = json.loads(pre_bytes)
found = False
for track in data.get('tracks', []):
    for block in track.get('blocks', []):
        if block.get('id') == bid:
            block['status'] = 'closed'
            found = True
            break
    if found:
        break

if not found:
    print('NOT_FOUND')
    sys.exit(0)

def diagnostics():
    r = subprocess.run(['mev', 'validate-brain', '--state'], capture_output=True, text=True)
    lines = (r.stdout + r.stderr).splitlines()
    return set(l for l in lines if l.strip().startswith('[E_') or l.strip().startswith('[W_'))

if not mev_available:
    print('FLIP_REFUSED:' + bid)
    print('MEV_OUTPUT: mev is not on PATH -- refusing to write an unvalidated status directly into state.json (D86). Install mev and re-run, or close this block by hand with a reviewed \`mev set-block-status\` once mev is available.')
    sys.exit(1)

baseline = diagnostics()

with open(path, 'w') as fh:
    json.dump(data, fh, indent=2, ensure_ascii=False)
    fh.write(chr(10))

after = diagnostics()
net_new = after - baseline

if net_new:
    with open(path, 'wb') as fh:
        fh.write(pre_bytes)
    print('REJECTED:' + bid)
    for line in sorted(net_new):
        print('NET_NEW: ' + line)
    sys.exit(1)

print('FLIPPED:' + bid)
" "<RESOLVED_ID>"`
}
// <</shared:renderStateFlipScript>>

// <<shared:renderStatusWriteScript>>
// The D64-style validate-then-commit mutation for planning/status.md's authored body content --
// a direct sibling of renderStateFlipScript above, for the analogous corpus write
// (BT.ticket.bookkeep-writes-invalid-status-frontmatter). Captures the pre-write bytes, mutates
// in memory, runs `mev validate-brain --sync` (one flag, never combined with another) BEFORE and
// AFTER the write, and rolls back byte-exactly on any NET-NEW diagnostic -- the same delta-
// attribution rule renderStateFlipScript and hooks/pre-push stage 1 already implement under D64.
// Two write defects this script structurally cannot reintroduce: it NEVER touches any line at or
// before the closing `---` fence (the YAML frontmatter block, including `timestamp` -- derived by
// `mev emit-state --write` later in this same stage, never hand-written here), and its own new
// body line is always inserted strictly AFTER that closing fence, never the opening one (the
// EN.ticket.term-core-real-tmux-option-reads break). Shared for the same reason as
// renderStateFlipScript -- executable Python performing a validated write both engines need
// identically. `indent` exists only because the two prompts nest it at different depths.
function renderStatusWriteScript({ runRoot, indent }) {
  return `${indent}cd ${runRoot} && python3 -c "
import subprocess, sys, shutil

path = 'planning/status.md'
recent_work_line = sys.argv[1]
last_updated_date = sys.argv[2]

with open(path, 'rb') as fh:
    pre_bytes = fh.read()

text = pre_bytes.decode('utf-8')
lines = text.splitlines()

fence_idx = [i for i, l in enumerate(lines) if l.strip() == '---']
# A frontmatter block exists only when the OPENING fence is line 1 (write-okf-markdown). Without
# anchoring on that, a body horizontal-rule pair reads as frontmatter and every guard below is
# computed from the wrong offset -- skipping real body lines and inserting after the wrong fence.
closing_fence = fence_idx[1] if (len(fence_idx) >= 2 and fence_idx[0] == 0) else -1

# Never touch the YAML frontmatter block (every line at or before the closing fence) -- 'timestamp'
# in there is derived by mev emit-state, not this stage's to write.
for i in range(closing_fence + 1, len(lines)):
    if lines[i].startswith('**Last updated:**'):
        lines[i] = '**Last updated:** ' + last_updated_date
        break

# recent_work_line already carries the CALLER's own append-vs-replace decision baked in (the
# caller passes the exact line text whether it is a brand-new line or a replacement for an
# existing one it identified by reading the file first) -- this script only decides WHERE a
# genuinely new line lands, never whether to dedupe one naming this spec.
insert_at = None
for i in range(closing_fence + 1, len(lines)):
    if lines[i].strip().startswith('## Current focus'):
        insert_at = i + 1
        break
if insert_at is None:
    insert_at = closing_fence + 1 if closing_fence != -1 else len(lines)

lines.insert(insert_at, recent_work_line)

new_text = chr(10).join(lines)
if text.endswith(chr(10)):
    new_text += chr(10)

mev_available = shutil.which('mev') is not None

def diagnostics():
    r = subprocess.run(['mev', 'validate-brain', '--sync'], capture_output=True, text=True)
    out = (r.stdout + r.stderr).splitlines()
    return set(l for l in out if l.strip().startswith('[E_') or l.strip().startswith('[W_'))

if not mev_available:
    with open(path, 'w', encoding='utf-8') as fh:
        fh.write(new_text)
    print('STATUS_WRITE:unvalidated')
    print('UNVALIDATED: mev not on PATH -- schema check skipped, write landed with only line-level parsing')
    sys.exit(0)

baseline = diagnostics()

with open(path, 'w', encoding='utf-8') as fh:
    fh.write(new_text)

after = diagnostics()
net_new = after - baseline

if net_new:
    with open(path, 'wb') as fh:
        fh.write(pre_bytes)
    print('STATUS_REJECTED:written')
    for line in sorted(net_new):
        print('NET_NEW: ' + line)
    sys.exit(1)

outcome = 'written'
print('STATUS_WRITE:' + outcome)
" "<RECENT_WORK_LINE>" "<LAST_UPDATED_DATE>"`
}
// <</shared:renderStatusWriteScript>>

// <<shared:renderTriagePrompt>>
// The failure-triage prompt: classify a failure RETRYABLE vs MAJOR so the pipeline either makes a
// bounded fix or bails to a human now. Shared because the two engines' copies were IDENTICAL apart
// from the engine name -- 38 lines each, zero residual difference once that one noun is normalised.
//
// This is the prompt where the reasoning quality matters most and the text is most load-bearing:
// the five immediate-bail reasons, the "when unsure, BAIL" bias, and the evidence clause that
// forbids asserting a failure pre-dates the task without actually re-running the check against base
// state. Two copies of that argument is two chances for one to be weakened.
//
// `bailReasons` is rendered by the CALLER, so a project's harness.json additions (flow.bailReasons)
// flow through unchanged in both engines.
function renderTriagePrompt({ engineName, context, attempt, maxAttempts, failBlob, bailReasons, onBail, sameContext, bailRecipe }) {
  return `You are the failure-triage agent for an ${engineName} run. Classify a failure so the pipeline either makes
a bounded fix or bails to a human NOW. Bailing is cheap; a wasted retry loop is not — when unsure, BAIL.

Context: ${context} (attempt ${attempt} of ${maxAttempts}).
Failure detail:
${failBlob || '(no detail captured)'}

IMMEDIATE-BAIL reasons — if the failure is ANY of these, class=MAJOR and put a short human-readable
bailReason describing which one and where:
${bailReasons}

This does NOT widen the bail set above — it only constrains what you may ASSERT once you bail.
Before writing any bailReason that claims a failure PRE-DATES this task / exists "at baseline" / is
"unrelated to this task's scope": you MUST first re-run ONLY the failing check against the base state
(the main working tree, or the task's base commit). If you do so, set baseStateChecked=true and put
the actual result in evidence. If you cannot re-run it in this run's context, set baseStateChecked=false
and phrase the claim explicitly as a HYPOTHESIS ("possibly pre-existing; NOT verified against base"),
never as observed fact.
Self-inflicted-environment caution: harness-created workspace state (git worktree, sparse-checkout,
copied .env files, repaired planning/ symlinks) is a CANDIDATE CAUSE, not a fixed backdrop. Identical
failure before and after the change is NOT evidence of pre-existence when both states share the same
possibly-broken environment.
This changes only the wording/evidence of bailReason — bailing on IMMEDIATE-BAIL reason #3
(environment/credential/auth/network) stays correct and fast, "when unsure, BAIL" stays, and no
additional retry attempts are introduced by this rule.

Otherwise:
  RETRYABLE — transient/infra (agent died, flaky), OR the failure CHANGED from the previous attempt
              (it is making progress and a bounded fix can plausibly close it).
  MAJOR     — the SAME failure again with no progress, OR structural (one of the bail reasons above).

${bailRecipe}
Return via StructuredOutput: class, reason, bailReason (empty when RETRYABLE), sameFailureAsBefore,
evidence (what was actually OBSERVED, quoting output — no causal claims), baseStateChecked (true only
if the failing check was actually re-run against the base state)${onBail ? ', stateWritten (true only if you performed the additional state write above)' : ''}.
${sameContext ? `(Previous attempt context for the same-failure check: ${sameContext})` : ''}`
}
// <</shared:renderTriagePrompt>>

// <<shared:renderTestPrompt>>
// The per-run test prompt. 96% common between the engines before extraction; the four seams below
// are the whole of the difference, and each is a NOUN or a whole sentence supplied by the caller --
// never a branch on engine identity inside this text (D83).
//
//   enginePhrase    "lean /sdlc-task" | "/sdlc-flow"
//   runRootLabel    what to CALL the directory in prose. Each engine decides: /sdlc-flow is
//                   mode-aware (worktree root vs repo root) because it defaults to a plain branch.
//   diffBase        the range the emoji gate's no-commits-recorded abort checks against --
//                   setup-time HEAD in the lean engine, the PR base in the flow engine.
//   emojiScopeNote  the one sentence that closes the diff-scoping rationale. The engines genuinely
//                   say different things here: the lean engine warns about a sibling session on a
//                   shared in-place branch, the flow engine about the PR footer. A whole sentence
//                   from the caller, not a conditional in the middle of one.
//   heartbeatRecipe the lane-heartbeat re-stamp block (renderLaneHeartbeatRecipe), pre-rendered by
//                   the caller (it is async; this function is not) -- see
//                   BT.ticket.lane-heartbeat-goes-stale-mid-block, task 4. NEVER one of the gating
//                   checks reported above it -- best-effort, and never affects allPassed.
function renderTestPrompt({ enginePhrase, overrideNote, runRootLabel, runRoot, checklistBody, diffBase, stateFile, recordedCommitsJson, emojiScopeNote, onPassRecipe, stateWrittenNote, heartbeatRecipe }) {
  return `You are the test agent for the ${enginePhrase} pipeline. Run the project's validation checks and report.

IMPORTANT — run ONLY the checks enumerated below (${overrideNote}). Do NOT invent
checks. All Bash calls run from the ${runRootLabel} (prefix each with: cd ${runRoot} &&).

${checklistBody}

Then run the universal emoji gate (a harness rule, always) — DIFF-SCOPED to this run's OWN
recorded commit SHAs, never the whole ${diffBase}..HEAD range: it judges only lines ADDED by
commits THIS run itself made, so neither a legacy file's pre-existing emoji nor a concurrent
${emojiScopeNote}
${renderEmojiGate({ runRoot, baseSha: diffBase, stateFile, recordedCommitsJson })}
  A stray emoji ADDED in a commit THIS run made FAILS this gate; a pre-existing emoji in a file
  this task did not touch a line of, or an emoji added by a different, concurrent session's
  commit on a shared branch, does not.

For each check record: name, passed (true iff exit code 0), the command, and failure output.
${heartbeatRecipe || ''}
${onPassRecipe}
Return via StructuredOutput: allPassed (true only if EVERY gating check passed and the emoji gate is
clean), passCount, failCount, failedTests (names), failBlob (compact: failing check names + the tail of
their output; empty when allPassed)${stateWrittenNote}.`
}
// <</shared:renderTestPrompt>>

// <<shared:renderImplementPrompt>>
// The per-task implement/fix prompt -- the largest shared stage at 88 lines, and 94% common before
// extraction. Carries the D8 completeness self-check, the D81 post-commit work assertion, and the
// D46 vaulted-planning commit recipe, all of which exist because of specific incidents and none of
// which should ever exist in two versions.
//
// Three seams, all caller-supplied:
//   roleIntro          the opening three lines. The engines describe the checkout they run in
//                      differently, and /sdlc-flow's is MODE-AWARE (it defaults to a plain branch).
//   runRootLabel       what to call the run directory in prose.
//   extraReturnFields  StructuredOutput fields this engine wants that the other does not
//                      (/sdlc-flow's reportFile). Empty string in the lean engine.
function renderImplementPrompt({ roleIntro, runRootLabel, runRoot, extraReturnFields, isFix, taskNum, attempt, stem, blockId, specFile, specDesc, tasksJsonFile, breakdownFile, prevFailBlob, vault, GIT, renderCommitSafetyGuard, renderWorkAssertion }) {
  return `${roleIntro}

Target:
  Spec:        ${blockId}
  Task:        Task ${taskNum} only
  Spec file:   ${specFile} ${specDesc}
  Tasks file:  ${tasksJsonFile} (the task list — find the entry with "task_id": ${taskNum})

1. Read CLAUDE.md and planning/context.md — internalize the project's standing rules (CLAUDE.md is the
   authority; assume no stack/locale/narrative/content rule unless written there). Universal harness
   rules always apply: no fabricated metrics or quotes, no emoji, every change ships with tests.
   Run: cd ${runRoot} && cat CLAUDE.md

2. Read the spec and the task list:
   Run: cd ${runRoot} && cat ${specFile} ${tasksJsonFile}
   tasks.json is a bare array — find the object whose "task_id" is ${taskNum}. Its "title",
   "description", and "files" define exactly what this task is.
   ${isFix ? `Do NOT re-implement from scratch. Make the MINIMUM targeted changes to address THIS failure:
   ${prevFailBlob ? 'Failing checks/output from the last test run:\n' + prevFailBlob.split('\n').map(l => '     ' + l).join('\n') : ''}` : `Implement ONLY task id ${taskNum} — do NOT implement other tasks.`}

2.5. Optional breakdown (more granular sub-steps from /breakdown):
   Run: cd ${runRoot} && ls ${breakdownFile} 2>/dev/null && echo "BREAKDOWN_EXISTS" || echo "NO_BREAKDOWN"
   If BREAKDOWN_EXISTS: read ${breakdownFile}, find "### Step ${taskNum}:", and use its atomic sub-steps as
   the execution guide (run each inline "Verify:" checkpoint). tasks.json stays authoritative for scope.

3. Execute methodically with Read/Edit/Write/Bash (all paths resolve from the ${runRootLabel}).

3a. STAY INSIDE THIS TASK'S OWN FILES — and NEVER revert a path you did not author. You may read
   anything in the repo. You may create/edit/delete only the paths in this task's "files" (plus what
   those changes directly require, e.g. a new test's fixture). You may NEVER restore, revert,
   discard, or overwrite a path outside that set: no \`${GIT} checkout -- <path>\`, no
   \`${GIT} restore <path>\`, no \`${GIT} reset\`, no \`${GIT} stash\`, no \`${GIT} clean\`, and no
   reverting a file to an earlier revision to "undo" an unrelated change you noticed. This is
   absolute, not tidiness: several agent lanes run concurrently in this fleet, some against the same
   working tree, and every repo's planning/ directory is tracked by one shared git repo — so a stray
   \`${GIT} checkout -- <path>\` silently and IRRECOVERABLY destroys another live session's
   uncommitted work, with no reflog entry to recover from because those bytes were never committed.
   If a file outside your files[] looks wrong, is uncommitted, or appears to block this task, STOP:
   leave it exactly as it is and say so in notes. Do not fix it, do not revert it, do not stage it.

3b. RELATED: DOC_ID RESOLUTION (BT.ticket.engines-must-not-author-unverified-records, rule 2) — if
   this task creates or edits ANY markdown file carrying OKF frontmatter (every new \`.md\` under
   \`docs/\` or \`planning/\` must, per CLAUDE.md standing rule 5/6), resolve every \`related:\` entry
   BEFORE you write the file. A \`related:\` entry is a doc_id — the target file's own \`doc_id:\`
   frontmatter field, defaulting to its filename stem when that field is absent — NEVER a filename, a
   slug, a title, a task id, or a block id guessed from a sibling path. Confirm each target actually
   resolves in the corpus (e.g. \`rg -L -n "^doc_id: <id>$" <repo>\`, or that a crawled file whose stem
   is \`<id>\` exists — a leading \`_\` in a filename excludes it from the corpus, so such a target is
   UNRESOLVED even though the file is on disk). An unresolvable target is OMITTED, not guessed —
   dropping the whole \`related:\` field is the correct move when nothing resolves; writing an invented
   doc_id red-gates the whole corpus (E_GRAPH_DANGLING_RELATED) for every concurrent lane, not just
   this one. Load the \`write-okf-markdown\` skill for the full procedure, including the cross-repo
   \`<scope>:<doc_id>\` prefix form a target outside this file's own scope needs.

4. Follow every CLAUDE.md standing rule; add/update tests for new code/logic; verify any model ids /
   package names via the claude-api skill — never from memory.

5. COMPLETENESS SELF-CHECK before committing (D8): no stub/placeholder on any path the task's acceptance
   criteria require (no \`todo!()\`/\`unimplemented!()\`/\`unreachable!()\`, \`raise NotImplementedError\`,
   \`throw new Error('not implemented')\`, empty \`pass\`-only bodies, or \`TODO\`/\`FIXME\` in required
   paths); every deliverable named for Task ${taskNum} exists; any "unit-tested" criterion has a real,
   hermetic test. Sanity-grep ONLY the files the in-scope criteria require:
     cd ${runRoot} && grep -nE 'todo!\\(|unimplemented!\\(|unreachable!\\(|NotImplementedError|not implemented|FIXME' <those paths> 2>/dev/null
   If something required is incomplete, finish it now — do not commit a partial task.

6. Run the spec's "## Validation Commands" for Task ${taskNum} to confirm correctness.

7. Commit on the branch. Never use git add -A or git add . — stage files explicitly by name.
   Run: cd ${runRoot} && ${GIT} status
   Stage your changed source/test files explicitly, then commit using HEREDOC — ${renderNoAttributionTrailer()}:
     cd ${runRoot} && ${renderCommitSafetyGuard()} && ${GIT} commit -m "$(cat <<'EOF'
${isFix ? `fix: fix pass ${attempt - 1} for ${stem}` : `feat: implement ${stem}`}
EOF
)"
   Run: cd ${runRoot} && ${GIT} log --oneline -1   (capture the short hash)

7a. Post-commit work assertion (D81 lift condition 2) — prove this commit actually contains Task
   ${taskNum}'s declared work, not the absence of it:
   Run: cd ${runRoot} && ${renderWorkAssertion('git', taskNum, tasksJsonFile)}
   If this prints WORK_ASSERTION_ABORT, the commit failed the check — treat this as a task failure
   (investigate, fix, and re-commit) before proceeding; do NOT report success with a failing assertion.
   Capture the outcome as a STRUCTURED field, not only prose: this command's FINAL run this attempt
   (after any fix + re-commit) must print no WORK_ASSERTION_ABORT line and exit 0 for
   workAssertionPassed to be true. The terminal write recipe refuses to record this task done/passed
   without a positive workAssertionPassed — never omit or fabricate this field.
   VAULT-ONLY TASKS (D46): if EVERY path in this task's declared files[] begins with "planning/",
   the work landed in the vault repo by step 7b and this repo's own history structurally CANNOT
   contain it — the assertion above will abort on condition 1 (empty diff) forever, and no retry
   can clear it. That is a false negative, not missing work. In that case ONLY, satisfy the
   assertion against the repo the work actually went to: run the same
   \`diff --name-status HEAD~1 HEAD\` with \`-C\` pointed at the vault's planning path, and confirm
   the changed paths correspond to this task's declared files[] with the leading "planning/"
   replaced by this repo's subdirectory name in the vault. Set workAssertionPassed=true only if
   that vault-side diff is non-empty AND corresponds; otherwise false. Say in notes that the
   assertion was satisfied vault-side and name the vault commit. A task with a MIX of vaulted and
   non-vaulted files is NOT this case — it must still pass the ordinary assertion above.
${vault.vaulted ? `
7b. planning/ is a vaulted symlink (D46) — its bytes live at ${vault.planningPath}, a DIFFERENT git
    repo, invisible to the commit you just made in step 7. If this attempt created or edited ANY file
    under planning/ (i.e. it belongs in filesModified with a "planning/" prefix), you MUST ALSO stage
    and commit it there, through the real path — derive the exact set from what you actually wrote,
    never a fixed list of filenames. NEVER git add -A, git add ., git reset, or git stash against the
    vault repo — another lane's session may have unrelated work staged there right now; touch ONLY
    your own paths, and do not checkout/switch/branch inside it (stay on whatever branch it is
    already on). For each such file, let <relpath> be the part of its path AFTER "planning/":
      cd ${runRoot} && ${GIT} -C ${vault.planningPath} add ${vault.planningPath}/<relpath>
    Then, once every such path is staged, commit ONLY those paths — pass them explicitly to \`git commit\`
    itself (not merely to \`git add\`), so a sibling lane's unrelated pre-staged files are never swept
    into this commit even if they happen to already be staged; ${renderNoAttributionTrailer()}:
      cd ${runRoot} && ${GIT} -C ${vault.planningPath} diff --cached --quiet -- <relpath1> <relpath2> ... || (${renderCommitSafetyGuard('git -C ' + vault.planningPath)} && ${GIT} -C ${vault.planningPath} commit -m "$(cat <<'EOF'
${isFix ? `fix: fix pass ${attempt - 1} for ${stem} (vault)` : `feat: implement ${stem} (vault)`}
EOF
)" -- <relpath1> <relpath2> ...)
      cd ${runRoot} && ${GIT} -C ${vault.planningPath} log --oneline -1
    If NOTHING you wrote this attempt lives under planning/, skip this step entirely — do not run any
    vault command. If a vault add/commit fails, report it PLAINLY in notes; never paper over it, and
    never "repair" it by committing on a different branch inside the vault.
` : ''}
Return via StructuredOutput:${extraReturnFields}
  success: true if the work completed and the spec validation passed
  filesModified: every file you created or modified this attempt — including any under planning/
    (do NOT omit vault-side files just because they commit through a different repo)
  commitHash: the 7-char short hash of THIS repo's commit (empty string if no commit was made here)
  summary: one line — what this task now does
  decisions: any non-obvious choices (empty array if none)
  filesReadKb: telemetry — before returning, sum the byte size of every file you cat/Read this attempt
    (cd ${runRoot} && wc -c <each file>), divide the total by 1024, and report the number.
  workAssertionPassed: true only if step 7a's FINAL run this attempt printed no WORK_ASSERTION_ABORT
    and exited 0; false otherwise. Never omit this field.
  notes: one-line status${vault.vaulted ? ' — mention explicitly whether a vault commit (step 7b) happened and, if so, its outcome' : ''}`
}
// <</shared:renderImplementPrompt>>

// Given a task stage's self-reported filesModified (repo-root-relative) and a resolved vault, return
// the vault-relative subset (the part of the path after "planning/") that needs an independent
// vault-commit check. Derived from what the task ACTUALLY wrote — never a hard-coded filename list.
// <<shared:vaultRelPathsFrom>>
function vaultRelPathsFrom(filesModified, vault) {
  if (!vault.vaulted || !Array.isArray(filesModified)) return []
  return filesModified
    .filter(f => typeof f === 'string' && (f === 'planning' || f.startsWith('planning/')))
    .map(f => f.slice('planning/'.length))
    // A stage may self-report a path carrying its own "(vault: <path>)" annotation --
    // e.g. 'harness.json (vault: side/_planning/price-scout/harness.json)' -- which must
    // be stripped before stat-ing, or the literal annotation text gets treated as part of
    // the path (BT.chore.vault-commit-checker-misparses-its-own-annotation). Only the
    // exact trailing " (vault: ...)" annotation shape is stripped -- a path containing
    // unrelated, legitimate parentheses must survive untouched.
    .map(f => f.replace(/\s*\(vault:[^)]*\)\s*$/, '').trim())
    .filter(Boolean)
}
// <</shared:vaultRelPathsFrom>>

log(`Target: ${blockId} (${selectedTasks ? [...selectedTasks].sort((a, b) => a - b).join(', ') : 'all tasks'})`)
log(`Spec: ${blockId} (resolving block record first, tasks.md fallback) | mode: ${useWorktree ? 'worktree' : 'in-place'}${resumeMode ? ' | RESUME' : ''}`)

// ================================================================
// Schemas
// ================================================================
const SETUP_SCHEMA = {
  type: 'object',
  required: ['runDir', 'branchName', 'currentBranch', 'baseSha', 'worktreeFailed'],
  properties: {
    runDir:         { type: 'string', description: 'Absolute path the pipeline runs from (worktree path under --worktree; else the repo root)' },
    branchName:     { type: 'string', description: 'The branch commits land on (a new worktree branch under --worktree; else the current branch)' },
    currentBranch:  { type: 'string', description: 'STEP 1: the branch HEAD was on when setup started (rev-parse --abbrev-ref HEAD via GIT), captured BEFORE any worktree work. Reported in both modes so the engine can deterministically detect a --worktree run that silently resolved to the current branch (BT.ticket.sdlc-task-worktree-flag-is-intermittently-ignored).' },
    baseSha:        { type: 'string', description: 'The HEAD short sha AFTER setup, BEFORE any task commit — the emoji-gate diff base' },
    wasCreated:     { type: 'boolean', description: 'true if a new worktree was created (--worktree only)' },
    worktreeFailed: { type: 'boolean', description: '--worktree only: true iff the worktree could not be resolved or created and setup stopped rather than falling back to the current branch — either no free candidate name was found among "<base>" through "<base>-10", or the worktree-add/creation step itself errored. Always false in in-place mode.' },
    worktreeFailureReason: { type: 'string', description: 'Empty unless worktreeFailed is true. Names the spec slug, every candidate branch name tried, and (for a creation failure) the exact command output.' },
    specFileExists: { type: 'boolean', description: 'true if EITHER the block record or the legacy tasks.md exists (D65 stage 2)' },
    specSource:     { type: 'string', enum: ['block-record', 'tasks-md', 'missing'], description: "D65 stage 2: 'block-record' if planning/blocks/<BlockID>.json exists (preferred), else 'tasks-md' if the legacy spec file exists, else 'missing'. Evaluated at the WINNING location (root if the spec exists there, else tier) — see specFoundInTier." },
    tierPrefix:     { type: 'string', description: 'The invoking directory\'s path relative to the git root, with a trailing slash (e.g. "business/"), or "" when /sdlc-task was invoked at the git root. This is the CANDIDATE tier location checked in STEP 4a — reported regardless of whether the spec was actually found there.' },
    specFoundInTier: { type: 'boolean', description: 'true iff the spec (block record or legacy tasks.md) exists ONLY at the tier location (<tierPrefix>planning/<blockId>), not at the root (planning/<blockId>). False when found at the root (even if ALSO present at the tier — the root always wins) or found nowhere.' },
    blockStatus:    { type: 'string', description: "This spec's Status in status.md (title-case), or 'Unknown'" },
    specThin:       { type: 'boolean', description: 'D19: true on a fresh (non-resume) run with a structurally-valid but substantively-thin spec; false on resume or a healthy spec.' },
    thinReason:     { type: 'string', description: 'D19: the specific thin-spec failures when specThin; empty string otherwise.' },
    envFilesCopied: { type: 'array', items: { type: 'string' }, description: '--worktree only: repo-root-relative paths of every gitignored env-shaped file seeded into the worktree (from ENV_COPIED: lines); empty array if none existed to copy.' },
    worktreeListPorcelain: { type: 'string', description: '--worktree only (task 2): the COMPLETE, UNMODIFIED stdout from the worktree list porcelain command captured in STEP 2d, after the worktree was created/resolved. The engine parses this itself and treats it as ground truth for runDir/branchName rather than trusting the agent\'s own STEP 2/2b bookkeeping — a worktree absent from this listing, or one whose listed path/branch does not match what setup intended, is a bail. Empty string in in-place mode. "COMMAND_FAILED: <output>" if the listing command itself errored.' },
    notes:          { type: 'string' }
  }
}

// D16 preflight lint — the spec MUST carry a non-empty tasks.json array (a bare array of
// SDLCTask-shaped objects, matching orchestrator's app/schemas/sdlc_schema.py — see D45) or the
// loop would have to guess the task count non-deterministically.
const ENUMERATE_SCHEMA = {
  type: 'object',
  required: ['hasTasks', 'allTasks'],
  properties: {
    hasTasks: { type: 'boolean', description: 'true if tasks.json parses as a non-empty array' },
    allTasks: { type: 'array', items: { type: 'integer' }, description: 'Every task_id in tasks.json, in array order' },
    // Per-task validation override — see the matching block in sdlc-flow.js. `validation_commands`
    // is a real SDLCTask field this engine used to ignore; honouring it lets a docs-only or
    // config-only task declare a cheaper tripwire than the project-wide gating set. Empty/absent
    // => harness checks, i.e. the pre-existing behaviour.
    taskChecks: {
      type: 'array',
      description: "One entry per task that declares a non-empty validation_commands array. Omit tasks whose validation_commands is absent or empty.",
      items: {
        type: 'object',
        required: ['taskId', 'validationCommands'],
        properties: {
          taskId:             { type: 'integer' },
          validationCommands: { type: 'array', items: { type: 'string' } }
        }
      }
    },
    // expect_red — BT.ticket.sdlc-task-cannot-express-a-deliberate-failing-test. A task whose
    // declared deliverable IS a test observed FAILING (D68) may name a subset of its own
    // validation_commands whose verdict is inverted: the check PASSES when that command exits
    // NON-ZERO and FAILS when it exits 0. Scoped strictly to that task's own validationCommands —
    // it can never touch a project-wide gates:true harness check (those are computed separately by
    // gatingChecks() and this field is never consulted there).
    taskExpectRed: {
      type: 'array',
      description: "One entry per task that declares a non-empty expect_red array. Omit tasks whose expect_red is absent or empty. Every command listed here MUST also appear in that same task's own validationCommands entry above — an expect_red command names ONE of the task's own declared checks and inverts its verdict, it does not add a new command.",
      items: {
        type: 'object',
        required: ['taskId', 'commands'],
        properties: {
          taskId:   { type: 'integer' },
          commands: { type: 'array', items: { type: 'string' }, description: "Subset of this task's own validationCommands whose verdict is inverted: PASSES on non-zero exit, FAILS on exit 0." }
        }
      }
    },
    // Hardcoded engine-parse gate — mechanism, not project policy (see renderCheckList). Captures,
    // per task, ONLY the entries of that task's "files" array that live under .claude/workflows/ —
    // never the full files[] list. Omit tasks with no such path.
    engineFiles: {
      type: 'array',
      description: "One entry per task whose 'files' array includes at least one path under .claude/workflows/. 'files' holds ONLY the matching .claude/workflows/ paths (not the task's full files[] list). Omit tasks with no such path.",
      items: {
        type: 'object',
        required: ['taskId', 'files'],
        properties: {
          taskId: { type: 'integer' },
          files:  { type: 'array', items: { type: 'string' } }
        }
      }
    },
    notes:    { type: 'string' }
  }
}

// D16 derive-from-tasks.md fallback — see the abort below. Mirrors /generate-tasks' --from mode: read the spec's authored step decomposition and
// write a fresh D45-shaped tasks.json from it (never a verbatim copy of the prose, never the
// superseded D44 {"tasks": [...]} wrapper).
// <<shared:DERIVE_SCHEMA>>
const DERIVE_SCHEMA = {
  type: 'object',
  required: ['derivable', 'written'],
  properties: {
    derivable:  { type: 'boolean', description: 'true iff tasks.md exists and carries a numbered step decomposition to derive from' },
    written:    { type: 'boolean', description: 'true iff a D45-shaped tasks.json (bare array, integer task_id, single-string description, no status/attempt_count) was written and committed' },
    commitHash: { type: 'string' },
    taskCount:  { type: 'integer' },
    notes:      { type: 'string' }
  }
}
// <</shared:DERIVE_SCHEMA>>

const STATE_LOAD_SCHEMA = {
  type: 'object',
  required: ['exists'],
  properties: {
    exists:      { type: 'boolean', description: 'true if a valid sdlc-task-state.json was read' },
    startedAt:   { type: 'string',  description: "the file's started_at value, or '' when absent" },
    passedTasks: { type: 'array', items: { type: 'integer' }, description: 'task numbers whose status is "passed"' },
    bailReason:  { type: 'string',  description: 'the prior bail_reason, or "" when none' },
    tasksJson:   { type: 'string',  description: 'Verbatim JSON (as a string) of the state file\'s top-level "tasks" object, so the engine can carry the full prior task history forward. "{}" when absent/no state.' },
    bails:       { type: 'array', items: { type: 'object' }, description: 'Verbatim contents (each entry as-is, unmodified) of the state file\'s top-level "bails" array — BT.ticket.bails-must-be-append-only. [] when absent/no state.' },
    notes:       { type: 'string' }
  }
}

const STAGE_SCHEMA = {
  type: 'object',
  required: ['success'],
  properties: {
    success:             { type: 'boolean' },
    filesModified:       { type: 'array', items: { type: 'string' } },
    commitHash:          { type: 'string', description: 'Short hash of the commit this agent made, or empty string' },
    summary:             { type: 'string', description: 'One-line summary of what was implemented/fixed (folded into state.tasks[N].summary)' },
    decisions:           { type: 'array', items: { type: 'string' }, description: 'Non-obvious choices made (folded into state)' },
    filesReadKb:         { type: 'number', description: 'Telemetry (optional): sum of bytes of all files this stage cat/Read, divided by 1024.' },
    // BT.ticket.engine-terminal-state-needs-evidence (task 3, Gap 1): step 7a's renderWorkAssertion
    // check previously lived only as prose the agent could skip or misreport. This is that check's
    // outcome as a STRUCTURED field: true only if step 7a's FINAL run (after any fix + re-commit
    // this attempt) printed no WORK_ASSERTION_ABORT and exited 0. Absent/false is treated as a
    // failed assertion by the terminal write recipe below — never a silent pass.
    workAssertionPassed: { type: 'boolean', description: 'true only if step 7a\'s renderWorkAssertion check printed no WORK_ASSERTION_ABORT and exited 0 on its final run this attempt; false/absent means the terminal write must refuse to record this task done/passed' },
    notes:               { type: 'string' }
  }
}

const TEST_SCHEMA = {
  type: 'object',
  required: ['allPassed', 'passCount', 'failCount'],
  properties: {
    allPassed:   { type: 'boolean' },
    passCount:   { type: 'integer' },
    failCount:   { type: 'integer' },
    failedTests: { type: 'array', items: { type: 'string' } },
    failBlob:    { type: 'string', description: 'Compact failure output (failing check names + the tail of their output) for triage; empty when allPassed' },
    stateWritten: { type: 'boolean', description: 'true if the agent ALSO persisted sdlc-task-state.json this same turn (the per-task pass-path state-write fold); false/omitted when it did not (no onPass instructions given, a check failed, or the write was not attempted/completed)' },
    notes:       { type: 'string' }
  }
}

// Triage a per-task failure: RETRYABLE (a bounded fix can help) vs MAJOR (bail to a human now).
const TRIAGE_SCHEMA = {
  type: 'object',
  required: ['class', 'reason'],
  properties: {
    class:               { type: 'string', enum: ['RETRYABLE', 'MAJOR'] },
    reason:              { type: 'string', description: 'One sentence: why retryable (transient/changed/progressing) or major (an immediate-bail reason, stuck, or structural)' },
    bailReason:          { type: 'string', description: 'When class=MAJOR: a short human-readable reason for the handoff; empty when RETRYABLE' },
    sameFailureAsBefore: { type: 'boolean', description: 'true if the SAME failure as the previous attempt (no progress)' },
    evidence:            { type: 'string', description: 'What was actually OBSERVED, quoting the failing check output. No causal claims.' },
    baseStateChecked:    { type: 'boolean', description: 'true only if the failing check was actually re-run against the base state (main working tree or the task base commit). false means any claim about the base state is a hypothesis.' },
    stateWritten:        { type: 'boolean', description: 'true if the agent ALSO persisted sdlc-task-state.json this same turn (the terminal-bail state-write fold); false/omitted when it did not (no onBail instructions given, the outcome was not terminal, or the write was not attempted/completed)' }
  }
}

const STATE_WRITE_SCHEMA = {
  type: 'object',
  required: ['written'],
  properties: {
    written:   { type: 'boolean', description: 'true if sdlc-task-state.json was written to disk' },
    startedAt: { type: 'string',  description: 'the started_at value used in this write (preserved from the existing file, or newly stamped)' },
    updatedAt: { type: 'string',  description: 'the updated_at value written in this write' },
    notes:     { type: 'string' }
  }
}

const BOOKKEEP_SCHEMA = {
  type: 'object',
  required: ['statusUpdated'],
  properties: {
    statusUpdated:      { type: 'boolean', description: 'true if planning/status.md was updated' },
    statusWriteValidated: { type: 'boolean', description: 'true if mev validate-brain --sync gated the planning/status.md mutation (before/after diff, net-new only); false when mev was not on PATH and the write landed with only line-level parsing (a degrade, not a pass)' },
    statusWriteRejected: { type: 'boolean', description: 'true if the planning/status.md mutation introduced net-new corpus errors and was rolled back byte-exact; status.md on disk is unchanged from before this step ran' },
    tasksMarked:        { type: 'boolean', description: 'true if tasks.md task markers were updated' },
    blockStatusFlipped: { type: 'string', description: 'the state.json tracks[].blocks[].id whose flip to "closed" was reported by `mev set-block-status`\'s own exit code (deterministic route) or by the degraded hand-edit fallback, transcribed from the flip script\'s stdout — never agent-authored; "" if none (partial run, no state.json, block not found, `mev set-block-status` refused via FLIP_REFUSED, or the fallback write was rejected by validation)' },
    stateWriteValidated: { type: 'boolean', description: 'true when the deterministic `mev set-block-status --write` route ran and reported FLIPPED (mev\'s own exit code validated the write), or when the fallback hand-edit passed `mev validate-brain --state` (before/after diff, net-new only); false otherwise — since D86 no route writes an unvalidated status (mev absent makes the fallback refuse via FLIP_REFUSED instead of writing)' },
    stateWriteRejected: { type: 'boolean', description: 'true if the state.json mutation introduced net-new schema errors and was rolled back byte-exact; the block was NOT flipped to closed this run' },
    emitStateRan:       { type: 'boolean', description: 'true if mev emit-state --write ran successfully (false when skipped: worktree mode or mev/brain.toml absent)' },
    postEmitHookRan:    { type: 'boolean', description: 'true if planning/harness.json\'s postEmitCommitCommand was configured AND invoked this run (in-place only, and only when emitStateRan is true); false when absent, or skipped (worktree mode / emit-state did not run)' },
    postEmitHookFailed: { type: 'boolean', description: 'true if the configured postEmitCommitCommand was invoked and exited non-zero; false otherwise' },
    commitHash:         { type: 'string' },
    notes:              { type: 'string' }
  }
}

// BT.ticket.sdlc-task-must-verify-its-blocks-acceptance-criteria (task 2): the criteria-evidence
// stage returns EVIDENCE ONLY, never a verdict or a close decision — those are computed in engine
// code by acceptanceCriteriaVerdicts() (task 1), because this stage (like bookkeep) runs on
// MODEL.bookkeep = 'haiku' and a correctness gate must not rest on a haiku agent's self-report.
const CRITERIA_EVIDENCE_SCHEMA = {
  type: 'object',
  required: ['criteria'],
  properties: {
    criteria: {
      type: 'array',
      description: 'One entry per acceptance criterion supplied in the prompt, in the same order.',
      items: {
        type: 'object',
        required: ['criterion', 'evaluated'],
        properties: {
          criterion: { type: 'string', description: 'The criterion text, copied verbatim from the prompt (the bare string, or the object form\'s "criterion" field)' },
          evaluated: { type: 'boolean', description: 'true only if THIS run actually checked whether the criterion holds (a command was run, a file inspected, a test executed); false if nothing this run addressed it' },
          met:       { type: 'boolean', description: 'meaningful only when evaluated is true — the observed result' },
          evidence:  { type: 'string', description: 'one line quoting what was actually observed; never a guess' }
        }
      }
    },
    notes: { type: 'string' }
  }
}

// ----------------------------------------------------------------
// MODEL TIERING — the primary token lever for this pipeline.
//
// Match the model to the work (mirrors sdlc-flow). To re-tier, change one value here.
// Valid values: 'haiku' | 'sonnet' | 'opus' | undefined (inherit session model).
// ----------------------------------------------------------------
const MODEL = {
  setup:       'haiku',    // scripted git: locate the repo root, or follow the worktree free-name recipe
  enumerate:   'haiku',    // read + parse tasks.json's task list — a fixed procedure
  derive:      'opus',     // D16 fallback: author a fresh tasks.json from tasks.md's step list — real judgment, mirrors sdlc-flow.js's ensureTasks() generator
  stateLoad:   'haiku',    // read + parse one JSON file (resume only)
  implement:   'sonnet',   // writes code/content + tests against a scoped task
  fix:         'sonnet',   // targeted fixes; failures escalate, never silently ship
  test:        'haiku',    // runs the project's validation suite, reads exit codes
  triage:      'sonnet',   // classifies a failure RETRYABLE vs MAJOR — light judgment
  stateWriter: 'haiku',    // stamps timestamps, writes state.json, commits when asked
  bookkeep:    'haiku',    // lean close-out: mark tasks.md done, flip status.md + state.json block status, emit-state — a fixed procedure (mirrors /start-block)
}

// Final per-task fix pass before the loop gives up runs on a stronger model. The common path
// stays on Sonnet; only the genuinely-hard case that already failed gets an Opus shot.
// <<shared:ESCALATION_MODEL>>
const ESCALATION_MODEL = 'opus'
// <</shared:ESCALATION_MODEL>>

// Merge an optional model override into an agent's opts (omits the key when undefined, so the agent
// inherits the session model rather than receiving model: undefined).
// <<shared:withModel>>
function withModel(base, model) {
  return model ? { ...base, model } : base
}
// <</shared:withModel>>

// ----------------------------------------------------------------
// TOKEN TELEMETRY (Block A — the shared committed-state token contract)
//
// Lifted verbatim across all four engines (engines are self-contained — lift, don't import). Each
// substantive stage runs through tracedAgent, which records the injected-prompt size and the
// output-token delta off the shared budget pool. buildTokensBlock() rolls the accumulated metrics
// into the canonical `tokens` block committed state carries (per-stage + a cumulative total).
//
//   promptTokEst — injected input only (~prompt.length / 4)
//   outTok       — output-token delta from the shared budget pool; null when no +Nk target is set.
//                  sdlc-task is fully SEQUENTIAL, so the delta attributes cleanly to its stage.
//   filesReadKb  — a stage's self-reported ingestion estimate, folded in via recordFilesRead().
//   inTokEst     — D15 input-cost estimate = promptTokEst + filesReadKb→tokens (~256 tok/KB).
// ----------------------------------------------------------------
const metrics = []
// <<shared:tracedAgent>>
async function tracedAgent(prompt, opts = {}) {
  const before = (typeof budget !== 'undefined' && budget.spent) ? budget.spent() : 0
  const r = await agent(prompt, opts)
  const after = (typeof budget !== 'undefined' && budget.spent) ? budget.spent() : 0
  metrics.push({
    label: opts.label || 'agent',
    model: opts.model || 'session',
    promptTokEst: Math.round(prompt.length / 4),
    outTok: after - before > 0 ? after - before : null,
  })
  return r
}
// <</shared:tracedAgent>>

// Fold a stage's self-reported `filesReadKb` into the metrics entry the wrapper just pushed.
// Safe to call immediately after the awaited tracedAgent call — that entry is always metrics[last].
// <<shared:recordFilesRead>>
function recordFilesRead(result) {
  if (result && result.filesReadKb != null && metrics.length) {
    metrics[metrics.length - 1].filesReadKb = result.filesReadKb
  }
}
// <</shared:recordFilesRead>>

// Build the canonical `tokens` block from the accumulated per-agent metrics (Block A — the shared
// committed-state token contract, identical across all four engines): per-stage output tokens + the
// D15 input-cost estimate (promptTok + filesReadKb→tokens at ~256 tok/KB) + a cumulative total.
//
// CONTRACT SCOPE (Phase 0 /code-review carry-in): `metrics` — and therefore `tokens.total` — cover the
// SUBSTANTIVE stages only. Cheap helper / state-writer agents (the Haiku state-writer, config + baseline
// loaders) deliberately use bare agent() and are EXCLUDED; this bounded, Haiku-cheap exclusion is the
// same boundary in both engines, named here so it is explicit rather than silent.
// <<shared:buildTokensBlock>>
function buildTokensBlock() {
  const stages = metrics.map(m => {
    const filesReadKb = m.filesReadKb != null ? m.filesReadKb : null
    const inTokEst = m.promptTokEst + (filesReadKb != null ? Math.round(filesReadKb * 256) : 0)
    return { label: m.label, model: m.model, promptTokEst: m.promptTokEst, filesReadKb, inTokEst, outTok: m.outTok }
  })
  const total = stages.reduce((acc, s) => {
    acc.promptTokEst += s.promptTokEst
    acc.filesReadKb  += s.filesReadKb || 0
    acc.inTokEst     += s.inTokEst
    acc.outTok       += s.outTok || 0
    return acc
  }, { promptTokEst: 0, filesReadKb: 0, inTokEst: 0, outTok: 0 })
  return { stages, total }
}
// <</shared:buildTokensBlock>>

// ----------------------------------------------------------------
// HARNESS CONFIG — mechanism/policy split (see planning/harness.json)
//
// The engine ships NO stack defaults. A project declares its validation policy in
// planning/harness.json. The runtime has no filesystem access, so a micro-loader agent reads + parses
// the file. Returns the parsed config (or null when absent/invalid) — callers then degrade to the
// spec's `## Validation Commands`. Loaded from runDir (the worktree under --worktree; else repo root).
// ----------------------------------------------------------------
const HARNESS_CONFIG_SCHEMA = {
  type: 'object',
  required: ['present'],
  properties: {
    present: { type: 'boolean', description: 'true if planning/harness.json exists and parsed as valid JSON' },
    config: {
      type: 'object',
      description: 'The parsed harness.json (omit when present is false)',
      properties: {
        stack: { type: 'string' },
        postEmitCommitCommand: { type: 'string', description: 'OPTIONAL. Shell command the bookkeep stage runs after `mev emit-state --write` succeeds, in-place only. Absent means no post-emit command runs.' },
        validation: {
          type: 'object',
          properties: {
            checks: {
              type: 'array',
              items: {
                type: 'object',
                properties: {
                  kind:    { type: 'string', description: 'command (default) | baseline-diff | count-delta | warning-scan | forbidden-pattern-scan | skip-count-regression' },
                  name:    { type: 'string' },
                  command: { type: 'string' },
                  purpose: { type: 'string' },
                  gates:   { type: 'boolean' },
                  perTask:     { type: 'boolean' },
                  fastCommand: { type: 'string' },
                  baselineCommand: { type: 'string' },
                  reasonCommand:   { type: 'string' },
                  compareKeys:     { type: 'array', items: { type: 'string' } },
                  countPattern:    { type: 'string' },
                  failOn:          { type: 'string' },
                  warningPatterns: { type: 'array', items: { type: 'string' } },
                  rules: {
                    type: 'array',
                    items: {
                      type: 'object',
                      properties: {
                        id:               { type: 'string' },
                        pattern:          { type: 'string' },
                        paths:            { type: 'string' },
                        allowlistPattern: { type: 'string' }
                      }
                    }
                  }
                }
              }
            }
          }
        },
        flow: {
          type: 'object',
          description: 'Shared engine policy block. Historically flow-only, hence the name; this engine reads testDepth and bailReasons out of it and ignores autoMerge/prBase, which ARE flow-only.',
          properties: {
            testDepth:   { type: 'string', description: 'fast (default) | full — per-task validation depth' },
            bailReasons: { type: 'array', items: { type: 'string' }, description: 'extra project-specific immediate-bail reasons' }
          }
        }
      }
    },
    notes: { type: 'string' }
  }
}

// Named diagnostics for the two hard-bail conditions the harness-config stage can raise
// (BT.ticket.harness-config-must-bail-not-warn-on-a-malformed-payload). Named so a run journal can
// be grepped for either string.
const HARNESS_CONFIG_BAIL = {
  unparseable: 'HARNESS_CONFIG_UNPARSEABLE',
  zeroGatingChecks: 'HARNESS_CONFIG_ZERO_GATING_CHECKS',
}

async function loadHarnessConfig(cwd) {
  const result = await agent(`
You are the harness-config loader for the SDLC pipeline. Your ONLY job is to read the project's
validation-policy file and return it as structured data. Do not run any checks or modify anything.

STEP 1 — Read the config file (from the run root):
  cd ${cwd} && cat planning/harness.json 2>/dev/null && echo "__HARNESS_PRESENT__" || echo "__HARNESS_ABSENT__"

STEP 2 — Decide:
  - "__HARNESS_ABSENT__" (file missing) → present=false, omit config.
  - File printed but NOT valid JSON → present=false, notes="harness.json present but invalid JSON: <reason>".
  - File printed and valid JSON → present=true, and copy the parsed object into "config", keeping ONLY
    these fields when present: stack; postEmitCommitCommand (string, verbatim, do not interpret it);
    validation.checks[] (each: {kind, name, command, purpose, gates,
    perTask, fastCommand} plus any kind-specific fields present — baselineCommand, reasonCommand,
    compareKeys[], countPattern, failOn, warningPatterns[], rules[] ({id, pattern, paths,
    allowlistPattern})); flow ({testDepth, bailReasons[]} only — ignore autoMerge/prBase, which
    belong to the other engine). Preserve kind-specific fields verbatim; ignore any other fields.

Return your findings using the StructuredOutput tool.
`, { label: 'harness-config', schema: HARNESS_CONFIG_SCHEMA, model: 'sonnet' })

  // "__HARNESS_ABSENT__" or present-but-invalid-JSON both come back as present=false (STEP 2 above)
  // — both degrade to the spec's `## Validation Commands`, never a bail (D5 / standing rule 1: the
  // engine ships no stack defaults, and every scaffolded repo with no harness.json must keep running).
  if (!result || !result.present) return null

  // Defensive unwrap (BT.ticket.harness-config-must-bail-not-warn-on-a-malformed-payload): the
  // loader agent has been observed returning a double-wrapped payload
  // ({"config":{"present":true,"config":{...}}}) instead of the flat {present, config} shape
  // HARNESS_CONFIG_SCHEMA declares. Detect it by SHAPE, not by trusting the agent's own claimed
  // shape — either the outer `config` value itself carries a `present` key (the double-wrap
  // signature: a whole second {present, config} envelope one level too deep), or its own `.config`
  // carries `validation`/`stack` (the real config content one level too deep, present key or not).
  let cfg = result.config
  if (cfg && typeof cfg === 'object' && !Array.isArray(cfg)) {
    const nested = cfg.config
    if (nested && typeof nested === 'object' && !Array.isArray(nested)) {
      const wrapperLooksDoubled = Object.prototype.hasOwnProperty.call(cfg, 'present')
      const nestedLooksLikeConfig = Object.prototype.hasOwnProperty.call(nested, 'validation') ||
        Object.prototype.hasOwnProperty.call(nested, 'stack')
      if (wrapperLooksDoubled || nestedLooksLikeConfig) cfg = nested
    }
  }

  // present=true (planning/harness.json exists and parsed as JSON) but, even after the defensive
  // unwrap above, there is no usable config object to return — this must be a hard BAIL, never a
  // silent null-fallback: a null return here is indistinguishable downstream from "no harness.json
  // at all", and the engine cannot tell "the project configured zero checks" apart from "I could
  // not read the config" any other way (this exact ambiguity is what let BA.22.A close a block
  // having run no project gate at all, see the block record's `why`).
  if (!cfg || typeof cfg !== 'object' || Array.isArray(cfg)) {
    return { __bail: HARNESS_CONFIG_BAIL.unparseable }
  }
  return cfg
}

// Pure delta-evaluation for the skip-count-regression kind: fail ONLY when currentCount exceeds
// baselineCount (coverage silently switched off), never on a nonzero absolute count. Kept as a
// standalone pure function (no I/O) — exercised directly in unit tests without running a suite —
// and mirrored verbatim into the rendered shell snippet's comparison so the two never drift.
// <<shared:skipCountRegressionResult>>
function skipCountRegressionResult(baselineCount, currentCount, dominantReason) {
  const regressed = currentCount > baselineCount
  const delta = currentCount - baselineCount
  const message = regressed
    ? `SKIP COUNT REGRESSED: baseline=${baselineCount} current=${currentCount} (rose by ${delta})${dominantReason ? ` — dominant reason: ${dominantReason}` : ''}`
    : `skip count did not rise (baseline=${baselineCount}, current=${currentCount})`
  return { regressed, message }
}
// <</shared:skipCountRegressionResult>>

// Hardcoded, project-agnostic parse-time safety gate (mechanism, not policy — see CLAUDE.md standing
// rule 1). Independent of harness.json/spec checks: any .js .claude/workflows/ file this task's own
// tasks.json `files[]` names gets an unconditional `node --check`, in BOTH the fast-tripwire and
// full-suite render paths, even when the project ships no harness.json at all. Scoped to .js files
// only — `node --check` throws ERR_UNKNOWN_FILE_EXTENSION on non-JS paths (.md/.json) regardless of
// content, which is a false positive, not a real defect. No-op (renders '') when the task touches no
// such file — never emits a check with no target.
// <<shared:renderEngineParseChecks>>
function renderEngineParseChecks(files, cd, startIndex) {
  files = (files || []).filter(f => f.endsWith('.js'))
  if (!files || !files.length) return ''
  return files.map((f, i) => {
    const n = startIndex + i
    return `CHECK ${n} — engine-parse-safety (hardcoded parse-time gate on modified SDLC engine file — mechanism, unconditional on harness.json) [GATING — a failure here blocks the verdict]:
  ${cd}if [ -f ${f} ]; then node --check ${f}; else echo "engine-parse-safety: ${f} does not exist (deleted by this task) — nothing to parse"; fi
  echo "CHECK${n}_EXIT:$?"
  Run that line EXACTLY as written and judge it ONLY by CHECK${n}_EXIT. Do NOT substitute a bare
  node --check on ${f}: this task may legitimately DELETE ${f}, and a deleted engine has no syntax
  to be wrong. The [ -f ] guard IS the check. "Cannot find module" from an unguarded node --check
  is YOUR command failing, not this gate failing, and reporting it as a gate failure bails the run
  on work that is actually correct (observed twice on 2026-08-19).`
  }).join('\n\n')
}
// <</shared:renderEngineParseChecks>>

// Pure per-criterion verdict + close-decision evaluator (BT.ticket.sdlc-task-must-verify-its-blocks-
// acceptance-criteria, task 1). Modelled on the skipCountRegressionResult() pattern above: a pure
// function in engine code, mirrored verbatim into the bookkeep prompt text, so verdict logic and the
// agent-facing description of it cannot drift. No I/O — exercised directly by
// scripts/test_sdlc_task_criteria_verdicts.py without launching an engine run.
//
// `acceptanceCriteria` is the block record's array as declared in block.schema.json's oneOf: each
// entry is EITHER a bare string (gateable defaults to true) OR an object
// {criterion, gateable, evidence, ...} (gateable defaults to true when omitted on the object form
// too). `evidenceByCriterion` is a Map or plain object keyed by the criterion's exact text, valued
// with { evaluated: boolean, met: boolean } — the bookkeep stage supplies this from what the run
// actually observed; this function derives no evidence of its own.
//
// Verdict rules:
//   - gateable:false                                  -> 'not-evaluated' (never fails the run)
//   - gateable:true (default) and no evidence entry,
//     or evidence entry with evaluated:false           -> 'not-evaluated'
//   - gateable:true and evaluated:true and met:true     -> 'met'
//   - gateable:true and evaluated:true and met:false    -> 'unmet'
//
// Close decision: refuse when ANY criterion is 'not-evaluated' AND was not declared gateable:false
// (i.e. an undeclared not-evaluated criterion), OR when any criterion is 'unmet'. A gateable:false
// criterion reported not-evaluated never causes a refusal by itself.
//
// sdlc-task-ONLY, not a <<shared:...>> library block: sdlc-flow.js already re-reads the complete
// acceptance criteria at its own review stage (sdlc-flow.js:~2874) and this ticket explicitly
// keeps sdlc-flow.js untouched, so there is no second engine copy for scripts/build_engines.py to
// reconcile this against.
function acceptanceCriteriaVerdicts(acceptanceCriteria, evidenceByCriterion) {
  const evidenceFor = (text) => {
    if (!evidenceByCriterion) return undefined
    if (evidenceByCriterion instanceof Map) return evidenceByCriterion.get(text)
    return evidenceByCriterion[text]
  }

  const criteria = (acceptanceCriteria || []).map((entry) => {
    if (typeof entry === 'string') {
      return { text: entry, gateable: true }
    }
    // Object form: {criterion, gateable, evidence, ...}. gateable defaults to true when omitted.
    const gateable = entry && Object.prototype.hasOwnProperty.call(entry, 'gateable')
      ? !!entry.gateable
      : true
    return { text: entry && entry.criterion, gateable }
  })

  const results = criteria.map(({ text, gateable }) => {
    if (!gateable) {
      return { criterion: text, gateable, verdict: 'not-evaluated' }
    }
    const evidence = evidenceFor(text)
    if (!evidence || !evidence.evaluated) {
      return { criterion: text, gateable, verdict: 'not-evaluated' }
    }
    return { criterion: text, gateable, verdict: evidence.met ? 'met' : 'unmet' }
  })

  const unmet = results.filter((r) => r.verdict === 'unmet')
  const undeclaredNotEvaluated = results.filter((r) => r.verdict === 'not-evaluated' && r.gateable)

  let refuse = false
  let reason = null
  if (unmet.length) {
    refuse = true
    reason = `acceptance criterion UNMET: "${unmet[0].criterion}"`
  } else if (undeclaredNotEvaluated.length) {
    refuse = true
    reason = `acceptance criterion NOT EVALUATED and not declared gateable:false: "${undeclaredNotEvaluated[0].criterion}"`
  }

  return { results, refuse, reason }
}

// Render the inner project-validation check list for a Test stage. When gatingOnly is true (the fast
// per-task tripwire), emit only the checks with gates:true; --test-depth full runs the whole suite.
// When the config is absent (or carries no checks), fall back to the spec's `## Validation Commands` —
// the engine ships NO stack defaults. Handles all D6 check kinds. `engineFiles` (this task's
// .claude/workflows/ paths, if any) is additive on top of everything below — see renderEngineParseChecks.
function renderCheckList(cfg, { gatingOnly = false, cwd, engineFiles = [] } = {}) {
  let checks = cfg?.validation?.checks ?? []
  if (gatingOnly) checks = checks.filter(c => c.gates && c.perTask !== false)
  const cd = cwd ? `cd ${cwd} && ` : ''
  if (!checks.length) {
    const fallback = `The project ships no matching \`planning/harness.json\` validation ${gatingOnly ? 'GATING ' : ''}checks, so derive the checks from the spec instead:
  - Read the spec's optional "## Validation Commands" section.
  - Run each command it lists, IN ORDER (prefix each Bash call with: ${cd}). Each command is one check —
    record its name, the command, passed (true iff exit code 0), and the output on failure.
  - If the spec has no "## Validation Commands" section, run no project checks — record a single
    informational row (name "no_validation_suite", passed true) noting the project declared none.`
    const engineChecks = renderEngineParseChecks(engineFiles, cd, 1)
    return engineChecks ? `${fallback}\n\n${engineChecks}` : fallback
  }
  const rendered = checks.map((c, i) => {
    const n = i + 1
    const kind = c.kind || 'command'
    const slug = (c.name || `check${n}`).toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '')
    const gate = c.gates
      ? 'GATING — a failure here blocks the verdict'
      : 'non-gating — informational; a failure here does not block the verdict'
    const header = `CHECK ${n} — ${c.name} (${c.purpose}) [${gate}]`

    if (kind === 'baseline-diff') {
      const baselinePath = `${reportsDir}/${slug}-baseline.json`
      const currentPath = `/tmp/${blockId}-task-${slug}-current.json`
      const keysLiteral = JSON.stringify(c.compareKeys || [])
      return `${header} — baseline-diff (fail ONLY on net-new items vs the baseline snapshotted before the run):
  ${cd}${c.command} > ${currentPath} 2>/dev/null; true
  python3 << 'PYEOF'
import json, sys
try:
    b = json.load(open('${cwd ? cwd + '/' : ''}${baselinePath}', encoding='utf-8'))
except Exception as e:
    print(f'WARNING: could not load baseline ({e}) — treating all current items as pre-existing'); b = []
try:
    c = json.load(open('${currentPath}', encoding='utf-8'))
except Exception:
    c = []
keys = ${keysLiteral}
def k(v): return tuple(str(v.get(x, '')) for x in keys) if isinstance(v, dict) else (str(v),)
seen = set(k(v) for v in b)
new = [v for v in c if k(v) not in seen]
if new:
    print(f'NET-NEW ({len(new)} introduced by this run, absent from baseline):')
    for v in new[:20]: print('  ' + json.dumps(v)[:200])
    sys.exit(1)
print(f'CHECK ${n} PASSED: no net-new items (baseline {len(b)}, current {len(c)})'); sys.exit(0)
PYEOF
  echo "CHECK${n}_EXIT:$?"`
    }

    if (kind === 'skip-count-regression') {
      const baselinePath = `${reportsDir}/${slug}-skip-baseline.txt`
      const reasonStep = c.reasonCommand
        ? `\n    DOMINANT_REASON=$(${cd}${c.reasonCommand} 2>/dev/null | head -1)`
        : ''
      const reasonSuffix = c.reasonCommand ? ' — dominant reason: $DOMINANT_REASON' : ''
      return `${header} — skip-count-regression (fail ONLY when the current skip count EXCEEDS the baseline — coverage silently switched off; never fail on a nonzero absolute count):
  BASELINE_SKIPS=$(cat ${baselinePath} 2>/dev/null || echo 0)
  CURRENT_SKIPS=$(${cd}${c.command} 2>/dev/null | tail -1)
  echo "BASELINE_SKIPS=$BASELINE_SKIPS CURRENT_SKIPS=$CURRENT_SKIPS"
  if [ "$CURRENT_SKIPS" -gt "$BASELINE_SKIPS" ] 2>/dev/null; then${reasonStep}
    echo "SKIP COUNT REGRESSED: baseline=$BASELINE_SKIPS current=$CURRENT_SKIPS (rose by $((CURRENT_SKIPS - BASELINE_SKIPS)))${reasonSuffix}"
    echo "CHECK${n}_EXIT:1"
  else
    echo "CHECK${n} PASSED: skip count did not rise (baseline=$BASELINE_SKIPS, current=$CURRENT_SKIPS)"
    echo "CHECK${n}_EXIT:0"
  fi`
    }

    if (kind === 'warning-scan') {
      const outPath = `/tmp/${blockId}-task-${slug}.out`
      const alternation = (c.warningPatterns || []).map(p => `(${p})`).join('|')
      const patternSeverity = c.gates
        ? 'Because gates:true, a pattern match ALSO FAILS this check.'
        : 'Because gates:false, pattern matches are informational WARN entries — they do NOT fail the check (but DO record them).'
      return `${header} — warning-scan (run the command, gate on its exit code, then scan its output):
  ${cd}${c.command} > ${outPath} 2>&1; echo "CMD_EXIT:$?"
  grep -nE '${alternation}' ${outPath} && echo "WARNINGS_FOUND" || echo "NO_WARNINGS"
  Pass/fail: FAILS if CMD_EXIT is non-zero. Record every matched warning line. ${patternSeverity}
  echo "CHECK${n}_EXIT:<0 if CMD_EXIT==0 and not failed-by-pattern, else 1>"`
    }

    if (kind === 'forbidden-pattern-scan') {
      const ruleLines = (c.rules || []).map(r => {
        const paths = r.paths || '.'
        const allow = r.allowlistPattern ? ` | grep -vE '${r.allowlistPattern}'` : ''
        return `  Rule "${r.id}":
    ${cd}grep -rnE '${r.pattern}' ${paths}${allow} && echo "RULE ${r.id}: MATCHED (violation)" || echo "RULE ${r.id}: clean"`
      }).join('\n')
      return `${header} — forbidden-pattern scan (every rule below must find NO matches):
${ruleLines}
  This check PASSES only if EVERY rule reports "clean". If any rule MATCHED, the check FAILS.
  echo "CHECK${n}_EXIT:0  (set to 1 if any rule MATCHED, else 0)"`
    }

    // count-delta has no analog in this consolidated-per-run model — treat as a plain command run
    // (its exit code still gates if gates:true).
    const cmd = (gatingOnly && c.fastCommand) ? c.fastCommand : c.command
    return `${header}:
  ${cd}${cmd}
  echo "CHECK${n}_EXIT:$?"`
  }).join('\n\n')
  const engineChecks = renderEngineParseChecks(engineFiles, cd, checks.length + 1)
  return engineChecks ? `${rendered}\n\n${engineChecks}` : rendered
}

// Snapshot baseline artifacts for any baseline-diff / skip-count-regression checks before the first
// task, so the test stages can diff current output vs the pre-run state and fail only on regressions.
// Resume-safe: only writes a baseline that does not already exist. No-op when no such checks are
// configured. skip-count-regression writes a bare-integer count file (not JSON) at a sibling path.
// <<shared:snapshotBaselines>>
async function snapshotBaselines(cfg, cwd) {
  const checks = (cfg?.validation?.checks || [])
    .filter(c => (c.kind === 'baseline-diff' || c.kind === 'skip-count-regression') && c.baselineCommand)
  if (!checks.length) return
  const steps = checks.map(c => {
    const slug = (c.name || 'check').toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '')
    const path = c.kind === 'skip-count-regression'
      ? `${reportsDir}/${slug}-skip-baseline.txt`
      : `${reportsDir}/${slug}-baseline.json`
    return `Baseline "${c.name}" -> ${path}:
  cd ${cwd} && mkdir -p ${reportsDir}
  cd ${cwd} && { [ -f ${path} ] && echo "BASELINE EXISTS (kept): ${path}" || { ${c.baselineCommand} > ${path} 2>/dev/null; echo "BASELINE WRITTEN: ${path}"; } ; }`
  }).join('\n\n')
  await agent(`
You are the baseline-snapshot agent for the SDLC pipeline. Capture the pre-run baseline for each
baseline-diff / skip-count-regression validation check BEFORE any implementation runs. Run each block
exactly as written. Do NOT modify source. Existing baselines are kept (resume-safe).

${steps}

Return using StructuredOutput: done=true, and note which baselines were written vs already present.
`, { label: 'baseline-snapshot', schema: { type: 'object', required: ['done'], properties: { done: { type: 'boolean' }, notes: { type: 'string' } } }, model: 'haiku' })
}
// <</shared:snapshotBaselines>>

// ----------------------------------------------------------------
// COMMITTED AUTHORITATIVE STATE (Block A)
//
// `state` is the in-memory source of truth; writeTaskState() persists it to sdlc-task-state.json.
// WRITE-ONLY — no git command ever runs here (see writeTaskState below for why). The runtime has no
// fs/clock, so a Haiku writer stamps started_at/updated_at and does the Write. Committed
// report/code commits remain the authoritative resume signal; state on disk is the at-a-glance index,
// read back only by --resume.
// ----------------------------------------------------------------
const state = {
  spec_slug: blockId,
  mode: useWorktree ? 'worktree' : 'in-place',
  branch: baseBranchName,
  worktree_path: '',
  status: 'running',
  current_task: null,
  // Resolved by BT.ticket.engine-terminal-state-needs-evidence task 2: emitStateRan was declared,
  // instructed, gated and logged in six sites but never persisted to disk (a key in 0 of 150
  // corpus state files). Written here, once, from bookkeepResult.emitStateRan just before the
  // final writeTaskState() call below — null until the bookkeep stage runs or is skipped
  // (bailed/reconcile_failed), matching every other field this object carries only from the
  // point its stage actually resolves it.
  emitStateRan: null,
  // DELIBERATE (ticket-sdlc-task-resume-truncates-run-state): tasks_run stays PER-INVOCATION
  // telemetry — "what did THIS invocation run" — and is never unioned across a resume, because
  // doing so would erase the record of which run did what. `tasks` below is the opposite: it is
  // the resume breadcrumb and must answer "has this task ever passed?", so it is seeded from the
  // prior state file's tasks object on --resume (see the state-load block) and is CUMULATIVE.
  tasks_run: [],
  tasks: {},        // "N": { status, attempts, summary, issues, fixes, decisions, files_changed, commit, validated }
  bail_reason: null,
  // APPEND-ONLY (BT.ticket.bails-must-be-append-only) — one entry per bail: {occurred_at, task_id,
  // check_id, failing_artifact, ownership, bail_class, reason, resolution}. A bail that is later
  // resumed cleanly is ANNOTATED (resolution: 'resumed-clean'), never removed. `bail_reason` above
  // stays as a mirror of the newest entry's `reason` — never independently truthful on its own.
  // Nothing reads `bails` yet (clustering/counting is separate, out-of-scope work); this is the
  // durable per-run record that work will consume.
  bails: [],
  // BT.ticket.criteria-verdict-stage-silently-no-ops-and-is-never-persisted (task 3): the
  // per-criterion verdict list acceptanceCriteriaVerdicts() computes (see criteriaVerdicts
  // further down this file), mirrored here so writeTaskState()'s wholesale JSON.stringify(state)
  // carries it to disk instead of the value dying with the process when the session ends.
  // ABSENT-OR-EMPTY, NEVER MISSING: stays [] here (Criteria stage not yet reached, or this run
  // never reaches it — legacy tasks-md spec, bail, reconcile_failed), and is assigned the real
  // computed array the moment the Criteria stage produces one (see the `state.criteriaVerdicts =`
  // assignment below). Every intermediate write in between (per-task buildPassPayload/
  // buildBailPayload snapshots) legitimately serializes whatever this field holds at that point
  // in the run — [] until the Criteria stage runs, which is correct: those stages fire before
  // Criteria ever does.
  criteriaVerdicts: [],
  tokens: { stages: [], total: { promptTokEst: 0, filesReadKb: 0, inTokEst: 0, outTok: 0 } },  // Block A — refreshed on every write
}

// Learned from the first successful state write of this process. Later writes are handed it as a
// literal so they can skip reading the state file back — the `cat` exists only to preserve
// started_at, and once any write has reported the value it used, re-reading it is a wasted Bash
// round trip. A fresh process — including every --resume — starts empty, so the first write always
// does the full read-and-preserve path and resume semantics are unchanged. A failed write leaves
// this null, so the next write re-reads rather than inventing a new started_at.
let cachedStartedAt = null

// Persist `state` to sdlc-task-state.json. This is deliberately WRITE-ONLY — no git command runs
// here, and the `commit` option (if a caller still passes one) is ignored.
//
// Why: this run-state lives under planning/<blockId>/sdlc/, and under D46 every vaulted repo's
// planning/ is a relative symlink into a brain-owned vault, so `git add planning/...` fails with
// "fatal: pathspec is beyond a symbolic link". The state-writer agent used to "repair" that failure
// by operating in the brain repo directly and checking out the run's branch there — contaminating
// HQ with spec-named branches and a `chore: sdlc-task state` commit per task. Run-state is read back
// only off disk (by --resume, via ${stateFile}), never out of git history, so there is no need to
// commit it at all — removing the commit removes the git verb the agent was getting wrong.
async function writeTaskState(label, { cwd }) {
  state.tokens = buildTokensBlock()   // Block A — refresh the token roll-up before persisting
  const firstWrite = cachedStartedAt === null
  // On later writes, started_at is already known (cachedStartedAt) — splice it into the
  // serialized object BEFORE JSON.stringify, immediately after "branch", so the agent is
  // handed a JSON blob that already carries the correct value and only has to insert
  // "updated_at". This removes the two-value ambiguity that let the agent stamp both keys
  // from the cached literal (see ticket-state-write-updated-at-freeze). On a first write the
  // object is serialized exactly as before — the agent still derives started_at from STEP 1's
  // `cat` output.
  const stateJson = firstWrite
    ? JSON.stringify(state, null, 2)
    : JSON.stringify((() => {
        const entries = Object.entries(state)
        const branchIdx = entries.findIndex(([k]) => k === 'branch')
        entries.splice(branchIdx + 1, 0, ['started_at', cachedStartedAt])
        return Object.fromEntries(entries)
      })(), null, 2)
  // The two JS-side `state.bails` mutation sites (inline bail-assignment, terminal reconcile —
  // BT.ticket.bails-must-not-mint-time-in-the-engine) have no JS clock legal under the Workflow
  // runtime shim, so they stamp the same "__BAIL_OCCURRED_AT__" sentinel buildBailPayload already
  // uses and leave it in the LIVE `state` object for this dedicated writer to resolve — this is
  // the ONLY place either sentinel ever reaches disk from that route, so the substitution below is
  // what keeps the guarantee that no bails[] entry persists with it unresolved.
  const bailOccurredAtNote = stateJson.includes('__BAIL_OCCURRED_AT__')
    ? `\n\nThe object below also contains one or more literal "__BAIL_OCCURRED_AT__" placeholder
  strings inside \`bails[]\` entries (a JS-side bail was just recorded, which has no shim-legal
  clock of its own). Replace EVERY occurrence of that exact literal with NOW — the same value you
  read as the first line of STEP 1's output — in this SAME turn, alongside the
  started_at/updated_at insertion. Never leave one unresolved on disk.`
    : ''
  const stepTwoText = firstWrite
    ? `STEP 2 — write ${stateFile} with EXACTLY this JSON, but inserting two extra top-level keys
  "started_at" (preserved or NOW, per STEP 1) and "updated_at" (NOW) right after "branch". Valid JSON only
  (double quotes, no trailing commas, no markdown fences). The object to write (verbatim except for
  adding those two timestamp keys):${bailOccurredAtNote}
${stateJson}`
    : `STEP 2 — write ${stateFile} with EXACTLY this JSON, but inserting exactly one extra top-level
  key: "updated_at" (NOW), right after "started_at" (already present in the object below,
  immediately after "branch" — it was set from the value given in STEP 1). Valid JSON only
  (double quotes, no trailing commas, no markdown fences). The object to write (verbatim except for
  adding that one timestamp key):${bailOccurredAtNote}
${stateJson}`
  const result = await agent(`
You maintain the run-state for an /sdlc-task pipeline. You run from the run root. Write ONE JSON
file to disk — do NOT run git commands, do not run checks, do not edit source, do not touch anything
else. This state is read back off disk only (never out of git); it is deliberately not committed.

STEP 1 — run this as ONE Bash call, exactly as written. Do not split it into several calls.
${firstWrite
  ? `  cd ${cwd} && mkdir -p ${blockDir}/sdlc && date -u +%Y-%m-%dT%H:%M:%SZ && { cat ${stateFile} 2>/dev/null || echo "__NO_STATE__"; }
  The FIRST line of output is NOW. Everything after it is the existing state file, or __NO_STATE__
  when there is none. If that file exists and has a "started_at" value, REUSE it verbatim for
  started_at below. Otherwise started_at = NOW.`
  : `  cd ${cwd} && date -u +%Y-%m-%dT%H:%M:%SZ
  That single line of output is NOW. started_at is already known for this run — use exactly
  "${cachedStartedAt}". Do NOT read the existing state file and do NOT run mkdir: the directory
  already exists and an earlier write in this run already established started_at.`}

${stepTwoText}

Use the Write tool for the file. Do not run \`git add\`, \`git commit\`, \`git checkout\`,
\`git switch\`, or \`git branch\` — this write is disk-only. Return via StructuredOutput: written=true
once the file is written to disk, startedAt set to the started_at value you used, and updatedAt set
to the updated_at value you used.
`, withModel({ label: `state:${label}`, schema: STATE_WRITE_SCHEMA }, MODEL.stateWriter))
  if (result && result.startedAt) cachedStartedAt = result.startedAt
  if (!result || !result.written) {
    log(`(state) could not persist task state for "${label}" — continuing`)
  }
  // Freeze-detection guard (non-fatal): on a later write, updated_at should never equal
  // started_at — that is the exact signature of the prompt ambiguity this ticket fixes. Warn
  // only; never throw, retry, or touch cachedStartedAt / disk content.
  if (!firstWrite && result && result.updatedAt && result.updatedAt === result.startedAt) {
    log(`state:${label} WARNING updated_at froze at started_at (${result.updatedAt}) — see ticket-state-write-updated-at-freeze`)
  }
  return result
}

// ================================================================
// PHASE 0: SETUP — locate the repo root, or create the isolated worktree (--worktree)
// ================================================================
phase('Setup')
log(`Setting up (${useWorktree ? 'isolated worktree' : 'in place'})${resumeMode ? ', resume' : ''}...`)

// Resolve repoRoot ONCE, in the engine, before the setup agent ever runs — see resolveRepoRoot()
// above for why this exists. Everything downstream gets repoRoot (and candidateTierPrefix) as a
// literal; only [branchName] and currentBranch remain agent-derived (branchName is genuinely
// chosen inside STEP 2 for worktree mode, and currentBranch reflects live repo state at the
// moment setup runs — neither can be pre-computed here).
const repoRootResult = await resolveRepoRoot()
if (!repoRootResult) {
  log('resolveRepoRoot agent returned null — aborting pipeline before any branch/worktree work')
  return { error: 'resolveRepoRoot failed', blockId }
}
const { repoRoot, gitCommonDir, tierPrefix: invocationTierPrefix, brainTomlAtRoot } = repoRootResult
log(`Resolved repoRoot: ${repoRoot} (git-common-dir: ${gitCommonDir})`)

const setupResult = await tracedAgent(`
You are the setup agent for the lean /sdlc-task pipeline. ${useWorktree
  ? 'Create (or locate) ONE isolated git worktree for this run.'
  : 'The pipeline runs IN PLACE on the current branch — do NOT create a worktree.'} All bash commands run
from the MAIN REPO ROOT (your current CWD).

Target:
  Spec:              ${blockId}
  Block record:      ${blockRecordFile} (preferred spec source, D65 stage 2)
  Legacy spec file:  ${specFile} (fallback — only used when the block record is absent)
${useWorktree ? `  Base name:  ${baseBranchName}` : ''}

STEP 1 — repoRoot and candidateTierPrefix are GIVEN, not derived. The engine already resolved them
  before you were invoked:
    repoRoot = ${repoRoot}
    candidateTierPrefix = "${invocationTierPrefix}" (the invoking directory's path relative to
      repoRoot, with a trailing slash, e.g. "business/", or "" when /sdlc-task was invoked at the
      repo root — it reflects where /sdlc-task was actually invoked from, not runDir, which may differ)
  Use both values VERBATIM everywhere below. Do NOT re-derive repoRoot with \`${GIT} rev-parse
  --show-toplevel\` or any other command, and do NOT cd outside repoRoot at any point in this
  recipe — re-deriving it is exactly the failure this step exists to prevent.
  Also run: ${GIT} rev-parse --abbrev-ref HEAD       (store as currentBranch — report this verbatim
    in the final StructuredOutput call regardless of mode; it is how the engine detects a --worktree
    run that silently resolved to the current branch)
${useWorktree ? `
WORKTREE MODE (--worktree) — create or reuse an isolated worktree:
${resumeMode ? `  RESUME — reuse the existing worktree for this spec if present:
    a. ${GIT} worktree list | grep "trees/${baseBranchName}" && echo "WT_EXISTS" || echo "WT_MISSING"
    b. ${GIT} branch --list "${baseBranchName}"
    - WT_EXISTS → REUSE verbatim. branchName="${baseBranchName}", wasCreated=false. Skip to STEP 2c.
    - WT_MISSING but branch "${baseBranchName}" exists (orphan branch, dir removed) → re-attach (NO -b flag):
        mkdir -p trees
        ${GIT} worktree add --no-checkout trees/${baseBranchName} ${baseBranchName}
        ${GIT} -C trees/${baseBranchName} sparse-checkout init --cone
        ${GIT} -C trees/${baseBranchName} sparse-checkout set $(${GIT} ls-tree HEAD --name-only -d | tr '\\n' ' ')
        ${GIT} -C trees/${baseBranchName} checkout
        ${GIT} ls-files --others --ignored --exclude-standard -- . | grep -E '(^|/)\\.env(\\.[^/]*)?$' | grep -Ev '(^|/)(node_modules|\\.venv|venv|trees|vendor)/' | while IFS= read -r f; do dest="trees/${baseBranchName}/$f"; if [ ! -f "$dest" ]; then mkdir -p "$(dirname "$dest")"; cp "$f" "$dest"; echo "ENV_COPIED: $f"; fi; done
      branchName="${baseBranchName}", wasCreated=false. Skip to STEP 2c.
    - Neither exists → fall through and create a fresh worktree as normal.
` : ''}  STEP 2 — Find a free worktree name. Start with candidate "${baseBranchName}"; for each candidate run:
      ${GIT} worktree list | grep "trees/<candidate>"
      ${GIT} branch --list "<candidate>"
    If BOTH return nothing → the candidate is free; use it. Otherwise try "${baseBranchName}-2",
    "${baseBranchName}-3", … up to "-10". Store the chosen name as branchName.
    FAIL CLOSED — if NONE of "${baseBranchName}" through "${baseBranchName}-10" come back free (all
    10 are already a worktree and/or a branch), do NOT fall back to currentBranch or invent an
    unlisted name. STOP here: set worktreeFailed=true, worktreeFailureReason naming the spec slug
    "${blockId}" and every candidate tried (e.g. "${baseBranchName} through ${baseBranchName}-10 all
    taken"), and skip straight to the final StructuredOutput call with runDir/branchName left as
    whatever was last computed — the engine bails on worktreeFailed before using them.

  STEP 2b — Create the worktree (replace [branchName] with the chosen name):
    a. mkdir -p trees
    b. ${GIT} worktree add --no-checkout trees/[branchName] -b [branchName]
    c. ${GIT} -C trees/[branchName] sparse-checkout init --cone
    d. # Cone ALL tracked top-level directories — stack-agnostic, no project-layout assumptions (D5/P5).
       ${GIT} -C trees/[branchName] sparse-checkout set $(${GIT} ls-tree HEAD --name-only -d | tr '\\n' ' ')
    e. ${GIT} -C trees/[branchName] checkout
    f. Discover and copy EVERY gitignored env-shaped file (.env, .env.local, .env.* in any
       directory) from ${repoRoot} into trees/[branchName], preserving each file's path relative to
       the repo root (creating parent directories as needed — so app/.env lands at
       trees/[branchName]/app/.env). Only files git actually ignores; exclude node_modules/,
       .venv/, venv/, trees/, and vendor/; never overwrite a file that already exists in the
       worktree. Run:
         ${GIT} ls-files --others --ignored --exclude-standard -- . | grep -E '(^|/)\.env(\.[^/]*)?$' | grep -Ev '(^|/)(node_modules|\.venv|venv|trees|vendor)/' | while IFS= read -r f; do dest="trees/[branchName]/$f"; if [ ! -f "$dest" ]; then mkdir -p "$(dirname "$dest")"; cp "$f" "$dest"; echo "ENV_COPIED: $f"; fi; done
       Record the list of "ENV_COPIED:" lines — report them in STEP 4.
    g. # COMMIT-SAFETY GUARD EXEMPT: this is the worktree-init commit — its index is legitimately
       # populated after checkout (it is the very first commit on the branch), so the guard's
       # "empty index against a non-empty HEAD" signal cannot fire here; --allow-empty is orthogonal.
       ${GIT} -C trees/[branchName] commit --allow-empty -m "chore: init worktree [branchName]"
    Set wasCreated=true.
    FAIL CLOSED — if ANY command in this step (a-g) errors or exits non-zero (in particular
    \`${GIT} worktree add\`), STOP immediately. Do NOT fall back to running the rest of this pipeline
    on the current branch in the main tree. Set worktreeFailed=true, worktreeFailureReason with the
    failing command and its exact error output, and skip straight to the final StructuredOutput call
    without attempting STEP 2c or STEP 3.

  STEP 2c — Fix the planning/ symlink for the worktree (run from the MAIN repo root, for ALL worktree
    paths — fresh create, re-attach, or reuse). In brain-vaulted repos the MAIN repo's \`planning\` is
    a RELATIVE symlink into a vault (e.g. planning -> ../_planning/<repo>) and is gitignored; from
    inside trees/[branchName]/ that relative target breaks. Point the worktree's planning/ at the SAME
    real vault via an ABSOLUTE symlink (gitignored, so never committed/merged) so reads+writes hit the
    vault and no real planning/ dir is created to clobber the link on merge:
      if [ -L planning ]; then
        TARGET="$(python3 -c "import os; print(os.path.realpath('planning'))")"
        rm -f trees/[branchName]/planning
        ln -s "$TARGET" trees/[branchName]/planning
        echo "PLANNING_SYMLINK_FIXED -> $TARGET"
      else
        echo "PLANNING_REAL_DIR (no symlink fix needed)"
      fi
    If \`planning\` is a real tracked directory (non-vaulted repo), the sparse-checkout already
    populated it — do nothing.

  STEP 2d — Capture the ACTUAL worktree listing (run from the MAIN repo root, for ALL worktree paths
    — fresh create, re-attach, or reuse; this is what makes the intermittent silent main-tree
    fallback impossible rather than merely unlikely — BT.ticket.sdlc-task-worktree-flag-is-
    intermittently-ignored, task 2). Do NOT trust your own STEP 2/2b bookkeeping of branchName/runDir
    for the final report — capture ground truth instead:
      ${GIT} worktree list --porcelain
    Report the COMPLETE, UNMODIFIED stdout of that command as worktreeListPorcelain (every line,
    every worktree entry — do not filter it down to the one you think is relevant; the engine parses
    it itself). If the command errors, report worktreeListPorcelain as the literal string
    "COMMAND_FAILED: " followed by the error output.
` : `
IN-PLACE MODE — no worktree. branchName=currentBranch, wasCreated=false, worktreeFailed=false,
  worktreeFailureReason="". runDir=${repoRoot} (repoRoot is GIVEN from STEP 1 — do not recompute it).
`}
STEP 3 — Compute runDir (repoRoot is GIVEN as ${repoRoot} — use it verbatim, never recompute it):
  ${useWorktree ? `runDir = "${repoRoot}" + "/trees/" + branchName` : `runDir = "${repoRoot}"`}

STEP 4 — Report pipeline-start inputs (run these from runDir):
  a. Spec source AND location (D65 stage 2 + tier resolution) — the block record is checked FIRST
     and is preferred; tasks.md is only a fallback for a legacy spec that predates the block-record
     migration. Check the ROOT first (it always wins when the spec exists at both locations):
       cd <runDir> && ls ${blockRecordFile} 2>/dev/null && echo "RECORD_ROOT_EXISTS" || echo "RECORD_ROOT_MISSING"
       cd <runDir> && ls ${specFile} 2>/dev/null && echo "LEGACY_ROOT_EXISTS" || echo "LEGACY_ROOT_MISSING"
     ONLY IF candidateTierPrefix (from STEP 1) is non-empty, ALSO check the tier location:
       cd <runDir> && ls <candidateTierPrefix>${blockRecordFile} 2>/dev/null && echo "RECORD_TIER_EXISTS" || echo "RECORD_TIER_MISSING"
       cd <runDir> && ls <candidateTierPrefix>${specFile} 2>/dev/null && echo "LEGACY_TIER_EXISTS" || echo "LEGACY_TIER_MISSING"
     Resolve, in this order:
       - specFoundInTier = true ONLY when neither RECORD_ROOT_EXISTS nor LEGACY_ROOT_EXISTS, AND
         either RECORD_TIER_EXISTS or LEGACY_TIER_EXISTS. Otherwise specFoundInTier = false — this
         is what makes the root win whenever the spec exists at both locations.
       - specSource, evaluated at the WINNING location (root unless specFoundInTier): "block-record"
         if that location's block record exists; else "tasks-md" if that location's legacy file
         exists; else "missing".
       - specFileExists = true iff specSource != "missing".
     tierPrefix = candidateTierPrefix from STEP 1 (report it as-is, even when specFoundInTier is
     false or specSource is "missing" — it is the location that was CHECKED, not just a winner).
  b. Block status — find this spec's row in status.md:
       cd <runDir> && grep -iE "${blockId}" planning/status.md | head -5
     blockStatus = the title-case Status value (Not started / In progress / Done / Blocked / Skipped),
     or "Unknown" if no row is found.
  c. Thin-spec check (D19) — evaluate ONLY when specSource == "tasks-md" (the legacy path — a
     block-record spec is authored structured JSON, not markdown prose, so the {{TOKEN}}/section checks
     below do not apply to it) AND this is NOT a resume run (a fresh run about to spend implement
     tokens). Set specThin=true ONLY on these high-confidence signals (a blocked valid spec is far
     costlier than a missed thin one — when in doubt do NOT flag):
       - cd <runDir> && grep -n '{{' ${specFile}  → any unfilled {{TOKEN}} is thin.
       - The '## Acceptance Criteria' section has no real '- ' bullet (empty, or only a template seed) → thin.
     Do NOT flag bare 'TODO'/'TBD' prose, do NOT treat '<...>' as a token (legitimate in 'Vec<T>', globs),
     never flag the Amendment Log seed '_No amendments yet._'. Else specThin=false, thinReason="".
${useWorktree ? `  d. Env files seeded — collect the "ENV_COPIED: <path>" lines printed during worktree setup
     (STEP 2b step f, or the RESUME re-attach path) into envFilesCopied (one path per entry; empty
     array if none printed — that means no gitignored env-shaped file exists in this repo, not that
     the copy failed silently). Report this list; a run missing config should say so at setup time
     rather than surface later as a confusing downstream failure (e.g. a fallback DB connection).
     Note: the worktree's path is derived from the SPEC SLUG (trees/${baseBranchName}), not any
     program/block ID — anything discovering it externally must use \`git worktree list\`, not guess.
` : ''}
STEP 5 — Capture the emoji-gate diff base — the HEAD short sha as it stands NOW, before any task commit:
  cd <runDir> && ${GIT} rev-parse --short HEAD     (store as baseSha)

Return your result using the StructuredOutput tool:
  runDir, branchName, currentBranch, baseSha, wasCreated, worktreeFailed, worktreeFailureReason, specFileExists, specSource, tierPrefix, specFoundInTier, blockStatus, specThin, thinReason,${useWorktree ? ' envFilesCopied, worktreeListPorcelain,' : ''} notes.
  ${useWorktree ? 'If worktreeFailed is true, runDir/branchName/baseSha may be whatever was last computed before stopping — the engine ignores them and bails on worktreeFailed alone. Do NOT report mode:"worktree" success fields (a clean runDir/branchName) when worktreeFailed is true.' : ''}
`, withModel({ label: 'setup', schema: SETUP_SCHEMA, phase: 'Setup' }, MODEL.setup))

if (!setupResult) {
  log('Setup agent returned null — aborting pipeline')
  return { error: 'Setup failed', blockId }
}
let { runDir, branchName } = setupResult
const { baseSha } = setupResult

// WORKTREE FAIL-CLOSED GUARD (BT.ticket.sdlc-task-worktree-flag-is-intermittently-ignored, task 1) —
// decided HERE IN JS, never left to the setup agent's own self-report, exactly like the binding/
// brain-root guards below. Two independent checks, either one alone is enough to bail:
//   1. worktreeFailed: the agent explicitly reported it could not resolve a free branch name (all
//      10 candidates taken) or a worktree-creation command errored, and correctly stopped rather
//      than falling back — this trusts the self-report ONLY for the fail case, never the success case.
//   2. A cause-independent cross-check that catches a fallback the agent did NOT self-report: if
//      --worktree was requested but the reported branchName/runDir match the branch/root the run
//      STARTED on, worktree mode silently resolved to the main tree — the exact measured defect
//      (mode:"worktree" with branch:"main", runDir:<main tree>). This does not rely on the model
//      noticing its own failure, so it also catches a future prompt regression.
// This must run BEFORE any state is recorded and BEFORE the binding/brain-root/population guards,
// so a failed-closed worktree run never gets as far as touching the main tree.
if (useWorktree) {
  if (setupResult.worktreeFailed) {
    log(`WORKTREE SETUP FAILED CLOSED for ${blockId}: ${setupResult.worktreeFailureReason || '(setup agent reported worktreeFailed=true with no reason)'} — refusing to run against the main tree.`)
    return { error: 'Worktree setup failed', reason: setupResult.worktreeFailureReason || 'worktree resolution/creation failed', blockId }
  }
  if (branchName === setupResult.currentBranch || runDir === repoRoot) {
    log(`WORKTREE FAIL-CLOSED for ${blockId}: --worktree was requested but setup resolved to the main tree instead of an isolated worktree (branchName="${branchName}", currentBranch="${setupResult.currentBranch}", runDir="${runDir}", repoRoot="${repoRoot}") — this is the measured intermittent defect; bailing rather than silently committing onto the current branch.`)
    return { error: 'Worktree setup failed closed', reason: `branchName/runDir resolved to the main tree (branchName=${branchName}, currentBranch=${setupResult.currentBranch}, runDir=${runDir}, repoRoot=${repoRoot})`, blockId }
  }

  // WORKTREE-LIST CROSS-CHECK (task 2) — stop ASSUMING the setup agent's own runDir/branchName are
  // real and read them back from `git worktree list --porcelain` ground truth instead. This is what
  // makes the intermittent silent main-tree fallback impossible rather than merely unlikely: even a
  // setup agent that fabricated a plausible-looking runDir/branchName without ever actually creating
  // the worktree is caught here, because no such entry exists in the real listing.
  const worktreeListRaw = setupResult.worktreeListPorcelain || ''
  if (!worktreeListRaw || worktreeListRaw.startsWith('COMMAND_FAILED:')) {
    log(`WORKTREE FAIL-CLOSED for ${blockId}: \`git worktree list --porcelain\` was not captured or errored during setup (worktreeListPorcelain=${JSON.stringify(worktreeListRaw)}) — cannot verify the worktree actually exists; refusing to run against an unverified path.`)
    return { error: 'Worktree setup failed closed', reason: `worktree listing missing or failed: ${worktreeListRaw || '(empty)'}`, blockId }
  }
  const worktreeEntries = parseWorktreeListPorcelain(worktreeListRaw)
  const listedEntry = worktreeEntries.find(e => e.path === runDir)
  if (!listedEntry) {
    log(`WORKTREE FAIL-CLOSED for ${blockId}: expected worktree at runDir="${runDir}" is ABSENT from the worktree list porcelain output (listed paths: ${worktreeEntries.map(e => e.path).join(', ') || '(none)'}) — the setup agent's reported runDir does not correspond to a real worktree; bailing rather than running against it.`)
    return { error: 'Worktree setup failed closed', reason: `expected runDir ${runDir} absent from worktree list (listed: ${worktreeEntries.map(e => e.path).join(', ') || '(none)'})`, blockId }
  }
  if (listedEntry.branch !== branchName) {
    log(`WORKTREE FAIL-CLOSED for ${blockId}: worktree at runDir="${runDir}" is on branch "${listedEntry.branch}" per \`git worktree list --porcelain\`, but setup reported branchName="${branchName}" — expected vs. observed mismatch; bailing rather than trusting the mismatched self-report.`)
    return { error: 'Worktree setup failed closed', reason: `branch mismatch at runDir ${runDir}: expected branchName=${branchName}, observed=${listedEntry.branch}`, blockId }
  }
  // Ground truth confirmed the self-report exactly; re-assign from the listed entry anyway so
  // downstream state is derived from `git worktree list`, never merely "assumed correct because it
  // matched" (acceptance criterion: "read back ... rather than assumed").
  runDir = listedEntry.path
  branchName = listedEntry.branch
  log(`WORKTREE-LIST CROSS-CHECK passed for ${blockId}: runDir and branchName confirmed against \`git worktree list --porcelain\` ground truth.`)
}

state.branch = branchName
state.base_sha = baseSha
state.worktree_path = useWorktree ? runDir : ''
log(`Run root: ${runDir} | branch: ${branchName} | base: ${baseSha}`)

// BINDING / BRAIN-ROOT / POPULATION GUARDS — run before any task work, before even the
// enumerate stage. Covers BOTH the worktree branch and the in-place branch (runDir === repoRoot
// in-place). See verifySetupBinding() above for why the decision is made here in JS.
const bindingCheck = await verifySetupBinding(runDir, useWorktree)
if (!bindingCheck) {
  log('verifySetupBinding agent returned null — aborting pipeline before any task runs')
  return { error: 'Setup binding guard failed', reason: 'verification agent returned null', blockId }
}
if (!bindingCheck.gitCommonDir.startsWith(repoRoot)) {
  log(`BINDING GUARD FAILED: run directory's git-common-dir (${bindingCheck.gitCommonDir}) does not resolve under the engine-resolved repoRoot (${repoRoot}) — aborting before any task runs`)
  return { error: 'Setup binding guard failed', reason: `git-common-dir ${bindingCheck.gitCommonDir} does not resolve under repoRoot ${repoRoot}`, blockId }
}
log(`BINDING GUARD passed: git-common-dir (${bindingCheck.gitCommonDir}) resolves under repoRoot (${repoRoot})`)
if (bindingCheck.brainTomlAtRun && !brainTomlAtRoot) {
  log(`BRAIN-ROOT GUARD FAILED: run root (${runDir}) holds a brain.toml but the engine-resolved invocation root (${repoRoot}) did not — this run has adopted the brain root as its repo root — aborting before any task runs`)
  return { error: 'Setup binding guard failed', reason: `brain.toml present at run root ${runDir} but absent at invocation root ${repoRoot}`, blockId }
}
log('BRAIN-ROOT GUARD passed: run root brain.toml presence matches the invocation root')
if (useWorktree) {
  const missingCount = bindingCheck.missingCount || 0
  if (missingCount > 0) {
    log(`POPULATION GUARD FAILED: ${missingCount} tracked path(s) missing on disk in worktree ${runDir} — examples: ${(bindingCheck.missingSample || []).join(', ') || '(none reported)'} — aborting before any task runs`)
    return { error: 'Setup binding guard failed', reason: `${missingCount} tracked paths missing from worktree ${runDir}`, blockId }
  }
  log('POPULATION GUARD passed: every tracked path in the worktree index is present on disk')
}

if (useWorktree) {
  const envFilesCopied = setupResult.envFilesCopied || []
  log(envFilesCopied.length
    ? `Env files copied into worktree: ${envFilesCopied.join(', ')}`
    : 'Env files copied into worktree: none found')
  log(`Worktree path derives from the spec slug (trees/${branchName}), not any block ID — use "git worktree list" to locate it, never guess.`)
}

// Tier resolution — the candidate prefix is always reported (STEP 1); only actually applied to
// blockDir and everything derived from it when the setup agent found the spec ONLY at the tier
// location, never at the root (specFoundInTier). The root wins whenever the spec exists at both —
// see SETUP_SCHEMA.specFoundInTier and the STEP 4a resolution order.
const tierPrefixCandidate = setupResult.tierPrefix || ''
const rootBlockRecordFile = blockRecordFile   // pre-tier root form, kept for the Missing-spec abort
const rootSpecFile        = specFile          // pre-tier root form, kept for the Missing-spec abort
if (tierPrefixCandidate && setupResult.specFoundInTier) {
  blockDir        = `${tierPrefixCandidate}planning/${blockId}`
  blockRecordFile = `${tierPrefixCandidate}planning/blocks/${blockId}.json`
  specFile        = `${blockDir}/tasks.md`
  tasksJsonFile   = `${blockDir}/tasks.json`
  breakdownFile   = `${blockDir}/breakdown.md`
  reportsDir      = `${blockDir}/sdlc/reports`
  stateFile       = `${blockDir}/sdlc/sdlc-task-state.json`
  log(`Spec resolved at tier location (${tierPrefixCandidate}) — not found at the root.`)
}

// D65 stage 2: resolve which spec source this run actually has. specSource defaults to 'tasks-md'
// only if the setup agent omitted the field (older cached run) — never silently prefer a source
// that was not actually checked.
const specSource = setupResult.specSource || (setupResult.specFileExists ? 'tasks-md' : 'missing')
if (specSource === 'block-record') {
  specFile = blockRecordFile
  log(`Spec source: block record (${specFile})`)
} else if (specSource === 'tasks-md') {
  log(`Spec source: legacy tasks.md (${specFile}) — no block record found at ${blockRecordFile}`)
}
const specDesc = specSource === 'block-record'
  ? '(JSON block record — what/why/acceptance_criteria/testing_strategy/validation_commands fields)'
  : '(prose — Goal, Acceptance Criteria, Validation Commands)'

if (!setupResult.specFileExists) {
  const rootPaths = `${rootBlockRecordFile} or ${rootSpecFile}`
  const tierPaths = tierPrefixCandidate ? `${tierPrefixCandidate}planning/blocks/${blockId}.json or ${tierPrefixCandidate}planning/${blockId}/tasks.md` : null
  log(`No spec found — searched the root (${rootPaths})${tierPaths ? ` AND the tier location (${tierPaths})` : ''}. /sdlc-task expects an authored spec.`)
  log(`Fix: run /generate-tasks ${blockId} (and /breakdown) on main, commit, then re-run /sdlc-task ${blockId}.`)
  return { error: 'Missing spec', blockId, searchedRoot: [rootBlockRecordFile, rootSpecFile], searchedTier: tierPaths ? [`${tierPrefixCandidate}planning/blocks/${blockId}.json`, `${tierPrefixCandidate}planning/${blockId}/tasks.md`] : [] }
}

// D19 — thin-spec guard for a fresh run (legacy tasks.md path only — see STEP 4c above).
if (setupResult.specThin && !resumeMode) {
  log(`ABORTED (D19) — spec is structurally valid but substantively thin: ${setupResult.thinReason || '(no reason given)'}`)
  log(`Fix: flesh out ${specFile} (run /generate-tasks --force to regenerate, or edit + commit), then re-run.`)
  return { error: 'Thin spec (D19)', reason: setupResult.thinReason || '', blockId }
}

// Run-root path injection header — prepended to every agent prompt that does real work.
const W = `Run root = ${runDir}${useWorktree ? ' (an isolated WORKTREE, not the main repo)' : ' (the main repo, IN PLACE on branch ' + branchName + ')'}.
Shell state does NOT persist between Bash calls — START EVERY Bash call with: cd ${runDir} &&
Run all build/test/validation from the run root; relative paths (planning/...) resolve from there.
`

// ================================================================
// PHASE 1: PLAN — enumerate tasks (D16 lint) + load resume state
// ================================================================
phase('Plan')

const ENUMERATE_PROMPT = `${W}
You enumerate the tasks defined in a spec's tasks.json. Do NOT modify anything.

STEP 1 — read the task list:
  cd ${runDir} && cat ${tasksJsonFile} 2>/dev/null || echo "NO_TASKS_JSON"

STEP 2 — Parse it as JSON. It is a BARE ARRAY (not wrapped in an object — matches orchestrator's
  SDLCTask schema). Collect every task's "task_id" (in array order) into allTasks.
  Set hasTasks=true iff it parsed as an array with at least one entry.

STEP 3 — Per-task validation overrides. For each task whose "validation_commands" is present AND a
  non-empty array, add {taskId, validationCommands} to taskChecks. Skip every task whose
  "validation_commands" is absent, null, or [] — those fall back to the project-wide harness checks.
  Copy the command strings VERBATIM; do not normalize, reorder, or invent commands.

STEP 4 — Engine-parse gate scan. For each task, look at its "files" array. If ANY entry is a path
  under .claude/workflows/ (e.g. ".claude/workflows/sdlc-task.js"), add {taskId, files} to
  engineFiles, where files is ONLY the matching .claude/workflows/ path(s) from that task (never the
  task's other files). Skip every task whose "files" has no such path.

STEP 5 — Deliberate-failing-test overrides (D68). For each task whose "expect_red" is present AND a
  non-empty array, add {taskId, commands} to taskExpectRed. Skip every task whose "expect_red" is
  absent, null, or []. Copy the command strings VERBATIM. Do NOT validate the subset rule yourself —
  the engine enforces it after this call; just report exactly what tasks.json contains.

Return via StructuredOutput: hasTasks, allTasks (integers in order), taskChecks, taskExpectRed,
engineFiles, notes.
`

let enumResult = await tracedAgent(ENUMERATE_PROMPT, withModel({ label: 'enumerate', schema: ENUMERATE_SCHEMA, phase: 'Plan' }, MODEL.enumerate))

if (!enumResult || !enumResult.hasTasks || !(enumResult.allTasks || []).length) {
  if (specSource === 'block-record') {
    // D16 derive-from-block-record fallback — the D65 stage 2 counterpart of the
    // derive-from-tasks.md branch below, used when this run's spec source is the authored block
    // record rather than legacy tasks.md prose. Mirrors /generate-tasks' --from mode: read the
    // block record's what/why/files/acceptance_criteria/testing_strategy fields and author a
    // FRESH D45-shaped tasks.json from them (never a verbatim copy, never the superseded D44
    // {"tasks": [...]} wrapper). Deriving from an authored block record is not guessing the task
    // structure — D16 exists to refuse fabricating one out of nothing, which the abort below still does.
    const deriveFromRecordResult = await tracedAgent(`${W}
You are the D16 recovery generator for one lean-engine spec. ${tasksJsonFile} is missing, invalid, or
empty; ${blockRecordFile} (the authored block record, per block.schema.json) may still carry enough
to decompose into tasks. Do NOT implement anything.

STEP 1 — check for a derivable source:
  cd ${runDir} && cat ${blockRecordFile} 2>/dev/null || echo "NO_BLOCK_RECORD"

STEP 2 — Parse it as JSON per block.schema.json. If the block record is missing, invalid JSON, or
  lacks a non-empty "what" and a non-empty "acceptance_criteria" array to decompose, set
  derivable=false, written=false, and STOP — do not write anything.

STEP 3 — Otherwise, author a FRESH decomposed ${tasksJsonFile} from the block record's "what" (scope),
  "why" (intent), "files" (new/modified — use these to keep tasks disjoint), "acceptance_criteria",
  "testing_strategy", and "validation_commands" fields (mirrors /generate-tasks' --from mode: a real
  decomposition, not a verbatim copy of the record's prose). Write it as valid JSON: a BARE ARRAY (D45
  shape — NOT the superseded D44 {"tasks": [...]} wrapper), each entry shaped { task_id, title,
  description, acceptance_criteria, validation_commands, max_attempts, files, dependsOn } — task_id is
  a 1-indexed integer in dependency order with no gaps, description is a single string, max_attempts is
  3, and you must NEVER author a "status" or "attempt_count" key (those are engine-owned). Each task
  names the concrete file(s) it owns in "files" (drawn from the record's files.new / files.modified
  paths) so tasks stay disjoint.

  Per-task "validation_commands" scoping — follow the convention documented at
  \`.claude/commands/generate-tasks.md\` (search it for "validation_commands"); do not restate the
  rubric in your own words, just apply it: "validation_commands" is [] for any task that touches
  source the project's checks compile or lint — those tasks fall back to the project-wide harness
  checks, which are authoritative for them. Set it ONLY for a task that CANNOT break the build
  (docs-only, config-only, fixture-only), with cheap commands that actually verify that task (file
  exists, frontmatter present, index updated). If you DO author an override that runs tests, it MUST
  target that task's own tests specifically — never a bare/positional filter that could silently
  match zero or the wrong tests — and a command matching nothing must fail rather than pass. Never
  hardcode a stack-specific command (e.g. a particular test runner invocation) into this prompt;
  that judgment belongs to the deriving agent at run time, per task.

STEP 4 — Commit it on the current branch with an explicit pathspec:
  ${GIT} add ${tasksJsonFile}
  ${renderCommitSafetyGuard()} && ${GIT} commit -m "chore: derive tasks.json from block record (D16 fallback)"
  ${GIT} log --oneline -1   (capture the short hash)

Return via StructuredOutput: derivable, written, commitHash, taskCount, notes.
`, withModel({ label: 'derive-tasks-json-from-record', schema: DERIVE_SCHEMA, phase: 'Plan' }, MODEL.derive))

    if (deriveFromRecordResult?.derivable && deriveFromRecordResult?.written) {
      log(`Derived tasks.json from block record (D16 derive-from-block-record fallback) — ${deriveFromRecordResult.taskCount || '?'} task(s), commit ${deriveFromRecordResult.commitHash || 'unknown'}.`)
      enumResult = await tracedAgent(ENUMERATE_PROMPT, withModel({ label: 'enumerate-post-derive', schema: ENUMERATE_SCHEMA, phase: 'Plan' }, MODEL.enumerate))
    }
  } else {
  // D16 derive-from-tasks.md fallback — before refusing, check whether the spec's authored
  // tasks.md carries a derivable step decomposition. Mirrors /generate-tasks' --from mode: author a FRESH decomposition from tasks.md (never
  // a verbatim copy of its prose). Deriving from an authored tasks.md is not guessing the task
  // structure — D16 exists to refuse fabricating one out of nothing, which the abort below still does.
  const deriveResult = await tracedAgent(`${W}
You are the D16 recovery generator for one lean-engine spec. ${tasksJsonFile} is missing, invalid, or
empty; ${specFile} (tasks.md) may still carry a usable step decomposition. Do NOT implement anything.

STEP 1 — check for a derivable source:
  cd ${runDir} && cat ${specFile} 2>/dev/null || echo "NO_TASKS_MD"

STEP 2 — If tasks.md is missing, or has no "## Step-by-Step Tasks" / "## Step by Step Tasks"
  section with at least one numbered step, set derivable=false, written=false, and STOP — do not
  write anything.

STEP 3 — Otherwise, author a FRESH decomposed ${tasksJsonFile} from tasks.md's step list plus its
  Acceptance Criteria / Validation Commands sections (mirrors /generate-tasks' --from mode: a real
  decomposition, not a verbatim copy of the prose). Write it as valid JSON: a BARE ARRAY (D45 shape —
  NOT the superseded D44 {"tasks": [...]} wrapper), each entry shaped { task_id, title, description,
  acceptance_criteria, validation_commands, max_attempts, files, dependsOn } — task_id is a 1-indexed
  integer in dependency order with no gaps, description is a single string, max_attempts is 3, and
  you must NEVER author a "status" or "attempt_count" key (those are engine-owned). Each task names
  the concrete file(s) it owns in "files" so tasks stay disjoint.

  Per-task "validation_commands" scoping — follow the convention documented at
  \`.claude/commands/generate-tasks.md\` (search it for "validation_commands"); do not restate the
  rubric in your own words, just apply it: "validation_commands" is [] for any task that touches
  source the project's checks compile or lint — those tasks fall back to the project-wide harness
  checks, which are authoritative for them. Set it ONLY for a task that CANNOT break the build
  (docs-only, config-only, fixture-only), with cheap commands that actually verify that task (file
  exists, frontmatter present, index updated). If you DO author an override that runs tests, it MUST
  target that task's own tests specifically — never a bare/positional filter that could silently
  match zero or the wrong tests — and a command matching nothing must fail rather than pass. Never
  hardcode a stack-specific command (e.g. a particular test runner invocation) into this prompt;
  that judgment belongs to the deriving agent at run time, per task.

STEP 4 — Commit it on the current branch with an explicit pathspec:
  ${GIT} add ${tasksJsonFile}
  ${renderCommitSafetyGuard()} && ${GIT} commit -m "chore: derive tasks.json from tasks.md (D16 fallback)"
  ${GIT} log --oneline -1   (capture the short hash)

Return via StructuredOutput: derivable, written, commitHash, taskCount, notes.
`, withModel({ label: 'derive-tasks-json', schema: DERIVE_SCHEMA, phase: 'Plan' }, MODEL.derive))

  if (deriveResult?.derivable && deriveResult?.written) {
    log(`Derived tasks.json from tasks.md (D16 derive-from-tasks.md fallback) — ${deriveResult.taskCount || '?'} task(s), commit ${deriveResult.commitHash || 'unknown'}.`)
    enumResult = await tracedAgent(ENUMERATE_PROMPT, withModel({ label: 'enumerate-post-derive', schema: ENUMERATE_SCHEMA, phase: 'Plan' }, MODEL.enumerate))
  }
  }
}

if (!enumResult || !enumResult.hasTasks || !(enumResult.allTasks || []).length) {
  // D16 preflight lint — refuse to guess the task structure when nothing was derivable either.
  log(`ABORTED (D16) — ${tasksJsonFile} is missing, invalid, or is an empty array.`)
  log(`Fix: run /generate-tasks ${blockId} to author tasks.json (see the spec template), commit, then re-run.`)
  return { error: 'No tasks.json (D16)', blockId, specFile: tasksJsonFile }
}

const allTasks = enumResult.allTasks
const taskList = selectedTasks ? allTasks.filter(n => selectedTasks.has(n)) : allTasks.slice()
if (!taskList.length) {
  log(`No tasks match the selection "${rangeSpec}" against spec tasks [${allTasks.join(', ')}].`)
  return { error: 'Empty task selection', blockId, rangeSpec, allTasks }
}
state.tasks_run = taskList
log(`Tasks in spec: ${allTasks.join(', ')}${selectedTasks ? ` | selected: ${taskList.join(', ')}` : ''}`)

// Per-task validation overrides from tasks.json's `validation_commands` (see ENUMERATE_SCHEMA).
// null => use the harness gating checks, the pre-existing behaviour for every existing spec.
// D63 (planning/decisions/D63-per-task-validation-commands-augment-gating.md) — augment-gating-only:
// when present, this AUGMENTS the project's `gates:true` harness checks (fast form) rather than
// replacing them. See runTests()'s usingOverride branch below for the combined rendering.
const taskCheckMap = new Map(
  (enumResult.taskChecks || [])
    .filter(tc => tc && Number.isInteger(tc.taskId) && Array.isArray(tc.validationCommands) && tc.validationCommands.length)
    .map(tc => [tc.taskId, tc.validationCommands])
)
function taskCommandsFor(taskNum) { return taskCheckMap.get(taskNum) || null }
if (taskCheckMap.size) {
  log(`Per-task validation overrides (tasks.json validation_commands): ${[...taskCheckMap.keys()].sort((a, b) => a - b).join(', ')} — D63: these AUGMENT the project's gates:true harness checks (fast form) rather than replacing them; each task's own commands run in addition, never instead.`)
}

// expect_red (BT.ticket.sdlc-task-cannot-express-a-deliberate-failing-test, D68) — a task whose
// declared deliverable IS a test observed FAILING may invert the verdict of a NAMED SUBSET of its
// own validation_commands. Enforced here, in the engine, not only in the docs: every expect_red
// entry MUST also appear in that same task's own validationCommands (taskCheckMap above) — an entry
// that does not is a hard spec error, refused outright, never silently ignored or downgraded to a
// warning. BOUNDARY (D68): expect_red can never touch a project-wide gates:true harness check — it
// is scoped strictly to that task's own validation_commands, so it can never invert or suppress a
// harness check; gatingChecks() below computes the harness gating set and never consults this map.
//
// NOTE: this block sits ABOVE the manifest-pinned isolation-and-branch-naming /
// triage-bail-taxonomy / bookkeep-vault-commit anchor line ranges scripts/skill_sync_manifest.json
// and scripts/engine_docs_sync_manifest.json pin — inserting it here shifts every one of those
// ranges even though none of their described behavior changed. That is expected (see this ticket's
// task 4 description) and is fixed by re-reading the anchor content against the guide/docs, then
// `python3 scripts/check_skill_sync.py --update` and `python3 scripts/check_engine_docs_sync.py
// --update` — never by moving this block to dodge the shift.
const taskExpectRedMap = new Map()
for (const er of (enumResult.taskExpectRed || [])) {
  if (!er || !Number.isInteger(er.taskId) || !Array.isArray(er.commands) || !er.commands.length) continue
  const ownCommands = taskCheckMap.get(er.taskId) || []
  const invalidEntries = er.commands.filter(c => !ownCommands.includes(c))
  if (invalidEntries.length) {
    log(`ABORTED (spec error) — task ${er.taskId}'s expect_red names command(s) not present in its own validation_commands: ${JSON.stringify(invalidEntries)}. expect_red must be a subset of that task's own validation_commands (it can never invert a project-wide gates:true harness check).`)
    return { error: 'expect_red not a subset of validation_commands', blockId, taskId: er.taskId, invalidEntries }
  }
  taskExpectRedMap.set(er.taskId, new Set(er.commands))
}
// <<shared:expectRedFor>>
function expectRedFor(taskNum) { return taskExpectRedMap.get(taskNum) || new Set() }
if (taskExpectRedMap.size) {
  log(`Per-task expect_red overrides (inverted-verdict, D68): ${[...taskExpectRedMap.keys()].sort((a, b) => a - b).join(', ')} — each named command PASSES on a NON-ZERO exit and FAILS on exit 0; every other check on that task's list is judged normally.`)
}
// <</shared:expectRedFor>>

// D63 — shared validated: vocabulary (identical strings in sdlc-flow.js, per the ADR). A pass
// always lands on exactly one of these three; never a fourth ad hoc label.
const VALIDATED_LABEL = {
  ranHarnessList: 'ran the harness list',
  substitutedSubset: 'substituted a documented subset (gates:true checks still ran)',
  ranNoneOfHarnessList: 'ran none of the harness list (tasks.json override, /sdlc-flow end review will reconcile)',
}

// D63 — the set of harness.json checks that gate a per-task fast-tripwire pass (mirrors
// renderCheckList's own gatingOnly filter). Used to (a) number the combined check list when a task
// also declares its own validation_commands, and (b) detect the edge case where a project's
// harness.json defines no gates:true checks at all, so even the augmented list has nothing of the
// harness's own to run — a pre-existing harness-authoring gap (D56's domain), but one that must
// stay VISIBLE (case 3 below) rather than be silently folded into "substituted".
function gatingChecks(cfg) {
  return (cfg?.validation?.checks ?? []).filter(c => c.gates && c.perTask !== false)
}

// Hardcoded engine-parse gate (mechanism, not project policy — see renderCheckList). Per-task
// .claude/workflows/ paths from tasks.json's own "files" array, captured at enumerate-time so the
// gate is unconditional on harness.json and independent of whatever project checks apply.
const taskEngineFilesMap = new Map(
  (enumResult.engineFiles || [])
    .filter(ef => ef && Number.isInteger(ef.taskId) && Array.isArray(ef.files) && ef.files.length)
    .map(ef => [ef.taskId, ef.files])
)
function engineFilesFor(taskNum) { return taskEngineFilesMap.get(taskNum) || [] }
if (taskEngineFilesMap.size) {
  log(`Engine-parse gate (hardcoded, unconditional): task(s) touching .claude/workflows/ → ${[...taskEngineFilesMap.keys()].sort((a, b) => a - b).join(', ')}.`)
}

// Resume: load the committed state.json to skip already-passed tasks. Also seeds the in-memory
// `state.tasks` with the FULL prior tasks object — writeTaskState() serializes `state` wholesale on
// every write, and the per-task loop below only ever populates `state.tasks[N]` for tasks it actually
// runs (skipped/already-passed tasks never re-enter it) — so without this seed, the first write after
// a resume would silently drop the earlier-passed tasks from the committed file, and the *next*
// resume would see them as never-passed and re-run them.
const passedFromState = new Set()
if (resumeMode) {
  const loaded = await tracedAgent(`${W}
You read the COMMITTED run-state for an /sdlc-task resume. Do NOT modify anything.
  cd ${runDir} && cat ${stateFile} 2>/dev/null || echo "__NO_STATE__"
If "__NO_STATE__" or invalid JSON → exists=false, tasksJson="{}", bails=[]. Otherwise exists=true,
startedAt = its started_at, passedTasks = the task numbers whose tasks[N].status == "passed",
bailReason = its bail_reason or "", tasksJson = the exact JSON (as a string) of its top-level "tasks"
object, verbatim, bails = the exact contents (each entry unmodified) of its top-level "bails" array,
or [] when absent — this is how the engine carries the full prior task history AND every prior bail
record forward across a resume (BT.ticket.bails-must-be-append-only: never drop or rewrite an entry).
Return via StructuredOutput.
`, withModel({ label: 'state-load', schema: STATE_LOAD_SCHEMA, phase: 'Plan' }, MODEL.stateLoad))
  if (loaded && loaded.exists) {
    for (const n of (loaded.passedTasks || [])) passedFromState.add(n)
    log(`Resume: ${passedFromState.size} task(s) already passed (${[...passedFromState].sort((a, b) => a - b).join(', ') || 'none'}); skipping them.`)
    try {
      const priorTasks = JSON.parse(loaded.tasksJson || '{}')
      // APPEND-ONLY (BT.ticket.bails-must-be-append-only): the prior run's bails[] MUST be merged
      // forward, never re-initialised — that silent re-init is the exact defect this ticket fixes
      // (eight of nine measured foreign-state bails left no trace on disk because of it). Any entry
      // still open (resolution === null) that survives to a resume is, by definition, about to be
      // retried; annotate it resumed-clean now rather than leaving it open forever — if THIS run
      // bails again it gets its OWN fresh entry, so nothing is lost either way.
      const priorState = { bails: Array.isArray(loaded.bails) ? loaded.bails : [] }
      if (priorTasks && typeof priorTasks === 'object') Object.assign(state.tasks, priorTasks); const inheritedBails = (typeof priorState !== 'undefined' && Array.isArray(priorState.bails) ? priorState.bails : (priorTasks && Array.isArray(priorTasks.__pendingBails) ? priorTasks.__pendingBails : [])).map(b => (b && b.resolution === null) ? { ...b, resolution: 'resumed-clean' } : b); state.bails = inheritedBails.concat(state.bails); if (state.tasks) delete state.tasks.__pendingBails
    } catch {
      log('(resume) could not parse prior tasks JSON from state.json — already-passed tasks may drop out of the committed history on the next write.')
    }
  } else {
    log('Resume requested but no valid state.json found — running all selected tasks fresh.')
  }
}

// Load the project's validation policy once (from the run root). null → fall back to the spec.
let harnessCfg = await loadHarnessConfig(runDir)
if (harnessCfg && harnessCfg.__bail) {
  const diagnostic = harnessCfg.__bail
  log(`BAILED (${diagnostic}) — planning/harness.json is present but the harness-config stage could not resolve it into a usable config even after the defensive double-wrap unwrap. Refusing to run this block with an unknown gating set rather than silently falling back or running ungated.`)
  return { error: diagnostic, blockId }
}
log(harnessCfg
  ? `Harness config: ${(harnessCfg.validation?.checks || []).length} check(s).`
  : 'No planning/harness.json — validation falls back to the spec.')

// D63 — computed once; see gatingChecks() above. A project with zero gates:true checks means an
// overridden task's augmentation has nothing of the harness's own to add, which is the one case
// where /sdlc-task can still land on VALIDATED_LABEL.ranNoneOfHarnessList (see runTests below).
const harnessGatingCheckCount = gatingChecks(harnessCfg).length

// BT.ticket.harness-config-must-bail-not-warn-on-a-malformed-payload: a PRESENT (non-empty)
// planning/harness.json that resolves to ZERO gates:true checks is a hard BAIL, not a warning —
// this is the "reports success FASTER for having run nothing" defect (see the block record's
// `why`). Scoped strictly to the present case: `harnessCfg` is null when the file is absent (or
// present-but-invalid-JSON, per loadHarnessConfig above), and that case must keep falling back to
// the spec's `## Validation Commands`, never bail (D5 / standing rule 1).
if (harnessCfg && harnessGatingCheckCount === 0) {
  log(`BAILED (${HARNESS_CONFIG_BAIL.zeroGatingChecks}) — planning/harness.json is present and non-empty but resolves to ZERO gates:true checks; refusing to run this block with no project-wide gating rather than silently running with none (previously only a D63 warning).`)
  return { error: HARNESS_CONFIG_BAIL.zeroGatingChecks, blockId }
}

// Resolve test depth: CLI flag overrides harness.json overrides the built-in 'fast' default.
//
// The config block is named `flow` for historical reasons — it predates this engine reading any of
// it. `testDepth` and `bailReasons` are NOT flow-specific (both engines accept --test-depth, and a
// failure is retryable or fatal for the same reasons in either), so this engine reads those two
// keys out of the same block rather than inventing a second one. It deliberately does NOT read
// `autoMerge` or `prBase`, which ARE flow-only. The block is not renamed because six repos already
// set it on disk and one (jynx) carries real project-specific bailReasons there; a rename would
// silently drop them.
const flowCfg = harnessCfg?.flow || {}
const testDepth = testDepthFlag || (VALID_TEST_DEPTHS.includes(flowCfg.testDepth) ? flowCfg.testDepth : 'fast')
const extraBailReasons = Array.isArray(flowCfg.bailReasons) ? flowCfg.bailReasons : []
log(`Policy: testDepth=${testDepth}`)

// Snapshot baselines once (resume-safe; no-op without baseline-diff checks).
await snapshotBaselines(harnessCfg, runDir)

// The immediate-bail reason set the triage agent enforces. "When unsure, prefer bail."
// <<shared:BAIL_REASONS>>
const BAIL_REASONS = [
  'Missing/undefined upstream dependency or symbol the spec assumes exists.',
  'Spec ambiguity/contradiction — intended behavior is genuinely undeterminable.',
  'Environment/credential/auth/network failure (not a code defect).',
  'Change would require a destructive or out-of-scope action.',
  'Same failure twice with no progress (stuck), or a structural design flaw needing a re-plan.',
  ...extraBailReasons,
].map((r, i) => `  ${i + 1}. ${r}`).join('\n')
// <</shared:BAIL_REASONS>>

// <<shared:RENDER_IDENTITY_SCHEMA>>
const RENDER_IDENTITY_SCHEMA = {
  type: 'object',
  required: ['value'],
  properties: {
    value: { type: 'string', description: 'the text after "VALUE:" on the probe script\'s stdout, or "" if that line is missing or the script produced no output' }
  }
}
// <</shared:RENDER_IDENTITY_SCHEMA>>

// ----------------------------------------------------------------
// Test stage helper — gatingOnly=true → fast tripwire (gating checks); false → full suite.
// ----------------------------------------------------------------
// Render a per-task validation override (tasks.json `validation_commands`) in the same shape
// renderCheckList emits, so the test agent's instructions are identical either way. `startIndex`
// (D63) lets this continue the numbering after the harness gating checks it now augments, rather
// than restarting at CHECK 1 and colliding with them.
function renderTaskCheckList(commands, cwd, startIndex = 1, expectRedSet = new Set()) {
  const cd = cwd ? `cd ${cwd} && ` : ''
  return commands.map((cmd, i) => {
    const n = startIndex + i
    if (expectRedSet.has(cmd)) {
      // expect_red (D68) — this task's declared DELIVERABLE is a test observed FAILING, so this
      // ONE named command's verdict is inverted from every other check on this list: it PASSES on
      // a NON-ZERO exit and FAILS on exit 0. The CHECK${n}_EXIT convention is unchanged so the
      // test agent's parsing stays identical to every other check — only the pass/fail JUDGMENT
      // of that same exit code is reversed for this command.
      return `CHECK ${n} — task_validation_${i + 1} (per-task validation_commands override from tasks.json — additive, per D63) [GATING — a failure here blocks the verdict] — EXPECT_RED (D68, INVERTED VERDICT — do not read this as an ordinary check):
  ${cd}${cmd}
  echo "CHECK${n}_EXIT:$?"
  This check's verdict is INVERTED: it PASSES on a NON-ZERO exit and FAILS on exit 0 — the exact
  opposite of every other check on this list. Judge CHECK${n} ONLY by that inverted rule:
  CHECK${n}_EXIT != 0 → PASS; CHECK${n}_EXIT == 0 → FAIL. This task's own deliverable is a test
  that must be observed failing (D68); a zero exit here means the deliverable is missing, not that
  the task succeeded.`
    }
    return `CHECK ${n} — task_validation_${i + 1} (per-task validation_commands override from tasks.json — additive, per D63) [GATING — a failure here blocks the verdict]:
  ${cd}${cmd}
  echo "CHECK${n}_EXIT:$?"`
  }).join('\n\n')
}

// Renders the "if allPassed, ALSO perform this exact state write, in this same turn" instruction
// block for a passing test agent — mirrors sdlc-flow.js's renderOnPassStateWriteRecipe, but this
// engine has no worklog.md (state.json only). `onPass` is { stateFile, stateJson } — fully
// computable in JS before the test call is made, from the prior implement/fix stage's result.
function renderOnPassStateWriteRecipe(onPass) {
  return `
IF AND ONLY IF allPassed is true above, ALSO perform this state write as part of THIS SAME turn —
do NOT do this if any check failed (leave stateWritten unset/false in that case):

STEP W1 — run this as ONE Bash call, exactly as written. Do not split it into several calls:
  cd ${runDir} && mkdir -p ${blockDir}/sdlc && date -u +%Y-%m-%dT%H:%M:%SZ && { cat ${onPass.stateFile} 2>/dev/null || echo "__NO_STATE__"; }
  The FIRST line of output is NOW. Everything after it is the existing state file, or __NO_STATE__
  when there is none. If that file exists and has a "started_at" value, REUSE it verbatim for
  started_at below. Otherwise started_at = NOW.

STEP W2 — write ${onPass.stateFile} with EXACTLY this JSON, but inserting two extra top-level keys
  "started_at" (preserved or NOW, per STEP W1) and "updated_at" (NOW) right after "branch". Valid
  JSON only (double quotes, no trailing commas, no markdown fences). The object to write (verbatim
  except for adding those two timestamp keys):
${onPass.stateJson}

STEP W3 — use the Write tool for the file. Do NOT run \`git add\`, \`git commit\`, \`git checkout\`,
  \`git switch\`, or \`git branch\` — this write is disk-only, exactly like writeTaskState(). Set
  stateWritten=true in your StructuredOutput once the file is written to disk; leave it false/unset
  if you skipped this because a check failed.
`
}

// Renders the "if this triage call is terminal, ALSO perform this exact state write, in this same
// turn" instruction block for the triage agent — mirrors sdlc-flow.js's renderBailStateWriteRecipe,
// state.json only (no worklog.md in this engine). `onBail` is
// { stateFile, stateJson, majorFallback, exhaustionFallback } — exhaustionFallback is null at call
// sites that have no attempt-exhaustion bail path (mirrors the asymmetry between the NULL_RESULT
// and test-failure call sites in the per-task loop below).
function renderBailStateWriteRecipe(onBail, attempt, maxAttempts) {
  const esc = s => String(s).replace(/"/g, '\\"')
  return `
IF AND ONLY IF your class above is MAJOR${onBail.exhaustionFallback ? `, OR this is the final attempt (attempt ${attempt} of ${maxAttempts})` : ''}, ALSO perform this state
write as part of THIS SAME turn — do NOT do this ${onBail.exhaustionFallback ? `if class is RETRYABLE and this is NOT the final attempt` : `unless class is MAJOR`} (leave stateWritten unset/false in that case):

First compute the effective bail reason (used in STEP W2 below):
  - If your class is MAJOR: use your own bailReason field if you set a non-empty value; otherwise
    your own reason field if non-empty; otherwise this exact fallback text: "${esc(onBail.majorFallback)}"
${onBail.exhaustionFallback ? `  - If your class is RETRYABLE but this IS the final attempt (attempt ${attempt} of ${maxAttempts}):
    IGNORE your own bailReason/reason and use this EXACT fallback text instead: "${esc(onBail.exhaustionFallback)}"` : ''}

STEP W1 — run this as ONE Bash call, exactly as written. Do not split it into several calls:
  cd ${runDir} && mkdir -p ${blockDir}/sdlc && date -u +%Y-%m-%dT%H:%M:%SZ && { cat ${onBail.stateFile} 2>/dev/null || echo "__NO_STATE__"; }
  The FIRST line of output is NOW. Everything after it is the existing state file, or __NO_STATE__
  when there is none. If that file exists and has a "started_at" value, REUSE it verbatim for
  started_at below. Otherwise started_at = NOW.

STEP W2 — write ${onBail.stateFile} with EXACTLY this JSON, but: (a) inserting two extra top-level
  keys "started_at" (preserved or NOW, per STEP W1) and "updated_at" (NOW) right after "branch",
  (b) replacing the literal placeholder string "__BAIL_REASON__" (the top-level "bail_reason" field
  AND the "reason" field inside the new bails[] entry — both occurrences) with the effective bail
  reason computed above, and (c) replacing the literal placeholder string "__BAIL_OCCURRED_AT__"
  (the "occurred_at" field inside that same new bails[] entry) with NOW, the exact value you read
  as the first line of STEP W1's output — do this substitution in this SAME turn, alongside (b).
  Valid JSON only (double quotes, no trailing commas, no markdown fences). The object to write
  (verbatim except for those substitutions):
${onBail.stateJson}

STEP W3 — use the Write tool for the file. Do NOT run \`git add\`, \`git commit\`, \`git checkout\`,
  \`git switch\`, or \`git branch\` — this write is disk-only, exactly like writeTaskState(). Set
  stateWritten=true in your StructuredOutput once the file is written to disk; leave it false/unset
  if you skipped this because the outcome was not terminal.
`
}

// Precompute the exact state.json content for the case where task `taskNum` PASSES on this
// attempt — content that is fully known from the implement/fix stage's result (t.summary,
// t.commit, t.files_changed, t.decisions) BEFORE the test call is even made; the test call only
// determines whether this precomputed content actually gets used. Handed to runTests() as `onPass`
// so a passing test agent can write it in its own turn instead of a follow-up dedicated
// state-writer agent. Does NOT mutate the live `state`/`t` objects — this is a snapshot for the
// CANDIDATE outcome.
function buildPassPayload(taskNum, t, validatedLabel) {
  const snapshot = JSON.parse(JSON.stringify(state))
  snapshot.tasks[String(taskNum)] = { ...t, status: 'passed', validated: validatedLabel }
  snapshot.tokens = buildTokensBlock()
  return { stateFile, stateJson: JSON.stringify(snapshot, null, 2) }
}

// Precompute the exact state.json content for the case where THIS triage call turns out to be
// terminal (class=MAJOR, or — only at call sites that pass exhaustionFallback — this is the final
// allowed attempt) — content that is fully known BEFORE the triage call is made, except the
// effective bail reason, which the triage agent itself computes as part of classifying (see
// renderBailStateWriteRecipe). Handed to triage() as `onBail` so a terminal triage call can write
// it in its own turn instead of a follow-up dedicated state-writer agent. Does NOT mutate the live
// `state`/`t` objects — this is a snapshot for the CANDIDATE outcome.
function buildBailPayload(taskNum, t, majorFallback, exhaustionFallback = null) {
  const snapshot = JSON.parse(JSON.stringify(state))
  snapshot.tasks[String(taskNum)] = { ...t, status: 'failed' }
  snapshot.status = 'blocked'
  snapshot.bail_reason = '__BAIL_REASON__'
  // APPEND-ONLY (BT.ticket.bails-must-be-append-only) — one entry per bail, never overwritten.
  // `reason` carries the SAME "__BAIL_REASON__" placeholder as `bail_reason` above (b) below), so
  // the one substitution the writing agent performs for the reason keeps both in sync.
  // `occurred_at` carries the sibling "__BAIL_OCCURRED_AT__" placeholder (BT.ticket.bails-must-
  // not-mint-time-in-the-engine) — a JS-side clock call is illegal under the Workflow runtime
  // shim, so the writing agent substitutes NOW (already obtained via STEP W1's `date -u` call)
  // for this sentinel in the SAME turn it substitutes __BAIL_REASON__; see (c) in
  // renderBailStateWriteRecipe's STEP W2 below. check_id best-effort from
  // the task's own recorded issues (the harness check name already on `t`, never reimplemented);
  // failing_artifact/ownership/bail_class stay null here — not yet derivable at this call site
  // (see out_of_scope: checks-must-name-their-failing-artifact is separate work).
  snapshot.bails = [...(snapshot.bails || []), {
    occurred_at: '__BAIL_OCCURRED_AT__',
    task_id: taskNum,
    check_id: (t.issues && t.issues.length) ? t.issues[t.issues.length - 1] : null,
    failing_artifact: null,
    ownership: null,
    bail_class: null,
    reason: '__BAIL_REASON__',
    resolution: null,
  }]
  snapshot.tokens = buildTokensBlock()
  return { stateFile, stateJson: JSON.stringify(snapshot, null, 2), majorFallback, exhaustionFallback }
}

async function runTests(label, { gatingOnly, taskCommands = null, expectRedSet = new Set(), onPass = null, engineFiles = [] }) {
  const usingOverride = Array.isArray(taskCommands) && taskCommands.length > 0
  const cd = runDir ? `cd ${runDir} && ` : ''

  // Diff-window concurrent-sessions fix: the emoji gate scopes to the commit SHAs THIS run itself
  // recorded in the run-state (state.tasks[N].commit — the in-memory object writeTaskState()
  // persists to disk at stateFile), never to the whole baseSha..HEAD range. On an in-place
  // (--no-worktree) run the branch is shared, so baseSha..HEAD can legitimately contain a sibling
  // session's concurrent commits, indistinguishable from this run's own. Reading state.tasks
  // in-memory (rather than re-reading stateFile off disk) is deliberate: disk writes only happen
  // after a task fully passes (see writeTaskState call sites), so by the time THIS task's own gate
  // runs, its own just-made commit would not yet be on disk — only the in-memory object already
  // reflects it at prompt-build time, right after `t.commit = stageResult.commit` in the caller.
  const recordedCommits = Object.values(state.tasks).map(x => x.commit).filter(Boolean)
  const recordedCommitsJson = JSON.stringify(recordedCommits)

  // D63 — augment-gating-only: a per-task validation_commands override NEVER causes a gates:true
  // harness check to be skipped. When present, the harness gating checks render FIRST (fast form,
  // gatingOnly:true, regardless of --test-depth — the cheap form is what augments, per the ADR),
  // numbered 1..N, followed by the task's own override commands continuing the numbering, then any
  // hardcoded engine-parse checks. When the project defines zero gates:true checks, the harness
  // portion is simply empty — there is nothing to augment with (see harnessGatingCheckCount above).
  let checklistBody
  let overrideNote
  if (usingOverride) {
    const harnessPart = harnessGatingCheckCount > 0
      ? renderCheckList(harnessCfg, { gatingOnly: true, cwd: runDir, engineFiles: [] })
      : ''
    const taskPart = renderTaskCheckList(taskCommands, runDir, harnessGatingCheckCount + 1, expectRedSet)
    const engineChecks = renderEngineParseChecks(engineFiles, cd, harnessGatingCheckCount + taskCommands.length + 1)
    checklistBody = [harnessPart, taskPart, engineChecks].filter(Boolean).join('\n\n')
    overrideNote = harnessGatingCheckCount > 0
      ? 'this task ALSO declares its OWN validation_commands in tasks.json — per D63 these AUGMENT the project gates:true harness checks below, they do NOT replace them'
      : 'this task declares its OWN validation_commands in tasks.json; this project configures zero gates:true harness checks, so only the task-declared commands below run (D63 — reported, not silent)'
  } else {
    checklistBody = renderCheckList(harnessCfg, { gatingOnly, cwd: runDir, engineFiles })
    overrideNote = 'from planning/harness.json + the spec'
  }

  const heartbeatRecipe = await renderLaneHeartbeatRecipe({ runRoot: runDir, blockId })

  return tracedAgent(`${W}
${renderTestPrompt({ enginePhrase: 'lean /sdlc-task', overrideNote, runRootLabel: 'run root', runRoot: runDir, checklistBody, diffBase: baseSha, stateFile, recordedCommitsJson, emojiScopeNote: "sibling session's commit on a shared in-place branch can fail a diff this run never touched:", onPassRecipe: onPass ? renderOnPassStateWriteRecipe(onPass) : '', stateWrittenNote: onPass ? ', stateWritten (true only if you performed the additional state write above)' : '', heartbeatRecipe })}
`, withModel({ label, schema: TEST_SCHEMA, phase: 'Tasks' }, MODEL.test))
}

// ----------------------------------------------------------------
// Triage helper — classify a failure RETRYABLE vs MAJOR.
// ----------------------------------------------------------------
async function triage(context, attempt, maxAttempts, failBlob, sameContext, onBail = null) {
  return tracedAgent(`
${renderTriagePrompt({ engineName: '/sdlc-task', context, attempt, maxAttempts, failBlob, bailReasons: BAIL_REASONS, onBail, sameContext, bailRecipe: onBail ? renderBailStateWriteRecipe(onBail, attempt, maxAttempts) : '' })}
`, withModel({ label: `triage:${context}:${attempt}`, schema: TRIAGE_SCHEMA, phase: 'Tasks' }, MODEL.triage))
}

// ================================================================
// PHASE 2: PER-TASK LOOP (sequential)
// ================================================================
phase('Tasks')

// D46 + vault-aware task commits: resolve ONCE for the whole run and reuse everywhere below (the
// per-task commit step and the bookkeep close-out) — never re-detect per task/stage, and never a
// second detection idiom.
const vault = await detectPlanningVault(runDir)

let bailed = false
let bailReason = null

for (const taskNum of taskList) {
  if (passedFromState.has(taskNum)) {
    log(`Task ${taskNum}: already passed (resume) — skipping.`)
    continue
  }
  state.current_task = taskNum
  const stem = `${blockId}-task${taskNum}`
  state.tasks[String(taskNum)] = state.tasks[String(taskNum)] || { status: 'running', attempts: 0, summary: '', issues: [], fixes: [], decisions: [], files_changed: [], commit: '', validated: '' }
  const t = state.tasks[String(taskNum)]

  let taskPassed = false
  let prevFailBlob = null
  let taskStateWritten = false

  for (let attempt = 1; attempt <= MAX_TASK_ATTEMPTS && !bailed; attempt++) {
    t.attempts = attempt
    const isFix = attempt > 1
    const fixModel = (ESCALATION_MODEL && attempt === MAX_TASK_ATTEMPTS) ? ESCALATION_MODEL : MODEL.fix
    if (isFix && fixModel !== MODEL.fix) log(`Task ${taskNum}: final fix pass — escalating model to ${fixModel}.`)
    log(`Task ${taskNum}: ${isFix ? `fix pass ${attempt - 1}` : 'implement'} (attempt ${attempt}/${MAX_TASK_ATTEMPTS})...`)

    // Implement (attempt 1) or targeted Fix (attempt > 1).
    const roleIntro = `You are the ${isFix ? 'fix' : 'implementation'} agent for the lean /sdlc-task pipeline. You run IN PLACE on
    the branch (sequential — earlier tasks in this spec are already committed on this branch). Work ONLY on
    Task ${taskNum} of this spec.`
    const stageResult = await tracedAgent(`${W}
${renderImplementPrompt({ roleIntro, runRootLabel: 'run root', runRoot: runDir, extraReturnFields: '', isFix, taskNum, attempt, stem, blockId, specFile, specDesc, tasksJsonFile, breakdownFile, prevFailBlob, vault, GIT, renderCommitSafetyGuard, renderWorkAssertion })}
`, withModel({ label: `${isFix ? 'fix' : 'implement'}-${taskNum}-${attempt}`, schema: STAGE_SCHEMA, phase: 'Tasks' }, isFix ? fixModel : MODEL.implement))
    recordFilesRead(stageResult)

    if (!stageResult) {
      log(`Task ${taskNum} attempt ${attempt}: agent returned null.`)
      // No attempt-exhaustion bail path exists at this call site today (an exhausted NULL_RESULT
      // loop just falls out of the `for` naturally without ever setting `bailed`), so
      // exhaustionFallback is omitted: the folded write only fires when this call classifies MAJOR.
      const nullBailPayload = buildBailPayload(taskNum, t, 'agent returned null')
      const tr = await triage(`task ${taskNum} implement`, attempt, MAX_TASK_ATTEMPTS, 'NULL_RESULT — the agent died or returned nothing.', prevFailBlob, nullBailPayload)
      if (tr && tr.class === 'MAJOR') {
        bailed = true
        bailReason = tr.bailReason || tr.reason || 'agent returned null'
        if (tr.stateWritten) taskStateWritten = true
        break
      }
      continue
    }
    // D-fix (BT.ticket.emoji-gate-diff-window-concurrent-sessions): the stage schema's field is
    // `commitHash`, never `commit` — reading `.commit` here silently left t.commit unset on EVERY
    // run in this fleet's history. Harmless while it only fed the state file's index; load-bearing
    // now that the emoji gate scopes to these SHAs, where an empty set trips the cannot-scope abort.
    // A stage occasionally returns a quoted empty string (observed live: commitHash === '""'), so
    // require something that actually looks like a short hash rather than merely truthy.
    const rawCommit = (stageResult.commitHash || '').replace(/["']/g, '').trim()
    if (/^[0-9a-f]{7,40}$/i.test(rawCommit)) t.commit = rawCommit
    if (stageResult.summary) t.summary = stageResult.summary
    if (Array.isArray(stageResult.filesModified)) t.files_changed = [...new Set([...(t.files_changed || []), ...stageResult.filesModified])]
    if (Array.isArray(stageResult.decisions) && stageResult.decisions.length) t.decisions = [...(t.decisions || []), ...stageResult.decisions]

    // Work-assertion evidence gate (BT.ticket.engine-terminal-state-needs-evidence, task 3, Gap 1).
    // renderWorkAssertion's files[]-vs-diff check (BT.ticket.a-run-must-prove-its-commits-contain-
    // the-work) previously lived only as prose in step 7a — an agent that skipped or misreported it
    // still yielded a task the engine could record as done. workAssertionPassed is that check's
    // outcome as a structured field; absent/false is treated as a failed assertion, never a silent
    // pass, exactly like the vault-commit check below.
    t.workAssertionPassed = stageResult.workAssertionPassed === true
    if (!t.workAssertionPassed) {
      log(`Task ${taskNum} attempt ${attempt}: work assertion not confirmed (workAssertionPassed=${stageResult.workAssertionPassed === false ? 'false' : 'absent'}) — refusing to record this task done/passed without that evidence.`)
      const waFailBlob = `WORK_ASSERTION_NOT_CONFIRMED — step 7a's renderWorkAssertion outcome was ${stageResult.workAssertionPassed === false ? 'reported false (WORK_ASSERTION_ABORT fired)' : 'not reported at all'} for task ${taskNum}. The terminal write recipe refuses done/passed without a positive workAssertionPassed field.`
      t.issues = [...(t.issues || []), 'work assertion not confirmed']
      const waBailPayload = buildBailPayload(taskNum, t, `Task ${taskNum}: work assertion not confirmed`)
      const tr = await triage(`task ${taskNum} work-assertion`, attempt, MAX_TASK_ATTEMPTS, waFailBlob, prevFailBlob, waBailPayload)
      prevFailBlob = waFailBlob
      if (tr && tr.class === 'MAJOR') {
        bailed = true
        bailReason = tr.bailReason || tr.reason || waFailBlob
        if (tr.stateWritten) taskStateWritten = true
        log(`Task ${taskNum}: triage → MAJOR on unconfirmed work assertion — bailing immediately.`)
        break
      }
      if (attempt === MAX_TASK_ATTEMPTS) {
        bailed = true
        bailReason = `Task ${taskNum} still lacking a positive workAssertionPassed outcome after ${MAX_TASK_ATTEMPTS} attempts.`
        if (tr && tr.stateWritten) taskStateWritten = true
        log(`Task ${taskNum}: exhausted ${MAX_TASK_ATTEMPTS} attempts without a confirmed work assertion — bailing.`)
        break
      }
      if (tr) t.fixes = [...(t.fixes || []), tr.reason]
      log(`Task ${taskNum}: triage → RETRYABLE on unconfirmed work assertion — fix pass ${attempt}/${MAX_TASK_ATTEMPTS - 1}. ${tr?.reason || ''}`)
      continue
    }

    // Vault-commit verification — independent of the stage's self-report. A non-empty commitHash
    // proves nothing about the vault half (observed live: one run's commitHash was valid and covered
    // only the source half, with the vault edit silently uncommitted — see this ticket's amendment
    // log). So this ALWAYS re-derives the vault-relevant subset from filesModified and re-checks it
    // directly, rather than trusting anything the stage reported. A failure here surfaces exactly
    // like a test failure: the task is never marked passed on this attempt.
    const vaultRelPaths = vaultRelPathsFrom(stageResult.filesModified, vault)
    if (vaultRelPaths.length) {
      const vaultVerify = await verifyVaultCommit(runDir, vault, vaultRelPaths)
      if (!vaultVerify.allCommitted) {
        const uncommitted = (vaultVerify.uncommittedPaths && vaultVerify.uncommittedPaths.length) ? vaultVerify.uncommittedPaths : vaultRelPaths
        log(`Task ${taskNum} attempt ${attempt}: vault commit incomplete — not committed in ${vault.planningPath}: ${uncommitted.join(', ')}.`)
        const vaultFailBlob = `VAULT_COMMIT_INCOMPLETE — planning/ path(s) not committed in the vault repo (${vault.planningPath}): ${uncommitted.join(', ')}. ${vaultVerify.notes || ''}`.trim()
        t.issues = [...(t.issues || []), 'vault commit incomplete']
        const vaultBailPayload = buildBailPayload(taskNum, t, `Task ${taskNum}: vault commit incomplete — ${uncommitted.join(', ')}`)
        const tr = await triage(`task ${taskNum} vault-commit`, attempt, MAX_TASK_ATTEMPTS, vaultFailBlob, prevFailBlob, vaultBailPayload)
        prevFailBlob = vaultFailBlob
        if (tr && tr.class === 'MAJOR') {
          bailed = true
          bailReason = tr.bailReason || tr.reason || vaultFailBlob
          if (tr.stateWritten) taskStateWritten = true
          log(`Task ${taskNum}: triage → MAJOR on vault-commit failure — bailing immediately.`)
          break
        }
        if (attempt === MAX_TASK_ATTEMPTS) {
          bailed = true
          bailReason = `Task ${taskNum} still failing to commit vault paths after ${MAX_TASK_ATTEMPTS} attempts: ${uncommitted.join(', ')}`
          if (tr && tr.stateWritten) taskStateWritten = true
          log(`Task ${taskNum}: exhausted ${MAX_TASK_ATTEMPTS} attempts on a vault-commit failure — bailing.`)
          break
        }
        if (tr) t.fixes = [...(t.fixes || []), tr.reason]
        log(`Task ${taskNum}: triage → RETRYABLE on vault-commit failure — fix pass ${attempt}/${MAX_TASK_ATTEMPTS - 1}. ${tr?.reason || ''}`)
        continue
      }
    }

    // Fast test (tripwire) — gating checks only unless testDepth=full. A task declaring its own
    // `validation_commands` in tasks.json AUGMENTS those gating checks rather than replacing them
    // (D63). passValidatedLabel is always one of the shared VALIDATED_LABEL trichotomy.
    const hasOverride = !!taskCommandsFor(taskNum)
    const passValidatedLabel = !hasOverride
      ? VALIDATED_LABEL.ranHarnessList
      : (harnessGatingCheckCount > 0 ? VALIDATED_LABEL.substitutedSubset : VALIDATED_LABEL.ranNoneOfHarnessList)
    const passPayload = buildPassPayload(taskNum, t, passValidatedLabel)
    const testResult = await runTests(`test-${taskNum}-${attempt}`, { gatingOnly: testDepth === 'fast', taskCommands: taskCommandsFor(taskNum), expectRedSet: expectRedFor(taskNum), onPass: passPayload, engineFiles: engineFilesFor(taskNum) })
    if (testResult && testResult.allPassed) {
      t.validated = passValidatedLabel
      // D63 — a task that ran ZERO harness.json gating checks must be VISIBLE in terminal output,
      // never only recorded in state. This is the one case /sdlc-task can still reach it (the
      // project defines no gates:true checks to augment with); the ordinary override case always
      // lands on "substituted" because the harness gating checks ran alongside it.
      log(`Task ${taskNum}: validated → "${passValidatedLabel}".${passValidatedLabel === VALIDATED_LABEL.ranNoneOfHarnessList ? ' WARNING: this task ran ZERO planning/harness.json gates:true checks.' : ''}`)
      taskPassed = true
      if (testResult.stateWritten) {
        // The folded write went straight to disk (no STATE_WRITE_SCHEMA result to read startedAt
        // back from), so cachedStartedAt is deliberately left as-is: the next dedicated
        // writeTaskState call (a later task, or this task's own reliability-net fallback, or the
        // final run-state write) will just re-`cat` the file it wrote — which still correctly
        // preserves started_at, just without the caching shortcut.
        taskStateWritten = true
      }
      break
    }

    // Failure → triage.
    const failBlob = (testResult && testResult.failBlob) || `Test stage failed or returned null (failCount=${testResult?.failCount ?? '?'}, failed=${(testResult?.failedTests || []).join(', ')}).`
    t.issues = [...(t.issues || []), ...((testResult?.failedTests) || [])]
    // This call site DOES have an attempt-exhaustion bail path (below), with its own fallback text
    // that ignores the triage agent's own bailReason/reason entirely — pass both fallbacks through
    // so the folded write mirrors whichever terminal path actually fires, exactly.
    const majorFallback = `Task ${taskNum}: ${(testResult?.failedTests || []).join(', ')}`
    const exhaustionFallback = attempt === MAX_TASK_ATTEMPTS
      ? `Task ${taskNum} still failing after ${MAX_TASK_ATTEMPTS} attempts: ${(testResult?.failedTests || []).join(', ')}`
      : null
    const testBailPayload = buildBailPayload(taskNum, t, majorFallback, exhaustionFallback)
    const tr = await triage(`task ${taskNum} test`, attempt, MAX_TASK_ATTEMPTS, failBlob, prevFailBlob, testBailPayload)
    prevFailBlob = failBlob
    if (tr && tr.class === 'MAJOR') {
      bailed = true
      bailReason = tr.bailReason || tr.reason || majorFallback
      if (tr.stateWritten) taskStateWritten = true
      log(`Task ${taskNum}: triage → MAJOR — bailing immediately (not burning the remaining attempts). Reason: ${bailReason}`)
      break
    }
    if (attempt === MAX_TASK_ATTEMPTS) {
      bailed = true
      bailReason = exhaustionFallback
      if (tr && tr.stateWritten) taskStateWritten = true
      log(`Task ${taskNum}: exhausted ${MAX_TASK_ATTEMPTS} attempts — bailing.`)
      break
    }
    if (tr) t.fixes = [...(t.fixes || []), tr.reason]
    log(`Task ${taskNum}: triage → RETRYABLE — fix pass ${attempt}/${MAX_TASK_ATTEMPTS - 1}. ${tr?.reason || ''}`)
  }

  // One state write per task — disk-only, never committed (see writeTaskState).
  t.status = taskPassed ? 'passed' : 'failed'
  // APPEND-ONLY (BT.ticket.bails-must-be-append-only): record this terminal bail on `state.bails`
  // itself (never overwritten). `state.tasks.__pendingBails` is a resume-safety carrier: it rides
  // along inside `state.tasks` (which already survives every resume path this engine has), so the
  // resume merge below can recover this run's bail even on the rare path where the dedicated
  // `bails` read comes back empty; the merge deletes it the moment it is consumed, so it is
  // transient scaffolding, never load-bearing on its own.
  if (bailed && !taskPassed) { state.status = 'blocked'; state.bail_reason = bailReason }; if (bailed && !taskPassed) { const bailEntry = { occurred_at: '__BAIL_OCCURRED_AT__', task_id: state.current_task, check_id: null, failing_artifact: null, ownership: null, bail_class: null, reason: bailReason, resolution: null }; state.bails = [...state.bails, bailEntry]; state.tasks.__pendingBails = [...(state.tasks.__pendingBails || []), bailEntry] }
  // Reliability net: either the pass-path fold (runTests' onPass) or the terminal-bail fold
  // (triage's onBail) already wrote sdlc-task-state.json in the SAME turn as the resolving
  // test/triage call when taskStateWritten is true — skip the dedicated writer in that case.
  // taskStateWritten is only ever set true alongside taskPassed or bailed (never both), so
  // checking it alone is sufficient. Any other outcome (stateWritten false/unset, testResult/triage
  // null) falls through to the dedicated call so no task outcome is ever left unpersisted.
  if (!taskStateWritten) {
    await writeTaskState(`task ${taskNum} ${t.status}`, { cwd: runDir })
  } else {
    log(`Task ${taskNum}: state write folded into the ${taskPassed ? 'passing test' : 'terminal triage'} agent's own turn — skipped the dedicated state-writer call.`)
  }

  if (bailed) break
}

// ================================================================
// FINAL STATE COMMIT + SUMMARY
// ================================================================
const passedTasks = taskList.filter(n => state.tasks[String(n)]?.status === 'passed' || passedFromState.has(n))
const fullRun = !selectedTasks   // no explicit selection = every task in the spec ran; still used to gate
                                  // the once-per-full-run reconcile step below (D56) — unaffected by this ticket.

// BT.ticket.resume-cannot-close-its-block: the close decision is derived from every task in the
// SPEC (allTasks), never from taskList (which is the SELECTED subset when a range/task was passed —
// see :1212). Comparing against taskList made `passedTasks.length === taskList.length` trivially
// true on a subset run, so `fullRun` (a proxy for "was a selection passed?") was the only thing
// preventing a wrong close; comparing against allTasks makes the condition honest on its own and a
// "was a selection passed?" proxy is no longer needed for it. This is safe now — and was NOT safe
// before — because BT.ticket.sdlc-task-resume-truncates-run-state (closed 2026-08-20) made
// state.tasks survive a --resume, so passedAll no longer leans on passedFromState (the git-derived
// scout) as anything but the augment D37 says it must always be, not a load-bearing replacement.
const passedAll = allTasks.filter(n => state.tasks[String(n)]?.status === 'passed' || passedFromState.has(n))
const outstandingTasks = allTasks.filter(n => !passedAll.includes(n))

// sdlc-flow.js audited 2026-08-20: it has NO fullRun/blockDone-shaped proxy to fix. It never
// computes a single close boolean in code — the bookkeep/PR agent prompt (sdlc-flow.js:~2335) is
// handed `selectedTasks` and `taskList` directly and told in prose to judge "if tasks remain, keep
// status In progress ...; if this was the last, flip to Done", i.e. the "was every task passed"
// judgment already happens per-run rather than being gated by a proxy for "was a selection passed".
// So the defect this ticket fixes does not reproduce there, and sdlc-flow.js is left unchanged.

// ----------------------------------------------------------------
// PHASE 2.5: TERMINAL AUTHORITATIVE RECONCILE (D56) — after every task passes, before bookkeep.
//
// The per-task tripwire above always ran with `gatingOnly: testDepth === 'fast'`. Under that
// gating, renderCheckList() (a) runs a check's `fastCommand` instead of its authoritative
// `command` whenever one is configured, and (b) drops every `perTask: false` check from the
// per-task list entirely (see renderCheckList's `gatingOnly` filter). Neither form's REAL,
// authoritative command was ever verified anywhere in the run — this is the exact gap D56
// documents and fixes.
//
// Scope (narrow, per D56's Call 1 — NOT a full re-run of every gating check): only the checks
// the per-task tripwire actually skipped — those whose `fastCommand` differs from `command`,
// plus every `perTask: false` gating check. A check with no `fastCommand` already ran its
// authoritative `command` on EVERY per-task pass, so re-running it here would buy zero new
// coverage at real cost — see D56's `bella` measurement (a full sweep costs ~29% more than this
// narrow scope for exactly that reason).
//
// Reuses sdlc-flow.js's existing `renderCheckList(cfg, { gatingOnly: false, ... })` idiom
// (sdlc-flow.js:1666 — the end-of-flow review's authoritative re-run) rather than inventing a
// second one: passing gatingOnly:false makes renderCheckList emit each check's real `command`
// (never `fastCommand`) with no `perTask` filtering, for whatever check list it is given —
// so filtering the check list itself, before the call, is exactly enough to narrow the scope.
//
// Runs only once per FULL spec run (the fullRun guard below is unchanged by this decision — a
// partial task-subset run, e.g. `/sdlc-task <slug> 1`, never triggers it and never closes the
// block) that did NOT bail, and only when testDepth is 'fast' — under `--test-depth full` every
// check (including perTask:false ones) already ran authoritative on every per-task pass, via the
// same `gatingOnly:false` codepath, so reconciling again here would be a pure double-run.
//
// Resume semantics fall out for free: `--resume` on an already-fully-passed task set (e.g. after
// a prior `reconcile_failed`) skips every task in the per-task loop (passedFromState already has
// them all) and lands straight here — re-running ONLY the reconcile, never the task loop, exactly
// as D56's failure-path recovery describes.
//
// Failure path (D56 Call 2): a failing reconcile does NOT run bookkeep, does NOT flip the block
// to done, and does NOT touch the per-task commits already made — it bails into a distinct
// terminal status, `reconcile_failed`, with the raw failing output preserved for the operator.
// This is never folded into an ordinary `blocked`/bail: there is no task to attribute it to and
// no per-task attempt budget left to spend retrying it here.
// ----------------------------------------------------------------
let reconcileFailed = false
let reconcileFailBlob = ''
if (!bailed && fullRun && testDepth === 'fast') {
  phase('Reconcile')
  const reconcileChecks = (harnessCfg?.validation?.checks ?? [])
    .filter(c => c.gates && ((c.fastCommand && c.fastCommand !== c.command) || c.perTask === false))
  if (reconcileChecks.length) {
    log(`Terminal reconcile (D56): ${reconcileChecks.length} check(s) the per-task fast tripwire substituted or skipped — running their authoritative form once before bookkeep.`)
    const reconcileCfg = { ...harnessCfg, validation: { ...(harnessCfg.validation || {}), checks: reconcileChecks } }
    const reconcileResult = await tracedAgent(`${W}
You are the terminal authoritative-reconcile agent for the lean /sdlc-task pipeline (D56). Every
task in this spec already passed its fast, per-task tripwire — but that tripwire ran a narrower
\`fastCommand\` in place of some checks' real \`command\`, and skipped every \`perTask: false\` check
entirely. This is the ONE point in the run where their real, authoritative form is verified,
before the block can be reported done. All Bash calls run from the run root (prefix each with:
cd ${runDir} &&).

${renderCheckList(reconcileCfg, { gatingOnly: false, cwd: runDir, engineFiles: [] })}

For each check record: name, passed (true iff exit code 0), the command, and failure output.
Return via StructuredOutput: allPassed (true only if EVERY check above passed), passCount,
failCount, failedTests (names), failBlob (compact: failing check names + the tail of their
output; empty when allPassed), notes.
`, withModel({ label: 'reconcile', schema: TEST_SCHEMA, phase: 'Tasks' }, MODEL.test))
    if (!reconcileResult || !reconcileResult.allPassed) {
      reconcileFailed = true
      reconcileFailBlob = (reconcileResult && reconcileResult.failBlob) || 'Reconcile agent returned null or an incomplete result.'
      log(`Terminal reconcile FAILED (D56) — bookkeep is skipped; the block is NOT reported done. ${reconcileFailBlob}`)
    } else {
      log(`Terminal reconcile passed (D56): ${reconcileResult.passCount} check(s), all authoritative.`)
    }
  } else {
    log('Terminal reconcile (D56): no gating check needed reconciling (no fastCommand substitutions, no perTask:false checks in this project) — skipped, zero added cost.')
  }
}

state.status = bailed ? 'blocked' : (reconcileFailed ? 'reconcile_failed' : 'done')
if (reconcileFailed) {
  const reconcileBailReason = `Terminal reconcile failed (D56): ${reconcileFailBlob}`
  // APPEND-ONLY (BT.ticket.bails-must-be-append-only) — no task to attribute this to (D56: it
  // fires after every task already passed its own tripwire), so task_id stays null.
  state.bails = [...state.bails, { occurred_at: '__BAIL_OCCURRED_AT__', task_id: null, check_id: 'terminal-reconcile', failing_artifact: null, ownership: null, bail_class: null, reason: reconcileBailReason, resolution: null }]
  state.bail_reason = reconcileBailReason
}

// ----------------------------------------------------------------
// LEAN BOOKKEEP CLOSE-OUT — the one bit of authored state the lean engine still owes.
// Not a full wrap-up: no prose log.md entry, no D18 amendment log, no review/docs/PR (run /log-work
// for the narrative). It only flips the AUTHORED markers a passing run leaves stale — tasks.md task
// status, the status.md Progress row, and the state.json block status — then (in place, on main only)
// regenerates the derived surfaces via `mev emit-state --write`. Mirrors /start-block's flip pattern.
// Skipped entirely on a bail or a reconcile_failed (the block is not done) and on a partial task
// selection (can't close the block).
// ----------------------------------------------------------------
let blockDone = !bailed && !reconcileFailed && passedAll.length === allTasks.length

// BT.ticket.sdlc-task-must-verify-its-blocks-acceptance-criteria (task 2): re-read the block
// record's acceptance_criteria array directly off disk (engine code, never an agent's
// transcription of it — this is the array acceptanceCriteriaVerdicts() below is graded against,
// so it must be the same bytes the block record actually carries). Legacy tasks-md specs
// (specSource !== 'block-record') carry no such array — [] is correct there, never a bail: this
// gate is a pure add-on to the D65 block-record path and must not touch the legacy path at all.
// Returns {criteria, reason}: criteria is the block record's acceptance_criteria array (possibly
// empty), and reason is null when criteria is non-empty and a short, named cause when it is
// empty — file unreadable, invalid JSON, key absent, or the array present but empty — so the
// caller can log WHY the Criteria stage is being skipped instead of silently no-opping (this is
// the defect measured on run wf_5ef1102e-490, 2026-09-07: the guard was true, the stage never
// fired, and every cause collapsed into the same bare []).
// NOTE: this function body inserted 26 net lines above the bookkeep-vault-commit anchor further
// down this file -- scripts/check_skill_sync.py, scripts/check_engine_docs_sync.py and
// scripts/test_engines_pass_agent.py's FROZEN_BASELINE were all re-pinned to match (content
// confirmed byte-identical to the pre-shift versions via diff).
// NO IN-PROCESS FILE I/O IS AVAILABLE HERE. `process` (and therefore `require`) does not exist in
// the Workflow runtime -- measured 2026-09-07 on run wf_dd76eb8f-565, which died with
// `ReferenceError: require is not defined at loadBlockRecordAcceptanceCriteria`, and confirmed
// directly by a standalone probe (`typeof process` -> 'undefined') during
// BT.ticket.engine-helpers-call-require-which-the-workflow-runtime-does-not-define. That crash was
// ALSO the root cause of the silent no-op this function was originally written to diagnose (run
// wf_5ef1102e-490): the very first body called require('fs') INSIDE a try whose catch returned a
// bare [], so the ReferenceError was swallowed and every block-record run silently graded zero
// criteria. This version reads the file the same way every other file-reading helper in this
// engine does -- a cheap probe agent that runs one Bash command and transcribes its output --
// never in-process fs.
const CRITERIA_LOAD_SCHEMA = {
  type: 'object',
  required: ['found', 'criteria', 'reason'],
  properties: {
    found:    { type: 'boolean', description: 'true iff the probe script printed FOUND:true' },
    criteria: { type: 'array', items: {}, description: 'when found is true, the JSON array parsed from the script\'s CRITERIA_JSON: line -- [] when found is false' },
    reason:   { type: 'string', description: 'the text after REASON: on the script\'s stdout (empty when found is true)' }
  }
}
async function loadBlockRecordAcceptanceCriteria(cwd, recordFile) {
  // Classification runs entirely inside the script below, never in the agent's own reasoning --
  // the same discipline verifyVaultCommit() (prompts/shared.js) documents: a cheap model
  // following multi-branch conditional prose reliably skips a branch, where a script cannot.
  const result = await agent(`
Run this exact script with Bash, verbatim — do not reason about the classification yourself, the
script already decided it:
\`\`\`
python3 -c "
import json, os, sys

path = os.path.join('${cwd}', '${recordFile}')

if not os.path.exists(path):
    print('FOUND:false'); print('REASON:file not found: ' + path); sys.exit(0)

try:
    with open(path) as f:
        raw = f.read()
except Exception as e:
    print('FOUND:false'); print('REASON:file not found: ' + str(e)); sys.exit(0)

try:
    parsed = json.loads(raw)
except Exception as e:
    print('FOUND:false'); print('REASON:invalid JSON: ' + str(e)); sys.exit(0)

if not isinstance(parsed, dict) or 'acceptance_criteria' not in parsed:
    print('FOUND:false'); print('REASON:no acceptance_criteria key'); sys.exit(0)

ac = parsed['acceptance_criteria']
if not isinstance(ac, list):
    print('FOUND:false'); print('REASON:acceptance_criteria is present but not an array'); sys.exit(0)

if len(ac) == 0:
    print('FOUND:false'); print('REASON:acceptance_criteria array is empty'); sys.exit(0)

print('FOUND:true')
print('CRITERIA_JSON:' + json.dumps(ac))
"
\`\`\`
Return via StructuredOutput: found (true iff the script printed "FOUND:true"), criteria (when
found is true, the JSON array parsed from the "CRITERIA_JSON:" line, copied verbatim -- [] when
found is false), reason (the text after "REASON:" on the script's stdout -- "" when found is
true).
`, { label: 'load-block-record-criteria', schema: CRITERIA_LOAD_SCHEMA, model: 'haiku' })
  if (!result || !result.found) {
    const reason = (result && result.reason) ? result.reason : 'probe agent returned no result'
    return { criteria: [], reason: `block record at ${recordFile}: ${reason}` }
  }
  return { criteria: Array.isArray(result.criteria) ? result.criteria : [], reason: null }
}
const blockAcceptanceCriteriaLoad = (specSource === 'block-record' && !bailed && !reconcileFailed)
  ? await loadBlockRecordAcceptanceCriteria(runDir, blockRecordFile)
  : { criteria: [], reason: null }
const blockAcceptanceCriteria = blockAcceptanceCriteriaLoad.criteria
if (blockAcceptanceCriteria.length === 0 && blockAcceptanceCriteriaLoad.reason) {
  log(`Acceptance-criteria stage skipped: ${blockAcceptanceCriteriaLoad.reason}`)
}

// Per-criterion verdicts for THIS run's payload (task 2 AC1: "the run payload carries a per-
// criterion verdict of met, unmet or not-evaluated, not a boolean over tasks"). Populated only
// when there is something to grade; stays [] for a bail/reconcile_failed run or a legacy spec.
let criteriaVerdicts = []
let criteriaRefuse = false
let criteriaRefuseReason = null
if (blockAcceptanceCriteria.length) {
  phase('Criteria')
  const criteriaEvidenceResult = await tracedAgent(`${W}
You are the acceptance-criteria EVIDENCE agent for the lean /sdlc-task pipeline
(BT.ticket.sdlc-task-must-verify-its-blocks-acceptance-criteria). Report EVIDENCE ONLY for each
criterion below — whether THIS run actually evaluated it, and if so what was observed. Do NOT
decide met/unmet/not-evaluated yourself and do NOT decide whether the block may close: that
verdict and that decision are computed in engine code from what you report here, never from your
own judgment call — this is the same discipline the bookkeep stage below already follows for
state.json flips. All Bash from the run root (cd ${runDir} && ...).

Block: ${blockId}
Tasks run this run: ${taskList.join(', ')}  (passed: ${passedTasks.join(', ') || 'none'})
Full spec run: ${fullRun ? 'yes (every task in the spec)' : 'no (a task subset)'}

The block record's acceptance_criteria array, verbatim from ${blockRecordFile} (each entry is
EITHER a bare string OR an object {criterion, gateable, evidence, ...} per block.schema.json's
oneOf — gateable defaults to true when omitted on either form):
${JSON.stringify(blockAcceptanceCriteria, null, 2)}

For EACH entry above, inspect what this run actually produced — the implement/test/fix output
already captured this run, this run's committed diffs, and any validation-command output — and
report one object:
  - criterion: the criterion's exact text (the bare string itself, or the object form's
    "criterion" field) — copied verbatim so it can be matched back to the entry above.
  - evaluated: true ONLY if this run actually checked whether the criterion holds (a command was
    run, a file was inspected, a test executed this run). false if nothing this run addressed it,
    even if it seems likely to be true.
  - met: meaningful only when evaluated is true — the observed result (true/false).
  - evidence: one line quoting what was actually observed (a command plus its output, or the exact
    file content inspected). Never a guess and never "it should work" / "presumably fine".
An entry whose object form declares "gateable": false is the spec author's own admission that this
run's evidence cannot cover it — it is fine, and often correct, to report evaluated:false for one
of those; do not strain to invent evidence for it.

Return via StructuredOutput: criteria (array of {criterion, evaluated, met, evidence}, one per
entry above, same order), notes.
`, withModel({ label: 'criteria-evidence', schema: CRITERIA_EVIDENCE_SCHEMA }, MODEL.bookkeep))

  const evidenceByCriterion = {}
  for (const item of (criteriaEvidenceResult && criteriaEvidenceResult.criteria) || []) {
    if (item && typeof item.criterion === 'string') {
      evidenceByCriterion[item.criterion] = { evaluated: !!item.evaluated, met: !!item.met }
    }
  }
  const verdict = acceptanceCriteriaVerdicts(blockAcceptanceCriteria, evidenceByCriterion)
  criteriaVerdicts = verdict.results
  criteriaRefuse = verdict.refuse
  criteriaRefuseReason = verdict.reason
  // BT.ticket.criteria-verdict-stage-silently-no-ops-and-is-never-persisted (task 3): mirror
  // into the in-memory `state` object the instant the verdicts are known, so every write from
  // here on (including the final writeTaskState() call) persists them to sdlc-task-state.json
  // instead of the value dying with the process. Never assigned when the Criteria stage does not
  // run (state.criteriaVerdicts stays the [] the state literal initialises it to).
  state.criteriaVerdicts = criteriaVerdicts
  if (criteriaRefuse) {
    log(`Acceptance-criteria verdict REFUSES a clean close: ${criteriaRefuseReason}`)
  } else {
    log(`Acceptance-criteria verdicts: ${criteriaVerdicts.map(r => r.verdict).join(', ')} — clean close not blocked on criteria.`)
  }
}

// A refused criteria verdict behaves exactly like reconcileFailed for closing purposes (task 2:
// "do NOT flip the block to done in state.json, and report the run as not cleanly closed — the
// same shape the existing reconcile_failed path uses") — it only ever narrows blockDone (never
// widens it back to true), and only matters when the run would otherwise have closed the block.
if (criteriaRefuse) {
  blockDone = false
  if (state.status === 'done') state.status = 'criteria_refused'
  state.bail_reason = `Acceptance criteria refused clean close: ${criteriaRefuseReason}`
  // APPEND-ONLY (BT.ticket.bails-must-be-append-only) — no task to attribute this to (it fires
  // after every task already passed its own tripwire, exactly like the reconcile bail above), so
  // task_id stays null.
  state.bails = [...state.bails, { occurred_at: '__BAIL_OCCURRED_AT__', task_id: null, check_id: 'acceptance-criteria', failing_artifact: null, ownership: null, bail_class: null, reason: state.bail_reason, resolution: null }]
}
// BT.ticket.bookkeep-leaves-derived-output-uncommitted (task 4): OPTIONAL post-emit commit hook,
// project policy only (mechanism: run it if configured; never a default, never a fact about where
// any project's scripts live). String, not boolean — a missing/blank key means "no hook". Manual-
// replication guide for this hook: .agents/skills/sdlc-task/SKILL.md, Step 4 item 5.
const postEmitCommitCommand = typeof harnessCfg?.postEmitCommitCommand === 'string' && harnessCfg.postEmitCommitCommand.trim()
  ? harnessCfg.postEmitCommitCommand
  : ''
let bookkeepResult = null
if (!bailed && !reconcileFailed) {
  // D46: when planning/ is a vaulted symlink, ${specFile}, planning/status.md, and planning/state.json
  // do not live in this repo at all — they live in the brain-owned vault repo at the symlink target. A
  // plain `git add` against any of them from the run root fails ("pathspec is beyond a symbolic link"),
  // and the wrong repair is to checkout/commit inside the vault. The right behaviour is to stage+commit
  // them THROUGH their real path via `git -C <vault>`, on whatever branch the vault repo is already on,
  // with no checkout at all. `vault` was already resolved once, before the per-task loop, and is
  // reused here (never a second detectPlanningVault() call).
  bookkeepResult = await tracedAgent(`${W}
You are the lean bookkeeping close-out for an /sdlc-task run. Flip ONLY the authored status markers a
passing run leaves stale, then commit. Do NOT write a log.md narrative entry, a D18 amendment log, or
any prose — that is /log-work's job. All Bash from the run root.

${renderOperatorGatedACRule()}

Target:
  Spec:        ${blockId}
  Tasks run:   ${taskList.join(', ')}  (passed: ${passedTasks.join(', ') || 'none'})
  Full spec run: ${fullRun ? 'yes (every task in the spec)' : 'no (a task subset — do NOT close the block)'}
  Spec-wide:   ${passedAll.length}/${allTasks.length} tasks passed across all runs${outstandingTasks.length ? ` | outstanding: ${outstandingTasks.join(', ')}` : ''}
  Block done:  ${blockDone ? 'yes — every task in the spec has passed, each with a confirmed workAssertionPassed outcome from its own implement/fix stage (the per-task loop refuses to mark a task passed without one)' : criteriaRefuse ? `no — REFUSED by the acceptance-criteria gate (BT.ticket.sdlc-task-must-verify-its-blocks-acceptance-criteria): ${criteriaRefuseReason}. Every task passed, but this refusal overrides that — do NOT flip the block to done in state.json this run regardless.` : `no — keep the block open/in-progress (outstanding: ${outstandingTasks.join(', ') || 'none, but bailed/reconcile_failed this run'})`}

1. Read the surfaces:
   cd ${runDir} && cat ${specFile}
   cd ${runDir} && cat planning/status.md
   cd ${runDir} && cat ${stateFile}

2. Mark the passed tasks (${passedTasks.join(', ') || 'none'}) done in ${specFile} (Edit tool): add the
   engine's task-done marker to each passed task's line if the spec uses one (e.g. a leading "[done]"),
   mirroring how completed tasks are already marked in that file. NEVER remove or alter a marker
   already present from a prior run — this run only ADDS markers for ${passedTasks.join(', ') || 'none'}.
   If the spec has no such marker convention, leave it and set tasksMarked=false.
   - After marking, COUNT the CUMULATIVE total: how many of the spec's tasks now carry a done marker
     (this run's + every prior run's combined), out of the spec's total task count (${allTasks.length}).
     This run's own tally — ${passedTasks.length} of ${taskList.length} selected this run — is only a
     SLICE. Never use that slice alone as "how many tasks are done" anywhere you write a count; use the
     cumulative count you just derived from the file.
   - Update the spec's provenance stub, if it has one. Many specs open with "**Status:**" /
     "**Last run:**" lines near the top. Those are AUTHORED markers, not derived — nothing else in
     the pipeline ever refreshes them, so a run that skips this leaves a fully-executed spec still
     reading "**Status:** Not started" (measured on \`micro-spec-large\`, which ran to completion with
     its stub untouched). Set "**Status:**" to ${blockDone ? '"Done"' : '"In progress"'} and
     "**Last run:**" to today's date (cd ${runDir} && date +%Y-%m-%d) plus this run's outcome
     (e.g. "2026-01-01 — tasks ${taskList.join(', ')} passed"). If the spec carries no such stub,
     do nothing — never invent one.

3. Before editing planning/status.md, load the \`write-okf-markdown\` skill — this step edits an
   EXISTING file's YAML frontmatter, and the skill carries the frontmatter-must-start-at-line-1 rule
   and the insert-point trap (a row inserted after the OPENING \`---\` instead of the CLOSING one
   destroys the block, per E_SYNC_WATERMARK_MALFORMED). HQ standing rule 6 and base-template standing
   rule 11 both require this before writing or editing any \`.md\`; this stage is one of the fleet's
   two highest-volume \`.md\` writers.
   "Current focus" is APPEND-ONLY narrative — never delete or rewrite any existing line under it; a
   prior block's narrative must survive this edit VERBATIM. The one exception: if an existing line
   already refers to THIS spec ("${blockId}") by name (e.g. from an earlier partial run), delete
   ONLY that one line now (Edit tool) — never the whole section — so the scripted write below lands
   its replacement as a clean single line rather than a duplicate.
   Compose the ONE new Current-focus line as plain text (do not write it into the file yet):
   ${blockDone
     ? `- The full spec "${blockId}" is done. The line must record that, citing the CUMULATIVE task
     count you derived in step 2 (e.g. "${blockId}: done (N of ${allTasks.length} tasks)").`
     : `- The spec stays "In progress" (a task subset ran). The line should point at the next task if
     helpful, citing the cumulative count from step 2.`}
   Then run \`date +%Y-%m-%d\` to get today's date.

   VALIDATE-THEN-COMMIT CONTRACT — the write must not stand unless \`mev validate-brain --sync\`
   accepts it. Run ONE scripted mutation (never the Edit tool for this part) that captures the
   pre-write bytes, mutates in memory, runs \`mev validate-brain --sync\` (one flag, never combined
   with another flag) BEFORE and AFTER the write, and rejects — byte-exact rollback — any write that
   introduces diagnostic lines NOT present in the BEFORE baseline. A pre-existing corpus error (e.g.
   a sibling lane's unrelated breakage) must never block this write — NET-NEW only, the same delta-
   attribution rule step 4 below and the push gate use under D64. Substitute the Current-focus line
   for <RECENT_WORK_LINE> and today's date for <LAST_UPDATED_DATE> (keep both as the script's sole
   argv, each quoted):
${renderStatusWriteScript({ runRoot: runDir, indent: '   ' })}
   This script structurally cannot reintroduce either of the two frontmatter write defects: it never
   touches any line at or before the closing \`---\` fence (so \`timestamp\`, an RFC3339 value derived
   by \`mev emit-state --write\` later in this same stage — step 5 below — is never hand-written
   here, which is what keeps it from going out of step with the HQ cache doc's \`synced_from\`,
   E_SYNC_DRIFT), and its own new body line always lands strictly AFTER that closing fence, never the
   opening one (the historical EN.ticket.term-core-real-tmux-option-reads break). Read the script's
   own stdout AND exit code — do not infer success yourself:
     - "STATUS_WRITE:written" (exit 0) → mev validated the write and found no net-new diagnostics.
       Set statusUpdated=true and statusWriteValidated=true. If step 2's "Block done" is yes, also
       flip the Progress Table's Status cell for "${blockId}" to "Done" now (Edit tool — a plain body
       table-cell edit, safe once the risky part above has already landed and validated).
     - "STATUS_WRITE:unvalidated" (exit 0) → mev is not installed; the write landed unchecked
       (line-level parsing only, matching how the harness degrades other absent tooling). Set
       statusUpdated=true, statusWriteValidated=false, and copy the UNVALIDATED line verbatim into
       notes — this is a DEGRADE, not a silent pass. Still flip the Progress Table cell as above when
       "Block done" is yes.
     - "STATUS_REJECTED:written" (exit 1) → the write introduced net-new corpus errors and was rolled
       back; planning/status.md on disk is now byte-identical to its content before this step ran. Set
       statusUpdated=false, statusWriteRejected=true, and copy every "NET_NEW:" line verbatim into
       notes — this MUST be reported, never silently swallowed. Do NOT touch the Progress Table cell
       this run even if "Block done" said yes; the block stays open (see step 4) until a clean write
       lands on a later run.

4. Flip the block's AUTHORED status in planning/state.json (skip this entire step silently if the repo
   has no planning/state.json, if "Block done" above is "no", OR if step 3's write was
   STATUS_REJECTED — never flip state.json to closed while status.md itself failed to record the
   close). state.json is the authoritative block graph — leaving it stale poisons every derived
   surface, because \`mev emit-state\` reads this field and NEVER infers completion from status.md.
   - Resolve the block's canonical ID from the status.md Progress Table row (the <BlockID> column, or
     the id that row maps to in state.json). This is the only part of this step that stays your
     judgment call. Do NOT locate or hand-edit the block's \`tracks[].blocks[]\` entry yourself on any
     path — the mutation is entirely scripted below, never an Edit-tool diff, whether \`mev\` is
     present or absent.
   - DETERMINISTIC-FIRST CONTRACT: when \`mev\` is on PATH, this repo resolves to a brain.toml repo
     slug, and this run is NOT in a worktree, the script below calls \`mev set-block-status
     <repo>:<id> closed --write\` and derives success or failure from THAT SUBPROCESS'S OWN EXIT
     CODE — never from anything you narrate. Only when that deterministic route is unavailable (mev
     absent, no resolvable repo slug, or a worktree run) does the script fall back to the validated
     hand-edit contract: capture the pre-write bytes, mutate in memory, run \`mev validate-brain
     --state\` BEFORE and AFTER the write, and reject — byte-exact rollback — any write that
     introduces diagnostic lines NOT present in the BEFORE baseline (pre-existing corpus errors, e.g.
     a sibling lane's unrelated breakage, must never block this write — NET-NEW only, the same
     delta-attribution rule the push gate uses under D64). Run the script exactly once; do not choose
     between the two routes yourself — the script resolves that at generation/run time.
     Substitute the id you resolved for <RESOLVED_ID> (keep it as the script's sole argv, quoted):
${await renderStateFlipScript({ runRoot: runDir, indent: '     ', runningInWorktree: useWorktree })}
     Read the script's own stdout AND exit code — do not infer success yourself, and simply
     TRANSCRIBE which of these lines it printed rather than re-deriving the outcome:
       - "FLIPPED: <repo>:<id>" (exit 0, deterministic route) → \`mev set-block-status --write\`
         reported success via its own exit code. Set blockStatusFlipped to <id> and
         stateWriteValidated=true (mev-backed, deterministic).
       - "FLIP_REFUSED: <repo>:<id>" followed by one or more "MEV_OUTPUT:" lines (exit 1,
         deterministic route) → \`mev set-block-status --write\` refused the flip. Set
         blockStatusFlipped to "", and copy every "MEV_OUTPUT:" line verbatim into notes — this MUST
         be reported, never silently swallowed. Do not treat the block as closed this run even though
         "Block done" above said yes; it stays open until a clean write lands on a later run.
       - "NOT_FOUND" (exit 0, fallback route) → the file stays byte-unchanged. Report it in notes, do
         NOT fabricate a block entry, and set blockStatusFlipped to "".
       - "FLIPPED:<id>" with NO "UNVALIDATED:" line (exit 0, fallback route) → mev validated the
         write and found no net-new diagnostics. Set blockStatusFlipped to that id and
         stateWriteValidated=true.
       - "FLIP_REFUSED:<id>" followed by one or more "MEV_OUTPUT:" lines (exit 1, fallback route) →
         mev is not on PATH, so the script REFUSED to write an unvalidated status into state.json
         (D86) and the file is byte-unchanged. Set blockStatusFlipped to "", and copy every
         "MEV_OUTPUT:" line verbatim into notes — this MUST be reported, never silently swallowed. The
         block stays open until mev is installed and a validated write lands on a later run.
       - "REJECTED:<id>" (exit 1, fallback route) → the write introduced net-new schema errors and
         was rolled back; state.json on disk is now byte-identical to its content before this step
         ran. Set blockStatusFlipped to "", stateWriteRejected=true, and copy every "NET_NEW:" line
         verbatim into notes. This MUST be reported — never silently swallow it, and do not treat the
         block as closed this run even though "Block done" above said yes; step 3's status.md edit
         already recorded progress narrative, but the block stays open until a clean write lands on a
         later run.
   - WORKTREE NOTE (decided, not deferred): inside a worktree the script above never calls \`mev
     set-block-status\` at all (it always chains \`emit-state --write\`, which refuses to run inside a
     linked worktree) — it goes straight to the fallback hand-edit contract, validated the same way
     as in-place (\`mev validate-brain --state\` reads planning/state.json in THIS repo directly and
     needs no cross-repo BRAIN_ROOT resolution). Only step 5's \`emit-state --write\` (regenerating
     derived surfaces) is deferred to merge in worktree mode; this step's write and its validation are
     never deferred.

5. Regenerate derived surfaces via \`mev emit-state --write\`. Run this step whenever this bookkeep
   stage runs at all — it is NOT conditional on "Block done" above: step 2/3 already edited
   ${specFile}/planning/status.md regardless of whether the block closed this run, so the derived
   surfaces (status.md rollups, /attention boards, wave tables) need resyncing every time, not only on
   a full block close.
   ${useWorktree
     ? `- Do NOT run \`mev emit-state --write\`: this is a linked git worktree, where emit-state refuses to run. The derived surfaces regenerate on MAIN when the branch merges (/clean-worktree). Set emitStateRan=false.`
     : `- This run is IN PLACE on main, so emit-state is safe: cd ${runDir} && mev emit-state --write${await renderAgentFlag()}${await renderScopeFlag()} . If \`mev\` or brain.toml is absent (standalone repo), skip it silently and set emitStateRan=false; else emitStateRan=true. Do NOT hand-reimplement focus/rollup derivation.`}

6. OPTIONAL post-emit commit hook. ${postEmitCommitCommand
     ? `planning/harness.json declares postEmitCommitCommand — run it ONLY when step 5 set emitStateRan=true
   (i.e. never in worktree mode, and never when emit-state itself was skipped). This mechanism does not
   know or care what the command does — it is project policy, not engine fact:
     cd ${runDir} && ${postEmitCommitCommand}
   Check the real exit code (not a piped one — see the pipe-exit-code trap). Exit 0 → postEmitHookRan=true,
   postEmitHookFailed=false. Non-zero → postEmitHookRan=true, postEmitHookFailed=true, and copy the
   command's stderr/stdout tail verbatim into notes — this MUST be reported, never swallowed. Do not
   retry it and do not attempt to "fix" or roll anything back yourself; the command owns its own
   transaction, so a failure here means step 7 below still runs as normal (this hook is independent of
   this stage's own commit).`
     : `planning/harness.json defines no postEmitCommitCommand — skip this step entirely. Set
   postEmitHookRan=false and postEmitHookFailed=false. This is the default, unchanged behaviour.`}

7. Commit your edits (stage explicitly — never git add -A). NEVER run git checkout, git switch, or git
   branch outside this repo's own root (${runDir})${vault.vaulted ? ` or the vault's own root (${vault.planningPath})` : ''} —
   if a git add fails, report the failure in notes; do not relocate the commit to make it succeed.
${vault.vaulted ? `
   planning/ is a vaulted symlink (D46) — its bytes live at ${vault.planningPath}, a different repo. Every
   file this step touches (the spec, status.md, state.json) lives under planning/, so stage + commit them
   ALL there, via \`git -C\`, on whatever branch that repo is already on. Do NOT cd into it and do NOT
   checkout/switch/branch there:
   cd ${runDir} && ${GIT} -C ${vault.planningPath} add ${vault.planningPath}/${blockId}/tasks.md 2>/dev/null || true
   cd ${runDir} && ${GIT} -C ${vault.planningPath} add ${vault.planningPath}/status.md
   cd ${runDir} && ${GIT} -C ${vault.planningPath} add ${vault.planningPath}/state.json 2>/dev/null || true
   Then commit ONLY these three paths — pass them explicitly to \`git commit\` itself (not merely to
   \`git add\`), so anything a sibling lane already had staged in this same vault repo is left staged
   and untouched by this commit; ${renderNoAttributionTrailer()}:
   cd ${runDir} && ${GIT} -C ${vault.planningPath} diff --cached --quiet -- ${vault.planningPath}/${blockId}/tasks.md ${vault.planningPath}/status.md ${vault.planningPath}/state.json || (${renderCommitSafetyGuard('git -C ' + vault.planningPath)} && ${GIT} -C ${vault.planningPath} commit -m "$(cat <<'EOF'
chore: sdlc-task bookkeep — ${blockId}
EOF
)" -- ${vault.planningPath}/${blockId}/tasks.md ${vault.planningPath}/status.md ${vault.planningPath}/state.json)
   cd ${runDir} && ${GIT} -C ${vault.planningPath} log --oneline -1` : `
   planning/ is a plain directory here (not vaulted) — everything commits together as before:
   cd ${runDir} && ${GIT} add ${specFile} planning/status.md
   cd ${runDir} && ${GIT} add planning/state.json 2>/dev/null || true
   ${renderNoAttributionTrailer()}:
   cd ${runDir} && ${renderCommitSafetyGuard()} && ${GIT} commit -m "$(cat <<'EOF'
chore: sdlc-task bookkeep — ${blockId}
EOF
)" || echo "NOTHING_TO_COMMIT"
   cd ${runDir} && ${GIT} log --oneline -1`}

Return via StructuredOutput: statusUpdated, statusWriteValidated, statusWriteRejected, tasksMarked, blockStatusFlipped, emitStateRan, postEmitHookRan, postEmitHookFailed, commitHash, notes.
`, withModel({ label: 'bookkeep', schema: BOOKKEEP_SCHEMA }, MODEL.bookkeep))
  if (bookkeepResult?.statusWriteRejected) {
    log(`status.md: write REJECTED — net-new corpus error(s) from mev validate-brain --sync; rolled back byte-exact. ${bookkeepResult?.notes || ''}`)
  }
  if (bookkeepResult?.stateWriteRejected) {
    log(`state.json: write REJECTED — net-new schema error(s) from mev validate-brain --state; rolled back byte-exact, block NOT closed this run. ${bookkeepResult?.notes || ''}`)
  } else if (bookkeepResult?.blockStatusFlipped) {
    log(`state.json: block "${bookkeepResult.blockStatusFlipped}" → closed (${bookkeepResult.stateWriteValidated ? 'deterministic: mev set-block-status --write exit code, or fallback validated via mev validate-brain --state net-new only' : 'stateWriteValidated=false reported -- unexpected since D86 (mev absent refuses rather than writing); check bookkeep notes'})${bookkeepResult.emitStateRan ? '; derived surfaces (incl. focus.next) regenerated (mev emit-state --write).' : useWorktree ? '; focus.next is DEFERRED — it still points at the pre-close state until /clean-worktree runs `mev emit-state --write` on merge.' : '.'}`)
  } else if (blockDone) {
    log(`Bookkeep: no state.json block flipped (${bookkeepResult?.notes || 'no state.json, or block not found'}).`)
  }
  if (bookkeepResult?.postEmitHookRan) {
    log(bookkeepResult?.postEmitHookFailed
      ? `postEmitCommitCommand FAILED (planning/harness.json) — reported, not swallowed. ${bookkeepResult?.notes || ''}`
      : `postEmitCommitCommand ran (planning/harness.json).`)
  }
}

// Fold bookkeep's emitStateRan into the in-memory state so the final write below persists it —
// the first site in this file where the field actually reaches disk (see the `state` declaration
// above). `false` (not left null) when bookkeep ran but the agent omitted the field; stays null
// only when bookkeep itself was skipped (bailed / reconcile_failed).
if (bookkeepResult) state.emitStateRan = bookkeepResult.emitStateRan ?? false

// Final run-state write — disk-only, never committed (see writeTaskState). Captures the final
// token roll-up after the bookkeep close-out ran.
await writeTaskState(`run ${state.status} (${passedTasks.length}/${taskList.length})`, { cwd: runDir })

const tokensBlock = state.tokens   // already rebuilt by the writeTaskState call just above (no traced agent ran since); reuse it rather than rebuilding (carry-in #3)
log(`Token roll-up: ${tokensBlock.total.inTokEst} inTokEst${tokensBlock.total.outTok ? ` | ${tokensBlock.total.outTok} outTok` : ''} across ${tokensBlock.stages.length} stage(s) — persisted in ${stateFile}.`)
log(`/sdlc-task complete. ${bailed ? `BAILED: ${bailReason}` : reconcileFailed ? `RECONCILE FAILED (D56): ${reconcileFailBlob}` : 'all selected tasks passed'} | passed ${passedTasks.length}/${taskList.length}.`)
if (useWorktree) {
  log(`Worktree branch "${branchName}" carries the commits at ${runDir}.`)
  log(`Integrate it when ready: git checkout main && git merge ${branchName}, then git worktree remove ${runDir} && git branch -d ${branchName}.`)
} else {
  log(`Commits landed in place on branch "${branchName}".`)
}
if (bailed) {
  log(`Pick up: read ${stateFile} for per-task state, fix the blocker, then re-run with --resume.`)
} else if (reconcileFailed) {
  log(`Pick up: all per-task commits stand — only the terminal reconcile failed. Fix the surfaced failure, then re-run with --resume (every task is already "passed", so this re-runs ONLY the reconcile) or drive it manually with /fix.`)
} else {
  log(`Run /log-work to record the narrative log.md entry (the lean bookkeep flipped status only — no prose was written).`)
}

return {
  blockId,
  mode: state.mode,
  branch: branchName,
  runDir,
  bailed,
  reconcileFailed,
  bailReason: bailReason || (reconcileFailed ? state.bail_reason : (criteriaRefuse ? state.bail_reason : null)),
  tasksRun: taskList,
  tasksPassed: passedTasks,
  // BT.ticket.sdlc-task-must-verify-its-blocks-acceptance-criteria (task 2): the run's actual
  // per-criterion verdict list (met/unmet/not-evaluated), computed in engine code by
  // acceptanceCriteriaVerdicts() from evidence the criteria-evidence stage reported — never a
  // boolean over tasks. [] for a legacy tasks-md spec or a bail/reconcile_failed run that never
  // reached the criteria stage.
  criteriaVerdicts,
  criteriaRefuse,
  criteriaRefuseReason,
  stateFile,
  tokens: tokensBlock,
}

// <<shared:renderAgentFlag>>
// Renders the `--agent <id>` argument for a `mev emit-state --write` / `mev set-block-status
// --write` invocation so a lane that holds its own exclusive lease is exempt from mev's
// `refuse_if_quiesced` (BT.ticket.engines-must-pass-agent-to-mev). Returns '' (empty string) when
// no identity resolves — an unconditional flag would change every non-lane, standalone-repo run
// of these engines across 18+ downstream repos with no brain.toml at all.
//
// MEASURED 2026-09-07 (BT.ticket.engine-helpers-call-require-which-the-workflow-runtime-does-not-
// define): the Workflow script sandbox has NO `process` global at all, not merely no `require` —
// a direct probe (`typeof process`) returned 'undefined'. The previous version of this function
// read `process.env.FLEET_LANE_AGENT` as its very first statement, inside its own try/catch, so
// every call threw immediately and fell straight to the catch's `return ''` — this function was
// dead code, unconditionally, in every real engine run, not merely on the `require`-only
// branches downstream of that line. There is no in-process fallback: env lookups and file reads
// both go through a cheap probe agent, the same convention `detectPlanningVault()` /
// `resolveRepoRoot()` above already use, and (per `verifyVaultCommit()`'s note above) the
// resolution logic runs entirely inside the probe SCRIPT — the agent only transcribes its one
// output line, it never reasons about TOML or lease-file structure itself.
//
// Resolution order (all decided inside the probe script):
//   1. FLEET_LANE_AGENT env var, if set and non-empty.
//   2. Else the `agent` field of <lock_dir>/leases/lease-<repo>.json, where <repo> is the
//      brain.toml [[repos]] slug whose repo_path resolves to (or is an ancestor of) cwd, and
//      <lock_dir> uses the SAME precedence scripts/check_lane_agents.py's find_lock_dir()
//      already uses: FLEET_LOCK_DIR env var, else a brain.toml found by walking up from cwd,
//      joined with .fleet-locks. No new precedence is introduced.
//   3. Else no identity resolves and '' is returned.
async function renderAgentFlag() {
  const result = await agent(`
Resolve this fleet lane's agent identity for a mev '--agent' exemption flag. Run this exact
script with Bash, verbatim — do not reason about the resolution yourself, the script already
decided it:
\`\`\`
python3 -c "
import os, json, sys

def find_brain_root(start):
    d = os.path.abspath(start)
    while True:
        if os.path.exists(os.path.join(d, 'brain.toml')):
            return d
        parent = os.path.dirname(d)
        if parent == d:
            return None
        d = parent

def repo_blocks(text):
    blocks, cur = [], None
    for line in text.splitlines():
        if line.strip() == '[[repos]]':
            cur = []
            blocks.append(cur)
        elif cur is not None:
            cur.append(line)
    return [chr(10).join(b) for b in blocks]

def block_value(block_text, key):
    dq = chr(34)
    for line in block_text.splitlines():
        s = line.strip()
        eq = s.find('=')
        if eq == -1 or s[:eq].strip() != key:
            continue
        v = s[eq + 1:].strip()
        if len(v) >= 2 and v[0] == dq and v[-1] == dq:
            return v[1:-1]
        return None
    return None

def best_slug(brain_root, cwd):
    with open(os.path.join(brain_root, 'brain.toml')) as f:
        text = f.read()
    here = os.path.abspath(cwd)
    best, best_depth = None, -1
    for block in repo_blocks(text):
        slug = block_value(block, 'slug')
        repo_path = block_value(block, 'repo_path')
        if not slug or not repo_path:
            continue
        repo_abs = os.path.abspath(os.path.join(brain_root, repo_path))
        if here != repo_abs and not here.startswith(repo_abs + os.sep):
            continue
        depth = len(repo_abs.split(os.sep))
        if depth > best_depth:
            best_depth, best = depth, slug
    return best

env_agent = os.environ.get('FLEET_LANE_AGENT', '').strip()
if env_agent:
    print('VALUE:' + env_agent); sys.exit(0)

brain_root = find_brain_root(os.getcwd())
if not brain_root:
    print('VALUE:'); sys.exit(0)

slug = best_slug(brain_root, os.getcwd())
if not slug:
    print('VALUE:'); sys.exit(0)

lock_dir = os.environ.get('FLEET_LOCK_DIR', '').strip() or os.path.join(brain_root, '.fleet-locks')
lease_path = os.path.join(lock_dir, 'leases', 'lease-' + slug + '.json')
if not os.path.exists(lease_path):
    print('VALUE:'); sys.exit(0)

try:
    with open(lease_path) as f:
        lease = json.load(f)
    resolved = str(lease.get('agent') or '').strip()
except Exception:
    resolved = ''
print('VALUE:' + resolved)
"
\`\`\`
The script never fails destructively — any error inside it degrades to an empty VALUE: line.
Return via StructuredOutput: value (the text after "VALUE:" on the script's stdout, or "" if that
line is missing or the script produced no output).
`, { label: 'render-agent-flag', schema: RENDER_IDENTITY_SCHEMA, model: 'haiku' })
  const value = (result && typeof result.value === 'string') ? result.value.trim() : ''
  return value ? ` --agent ${value}` : ''
}
// <</shared:renderAgentFlag>>

// <<shared:renderScopeFlag>>
// Renders the `--scope <slug>` argument for a `mev emit-state --write` invocation so an
// in-place lane's wrap-up/bookkeep regenerates only its OWN repo's derived surfaces instead of
// the whole corpus (BT.ticket.engines-pass-scope-to-emit-state). Returns '' (empty string) when
// no repo slug resolves -- an unconditional flag would break every non-lane, standalone-repo run
// of these engines across 18+ downstream repos with no brain.toml at all.
//
// Same MEASURED 2026-09-07 finding as renderAgentFlag() above applies here identically: `process`
// does not exist in the Workflow sandbox, so the old `process.env.FLEET_LANE_REPO` first
// statement always threw and this function always returned '' — resolution now goes through the
// same probe-script convention.
//
// Resolution order (mirrors renderAgentFlag()'s FLEET_LANE_AGENT / lease-file precedence):
//   1. FLEET_LANE_REPO env var, if set and non-empty.
//   2. Else the brain.toml [[repos]] walk-up already used by renderAgentFlag(): the deepest
//      repo_path that is cwd or an ancestor of cwd, yielding that entry's slug.
//   3. Else no identity resolves and '' is returned.
async function renderScopeFlag() {
  const result = await agent(`
Resolve this fleet lane's own repo slug for a mev '--scope' argument. Run this exact script with
Bash, verbatim — do not reason about the resolution yourself, the script already decided it:
\`\`\`
python3 -c "
import os, sys

def find_brain_root(start):
    d = os.path.abspath(start)
    while True:
        if os.path.exists(os.path.join(d, 'brain.toml')):
            return d
        parent = os.path.dirname(d)
        if parent == d:
            return None
        d = parent

def repo_blocks(text):
    blocks, cur = [], None
    for line in text.splitlines():
        if line.strip() == '[[repos]]':
            cur = []
            blocks.append(cur)
        elif cur is not None:
            cur.append(line)
    return [chr(10).join(b) for b in blocks]

def block_value(block_text, key):
    dq = chr(34)
    for line in block_text.splitlines():
        s = line.strip()
        eq = s.find('=')
        if eq == -1 or s[:eq].strip() != key:
            continue
        v = s[eq + 1:].strip()
        if len(v) >= 2 and v[0] == dq and v[-1] == dq:
            return v[1:-1]
        return None
    return None

env_repo = os.environ.get('FLEET_LANE_REPO', '').strip()
if env_repo:
    print('VALUE:' + env_repo); sys.exit(0)

brain_root = find_brain_root(os.getcwd())
if not brain_root:
    print('VALUE:'); sys.exit(0)

with open(os.path.join(brain_root, 'brain.toml')) as f:
    text = f.read()
here = os.path.abspath(os.getcwd())
best, best_depth = None, -1
for block in repo_blocks(text):
    slug = block_value(block, 'slug')
    repo_path = block_value(block, 'repo_path')
    if not slug or not repo_path:
        continue
    repo_abs = os.path.abspath(os.path.join(brain_root, repo_path))
    if here != repo_abs and not here.startswith(repo_abs + os.sep):
        continue
    depth = len(repo_abs.split(os.sep))
    if depth > best_depth:
        best_depth, best = depth, slug
print('VALUE:' + (best or ''))
"
\`\`\`
The script never fails destructively — any error degrades to an empty VALUE: line.
Return via StructuredOutput: value (the text after "VALUE:" on the script's stdout, or "" if that
line is missing or the script produced no output).
`, { label: 'render-scope-flag', schema: RENDER_IDENTITY_SCHEMA, model: 'haiku' })
  const value = (result && typeof result.value === 'string') ? result.value.trim() : ''
  return value ? ` --scope ${value}` : ''
}
// <</shared:renderScopeFlag>>

// <<shared:renderLaneHeartbeatRecipe>>
// Re-stamps this lane's claim+lease heartbeat FROM INSIDE the per-task test-stage recipe
// (BT.ticket.lane-heartbeat-goes-stale-mid-block, task 4), so a long block re-stamps between
// tasks instead of only at a block boundary (the release-and-re-take /orchestrate rule 10 already
// does). scripts/lane_heartbeat.py is the writer this calls; see that script's own module
// docstring for why a hand-driven lane needs this too, not only an /orchestrate-driven one.
//
// BEST-EFFORT, NEVER GATING: a spec run with no live claim or lease (outside /orchestrate, or a
// standalone downstream repo with no fleet lock dir at all) must not bail because a heartbeat
// could not be written -- the call is suffixed ` || true` and the prompt says explicitly that its
// exit code never affects allPassed.
//
// IDENTITY: reuses renderAgentFlag()/renderScopeFlag() -- the SAME identity these engines already
// thread to `mev emit-state --write` (and the same identity concept /orchestrate threads to
// `scripts/fleet_concurrency_check.py register --agent <this lane's agent identity>`) -- never a
// second, invented identity source. renderScopeFlag() renders a full `--scope <slug>` argument for
// mev, so the slug is pulled back out of it (mirrors renderStateFlipScript's identical extraction
// a few hundred lines above) rather than resolving the repo slug a third way.
async function renderLaneHeartbeatRecipe({ runRoot, blockId }) {
  const agentFlag = await renderAgentFlag()
  const scopeFlagRaw = await renderScopeFlag()
  const scopeMatch = scopeFlagRaw.match(/--scope\s+(\S+)/)
  const repoSlug = scopeMatch ? scopeMatch[1] : null
  const repoFlag = repoSlug ? ` --repo ${repoSlug}` : ''
  return `
Also re-stamp this lane's claim+lease heartbeat now (best-effort, NEVER gating -- a spec run with
no live claim or lease must not fail because of this; its own exit code never affects allPassed
above, which is why it is suffixed \` || true\`):
  cd ${runRoot} && python3 scripts/lane_heartbeat.py${agentFlag}${repoFlag} --current-block ${blockId} || true
`
}
// <</shared:renderLaneHeartbeatRecipe>>
