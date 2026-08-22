#!/usr/bin/env python3
"""Validate block records against block.schema.json (D65).

Dependency-free on purpose: `jsonschema` is not installed anywhere in this fleet, so a
validator that imports it validates nothing and reports success. This checks the
constraints that actually matter — required fields, the ID grammar, the ID/filename/
spec_dir agreement, enums, date formats, and the dependency edge shapes — by hand.

It is the interim gate until mev's W_BLOCK_* checks ship
(MV.ticket.block-record-validation), and it stays useful afterwards as base-template's
own local check.

Usage:
    check_block_records.py [--planning DIR] [--fleet] [--quiet]

    --planning DIR   validate one repo's planning/blocks/ (default: planning)
    --fleet          walk every _planning/<repo>/blocks/ under the brain root, plus the
                     brain root's own planning/blocks/
    --quiet          print only failures and the summary

Exit code 1 if any record fails. A repo with no blocks/ directory is not a failure —
that is the majority state during the D65 backfill and must stay silent.
"""

import argparse
import json
import os
import re
import sys

ID_RE = re.compile(r"^[A-Z]{2,4}\.(?:\d+[A-Z]?|ticket|chore)\.[A-Za-z0-9][A-Za-z0-9._-]*$")
DATE_RE = re.compile(r"^\d{4}-\d{2}-\d{2}$")
COMMIT_RE = re.compile(r"^[0-9a-f]{7,40}$")
DIGEST_RE = re.compile(r"^[a-z0-9]+:[0-9a-f]+$")
OPERATOR_SLUG_RE = re.compile(r"^operator-[a-z0-9][a-z0-9-]*$")

REQUIRED = ["id", "repo", "kind", "title", "description", "what", "why",
            "sdlc_workflow", "model", "out_of_scope",
            "acceptance_criteria", "spec_dir", "created", "updated"]

# `files` is load-bearing -- it is how /generate-tasks derives disjoint task ownership
# without guessing -- but it is a WARNING, not an error. Many blocks predate D65 and their
# master-plan sections never named paths; hard-requiring it during the backfill would force
# an agent to either invent file paths or refuse to write the record at all, and inventing
# is the precise failure D65 exists to end. A warning surfaces the debt without buying it
# back with fabrication. Same reasoning as mev's W_BLOCK_* codes shipping warning-first.
WARN_IF_MISSING = ["files", "validation_commands"]
KINDS = {"block", "ticket", "chore"}
WORKFLOWS = {"none", "patch", "task", "run", "flow"}
MODELS = {"sonnet", "gemini-pro", "gemini-flash", "either"}


