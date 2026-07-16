// =============================================================================
// sdlc-flow — single-branch, single-review, PR-terminating SDLC engine
// =============================================================================
//
// The default engine for non-trivial feature work. Runs one spec's tasks
// SEQUENTIALLY on a SINGLE shared branch (so there are no inter-task merges to
// conflict — sdlc-block's #1 failure mode), with a per-task test→fix loop, ONE
// consolidated review at the end, a docs patch, and a PR as the terminal step.
//
// ISOLATION MODE
//   Default: a plain branch (<spec>-flow) checked out IN THE MAIN WORKING TREE. No
//   sparse-checkout worktree, so a relative planning/ symlink (brain-vaulted repos)
//   stays intact. main is left on the branch until the PR merges.
//   --worktree: the isolated sparse-checkout worktree under trees/<spec>-flow/ —
//   opt in when you need true isolation (e.g. /sdlc-block fans out parallel children).
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

const autoMergeFlag = hasFlag('--auto-merge')
const noPr          = hasFlag('--no-pr')
const resumeMode    = hasFlag('--resume')
// Isolation mode: default runs on a plain branch checked out in the MAIN working tree (keeps a relative
// planning/ symlink intact — worktrees break it). --worktree opts back into the isolated sparse-checkout
// worktree (needed for true parallelism — e.g. /sdlc-block fans out concurrent children).
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

const blockDir      = `planning/${blockId}`
const specFile      = `${blockDir}/tasks.md`
const tasksJsonFile = `${blockDir}/tasks.json`
const breakdownFile = `${blockDir}/breakdown.md`
const reportsDir    = `${blockDir}/sdlc/reports`
const stateFile     = `${blockDir}/sdlc/sdlc-flow-state.json`   // COMMITTED authoritative run index (D31)
const worklogFile   = `${blockDir}/sdlc/worklog.md`            // COMMITTED human-readable trail (D31)
const baseBranchName = `${blockId}-flow`                        // one shared branch for the whole spec

const MAX_TASK_ATTEMPTS   = 3   // implement→test→fix attempts per task before bail
const MAX_REVIEW_ATTEMPTS = 3   // consolidated-review fix passes before bail

log(`Target: ${blockId} (${selectedTasks ? [...selectedTasks].sort((a, b) => a - b).join(', ') : 'all tasks'})`)
log(`Spec: ${specFile} | branch: ${baseBranchName} | mode: ${useWorktree ? 'worktree' : 'branch'}${resumeMode ? ' | RESUME' : ''}`)

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
    specFileExists: { type: 'boolean', description: 'true if the task spec file exists in the worktree' },
    blockStatus:    { type: 'string', description: "This spec's Status in status.md (title-case), or 'Unknown'" },
    specThin:       { type: 'boolean', description: 'D19: true ONLY on a fresh run (wasCreated && specFileExists) with a structurally-valid but substantively-thin spec. false on resume or a healthy spec.' },
    thinReason:     { type: 'string', description: 'D19: the specific thin-spec failures when specThin; empty string otherwise.' },
    setupError:     { type: 'string', description: 'Non-empty when setup could not proceed safely (e.g. branch mode aborted on a dirty working tree). The engine aborts and reports this. Empty string on success.' },
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
    notes:    { type: 'string' }
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
// human now). Mirrors sdlc-block's TRIAGE_SCHEMA + the immediate-bail reason set.
const TRIAGE_SCHEMA = {
  type: 'object',
  required: ['class', 'reason'],
  properties: {
    class:               { type: 'string', enum: ['RETRYABLE', 'MAJOR'] },
    reason:              { type: 'string', description: 'One sentence: why retryable (transient/changed/progressing) or major (one of the immediate-bail reasons, stuck, or structural)' },
    bailReason:          { type: 'string', description: 'When class=MAJOR: a short human-readable reason for the draft-PR handoff; empty when RETRYABLE' },
    sameFailureAsBefore: { type: 'boolean', description: 'true if the SAME failure as the previous attempt (no progress)' }
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
    blockStatusFlipped: { type: 'string', description: 'The state.json tracks[].blocks[].id flipped to "closed" on the branch this run, or "" if none (spec not fully done, no state.json, or block not found).' },
    notes:         { type: 'string' }
  }
}

