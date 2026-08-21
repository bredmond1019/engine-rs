// =============================================================================
// sdlc-flow — single-branch, single-review, PR-terminating SDLC engine
// =============================================================================
//
// The default engine for non-trivial feature work. Runs one spec's tasks
// SEQUENTIALLY on a SINGLE shared branch (so there are no inter-task merges to
// conflict), with a per-task test→fix loop, ONE
// consolidated review at the end, a docs patch, and a PR as the terminal step.
//
// ISOLATION MODE
//   Default: a plain branch (<spec>-flow) checked out IN THE MAIN WORKING TREE. No
//   sparse-checkout worktree, so a relative planning/ symlink (brain-vaulted repos)
//   stays intact. main is left on the branch until the PR merges.
//   --worktree: the isolated sparse-checkout worktree under trees/<spec>-flow/ —
//   opt in when you need true isolation.
//
// A compact, COMMITTED, AUTHORITATIVE state.json + one worklog.md replace the 5×N
// per-stage report files: resume + review + wrap-up read a structured index instead
// of re-reading verbose prose. This inverts the harness's usual "committed report
// files are authoritative, state JSON is gitignored" rule on purpose (see D31).
//
// USAGE
//   /sdlc-flow <spec-slug>                  run every task in the spec, open a PR, stop
//   /sdlc-flow <spec-slug> 1-3              scope to a task range (1-3, 1,3,5, 5)
//   /sdlc-flow <spec-slug> --auto-merge     merge the PR + clean up on success
//   /sdlc-flow <spec-slug> --no-pr          stop after wrap-up; do not create a PR
//   /sdlc-flow <spec-slug> --worktree       run in an isolated worktree (default: plain branch)
//   /sdlc-flow <spec-slug> --resume         re-attach the branch/worktree, resume from state.json
//   /sdlc-flow <spec-slug> --test-depth full  run the FULL gating suite per task (default: fast)
//
// PIPELINE
//   worktree-setup → enumerate (D16 lint) → [resume load] → per-task loop
//     → end-review → docs (gated on PASS) → wrap-up(PR)
//
//   Per-task loop (sequential, on the one branch):
//     implement → fast-test → (triage → fix/​bail) ×≤3
//     One state-commit per task. A triage MAJOR / immediate-bail reason breaks
//     straight to wrap-up (draft PR) — it does NOT burn three attempts.
//
//   End-review: ONE review over the integrated tree, fed state.json as the index but
//   reading `git diff <prBase>..HEAD` + tasks.md criteria directly + re-running the
//   FULL gating suite (authoritative). PASS → docs; FAIL/PARTIAL → triage findings:
//   small/localized → bounded fix→test→review (≤2, Opus last); broad → bail.
//
// COMMIT STRATEGY (crash recovery — everything lands on the branch)
//   feat: implement <stem> task N      implement agent (per task)
//   fix:  fix pass P for <stem> task N  fix agent (per pass)
//   chore: flow state — <label>         state-writer (state.json + worklog.md + checkbox)
//   docs: update docs for <spec>        docs agent
//   chore: wrap up <spec>               wrap-up agent (status/log/amendment-log)
//
// MODEL TIERING (the token lever — see the MODEL map below)
//   haiku : setup, enumerate, scout/state-load, test, state-writer
//   sonnet: implement, fix, review, triage, docs, wrap-up
//   opus  : ESCALATION on the FINAL per-task fix pass and the FINAL review attempt
//
// STATE  (committed — NOT gitignored — at planning/<spec>/sdlc/)
//   sdlc-flow-state.json   the authoritative run index (per-task summary/issues/fixes/commit)
//   worklog.md             the human-readable trail — one short section per task
// =============================================================================

export const meta = {
  name: 'sdlc-flow',
  description: 'Run a spec sequentially on one branch (or --worktree) with a per-task test→fix loop, one end review, a docs patch, and a PR',
  whenToUse: 'The default for non-trivial feature work — many moving parts in one spec. Sequential, no inter-task merges, one consolidated review, terminates in a PR. Runs on a plain branch in the main tree by default; pass --worktree for isolation. Usage: /sdlc-flow <spec-slug> [range] [--auto-merge] [--no-pr] [--worktree] [--resume]',
  phases: [
    { title: 'Setup',    detail: 'Create (or re-attach) the branch (or --worktree) for the whole spec' },
    { title: 'Plan',     detail: 'Enumerate tasks from tasks.json (D16 lint) + load resume state' },
    { title: 'Tasks',    detail: 'Per task: implement → fast-test → (triage → fix/bail)' },
    { title: 'Review',   detail: 'ONE consolidated review of the integrated tree; full gating suite' },
    { title: 'Docs',     detail: 'Surgical /update-docs --patch (gates on PASS verdict)' },
    { title: 'Wrap-up',  detail: 'status/log + amendment log on the branch, then open a PR (or draft PR on bail)' },
  ]
}

