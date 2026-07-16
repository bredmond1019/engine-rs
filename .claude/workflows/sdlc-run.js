// =============================================================================
// sdlc-run — SDLC Pipeline Workflow
// =============================================================================
//
// Runs the full SDLC pipeline for a spec from the current stage to
// completion. Each stage is a separate agent with its own context window;
// agents communicate only through report files on disk.
//
// USAGE
//   /sdlc-run <spec-slug>                  runs all tasks in the spec
//   /sdlc-run <spec-slug> 2                scopes every stage to task 2 only
//   /sdlc-run <spec-slug> --from implement  skips scout; starts at the named stage
//   /sdlc-run <spec-slug> 2 --from test    task-scoped + skip scout
//
// PIPELINE STAGES (in order)
//   Scout      → detect current stage from report files + status.md + log
//   Plan       → generate task spec (skipped if spec file already exists)
//   Implement  → execute tasks from spec
//   Fix        → targeted fixes for FAIL/PARTIAL review (one pass per retry)
//   Test       → run the project's validation suite from planning/harness.json (+ universal emoji gate)
//   Review     → fresh validation run + acceptance criteria check; verdict gates next
//   Document   → surgical patches to docs/ (skipped if verdict is not PASS)
//   Wrap-up    → update status.md + log, commit planning files, write report
//
// COMMIT STRATEGY
//   Each agent commits its own work immediately after completing it:
//     feat: implement <stem>          implement agent (fix: if validation failed)
//     fix: fix pass N for <stem>      fix agent — one commit per pass
//     docs: update docs for <stem>    document agent
//     chore: wrap up <stem>           wrap-up agent (status/log/reports)
//
//   This ensures crash recovery: if the pipeline dies mid-run, all completed
//   work is already in git history and visible to future agents via git log.
//
// RESUMPTION
//   The scout checks which report files exist to determine where to resume.
//   Priority order:
//     no spec file      → generate-tasks
//     no implement.md   → implement
//     no test.md        → test
//     no review.md      → review
//     review = FAIL     → fix
//     no document.md    → document
//     document.md exists → wrap-up
//   Report files are authoritative; log is a cross-reference sanity check.
//   Safe to re-run — the scout will pick up exactly where the pipeline stopped.
//
// RETRY LOOP (max 3 review attempts)
//   implement → test → review → [PASS: document] or [FAIL: fix → test → review]
//   Each fix pass is a separate commit so the diff from each pass is auditable.
//
// MODEL TIERING (token lever — see the MODEL map below)
//   Three tiers, matched to the work: Opus on PLANNING (generate-tasks fallback); Haiku on the
//   purely-mechanical stages (scout, start-block, test); Sonnet on the judgment work
//   (implement/fix/review/document/wrap-up). Without this map every stage inherits the SESSION
//   model — so launching from an Opus session would run scout/test on Opus too. Tune
//   one place: the MODEL map.
//
// STAGED MODEL ESCALATION (ESCALATION_MODEL)
//   The FINAL fix pass and FINAL review attempt before the loop gives up run on Opus. The cheap
//   path stays on Sonnet; a genuinely hard failure that has already failed twice gets one strong
//   shot. Set null to disable.
//
// REPORT FILES  (all written to planning/<name>/sdlc/reports/)
//   [taskN-]implement.md  implement agent; overwritten by each fix pass
//   [taskN-]test.md       test agent
//   [taskN-]review.md     review agent
//   [taskN-]document.md   document agent
//   [taskN-]workflow.md   wrap-up agent (full pipeline run summary)
//
// =============================================================================

export const meta = {
  name: 'sdlc-run',
  description: 'Run the SDLC pipeline for a content/feature spec from current stage to completion',
  whenToUse: 'When starting or resuming a spec through the full implement→test→review→document→wrap-up cycle. Usage: /sdlc-run <spec-slug> or /sdlc-run <spec-slug> 2',
  phases: [
    { title: 'Scout',     detail: 'Determine current pipeline stage from files and log' },
    { title: 'Plan',      detail: 'Generate task spec (only if spec file does not yet exist)' },
    { title: 'Implement', detail: 'Execute implementation tasks' },
    { title: 'Fix',       detail: 'Targeted fixes for FAIL/PARTIAL review — overwrites implement report' },
    { title: 'Test',      detail: "Run the project's validation suite (from planning/harness.json)" },
    { title: 'Review',    detail: 'Verify acceptance criteria; run fresh tests; issue verdict' },
    { title: 'UI Test',   detail: 'Browser smoke check (only when planning/harness.json enables uiTest)' },
    { title: 'Document',  detail: 'Surgically patch docs/ (gates on PASS verdict)' },
    { title: 'Wrap-up',   detail: 'Log work, chore commit (status/log/reports), write workflow report' },
  ]
}

// ----------------------------------------------------------------
// Parse args: "<spec-slug>" or "<spec-slug> 2"
// ----------------------------------------------------------------
const rawArgs = typeof args === 'string' ? args.trim() : ''
if (!rawArgs) {
  log('ERROR: No spec name provided.')
  log('Usage: /sdlc-run <spec-slug>')
  log('       /sdlc-run <spec-slug> 2')
  return { error: 'Missing required argument: spec name (e.g. "<spec-slug>" or "<spec-slug> 2")' }
}

// Parse optional --from <stage> (skips scout when the caller already knows the start stage)
const VALID_FROM_STAGES = ['implement', 'fix', 'test', 'review', 'ui-test', 'document', 'wrap-up']
const fromMatch = rawArgs.match(/--from\s+(\S+)/)
const fromStage = fromMatch ? fromMatch[1] : null
if (fromStage && !VALID_FROM_STAGES.includes(fromStage)) {
  log(`ERROR: Unknown --from stage "${fromStage}". Valid values: ${VALID_FROM_STAGES.join(', ')}`)
  return { error: `Invalid --from stage: ${fromStage}` }
}
const cleanArgs = rawArgs.replace(/--from\s+\S+/, '').trim()
const parts = cleanArgs.split(/\s+/)
const blockId = parts[0]
const taskNumber = parts.length > 1 && !isNaN(parseInt(parts[1], 10)) ? parseInt(parts[1], 10) : null
const specFile = `planning/${blockId}/tasks.md`
const tasksJsonFile = `planning/${blockId}/tasks.json`
const stem = taskNumber !== null ? `${blockId}-task${taskNumber}` : blockId
const reportsDir = `planning/${blockId}/sdlc/reports`
const taskPrefix = taskNumber !== null ? `task${taskNumber}-` : ''
const implementReport = `${reportsDir}/${taskPrefix}implement.md`
const testReport      = `${reportsDir}/${taskPrefix}test.md`
const reviewReport    = `${reportsDir}/${taskPrefix}review.md`
const documentReport  = `${reportsDir}/${taskPrefix}document.md`
const uitestReport    = `${reportsDir}/${taskPrefix}ui-test.md`
const workflowReport  = `${reportsDir}/${taskPrefix}workflow.md`
const breakdownFile   = `planning/${blockId}/breakdown.md`

log(`Target: ${blockId}${taskNumber !== null ? ` task ${taskNumber}` : ' (all tasks)'}`)
log(`Spec: ${specFile} | Stem: ${stem}`)

// ----------------------------------------------------------------
// Schemas
// ----------------------------------------------------------------
const SCOUT_SCHEMA = {
  type: 'object',
  required: ['startStage', 'specFileExists', 'blockStatus', 'existingReports', 'statusSummary'],
  properties: {
    startStage: {
      type: 'string',
      enum: ['generate-tasks', 'implement', 'fix', 'test', 'review', 'ui-test', 'document', 'wrap-up'],
      description: 'The stage to start from, determined by which report files exist'
    },
    specFileExists: { type: 'boolean' },
    blockStatus: {
      type: 'string',
      enum: ['Not started', 'In progress', 'Done', 'Blocked', 'Skipped', 'Unknown'],
      description: 'Current status of this spec in status.md progress table'
    },
    existingReports: {
      type: 'array',
      items: { type: 'string' },
      description: 'List of report file paths that already exist'
    },
    reviewVerdict: {
      type: 'string',
      description: 'Verdict extracted from the review report if it exists: PASS, FAIL, PARTIAL, or empty string if no review report'
    },
    currentFocus: { type: 'string', description: 'The Current focus line from status.md' },
    lastDevlogEntry: { type: 'string', description: 'Summary of the most recent log entry (first 6 lines)' },
    statusSummary: { type: 'string', description: 'Human-readable summary of what the scout found and why it chose startStage' },
    discrepancies: { type: 'string', description: 'Any discrepancies between log entries and report files, or empty string if none' },
    specThin: { type: 'boolean', description: 'D19: true ONLY when startStage is "implement" (a fresh run) AND the spec is structurally present but substantively thin per the scout STEP 9 signals. false in every other case (resume, missing spec, or a healthy spec).' },
    thinReason: { type: 'string', description: 'D19: when specThin is true, the specific thin-spec failures named; empty string otherwise.' }
  }
}

const STAGE_SCHEMA = {
  type: 'object',
  required: ['reportFile', 'success'],
  properties: {
    reportFile: { type: 'string', description: 'Path to the report file written' },
    success: { type: 'boolean' },
    filesModified: { type: 'array', items: { type: 'string' } },
    commitHash: { type: 'string', description: 'Short hash of the commit made by this agent, or empty string if no commit' },
    notes: { type: 'string' }
  }
}

const TEST_SCHEMA = {
  type: 'object',
  required: ['reportFile', 'allPassed', 'passCount', 'failCount'],
  properties: {
    reportFile: { type: 'string' },
    allPassed: { type: 'boolean' },
    passCount: { type: 'integer' },
    failCount: { type: 'integer' },
    failedTests: { type: 'array', items: { type: 'string' } },
    notes: { type: 'string' }
  }
}

const REVIEW_SCHEMA = {
  type: 'object',
  required: ['reportFile', 'verdict'],
  properties: {
    reportFile: { type: 'string' },
    verdict: { type: 'string', enum: ['PASS', 'FAIL', 'PARTIAL'] },
    failureReasons: { type: 'array', items: { type: 'string' } },
    unmetCriteria: { type: 'array', items: { type: 'string' } },
    notes: { type: 'string' }
  }
}

