#!/usr/bin/env python3
"""Render a spec's tasks.md from its block record (D65 stage 1).

The block record at planning/blocks/<BlockID>.json is the authored source of truth.
tasks.md is a generated view of it, kept on disk only because the SDLC engines read it
as the spec document (sdlc-task.js sets specFile = <blockDir>/tasks.md and feeds it to
the implement, test and review stages). Once the engines read block.json directly
(D65 stage 2), this script and the file it writes both go away.

Never hand-edit a rendered tasks.md — edit the block record and re-render.

The one exception is the Amendment Log: pipeline stages append to it mid-run, so an
existing Amendment Log section in the target file is preserved verbatim rather than
overwritten.

Usage:
    render_spec.py <BlockID> [--planning DIR] [--check]
    render_spec.py --all [--planning DIR] [--check]

--check renders to memory and exits non-zero if the file on disk differs, without
writing. Use it as a gate: a drifted tasks.md means someone hand-edited a generated
file.
"""

import argparse
import json
import os
import re
import sys

GENERATED_BANNER = (
    "<!-- GENERATED from planning/blocks/{id}.json by scripts/render_spec.py — "
    "do not hand-edit. Edit the block record and re-render. -->"
)

AMENDMENT_HEADING = "## Amendment Log"


def _bullets(items):
    if not items:
        return "_None._"
    return "\n".join(f"- {i}" for i in items)


def _criteria(items):
    """Acceptance criteria accept either a bare string or the D64 object form."""
    if not items:
        return "_None._"
    out = []
    for c in items:
        if isinstance(c, str):
            out.append(f"- {c}")
        else:
            text = c.get("criterion", "")
            if c.get("gateable", True):
                out.append(f"- {text}")
            else:
                ev = c.get("evidence", "").strip()
                suffix = f" Evidence: {ev}" if ev else ""
                out.append(f"- {text}  \n  **UN-GATEABLE (D64)** — no in-repo check observes "
                           f"this.{suffix}")
    return "\n".join(out)


def _files(files):
    new = files.get("new") or []
    mod = files.get("modified") or []
    if not new and not mod:
        return "_None named._"
    lines = []
    if new:
        lines.append("**New**\n")
        lines += [f"- `{f['path']}` — {f['purpose']}" for f in new]
    if mod:
        if lines:
            lines.append("")
        lines.append("**Modified**\n")
        lines += [f"- `{f['path']}` — {f['change']}" for f in mod]
    return "\n".join(lines)


def _depends(edges):
    if not edges:
        return "_None._"
    out = []
    for e in edges:
        t = e.get("type")
        if t == "block":
            why = f" — {e['why']}" if e.get("why") else ""
            out.append(f"- block `{e['repo']}:{e['id']}`{why}")
        elif t == "external":
            out.append(f"- external — {e['what']}")
        elif t == "operator":
            out.append(
                f"- **operator gate** `{e['slug']}` — ends when: {e['exit']}\n"
                f"  - start: `{e['start']}`"
            )
        elif t == "approval":
            out.append(f"- **approval** `{e['slug']}` — {e['what']} (`{e['digest']}`)")
    return "\n".join(out)


