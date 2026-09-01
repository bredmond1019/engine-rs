#!/usr/bin/env bash
#
# test_pre-commit.sh — regression test for hooks/pre-commit.
#
# Self-contained: builds throwaway git repos in a temp dir, installs the REAL
# pre-commit hook and its check_frontmatter.py checker. Uses the real python3/PyYAML
# on this machine (no shim needed — this is pure YAML parsing, no external services),
# same property test_post_commit.sh and test_pre_push.sh preserve for their hooks.
#
#   bash hooks/test_pre-commit.sh
#
# Exit status 0 = all cases pass; non-zero = at least one failure.
#
set -uo pipefail

# Scrub the inherited git environment before any git call — git exports
# GIT_DIR (and friends) to every hook it runs, and hooks/pre-push stage 2
# runs this suite, so an unscrubbed fixture git call here targets the
# BRAIN ROOT's real repository instead of the throwaway repo below.
# Canonical copy + full defect history: scrub_git_env() in scripts/sync/lib.sh.
# Not sourced (would change this suite's PATH) — duplicated deliberately.
unset -v GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_OBJECT_DIRECTORY \
    GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_CEILING_DIRECTORIES GIT_COMMON_DIR

HOOK_SRC="$(cd "$(dirname "$0")" && pwd)/pre-commit"
CHECKER_SRC="$(cd "$(dirname "$0")" && pwd)/check_frontmatter.py"
GATE_SRC="$(cd "$(dirname "$0")" && pwd)/validate_brain_gate.sh"
BRAIN_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# check_frontmatter.py's two escape hatches delegate to base-template's
# scripts/check_frontmatter_presence.py (BT.ticket.engine-docs-drift-tripwire HALF A).
# Prefer base-template's own main-tree copy; if this ticket's own spec hasn't merged to
# base-template main yet, fall back to the first worktree copy on disk so the delegation
# wiring can still be exercised. Empty PRESENCE_SRC degrades every case below to the
# pre-delegation (return-0-on-no-fence) behavior — the retro-fixture case makes that
# degradation visible instead of silently passing.
PRESENCE_SRC="$BRAIN_ROOT/base-template/scripts/check_frontmatter_presence.py"
if [ ! -f "$PRESENCE_SRC" ]; then
  PRESENCE_SRC="$(ls "$BRAIN_ROOT"/base-template/trees/*/scripts/check_frontmatter_presence.py 2>/dev/null | head -1)"
fi
RETRO_FIXTURE="$BRAIN_ROOT/_planning/base-template/BT.ticket.engine-docs-drift-tripwire/evidence/_status-md-frontmatter-displaced-2026-08-22.txt"
# The ticket that produced this fixture has since been archived, which moves the evidence
# under `archive/` and silently SKIPped Case 10 — the one case proving the delegation
# catches the 2026-08-22 displaced-frontmatter regression. Fall back to a search rather
# than a second hardcoded path, so a future move does not re-open the hole.
if [ ! -f "$RETRO_FIXTURE" ]; then
  RETRO_FIXTURE="$(find -L "$BRAIN_ROOT/_planning/base-template" "$BRAIN_ROOT/base-template/planning" \
    -name '_status-md-frontmatter-displaced-2026-08-22.txt' 2>/dev/null | head -1)"
fi
fail=0
check() { # check <description> <result: 0=pass>
  if [ "$2" -eq 0 ]; then printf 'PASS: %s\n' "$1"
  else printf 'FAIL: %s\n' "$1"; fail=1; fi
}

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

