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
// /sdlc-flow; for a whole spec in place, /sdlc-run; for a roadmap, /sdlc-block.
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
//   Default: IN PLACE on the current branch (no worktree) — cheapest, like /sdlc-run.
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
//                          the Block-A `tokens` block). Written to disk after every task and
//                          again at the end (cat-visible for crash inspection); read back off
//                          disk only, by --resume — never out of git — so it is disk-only, never
//                          committed (D46: planning/ may be a vaulted symlink into the brain repo,
//                          where a plain `git add planning/...` fails).
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

function hasFlag(name) { return tokens.includes(name) }
function flagStr(name) {
  const i = tokens.indexOf(name)
  return (i === -1 || i + 1 >= tokens.length) ? null : tokens[i + 1]
}
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

const blockDir      = `planning/${blockId}`
const specFile      = `${blockDir}/tasks.md`
const tasksJsonFile = `${blockDir}/tasks.json`
const breakdownFile = `${blockDir}/breakdown.md`
const reportsDir    = `${blockDir}/sdlc/reports`
const stateFile     = `${blockDir}/sdlc/sdlc-task-state.json`   // COMMITTED authoritative run index (Block A)
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
const VAULT_DETECT_SCHEMA = {
  type: 'object',
  required: ['vaulted', 'planningPath'],
  properties: {
    vaulted:      { type: 'boolean', description: 'true iff planning/ is a symlink' },
    planningPath: { type: 'string', description: 'the resolved absolute real path of planning/' }
  }
}
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
    if [ -z "$(git -C ${vault.planningPath} status --porcelain -- "$p")" ] && git -C ${vault.planningPath} ls-files --error-unmatch -- "$p" >/dev/null 2>&1; then
      echo "VAULT_OK:$p"
    else
      echo "UNCOMMITTED:$p"
    fi
  elif [ -e "$BRAIN_ROOT/planning/$p" ]; then
    if [ -z "$(git -C "$BRAIN_ROOT/planning" status --porcelain -- "$p")" ] && git -C "$BRAIN_ROOT/planning" ls-files --error-unmatch -- "$p" >/dev/null 2>&1; then
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

// Given a task stage's self-reported filesModified (repo-root-relative) and a resolved vault, return
// the vault-relative subset (the part of the path after "planning/") that needs an independent
// vault-commit check. Derived from what the task ACTUALLY wrote — never a hard-coded filename list.
function vaultRelPathsFrom(filesModified, vault) {
  if (!vault.vaulted || !Array.isArray(filesModified)) return []
  return filesModified
    .filter(f => typeof f === 'string' && (f === 'planning' || f.startsWith('planning/')))
    .map(f => f.slice('planning/'.length))
    .filter(Boolean)
}

log(`Target: ${blockId} (${selectedTasks ? [...selectedTasks].sort((a, b) => a - b).join(', ') : 'all tasks'})`)
log(`Spec: ${specFile} | mode: ${useWorktree ? 'worktree' : 'in-place'}${resumeMode ? ' | RESUME' : ''}`)

