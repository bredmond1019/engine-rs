#!/usr/bin/env bash
#
# test_pre_push.sh — regression test for hooks/pre-push.
#
# Self-contained: builds throwaway git repos in a temp dir, installs the REAL pre-push
# hook, and shadows `bastion` with a shim that emits canned
# `validated <path>: N error(s), M warning(s)` lines instead of running the real
# validator. No bastion/mev binary, database, or network needed, so this is safe and
# fast to run anywhere — same property as hooks/test_post_commit.sh preserves for
# hooks/post-commit.
#
#   bash hooks/test_pre_push.sh
#
# Exit status 0 = all cases pass; non-zero = at least one failure.
#
set -uo pipefail

# The suite must be immune to the environment it is invoked from. PREPUSH_STRICT changes the
# hook's blocking mode, and this file is itself a gated check in HQ's planning/harness.json —
# so `PREPUSH_STRICT=1 git push` runs these tests with that variable inherited, silently
# putting every non-strict case into strict mode. That really happened on 2026-08-04: the
# "block does NOT list the pre-existing error as new" case failed, and the failure was in the
# harness, not the hook. Cases that want strict mode set it per-invocation.
unset PREPUSH_STRICT

HOOK_SRC="$(cd "$(dirname "$0")" && pwd)/pre-push"
fail=0
check() { # check <description> <result: 0=pass>
  if [ "$2" -eq 0 ]; then printf 'PASS: %s\n' "$1"
  else printf 'FAIL: %s\n' "$1"; fail=1; fi
}

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# Fake `bastion`: the hook calls `bastion validate-brain <flag>` once per flag in
# --sync --graph --state --links --structure. The shim reads the desired per-invocation
# error count from $BASTION_SHIM_ERRORS (space-separated, one per flag in that order —
# defaults to 0 for any flag beyond the list) and prints a canned summary line, plus a
# fake `error [...]` diagnostic line for each reported error so the "print offending
# diagnostics" behaviour is exercised too.
BIN="$WORK/bin"; mkdir -p "$BIN"
cat > "$BIN/bastion" <<'SH'
#!/usr/bin/env bash
# invoked as: bastion validate-brain <flag>
flag="$2"
flags=(--sync --graph --state --links --structure)
idx=-1
for i in "${!flags[@]}"; do
  [ "${flags[$i]}" = "$flag" ] && idx="$i"
done
read -r -a errs <<< "${BASTION_SHIM_ERRORS:-0 0 0 0 0}"
n="${errs[$idx]:-0}"
n="${n:-0}"
i=0
while [ "$i" -lt "$n" ]; do
  echo "error [E_FAKE] shim/doc-$i.md — fake diagnostic for $flag"
  i=$((i + 1))
done
echo "validated $PWD: $n error(s), 0 warning(s)"
[ "$n" -gt 0 ] && exit 1
exit 0
SH
chmod +x "$BIN/bastion"

