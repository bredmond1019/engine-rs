#!/usr/bin/env bash
#
# validate_brain_gate.sh — shared new-errors-only, this-repo-only `bastion validate-brain`
# delta gate.
#
# Extracted from hooks/pre-push's original stage 1 (2026-09-01) so hooks/pre-commit can run
# the same cheap subset of checks (--graph, --structure) at commit time, instead of every
# corpus relational error only ever surfacing at push time. One function, sourced by both
# hooks, so the attribution logic — the part worth getting right exactly once — is defined
# in exactly one place.
#
# Attribution rule, part 1 — NEW, not pre-existing: block on what THIS commit/push
# introduced, never on the corpus's whole pre-existing backlog. A bare `total > baseline`
# test would block every hooked repo whenever any file anywhere is already bad — including
# files this operation never touched, left broken by another session or an unattended
# routine. That is how a gate gets muted with --no-verify forever. This narrows the
# BLOCKING decision by delta (diffing the current error SET against
# `.git/validate-last-good.json`'s known set), never by path — the whole corpus is still
# VALIDATED every time, because the errors worth catching are relational (delete a doc in
# one repo, dangle a `related:` edge in another — the break surfaces on a FILE this
# operation never touched).
#
# Attribution rule, part 2 — OWNED by this repo, not another one (added 2026-09-01, same
# day as gate 2's launch — measured live within hours of enabling it): `validate-brain`
# reads the live FILESYSTEM, not git history or the index. It has no concept of "staged,"
# "committed," or even "belongs to this repo" — an agent three edits into an unfinished
# five-edit rename in `core/engine-rs`, nothing committed yet, will make `bastion
# validate-brain --graph` report an error on THAT repo's file the instant it's scanned. Part
# 1 alone does not protect against this: the error is genuinely NEW (nobody's last-good has
# seen it yet), so an unrelated commit in `core/mev` — or in HQ — would be blocked for a
# different repo's mid-flight work it had nothing to do with. Part 2 fixes this: a NEW error
# only BLOCKS if the file it names is physically owned by the repo currently being committed
# to (its own `git rev-parse --show-toplevel` matches this commit's). A new error owned by a
# DIFFERENT repo is still reported — loudly, as an advisory, never silently — but does not
# block. The residual case this does not remove: agent A edits a file inside agent B's repo
# (crossing a lane boundary /begin-orchestration rule 6 already asks lanes not to do) and
# commits from ITS OWN repo — that commit is not blocked (it owns nothing wrong in its own
# tree), and agent B is the one who discovers it, mid-flight, on its own next commit. Small
# blast radius (one lane, usually a small fix) rather than the whole fleet.
#
# `.git/validate-last-good.json` lives under `.git/` deliberately: untracked, per-clone,
# never committed, and scoped to the repo it lives in — HQ's is a different file from
# core/mev's, from core/bastion-ui's, etc., even though every repo's gate validates the
# SAME shared corpus (validate-brain always resolves the one HQ brain.toml walking up,
# regardless of which repo's cwd it's invoked from). It only ever advances past a run that
# did NOT block, so within one repo, the only way an unrelated commit gets blocked for that
# repo's own already-committed error is if someone bypassed the gate with --no-verify.
#
# Absent last-good (fresh clone, or nothing has passed through a gated hook yet in this
# repo) falls back to the tracked, git-committed `hooks/validate-baseline.json` — the
# original whole-corpus-total behavior, ALSO now repo-scoped for the blocking decision (see
# classify_new_errors below). That baseline only ever ratchets DOWN
# (scripts/sync/validate_brain.sh) — it is safe to be stale-high, and must never silently
# absorb an increase.
#
# VALIDATE_BRAIN_STRICT=1 (or the older PREPUSH_STRICT=1, kept as an alias) forces the
# whole-corpus, EVERY-repo total instead of the delta or the repo scoping — for a deploy, a
# big merge, or before propagating hooks fleet-wide, when "all of it correct, everywhere"
# is the actual question.
#
# Degrades gracefully, same spirit as every other check in these hooks:
#   - no brain.toml found walking up          -> skip, notice only (standalone checkout)
#   - `bastion` not on PATH                   -> skip, warning only
#   - validate-brain output for a flag doesn't parse -> that flag contributes 0, warning only
#   - `.git/validate-last-good.json` unreadable -> falls back to the tracked baseline
#   - this commit's own repo root can't be resolved -> no new error is ever classified as
#     this repo's own, so nothing blocks (same "when in doubt, don't block" posture as
#     every other soft path here) — reported as advisory only.
# Warnings never block. Only a measured, real over-baseline/new-error result THIS REPO OWNS
# blocks.
#
set -uo pipefail