// ================================================================
// Schemas
// ================================================================
const SETUP_SCHEMA = {
  type: 'object',
  required: ['runDir', 'branchName', 'baseSha'],
  properties: {
    runDir:         { type: 'string', description: 'Absolute path the pipeline runs from (worktree path under --worktree; else the repo root)' },
    branchName:     { type: 'string', description: 'The branch commits land on (a new worktree branch under --worktree; else the current branch)' },
    baseSha:        { type: 'string', description: 'The HEAD short sha AFTER setup, BEFORE any task commit — the emoji-gate diff base' },
    wasCreated:     { type: 'boolean', description: 'true if a new worktree was created (--worktree only)' },
    specFileExists: { type: 'boolean', description: 'true if the task spec file exists' },
    blockStatus:    { type: 'string', description: "This spec's Status in status.md (title-case), or 'Unknown'" },
    specThin:       { type: 'boolean', description: 'D19: true on a fresh (non-resume) run with a structurally-valid but substantively-thin spec; false on resume or a healthy spec.' },
    thinReason:     { type: 'string', description: 'D19: the specific thin-spec failures when specThin; empty string otherwise.' },
    envFilesCopied: { type: 'array', items: { type: 'string' }, description: '--worktree only: repo-root-relative paths of every gitignored env-shaped file seeded into the worktree (from ENV_COPIED: lines); empty array if none existed to copy.' },
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

// D16 derive-from-tasks.md fallback — see the abort below. Mirrors sdlc-block.js's ensureTasks()
// generator and /generate-tasks' --from mode: read the spec's authored step decomposition and
// write a fresh D45-shaped tasks.json from it (never a verbatim copy of the prose, never the
// superseded D44 {"tasks": [...]} wrapper).
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

const STATE_LOAD_SCHEMA = {
  type: 'object',
  required: ['exists'],
  properties: {
    exists:      { type: 'boolean', description: 'true if a valid sdlc-task-state.json was read' },
    startedAt:   { type: 'string',  description: "the file's started_at value, or '' when absent" },
    passedTasks: { type: 'array', items: { type: 'integer' }, description: 'task numbers whose status is "passed"' },
    bailReason:  { type: 'string',  description: 'the prior bail_reason, or "" when none' },
    notes:       { type: 'string' }
  }
}

const STAGE_SCHEMA = {
  type: 'object',
  required: ['success'],
  properties: {
    success:       { type: 'boolean' },
    filesModified: { type: 'array', items: { type: 'string' } },
    commitHash:    { type: 'string', description: 'Short hash of the commit this agent made, or empty string' },
    summary:       { type: 'string', description: 'One-line summary of what was implemented/fixed (folded into state.tasks[N].summary)' },
    decisions:     { type: 'array', items: { type: 'string' }, description: 'Non-obvious choices made (folded into state)' },
    filesReadKb:   { type: 'number', description: 'Telemetry (optional): sum of bytes of all files this stage cat/Read, divided by 1024.' },
    notes:         { type: 'string' }
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
    tasksMarked:        { type: 'boolean', description: 'true if tasks.md task markers were updated' },
    blockStatusFlipped: { type: 'string', description: 'the state.json tracks[].blocks[].id flipped to "closed", or "" if none (partial run, no state.json, or block not found)' },
    emitStateRan:       { type: 'boolean', description: 'true if mev emit-state --write ran successfully (false when skipped: worktree mode or mev/brain.toml absent)' },
    commitHash:         { type: 'string' },
    notes:              { type: 'string' }
  }
}

// ----------------------------------------------------------------
// MODEL TIERING — the primary token lever for this pipeline.
//
// Match the model to the work (mirrors sdlc-run/flow). To re-tier, change one value here.
// Valid values: 'haiku' | 'sonnet' | 'opus' | undefined (inherit session model).
// ----------------------------------------------------------------
const MODEL = {
  setup:       'haiku',    // scripted git: locate the repo root, or follow the worktree free-name recipe
  enumerate:   'haiku',    // read + parse tasks.json's task list — a fixed procedure
  derive:      'opus',     // D16 fallback: author a fresh tasks.json from tasks.md's step list — real judgment, mirrors sdlc-block.js's ensureTasks() generator
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
const ESCALATION_MODEL = 'opus'

// Merge an optional model override into an agent's opts (omits the key when undefined, so the agent
// inherits the session model rather than receiving model: undefined).
function withModel(base, model) {
  return model ? { ...base, model } : base
}

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

// Fold a stage's self-reported `filesReadKb` into the metrics entry the wrapper just pushed.
// Safe to call immediately after the awaited tracedAgent call — that entry is always metrics[last].
function recordFilesRead(result) {
  if (result && result.filesReadKb != null && metrics.length) {
    metrics[metrics.length - 1].filesReadKb = result.filesReadKb
  }
}

// Build the canonical `tokens` block from the accumulated per-agent metrics (Block A — the shared
// committed-state token contract, identical across all four engines): per-stage output tokens + the
// D15 input-cost estimate (promptTok + filesReadKb→tokens at ~256 tok/KB) + a cumulative total.
//
// CONTRACT SCOPE (Phase 0 /code-review carry-in): `metrics` — and therefore `tokens.total` — cover the
// SUBSTANTIVE stages only. Cheap helper / state-writer agents (the Haiku state-writer, config + baseline
// loaders) deliberately use bare agent() and are EXCLUDED; this bounded, Haiku-cheap exclusion is the
// same boundary in all four engines, named here so it is explicit rather than silent — it keeps the
// two-level /sdlc-block roll-up summing comparable substantive-stage totals at both levels.
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
        }
      }
    },
    notes: { type: 'string' }
  }
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
    these fields when present: stack; validation.checks[] (each: {kind, name, command, purpose, gates,
    perTask, fastCommand} plus any kind-specific fields present — baselineCommand, reasonCommand,
    compareKeys[], countPattern, failOn, warningPatterns[], rules[] ({id, pattern, paths,
    allowlistPattern})). Preserve kind-specific fields verbatim; ignore any other fields.

Return your findings using the StructuredOutput tool.
`, { label: 'harness-config', schema: HARNESS_CONFIG_SCHEMA, model: 'sonnet' })

  if (!result || !result.present || !result.config) return null
  return result.config
}

// Pure delta-evaluation for the skip-count-regression kind: fail ONLY when currentCount exceeds
// baselineCount (coverage silently switched off), never on a nonzero absolute count. Kept as a
// standalone pure function (no I/O) — exercised directly in unit tests without running a suite —
// and mirrored verbatim into the rendered shell snippet's comparison so the two never drift.
function skipCountRegressionResult(baselineCount, currentCount, dominantReason) {
  const regressed = currentCount > baselineCount
  const delta = currentCount - baselineCount
  const message = regressed
    ? `SKIP COUNT REGRESSED: baseline=${baselineCount} current=${currentCount} (rose by ${delta})${dominantReason ? ` — dominant reason: ${dominantReason}` : ''}`
    : `skip count did not rise (baseline=${baselineCount}, current=${currentCount})`
  return { regressed, message }
}

// Hardcoded, project-agnostic parse-time safety gate (mechanism, not policy — see CLAUDE.md standing
// rule 1). Independent of harness.json/spec checks: any .claude/workflows/ file this task's own
// tasks.json `files[]` names gets an unconditional `node --check`, in BOTH the fast-tripwire and
// full-suite render paths, even when the project ships no harness.json at all. No-op (renders '')
// when the task touches no such file — never emits a check with no target.
function renderEngineParseChecks(files, cd, startIndex) {
  if (!files || !files.length) return ''
  return files.map((f, i) => {
    const n = startIndex + i
    return `CHECK ${n} — engine-parse-safety (hardcoded parse-time gate on modified SDLC engine file — mechanism, unconditional on harness.json) [GATING — a failure here blocks the verdict]:
  ${cd}node --check ${f}
  echo "CHECK${n}_EXIT:$?"`
  }).join('\n\n')
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
  tasks_run: [],
  tasks: {},        // "N": { status, attempts, summary, issues, fixes, decisions, files_changed, commit, validated }
  bail_reason: null,
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
  const stepTwoText = firstWrite
    ? `STEP 2 — write ${stateFile} with EXACTLY this JSON, but inserting two extra top-level keys
  "started_at" (preserved or NOW, per STEP 1) and "updated_at" (NOW) right after "branch". Valid JSON only
  (double quotes, no trailing commas, no markdown fences). The object to write (verbatim except for
  adding those two timestamp keys):
