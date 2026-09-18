// ============================================================================
// SHARED ENGINE LIBRARY — the master copy of every block that is identical in
// BOTH .claude/workflows/sdlc-task.js and .claude/workflows/sdlc-flow.js.
//
// THIS FILE IS NEVER EXECUTED. The Workflow harness snapshots and runs ONE .js
// file per engine (base-template standing rule 10), so the engines must stay
// self-contained -- they cannot `import` this. Instead `scripts/build_engines.py`
// INLINES each block below into the matching `// <<shared:NAME>> ... <</shared:NAME>>`
// region of both engines, in place, and the gated `engines-inlined` check
// re-runs that build and fails if either engine differs by a single byte.
//
// SO: edit a shared block HERE, then run
//     python3 scripts/build_engines.py --write
// and commit the library and both engines together. Editing the inlined copy
// inside an engine directly is pointless -- the next build overwrites it, and
// the gate fails until it does.
//
// A block belongs here ONLY while it is byte-identical in both engines. Where
// the engines genuinely must differ (run-root variable, worklog vs no worklog,
// review vs reconcile), the block stays engine-local and is recorded as an
// INTENDED difference in docs/workflows/prompt-parity.md section 2 -- never
// forced into this file with a flag.
// ============================================================================


// <<shared:GIT>>
const GIT = 'env -u GIT_DIR -u GIT_COMMON_DIR -u GIT_WORK_TREE -u GIT_INDEX_FILE -u GIT_OBJECT_DIRECTORY -u GIT_ALTERNATE_OBJECT_DIRECTORIES -u GIT_NAMESPACE -u GIT_PREFIX -u GIT_CEILING_DIRECTORIES git'
// <</shared:GIT>>

// <<shared:hasFlag>>
function hasFlag(name) { return tokens.includes(name) }
// <</shared:hasFlag>>

// <<shared:flagStr>>
function flagStr(name) {
  const i = tokens.indexOf(name)
  return (i === -1 || i + 1 >= tokens.length) ? null : tokens[i + 1]
}
// <</shared:flagStr>>

// <<shared:withModel>>
function withModel(base, model) {
  return model ? { ...base, model } : base
}
// <</shared:withModel>>

// <<shared:ESCALATION_MODEL>>
const ESCALATION_MODEL = 'opus'
// <</shared:ESCALATION_MODEL>>

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

// <<shared:recordFilesRead>>
function recordFilesRead(result) {
  if (result && result.filesReadKb != null && metrics.length) {
    metrics[metrics.length - 1].filesReadKb = result.filesReadKb
  }
}
// <</shared:recordFilesRead>>

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

// <<shared:PREPARE_RUN_SCHEMA>>
// BT.ticket.prepare-run-replaces-setup-agents, task 6: the schema for the ONE 'prepare-run' agent
// turn that replaces the eight mechanical setup-phase agents (resolve-repo-root, detect-vault,
// verify-setup-binding, render-agent-flag, render-scope-flag, harness-config, enumerate,
// state-load — see the block record's `what`). The agent's ONLY job is to run
// .claude/workflows/bin/prepare_run.py and transcribe its stdout VERBATIM into `rawOutput` — a
// two-turn shell task (the block's own `why`), never a reasoning task. All parsing and every
// decision (refused vs. not, which field means what) happen HERE IN JS, in
// parsePrepareRunOutput() below, exactly like every other mechanical stage in this engine
// (verifySetupBinding, verifyVaultCommit) already does.
const PREPARE_RUN_SCHEMA = {
  type: 'object',
  required: ['rawOutput'],
  properties: {
    rawOutput: { type: 'string', description: 'Everything the prepare-run script printed to stdout, verbatim, unmodified, unsummarized, unreformatted' }
  }
}
// <</shared:PREPARE_RUN_SCHEMA>>

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

// <<shared:parsePrepareRunOutput>>
// Parses runPrepareRun()'s transcribed stdout into { prepareRun, gitCommonDir, tierPrefix,
// brainTomlAtRoot } — never throws; returns null on any parse failure so callers fail exactly the
// way resolveRepoRoot() returning null already did before this ticket. `prepareRun` is
// prepare_run.py's own JSON object verbatim (repo_root, is_vaulted, vault_root, agent_flag,
// scope_flag, harness_config, tasks_enumeration, lint, probes, refused[, reason]) — see that
// script's module docstring for the field list.
function parsePrepareRunOutput(rawOutput) {
  if (!rawOutput || typeof rawOutput !== 'string') return null
  const marker = '---PREPARE_RUN_EXTRA---'
  const idx = rawOutput.indexOf(marker)
  const jsonPart = (idx === -1 ? rawOutput : rawOutput.slice(0, idx)).trim()
  const extraPart = idx === -1 ? '' : rawOutput.slice(idx + marker.length)
  let prepareRun
  try {
    prepareRun = JSON.parse(jsonPart)
  } catch {
    return null
  }
  const gitCommonDirMatch = extraPart.match(/GIT_COMMON_DIR:(.*)/)
  const tierPrefixMatch   = extraPart.match(/TIER_PREFIX:(.*)/)
  const brainTomlMatch    = extraPart.match(/BRAIN_TOML:(.*)/)
  return {
    prepareRun,
    gitCommonDir:    gitCommonDirMatch ? gitCommonDirMatch[1].trim() : null,
    tierPrefix:      tierPrefixMatch ? tierPrefixMatch[1].trim() : '',
    brainTomlAtRoot: brainTomlMatch ? brainTomlMatch[1].trim() === 'yes' : false,
  }
}
// <</shared:parsePrepareRunOutput>>

