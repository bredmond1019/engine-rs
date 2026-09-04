---
name: check-blast-radius
description: >
  Find out what else breaks before you change a doc, a doc_id, a Rust symbol, or a crate this
  fleet path-depends on — which of the four instruments answers which question, and why the
  most obvious one (bastion brain) returns a clean-looking WRONG answer on almost every
  document in this corpus. Use BEFORE renaming or deleting a markdown file or doc_id, before
  changing a public Rust signature in mev/okf-core/engine-rs/claude-code-rs/bella, before
  removing something that looks unused, and whenever you are about to say "nothing references
  this".
---

# Checking blast radius before you change something

Two graphs exist over this fleet and they are **not** the same graph. Picking the wrong one gives
you "nothing references this" on a document with sixteen inbound edges — a silent wrong answer, not
an error (HQ standing rule 11's exact failure class).

| Question | Instrument | Not this |
|---|---|---|
| What documents point at this doc? | `mev emit-graph` (OKF `related:` edges) | `bastion brain` |
| What did I just break by moving/deleting a file? | `mev validate-brain --links`, then `--graph` | anything else |
| What documents wikilink this one? | `bastion brain --dependents` | — |
| What Rust code calls this symbol, in this repo? | `bastion code --workspace <slug> --refs/--dependents` | — |
| What *other repos* fail to compile against my `mev` change? | `mev check-consumers` | `cargo build` here |

## The trap: `bastion brain` reads `[[wikilinks]]`, not `related:`

`bastion brain` builds its graph from the `[[link]]` corpus. This fleet's structural graph is OKF
`related:` frontmatter. **About 30 files in the whole corpus use wikilinks at all**, against 3845
`related:` edges over 1345 nodes — so `bastion brain` is blind to almost every real edge, and it
reports that blindness as a clean empty result with **exit 0**.

Measured 2026-08-31:

```bash
$ bastion brain --dependents D24-rust-substrate-seam
# no dependent results for 'D24-rust-substrate-seam'        # exit 0

$ mev emit-graph | python3 -c "..."                          # see the recipe below
16 incoming: D20, D23, D25, D26, D27, D29, D38, D41, D42, D78,
             brain-index, index, claude-code-llm-provider,
             hq-capabilities-index, core:bastion-product-{architecture,ownership}
```

The two graphs are not even nested. On `D15-okf-lowercase-doc-names`, `bastion brain` returns
`D17` and `D16`; the `related:` graph returns `D17`, `D27` and `okf-frontmatter`. Each finds
something the other misses. **Never treat one as a superset of the other.**

## The authoritative doc-side answer

`mev emit-graph` prints the whole `scope:doc_id` graph as JSON — nodes, `related:` edges, and
leaves (corpus files with no `doc_id`). It writes nothing.

```bash
mev emit-graph > /tmp/graph.json
python3 - <<'EOF'
import json; g=json.load(open("/tmp/graph.json"))
t = "brain:D24-rust-substrate-seam"          # scope:doc_id — find it in g["nodes"]
print("incoming:", sorted(e["from"] for e in g["edges"] if e.get("target_node_id")==t))
print("outgoing:", sorted(e.get("target_node_id") or e["to_ref"] for e in g["edges"] if e["from"]==t))
EOF
```

- **Node ids differ between the two tools.** `mev` uses `scope:doc_id` (`brain:D24-…`,
  `core:bastion-product-ownership`); `bastion brain` takes the **bare** `doc_id` and rejects a
  scoped one (`Error: brain: unknown node id: brain:D24-… `, error code `C006`).
- Grep the node list for a partial id rather than guessing:
  `python3 -c "import json;print([n['id'] for n in json.load(open('/tmp/graph.json'))['nodes'] if 'bastion' in n['id']])"`.
- `syn walk DOC_ID --depth N` answers the same question from Synapse's `brain_edges` table — use it
  when the Brain is up and you want transitive hops; see the `query-brain` skill. It is only as
  fresh as the last index, so `mev emit-graph` is the one to trust after an edit.

## Reading the exit code correctly

`bastion brain` and `bastion code` distinguish two very different outcomes, and only one of them
is an answer:

| Output | Exit | Means |
|---|---|---|
| `Error: brain: unknown node id: X` | non-zero (`C006`) | **You typed an id that is not in the graph.** Not evidence about X. |
| `# no dependent results for 'X'` | 0 | X is a real node with zero edges *of the kind this tool indexes*. |

Because an empty result is the common case here, **run a positive control before you act on one**
(standing rule 11). Known-good controls, both verified 2026-08-31:

```bash
bastion brain --dependents D15-okf-lowercase-doc-names    # must print 2 rows
bastion code --workspace mev --def emit_state             # must print 1 def line
```

If a control comes back empty, the instrument is wrong — wrong root, wrong workspace, wrong
binary — not the world.

## Code side

```bash
bastion code --workspace mev --def        emit_state   # where is it defined (file:line)
bastion code --workspace mev --refs       emit_state   # every call site / use import
bastion code --workspace mev --dependents emit_state   # symbols that directly call it
```

Deterministic tree-sitter extraction, no LLM. Constraints that decide whether the answer is
complete:

- **Rust `.rs` only.** Python (`synapse`), TS (`bastion-web`, `learn-ai`), Dart (`bastion-ui`) are
  skipped silently — an empty result there means "unsupported", not "unused".
- **One workspace root per invocation.** `--workspace <slug>` resolves through
  `~/.config/bastion/config.toml`'s `[workspaces]`; `--root <path>` overrides it. A caller in
  *another* repo is invisible, which is exactly the case that matters for a shared crate.
- Cross-repo callers are the next section's job, never this one's.

## Cross-repo compile blast radius

`mev`, `okf-core`, `claude-code-rs`, `engine-rs` and `bella` are consumed by sibling repos as Cargo
**path** dependencies, so a signature change compiles fine in its own repo and breaks a neighbour.

For a change to `mev`, there is a purpose-built verb — run it from `core/mev`:

```bash
mev check-consumers              # every discovered consumer
mev check-consumers --consumer bastion
```

It runs `cargo nextest run --no-run --locked` per consumer in a fresh `CARGO_TARGET_DIR`
(`--no-run` matters: the break class usually lives in test fixtures, invisible to `cargo build`).
Four outcomes, one action each:

| Outcome | Fails the run | What to do |
|---|---|---|
| `pass` | no | nothing |
| `broken` | **yes** | a real API break — fix the named sites in that consumer repo |
| `lockfile-stale` | no | refresh that consumer's `Cargo.lock`; not a code break |
| `skipped-dirty` | no | that repo has uncommitted changes, so its result is not evidence — commit/stash there and re-run |
| `not-evaluable` | no | unknown failure signature; read the reason, never assume `broken` |

**There is no equivalent verb for the other shared crates** — `check-consumers` discovers *mev's*
consumers specifically. For okf-core / claude-code-rs / engine-rs / bella, find the dependents by
hand and compile them:

```bash
# from the HQ root. --no-ignore is REQUIRED: every sub-repo is in HQ's .gitignore,
# so a plain rg searches zero files and prints "No files were searched" (measured).
rg -L --no-ignore --glob '**/Cargo.toml' --glob '!**/target/**' -l \
   -e 'path *= *"[^"]*(okf-core|bella|claude-code-rs|mev)'
cargo nextest run --no-run --locked --manifest-path <consumer>/Cargo.toml
```

Ignore every hit under a `trees/` path — those are live SDLC worktrees, i.e. duplicate copies of a
repo you already have in the list, not additional consumers.

`bella-engine` in particular is a **path** dependency of `bastion`, not a versioned pin — see
`core/bella/planning/decisions/D3-bella-engine-shared-with-bastion.md`.

## Doc moves and deletions

Renaming a file, changing a `doc_id`, or deleting a doc breaks inbound `related:` edges and
markdown links elsewhere in the corpus, and the push gate attributes the failure to **the other
file** (D64 — attribution is by delta, never by path). Order:

1. `mev emit-graph` → list the inbound edges (recipe above). Those are the files you must edit.
2. Make the change, fix every citing file.
3. `mev validate-brain --links` then `mev validate-brain --graph` — **one flag per invocation**,
   they do not compose (see `run-the-gates`).
4. If the doc was deleted or renamed, the Brain corpus still holds its rows — the post-commit hook
   calls `syn prune`; verify with `syn stale` rather than assuming.

## When the answer is "nothing references this"

Before you delete on that basis, all three must be true:

- `mev emit-graph` shows zero incoming edges (not `bastion brain`),
- `rg -L -i -e '<doc_id|symbol>'` across the fleet is empty — **with `-L`**, because every
  `planning/` is a symlink and `rg` is symlink-blind by default (standing rule 9), and with
  `-e`, because `rg -E` is `--encoding` and dies with an error that a `2>/dev/null` will swallow,
- your positive control found something.

**If that `rg` also needs `-uu` (`--no-ignore --hidden`) — e.g. because the target itself is
gitignored — add explicit `--glob` excludes for build/vendor dirs, always.** `-u` alone already
stops `rg` from honoring `.gitignore`, and this fleet's own repos carry **~43GB of Rust `target/`
dirs** (`engine-rs` 16G, `bastion` 13G, `mev` 7.4G, others) plus `node_modules` — normally invisible
to `rg` via `.gitignore`, but nothing filters them out once `-uu` is set. Measured 2026-09-03: an
`rg -L -uu --no-messages` corpus sweep with no excludes pegged 350–500% CPU for 3+ minutes walking
those directories, read from outside as a runaway/hung process. **From inside the session running
it, this looks different and easier to misdiagnose: the command hits the Bash tool's timeout and
returns exit 143, or never returns.** That reads as "no matches" or a hung tool, not as a slow
search — one session nearly logged it as a clean negative on a dangling-reference check (standing
rule 11's exact failure mode, arriving through a performance path instead of a flag-parsing one).
If a `-uu` sweep times out or returns suspiciously fast-empty, suspect this before trusting the
result — fall back to a targeted `grep -rl` over the specific directories in question plus a
positive control on a value known to exist. The safe form:

```bash
rg -L -uu --no-messages \
  --glob '!**/target/**' --glob '!**/node_modules/**' --glob '!**/.git/**' \
  --glob '!**/.next/**' --glob '!**/dist/**' \
  -l '<pattern>' .
```

Plain `-L` without `-uu` does not need this — `.gitignore` already keeps `rg` out of `target/`/
`node_modules/` in that case.

## See also

- `run-the-gates` — one flag per `validate-brain` invocation; a pipe's exit code is the pipe's.
- `query-brain` — `syn recall` / `syn walk` over the indexed corpus.
- `write-okf-markdown` — the `related:` frontmatter rules that create these edges in the first place.
- `core/bastion/docs/knowledge/brain.md` and `code.md` — the two commands' own docs.
- `core/mev/docs/cli/lanes.md` — `check-consumers`, `emit-graph`, `generate-graph`.