# find_brain_root <start-dir>
# Walks UP from start-dir for brain.toml. Prints the brain root and returns 0, or returns 1
# with nothing printed if none is found before reaching /.
find_brain_root() {
  local d="$1"
  while [ "$d" != "/" ] && [ -n "$d" ]; do
    if [ -f "$d/brain.toml" ]; then
      printf '%s\n' "$d"
      return 0
    fi
    d="$(dirname "$d")"
  done
  return 1
}

# classify_new_errors <brain_root> <this_repo_toplevel> <staged_files>
# Reads newline-separated `error [CODE] <path> — <message>` diagnostics on stdin.
#
# LANE, not just repo: a file's "lane" is (a) the toplevel of the separate git repo that
# owns it, if any (e.g. core/bastion-ui, its own .git) — same as before; otherwise (b), if
# the path matches `[<tier>/]_planning/<slug>/...` (a sub-repo's planning vault, physically
# tracked by THIS repo's own git per CLAUDE.md standing rule 10 — `core/mev/planning` is a
# symlink into `core/_planning/mev/`), the lane is that specific `_planning/<slug>` vault,
# NOT the whole brain repo; otherwise (c) the brain root itself (genuinely shared top-level
# content: docs/, hooks/, scripts/, HQ's own top-level planning/). (b) is why gate 2 needed
# a second pass 2026-09-01: a commit scoped to core/_planning/mev/ was blocked by an
# untracked file in core/_planning/bella/ — different lanes, same physical git repo, so the
# original repo-toplevel-only check (a) alone couldn't tell them apart.
#
# <staged_files> is a newline-separated list of the paths THIS operation is actually
# touching, brain-root-relative — normally `git diff --cached --name-only` for a commit,
# converted to brain-root-relative if this repo isn't the brain root itself. Empty for
# `pre-push` (nothing is staged post-commit, pre-push), which deliberately falls back to
# the exact old whole-repo behavior: committing lane = {this_repo_toplevel} alone, matching
# pre-push stage 1 before this lane split existed and keeping it unaffected by design.
#
# When staged files ARE given, the committing lane set is every lane touched by them (a
# commit spanning two lanes is blocked by either); a new error blocks iff its own lane is
# in that set — never merely "owned by the physical repo."
#
# Writes blocking lines (this operation's own lane(s)) and advisory lines (every other
# lane, or unresolvable) to two temp files and prints their paths as
# "<blocking-file>\t<advisory-file>". Caller is responsible for removing both.
classify_new_errors() {
  local brain_root="$1" this_repo="$2" staged_files="$3"
  local blocking_file advisory_file
  blocking_file="$(mktemp)"
  advisory_file="$(mktemp)"
  python3 -c '
import os
import subprocess
import sys

brain_root, this_repo, staged_raw, blocking_path, advisory_path = sys.argv[1:6]
lines = [ln for ln in sys.stdin.read().splitlines() if ln.strip()]
staged_files = [p for p in staged_raw.splitlines() if p.strip()]

def lane_of(path):
    fdir = os.path.dirname(os.path.join(brain_root, path))
    owner = ""
    try:
        r = subprocess.run(
            ["git", "-C", fdir, "rev-parse", "--show-toplevel"],
            capture_output=True, text=True, timeout=5,
        )
        if r.returncode == 0:
            owner = r.stdout.strip()
    except Exception:
        owner = ""
    if owner and owner != brain_root:
        return owner
    # A sub-repo planning vault shows up under TWO different path shapes that must map
    # to the SAME lane token: the real vault path `[<tier>/]_planning/<slug>/...` (how a
    # commit stages it, e.g. via `git add core/_planning/mev/...`) and the symlinked face
    # `<tier>/<slug>/planning/...` (how `bastion validate-brain` actually REPORTS it, e.g.
    # `core/bella/planning/...` -- validate-brain resolves through the symlink). Missing
    # this equivalence was the exact bug that shipped 2026-09-01: staging the real vault
    # path for the offending file itself still read as advisory, because its lane token
    # never matched the symlink-face token in the validate-brain diagnostic line.
    parts = path.split("/")
    for i, part in enumerate(parts):
        if part == "_planning":
            prefix = "/".join(parts[:i])
            slug = parts[i + 1] if i + 1 < len(parts) else ""
            return "planning:" + (prefix + "/" + slug if prefix else slug)
        if part == "planning":
            prefix = "/".join(parts[:i])
            return "planning:" + (prefix if prefix else "_root")
    return brain_root

if staged_files:
    committing_lanes = {lane_of(p) for p in staged_files}
else:
    committing_lanes = {this_repo} if this_repo else set()

blocking = []
advisory = []
for ln in lines:
    # "error [CODE] <path> -- <message>" -- path never contains " -- " itself.
    try:
        rest = ln.split("] ", 1)[1]
        path = rest.split(" — ", 1)[0]
    except IndexError:
        advisory.append(ln)
        continue
    if lane_of(path) in committing_lanes:
        blocking.append(ln)
    else:
        advisory.append(ln)
with open(blocking_path, "w") as f:
    f.write("\n".join(blocking))
with open(advisory_path, "w") as f:
    f.write("\n".join(advisory))
' "$brain_root" "$this_repo" "$staged_files" "$blocking_file" "$advisory_file" 2>/dev/null
  printf '%s\t%s\n' "$blocking_file" "$advisory_file"
}

