// =============================================================================
// sdlc-block — Block-level roadmap orchestrator (a branch train of /sdlc-flow runs)
// =============================================================================
//
// Drives a whole ROADMAP (a master-plan-format file) to completion by fanning out ONE /sdlc-flow per
// independent BLOCK over dependency-ordered waves, producing a branch train of reviewable PRs. This
// REPLACES the legacy task-level wave machine (which ran task-level waves WITHIN a single spec and
// "almost always hit an inter-task merge conflict", D30): blocks in a wave are independent BY
// CONSTRUCTION (the master-plan's per-block Files + Out-of-scope contract), and the proven /sdlc-flow
// engine — one shared worktree, per-task test→fix loop, one end review, a PR — is the inner unit.
//
// USAGE
//   /sdlc-block                              orchestrate planning/master-plan.md (every block)
//   /sdlc-block <plan-file>                  orchestrate a /plan output (planning/plan-<slug>/plan.md)
//   /sdlc-block <plan-file> --blocks 0-1     scope to a phase/wave selection (see --blocks)
//   /sdlc-block --auto-merge                 merge each block into the base in dependency order
//   /sdlc-block --no-pr                      produce the branch train only (no PRs)
//   /sdlc-block --base develop               branch/merge against a base other than main
//   /sdlc-block --resume                     re-read block-orchestration-state.json and continue
//
//   ARGS
//     [planFileOrSlug]        optional 1st positional — a path to a master-plan-format file, OR a slug
//                             resolved to planning/<slug>/plan.md. Default: planning/master-plan.md.
//     --base <branch>         the base branch the train forks from / merges into (default: main).
//     --auto-merge            merge each completed block branch into <base> in dependency order
//                             (resolving conflicts); the train IS the base. Default off.
//     --no-pr                 child flows produce branches only — no PRs anywhere.
//     --max-parallel-blocks N max /sdlc-flow runs in flight per wave (default 3).
//     --blocks <sel>          phase selection (e.g. 0, 0-1, 0,2) — only those phases' blocks run.
//     --resume                load block-orchestration-state.json, skip done blocks, continue.
//
// MODEL
//   sonnet : pre-flight, enumerate-blocks, merge, report   |   opus : per-block generate-tasks (planning)
//   haiku  : state-writer   |   the inner /sdlc-flow carries its OWN model tiering per stage.
//
// BRANCH TRAIN
//   The orchestrator keeps a "train" branch checked out at the MAIN repo root; every wave's child
//   /sdlc-flow worktrees fork off it (sdlc-flow's worktree-setup branches off HEAD), so a Phase-N block
//   sees the Phase-0..N-1 work its dependencies produced. After a wave, each successful block branch is
//   merged into the train in dependency order; the next wave forks off the advanced train.
//     - default  : train = `<planSlug>-train` (off <base>); each child flow opens its OWN PR (PR per
//                  block); the orchestrator records merge_order for /merge-train; <base> is untouched.
//     - --auto-merge : train = <base>; each block branch is merged straight into <base> in dependency
//                  order as waves complete (no PRs).
//     - --no-pr  : train = `<planSlug>-train`; branches only, no PRs.
//
//   PR-base caveat (default mode): /sdlc-flow PRs target its own prBase (planning/harness.json
//   flow.prBase, default main) — there is no per-PR base override (that would require changing
//   sdlc-flow, out of scope here). A Phase-N block forked off the advanced train therefore opens a
//   "fat" PR whose diff includes its ancestors' work. /merge-train (Phase 1 B) merges the train
//   bottom-up in recorded dependency order, which is the intended review→merge path.
//
// STATE (committed authoritative index — block-orchestration-state.json under planning/<planSlug>/sdlc/)
//   Per-block status + branch + PR + verdict + the child flow's token total, plus a TWO-LEVEL token
//   roll-up: this engine's own orchestration stages + each child /sdlc-flow's tokens.total. Written by
//   a cheap Haiku state-writer after each wave (the committed child commits/PRs remain the authoritative
//   resume signal; state is the at-a-glance index + review artifact).
//
// RESUMPTION
//   Re-run with --resume: the orchestrator re-reads block-orchestration-state.json, skips blocks already
//   'done'/'merged', and continues from the first incomplete wave. Escalated blocks (a child flow bailed
//   or a merge conflicted) poison their dependent subtree for that run; fix the blocker and re-run.
//
//   Validation is downstream only (e.g. bella) — never run an SDLC workflow against base-template.
// =============================================================================

export const meta = {
  name: 'sdlc-block',
  description: 'Orchestrate a master-plan roadmap as a branch train: one /sdlc-flow per independent block, in dependency-ordered waves, each block a reviewable PR.',
  whenToUse: 'When driving a whole roadmap (a master-plan-format file with ## Phase / ### Block sections) to completion across many independent blocks. Optional plan-file path + phase selection. Usage: /sdlc-block [plan-file] [--auto-merge] [--no-pr] [--base <branch>] [--blocks 0-1]',
  phases: [
    { title: 'Pre-flight',  detail: 'Clean tree, locate + commit the plan file, set up the train branch off the base', model: 'sonnet' },
    { title: 'Enumerate',   detail: 'Parse ## Phase / ### Block sections → blocks + dependency graph; compute block-level waves', model: 'sonnet' },
    { title: 'Wave',        detail: 'Per block: ensure tasks.md, fan out one /sdlc-flow; collect verdict, PR, and token roll-up' },
    { title: 'Merge',       detail: 'Advance the train — merge each successful block branch in dependency order (to base when --auto-merge)' },
    { title: 'Report',      detail: 'Write the committed orchestration state + report; surface PRs, merge order, and escalations' },
  ]
}

// ----------------------------------------------------------------
// Parse args: "[planFileOrSlug] [--base b] [--auto-merge] [--no-pr] [--max-parallel-blocks N] [--blocks sel] [--resume]"
// ----------------------------------------------------------------
const rawArgs = typeof args === 'string' ? args.trim() : ''
const tokens = rawArgs ? rawArgs.split(/\s+/) : []

function hasFlag(name) { return tokens.includes(name) }
function flagStr(name) {
  const i = tokens.indexOf(name)
  return (i === -1 || i + 1 >= tokens.length) ? null : tokens[i + 1]
}
function flagInt(name, dflt) {
  const v = parseInt(flagStr(name), 10)
  return isNaN(v) ? dflt : v
}
// Parse a selection like "0-1", "0,2", "1-3,7", or "5" into a sorted int array (used for phases).
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
const baseBranch    = flagStr('--base') || 'main'
const MAX_PARALLEL_BLOCKS = Math.max(1, flagInt('--max-parallel-blocks', 3))

if (autoMergeFlag && noPr) {
  log('ERROR: --auto-merge and --no-pr are mutually exclusive (auto-merge lands work on the base; no-pr produces branches only).')
  return { error: 'Conflicting flags: --auto-merge + --no-pr' }
}

// First positional (not a flag, and not a flag's value) = the plan file or slug. Default master-plan.md.
const flagsTakingValue = new Set(['--base', '--max-parallel-blocks', '--blocks'])
let positional = null
for (let i = 0; i < tokens.length; i++) {
  const tk = tokens[i]
  if (tk.startsWith('--')) { i += flagsTakingValue.has(tk) ? 1 : 0; continue }
  positional = tk
  break
}

// Resolve the plan file: an explicit path (has a slash or ends in .md) is used verbatim; a bare slug
// resolves to planning/<slug>/plan.md; nothing → planning/master-plan.md. The pre-flight agent verifies
// existence and aborts with a clear message if the resolved path is missing.
let planFile
if (!positional) planFile = 'planning/master-plan.md'
else if (positional.includes('/') || positional.endsWith('.md')) planFile = positional
else planFile = `planning/${positional}/plan.md`