// Wrap-up = log-work + finalize merged into one bookkeeping agent (D14). Unlike sdlc-task's wrap-up
// (which only RECORDS what status/log should become, deferred to /clean-worktree merge time), this
// runs on main with no worktree — so it edits status.md + log.md directly, then writes the workflow
// report and commits every remaining planning file in one chore commit. Kept on Sonnet (not Haiku):
// the human-facing status/log prose is the judgment-heavy half and stays authoritative.
const WRAPUP_SCHEMA = {
  type: 'object',
  required: ['statusUpdated', 'devlogUpdated', 'workflowReportFile', 'commitMessage'],
  properties: {
    statusUpdated:      { type: 'boolean' },
    devlogUpdated:      { type: 'boolean' },
    nextFocus:          { type: 'string' },
    workflowReportFile: { type: 'string' },
    commitMessage:      { type: 'string' },
    commitHash:         { type: 'string' },
    amendments:         { type: 'array', items: { type: 'string' }, description: 'D18: the dated amendment-log lines appended to the spec for genuine deviations this run (empty array if none).' },
    blockStatusFlipped: { type: 'string', description: 'The state.json tracks[].blocks[].id flipped to "closed" this run, or "" if none flipped (spec not fully done, no state.json, or block not found).' },
    notes:              { type: 'string' }
  }
}

const UI_TEST_SCHEMA = {
  type: 'object',
  required: ['reportFile', 'verdict'],
  properties: {
    reportFile: { type: 'string' },
    verdict: { type: 'string', enum: ['PASS', 'WARN', 'FAIL', 'SKIPPED'] },
    failureReasons: { type: 'array', items: { type: 'string' } },
    notes: { type: 'string' }
  }
}

// ----------------------------------------------------------------
// MODEL TIERING — the primary token lever for this pipeline.
//
// Without this map every stage would inherit the SESSION model — so launching /sdlc-run from an
// Opus session silently runs scout, test, and finalize on Opus too. This map fixes the cost per
// stage regardless of how the workflow was launched.
//
// Principle (mirrors sdlc-task): match the model to the work.
//   • Opus   — generate-tasks (authors the spec; fallback path only)
//   • Haiku  — scout / start-block / test. Fixed procedures, no real judgment.
//   • Sonnet — implement / fix / review / document / wrap-up (judgment work).
//
// To re-tier, change one value here — nothing else moves.
// Valid values: 'haiku' | 'sonnet' | 'opus' | undefined (inherit session model).
// ----------------------------------------------------------------
const MODEL = {
  scout:         'haiku',    // deterministic decision tree: ls a few files, apply a fixed 7-rule order
  startBlock:    'haiku',    // one surgical status.md edit + a date stamp
  generateTasks: 'opus',     // PLANNING — authors the spec that drives everything (fallback path)
  implement:     'sonnet',   // writes content/code + tests against a scoped spec/breakdown
  fix:           'sonnet',   // targeted fixes; failures escalate, never silently ship
  test:          'haiku',    // runs the project's validation suite, reads exit codes; review re-runs the gating checks
  review:        'sonnet',   // verify criteria; gated by an authoritative fresh run of the gating checks
  uiTest:        'sonnet',   // live browser smoke checks (when uiTest.enabled); needs judgment to interpret results
  document:      'sonnet',   // surgical doc patches, gated on PASS
  wrapup:        'sonnet',   // log-work + finalize merged (D14): authors the human-facing status/log prose (judgment) + writes the report + scripted git add
}

// Merge an optional model override into an agent's opts (omits the key when undefined,
// so the agent inherits the session model rather than receiving model: undefined).
function withModel(base, model) {
  return model ? { ...base, model } : base
}

// ----------------------------------------------------------------
// TOKEN TELEMETRY (Block A — the shared committed-state token contract)
//
// Lifted verbatim from sdlc-task.js (engines are self-contained — lift, don't import). Each
// substantive stage runs through tracedAgent, which records the injected-prompt size and the
// output-token delta off the shared budget pool. buildTokensBlock() rolls the accumulated metrics
// into the canonical `tokens` block that committed state carries (per-stage + a cumulative total).
//
//   promptTokEst — injected input only (~prompt.length / 4)
//   outTok       — output-token delta from the shared budget pool; null when no +Nk target is set.
//                  sdlc-run is fully SEQUENTIAL (no parallel waves), so the delta attributes cleanly
//                  to its stage — no D12 contamination caveat applies here.
//   filesReadKb  — a stage's self-reported ingestion estimate, folded in via recordFilesRead() when
//                  the stage schema carries it (none do today; the plumbing is ready for later use).
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

// Build the canonical `tokens` block from the accumulated per-agent metrics. This is the shared
// token-telemetry contract every engine's committed state carries (Block A): per-stage output
// tokens + the D15 input-cost estimate + a cumulative run total. inTokEst is always present
// (prompt-derived); outTok is null when no budget target is set and is summed as 0 in the total.
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
// planning/harness.json. The workflow runtime has no filesystem access, so a dedicated
// micro-loader agent reads + parses the file (the same pattern every engine uses for harness.json).
// Returns the parsed config object, or null when the file is absent or invalid — callers then
// degrade to the spec's `## Validation Commands` section and disable the UI-test stage.
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
                  baselineCommand: { type: 'string', description: 'baseline-diff only' },
                  compareKeys:     { type: 'array', items: { type: 'string' }, description: 'baseline-diff only' },
                  countPattern:    { type: 'string', description: 'count-delta only' },
                  failOn:          { type: 'string', description: 'count-delta only: decrease | zero-or-decrease' },
                  warningPatterns: { type: 'array', items: { type: 'string' }, description: 'warning-scan only' },
                  rules: {
                    type: 'array',
                    description: 'forbidden-pattern-scan only',
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
        uiTest: {
          type: 'object',
          properties: {
            enabled:          { type: 'boolean' },
            devServerCommand: { type: 'string' },
            readySignal:      { type: 'string' },
            port:             { type: 'integer' },
            routes:           { type: 'array', items: { type: 'string' } }
          }
        }
      }
    },
    notes: { type: 'string' }
  }
}

// Spawn the micro-loader agent and return the parsed config (or null). Wired into the stages
// in P4; defined here so the loader path exists from P1. No stack defaults on absence.
async function loadHarnessConfig() {
  const result = await agent(`
You are the harness-config loader for the SDLC pipeline. Your ONLY job is to read the project's
validation-policy file and return it as structured data. Do not run any checks or modify anything.

STEP 1 — Read the config file (from the repo root):
  cat planning/harness.json 2>/dev/null && echo "__HARNESS_PRESENT__" || echo "__HARNESS_ABSENT__"

STEP 2 — Decide:
  - "__HARNESS_ABSENT__" (file missing) → present=false, omit config.
  - File printed but NOT valid JSON → present=false, notes="harness.json present but invalid JSON: <reason>".
  - File printed and valid JSON → present=true, and copy the parsed object into "config", keeping ONLY
    these fields when present: stack; validation.checks[] (each: {kind, name, command, purpose, gates}
    plus any kind-specific fields that are present — baselineCommand, compareKeys[], countPattern,
    failOn, warningPatterns[], rules[] ({id, pattern, paths, allowlistPattern})); uiTest ({enabled,
    devServerCommand, readySignal, port, routes[]}). Preserve kind-specific fields verbatim; ignore
    any other fields.

Return your findings using the StructuredOutput tool.
`, { label: 'harness-config', schema: HARNESS_CONFIG_SCHEMA, model: 'sonnet' })

  if (!result || !result.present || !result.config) return null
  return result.config
}