# `mev` shim, dispatched on $1 so it can stand in for both the freshness advisory
# (`mev conformance`) and the stage 3 consumer compile gate (`mev --help` /
# `mev check-consumers`).
#
#   MEV_SHIM_MODE=drift|pass (default pass) — `mev conformance` freshness banner. Only the
#     `toolchain-freshness [...]` marker line matters to the hook; the detail lines exist so
#     the test also covers the grep -A3 excerpt echoed back to the user.
#   MEV_HELP_HAS_CHECK_CONSUMERS=0 — `mev --help` omits "check-consumers", simulating a
#     stale installed binary predating the subcommand (default: present).
#   MEV_CHECK_CONSUMERS_MODE=pass|broken|no-brain (default pass) — `mev check-consumers`
#     behaviour: pass -> a lockfile-stale line + exit 0 (stale is non-blocking, per the
#     ticket's own contract); broken -> a broken line + exit 1; no-brain -> the real CLI's
#     "brain.toml not found" message + exit 1 (must be treated as an environment skip, not
#     a real break).
cat > "$BIN/mev" <<'SH'
#!/usr/bin/env bash
case "${1:-}" in
  --help)
    echo "mev — content validator"
    if [ "${MEV_HELP_HAS_CHECK_CONSUMERS:-1}" = "1" ]; then
      echo "  check-consumers   Compile every path-dependent consumer's test targets"
    fi
    exit 0
    ;;
  check-consumers)
    case "${MEV_CHECK_CONSUMERS_MODE:-pass}" in
      broken)
        echo "bastion: broken (2 errors: E0063 src/foo.rs:10, src/bar.rs:20)"
        exit 1
        ;;
      no-brain)
        echo "error: brain.toml not found: walked up from . and reached filesystem root"
        exit 1
        ;;
      *)
        echo "engine-rs: lockfile-stale"
        exit 0
        ;;
    esac
    ;;
  *)
    case "${MEV_SHIM_MODE:-pass}" in
      drift)
        echo "toolchain-freshness [DRIFT] — compiled-in build stamp vs source tree HEAD"
        echo "  compiled-in build stamp (2 items): aaaaaaaaaaaaaaaa"
        echo "  live source tree HEAD (1 items): bbbbbbbbbbbbbbbb"
        echo "    the running binary was built from aaaaaaa but the source is now at bbbbbbb"
        ;;
      *)
        echo "toolchain-freshness [PASS] — compiled-in build stamp vs source tree HEAD"
        ;;
    esac
    exit 0
    ;;
esac
SH
chmod +x "$BIN/mev"

new_repo() { # new_repo <dir>
  local d="$1"
  mkdir -p "$d"
  ( cd "$d"
    git init -q
    git config user.email t@t; git config user.name t
    printf '[[repos]]\n' > brain.toml
    mkdir -p hooks
    cp "$HOOK_SRC" hooks/pre-push; chmod +x hooks/pre-push
  )
}

# new_mev_repo <dir> — same as new_repo, plus a Cargo.toml naming the "mev" package, so
# stage 3 (mev-repo-only) actually engages instead of short-circuiting on the
# `grep '^name = "mev"' Cargo.toml` guard.
new_mev_repo() {
  local d="$1"
  new_repo "$d"
  printf '[package]\nname = "mev"\nversion = "0.1.0"\n' > "$d/Cargo.toml"
}

run_hook() { # run_hook <dir> -> sets HOOK_RC, HOOK_OUT
  local d="$1"
  HOOK_OUT="$(cd "$d" && PATH="$BIN:$PATH" ./hooks/pre-push 2>&1)"
  HOOK_RC=$?
}

# Same as run_hook but with a MINIMAL PATH — $BIN plus the system dirs only, deliberately
# excluding ~/.cargo/bin. Needed for the "binary absent" case: with the inherited PATH,
# hiding $BIN/mev would just fall through to the developer's real installed mev and the
# test would assert nothing.
run_hook_isolated() { # run_hook_isolated <dir> -> sets HOOK_RC, HOOK_OUT
  local d="$1"
  HOOK_OUT="$(cd "$d" && PATH="$BIN:/usr/bin:/bin" ./hooks/pre-push 2>&1)"
  HOOK_RC=$?
}

# --- Case 1: under baseline -> exit 0 ---
R1="$WORK/r1"; new_repo "$R1"
echo '{"errors": 5, "updated": "2026-07-31"}' > "$R1/hooks/validate-baseline.json"
BASTION_SHIM_ERRORS="1 0 0 0 0" run_hook "$R1"   # total 1 < baseline 5
check "under baseline: exit 0" "$([ "$HOOK_RC" -eq 0 ]; echo $?)"

# --- Case 2: at baseline -> exit 0 ---
R2="$WORK/r2"; new_repo "$R2"
echo '{"errors": 3, "updated": "2026-07-31"}' > "$R2/hooks/validate-baseline.json"
BASTION_SHIM_ERRORS="1 1 1 0 0" run_hook "$R2"   # total 3 == baseline 3
check "at baseline: exit 0" "$([ "$HOOK_RC" -eq 0 ]; echo $?)"