new_repo() { # new_repo <dir>
  local d="$1"
  mkdir -p "$d"
  ( cd "$d"
    git init -q
    git config user.email t@t; git config user.name t
    git config core.hooksPath hooks
    mkdir -p hooks
    cp "$HOOK_SRC" hooks/pre-commit; chmod +x hooks/pre-commit
    cp "$CHECKER_SRC" hooks/check_frontmatter.py
    cp "$GATE_SRC" hooks/validate_brain_gate.sh
    # Mirror the sibling-repo layout check_frontmatter.py's _presence_check() expects:
    # <this test repo>/base-template/scripts/check_frontmatter_presence.py.
    if [ -n "$PRESENCE_SRC" ] && [ -f "$PRESENCE_SRC" ]; then
      mkdir -p base-template/scripts
      cp "$PRESENCE_SRC" base-template/scripts/check_frontmatter_presence.py
    fi
  )
}

commit_ok() { # commit_ok <dir> -> sets RC, OUT
  local d="$1"
  OUT="$(cd "$d" && git commit -q -m test 2>&1)"
  RC=$?
}

# --- Case 1: clean frontmatter (no colon trap) -> commit succeeds ---
R1="$WORK/r1"; new_repo "$R1"
cat > "$R1/clean.md" <<'EOF'
---
type: Note
title: Clean file
description: A perfectly ordinary description
---
# Clean
EOF
( cd "$R1" && git add clean.md )
commit_ok "$R1"
check "clean frontmatter: commit succeeds" "$([ "$RC" -eq 0 ]; echo $?)"

# --- Case 2: unquoted colon in description -> commit blocked ---
R2="$WORK/r2"; new_repo "$R2"
cat > "$R2/bad.md" <<'EOF'
---
type: Note
title: Bad
description: Retrieval regression: recall fell from 1.0 to 0.88
---
# Bad
EOF
( cd "$R2" && git add bad.md )
commit_ok "$R2"
check "unquoted colon in description: commit blocked" "$([ "$RC" -ne 0 ]; echo $?)"
check "unquoted colon: error names the file and line" "$(printf '%s' "$OUT" | grep -q "bad.md:4" ; echo $?)"

# --- Case 3: same value, properly quoted -> commit succeeds ---
R3="$WORK/r3"; new_repo "$R3"
cat > "$R3/quoted.md" <<'EOF'
---
type: Note
title: Quoted
description: "Retrieval regression: recall fell from 1.0 to 0.88"
---
# Quoted
EOF
( cd "$R3" && git add quoted.md )
commit_ok "$R3"
check "quoted colon: commit succeeds" "$([ "$RC" -eq 0 ]; echo $?)"

# --- Case 4: no frontmatter at all -> commit succeeds (not this gate's concern) ---
R4="$WORK/r4"; new_repo "$R4"
cat > "$R4/plain.md" <<'EOF'
# Just a heading

No frontmatter here.
EOF
( cd "$R4" && git add plain.md )
commit_ok "$R4"
check "no frontmatter: commit succeeds" "$([ "$RC" -eq 0 ]; echo $?)"

# --- Case 5: non-.md staged file with bad YAML-shaped content -> ignored ---
R5="$WORK/r5"; new_repo "$R5"
cat > "$R5/bad.txt" <<'EOF'
---
description: this: is not valid yaml either
---
EOF
( cd "$R5" && git add bad.txt )
commit_ok "$R5"
check "non-.md staged file: ignored, commit succeeds" "$([ "$RC" -eq 0 ]; echo $?)"

# --- Case 6: unstaged broken file -> ignored (only staged content is checked) ---
R6="$WORK/r6"; new_repo "$R6"
cat > "$R6/good.md" <<'EOF'
---
type: Note
title: Good
description: fine
---
# Good
EOF
( cd "$R6" && git add good.md )
cat > "$R6/broken.md" <<'EOF'
---
type: Note
title: Broken
description: this: breaks
---
# Broken
EOF
# broken.md exists in the working tree but is never staged.
commit_ok "$R6"
check "unstaged broken file: ignored, commit succeeds" "$([ "$RC" -eq 0 ]; echo $?)"

