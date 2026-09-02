#!/usr/bin/env python3
"""check_frontmatter.py — parse a file's OKF YAML frontmatter and report parse errors.

Built for hooks/pre-commit (the author-time frontmatter YAML gate). Reads a file's
content on stdin — normally the STAGED content via `git show :<path>`, not the working
tree, so the check reflects exactly what is about to be committed — and attempts to
parse the leading `---`-delimited frontmatter block as YAML.

A `: ` (colon-space), unquoted `#`, or an em-dash clause inside an unquoted plain scalar
(typically `description:`/`title:`) breaks YAML parsing with `mapping values are not
allowed in this context` — the trap this exists to catch at commit time instead of at
`mev validate-brain` time, where it silently fails --structure/--links/--graph/--state
all at once. See planning/state.json carryover `okf-frontmatter-unquoted-colon-trap`.

Exit 0: no frontmatter, frontmatter parses cleanly, or the presence/placement gate this
script delegates to (below) also finds nothing wrong. This script itself is a parse gate,
not a presence gate — a missing, unterminated, or displaced frontmatter block is base-
template's scripts/check_frontmatter_presence.py's job, and both of this script's own
escape hatches now hand off to it by name instead of silently passing.
Exit 1: frontmatter present but fails to parse, OR the delegated presence/placement gate
rejects the file (displaced, unterminated, or absent — see check_frontmatter_presence.py).
Prints file:line, the offending source line, and a fix hint to stderr either way.

A second check rides along: the `created`/`updated` date-field gate (see
check_date_fields below). It is ON by default as of 2026-08-31 (see date_gate_enabled);
set OKF_DATE_GATE=0 to turn it off for one commit.

Usage: python3 check_frontmatter.py <path-for-display> < content
"""
import datetime
import os
import re
import subprocess
import sys

try:
    import yaml
except ImportError:
    # The hook itself already checks for PyYAML before invoking this script, but stay
    # non-fatal here too in case this script is ever run standalone.
    print("check_frontmatter.py: PyYAML not available — cannot check", file=sys.stderr)
    sys.exit(0)


def _presence_check(path: str, content: str) -> int:
    """Delegate to base-template's frontmatter presence/placement gate.

    Neither of this script's two `return 0` escape hatches below ever pointed at a real
    check (see the 2026-08-22 status.md incident in
    scripts/check_frontmatter_presence.py's own docstring, base-template repo). This is
    that delegation: shell out to base-template/scripts/check_frontmatter_presence.py,
    which decides ABSENT / UNTERMINATED / DISPLACED using the same corpus-membership
    scope rule the write-okf-markdown skill documents.

    Degrades gracefully (returns 0, warns on stderr) if base-template is not checked out
    as a sibling of this repo, or the script is missing — this repo (and this hook) must
    still function in a clone that lacks base-template alongside it.
    """
    # Walk up for brain.toml rather than assuming this hook sits directly under the brain
    # root. It does in HQ (<brain>/hooks/), but once this hook is distributed downstream it
    # sits at <brain>/core/<repo>/hooks/, where the old parent-of-parent guess resolved to
    # <repo>/base-template/... — a path that never exists, so the delegation degraded to a
    # warning on EVERY commit in every sub-repo instead of running. Fall back to the old
    # guess so a clone with no brain.toml above it behaves as before.
    here = os.path.dirname(os.path.abspath(__file__))
    brain_root = os.path.dirname(here)
    probe = brain_root
    while True:
        if os.path.isfile(os.path.join(probe, "brain.toml")):
            brain_root = probe
            break
        parent = os.path.dirname(probe)
        if parent == probe:
            break
        probe = parent
    script = os.path.join(brain_root, "base-template", "scripts", "check_frontmatter_presence.py")
    if not os.path.isfile(script):
        print(
            f"check_frontmatter.py: warning: presence gate not found at {script} "
            "— skipping presence/placement check",
            file=sys.stderr,
        )
        return 0
    try:
        proc = subprocess.run(
            [sys.executable, script, path],
            input=content,
            capture_output=True,
            text=True,
        )
    except OSError as e:
        print(f"check_frontmatter.py: warning: could not run presence gate: {e}", file=sys.stderr)
        return 0
    if proc.returncode != 0:
        sys.stderr.write(proc.stderr)
        return 1
    return 0


