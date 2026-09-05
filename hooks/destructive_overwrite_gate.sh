#!/usr/bin/env bash
#
# destructive_overwrite_gate.sh — shared "does this rewrite silently drop open work"
# check for hooks/pre-commit gate 3.
#
# Background and the measurement this implements:
# planning/HQ.chore.a-write-to-an-existing-corpus-path-is-unguarded/analysis.md, Q2. Raw
# deletion size/ratio was tested first and RULED OUT — a legitimate full handoff.md rewrite
# (commit feb34b89f, 90.6% deletion ratio) deletes MORE proportionally than the verified
# destructive commit (28b62274f, 94.2%), so no size threshold separates them. The signal
# that does separate every case tested (see analysis.md for the full commands and output):
# a small, greppable status-marker vocabulary (BLOCKER / STILL OPEN / UNRESOLVED) present in
# the file's prior content and gone from its new content, at the SAME path (no rename
# detected), UNLESS the same change also adds content to planning/knowledge.md,
# planning/memory.md, or an archive/-prefixed path — the D35-distillation / `/archive`
# compensating-write pattern, which is how legitimate large deletions in this corpus record
# where the content went instead of just discarding it.
#
# One function, sourced by hooks/pre-commit (staged vs. HEAD) and callable standalone
# against any two historical refs (used to observe the gate against real past commits
# without touching the working tree — see analysis.md's "Task 2" section for that run).
#
# check_destructive_overwrite <old-treeish> <new-treeish-or-empty>
#   old-treeish: a commit/ref content is compared FROM (usually HEAD, or "<sha>^").
#   new-treeish: a commit/ref content is compared TO, or the empty string "" to mean
#                "the staged index" (git's `:<path>` blob) — the live pre-commit case.
# Returns 0 if nothing destructive is found, 1 (and prints one line per offending file to
# stderr) if at least one file lost every marker it had with no compensating write.
check_destructive_overwrite() {
  local old_ref="$1" new_ref="$2"
  local marker_re='BLOCKER|STILL OPEN|UNRESOLVED'
  local diff_files
  if [ -z "$new_ref" ]; then
    diff_files="$(git diff --cached --name-only --diff-filter=M -M -- '*.md' 2>/dev/null)"
  else
    diff_files="$(git diff --name-only --diff-filter=M -M "$old_ref" "$new_ref" -- '*.md' 2>/dev/null)"
  fi
  [ -z "$diff_files" ] && return 0

  local hit=0
  while IFS= read -r f; do
    [ -z "$f" ] && continue
    local old_content new_content old_hits new_hits
    old_content="$(git show "${old_ref}:$f" 2>/dev/null)"
    if [ -z "$new_ref" ]; then
      new_content="$(git show ":$f" 2>/dev/null)"
    else
      new_content="$(git show "${new_ref}:$f" 2>/dev/null)"
    fi
    old_hits="$(printf '%s\n' "$old_content" | grep -icE "$marker_re")"
    new_hits="$(printf '%s\n' "$new_content" | grep -icE "$marker_re")"
    old_hits="${old_hits:-0}"
    new_hits="${new_hits:-0}"

    if [ "$old_hits" -gt 0 ] && [ "$new_hits" -eq 0 ]; then
      # Candidate destructive overwrite. Check for a same-change compensating write —
      # the D35 / /archive pattern (analysis.md Q2, commit 7971e2fb6).
      local comp comp_hit=0
      if [ -z "$new_ref" ]; then
        comp="$(git diff --cached --numstat -- 'planning/knowledge.md' 'planning/memory.md' '*archive/*' 2>/dev/null)"
      else
        comp="$(git diff --numstat "$old_ref" "$new_ref" -- 'planning/knowledge.md' 'planning/memory.md' '*archive/*' 2>/dev/null)"
      fi
      while IFS=$'\t' read -r add _del cpath; do
        [ -z "$cpath" ] && continue
        case "$add" in
          ''|*[!0-9]*) continue ;;
        esac
        if [ "$add" -gt 0 ]; then
          comp_hit=1
        fi
      done <<< "$comp"

      if [ "$comp_hit" -eq 0 ]; then
        echo "destructive-overwrite: $f loses status marker(s) matching /${marker_re}/i present at '${old_ref}' with no compensating write to planning/knowledge.md, planning/memory.md, or an archive/ path in this change." >&2
        hit=1
      fi
    fi
  done <<< "$diff_files"

  return "$hit"
}