// <<shared:runPrepareRun>>
// ONE cached agent turn per process, called lazily by whichever of resolveRepoRoot()/
// detectPlanningVault()/renderAgentFlag()/renderScopeFlag()/loadHarnessConfig() runs first — every
// later caller in the SAME run reuses the cached result instead of spawning its own agent, which is
// the actual mechanism of the setup-agent collapse (block AC1/AC2). `_prepareRunCache` can also be
// seeded directly from a resumed run's recorded `state.setup` (see the --resume block in the engine
// body) — in that case this function is never called at all for the rest of that run.
//
// specSlug is optional: resolveRepoRoot() (the very first caller, before any spec-file existence
// check has even happened) calls this with no slug, so the very first prepare-run turn never
// depends on knowing the spec is real yet. A later caller passing a DIFFERENT specSlug than the
// cached one forces a fresh call — this does not happen in either engine's normal flow, since both
// resolve blockId once, before Setup, and never change it mid-run.
let _prepareRunCache = null
let _prepareRunCacheSlug = undefined
async function runPrepareRun(specSlug) {
  if (_prepareRunCache && _prepareRunCacheSlug === (specSlug || null)) return _prepareRunCache
  const specFlag = specSlug ? ` --spec-slug ${specSlug}` : ''
  const result = await agent(`
Run exactly this ONE Bash call, from the invoking directory — do not cd anywhere first, do not
substitute or re-derive any value, and do not run any other command:
  REPO_ROOT=$(${GIT} rev-parse --show-toplevel) && python3 "$REPO_ROOT/.claude/workflows/bin/prepare_run.py"${specFlag} --repo-root "$REPO_ROOT"; echo "---PREPARE_RUN_EXTRA---" && echo "GIT_COMMON_DIR:$(${GIT} rev-parse --path-format=absolute --git-common-dir)" && echo "TIER_PREFIX:$(python3 -c "import os; r=os.path.relpath(os.getcwd(), '$REPO_ROOT'); print('' if r=='.' else r+'/')")" && { [ -f "$REPO_ROOT/brain.toml" ] && echo "BRAIN_TOML:yes" || echo "BRAIN_TOML:no"; }
This is a two-turn shell task, not a reasoning task: prepare_run.py already resolved every setup
fact, ran the tasks.json lint, and probed runnability. Do not interpret, summarize, or reformat its
JSON — transcribe stdout EXACTLY as printed (including the JSON's own newlines and indentation)
into one string.
Return via StructuredOutput: rawOutput (everything printed above, verbatim, in order).
`, { label: 'prepare-run', schema: PREPARE_RUN_SCHEMA, model: 'haiku' })
  _prepareRunCache = parsePrepareRunOutput(result && result.rawOutput)
  _prepareRunCacheSlug = specSlug || null
  return _prepareRunCache
}
// <</shared:runPrepareRun>>

// <<shared:detectPlanningVault>>
// BT.ticket.prepare-run-replaces-setup-agents, task 6: sourced from the shared runPrepareRun()
// cache instead of its own agent turn — prepare_run.py's detect_vault() is the same
// os.path.islink/os.path.realpath check this used to hand a Haiku agent to run and transcribe.
// repoRoot is accepted for call-site compatibility and used only as the fallback root when the
// cache is unusable (agent failure, or a refused run) — no agent call happens in that fallback.
async function detectPlanningVault(repoRoot) {
  const cache = await runPrepareRun()
  const pr = cache && cache.prepareRun
  if (!pr || pr.refused) return { vaulted: false, planningPath: `${repoRoot}/planning` }
  return { vaulted: !!pr.is_vaulted, planningPath: pr.vault_root || `${repoRoot}/planning` }
}
// <</shared:detectPlanningVault>>

// <<shared:resolveRepoRoot>>
// BT.ticket.prepare-run-replaces-setup-agents, task 6: the FIRST caller of runPrepareRun() on a
// fresh (non-resumed) run — Setup calls this before anything else (see the engine body below), so
// this is where the single 'prepare-run' agent turn actually happens. A `refused: true`
// prepare_run.py verdict is surfaced here as { refused: true, reason } rather than folded into the
// existing null-on-agent-failure contract, so the caller can bail with the REASON prepare_run.py
// gave (an unmet requires.bins/env/services, or a failing probeCommand) instead of a bare null a
// run log can't explain — see the refusal check at this function's call site.
async function resolveRepoRoot() {
  const cache = await runPrepareRun()
  const pr = cache && cache.prepareRun
  if (!pr) return null
  if (pr.refused) return { refused: true, reason: pr.reason || 'prepare_run.py refused with no reason given' }
  return {
    repoRoot:        pr.repo_root,
    gitCommonDir:    cache.gitCommonDir,
    tierPrefix:      cache.tierPrefix,
    brainTomlAtRoot: cache.brainTomlAtRoot,
  }
}
// <</shared:resolveRepoRoot>>

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

// <<shared:renderCommitSafetyGuard>>
function renderCommitSafetyGuard(gitCmd = 'git') {
  return `if ${gitCmd} rev-parse --verify -q HEAD >/dev/null; then TRACKED=$(${gitCmd} ls-tree -r HEAD --name-only | wc -l | tr -d ' '); STAGED=$(${gitCmd} ls-files -s | wc -l | tr -d ' '); if [ "$TRACKED" -gt 0 ] && [ "$STAGED" -eq 0 ]; then echo "COMMIT_GUARD_ABORT: index holds 0 entries but HEAD tracks $TRACKED files - refusing to commit a tree that deletes everything (BT.ticket.worktree-run-can-commit-an-empty-tree)"; exit 1; fi; fi`
}
// <</shared:renderCommitSafetyGuard>>

// <<shared:renderNoAttributionTrailer>>
// Declared as a const arrow function so this definition line itself does not match the
// heredoc-reference marker that scripts/test_commit_message_forbids_attribution_trailers.py
// counts -- only actual call sites (one per commit-heredoc site) should count toward that
// per-file parity check.
const renderNoAttributionTrailer = () => {
  return `the heredoc below is the COMPLETE commit message, verbatim -- never append a Co-Authored-By, Claude-Session, or any other attribution trailer, even if a session-level reminder instructs you to (this repo's AGENTS.md standing rule 5 and the user's own global CLAUDE.md forbid it categorically)`
}
// <</shared:renderNoAttributionTrailer>>