${stateJson}`
    : `STEP 2 — write ${stateFile} with EXACTLY this JSON, but inserting exactly one extra top-level
  key: "updated_at" (NOW), right after "started_at" (already present in the object below,
  immediately after "branch" — it was set from the value given in STEP 1). Valid JSON only
  (double quotes, no trailing commas, no markdown fences). The object to write (verbatim except for
  adding that one timestamp key):
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

const setupResult = await tracedAgent(`
You are the setup agent for the lean /sdlc-task pipeline. ${useWorktree
  ? 'Create (or locate) ONE isolated git worktree for this run.'
  : 'The pipeline runs IN PLACE on the current branch — do NOT create a worktree.'} All bash commands run
from the MAIN REPO ROOT (your current CWD).

Target:
  Spec:       ${blockId}
  Spec file:  ${specFile}
${useWorktree ? `  Base name:  ${baseBranchName}` : ''}

STEP 1 — Get the absolute repo root and the current branch:
  Run: git rev-parse --show-toplevel        (store trimmed output as repoRoot)
  Run: git rev-parse --abbrev-ref HEAD       (store as currentBranch)
${useWorktree ? `
WORKTREE MODE (--worktree) — create or reuse an isolated worktree:
${resumeMode ? `  RESUME — reuse the existing worktree for this spec if present:
    a. git worktree list | grep "trees/${baseBranchName}" && echo "WT_EXISTS" || echo "WT_MISSING"
    b. git branch --list "${baseBranchName}"
    - WT_EXISTS → REUSE verbatim. branchName="${baseBranchName}", wasCreated=false. Skip to STEP 2c.
    - WT_MISSING but branch "${baseBranchName}" exists (orphan branch, dir removed) → re-attach (NO -b flag):
        mkdir -p trees
        git worktree add --no-checkout trees/${baseBranchName} ${baseBranchName}
        git -C trees/${baseBranchName} sparse-checkout init --cone
        git -C trees/${baseBranchName} sparse-checkout set $(git ls-tree HEAD --name-only -d | tr '\\n' ' ')
        git -C trees/${baseBranchName} checkout
        git ls-files --others --ignored --exclude-standard -- . | grep -E '(^|/)\\.env(\\.[^/]*)?$' | grep -Ev '(^|/)(node_modules|\\.venv|venv|trees|vendor)/' | while IFS= read -r f; do dest="trees/${baseBranchName}/$f"; if [ ! -f "$dest" ]; then mkdir -p "$(dirname "$dest")"; cp "$f" "$dest"; echo "ENV_COPIED: $f"; fi; done
      branchName="${baseBranchName}", wasCreated=false. Skip to STEP 2c.
    - Neither exists → fall through and create a fresh worktree as normal.
` : ''}  STEP 2 — Find a free worktree name. Start with candidate "${baseBranchName}"; for each candidate run:
      git worktree list | grep "trees/<candidate>"
      git branch --list "<candidate>"
    If BOTH return nothing → the candidate is free; use it. Otherwise try "${baseBranchName}-2",
    "${baseBranchName}-3", … up to "-10". Store the chosen name as branchName.

  STEP 2b — Create the worktree (replace [branchName] with the chosen name):
    a. mkdir -p trees
    b. git worktree add --no-checkout trees/[branchName] -b [branchName]
    c. git -C trees/[branchName] sparse-checkout init --cone
    d. # Cone ALL tracked top-level directories — stack-agnostic, no project-layout assumptions (D5/P5).
       git -C trees/[branchName] sparse-checkout set $(git ls-tree HEAD --name-only -d | tr '\\n' ' ')
    e. git -C trees/[branchName] checkout
    f. Discover and copy EVERY gitignored env-shaped file (.env, .env.local, .env.* in any
       directory) from repoRoot into trees/[branchName], preserving each file's path relative to
       the repo root (creating parent directories as needed — so app/.env lands at
       trees/[branchName]/app/.env). Only files git actually ignores; exclude node_modules/,
       .venv/, venv/, trees/, and vendor/; never overwrite a file that already exists in the
       worktree. Run:
         git ls-files --others --ignored --exclude-standard -- . | grep -E '(^|/)\.env(\.[^/]*)?$' | grep -Ev '(^|/)(node_modules|\.venv|venv|trees|vendor)/' | while IFS= read -r f; do dest="trees/[branchName]/$f"; if [ ! -f "$dest" ]; then mkdir -p "$(dirname "$dest")"; cp "$f" "$dest"; echo "ENV_COPIED: $f"; fi; done
       Record the list of "ENV_COPIED:" lines — report them in STEP 4.
    g. git -C trees/[branchName] commit --allow-empty -m "chore: init worktree [branchName]"
    Set wasCreated=true.

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
` : `
IN-PLACE MODE — no worktree. branchName=currentBranch, wasCreated=false. runDir=repoRoot.
`}
STEP 3 — Compute runDir:
  ${useWorktree ? 'runDir = repoRoot + "/trees/" + branchName' : 'runDir = repoRoot'}

STEP 4 — Report pipeline-start inputs (run these from runDir):
  a. Spec file:
       cd <runDir> && ls ${specFile} 2>/dev/null && echo "SPEC_EXISTS" || echo "SPEC_MISSING"
     specFileExists = true iff "SPEC_EXISTS" printed.
  b. Block status — find this spec's row in status.md:
       cd <runDir> && grep -iE "${blockId}" planning/status.md | head -5
     blockStatus = the title-case Status value (Not started / In progress / Done / Blocked / Skipped),
     or "Unknown" if no row is found.
  c. Thin-spec check (D19) — evaluate ONLY when specFileExists AND this is NOT a resume run (a fresh run
     about to spend implement tokens). Set specThin=true ONLY on these high-confidence signals (a blocked
     valid spec is far costlier than a missed thin one — when in doubt do NOT flag):
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
  cd <runDir> && git rev-parse --short HEAD     (store as baseSha)

Return your result using the StructuredOutput tool:
  runDir, branchName, baseSha, wasCreated, specFileExists, blockStatus, specThin, thinReason,${useWorktree ? ' envFilesCopied,' : ''} notes.
`, withModel({ label: 'setup', schema: SETUP_SCHEMA, phase: 'Setup' }, MODEL.setup))