# --- Case 3: over baseline -> exit 1, offending diagnostics printed ---
R3="$WORK/r3"; new_repo "$R3"
echo '{"errors": 2, "updated": "2026-07-31"}' > "$R3/hooks/validate-baseline.json"
BASTION_SHIM_ERRORS="2 2 0 0 0" run_hook "$R3"   # total 4 > baseline 2
{ [ "$HOOK_RC" -eq 1 ]; }; check "over baseline: exit 1" $?
printf '%s' "$HOOK_OUT" | grep -q "E_FAKE"; check "over baseline: offending diagnostics printed" $?
printf '%s' "$HOOK_OUT" | grep -q "baseline is 2"; check "over baseline: message names the baseline" $?
printf '%s' "$HOOK_OUT" | grep -q "4 error(s)"; check "over baseline: message names the new count" $?
printf '%s' "$HOOK_OUT" | grep -q -- "--no-verify"; check "over baseline: message names the --no-verify escape" $?

# --- Case 4: bastion absent from PATH -> exit 0 with warning ---
R4="$WORK/r4"; new_repo "$R4"
echo '{"errors": 0}' > "$R4/hooks/validate-baseline.json"
HOOK_OUT="$(cd "$R4" && PATH="/usr/bin:/bin" ./hooks/pre-push 2>&1)"; HOOK_RC=$?
{ [ "$HOOK_RC" -eq 0 ]; }; check "bastion absent: exit 0" $?
printf '%s' "$HOOK_OUT" | grep -qi "warning"; check "bastion absent: warning printed" $?

# --- Case 5: no brain.toml found walking up -> exit 0 ---
R5="$WORK/r5_no_brain_toml"; mkdir -p "$R5/hooks"
cp "$HOOK_SRC" "$R5/hooks/pre-push"; chmod +x "$R5/hooks/pre-push"
HOOK_OUT="$(cd "$R5" && PATH="$BIN:$PATH" ./hooks/pre-push 2>&1)"; HOOK_RC=$?
{ [ "$HOOK_RC" -eq 0 ]; }; check "no brain.toml: exit 0" $?

# --- Case 6: missing baseline file treated as 0 -> any error total blocks ---
R6="$WORK/r6"; new_repo "$R6"
# deliberately do not write hooks/validate-baseline.json
BASTION_SHIM_ERRORS="1 0 0 0 0" run_hook "$R6"   # total 1 > missing-baseline-as-0
check "missing baseline treated as 0: exit 1 on any error" "$([ "$HOOK_RC" -eq 1 ]; echo $?)"

R6B="$WORK/r6b"; new_repo "$R6B"
BASTION_SHIM_ERRORS="0 0 0 0 0" run_hook "$R6B"  # total 0, missing baseline as 0 -> 0 <= 0
check "missing baseline treated as 0: exit 0 when total is also 0" "$([ "$HOOK_RC" -eq 0 ]; echo $?)"

# --- Case 7: warnings alone never block (unparseable flag output contributes 0, non-fatal) ---
R7="$WORK/r7"; new_repo "$R7"
echo '{"errors": 0}' > "$R7/hooks/validate-baseline.json"
cat > "$BIN/bastion" <<'SH'
#!/usr/bin/env bash
# Emits an unparseable line for every flag (simulates a bastion output-format drift) —
# the hook must warn, not crash and not block, since there is no over-baseline signal.
echo "unparseable garbage output for $1"
exit 0
SH
chmod +x "$BIN/bastion"
run_hook "$R7"
{ [ "$HOOK_RC" -eq 0 ]; }; check "unparseable output: never blocks on its own" $?
printf '%s' "$HOOK_OUT" | grep -qi "warning"; check "unparseable output: warning printed" $?

# Case 7 permanently shadowed $BIN/bastion with an always-unparseable shim; restore the
# real shim before any later case that relies on BASTION_SHIM_ERRORS driving stage 1.
cat > "$BIN/bastion" <<'SH'
#!/usr/bin/env bash
# invoked as: bastion validate-brain <flag>
flag="$2"
flags=(--sync --graph --state --links --structure)
idx=-1
for i in "${!flags[@]}"; do
  [ "${flags[$i]}" = "$flag" ] && idx="$i"
