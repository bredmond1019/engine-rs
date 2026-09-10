---
name: brain-graph
description: >
  Drive `bastion brain` (the OKF wikilink structural graph) and `bastion code`
  (tree-sitter Rust symbol graph) — every flag, the exactly-one-of query rule for each,
  the greppable output grammar, and --json vs --json-logs. Use BEFORE running either verb
  by hand, when writing a recipe or script that shells them, and when deciding which query
  flag (--dependents/--blast-radius/--lineage, or --def/--refs/--dependents) answers the
  question at hand. For WHICH INSTRUMENT (bastion brain vs mev's related: graph) to use in
  the first place, see check-blast-radius — that trap is not repeated here.
---

# Driving `bastion brain` and `bastion code`

This skill is the verb-surface reference for the two `bastion` graph subcommands. It does **not**
decide which instrument to reach for — `bastion brain` reads `[[wikilinks]]`, not OKF `related:`,
and returns a clean, misleading empty result on the ~97% of the corpus that only uses `related:`.
That trap, and the doc-side `mev emit-graph` alternative, are owned by the **`check-blast-radius`**
skill — load it first if you are not already certain `bastion brain` is the graph you want.

## Preflight — fail loudly, don't pin a version

`bastion --version` prints `bastion 0.1.0` for every build in this fleet (no semver movement), so a
version pin cannot detect a stale binary. Run this instead before trusting anything below:

```bash
bastion brain --help >/dev/null && bastion code --help >/dev/null && echo preflight-ok
```

If either half fails, the installed binary predates these verbs or their flags — reinstall with
`cargo install --path core/bastion --force`. Never edit this skill to work around a preflight
failure; the skill describes the INSTALLED binary, which lags source by however long since the last
install.

## `bastion brain` — the OKF wikilink structural graph

Builds a directed graph from the `[[link]]` corpus under `--root` (or the workspace resolved via
`--workspace` / config default).

**Exactly one of `--dependents <ID>`, `--blast-radius <ID>`, or `--lineage <ID>` is required** —
supplying zero or more than one is a usage error, not a silent pick-first:

```
$ bastion brain --workspace brain
error: the following required arguments were not provided:
  <--dependents <NODE_ID>|--blast-radius <NODE_ID>|--lineage <NODE_ID>>
```

| Flag | Answers |
|---|---|
| `--dependents <ID>` | Nodes that directly reference `<ID>` via `[[link]]` (incoming edges) |
| `--blast-radius <ID>` | All nodes transitively affected by a change to `<ID>` |
| `--lineage <ID>` | All nodes `<ID>` transitively references (forward reachability) |

Output grammar, one greppable line per result: `<relation>: <id>\t<path>`. Verified:

```bash
$ bastion brain --dependents D15-okf-lowercase-doc-names --workspace brain
dependent: D17-index-md-convention	/Users/brandon/Dev/agentic-portfolio/docs/decisions/D17-index-md-convention.md
dependent: D16-okf-concept-folder-planning	/Users/brandon/Dev/agentic-portfolio/docs/decisions/D16-okf-concept-folder-planning.md
```

`--dependents` is the only real flag name. A near-miss spelling that swaps the hyphen for the
wrong word is rejected outright with a "did you mean" hint pointing back at `--dependents`, not
silently accepted — verified against the installed binary.

## `bastion code` — the tree-sitter Rust symbol graph

Builds a directed symbol graph from `.rs` source files under `--root` (or the resolved workspace)
using deterministic tree-sitter extraction — no LLM. **Rust (`.rs`) only**; every other language
(Python, TS, Dart, …) is skipped silently, so an empty result on a non-Rust repo means
"unsupported," never "unused."

**Exactly one of `--def <SYMBOL>`, `--refs <SYMBOL>`, or `--dependents <SYMBOL>` is required**, same
usage-error behavior as `bastion brain`.

| Flag | Answers |
|---|---|
| `--def <SYMBOL>` | Where the symbol is defined (file:line) |
| `--refs <SYMBOL>` | Every call site / use-import of the symbol |
| `--dependents <SYMBOL>` | Symbols that directly call it (direct predecessors) |

Output grammar, one greppable line per result:
`def: <name>\t<path>:<line>` / `ref: <name>\t<path>:<line>` / `dependent: <name>\t<path>`. Verified
against `core/mev`:

```bash
$ bastion code --workspace mev --def emit_state
def: emit_state	/Users/brandon/Dev/agentic-portfolio/core/mev/src/lib.rs:2040

$ bastion code --workspace mev --refs emit_state
ref: emit_state	/Users/brandon/Dev/agentic-portfolio/core/mev/src/lib.rs:1795
ref: emit_state	/Users/brandon/Dev/agentic-portfolio/core/mev/src/lib.rs:1913
...

$ bastion code --workspace mev --dependents emit_state
dependent: create_block_body	/Users/brandon/Dev/agentic-portfolio/core/mev/src/lib.rs
dependent: set_block_status_body	/Users/brandon/Dev/agentic-portfolio/core/mev/src/lib.rs
...
```

## `--root` vs `--workspace`

Both verbs resolve their scan root the same way: `--root <path>` is an explicit override and always
wins; `--workspace <name>` resolves through `[workspaces]` in `~/.config/bastion/config.toml`
(alias `--knowledge-dir`); with neither given, the config's `default_workspace` applies. For a fleet
repo, prefer `--workspace <slug>` over a hand-typed `--root` — the config is the one place the path
is kept correct as repos move, and a typo in a manual `--root` fails as "wrong tree scanned", not as
an error.

## `--json` vs `--json-logs` — different axes, do not confuse them

These are **not** two ways to ask for machine output — they are orthogonal:

- **`--json`** is a *result-format* flag: swaps the greppable text lines above for the machine-
  readable JSON envelope (documented shape: `docs/brain-graph-output.md`). Use it when a script
  needs to parse results.
- **`--json-logs`** is a *global log-format* flag: swaps bastion's human-readable stderr log lines
  (`INFO command started …`) for structured JSON log lines. It says nothing about the result
  format and can be combined with either text or `--json` results.

Naming the wrong one when you mean "give me parseable output" is exactly the class of error the
`cli-invocations` gate (`scripts/check_cli_invocations.py`, `FLAG_CHECKED_VERBS`) now catches for
both verbs — every flag in this file has been checked against the installed binary's `--help`.

```bash
bastion brain --dependents D15-okf-lowercase-doc-names --workspace brain --json   # result format
bastion --json-logs brain --dependents D15-okf-lowercase-doc-names --workspace brain  # log format
```

## See also

- **`check-blast-radius`** — which instrument (`bastion brain` vs `mev emit-graph`) answers which
  question, the wikilinks-vs-`related:` blindness, and the exit-code-vs-empty-result trap. Read
  that first if you have not already decided `bastion brain` is the right tool.
- **`run-the-gates`** — why a piped command's exit code is the pipe's, not the command's.
- `core/bastion/docs/knowledge/brain.md` and `code.md` — the two commands' own docs.