// planSlug + state/report locations. A plan named plan.md / master-plan.md takes its CONCEPT from the
// parent dir (plan-<slug>/), or "master-plan" when it sits directly under planning/. Everything the
// orchestrator persists lives under planning/<planSlug>/sdlc/.
function derivePlanSlug(path) {
  const parts = path.split('/').filter(Boolean)
  const base = parts[parts.length - 1].replace(/\.md$/, '')
  const parent = parts.length >= 2 ? parts[parts.length - 2] : ''
  if ((base === 'plan' || base === 'master-plan') && parent && parent !== 'planning') return parent
  if (base === 'master-plan') return 'master-plan'
  return base
}
const planSlug   = derivePlanSlug(planFile)
const stateDir   = `planning/${planSlug}/sdlc`
const stateFile  = `${stateDir}/block-orchestration-state.json`   // committed authoritative index
const reportFile = `${stateDir}/block-orchestration.md`

// Phase selection (--blocks): restrict to these phase numbers. Default: all phases.
const blocksSel = flagStr('--blocks')
let selectedPhases = null
if (blocksSel) {
  const parsed = parseRange(blocksSel)
  if (!parsed || !parsed.length) {
    log(`ERROR: could not parse --blocks "${blocksSel}". Use phase forms like 0, 0-1, or 0,2.`)
    return { error: 'Invalid --blocks selection', blocksSel }
  }
  selectedPhases = new Set(parsed)
}

// Orchestration mode + the branch the orchestrator keeps checked out and merges into.
//   --auto-merge : train IS the base (blocks land on <base> directly, in dependency order).
//   else         : train is a dedicated `<planSlug>-train` branch off <base>; <base> is untouched.
const mode = autoMergeFlag ? 'auto-merge' : (noPr ? 'no-pr' : 'pr')
const trainBranch = autoMergeFlag ? baseBranch : `${planSlug}-train`

log(`Plan: ${planFile} | concept: ${planSlug} | base: ${baseBranch} | mode: ${mode}`)
log(`Train branch: ${trainBranch} | max parallel blocks/wave: ${MAX_PARALLEL_BLOCKS}${resumeMode ? ' | RESUME' : ''}`)
if (selectedPhases) log(`Phase selection: ${[...selectedPhases].sort((a, b) => a - b).join(', ')} (others skipped)`)

// ================================================================
// Schemas
// ================================================================
const PREFLIGHT_SCHEMA = {
  type: 'object',
  required: ['ready', 'action'],
  properties: {
    ready:      { type: 'boolean', description: 'true if the tree is clean, the plan is committed, and the train branch is ready' },
    action:     { type: 'string', enum: ['ready', 'aborted'], description: 'ready = safe to proceed; aborted = a blocker (see reason)' },
    reason:     { type: 'string', description: 'If aborted: why (missing plan file, unrelated dirty files, base checkout failed)' },
    dirtyFiles: { type: 'array', items: { type: 'string' }, description: 'non-plan files blocking the run (when aborted)' },
    trainExisted: { type: 'boolean', description: 'true if the train branch already existed (resume); false if freshly created' }
  }
}

const ENUMERATE_BLOCKS_SCHEMA = {
  type: 'object',
  required: ['blocks'],
  properties: {
    planFormatOk: { type: 'boolean', description: 'true if the file has at least one "## Phase N" with a "### Block X" subsection' },
    blocks: {
      type: 'array',
      description: 'One entry per "### Block X" found under a "## Phase N" heading, in document order.',
      items: {
        type: 'object',
        required: ['phase', 'block', 'slug'],
        properties: {
          phase:     { type: 'integer', description: 'the N from "## Phase N"' },
          block:     { type: 'string', description: 'the X from "### Block X" (single letter, uppercase)' },
          slug:      { type: 'string', description: 'the parseable identifier phaseN-blockX, lowercased (e.g. phase0-blocka)' },
          title:     { type: 'string', description: 'the block title after the em-dash, if any' },
          dependsOn: { type: 'array', items: { type: 'string' }, description: 'slugs of blocks this block depends on, from an explicit "- **Depends on:** ..." line (resolved to phaseN-blockX). Empty when none.' },
          forwardLooking: { type: 'boolean', description: 'true if the block is flagged forward-looking / provisional' }
        }
      }
    },
    notes: { type: 'string' }
  }
}

const GENTASKS_SCHEMA = {
  type: 'object',
  required: ['success'],
  properties: {
    success:    { type: 'boolean', description: 'true if a runnable tasks.md with "### N." headings was written and committed (or already existed)' },
    tasksFile:  { type: 'string' },
    taskCount:  { type: 'integer' },
    commitHash: { type: 'string' },
    notes:      { type: 'string' }
  }
}

const MERGE_SCHEMA = {
  type: 'object',
  required: ['merged', 'escalated'],
  properties: {
    merged:          { type: 'boolean' },
    escalated:       { type: 'boolean', description: 'true if a conflict forced an abort (the block branch is preserved)' },
    conflictedFiles: { type: 'array', items: { type: 'string' } },
    commitHash:      { type: 'string' },
    notes:           { type: 'string' }
  }
}

const REPORT_SCHEMA = {
  type: 'object',
  required: ['reportFile', 'overallVerdict'],
  properties: {
    reportFile:     { type: 'string' },
    overallVerdict: { type: 'string', enum: ['PASS', 'PARTIAL', 'BLOCKED'] },
    notes:          { type: 'string' }
  }
}

const GAP_CHECK_SCHEMA = {
  type: 'object',
  required: ['passed'],
  properties: {
    passed:     { type: 'boolean', description: 'true if all gating checks passed and no blocking coverage gaps remain' },
    fixesMade:  { type: 'boolean', description: 'true if the gap-check wrote or fixed any files' },
    commitHash: { type: 'string',  description: 'short commit hash if fixesMade and changes were committed, else ""' },
    notes:      { type: 'string' }
  }
}

const BLOCK_PR_SCHEMA = {
  type: 'object',
  required: ['created'],
  properties: {
    created: { type: 'boolean', description: 'true if a PR was created (or gh reported one already exists)' },
    url:     { type: 'string',  description: 'the PR URL, or "" if not created' },
    number:  { type: 'integer', description: 'the PR number, or 0 if not created' },
    notes:   { type: 'string' }
  }
}

const STATE_LOAD_SCHEMA = {
  type: 'object',
  required: ['exists'],
  properties: {
    exists:    { type: 'boolean', description: 'true if a valid block-orchestration-state.json was read' },
    startedAt: { type: 'string',  description: "the file's started_at value, or '' when absent" },
    blocks:    { type: 'object',  description: 'the per-block status map (slug -> {status,...}), or {} when absent', additionalProperties: true }
  }
}
const STATE_WRITE_SCHEMA = { type: 'object', required: ['written'], properties: { written: { type: 'boolean' }, commitHash: { type: 'string' } } }

// ================================================================
// TOKEN TELEMETRY — the canonical committed-state token contract (Block A; engines are self-contained,
// so buildTokensBlock is LIFTED byte-identical, not imported). This engine adds a SECOND level: it rolls
// each child /sdlc-flow's tokens.total up into the orchestration state alongside its own stages.
//
// CONTRACT SCOPE (carry-in #1, Phase 0 /code-review). The per-stage `metrics` — and therefore the
// committed `tokens.total` — cover the SUBSTANTIVE orchestration stages only: cheap helper/state-writer
// agents (the Haiku state-writer, config loader) deliberately use bare agent() and are EXCLUDED. This is
// the same boundary every Phase-0 engine already draws; naming it here makes the (bounded, Haiku-cheap)
// exclusion explicit rather than silent, so the two-level child roll-up sums comparable
// substantive-stage totals at both levels. `agent` stays available for the excluded helper calls.
// ================================================================
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
// filesReadKb→tokens at ~256 tok/KB) + a cumulative total. filesReadKb is null here (orchestration
// stages do not self-report it); inTokEst then reduces to promptTokEst.
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