// ----------------------------------------------------------------
// Parse args: "<spec-slug> [range] [--auto-merge] [--no-pr] [--worktree] [--resume] [--test-depth fast|full]"
// ----------------------------------------------------------------
const rawArgs = typeof args === 'string' ? args.trim() : ''
if (!rawArgs) {
  log('ERROR: No spec name provided.')
  log('Usage: /sdlc-flow <spec-slug> [range] [--auto-merge] [--no-pr] [--worktree] [--resume] [--test-depth fast|full]')
  return { error: 'Missing required argument: spec name (e.g. "<spec-slug>" or "<spec-slug> 1-3")' }
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
// when vaulted, the plain planning/ directory otherwise.
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

// Vault-aware task commits (extends D46): the per-task implement/fix stage and the docs stage below
// are instructed to stage + commit any planning/ paths they wrote THROUGH the vault repo (git -C
// <vault.planningPath>), reusing detectPlanningVault's real path exactly like the wrap-up recipe
// already does — never a second detection idiom. But that instruction is self-reported: this
// ticket's amendment log recorded a live run where a stage returned a perfectly valid commitHash
// that covered ONLY the source half of a task, with the vault half silently uncommitted. So a valid
// commitHash proves nothing about the vault half, and this check never keys on it — it independently
// re-verifies, for every filesModified path that resolves under the vault, that the path is BOTH
// tracked and free of any staged/unstaged diff in the vault repo (i.e. actually landed in a commit
// there), via a cheap Haiku agent turn rather than trusting the implementer's own report.
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

// Given a stage's self-reported filesModified (repo-root-relative) and a resolved vault, return the
// vault-relative subset (the part of the path after "planning/") that needs an independent
// vault-commit check. Derived from what the stage ACTUALLY wrote — never a hard-coded filename list.
function vaultRelPathsFrom(filesModified, vault) {
  if (!vault.vaulted || !Array.isArray(filesModified)) return []
  return filesModified
    .filter(f => typeof f === 'string' && (f === 'planning' || f.startsWith('planning/')))
    .map(f => f.slice('planning/'.length))
    .filter(Boolean)
}

const autoMergeFlag = hasFlag('--auto-merge')
const noPr          = hasFlag('--no-pr')
const resumeMode    = hasFlag('--resume')
// Isolation mode: default runs on a plain branch checked out in the MAIN working tree (keeps a relative
// planning/ symlink intact — worktrees break it). --worktree opts back into the isolated sparse-checkout
// worktree (needed for true isolation).
const useWorktree   = hasFlag('--worktree')

const VALID_TEST_DEPTHS = ['fast', 'full']
const testDepthFlag = flagStr('--test-depth')
if (testDepthFlag && !VALID_TEST_DEPTHS.includes(testDepthFlag)) {
  log(`ERROR: unknown --test-depth "${testDepthFlag}". Valid values: ${VALID_TEST_DEPTHS.join(', ')}.`)
  return { error: 'Invalid --test-depth', testDepthFlag, blockId }
}

// Optional task selection: `--tasks 1-7` OR a positional range as the 2nd token.
const rangeSpec = flagStr('--tasks') || (tokens[1] && !tokens[1].startsWith('--') ? tokens[1] : null)
let selectedTasks = null
if (rangeSpec) {
  const parsed = parseRange(rangeSpec)
  if (!parsed || parsed.length === 0) {
    log(`ERROR: could not parse task selection "${rangeSpec}". Use forms like 1-7, 1,3,5, or 1-3,7.`)
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
let stateFile     = `${blockDir}/sdlc/sdlc-flow-state.json`   // COMMITTED authoritative run index (D31)
let worklogFile   = `${blockDir}/sdlc/worklog.md`            // COMMITTED human-readable trail (D31)
const baseBranchName = `${blockId}-flow`                        // one shared branch for the whole spec

const MAX_TASK_ATTEMPTS   = 3   // implement→test→fix attempts per task before bail
const MAX_REVIEW_ATTEMPTS = 3   // consolidated-review fix passes before bail

log(`Target: ${blockId} (${selectedTasks ? [...selectedTasks].sort((a, b) => a - b).join(', ') : 'all tasks'})`)
log(`Spec: ${blockId} (resolving block record first, tasks.md fallback) | branch: ${baseBranchName} | mode: ${useWorktree ? 'worktree' : 'branch'}${resumeMode ? ' | RESUME' : ''}`)

// ================================================================
// Schemas
// ================================================================
const SETUP_SCHEMA = {
  type: 'object',
  required: ['branchName', 'worktreePath', 'wasCreated'],
  properties: {
    branchName:     { type: 'string', description: 'Actual branch name used (may have -2, -3 suffix if base was taken)' },
    worktreePath:   { type: 'string', description: 'Absolute path to the worktree directory' },
    wasCreated:     { type: 'boolean', description: 'true if a new worktree was created, false if an existing one was reused' },
    specFileExists: { type: 'boolean', description: 'true if EITHER the block record or the legacy tasks.md exists (D65 stage 2)' },
    specSource:     { type: 'string', enum: ['block-record', 'tasks-md', 'missing'], description: "D65 stage 2: 'block-record' if planning/blocks/<BlockID>.json exists (preferred), else 'tasks-md' if the legacy spec file exists, else 'missing'. Evaluated at the WINNING location (root if the spec exists there, else tier) — see specFoundInTier." },
    tierPrefix:     { type: 'string', description: 'The invoking directory\'s path relative to the git root, with a trailing slash (e.g. "business/"), or "" when /sdlc-flow was invoked at the git root. This is the CANDIDATE tier location checked in STEP 6a — reported regardless of whether the spec was actually found there.' },
    specFoundInTier: { type: 'boolean', description: 'true iff the spec (block record or legacy tasks.md) exists ONLY at the tier location (<tierPrefix>planning/<blockId>), not at the root (planning/<blockId>). False when found at the root (even if ALSO present at the tier — the root always wins) or found nowhere.' },
    blockStatus:    { type: 'string', description: "This spec's Status in status.md (title-case), or 'Unknown'" },
    specThin:       { type: 'boolean', description: 'D19: true ONLY on a fresh run (wasCreated && specFileExists) with a structurally-valid but substantively-thin spec. false on resume or a healthy spec.' },
    thinReason:     { type: 'string', description: 'D19: the specific thin-spec failures when specThin; empty string otherwise.' },
    setupError:     { type: 'string', description: 'Non-empty when setup could not proceed safely (e.g. branch mode aborted on a dirty working tree). The engine aborts and reports this. Empty string on success.' },
    envFilesCopied: { type: 'array', items: { type: 'string' }, description: '--worktree only: repo-root-relative paths of every gitignored env-shaped file seeded into the worktree (from ENV_COPIED: lines); empty array if none existed to copy, or in branch mode.' },
    notes:          { type: 'string' }
  }
}

// D16 preflight lint — the spec MUST carry a non-empty tasks.json array (a bare array of
// SDLCTask-shaped objects, matching orchestrator's app/schemas/sdlc_schema.py — see D45) or the
// per-task loop would have to guess the task count non-deterministically.
const ENUMERATE_SCHEMA = {
  type: 'object',
  required: ['hasTasks', 'allTasks'],
  properties: {
    hasTasks: { type: 'boolean', description: 'true if tasks.json parses as a non-empty array' },
    allTasks: { type: 'array', items: { type: 'integer' }, description: 'Every task_id in tasks.json, in array order' },
    // Per-task validation override. `validation_commands` is a real field in orchestrator's SDLCTask
    // schema that this engine used to ignore entirely. Honouring it lets a spec declare that a
    // given task needs a CHEAPER (or no) tripwire than the harness-wide gating set — the payoff is
    // largest in compile-expensive repos, where a docs-only or config-only task otherwise pays a
    // full build to validate a markdown edit. Empty array => fall back to the harness checks, so
    // every existing spec behaves exactly as before.
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

// D16 derive-from-tasks.md fallback — see the abort below. Mirrors /generate-tasks' --from mode:
// read the spec's authored step decomposition and
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
    exists:      { type: 'boolean', description: 'true if a valid sdlc-flow-state.json was read from the worktree' },
    startedAt:   { type: 'string',  description: "the file's started_at value, or '' when absent" },
    passedTasks: { type: 'array', items: { type: 'integer' }, description: 'task numbers whose status is "passed"' },
    bailReason:  { type: 'string',  description: 'the prior bail_reason, or "" when none' },
    tasksJson:   { type: 'string',  description: 'Verbatim JSON (as a string) of the state file\'s top-level "tasks" object, so the engine can carry the full prior task history forward. "{}" when absent/no state.' },
    notes:       { type: 'string' }
  }
}

const STAGE_SCHEMA = {
  type: 'object',
  required: ['reportFile', 'success'],
  properties: {
    reportFile:    { type: 'string', description: 'Path to the report written (or empty string — flow keeps state in state.json, not per-stage reports)' },
    success:       { type: 'boolean' },
    filesModified: { type: 'array', items: { type: 'string' } },
    commitHash:    { type: 'string', description: 'Short hash of the commit this agent made, or empty string' },
    summary:       { type: 'string', description: 'One-line summary of what was implemented/fixed (folded into state.tasks[N].summary)' },
    decisions:     { type: 'array', items: { type: 'string' }, description: 'Non-obvious choices made (folded into state)' },
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
    stateWritten: { type: 'boolean', description: 'true if the agent ALSO persisted sdlc-flow-state.json + worklog.md this same turn (the per-task pass-path state-write fold); false/omitted when it did not (no onPass instructions given, a check failed, or the write was not attempted/completed)' },
    notes:       { type: 'string' }
  }
}

const REVIEW_SCHEMA = {
  type: 'object',
  required: ['verdict'],
  properties: {
    verdict:        { type: 'string', enum: ['PASS', 'FAIL', 'PARTIAL'] },
    failureReasons: { type: 'array', items: { type: 'string' } },
    unmetCriteria:  { type: 'array', items: { type: 'string' } },
    localized:      { type: 'boolean', description: 'true if FAIL/PARTIAL failures are small/localized (a bounded fix can address them); false if broad/structural (needs a human re-plan)' },
    reportFile:     { type: 'string' },
    notes:          { type: 'string' }
  }
}

// Triage a per-task or per-review failure: RETRYABLE (a bounded fix can help) vs MAJOR (bail to a
// human now).
const TRIAGE_SCHEMA = {
  type: 'object',
  required: ['class', 'reason'],
  properties: {
    class:               { type: 'string', enum: ['RETRYABLE', 'MAJOR'] },
    reason:              { type: 'string', description: 'One sentence: why retryable (transient/changed/progressing) or major (one of the immediate-bail reasons, stuck, or structural)' },
    bailReason:          { type: 'string', description: 'When class=MAJOR: a short human-readable reason for the draft-PR handoff; empty when RETRYABLE' },
    sameFailureAsBefore: { type: 'boolean', description: 'true if the SAME failure as the previous attempt (no progress)' },
    evidence:            { type: 'string', description: 'What was actually OBSERVED, quoting the failing check output. No causal claims.' },
    baseStateChecked:    { type: 'boolean', description: 'true only if the failing check was actually re-run against the base state (main working tree or the task base commit). false means any claim about the base state is a hypothesis.' },
    stateWritten:        { type: 'boolean', description: 'true if the agent ALSO persisted sdlc-flow-state.json + worklog.md this same turn (the terminal-bail state-write fold); false/omitted when it did not (no onBail instructions given, the outcome was not terminal, or the write was not attempted/completed)' }
  }
}

const DOCS_SCHEMA = {
  type: 'object',
  required: ['success'],
  properties: {
    success:  { type: 'boolean' },
    changed:  { type: 'array', items: { type: 'string' }, description: 'doc files patched' },
    created:  { type: 'array', items: { type: 'string' }, description: 'doc files created' },
    flagged:  { type: 'array', items: { type: 'string' }, description: 'docs flagged NEEDS_REVIEW (not edited)' },
    commitHash: { type: 'string' },
    stateWritten: { type: 'boolean', description: 'true if the agent ALSO persisted sdlc-flow-state.json + worklog.md this same turn (the docs-phase state-write fold); false/omitted when it did not (write not attempted/completed)' },
    notes:    { type: 'string' }
  }
}

const WRAPUP_SCHEMA = {
  type: 'object',
  required: ['statusUpdated', 'devlogUpdated'],
  properties: {
    statusUpdated: { type: 'boolean' },
    devlogUpdated: { type: 'boolean' },
    nextFocus:     { type: 'string' },
    amendments:    { type: 'array', items: { type: 'string' }, description: 'D18 dated amendment-log lines appended to the spec (empty if none)' },
    commitHash:    { type: 'string' },
    blockStatusFlipped: { type: 'string', description: 'The state.json tracks[].blocks[].id flipped to "closed" on the branch this run, or "" if none (spec not fully done, no state.json, block not found, or the write was rejected by validation).' },
    stateWriteValidated: { type: 'boolean', description: 'true if mev validate-brain --state gated the state.json mutation (before/after diff, net-new only); false when mev was not on PATH and the write landed with only json.load-level parsing (a degrade, not a pass)' },
    stateWriteRejected: { type: 'boolean', description: 'true if the state.json mutation introduced net-new schema errors and was rolled back byte-exact; the block was NOT flipped to closed this run' },
    emitStateRan:  { type: 'boolean', description: 'true if `mev emit-state --write` regenerated derived surfaces on the branch itself during this in-place (non-worktree) wrap-up; false when skipped (worktree mode, or mev/brain.toml absent)' },
    stateWritten:  { type: 'boolean', description: 'true if the agent ALSO persisted sdlc-flow-state.json + worklog.md this same turn (the wrap-up-phase state-write fold); false/omitted when it did not (write not attempted/completed)' },
    notes:         { type: 'string' }
  }
}

// Outcome vocabulary (replaces the old single `created` boolean, which collapsed three distinct
// cases into one flag the engine trusted blindly — see planning/decisions/ for the PR-stage
// outcome vocabulary ADR):
//   'impossible' — no gh CLI or no git remote in this environment. Correct, expected, MUST NOT
//                  fail the run. Set only when step 2's GH_ABSENT/NO_REMOTE check fires.
//   'failed'     — a PR was genuinely attempted (push and/or `gh pr create`) and it errored out.
//                  Previously indistinguishable from 'impossible'; this is one of the two bugs.
//   'created'    — the PR exists. The engine does NOT take this on faith — see the dedicated
//                  PR_VERIFY_SCHEMA call below, which independently confirms it via `gh pr view`
//                  rather than trusting this self-report (the other bug: the old `|| true` on the
//                  lookup made a failed lookup indistinguishable from an absent PR).
const PR_SCHEMA = {
  type: 'object',
  required: ['outcome'],
  properties: {
    outcome:   { type: 'string', enum: ['created', 'impossible', 'failed'], description: "'impossible' = no gh/no remote (do NOT fail); 'failed' = push or gh pr create was attempted and errored; 'created' = PR exists (still independently re-verified by the engine, not trusted on its own)" },
    url:       { type: 'string', description: 'the PR URL, or "" if not created' },
    number:    { type: 'integer', description: 'the PR number, or 0 if not created' },
    draft:     { type: 'boolean', description: 'true if a draft PR (a bail handoff)' },
    pushed:    { type: 'boolean', description: 'true if the branch was pushed to the remote' },
    ghPresent: { type: 'boolean', description: 'true if the gh CLI was available' },
    notes:     { type: 'string', description: "when outcome != 'created': for 'impossible', the branch name + manual instructions; for 'failed', the actual push/gh error text so the operator can act on it" }
  }
}

// Independent verification of PR_SCHEMA's self-reported `outcome` — a SEPARATE agent turn, so the
// engine is not trusting the pr-create agent's own account of its own work. Reads the raw exit
// code of `gh pr view` on the branch itself; the engine (not the agent) decides `created` from
// that code, closing the `|| true` hole that made a failed lookup indistinguishable from "no PR".
const PR_VERIFY_SCHEMA = {
  type: 'object',
  required: ['exitCode'],
  properties: {
    exitCode: { type: 'integer', description: 'the exact process exit code `gh pr view <branch> --json number,url,state` returned — 0 only if a PR was actually found (the branch MUST be passed as the positional argument, never via --head, which is a `gh pr list`-only flag that `gh pr view` rejects as an unknown flag)' },
    url:      { type: 'string', description: 'the PR URL if exitCode == 0, else ""' },
    number:   { type: 'integer', description: 'the PR number if exitCode == 0, else 0' },
    state:    { type: 'string', description: 'OPEN/MERGED/CLOSED if exitCode == 0, else ""' }
  }
}

const MERGE_SCHEMA = {
  type: 'object',
  required: ['merged'],
  properties: {
    merged:        { type: 'boolean' },
    worktreeRemoved: { type: 'boolean', description: 'true if a worktree was removed; false in branch mode (there is none)' },
    branchDeleted: { type: 'boolean' },
    emitStateRan:  { type: 'boolean', description: 'true if `mev emit-state --write` regenerated derived surfaces on the base; false when skipped (mev/brain.toml absent or merge did not complete)' },
    notes:         { type: 'string' }
  }
}

const STATE_WRITE_SCHEMA = {
  type: 'object',
  required: ['written'],
  properties: {
    written:   { type: 'boolean', description: 'true if sdlc-flow-state.json (+ worklog.md) were written to disk' },
    startedAt: { type: 'string',  description: 'the started_at value used in this write (preserved from the existing file, or newly stamped)' },
    updatedAt: { type: 'string',  description: 'the updated_at value written in this write' },
    notes:     { type: 'string' }
  }
}

// ----------------------------------------------------------------
// MODEL TIERING — the primary token lever for this pipeline.
//
// Without this map every stage inherits the SESSION model — so launching /sdlc-flow from an Opus
// session silently runs the mechanical stages on Opus too. Principle (mirrors sdlc-task): match
// the model to the work. To re-tier, change one value here — nothing else moves.
// Valid values: 'haiku' | 'sonnet' | 'opus' | undefined (inherit session model).
// ----------------------------------------------------------------
const MODEL = {
  worktreeSetup: 'haiku',    // scripted git following an exact free-name + sparse-checkout recipe
  enumerate:     'haiku',    // read + parse tasks.json's task list — a fixed procedure
  derive:        'opus',     // D16 fallback: author a fresh tasks.json from tasks.md's step list — real judgment
  stateLoad:     'haiku',    // read + parse one JSON file (resume only)
  generateTasks: 'opus',     // PLANNING — authors the spec (fallback path only)
  implement:     'sonnet',   // writes code/content + tests against a scoped task
  fix:           'sonnet',   // targeted fixes; failures escalate, never silently ship
  test:          'haiku',    // runs the project's validation suite, reads exit codes
  triage:        'sonnet',   // classifies a failure RETRYABLE vs MAJOR — light judgment
  review:        'sonnet',   // the one consolidated review; gated by an authoritative fresh run
  docs:          'sonnet',   // surgical doc patches, gated on PASS
  wrapup:        'sonnet',   // human-facing status/log prose + the D18 amendment log (judgment)
  pr:            'sonnet',   // push + gh pr create with a handoff body; degrades if gh absent
  prVerify:      'haiku',    // ONE `gh pr view` call + report its exit code — mechanical, not judgment
  merge:         'sonnet',   // --auto-merge: merge the PR + clean up + emit-state on the base
  stateWriter:   'haiku',    // stamps timestamps, writes state.json + worklog.md, commits
}

// Final per-task fix pass and final review attempt before the loop gives up run on a stronger model.
// The common path stays on Sonnet; only the genuinely-hard case that already failed gets an Opus shot.
const ESCALATION_MODEL = 'opus'

// Merge an optional model override into an agent's opts (omits the key when undefined, so the agent
// inherits the session model rather than receiving model: undefined).
function withModel(base, model) {
  return model ? { ...base, model } : base
}

// ----------------------------------------------------------------
// TOKEN TELEMETRY (additive, no behavior change) — mirrors sdlc-task/run.
//   promptTokEst — injected input only (~prompt.length / 4)
//   outTok       — output-token delta from the shared budget pool; null when no +Nk target is set.
//                  Attributes cleanly for SEQUENTIAL stages — which is this engine's whole pipeline.
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

// Build the canonical `tokens` block from the accumulated per-agent metrics (Block A — the shared
// committed-state token contract, identical across all four engines; engines are self-contained, so
// this is lifted, not imported). Per-stage output tokens + the D15 input-cost estimate (promptTok +
// filesReadKb→tokens at ~256 tok/KB) + a cumulative total. filesReadKb is null here (flow stages do
// not self-report it yet); inTokEst then reduces to promptTokEst. writeFlowState folds the latest
// block into the COMMITTED state.json on every write, so token usage is persisted and rolled up
// rather than vanishing when the run ends.
//
// CONTRACT SCOPE (Phase 0 /code-review carry-in): `metrics` — and therefore `tokens.total` — cover the
// SUBSTANTIVE stages only. Cheap helper / state-writer agents (the Haiku state-writer, config + baseline
// loaders) deliberately use bare agent() and are EXCLUDED; this bounded, Haiku-cheap exclusion is the
// same boundary in both engines, named here so it is explicit rather than silent.
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
// spec's `## Validation Commands`. The flow.* block carries this engine's policy (autoMerge / testDepth
// / prBase / bailReasons). This engine runs inside a worktree — the loader cd's into worktreePath.
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
        },
        flow: {
          type: 'object',
          description: 'sdlc-flow policy block',
          properties: {
            autoMerge:   { type: 'boolean', description: 'default for --auto-merge when the flag is omitted' },
            testDepth:   { type: 'string', description: 'fast (default) | full — per-task validation depth' },
            prBase:      { type: 'string', description: 'the base branch for the PR (default: main)' },
            bailReasons: { type: 'array', items: { type: 'string' }, description: 'extra project-specific immediate-bail reasons' }
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

STEP 1 — Read the config file (from the worktree root):
  cd ${cwd} && cat planning/harness.json 2>/dev/null && echo "__HARNESS_PRESENT__" || echo "__HARNESS_ABSENT__"

STEP 2 — Decide:
  - "__HARNESS_ABSENT__" (file missing) → present=false, omit config.
  - File printed but NOT valid JSON → present=false, notes="harness.json present but invalid JSON: <reason>".
  - File printed and valid JSON → present=true, and copy the parsed object into "config", keeping ONLY
    these fields when present: stack; validation.checks[] (each: {kind, name, command, purpose, gates,
    perTask, fastCommand} plus any kind-specific fields present — baselineCommand, reasonCommand,
    compareKeys[], countPattern, failOn, warningPatterns[], rules[] ({id, pattern, paths,
    allowlistPattern})); flow ({autoMerge, testDepth, prBase, bailReasons[]}). Preserve kind-specific
    fields verbatim; ignore any other fields.

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
// rule 1). Independent of harness.json/spec checks: any .js .claude/workflows/ file this task's own
// tasks.json `files[]` names gets an unconditional `node --check`, in BOTH the fast-tripwire and
// full-suite render paths, even when the project ships no harness.json at all. Scoped to .js files
// only — `node --check` throws ERR_UNKNOWN_FILE_EXTENSION on non-JS paths (.md/.json) regardless of
// content, which is a false positive, not a real defect. No-op (renders '') when the task touches no
// such file — never emits a check with no target.
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

// Render the inner project-validation check list for a Test stage. When gatingOnly is true (the fast
// per-task tripwire), emit only the checks with gates:true; the end-review runs the FULL suite. When
// the config is absent (or carries no checks), fall back to the spec's `## Validation Commands` — the
// engine ships NO stack defaults. Handles all D6 check kinds. `engineFiles` (the .claude/workflows/
// paths in scope for this render, if any) is additive on top of everything below — see
// renderEngineParseChecks.
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
      const currentPath = `/tmp/${blockId}-flow-${slug}-current.json`
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
      const outPath = `/tmp/${blockId}-flow-${slug}.out`
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

    // count-delta is a per-task comparison with no analog in flow's consolidated model — treat as a
    // plain command run (its exit code still gates if gates:true).
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
// COMMITTED AUTHORITATIVE STATE (D31)
//
// `state` is the in-memory source of truth; writeFlowState() persists it to the COMMITTED
// state.json + appends a worklog.md section, then commits both (and any tasks.md checkbox change) in
// ONE commit on the branch. The runtime has no fs/clock, so a Haiku writer stamps started_at/updated_at
// and does the Write + git. This is the deliberate inversion of the harness's "reports are
// authoritative, state JSON is gitignored" rule — here state.json IS the index for resume + review +
// wrap-up. worklog.md keeps the run human-auditable.
// ----------------------------------------------------------------
const state = {
  spec_slug: blockId,
  branch: baseBranchName,
  mode: useWorktree ? 'worktree' : 'branch',
  worktree_path: '',
  status: 'running',
  current_task: null,
  tasks: {},        // "N": { status, attempts, summary, issues, fixes, decisions, files_changed, commit, validated }
  review: { verdict: null, findings: [], attempts: 0 },
  docs: { changed: [], created: [] },
  bail_reason: null,
  pr: { url: null, number: null },
  tokens: { stages: [], total: { promptTokEst: 0, filesReadKb: 0, inTokEst: 0, outTok: 0 } },  // Block A — refreshed by writeFlowState on every write
}

// Learned from the first successful state write of this process. Later writes are handed it as a
// literal so they can skip reading the state file back — the `cat` exists only to preserve
// started_at, and once any write has reported the value it used, re-reading it is a wasted Bash
// round trip. A fresh process — including every --resume — starts empty, so the first write always
// does the full read-and-preserve path and resume semantics are unchanged. A failed write leaves
// this null, so the next write re-reads rather than inventing a new started_at.
let cachedStartedAt = null
// Set once a write with a non-empty worklogEntry lands, so later writes skip the "write the header
// if the file is missing" branch (and the existence check it implies).
let worklogHeaderWritten = false

// Persist `state` to sdlc-flow-state.json + append `worklogEntry` (markdown) to worklog.md.
// `label` names the write for logging. This is deliberately WRITE-ONLY — no git command runs here.
//
// Why: this run-state lives under planning/<blockId>/sdlc/, and under D46 every vaulted repo's
// planning/ is a relative symlink into a brain-owned vault, so `git add planning/...` fails with
// "fatal: pathspec is beyond a symbolic link". The state-writer agent used to "repair" that failure
// by operating in the brain repo directly and checking out the run's branch there — contaminating
// HQ with spec-named branches and a `chore: flow state` commit per task. Run-state is read back only
// off disk (by --resume, via ${stateFile}), never out of git history, so there is no need to commit
// it at all — removing the commit removes the git verb the agent was getting wrong. `extraAdd` is
// kept in the signature for callers that have not yet been migrated off it; it is ignored here.
async function writeFlowState(label, worklogEntry, { cwd, extraAdd = [] } = {}) {
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
You maintain the run-state for an /sdlc-flow pipeline. You run from the WORKTREE root. Write two
files to disk — do NOT run git commands, do not run checks, do not edit source, do not touch
anything else. This state is read back off disk only (never out of git); it is deliberately not
committed.

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

STEP 3 — append to ${worklogFile}. ${worklogHeaderWritten
  ? 'The file already exists — append only, do not write a header. Append'
  : `If the file does not exist, first write a header line "# Worklog — ${blockId}" then a blank line. Then append`} this section verbatim (a blank line before it):
${worklogEntry ? '```\n' + worklogEntry + '\n```' : '(no worklog entry this write — skip the append)'}

Use the Write tool for both files. Do not run \`git add\`, \`git commit\`, \`git checkout\`,
\`git switch\`, or \`git branch\` — this write is disk-only. Return via StructuredOutput: written=true
once both files are written to disk, startedAt set to the started_at value you used, and updatedAt
set to the updated_at value you used.
`, withModel({ label: `state:${label}`, schema: STATE_WRITE_SCHEMA }, MODEL.stateWriter))
  if (result && result.startedAt) cachedStartedAt = result.startedAt
  if (result && result.written && worklogEntry) worklogHeaderWritten = true
  if (!result || !result.written) {
    log(`(state) could not persist flow state for "${label}" — continuing`)
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
// PHASE 0: SETUP — one branch (default) or one shared worktree (--worktree) for the whole spec
// ================================================================
phase('Setup')
log(`Setting up the ${useWorktree ? 'shared worktree' : 'branch'} for ${blockId}${resumeMode ? ' (resume — reuse existing if present)' : ''}...`)

// The working directory the STEP 6 reads run from once the branch/worktree is live. [placeholders]
// are filled by the agent with the resolved values.
const setupWorkdir = useWorktree ? 'trees/[branchName]' : '[repoRoot]'

const worktreeRecipe = `${resumeMode ? `
RESUME MODE IS ON — reuse the existing worktree for this spec instead of creating a fresh one.
  a. git worktree list | grep "trees/${baseBranchName}" && echo "WT_EXISTS" || echo "WT_MISSING"
  b. git branch --list "${baseBranchName}"
  Then:
  - WT_EXISTS → REUSE verbatim. branchName="${baseBranchName}", wasCreated=false. Skip STEP 2/3; go to STEP 3.5.
  - WT_MISSING but branch "${baseBranchName}" exists (orphan branch, dir removed) → re-attach (NO -b flag):
       mkdir -p trees
       git worktree add --no-checkout trees/${baseBranchName} ${baseBranchName}
       git -C trees/${baseBranchName} sparse-checkout init --cone
       git -C trees/${baseBranchName} sparse-checkout set $(git ls-tree HEAD --name-only -d | tr '\\n' ' ')
       git -C trees/${baseBranchName} checkout
       git ls-files --others --ignored --exclude-standard -- . | grep -E '(^|/)\\.env(\\.[^/]*)?$' | grep -Ev '(^|/)(node_modules|\\.venv|venv|trees|vendor)/' | while IFS= read -r f; do dest="trees/${baseBranchName}/$f"; if [ ! -f "$dest" ]; then mkdir -p "$(dirname "$dest")"; cp "$f" "$dest"; echo "ENV_COPIED: $f"; fi; done
    branchName="${baseBranchName}", wasCreated=false. Skip STEP 2/3; go to STEP 3.5.
  - Neither exists → fall through to STEP 2/3 and create a fresh worktree as normal.
` : ''}
STEP 2 — Find a free worktree name. FIRST check the exact base candidate "${baseBranchName}":
    git worktree list | grep "trees/${baseBranchName}" && echo "WT_EXISTS" || echo "WT_MISSING"
    git branch --list "${baseBranchName}"
  If EITHER exists, that is evidence of a PRIOR /sdlc-flow run on this exact spec — do NOT silently
  bump to a "-2" name and orphan it (this is how prior progress has been lost before: an agent
  restarts the pipeline after a failure/interruption without realizing --resume was needed, and a
  fresh "-2" worktree quietly starts the spec over from task 1). STOP instead: set wasCreated=false
  and setupError="A branch/worktree named '${baseBranchName}' already exists from a prior /sdlc-flow
  run on this spec. Re-run with --resume to continue it — this is required even if you are restarting
  via a cached Workflow resumeFromRunId, which does NOT by itself skip already-completed tasks. If you
  are certain you want to discard it and start over: git worktree remove trees/${baseBranchName}
  --force && git branch -D ${baseBranchName}, then re-run without --resume." Skip to STEP 6 and return.
  Otherwise (the base candidate is genuinely free), for each candidate run:
    git worktree list | grep "trees/<candidate>"
    git branch --list "<candidate>"
  If BOTH return nothing → the candidate is free; use it. Otherwise try "${baseBranchName}-2",
  "${baseBranchName}-3", … up to "-10" (an unrelated name collision, not a prior attempt on this
  spec — bumping past those is fine). Store the chosen name as branchName.

STEP 3 — Create the worktree (replace [branchName] / [repoRoot] with actual values):
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
     Record the list of "ENV_COPIED:" lines — report them in STEP 6.
  g. git -C trees/[branchName] commit --allow-empty -m "chore: init worktree [branchName]"

STEP 3.5 — Fix the planning/ symlink for the worktree (run from the MAIN repo root, for ALL paths —
  fresh create, re-attach, or reuse). In brain-vaulted repos the MAIN repo's \`planning\` is a
  RELATIVE symlink into a vault (e.g. planning -> ../_planning/<repo>) and is gitignored. Evaluated
  from inside trees/[branchName]/ that relative target breaks — so agents would hit a broken link,
  delete it, and write a real planning/ dir that later clobbers the symlink on merge. Prevent that by
  pointing the worktree's planning/ at the SAME real vault via an ABSOLUTE symlink (gitignored, so it
  is never committed or merged):
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

STEP 4 — Verify:
  Run: git worktree list
  Run: ls trees/[branchName]/
  Confirm it contains the tracked top-level directories — at minimum planning/ (real dir or the fixed
  symlink) and .claude/. Confirm planning/ resolves: ls trees/[branchName]/planning/ >/dev/null 2>&1 && echo "PLANNING_OK".

STEP 5 — Compute worktreePath = repoRoot + "/trees/" + branchName`

const branchRecipe = `${resumeMode ? `
RESUME MODE IS ON — reuse the existing branch for this spec instead of creating a fresh one.
  a. git branch --list "${baseBranchName}"
  Then:
  - If branch "${baseBranchName}" exists → check it out: git checkout ${baseBranchName}
    branchName="${baseBranchName}", wasCreated=false. Skip STEP 2/3; go to STEP 4.
  - If it does NOT exist → fall through to STEP 2/3 and create a fresh branch as normal.
` : ''}
STEP 2 — Find a free branch name. FIRST check the exact base candidate "${baseBranchName}":
    git branch --list "${baseBranchName}"
  If it exists, that is evidence of a PRIOR /sdlc-flow run on this exact spec — do NOT silently bump
  to a "-2" name and orphan it (this is how prior progress has been lost before: an agent restarts the
  pipeline after a failure/interruption without realizing --resume was needed, and a fresh "-2" branch
  quietly starts the spec over from task 1). STOP instead: set wasCreated=false and setupError="A
  branch named '${baseBranchName}' already exists from a prior /sdlc-flow run on this spec. Re-run
  with --resume to continue it — this is required even if you are restarting via a cached Workflow
  resumeFromRunId, which does NOT by itself skip already-completed tasks. If you are certain you want
  to discard it and start over: git branch -D ${baseBranchName}, then re-run without --resume." Skip
  to STEP 6 and return.
  Otherwise (the base candidate is genuinely free), for each candidate run:
    git branch --list "<candidate>"
  If it returns nothing → the candidate is free; use it. Otherwise try "${baseBranchName}-2",
  "${baseBranchName}-3", … up to "-10" (an unrelated name collision, not a prior attempt on this
  spec — bumping past those is fine). Store the chosen name as branchName.

STEP 3 — Create the branch and check it out IN THE MAIN WORKING TREE (no worktree, no trees/ dir):
  a. Guard against a dirty tree — uncommitted changes would ride onto the branch and into the run's
     commits. Run: git status --porcelain
     If it prints ANYTHING, STOP: do NOT create the branch. Set wasCreated=false and
     setupError="Working tree is not clean — commit or stash your changes, then re-run (or use --worktree
     for an isolated checkout). Dirty paths: <the porcelain output>". Then skip to STEP 6 and return.
  b. git checkout -b [branchName]
  No sparse-checkout, no env copy, no init commit — this is the real repo checkout, so the working tree
  (including any relative planning/ symlink) is already fully present and intact.

STEP 4 — Verify:
  Run: git branch --show-current      (must print [branchName])
  Run: ls planning/ .claude/ >/dev/null 2>&1 && echo "TREE_OK" || echo "TREE_MISSING"

STEP 5 — worktreePath = repoRoot  (branch mode runs in the main working tree — there is no separate worktree dir)`

const setupResult = await tracedAgent(`
You are the setup agent. ${useWorktree
  ? 'Create (or locate) ONE isolated git worktree for this whole spec — every task in the run shares it (sequential, so there are no inter-task merges).'
  : 'Create (or re-attach) ONE plain git branch for this whole spec and check it out IN THE MAIN WORKING TREE — every task in the run shares it (sequential, so there are no inter-task merges). No worktree is used, which keeps a relative planning/ symlink intact.'}
All bash commands run from the MAIN REPO ROOT (your current CWD).

Target:
  Spec:              ${blockId}
  Block record:      ${blockRecordFile} (preferred spec source, D65 stage 2)
  Legacy spec file:  ${specFile} (fallback — only used when the block record is absent)
  Base name:  ${baseBranchName}

STEP 1 — Get the absolute repo root:
  Run: git rev-parse --show-toplevel
  Store the trimmed output as repoRoot.
  Run this too — it must reflect where /sdlc-flow was actually invoked from (e.g. a sub-brain tier
  like business/), not worktreePath, which may differ:
    REPO_ROOT=$(git rev-parse --show-toplevel) && python3 -c "import os; r=os.path.relpath(os.getcwd(), '$REPO_ROOT'); print('' if r=='.' else r+'/')"
       (store trimmed stdout as candidateTierPrefix — "" when invoking at the git root, otherwise
       the invoking directory's path relative to repoRoot with a trailing slash, e.g. "business/")
${useWorktree ? worktreeRecipe : branchRecipe}

STEP 6 — Report pipeline-start inputs (run these from the live checkout):
  a. Spec source AND location (D65 stage 2 + tier resolution) — the block record is checked FIRST
     and is preferred; tasks.md is only a fallback for a legacy spec that predates the block-record
     migration. Check the ROOT first (it always wins when the spec exists at both locations):
       cd ${setupWorkdir} && ls ${blockRecordFile} 2>/dev/null && echo "RECORD_ROOT_EXISTS" || echo "RECORD_ROOT_MISSING"
       cd ${setupWorkdir} && ls ${specFile} 2>/dev/null && echo "LEGACY_ROOT_EXISTS" || echo "LEGACY_ROOT_MISSING"
     ONLY IF candidateTierPrefix (from STEP 1) is non-empty, ALSO check the tier location:
       cd ${setupWorkdir} && ls <candidateTierPrefix>${blockRecordFile} 2>/dev/null && echo "RECORD_TIER_EXISTS" || echo "RECORD_TIER_MISSING"
       cd ${setupWorkdir} && ls <candidateTierPrefix>${specFile} 2>/dev/null && echo "LEGACY_TIER_EXISTS" || echo "LEGACY_TIER_MISSING"
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
       cd ${setupWorkdir} && grep -iE "${blockId}" planning/status.md | head -5
     blockStatus = the title-case Status value (Not started / In progress / Done / Blocked / Skipped),
     or "Unknown" if no row is found.
  c. Thin-spec check (D19) — ONLY when wasCreated AND specSource == "tasks-md" (the legacy path — a
     block-record spec is authored structured JSON, not markdown prose, so the {{TOKEN}}/section checks
     below do not apply to it) (a fresh run about to spend implement tokens; skip on resume). Set
     specThin=true ONLY on these high-confidence signals (a blocked valid spec is far costlier than a
     missed thin one — when in doubt do NOT flag):
       - cd ${setupWorkdir} && grep -n '{{' ${specFile}  → any unfilled {{TOKEN}} is thin.
       - The '## Acceptance Criteria' section has no real '- ' bullet (empty, or only a template seed) → thin.
     Do NOT flag bare 'TODO'/'TBD' prose, do NOT treat '<...>' as a token (legitimate in 'Vec<T>', globs),
     never flag the Amendment Log seed '_No amendments yet._'. Else specThin=false, thinReason="".
${useWorktree ? `  d. Env files seeded — collect the "ENV_COPIED: <path>" lines printed during worktree setup
     (STEP 3 step f, or the RESUME re-attach path) into envFilesCopied (one path per entry; empty
     array if none printed — that means no gitignored env-shaped file exists in this repo, not that
     the copy failed silently). Report this list; a run missing config should say so at setup time
     rather than surface later as a confusing downstream failure (e.g. a fallback DB connection).
     Note: the worktree's path is derived from the SPEC SLUG (trees/${baseBranchName}), not any
     program/block ID — anything discovering it externally must use \`git worktree list\`, not guess.
` : ''}
Set setupError="" unless STEP 3 aborted (branch mode, dirty tree). Return your result using the StructuredOutput tool.
`, withModel({ label: 'setup', schema: SETUP_SCHEMA, phase: 'Setup' }, MODEL.worktreeSetup))

if (!setupResult) {
  log('Setup agent returned null — aborting pipeline')
  return { error: 'Setup failed', blockId }
}
if (setupResult.setupError) {
  log(`Setup aborted: ${setupResult.setupError}`)
  return { error: 'Setup aborted', reason: setupResult.setupError, blockId }
}
const { branchName, worktreePath } = setupResult
state.branch = branchName
state.worktree_path = worktreePath
log(`${useWorktree ? 'Worktree' : 'Branch'} ready: ${worktreePath} (branch: ${branchName})`)
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
// see SETUP_SCHEMA.specFoundInTier and the STEP 6a resolution order.
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
  stateFile       = `${blockDir}/sdlc/sdlc-flow-state.json`
  worklogFile     = `${blockDir}/sdlc/worklog.md`
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

// D19 — thin-spec guard for a fresh run (legacy tasks.md path only — see STEP 6c above).
if (setupResult.specThin) {
  log(`ABORTED (D19) — spec is structurally valid but substantively thin: ${setupResult.thinReason || '(no reason given)'}`)
  log(`Fix: flesh out ${specFile} (run /generate-tasks --force to regenerate, or edit + commit), then re-run.`)
  return { error: 'Thin spec (D19)', reason: setupResult.thinReason || '', blockId }
}

// Run-context injection header — prepended to every agent prompt that runs on the branch/worktree.
// In branch mode worktreePath === repoRoot, so the same `cd ${worktreePath}` prefix works in both modes.
const W = useWorktree
  ? `WORKTREE (not the main repo). repo root = ${worktreePath}
Shell state does NOT persist between Bash calls — START EVERY Bash call with: cd ${worktreePath} &&
Run all build/test/validation from the repo root; relative paths (planning/...) resolve from there.
`
  : `MAIN WORKING TREE, on branch ${branchName} (a plain branch — no worktree). repo root = ${worktreePath}
Shell state does NOT persist between Bash calls — START EVERY Bash call with: cd ${worktreePath} &&
You are already on branch ${branchName}; commit here and do NOT switch branches. Run all
build/test/validation from the repo root; relative paths (planning/...) resolve from there.
`

// ================================================================
// PHASE 1: PLAN — enumerate tasks (D16 lint) + load resume state
// ================================================================
phase('Plan')

if (!setupResult.specFileExists) {
  const rootPaths = `${rootBlockRecordFile} or ${rootSpecFile}`
  const tierPaths = tierPrefixCandidate ? `${tierPrefixCandidate}planning/blocks/${blockId}.json or ${tierPrefixCandidate}planning/${blockId}/tasks.md` : null
  log(`No spec found — searched the root (${rootPaths})${tierPaths ? ` AND the tier location (${tierPaths})` : ''}. /sdlc-flow expects an authored spec.`)
  log(`Fix: run /generate-tasks ${blockId} (and /breakdown) on main, commit, then re-run /sdlc-flow ${blockId}.`)
  return { error: 'Missing spec', blockId, searchedRoot: [rootBlockRecordFile, rootSpecFile], searchedTier: tierPaths ? [`${tierPrefixCandidate}planning/blocks/${blockId}.json`, `${tierPrefixCandidate}planning/${blockId}/tasks.md`] : [] }
}

const ENUMERATE_PROMPT = `${W}
You enumerate the tasks defined in a spec's tasks.json. Do NOT modify anything.

STEP 1 — read the task list:
  cd ${worktreePath} && cat ${tasksJsonFile} 2>/dev/null || echo "NO_TASKS_JSON"

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
  // tasks.md carries a derivable step decomposition. Mirrors /generate-tasks' --from mode:
  // author a FRESH decomposition from tasks.md (never
  // a verbatim copy of its prose). Deriving from an authored tasks.md is not guessing the task
  // structure — D16 exists to refuse fabricating one out of nothing, which the abort below still does.
  const deriveResult = await tracedAgent(`${W}
You are the D16 recovery generator for one /sdlc-flow spec. ${tasksJsonFile} is missing, invalid, or
empty; ${specFile} (tasks.md) may still carry a usable step decomposition. Do NOT implement anything.

STEP 1 — check for a derivable source:
  cd ${worktreePath} && cat ${specFile} 2>/dev/null || echo "NO_TASKS_MD"

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
let taskList = selectedTasks ? allTasks.filter(n => selectedTasks.has(n)) : allTasks.slice()
log(`Tasks in spec: ${allTasks.join(', ')}${selectedTasks ? ` | selected: ${taskList.join(', ')}` : ''}`)

// Per-task validation overrides from tasks.json's `validation_commands` (see ENUMERATE_SCHEMA).
// Returns null when the task declared none, which means "use the harness gating checks" — the
// pre-existing behaviour for every task in every existing spec.
// D63 (planning/decisions/D63-per-task-validation-commands-augment-gating.md) — DELIBERATELY
// DIFFERENT from sdlc-task.js: this engine stays a PURE SUBSTITUTE, unchanged. When present, a
// task's validation_commands still fully replaces the harness gating checks for that task's
// per-task tripwire (zero harness.json gates:true checks run for that task). This is safe here,
// and unsafe in sdlc-task.js, because this engine's end review (~line 1932 below) unconditionally
// re-runs the FULL gates:true harness suite over the integrated tree regardless of any per-task
// override — nothing is ever silently skipped forever, only deferred to the end review.
const taskCheckMap = new Map(
  (enumResult.taskChecks || [])
    .filter(tc => tc && Number.isInteger(tc.taskId) && Array.isArray(tc.validationCommands) && tc.validationCommands.length)
    .map(tc => [tc.taskId, tc.validationCommands])
)
function taskCommandsFor(taskNum) { return taskCheckMap.get(taskNum) || null }
if (taskCheckMap.size) {
  log(`Per-task validation overrides (tasks.json validation_commands): ${[...taskCheckMap.keys()].sort((a, b) => a - b).join(', ')} — D63: these tasks run ZERO planning/harness.json gates:true checks on their per-task tripwire (pure substitute, unchanged); the end review's full gating suite is the backstop.`)
}

// D63 — shared validated: vocabulary (identical strings in sdlc-task.js, per the ADR). This engine
// only ever lands on ranHarnessList (no override) or ranNoneOfHarnessList (override present) — it
// never reaches substitutedSubset, which is an /sdlc-task-only case (see the ADR's "never actually
// lands on both case 2 and case 3 within the same engine").
const VALIDATED_LABEL = {
  ranHarnessList: 'ran the harness list',
  substitutedSubset: 'substituted a documented subset (gates:true checks still ran)',
  ranNoneOfHarnessList: 'ran none of the harness list (tasks.json override, /sdlc-flow end review will reconcile)',
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
// `state.tasks` with the FULL prior tasks object — writeFlowState() serializes `state` wholesale on
// every write, and the per-task loop below only ever populates `state.tasks[N]` for tasks it actually
// runs (skipped/already-passed tasks never re-enter it) — so without this seed, the first write after
// a resume would silently drop the earlier-passed tasks from the committed file, and the *next*
// resume would see them as never-passed and re-run them.
const passedFromState = new Set()
if (resumeMode) {
  const loaded = await tracedAgent(`${W}
You read the COMMITTED run-state for an /sdlc-flow resume. Do NOT modify anything.
  cd ${worktreePath} && cat ${stateFile} 2>/dev/null || echo "__NO_STATE__"
If "__NO_STATE__" or invalid JSON → exists=false, tasksJson="{}". Otherwise exists=true,
startedAt = its started_at, passedTasks = the task numbers whose tasks[N].status == "passed",
bailReason = its bail_reason or "", tasksJson = the exact JSON (as a string) of its top-level "tasks"
object, verbatim — this is how the engine carries the full prior task history forward across a resume.
Return via StructuredOutput.
`, withModel({ label: 'state-load', schema: STATE_LOAD_SCHEMA, phase: 'Plan' }, MODEL.stateLoad))
  if (loaded && loaded.exists) {
    for (const n of (loaded.passedTasks || [])) passedFromState.add(n)
    log(`Resume: ${passedFromState.size} task(s) already passed (${[...passedFromState].sort((a, b) => a - b).join(', ') || 'none'}); skipping them.`)
    try {
      const priorTasks = JSON.parse(loaded.tasksJson || '{}')
      if (priorTasks && typeof priorTasks === 'object') Object.assign(state.tasks, priorTasks)
    } catch {
      log('(resume) could not parse prior tasks JSON from state.json — already-passed tasks may drop out of the committed history on the next write.')
    }
  } else {
    log('Resume requested but no valid state.json found — running all selected tasks fresh.')
  }
}

// Load the project's validation policy once (inside the worktree). null → fall back to the spec.
const harnessCfg = await loadHarnessConfig(worktreePath)
log(harnessCfg
  ? `Harness config: ${(harnessCfg.validation?.checks || []).length} check(s); flow.${JSON.stringify(harnessCfg.flow || {})}`
  : 'No planning/harness.json — validation falls back to the spec.')

// Resolve flow policy: CLI flag overrides harness.json overrides built-in default.
const flowCfg = harnessCfg?.flow || {}
const testDepth = testDepthFlag || (VALID_TEST_DEPTHS.includes(flowCfg.testDepth) ? flowCfg.testDepth : 'fast')
const autoMerge = autoMergeFlag || flowCfg.autoMerge === true
const prBase = flowCfg.prBase || 'main'
const extraBailReasons = Array.isArray(flowCfg.bailReasons) ? flowCfg.bailReasons : []
log(`Policy: testDepth=${testDepth} | autoMerge=${autoMerge} | prBase=${prBase} | PR=${noPr ? 'disabled' : 'enabled'}`)

// Snapshot baselines once (resume-safe; no-op without baseline-diff checks).
await snapshotBaselines(harnessCfg, worktreePath)

// The immediate-bail reason set the triage agent enforces (plan.md). "When unsure, prefer bail."
const BAIL_REASONS = [
  'Missing/undefined upstream dependency or symbol the spec assumes exists.',
  'Spec ambiguity/contradiction — intended behavior is genuinely undeterminable.',
  'Environment/credential/auth/network failure (not a code defect).',
  'Change would require a destructive or out-of-scope action.',
  'Same failure twice with no progress (stuck), or a structural design flaw needing a re-plan.',
  ...extraBailReasons,
].map((r, i) => `  ${i + 1}. ${r}`).join('\n')

// ----------------------------------------------------------------
// Test stage helper (shared by per-task tripwire + the review's re-run)
// gatingOnly=true → fast tripwire (gating checks); false → full authoritative suite.
// ----------------------------------------------------------------
// Render a per-task validation override (tasks.json `validation_commands`) as a check list in the
// same shape renderCheckList emits, so the test agent's instructions are identical either way.
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
// block for a passing test agent — the identical STEP 1-4 recipe writeFlowState() uses today (cat
// for started_at preservation, Write two files, explicit no-git-commands prohibition), inlined here
// so the fold doesn't need a follow-up dedicated state-writer agent. `onPass` is
// { stateFile, stateJson, worklogFile, worklogEntry } — all fully computable in JS before the test
// call is made, from the prior implement/fix stage's result.
function renderOnPassStateWriteRecipe(onPass) {
  return `
IF AND ONLY IF allPassed is true above, ALSO perform this state write as part of THIS SAME turn —
do NOT do this if any check failed (leave stateWritten unset/false in that case):

STEP W1 — run this as ONE Bash call, exactly as written. Do not split it into several calls:
  cd ${worktreePath} && mkdir -p ${blockDir}/sdlc && date -u +%Y-%m-%dT%H:%M:%SZ && { cat ${onPass.stateFile} 2>/dev/null || echo "__NO_STATE__"; }
  The FIRST line of output is NOW. Everything after it is the existing state file, or __NO_STATE__
  when there is none. If that file exists and has a "started_at" value, REUSE it verbatim for
  started_at below. Otherwise started_at = NOW.

STEP W2 — write ${onPass.stateFile} with EXACTLY this JSON, but inserting two extra top-level keys
  "started_at" (preserved or NOW, per STEP W1) and "updated_at" (NOW) right after "branch". Valid
  JSON only (double quotes, no trailing commas, no markdown fences). The object to write (verbatim
  except for adding those two timestamp keys):
${onPass.stateJson}

STEP W3 — append to ${onPass.worklogFile}. If the file does not exist, first write a header line
  "# Worklog — ${blockId}" then a blank line. Then append this section verbatim (a blank line
  before it):
\`\`\`
${onPass.worklogEntry}
\`\`\`

STEP W4 — use the Write tool for both files. Do NOT run \`git add\`, \`git commit\`, \`git checkout\`,
  \`git switch\`, or \`git branch\` — this write is disk-only, exactly like writeFlowState(). Set
  stateWritten=true in your StructuredOutput once both files are written to disk; leave it
  false/unset if you skipped this because a check failed.
`
}

async function runTests(label, { gatingOnly, taskCommands = null, onPass = null, engineFiles = [] }) {
  // Diff-window concurrent-sessions fix: the emoji gate scopes to the commit SHAs THIS run itself
  // recorded in the run-state (state.tasks[N].commit — the in-memory object writeFlowState()
  // persists to disk at stateFile), never to the whole prBase..HEAD range. Reading state.tasks
  // in-memory (rather than re-reading stateFile off disk) is deliberate: disk writes only happen
  // after a task fully passes, so by the time THIS task's own gate runs, its own just-made commit
  // would not yet be on disk — only the in-memory object already reflects it at prompt-build time,
  // right after `t.commit = stageResult.commit` in the caller.
  const recordedCommits = Object.values(state.tasks).map(x => x.commit).filter(Boolean)
  const recordedCommitsJson = JSON.stringify(recordedCommits)

  // D63 — pure substitute, unchanged: usingOverride still fully replaces the harness gating checks
  // for this task's per-task tripwire (not augmented, unlike sdlc-task.js). Safe here because the
  // end review below unconditionally re-runs the full gates:true suite over the integrated tree.
  const usingOverride = Array.isArray(taskCommands) && taskCommands.length > 0
  return tracedAgent(`${W}
You are the test agent for the /sdlc-flow pipeline. Run the project's validation checks and report.

IMPORTANT — run ONLY the checks enumerated below (${usingOverride
    ? "this task declares its OWN validation_commands in tasks.json, which REPLACE the project-wide harness checks for this task (D63 — pure substitute for this engine) — the full harness suite still runs at the end review"
    : 'from planning/harness.json + the spec'}). Do NOT invent
checks. All Bash calls run from the worktree root (prefix each with: cd ${worktreePath} &&).

${usingOverride
    ? renderTaskCheckList(taskCommands, worktreePath)
    : renderCheckList(harnessCfg, { gatingOnly, cwd: worktreePath, engineFiles })}

Then run the universal emoji gate (a harness rule, always) — DIFF-SCOPED to this run's OWN
recorded commit SHAs, never the whole ${prBase}..HEAD range: it judges only lines ADDED by
commits THIS run itself made, so neither a legacy file's pre-existing emoji nor a concurrent
sibling session's commit can fail a diff this run never touched (the literal "🤖 Generated with
Claude Code" PR footer is exempt — it lives in the PR body, not a file, but the check exempts the
phrase defensively too):
  cd ${worktreePath} && python3 - <<'PYEOF'
import subprocess, re, sys
EMOJI = re.compile(r'[\\U0001F300-\\U0001FAFF\\U00002600-\\U000027BF]')
FOOTER = 'Generated with Claude Code'
BASE_SHA = '${prBase}'
STATE_FILE = '${stateFile}'
RUN_COMMITS = ${recordedCommitsJson}
if not RUN_COMMITS:
    base_diff = subprocess.run(['git','diff','--name-only',f'{BASE_SHA}..HEAD'], capture_output=True, text=True).stdout.strip()
    if base_diff:
        print(f'EMOJI CHECK: cannot scope diff -- no commits recorded in the run-state ({STATE_FILE}) for this run, but {BASE_SHA}..HEAD is non-empty. Refusing to pass on an unscoped diff.')
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
PYEOF
  A stray emoji ADDED in a commit THIS run made FAILS this gate.

For each check record: name, passed (true iff exit code 0), the command, and failure output.
${onPass ? renderOnPassStateWriteRecipe(onPass) : ''}
Return via StructuredOutput: allPassed (true only if EVERY check passed), passCount, failCount,
failedTests (names), failBlob (compact: failing check names + the tail of their output; empty when allPassed)${onPass ? ', stateWritten (true only if you performed the additional state write above)' : ''}.
`, withModel({ label, schema: TEST_SCHEMA, phase: 'Tasks' }, MODEL.test))
}

// Renders the "if this triage call is terminal, ALSO perform this exact state write, in this same
// turn" instruction block for the triage agent — mirrors renderOnPassStateWriteRecipe's STEP 1-4
// recipe, with one addition: the bail_reason placeholder in the state JSON must be filled with the
// SAME formula the JS per-task loop uses for that outcome (MAJOR: bailReason || reason || a
// precomputed fallback; exhausted-attempts-while-RETRYABLE: a different precomputed fallback,
// ignoring bailReason/reason entirely) — mirrored exactly here since the effective bail reason is
// only known once the agent has classified, inside this same turn. `onBail` is
// { stateFile, stateJson, worklogFile, worklogEntry, majorFallback, exhaustionFallback } —
// exhaustionFallback is null at call sites that have no attempt-exhaustion bail path (mirrors the
// asymmetry between the NULL_RESULT and test-failure call sites in the per-task loop today).
function renderBailStateWriteRecipe(onBail, attempt, maxAttempts) {
  const esc = s => String(s).replace(/"/g, '\\"')
  return `
IF AND ONLY IF your class above is MAJOR${onBail.exhaustionFallback ? `, OR this is the final attempt (attempt ${attempt} of ${maxAttempts})` : ''}, ALSO perform this state
write as part of THIS SAME turn — do NOT do this ${onBail.exhaustionFallback ? `if class is RETRYABLE and this is NOT the final attempt` : `unless class is MAJOR`} (leave stateWritten unset/false in that case):

First compute the effective bail reason (used in STEP W2/W3 below):
  - If your class is MAJOR: use your own bailReason field if you set a non-empty value; otherwise
    your own reason field if non-empty; otherwise this exact fallback text: "${esc(onBail.majorFallback)}"
${onBail.exhaustionFallback ? `  - If your class is RETRYABLE but this IS the final attempt (attempt ${attempt} of ${maxAttempts}):
    IGNORE your own bailReason/reason and use this EXACT fallback text instead: "${esc(onBail.exhaustionFallback)}"` : ''}

STEP W1 — run this as ONE Bash call, exactly as written. Do not split it into several calls:
  cd ${worktreePath} && mkdir -p ${blockDir}/sdlc && date -u +%Y-%m-%dT%H:%M:%SZ && { cat ${onBail.stateFile} 2>/dev/null || echo "__NO_STATE__"; }
  The FIRST line of output is NOW. Everything after it is the existing state file, or __NO_STATE__
  when there is none. If that file exists and has a "started_at" value, REUSE it verbatim for
  started_at below. Otherwise started_at = NOW.

STEP W2 — write ${onBail.stateFile} with EXACTLY this JSON, but: (a) inserting two extra top-level
  keys "started_at" (preserved or NOW, per STEP W1) and "updated_at" (NOW) right after "branch", and
  (b) replacing the literal placeholder string "__BAIL_REASON__" (the top-level "bail_reason" field)
  with the effective bail reason computed above. Valid JSON only (double quotes, no trailing commas,
  no markdown fences). The object to write (verbatim except for those substitutions):
${onBail.stateJson}

STEP W3 — append to ${onBail.worklogFile}. If the file does not exist, first write a header line
  "# Worklog — ${blockId}" then a blank line. Then append this section verbatim (a blank line
  before it), with one more line appended at the end reading exactly
  "Bail reason: <the effective bail reason computed above>":
\`\`\`
${onBail.worklogEntry}
\`\`\`

STEP W4 — use the Write tool for both files. Do NOT run \`git add\`, \`git commit\`, \`git checkout\`,
  \`git switch\`, or \`git branch\` — this write is disk-only, exactly like writeFlowState(). Set
  stateWritten=true in your StructuredOutput once both files are written to disk; leave it
  false/unset if you skipped this because the outcome was not terminal.
`
}

// Precompute the exact state.json + worklog.md content for the case where THIS triage call turns
// out to be terminal (class=MAJOR, or — only at call sites that pass exhaustionFallback — this is
// the final allowed attempt) — content that is fully known BEFORE the triage call is made, except
// the effective bail reason, which the triage agent itself computes as part of classifying (see
// renderBailStateWriteRecipe). Handed to triage() as `onBail` so a terminal triage call can write
// it in its own turn instead of a follow-up dedicated state-writer agent. Does NOT mutate the live
// `state`/`t` objects — this is a snapshot for the CANDIDATE outcome.
function buildBailPayload(taskNum, t, attempt, majorFallback, exhaustionFallback = null) {
  const snapshot = JSON.parse(JSON.stringify(state))
  snapshot.tasks[String(taskNum)] = { ...t, status: 'failed' }
  snapshot.status = 'blocked'
  snapshot.bail_reason = '__BAIL_REASON__'
  snapshot.tokens = buildTokensBlock()
  const worklogEntry = [
    `## Task ${taskNum} — FAILED (${attempt} attempt${attempt === 1 ? '' : 's'})`,
    t.summary ? `What: ${t.summary}` : '',
    (t.issues || []).length ? `Issues hit: ${t.issues.join('; ')}` : '',
    (t.fixes || []).length ? `Fixed via: ${t.fixes.join('; ')}` : '',
    (t.decisions || []).length ? `Decisions: ${t.decisions.join('; ')}` : '',
    t.commit ? `Commit: ${t.commit}` : '',
  ].filter(Boolean).join('\n')
  return {
    stateFile,
    stateJson: JSON.stringify(snapshot, null, 2),
    worklogFile,
    worklogEntry,
    majorFallback,
    exhaustionFallback,
  }
}

// ----------------------------------------------------------------
// Triage helper (shared by the per-task loop + the review fix loop)
// ----------------------------------------------------------------
async function triage(context, attempt, maxAttempts, failBlob, sameContext, onBail = null) {
  return tracedAgent(`
You are the failure-triage agent for an /sdlc-flow run. Classify a failure so the pipeline either makes
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

// Precompute the exact state.json + worklog.md content for the case where task `taskNum` PASSES on
// this attempt — content that is fully known from the implement/fix stage's result (t.summary,
// t.commit, t.files_changed, t.decisions) BEFORE the test call is even made; the test call only
// determines whether this precomputed content actually gets used. Handed to runTests() as `onPass`
// so a passing test agent can write it in its own turn instead of a follow-up dedicated state-writer
// agent. Does NOT mutate the live `state`/`t` objects — this is a snapshot for the CANDIDATE outcome.
function buildPassPayload(taskNum, t, attempt, validatedLabel) {
  const snapshot = JSON.parse(JSON.stringify(state))
  snapshot.tasks[String(taskNum)] = { ...t, status: 'passed', validated: validatedLabel }
  snapshot.tokens = buildTokensBlock()
  const worklogEntry = [
    `## Task ${taskNum} — PASSED (${attempt} attempt${attempt === 1 ? '' : 's'})`,
    t.summary ? `What: ${t.summary}` : '',
    (t.issues || []).length ? `Issues hit: ${t.issues.join('; ')}` : '',
    (t.fixes || []).length ? `Fixed via: ${t.fixes.join('; ')}` : '',
    (t.decisions || []).length ? `Decisions: ${t.decisions.join('; ')}` : '',
    t.commit ? `Commit: ${t.commit}` : '',
    `Validated: ${validatedLabel}`,
  ].filter(Boolean).join('\n')
  return {
    stateFile,
    stateJson: JSON.stringify(snapshot, null, 2),
    worklogFile,
    worklogEntry,
  }
}

// ================================================================
// PHASE 2: PER-TASK LOOP (sequential, in the one shared worktree)
// ================================================================
phase('Tasks')

// D46 + vault-aware task commits: resolve ONCE for the whole run and reuse everywhere below (the
// per-task commit step, the docs stage, and the wrap-up stage) — never re-detect per task/stage, and
// never a second detection idiom.
const vault = await detectPlanningVault(worktreePath)

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

  // "in-progress" is already tracked in state.tasks[N].status (set below, committed by the
  // state-writer) — tasks.json is a task-definition file, not a live-status file, so there is no
  // separate checkbox/marker to edit here.

  let taskPassed = false
  let prevFailBlob = null
  let taskStateWritten = false

  for (let attempt = 1; attempt <= MAX_TASK_ATTEMPTS && !bailed; attempt++) {
    t.attempts = attempt
    const isFix = attempt > 1
    const fixModel = (ESCALATION_MODEL && attempt === MAX_TASK_ATTEMPTS) ? ESCALATION_MODEL : MODEL.fix
    if (isFix && fixModel !== MODEL.fix) log(`Task ${taskNum}: final fix pass — escalating model to ${fixModel}.`)
    log(`Task ${taskNum}: ${isFix ? `fix pass ${attempt - 1}` : 'implement'} (attempt ${attempt}/${MAX_TASK_ATTEMPTS})...`)

    // 2 / 5b. Implement (attempt 1) or targeted Fix (attempt > 1).
    const stageResult = await tracedAgent(`${W}
You are the ${isFix ? 'fix' : 'implementation'} agent for the /sdlc-flow pipeline. You run IN PLACE in the
shared worktree (sequential — earlier tasks in this spec are already committed on this branch). Work ONLY
on Task ${taskNum} of this spec.

Target:
  Spec:        ${blockId}
  Task:        Task ${taskNum} only
  Spec file:   ${specFile} ${specDesc}
  Tasks file:  ${tasksJsonFile} (the task list — find the entry with "task_id": ${taskNum})

1. Read CLAUDE.md and planning/context.md — internalize the project's standing rules (CLAUDE.md is the
   authority; assume no stack/locale/narrative/content rule unless written there). Universal harness
   rules always apply: no fabricated metrics or quotes, no emoji, every change ships with tests.
   Run: cd ${worktreePath} && cat CLAUDE.md

2. Read the spec and the task list:
   Run: cd ${worktreePath} && cat ${specFile} ${tasksJsonFile}
   tasks.json is a bare array — find the object whose "task_id" is ${taskNum}. Its "title",
   "description", and "files" define exactly what this task is.
   ${isFix ? `Do NOT re-implement from scratch. Make the MINIMUM targeted changes to address THIS failure:
   ${prevFailBlob ? 'Failing checks/output from the last test run:\n' + prevFailBlob.split('\n').map(l => '     ' + l).join('\n') : ''}` : `Implement ONLY task id ${taskNum} — do NOT implement other tasks.`}

2.5. Optional breakdown (more granular sub-steps from /breakdown):
   Run: cd ${worktreePath} && ls ${breakdownFile} 2>/dev/null && echo "BREAKDOWN_EXISTS" || echo "NO_BREAKDOWN"
   If BREAKDOWN_EXISTS: read ${breakdownFile}, find "### Step ${taskNum}:", and use its atomic sub-steps as
   the execution guide (run each inline "Verify:" checkpoint). tasks.json stays authoritative for scope.

3. Execute methodically with Read/Edit/Write/Bash (all paths resolve from the worktree root).

4. Follow every CLAUDE.md standing rule; add/update tests for new code/logic; verify any model ids /
   package names via the claude-api skill — never from memory.

5. COMPLETENESS SELF-CHECK before committing (D8): no stub/placeholder on any path the task's acceptance
   criteria require; every deliverable named for Task ${taskNum} exists; any "unit-tested" criterion has a
   real test. If something required is incomplete, finish it now — do not commit a partial task.

6. Run the spec's "## Validation Commands" for Task ${taskNum} to confirm correctness.

7. Commit on the branch. Never use git add -A or git add . — stage files explicitly by name.
   Run: cd ${worktreePath} && git status
   Stage your changed source/test files explicitly, then commit using HEREDOC:
     cd ${worktreePath} && git commit -m "$(cat <<'EOF'
${isFix ? `fix: fix pass ${attempt - 1} for ${stem}` : `feat: implement ${stem}`}
EOF
)"
   Run: cd ${worktreePath} && git log --oneline -1   (capture the short hash)
${vault.vaulted ? `
7b. planning/ is a vaulted symlink (D46) — its bytes live at ${vault.planningPath}, a DIFFERENT git
    repo, invisible to the commit you just made in step 7. If this attempt created or edited ANY file
    under planning/ (i.e. it belongs in filesModified with a "planning/" prefix), you MUST ALSO stage
    and commit it there, through the real path — derive the exact set from what you actually wrote,
    never a fixed list of filenames. NEVER git add -A, git add ., git reset, or git stash against the
    vault repo — another lane's session may have unrelated work staged there right now; touch ONLY
    your own paths, and do not checkout/switch/branch inside it (stay on whatever branch it is
    already on). For each such file, let <relpath> be the part of its path AFTER "planning/":
      cd ${worktreePath} && git -C ${vault.planningPath} add ${vault.planningPath}/<relpath>
    Then, once every such path is staged, commit ONLY those paths — pass them explicitly to \`git commit\`
    itself (not merely to \`git add\`), so a sibling lane's unrelated pre-staged files are never swept
    into this commit even if they happen to already be staged:
      cd ${worktreePath} && git -C ${vault.planningPath} diff --cached --quiet -- <relpath1> <relpath2> ... || git -C ${vault.planningPath} commit -m "$(cat <<'EOF'
${isFix ? `fix: fix pass ${attempt - 1} for ${stem} (vault)` : `feat: implement ${stem} (vault)`}
EOF
)" -- <relpath1> <relpath2> ...
      cd ${worktreePath} && git -C ${vault.planningPath} log --oneline -1
    If NOTHING you wrote this attempt lives under planning/, skip this step entirely — do not run any
    vault command. If a vault add/commit fails, report it PLAINLY in notes; never paper over it, and
    never "repair" it by committing on a different branch inside the vault.
` : ''}
Return via StructuredOutput:
  reportFile: ""   (flow keeps state in state.json, not per-stage reports)
  success: true if the work completed and the spec validation passed
  filesModified: every file you created or modified this attempt — including any under planning/
    (do NOT omit vault-side files just because they commit through a different repo)
  commitHash: the 7-char short hash of THIS repo's commit (empty string if no commit was made here)
  summary: one line — what this task now does
  decisions: any non-obvious choices (empty array if none)
  notes: one-line status${vault.vaulted ? ' — mention explicitly whether a vault commit (step 7b) happened and, if so, its outcome' : ''}
`, withModel({ label: `${isFix ? 'fix' : 'implement'}-${taskNum}-${attempt}`, schema: STAGE_SCHEMA, phase: 'Tasks' }, isFix ? fixModel : MODEL.implement))

    if (!stageResult) {
      log(`Task ${taskNum} attempt ${attempt}: agent returned null.`)
      // No attempt-exhaustion bail path exists at this call site today (an exhausted NULL_RESULT
      // loop just falls out of the `for` naturally without ever setting `bailed` — that pre-existing
      // asymmetry with the test-failure site below is unchanged by this fold), so exhaustionFallback
      // is omitted: the folded write only fires when this call classifies MAJOR.
      const nullBailPayload = buildBailPayload(taskNum, t, attempt, 'agent returned null')
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

    // 2b. Vault-commit verification — independent of the stage's self-report. A non-empty commitHash
    // proves nothing about the vault half (observed live: one run's commitHash was valid and covered
    // only the source half, with the vault edit silently uncommitted — see this ticket's amendment
    // log). So this ALWAYS re-derives the vault-relevant subset from filesModified and re-checks it
    // directly, rather than trusting anything the stage reported. A failure here surfaces exactly
    // like a test failure: the task is never marked passed on this attempt.
    const vaultRelPaths = vaultRelPathsFrom(stageResult.filesModified, vault)
    if (vaultRelPaths.length) {
      const vaultVerify = await verifyVaultCommit(worktreePath, vault, vaultRelPaths)
      if (!vaultVerify.allCommitted) {
        const uncommitted = (vaultVerify.uncommittedPaths && vaultVerify.uncommittedPaths.length) ? vaultVerify.uncommittedPaths : vaultRelPaths
        log(`Task ${taskNum} attempt ${attempt}: vault commit incomplete — not committed in ${vault.planningPath}: ${uncommitted.join(', ')}.`)
        const vaultFailBlob = `VAULT_COMMIT_INCOMPLETE — planning/ path(s) not committed in the vault repo (${vault.planningPath}): ${uncommitted.join(', ')}. ${vaultVerify.notes || ''}`.trim()
        t.issues = [...(t.issues || []), 'vault commit incomplete']
        const vaultBailPayload = buildBailPayload(taskNum, t, attempt, `Task ${taskNum}: vault commit incomplete — ${uncommitted.join(', ')}`)
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
          log(`Task ${taskNum}: exhausted ${MAX_TASK_ATTEMPTS} attempts on a vault-commit failure — bailing to wrap-up.`)
          break
        }
        if (tr) t.fixes = [...(t.fixes || []), tr.reason]
        log(`Task ${taskNum}: triage → RETRYABLE on vault-commit failure — fix pass ${attempt}/${MAX_TASK_ATTEMPTS - 1}. ${tr?.reason || ''}`)
        continue
      }
    }

    // 3. Fast test (tripwire) — gating checks only unless testDepth=full. A task that declares its
    //    own `validation_commands` in tasks.json runs THOSE instead (D63 — pure substitute, unchanged
    //    for this engine: the end review still runs the full harness suite over the integrated tree,
    //    so nothing escapes validation — this only changes what the per-task tripwire costs).
    //    passValidatedLabel is always one of the shared VALIDATED_LABEL trichotomy (D63).
    const hasOverride = !!taskCommandsFor(taskNum)
    const passValidatedLabel = hasOverride ? VALIDATED_LABEL.ranNoneOfHarnessList : VALIDATED_LABEL.ranHarnessList
    const passPayload = buildPassPayload(taskNum, t, attempt, passValidatedLabel)
    const testResult = await runTests(`test-${taskNum}-${attempt}`, { gatingOnly: testDepth === 'fast', taskCommands: taskCommandsFor(taskNum), onPass: passPayload, engineFiles: engineFilesFor(taskNum) })
    if (testResult && testResult.allPassed) {
      t.validated = passValidatedLabel
      // D63 — a task that ran ZERO harness.json gating checks must be VISIBLE in terminal output,
      // never only recorded in state. In this engine that is the ordinary override case (pure
      // substitute), backstopped by the end review's unconditional full-suite re-run.
      log(`Task ${taskNum}: validated → "${passValidatedLabel}".${passValidatedLabel === VALIDATED_LABEL.ranNoneOfHarnessList ? ' NOTE: this task ran ZERO planning/harness.json gates:true checks on its per-task tripwire; the end review will re-run the full gating suite over the integrated tree.' : ''}`)
      taskPassed = true
      if (testResult.stateWritten) {
        // The folded write went straight to disk (no STATE_WRITE_SCHEMA result to read startedAt
        // back from), so cachedStartedAt is deliberately left as-is: the next dedicated writeFlowState
        // call (a later task, or this task's own reliability-net fallback) will just re-`cat` the file
        // it wrote — which still correctly preserves started_at, just without the caching shortcut.
        taskStateWritten = true
        worklogHeaderWritten = true
      }
      break
    }

    // 5. Failure → triage.
    const failBlob = (testResult && testResult.failBlob) || `Test stage failed or returned null (failCount=${testResult?.failCount ?? '?'}, failed=${(testResult?.failedTests || []).join(', ')}).`
    t.issues = [...(t.issues || []), ...((testResult?.failedTests) || [])]
    // This call site DOES have an attempt-exhaustion bail path (below), with its own fallback text
    // that ignores the triage agent's own bailReason/reason entirely — pass both fallbacks through so
    // the folded write mirrors whichever terminal path actually fires, exactly.
    const majorFallback = `Task ${taskNum}: ${(testResult?.failedTests || []).join(', ')}`
    const exhaustionFallback = attempt === MAX_TASK_ATTEMPTS
      ? `Task ${taskNum} still failing after ${MAX_TASK_ATTEMPTS} attempts: ${(testResult?.failedTests || []).join(', ')}`
      : null
    const testBailPayload = buildBailPayload(taskNum, t, attempt, majorFallback, exhaustionFallback)
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
      log(`Task ${taskNum}: exhausted ${MAX_TASK_ATTEMPTS} attempts — bailing to wrap-up.`)
      break
    }
    if (Array.isArray(stageResult.filesModified) && tr) t.fixes = [...(t.fixes || []), tr.reason]
    log(`Task ${taskNum}: triage → RETRYABLE — fix pass ${attempt}/${MAX_TASK_ATTEMPTS - 1}. ${tr?.reason || ''}`)
  }

  // 6. One state-commit for this task (state.json + worklog.md + the in-progress checkbox edit).
  t.status = taskPassed ? 'passed' : (bailed ? 'failed' : 'failed')
  if (bailed && !taskPassed) { state.status = 'blocked'; state.bail_reason = bailReason }
  const worklogEntry = [
    `## Task ${taskNum} — ${t.status.toUpperCase()} (${t.attempts} attempt${t.attempts === 1 ? '' : 's'})`,
    t.summary ? `What: ${t.summary}` : '',
    (t.issues || []).length ? `Issues hit: ${t.issues.join('; ')}` : '',
    (t.fixes || []).length ? `Fixed via: ${t.fixes.join('; ')}` : '',
    (t.decisions || []).length ? `Decisions: ${t.decisions.join('; ')}` : '',
    t.commit ? `Commit: ${t.commit}` : '',
    t.validated ? `Validated: ${t.validated}` : '',
  ].filter(Boolean).join('\n')
  // Reliability net: either the pass-path fold (runTests' onPass, task 1) or the terminal-bail fold
  // (triage's onBail, task 2) already wrote state.json + worklog.md in the SAME turn as the
  // resolving test/triage call when taskStateWritten is true — skip the dedicated writer in that
  // case. taskStateWritten is only ever set true alongside taskPassed or bailed (never both), so
  // checking it alone is sufficient. Any other outcome (stateWritten false/unset, testResult/triage
  // null) falls through to the dedicated call so no task outcome is ever left unpersisted.
  if (!taskStateWritten) {
    await writeFlowState(`task ${taskNum} ${t.status}`, worklogEntry, { cwd: worktreePath })
  } else {
    log(`Task ${taskNum}: state write folded into the ${taskPassed ? 'passing test' : 'terminal triage'} agent's own turn — skipped the dedicated state-writer call.`)
  }

  if (bailed) break
}

// ================================================================
// PHASE 3: END-OF-RUN REVIEW — one consolidated review of the integrated tree
//   Runs only if every selected task passed (no bail).
// ================================================================
let finalVerdict = bailed ? 'BAILED' : 'NOT_REACHED'

if (!bailed) {
  phase('Review')
  state.status = 'review'
  let reviewAttempts = 0
  let lastReview = null

  while (reviewAttempts < MAX_REVIEW_ATTEMPTS) {
    reviewAttempts++
    state.review.attempts = reviewAttempts
    const reviewModel = (ESCALATION_MODEL && reviewAttempts === MAX_REVIEW_ATTEMPTS) ? ESCALATION_MODEL : MODEL.review
    if (reviewModel !== MODEL.review) log(`Final review attempt — escalating model to ${reviewModel}.`)
    log(`Consolidated review (attempt ${reviewAttempts}/${MAX_REVIEW_ATTEMPTS})...`)

    // The authoritative gate: re-run the FULL gating suite, then judge criteria against the real diff.
    const reviewResult = await tracedAgent(`${W}
You are the SINGLE consolidated review agent for an /sdlc-flow run — one review over the whole
integrated tree (it replaces per-task review entirely). Verify the spec's acceptance criteria against
the ACTUAL code and issue a verdict. All Bash calls run from the worktree root.

Target:
  Spec:        ${blockId}
  Spec file:   ${specFile} ${specDesc}
  Tasks run:   ${taskList.join(', ')}
  Base branch: ${prBase}

The committed run-state is your INDEX (per-task summary/issues/fixes/decisions/files) — read it first,
but it does NOT replace verifying the criteria against the code:
  cd ${worktreePath} && cat ${stateFile}

1. Read the spec's COMPLETE acceptance criteria — this is your checklist (the "## Acceptance Criteria"
   section in prose, or the "acceptance_criteria" array in a JSON block record).
   Run: cd ${worktreePath} && cat ${specFile}

2. Read the actual integrated diff (every task's work is sequential commits on this branch):
   Run: cd ${worktreePath} && git diff --stat ${prBase}..HEAD
   Run: cd ${worktreePath} && git diff ${prBase}..HEAD        (read the real changes; spot-check key files)

3. Run the FRESH AUTHORITATIVE checks (this determines the verdict — NOT the per-task tripwire):
   Re-run the FULL gating suite below in order. A fresh failure of any GATING check ALWAYS prevents PASS.

${renderCheckList(harnessCfg, { gatingOnly: false, cwd: worktreePath, engineFiles: [...new Set(taskList.flatMap(n => engineFilesFor(n)))] })}

   Plus the universal emoji gate, DIFF-SCOPED to ${prBase}..HEAD (only lines ADDED across the
   branch are judged, never a whole changed file — same script as the per-task gate above, same
   base): run it again here as the fresh authoritative check (the literal
   "🤖 Generated with Claude Code" footer is allowed only in a PR body, not in docs; the check
   exempts the phrase defensively too).

4. For each acceptance criterion, read the relevant source and mark MET / PARTIAL / NOT_MET. Also check
   CLAUDE.md standing-rule compliance (a violation is a failing criterion) and IDENTITY INTEGRITY (flag
   any handle/URL contradicting CLAUDE.md's verified identities). Do NOT fix environment/infra issues
   yourself — report them as FAIL for the fix loop to resolve.

5. Verdict:
   PASS    — ALL in-scope criteria MET AND every fresh gating check passes.
   PARTIAL — some criteria PARTIAL, or gating passes but some criteria not fully met.
   FAIL    — any criterion NOT_MET, or any fresh gating check fails.

6. localized — set true if the FAIL/PARTIAL issues are small and localized (a bounded fix can close
   them: a few files, clear cause); false if broad/structural (cross-cutting, ambiguous, or needs a
   human re-plan). PASS → localized is irrelevant (set true).

Return via StructuredOutput: verdict, failureReasons, unmetCriteria, localized, reportFile="", notes.
`, withModel({ label: `review-${reviewAttempts}`, schema: REVIEW_SCHEMA, phase: 'Review' }, reviewModel))

    lastReview = reviewResult || { verdict: 'FAIL', failureReasons: ['Review agent returned null'], unmetCriteria: [], localized: false }
    state.review.verdict = lastReview.verdict
    state.review.findings = [...(lastReview.failureReasons || []), ...(lastReview.unmetCriteria || [])]
    log(`Review verdict: ${lastReview.verdict} (attempt ${reviewAttempts}/${MAX_REVIEW_ATTEMPTS})`)

    if (lastReview.verdict === 'PASS') { finalVerdict = 'PASS'; break }

    // FAIL/PARTIAL → triage the findings: localized → bounded fix loop; broad → bail.
    const findingsBlob = [...(lastReview.failureReasons || []), ...(lastReview.unmetCriteria || [])].join('\n') || '(no detail)'
    const tr = await triage(`consolidated review`, reviewAttempts, MAX_REVIEW_ATTEMPTS, findingsBlob, null)
    const broad = lastReview.localized === false || (tr && tr.class === 'MAJOR')
    if (broad || reviewAttempts >= MAX_REVIEW_ATTEMPTS) {
      bailed = true
      bailReason = (tr && tr.bailReason) || `Review ${lastReview.verdict} (${broad ? 'broad/structural' : `after ${MAX_REVIEW_ATTEMPTS} attempts`}): ${findingsBlob.slice(0, 300)}`
      finalVerdict = lastReview.verdict
      state.status = 'blocked'
      state.bail_reason = bailReason
      log(`Review → bail. ${bailReason}`)
      await writeFlowState(`review ${lastReview.verdict} — bail`, `## Review — ${lastReview.verdict} (bail)\n${findingsBlob}`, { cwd: worktreePath })
      break
    }

    // Bounded fix over the integrated tree, then loop back to review.
    log(`Review ${lastReview.verdict} — localized; running a bounded fix (review pass ${reviewAttempts})...`)
    const fixModel = (ESCALATION_MODEL && reviewAttempts === MAX_REVIEW_ATTEMPTS - 1) ? ESCALATION_MODEL : MODEL.fix
    await tracedAgent(`${W}
You are the fix agent for the consolidated review of an /sdlc-flow run. Make the MINIMUM targeted changes
to address ONLY the review's findings — do not re-implement or touch passing criteria. All Bash from the
worktree root.

Review findings to address:
${findingsBlob}

1. Read CLAUDE.md standing rules (cd ${worktreePath} && cat CLAUDE.md).
2. Read only the source files relevant to the findings; make the minimum fix.
3. Add/adjust tests as needed; no emoji; no fabricated metrics.
4. Run the spec's "## Validation Commands" to confirm.
5. Commit on the branch (stage files explicitly — never git add -A):
     cd ${worktreePath} && git commit -m "$(cat <<'EOF'
fix: review pass ${reviewAttempts} for ${blockId}
EOF
)"
   Run: cd ${worktreePath} && git log --oneline -1
Return via StructuredOutput: reportFile="", success=true if applied, filesModified, commitHash, summary, notes.
`, withModel({ label: `review-fix-${reviewAttempts}`, schema: STAGE_SCHEMA, phase: 'Review' }, fixModel))
    await writeFlowState(`review pass ${reviewAttempts}`, `## Review pass ${reviewAttempts}\nAddressed: ${findingsBlob.slice(0, 300)}`, { cwd: worktreePath })
  }
}

// ----------------------------------------------------------------
// Task 5 fold: docs-phase and wrap-up-phase state writes.
//
// Unlike the per-task pass/bail folds (tasks 1-2), Docs and Wrap-up each run EXACTLY ONCE and
// UNCONDITIONALLY when reached (no pass/fail branching) — today's code always calls writeFlowState
// once, right after each agent returns, regardless of outcome. So there is no "only if X" gate in
// either recipe below; the instruction is simply "also do this, in this same turn, after your other
// steps." Research finding (ticket Notes has the full writeup): safe to fold both, because (a) the
// run-state file (stateFile) is deliberately NEVER committed by any agent (see writeFlowState's own
// comment above) — it lives under planning/<blockId>/sdlc/, so it never collides with either
// agent's own git commit step (docs' doc-file commit; wrap-up's vault-aware status.md/state.json
// commit), and (b) --resume only reads stateFile at the TOP of a fresh run, long after either phase
// would have finished writing it — moving the write from "a follow-up agent spawn" to "the last
// step of the same agent's turn" changes nothing about what's on disk by the time any reader looks.
// ----------------------------------------------------------------

// Docs: the state JSON is known in JS EXCEPT the "docs" field (what got patched/created is only
// known to the agent itself, from its own steps 3-4) -- so the payload carries a placeholder object
// there, mirroring the bail_reason placeholder substitution triage() already uses for onBail.
function buildDocsStatePayload() {
  const snapshot = JSON.parse(JSON.stringify(state))
  snapshot.docs = { changed: '__DOCS_CHANGED__', created: '__DOCS_CREATED__' }
  snapshot.tokens = buildTokensBlock()
  return { stateFile, stateJson: JSON.stringify(snapshot, null, 2), worklogFile }
}

function renderDocsStateWriteRecipe(onDone) {
  return `
AFTER completing steps 1-5 above, in THIS SAME turn, ALSO perform this state write:

STEP W1 — run this as ONE Bash call, exactly as written. Do not split it into several calls:
  cd ${worktreePath} && mkdir -p ${blockDir}/sdlc && date -u +%Y-%m-%dT%H:%M:%SZ && { cat ${onDone.stateFile} 2>/dev/null || echo "__NO_STATE__"; }
  The FIRST line of output is NOW. Everything after it is the existing state file, or __NO_STATE__
  when there is none. If that file exists and has a "started_at" value, REUSE it verbatim for
  started_at below. Otherwise started_at = NOW.

STEP W2 — write ${onDone.stateFile} with EXACTLY this JSON, but: (a) inserting two extra top-level
  keys "started_at" (preserved or NOW, per STEP W1) and "updated_at" (NOW) right after "branch", and
  (b) replacing the placeholder top-level "docs" object — currently
  {"changed": "__DOCS_CHANGED__", "created": "__DOCS_CREATED__"} — with the doc files you ACTUALLY
  patched in step 3 (changed[], [] if the "nothing needed changing" branch applied) and created in
  step 2b's BOOTSTRAP MODE (created[], [] otherwise). Valid JSON only (double quotes, no trailing
  commas, no markdown fences). The object to write (verbatim except for those substitutions):
${onDone.stateJson}

STEP W3 — append to ${onDone.worklogFile}. If the file does not exist, first write a header line
  "# Worklog — ${blockId}" then a blank line. Then append a section formatted exactly like this
  (a blank line before it) — "changed"/"created" are the SAME lists you just substituted into
  STEP W2, comma-joined; use "none" if changed is empty; omit the "| Created: ..." clause entirely
  if created is empty:
  ## Docs
  Patched: <changed, comma-joined, or "none">[ | Created: <created, comma-joined>]

STEP W4 — use the Write tool for both files. Do NOT run \`git add\`, \`git commit\`, \`git checkout\`,
  \`git switch\`, or \`git branch\` — this write is disk-only, exactly like writeFlowState(). Set
  stateWritten=true in your StructuredOutput once both files are written to disk.
`
}

// Wrap-up: the state JSON is FULLY known in JS before this call (state.status derives from
// bailed/finalVerdict, both already resolved by the time Phase 5 starts) -- no placeholder needed
// there. Only the worklog entry's "Next: ..." line depends on the agent's own nextFocus.
function buildWrapupStatePayload() {
  state.status = bailed ? 'blocked' : 'done'
  state.tokens = buildTokensBlock()
  const snapshot = JSON.parse(JSON.stringify(state))
  return { stateFile, stateJson: JSON.stringify(snapshot, null, 2), worklogFile }
}

function renderWrapupStateWriteRecipe(onDone) {
  return `
AFTER completing steps 1-5 above, in THIS SAME turn, ALSO perform this state write:

STEP W1 — run this as ONE Bash call, exactly as written. Do not split it into several calls:
  cd ${worktreePath} && mkdir -p ${blockDir}/sdlc && date -u +%Y-%m-%dT%H:%M:%SZ && { cat ${onDone.stateFile} 2>/dev/null || echo "__NO_STATE__"; }
  The FIRST line of output is NOW. Everything after it is the existing state file, or __NO_STATE__
  when there is none. If that file exists and has a "started_at" value, REUSE it verbatim for
  started_at below. Otherwise started_at = NOW.

STEP W2 — write ${onDone.stateFile} with EXACTLY this JSON, but inserting two extra top-level keys
  "started_at" (preserved or NOW, per STEP W1) and "updated_at" (NOW) right after "branch". Valid
  JSON only (double quotes, no trailing commas, no markdown fences). The object to write (verbatim
  except for adding those two timestamp keys):
${onDone.stateJson}

STEP W3 — append to ${onDone.worklogFile}. If the file does not exist, first write a header line
  "# Worklog — ${blockId}" then a blank line. Then append a section formatted exactly like this
  (a blank line before it), with <next> replaced by your own nextFocus value from step 2 above (or
  "(see status.md)" if you did not set one):
  ## Wrap-up — ${finalVerdict}
  Next: <next>

STEP W4 — use the Write tool for both files. Do NOT run \`git add\`, \`git commit\`, \`git checkout\`,
  \`git switch\`, or \`git branch\` — this write is disk-only, exactly like writeFlowState(). Set
  stateWritten=true in your StructuredOutput once both files are written to disk.
`
}

// ================================================================
// PHASE 4: DOCS — surgical /update-docs --patch (gated on PASS)
// ================================================================
if (!bailed && finalVerdict === 'PASS') {
  phase('Docs')
  state.status = 'docs'
  log('Running docs patch (/update-docs --patch over the changed surface)...')

  const docsStatePayload = buildDocsStatePayload()
  const docResult = await tracedAgent(`${W}
You are the documentation agent for the /sdlc-flow pipeline — a surgical /update-docs --patch over only
the surface this run changed. All Bash from the worktree root.

1. Read the committed run-state for the list of files changed across all tasks:
   Run: cd ${worktreePath} && cat ${stateFile}
   Run: cd ${worktreePath} && git diff --stat ${prBase}..HEAD

2. For each changed source file, find docs/*.md that reference it (component/function/route/file names):
   Run: cd ${worktreePath} && grep -rl "<name>" docs/ 2>/dev/null

2b. CHECK — does docs/ have any project-facing docs?
   Run: cd ${worktreePath} && ls docs/ 2>/dev/null | grep -v '^workflows$' | grep '\\.md$' | wc -l
   If the count is 0 (no project docs exist yet), switch to BOOTSTRAP MODE:
   - Read every source file from step 1's git diff (full read, not just stat).
   - Create appropriate reference docs from scratch based on what the source actually contains.
     At minimum: docs/architecture.md (module map, key types, data flow). Add docs/cli.md for
     CLIs, docs/api-reference.md for servers/APIs, docs/pages.md for web apps — as applicable.
   - Create docs/index.md if it does not exist; add a row per created doc.
   - Every new file must include OKF frontmatter (required: type, title, description).
   - Skip step 3 and go directly to step 4 (NEEDS_REVIEW flag check) then step 5 (commit).
   If count > 0: proceed with surgical patch in step 3.

3. Surgically patch ONLY the affected sections (Edit tool — never rewrite whole files). Update changed
   signatures/prop tables/route lists/descriptions; add docs for new public APIs. Never delete documented
   items that still exist. Never edit CLAUDE.md. No emoji.

4. If a top-level architecture/overview/index doc needs changes, FLAG it NEEDS_REVIEW (in the flagged[]
   field) rather than editing it directly.

5. Commit on the branch (stage explicitly — never git add -A):
   If docs were patched:
     cd ${worktreePath} && git add <each doc file>
     cd ${worktreePath} && git commit -m "$(cat <<'EOF'
docs: update docs for ${blockId}
EOF
)"
     cd ${worktreePath} && git log --oneline -1
   If nothing needed changing, make no commit and report success=true with empty changed/created.
${vault.vaulted ? `
   This step almost never touches planning/ (docs live under docs/), but if it genuinely did — e.g. a
   patched/created doc path in changed[]/created[] above starts with "planning/" — that path is a
   vaulted symlink (D46), a DIFFERENT git repo, invisible to the commit you just made. Stage + commit
   it there too, through the real path, deriving the exact set from changed[]/created[] (never a fixed
   list): for each such path, let <relpath> be the part after "planning/":
     cd ${worktreePath} && git -C ${vault.planningPath} add ${vault.planningPath}/<relpath>
     Then commit ONLY those paths — pass them explicitly to \`git commit\` itself (not merely to
     \`git add\`), so a sibling lane's unrelated pre-staged files are never swept into this commit:
     cd ${worktreePath} && git -C ${vault.planningPath} diff --cached --quiet -- <relpath1> <relpath2> ... || git -C ${vault.planningPath} commit -m "$(cat <<'EOF'
docs: update docs for ${blockId} (vault)
EOF
)" -- <relpath1> <relpath2> ...
     cd ${worktreePath} && git -C ${vault.planningPath} log --oneline -1
   NEVER git add -A, git add ., git reset, or git stash against the vault repo, and never checkout/
   switch/branch inside it. If nothing you patched/created lives under planning/, skip this entirely.
` : ''}
${renderDocsStateWriteRecipe(docsStatePayload)}
Return via StructuredOutput: success, changed[], created[], flagged[], commitHash, stateWritten (true
only if you performed the additional state write above), notes.
`, withModel({ label: 'docs', schema: DOCS_SCHEMA, phase: 'Docs' }, MODEL.docs))

  if (docResult) {
    state.docs.changed = docResult.changed || []
    state.docs.created = docResult.created || []
    log(`Docs: ${(docResult.changed || []).length} patched, ${(docResult.created || []).length} created${(docResult.flagged || []).length ? `, ${docResult.flagged.length} flagged NEEDS_REVIEW` : ''}`)
    // Same independent vault-commit verification as the per-task loop — a docs-agent self-report of
    // success/commitHash proves nothing about a planning/ path landing committed in the vault repo.
    const docsVaultRelPaths = vaultRelPathsFrom([...(docResult.changed || []), ...(docResult.created || [])], vault)
    if (docsVaultRelPaths.length) {
      const docsVaultVerify = await verifyVaultCommit(worktreePath, vault, docsVaultRelPaths)
      if (!docsVaultVerify.allCommitted) {
        const uncommitted = (docsVaultVerify.uncommittedPaths && docsVaultVerify.uncommittedPaths.length) ? docsVaultVerify.uncommittedPaths : docsVaultRelPaths
        log(`Docs: vault commit incomplete — not committed in ${vault.planningPath}: ${uncommitted.join(', ')}. Treating docs phase as failed.`)
        docResult.success = false
        docResult.notes = `${docResult.notes ? docResult.notes + ' | ' : ''}VAULT_COMMIT_INCOMPLETE: ${uncommitted.join(', ')} — ${docsVaultVerify.notes || ''}`.trim()
      }
    }
  } else {
    log('Docs agent returned null — continuing to wrap-up.')
  }
  // Reliability net: skip the dedicated writer only when the docs agent itself reports the folded
  // write succeeded; a null result or stateWritten=false falls through so this phase's outcome is
  // never left unpersisted.
  if (docResult && docResult.stateWritten) {
    log('Docs: state write folded into the docs agent\'s own turn — skipped the dedicated state-writer call.')
  } else {
    await writeFlowState('docs', `## Docs\nPatched: ${(state.docs.changed || []).join(', ') || 'none'}${(state.docs.created || []).length ? ` | Created: ${state.docs.created.join(', ')}` : ''}`, { cwd: worktreePath })
  }
}

// ================================================================
// PHASE 5: WRAP-UP → PR
// ================================================================
phase('Wrap-up')
state.status = 'wrapup'

const passedTasks = taskList.filter(n => state.tasks[String(n)]?.status === 'passed')
const stem = `${blockId}`
log(`Wrap-up. Verdict: ${finalVerdict} | passed ${passedTasks.length}/${taskList.length} tasks${bailed ? ` | BAILED: ${bailReason}` : ''}`)

// Wrap-up writes status/log + the D18 amendment log ON THE BRANCH (so the PR is self-contained — no
// deferred ff-merge dance). Sonnet: the human-facing prose + amendment judgment is the work.
//
// D46: when planning/ is a vaulted symlink, `planning/status.md` and `planning/state.json` do not
// live in this repo at all — they live in the brain-owned vault repo at their symlink target. A
// plain `git add planning/status.md` from the worktree root fails ("pathspec is beyond a symbolic
// link"), and the wrong repair is to checkout/commit inside the vault. The right behaviour is to
// stage+commit the vaulted files THROUGH their real path via `git -C <vault>`, on whatever branch
// the vault repo is already on, with no checkout at all — while repo-local files (log.md, the spec)
// stay staged and committed in the invoking repo exactly as before. `vault` was already resolved
// once, before the per-task loop, and is reused here (never a second detectPlanningVault() call).
const wrapupStatePayload = buildWrapupStatePayload()
const wrapupResult = await tracedAgent(`${W}
You are the wrap-up agent for an /sdlc-flow run. Write the human-facing status/log + the D18 amendment log
ON THIS BRANCH (the PR will carry them), then commit. All Bash from the worktree root.

Target:
  Spec:          ${blockId}
  Tasks run:     ${taskList.join(', ')}  (passed: ${passedTasks.join(', ') || 'none'})
  Final verdict: ${finalVerdict}${bailed ? `  (BAILED: ${bailReason})` : ''}
  Run-state:     ${stateFile}  (the authoritative index — read it)

1. Read the run-state, status.md, the spec, and the log:
   cd ${worktreePath} && cat ${stateFile}
   cd ${worktreePath} && cat planning/status.md
   cd ${worktreePath} && cat ${specFile}
   cd ${worktreePath} && head -40 log.md
   cd ${worktreePath} && git log --oneline -20

2. Update planning/status.md (Edit tool, surgical). "Current focus" is APPEND-ONLY narrative — never
   delete or rewrite any existing line under it; a prior block's narrative must survive this edit
   VERBATIM. The one exception: if an existing line already refers to THIS spec ("${blockId}") by name
   (e.g. from an earlier partial run), you may replace only that one line — never the whole section —
   with the update below.
   ${bailed
     ? `- This run BAILED. Keep the spec status "In progress" (or "Blocked" if appropriate). Add ONE
       new line under "Current focus" (or replace this spec's own prior line, per the exception
       above): "${blockId} — BLOCKED: ${bailReason}" — do not touch any other existing line.`
     : `- ${selectedTasks ? `Tasks ${taskList.join(', ')} of "${blockId}" are done.` : `Full spec "${blockId}" is done.`} ${selectedTasks ? 'If tasks remain, keep status "In progress" and add a new line under Current focus pointing at the next task; if this was the last, flip to "Done".' : 'Flip its Status to "Done".'} Add ONE new line under "Current focus" recording this outcome (or replace this spec's own prior line, per the exception above) — do not touch any other existing line.`}
   - Update "Last updated" — run: date +%Y-%m-%d

2b. Flip the block's AUTHORED status in planning/state.json (skip this entire step silently if the
    repo has no planning/state.json). state.json is the authoritative block graph — leaving it stale
    poisons every derived surface, because \`mev emit-state\` reads this field and NEVER infers
    completion from status.md.
    ${bailed
      ? `- This run BAILED — do NOT flip anything. Set blockStatusFlipped to "".`
      : selectedTasks
        ? `- Only proceed if you flipped the spec's status.md status to "Done" above (this was the last task). If tasks remain, leave state.json untouched and set blockStatusFlipped to "".`
        : `- The full spec is done, so proceed.`}
    - Resolve the block's canonical ID from the status.md Progress Table row you just edited (the
      <BlockID> column, or the id that row maps to in state.json). This is the only part of this
      step that stays your judgment call — the mutation itself is scripted below, not an Edit-tool
      diff.
    - VALIDATE-THEN-COMMIT CONTRACT (same as sdlc-task.js's bookkeep stage): the mutation must not
      stand unless it passes the real typed schema check. \`json.load()\` succeeding is NOT schema
      validity — mev deserializes state.json into typed structs, so a scalar where a struct belongs
      parses fine as JSON and fails deserialization for the WHOLE FILE (this is exactly what happened
      2026-08-09 with a string \`origin\` where the schema types it as a struct). Run ONE scripted
      mutation (never the Edit tool) that captures the pre-write bytes, mutates in memory, runs
      \`mev validate-brain --state\` BEFORE and AFTER the write, and rejects — byte-exact rollback —
      any write that introduces diagnostic lines NOT present in the BEFORE baseline. Pre-existing
      corpus errors (e.g. a sibling lane's unrelated breakage) must never block this write — NET-NEW
      only, the same delta-attribution rule the push gate uses under D64. Substitute the id you
      resolved for <RESOLVED_ID> (keep it as the script's sole argv, quoted):
        cd ${worktreePath} && python3 -c "
import json, subprocess, sys, shutil

path = 'planning/state.json'
bid = sys.argv[1]

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

mev_available = shutil.which('mev') is not None

def diagnostics():
    r = subprocess.run(['mev', 'validate-brain', '--state'], capture_output=True, text=True)
    lines = (r.stdout + r.stderr).splitlines()
    return set(l for l in lines if l.strip().startswith('[E_') or l.strip().startswith('[W_'))

if not mev_available:
    with open(path, 'w') as fh:
        json.dump(data, fh, indent=2, ensure_ascii=False)
        fh.write(chr(10))
    print('FLIPPED:' + bid)
    print('UNVALIDATED: mev not on PATH -- schema check skipped, write landed with only json.load-level parsing')
    sys.exit(0)

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
" "<RESOLVED_ID>"
      The script searches EVERY tracks[].blocks[] entry and only ever mutates the one matching block's
      "status" field. Read the script's own stdout AND exit code — do not infer success yourself:
        - "NOT_FOUND" (exit 0) → the file stays byte-unchanged. Report it in notes, do NOT fabricate a
          block entry, and set blockStatusFlipped to "".
        - "FLIPPED:<id>" with NO "UNVALIDATED:" line (exit 0) → mev validated the write and found no
          net-new diagnostics. Set blockStatusFlipped to that id and stateWriteValidated=true.
        - "FLIPPED:<id>" WITH an "UNVALIDATED:" line (exit 0) → mev is not installed; the write landed
          unchecked (json.load-level parse only, matching how the harness degrades other absent
          tooling). Set blockStatusFlipped to that id, stateWriteValidated=false, and copy the
          UNVALIDATED line verbatim into notes — this is a DEGRADE, not a silent pass.
        - "REJECTED:<id>" (exit 1) → the write introduced net-new schema errors and was rolled back;
          state.json on disk is now byte-identical to its content before this step ran. Set
          blockStatusFlipped to "", stateWriteRejected=true, and copy every "NET_NEW:" line verbatim
          into notes. This MUST be reported — never silently swallow it, and do not treat the block as
          closed this run even though earlier logic said to proceed; the spec's status.md edit already
          recorded progress narrative, but the block stays open until a clean write lands on a later
          run.
    - WORKTREE NOTE (decided, not deferred — same as sdlc-task.js and verified here on the worktree
      path specifically, since \`/sdlc-flow\` runs in a worktree far more often than \`/sdlc-task\`
      does): this validation step runs the SAME WAY in worktree mode as in place. \`mev validate-brain
      --state\` reads planning/state.json in THIS repo's working tree directly (\`${worktreePath}\`) — it
      does not need the cross-repo BRAIN_ROOT resolution that makes \`emit-state --write\` unsafe inside
      a linked worktree. Only step 2c's \`emit-state --write\` (regenerating derived surfaces) is
      deferred to merge in worktree mode; this validation is never deferred, in either engine.
    - Set blockStatusFlipped to the block id you closed (or "" if none, or if the write was rejected).

2c. Regenerate derived surfaces via \`mev emit-state --write\`. Run this step whenever this wrap-up
    stage runs at all — it is NOT conditional on "was this the last task" / full-spec completion above:
    step 2 already edited planning/status.md regardless of whether the spec fully completed this run
    (a task-subset run, or a bail, still leaves it changed on disk), so the derived surfaces (status.md
    rollups, /attention boards, wave tables) need resyncing every time, not only on a full close.
    ${useWorktree
      ? `- Do NOT run \`mev emit-state --write\` here: this is a linked git worktree, where emit-state refuses to run. The authored edits are committed on the branch below (step 5); the derived surfaces regenerate on the base branch when this branch merges (/clean-worktree or /close-out --merge-branch run emit-state after integration). Set emitStateRan=false.`
      : `- This run is IN PLACE on branch ${branchName} (in the main repo tree, not an isolated worktree) — emit-state is safe to run right here on the branch, the same way \`git commit\` already lands right here: cd ${worktreePath} && mev emit-state --write . If \`mev\` or brain.toml is absent (standalone repo), skip it silently and set emitStateRan=false; else emitStateRan=true. Do NOT hand-reimplement focus/rollup derivation. (This is separate from the --auto-merge path's own emit-state call in step 5 below, which re-derives again on ${prBase} after the PR merges — that call is unaffected and still runs unconditionally there.)`}

3. Prepend a new log.md entry (newest first):
   ## [run: date +%Y-%m-%d]
   [One paragraph: what was implemented across tasks ${taskList.join(', ')}, the ${finalVerdict} verdict${bailed ? ` and why it bailed (${bailReason})` : ''}, notable decisions. End with "Next: ...".]
   \`\`\`
   [git log --oneline -8 — the commits from this run]
   \`\`\`

4. Living-artifact amendment log (D18): review the run-state's per-task issues/fixes/decisions for genuine
   DEVIATIONS from the spec as written (a task done materially differently, a scope change, a substitution,
   a deferral). Routine success is NOT a deviation. For each, append ONE dated line to the spec's
   "## Amendment Log" (Edit tool, append-only; replace "_No amendments yet._" if it is the first line):
     - YYYY-MM-DD [task N] <what changed vs the spec, and why>
   If the spec has a provenance stub ("**Status:**"/"**Last run:**"), update it. Return the lines in amendments[].

5. Commit (stage explicitly — never git add -A). NEVER run git checkout, git switch, or git branch
   outside this repo's own root (${worktreePath})${vault.vaulted ? ` or the vault's own root (${vault.planningPath})` : ''} —
   if a git add fails, report the failure in notes; do not relocate the commit to make it succeed.
${vault.vaulted ? `
   planning/ is a vaulted symlink (D46) — its bytes live at ${vault.planningPath}, a different repo.
   Stage + commit the vaulted files THERE, via \`git -C\`, on whatever branch that repo is already on.
   Do NOT cd into it and do NOT checkout/switch/branch there:
   cd ${worktreePath} && git -C ${vault.planningPath} add ${vault.planningPath}/status.md
   cd ${worktreePath} && git -C ${vault.planningPath} add ${vault.planningPath}/state.json 2>/dev/null || true
   Then commit ONLY those two paths — pass them explicitly to \`git commit\` itself (not merely to
   \`git add\`), so anything a sibling lane already had staged in this same vault repo is left staged
   and untouched by this commit:
   cd ${worktreePath} && git -C ${vault.planningPath} diff --cached --quiet -- ${vault.planningPath}/status.md ${vault.planningPath}/state.json || git -C ${vault.planningPath} commit -m "$(cat <<'EOF'
chore: wrap up ${stem}
EOF
)" -- ${vault.planningPath}/status.md ${vault.planningPath}/state.json
   cd ${worktreePath} && git -C ${vault.planningPath} log --oneline -1

   Repo-local files stay staged and committed in THIS repo, on this branch, as before:
   cd ${worktreePath} && git add log.md
   cd ${worktreePath} && git add ${specFile} 2>/dev/null || true
   cd ${worktreePath} && git commit -m "$(cat <<'EOF'
chore: wrap up ${stem}
EOF
)"
   cd ${worktreePath} && git log --oneline -1` : `
   planning/ is a plain directory here (not vaulted) — everything commits together as before:
   cd ${worktreePath} && git add planning/status.md log.md
   cd ${worktreePath} && git add planning/state.json 2>/dev/null || true
   cd ${worktreePath} && git add ${specFile} 2>/dev/null || true
   cd ${worktreePath} && git commit -m "$(cat <<'EOF'
chore: wrap up ${stem}
EOF
)"
   cd ${worktreePath} && git log --oneline -1`}
${renderWrapupStateWriteRecipe(wrapupStatePayload)}
Return via StructuredOutput: statusUpdated, devlogUpdated, nextFocus, amendments[], commitHash,
blockStatusFlipped (the state.json block id closed in step 2b, or "" — including when the write was
rejected by validation), stateWriteValidated, stateWriteRejected (step 2b), emitStateRan (step 2c),
stateWritten (true only if you performed the additional state write above), notes.
`, withModel({ label: 'wrap-up', schema: WRAPUP_SCHEMA, phase: 'Wrap-up' }, MODEL.wrapup))

if (wrapupResult?.stateWriteRejected) {
  log(`state.json: write REJECTED — net-new schema error(s) from mev validate-brain --state; rolled back byte-exact, block NOT closed this run. ${wrapupResult?.notes || ''}`)
} else if (wrapupResult?.blockStatusFlipped) {
  log(`state.json: block "${wrapupResult.blockStatusFlipped}" → closed on the branch (${wrapupResult?.stateWriteValidated ? 'validated: mev validate-brain --state, net-new only' : 'UNVALIDATED: mev not available, json.load-level parse only'})${wrapupResult?.emitStateRan ? '; derived surfaces (incl. focus.next) regenerated (mev emit-state --write).' : '; focus.next is DEFERRED — it still points at the pre-close state until /clean-worktree or /close-out --merge-branch runs `mev emit-state --write` on merge.'}`)
}
if (wrapupResult?.amendments?.length) log(`Spec amendments (D18): ${wrapupResult.amendments.length} line(s) appended.`)
log(`Derived surfaces (in-place, this wrap-up): ${wrapupResult?.emitStateRan ? 'regenerated (mev emit-state --write).' : useWorktree ? 'skipped — worktree mode; focus.next stays stale until regenerated on merge.' : 'skipped (mev/brain.toml absent).'}`)

// Final state write (status reflects the terminal state; PR fields filled after creation).
// state.status was already set by buildWrapupStatePayload() above, before the agent call, so the
// folded write (when it happened) persisted the correct terminal status.
// Reliability net: skip the dedicated writer only when the wrap-up agent itself reports the folded
// write succeeded; a null result or stateWritten=false falls through so wrap-up's outcome is never
// left unpersisted.
if (wrapupResult && wrapupResult.stateWritten) {
  log('Wrap-up: state write folded into the wrap-up agent\'s own turn — skipped the dedicated state-writer call.')
} else {
  await writeFlowState(`wrap-up (${finalVerdict})`, `## Wrap-up — ${finalVerdict}\nNext: ${wrapupResult?.nextFocus || '(see status.md)'}`, { cwd: worktreePath })
}

// ----------------------------------------------------------------
// PR creation (the terminal step) — default: open a PR and STOP.
//   --no-pr → skip. On bail → DRAFT PR. --auto-merge → merge + clean (only on success).
// ----------------------------------------------------------------
const isDraft = bailed
let prInfo = null
let prVerify = null
// Default outcome for the --no-pr path: nothing was attempted, so nothing failed either — this
// is an intentional skip, not a stranded branch. See the `stranded` field on the final return.
let prOutcome = 'impossible'
if (!noPr) {
  const handoffTitle = bailed
    ? `[BLOCKED] ${blockId}: ${bailReason.slice(0, 60)}`
    : `${blockId}: ${passedTasks.length} task(s), review ${finalVerdict}`

  prInfo = await tracedAgent(`${W}
You open a pull request for a completed (or bailed) /sdlc-flow run. All Bash from the worktree root.
The branch "${branchName}" already carries every commit (code, state, docs, status/log). The PR body is
the handoff — build it from the committed run-state.

1. Check the gh CLI and the remote:
   cd ${worktreePath} && command -v gh >/dev/null 2>&1 && echo "GH_PRESENT" || echo "GH_ABSENT"
   cd ${worktreePath} && git remote -v | head -1 || echo "NO_REMOTE"

2. If GH_ABSENT or NO_REMOTE → do NOT fail. Set outcome="impossible", ghPresent=(GH_PRESENT?),
   pushed=false, and in notes print the branch name "${branchName}" and manual instructions:
   "Branch ${branchName} is ready. Push it and open a PR manually: git push -u origin ${branchName} && gh pr create --base ${prBase} --head ${branchName}". Then return.

3. Read the run-state for the body:
   cd ${worktreePath} && cat ${stateFile}

4. Push the branch:
   cd ${worktreePath} && git push -u origin ${branchName}
   If this command errors: set outcome="failed", pushed=false, ghPresent=true, put the ACTUAL error
   text in notes, and return — do not attempt step 6. Otherwise set pushed=true and continue.

5. Build the PR body (markdown) from the run-state:
   ## What & why
   [one paragraph from the spec goal + what each task delivered]
   ## Tasks
   [per task: number — status — one-line summary — commit, from state.tasks]
   ## Validation
   [the review verdict (${finalVerdict}) and what the consolidated review re-ran]
   ${bailed ? '## Why this is a DRAFT / blocked\n   [' + bailReason.replace(/[[\]]/g, '') + ' — exactly where and why it stopped, for human pickup]' : '## Remaining / follow-ups\n   [anything deferred, from state + the spec Notes]'}
   ## How it was validated
   [the gating checks the end-review ran]

   End the body with this exact footer line (the ONLY place an emoji is allowed):
   🤖 Generated with Claude Code

6. Create the PR:
   cd ${worktreePath} && gh pr create --base ${prBase} --head ${branchName} ${isDraft ? '--draft ' : ''}--title "$(cat <<'EOF'
${handoffTitle}
EOF
)" --body "$(cat <<'EOF'
<the body you built>
EOF
)"
   If this errors and the error text does NOT say a PR already exists for this branch: set
   outcome="failed", put the actual error text in notes, and return.
   Capture the printed PR URL. Run: cd ${worktreePath} && gh pr view --json number,url 2>/dev/null
   If create succeeded, OR gh reports a PR already exists for this branch (from the create error or
   the view above), set outcome="created" and capture url/number from whichever call returned them.

Return via StructuredOutput: outcome, url, number, draft=${isDraft}, pushed, ghPresent, notes.
`, withModel({ label: 'pr-create', schema: PR_SCHEMA, phase: 'Wrap-up' }, MODEL.pr))

  // Independent verification — a SEPARATE agent turn, run whenever a push/create was actually
  // attempted (outcome != 'impossible'), regardless of what the create agent itself claimed. This
  // is what catches case 3 (a PR that exists but was under-reported as failed) as well as
  // confirming case 2 (a genuine failure) and any 'created' claim — the engine never takes the
  // create agent's word for its own work.
  if (prInfo?.outcome && prInfo.outcome !== 'impossible') {
    prVerify = await tracedAgent(`${W}
You independently verify whether a PR exists for a branch. Do NOT trust any other agent's report of
whether a PR was created — only trust what this command actually returns.
   cd ${worktreePath} && gh pr view ${branchName} --json number,url,state 2>&1; echo "EXIT:$?"
   The branch MUST be passed as the POSITIONAL argument, exactly as above — do NOT use \`--head
   ${branchName}\`. \`--head\` is a \`gh pr list\`-only flag; \`gh pr view --head <branch>\` fails with
   "unknown flag: --head" and exits 1 before it even looks anything up, which makes every genuinely
   created PR misreport as absent. Run the command exactly as written above.
Read the literal number after "EXIT:" as the process exit code — do not infer success from output text.
If exitCode == 0, parse number/url/state from the JSON printed above it. If exitCode != 0, set
url="", number=0, state="".
Return via StructuredOutput: exitCode, url, number, state.
`, withModel({ label: 'pr-verify', schema: PR_VERIFY_SCHEMA, phase: 'Wrap-up' }, MODEL.prVerify))
  }

  const verifiedPr = (prVerify && prVerify.exitCode === 0 && prVerify.number) ? prVerify : null
  if (verifiedPr) {
    prOutcome = 'created'
    state.pr = { url: verifiedPr.url || prInfo?.url || null, number: verifiedPr.number || prInfo?.number || null }
    log(`${isDraft ? 'Draft PR' : 'PR'} opened: ${state.pr.url || '(see gh)'}${state.pr.number ? ` (#${state.pr.number})` : ''}`)
    await writeFlowState(`pr #${state.pr.number || '?'}`, `## PR\n${isDraft ? 'Draft ' : ''}${state.pr.url || ''}`, { cwd: worktreePath })
  } else if (prInfo?.outcome === 'impossible') {
    prOutcome = 'impossible'
    log(`PR not possible in this environment — ${prInfo?.notes || 'gh unavailable; branch is ready for a manual PR.'}`)
  } else {
    prOutcome = 'failed'
    log(`PR creation failed or could not be independently verified — ${prInfo?.notes || 'no usable outcome reported'}. Branch ${branchName} carries the work; open a PR manually: git push -u origin ${branchName} && gh pr create --base ${prBase} --head ${branchName}`)
  }
} else {
  log(`--no-pr — stopping after wrap-up. Branch ${branchName} carries all commits; open a PR manually when ready.`)
}

// ----------------------------------------------------------------
// --auto-merge: merge the PR + clean up + regenerate derived surfaces. ONLY on a clean success (PASS, not bailed).
// ----------------------------------------------------------------
let mergeInfo = null
if (autoMerge && !bailed && finalVerdict === 'PASS' && prOutcome === 'created' && !isDraft) {
  log(`--auto-merge — merging the PR and cleaning up (${useWorktree ? 'worktree' : 'branch'} mode)...`)
  mergeInfo = await tracedAgent(`
You complete an --auto-merge for an /sdlc-flow run. Merge the PR, ${useWorktree
    ? 'then remove the worktree and delete the branch'
    : 'switch back to the base branch and delete the merged local branch'}, then regenerate derived
surfaces on the base. Be careful and report honestly.

Branch:   ${branchName}
${useWorktree ? `Worktree: ${worktreePath}\n` : ''}PR:       ${state.pr?.number ? '#' + state.pr.number : state.pr?.url || '(look it up)'}
Base:     ${prBase}

1. Merge the PR via gh (delete the remote branch as part of the merge):
   gh pr merge ${state.pr?.number || state.pr?.url} --merge --delete-branch
   If gh errors (not mergeable, checks pending), STOP — do NOT clean up. Report merged=false + the error in notes.

2. Bring local ${prBase} up to date (this also moves the working tree onto ${prBase}):
   git checkout ${prBase} && git pull --ff-only
${useWorktree ? `
3. Remove the worktree + delete the local branch (mirrors /clean-worktree teardown):
   git worktree remove ${worktreePath} --force
   git worktree prune
   git branch -D ${branchName} 2>/dev/null || true
   Set worktreeRemoved / branchDeleted accordingly.

4. Verify:
   git worktree list
   git branch --list ${branchName}
` : `
3. Delete the merged local branch (no worktree to remove in branch mode):
   git branch -d ${branchName} 2>/dev/null || git branch -D ${branchName} 2>/dev/null || true
   Set worktreeRemoved=false, branchDeleted accordingly.

4. Verify:
   git branch --show-current   (should print ${prBase})
   git branch --list ${branchName}
`}
5. Regenerate derived surfaces on ${prBase} (you are now on ${prBase} in the main tree — emit-state is safe here):
   mev emit-state --write
   This re-derives the one-way surfaces (focus, rollups, cache synced_from watermarks, tier tables,
   the HQ Operating Board, master-plan wave tables) from the authored state.json block-status flip the
   merge just landed. If \`mev\` or brain.toml is absent (a standalone repo), skip it silently and set
   emitStateRan=false; else emitStateRan=true. Do NOT hand-reimplement any derived surface. If it warns
   W_EMIT_NO_SENTINEL, surface it in notes rather than hand-authoring the sentinel.

Return via StructuredOutput: merged, worktreeRemoved, branchDeleted, emitStateRan, notes.
`, withModel({ label: 'auto-merge', schema: MERGE_SCHEMA, phase: 'Wrap-up' }, MODEL.merge))
  if (mergeInfo?.merged) {
    log(`Merged into ${prBase}.${useWorktree ? ` Worktree ${mergeInfo.worktreeRemoved ? 'removed' : 'NOT removed'};` : ''} branch ${mergeInfo.branchDeleted ? 'deleted' : 'kept'}; emit-state ${mergeInfo.emitStateRan ? 'ran' : 'skipped'}.`)
  } else {
    log(`Auto-merge did not complete: ${mergeInfo?.notes || 'unknown'}. ${useWorktree ? `Worktree left intact at ${worktreePath}.` : `Branch ${branchName} left intact.`}`)
  }
} else if (autoMerge) {
  log(`--auto-merge skipped: ${bailed ? 'run bailed' : finalVerdict !== 'PASS' ? `verdict ${finalVerdict}` : `no PR created (outcome: ${prOutcome})`}. ${useWorktree ? 'Worktree' : 'Branch'} left intact for review.`)
}

// `stranded`: the one checkable signal a caller needs to tell "clean completion" apart from "the
// PR stage was attempted and failed, or could not be independently verified" — the case that used
// to come back indistinguishable from a completed run (pr: null, merged: false, no non-zero
// signal). `prOutcome === 'impossible'` (no gh / no remote) is deliberately NOT stranded — that is
// the degradation path that must keep working in a standalone repo. `bailed` runs are already
// surfaced via the `bailed` field, so `stranded` here is specifically the un-bailed, silently-
// incomplete case `.claude/commands/orchestrate.md` step 7 must stop the chain on.
const stranded = prOutcome === 'failed'

// ----------------------------------------------------------------
const tokensBlock = buildTokensBlock()
log(`Token roll-up: ${tokensBlock.total.inTokEst} inTokEst${tokensBlock.total.outTok ? ` | ${tokensBlock.total.outTok} outTok` : ''} across ${tokensBlock.stages.length} stage(s) — persisted in ${stateFile}.`)
log(`/sdlc-flow complete. Verdict: ${finalVerdict} | tasks passed: ${passedTasks.length}/${taskList.length}${bailed ? ` | BAILED: ${bailReason}` : ''}${prOutcome === 'created' ? ` | PR: ${state.pr?.url || state.pr?.number}` : ''}${stranded ? ' | STRANDED: PR stage failed or unverified — branch left intact, chain should stop.' : ''}`)
if (!noPr && !autoMerge) log('Next: run /close-out to verify coverage + patch docs before handing off.')

return {
  blockId,
  branch: branchName,
  mode: useWorktree ? 'worktree' : 'branch',
  worktreePath,
  finalVerdict,
  bailed,
  bailReason: bailReason || null,
  tasksRun: taskList,
  tasksPassed: passedTasks,
  review: state.review,
  docs: state.docs,
  // prOutcome: 'created' | 'impossible' | 'failed' — see PR_SCHEMA above for the vocabulary.
  // 'impossible' (no gh / no remote) is expected and NOT a failure. 'failed' means a PR was
  // attempted and either errored or could not be independently verified via `gh pr view` — the
  // engine no longer takes the pr-create agent's self-report on faith.
  prOutcome,
  pr: prOutcome === 'created' ? { url: state.pr?.url || null, number: state.pr?.number || null, draft: isDraft } : null,
  merged: mergeInfo?.merged || false,
  // stranded: true iff prOutcome === 'failed' — the one field a chain driver (`/orchestrate` step 7)
  // must check alongside `bailed` before treating this run as a clean completion. See the comment
  // where `stranded` is computed above.
  stranded,
  stateFile,
  worklogFile,
  tokens: tokensBlock,
}