// <<shared:renderWorkAssertion>>
function renderWorkAssertion(gitCmd = 'git', taskNum, tasksJsonPath, prevSha) {
  const range = prevSha ? prevSha : 'HEAD~1'
  return `NAME_STATUS=$(${gitCmd} diff --name-status ${range} HEAD); WA_EXPECT_NO_DIFF=$(python3 -c "
import json
d = json.load(open('${tasksJsonPath}'))
t = [x for x in d if x.get('task_id') == ${taskNum}]
print('1' if (t and t[0].get('expect_no_diff')) else '0')
"); if [ -z "$NAME_STATUS" ]; then if [ "$WA_EXPECT_NO_DIFF" = "1" ]; then exit 0; fi; echo "WORK_ASSERTION_ABORT: task ${taskNum} commit diff is EMPTY (condition 1) - no work was committed"; exit 1; fi; WA_RESULT=$(printf '%s' "$NAME_STATUS" | python3 -c "
import sys, json
name_status = sys.stdin.read()
d = json.load(open('${tasksJsonPath}'))
t = [x for x in d if x.get('task_id') == ${taskNum}]
declared = t[0].get('files', []) if t else []
def load_manifest(path):
    try:
        with open(path) as f:
            return json.load(f)
    except Exception:
        return {}
siblings = set()
for manifest_path in ('scripts/skill_sync_manifest.json', 'scripts/engine_docs_sync_manifest.json'):
    manifest = load_manifest(manifest_path)
    for key, entry in manifest.items():
        prefix = key.split('::', 1)[0]
        if prefix in declared and isinstance(entry, dict):
            for field in ('skill_md', 'docs_md'):
                val = entry.get(field)
                if val:
                    siblings.add(val)
def is_allowed(path):
    if path in declared or path in siblings:
        return True
    for entry in declared:
        if entry.endswith('/') and path.startswith(entry):
            return True
        if not entry.endswith('/') and path.startswith(entry + '/'):
            return True
    return False
match = False
bad_del = ''
for line in name_status.splitlines():
    if not line.strip():
        continue
    parts = line.split(chr(9))
    status = parts[0]
    chk = parts[-1]
    if is_allowed(chk):
        match = True
    elif status.startswith('D'):
        bad_del = chk
if not declared:
    match = True
print('MATCH=' + ('1' if match else '0'))
print('BADDEL=' + bad_del)
"); WA_MATCH=$(printf '%s\n' "$WA_RESULT" | sed -n 's/^MATCH=//p'); WA_BADDEL=$(printf '%s\n' "$WA_RESULT" | sed -n 's/^BADDEL=//p'); WA_DECLARED=$(python3 -c "
import json
d = json.load(open('${tasksJsonPath}'))
t = [x for x in d if x.get('task_id') == ${taskNum}]
print(chr(10).join(t[0].get('files', []) if t else []))
"); if [ "$WA_MATCH" != "1" ]; then echo "WORK_ASSERTION_ABORT: task ${taskNum} commit's changed paths do not intersect declared files[] (condition 2) - declared: [$WA_DECLARED] - changed: [$NAME_STATUS]"; exit 1; fi; if [ -n "$WA_BADDEL" ]; then echo "WORK_ASSERTION_ABORT: task ${taskNum} commit deletes undeclared file '$WA_BADDEL' not present in files[] (condition 3) - declared: [$WA_DECLARED]"; exit 1; fi`
}
// <</shared:renderWorkAssertion>>

// Operator-gated acceptance-criterion rule (BT.ticket.engines-must-not-author-unverified-records,
// task 1). Measured 2026-08-21: /sdlc-flow's wrap-up stage authored a COMPLETED sign-off record
// naming the operator for a criterion no operator had actually reviewed ("Brandon (operator, via
// this session)"), because the agent had no way to represent "this AC item cannot be done by me"
// and wrote a plausible completion instead. Every record-authoring stage in both engines must
// carry this rule so an operator-gated criterion renders PENDING rather than fabricated-passed.
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

// <<shared:snapshotBaselines>>
// `baseSha` (BT.ticket.failure-attribution-and-gate-cache, task 5): stamped alongside every NEWLY
// written baseline-diff snapshot as `<path>.sha`, so the fail-closed reconcile-time check
// (renderBaselineDiffCheck below) can tell a baseline taken against THIS run's base commit from a
// stale one taken against some earlier base. An EXISTING baseline is kept as-is (resume-safe) and
// is deliberately NOT backfilled with a sidecar it never had -- a baseline that predates base-SHA
// stamping must surface as its own named failure at reconcile time, not be silently repaired here.
async function snapshotBaselines(cfg, cwd, baseSha) {
  const checks = (cfg?.validation?.checks || [])
    .filter(c => (c.kind === 'baseline-diff' || c.kind === 'skip-count-regression') && c.baselineCommand)
  if (!checks.length) return
  const steps = checks.map(c => {
    const slug = (c.name || 'check').toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '')
    if (c.kind === 'skip-count-regression') {
      const path = `${reportsDir}/${slug}-skip-baseline.txt`
      return `Baseline "${c.name}" -> ${path}:
  cd ${cwd} && mkdir -p ${reportsDir}
  cd ${cwd} && { [ -f ${path} ] && echo "BASELINE EXISTS (kept): ${path}" || { ${c.baselineCommand} > ${path} 2>/dev/null; echo "BASELINE WRITTEN: ${path}"; } ; }`
    }
    const path = `${reportsDir}/${slug}-baseline.json`
    const shaPath = `${path}.sha`
    return `Baseline "${c.name}" -> ${path} (base-SHA sidecar: ${shaPath}):
  cd ${cwd} && mkdir -p ${reportsDir}
  cd ${cwd} && { [ -f ${path} ] && echo "BASELINE EXISTS (kept): ${path}" || { ${c.baselineCommand} > ${path} 2>/dev/null; printf '%s' '${baseSha}' > ${shaPath}; echo "BASELINE WRITTEN: ${path} (base_sha ${baseSha})"; } ; }`
  }).join('\n\n')
  await agent(`
You are the baseline-snapshot agent for the SDLC pipeline. Capture the pre-run baseline for each
baseline-diff / skip-count-regression validation check BEFORE any implementation runs. Run each block
exactly as written. Do NOT modify source. Existing baselines are kept (resume-safe) -- including
their SHA sidecar, if any; never (re)write a sidecar for a baseline you did not just create.

${steps}

Return using StructuredOutput: done=true, and note which baselines were written vs already present.
`, { label: 'baseline-snapshot', schema: { type: 'object', required: ['done'], properties: { done: { type: 'boolean' }, notes: { type: 'string' } } }, model: 'haiku' })
}
// <</shared:snapshotBaselines>>

