#!/usr/bin/env bash
# scripts/micro_spec_gate.sh
#
# EN.ticket.micro-spec-fixture-for-engine-seam-comparison task 1 — the mechanism
# that makes ONE task in a micro-spec fail its per-task validation gate on attempt
# 1 and pass on every attempt after that, deterministically, with no model
# judgement involved. This is deliberate: the retry/triage path must be exercised
# on EVERY run of the fixture rather than only when something goes wrong by
# accident (ticket AC 4) — a fixture that only ever goes green on the first try
# never proves the engine can recover from a failed gate.
#
# `rg` is NOT guaranteed on PATH under a non-login `bash -c` in this harness (see
# the note at the top of scripts/verify_move_fidelity.sh), so this script uses
# only bash/grep/git-safe builtins — no `rg` dependency.
#
# THE COUNTER FILE MUST NOT LIVE UNDER `planning/`: `.gitignore` line 7 is
# `/planning`, so a write there is invisible to `git status --porcelain` and
# defeats `TestTaskNode::verify_claimed_writes` — the exact trap documented in
# `planning/smoke-sdlc-flow/tasks.md`. Always pass a counter-file path outside
# `planning/` (the worktree root, or /tmp for ad-hoc testing); the default below
# is worktree-root-relative and disposable, never committed.
#
# Usage: scripts/micro_spec_gate.sh [counter-file-path]
#   counter-file-path defaults to .micro-spec-attempt (relative to invoking cwd,
#   which is the worktree root under /sdlc-flow, per SetupWorktreeNode).
#
# Behavior: read the integer in the counter file (absent = 0), increment it,
# write it back, then exit 1 if the new value is 1, exit 0 otherwise. The
# observed attempt number is printed to stdout on both paths so a harvested log
# says which attempt it was.

set -euo pipefail

counter_file="${1:-.micro-spec-attempt}"

prev=0
if [ -f "$counter_file" ]; then
  prev="$(cat "$counter_file")"
  case "$prev" in
    ''|*[!0-9]*) prev=0 ;;
  esac
fi

attempt=$((prev + 1))
printf '%s\n' "$attempt" > "$counter_file"

if [ "$attempt" -eq 1 ]; then
  echo "micro_spec_gate: attempt $attempt -> FAIL (manufactured, by design)"
  exit 1
fi

echo "micro_spec_gate: attempt $attempt -> PASS"
exit 0