# ── created/updated date gate (OPT-IN — see DATE_GATE_ENV) ────────────────────

DATE_GATE_ENV = "OKF_DATE_GATE"
DATE_FIELDS = ("created", "updated")
_DATE_RE = re.compile(r"^\d{4}-\d{2}-\d{2}$")


def date_gate_enabled() -> bool:
    """True unless OKF_DATE_GATE=0 is exported. ON by default since 2026-08-31.

    `created`/`updated` were added to `okf_core::OkfFrontmatter` on 2026-08-29 (okf-core
    block OK.ticket.add-created-updated-frontmatter) with NO validation anywhere in the
    fleet — okf-core models them as free strings and mev's OKF validator has no rule for
    either. This gate is the enforcement point.

    Blast radius, re-measured over the real corpus 2026-08-31 (7,425 tracked `.md` across
    every repo in the fleet): 45 files carry `created:`/`updated:`, of which 5 would be
    rejected — all five bastion *test fixtures* under `src/**/fixtures/`, each carrying an
    RFC3339 `updated:` that no parser in bastion reads. `is_code_fixture()` puts those out
    of scope (they are test data, not documents), so the gate's live blast radius on the
    corpus itself is zero. The earlier note in hooks/README.md measured 12 files / 0 blocked
    on 2026-08-29; the corpus has moved since, so re-measure before widening this gate.

    Escape hatch: `OKF_DATE_GATE=0 git commit ...` for a single commit.
    """
    return os.environ.get(DATE_GATE_ENV) != "0"


def is_code_fixture(path: str) -> bool:
    """True for a `.md` that is test data for code, not a document in the corpus.

    The hook checks every staged `.md`, but the corpus is only `docs/` + `planning/`
    (brain.toml). A markdown fixture under a source tree exists to be *parsed*, and may
    legitimately carry a malformed or unusual field precisely because that is what the
    test asserts on. Scope the date gate out of those rather than editing test data to
    please a document gate. The YAML *parse* gate above still applies to them — a fixture
    that cannot be parsed at all is a broken fixture either way.
    """
    norm = path.replace(os.sep, "/")
    return "/fixtures/" in norm or norm.startswith("fixtures/")


def check_date_fields(path: str, data) -> int:
    """Check `created`/`updated` are date-only YYYY-MM-DD, and ordered.

    The YAML trap this has to handle: an UNQUOTED `created: 2026-08-29` is parsed by
    PyYAML into a `datetime.date` object, not a string — so a naive regex over the
    value would reject the most common correct spelling. A quoted "2026-08-29" arrives
    as a str and is regex-checked. A full timestamp (`2026-08-29T10:00:00Z`) parses to
    `datetime.datetime`, which is a *subclass* of `date`, so it must be rejected
    explicitly and BEFORE the date check — that shape belongs in `timestamp:`, not here.

    Returns 1 (and prints to stderr) on a violation, 0 otherwise. Absent fields are
    fine: both are optional and the corpus is not backfilled.
    """
    if not isinstance(data, dict):
        return 0

    parsed: dict[str, datetime.date] = {}
    fail = 0
    for field in DATE_FIELDS:
        if field not in data:
            continue
        value = data[field]
        if isinstance(value, datetime.datetime):
            print(
                f"pre-commit: {path}: `{field}: {value}` is a full timestamp — "
                "expected a date only (YYYY-MM-DD)",
                file=sys.stderr,
            )
            print(
                "  fix: use a date (2026-08-29). A timestamp with a time and timezone "
                "belongs in `timestamp:`, which is a different field.",
                file=sys.stderr,
            )
            fail = 1
            continue
        if isinstance(value, datetime.date):
            parsed[field] = value
            continue
        if isinstance(value, str) and _DATE_RE.match(value):
            try:
                parsed[field] = datetime.date.fromisoformat(value)
                continue
            except ValueError:
                pass  # shaped like a date, is not one (2026-13-40) — falls through
        print(
            f"pre-commit: {path}: `{field}: {value!r}` is not a valid YYYY-MM-DD date",
            file=sys.stderr,
        )
        print(
            "  fix: write it as 2026-08-29. Both fields are optional — omit one rather "
            "than guess a date.",
            file=sys.stderr,
        )
        fail = 1

    if "created" in parsed and "updated" in parsed and parsed["updated"] < parsed["created"]:
        print(
            f"pre-commit: {path}: `updated: {parsed['updated']}` is earlier than "
            f"`created: {parsed['created']}`",
            file=sys.stderr,
        )
        print("  fix: correct whichever is wrong — a doc cannot be revised before it existed.", file=sys.stderr)
        fail = 1

    return fail