# --- Case 7: file staged, then the bad content amended and RE-staged -> checks staged blob ---
R7="$WORK/r7"; new_repo "$R7"
cat > "$R7/evolve.md" <<'EOF'
---
type: Note
title: Evolve
description: fine at first
---
# Evolve
EOF
( cd "$R7" && git add evolve.md )
cat > "$R7/evolve.md" <<'EOF'
---
type: Note
title: Evolve
description: now broken: this fails
---
# Evolve
EOF
( cd "$R7" && git add evolve.md )   # re-stage the broken version
commit_ok "$R7"
check "re-staged broken content: commit blocked (checks staged blob, not first add)" "$([ "$RC" -ne 0 ]; echo $?)"

# --- Case 8: no staged .md files at all -> exit 0 silently, nothing to check ---
R8="$WORK/r8"; new_repo "$R8"
printf 'a\n' > "$R8/x.txt"
( cd "$R8" && git add x.txt )
commit_ok "$R8"
check "no staged .md files: commit succeeds" "$([ "$RC" -eq 0 ]; echo $?)"

# --- Case 9: interpreter/dependency unusable (PyYAML not importable) -> non-fatal ---
# Isolate PATH to the system toolchain only (excludes homebrew's python3+PyYAML), same
# technique test_pre_push.sh uses for its "binary absent" case. macOS ships a bare
# /usr/bin/python3 with no PyYAML, so this exercises the "import yaml fails" degrade
# path rather than "no python3 at all" — both are covered by the same non-fatal branch
# in hooks/pre-commit (missing interpreter/dependency -> warn on stderr, exit 0).
R9="$WORK/r9"; new_repo "$R9"
cat > "$R9/bad.md" <<'EOF'
---
type: Note
title: Bad
description: this: also breaks
---
# Bad
EOF
( cd "$R9" && git add bad.md )
OUT="$(cd "$R9" && PATH="/usr/bin:/bin" git commit -q -m test 2>&1)"
RC=$?
check "PyYAML unimportable: non-fatal, commit succeeds" "$([ "$RC" -eq 0 ]; echo $?)"

# --- Case 10: retro-fixture — the real 2026-08-22 displaced frontmatter -> commit
# blocked. BEFORE this delegation existed, this exact content passed at exit 0 (hatch 1
# fired on lines[0] != "---" and returned 0 before the YAML parser ever ran) — that is
# the regression BT.ticket.engine-docs-drift-tripwire HALF A closes. Skipped, not
# failed, if base-template's check_frontmatter_presence.py isn't reachable on this
# machine (a bare clone of the brain repo without base-template checked out alongside
# it) — the delegation degrades gracefully in that case by design.
if [ -n "$PRESENCE_SRC" ] && [ -f "$PRESENCE_SRC" ] && [ -f "$RETRO_FIXTURE" ]; then
  R10="$WORK/r10"; new_repo "$R10"
  mkdir -p "$R10/planning"
  cp "$RETRO_FIXTURE" "$R10/planning/status.md"
  ( cd "$R10" && git add planning/status.md )
  commit_ok "$R10"
  check "retro-fixture (2026-08-22 displaced status.md): commit blocked" "$([ "$RC" -ne 0 ]; echo $?)"
  check "retro-fixture: error names DISPLACED" "$(printf '%s' "$OUT" | grep -q "DISPLACED" ; echo $?)"
else
  echo "SKIP: retro-fixture case (base-template check_frontmatter_presence.py or the fixture not found on this machine)"
fi

# --- Case 11: presence gate absent entirely -> delegation degrades gracefully, commit
# still succeeds (never block a commit on missing sibling tooling) ---
R11="$WORK/r11"; new_repo "$R11"
rm -rf "$R11/base-template"   # remove the presence-check wiring new_repo() just installed
cat > "$R11/no_presence.md" <<'EOF'
Some content that happens to start with a paragraph.

---
this: looks like frontmatter but is displaced
---
EOF
( cd "$R11" && git add no_presence.md )
commit_ok "$R11"
check "presence gate missing entirely: degrades gracefully, commit succeeds" "$([ "$RC" -eq 0 ]; echo $?)"

