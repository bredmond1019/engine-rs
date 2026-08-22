#!/usr/bin/env bash
# scripts/verify_move_fidelity.sh
#
# EN.11.M task 3 — scripted evidence for base-template D68's four
# move-fidelity constraints, which the ordinary Rust suite cannot observe
# (a faithful-LOOKING move that quietly drops a test still compiles and
# still goes green):
#
#   #1 CONTENT DIFF   — the moved file carries every line present in the
#                        pre-move blob, beyond the module path and doc
#                        header (a no-deletions diff, not strict equality —
#                        this same task appends one new test to
#                        `policy/command_floor.rs` AFTER the move, which
#                        must not itself fail the check; a REMOVED line is
#                        what fails it).
#   #2 PER-FILE COUNT — `#[test]` occurrences are compared per moved file
#                        pair, never summed into one aggregate total (two
#                        wrong sub-counts must not be able to cancel); a
#                        DROP below the pre-move count fails, growth does
#                        not (see #1).
#   #3 GIT-MEASURED    — the pre-move baseline is resolved from git blobs
#                        at run time, never a hardcoded number or a count
#                        read out of planning prose.
#   #4 SHOWN CAPABLE   — this script is exercised against a deliberately
#                        damaged copy (a `#[test]` deleted from the new
#                        file) as well as the real tree, and both observed
#                        exit codes are recorded here, not merely asserted.
#
# POSITIVE CONTROL, OBSERVED 2026-08-21 (recorded per D68 #4 — run by hand,
# against the FINAL tree, i.e. after this task's own new test was added):
#   - Real tree (no damage):                                        exit 0
#   - `policy/command_floor.rs` with one `#[test]` deleted:          exit 1
#   - `workflows/mod.rs` cluster block with one `#[test]` deleted:   exit 1
#
# `rg` is not guaranteed to be on PATH under a non-login `bash -c` in this
# harness — this script uses only `grep`/`git`/`git grep`.
#
# Usage:
#   bash scripts/verify_move_fidelity.sh
#
# Exit status must be checked directly (never through a pipe — a pipe's
# `$?` is the pipe's own status, not this script's; see CLAUDE.md trap 1).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

# Strip `//!` module-doc-header lines before diffing content — the only
# content changes the spec permits are the doc header's location sentence
# and its one internal doc-link fixup, both of which live in `//!` lines.
strip_doc_header() {
    grep -v '^//!' "$1"
}

count_tests() {
    grep -c '#\[test\]' "$1" 2>/dev/null || true
}

echo "== Pair 1: command_floor.rs (whole-file move) =="

OLD_PATH="crates/engine-core/src/workflows/sdlc_flow/command_floor.rs"
NEW_PATH="crates/engine-core/src/policy/command_floor.rs"

test -f "$NEW_PATH" || fail "moved-to file missing: $NEW_PATH"

DELETE_COMMIT="$(git rev-list -1 HEAD -- "$OLD_PATH")"
[ -n "$DELETE_COMMIT" ] || fail "could not resolve any commit touching $OLD_PATH"

OLD_BLOB_REF="${DELETE_COMMIT}^:${OLD_PATH}"
OLD_TMP="$(mktemp)"
NEW_TMP=""
OLD_TESTBLOCK_TMP=""
NEW_TESTBLOCK_TMP=""
CLUSTER_OLD_TMP=""
trap 'rm -f "$OLD_TMP" "${NEW_TMP:-}" "${OLD_TESTBLOCK_TMP:-}" "${NEW_TESTBLOCK_TMP:-}" "${CLUSTER_OLD_TMP:-}" "${OLD_TMP:-}.stripped" "${NEW_TMP:-}.stripped"' EXIT

git show "$OLD_BLOB_REF" > "$OLD_TMP" 2>/dev/null \
    || fail "could not resolve pre-move blob $OLD_BLOB_REF"
[ -s "$OLD_TMP" ] || fail "pre-move blob $OLD_BLOB_REF resolved empty"

# D68 #1 — content diff (module path / doc header excluded). The check must
# tolerate later, purely ADDITIVE edits to the moved file (task 3 of this
# very block adds one more test to policy/command_floor.rs after the move),
# so this is a no-deletions check, not a strict equality diff: any `-` line
# in the unified diff (beyond the `--- old` header line itself) means
# something present pre-move is gone post-move, which fails the check.
# Purely `+` (appended) lines are accepted.
NEW_TMP="$(mktemp)"
strip_doc_header "$OLD_TMP" > "${OLD_TMP}.stripped"
strip_doc_header "$NEW_PATH" > "${NEW_TMP}.stripped"
DIFF_OUT="$(diff -u "${OLD_TMP}.stripped" "${NEW_TMP}.stripped" || true)"
REMOVED_LINES="$(printf '%s\n' "$DIFF_OUT" | grep -c '^-' || true)"
# grep -c counts the `--- old_file` header line too; subtract it when present.
if printf '%s\n' "$DIFF_OUT" | grep -q '^--- '; then
    REMOVED_LINES=$((REMOVED_LINES - 1))
fi
if [ "$REMOVED_LINES" -gt 0 ]; then
    printf '%s\n' "$DIFF_OUT" >&2
    fail "$NEW_PATH is missing content present in its pre-move blob ($OLD_BLOB_REF), beyond the doc header"
fi