done
read -r -a errs <<< "${BASTION_SHIM_ERRORS:-0 0 0 0 0}"
n="${errs[$idx]:-0}"
n="${n:-0}"
i=0
while [ "$i" -lt "$n" ]; do
  echo "error [E_FAKE] shim/doc-$i.md — fake diagnostic for $flag"
  i=$((i + 1))
done
echo "validated $PWD: $n error(s), 0 warning(s)"
[ "$n" -gt 0 ] && exit 1
exit 0
SH
chmod +x "$BIN/bastion"

# --- Case 8: repo gate — no planning/harness.json -> exit 0, notice, no stage-2 noise ---
R8="$WORK/r8"; new_repo "$R8"
echo '{"errors": 0}' > "$R8/hooks/validate-baseline.json"
run_hook "$R8"
{ [ "$HOOK_RC" -eq 0 ]; }; check "repo gate: no harness.json -> exit 0" $?
printf '%s' "$HOOK_OUT" | grep -q "skipping repo gate"; check "repo gate: no harness.json -> notice printed" $?

# --- Case 9: repo gate — gated check passes -> exit 0 ---
R9="$WORK/r9"; new_repo "$R9"
echo '{"errors": 0}' > "$R9/hooks/validate-baseline.json"
mkdir -p "$R9/planning"
cat > "$R9/planning/harness.json" <<'JSON'
{
  "stack": "nextjs",
  "validation": { "checks": [
    { "name": "lint", "command": "true", "gates": true }
  ] }
}
JSON
touch "$R9/package.json"
run_hook "$R9"
{ [ "$HOOK_RC" -eq 0 ]; }; check "repo gate: passing check -> exit 0" $?
printf '%s' "$HOOK_OUT" | grep -q "lint (true) ... passed"; check "repo gate: passing check reported" $?

# --- Case 10: repo gate — gated check fails -> exit 1, offending output printed ---
R10="$WORK/r10"; new_repo "$R10"
echo '{"errors": 0}' > "$R10/hooks/validate-baseline.json"
mkdir -p "$R10/planning"
cat > "$R10/planning/harness.json" <<'JSON'
{
  "stack": "nextjs",
  "validation": { "checks": [
    { "name": "lint", "command": "echo boom-output && false", "gates": true }
  ] }
}
JSON
touch "$R10/package.json"
run_hook "$R10"
{ [ "$HOOK_RC" -eq 1 ]; }; check "repo gate: failing check -> exit 1" $?
printf '%s' "$HOOK_OUT" | grep -q "boom-output"; check "repo gate: failing check output printed" $?
printf '%s' "$HOOK_OUT" | grep -q "BLOCKED (stage 2)"; check "repo gate: block message names stage 2" $?
printf '%s' "$HOOK_OUT" | grep -q -- "--no-verify"; check "repo gate: block message names the --no-verify escape" $?

# --- Case 11: repo gate — stack marker file missing -> unscaffolded, skip (exit 0) ---
R11="$WORK/r11"; new_repo "$R11"
echo '{"errors": 0}' > "$R11/hooks/validate-baseline.json"
mkdir -p "$R11/planning"
cat > "$R11/planning/harness.json" <<'JSON'
{
  "stack": "nextjs",
  "validation": { "checks": [
    { "name": "lint", "command": "false", "gates": true }
  ] }
}
JSON
# deliberately no package.json -> repo looks unscaffolded
run_hook "$R11"
{ [ "$HOOK_RC" -eq 0 ]; }; check "repo gate: unscaffolded stack -> exit 0" $?
printf '%s' "$HOOK_OUT" | grep -q "looks unscaffolded"; check "repo gate: unscaffolded notice printed" $?

# --- Case 12: repo gate — a gated check's command isn't on PATH -> warn, skip, exit 0 ---
R12="$WORK/r12"; new_repo "$R12"
echo '{"errors": 0}' > "$R12/hooks/validate-baseline.json"
mkdir -p "$R12/planning"
cat > "$R12/planning/harness.json" <<'JSON'
{
  "stack": "nextjs",
  "validation": { "checks": [
    { "name": "lint", "command": "totally-nonexistent-tool run", "gates": true }
  ] }
}
JSON
touch "$R12/package.json"
run_hook "$R12"
{ [ "$HOOK_RC" -eq 0 ]; }; check "repo gate: missing tool -> exit 0" $?
printf '%s' "$HOOK_OUT" | grep -q "not on PATH"; check "repo gate: missing tool warning printed" $?