const PR_SCHEMA = {
  type: 'object',
  required: ['created'],
  properties: {
    created:   { type: 'boolean', description: 'true if a PR was created (or gh reported one already exists)' },
    url:       { type: 'string', description: 'the PR URL, or "" if not created' },
    number:    { type: 'integer', description: 'the PR number, or 0 if not created' },
    draft:     { type: 'boolean', description: 'true if a draft PR (a bail handoff)' },
    pushed:    { type: 'boolean', description: 'true if the branch was pushed to the remote' },
    ghPresent: { type: 'boolean', description: 'true if the gh CLI was available' },
    notes:     { type: 'string', description: 'when not created: the branch name + manual instructions printed for the user' }
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
    written:   { type: 'boolean', description: 'true if state.json (+ worklog.md) were written and committed' },
    commitHash:{ type: 'string' },
    notes:     { type: 'string' }
  }
}

// ----------------------------------------------------------------
// MODEL TIERING — the primary token lever for this pipeline.
//
// Without this map every stage inherits the SESSION model — so launching /sdlc-flow from an Opus
// session silently runs the mechanical stages on Opus too. Principle (mirrors sdlc-run/task): match
// the model to the work. To re-tier, change one value here — nothing else moves.
// Valid values: 'haiku' | 'sonnet' | 'opus' | undefined (inherit session model).
// ----------------------------------------------------------------
const MODEL = {
  worktreeSetup: 'haiku',    // scripted git following an exact free-name + sparse-checkout recipe
  enumerate:     'haiku',    // read + parse tasks.json's task list — a fixed procedure
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
                  kind:    { type: 'string', description: 'command (default) | baseline-diff | count-delta | warning-scan | forbidden-pattern-scan' },
                  name:    { type: 'string' },
                  command: { type: 'string' },
                  purpose: { type: 'string' },
                  gates:   { type: 'boolean' },
                  baselineCommand: { type: 'string' },
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
    these fields when present: stack; validation.checks[] (each: {kind, name, command, purpose, gates}
    plus any kind-specific fields present — baselineCommand, compareKeys[], countPattern, failOn,
    warningPatterns[], rules[] ({id, pattern, paths, allowlistPattern})); flow ({autoMerge, testDepth,
    prBase, bailReasons[]}). Preserve kind-specific fields verbatim; ignore any other fields.

Return your findings using the StructuredOutput tool.
`, { label: 'harness-config', schema: HARNESS_CONFIG_SCHEMA, model: 'sonnet' })

  if (!result || !result.present || !result.config) return null
  return result.config
}

// Render the inner project-validation check list for a Test stage. When gatingOnly is true (the fast
// per-task tripwire), emit only the checks with gates:true; the end-review runs the FULL suite. When
// the config is absent (or carries no checks), fall back to the spec's `## Validation Commands` — the
// engine ships NO stack defaults. Handles all D6 check kinds.
function renderCheckList(cfg, { gatingOnly = false, cwd } = {}) {
  let checks = cfg?.validation?.checks ?? []
  if (gatingOnly) checks = checks.filter(c => c.gates)
  const cd = cwd ? `cd ${cwd} && ` : ''
  if (!checks.length) {
    return `The project ships no matching \`planning/harness.json\` validation ${gatingOnly ? 'GATING ' : ''}checks, so derive the checks from the spec instead:
  - Read the spec's optional "## Validation Commands" section.
  - Run each command it lists, IN ORDER (prefix each Bash call with: ${cd}). Each command is one check —
    record its name, the command, passed (true iff exit code 0), and the output on failure.
  - If the spec has no "## Validation Commands" section, run no project checks — record a single
    informational row (name "no_validation_suite", passed true) noting the project declared none.`
  }
  return checks.map((c, i) => {
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
    return `${header}:
  ${cd}${c.command}
  echo "CHECK${n}_EXIT:$?"`
  }).join('\n\n')
}

// Snapshot baseline artifacts for any baseline-diff checks before the first task, so the test stages
// can diff current output vs the pre-run state and fail only on net-new items. Resume-safe: only
// writes a baseline that does not already exist. No-op when no baseline-diff checks are configured.
async function snapshotBaselines(cfg, cwd) {
  const checks = (cfg?.validation?.checks || []).filter(c => c.kind === 'baseline-diff' && c.baselineCommand)
  if (!checks.length) return
  const steps = checks.map(c => {
    const slug = (c.name || 'check').toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '')
    const path = `${reportsDir}/${slug}-baseline.json`
    return `Baseline "${c.name}" -> ${path}:
  cd ${cwd} && mkdir -p ${reportsDir}
  cd ${cwd} && { [ -f ${path} ] && echo "BASELINE EXISTS (kept): ${path}" || { ${c.baselineCommand} > ${path} 2>/dev/null; echo "BASELINE WRITTEN: ${path}"; } ; }`
  }).join('\n\n')
  await agent(`
You are the baseline-snapshot agent for the SDLC pipeline. Capture the pre-run baseline for each
baseline-diff validation check BEFORE any implementation runs. Run each block exactly as written.
Do NOT modify source. Existing baselines are kept (resume-safe).

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

// Persist `state` to the committed state.json + append `worklogEntry` (markdown) to worklog.md, then
// commit both on the branch. `label` names the commit. `extraAdd` lists any other paths to stage
// (e.g. the tasks.md checkbox edit was made by an upstream agent on the branch already).
async function writeFlowState(label, worklogEntry, { cwd, extraAdd = [] } = {}) {
  state.tokens = buildTokensBlock()   // Block A — refresh the committed token roll-up before persisting
  const stateJson = JSON.stringify(state, null, 2)
  const addList = ['planning/' + blockId + '/sdlc/sdlc-flow-state.json', 'planning/' + blockId + '/sdlc/worklog.md', ...extraAdd]
  const result = await agent(`
You maintain the COMMITTED, authoritative run-state for an /sdlc-flow pipeline. You run from the
WORKTREE root. Write two files and commit them — do not run checks, edit source, or touch anything else.

STEP 1 — timestamps + preserved start time (from the worktree root):
  cd ${cwd} && NOW=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  cd ${cwd} && cat ${stateFile} 2>/dev/null || echo "__NO_STATE__"
  If that file exists and has a "started_at" value, REUSE it verbatim. Otherwise started_at = NOW.

STEP 2 — ensure the dir exists:
  cd ${cwd} && mkdir -p ${blockDir}/sdlc

STEP 3 — write ${stateFile} with EXACTLY this JSON, but inserting two extra top-level keys
  "started_at" (preserved or NOW) and "updated_at" (NOW) right after "branch". Valid JSON only
  (double quotes, no trailing commas, no markdown fences). The object to write (verbatim except for
  adding those two timestamp keys):
${stateJson}

STEP 4 — append to ${worklogFile}. If the file does not exist, first write a header line
  "# Worklog — ${blockId}" then a blank line. Then append this section verbatim (a blank line before it):
${worklogEntry ? '```\n' + worklogEntry + '\n```' : '(no worklog entry this write — skip the append)'}

STEP 5 — commit on the branch (never git add -A; stage explicitly):
  cd ${cwd} && git add ${addList.join(' ')}
  cd ${cwd} && git commit -m "$(cat <<'EOF'
chore: flow state — ${label}
EOF
)" || echo "NOTHING_TO_COMMIT"
  cd ${cwd} && git log --oneline -1

Use the Write tool for both files. Return via StructuredOutput: written=true on success, commitHash from
the final git log line (empty string if nothing was committed).
`, withModel({ label: `state:${label}`, schema: STATE_WRITE_SCHEMA }, MODEL.stateWriter))
  if (!result || !result.written) {
    log(`(state) could not persist flow state for "${label}" — continuing`)
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
       if [ -f .env ]; then cp .env trees/${baseBranchName}/.env; fi
       if [ -f .env.local ]; then cp .env.local trees/${baseBranchName}/.env.local; fi
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
  f. if [ -f .env ]; then cp .env trees/[branchName]/.env; fi
  g. if [ -f .env.local ]; then cp .env.local trees/[branchName]/.env.local; fi
  h. git -C trees/[branchName] commit --allow-empty -m "chore: init worktree [branchName]"

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
  Spec:       ${blockId}
  Base name:  ${baseBranchName}

STEP 1 — Get the absolute repo root:
  Run: git rev-parse --show-toplevel
  Store the trimmed output as repoRoot.
${useWorktree ? worktreeRecipe : branchRecipe}

STEP 6 — Report pipeline-start inputs (run these from the live checkout):
  a. Spec file:
       cd ${setupWorkdir} && ls ${specFile} 2>/dev/null && echo "SPEC_EXISTS" || echo "SPEC_MISSING"
     specFileExists = true iff "SPEC_EXISTS" printed.
  b. Block status — find this spec's row in status.md:
       cd ${setupWorkdir} && grep -iE "${blockId}" planning/status.md | head -5
     blockStatus = the title-case Status value (Not started / In progress / Done / Blocked / Skipped),
     or "Unknown" if no row is found.
  c. Thin-spec check (D19) — ONLY when wasCreated AND specFileExists (a fresh run about to spend
     implement tokens; skip on resume). Set specThin=true ONLY on these high-confidence signals (a
     blocked valid spec is far costlier than a missed thin one — when in doubt do NOT flag):
       - cd ${setupWorkdir} && grep -n '{{' ${specFile}  → any unfilled {{TOKEN}} is thin.
       - The '## Acceptance Criteria' section has no real '- ' bullet (empty, or only a template seed) → thin.
     Do NOT flag bare 'TODO'/'TBD' prose, do NOT treat '<...>' as a token (legitimate in 'Vec<T>', globs),
     never flag the Amendment Log seed '_No amendments yet._'. Else specThin=false, thinReason="".

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

// D19 — thin-spec guard for a fresh run.
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
  log(`Spec file ${specFile} not found in the worktree. /sdlc-flow expects an authored spec.`)
  log(`Fix: run /generate-tasks ${blockId} (and /breakdown) on main, commit, then re-run /sdlc-flow ${blockId}.`)
  return { error: 'Missing spec', blockId, specFile }
}

const enumResult = await tracedAgent(`${W}
You enumerate the tasks defined in a spec's tasks.json. Do NOT modify anything.

STEP 1 — read the task list:
  cd ${worktreePath} && cat ${tasksJsonFile} 2>/dev/null || echo "NO_TASKS_JSON"

STEP 2 — Parse it as JSON. It is a BARE ARRAY (not wrapped in an object — matches orchestrator's
  SDLCTask schema). Collect every task's "task_id" (in array order) into allTasks.
  Set hasTasks=true iff it parsed as an array with at least one entry.

Return via StructuredOutput: hasTasks, allTasks (integers in order), notes.
`, withModel({ label: 'enumerate', schema: ENUMERATE_SCHEMA, phase: 'Plan' }, MODEL.enumerate))

if (!enumResult || !enumResult.hasTasks || !(enumResult.allTasks || []).length) {
  // D16 preflight lint — refuse to guess the task structure.
  log(`ABORTED (D16) — ${tasksJsonFile} is missing, invalid, or is an empty array.`)
  log(`Fix: run /generate-tasks ${blockId} to author tasks.json (see the spec template), commit, then re-run.`)
  return { error: 'No tasks.json (D16)', blockId, specFile: tasksJsonFile }
}

const allTasks = enumResult.allTasks
let taskList = selectedTasks ? allTasks.filter(n => selectedTasks.has(n)) : allTasks.slice()
log(`Tasks in spec: ${allTasks.join(', ')}${selectedTasks ? ` | selected: ${taskList.join(', ')}` : ''}`)

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
async function runTests(label, { gatingOnly }) {
  return tracedAgent(`${W}
You are the test agent for the /sdlc-flow pipeline. Run the project's validation checks and report.

IMPORTANT — run ONLY the checks enumerated below (from planning/harness.json + the spec). Do NOT invent
checks. All Bash calls run from the worktree root (prefix each with: cd ${worktreePath} &&).

${renderCheckList(harnessCfg, { gatingOnly, cwd: worktreePath })}

Then run the universal emoji gate (a harness rule, always): scan the files changed on this branch for
emoji in markdown/docs (excluding the literal "🤖 Generated with Claude Code" PR footer if present).
  cd ${worktreePath} && git diff --name-only ${prBase}..HEAD
  Inspect the changed .md files; a stray emoji in docs FAILS this gate.

For each check record: name, passed (true iff exit code 0), the command, and failure output.
Return via StructuredOutput: allPassed (true only if EVERY check passed), passCount, failCount,
failedTests (names), failBlob (compact: failing check names + the tail of their output; empty when allPassed).
`, withModel({ label, schema: TEST_SCHEMA, phase: 'Tasks' }, MODEL.test))
}

// ----------------------------------------------------------------
// Triage helper (shared by the per-task loop + the review fix loop)
// ----------------------------------------------------------------
async function triage(context, attempt, maxAttempts, failBlob, sameContext) {
  return tracedAgent(`
You are the failure-triage agent for an /sdlc-flow run. Classify a failure so the pipeline either makes
a bounded fix or bails to a human NOW. Bailing is cheap; a wasted retry loop is not — when unsure, BAIL.

Context: ${context} (attempt ${attempt} of ${maxAttempts}).
Failure detail:
${failBlob || '(no detail captured)'}

IMMEDIATE-BAIL reasons — if the failure is ANY of these, class=MAJOR and put a short human-readable
bailReason describing which one and where:
${BAIL_REASONS}

Otherwise:
  RETRYABLE — transient/infra (agent died, flaky), OR the failure CHANGED from the previous attempt
              (it is making progress and a bounded fix can plausibly close it).
  MAJOR     — the SAME failure again with no progress, OR structural (one of the bail reasons above).

Return via StructuredOutput: class, reason, bailReason (empty when RETRYABLE), sameFailureAsBefore.
${sameContext ? `(Previous attempt context for the same-failure check: ${sameContext})` : ''}
`, withModel({ label: `triage:${context}:${attempt}`, schema: TRIAGE_SCHEMA, phase: 'Tasks' }, MODEL.triage))
}

// ================================================================
// PHASE 2: PER-TASK LOOP (sequential, in the one shared worktree)
// ================================================================
phase('Tasks')

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
  Spec file:   ${specFile} (prose — Goal, Acceptance Criteria, Validation Commands)
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

Return via StructuredOutput:
  reportFile: ""   (flow keeps state in state.json, not per-stage reports)
  success: true if the work completed and the spec validation passed
  filesModified: every source file you created or modified this attempt
  commitHash: the 7-char short hash (empty string if no commit was made)
  summary: one line — what this task now does
  decisions: any non-obvious choices (empty array if none)
  notes: one-line status
`, withModel({ label: `${isFix ? 'fix' : 'implement'}-${taskNum}-${attempt}`, schema: STAGE_SCHEMA, phase: 'Tasks' }, isFix ? fixModel : MODEL.implement))

    if (!stageResult) {
      log(`Task ${taskNum} attempt ${attempt}: agent returned null.`)
      const tr = await triage(`task ${taskNum} implement`, attempt, MAX_TASK_ATTEMPTS, 'NULL_RESULT — the agent died or returned nothing.', prevFailBlob)
      if (tr && tr.class === 'MAJOR') { bailed = true; bailReason = tr.bailReason || tr.reason || 'agent returned null'; break }
      continue
    }
    if (stageResult.commit) t.commit = stageResult.commit
    if (stageResult.summary) t.summary = stageResult.summary
    if (Array.isArray(stageResult.filesModified)) t.files_changed = [...new Set([...(t.files_changed || []), ...stageResult.filesModified])]
    if (Array.isArray(stageResult.decisions) && stageResult.decisions.length) t.decisions = [...(t.decisions || []), ...stageResult.decisions]

    // 3. Fast test (tripwire) — gating checks only unless testDepth=full.
    const testResult = await runTests(`test-${taskNum}-${attempt}`, { gatingOnly: testDepth === 'fast' })
    if (testResult && testResult.allPassed) {
      t.validated = testDepth === 'fast' ? 'gating checks (fast tripwire)' : 'full gating suite'
      taskPassed = true
      break
    }

    // 5. Failure → triage.
    const failBlob = (testResult && testResult.failBlob) || `Test stage failed or returned null (failCount=${testResult?.failCount ?? '?'}, failed=${(testResult?.failedTests || []).join(', ')}).`
    t.issues = [...(t.issues || []), ...((testResult?.failedTests) || [])]
    const tr = await triage(`task ${taskNum} test`, attempt, MAX_TASK_ATTEMPTS, failBlob, prevFailBlob)
    prevFailBlob = failBlob
    if (tr && tr.class === 'MAJOR') {
      bailed = true
      bailReason = tr.bailReason || tr.reason || `Task ${taskNum}: ${(testResult?.failedTests || []).join(', ')}`
      log(`Task ${taskNum}: triage → MAJOR — bailing immediately (not burning the remaining attempts). Reason: ${bailReason}`)
      break
    }
    if (attempt === MAX_TASK_ATTEMPTS) {
      bailed = true
      bailReason = `Task ${taskNum} still failing after ${MAX_TASK_ATTEMPTS} attempts: ${(testResult?.failedTests || []).join(', ')}`
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
  await writeFlowState(`task ${taskNum} ${t.status}`, worklogEntry, { cwd: worktreePath, extraAdd: [specFile] })

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
  Spec file:   ${specFile}
  Tasks run:   ${taskList.join(', ')}
  Base branch: ${prBase}

The committed run-state is your INDEX (per-task summary/issues/fixes/decisions/files) — read it first,
but it does NOT replace verifying the criteria against the code:
  cd ${worktreePath} && cat ${stateFile}

1. Read the spec's COMPLETE "## Acceptance Criteria" — this is your checklist.
   Run: cd ${worktreePath} && cat ${specFile}

2. Read the actual integrated diff (every task's work is sequential commits on this branch):
   Run: cd ${worktreePath} && git diff --stat ${prBase}..HEAD
   Run: cd ${worktreePath} && git diff ${prBase}..HEAD        (read the real changes; spot-check key files)

3. Run the FRESH AUTHORITATIVE checks (this determines the verdict — NOT the per-task tripwire):
   Re-run the FULL gating suite below in order. A fresh failure of any GATING check ALWAYS prevents PASS.

${renderCheckList(harnessCfg, { gatingOnly: false, cwd: worktreePath })}

   Plus the universal emoji gate: scan changed .md files for stray emoji (the literal
   "🤖 Generated with Claude Code" footer is allowed only in a PR body, not in docs).

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

// ================================================================
// PHASE 4: DOCS — surgical /update-docs --patch (gated on PASS)
// ================================================================
if (!bailed && finalVerdict === 'PASS') {
  phase('Docs')
  state.status = 'docs'
  log('Running docs patch (/update-docs --patch over the changed surface)...')

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

Return via StructuredOutput: success, changed[], created[], flagged[], commitHash, notes.
`, withModel({ label: 'docs', schema: DOCS_SCHEMA, phase: 'Docs' }, MODEL.docs))

  if (docResult) {
    state.docs.changed = docResult.changed || []
    state.docs.created = docResult.created || []
    log(`Docs: ${(docResult.changed || []).length} patched, ${(docResult.created || []).length} created${(docResult.flagged || []).length ? `, ${docResult.flagged.length} flagged NEEDS_REVIEW` : ''}`)
  } else {
    log('Docs agent returned null — continuing to wrap-up.')
  }
  await writeFlowState('docs', `## Docs\nPatched: ${(state.docs.changed || []).join(', ') || 'none'}${(state.docs.created || []).length ? ` | Created: ${state.docs.created.join(', ')}` : ''}`, { cwd: worktreePath })
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

2. Update planning/status.md (Edit tool, surgical):
   ${bailed
     ? `- This run BAILED. Keep the spec status "In progress" (or "Blocked" if appropriate). Set "Current focus" to: "${blockId} — BLOCKED: ${bailReason}".`
     : `- ${selectedTasks ? `Tasks ${taskList.join(', ')} of "${blockId}" are done.` : `Full spec "${blockId}" is done.`} ${selectedTasks ? 'If tasks remain, keep status "In progress" and point Current focus at the next task; if this was the last, flip to "Done".' : 'Flip its Status to "Done".'} Update "Current focus" accordingly.`}
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
      <BlockID> column, or the id that row maps to in state.json). Find that block in state.json
      tracks[].blocks[] — search EVERY track. If found, set its "status" to "closed" (the only
      authored close value). If NOT found, report it in notes and do NOT fabricate a block entry.
    - Validate the file is still valid JSON:
        cd ${worktreePath} && python3 -c "import json;json.load(open('planning/state.json'))"
    - Do NOT run \`mev emit-state --write\` here. ${useWorktree
        ? 'This is a linked git worktree, where emit-state refuses to run.'
        : 'This run is on a feature branch, not the base branch — emit-state must run on the base, and only after the branch merges (deriving from the branch checkout would be wrong).'} The authored flip is
      committed on the branch below (step 5); the derived surfaces regenerate on the base branch when
      this branch merges (/clean-worktree, /merge-train, or /close-out --merge-branch run emit-state
      after integration).
    - Set blockStatusFlipped to the block id you closed (or "" if none).

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

5. Commit on the branch (stage explicitly — never git add -A):
   cd ${worktreePath} && git add planning/status.md log.md
   cd ${worktreePath} && git add planning/state.json 2>/dev/null || true
   cd ${worktreePath} && git add ${specFile} 2>/dev/null || true
   cd ${worktreePath} && git commit -m "$(cat <<'EOF'
chore: wrap up ${stem}
EOF
)"
   cd ${worktreePath} && git log --oneline -1

Return via StructuredOutput: statusUpdated, devlogUpdated, nextFocus, amendments[], commitHash,
blockStatusFlipped (the state.json block id closed in step 2b, or ""), notes.
`, withModel({ label: 'wrap-up', schema: WRAPUP_SCHEMA, phase: 'Wrap-up' }, MODEL.wrapup))

if (wrapupResult?.amendments?.length) log(`Spec amendments (D18): ${wrapupResult.amendments.length} line(s) appended.`)
if (wrapupResult?.blockStatusFlipped) log(`state.json: block "${wrapupResult.blockStatusFlipped}" → closed on the branch; derived surfaces regenerate on merge (/clean-worktree, /merge-train, or /close-out --merge-branch).`)

// Final state write (status reflects the terminal state; PR fields filled after creation).
state.status = bailed ? 'blocked' : 'done'
await writeFlowState(`wrap-up (${finalVerdict})`, `## Wrap-up — ${finalVerdict}\nNext: ${wrapupResult?.nextFocus || '(see status.md)'}`, { cwd: worktreePath })

// ----------------------------------------------------------------
// PR creation (the terminal step) — default: open a PR and STOP.
//   --no-pr → skip. On bail → DRAFT PR. --auto-merge → merge + clean (only on success).
// ----------------------------------------------------------------
let prInfo = null
if (!noPr) {
  const isDraft = bailed
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

2. If GH_ABSENT or NO_REMOTE → do NOT fail. Set created=false, ghPresent=(GH_PRESENT?), pushed=false, and
   in notes print the branch name "${branchName}" and manual instructions:
   "Branch ${branchName} is ready. Push it and open a PR manually: git push -u origin ${branchName} && gh pr create --base ${prBase} --head ${branchName}". Then return.

3. Read the run-state for the body:
   cd ${worktreePath} && cat ${stateFile}

4. Push the branch:
   cd ${worktreePath} && git push -u origin ${branchName}
   Set pushed=true on success.

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
   Capture the printed PR URL. Run: cd ${worktreePath} && gh pr view --json number,url 2>/dev/null || true
   If gh reports a PR already exists for this branch, treat created=true and capture its url/number.

Return via StructuredOutput: created, url, number, draft=${isDraft}, pushed, ghPresent, notes.
`, withModel({ label: 'pr-create', schema: PR_SCHEMA, phase: 'Wrap-up' }, MODEL.pr))

  if (prInfo?.created) {
    state.pr = { url: prInfo.url || null, number: prInfo.number || null }
    log(`${prInfo.draft ? 'Draft PR' : 'PR'} opened: ${prInfo.url || '(see gh)'}${prInfo.number ? ` (#${prInfo.number})` : ''}`)
    await writeFlowState(`pr #${prInfo.number || '?'}`, `## PR\n${prInfo.draft ? 'Draft ' : ''}${prInfo.url || ''}`, { cwd: worktreePath })
  } else {
    log(`PR not created — ${prInfo?.notes || 'gh unavailable; branch is ready for a manual PR.'}`)
  }
} else {
  log(`--no-pr — stopping after wrap-up. Branch ${branchName} carries all commits; open a PR manually when ready.`)
}

