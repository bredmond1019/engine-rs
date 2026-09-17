#!/usr/bin/env python3
"""check_observed_red.py -- gate for BT.ticket.gates-must-be-observed-red.

WHY THIS EXISTS
----------------
A `gates: true` check in `planning/harness.json` can be wired up, registered, green forever --
and never once have been watched actually fail. That is the exact failure mode the 2026-09-02
pattern analysis (M1) names: seventeen instances across the fleet where a gates:true check
reports clean forever because nothing ever provoked it. A check that only agrees with conforming
input is not evidence the check works; it might just never fire.

WHAT THIS SCRIPT CHECKS
-------------------------
For every check in `<harness>.validation.checks[]` with `gates: true`, an `observed_red` object
must be present with:
  - `date`      a `YYYY-MM-DD` string -- when the check was actually run against known-bad input
                and observed to fail.
  - `evidence`  a non-empty (post-strip) string -- the literal known-bad input, the command run,
                and the real failure output it produced (or, for a check that genuinely cannot be
                provoked, a `CANNOT-PROVOKE:` prefixed account of what was tried).
  - `note`      present as a key (schema requires it); this gate does not additionally require it
                be non-empty -- `evidence` is where the load-bearing proof lives.

A `gates: false` check is never required to carry one.

EMPTY-TRIGGER-SET VERDICT (BT.ticket.a-gated-check-with-an-empty-trigger-set-must-warn)
-----------------------------------------------------------------------------------------
`observed_red` proves a check WAS seen red once, against a fixture. It says nothing about
whether the check CAN go red against today's live corpus -- a check whose trigger set is empty
(it runs, exits 0, and matches nothing real) is indistinguishable, on any board, from a check
that is genuinely passing. This script attaches a COMPANION verdict to the same per-check walk,
for the classes task 1 of this ticket found decidable from `harness.json` plus the live corpus
alone (see `planning/BT.ticket.a-gated-check-with-an-empty-trigger-set-must-warn/
trigger-set-classes.md`):

  - `task-gate-boundaries` (class 1, argument-shape): RULE 3 inside
    `check_task_gate_boundaries.py` only has candidate input when some OTHER `gates:true` check's
    `command` names both a `scripts/*.py` detector and a separate path argument. This script
    reproduces that exact shape test against the live corpus and reports "empty" when zero
    checks qualify.

A check named in `UNDECIDABLE_TRIGGER_SET_CHECKS` (e.g. `engines-parse`, a parser blind spot --
class 2) is NOT silently treated as clean; it gets an explicit `NOTE` line saying the verdict is
out of reach for that class. Every other check gets no trigger-set verdict at all -- silence is
honest here, since this script has no mechanism to decide their class.

THIS VERDICT IS WARNING-FIRST: it is printed as `WARN`/`NOTE`, never `FAIL`, and never changes
`run()`'s exit code. A check can be legitimately inert in one repo (this factory) and load-bearing
downstream (the 18 repos scaffolded from it whose checks DO carry path arguments) -- making this
an error would red-gate the factory for behaving correctly.

SELF-PROBE (the same discipline as check_failure_output_shape.py's SELF_TEST_PROBE)
-------------------------------------------------------------------------------------
Before judging the real target file, this script evaluates a fabricated in-memory check --
`gates: true`, no `observed_red` at all -- through the exact same `evaluate_check()` function
used on real checks. If that fabricated, deliberately-bad check is reported as CONFORMING, the
detector itself is broken and this script aborts with a distinct `GATE BUG:` message rather than
silently agreeing with whatever the real file happens to contain. A checker whose only evidence
is "good input passes" proves nothing -- this is what the block record calls out by name.

A second, analogous self-probe pair does the same for the empty-trigger-set verdict: a
known-empty-by-construction fixture corpus (proving `classify_empty_trigger_set()` CAN report
"empty") and a known-non-empty one (proving it does not just always say "empty"). Both run
through the same `classify_empty_trigger_set()` function used on the real file.

FAIL LINE FORMAT
-----------------
Every violation is printed as `FAIL <path> <human text>`, matching the fleet-wide format gated
by scripts/check_failure_output_shape.py (`docs/harness.md` "The FAIL line format"). `<path>` is
the harness file that was checked (the `--harness` value, or the default), so a parser can name
the artifact that is wrong.

Usage:
    check_observed_red.py [--harness PATH] [--quiet]

    --harness PATH   path to the harness.json to check (default: planning/harness.json,
                     resolved relative to this repo's root)
    --quiet          print only failures and the summary

TRAP (re-confirmed twice in this ticket's source pattern-analysis run): a piped command's exit
code is the pipe's, not this script's. Redirect this script's output to a file and check `$?` on
its own line -- never `check_observed_red.py | tail` and then trust `$?`.

Exit code 1 if the self-probe fails to detect its own known-bad fixture (a GATE BUG), or if any
real gates:true check lacks a valid observed_red record. Exit code 0 otherwise, including a
harness file with zero checks (never a state this repo is actually in, but not this script's
job to opine on).
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Optional

REPO_ROOT = Path(__file__).resolve().parent.parent

DATE_RE = re.compile(r"^\d{4}-\d{2}-\d{2}$")

# The known-bad self-probe: a fabricated gates:true check with NO observed_red at all. This is
# never a real registered check -- it exists only to prove evaluate_check() is capable of
# flagging a violation, not merely of agreeing with input that already conforms. Mirrors
# check_failure_output_shape.py's SELF_TEST_PROBE / _self_test_negative_fixture pattern.
SELF_PROBE_CHECK = {
    "name": "self-probe-known-bad-no-observed-red",
    "command": "false",
    "purpose": "inline known-bad fixture for check_observed_red.py's own self-probe; never a real check",
    "gates": True,
}

# Named registry of checks this script knows how to classify for an empty trigger set, per
# trigger-set-classes.md's decidable classes only (class 1, argument-shape). A check name absent
# from BOTH this dict and UNDECIDABLE_TRIGGER_SET_CHECKS below gets no verdict at all -- this
# script has no mechanism to decide its class, and reporting "clean" would recreate the exact
# bug this ticket exists to catch, one level up.
#
# Class 3 (escalations-schema, foreign-path emptiness) is also decidable per trigger-set-classes.md,
# but escalations-schema is registered `gates: false` in this repo's live harness.json (it was
# already ungated for an unrelated reason), so it never reaches this script's gates:true walk --
# there is nothing to classify today. If it is ever re-gated, add its registry entry here.

# Checks whose emptiness class task 1 explicitly found NOT decidable from harness.json plus the
# live corpus alone -- named so the output can say "out of reach" instead of staying silent, per
# trigger-set-classes.md's warning that silent coverage of only SOME classes recreates this bug.
UNDECIDABLE_TRIGGER_SET_CHECKS = {
    "engines-parse": (
        "class 2, parser blind spot: node --check exits 0 on a syntax error placed after a "
        "file's first top-level export in this repo's Node runtime, so whether that blind spot "
        "is active is a fact about the Node parser, not about the shape of harness.json or the "
        "corpus text -- see trigger-set-classes.md class 2"
    ),
}


def _argument_shape(command: str) -> tuple[Optional[str], list[str]]:
    """Reproduce check_task_gate_boundaries.py's RULE 3 argument-shape parse verbatim: split
    `command` on whitespace and return (the first `scripts/*.py`-shaped token, every OTHER
    slash-containing token). A check whose command names no detector script, or names one but no
    separate path argument, has an empty result in the second element."""
    script_path: Optional[str] = None
    input_paths: list[str] = []
    for tok in command.split():
        if tok.startswith("-") or "/" not in tok:
            continue
        if script_path is None and tok.startswith("scripts/") and tok.endswith(".py"):
            script_path = tok
        elif tok != script_path:
            input_paths.append(tok)
    return script_path, input_paths


def classify_empty_trigger_set(
    check: dict, all_checks: list[dict],
) -> Optional[tuple[str, int, int, str]]:
    """Return (verdict, matched, total, detail) for a `gates:true` check this script knows how
    to classify, or None if `check`'s name is outside every class task 1 found decidable.

    `verdict` is "empty" (the check ran, matched nothing, and therefore proved nothing) or
    "non-empty" (real input was matched). `matched`/`total` are populated in BOTH cases so an
    empty result can be told apart, in the output, from a check that never ran at all.
    """
    name = check.get("name")
    if name == "task-gate-boundaries":
        # Class 1 (argument-shape): count how many gates:true checks in `all_checks` name both a
        # scripts/*.py detector and a separate path argument -- the exact shape RULE 3 (inside
        # check_task_gate_boundaries.py) needs in order to have anything to examine at all.
        total = sum(1 for c in all_checks if isinstance(c, dict) and c.get("gates"))
        matched = 0
        for c in all_checks:
            if not isinstance(c, dict) or not c.get("gates"):
                continue
            command = c.get("command")
            if not isinstance(command, str):
                continue
            script_path, input_paths = _argument_shape(command)
            if script_path and input_paths:
                matched += 1
        verdict = "empty" if matched == 0 else "non-empty"
        detail = (
            f"argument-shape (RULE 3 in check_task_gate_boundaries.py): {matched} of {total} "
            f"gates:true check(s) name both a detector script and a separate path argument"
        )
        return verdict, matched, total, detail
    return None


# Self-probes for the empty-trigger-set verdict: a fabricated `task-gate-boundaries`-named check
# judged against two fabricated fixture corpora, never the real harness.json. Neither is a real
# registered check -- each exists only to prove classify_empty_trigger_set() is CAPABLE of
# reporting both verdicts, not merely capable of agreeing with whatever the real file contains.
SELF_PROBE_TRIGGER_CHECK = {
    "name": "task-gate-boundaries",
    "command": "python3 scripts/check_task_gate_boundaries.py",
    "purpose": "inline fixture for check_observed_red.py's empty-trigger-set self-probe; never a real check",
    "gates": True,
}
# Known-empty-by-construction: no check in this fabricated corpus names a separate path argument.
SELF_PROBE_EMPTY_TRIGGER_CORPUS = [
    SELF_PROBE_TRIGGER_CHECK,
    {"name": "self-probe-other", "command": "python3 scripts/self_probe_other.py --quiet", "gates": True},
]
# Known-non-empty-by-construction: one check in this fabricated corpus DOES name a separate path
# argument, so the same detector logic must report "non-empty" here, not just "empty" always.
SELF_PROBE_NONEMPTY_TRIGGER_CORPUS = [
    SELF_PROBE_TRIGGER_CHECK,
    {
        "name": "self-probe-with-input",
        "command": "python3 scripts/self_probe_other.py fixtures/self_probe_input.png",
        "gates": True,
    },
]


def evaluate_check(check: dict) -> Optional[str]:
    """Return None if `check` conforms (gates:false, or gates:true with a valid observed_red).
    Otherwise return a human-readable detail string naming why it does not."""
    if not check.get("gates"):
        return None

    name = check.get("name", "<unnamed>")
    observed_red = check.get("observed_red")

    if observed_red is None:
        return f"check `{name}` (gates:true) has no observed_red record"
    if not isinstance(observed_red, dict):
        return f"check `{name}`'s observed_red is not an object"

    date = observed_red.get("date")
    if not isinstance(date, str) or not DATE_RE.fullmatch(date):
        return f"check `{name}`'s observed_red.date `{date!r}` is not YYYY-MM-DD"

    evidence = observed_red.get("evidence")
    if not isinstance(evidence, str) or not evidence.strip():
        return f"check `{name}`'s observed_red.evidence is empty or whitespace"

    return None


def evaluate_checks(checks: list[dict]) -> list[str]:
    """Return the list of failure-detail strings for every non-conforming gates:true check in
    `checks`, in order. Empty list means every gates:true check conforms."""
    details = []
    for check in checks:
        detail = evaluate_check(check)
        if detail is not None:
            details.append(detail)
    return details


def run(harness_path: Path, quiet: bool) -> int:
    # Self-probe first: this must run and be flagged as non-conforming before the real file is
    # ever judged. A detector that cannot detect its own known-bad fixture cannot be trusted to
    # judge anything else.
    self_probe_detail = evaluate_check(SELF_PROBE_CHECK)
    if self_probe_detail is None:
        print(
            "GATE BUG: self-probe-known-bad-no-observed-red was reported as CONFORMING, but it "
            "is a deliberately non-conforming fixture (gates:true, no observed_red at all) -- "
            "the detector cannot be trusted",
        )
        return 1
    if not quiet:
        print(f"ok   self-probe: correctly detected as non-conforming ({self_probe_detail})")

    # Empty-trigger-set self-probes, same discipline: prove classify_empty_trigger_set() can
    # report BOTH verdicts before trusting it to judge the real file. A verdict function that
    # always says "empty" (or always says "non-empty") would pass a single-sided self-probe.
    empty_probe = classify_empty_trigger_set(SELF_PROBE_TRIGGER_CHECK, SELF_PROBE_EMPTY_TRIGGER_CORPUS)
    if empty_probe is None or empty_probe[0] != "empty":
        print(
            "GATE BUG: self-probe-known-empty-trigger-set was not reported as empty against its "
            f"known-empty-by-construction fixture corpus (got {empty_probe!r}) -- the "
            "empty-trigger-set verdict cannot be trusted",
        )
        return 1
    nonempty_probe = classify_empty_trigger_set(
        SELF_PROBE_TRIGGER_CHECK, SELF_PROBE_NONEMPTY_TRIGGER_CORPUS,
    )
    if nonempty_probe is None or nonempty_probe[0] != "non-empty":
        print(
            "GATE BUG: self-probe-known-non-empty-trigger-set was not reported as non-empty "
            f"against its known-non-empty-by-construction fixture corpus (got {nonempty_probe!r}) "
            "-- the empty-trigger-set verdict always says \"empty\" and cannot be trusted",
        )
        return 1
    if not quiet:
        print(
            "ok   self-probe: empty-trigger-set verdict correctly reports both empty "
            f"({empty_probe[3]}) and non-empty ({nonempty_probe[3]}) on fabricated fixtures",
        )

    try:
        data = json.loads(harness_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        print(f"FAIL {harness_path} could not be read/parsed: {exc}")
        return 1

    checks = data.get("validation", {}).get("checks", [])
    failures: list[str] = []
    for check in checks:
        detail = evaluate_check(check)
        name = check.get("name", "<unnamed>")
        if detail is None:
            if not quiet and check.get("gates"):
                print(f"ok   {name}: carries a valid observed_red record")
        else:
            print(f"FAIL {harness_path} {detail}")
            failures.append(detail)

        # Companion empty-trigger-set verdict -- warning-first, never affects `failures`/exit
        # code. Runs for every gates:true check regardless of its observed_red verdict above:
        # the two questions ("was this ever seen red on a fixture" and "can this fire on today's
        # real corpus") are independent, per this ticket's `why`.
        if check.get("gates"):
            trigger = classify_empty_trigger_set(check, checks)
            if trigger is not None:
                verdict, matched, total, tdetail = trigger
                if verdict == "empty":
                    print(
                        f"WARN {name}: gates:true check ran and matched {matched} of {total} -- "
                        f"trigger set is empty, it proved nothing this run ({tdetail})",
                    )
                elif not quiet:
                    print(
                        f"ok   {name}: trigger set non-empty (matched {matched} of {total}) -- "
                        f"{tdetail}",
                    )
            elif name in UNDECIDABLE_TRIGGER_SET_CHECKS:
                print(
                    f"NOTE {name}: empty-trigger-set verdict is OUT OF REACH for this check -- "
                    f"{UNDECIDABLE_TRIGGER_SET_CHECKS[name]}",
                )

    gated_count = sum(1 for c in checks if c.get("gates"))
    if failures:
        print(
            f"\n{len(failures)} of {gated_count} gates:true check(s) lack a valid observed_red "
            f"record",
        )
        return 1

    print(f"\nall {gated_count} gates:true check(s) carry a valid observed_red record")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                  formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--harness", default=str(REPO_ROOT / "planning" / "harness.json"),
                     help="path to the harness.json to check (default: planning/harness.json)")
    ap.add_argument("--quiet", action="store_true", help="print only failures and the summary")
    args = ap.parse_args()
    return run(Path(args.harness), args.quiet)


if __name__ == "__main__":
    sys.exit(main())