// TWO-LEVEL roll-up (this engine's distinguishing token feature). Combine this engine's own
// substantive-stage block with each child /sdlc-flow's tokens.total into a grand total, so the
// orchestration state reports the whole roadmap's token cost. `childTotals` is [{slug, total}].
function rollupTokens(childTotals) {
  const own = buildTokensBlock()
  const grand = { promptTokEst: own.total.promptTokEst, filesReadKb: own.total.filesReadKb, inTokEst: own.total.inTokEst, outTok: own.total.outTok }
  for (const c of childTotals) {
    if (!c || !c.total) continue
    grand.promptTokEst += c.total.promptTokEst || 0
    grand.filesReadKb  += c.total.filesReadKb || 0
    grand.inTokEst     += c.total.inTokEst || 0
    grand.outTok       += c.total.outTok || 0
  }
  return { stages: own.stages, total: own.total, children: childTotals, grandTotal: grand }
}

// ================================================================
// Pure helpers — waves & failure blast-radius are computed in CODE (reused verbatim at block
// granularity: blocks are keyed by an integer index assigned in (phase, block-letter) order, so this
// numeric topological layering is byte-identical to the legacy task-level version).
// ================================================================

// Topological layering + conflict serialization. Caller supplies the graph; this turns it into ordered
// waves (one per topological layer). Entries in the same wave are mutually independent and share no
// EXCLUSIVE (non-additive) file. For blocks, filesModified is empty and additiveSet is empty, so only
// the dependency edges drive the layering.
function computeWaves(taskMap, additiveSet) {
  const nums = Object.keys(taskMap).map(Number).sort((a, b) => a - b)
  const mustFollow = new Map(nums.map(n => [n, new Set()])) // n must run after every task in this set

  // logical dependency edges
  for (const n of nums) {
    for (const d of (taskMap[n].dependsOn || [])) {
      if (taskMap[d]) mustFollow.get(n).add(d)
    }
  }
  // conflict edges: two tasks editing the same EXCLUSIVE file are serialized (lower number first)
  for (let i = 0; i < nums.length; i++) {
    for (let j = i + 1; j < nums.length; j++) {
      const a = nums[i], b = nums[j]
      const modA = new Set(taskMap[a].filesModified || [])
      const clash = (taskMap[b].filesModified || []).some(f => modA.has(f) && !additiveSet.has(f))
      if (clash) mustFollow.get(b).add(a)
    }
  }

  const remaining = new Set(nums)
  const waves = []
  while (remaining.size) {
    const layer = [...remaining].filter(n => [...mustFollow.get(n)].every(d => !remaining.has(d)))
    if (!layer.length) throw new Error(`Dependency cycle among blocks: ${[...remaining].join(', ')}`)
    layer.sort((a, b) => a - b)
    waves.push({
      label: `Wave ${waves.length + 1}`,
      parallel: layer.length > 1,
      tasks: layer,
      mergeOrder: [...layer].sort((a, b) => a - b)
    })
    layer.forEach(n => remaining.delete(n))
  }
  return waves
}

// A block is poisoned if any block it depends on escalated or was itself poisoned.
function isPoisoned(num, taskMap, badSet) {
  return (taskMap[num]?.dependsOn || []).some(d => badSet.has(d))
}

// ----------------------------------------------------------------
// HARNESS CONFIG — mechanism/policy split (see planning/harness.json). The orchestrator reads only the
// `block.maxParallelBlocks` policy value as a default for the --max-parallel-blocks CLI flag. (The mode is
// driven solely by the --auto-merge / --no-pr flags, which are needed before the config can load — they
// shape pre-flight — so no autoMerge config default is read here.) The engine ships NO stack defaults.
// The runtime has no filesystem access, so a micro-loader agent reads + parses the file. Returns the
// parsed `block` object, or null when absent/invalid. (The block.* schema keys are finalized in Phase 3 C;
// this loader reads maxParallelBlocks provisionally and tolerates its absence.)
// ----------------------------------------------------------------
async function loadBlockConfig() {
  const result = await agent(`
You are the harness-config loader for the SDLC pipeline. Read the project's validation-policy file and
return the orchestration policy. Do not run any checks or modify anything. You run from the MAIN repo root.

STEP 1 — Read the config file:
  cat planning/harness.json 2>/dev/null && echo "__HARNESS_PRESENT__" || echo "__HARNESS_ABSENT__"

STEP 2 — Decide:
  - "__HARNESS_ABSENT__" or not valid JSON → present=false, omit block.
  - Valid JSON → present=true, and copy its top-level "block" object (if any) into "block", keeping ONLY
    the maxParallelBlocks (integer) field when present. Ignore every other field.

Return using StructuredOutput: present, block, notes.
`, { label: 'block-config', schema: { type: 'object', required: ['present'], properties: { present: { type: 'boolean' }, block: { type: 'object', properties: { maxParallelBlocks: { type: 'integer' } } }, notes: { type: 'string' } } }, model: 'sonnet' })
  if (!result || !result.present || !result.block) return null
  return result.block
}

// ================================================================
// COMMITTED AUTHORITATIVE STATE (block-orchestration-state.json)
// ================================================================
const state = {
  plan_slug: planSlug,
  plan_file: planFile,
  base_branch: baseBranch,
  train_branch: trainBranch,
  mode,
  status: 'running',
  blocks: {},        // slug -> { phase, block, title, status, branch, verdict, pr, reasons, tokensTotal }
  waves: [],         // [{ label, parallel, slugs }]
  merge_order: [],   // [slug] — dependency order for /merge-train (default mode)
  tokens: { stages: [], total: { promptTokEst: 0, filesReadKb: 0, inTokEst: 0, outTok: 0 }, children: [], grandTotal: { promptTokEst: 0, filesReadKb: 0, inTokEst: 0, outTok: 0 } },
}

// Refresh the two-level token roll-up from the per-block child totals recorded so far.
function refreshStateTokens() {
  const childTotals = Object.entries(state.blocks)
    .filter(([, b]) => b.tokensTotal)
    .map(([slug, b]) => ({ slug, total: b.tokensTotal }))
  state.tokens = rollupTokens(childTotals)
}

// Persist `state` to the committed block-orchestration-state.json (on the train branch), via a cheap
// Haiku writer since the runtime has no filesystem access or Date.now(). Best-effort: a failed write
// logs a warning and never aborts — the child commits/PRs remain the authoritative resume signal.
async function writeBlockState(label) {
  refreshStateTokens()
  const stateJson = JSON.stringify(state, null, 2)
  const r = await agent(`
You maintain the SDLC orchestrator's committed state breadcrumb. Overwrite ONE JSON file and commit it
on the current branch — do NOT run checks, edit code, or touch anything else. You run from the MAIN repo root.

STEP 1 — current UTC timestamp + preserved start time:
  NOW=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  cat ${stateFile} 2>/dev/null || echo "__NO_STATE__"
  If that file exists and has a "started_at" value, REUSE it verbatim. Otherwise started_at = NOW.

STEP 2 — ensure the dir exists:  mkdir -p ${stateDir}

STEP 3 — write ${stateFile} with EXACTLY this JSON, inserting two extra top-level keys "started_at"
  (preserved or NOW) and "updated_at" (NOW) right after "mode". Valid JSON only (double quotes, no
  trailing commas, no markdown fences). The object to write (verbatim except those two keys):
${stateJson}

STEP 4 — commit on the current branch (stage explicitly):
  git add ${stateFile}
  git commit -m "chore: block orchestration state — ${label}" || echo "NOTHING_TO_COMMIT"
  git log --oneline -1

Use the Write tool for the file. Return via StructuredOutput: written=true on success, commitHash from the
final git log line (empty string if nothing was committed).
`, { label: `state:${label}`, schema: STATE_WRITE_SCHEMA, model: 'haiku' })
  if (!r || !r.written) log(`(state) could not persist orchestration state for "${label}" — continuing`)
}