# --- Case 13: repo gate — non-gated check is never run ---
R13="$WORK/r13"; new_repo "$R13"
echo '{"errors": 0}' > "$R13/hooks/validate-baseline.json"
mkdir -p "$R13/planning"
cat > "$R13/planning/harness.json" <<'JSON'
{
  "stack": "nextjs",
  "validation": { "checks": [
    { "name": "not-gated", "command": "false", "gates": false }
  ] }
}
JSON
touch "$R13/package.json"
run_hook "$R13"
{ [ "$HOOK_RC" -eq 0 ]; }; check "repo gate: non-gated check skipped -> exit 0" $?
printf '%s' "$HOOK_OUT" | grep -q "no gates:true checks"; check "repo gate: non-gated-only notice printed" $?

# --- Case 14: stage 1 failure alone still blocks even if stage 2 would pass ---
R14="$WORK/r14"; new_repo "$R14"
echo '{"errors": 0}' > "$R14/hooks/validate-baseline.json"
mkdir -p "$R14/planning"
cat > "$R14/planning/harness.json" <<'JSON'
{
  "stack": "nextjs",
  "validation": { "checks": [
    { "name": "lint", "command": "true", "gates": true }
  ] }
}
JSON
touch "$R14/package.json"
BASTION_SHIM_ERRORS="1 0 0 0 0" run_hook "$R14"   # stage 1: total 1 > baseline 0
{ [ "$HOOK_RC" -eq 1 ]; }; check "combined: stage 1 alone still blocks" $?
printf '%s' "$HOOK_OUT" | grep -q "BLOCKED (stage 1)"; check "combined: stage 1 block message present" $?
printf '%s' "$HOOK_OUT" | grep -q "stage 2/2"; check "combined: stage 2 still ran and reported" $?

# =========================================================================================
# Delta attribution (block on what THIS push introduced) + PREPUSH_STRICT.
#
# The shim's diagnostics are distinguishable per flag ("... for --sync" vs "... for --graph"),
# which is what lets these cases assert WHICH errors were reported as new.
# =========================================================================================

SYNC_ERR='error [E_FAKE] shim/doc-0.md — fake diagnostic for --sync'
GRAPH_ERR='error [E_FAKE] shim/doc-0.md — fake diagnostic for --graph'

seed_last_good() { # seed_last_good <repo> <line>...
  local d="$1"; shift
  mkdir -p "$d/.git"
  python3 -c '
import json, sys
json.dump({"errors": sys.argv[2:]}, open(sys.argv[1], "w"), indent=2)
' "$d/.git/validate-last-good.json" x "$@"
}

# --- Case 8: a pre-existing error, unchanged, does not block ---
R8="$WORK/r8"; new_repo "$R8"
echo '{"errors": 0}' > "$R8/hooks/validate-baseline.json"
seed_last_good "$R8" "$SYNC_ERR"
BASTION_SHIM_ERRORS="1 0 0 0 0" run_hook "$R8"   # total 1 > baseline 0, but nothing NEW
{ [ "$HOOK_RC" -eq 0 ]; }; check "delta: pre-existing error alone does not block" $?
printf '%s' "$HOOK_OUT" | grep -q "not blocking"; check "delta: pre-existing error reported as non-blocking" $?

# --- Case 9: a NEW error blocks, even though the old one is tolerated ---
R9="$WORK/r9"; new_repo "$R9"
echo '{"errors": 0}' > "$R9/hooks/validate-baseline.json"
seed_last_good "$R9" "$SYNC_ERR"
BASTION_SHIM_ERRORS="1 1 0 0 0" run_hook "$R9"   # --sync known, --graph is new
{ [ "$HOOK_RC" -eq 1 ]; }; check "delta: a newly introduced error blocks" $?
printf '%s' "$HOOK_OUT" | grep -q "for --graph"; check "delta: block names the NEW diagnostic" $?