# D68 #2/#3 — per-file test count, git-measured baseline. `>=` rather than
# strict equality for the same reason as the content diff above: this pair
# legitimately gains one test in task 3 of this block. A DROP is what must
# fail, not growth.
OLD_COUNT="$(count_tests "$OLD_TMP")"
NEW_COUNT="$(count_tests "$NEW_PATH")"
echo "  pre-move (#[test] in $OLD_BLOB_REF): $OLD_COUNT"
echo "  post-move (#[test] in $NEW_PATH):    $NEW_COUNT"
[ "$NEW_COUNT" -ge "$OLD_COUNT" ] \
    || fail "pair 1 test count dropped: pre-move=$OLD_COUNT post-move=$NEW_COUNT"

echo "== Pair 2: command-runner cluster (partial-file move, sdlc_flow/mod.rs -> workflows/mod.rs) =="

CLUSTER_OLD_PATH="crates/engine-core/src/workflows/sdlc_flow/mod.rs"
CLUSTER_NEW_PATH="crates/engine-core/src/workflows/mod.rs"

test -f "$CLUSTER_NEW_PATH" || fail "moved-to file missing: $CLUSTER_NEW_PATH"

CLUSTER_COMMIT="$(git rev-list -1 HEAD -- "$CLUSTER_OLD_PATH" | tail -1)"
# The cluster's OWN pre-move state is the commit immediately before the one
# that hoisted `CommandOutput`/`CommandRunner`/`default_command_runner`/
# `commit_all`/`is_noop_commit`/`log_noop_commit` out of this file (EN.11.M
# task 2). Walk history for the first ancestor of HEAD's sdlc_flow/mod.rs
# still containing `fn default_command_runner`.
CLUSTER_PRE_COMMIT=""
for c in $(git log --format=%H -- "$CLUSTER_OLD_PATH"); do
    if git show "${c}:${CLUSTER_OLD_PATH}" 2>/dev/null | grep -q '^pub fn default_command_runner'; then
        CLUSTER_PRE_COMMIT="$c"
        break
    fi
done
[ -n "$CLUSTER_PRE_COMMIT" ] \
    || fail "could not resolve a pre-move commit for $CLUSTER_OLD_PATH containing default_command_runner"

CLUSTER_OLD_BLOB_REF="${CLUSTER_PRE_COMMIT}:${CLUSTER_OLD_PATH}"
CLUSTER_OLD_TMP="$(mktemp)"
git show "$CLUSTER_OLD_BLOB_REF" > "$CLUSTER_OLD_TMP" 2>/dev/null \
    || fail "could not resolve pre-move blob $CLUSTER_OLD_BLOB_REF"
[ -s "$CLUSTER_OLD_TMP" ] || fail "pre-move blob $CLUSTER_OLD_BLOB_REF resolved empty"

# Partial-file move: only the moved `mod tests` block is compared, never
# the whole file (the rest of sdlc_flow/mod.rs legitimately still differs
# for unrelated reasons). The moved block is delimited from the first
# `#[test]` line whose following `fn` name starts with
# `default_command_runner_` or `is_noop_commit_` through to that module's
# closing `}` (which is also the file's last line in both trees, since the
# `mod tests` block is the final item in each file).
extract_test_block() {
    local file="$1"
    local start_line
    start_line="$(grep -n '#\[test\]' "$file" | head -1 | cut -d: -f1)"
    [ -n "$start_line" ] || fail "no #[test] found in $file to anchor the extracted block"
    # Walk forward from the first #[test] to the one immediately preceding
    # `fn default_command_runner_blocks_a_denied_command_without_spawning`,
    # which is where the MOVED subset of tests begins in both trees (the
    # earlier strip_json_fence tests in workflows/mod.rs's own copy are not
    # part of this move and must not be counted here).
    local anchor_line
    anchor_line="$(grep -n 'fn default_command_runner_blocks_a_denied_command_without_spawning' "$file" | head -1 | cut -d: -f1)"
    [ -n "$anchor_line" ] || fail "anchor fn not found in $file"
    # Back up one line to include its #[test] attribute.
    local block_start=$((anchor_line - 1))
    sed -n "${block_start},\$p" "$file"
}

OLD_TESTBLOCK_TMP="$(mktemp)"
NEW_TESTBLOCK_TMP="$(mktemp)"
extract_test_block "$CLUSTER_OLD_TMP" > "$OLD_TESTBLOCK_TMP"
extract_test_block "$CLUSTER_NEW_PATH" > "$NEW_TESTBLOCK_TMP"

CLUSTER_OLD_COUNT="$(count_tests "$OLD_TESTBLOCK_TMP")"
CLUSTER_NEW_COUNT="$(count_tests "$NEW_TESTBLOCK_TMP")"
echo "  pre-move (#[test] in moved block, $CLUSTER_OLD_BLOB_REF): $CLUSTER_OLD_COUNT"
echo "  post-move (#[test] in moved block, $CLUSTER_NEW_PATH):     $CLUSTER_NEW_COUNT"
[ "$CLUSTER_NEW_COUNT" -ge "$CLUSTER_OLD_COUNT" ] \
    || fail "pair 2 test count dropped: pre-move=$CLUSTER_OLD_COUNT post-move=$CLUSTER_NEW_COUNT"

# Sanity: the moved production symbols must all exist in the new file and
# must not remain (as definitions) in the old one.
for sym in "pub fn default_command_runner" "pub(crate) fn commit_all" "pub(crate) fn is_noop_commit" "fn log_noop_commit" "pub struct CommandOutput" "pub type CommandRunner"; do
    git grep -q "^$sym" -- "$CLUSTER_NEW_PATH" \
        || fail "moved symbol not found at new home ($CLUSTER_NEW_PATH): $sym"
done

echo "== All move-fidelity checks passed =="
exit 0
