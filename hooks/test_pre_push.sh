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

run_hook() { # run_hook <dir> -> sets HOOK_RC, HOOK_OUT
  local d="$1"
  HOOK_OUT="$(cd "$d" && PATH="$BIN:$PATH" ./hooks/pre-push 2>&1)"
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

echo
if [ "$fail" -eq 0 ]; then echo "ALL PASS"; else echo "FAILURES"; fi
exit "$fail"