# run_validate_brain_gate <label> <flag> [<flag> ...]
# <label> prefixes every line this prints (to stderr), e.g. "pre-commit" or
# "pre-push: [stage 1/2]" — purely cosmetic, callers keep their own voice.
# Each <flag> is one `bastion validate-brain` flag (--graph, --structure, --links, --state,
# --sync — the flags do NOT compose, so one invocation per flag, same as validate_brain.sh).
# Returns 0 (not blocking) or 1 (BLOCKED — see attribution rules above).
run_validate_brain_gate() {
  local label="$1"; shift
  local FLAGS=("$@")
  local start_dir
  start_dir="$(pwd)"

  local brain_root
  brain_root="$(find_brain_root "$start_dir")"
  if [ -z "$brain_root" ]; then
    echo "$label: no brain.toml found walking up from $start_dir — skipping validate-brain gate (standalone checkout)" >&2
    return 0
  fi
  # Normalize to the PHYSICAL path: find_brain_root is a pure dirname walk (never resolves
  # symlinks), but `git rev-parse --show-toplevel` always does (e.g. macOS's /var/folders/...
  # resolves to /private/var/folders/...). Left unnormalized, brain_root and
  # this_repo_toplevel below can name the SAME directory as two different strings, silently
  # breaking both the "am I at brain root" check and the staged-file relative-path
  # conversion that feeds classify_new_errors' lane scoping.
  brain_root="$(cd "$brain_root" && pwd -P)"

  if ! command -v bastion >/dev/null 2>&1; then
    echo "$label: warning: 'bastion' not found on PATH — skipping validate-brain gate. Install bastion to enable it." >&2
    return 0
  fi

  local total=0
  local diagnostics=""

  echo "$label: running validate-brain gate (${#FLAGS[@]} check(s): ${FLAGS[*]}, corpus root: $brain_root)..." >&2

  local flag out start_ts end_ts elapsed summary_line n
  for flag in "${FLAGS[@]}"; do
    printf '%s:   %s ... ' "$label" "$flag" >&2
    start_ts=$(date +%s 2>/dev/null || echo 0)
    out="$(bastion validate-brain "$flag" </dev/null 2>&1)"
    end_ts=$(date +%s 2>/dev/null || echo 0)
    elapsed=$((end_ts - start_ts))
    summary_line="$(printf '%s\n' "$out" | grep -E '^validated .*: [0-9]+ error\(s\), [0-9]+ warning\(s\)$' | tail -1)"
    if [ -z "$summary_line" ]; then
      echo "warning: unparseable output (${elapsed}s) — treating as 0 errors" >&2
      continue
    fi
    n="$(printf '%s\n' "$summary_line" | sed -E 's/^validated .*: ([0-9]+) error\(s\).*/\1/')"
    case "$n" in
      ''|*[!0-9]*)
        echo "warning: unparseable error count (${elapsed}s) — treating as 0 errors" >&2
        continue
        ;;
    esac
    echo "$summary_line (${elapsed}s)" >&2
    total=$((total + n))
    if [ "$n" -gt 0 ]; then
      local flag_diagnostics
      flag_diagnostics="$(printf '%s\n' "$out" | grep -E '^error \[' || true)"
      [ -n "$flag_diagnostics" ] && diagnostics="${diagnostics}${flag_diagnostics}
