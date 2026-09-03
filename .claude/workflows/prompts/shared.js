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

// <<shared:renderWorkAssertion>>
function renderWorkAssertion(gitCmd = 'git', taskNum, tasksJsonPath) {
  return `NAME_STATUS=$(${gitCmd} diff --name-status HEAD~1 HEAD); if [ -z "$NAME_STATUS" ]; then echo "WORK_ASSERTION_ABORT: task ${taskNum} commit diff is EMPTY (condition 1) - no work was committed"; exit 1; fi; WA_DECLARED=$(python3 -c "
import json
d = json.load(open('${tasksJsonPath}'))
t = [x for x in d if x.get('task_id') == ${taskNum}]
print(chr(10).join(t[0].get('files', []) if t else []))
"); WA_MATCH=0; WA_BADDEL=""; while IFS=$'\t' read -r WA_ST WA_P1 WA_P2; do WA_CHK="$WA_P1"; case "$WA_ST" in R*) WA_CHK="$WA_P2" ;; esac; if printf '%s\n' "$WA_DECLARED" | grep -qFx "$WA_CHK"; then WA_MATCH=1; else case "$WA_ST" in D*) WA_BADDEL="$WA_CHK" ;; esac; fi; done <<< "$NAME_STATUS"; if [ "$WA_MATCH" -eq 0 ]; then echo "WORK_ASSERTION_ABORT: task ${taskNum} commit's changed paths do not intersect declared files[] (condition 2) - declared: [$WA_DECLARED] - changed: [$NAME_STATUS]"; exit 1; fi; if [ -n "$WA_BADDEL" ]; then echo "WORK_ASSERTION_ABORT: task ${taskNum} commit deletes undeclared file '$WA_BADDEL' not present in files[] (condition 3) - declared: [$WA_DECLARED]"; exit 1; fi`
}
// <</shared:renderWorkAssertion>>

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
PYEOF`
}
// <</shared:renderEmojiGate>>

// <<shared:renderStateFlipScript>>
// The D64 validate-then-commit mutation for planning/state.json's authored block status: capture
// the pre-write bytes, mutate in memory, run `mev validate-brain --state` BEFORE and AFTER, and
// roll back byte-exactly on any NET-NEW diagnostic. Shared for the same reason as the emoji gate --
// it is executable Python performing a validated write, and the two engines had a full 57-line copy
// each. `indent` exists only because the two prompts nest it at different depths.
function renderStateFlipScript({ runRoot, indent }) {
  return `${indent}cd ${runRoot} && python3 -c "
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
" "<RESOLVED_ID>"`
}
// <</shared:renderStateFlipScript>>

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
function renderTestPrompt({ enginePhrase, overrideNote, runRootLabel, runRoot, checklistBody, diffBase, stateFile, recordedCommitsJson, emojiScopeNote, onPassRecipe, stateWrittenNote }) {
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
   Stage your changed source/test files explicitly, then commit using HEREDOC:
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
    into this commit even if they happen to already be staged:
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