# ── created/updated date gate (ON by default since 2026-08-31) ────────────────
# These cases pin BOTH halves of the default: case 12 proves a bad date is now blocked
# with NO env var set at all, case 12b proves OKF_DATE_GATE=0 is a working escape hatch,
# and case 12c proves a code test fixture is out of scope (is_code_fixture). The cases
# after them still set OKF_DATE_GATE=1 explicitly, which is redundant but harmless and
# keeps them pinning the flag's own plumbing. Env passes through `git commit` into the
# hook and on into check_frontmatter.py, so this exercises the real code path.

commit_dated() { # commit_dated <dir> -> sets RC, OUT, with the date gate ON
  local d="$1"
  OUT="$(cd "$d" && OKF_DATE_GATE=1 git commit -q -m test 2>&1)"
  RC=$?
}

write_md() { # write_md <path> <created-line> <updated-line>
  cat > "$1" <<EOF
---
type: Note
title: Dated
description: A file carrying authorship dates
$2
$3
---
# Dated
EOF
}

# --- Case 12: gate ON by default -> a plainly bad date is blocked with no env set ---
R12="$WORK/r12"; new_repo "$R12"
write_md "$R12/d.md" "created: soon" "updated: whenever"
( cd "$R12" && git add d.md )
commit_ok "$R12"
check "date gate ON by default: bad dates blocked with no env var" "$([ "$RC" -ne 0 ]; echo $?)"

# --- Case 12b: OKF_DATE_GATE=0 is the escape hatch -> the same file commits ---
R12B="$WORK/r12b"; new_repo "$R12B"
write_md "$R12B/d.md" "created: soon" "updated: whenever"
( cd "$R12B" && git add d.md )
OUT="$(cd "$R12B" && OKF_DATE_GATE=0 git commit -q -m test 2>&1)"; RC=$?
check "OKF_DATE_GATE=0: same bad dates commit cleanly" "$([ "$RC" -eq 0 ]; echo $?)"

# --- Case 12c: a code test fixture is out of the date gate's scope ---
# Measured 2026-08-31: the only 5 files in the fleet the gate would reject are bastion
# fixtures under src/**/fixtures/ carrying an RFC3339 `updated:` no parser reads. They
# are test data, not documents; is_code_fixture() exempts them. The YAML PARSE gate must
# still apply to them, which the second half of this case pins.
R12C="$WORK/r12c"; new_repo "$R12C"
mkdir -p "$R12C/src/serve/fixtures"
write_md "$R12C/src/serve/fixtures/f.md" "created: 2026-06-30T00:00:00Z" "updated: whenever"
( cd "$R12C" && git add src/serve/fixtures/f.md )
commit_ok "$R12C"
check "date gate: src/**/fixtures/*.md exempt, commits cleanly" "$([ "$RC" -eq 0 ]; echo $?)"

cat > "$R12C/src/serve/fixtures/broken.md" <<'EOF'
---
type: Note
title: Bad
description: this: still breaks
---
# Bad
EOF
( cd "$R12C" && git add src/serve/fixtures/broken.md )
commit_ok "$R12C"
check "parse gate still applies to a fixture: unquoted colon blocked" "$([ "$RC" -ne 0 ]; echo $?)"

# --- Case 13: gate ON, unquoted YYYY-MM-DD -> succeeds ---
# PyYAML parses this into a datetime.date, NOT a str — the shape a naive regex rejects.
R13="$WORK/r13"; new_repo "$R13"
write_md "$R13/d.md" "created: 2026-08-01" "updated: 2026-08-29"
( cd "$R13" && git add d.md )
commit_dated "$R13"
check "date gate on: unquoted YYYY-MM-DD (parsed as datetime.date) succeeds" "$([ "$RC" -eq 0 ]; echo $?)"

# --- Case 14: gate ON, quoted YYYY-MM-DD -> succeeds (arrives as a str) ---
R14="$WORK/r14"; new_repo "$R14"
write_md "$R14/d.md" 'created: "2026-08-01"' 'updated: "2026-08-29"'
( cd "$R14" && git add d.md )
commit_dated "$R14"
check "date gate on: quoted YYYY-MM-DD succeeds" "$([ "$RC" -eq 0 ]; echo $?)"