// ================================================================
// PHASE: PRE-FLIGHT — clean tree, committed plan file, train branch ready
// ================================================================
phase('Pre-flight')
log('Pre-flight: verifying clean tree, committed plan, and the train branch...')

const preflight = await tracedAgent(`
You are the pre-flight agent for a block-level SDLC roadmap orchestration. You run from the MAIN repo
root (CWD = the main checkout). Guarantee: (1) the working tree is clean, (2) the plan file exists and
is committed, and (3) the train branch "${trainBranch}" exists and is checked out — so every child
/sdlc-flow worktree forks off it.

Plan file:    ${planFile}
Base branch:  ${baseBranch}
Train branch: ${trainBranch}   (mode: ${mode})

STEP 1 — Inspect the tree:
  git rev-parse --show-toplevel
  git status --porcelain
  Classify dirty paths: PLAN = the plan file "${planFile}" itself; OTHER = everything else.

STEP 2 — Abort on unrelated dirt:
  If ANY OTHER (non-plan) path is dirty (modified, staged, or untracked), STOP. Return ready=false,
  action="aborted", reason="main working tree has uncommitted changes outside the plan file; commit or
  stash them before running", dirtyFiles=<the OTHER paths>. Do NOT commit them.

STEP 3 — Ensure the plan file exists:
  ls ${planFile} 2>/dev/null && echo "PLAN_PRESENT" || echo "PLAN_MISSING"
  If PLAN_MISSING → STOP. Return ready=false, action="aborted", reason="plan file ${planFile} not found;
  author it with /generate-master-plan (or /plan), commit, then re-run /sdlc-block.". Do NOT generate it here.

STEP 4 — Ensure the base branch is current and the train branch is ready:
  git checkout ${baseBranch}
  If the plan file is dirty, commit it on ${baseBranch}:
    git add ${planFile} && git commit -m "chore: commit plan ${planFile} for orchestration" || echo "nothing to commit"
  Then set up the train branch:
${mode === 'auto-merge'
  ? `  - mode is auto-merge → the train IS the base. Stay on ${baseBranch}. Set trainExisted=true.`
  : `  - Check whether "${trainBranch}" exists:  git branch --list "${trainBranch}"
    - If it exists → git checkout ${trainBranch}  (RESUME: keep its history). Set trainExisted=true.
    - If it does NOT exist → git checkout -b ${trainBranch}  (fork off ${baseBranch}). Set trainExisted=false.`}

STEP 5 — Confirm the final state:
  git rev-parse --abbrev-ref HEAD   (must be "${trainBranch}")
  git status --porcelain            (must be empty)
  If HEAD is not "${trainBranch}" or the tree is not clean, return ready=false, action="aborted",
  reason=<what went wrong>.

Return using StructuredOutput: ready, action, reason, dirtyFiles, trainExisted.
`, { label: 'pre-flight', schema: PREFLIGHT_SCHEMA, phase: 'Pre-flight', model: 'sonnet' })

if (!preflight || !preflight.ready) {
  const why = preflight?.reason || 'pre-flight agent returned null'
  log(`Pre-flight ABORTED — ${why}`)
  if (preflight?.dirtyFiles?.length) {
    log('Uncommitted files outside the plan:')
    for (const f of preflight.dirtyFiles) log(`  - ${f}`)
  }
  return { error: 'Pre-flight failed', reason: why, dirtyFiles: preflight?.dirtyFiles || [], planFile }
}
log(`Pre-flight OK — train branch ${trainBranch} ready (${preflight.trainExisted ? 'existing' : 'created'}).`)

// Read the orchestration policy (block.autoMerge / block.maxParallelBlocks) as CLI defaults.
const blockCfg = await loadBlockConfig()
const effectiveMaxParallel = flagStr('--max-parallel-blocks') ? MAX_PARALLEL_BLOCKS : (blockCfg?.maxParallelBlocks || MAX_PARALLEL_BLOCKS)

// ================================================================
// PHASE: ENUMERATE — parse the plan into blocks + the dependency graph
// ================================================================
phase('Enumerate')
log(`Enumerating blocks from ${planFile}...`)

const enumResult = await tracedAgent(`
You enumerate the BLOCKS of a master-plan-format roadmap so an orchestrator can fan one /sdlc-flow out
per block. You run from the MAIN repo root. Do NOT modify anything.

Plan file: ${planFile}

STEP 1 — Read the plan:
  cat ${planFile}

STEP 2 — Find every block. A block is a "### Block X — <title>" heading (X is a single letter) that
  appears under a "## Phase N — <name>" heading (N is an integer). For EACH block emit:
    - phase: the integer N of its enclosing "## Phase N".
    - block: the single uppercase letter X.
    - slug:  the lowercased identifier "phase<N>-block<x>" (e.g. "phase0-blocka", "phase1-blockb").
    - title: the text after the em-dash on the "### Block X" line (or "").
    - dependsOn: slugs of OTHER blocks this block depends on. Take these ONLY from an explicit
        "- **Depends on:** <ref>" line in the block's body (the master-plan convention). Resolve each
        <ref> to a "phase<N>-block<x>" slug: a bare "Block A" means Block A of THIS SAME phase; a
        "Phase 0 Block B" or "phase0-blockB" means that block. If the block has no "Depends on:" line,
        dependsOn is []. Do NOT invent dependencies from prose — phase ordering is handled by the engine.
    - forwardLooking: true if the block says it is forward-looking / provisional, else false.
  Set planFormatOk=true iff at least one block was found under a "## Phase N" heading.

Return using StructuredOutput: planFormatOk, blocks (in document order), notes.
`, { label: 'enumerate-blocks', schema: ENUMERATE_BLOCKS_SCHEMA, phase: 'Enumerate', model: 'sonnet' })

if (!enumResult || !enumResult.planFormatOk || !(enumResult.blocks || []).length) {
  log(`ABORTED — ${planFile} has no parseable "## Phase N" / "### Block X" structure.`)
  log(`Fix: author it with /generate-master-plan (or /plan), which emits the canonical block headings, commit, then re-run.`)
  return { error: 'Plan not in master-plan format', planFile, notes: enumResult?.notes }
}

// Apply the phase selection, then order blocks by (phase, block-letter) and assign a stable integer
// index so computeWaves can run on numeric keys (reused verbatim).
let blocks = enumResult.blocks
if (selectedPhases) blocks = blocks.filter(b => selectedPhases.has(b.phase))
if (!blocks.length) {
  log(`No blocks match the phase selection ${blocksSel}. Nothing to do.`)
  return { error: 'No blocks selected', planFile, blocksSel }
}
blocks.sort((a, b) => (a.phase - b.phase) || a.block.localeCompare(b.block))

const slugToIndex = {}
const indexToBlock = {}
blocks.forEach((b, i) => { slugToIndex[b.slug] = i + 1; indexToBlock[i + 1] = b })

// Build the block dependency map keyed by integer index. Two edge sources:
//   1. Explicit "Depends on" → resolve the slug to its index.
//   2. Phase-sequential default — every block implicitly depends on all blocks of the immediately
//      preceding (selected) phase, so phases run as ordered waves while same-phase blocks stay parallel
//      (an explicit intra-phase "Depends on" can still sub-split a phase into multiple waves).
const distinctPhases = [...new Set(blocks.map(b => b.phase))].sort((a, b) => a - b)
const prevPhaseOf = {}
distinctPhases.forEach((p, i) => { if (i > 0) prevPhaseOf[p] = distinctPhases[i - 1] })