def check(path, planning_root="planning"):
    """Return (errors, warnings) for one record file."""
    problems = []
    warnings = []

    def bad(msg):
        problems.append(msg)

    def warn(msg):
        warnings.append(msg)

    try:
        with open(path) as fh:
            b = json.load(fh)
    except Exception as exc:                      # noqa: BLE001 - report, never raise
        return [f"does not parse: {exc}"], []

    if not isinstance(b, dict):
        return ["top level must be an object"], []

    for field in REQUIRED:
        v = b.get(field)
        if v is None or (isinstance(v, (str, list, dict)) and len(v) == 0):
            bad(f"required field `{field}` is missing or empty")

    for field in WARN_IF_MISSING:
        v = b.get(field)
        if v is None or (isinstance(v, (str, list, dict)) and len(v) == 0):
            warn(f"`{field}` is empty — backfill debt, not a blocker")

    bid = b.get("id", "")
    if bid and not ID_RE.match(bid):
        bad(f"id `{bid}` does not match <PFX>.<phase|ticket|chore>.<name>")

    stem = os.path.splitext(os.path.basename(path))[0]
    if bid and stem != bid:
        bad(f"filename stem `{stem}` != id `{bid}`")

    # spec_dir: the question that matters is "does this point at a real spec", not "is the
    # directory name canonical". `/generate-tasks` step 2 is explicit that LEGACY directories
    # (`<phase>.<block>-<title>`, `chore-<slug>`, ...) still resolve and do not require migrating,
    # and this checker used to contradict that by hard-failing every one of them. It cost a real
    # run: HQ.9.A is a CLOSED block whose spec is `planning/chore-fleet-parking-pass/plan.md`, and
    # renaming that directory to satisfy the checker would have broken nine live citations across
    # two repos — including two that cite `plan.md:146` by line as the operator approval for 59
    # promoted rows. Rewriting closed history to make a checker green is the wrong trade.
    #
    # So: a spec_dir that does not exist on disk is a real defect (a pointer to nothing) and stays
    # an ERROR. A spec_dir that exists but is not the canonical `planning/<id>/` is a WARNING —
    # backfill debt, the same class as an empty `files`.
    spec = b.get("spec_dir", "")
    if bid and spec:
        canonical = f"planning/{bid}/"
        spec_abs = os.path.join(planning_root, os.path.relpath(spec, "planning")) \
            if spec.startswith("planning/") else os.path.join(planning_root, spec)
        if spec != canonical:
            if os.path.isdir(spec_abs):
                warn(f"spec_dir `{spec}` is legacy-named, not `{canonical}` — resolves, so not a "
                     f"blocker; migrate only if the block is still open")
            else:
                bad(f"spec_dir `{spec}` does not exist and is not `{canonical}`")
        elif not os.path.isdir(spec_abs):
            # Canonical but absent is the normal state of a block whose spec has not been
            # generated yet — `/generate-tasks` creates the directory. Only a NON-canonical
            # path that also does not resolve is a genuine dangling pointer, handled above.
            warn(f"spec_dir `{spec}` does not exist yet — run /generate-tasks {bid}")

    if b.get("kind") not in KINDS:
        bad(f"kind `{b.get('kind')}` not one of {sorted(KINDS)}")
    if b.get("sdlc_workflow") not in WORKFLOWS:
        bad(f"sdlc_workflow `{b.get('sdlc_workflow')}` not one of {sorted(WORKFLOWS)}")
    if b.get("model") not in MODELS:
        bad(f"model `{b.get('model')}` not one of {sorted(MODELS)}")

    if b.get("kind") == "block" and b.get("phase") is None:
        bad("kind `block` requires `phase`")
    if b.get("kind") == "ticket" and not b.get("testing_strategy"):
        bad("kind `ticket` requires a non-empty `testing_strategy`")

    for field in ("created", "updated", "closed"):
        v = b.get(field)
        if v is not None and not DATE_RE.match(str(v)):
            bad(f"{field} `{v}` is not YYYY-MM-DD")
    if b.get("commit") is not None and not COMMIT_RE.match(str(b["commit"])):
        bad(f"commit `{b['commit']}` is not a hex git hash")

    files = b.get("files")
    if isinstance(files, dict):
        if not files.get("new") and not files.get("modified"):
            warn("files names neither a new nor a modified path")
        for key, req in (("new", "purpose"), ("modified", "change")):
            for i, f in enumerate(files.get(key) or []):
                if not isinstance(f, dict) or not f.get("path") or not f.get(req):
                    bad(f"files.{key}[{i}] needs both `path` and `{req}`")
    elif files is not None:
        bad("files must be an object with `new` / `modified`")

    for i, c in enumerate(b.get("acceptance_criteria") or []):
        if isinstance(c, str):
            continue
        if not isinstance(c, dict) or not c.get("criterion"):
            bad(f"acceptance_criteria[{i}] must be a string or carry `criterion`")
        elif c.get("gateable") is False and not c.get("evidence"):
            # D64: an un-gateable criterion with no fixture is the failure the rule exists
            # to catch -- it reads as verified while nothing observes it.
            bad(f"acceptance_criteria[{i}] is gateable:false but names no `evidence`")

    for i, e in enumerate(b.get("depends_on") or []):
        if not isinstance(e, dict):
            bad(f"depends_on[{i}] must be an object")
            continue
        t = e.get("type")
        if t == "block":
            if not e.get("repo") or not e.get("id"):
                bad(f"depends_on[{i}] block edge needs `repo` and `id`")
        elif t == "external":
            if not e.get("what"):
                bad(f"depends_on[{i}] external edge needs `what`")
        elif t == "operator":
            for k in ("slug", "exit", "start"):
                if not e.get(k):
                    bad(f"depends_on[{i}] operator edge needs `{k}`")
            if e.get("slug") and not OPERATOR_SLUG_RE.match(e["slug"]):
                bad(f"depends_on[{i}] operator slug `{e['slug']}` must be kebab-case, "
                    f"prefixed `operator-`")
        elif t == "approval":
            for k in ("slug", "what", "digest"):
                if not e.get(k):
                    bad(f"depends_on[{i}] approval edge needs `{k}`")
            if e.get("digest") and not DIGEST_RE.match(e["digest"]):
                bad(f"depends_on[{i}] digest `{e['digest']}` must be <algo>:<hex>")
        else:
            bad(f"depends_on[{i}] unknown type `{t}`")

    return problems, warnings


def blocks_dirs(fleet, planning):
    if not fleet:
        return [os.path.join(planning, "blocks")]
    root = os.getcwd()
    found = []
    for dirpath, dirnames, _ in os.walk(root, followlinks=False):
        dirnames[:] = [d for d in dirnames
                       if d not in {"node_modules", ".git", "archive", "target"}]
        if os.path.basename(dirpath) == "blocks" and (
                "_planning" in dirpath.split(os.sep)
                or dirpath.endswith(os.path.join("planning", "blocks"))):
            found.append(dirpath)
    return sorted(set(found))


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--planning", default="planning")
    ap.add_argument("--fleet", action="store_true")
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args()

    total = failed = warned = 0
    for d in blocks_dirs(args.fleet, args.planning):
        if not os.path.isdir(d):
            continue
        for name in sorted(os.listdir(d)):
            if not name.endswith(".json"):
                continue
            path = os.path.join(d, name)
            total += 1
            # `d` is the blocks/ dir; the repo's planning root is its parent. spec_dir values are
            # written repo-relative as `planning/<...>/`, so resolving them needs that root, not cwd.
            problems, warnings = check(path, planning_root=os.path.dirname(d))
            if problems:
                failed += 1
                print(f"FAIL {path}")
                for p in problems:
                    print(f"       {p}")
            elif warnings:
                warned += 1
                if not args.quiet:
                    print(f"warn {path}")
            elif not args.quiet:
                print(f"ok   {path}")
            for w in warnings:
                if problems or not args.quiet:
                    print(f"       (warn) {w}")

    if total == 0:
        print("no block records found (not a failure)")
        return 0
    print(f"\n{total} record(s) checked, {failed} failed, {warned} with warnings")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