# --- Case 15: gate ON, non-date value -> blocked ---
R15="$WORK/r15"; new_repo "$R15"
write_md "$R15/d.md" "created: 2026-08-01" "updated: soon"
( cd "$R15" && git add d.md )
commit_dated "$R15"
check "date gate on: non-date 'updated' blocked" "$([ "$RC" -ne 0 ]; echo $?)"
check "date gate on: error names the offending field" "$(printf '%s' "$OUT" | grep -q "updated" ; echo $?)"

# --- Case 16: gate ON, full timestamp -> blocked (datetime is a subclass of date) ---
R16="$WORK/r16"; new_repo "$R16"
write_md "$R16/d.md" "created: 2026-08-29T10:00:00Z" "updated: 2026-08-29"
( cd "$R16" && git add d.md )
commit_dated "$R16"
check "date gate on: full timestamp blocked" "$([ "$RC" -ne 0 ]; echo $?)"
check "date gate on: timestamp error points at 'timestamp:'" "$(printf '%s' "$OUT" | grep -q "timestamp" ; echo $?)"

# --- Case 17: gate ON, updated before created -> blocked ---
R17="$WORK/r17"; new_repo "$R17"
write_md "$R17/d.md" "created: 2026-08-29" "updated: 2026-08-01"
( cd "$R17" && git add d.md )
commit_dated "$R17"
check "date gate on: updated earlier than created blocked" "$([ "$RC" -ne 0 ]; echo $?)"

# --- Case 18: gate ON, fields absent -> succeeds (both are optional) ---
R18="$WORK/r18"; new_repo "$R18"
cat > "$R18/none.md" <<'EOF'
---
type: Note
title: No dates
description: Carries neither field
---
# No dates
EOF
( cd "$R18" && git add none.md )
commit_dated "$R18"
check "date gate on: absent created/updated succeeds" "$([ "$RC" -eq 0 ]; echo $?)"

# --- Case 19: gate ON, shaped like a date but impossible -> blocked ---
R19="$WORK/r19"; new_repo "$R19"
write_md "$R19/d.md" 'created: "2026-13-40"' "updated: 2026-08-29"
( cd "$R19" && git add d.md )
commit_dated "$R19"
check "date gate on: impossible date (2026-13-40) blocked" "$([ "$RC" -ne 0 ]; echo $?)"

# ── gate 2: corpus graph/structure delta gate (added 2026-09-01) ──────────────
# Fake `bastion`: the gate calls `bastion validate-brain <flag>` once per flag in
# --graph --structure (that order — see hooks/pre-commit's own call). The shim reads the
# desired per-invocation error count from $BASTION_SHIM_ERRORS (space-separated, one per
# flag in that order) and prints a canned summary line, plus one `error [...]` diagnostic
# line per reported error — same shape hooks/test_pre_push.sh's shim uses for stage 1, so
# the two suites exercise the identical shared hooks/validate_brain_gate.sh contract.
GATE_BIN="$WORK/gate-bin"; mkdir -p "$GATE_BIN"
cat > "$GATE_BIN/bastion" <<'SH'
#!/usr/bin/env bash
# invoked as: bastion validate-brain <flag>
flag="$2"
flags=(--graph --structure)
idx=-1
for i in "${!flags[@]}"; do
  [ "${flags[$i]}" = "$flag" ] && idx="$i"
done
read -r -a errs <<< "${BASTION_SHIM_ERRORS:-0 0}"
n="${errs[$idx]:-0}"
n="${n:-0}"
# Real directory, inside THIS fixture's own git repo, so the gate's repo-ownership
# classifier (`git -C <dir> rev-parse --show-toplevel`) can resolve it and correctly
# attribute these diagnostics to this repo rather than falling back to unattributable
# (never-blocking) advisory.
mkdir -p shim
i=0
while [ "$i" -lt "$n" ]; do
  echo "error [E_FAKE] shim/doc-$i.md — fake diagnostic for $flag"
  i=$((i + 1))