// Render the inner project-validation check list for the Test stage from harness config.
// Returns the numbered CHECK blocks the agent runs before the universal emoji gate. When the
// config is absent (or carries no checks), returns instructions to fall back to the spec's
// optional `## Validation Commands` section — the engine ships NO stack defaults.
// Handles all D6 check kinds: command (default), baseline-diff, count-delta, warning-scan,
// forbidden-pattern-scan. changedPaths is reserved for the deferred conditionalChecks feature.
function renderCheckList(cfg, { changedPaths } = {}) {
  const checks = cfg?.validation?.checks ?? []
  if (!checks.length) {
    return `The project ships no \`planning/harness.json\` validation suite, so derive the checks
from the spec instead:
  - Read the spec's optional "## Validation Commands" section.
  - Run each command it lists, IN ORDER. Each command is one check — record test_name (a short
    slug for the command), execution_command (the command), test_purpose ("from the spec's
    Validation Commands"), passed (true iff exit code 0), and error (output on failure).
  - If the spec has no "## Validation Commands" section, run no project checks — record a single
    informational row (test_name "no_validation_suite", passed true, empty error) noting the
    project declared no validation suite. Then run the universal emoji gate below.`
  }
  return checks.map((c, i) => {
    const n = i + 1
    const kind = c.kind || 'command'
    const slug = (c.name || `check${n}`).toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '')
    const gate = c.gates
      ? 'GATING — a failure here blocks the review verdict'
      : 'non-gating — informational; a failure here does not block the verdict'
    const header = `CHECK ${n} — ${c.name} (${c.purpose}) [${gate}]`

    // --- baseline-diff: fail only on items absent from the pre-implement baseline ---
    if (kind === 'baseline-diff') {
      const baselinePath = `${reportsDir}/${taskPrefix}${slug}-baseline.json`
      const currentPath = `/tmp/${stem}-${slug}-current.json`
      const keysLiteral = JSON.stringify(c.compareKeys || [])
      return `${header} — baseline-diff (fail ONLY on net-new items vs the baseline snapshotted before implement):
  ${c.command} > ${currentPath} 2>/dev/null; true
  python3 << 'PYEOF'
import json, sys
baseline_path = '${baselinePath}'
current_path  = '${currentPath}'
keys = ${keysLiteral}
try:
    b = json.load(open(baseline_path, encoding='utf-8'))
except Exception as e:
    print(f'WARNING: could not load baseline ({e}) — treating all current items as pre-existing'); b = []
try:
    c = json.load(open(current_path, encoding='utf-8'))
except Exception:
    c = []
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

    // --- count-delta: compare an integer count vs the previous task's report ---
    // In a full-spec run (taskNumber null) there is no previous task to compare — skip.
    if (kind === 'count-delta') {
      if (taskNumber === null) {
        return `${header} — count-delta (SKIP: count-delta is per-task comparison; no task number in full-spec mode):
  echo "COUNT[${slug}]: N/A (full-spec run — count-delta skipped)"
  echo "CHECK${n}_EXIT:0"`
      }
      const prevReport = taskNumber > 1 ? `${reportsDir}/task${taskNumber - 1}-test.md` : ''
      const failRule = c.failOn === 'zero-or-decrease'
        ? 'FAIL if delta <= 0 (count must strictly increase)'
        : 'FAIL if delta < 0 (count must not decrease)'
      const prevStep = prevReport
        ? `  Read the previous task's recorded count:
    grep -oE 'COUNT\\[${slug}\\]: [0-9]+' ${prevReport} | head -1 || echo "NO_PREV_COUNT"
  If NO_PREV_COUNT (previous report has no marker), treat this check as SKIP — delta unknown, do not fail.`
        : `  This is task 1 — there is no previous task. Treat this check as SKIP (no delta to compare).`
      return `${header} — count-delta (${c.failOn}):
  ${c.command}
  Extract the current count: the first integer on the line matching the ERE /${c.countPattern}/.
${prevStep}
  Compute delta = current - previous. ${failRule}.
  IMPORTANT: write the marker line "COUNT[${slug}]: <current>" verbatim into the test report (any
  section) so the NEXT task can read it. Record the delta and the pass/fail in this check's row.
  echo "CHECK${n}_EXIT:0  (set to 1 only if the rule above fails; SKIP counts as pass)"`
    }

    // --- warning-scan: run a command (exit code gates) and record matches of warningPatterns ---
    if (kind === 'warning-scan') {
      const outPath = `/tmp/${stem}-${slug}.out`
      const alternation = (c.warningPatterns || []).map(p => `(${p})`).join('|')
      const patternSeverity = c.gates
        ? 'Because gates:true, a pattern match ALSO FAILS this check.'
        : 'Because gates:false, pattern matches are informational WARN entries — they do NOT fail the check (but DO record them).'
      return `${header} — warning-scan (run the command, gate on its exit code, then scan its output):
  ${c.command} > ${outPath} 2>&1; echo "CMD_EXIT:$?"
  grep -nE '${alternation}' ${outPath} && echo "WARNINGS_FOUND" || echo "NO_WARNINGS"
  Pass/fail: this check FAILS if CMD_EXIT is non-zero (the command itself failed). Record every matched
  warning line in this check's row/notes. ${patternSeverity}
  Set the exit marker accordingly:
  echo "CHECK${n}_EXIT:<0 if CMD_EXIT==0 and not failed-by-pattern, else 1>"`
    }

    // --- forbidden-pattern-scan: source greps that must find NO matches ---
    if (kind === 'forbidden-pattern-scan') {
      const ruleLines = (c.rules || []).map(r => {
        const paths = r.paths || '.'
        const allow = r.allowlistPattern ? ` | grep -vE '${r.allowlistPattern}'` : ''
        return `  Rule "${r.id}":
    grep -rnE '${r.pattern}' ${paths}${allow} && echo "RULE ${r.id}: MATCHED (violation)" || echo "RULE ${r.id}: clean"`
      }).join('\n')
      return `${header} — forbidden-pattern scan (every rule below must find NO matches):
${ruleLines}
  This check PASSES only if EVERY rule reports "clean". If any rule MATCHED, the check FAILS and the
  matched lines are violations — list them in this check's row.
  echo "CHECK${n}_EXIT:0  (set to 1 if any rule MATCHED, else 0)"`
    }

    // --- command (default): plain exit-code gate (unchanged behavior) ---
    return `${header}:
  ${c.command}
  echo "CHECK${n}_EXIT:$?"`
  }).join('\n\n')
}

// Snapshot baseline artifacts for any `baseline-diff` checks before implement, so the Test stage
// can diff current output vs the pre-run state and fail only on net-new items. Resume-safe: only
// writes a baseline that does not already exist. No-op when no baseline-diff checks are configured.
// When called from /sdlc-block (--from test), block already ran snapshotBlockBaselines() which
// wrote the same paths (no taskPrefix in full-spec mode) — so this is a no-op on those files.
async function snapshotBaselines(cfg) {
  const checks = (cfg?.validation?.checks || []).filter(c => c.kind === 'baseline-diff' && c.baselineCommand)
  if (!checks.length) return
  const steps = checks.map(c => {
    const slug = (c.name || 'check').toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '')
    const path = `${reportsDir}/${taskPrefix}${slug}-baseline.json`
    return `Baseline "${c.name}" -> ${path}:
  mkdir -p ${reportsDir}
  [ -f ${path} ] && echo "BASELINE EXISTS (kept): ${path}" || { ${c.baselineCommand} > ${path} 2>/dev/null; echo "BASELINE WRITTEN: ${path}"; }`
  }).join('\n\n')
  await agent(`
You are the baseline-snapshot agent for the SDLC pipeline. Capture the pre-implement baseline for each
baseline-diff validation check BEFORE any implementation runs. Run each block exactly as written.
Do NOT modify source. Existing baselines are kept (resume-safe).

${steps}

Return using StructuredOutput: done=true, and note which baselines were written vs already present.
`, { label: 'baseline-snapshot', schema: { type: 'object', required: ['done'], properties: { done: { type: 'boolean' }, notes: { type: 'string' } } }, model: 'haiku' })
}

// Render the UI-test stage prompt body from harness config. Called ONLY when cfg.uiTest.enabled.
// Interpolates the MVP fields (devServerCommand / readySignal / port / routes); the surrounding
// stage gate decides whether this runs at all.
function renderUiTestPrompt(cfg, port) {
  const ui = cfg.uiTest
  const routes = (Array.isArray(ui.routes) && ui.routes.length) ? ui.routes : ['/']
  const ready = ui.readySignal || 'ready'
  const devCmd = ui.devServerCommand || 'echo "ERROR: uiTest.enabled but devServerCommand missing in planning/harness.json" && false'
  const routeChecks = routes.map((r, i) => `  CHECK ${i + 1} — Route ${r} renders without error:
    playwright-cli goto http://localhost:${port}${r}
    playwright-cli snapshot
    playwright-cli console
    Verify: the page title / headings do not contain "404", "500", "Error", or "Not Found";
    the page shows real content (not a bare framework error screen); the console has no
    error-level entries ("warning"-level entries → WARN, not FAIL).`).join('\n\n')
  const routeRows = routes.map(r => `  | Route ${r} renders | PASS/WARN/FAIL | |`).join('\n')
  return { routes, ready, devCmd, port, routeChecks, routeRows }
}

// ----------------------------------------------------------------
// Stage results accumulator
// ----------------------------------------------------------------
const stageResults = []

// ----------------------------------------------------------------
// COMMITTED AUTHORITATIVE STATE (Block A — supersedes D27's gitignored breadcrumb)
//
// After each phase resolves, write planning/<concept>/sdlc/sdlc-run-state.json (write-only — NOT
// committed per phase). The file carries the phase trail (current/completed/failed) AND the canonical
// `tokens` block (per-stage output tokens + the D15 input-cost estimate + a cumulative total) — so
// token usage, which was render-only and vanished when a run ended, is now persisted and reviewable.
//
// COMMIT CADENCE: sdlc-run executes IN PLACE on the current branch (usually main, no worktree), so —
// unlike sdlc-flow's throwaway-worktree per-write commit — the state file is written uncommitted each
// phase (still `cat`-visible for crash inspection) and the WRAP-UP chore commit stages it in ONE shot.
// That keeps main free of per-phase state-churn commits. The committed report files remain the
// AUTHORITATIVE resume signal (see the RESUMPTION header) — state is a best-effort index/review
// artifact, so a crash before wrap-up leaves the state file uncommitted (acceptable: the per-stage
// reports are already committed by their own agents). A failed write logs a warning, never aborts.
//
// The runtime has no filesystem access and cannot call Date.now(), so a cheap Haiku writer agent
// stamps the timestamps (via `date`) and does the Write, preserving started_at across writes.
// ----------------------------------------------------------------
const stateFile = `planning/${blockId}/sdlc/sdlc-run-state.json`
const completedPhases = []

const STATE_WRITE_SCHEMA = {
  type: 'object',
  required: ['written'],
  properties: {
    written: { type: 'boolean', description: 'true if the state file was written successfully' },
    stateFile: { type: 'string' },
    notes: { type: 'string' }
  }
}

// Persist the phase + tokens state after a phase resolves (write-only — the wrap-up commit stages it).
// On success the phase is appended to completed_phases (monotonic, deduped). On failure, failed_phase +
// resume_from are set to the phase name and completed_phases is left untouched (the phase did not
// complete). The tokens block reflects every traced stage that has run so far (cumulative — the final
// write before wrap-up is the run total, modulo the wrap-up agent's own as-yet-unrun cost).
async function recordPhaseState(phaseName, { failed = false } = {}) {
  if (!failed && phaseName && !completedPhases.includes(phaseName)) {
    completedPhases.push(phaseName)
  }
  const failedPhase = failed ? phaseName : null
  const tokensJson = JSON.stringify(buildTokensBlock(), null, 2)
  const result = await agent(`
You maintain the SDLC pipeline's committed run-state. Write ONE JSON file — do not run any checks,
edit code, or commit anything (the wrap-up stage commits this file later).

STEP 1 — current UTC timestamp + preserved start time:
  NOW=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  cat ${stateFile} 2>/dev/null || echo "__NO_STATE__"
  If that file already exists and contains a "started_at" value, REUSE it verbatim for started_at
  below. Otherwise set started_at = the NOW value.

STEP 2 — ensure the directory exists:
  mkdir -p planning/${blockId}/sdlc

STEP 3 — write ${stateFile} with EXACTLY this JSON (valid JSON only: double quotes, no trailing
commas, no markdown fences). Substitute <STARTED_AT> (preserved or NOW) and <NOW> from STEP 1, and
insert the "tokens" object verbatim as given:
{
  "spec_slug": ${JSON.stringify(blockId)},
  "started_at": "<STARTED_AT>",
  "updated_at": "<NOW>",
  "current_phase": ${JSON.stringify(phaseName)},
  "completed_phases": ${JSON.stringify(completedPhases)},
  "failed_phase": ${failedPhase === null ? 'null' : JSON.stringify(failedPhase)},
  "task_number": ${taskNumber === null ? 'null' : taskNumber},
  "resume_from": ${failedPhase === null ? 'null' : JSON.stringify(failedPhase)},
  "tokens": ${tokensJson}
}

Use the Write tool to write ${stateFile}. Do NOT git add or commit. Then return via StructuredOutput
(written=true on success).
`, withModel({ label: `state:${phaseName}${failed ? ':failed' : ''}`, schema: STATE_WRITE_SCHEMA }, MODEL.scout))
  if (!result || !result.written) {
    log(`(state) could not persist run state for "${phaseName}"${failed ? ' (failure record)' : ''} — continuing`)
  }
}

// ================================================================
// PHASE 1: SCOUT — determine current pipeline stage
//   Skipped when --from <stage> is supplied (caller already knows the state).
// ================================================================
let scout
let currentStage

if (fromStage) {
  log(`--from ${fromStage} — skipping scout, starting at "${fromStage}"`)
  scout = {
    startStage: fromStage, specFileExists: true, blockStatus: 'In progress',
    existingReports: [], reviewVerdict: '', currentFocus: '', lastDevlogEntry: '',
    statusSummary: `--from ${fromStage} (scout skipped)`, discrepancies: ''
  }
  currentStage = fromStage
} else {
phase('Scout')

scout = await tracedAgent(`
You are the pipeline scout for the SDLC workflow system.

Target:
  Spec ID:     ${blockId}
  Task number: ${taskNumber !== null ? taskNumber : 'none (full spec)'}
  Spec file:   ${specFile}
  Report stem: ${stem}
  Reports dir: ${reportsDir}

Your job is to determine which SDLC stage to start from, based on which report files exist. Run these checks using the Bash tool:

STEP 1 — Check spec file:
  ls -la ${specFile} 2>/dev/null && echo "SPEC_EXISTS" || echo "SPEC_MISSING"

STEP 2 — Check report files (spec directory: ${reportsDir}):
  ls ${implementReport} 2>/dev/null && echo "HAS_IMPLEMENT" || echo "NO_IMPLEMENT"
  ls ${testReport} 2>/dev/null && echo "HAS_TEST" || echo "NO_TEST"
  ls ${reviewReport} 2>/dev/null && echo "HAS_REVIEW" || echo "NO_REVIEW"
  ls ${uitestReport} 2>/dev/null && echo "HAS_UITEST" || echo "NO_UITEST"
  ls ${documentReport} 2>/dev/null && echo "HAS_DOCUMENT" || echo "NO_DOCUMENT"
  ls ${reportsDir}/*.md 2>/dev/null | head -20 || echo "NO_BLOCK_REPORTS"

STEP 3 — Read status.md to find this spec's status and Current focus line:
  head -60 planning/status.md

STEP 4 — Read the most recent log entry (at repo root):
  head -60 log.md

STEP 5 — If the review report exists, extract the verdict:
  grep -iE "\\*\\*Verdict|## Verdict|^Verdict:" ${reviewReport} 2>/dev/null | head -5 || echo "NO_REVIEW_REPORT"

STEP 6 — Determine startStage using this EXACT priority order:
  1. Spec file MISSING → "generate-tasks"
  2. Spec exists, no implement report (and no variant) → "implement"
  3. Implement report exists, no test report → "test"
  4. Test report exists, no review report → "review"
  5. Review report exists with FAIL or PARTIAL verdict → "fix" (targeted fix cycle, not full re-implement)
  6. Review report exists with PASS verdict, no ui-test report → "ui-test"
  7. Review report exists with PASS verdict, ui-test report exists, no document report → "document"
  8. Document report exists → "wrap-up"

STEP 7 — Find the spec's status in status.md progress table. Look for a row containing "${blockId}" and extract its Status column value (Not started / In progress / Done / Blocked / Skipped).

STEP 8 — Note any discrepancy: if log says a stage is done but the matching report file is missing, record that.

STEP 9 — Thin-spec content check (D19). Set specThin and thinReason. Evaluate this ONLY when
  startStage from STEP 6 is "implement" (a fresh run about to spend implement tokens). In EVERY
  other case (resume at test/review/fix/etc., or spec missing → generate-tasks) set specThin=false,
  thinReason="". When startStage is "implement", read the spec and flag it thin ONLY on these
  high-confidence signals — a blocked valid spec is far costlier than a missed thin one, so when in
  doubt do NOT flag:
    a) grep -n '{{' ${specFile}  → any unfilled {{TOKEN}} scaffold token is thin.
    b) The '## Acceptance Criteria' section has no real '- ' bullet (empty, or only a verbatim
       template seed like '- <Observable, checkable condition') → thin.
  Do NOT flag a bare 'TODO'/'TBD' in prose, and do NOT treat any '<...>' as a token (it is legitimate
  in generics like 'Vec<T>', prose like 'the <concept> folder', and globs). The Amendment Log seed
  '_No amendments yet._' is the correct resting state — never flag it. If neither (a) nor (b) holds,
  specThin=false. If either holds, specThin=true and thinReason names the specific failures.

Collect the list of existing report files from the ls output in STEP 2 and STEP 6.

Return your findings using the StructuredOutput tool.
`, withModel({ label: 'scout', schema: SCOUT_SCHEMA, phase: 'Scout' }, MODEL.scout))

if (!scout) {
  log('Scout agent failed — cannot determine pipeline state, aborting')
  return { error: 'Scout failed', blockId, stem }
}

log(`Scout: start from "${scout.startStage}" | spec status: "${scout.blockStatus}"`)
if (scout.discrepancies) log(`Discrepancies: ${scout.discrepancies}`)
if (scout.statusSummary) log(scout.statusSummary)

// D19 — thin-spec guard. Abort before spending implement tokens on a structurally-valid but
// substantively-empty spec. Scout only sets specThin on a fresh implement-stage run (never on resume).
if (scout.specThin) {
  log(`ABORTED (D19) — spec is structurally valid but substantively thin: ${scout.thinReason || '(no reason given)'}`)
  log(`Fix: flesh out ${specFile} (run /generate-tasks --force to regenerate, or edit + commit), then re-run /sdlc-run ${blockId}.`)
  return { error: 'Thin spec (D19)', reason: scout.thinReason || '', blockId, stem }
}

// Auto-flip spec to "In progress" if it is "Not started"
if (scout.blockStatus === 'Not started') {
  log(`Spec "${blockId}" is Not started — marking In progress in status.md...`)
  await agent(`
You need to mark spec "${blockId}" as "In progress" in planning/status.md.

Instructions:
1. Read the file: planning/status.md
2. Find the row in the Progress Table where the Spec column contains "${blockId}"
3. Change that row's Status cell from "Not started" to "In progress"
4. Update the "Current focus:" line near the top of the file to:
   "${blockId}${taskNumber !== null ? ` — Task ${taskNumber}` : ''}"
5. Update the "Last updated:" date — run this to get today: date +%Y-%m-%d
6. Use the Edit tool to make these changes surgically (do not rewrite the entire file)
7. Confirm the edits are correct by reading back the relevant lines
`, withModel({ label: 'start-block', phase: 'Scout' }, MODEL.startBlock))
}

  currentStage = scout.startStage
} // end scout (else)
let reviewAttempts = 0
const MAX_REVIEW_ATTEMPTS = 3
let lastReviewResult = null

// STAGED MODEL ESCALATION — the FINAL fix pass and FINAL review attempt before the loop gives
// up run on a stronger model. The common path stays on Sonnet (MODEL.fix/review); only the
// genuinely-hard case that has already failed twice gets one Opus shot. Set to null to disable.
const ESCALATION_MODEL = 'opus'

// ================================================================
// PHASE 2: PLAN — generate-tasks (only if spec file missing)
// ================================================================
if (currentStage === 'generate-tasks') {
  phase('Plan')
  log('Spec file not found — running generate-tasks...')

  const genResult = await tracedAgent(`
You need to generate the task spec for spec "${blockId}".

Files to create: ${specFile} (prose) AND ${tasksJsonFile} (task list).

Instructions:

1. Read planning/master-plan.md — find the section covering "${blockId}". Look for phase/block headers. Read that entire section.

2. Read CLAUDE.md and planning/context.md — internalize and enforce the project's standing rules.
   CLAUDE.md is the authority; do not assume any stack, locale-parity, narrative, or content-layout
   rule unless written there. Universal harness rules always apply: no fabricated metrics or quotes,
   no emoji, every change ships with tests.

3. Read the generic spec skeleton as a format reference: .claude/workflows/templates/spec-template.md
   Study its structure: Goal, Context Pointers, a pointer to tasks.json, Acceptance Criteria,
   Validation Commands, Notes section — and the tasks.json schema shown in the same template (a
   bare array of SDLCTask-shaped objects: task_id, title, description, acceptance_criteria,
   validation_commands, max_attempts, files, dependsOn — matches orchestrator's
   app/schemas/sdlc_schema.py).

   Also create the spec directory structure now if it does not yet exist:
   mkdir -p planning/${blockId}/sdlc/reports

4. Write ${specFile} following that exact format:
   ## Goal
   [one-sentence purpose]

   ## Context Pointers
   [links to master plan sections, relevant content/code files, relevant DECISIONS entries]

   ## Step-by-Step Tasks
   See tasks.json in this directory — the task list is defined there, not here.

   ## Acceptance Criteria
   [bullet list of what "done" looks like, testable and specific]

   ## Validation Commands
   [bash commands to verify completion — use the project's own validation suite. Copy the commands
    from planning/harness.json (validation.checks[].command), in order. If that file is absent,
    write the commands the project's CLAUDE.md "Build / test / run" section documents.]

   ## Notes
   [empty section for in-progress updates]

5. Write ${tasksJsonFile} as valid JSON — a BARE ARRAY, not wrapped in an object:
   [
     { "task_id": 1, "title": "[Task Name]", "description": "[sub-steps with exact file paths and component/function names]", "acceptance_criteria": [], "validation_commands": [], "max_attempts": 3, "files": ["[path/to/file]"], "dependsOn": [] },
     ...
     { "task_id": N, "title": "Validate", "description": "Run the Validation Commands listed below and confirm all pass.", "acceptance_criteria": [], "validation_commands": [], "max_attempts": 3, "files": [], "dependsOn": [1, 2, "…every prior task_id"] }
   ]

   Rules:
   - Follow every CLAUDE.md standing rule; record any deferral in the Notes section
   - The Validation Commands section must mirror planning/harness.json (or the project's documented suite)
   - The final task's title must always be "Validate" and its dependsOn must list every other task_id
   - Tasks should be sized for the 21 hrs/week schedule
   - Every task's "files" must name exact file paths; every task but Validate needs ≥1 entry

Return your result using the StructuredOutput tool with fields:
  reportFile: path to the spec file written (${specFile})
  success: true if both files were written successfully
  filesModified: ["${specFile}", "${tasksJsonFile}"]
  notes: brief note about what was generated
`, withModel({ label: 'generate-tasks', schema: STAGE_SCHEMA, phase: 'Plan' }, MODEL.generateTasks))

  if (!genResult || !genResult.success) {
    log('generate-tasks failed — aborting pipeline')
    stageResults.push({ stage: 'generate-tasks', success: false })
    await recordPhaseState('generate-tasks', { failed: true })
    return { error: 'generate-tasks failed', blockId, stem, stageResults }
  }
  stageResults.push({ stage: 'generate-tasks', ...genResult })
  log(`Task spec written: ${genResult.reportFile}`)
  await recordPhaseState('generate-tasks')
  currentStage = 'implement'
}

// Load the project's validation policy once (mechanism/policy split — see planning/harness.json).
// null when absent/invalid → the Test stage falls back to the spec's ## Validation Commands and
// the UI-test stage is skipped. The engine ships no stack defaults.
const harnessCfg = await loadHarnessConfig()
log(harnessCfg
  ? `Harness config loaded: ${(harnessCfg.validation?.checks || []).length} validation check(s); uiTest ${harnessCfg.uiTest?.enabled ? 'enabled' : 'disabled'}.`
  : 'No planning/harness.json — validation falls back to the spec; UI-test disabled.')

// D6 baseline-diff: snapshot pre-implement state once (resume-safe — existing baselines kept).
// No-op when no baseline-diff checks are configured (the common case). When called from
// /sdlc-block via --from test, block already ran snapshotBlockBaselines() writing the same paths.
await snapshotBaselines(harnessCfg)

// ================================================================
// PHASES 3–5: IMPLEMENT → (FIX →) TEST → REVIEW (with retry loop)
// ================================================================
while (['implement', 'fix', 'test', 'review', 'ui-test'].includes(currentStage) && reviewAttempts < MAX_REVIEW_ATTEMPTS) {

  // ----------------------------------------------------------
  // IMPLEMENT (first pass only — retries go through fix instead)
  // ----------------------------------------------------------
  if (currentStage === 'implement') {
    phase('Implement')
    log('Running implement...')

    const implResult = await tracedAgent(`
You are the implementation agent for the SDLC pipeline.

Target:
  Spec:          ${blockId}
  Task:          ${taskNumber !== null ? `Task ${taskNumber} only` : 'all tasks'}
  Spec file:     ${specFile} (prose — Goal, Acceptance Criteria, Validation Commands)
  Tasks file:    ${tasksJsonFile} (the task list)
  Report to write: ${implementReport}

Instructions:

1. Read CLAUDE.md and planning/context.md — internalize and enforce the project's standing rules
   before writing any code or content. CLAUDE.md is the authority; do not assume any stack,
   locale-parity, narrative, or content-layout rule unless written there. Universal harness rules
   always apply: no fabricated metrics or quotes, no emoji, every change ships with tests.

2. Read the spec file and the task list: ${specFile} ${tasksJsonFile}
   ${taskNumber !== null
     ? `tasks.json is a bare array — find the object whose "task_id" is ${taskNumber}. Its "title", "description", and "files" define exactly what this task is. Implement ONLY that task. Do not implement other tasks.`
     : `tasks.json is a bare array — read every entry and execute them in array order from first to last.`}

2.5. Check for an optional breakdown file (more granular sub-steps written by /breakdown):
   Run: ls ${breakdownFile} 2>/dev/null && echo "BREAKDOWN_EXISTS" || echo "NO_BREAKDOWN"

   If BREAKDOWN_EXISTS:
     Read ${breakdownFile}
     ${taskNumber !== null
       ? `Find the section "### Step ${taskNumber}:" (may include a title after the colon).
     Use its atomic sub-steps (numbered N.1, N.2, …) as your primary execution guide.
     The inline "Verify:" commands are live checkpoints — run each one before moving to the next sub-step.`
       : `Read all "### Step N:" sections in order and use their atomic sub-steps as your execution guide.
     The inline "Verify:" commands are live checkpoints — run each one before moving to the next step.`}
     The breakdown's "## Acceptance Criteria" and "## Validation Commands" match the spec.
     tasks.json is still authoritative for scope and tasks.md for acceptance criteria; breakdown.md is authoritative
     for HOW to execute each step.

   If NO_BREAKDOWN: proceed using tasks.md only (normal behavior).

3. Execute each step in the task(s) methodically — use Read, Edit, Write, and Bash tools as needed.

4. As you implement:
   - Follow every CLAUDE.md standing rule (do not invent stack/locale/narrative rules not written there)
   - Write or update tests for new code/logic — every change ships with the validation that proves it
   - Verify any model ids / package names via the claude-api skill — never from memory

5. Run the Validation Commands from the spec to confirm correctness before writing the report.

6. Write the implementation report to: ${implementReport}

   Use EXACTLY this format:

   # Implementation Report — ${stem}

   **Date:** [run: date +%Y-%m-%d]
   **Plan:** ${specFile}
   **Scope:** ${taskNumber !== null ? `Task ${taskNumber}` : 'Full spec'}

   ## What Was Built or Changed
   - [bullet list of changes, each with file path]

   ## Files Created or Modified
   | File | Action |
   |---|---|
   | path/to/file | created / modified |

   ## Validation Output
   **Commands run:**
   \`\`\`
   [paste commands from spec]
   \`\`\`
   **Results:**
   \`\`\`
   [paste actual output]
   \`\`\`
   Status: PASSED / FAILED

   ## Decisions and Trade-offs
   [explain any non-obvious choices; reference CLAUDE.md DECISIONS entries if applicable]

   ## Follow-up Work
   [anything intentionally deferred to later tasks]

   ## git diff --stat
   \`\`\`
   [run: git diff --stat]
   \`\`\`

7. Commit your changes now. Never use git add -A or git add . — stage files explicitly by name.

   Run: git status
   Identify all changed/new source/content files (from git status) plus the implement report.

   Stage code/content files first, then the report:
     git add <each changed source/test file> ${implementReport}  (list each file explicitly)

   Commit using HEREDOC:
     git commit -m "$(cat <<'EOF'
     feat: implement ${stem}


     EOF
     )"

   If validation failed (Status: FAILED above), use "fix:" prefix instead of "feat:".
   Run: git log --oneline -1
   Capture the short hash from that output.

Return your result using the StructuredOutput tool:
  reportFile: "${implementReport}"
  success: true if implementation completed without critical errors
  filesModified: array of every source file you created or modified
  commitHash: the 7-character short hash from git log --oneline -1 (empty string if commit failed)
  notes: one-line summary
`, withModel({ label: 'implement', schema: STAGE_SCHEMA, phase: 'Implement' }, MODEL.implement))

    if (!implResult) {
      log('Implement agent returned null — aborting pipeline')
      stageResults.push({ stage: 'implement', success: false, notes: 'Agent returned null' })
      await recordPhaseState('implement', { failed: true })
      break
    }
    stageResults.push({ stage: 'implement', ...implResult })
    if (!implResult.success) {
      log('Implement reported failure — aborting pipeline')
      await recordPhaseState('implement', { failed: true })
      break
    }
    await recordPhaseState('implement')
    currentStage = 'test'
  }

  // ----------------------------------------------------------
  // FIX (review retry path — targeted fix, overwrites implement report)
  // ----------------------------------------------------------
  if (currentStage === 'fix') {
    phase('Fix')
    const fixPass = reviewAttempts + 1
    log(`Running fix (pass ${fixPass}) — targeting review failures...`)

    // Last fix pass before the loop can give up → escalate the model.
    const fixModel = (ESCALATION_MODEL && fixPass === MAX_REVIEW_ATTEMPTS) ? ESCALATION_MODEL : MODEL.fix
    if (fixModel !== MODEL.fix) log(`Final fix pass — escalating model to ${fixModel}.`)

    const fixResult = await tracedAgent(`
You are the fix agent for the SDLC pipeline. Your job is to make targeted fixes for the failures identified
in the last review — NOT to re-implement the entire spec from scratch.

Target:
  Spec:             ${blockId}
  Task:             ${taskNumber !== null ? `Task ${taskNumber}` : 'all tasks'}
  Spec file:        ${specFile}
  Review report:    ${reviewReport}
  Prior implement report: ${implementReport}
  Report to write:  ${implementReport}  ← overwrites this slot (Fix Pass ${fixPass})

GATE CHECKS (do these first):
1. Run: ls ${reviewReport} 2>/dev/null && echo EXISTS || echo MISSING
   If the review report does not exist → stop immediately, return success: false, notes: "No review report found."
2. Read ${reviewReport} and extract the verdict line.
   If verdict is PASS → stop, return success: false, notes: "Review verdict is already PASS — run /document instead."

Instructions:

1. Read the review report: ${reviewReport}
   Extract ONLY:
   - The failing rows from "## Acceptance Criteria Check" (PARTIAL or NOT_MET rows)
   - The entire "## Issues Found" section
   - The "## Fresh Test Results" section (to understand test failures)

2. Read the prior implement report: ${implementReport}
   Extract:
   - The "## Files Created or Modified" table — this is the baseline file list
   - Look for "Fix Pass" in the title to determine the current fix pass count (starts at 1 if none)

3. If a breakdown file exists, check the relevant sub-steps for original intent:
   Run: ls ${breakdownFile} 2>/dev/null && echo EXISTS || echo MISSING
   If EXISTS: read ${breakdownFile} and find the "### Step ${taskNumber !== null ? taskNumber : 'N'}:" section.
   Use it to understand what the original implementation was supposed to do for the failing criterion.
   Do NOT re-implement from scratch — use it only as context for the targeted fix.

4. Read the source files that are relevant to the failing criteria only.
   Do NOT read files unrelated to the issues found.

5. Make the MINIMUM targeted changes required to address the failing criteria and issues.
   - Fix ONLY what the review identified as failing
   - Do not modify passing criteria or unrelated code
   - Follow all CLAUDE.md standing rules

6. Run ONLY the Validation Commands from the spec (not the full validation suite — that belongs to /test):
   Find the "## Validation Commands" section of ${specFile} and run those commands.

7. Build the complete file list: union of the prior implement table PLUS any new files you touched.

8. Overwrite the implement report at: ${implementReport}
   This overwrites the slot. Downstream commands (/test, /review-task, /document) all read this slot.

   Use EXACTLY this format:

   # Fix Pass ${fixPass} — ${stem}

   **Date:** [run: date +%Y-%m-%d]
   **Plan:** ${specFile}
   **Scope:** ${taskNumber !== null ? `Task ${taskNumber}` : 'Full spec'}
   **Fix pass:** ${fixPass}

   ## Failures Addressed
   [list each failing criterion or issue from the review report, and how it was fixed]

   ## Changes Made
   - [bullet list of targeted changes with file paths — only what was changed this pass]

   ## Files Created or Modified
   | File | Action |
   |---|---|
   | path/to/file | created / modified |
   [IMPORTANT: include ALL files from the prior implement report PLUS any newly touched files]

   ## Validation Output
   **Commands run:**
   \`\`\`
   [spec validation commands]
   \`\`\`
   **Results:**
   \`\`\`
   [actual output]
   \`\`\`
   Status: PASSED / FAILED

   ## Decisions and Trade-offs
   [explain any non-obvious choices]

   ## git diff --stat
   \`\`\`
   [run: git diff --stat]
   \`\`\`

9. Commit your changes now. Never use git add -A or git add . — stage files explicitly by name.

   Run: git status
   Identify all changed/new source/content files (from git status) plus the updated implement report.

   Stage targeted changes and the updated report:
     git add <each changed source/test file> ${implementReport}  (list each file explicitly)

   Commit using HEREDOC:
     git commit -m "$(cat <<'EOF'
     fix: fix pass ${fixPass} for ${stem}


     EOF
     )"

   Run: git log --oneline -1
   Capture the short hash from that output.

Return your result using the StructuredOutput tool:
  reportFile: "${implementReport}"
  success: true if fixes were applied and validation passed
  filesModified: array of source files you actually changed this pass (not the full accumulated list)
  commitHash: the 7-character short hash from git log --oneline -1 (empty string if commit failed)
  notes: one-line summary of what was fixed
`, withModel({ label: `fix-${fixPass}`, schema: STAGE_SCHEMA, phase: 'Fix' }, fixModel))

    if (!fixResult) {
      log('Fix agent returned null — aborting pipeline')
      stageResults.push({ stage: 'fix', attempt: fixPass, success: false, notes: 'Agent returned null' })
      await recordPhaseState('fix', { failed: true })
      break
    }
    stageResults.push({ stage: 'fix', attempt: fixPass, ...fixResult })
    if (!fixResult.success) {
      log(`Fix pass ${fixPass} reported failure — aborting pipeline`)
      await recordPhaseState('fix', { failed: true })
      break
    }
    await recordPhaseState('fix')
    currentStage = 'test'
  }

  // ----------------------------------------------------------
  // TEST
  // ----------------------------------------------------------
  if (currentStage === 'test') {
    phase('Test')
    log('Running the project validation suite...')

    const testResult = await tracedAgent(`
You are the test agent for the SDLC pipeline. Run the project's validation suite and write a test report.

IMPORTANT — run ONLY the checks enumerated below (sourced from planning/harness.json and the spec's
Validation Commands). Do NOT invent or add checks that are not listed here; if a check name does not
appear in the list below, it is out of scope for this run.

Target:
  Spec:            ${specFile}
  Report to write: ${testReport}

Run EVERY check below IN ORDER using the Bash tool. Capture the full output (stdout + stderr) for
each. Run from the repo root.

${renderCheckList(harnessCfg)}

For each check record:
  test_name: descriptive name
  passed: true if exit code was 0
  execution_command: exact command run
  test_purpose: what this check verifies
  error: full error output if failed, empty string if passed

Write the test report to: ${testReport}

Use EXACTLY this format:

# Test Report — ${stem}

**Date:** [run: date +%Y-%m-%d]
**Spec:** ${specFile}
**Scope:** ${taskNumber !== null ? `Task ${taskNumber}` : 'Full spec'}

## Summary

| Test | Result | Error |
|---|---|---|
[FAILED rows first, then PASSED rows]

## Full Results (JSON)
\`\`\`json
[
  {
    "test_name": "...",
    "passed": true/false,
    "execution_command": "...",
    "test_purpose": "...",
    "error": "..."
  },
  ...
]
\`\`\`

Return your result using the StructuredOutput tool:
  reportFile: "${testReport}"
  allPassed: true only if EVERY check passed (each exit code 0)
  passCount: integer count of checks that passed
  failCount: integer count of checks that failed
  failedTests: array of test_name strings for failed checks
  notes: one-line summary
`, withModel({ label: 'test', schema: TEST_SCHEMA, phase: 'Test' }, MODEL.test))

    if (!testResult) {
      log('Test agent returned null — recording failure, continuing to review')
      stageResults.push({ stage: 'test', attempt: reviewAttempts + 1, allPassed: false, success: false, notes: 'Agent returned null' })
    } else {
      stageResults.push({ stage: 'test', attempt: reviewAttempts + 1, ...testResult, success: testResult.allPassed })
      if (!testResult.allPassed) {
        log(`Test failures (${testResult.failCount}): ${(testResult.failedTests || []).join(', ')}`)
      } else {
        log(`All ${testResult.passCount} checks passed`)
      }
    }
    await recordPhaseState('test')
    currentStage = 'review'
  }

  // ----------------------------------------------------------
  // REVIEW
  // ----------------------------------------------------------
  if (currentStage === 'review') {
    phase('Review')
    reviewAttempts++
    log(`Running review (attempt ${reviewAttempts}/${MAX_REVIEW_ATTEMPTS})...`)

    // Final review attempt before the loop can give up → escalate the model.
    const reviewModel = (ESCALATION_MODEL && reviewAttempts === MAX_REVIEW_ATTEMPTS) ? ESCALATION_MODEL : MODEL.review
    if (reviewModel !== MODEL.review) log(`Final review attempt — escalating model to ${reviewModel}.`)

    const reviewResult = await tracedAgent(`
You are the review agent for the SDLC pipeline. Verify the implementation against the spec and issue a verdict.

Target:
  Spec:             ${blockId}
  Task:             ${taskNumber !== null ? `Task ${taskNumber}` : 'all tasks'}
  Spec file:        ${specFile}
  Implement report: ${implementReport}
  Test report:      ${testReport}
  Report to write:  ${reviewReport}

Instructions:

1. Read the spec file: ${specFile}
   Extract the COMPLETE "## Acceptance Criteria" section — this is your checklist.

2. Read the implement report: ${implementReport}
   Understand what was changed and the decisions made.

3. Read the test report: ${testReport}
   This is historical context. You will run your own fresh tests below.

4. Run the FRESH authoritative checks (this result determines the verdict, not the test report):
   Re-run each GATING validation check — those whose test_purpose in the test report (${testReport})
   is marked "GATING", i.e. the checks with gates:true in planning/harness.json. Use each check's
   execution_command verbatim. If the project ships no harness suite, re-run the commands in the
   spec's "## Validation Commands" section. A fresh failure of any gating check ALWAYS prevents PASS.

5. Scope your review to ${taskNumber !== null ? `Task ${taskNumber} only` : 'all tasks'}.
   ${taskNumber !== null
     ? `The spec may list criteria spanning multiple tasks. For each criterion:
   - If tagged for a different task (e.g. "[T${taskNumber + 1}]") OR clearly belongs to a later
     task's scope → mark SKIP with a note. SKIP criteria do NOT affect the verdict.
   - All others: evaluate normally.`
     : `Evaluate all criteria — this is a full-spec run.`}

   For each in-scope criterion, read the relevant source files and determine one of:
   MET — criterion is fully satisfied by the current code
   PARTIAL — criterion is partially satisfied
   NOT_MET — criterion is not satisfied
   Also check compliance with the project's CLAUDE.md standing rules — a violation is a failing
   criterion. CLAUDE.md is the authority: do not assume any stack, locale-parity, narrative, or
   content-layout rule unless it is written there. Universal harness rules always apply: no
   fabricated metrics or quotes, no emoji, every change ships with tests.
   IDENTITY INTEGRITY: flag any handle, profile link, or URL that contradicts the verified
   identities/handles declared in CLAUDE.md, or that appears fabricated. Mark such a criterion
   NOT_MET — only the CLAUDE.md-declared identities are authoritative.

5.5. HARD RULE — do NOT fix environment or infrastructure issues yourself:
   If a fresh gating check fails due to environment/infrastructure causes (missing module files,
   missing hooks, import/dependency resolution failures), do NOT fix them yourself.
   Return verdict: FAIL with failureReasons: ["Environment issue — missing files; the fix
   agent must resolve them and re-run the pipeline."]. A review agent that resolves
   infrastructure issues itself bypasses the test gate that validates the fix.

6. Determine the verdict:
   PASS — ALL criteria are MET AND every fresh gating check passes (exit 0)
   PARTIAL — some criteria are PARTIAL, OR gating checks pass but some criteria are not fully met
   FAIL — any criterion is NOT_MET, OR any fresh gating check fails

   A fresh gating-check failure ALWAYS prevents PASS — even if all acceptance criteria appear met from reading the code.

7. Write the review report to: ${reviewReport}

   Use EXACTLY this format:

   # Review Report — ${stem}

   **Date:** [run: date +%Y-%m-%d]
   **Spec:** ${specFile}
   **Scope:** ${taskNumber !== null ? `Task ${taskNumber}` : 'Full spec'}
   **Verdict:** PASS / PARTIAL / FAIL

   ## Acceptance Criteria Check
   | Criterion | Status | Evidence |
   |---|---|---|
   | [criterion text] | MET / PARTIAL / NOT_MET | [file:line or test name] |

   ## Fresh Test Results
   [paste the fresh gating-check output — per-check pass/fail and any failure output]

   ## Verdict: PASS / PARTIAL / FAIL
   [one paragraph explaining the verdict]

   ## Issues Found
   [list of specific problems — empty section if PASS]

   ## Next Steps
   [what to do based on the verdict]

Return your result using the StructuredOutput tool:
  reportFile: "${reviewReport}"
  verdict: "PASS", "FAIL", or "PARTIAL"
  failureReasons: array of strings describing what failed (empty array if PASS)
  unmetCriteria: array of criterion texts that were NOT_MET or PARTIAL (empty if PASS)
  notes: one-line summary
`, withModel({ label: `review-${reviewAttempts}`, schema: REVIEW_SCHEMA, phase: 'Review' }, reviewModel))

    if (!reviewResult) {
      log(`Review agent returned null (attempt ${reviewAttempts}) — treating as FAIL`)
      lastReviewResult = { verdict: 'FAIL', failureReasons: ['Review agent returned null'], unmetCriteria: [], reportFile: reviewReport }
      stageResults.push({ stage: 'review', attempt: reviewAttempts, verdict: 'FAIL', success: false, notes: 'Agent returned null' })
    } else {
      lastReviewResult = reviewResult
      stageResults.push({ stage: 'review', attempt: reviewAttempts, ...reviewResult, success: reviewResult.verdict === 'PASS' })
      log(`Review verdict: ${reviewResult.verdict} (attempt ${reviewAttempts}/${MAX_REVIEW_ATTEMPTS})`)
    }
    await recordPhaseState('review')

    if (lastReviewResult.verdict === 'PASS') {
      currentStage = 'ui-test'
    } else if (reviewAttempts < MAX_REVIEW_ATTEMPTS) {
      log(`Review ${lastReviewResult.verdict} — running fix pass ${reviewAttempts + 1}/${MAX_REVIEW_ATTEMPTS}...`)
      currentStage = 'fix'
    } else {
      log(`Review FAILED after ${MAX_REVIEW_ATTEMPTS} attempts — skipping to wrap-up with FAIL status`)
      currentStage = 'wrap-up'
    }
  }

  // ----------------------------------------------------------
  // UI TEST (after Review PASS — browser smoke check)
  // ----------------------------------------------------------
  if (currentStage === 'ui-test') {
    phase('UI Test')

    if (!harnessCfg?.uiTest?.enabled) {
      log('UI test stage disabled (harness.json uiTest.enabled is false or config absent) — SKIPPED.')
      stageResults.push({ stage: 'ui-test', verdict: 'SKIPPED', success: true, notes: 'uiTest disabled in harness.json' })
      await recordPhaseState('ui-test')
      currentStage = 'document'
    } else {
      log('Running UI test stage...')
      const ui = renderUiTestPrompt(harnessCfg, harnessCfg.uiTest.port ?? 3000)

      const uitestResult = await tracedAgent(`
You are the UI test agent for the SDLC pipeline. Run a quick live browser smoke check using
playwright-cli to catch visual/runtime regressions that the validation suite cannot catch.

Target:
  Spec:              ${blockId}
  Task:              ${taskNumber !== null ? `Task ${taskNumber}` : 'all tasks'}
  Implement report:  ${implementReport}
  Report to write:   ${uitestReport}
  Dev server URL:    http://localhost:${ui.port}

STEP 1 — Triage: did this work change application source?

  Read the implement report: ${implementReport}
  Scan the "Files Modified" list. If EVERY changed file is documentation/markdown or planning
  metadata only (no application source), set verdict = SKIPPED, write the report, and stop.
  Otherwise continue to STEP 2.

STEP 2 — Start the dev server.

  Check if port ${ui.port} is already in use:
    lsof -ti :${ui.port} 2>/dev/null && echo "PORT_IN_USE" || echo "PORT_FREE"

  If PORT_IN_USE: the server is already running — skip to STEP 3.
  If PORT_FREE: start the server in the background:
    ${ui.devCmd} > /tmp/uitest-run.log 2>&1 &
    echo "SERVER_PID=$!"

  Wait up to 60 seconds for the ready signal:
    for i in $(seq 1 30); do grep -q "${ui.ready}" /tmp/uitest-run.log 2>/dev/null && echo "READY" && break; sleep 2; done
    tail -20 /tmp/uitest-run.log

  If "READY" not seen within 60 s, write the report with verdict = FAIL (dev server did not start),
  kill the background process, and stop.

STEP 3 — Run smoke checks using playwright-cli, one per configured route.

  Open a browser session:
    playwright-cli open http://localhost:${ui.port}${ui.routes[0]}

  For each route below, record PASS, WARN, or FAIL with quoted evidence:

${ui.routeChecks}

  Also confirm at least one internal link works: from any route's snapshot, pick an internal link
  and \`playwright-cli click <ref>\`, then \`playwright-cli snapshot\` — the target must load without
  an error page.

  Close the browser session:
    playwright-cli close

STEP 4 — Kill the dev server (only if YOU started it in STEP 2).
  If SERVER_PID was captured: kill $SERVER_PID 2>/dev/null || true

STEP 5 — Determine verdict and write report.

  Verdict rules:
  - PASS:    All route checks passed with no errors.
  - WARN:    All checks passed but console warnings were found.
  - FAIL:    One or more checks failed — list each with quoted evidence.
  - SKIPPED: No application source changed (from STEP 1 triage).

  Write the report to ${uitestReport}:
  \`\`\`markdown
  # UI Test Report: ${stem}

  **Verdict:** <PASS|WARN|FAIL|SKIPPED>
  **Date:** <today>

  ## Smoke Check Results

  | Check | Result | Notes |
  |---|---|---|
${ui.routeRows}

  ## Summary
  <one paragraph — what was tested and what was found>
  \`\`\`

  Commit the report:
    git add ${uitestReport}
    git commit -m "test(ui): ui smoke check for ${stem}"

Return the result using StructuredOutput.
`, withModel({ label: 'ui-test', schema: UI_TEST_SCHEMA, phase: 'UI Test' }, MODEL.uiTest))

      if (!uitestResult) {
        log('UI test agent returned null — treating as WARN, continuing to document')
        stageResults.push({ stage: 'ui-test', verdict: 'WARN', success: true, notes: 'Agent returned null' })
        currentStage = 'document'
      } else {
        stageResults.push({ stage: 'ui-test', ...uitestResult, success: uitestResult.verdict !== 'FAIL' })
        log(`UI test verdict: ${uitestResult.verdict}`)

        if (uitestResult.verdict === 'FAIL') {
          if (reviewAttempts < MAX_REVIEW_ATTEMPTS) {
            log(`UI test FAIL — running fix pass ${reviewAttempts + 1}/${MAX_REVIEW_ATTEMPTS}...`)
            currentStage = 'fix'
          } else {
            log(`UI test FAILED after ${MAX_REVIEW_ATTEMPTS} attempts — skipping to wrap-up`)
            currentStage = 'wrap-up'
          }
        } else {
          currentStage = 'document'
        }
      }
      await recordPhaseState('ui-test')
    }
  }
} // end implement→fix→test→review→ui-test retry loop

// ================================================================
// PHASE 6: DOCUMENT (gates on PASS verdict)
// ================================================================
if (currentStage === 'document') {
  phase('Document')
  log('Running document stage...')

  const docResult = await tracedAgent(`
You are the documentation agent for the SDLC pipeline. Surgically patch docs/ to reflect the completed implementation.

Target:
  Spec:             ${blockId}
  Task:             ${taskNumber !== null ? `Task ${taskNumber}` : 'all tasks'}
  Review report:    ${reviewReport}
  Implement report: ${implementReport}
  Report to write:  ${documentReport}

Instructions:

1. Read the review report: ${reviewReport}
   GATE CHECK: If the verdict is FAIL or PARTIAL, stop immediately.
   Return: success: false, notes: "Blocked — review verdict was not PASS".

2. Read the implement report: ${implementReport}
   Find the "## Files Created or Modified" table. This scopes which source files changed.

2b. CHECK — does docs/ have any project-facing docs?
   Run: ls docs/ 2>/dev/null | grep -v '^workflows$' | grep '\\.md$' | wc -l
   If the count is 0 (no project docs exist yet), switch to BOOTSTRAP MODE:
   - Read every source file listed in step 2's table.
   - Create appropriate reference docs from scratch based on what the source actually contains.
     At minimum: docs/architecture.md (module map, key types, data flow). Add docs/cli.md for
     CLIs, docs/api-reference.md for servers/APIs, docs/pages.md for web apps — as applicable.
   - Create docs/index.md if it does not exist; add a row per created doc.
   - Every new file must include OKF frontmatter (required: type, title, description).
   - Skip steps 3–5 and go directly to step 6 (report + commit) after creating docs.
   If count > 0: proceed with surgical patch in steps 3–5.

3. For each source file in that table, identify which docs/*.md files reference it.
   Search for the filename and the key component/function/route names that changed.
   Use Bash: grep -rl "ComponentName\\|function_name\\|filename" docs/ 2>/dev/null

4. Read each relevant doc file (use the Read tool).

5. Surgically patch ONLY the affected sections:
   - Update component signatures, prop tables, route lists, descriptions that changed
   - Add documentation for any new public APIs or content areas
   - Never delete documented items that still exist in the code
   - Keep surrounding unchanged content intact
   - Use the Edit tool — never rewrite entire doc files

6. IMPORTANT: If a top-level architecture/overview doc (e.g. a docs/ index, or changes that touch core
   wiring / entry points / routing) needs updating, add it to the "NEEDS_REVIEW" section of
   the document report but do NOT edit that file directly. Never edit CLAUDE.md.

7. Write the document report to: ${documentReport}

   Use EXACTLY this format:

   # Documentation Report — ${stem}

   **Date:** [run: date +%Y-%m-%d]
   **Spec:** ${specFile}
   **Verdict gate:** PASS (confirmed)

   ## Docs Patched
   | Doc File | Section Updated | Change Summary |
   |---|---|---|

   ## Docs Flagged NEEDS_REVIEW
   [list any docs that need human review — flag a top-level architecture/overview doc if it references changed files]

   ## Docs Clean (checked, no changes needed)
   [list any docs checked but requiring no updates]

7. Commit your changes now. Never use git add -A or git add . — stage files explicitly by name.

   Run: git status
   Identify all changed doc files under docs/ and the document report.

   If no doc files were actually patched (only the report was written), commit just the report:
     git add ${documentReport}

   If docs were patched, stage them and the report:
     git add docs/file1.md docs/file2.md ${documentReport}  (list each file explicitly)

   Commit using HEREDOC:
     git commit -m "$(cat <<'EOF'
     docs: update docs for ${stem}


     EOF
     )"

   Run: git log --oneline -1
   Capture the short hash from that output.

Return your result using the StructuredOutput tool:
  reportFile: "${documentReport}"
  success: true if docs were checked and report written (even if no changes were needed)
  filesModified: array of doc files that were actually patched (empty array if none needed changes)
  commitHash: the 7-character short hash from git log --oneline -1 (empty string if commit failed)
  notes: one-line summary
`, withModel({ label: 'document', schema: STAGE_SCHEMA, phase: 'Document' }, MODEL.document))

  if (!docResult) {
    stageResults.push({ stage: 'document', success: false, notes: 'Document agent returned null' })
    log('Document agent returned null')
  } else {
    stageResults.push({ stage: 'document', ...docResult })
    if (!docResult.success) {
      log(`Document stage blocked: ${docResult.notes}`)
    } else {
      log(`Docs updated: ${(docResult.filesModified || []).join(', ') || 'none needed changes'}`)
    }
  }
  await recordPhaseState('document')
  currentStage = 'wrap-up'
}

// ================================================================
// PHASE 7: WRAP-UP — log-work + commit + workflow report
// ================================================================
phase('Wrap-up')

const finalVerdict = lastReviewResult?.verdict || 'NOT_REACHED'
const stageResultsSummary = stageResults
  .map(r => `${r.stage}${r.attempt ? `(#${r.attempt})` : ''}: ${r.success ? (r.verdict || 'OK') : 'FAILED'}`)
  .join(' → ')

log(`Wrap-up. Final verdict: ${finalVerdict}. Pipeline: ${stageResultsSummary}`)

// ----------------------------------------------------------------
// WRAP-UP (D14): log-work + finalize in one pass — update status.md + append log, write the
// workflow report, then commit every remaining planning file in a single chore commit.
// ----------------------------------------------------------------
log('Running wrap-up: status/log update + workflow report + chore commit...')

// Write the final run-state (phase=wrap-up + the cumulative tokens block) BEFORE the wrap-up agent
// runs, so the wrap-up's single chore commit stages it (defer-to-wrap-up cadence — no per-phase
// commit). The wrap-up agent's own token cost is not yet in the metrics here; that is acceptable and
// precedented (the wrap-up stage has historically been absent from the persisted token roll-up).
await recordPhaseState('wrap-up')

const stageTable = stageResults.map(r => {
  const label = r.stage + (r.attempt ? ` (attempt ${r.attempt})` : '')
  const status = r.verdict ? r.verdict : (r.success ? 'completed' : 'FAILED')
  const file = r.reportFile || r.workflowReportFile || '—'
  const commit = r.commitHash ? r.commitHash.substring(0, 7) : '—'
  const notes = (r.notes || '').substring(0, 60)
  return `| ${label} | ${status} | ${file} | ${commit} | ${notes} |`
}).join('\n')

const wrapupResult = await tracedAgent(`
You are the wrap-up agent for the SDLC pipeline. You do THREE things in one pass — update status.md,
append a log entry, then write the workflow report — and finish by committing all the remaining
planning files in one chore commit.

Target:
  Spec:            ${blockId}
  Task:            ${taskNumber !== null ? `Task ${taskNumber}` : 'all tasks (full spec)'}
  Final verdict:   ${finalVerdict}
  Review attempts: ${reviewAttempts}
  Pipeline summary: ${stageResultsSummary}
  Workflow report to write: ${workflowReport}

PART A — Update status.md + log (code/doc changes are already committed by their agents):

1. Read planning/status.md
2. Read ${specFile}
3. Read log.md (at the repo root — NOT in planning/)
4. Run: git log --oneline -15
   (git log shows the full picture of commits made during this run.)

5. Update planning/status.md using the Edit tool:
   ${taskNumber !== null
     ? `- Task ${taskNumber} is done. Check if there are more tasks in the spec.
        - If more tasks remain: keep spec status as "In progress", update "Current focus" to the next task.
        - If this was the last task: flip spec status to "Done", update "Current focus" to the next spec.`
     : `- Full spec "${blockId}" is done. Flip its Status to "Done" in the Progress Table.
        - Update "Current focus" to the next spec or phase.`}
   - Update "Last updated" — run: date +%Y-%m-%d

5b. Flip the block's AUTHORED status in planning/state.json (skip this entire step silently if the
    repo has no planning/state.json). state.json is the authoritative block graph — leaving it stale
    poisons every derived surface, because \`mev emit-state\` reads this field and NEVER infers
    completion from status.md (the sync is one-way by design).
    ${taskNumber !== null
      ? `- Only proceed if you flipped the spec's status.md status to "Done" above (i.e. this was the last task). If tasks remain, leave state.json untouched and set blockStatusFlipped to "".`
      : `- The full spec is done, so proceed.`}
    - Resolve the block's canonical ID from the status.md Progress Table row you just edited (the
      <BlockID> column, or the id that row maps to in state.json). Find that block in state.json
      tracks[].blocks[] — search EVERY track. If found, set its "status" to "closed" (the only
      authored close value). If NOT found, report it in notes and do NOT fabricate a block entry.
    - Validate the file is still valid JSON:
        python3 -c "import json;json.load(open('planning/state.json'))"
    - Then regenerate the derived surfaces from the authored graph. This run is ON MAIN (not a linked
      worktree), so emit-state is safe here:
        mev emit-state --write
      If \`mev\` or brain.toml is absent (a standalone repo), skip this command silently — the
      state.json flip still stands. Do NOT hand-reimplement focus/rollup/wave-table derivation.
    - Set blockStatusFlipped to the block id you closed (or "" if none).

6. Append a new entry to log.md (prepend at the TOP, newest entries first):

   ## [YYYY-MM-DD — run date +%Y-%m-%d to get this]
   [One paragraph: what was implemented, how the review went (${finalVerdict} verdict${reviewAttempts > 1 ? ` after ${reviewAttempts} attempts` : ''}), any notable findings, decisions made. End with: "Next: [next task or spec]."]

   \`\`\`
   [git log --oneline -5 output — the commits made during this pipeline run]
   \`\`\`

7. If the implement report's "Decisions and Trade-offs" section contains any settled choices, mention them in your notes — but do NOT edit planning/decisions/ yourself (that is a manual step).

PART A.5 — Living-artifact amendment log (D18). Make the spec record how it actually ran:
   a. Review the implement/fix reports and the review verdict for genuine DEVIATIONS from the spec as
      written — a task implemented materially differently than specified, a scope adjustment, a
      substitution, a deferral. Routine successful implementation is NOT a deviation.
   b. For EACH genuine deviation, append ONE dated line to the "## Amendment Log" section of
      ${specFile} using the Edit tool (append-only — never rewrite existing lines):
        - YYYY-MM-DD [<stage>] <what changed vs the spec, and why>
      If the section still shows only "_No amendments yet._" and you are adding the first line, replace
      that placeholder text with your line(s). If there were NO genuine deviations, leave the section
      unchanged.
   c. If ${specFile} has a provenance stub near the top (a line with "**Status:**" / "**Last run:**"),
      update "**Last run:**" to today's date (date +%Y-%m-%d) and "**Status:**" to the spec's new status.
   d. Return every appended line in the amendments[] field (empty array if none).

PART B — Write the workflow report to: ${workflowReport}

  Use EXACTLY this format:

  # SDLC Workflow Report — ${blockId}${taskNumber !== null ? ` Task ${taskNumber}` : ''}

  **Date:** [run: date +%Y-%m-%d]
  **Spec:** ${blockId}
  **Task scope:** ${taskNumber !== null ? `Task ${taskNumber}` : 'All tasks'}
  **Pipeline started from:** ${scout.startStage}
  **Review attempts:** ${reviewAttempts} of ${MAX_REVIEW_ATTEMPTS} max

  ## Final Verdict
  ${finalVerdict} — [one sentence explanation]

  ## Stage Results

  | Stage | Status | Report | Commit | Notes |
  |---|---|---|---|---|
  ${stageTable}

  ## Key Findings
  [Summarize: what was implemented, notable decisions, any bilingual-parity deferrals]

  ## Files Modified
  [List source files that were created or modified — from the implement report]

  ## Docs Updated
  [List doc files patched — from the document report; list any NEEDS_REVIEW flags]

  ## Commits (this pipeline run)
  [Paste the relevant lines from git log --oneline — the implement, fix, docs commits made during this run]

PART C — Commit the remaining planning files as a single chore commit.
  Never use git add -A or git add . — stage files explicitly by name.

  Run: git status
  Look for any uncommitted files in: planning/status.md, log.md, ${specFile}
  (modified iff you appended an amendment line or updated its provenance stub in PART A.5),
  ${testReport}, ${reviewReport},
  and ${workflowReport} (which you just wrote).

  Stage them:
    git add planning/status.md log.md ${workflowReport}
    git add planning/state.json 2>/dev/null || true
    git add ${stateFile} 2>/dev/null || true
    git add ${specFile} 2>/dev/null || true
    git add ${testReport} 2>/dev/null || true
    git add ${reviewReport} 2>/dev/null || true
    git add ${uitestReport} 2>/dev/null || true
    (only add files that actually exist and are untracked/modified)
  ${stateFile} is the committed run-state (phase trail + token roll-up) written just before this
  stage — always stage it so the run leaves a clean tree.

  Commit using HEREDOC:
    git commit -m "$(cat <<'EOF'
    chore: wrap up ${stem}


    EOF
    )"

  Run: git log --oneline -1
  Capture the short hash.

Return your result using the StructuredOutput tool:
  statusUpdated: true if status.md was successfully updated
  devlogUpdated: true if log.md was successfully updated
  nextFocus: the new "Current focus" value written to status.md
  workflowReportFile: "${workflowReport}"
  commitMessage: "chore: wrap up ${stem}"
  commitHash: the 7-character short hash from git log --oneline -1
  amendments: the dated amendment-log lines you appended to the spec in PART A.5 (empty array if none)
  blockStatusFlipped: the state.json block id you flipped to "closed" in PART A step 5b (or "" if none)
  notes: any follow-up items (settled decisions to add to planning/decisions/, NEEDS_REVIEW doc flags)
`, withModel({ label: 'wrap-up', schema: WRAPUP_SCHEMA, phase: 'Wrap-up' }, MODEL.wrapup))

if (wrapupResult) {
  stageResults.push({ stage: 'wrap-up', ...wrapupResult, success: wrapupResult.statusUpdated && wrapupResult.devlogUpdated })
  if (wrapupResult.notes) log(`Decisions to log: ${wrapupResult.notes}`)
  if (wrapupResult.amendments?.length) log(`Spec amendments (D18): ${wrapupResult.amendments.length} line(s) appended to ${specFile}`)
  if (wrapupResult.blockStatusFlipped) log(`state.json: block "${wrapupResult.blockStatusFlipped}" → closed; derived surfaces regenerated (mev emit-state --write).`)
  log(`Committed: ${wrapupResult.commitMessage}`)
  log(`Workflow report: ${wrapupResult.workflowReportFile}`)
} else {
  stageResults.push({ stage: 'wrap-up', success: false, notes: 'Agent returned null' })
  log('Wrap-up agent returned null — manual status/log update, commit, and workflow report may be needed')
}

const tokensBlock = buildTokensBlock()
log(`Token roll-up: ${tokensBlock.total.inTokEst} inTokEst${tokensBlock.total.outTok ? ` | ${tokensBlock.total.outTok} outTok` : ''} across ${tokensBlock.stages.length} stage(s) — persisted in ${stateFile}.`)
log(`Pipeline complete. Verdict: ${finalVerdict} | Attempts: ${reviewAttempts} | Report: ${workflowReport}`)
log('Next: run /close-out to verify coverage + patch docs before handing off.')

return {
  blockId,
  taskNumber,
  stem,
  finalVerdict,
  reviewAttempts,
  startStage: scout.startStage,
  workflowReport: wrapupResult?.workflowReportFile || workflowReport,
  commitMessage: wrapupResult?.commitMessage,
  stateFile,
  tokens: tokensBlock,
  stageResults
}