const blockMap = {}
for (const b of blocks) {
  const idx = slugToIndex[b.slug]
  const deps = new Set()
  for (const depSlug of (b.dependsOn || [])) {
    // slugToIndex keys are the lowercased phaseN-blockX slugs the enumerate agent emits; normalize the
    // dependency ref the same way so a mixed-case "Depends on:" entry (e.g. phase0-blockB) still resolves.
    const depIdx = slugToIndex[String(depSlug).toLowerCase()]
    if (depIdx && depIdx !== idx) deps.add(depIdx)
  }
  if (prevPhaseOf[b.phase] != null) {
    for (const other of blocks) {
      if (other.phase === prevPhaseOf[b.phase]) deps.add(slugToIndex[other.slug])
    }
  }
  blockMap[idx] = { num: idx, slug: b.slug, dependsOn: [...deps], filesModified: [] }
}

let waves
try {
  waves = computeWaves(blockMap, new Set())
} catch (e) {
  log(`Could not layer blocks into waves: ${e.message}`)
  return { error: 'Wave computation failed', detail: e.message, planFile, blockMap }
}

// Seed the per-block state + record the wave plan.
for (const b of blocks) {
  state.blocks[b.slug] = state.blocks[b.slug] || {
    phase: b.phase, block: b.block, title: b.title || '', status: 'pending',
    branch: null, verdict: null, pr: null, reasons: [], tokensTotal: null,
  }
}
state.waves = waves.map(w => ({ label: w.label, parallel: w.parallel, slugs: w.tasks.map(n => indexToBlock[n].slug) }))
for (const w of state.waves) {
  log(`${w.label}: blocks [${w.slugs.join(', ')}]${w.parallel ? ' (parallel)' : ' (sequential)'}`)
}

// ----------------------------------------------------------------
// RESUME — load the prior orchestration state and skip blocks already done.
// ----------------------------------------------------------------
const doneSlugs = new Set()
if (resumeMode) {
  const loaded = await tracedAgent(`
You run from the MAIN repo root. Read the orchestration state breadcrumb if it exists — do NOT modify anything.
  cat ${stateFile} 2>/dev/null || echo "__NO_STATE__"
If "__NO_STATE__", empty, or not valid JSON → exists=false, startedAt="", blocks={}.
Otherwise parse it as JSON and return exists=true, startedAt = its "started_at" (verbatim), blocks = its
"blocks" object verbatim (the per-block status map, keyed by slug).
Return via StructuredOutput: exists, startedAt, blocks.
`, { label: 'load-state', schema: STATE_LOAD_SCHEMA, phase: 'Enumerate', model: 'haiku' })
  if (loaded?.exists && loaded.blocks && typeof loaded.blocks === 'object') {
    for (const [slug, st] of Object.entries(loaded.blocks)) {
      if (state.blocks[slug] && st && (st.status === 'done' || st.status === 'merged')) {
        state.blocks[slug] = { ...state.blocks[slug], ...st }
        doneSlugs.add(slug)
      }
    }
    log(`Resume: ${doneSlugs.size} block(s) already complete — skipping: ${[...doneSlugs].join(', ') || 'none'}`)
  } else {
    log('No orchestration state breadcrumb — treating as a first run.')
  }
}

await writeBlockState('enumerated')

// ----------------------------------------------------------------
// Per-block helpers
// ----------------------------------------------------------------
const badSet = new Set()   // escalated OR poisoned block indices

// Ensure planning/<slug>/tasks.md + tasks.json exist & are committed on the train branch. If
// missing, an inline agent mirrors /generate-tasks (the runtime cannot invoke a slash command) —
// reading the block's section out of the plan file, so this works for master-plan.md AND a /plan
// output path alike.
async function ensureTasks(slug, blk) {
  const blockTasks = `planning/${slug}/tasks.md`
  const blockTasksJson = `planning/${slug}/tasks.json`
  const present = await agent(`
You run from the MAIN repo root. Print whether a runnable spec already exists for this block:
  cat ${blockTasksJson} 2>/dev/null || echo "MISSING"
Return via StructuredOutput: exists=true iff the output is valid JSON and is a non-empty array (a bare array — not wrapped in an object; matches orchestrator's SDLCTask schema), else false.
`, { label: `tasks-check-${slug}`, schema: { type: 'object', required: ['exists'], properties: { exists: { type: 'boolean' } } }, model: 'haiku' })
  if (present?.exists) { log(`Block ${slug}: tasks.json present — reusing.`); return { success: true, tasksFile: blockTasks } }

  log(`Block ${slug}: no tasks.json — generating from the plan's "### Block ${blk.block}" section...`)
  return tracedAgent(`
You are the spec generator for one roadmap block (mirroring /generate-tasks). You run from the MAIN repo
root, on the train branch. Read ONE block's definition out of the plan file and explode it into a
runnable, decomposed tasks.md + tasks.json. Do NOT implement anything.

Block:           ${slug}  (Phase ${blk.phase}, Block ${blk.block}${blk.title ? ' — ' + blk.title : ''})
Plan file:       ${planFile}
Write to:        ${blockTasks} (prose) and ${blockTasksJson} (task list)

1. Read the project's standing rules and the block definition:
   - cat CLAUDE.md planning/context.md   (CLAUDE.md is the authority; assume no stack/locale/narrative/
     content rule unless written there. Universal: no fabricated metrics or quotes, no emoji, every
     change ships with tests.)
   - cat ${planFile}   → find the "## Phase ${blk.phase}" → "### Block ${blk.block}" section. Read ONLY
     that block's What / Why / Files / Interfaces / Out of scope / Acceptance criteria. Carry its named
     Files (New vs Modified) through to per-task ownership and its Out-of-scope as a hard boundary.
   - cat .claude/workflows/templates/spec-template.md   → the FORMAT reference (includes the tasks.json
     schema).

2. Write ${blockTasks} in the standard spec format: ## Goal, ## Context Pointers, ## Step-by-Step Tasks
   (a one-line pointer at tasks.json — the task list is NOT written here), ## Acceptance Criteria
   (observable, true/false against a diff), ## Validation Commands (mirror
   planning/harness.json validation.checks[].command in order; if absent, use the project's documented
   build/test commands), and an empty "## Amendment Log" with "_No amendments yet._". Record any
   deferral under ## Notes.

3. Write ${blockTasksJson} as valid JSON: a BARE ARRAY (not wrapped in an object — matches
   orchestrator's SDLCTask schema, app/schemas/sdlc_schema.py), each entry shaped
   { task_id, title, description, acceptance_criteria, validation_commands, max_attempts, files,
   dependsOn } — 1-indexed task_ids, dependency-ordered, no gaps; each task names the concrete
   file(s) it owns in "files" so tasks are disjoint and merge-safe (final Validate task exempt, and
   its "dependsOn" lists every other task_id); the final task is always titled "Validate".
   acceptance_criteria/validation_commands can stay `[]` per task — the spec-level markdown
   sections are authoritative; max_attempts defaults to 3.

4. Commit on the train branch (stage explicitly):
   git add planning/${slug}
   git commit -m "chore: generate tasks for ${slug}"
   git log --oneline -1   (capture the short hash)

Return via StructuredOutput: success (true iff tasks.md + tasks.json were written and committed),
tasksFile="${blockTasks}", taskCount, commitHash, notes.
`, { label: `generate-tasks-${slug}`, schema: GENTASKS_SCHEMA, phase: 'Enumerate', model: 'opus' })
}

// Run ONE block through /sdlc-flow as the inner engine. Returns the child flow's result (or null).
// Always passes --no-pr so the orchestrator can run the per-block gap-check before opening the PR
// (PR mode) or skip PR creation entirely (no-pr / auto-merge modes where the orchestrator owns merging).
async function runBlockFlow(slug) {
  log(`Block ${slug}: running /sdlc-flow --no-pr...`)
  const r = await workflow('sdlc-flow', `${slug} --no-pr`)
  return r
}