done
# One more, ONLY on the flag NESTED_ERROR_ON_FLAG names, pointing at NESTED_REPO_ERROR_PATH
# — used by the cross-repo advisory cases below, which need an error attributable to a
# DIFFERENT git repo than the one committing. Counted into the reported total so the
# summary line and the diagnostic count stay consistent.
total="$n"
if [ -n "${NESTED_REPO_ERROR_PATH:-}" ] && [ "${NESTED_ERROR_ON_FLAG:-}" = "$flag" ]; then
  echo "error [E_FAKE] $NESTED_REPO_ERROR_PATH — fake diagnostic for a DIFFERENT repo's file"
  total=$((total + 1))
fi
echo "validated $PWD: $total error(s), 0 warning(s)"
[ "$total" -gt 0 ] && exit 1
exit 0
SH
chmod +x "$GATE_BIN/bastion"

new_gated_repo() { # new_gated_repo <dir> -- new_repo() plus a brain.toml, so gate 2 engages
  local d="$1"
  new_repo "$d"
  printf '[[repos]]\n' > "$d/brain.toml"
}

commit_ok_gated() { # commit_ok_gated <dir> [env-assignment ...] -> sets RC, OUT; bastion shim on PATH
  local d="$1"; shift
  OUT="$(cd "$d" && env "$@" PATH="$GATE_BIN:$PATH" git commit -q -m test 2>&1)"
  RC=$?
}

# --- Case 20: gate 2, brain.toml present, bastion shim reports 0 errors -> commit succeeds,
# and the gate visibly ran (not silently skipped) ---
R20="$WORK/r20"; new_gated_repo "$R20"
cat > "$R20/clean.md" <<'EOF'
---
type: Note
title: Clean
description: fine
---
# Clean
EOF
( cd "$R20" && git add clean.md )
commit_ok_gated "$R20" BASTION_SHIM_ERRORS="0 0"
check "gate 2: brain.toml + bastion, 0 errors: commit succeeds" "$([ "$RC" -eq 0 ]; echo $?)"
check "gate 2: ran (not silently skipped)" "$(printf '%s' "$OUT" | grep -q "running validate-brain gate" ; echo $?)"

# --- Case 21: gate 2, a fresh fixture (no .git/validate-last-good.json yet) introduces
# 1 --graph error -> falls back to the tracked baseline (0, no hooks/validate-baseline.json
# in this fixture either) -> BLOCKED ---
R21="$WORK/r21"; new_gated_repo "$R21"
cat > "$R21/clean.md" <<'EOF'
---
type: Note
title: Clean
description: fine
---
# Clean
EOF
( cd "$R21" && git add clean.md )
commit_ok_gated "$R21" BASTION_SHIM_ERRORS="1 0"
check "gate 2: new error, no baseline: commit blocked" "$([ "$RC" -ne 0 ]; echo $?)"
check "gate 2: block message names the new diagnostic" "$(printf '%s' "$OUT" | grep -q "E_FAKE" ; echo $?)"

# --- Case 22: gate 2, the SAME error already recorded in .git/validate-last-good.json
# (simulating: someone else's earlier commit already let this one through) -> this commit
# is NOT blocked for it — the fairness property the whole gate exists for ---
R22="$WORK/r22"; new_gated_repo "$R22"
git_dir="$(cd "$R22" && git rev-parse --git-dir)"
cat > "$R22/$git_dir/validate-last-good.json" <<'EOF'
{"errors": ["error [E_FAKE] shim/doc-0.md — fake diagnostic for --graph"]}
EOF
cat > "$R22/clean.md" <<'EOF'
---
type: Note
title: Clean
description: fine
---
# Clean
EOF
( cd "$R22" && git add clean.md )
commit_ok_gated "$R22" BASTION_SHIM_ERRORS="1 0"
check "gate 2: pre-existing (already-known) error: commit succeeds" "$([ "$RC" -eq 0 ]; echo $?)"
check "gate 2: pre-existing error reported as non-blocking" "$(printf '%s' "$OUT" | grep -q "no NEW errors" ; echo $?)"