// <<shared:renderBaselineDiffCheck>>
// BT.ticket.failure-attribution-and-gate-cache, task 5: the baseline-diff check kind, fail-CLOSED,
// for the terminal reconcile / end-review stage (D64 -- by delta, never path-scoped; one check per
// flag, flags never compose). Every failure names its own specific cause; nothing here is silently
// treated as zero items or skipped. `baselinePath`'s sidecar `<baselinePath>.sha` is stamped by
// snapshotBaselines with the run's base_sha at the moment a baseline is first written -- a baseline
// with no sidecar predates that stamping and fails closed rather than being assumed compatible.
function renderBaselineDiffCheck({ header, n, cd, command, currentPath, baselinePath, baseSha, compareKeys }) {
  const shaPath = `${baselinePath}.sha`
  const keysLiteral = JSON.stringify(compareKeys || [])
  return `${header} — baseline-diff (fails CLOSED: net-new items vs the baseline FAIL naming them; a
non-array/empty/unparseable current output, a missing baseline, a base-SHA mismatch, or a baseline
with no .sha sidecar all FAIL naming the specific cause -- D64: by delta, never path-scoped):
  ${cd}${command} > ${currentPath} 2>/dev/null; true
  python3 << 'PYEOF'
import json, sys

def fail(msg):
    print(f'BASELINE-DIFF FAILED: {msg}')
    sys.exit(1)

BASELINE_PATH = '${baselinePath}'
BASELINE_SHA_PATH = '${shaPath}'
RUN_BASE_SHA = '${baseSha}'

try:
    with open(BASELINE_PATH, encoding='utf-8') as f:
        baseline_raw = f.read()
except FileNotFoundError:
    fail(f'missing baseline at {BASELINE_PATH}')
except Exception as e:
    fail(f'could not read baseline at {BASELINE_PATH}: {e}')

try:
    with open(BASELINE_SHA_PATH, encoding='utf-8') as f:
        baseline_sha = f.read().strip()
except FileNotFoundError:
    fail('baseline predates base-SHA stamping')
except Exception as e:
    fail(f'could not read baseline SHA sidecar at {BASELINE_SHA_PATH}: {e}')

if baseline_sha != RUN_BASE_SHA:
    fail(f'base-SHA mismatch: baseline={baseline_sha} run={RUN_BASE_SHA}')

try:
    baseline_items = json.loads(baseline_raw)
except Exception as e:
    fail(f'baseline at {BASELINE_PATH} is not valid JSON: {e}')
if not isinstance(baseline_items, list):
    fail(f'baseline at {BASELINE_PATH} is not a JSON array (got {type(baseline_items).__name__})')

try:
    with open('${currentPath}', encoding='utf-8') as f:
        current_raw = f.read()
except Exception as e:
    fail(f'could not read current output at ${currentPath}: {e}')
if not current_raw.strip():
    fail('current output is empty')
try:
    current_items = json.loads(current_raw)
except Exception as e:
    fail(f'current output is not valid JSON ({e}) -- unparseable output fails closed, never treated as zero items')
if not isinstance(current_items, list):
    fail(f'current output is not a JSON array (got {type(current_items).__name__}) -- non-array output fails closed')

keys = ${keysLiteral}
def k(v): return tuple(str(v.get(x, '')) for x in keys) if isinstance(v, dict) else (str(v),)
seen = set(k(v) for v in baseline_items)
new = [v for v in current_items if k(v) not in seen]
if new:
    print(f'NET-NEW ({len(new)} introduced by this run, absent from baseline):')
    for v in new[:20]: print('  ' + json.dumps(v)[:200])
    sys.exit(1)
print(f'CHECK ${n} PASSED: no net-new items (baseline {len(baseline_items)}, current {len(current_items)}, base_sha {RUN_BASE_SHA})')
sys.exit(0)
PYEOF
  echo "CHECK${n}_EXIT:$?"`
}
// <</shared:renderBaselineDiffCheck>>

// <<shared:expectRedFor>>
function expectRedFor(taskNum) { return taskExpectRedMap.get(taskNum) || new Set() }
if (taskExpectRedMap.size) {
  log(`Per-task expect_red overrides (inverted-verdict, D68): ${[...taskExpectRedMap.keys()].sort((a, b) => a - b).join(', ')} — each named command PASSES on a NON-ZERO exit and FAILS on exit 0; every other check on that task's list is judged normally.`)
}
// <</shared:expectRedFor>>

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

Re-measured vs. carried-over data: a work-assertion, vault-commit, or removed-literal-scan failure
happens BEFORE the test stage runs, so any gate_results/issues data shown to you below (in the state
object, if this bail turns out to be MAJOR) can be CARRIED OVER unchanged from an earlier attempt
that actually ran tests, not a fresh measurement of the current diff — check for an explicit
DATA FRESHNESS WARNING above the state-write instructions. Never write sameFailureAsBefore=true or
describe data as "byte-identical"/"no progress" from carried-over data alone; say explicitly that
the check was not re-run this attempt instead.

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

Before anything else, read .claude/workflows/agent-rules.md (from ${runRoot}) — the engine's own
minimal-context rules for this run. It is short by design; do not skip it because this looks like a
mechanical stage.

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