// Run the close-out gap-check (Steps 1-3: validation suite + coverage scan + docs patch) in a
// block's worktree. Non-blocking — if the gap-check fails gating the block is still passed but
// the failure is recorded so the PR body reflects it.
// `baseRef` is the train branch the block forked off; the gate scopes its diffs to the WHOLE block
// (`<baseRef>...HEAD`, the merge-base range), not just the last commit — a block is many commits
// (one per task + state + docs + wrap-up), so a HEAD^ diff would miss everything but the final one.
async function gapCheckBlock(slug, worktreePath, baseRef) {
  if (!worktreePath) {
    log(`Block ${slug}: skipping gap-check (no worktreePath returned by sdlc-flow).`)
    return null
  }
  log(`Block ${slug}: running gap-check in ${worktreePath} (diff base ${baseRef})...`)
  return tracedAgent(`
You are the gap-check agent for block "${slug}". All Bash commands run from the WORKTREE root: ${worktreePath}

Your job: run the close-out gap-check (equivalent to /close-out --gap-check-only) against the block's
changes. Do NOT trigger a handoff. The block branch forked off "${baseRef}", so its full set of changes
is the merge-base range "${baseRef}...HEAD" — scope every diff to that range (NOT HEAD^, which would see
only the last of the block's many commits).

STEP 1 — Validation suite
Read: cd ${worktreePath} && cat planning/harness.json
Run every check in validation.checks[] in order. Then run the emoji gate:
  python3 - <<'PYEOF'
import subprocess, re, sys, os
EMOJI = re.compile(r'[\\U0001F300-\\U0001FAFF\\U00002600-\\U000027BF]')
changed = subprocess.run(['git','diff','${baseRef}...HEAD','--name-only'], capture_output=True, text=True).stdout.splitlines()
md_files = [f for f in changed if f.endswith(('.md','.mdx')) and os.path.isfile(f)]
hits = []
for path in md_files:
    for n, line in enumerate(open(path, errors='ignore'), 1):
        if EMOJI.search(line): hits.append(f'{path}:{n}: {line.rstrip()[:100]}')
if hits:
    print('EMOJI CHECK FAIL:'); [print(h) for h in hits[:25]]; sys.exit(1)
print('EMOJI CHECK: OK'); sys.exit(0)
PYEOF
If any gating check fails → set passed=false and stop; report what failed in notes. Do NOT
attempt to fix failures — that is out of scope for a gap-check.
If all gating checks pass (non-gating surfaced but don't block) → continue.

STEP 2 — Coverage scan
Run: cd ${worktreePath} && git diff ${baseRef}...HEAD --name-only
Filter to source files (exclude *.md, *.json, *.toml, *.yaml, *.yml, planning/, docs/, scaffold/).
If no source files changed → skip STEP 2 silently.
For each changed source file check for test coverage (sibling test files, inline test blocks).
Classify: Adequate / Non-blocking gap / Blocking gap.
Fill blocking gaps: write a minimal targeted test. After writing tests, re-run the gating checks.
Record non-blocking gaps in notes.

STEP 3 — Patch docs
Invoke the /update-docs --patch skill. Wait for completion.

If any files were written or edited in STEPs 2-3, commit them:
  cd ${worktreePath} && git add -A && git commit -m "chore: gap-check fixes for ${slug}" || echo "nothing to commit"
  git log --oneline -1   (capture commitHash)

Return via StructuredOutput: passed, fixesMade, commitHash, notes.
`, { label: `gap-check-${slug}`, schema: GAP_CHECK_SCHEMA, phase: 'Gap-check' })
}

// Open a GitHub PR for a completed block (PR mode only). Called after the per-block gap-check.
async function openBlockPr(slug, worktreePath, branchName, stateFile, blockId, finalVerdict, passedCount, totalCount) {
  if (!worktreePath || !branchName) {
    log(`Block ${slug}: skipping PR open (missing worktreePath or branchName).`)
    return null
  }
  log(`Block ${slug}: opening PR for branch ${branchName}...`)
  return tracedAgent(`
You open a pull request for a completed /sdlc-flow block. All Bash from the WORKTREE root: ${worktreePath}

Branch: ${branchName}   BlockId: ${blockId}   Verdict: ${finalVerdict}   Tasks passed: ${passedCount}/${totalCount}

1. Check gh CLI and remote:
   cd ${worktreePath} && command -v gh >/dev/null 2>&1 && echo "GH_PRESENT" || echo "GH_ABSENT"
   cd ${worktreePath} && git remote -v | head -1 || echo "NO_REMOTE"
   If GH_ABSENT or NO_REMOTE → return created=false, notes="Branch ${branchName} is ready. Push it and open a PR manually."

2. Read the run-state for the PR body:
   cd ${worktreePath} && cat ${stateFile} 2>/dev/null || echo "(no state file)"

3. Push the branch:
   cd ${worktreePath} && git push -u origin ${branchName}

4. Build the PR body from the run-state:
   ## What & why
   [one paragraph from the spec goal + what each task delivered]
   ## Tasks
   [per task: number — status — one-line summary, from state.tasks]
   ## Validation
   [review verdict (${finalVerdict}) and what the consolidated review covered]
   ## Remaining / follow-ups
   [anything deferred, from state + spec Notes]
   ## How it was validated
   [gating checks the end-review ran]
   End the body with this exact footer line (the ONLY place an emoji is allowed):
   🤖 Generated with Claude Code

5. Create the PR:
   cd ${worktreePath} && gh pr create --base ${baseBranch} --head ${branchName} --title "${blockId}: ${passedCount} task(s), review ${finalVerdict}" --body "$(cat <<'EOF'
<the body you built>
EOF
)"
   Run: cd ${worktreePath} && gh pr view --json number,url 2>/dev/null || true
   If gh reports a PR already exists for this branch, treat created=true and capture its url/number.

Return via StructuredOutput: created, url, number, notes.
`, { label: `pr-open-${slug}`, schema: BLOCK_PR_SCHEMA, phase: 'Gap-check' })
}