# --- Case 23: gate 2, brain.toml present but bastion NOT on PATH -> degrades gracefully,
# commit succeeds, warning printed (same non-fatal contract as gate 1's missing-tool cases) ---
R23="$WORK/r23"; new_gated_repo "$R23"
cat > "$R23/clean.md" <<'EOF'
---
type: Note
title: Clean
description: fine
---
# Clean
EOF
( cd "$R23" && git add clean.md )
OUT="$(cd "$R23" && PATH="/usr/bin:/bin" git commit -q -m test 2>&1)"; RC=$?
check "gate 2: bastion absent: non-fatal, commit succeeds" "$([ "$RC" -eq 0 ]; echo $?)"
check "gate 2: bastion absent: warning printed" "$(printf '%s' "$OUT" | grep -q "'bastion' not found on PATH" ; echo $?)"

# ── repo-scoped blocking: a NEW error owned by a DIFFERENT repo is advisory, not
# blocking (added 2026-09-01, same day as gate 2 itself — the concurrent-agents fairness
# fix) ────────────────────────────────────────────────────────────────────────
# Mirrors the real HQ / core/bastion-ui relationship: an OUTER repo (this fixture's own
# git, with brain.toml) containing a NESTED repo (its own separate .git, no brain.toml —
# find_brain_root walks up through it to the outer one, exactly like a real sub-repo).
new_nested_repo() { # new_nested_repo <outer-dir> -> also creates <outer-dir>/nested (own git)
  local d="$1"
  new_gated_repo "$d"
  mkdir -p "$d/nested"
  ( cd "$d/nested" && git init -q && git config user.email t@t; git config user.name t )
}

# --- Case 24: a NEW error OWNED BY THE NESTED REPO does not block a commit in the outer
# repo — it is reported as an advisory notice instead ---
R24="$WORK/r24"; new_nested_repo "$R24"
cat > "$R24/clean.md" <<'EOF'
---
type: Note
title: Clean
description: fine
---
# Clean
EOF
( cd "$R24" && git add clean.md )
commit_ok_gated "$R24" BASTION_SHIM_ERRORS="0 0" NESTED_ERROR_ON_FLAG="--graph" NESTED_REPO_ERROR_PATH="nested/doc.md"
check "cross-repo: error owned by a NESTED repo does not block the OUTER repo's commit" "$([ "$RC" -eq 0 ]; echo $?)"
check "cross-repo: reported as a NOTICE, not a block" "$(printf '%s' "$OUT" | grep -q "NOTICE" ; echo $?)"
check "cross-repo: notice names the nested repo's file" "$(printf '%s' "$OUT" | grep -q "nested/doc.md" ; echo $?)"

# --- Case 25: same setup, but the error is owned by the OUTER repo itself (a path with
# no nested .git between it and the outer root) -> blocks normally ---
R25="$WORK/r25"; new_nested_repo "$R25"
cat > "$R25/clean.md" <<'EOF'
---
type: Note
title: Clean
description: fine
---
# Clean
EOF
( cd "$R25" && git add clean.md )
commit_ok_gated "$R25" BASTION_SHIM_ERRORS="0 0" NESTED_ERROR_ON_FLAG="--graph" NESTED_REPO_ERROR_PATH="own-doc.md"
check "cross-repo: error owned by THIS (outer) repo still blocks" "$([ "$RC" -ne 0 ]; echo $?)"
check "cross-repo: block names the outer repo's own file" "$(printf '%s' "$OUT" | grep -q "own-doc.md" ; echo $?)"

echo
if [ "$fail" -eq 0 ]; then echo "ALL PASS"; else echo "FAILURES"; fi
exit "$fail"