"
    fi
  done

  echo "$label: validate-brain gate finished — $total total error(s) (baseline check next)" >&2

  local baseline_file="$brain_root/hooks/validate-baseline.json"
  local baseline=0
  if [ -f "$baseline_file" ]; then
    local parsed
    parsed="$(python3 -c '
import json, sys
try:
    with open(sys.argv[1]) as f:
        d = json.load(f)
    print(int(d.get("errors", 0)))
except Exception:
    print(0)
' "$baseline_file" 2>/dev/null)"
    case "$parsed" in
      ''|*[!0-9]*) baseline=0 ;;
      *) baseline="$parsed" ;;
    esac
  fi

  local last_good=""
  local git_dir
  git_dir="$(git rev-parse --git-dir 2>/dev/null || true)"
  [ -n "$git_dir" ] && last_good="$git_dir/validate-last-good.json"

  local this_repo_toplevel
  this_repo_toplevel="$(git rev-parse --show-toplevel 2>/dev/null || true)"

  # staged_files: what THIS operation actually touches, brain-root-relative — used by
  # classify_new_errors to scope blocking by LANE, not just by physical repo (see its
  # header). Empty at push time (nothing staged post-commit), so pre-push falls back to its
  # original whole-repo behavior automatically — this is deliberate, not a gap.
  local staged_files=""
  local raw_staged
  raw_staged="$(git diff --cached --name-only 2>/dev/null || true)"
  if [ -n "$raw_staged" ]; then
    if [ "$this_repo_toplevel" = "$brain_root" ]; then
      staged_files="$raw_staged"
    else
      local repo_prefix="${this_repo_toplevel#"$brain_root"/}"
      staged_files="$(printf '%s\n' "$raw_staged" | sed "s#^#$repo_prefix/#")"
    fi
  fi

  local strict="${VALIDATE_BRAIN_STRICT:-${PREPUSH_STRICT:-0}}"

  if [ "$strict" = "1" ]; then
    if [ "$total" -gt "$baseline" ]; then
      echo "" >&2
      echo "$label: BLOCKED (STRICT) — the corpus carries $total error(s) against a baseline of $baseline." >&2
      echo "" >&2
      [ -n "$diagnostics" ] && printf '%s' "$diagnostics" >&2
      echo "" >&2
      echo "$label: strict mode blocks on the whole-corpus total, everywhere, not just what this" >&2
      echo "$label: repo introduced. Fix the error(s) above, re-run without VALIDATE_BRAIN_STRICT," >&2
      echo "$label: or bypass with --no-verify." >&2
      echo "" >&2
      return 1
    fi
    echo "$label: STRICT: whole corpus clean ($total error(s) vs baseline $baseline)." >&2
  elif [ -n "$last_good" ] && [ -f "$last_good" ] && command -v python3 >/dev/null 2>&1; then
    local introduced
    introduced="$(printf '%s' "$diagnostics" | python3 -c '
import json, sys
try:
    with open(sys.argv[1]) as f:
        known = set(json.load(f).get("errors", []))
except Exception:
    known = None
current = [ln for ln in sys.stdin.read().splitlines() if ln.strip()]
if known is None:          # unreadable last-good: say nothing, caller falls back
    sys.exit(3)
# dict.fromkeys dedupes while preserving order: the same diagnostic is reported once per
# flag that surfaces it, and printing it several times helps nobody.
for ln in dict.fromkeys(current):
    if ln not in known:
        print(ln)