// ================================================================
// WAVE LOOP
// ================================================================
for (let wi = 0; wi < waves.length; wi++) {
  const wave = waves[wi]
  const waveLabel = `Wave ${wi + 1}`
  phase(waveLabel)

  // Budget guard — each block is a full /sdlc-flow run; stop between waves if the remaining budget
  // can't cover the wave's parallel batch. `budget` may be absent (the engine is run in contexts that
  // don't inject it — same reason tracedAgent guards with typeof), so guard before touching it.
  if (typeof budget !== 'undefined' && budget.total) {
    const estPerBlock = 180_000
    const waveCost = Math.min(wave.tasks.length, effectiveMaxParallel) * estPerBlock
    if (budget.remaining() < waveCost) {
      log(`Budget guard: ~${Math.round(budget.remaining() / 1000)}k remaining < ~${Math.round(waveCost / 1000)}k needed for ${waveLabel}. Stopping — re-run with --resume to continue.`)
      state.status = 'paused-budget'
      break
    }
  }

  // Decide which blocks actually run this wave: honor selection, skip done (resume) and poisoned.
  const runnable = []
  for (const n of wave.tasks) {
    const slug = indexToBlock[n].slug
    if (doneSlugs.has(slug)) { log(`Block ${slug}: already complete (resume) — skipping.`); continue }
    if (isPoisoned(n, blockMap, badSet)) {
      const blockingDeps = (blockMap[n].dependsOn || []).filter(d => badSet.has(d)).map(d => indexToBlock[d].slug)
      log(`Block ${slug}: SKIPPED — depends on an escalated/failed block (${blockingDeps.join(', ')}).`)
      badSet.add(n)
      state.blocks[slug].status = 'skipped'
      state.blocks[slug].reasons = ['blocked by an upstream escalation']
      continue
    }
    runnable.push(n)
  }
  if (!runnable.length) { log(`${waveLabel}: nothing runnable — moving on.`); continue }

  // The orchestrator stays on the train branch (set in pre-flight / advanced after each wave), so the
  // child worktrees fork off it. Ensure each runnable block has a committed tasks.md FIRST (on the
  // train), then fan the flows out.
  for (const n of runnable) {
    const slug = indexToBlock[n].slug
    state.blocks[slug].status = 'generating'
    const gen = await ensureTasks(slug, indexToBlock[n])
    if (!gen || !gen.success) {
      log(`Block ${slug}: could not produce a tasks.md — escalating.`)
      badSet.add(n)
      state.blocks[slug].status = 'escalated'
      state.blocks[slug].reasons = [gen?.notes || 'tasks generation failed']
    }
  }
  const ready = runnable.filter(n => !badSet.has(n))

  // Fan out one /sdlc-flow per ready block, in batches of effectiveMaxParallel.
  for (let k = 0; k < ready.length; k += effectiveMaxParallel) {
    const batch = ready.slice(k, k + effectiveMaxParallel)
    for (const n of batch) state.blocks[indexToBlock[n].slug].status = 'running'
    const batchResults = await parallel(batch.map(n => () => runBlockFlow(indexToBlock[n].slug).then(r => ({ n, r })).catch(() => ({ n, r: null }))))

    for (const item of batchResults) {
      if (!item) continue
      const { n, r } = item
      const slug = indexToBlock[n].slug
      const b = state.blocks[slug]
      if (r) {
        b.branch = r.branch || null
        b.verdict = r.finalVerdict || null
        // pr is null here because runBlockFlow always passes --no-pr; PR mode fills b.pr below.
        // tokensTotal is persisted on the block so the report can derive child totals from state
        // (covers resumed blocks too — see the report's childTokenRecords derivation).
        if (r.tokens?.total) b.tokensTotal = r.tokens.total
      }
      // A clean PASS (not bailed) is mergeable into the train; anything else escalates and poisons deps.
      if (r && !r.bailed && r.finalVerdict === 'PASS') {
        b.status = 'passed'
      } else {
        badSet.add(n)
        b.status = 'escalated'
        b.reasons = [r ? `child flow ${r.finalVerdict || 'no verdict'}${r.bailed ? ` (BAILED: ${r.bailReason || '?'})` : ''}` : 'child flow returned null']
        log(`Block ${slug}: ESCALATED — ${b.reasons[0]}. Branch ${b.branch || '(none)'} preserved.`)
      }
    }

    // Quality gate: run the per-block gap-check for EVERY passed block (all modes — its fixes land on the
    // block branch before that branch merges into the train/base), and additionally open a PR in PR mode.
    // Each passed block is in its own worktree, so process them in parallel. The gap-check diffs the whole
    // block against the train branch it forked off.
    const passedItems = batchResults.filter(Boolean).filter(({ n }) => state.blocks[indexToBlock[n].slug].status === 'passed')
    if (passedItems.length) {
      phase(`Gap-check`)
      const gapAndPrResults = await parallel(passedItems.map(({ n, r }) => async () => {
        const slug = indexToBlock[n].slug
        const gapResult = await gapCheckBlock(slug, r.worktreePath, trainBranch)
        const prResult  = mode === 'pr'
          ? await openBlockPr(slug, r.worktreePath, r.branch, r.stateFile, r.blockId, r.finalVerdict, r.tasksPassed?.length || 0, r.tasksRun?.length || 0)
          : null
        return { n, gapResult, prResult }
      }))
      for (const item of gapAndPrResults) {
        if (!item) continue
        const { n, gapResult, prResult } = item
        const slug = indexToBlock[n].slug
        const b = state.blocks[slug]
        const gapLabel = gapResult?.passed !== false ? 'PASS' : 'FAIL (non-blocking)'
        if (mode === 'pr') {
          if (prResult?.created) {
            b.pr = { url: prResult.url || null, number: prResult.number || null, draft: false }
            log(`Block ${slug}: PR opened ${prResult.url || '#' + (prResult.number || '?')} | gap-check: ${gapLabel}`)
          } else {
            log(`Block ${slug}: PR not created — ${prResult?.notes || 'gh unavailable; open manually'} | gap-check: ${gapLabel}`)
          }
        } else {
          log(`Block ${slug}: gap-check ${gapLabel}`)
        }
      }
    }
  }

  // ---- Advance the train: merge this wave's passed block branches in dependency (index) order ----
  const toMerge = wave.mergeOrder
    .filter(n => state.blocks[indexToBlock[n].slug].status === 'passed')
    .map(n => ({ n, slug: indexToBlock[n].slug, branch: state.blocks[indexToBlock[n].slug].branch }))
    .filter(p => p.branch)

  if (toMerge.length) {
    phase(`Merge ${wi + 1}`)
    for (const p of toMerge) {
      const b = state.blocks[p.slug]
      log(`Merging block ${p.slug} (branch ${p.branch}) into ${trainBranch}...`)
      const m = await tracedAgent(`
You are the merge agent for a block-level roadmap orchestration. You run from the MAIN repo root, which
is checked out on the train branch "${trainBranch}". Merge the block branch "${p.branch}" into it so the
next wave's blocks (which fork off "${trainBranch}") see this block's work.

STEP 1 — Safety: confirm you are on the train branch and the tree is clean:
  git rev-parse --abbrev-ref HEAD      (must be "${trainBranch}")
  git status --porcelain               (must be EMPTY)
  If HEAD is not "${trainBranch}" or the tree is dirty, STOP — return merged=false, escalated=true,
  notes="train branch not clean/checked out; resolve manually then re-run".

STEP 2 — Merge (no fast-forward so the block stays an identifiable unit; do NOT --ff-only):
  git merge --no-ff --no-edit ${p.branch}
  If it exits 0 with no conflicts → merged=true, escalated=false. Go to STEP 4.

STEP 3 — Conflict → abort and escalate (blocks in a wave are independent by the master-plan's Files /
  Out-of-scope contract, so a conflict means that contract was violated and needs a human):
  git diff --name-only --diff-filter=U   (the conflicted files)
  git merge --abort
  Return merged=false, escalated=true, conflictedFiles=<them>, notes="non-additive conflict merging ${p.slug}".

STEP 4 — On success, capture the commit. Do NOT delete the block branch — it backs the block's PR (or the
  branch train) and /merge-train / the human review still need it.
  git log --oneline -1   (commitHash = the short hash)

Return using StructuredOutput: merged, escalated, conflictedFiles, commitHash, notes.
`, { label: `merge-${p.slug}`, schema: MERGE_SCHEMA, phase: `Merge ${wi + 1}`, model: 'sonnet' })

      if (!m || !m.merged) {
        log(`Block ${p.slug}: MERGE ESCALATED — ${m?.notes || 'merge agent returned null'}. Branch preserved.`)
        badSet.add(p.n)
        b.status = 'escalated'
        b.reasons = [...(b.reasons || []), `merge conflict: ${(m?.conflictedFiles || []).join(', ') || m?.notes || 'unknown'}`]
      } else {
        log(`Block ${p.slug}: merged into ${trainBranch} ${m.commitHash || ''}`)
        b.status = mode === 'auto-merge' ? 'merged' : 'done'
        b.mergeCommit = m.commitHash || null
        state.merge_order.push(p.slug)
      }
    }
  }

  await writeBlockState(waveLabel)
  if (typeof budget !== 'undefined' && budget.total) log(`Budget: ~${Math.round(budget.remaining() / 1000)}k tokens remaining.`)
}

// ================================================================
// REPORT — write the committed orchestration report + final state.
// ================================================================
phase('Report')

const completedBlocks = Object.entries(state.blocks).filter(([, b]) => b.status === 'done' || b.status === 'merged')
const escalatedBlocks = Object.entries(state.blocks).filter(([, b]) => b.status === 'escalated')
const skippedBlocks   = Object.entries(state.blocks).filter(([, b]) => b.status === 'skipped')
const allClean = escalatedBlocks.length === 0 && skippedBlocks.length === 0 && state.status !== 'paused-budget'
const overall = allClean ? 'PASS' : (completedBlocks.length > 0 ? 'PARTIAL' : 'BLOCKED')
state.status = allClean ? 'done' : 'blocked'