def _in_code_fence_mask(lines):
    """True for every line inside (or marking) a ``` fenced code block.

    A `---` inside a fence is an ILLUSTRATIVE EXAMPLE — e.g. the OKF sample in standing
    rule 6 of the brain root's own CLAUDE.md — never a real frontmatter fence. Mirrors
    base-template/scripts/check_frontmatter_presence.py's mask of the same name so this
    parse gate and the presence gate it delegates to agree on what a fence is.
    """
    mask = [False] * len(lines)
    in_fence = False
    for i, line in enumerate(lines):
        if line.strip().startswith("```"):
            mask[i] = True
            in_fence = not in_fence
            continue
        mask[i] = in_fence
    return mask


def main() -> int:
    if len(sys.argv) < 2:
        print("check_frontmatter.py: missing <path> argument", file=sys.stderr)
        return 0  # non-fatal: never block a commit on a checker misuse

    path = sys.argv[1]
    if sys.stdin.isatty():
        print(
            f"check_frontmatter.py: {path}: no content on stdin (stdin is a terminal)",
            file=sys.stderr,
        )
        print(
            "  fix: this checker reads the file's content on STDIN — "
            f"`git show :{path} | python3 hooks/check_frontmatter.py {path}`. "
            "Passing the path alone checks nothing and used to exit 0 (a vacuous pass).",
            file=sys.stderr,
        )
        return 1
    content = sys.stdin.read()
    if not content.strip():
        print(
            f"check_frontmatter.py: {path}: empty content on stdin — nothing to check",
            file=sys.stderr,
        )
        print(
            "  fix: pipe the content in — "
            f"`git show :{path} | python3 hooks/check_frontmatter.py {path}`. "
            "If the file really is empty, it carries no OKF frontmatter and should not "
            "be committed into the corpus.",
            file=sys.stderr,
        )
        return 1
    lines = content.splitlines()

    if not lines or lines[0].strip() != "---":
        # No fence at line 1 — could be legitimately no-frontmatter, or a DISPLACED /
        # ABSENT corpus file. That's check_frontmatter_presence.py's call, not this
        # parse gate's; delegate instead of punting to a check that doesn't exist.
        return _presence_check(path, content)

    # Skip ``` fenced regions when hunting for the CLOSING fence: an unterminated
    # frontmatter block whose file later contains a fenced OKF example would otherwise
    # take the example's `---` as its terminator and parse prose as YAML.
    in_code = _in_code_fence_mask(lines)
    end = None
    for i in range(1, len(lines)):
        if not in_code[i] and lines[i].strip() == "---":
            end = i
            break
    if end is None:
        # Unterminated frontmatter is a structural/placement issue, not a YAML parse
        # one — delegate to check_frontmatter_presence.py, same reasoning as above.
        return _presence_check(path, content)

    fm_text = "\n".join(lines[1:end])

    try:
        data = yaml.safe_load(fm_text)
    except yaml.YAMLError as e:
        mark = getattr(e, "problem_mark", None)
        if mark is not None:
            # mark.line is 0-indexed within fm_text; the frontmatter body starts at
            # file line 2 (line 1 is the opening `---`), so +2 converts to a 1-indexed
            # line number in the full file.
            file_line = mark.line + 2
            snippet = lines[file_line - 1] if 0 <= file_line - 1 < len(lines) else ""
        else:
            file_line = "?"
            snippet = ""
        print(f"pre-commit: {path}:{file_line}: malformed YAML frontmatter", file=sys.stderr)
        if snippet:
            print(f"  {snippet.strip()}", file=sys.stderr)
        print(
            "  fix: quote the value (wrap it in \"...\") — likely an unquoted ':', "
            "'#', or em-dash clause inside a plain scalar",
            file=sys.stderr,
        )
        return 1

    # Only reachable once the frontmatter has parsed cleanly above.
    if date_gate_enabled() and not is_code_fixture(path):
        return check_date_fields(path, data)

    return 0


if __name__ == "__main__":
    sys.exit(main())
