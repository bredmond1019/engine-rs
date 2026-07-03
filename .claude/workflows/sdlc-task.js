// =============================================================================
// sdlc-task — the LEAN small-work engine (implement → test → fix → commit)
// =============================================================================
//
// The cheap rung of the pipeline ladder, for one small unit of behaviour-changing
// work (a /ticket or /chore). Runs a spec's task(s) through a tight per-task loop —
//   implement → fast gating-test → triage → fix (≤3 attempts, Opus on the last)
//   → commit
// and nothing else. No scout, no separate review, no document stage, no ui-test,
// no wrap-up agent, no PR. When you need a consolidated review + docs + a PR, use
// /sdlc-flow; for a whole spec in place, /sdlc-run; for a roadmap, /sdlc-block.
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
//     → per-task loop → final state commit
//
//   Per-task loop (sequential):
//     implement → fast-test → (triage → fix/bail) ×≤3 → one state write per task
//   A triage MAJOR / immediate-bail reason breaks straight out (does NOT burn the
//   remaining attempts); the run stops and reports for human pickup.
//
// STATE (committed — NOT gitignored — at planning/<spec>/sdlc/)
//   sdlc-task-state.json   the authoritative run index (per-task summary/issues/
//                          fixes/commit + the Block-A `tokens` block). Committed in the
//                          worktree under --worktree; in place it is written uncommitted
//                          each task (cat-visible for crash inspection) and swept into
//                          ONE final `chore:` commit at the end.
//
// COMMIT STRATEGY
//   feat: implement <stem>         implement agent (per task)
//   fix:  fix pass P for <stem>    fix agent (per pass)
//   chore: sdlc-task state — <…>   state-writer (committed writes)
//
// MODEL TIERING (the token lever — see the MODEL map below)
//   haiku : setup, enumerate, state-load, test, state-writer
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
    notes:    { type: 'string' }
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
    sameFailureAsBefore: { type: 'boolean', description: 'true if the SAME failure as the previous attempt (no progress)' }
  }
}