refreshStateTokens()
const grand = state.tokens.grandTotal

// Child token records for the Level-2 table + summary count — derived from committed state so RESUMED
// blocks (completed in a prior invocation, never appended during this run) are still counted. This is
// the same source refreshStateTokens() rolls up from, so the per-block table sums to the grand total.
const childTokenRecords = Object.entries(state.blocks)
  .filter(([, b]) => b.tokensTotal)
  .map(([slug, b]) => ({ slug, total: b.tokensTotal }))

const blockRows = blocks.map(b => {
  const s = state.blocks[b.slug]
  const pr = s.pr ? (s.pr.number ? `#${s.pr.number}` : (s.pr.url || 'open')) : '—'
  const tok = s.tokensTotal ? (s.tokensTotal.outTok || s.tokensTotal.inTokEst || '—') : '—'
  return `| ${b.slug} | ${s.status} | ${s.verdict || '—'} | ${s.branch || '—'} | ${pr} | ${tok} | ${(s.reasons || []).join('; ').slice(0, 60) || '—'} |`
}).join('\n')

const waveRows = state.waves.map(w => `| ${w.label} | ${w.parallel ? 'parallel' : 'sequential'} | ${w.slugs.join(', ')} |`).join('\n')

const escalationBlock = escalatedBlocks.length
  ? escalatedBlocks.map(([slug, b]) => `- **${slug}** — ${(b.reasons || []).join('; ') || 'see the child flow PR/branch'}${b.branch ? ` (branch \`${b.branch}\` preserved)` : ''}`).join('\n')
  : '_None._'

const mergeOrderLine = state.merge_order.length
  ? `Recorded dependency merge order (for \`/merge-train\`): ${state.merge_order.join(' → ')}`
  : '_No branches merged this run._'

const tokenRows = state.tokens.stages.map(s => `| ${s.label} | ${s.model} | ${s.inTokEst} | ${s.outTok != null ? s.outTok : '—'} |`).join('\n')
const childRows = childTokenRecords.map(c => `| ${c.slug} | ${c.total.inTokEst} | ${c.total.outTok != null ? c.total.outTok : '—'} |`).join('\n') || '| _none_ | — | — |'

const reportResult = await tracedAgent(`
You are the report agent for a block-level roadmap orchestration. You run from the MAIN repo root, on the
train branch. Write the orchestration report and commit it. Do NOT touch planning/status.md or log.md —
each block's own /sdlc-flow wrap-up already updated those on its branch.

DO THIS, IN ORDER:

1. Write the report to ${reportFile} (create parent dirs if needed):

   # Roadmap Orchestration Report — ${planSlug}

   **Date:** [run: date +%Y-%m-%d]
   **Plan:** ${planFile}
   **Base:** ${baseBranch}  ·  **Train:** ${trainBranch}  ·  **Mode:** ${mode}
   **Overall verdict:** ${overall}
   **Blocks complete:** ${completedBlocks.length}  |  **Escalated:** ${escalatedBlocks.length}  |  **Skipped:** ${skippedBlocks.length}

   ## Waves
   | Wave | Order | Blocks |
   |---|---|---|
${waveRows}

   ## Outcome by Block
   (Status: done/merged = landed on the train; passed = built but not merged; escalated/skipped = needs you.
   PR is the child /sdlc-flow's PR; tok is that child flow's token total.)
   | Block | Status | Verdict | Branch | PR | Tokens | Notes |
   |---|---|---|---|---|---|---|
${blockRows}

   ## Merge order
   ${mergeOrderLine}

   ## Escalations (need your attention)
${escalationBlock}

   ## Resume
   Re-run after fixing any blocker:  /sdlc-block ${positional || ''} --resume
   Completed blocks are skipped; escalated/skipped blocks are retried once their blocker is resolved.

2. Append the two-level token roll-up EXACTLY as written (literal heredoc — do not retype or summarize):
   cat >> ${reportFile} <<'ROLLUP_EOF'

## Token Roll-up (two-level)
Level 1 — this orchestrator's own substantive stages (pre-flight / enumerate / generate-tasks / merge /
report). Helper + state-writer agents are excluded by the contract (substantive-stages only). inTok =
injected-input estimate; outTok = output-token delta ("—" without a +Nk budget target).

| Stage | Model | inTokEst | outTok |
|---|---|---|---|
${tokenRows}

Level 2 — each child /sdlc-flow's persisted tokens.total:

| Block | inTokEst | outTok |
|---|---|---|
${childRows}

**Grand total (orchestrator + all child flows):** inTokEst ${grand.inTokEst}${grand.outTok ? ` · outTok ${grand.outTok}` : ''}
ROLLUP_EOF

3. Commit ONLY the report (not status.md or log.md):
   git add ${reportFile}
   git commit -m "chore: roadmap orchestration report for ${planSlug}" || echo "nothing to commit"
   git log --oneline -1

Return using StructuredOutput: reportFile="${reportFile}", overallVerdict="${overall}", notes.
`, { label: 'report', schema: REPORT_SCHEMA, phase: 'Report', model: 'sonnet' })

await writeBlockState('report')

// ----------------------------------------------------------------
// Final close-out (gap-check-only: no handoff) — quality gate over the full train branch.
// ----------------------------------------------------------------
phase('Close-out')
log('Running final close-out gap-check over the train branch...')
await agent(`
You run the final close-out gap-check after a block-level roadmap orchestration. You are on the MAIN
repo root, checked out on the train branch "${trainBranch}".

Invoke the /close-out skill with args "--gap-check-only". Wait for it to complete.

This is the post-orchestration quality gate: it validates the full integrated train branch (not a single
block), fills any remaining coverage gaps, and patches docs — without triggering a handoff.

Report the result in plain text: "Final close-out: <PASS|FAIL> — <one-line summary>".
`, { label: 'final-close-out', phase: 'Close-out', model: 'sonnet' })

// ----------------------------------------------------------------
// Final console summary
// ----------------------------------------------------------------
log('=== ROADMAP ORCHESTRATION COMPLETE ===')
log(`Overall: ${overall} | complete: ${completedBlocks.map(([s]) => s).join(', ') || 'none'} | escalated: ${escalatedBlocks.map(([s]) => s).join(', ') || 'none'} | skipped: ${skippedBlocks.map(([s]) => s).join(', ') || 'none'}`)
log(`Token grand total: inTokEst=${grand.inTokEst}${grand.outTok ? ` | outTok=${grand.outTok}` : ''} (orchestrator + ${childTokenRecords.length} child flow(s))`)
if (state.merge_order.length && mode !== 'auto-merge') log(`Merge order for /merge-train: ${state.merge_order.join(' → ')}`)
if (escalatedBlocks.length) {
  log('Escalations need your analysis:')
  for (const [slug, b] of escalatedBlocks) log(`  ${slug}: ${(b.reasons || []).join('; ')}${b.branch ? ` | branch: ${b.branch}` : ''}`)
  log(`After fixing, resume with: /sdlc-block ${positional || ''} --resume`)
}
log(`Orchestration report: ${reportResult?.reportFile || reportFile}`)

return {
  planFile,
  planSlug,
  base: baseBranch,
  trainBranch,
  mode,
  overallVerdict: overall,
  waves: state.waves,
  blocks: Object.fromEntries(Object.entries(state.blocks).map(([slug, b]) => [slug, { status: b.status, verdict: b.verdict, branch: b.branch, pr: b.pr }])),
  completed: completedBlocks.map(([s]) => s),
  escalated: escalatedBlocks.map(([slug, b]) => ({ slug, branch: b.branch, reasons: b.reasons })),
  skipped: skippedBlocks.map(([s]) => s),
  mergeOrder: state.merge_order,
  tokens: state.tokens,
  reportFile: reportResult?.reportFile || reportFile,
  resumeCommand: `/sdlc-block ${positional || ''} --resume`.trim(),
}