# --- Case 10: the block report lists only what is new, not the pre-existing noise ---
# This is the fairness property: you are told what YOU broke, not handed the whole corpus.
printf '%s' "$HOOK_OUT" | grep -q "for --sync"
{ [ "$?" -ne 0 ]; }; check "delta: block does NOT list the pre-existing error as new" $?

# --- Case 11: an error surfacing in a file this push never touched still blocks ---
# The delete-breaks-someone-else's-edge case: attribution is by DELTA, never by path, so a
# diagnostic naming an untouched file is still attributed to the push that introduced it.
printf '%s' "$HOOK_OUT" | grep -q "shim/doc-0.md"; check "delta: blocks on an error in an untouched file" $?

# --- Case 12: PREPUSH_STRICT gates on the whole corpus, even with nothing new ---
R12="$WORK/r12"; new_repo "$R12"
echo '{"errors": 0}' > "$R12/hooks/validate-baseline.json"
seed_last_good "$R12" "$SYNC_ERR"
HOOK_OUT="$(cd "$R12" && PATH="$BIN:$PATH" PREPUSH_STRICT=1 BASTION_SHIM_ERRORS="1 0 0 0 0" ./hooks/pre-push 2>&1)"; HOOK_RC=$?
{ [ "$HOOK_RC" -eq 1 ]; }; check "strict: blocks on a pre-existing error the delta mode allows" $?
printf '%s' "$HOOK_OUT" | grep -q "STRICT"; check "strict: message identifies strict mode" $?

# --- Case 13: a clean strict run passes ---
R13="$WORK/r13"; new_repo "$R13"
echo '{"errors": 0}' > "$R13/hooks/validate-baseline.json"
HOOK_OUT="$(cd "$R13" && PATH="$BIN:$PATH" PREPUSH_STRICT=1 BASTION_SHIM_ERRORS="0 0 0 0 0" ./hooks/pre-push 2>&1)"; HOOK_RC=$?
{ [ "$HOOK_RC" -eq 0 ]; }; check "strict: clean corpus passes" $?

# --- Case 14: a successful push records the high-water mark for next time ---
R14="$WORK/r14"; new_repo "$R14"
echo '{"errors": 5}' > "$R14/hooks/validate-baseline.json"
BASTION_SHIM_ERRORS="1 0 0 0 0" run_hook "$R14"   # under baseline, no last-good yet -> passes
{ [ "$HOOK_RC" -eq 0 ]; }; check "last-good: fresh clone falls back to baseline and passes" $?
[ -f "$R14/.git/validate-last-good.json" ]; check "last-good: written after a successful push" $?
grep -q -- "--sync" "$R14/.git/validate-last-good.json" 2>/dev/null
check "last-good: records the actual diagnostics seen" $?

# --- Case 15: an unreadable last-good falls back to the baseline rather than failing open ---
R15="$WORK/r15"; new_repo "$R15"
echo '{"errors": 0}' > "$R15/hooks/validate-baseline.json"
mkdir -p "$R15/.git"; printf 'not json{' > "$R15/.git/validate-last-good.json"
BASTION_SHIM_ERRORS="1 0 0 0 0" run_hook "$R15"   # total 1 > baseline 0
{ [ "$HOOK_RC" -eq 1 ]; }; check "last-good: corrupt file falls back to baseline and still blocks" $?

# --- Case 16: a stale mev binary warns loudly but NEVER blocks the push ---
R16="$WORK/r16"; new_repo "$R16"
echo '{"errors": 5, "updated": "2026-07-31"}' > "$R16/hooks/validate-baseline.json"
MEV_SHIM_MODE=drift BASTION_SHIM_ERRORS="0 0 0 0 0" run_hook "$R16"
{ [ "$HOOK_RC" -eq 0 ] && printf '%s' "$HOOK_OUT" | grep -q "installed 'mev' binary is STALE"; }
check "mev freshness: stale binary warns but never blocks" $?

# --- Case 17: the drift excerpt and the remediation command both reach the user ---
{ printf '%s' "$HOOK_OUT" | grep -q "the running binary was built from" \
  && printf '%s' "$HOOK_OUT" | grep -q "cargo install --path core/mev --force"; }