' "$last_good" 2>/dev/null)"
    local rc=$?
    if [ "$rc" -eq 3 ]; then
      echo "$label: warning: could not read $last_good — falling back to the tracked baseline." >&2
      if [ "$total" -gt "$baseline" ]; then
        echo "$label: BLOCKED — $total error(s) against baseline $baseline." >&2
        [ -n "$diagnostics" ] && printf '%s' "$diagnostics" >&2
        return 1
      fi
    elif [ -n "$introduced" ]; then
      local files blocking_file advisory_file blocking_new advisory_new
      files="$(classify_new_errors "$brain_root" "$this_repo_toplevel" "$staged_files" <<< "$introduced")"
      blocking_file="${files%%$'\t'*}"
      advisory_file="${files#*$'\t'}"
      blocking_new="$(cat "$blocking_file" 2>/dev/null)"
      advisory_new="$(cat "$advisory_file" 2>/dev/null)"
      rm -f "$blocking_file" "$advisory_file"

      if [ -n "$advisory_new" ]; then
        echo "" >&2
        echo "$label: NOTICE — new corpus error(s) elsewhere in the fleet (not blocking THIS repo's commit):" >&2
        echo "" >&2
        printf '%s\n' "$advisory_new" >&2
      fi

      if [ -n "$blocking_new" ]; then
        echo "" >&2
        echo "$label: BLOCKED — this repo's own commit introduces new corpus error(s):" >&2
        echo "" >&2
        printf '%s\n' "$blocking_new" >&2
        echo "" >&2
        echo "$label: these are new since the last successful validate-brain gate on this clone. The corpus" >&2
        echo "$label: carries $total error(s) in total; the rest pre-date your work and are not blocking you." >&2
        echo "$label: fix the new error(s) above, or bypass this check with --no-verify." >&2
        echo "" >&2
        return 1
      fi
    else
      if [ "$total" -gt 0 ]; then
        echo "$label: no NEW errors owned by this repo; $total pre-existing/other-repo error(s) reported, not blocking." >&2
        echo "$label:   (set VALIDATE_BRAIN_STRICT=1 to gate on the whole corpus everywhere, or scripts/sync/validate_brain.sh to see them all)" >&2
      fi
    fi
  else
    # No last-good yet (fresh clone) or no python3: fall back to the tracked baseline TOTAL
    # to decide whether to look further, but still only ever BLOCK on an error this repo
    # itself owns — an over-baseline total caused entirely by another repo's files must not
    # block a repo that introduced nothing of its own.
    if [ "$total" -gt "$baseline" ] && [ -n "$diagnostics" ] && command -v python3 >/dev/null 2>&1; then
      local files blocking_file advisory_file blocking_new advisory_new
      files="$(classify_new_errors "$brain_root" "$this_repo_toplevel" "$staged_files" <<< "$diagnostics")"
      blocking_file="${files%%$'\t'*}"
      advisory_file="${files#*$'\t'}"
      blocking_new="$(cat "$blocking_file" 2>/dev/null)"
      advisory_new="$(cat "$advisory_file" 2>/dev/null)"
      rm -f "$blocking_file" "$advisory_file"

      if [ -n "$advisory_new" ]; then
        echo "" >&2
        echo "$label: NOTICE — corpus error(s) elsewhere in the fleet (not blocking THIS repo's commit):" >&2
        echo "" >&2
        printf '%s\n' "$advisory_new" >&2
      fi

      if [ -n "$blocking_new" ]; then
        echo "" >&2
        echo "$label: BLOCKED — this repo owns error(s) pushing the corpus over baseline:" >&2
        echo "" >&2
        printf '%s\n' "$blocking_new" >&2
        echo "" >&2
        echo "$label: baseline is $baseline error(s); this operation would leave the corpus at $total error(s)." >&2
        echo "$label: fix the new error(s) above, or bypass this check with --no-verify." >&2
        echo "" >&2
        return 1
      fi
    elif [ "$total" -gt "$baseline" ]; then
      # No python3 to classify by owning repo: the original whole-corpus behaviour.
      echo "" >&2
      echo "$label: BLOCKED — validate-brain error count increased (no python3 available to attribute by repo)." >&2
      echo "" >&2
      [ -n "$diagnostics" ] && printf '%s' "$diagnostics" >&2
      echo "" >&2
      echo "$label: baseline is $baseline error(s); this operation would leave the corpus at $total error(s)." >&2
      echo "$label: fix the new error(s) above, or bypass this check with --no-verify." >&2
      echo "" >&2
      return 1
    fi
  fi

  # Passed. Record the current error set as this clone's new high-water mark, so the next
  # gated commit or push is judged against what the corpus looked like when we last let one
  # through.
  if [ -n "$last_good" ] && command -v python3 >/dev/null 2>&1; then
    printf '%s' "$diagnostics" | python3 -c '
import json, sys
path = sys.argv[1]
errors = sorted({ln for ln in sys.stdin.read().splitlines() if ln.strip()})
try:
    with open(path, "w") as f:
        json.dump({"errors": errors}, f, indent=2)
        f.write("\n")
except OSError:
    pass
' "$last_good" 2>/dev/null || true
  fi

  return 0
}