const STATE_WRITE_SCHEMA = {
  type: 'object',
  required: ['written'],
  properties: {
    written:    { type: 'boolean', description: 'true if state.json was written (and committed when asked)' },
    commitHash: { type: 'string' },
    notes:      { type: 'string' }
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
  stateLoad:   'haiku',    // read + parse one JSON file (resume only)
  implement:   'sonnet',   // writes code/content + tests against a scoped task
  fix:         'sonnet',   // targeted fixes; failures escalate, never silently ship
  test:        'haiku',    // runs the project's validation suite, reads exit codes
  triage:      'sonnet',   // classifies a failure RETRYABLE vs MAJOR — light judgment
  stateWriter: 'haiku',    // stamps timestamps, writes state.json, commits when asked
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
    these fields when present: stack; validation.checks[] (each: {kind, name, command, purpose, gates}
    plus any kind-specific fields present — baselineCommand, compareKeys[], countPattern, failOn,
    warningPatterns[], rules[] ({id, pattern, paths, allowlistPattern})). Preserve kind-specific fields
    verbatim; ignore any other fields.

Return your findings using the StructuredOutput tool.
`, { label: 'harness-config', schema: HARNESS_CONFIG_SCHEMA, model: 'sonnet' })

  if (!result || !result.present || !result.config) return null
  return result.config
}

// Render the inner project-validation check list for a Test stage. When gatingOnly is true (the fast
// per-task tripwire), emit only the checks with gates:true; --test-depth full runs the whole suite.
// When the config is absent (or carries no checks), fall back to the spec's `## Validation Commands` —
// the engine ships NO stack defaults. Handles all D6 check kinds.
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
// COMMITTED AUTHORITATIVE STATE (Block A)
//
// `state` is the in-memory source of truth; writeTaskState() persists it to sdlc-task-state.json.
// COMMIT CADENCE mirrors the rest of the engine family:
//   --worktree → commit every write (the worktree is throwaway; mirrors sdlc-flow).
//   in place   → write uncommitted each task (cat-visible for crash inspection) and sweep into ONE
//                final `chore:` state commit at the end (mirrors sdlc-run's defer-to-final pattern,
//                keeping the current branch free of per-task state-churn commits).
// The runtime has no fs/clock, so a Haiku writer stamps started_at/updated_at and does the Write
// (+ git when committing). Committed report/code commits remain the authoritative resume signal;
// state is the at-a-glance index.
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

// Persist `state` to sdlc-task-state.json. `commit` controls whether this write is also committed.
async function writeTaskState(label, { cwd, commit }) {
  state.tokens = buildTokensBlock()   // Block A — refresh the committed token roll-up before persisting
  const stateJson = JSON.stringify(state, null, 2)
  const commitStep = commit
    ? `STEP 4 — commit on the branch (never git add -A; stage explicitly):
  cd ${cwd} && git add ${stateFile}
  cd ${cwd} && git commit -m "$(cat <<'EOF'
chore: sdlc-task state — ${label}
EOF
)" || echo "NOTHING_TO_COMMIT"
  cd ${cwd} && git log --oneline -1`
    : `STEP 4 — do NOT commit. Leave ${stateFile} written but unstaged (it is swept into one final commit later).`
  const result = await agent(`
You maintain the COMMITTED, authoritative run-state for an /sdlc-task pipeline. You run from the run
root. Write ONE JSON file${commit ? ' and commit it' : ''} — do not run checks, edit source, or touch anything else.

STEP 1 — timestamps + preserved start time (from the run root):
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

${commitStep}

Use the Write tool for the file. Return via StructuredOutput: written=true on success, commitHash from
the final git log line (empty string when not committed / nothing to commit).
`, withModel({ label: `state:${label}`, schema: STATE_WRITE_SCHEMA }, MODEL.stateWriter))
  if (!result || !result.written) {
    log(`(state) could not persist task state for "${label}" — continuing`)
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
    - WT_EXISTS → REUSE verbatim. branchName="${baseBranchName}", wasCreated=false. Skip to STEP 3.
    - WT_MISSING but branch "${baseBranchName}" exists (orphan branch, dir removed) → re-attach (NO -b flag):
        mkdir -p trees
        git worktree add --no-checkout trees/${baseBranchName} ${baseBranchName}
        git -C trees/${baseBranchName} sparse-checkout init --cone
        git -C trees/${baseBranchName} sparse-checkout set $(git ls-tree HEAD --name-only -d | tr '\\n' ' ')
        git -C trees/${baseBranchName} checkout
        if [ -f .env ]; then cp .env trees/${baseBranchName}/.env; fi
        if [ -f .env.local ]; then cp .env.local trees/${baseBranchName}/.env.local; fi
      branchName="${baseBranchName}", wasCreated=false. Skip to STEP 3.
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
    f. if [ -f .env ]; then cp .env trees/[branchName]/.env; fi
    g. if [ -f .env.local ]; then cp .env.local trees/[branchName]/.env.local; fi
    h. git -C trees/[branchName] commit --allow-empty -m "chore: init worktree [branchName]"
    Set wasCreated=true.
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

STEP 5 — Capture the emoji-gate diff base — the HEAD short sha as it stands NOW, before any task commit:
  cd <runDir> && git rev-parse --short HEAD     (store as baseSha)

Return your result using the StructuredOutput tool:
  runDir, branchName, baseSha, wasCreated, specFileExists, blockStatus, specThin, thinReason, notes.
`, withModel({ label: 'setup', schema: SETUP_SCHEMA, phase: 'Setup' }, MODEL.setup))

if (!setupResult) {
  log('Setup agent returned null — aborting pipeline')
  return { error: 'Setup failed', blockId }
}
const { runDir, branchName, baseSha } = setupResult
state.branch = branchName
state.worktree_path = useWorktree ? runDir : ''
log(`Run root: ${runDir} | branch: ${branchName} | base: ${baseSha}`)

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

const enumResult = await tracedAgent(`${W}
You enumerate the tasks defined in a spec's tasks.json. Do NOT modify anything.

STEP 1 — read the task list:
  cd ${runDir} && cat ${tasksJsonFile} 2>/dev/null || echo "NO_TASKS_JSON"

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
const taskList = selectedTasks ? allTasks.filter(n => selectedTasks.has(n)) : allTasks.slice()
if (!taskList.length) {
  log(`No tasks match the selection "${rangeSpec}" against spec tasks [${allTasks.join(', ')}].`)
  return { error: 'Empty task selection', blockId, rangeSpec, allTasks }
}
state.tasks_run = taskList
log(`Tasks in spec: ${allTasks.join(', ')}${selectedTasks ? ` | selected: ${taskList.join(', ')}` : ''}`)

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
async function runTests(label, { gatingOnly }) {
  return tracedAgent(`${W}
You are the test agent for the lean /sdlc-task pipeline. Run the project's validation checks and report.

IMPORTANT — run ONLY the checks enumerated below (from planning/harness.json + the spec). Do NOT invent
checks. All Bash calls run from the run root (prefix each with: cd ${runDir} &&).

${renderCheckList(harnessCfg, { gatingOnly, cwd: runDir })}

Then run the universal emoji gate (a harness rule, always): scan the files changed by THIS run for emoji
in markdown/docs.
  cd ${runDir} && git diff --name-only ${baseSha}..HEAD
  Inspect the changed .md/.mdx files; a stray emoji in docs FAILS this gate.

For each check record: name, passed (true iff exit code 0), the command, and failure output.
Return via StructuredOutput: allPassed (true only if EVERY gating check passed and the emoji gate is
clean), passCount, failCount, failedTests (names), failBlob (compact: failing check names + the tail of
their output; empty when allPassed).
`, withModel({ label, schema: TEST_SCHEMA, phase: 'Tasks' }, MODEL.test))
}

// ----------------------------------------------------------------
// Triage helper — classify a failure RETRYABLE vs MAJOR.
// ----------------------------------------------------------------
async function triage(context, attempt, maxAttempts, failBlob, sameContext) {
  return tracedAgent(`
You are the failure-triage agent for an /sdlc-task run. Classify a failure so the pipeline either makes
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
// PHASE 2: PER-TASK LOOP (sequential)
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

  let taskPassed = false
  let prevFailBlob = null

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

Return via StructuredOutput:
  success: true if the work completed and the spec validation passed
  filesModified: every source file you created or modified this attempt
  commitHash: the 7-char short hash (empty string if no commit was made)
  summary: one line — what this task now does
  decisions: any non-obvious choices (empty array if none)
  filesReadKb: telemetry — before returning, sum the byte size of every file you cat/Read this attempt
    (cd ${runDir} && wc -c <each file>), divide the total by 1024, and report the number.
  notes: one-line status
`, withModel({ label: `${isFix ? 'fix' : 'implement'}-${taskNum}-${attempt}`, schema: STAGE_SCHEMA, phase: 'Tasks' }, isFix ? fixModel : MODEL.implement))
    recordFilesRead(stageResult)

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

    // Fast test (tripwire) — gating checks only unless testDepth=full.
    const testResult = await runTests(`test-${taskNum}-${attempt}`, { gatingOnly: testDepth === 'fast' })
    if (testResult && testResult.allPassed) {
      t.validated = testDepth === 'fast' ? 'gating checks (fast tripwire)' : 'full gating suite'
      taskPassed = true
      break
    }

    // Failure → triage.
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
      log(`Task ${taskNum}: exhausted ${MAX_TASK_ATTEMPTS} attempts — bailing.`)
      break
    }
    if (tr) t.fixes = [...(t.fixes || []), tr.reason]
    log(`Task ${taskNum}: triage → RETRYABLE — fix pass ${attempt}/${MAX_TASK_ATTEMPTS - 1}. ${tr?.reason || ''}`)
  }

  // One state write per task. --worktree commits each write; in place writes uncommitted (swept at end).
  t.status = taskPassed ? 'passed' : 'failed'
  if (bailed && !taskPassed) { state.status = 'blocked'; state.bail_reason = bailReason }
  await writeTaskState(`task ${taskNum} ${t.status}`, { cwd: runDir, commit: useWorktree })

  if (bailed) break
}

// ================================================================
// FINAL STATE COMMIT + SUMMARY
// ================================================================
const passedTasks = taskList.filter(n => state.tasks[String(n)]?.status === 'passed' || passedFromState.has(n))
state.status = bailed ? 'blocked' : 'done'

// In-place mode wrote state uncommitted per task — sweep it into ONE final commit now. Worktree mode
// already committed each write, so a final commit is a cheap no-op (NOTHING_TO_COMMIT).
await writeTaskState(`run ${state.status} (${passedTasks.length}/${taskList.length})`, { cwd: runDir, commit: true })

const tokensBlock = state.tokens   // already rebuilt by the writeTaskState call just above (no traced agent ran since); reuse it rather than rebuilding (carry-in #3)
log(`Token roll-up: ${tokensBlock.total.inTokEst} inTokEst${tokensBlock.total.outTok ? ` | ${tokensBlock.total.outTok} outTok` : ''} across ${tokensBlock.stages.length} stage(s) — persisted in ${stateFile}.`)
log(`/sdlc-task complete. ${bailed ? `BAILED: ${bailReason}` : 'all selected tasks passed'} | passed ${passedTasks.length}/${taskList.length}.`)
if (useWorktree) {
  log(`Worktree branch "${branchName}" carries the commits at ${runDir}.`)
  log(`Integrate it when ready: git checkout main && git merge ${branchName}, then git worktree remove ${runDir} && git branch -d ${branchName}.`)
} else {
  log(`Commits landed in place on branch "${branchName}".`)
}
if (bailed) {
  log(`Pick up: read ${stateFile} for per-task state, fix the blocker, then re-run with --resume.`)
}

return {
  blockId,
  mode: state.mode,
  branch: branchName,
  runDir,
  bailed,
  bailReason: bailReason || null,
  tasksRun: taskList,
  tasksPassed: passedTasks,
  stateFile,
  tokens: tokensBlock,
}