if (!setupResult) {
  log('Setup agent returned null — aborting pipeline')
  return { error: 'Setup failed', blockId }
}
const { runDir, branchName, baseSha } = setupResult
state.branch = branchName
state.worktree_path = useWorktree ? runDir : ''
log(`Run root: ${runDir} | branch: ${branchName} | base: ${baseSha}`)
if (useWorktree) {
  const envFilesCopied = setupResult.envFilesCopied || []
  log(envFilesCopied.length
    ? `Env files copied into worktree: ${envFilesCopied.join(', ')}`
    : 'Env files copied into worktree: none found')
  log(`Worktree path derives from the spec slug (trees/${branchName}), not any block ID — use "git worktree list" to locate it, never guess.`)
}

if (!setupResult.specFileExists) {
  log(`Spec file ${specFile} not found. /sdlc-task expects an authored spec.`)
  log(`Fix: run /generate-tasks ${blockId} (and /breakdown) on main, commit, then re-run /sdlc-task ${blockId}.`)
  return { error: 'Missing spec', blockId, specFile }
}

// D19 — thin-spec guard for a fresh run.
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

Return via StructuredOutput: hasTasks, allTasks (integers in order), taskChecks, engineFiles, notes.
`

let enumResult = await tracedAgent(ENUMERATE_PROMPT, withModel({ label: 'enumerate', schema: ENUMERATE_SCHEMA, phase: 'Plan' }, MODEL.enumerate))

if (!enumResult || !enumResult.hasTasks || !(enumResult.allTasks || []).length) {
  // D16 derive-from-tasks.md fallback — before refusing, check whether the spec's authored
  // tasks.md carries a derivable step decomposition. Mirrors sdlc-block.js's ensureTasks()
  // generator and /generate-tasks' --from mode: author a FRESH decomposition from tasks.md (never
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

STEP 4 — Commit it on the current branch with an explicit pathspec:
  git add ${tasksJsonFile}
  git commit -m "chore: derive tasks.json from tasks.md (D16 fallback)"
  git log --oneline -1   (capture the short hash)

Return via StructuredOutput: derivable, written, commitHash, taskCount, notes.
`, withModel({ label: 'derive-tasks-json', schema: DERIVE_SCHEMA, phase: 'Plan' }, MODEL.derive))

  if (deriveResult?.derivable && deriveResult?.written) {
    log(`Derived tasks.json from tasks.md (D16 derive-from-tasks.md fallback) — ${deriveResult.taskCount || '?'} task(s), commit ${deriveResult.commitHash || 'unknown'}.`)
    enumResult = await tracedAgent(ENUMERATE_PROMPT, withModel({ label: 'enumerate-post-derive', schema: ENUMERATE_SCHEMA, phase: 'Plan' }, MODEL.enumerate))
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
const taskCheckMap = new Map(
  (enumResult.taskChecks || [])
    .filter(tc => tc && Number.isInteger(tc.taskId) && Array.isArray(tc.validationCommands) && tc.validationCommands.length)
    .map(tc => [tc.taskId, tc.validationCommands])
)
function taskCommandsFor(taskNum) { return taskCheckMap.get(taskNum) || null }
if (taskCheckMap.size) {
  log(`Per-task validation overrides (tasks.json validation_commands): ${[...taskCheckMap.keys()].sort((a, b) => a - b).join(', ')} — these tasks skip the project-wide harness tripwire.`)
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

// Resume: load the committed state.json to skip already-passed tasks.
const passedFromState = new Set()
if (resumeMode) {
  const loaded = await tracedAgent(`${W}
You read the COMMITTED run-state for an /sdlc-task resume. Do NOT modify anything.
  cd ${runDir} && cat ${stateFile} 2>/dev/null || echo "__NO_STATE__"
If "__NO_STATE__" or invalid JSON → exists=false. Otherwise exists=true, startedAt = its started_at,
passedTasks = the task numbers whose tasks[N].status == "passed", bailReason = its bail_reason or "".
Return via StructuredOutput.
`, withModel({ label: 'state-load', schema: STATE_LOAD_SCHEMA, phase: 'Plan' }, MODEL.stateLoad))
  if (loaded && loaded.exists) {
    for (const n of (loaded.passedTasks || [])) passedFromState.add(n)
    log(`Resume: ${passedFromState.size} task(s) already passed (${[...passedFromState].sort((a, b) => a - b).join(', ') || 'none'}); skipping them.`)
  } else {
    log('Resume requested but no valid state.json found — running all selected tasks fresh.')
  }
}

// Load the project's validation policy once (from the run root). null → fall back to the spec.
const harnessCfg = await loadHarnessConfig(runDir)
log(harnessCfg
  ? `Harness config: ${(harnessCfg.validation?.checks || []).length} check(s).`
  : 'No planning/harness.json — validation falls back to the spec.')

// Resolve test depth: CLI flag overrides the built-in 'fast' default.
const testDepth = testDepthFlag || 'fast'
log(`Policy: testDepth=${testDepth}`)

// Snapshot baselines once (resume-safe; no-op without baseline-diff checks).
await snapshotBaselines(harnessCfg, runDir)

// The immediate-bail reason set the triage agent enforces. "When unsure, prefer bail."
const BAIL_REASONS = [
  'Missing/undefined upstream dependency or symbol the spec assumes exists.',
  'Spec ambiguity/contradiction — intended behavior is genuinely undeterminable.',
  'Environment/credential/auth/network failure (not a code defect).',
  'Change would require a destructive or out-of-scope action.',
  'Same failure twice with no progress (stuck), or a structural design flaw needing a re-plan.',
].map((r, i) => `  ${i + 1}. ${r}`).join('\n')

// ----------------------------------------------------------------
// Test stage helper — gatingOnly=true → fast tripwire (gating checks); false → full suite.
// ----------------------------------------------------------------
// Render a per-task validation override (tasks.json `validation_commands`) in the same shape
// renderCheckList emits, so the test agent's instructions are identical either way.
function renderTaskCheckList(commands, cwd) {
  const cd = cwd ? `cd ${cwd} && ` : ''
  return commands.map((cmd, i) => {
    const n = i + 1
    return `CHECK ${n} — task_validation_${n} (per-task validation_commands override from tasks.json) [GATING — a failure here blocks the verdict]:
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
  keys "started_at" (preserved or NOW, per STEP W1) and "updated_at" (NOW) right after "branch", and
  (b) replacing the literal placeholder string "__BAIL_REASON__" (the top-level "bail_reason" field)
  with the effective bail reason computed above. Valid JSON only (double quotes, no trailing commas,
  no markdown fences). The object to write (verbatim except for those substitutions):
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
  snapshot.tokens = buildTokensBlock()
  return { stateFile, stateJson: JSON.stringify(snapshot, null, 2), majorFallback, exhaustionFallback }
}

async function runTests(label, { gatingOnly, taskCommands = null, onPass = null, engineFiles = [] }) {
  const usingOverride = Array.isArray(taskCommands) && taskCommands.length > 0
  return tracedAgent(`${W}
You are the test agent for the lean /sdlc-task pipeline. Run the project's validation checks and report.

IMPORTANT — run ONLY the checks enumerated below (${usingOverride
    ? 'this task declares its OWN validation_commands in tasks.json, which REPLACE the project-wide harness checks for this task'
    : 'from planning/harness.json + the spec'}). Do NOT invent
checks. All Bash calls run from the run root (prefix each with: cd ${runDir} &&).

${usingOverride
    ? renderTaskCheckList(taskCommands, runDir)
    : renderCheckList(harnessCfg, { gatingOnly, cwd: runDir, engineFiles })}

Then run the universal emoji gate (a harness rule, always) — DIFF-SCOPED: it judges only lines
ADDED by this task, never a whole changed file, so a legacy file's pre-existing emoji does not fail
a diff that never touched it:
  cd ${runDir} && python3 - <<'PYEOF'
import subprocess, re, sys
EMOJI = re.compile(r'[\\U0001F300-\\U0001FAFF\\U00002600-\\U000027BF]')
FOOTER = 'Generated with Claude Code'
diff = subprocess.run(['git','diff','-M','-U0','${baseSha}..HEAD','--','*.md','*.mdx'], capture_output=True, text=True).stdout.splitlines()
hits = []
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
PYEOF
  A stray emoji ADDED in docs FAILS this gate; a pre-existing emoji in a file this task did not
  touch a line of does not.

For each check record: name, passed (true iff exit code 0), the command, and failure output.
${onPass ? renderOnPassStateWriteRecipe(onPass) : ''}
Return via StructuredOutput: allPassed (true only if EVERY gating check passed and the emoji gate is
clean), passCount, failCount, failedTests (names), failBlob (compact: failing check names + the tail of
their output; empty when allPassed)${onPass ? ', stateWritten (true only if you performed the additional state write above)' : ''}.
`, withModel({ label, schema: TEST_SCHEMA, phase: 'Tasks' }, MODEL.test))
}

// ----------------------------------------------------------------
// Triage helper — classify a failure RETRYABLE vs MAJOR.
// ----------------------------------------------------------------
async function triage(context, attempt, maxAttempts, failBlob, sameContext, onBail = null) {
  return tracedAgent(`
You are the failure-triage agent for an /sdlc-task run. Classify a failure so the pipeline either makes
a bounded fix or bails to a human NOW. Bailing is cheap; a wasted retry loop is not — when unsure, BAIL.

Context: ${context} (attempt ${attempt} of ${maxAttempts}).
Failure detail:
${failBlob || '(no detail captured)'}

IMMEDIATE-BAIL reasons — if the failure is ANY of these, class=MAJOR and put a short human-readable
bailReason describing which one and where:
${BAIL_REASONS}

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

${onBail ? renderBailStateWriteRecipe(onBail, attempt, maxAttempts) : ''}
Return via StructuredOutput: class, reason, bailReason (empty when RETRYABLE), sameFailureAsBefore,
evidence (what was actually OBSERVED, quoting output — no causal claims), baseStateChecked (true only
if the failing check was actually re-run against the base state)${onBail ? ', stateWritten (true only if you performed the additional state write above)' : ''}.
${sameContext ? `(Previous attempt context for the same-failure check: ${sameContext})` : ''}
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
    const stageResult = await tracedAgent(`${W}
You are the ${isFix ? 'fix' : 'implementation'} agent for the lean /sdlc-task pipeline. You run IN PLACE on
the branch (sequential — earlier tasks in this spec are already committed on this branch). Work ONLY on
Task ${taskNum} of this spec.

Target:
  Spec:        ${blockId}
  Task:        Task ${taskNum} only
  Spec file:   ${specFile} (prose — Goal, Acceptance Criteria, Validation Commands)
  Tasks file:  ${tasksJsonFile} (the task list — find the entry with "task_id": ${taskNum})

1. Read CLAUDE.md and planning/context.md — internalize the project's standing rules (CLAUDE.md is the
   authority; assume no stack/locale/narrative/content rule unless written there). Universal harness
   rules always apply: no fabricated metrics or quotes, no emoji, every change ships with tests.
   Run: cd ${runDir} && cat CLAUDE.md

2. Read the spec and the task list:
   Run: cd ${runDir} && cat ${specFile} ${tasksJsonFile}
   tasks.json is a bare array — find the object whose "task_id" is ${taskNum}. Its "title",
   "description", and "files" define exactly what this task is.
   ${isFix ? `Do NOT re-implement from scratch. Make the MINIMUM targeted changes to address THIS failure:
   ${prevFailBlob ? 'Failing checks/output from the last test run:\n' + prevFailBlob.split('\n').map(l => '     ' + l).join('\n') : ''}` : `Implement ONLY task id ${taskNum} — do NOT implement other tasks.`}

2.5. Optional breakdown (more granular sub-steps from /breakdown):
   Run: cd ${runDir} && ls ${breakdownFile} 2>/dev/null && echo "BREAKDOWN_EXISTS" || echo "NO_BREAKDOWN"
   If BREAKDOWN_EXISTS: read ${breakdownFile}, find "### Step ${taskNum}:", and use its atomic sub-steps as
   the execution guide (run each inline "Verify:" checkpoint). tasks.json stays authoritative for scope.

3. Execute methodically with Read/Edit/Write/Bash (all paths resolve from the run root).

4. Follow every CLAUDE.md standing rule; add/update tests for new code/logic; verify any model ids /
   package names via the claude-api skill — never from memory.

5. COMPLETENESS SELF-CHECK before committing (D8): no stub/placeholder on any path the task's acceptance
   criteria require (no \`todo!()\`/\`unimplemented!()\`/\`unreachable!()\`, \`raise NotImplementedError\`,
   \`throw new Error('not implemented')\`, empty \`pass\`-only bodies, or \`TODO\`/\`FIXME\` in required
   paths); every deliverable named for Task ${taskNum} exists; any "unit-tested" criterion has a real,
   hermetic test. Sanity-grep ONLY the files the in-scope criteria require:
     cd ${runDir} && grep -nE 'todo!\\(|unimplemented!\\(|unreachable!\\(|NotImplementedError|not implemented|FIXME' <those paths> 2>/dev/null
   If something required is incomplete, finish it now — do not commit a partial task.

6. Run the spec's "## Validation Commands" for Task ${taskNum} to confirm correctness.

7. Commit on the branch. Never use git add -A or git add . — stage files explicitly by name.
   Run: cd ${runDir} && git status
   Stage your changed source/test files explicitly, then commit using HEREDOC:
     cd ${runDir} && git commit -m "$(cat <<'EOF'
${isFix ? `fix: fix pass ${attempt - 1} for ${stem}` : `feat: implement ${stem}`}
EOF
)"
   Run: cd ${runDir} && git log --oneline -1   (capture the short hash)
${vault.vaulted ? `
7b. planning/ is a vaulted symlink (D46) — its bytes live at ${vault.planningPath}, a DIFFERENT git
    repo, invisible to the commit you just made in step 7. If this attempt created or edited ANY file
    under planning/ (i.e. it belongs in filesModified with a "planning/" prefix), you MUST ALSO stage
    and commit it there, through the real path — derive the exact set from what you actually wrote,
    never a fixed list of filenames. NEVER git add -A, git add ., git reset, or git stash against the
    vault repo — another lane's session may have unrelated work staged there right now; touch ONLY
    your own paths, and do not checkout/switch/branch inside it (stay on whatever branch it is
    already on). For each such file, let <relpath> be the part of its path AFTER "planning/":
      cd ${runDir} && git -C ${vault.planningPath} add ${vault.planningPath}/<relpath>
    Then, once every such path is staged, commit ONLY those paths — pass them explicitly to \`git commit\`
    itself (not merely to \`git add\`), so a sibling lane's unrelated pre-staged files are never swept
    into this commit even if they happen to already be staged:
      cd ${runDir} && git -C ${vault.planningPath} diff --cached --quiet -- <relpath1> <relpath2> ... || git -C ${vault.planningPath} commit -m "$(cat <<'EOF'
${isFix ? `fix: fix pass ${attempt - 1} for ${stem} (vault)` : `feat: implement ${stem} (vault)`}
EOF
)" -- <relpath1> <relpath2> ...
      cd ${runDir} && git -C ${vault.planningPath} log --oneline -1
    If NOTHING you wrote this attempt lives under planning/, skip this step entirely — do not run any
    vault command. If a vault add/commit fails, report it PLAINLY in notes; never paper over it, and
    never "repair" it by committing on a different branch inside the vault.
` : ''}
Return via StructuredOutput:
  success: true if the work completed and the spec validation passed
  filesModified: every file you created or modified this attempt — including any under planning/
    (do NOT omit vault-side files just because they commit through a different repo)
  commitHash: the 7-char short hash of THIS repo's commit (empty string if no commit was made here)
  summary: one line — what this task now does
  decisions: any non-obvious choices (empty array if none)
  filesReadKb: telemetry — before returning, sum the byte size of every file you cat/Read this attempt
    (cd ${runDir} && wc -c <each file>), divide the total by 1024, and report the number.
  notes: one-line status${vault.vaulted ? ' — mention explicitly whether a vault commit (step 7b) happened and, if so, its outcome' : ''}
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
    if (stageResult.commit) t.commit = stageResult.commit
    if (stageResult.summary) t.summary = stageResult.summary
    if (Array.isArray(stageResult.filesModified)) t.files_changed = [...new Set([...(t.files_changed || []), ...stageResult.filesModified])]
    if (Array.isArray(stageResult.decisions) && stageResult.decisions.length) t.decisions = [...(t.decisions || []), ...stageResult.decisions]

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
    // `validation_commands` in tasks.json runs THOSE instead.
    const passValidatedLabel = taskCommandsFor(taskNum)
      ? 'per-task validation_commands (tasks.json override)'
      : (testDepth === 'fast' ? 'gating checks (fast tripwire)' : 'full gating suite')
    const passPayload = buildPassPayload(taskNum, t, passValidatedLabel)
    const testResult = await runTests(`test-${taskNum}-${attempt}`, { gatingOnly: testDepth === 'fast', taskCommands: taskCommandsFor(taskNum), onPass: passPayload, engineFiles: engineFilesFor(taskNum) })
    if (testResult && testResult.allPassed) {
      t.validated = passValidatedLabel
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
  if (bailed && !taskPassed) { state.status = 'blocked'; state.bail_reason = bailReason }
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
const fullRun = !selectedTasks   // no explicit selection = every task in the spec ran

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
if (reconcileFailed) state.bail_reason = `Terminal reconcile failed (D56): ${reconcileFailBlob}`

// ----------------------------------------------------------------
// LEAN BOOKKEEP CLOSE-OUT — the one bit of authored state the lean engine still owes.
// Not a full wrap-up: no prose log.md entry, no D18 amendment log, no review/docs/PR (run /log-work
// for the narrative). It only flips the AUTHORED markers a passing run leaves stale — tasks.md task
// status, the status.md Progress row, and the state.json block status — then (in place, on main only)
// regenerates the derived surfaces via `mev emit-state --write`. Mirrors /start-block's flip pattern.
// Skipped entirely on a bail or a reconcile_failed (the block is not done) and on a partial task
// selection (can't close the block).
// ----------------------------------------------------------------
const blockDone = !bailed && !reconcileFailed && fullRun && passedTasks.length === taskList.length
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

Target:
  Spec:        ${blockId}
  Tasks run:   ${taskList.join(', ')}  (passed: ${passedTasks.join(', ') || 'none'})
  Full spec run: ${fullRun ? 'yes (every task in the spec)' : 'no (a task subset — do NOT close the block)'}
  Block done:  ${blockDone ? 'yes — the whole spec is complete this run' : 'no — keep the block open/in-progress'}

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

3. Update planning/status.md (Edit tool, surgical). "Current focus" is APPEND-ONLY narrative — never
   delete or rewrite any existing line under it; a prior block's narrative must survive this edit
   VERBATIM. The one exception: if an existing line already refers to THIS spec ("${blockId}") by name
   (e.g. from an earlier partial run), you may replace only that one line — never the whole section —
   with the update below.
   ${blockDone
     ? `- The full spec "${blockId}" is done — flip its Status to "Done" in the Progress Table.
   - Add ONE new line under "Current focus" recording that "${blockId}" is done, citing the CUMULATIVE
     task count you derived in step 2 (e.g. "${blockId}: done (N of ${allTasks.length} tasks)") — do
     not touch any other existing line.`
     : `- Keep the spec "In progress" (a task subset ran). Add ONE new line under "Current focus"
   pointing at the next task if helpful, citing the cumulative count from step 2 — do not touch any
   other existing line.`}
   - Update "Last updated" — run: date +%Y-%m-%d

4. Flip the block's AUTHORED status in planning/state.json (skip this entire step silently if the repo
   has no planning/state.json, OR if "Block done" above is "no"). state.json is the authoritative block
   graph — leaving it stale poisons every derived surface, because \`mev emit-state\` reads this field
   and NEVER infers completion from status.md.
   - Resolve the block's canonical ID from the status.md Progress Table row (the <BlockID> column, or
     the id that row maps to in state.json). This is the only part of this step that stays your
     judgment call — the mutation itself is scripted below, not an Edit-tool diff.
   - Run ONE scripted mutation (never the Edit tool) to perform the write — substitute the id you
     resolved for <RESOLVED_ID> (keep it as the script's sole argv, quoted):
     cd ${runDir} && python3 -c "
import json, sys
path = 'planning/state.json'
bid = sys.argv[1]
data = json.load(open(path))
found = False
for track in data.get('tracks', []):
    for block in track.get('blocks', []):
        if block.get('id') == bid:
            block['status'] = 'closed'
            found = True
            break
    if found:
        break
if found:
    with open(path, 'w') as fh:
        json.dump(data, fh, indent=2, ensure_ascii=False)
        fh.write(chr(10))
    print('FLIPPED:' + bid)
else:
    print('NOT_FOUND')
" "<RESOLVED_ID>"
     The script searches EVERY tracks[].blocks[] entry and only ever mutates the one matching block's
     "status" field; on a miss it prints NOT_FOUND and never opens the file for writing, so it stays
     byte-unchanged. Read the script's own stdout — do not infer success yourself: on "FLIPPED:<id>"
     set blockStatusFlipped to that id; on "NOT_FOUND" report it in notes, do NOT fabricate a block
     entry, and set blockStatusFlipped to "".
   - Validate: cd ${runDir} && python3 -c "import json;json.load(open('planning/state.json'))"

5. Regenerate derived surfaces via \`mev emit-state --write\`. Run this step whenever this bookkeep
   stage runs at all — it is NOT conditional on "Block done" above: step 2/3 already edited
   ${specFile}/planning/status.md regardless of whether the block closed this run, so the derived
   surfaces (status.md rollups, /attention boards, wave tables) need resyncing every time, not only on
   a full block close.
   ${useWorktree
     ? `- Do NOT run \`mev emit-state --write\`: this is a linked git worktree, where emit-state refuses to run. The derived surfaces regenerate on MAIN when the branch merges (/clean-worktree or /merge-train). Set emitStateRan=false.`
     : `- This run is IN PLACE on main, so emit-state is safe: cd ${runDir} && mev emit-state --write . If \`mev\` or brain.toml is absent (standalone repo), skip it silently and set emitStateRan=false; else emitStateRan=true. Do NOT hand-reimplement focus/rollup derivation.`}

6. Commit your edits (stage explicitly — never git add -A). NEVER run git checkout, git switch, or git
   branch outside this repo's own root (${runDir})${vault.vaulted ? ` or the vault's own root (${vault.planningPath})` : ''} —
   if a git add fails, report the failure in notes; do not relocate the commit to make it succeed.
${vault.vaulted ? `
   planning/ is a vaulted symlink (D46) — its bytes live at ${vault.planningPath}, a different repo. Every
   file this step touches (the spec, status.md, state.json) lives under planning/, so stage + commit them
   ALL there, via \`git -C\`, on whatever branch that repo is already on. Do NOT cd into it and do NOT
   checkout/switch/branch there:
   cd ${runDir} && git -C ${vault.planningPath} add ${vault.planningPath}/${blockId}/tasks.md 2>/dev/null || true
   cd ${runDir} && git -C ${vault.planningPath} add ${vault.planningPath}/status.md
   cd ${runDir} && git -C ${vault.planningPath} add ${vault.planningPath}/state.json 2>/dev/null || true
   Then commit ONLY these three paths — pass them explicitly to \`git commit\` itself (not merely to
   \`git add\`), so anything a sibling lane already had staged in this same vault repo is left staged
   and untouched by this commit:
   cd ${runDir} && git -C ${vault.planningPath} diff --cached --quiet -- ${vault.planningPath}/${blockId}/tasks.md ${vault.planningPath}/status.md ${vault.planningPath}/state.json || git -C ${vault.planningPath} commit -m "$(cat <<'EOF'
chore: sdlc-task bookkeep — ${blockId}
EOF
)" -- ${vault.planningPath}/${blockId}/tasks.md ${vault.planningPath}/status.md ${vault.planningPath}/state.json
   cd ${runDir} && git -C ${vault.planningPath} log --oneline -1` : `
   planning/ is a plain directory here (not vaulted) — everything commits together as before:
   cd ${runDir} && git add ${specFile} planning/status.md
   cd ${runDir} && git add planning/state.json 2>/dev/null || true
   cd ${runDir} && git commit -m "$(cat <<'EOF'
chore: sdlc-task bookkeep — ${blockId}
EOF
)" || echo "NOTHING_TO_COMMIT"
   cd ${runDir} && git log --oneline -1`}

Return via StructuredOutput: statusUpdated, tasksMarked, blockStatusFlipped, emitStateRan, commitHash, notes.
`, withModel({ label: 'bookkeep', schema: BOOKKEEP_SCHEMA }, MODEL.bookkeep))
  if (bookkeepResult?.blockStatusFlipped) {
    log(`state.json: block "${bookkeepResult.blockStatusFlipped}" → closed${bookkeepResult.emitStateRan ? '; derived surfaces (incl. focus.next) regenerated (mev emit-state --write).' : useWorktree ? '; focus.next is DEFERRED — it still points at the pre-close state until /clean-worktree or /merge-train runs `mev emit-state --write` on merge.' : '.'}`)
  } else if (blockDone) {
    log(`Bookkeep: no state.json block flipped (${bookkeepResult?.notes || 'no state.json, or block not found'}).`)
  }
}

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
  bailReason: bailReason || (reconcileFailed ? state.bail_reason : null),
  tasksRun: taskList,
  tasksPassed: passedTasks,
  stateFile,
  tokens: tokensBlock,
}