def render(block):
    bid = block["id"]
    parts = [
        GENERATED_BANNER.format(id=bid),
        "",
        f"# {bid} — {block['title']}",
        "",
        f"> {block['description']}",
        "",
        "## Metadata",
        "",
        f"- **Block ID:** `{bid}`",
        f"- **Repo:** {block['repo']}",
        f"- **Kind:** {block['kind']}",
        f"- **SDLC workflow:** {block['sdlc_workflow']}",
        f"- **Model:** {block['model']}",
        f"- **Created:** {block['created']}  ·  **Updated:** {block['updated']}",
    ]
    if block.get("phase") is not None:
        parts.insert(-1, f"- **Phase:** {block['phase']}")
    if block.get("initiative"):
        parts.append(f"- **Initiative:** {block['initiative']}")
    if block.get("forward_looking"):
        parts.append(
            "- **Forward-looking** — files and interfaces are provisional; refine when this "
            "block becomes next."
        )
    if block.get("workflow_rationale"):
        parts += ["", f"**Workflow & model rationale.** {block['workflow_rationale']}"]

    parts += [
        "",
        "## What",
        "",
        block["what"],
        "",
        "## Why",
        "",
        block["why"],
        "",
        "## Relevant Files",
        "",
        _files(block.get("files") or {}),
    ]

    if block.get("interfaces"):
        parts += ["", "## Interfaces / shared surface", "", _bullets(block["interfaces"])]

    parts += [
        "",
        "## Out of Scope",
        "",
        _bullets(block.get("out_of_scope")),
        "",
        "## Step by Step Tasks",
        "",
        "See `tasks.json` in this directory — the task list is defined there, not here.",
        "",
        "## Acceptance Criteria",
        "",
        _criteria(block.get("acceptance_criteria")),
    ]

    if block.get("testing_strategy"):
        parts += ["", "## Testing Strategy", "", block["testing_strategy"]]

    parts += [
        "",
        "## Validation Commands",
        "",
        _bullets([f"`{c}`" for c in (block.get("validation_commands") or [])])
        if block.get("validation_commands")
        else "See `planning/harness.json` -> `validation.checks[]`.",
        "",
        "## Dependencies",
        "",
        _depends(block.get("depends_on")),
    ]

    if block.get("carryover_context"):
        parts += [
            "",
            "## Carryover context",
            "",
            "Open carryover entries this block must not re-break:",
            "",
            _bullets([f"`{s}`" for s in block["carryover_context"]]),
        ]

    if block.get("notes"):
        parts += ["", "## Notes", "", block["notes"]]

    return "\n".join(parts) + "\n"


def preserve_amendments(rendered, existing):
    """Carry an existing Amendment Log through the render — stages append to it mid-run."""
    if not existing or AMENDMENT_HEADING not in existing:
        return rendered
    tail = existing[existing.index(AMENDMENT_HEADING):]
    return rendered + "\n" + tail.rstrip() + "\n"


def process(block_path, planning_dir, check):
    with open(block_path) as fh:
        block = json.load(fh)
    bid = block["id"]
    spec_dir = os.path.join(planning_dir, bid)
    target = os.path.join(spec_dir, "tasks.md")

    existing = None
    if os.path.exists(target):
        with open(target) as fh:
            existing = fh.read()

    out = preserve_amendments(render(block), existing)

    if check:
        if existing is None:
            print(f"MISSING  {target}")
            return 1
        if existing != out:
            print(f"DRIFTED  {target} — hand-edited, or the block record changed without a re-render")
            return 1
        return 0

    os.makedirs(spec_dir, exist_ok=True)
    with open(target, "w") as fh:
        fh.write(out)
    print(f"rendered {target}")
    return 0


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("block_id", nargs="?", help="Block ID, e.g. MV.3.A")
    ap.add_argument("--all", action="store_true", help="render every block record")
    ap.add_argument("--planning", default="planning", help="planning directory (default: planning)")
    ap.add_argument("--check", action="store_true",
                    help="verify the rendered output matches disk; write nothing")
    args = ap.parse_args()

    blocks_dir = os.path.join(args.planning, "blocks")
    if not os.path.isdir(blocks_dir):
        print(f"no block records at {blocks_dir}", file=sys.stderr)
        return 0

    if args.all:
        paths = sorted(
            os.path.join(blocks_dir, f) for f in os.listdir(blocks_dir) if f.endswith(".json")
        )
    elif args.block_id:
        paths = [os.path.join(blocks_dir, f"{args.block_id}.json")]
        if not os.path.exists(paths[0]):
            print(f"no block record at {paths[0]}", file=sys.stderr)
            return 1
    else:
        ap.error("give a block ID or --all")

    rc = 0
    for p in paths:
        rc |= process(p, args.planning, args.check)
    return rc


if __name__ == "__main__":
    sys.exit(main())