ALSO populate \`gate_results\` — one entry per check you ran above (same set, same order), each
\`{check_id, status, failing_ids}\`: check_id is the check's own name exactly as it appears in its
"CHECK N — <name>" header (or the harness.json check name, when the checklist is driven by one);
status is \`"pass"\` or \`"fail"\`; failing_ids is an array of the specific ids that failed FOR THAT
CHECK — derive it from the check's own structured runner output where one exists (nextest's JUnit
XML, pytest's \`--junitxml\` or \`-rf\` flag output: use the individual failing test/case ids), and
when the check produces no such structured per-item output, fall back to a single-element array
holding the check's own check_id as the one failing id. A passing check still gets an entry (status
\`"pass"\`, failing_ids \`[]\`).
${heartbeatRecipe || ''}
${onPassRecipe}
Return via StructuredOutput: allPassed (true only if EVERY gating check passed and the emoji gate is
clean), passCount, failCount, failedTests (names), failBlob (compact: failing check names + the tail of
their output; empty when allPassed), gate_results (the per-check array described above)${stateWrittenNote}.`
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
function renderImplementPrompt({ roleIntro, runRootLabel, runRoot, extraReturnFields, isFix, taskNum, attempt, stem, blockId, specFile, specDesc, tasksJsonFile, breakdownFile, prevFailBlob, vault, GIT, renderCommitSafetyGuard, renderWorkAssertion, prevSha }) {
  return `${roleIntro}

Target:
  Spec:        ${blockId}
  Task:        Task ${taskNum} only
  Spec file:   ${specFile} ${specDesc}
  Tasks file:  ${tasksJsonFile} (the task list — find the entry with "task_id": ${taskNum})

1. Read CLAUDE.md and planning/context.md — internalize the project's standing rules (CLAUDE.md is the
   authority; assume no stack/locale/narrative/content rule unless written there). Universal harness
   rules always apply: no fabricated metrics or quotes, no emoji, every change ships with tests.
   Also read .claude/workflows/agent-rules.md — the engine's own minimal-context rules for this run.
   Run: cd ${runRoot} && cat CLAUDE.md .claude/workflows/agent-rules.md

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

3c. READ WITH THE READ TOOL, NOT WITH BASH. Open source files with Read (use offset/limit on a large
   file) and search with Grep/Glob. Do not read or search source through Bash (\`cat\`, \`sed -n\`,
   \`head\`, \`grep\`, \`rg\`): every Bash result stays in context for the rest of this task and is
   re-sent on every later turn. Bash is for running commands, not for reading files.

4. Follow every CLAUDE.md standing rule; add/update tests for new code/logic; verify any model ids /
   package names via the claude-api skill — never from memory.

5. COMPLETENESS SELF-CHECK before committing (D8): no stub/placeholder on any path the task's acceptance
   criteria require (no \`todo!()\`/\`unimplemented!()\`/\`unreachable!()\`, \`raise NotImplementedError\`,
   \`throw new Error('not implemented')\`, empty \`pass\`-only bodies, or \`TODO\`/\`FIXME\` in required
   paths); every deliverable named for Task ${taskNum} exists; any "unit-tested" criterion has a real,
   hermetic test. Sanity-grep ONLY the files the in-scope criteria require:
     cd ${runRoot} && grep -nE 'todo!\\(|unimplemented!\\(|unreachable!\\(|NotImplementedError|not implemented|FIXME' <those paths> 2>/dev/null
   If something required is incomplete, finish it now — do not commit a partial task.

6. Confirm correctness with the NARROWEST commands that exercise this task's own change: the tests
   for the files and modules Task ${taskNum} touched (one test file, one module, or one test-name
   filter), plus the build/typecheck those files need. Do NOT run the project's whole test suite or
   the full gating set here. The test stage runs every gating check right after you commit, so a
   full run here pays for the same suite twice on every attempt.

7. Commit on the branch. Never use git add -A or git add . — stage files explicitly by name.
   Run: cd ${runRoot} && ${GIT} status
   Stage your changed source/test files explicitly, then commit using HEREDOC — ${renderNoAttributionTrailer()}:
     cd ${runRoot} && ${renderCommitSafetyGuard()} && ${GIT} commit -m "$(cat <<'EOF'
${isFix ? `fix: fix pass ${attempt - 1} for ${stem}` : `feat: implement ${stem}`}
EOF
)"
   Run: cd ${runRoot} && ${GIT} log --oneline -1   (capture the short hash)

7a. Post-commit work assertion (D81 lift condition 2) — prove this commit actually contains Task
   ${taskNum}'s declared work, not the absence of it. The check's range runs from this task's own
   start point (the previous task's recorded commit, or the run's base_sha for task 1 — never a
   literal "one commit back") through HEAD, so a wrap-up or resume commit landing on top does not
   change the verdict. It PASSES when the changed paths intersect declared files[] (a directory
   entry in files[] matches as a prefix), OR when the diff lands only in a declared file's
   sync-manifest sibling (a SKILL.md replication guide or docs/workflows/*.md page), OR when this
   task's tasks.json entry declares \`expect_no_diff: true\` and the diff is genuinely empty (the
   task's correct outcome IS no diff — say so via that field rather than leaving an empty diff to
   be read as work-not-done):
   Run: cd ${runRoot} && ${renderWorkAssertion('git', taskNum, tasksJsonFile, prevSha)}
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

// <<shared:RENDER_IDENTITY_SCHEMA>>
const RENDER_IDENTITY_SCHEMA = {
  type: 'object',
  required: ['value'],
  properties: {
    value: { type: 'string', description: 'the text after "VALUE:" on the probe script\'s stdout, or "" if that line is missing or the script produced no output' }
  }
}
// <</shared:RENDER_IDENTITY_SCHEMA>>

// <<shared:renderAgentFlag>>
// Renders the `--agent <id>` argument for a `mev emit-state --write` / `mev set-block-status
// --write` invocation so a lane that holds its own exclusive lease is exempt from mev's
// `refuse_if_quiesced` (BT.ticket.engines-must-pass-agent-to-mev). Returns '' (empty string) when
// no identity resolves — an unconditional flag would change every non-lane, standalone-repo run
// of these engines across 18+ downstream repos with no brain.toml at all.
//
// BT.ticket.prepare-run-replaces-setup-agents, task 6: sourced from the shared runPrepareRun()
// cache instead of its own agent turn — prepare_run.py's render_agent_flag() is a line-for-line
// port of the same FLEET_LANE_AGENT / lease-file resolution this used to hand a Haiku agent to run
// and transcribe (MEASURED 2026-09-07, BT.ticket.engine-helpers-call-require-which-the-workflow-
// runtime-does-not-define — this JS function itself never touches `process`; the whole resolution
// still happens inside prepare_run.py's python, never in this function's own reasoning).
async function renderAgentFlag() {
  const cache = await runPrepareRun()
  const pr = cache && cache.prepareRun
  return (pr && !pr.refused && pr.agent_flag) || ''
}
// <</shared:renderAgentFlag>>

// <<shared:renderScopeFlag>>
// Renders the `--scope <slug>` argument for a `mev emit-state --write` invocation so an
// in-place lane's wrap-up/bookkeep regenerates only its OWN repo's derived surfaces instead of
// the whole corpus (BT.ticket.engines-pass-scope-to-emit-state). Returns '' (empty string) when
// no repo slug resolves -- an unconditional flag would break every non-lane, standalone-repo run
// of these engines across 18+ downstream repos with no brain.toml at all.
//
// Same replacement as renderAgentFlag() immediately above — prepare_run.py's render_scope_flag()
// is a line-for-line port of the same brain.toml [[repos]] walk-up this used to hand a Haiku agent
// to run and transcribe.
async function renderScopeFlag() {
  const cache = await runPrepareRun()
  const pr = cache && cache.prepareRun
  return (pr && !pr.refused && pr.scope_flag) || ''
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

// <<shared:buildTaskGateHistory>>
// BT.ticket.failure-attribution-and-gate-cache, task 3: a PURE helper -- no agent call, no I/O --
// that reads this run's own `state.tasks` (already in memory; every task's gate_results is folded
// onto it right after its own test stage, see the `t.gate_results = ...` fold at each engine's
// per-task test-failure call site) into the ordered history decideAttribution() needs: one entry
// per EARLIER task (task_id < beforeTaskNum) that actually recorded a gate_results array, oldest
// first. A task with no recorded gate_results (never reached its test stage this run, e.g. a
// resumed run's still-pending task) is simply absent from the result -- not a zero-length entry --
// so decideAttribution's lookback naturally skips it.
function buildTaskGateHistory(stateTasks, beforeTaskNum) {
  return Object.keys(stateTasks || {})
    .filter(k => k !== '__pendingBails')
    .map(Number)
    .filter(n => Number.isFinite(n) && n < beforeTaskNum)
    .sort((a, b) => a - b)
    .map(n => ({ taskId: n, gateResults: (stateTasks[String(n)] || {}).gate_results || [] }))
    .filter(entry => entry.gateResults.length > 0)
}
// <</shared:buildTaskGateHistory>>

// <<shared:decideAttribution>>
// BT.ticket.failure-attribution-and-gate-cache, task 3: the attribution decision itself -- PURE,
// deterministic, and callable over already-recorded inputs with NO agent call and NO engine launch
// (task 6 replays fixtures through this exact function). Verdict vocabulary is
// .claude/workflows/sdlc-state-vocab.json's `ownership` (self|foreign) and `failure_class`
// (fixable|escalate) -- never a third spelling.
//
// Decision order (the block record's own `what`, reproduced here so the two live side by side):
//   1. Look back through THIS RUN's own recorded gate_results (`taskGateHistory`, most-recent-first)
//      for an earlier task that already ran this same check_id:
//        - that task recorded it PASS  -> the breakage was introduced strictly after that task's
//          commit -> ownership=self, failure_class=fixable (today's ordinary fix loop, UNCHANGED --
//          this is the regression control, including the plain "green at N-1, red at N" case).
//        - that task recorded it FAIL  -> it was ALREADY red at that earlier task and never fixed
//          -> in-spec debt: failure_class=fixable, ownership=self (declared by that earlier task,
//          not introduced by the current one), writable set widened to the failing artifact plus
//          the current task's own files[]. Carries forward (failure_class=escalate) if it reaches
//          the task that declared it, or terminal reconcile, still unresolved -- that escalation is
//          the CALLER's responsibility (it owns "which task is that" and "have we reached it"); this
//          function only reports the debt and who declared it.
//   2. No record in this run's own history at all (first time this check has been evaluated this
//      run) -> consult the gate cache at base_sha (`cacheStatus`, resolved by the caller via
//      attributionLookback() below -- a cache MISS re-runs ONLY this check id, never the suite):
//        - red at base_sha  -> ownership=foreign, carried and recorded, NO attempt burned, no bail.
//        - green at base_sha -> in-spec debt, same shape as above, but declaredByTask is unknown
//          (the cache has no per-task resolution) -- the caller decides where it carries forward to.
//   3. Nothing decides (no history record AND cacheStatus is null/unknown, e.g. the cache could not
//      be consulted) -> returns null. The caller's existing, unchanged triage flow is the correct
//      fallback for a null verdict -- this function never guesses.
//
// `currentTaskFiles` is caller-supplied (the current task's own tasks.json files[]) purely so the
// returned writableSet is complete without a second call; this function does no file-system I/O of
// its own to obtain it.
function decideAttribution({ checkId, taskGateHistory, cacheStatus = null, currentTaskFiles = [] }) {
  const history = [...(taskGateHistory || [])].sort((a, b) => b.taskId - a.taskId)
  for (const entry of history) {
    const found = (entry.gateResults || []).find(g => g && g.check_id === checkId)
    if (!found) continue
    if (found.status === 'pass') {
      return {
        ownership: 'self',
        failure_class: 'fixable',
        inSpecDebt: false,
        declaredByTask: null,
        introducedAfterTask: entry.taskId,
        writableSet: [...currentTaskFiles],
        reason: `check ${checkId} was green at task ${entry.taskId}'s own recorded gate_results -- introduced after that, ordinary fix loop (regression control).`,
      }
    }
    const failingIds = Array.isArray(found.failing_ids) ? found.failing_ids : []
    return {
      ownership: 'self',
      failure_class: 'fixable',
      inSpecDebt: true,
      declaredByTask: entry.taskId,
      introducedAfterTask: null,
      writableSet: [...new Set([...failingIds, ...currentTaskFiles])],
      reason: `check ${checkId} was already red at task ${entry.taskId} in this run's own gate_results (never fixed) -- in-spec debt declared by task ${entry.taskId}, carries forward.`,
    }
  }
  if (cacheStatus === 'fail') {
    return {
      ownership: 'foreign',
      failure_class: null,
      inSpecDebt: false,
      declaredByTask: null,
      introducedAfterTask: null,
      writableSet: [],
      reason: `check ${checkId} was already red at base_sha per the gate cache -- carried and recorded, no attempt burned, no bail.`,
    }
  }
  if (cacheStatus === 'pass') {
    return {
      ownership: 'self',
      failure_class: 'fixable',
      inSpecDebt: true,
      declaredByTask: null,
      introducedAfterTask: null,
      writableSet: [...currentTaskFiles],
      reason: `check ${checkId} was green at base_sha per the gate cache but has no recorded pass in this run's own history -- introduced by an earlier task in this spec, never caught until now.`,
    }
  }
  return null
}
// <</shared:decideAttribution>>

// <<shared:renderAttributionCacheLookup>>
// The read-only gate-cache consult for decideAttribution()'s step 2 -- an agent turn because the
// engine script itself has no filesystem/subprocess access (base-template CLAUDE.md's "stamp-
// workflow-run-id" note: the same reason runPrepareRun() exists). A cache MISS re-runs ONLY this
// one check id, and NEVER on this run's own branch/tree -- an isolated, throwaway `git worktree add
// --detach` at base_sha, removed again immediately, so a concurrent sibling session sharing this
// same working tree is never touched (this repo's own commit-in-this-fleet discipline).
function renderAttributionCacheLookup({ runRoot, checkId, baseSha, repoSlug, checkCommand, GIT }) {
  const repoArg = repoSlug || 'unknown-repo'
  const checkIdJson = JSON.stringify(checkId)
  return `You are the attribution-cache-lookup agent (BT.ticket.failure-attribution-and-gate-cache,
task 3). Determine whether check ${checkIdJson} was ALREADY red at base_sha ${baseSha}, using the
lazy cross-lane gate cache -- read-only by default, never touching this run's own branch or tree.

Run exactly this ONE Bash call from ${runRoot}:
  cd ${runRoot} && python3 .claude/workflows/bin/gate_cache.py lookup --repo ${repoArg} --base-sha ${baseSha} --check-ids ${checkIdJson}

Parse its JSON stdout. If ${checkIdJson} appears under "hits", that recorded status ("pass"/"fail")
IS the answer -- report it as cacheStatus directly, do NOT re-run anything.

${checkCommand ? `If ${checkIdJson} appears under "misses" instead (a cache miss), re-run ONLY this
one check id at base_sha -- never the whole suite, and NEVER against this run's own branch/tree. Use
an isolated git worktree (throwaway, removed again immediately) so nothing here touches the shared branch:
  cd ${runRoot} && WT=$(mktemp -d) && ${GIT} worktree add --detach "$WT" ${baseSha} >/dev/null 2>&1 && (cd "$WT" && ${checkCommand}); CHECK_EXIT=$?; ${GIT} worktree remove --force "$WT" >/dev/null 2>&1
CHECK_EXIT 0 means the check PASSED at base_sha; non-zero means it FAILED. Then warm the cache with
exactly what you found (a manual verb, never scheduled -- out_of_scope):
  cd ${runRoot} && python3 .claude/workflows/bin/gate_cache.py warm --repo ${repoArg} --base-sha ${baseSha} --result "${checkId}=<pass or fail, from CHECK_EXIT>"
Report the status you just determined as cacheStatus.` : `If ${checkIdJson} appears under "misses"
instead, this run has no known command to safely re-run it against base_sha (it is not one of
planning/harness.json's named checks) -- report cacheStatus "unknown" rather than guessing.`}

Return via StructuredOutput: cacheStatus ("pass" | "fail" | "unknown" -- "unknown" only when neither
a cache hit nor a safe re-run was possible), notes (what you actually observed, quoting the cache
lookup's JSON or the re-run's exit code).`
}
// <</shared:renderAttributionCacheLookup>>

// <<shared:attributionLookback>>
// The full orchestration for one failing check_id: PURE history lookback first (no agent call --
// decideAttribution() alone answers it whenever this run's own gate_results already saw this check
// at an earlier task), falling back to ONE cheap gate-cache-lookup agent turn only when this run's
// history has nothing to say. Returns decideAttribution()'s verdict object, or null when nothing
// decides (the caller's existing, unchanged triage flow is the correct fallback for null).
//
// The schema below was a separate top-level ATTRIBUTION_CACHE_SCHEMA const until 2026-09-17: this
// function is called from the main per-task loop (on a failing check), which is textually BEFORE
// where that const was declared in the generated engines -- the same top-level-await TDZ hazard as
// REMOVED_LITERAL_SCAN_CONFIG/_SCHEMA (see that fix's comment on removedLiteralScan() below), just
// not yet triggered by a real run when it was found (no gate check had failed requiring lookback
// yet). Found by sweeping every top-level const declared after the main loop's own `for` statement
// while fixing the REMOVED_LITERAL_SCAN crash. Inlined for the same reason: removes the late
// top-level const entirely rather than trying to reorder it correctly.
async function attributionLookback({ checkId, state, taskNum, runRoot, baseSha, harnessCfg, GIT, currentTaskFiles = [] }) {
  const history = buildTaskGateHistory(state.tasks, taskNum)
  const fromHistory = decideAttribution({ checkId, taskGateHistory: history, cacheStatus: null, currentTaskFiles })
  if (fromHistory) return fromHistory
  if (!baseSha) return null
  const scopeFlagRaw = await renderScopeFlag()
  const scopeMatch = scopeFlagRaw.match(/--scope\s+(\S+)/)
  const repoSlug = scopeMatch ? scopeMatch[1] : null
  const checkCfg = (harnessCfg?.validation?.checks || []).find(c => c.name === checkId)
  const checkCommand = checkCfg ? (checkCfg.command || null) : null
  const attributionCacheSchema = {
    type: 'object',
    required: ['cacheStatus'],
    properties: {
      cacheStatus: { type: 'string', enum: ['pass', 'fail', 'unknown'], description: 'the check\'s status at base_sha per the gate cache -- "pass" or "fail" from a cache hit or a safe re-run, "unknown" only when neither was possible' },
      notes: { type: 'string' }
    }
  }
  const result = await tracedAgent(`
${renderAttributionCacheLookup({ runRoot, checkId, baseSha, repoSlug, checkCommand, GIT })}
`, { label: `attribution-cache:${checkId}`, schema: attributionCacheSchema, model: 'haiku' })
  if (!result || result.cacheStatus === 'unknown') return null
  return decideAttribution({ checkId, taskGateHistory: history, cacheStatus: result.cacheStatus, currentTaskFiles })
}
// <</shared:attributionLookback>>

// <<shared:parseRemovedLiteralScanOutput>>
// Parses runRemovedLiteralScan()'s transcribed stdout -- never throws; a malformed/empty
// transcription is reported as instrumentOk=false rather than silently read as "no hits" (the
// task's own positive-control requirement: an empty result must be distinguishable from a broken
// instrument, never asserted by an empty grep alone).
function parseRemovedLiteralScanOutput(rawOutput) {
  if (!rawOutput || typeof rawOutput !== 'string') return { hits: [], instrumentOk: false, note: 'no output transcribed' }
  const lines = rawOutput.split('\n').map(l => l.trim()).filter(Boolean)
  if (lines.some(l => l.startsWith('INSTRUMENT_BROKEN:'))) {
    const broken = lines.find(l => l.startsWith('INSTRUMENT_BROKEN:'))
    return { hits: [], instrumentOk: false, note: broken.slice('INSTRUMENT_BROKEN:'.length) }
  }
  if (!lines.some(l => l.startsWith('CANDIDATE_TEST_COUNT:'))) {
    return { hits: [], instrumentOk: false, note: 'scan script never reported CANDIDATE_TEST_COUNT -- transcription incomplete or script did not run' }
  }
  const hits = []
  for (const line of lines) {
    if (!line.startsWith('HIT:')) continue
    const rest = line.slice('HIT:'.length)
    const parts = rest.split('|')
    if (parts.length < 2) continue
    const [literal, file, lineNo] = parts
    hits.push({ literal, file, line: lineNo ? Number(lineNo) || null : null })
  }
  return { hits, instrumentOk: true, note: lines.find(l => l.startsWith('CANDIDATE_TEST_COUNT:')) || '' }
}
// <</shared:parseRemovedLiteralScanOutput>>

// <<shared:renderRemovedLiteralScanScript>>
// A mechanical, single Python invocation -- the agent's only job is to run it and transcribe stdout
// (same "script decides, agent transcribes" discipline as runPrepareRun()/verifyVaultCommit()), so
// no per-literal or per-file judgment is delegated to the model. `range` is the SAME prevSha-or-
// HEAD~1 commit-range boundary renderWorkAssertion() already uses for this task, so the scan and the
// work assertion agree on exactly what this task's own commit contains.
function renderRemovedLiteralScanScript({ range, tasksJsonPath, taskNum, testGlobRegex, minLiteralLen, identifierMinLen }) {
  return `cat > "$RLS_TMP" <<'PYEOF'
import json, re, subprocess, sys

def sh(cmd):
    return subprocess.run(cmd, shell=True, capture_output=True, text=True).stdout

TASK_NUM = ${JSON.stringify(taskNum)}
TASKS_PATH = ${JSON.stringify(tasksJsonPath)}
RANGE = ${JSON.stringify(range)}
TEST_GLOB_REGEX = ${JSON.stringify(testGlobRegex)}
MIN_LEN = ${JSON.stringify(minLiteralLen)}
IDENT_MIN_LEN = ${JSON.stringify(identifierMinLen)}

try:
    data = json.load(open(TASKS_PATH))
except (OSError, ValueError):
    data = []
matches = [x for x in data if isinstance(x, dict) and x.get('task_id') == TASK_NUM]
task_files = matches[0].get('files', []) if matches else []

def is_own(path):
    for tf in task_files:
        tf = tf.rstrip('/')
        if path == tf or path.startswith(tf + '/'):
            return True
    return False

test_re = re.compile(TEST_GLOB_REGEX)
candidates = [l for l in sh('${GIT} ls-files').splitlines() if test_re.search(l)]
if not candidates:
    print('INSTRUMENT_BROKEN:no candidate test files matched TEST_GLOB_REGEX=%r under this repo' % TEST_GLOB_REGEX)
    sys.exit(0)
print('CANDIDATE_TEST_COUNT:%d' % len(candidates))

diff = sh('${GIT} diff --unified=0 %s HEAD -- .' % RANGE)
removed_lines = [l[1:] for l in diff.splitlines() if l.startswith('-') and not l.startswith('---')]
# Quoted-string literals gate on MIN_LEN. Bare identifiers gate on EITHER containing an underscore
# (a real snake_case/CONST_CASE symbol, reported at any length) OR being at least IDENT_MIN_LEN chars
# with no underscore -- this is what keeps an ordinary removed English word ("failed", "returned")
# out of the result: those are short and underscore-free, unlike a real removed symbol name.
lit_re = re.compile(
    r'"([^"]{%d,})"' % MIN_LEN
    + r"|'([^']{%d,})'" % MIN_LEN
    + r'|\\b([A-Za-z_][A-Za-z0-9]*_[A-Za-z0-9_]*)\\b'
    + r'|\\b([A-Za-z][A-Za-z0-9]{%d,})\\b' % (IDENT_MIN_LEN - 1)
)
literals = set()
for line in removed_lines:
    for m in lit_re.finditer(line):
        lit = m.group(1) or m.group(2) or m.group(3) or m.group(4)
        if lit:
            literals.add(lit)

if not literals:
    print('NO_LITERALS_REMOVED')
    sys.exit(0)

hits = []
for lit in sorted(literals):
    for f in candidates:
        if is_own(f):
            continue
        try:
            with open(f, encoding='utf-8', errors='ignore') as fh:
                for i, line in enumerate(fh, 1):
                    if lit in line:
                        hits.append((lit, f, i))
                        break
        except OSError:
            continue

if not hits:
    print('NO_HITS')
else:
    for lit, f, i in hits:
        print('HIT:%s|%s|%d' % (lit, f, i))
PYEOF
python3 "$RLS_TMP"; RLS_EXIT=$?; rm -f "$RLS_TMP"; exit $RLS_EXIT`
}
// <</shared:renderRemovedLiteralScanScript>>

// <<shared:renderRemovedLiteralScan>>
function renderRemovedLiteralScan({ runRoot, taskNum, tasksJsonPath, range, testGlobRegex, minLiteralLen, identifierMinLen }) {
  const script = renderRemovedLiteralScanScript({ range, tasksJsonPath, taskNum, testGlobRegex, minLiteralLen, identifierMinLen })
  return `You are the removed-literal-scan agent (BT.ticket.failure-attribution-and-gate-cache, task
4). Task ${taskNum}'s own commit(s) (range ${range}..HEAD) may have removed a string literal or
identifier that a test OUTSIDE this task's own files[] still references -- a silent breakage the
fast test tripwire does not otherwise catch. Do NOT reason about which literals matter yourself; the
script below already decided it. Run exactly this ONE Bash call from ${runRoot}, verbatim:

  cd ${runRoot} && RLS_TMP=$(mktemp) && ${script}

Transcribe every line it prints to stdout, in order, exactly as printed -- do not summarize,
reformat, or drop any line (including INSTRUMENT_BROKEN:, CANDIDATE_TEST_COUNT:, NO_LITERALS_REMOVED,
NO_HITS, or any HIT: line).

Return via StructuredOutput: rawOutput (everything printed above, verbatim, in order).`
}
// <</shared:renderRemovedLiteralScan>>

// <<shared:removedLiteralScan>>
// Orchestrates one post-commit removed-literal scan for the current task: resolves the config knobs
// (project override via harnessCfg.removedLiteralScan, else the built-in defaults below), renders
// and runs the mechanical script above, and returns { hits, instrumentOk, note }. A hit inside the
// task's own files[] is filtered out BY THE SCRIPT ITSELF (never reported here at all) -- see
// renderRemovedLiteralScanScript's is_own() check. instrumentOk=false means the scan could not
// positively confirm it ran (no candidate test files, or an incomplete transcription) -- callers
// must treat that as "scan inconclusive", never as "no hits found".
//
// BT.ticket.failure-attribution-and-gate-cache task 4 shipped these three defaults as a shared
// top-level REMOVED_LITERAL_SCAN_CONFIG const declared ~150 lines above this function's own
// definition (standing rule 12: all three are config knobs, never literals baked into the scan
// script itself; a project overrides any of them via planning/harness.json's optional
// `removedLiteralScan: { testGlobRegex, minLiteralLen, identifierMinLen }` object). That const,
// positioned very late in the ~4600-line generated engine files, hit a
// `ReferenceError: Cannot access '...' before initialization` when invoked through the Workflow
// tool's execution model (reproduced deterministically 2026-09-17 on a real engine-rs
// `/sdlc-task` run) despite being lexically declared before its only use -- some interaction with
// how that tool wraps/evaluates a script this large. Inlining the defaults removes the cross-
// reference to a late top-level `const` entirely; the values and the override contract are
// unchanged.
async function removedLiteralScan({ runRoot, taskNum, tasksJsonPath, prevSha, harnessCfg }) {
  const cfg = harnessCfg?.removedLiteralScan || {}
  // Matches this fleet's own test-naming conventions plus the common cross-language ones, so the
  // harness ships one sane default without hardcoding a single project's directory layout.
  const defaultTestGlobRegex = '(^|/)test_[^/]+\\.py$|(^|/)[^/]+_test\\.py$|(^|/)tests?/.*|\\.test\\.[jt]sx?$|\\.spec\\.[jt]sx?$'
  const defaultMinLiteralLen = 8
  // identifierMinLen exists SEPARATELY from minLiteralLen (quoted strings) because a bare-
  // identifier match at the same low threshold is noisy -- ordinary English words removed from a
  // comment or log message (e.g. "failed", "returned") are common at 6-8 chars and are not the
  // distinctive symbol names this scan exists to catch; an underscored identifier of any length
  // (a real snake_case/CONST_CASE symbol) is always reported regardless of identifierMinLen.
  const defaultIdentifierMinLen = 12
  const testGlobRegex = typeof cfg.testGlobRegex === 'string' && cfg.testGlobRegex ? cfg.testGlobRegex : defaultTestGlobRegex
  const minLiteralLen = Number.isInteger(cfg.minLiteralLen) && cfg.minLiteralLen > 0 ? cfg.minLiteralLen : defaultMinLiteralLen
  const identifierMinLen = Number.isInteger(cfg.identifierMinLen) && cfg.identifierMinLen > 0 ? cfg.identifierMinLen : defaultIdentifierMinLen
  const range = prevSha || 'HEAD~1'
  const removedLiteralScanSchema = {
    type: 'object',
    required: ['rawOutput'],
    properties: {
      rawOutput: { type: 'string', description: 'Everything the removed-literal scan script printed to stdout, verbatim, unmodified, unsummarized' }
    }
  }
  const result = await tracedAgent(`
${renderRemovedLiteralScan({ runRoot, taskNum, tasksJsonPath, range, testGlobRegex, minLiteralLen, identifierMinLen })}
`, { label: `removed-literal-scan-${taskNum}`, schema: removedLiteralScanSchema, model: 'haiku' })
  return parseRemovedLiteralScanOutput(result && result.rawOutput)
}
// <</shared:removedLiteralScan>>