// ----------------------------------------------------------------
// --auto-merge: merge the PR + clean up + regenerate derived surfaces. ONLY on a clean success (PASS, not bailed).
// ----------------------------------------------------------------
let mergeInfo = null
if (autoMerge && !bailed && finalVerdict === 'PASS' && prInfo?.created && !prInfo.draft) {
  log(`--auto-merge — merging the PR and cleaning up (${useWorktree ? 'worktree' : 'branch'} mode)...`)
  mergeInfo = await tracedAgent(`
You complete an --auto-merge for an /sdlc-flow run. Merge the PR, ${useWorktree
    ? 'then remove the worktree and delete the branch'
    : 'switch back to the base branch and delete the merged local branch'}, then regenerate derived
surfaces on the base. Be careful and report honestly.

Branch:   ${branchName}
${useWorktree ? `Worktree: ${worktreePath}\n` : ''}PR:       ${prInfo.number ? '#' + prInfo.number : prInfo.url || '(look it up)'}
Base:     ${prBase}

1. Merge the PR via gh (delete the remote branch as part of the merge):
   gh pr merge ${prInfo.number || prInfo.url} --merge --delete-branch
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
  log(`--auto-merge skipped: ${bailed ? 'run bailed' : finalVerdict !== 'PASS' ? `verdict ${finalVerdict}` : 'no PR created'}. ${useWorktree ? 'Worktree' : 'Branch'} left intact for review.`)
}

// ----------------------------------------------------------------
const tokensBlock = buildTokensBlock()
log(`Token roll-up: ${tokensBlock.total.inTokEst} inTokEst${tokensBlock.total.outTok ? ` | ${tokensBlock.total.outTok} outTok` : ''} across ${tokensBlock.stages.length} stage(s) — persisted in ${stateFile}.`)
log(`/sdlc-flow complete. Verdict: ${finalVerdict} | tasks passed: ${passedTasks.length}/${taskList.length}${bailed ? ` | BAILED: ${bailReason}` : ''}${prInfo?.created ? ` | PR: ${prInfo.url || prInfo.number}` : ''}`)
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
  pr: prInfo?.created ? { url: prInfo.url, number: prInfo.number, draft: prInfo.draft } : null,
  merged: mergeInfo?.merged || false,
  stateFile,
  worklogFile,
  tokens: tokensBlock,
}