check "mev freshness: prints the drift detail and the fix command" $?

# --- Case 18: a current mev binary stays silent (no noise on the common path) ---
R18="$WORK/r18"; new_repo "$R18"
echo '{"errors": 5, "updated": "2026-07-31"}' > "$R18/hooks/validate-baseline.json"
MEV_SHIM_MODE=pass BASTION_SHIM_ERRORS="0 0 0 0 0" run_hook "$R18"
{ [ "$HOOK_RC" -eq 0 ] && ! printf '%s' "$HOOK_OUT" | grep -q "STALE"; }
check "mev freshness: current binary stays silent" $?

# --- Case 19: mev not installed at all is not an error (degrade, don't fail) ---
R19="$WORK/r19"; new_repo "$R19"
echo '{"errors": 5, "updated": "2026-07-31"}' > "$R19/hooks/validate-baseline.json"
mv "$BIN/mev" "$BIN/mev.hidden"
BASTION_SHIM_ERRORS="0 0 0 0 0" run_hook_isolated "$R19"
mv "$BIN/mev.hidden" "$BIN/mev"
{ [ "$HOOK_RC" -eq 0 ] && ! printf '%s' "$HOOK_OUT" | grep -q "STALE"; }
check "mev freshness: absent binary is not an error" $?

# --- Case 20: the advisory still fires on the BLOCKED path (stale writer matters either way) ---
R20="$WORK/r20"; new_repo "$R20"
echo '{"errors": 0, "updated": "2026-07-31"}' > "$R20/hooks/validate-baseline.json"
MEV_SHIM_MODE=drift BASTION_SHIM_ERRORS="3 0 0 0 0" run_hook "$R20"   # total 3 > baseline 0
{ [ "$HOOK_RC" -eq 1 ] && printf '%s' "$HOOK_OUT" | grep -q "installed 'mev' binary is STALE"; }
check "mev freshness: advisory still prints when the push is blocked" $?

# --- Case 21: non-mev repo -> stage 3 never even runs mev check-consumers ---
R21="$WORK/r21"; new_repo "$R21"   # plain new_repo: no Cargo.toml at all
echo '{"errors": 0}' > "$R21/hooks/validate-baseline.json"
MEV_CHECK_CONSUMERS_MODE=broken BASTION_SHIM_ERRORS="0 0 0 0 0" run_hook "$R21"
{ [ "$HOOK_RC" -eq 0 ] && ! printf '%s' "$HOOK_OUT" | grep -q "stage 3"; }
check "consumer gate: non-mev repo skips stage 3 entirely" $?

# --- Case 22: mev repo, a consumer is genuinely broken -> BLOCKS ---
R22="$WORK/r22"; new_mev_repo "$R22"
echo '{"errors": 0}' > "$R22/hooks/validate-baseline.json"
MEV_CHECK_CONSUMERS_MODE=broken BASTION_SHIM_ERRORS="0 0 0 0 0" run_hook "$R22"
{ [ "$HOOK_RC" -eq 1 ]; }; check "consumer gate: broken consumer blocks (exit 1)" $?
printf '%s' "$HOOK_OUT" | grep -q "BLOCKED (stage 3)"; check "consumer gate: block message names stage 3" $?
printf '%s' "$HOOK_OUT" | grep -q -- "--no-verify"; check "consumer gate: block message names the --no-verify escape" $?

# --- Case 23: mev repo, worst outcome is lockfile-stale -> reports loudly, does NOT block ---
R23="$WORK/r23"; new_mev_repo "$R23"
echo '{"errors": 0}' > "$R23/hooks/validate-baseline.json"
MEV_CHECK_CONSUMERS_MODE=pass BASTION_SHIM_ERRORS="0 0 0 0 0" run_hook "$R23"
{ [ "$HOOK_RC" -eq 0 ]; }; check "consumer gate: lockfile-stale does not block (exit 0)" $?
printf '%s' "$HOOK_OUT" | grep -q "lockfile-stale"; check "consumer gate: lockfile-stale reported to the operator" $?

# --- Case 24: mev repo, installed mev predates check-consumers -> skip, never block ---
R24="$WORK/r24"; new_mev_repo "$R24"
echo '{"errors": 0}' > "$R24/hooks/validate-baseline.json"
MEV_HELP_HAS_CHECK_CONSUMERS=0 MEV_CHECK_CONSUMERS_MODE=broken BASTION_SHIM_ERRORS="0 0 0 0 0" run_hook "$R24"
{ [ "$HOOK_RC" -eq 0 ] && printf '%s' "$HOOK_OUT" | grep -q "predates 'check-consumers'"; }
check "consumer gate: stale binary without the subcommand skips rather than blocks" $?

# --- Case 25: mev repo, no brain.toml discoverable (standalone checkout) -> skip, never block ---
R25="$WORK/r25"; new_mev_repo "$R25"
rm -f "$R25/brain.toml"
echo '{"errors": 0}' > "$R25/hooks/validate-baseline.json"
MEV_CHECK_CONSUMERS_MODE=no-brain BASTION_SHIM_ERRORS="0 0 0 0 0" run_hook "$R25"
{ [ "$HOOK_RC" -eq 0 ] && printf '%s' "$HOOK_OUT" | grep -q "no HQ tree here"; }
check "consumer gate: missing brain.toml skips rather than blocks" $?

# =========================================================================================
# HQ.7.B — the stray-`./~`-tree predicate is wired through stage 2's harness.json read, not
# a native hook stage (that's the whole point of registering it there instead of adding a
# fourth native stage). These cases prove stage 2 actually BLOCKS on the predicate rather
# than merely that scripts/check_no_stray_tilde.sh works standalone (already covered by the
# script's own red baseline in planning/HQ.7.B/tasks.md). The fixture's harness.json check
# mirrors the real script's own predicate (`test ! -e './~'`, filesystem-only, no git) so the
# fixture repo never needs scripts/lib.sh or HQ_ROOT resolution — only the wiring is under
# test here. The fixture's `~` lives under $WORK (a mktemp dir), never near the real repo root.
# =========================================================================================

# --- Case 26: registered check + a stray `~` present in the fixture repo -> BLOCKS ---
R26="$WORK/r26"; new_repo "$R26"
echo '{"errors": 0}' > "$R26/hooks/validate-baseline.json"
mkdir -p "$R26/planning"
cat > "$R26/planning/harness.json" <<'JSON'
{
  "stack": "nextjs",
  "validation": { "checks": [
    { "name": "no-stray-tilde-tree", "command": "test ! -e './~'", "gates": true }
  ] }
}
JSON
touch "$R26/package.json"
mkdir -p "$R26/~"; touch "$R26/~/SKILL.md"
run_hook "$R26"
{ [ "$HOOK_RC" -eq 1 ]; }; check "stray-tilde gate: registered check + tree present -> exit 1" $?
printf '%s' "$HOOK_OUT" | grep -q "no-stray-tilde-tree"; check "stray-tilde gate: block names the check" $?
printf '%s' "$HOOK_OUT" | grep -q "BLOCKED (stage 2)"; check "stray-tilde gate: block message names stage 2" $?

# --- Case 27: same registered check, no `~` present -> exit 0 ---
R27="$WORK/r27"; new_repo "$R27"
echo '{"errors": 0}' > "$R27/hooks/validate-baseline.json"
mkdir -p "$R27/planning"
cat > "$R27/planning/harness.json" <<'JSON'
{
  "stack": "nextjs",
  "validation": { "checks": [
    { "name": "no-stray-tilde-tree", "command": "test ! -e './~'", "gates": true }
  ] }
}
JSON
touch "$R27/package.json"
run_hook "$R27"
{ [ "$HOOK_RC" -eq 0 ]; }; check "stray-tilde gate: registered check + tree absent -> exit 0" $?
printf '%s' "$HOOK_OUT" | grep -q "no-stray-tilde-tree (test ! -e './~') ... passed"; check "stray-tilde gate: passing check reported" $?

echo
if [ "$fail" -eq 0 ]; then echo "ALL PASS"; else echo "FAILURES"; fi
exit "$fail"
