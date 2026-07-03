---
type: Reference
title: harness.json — copy-paste profiles
description: Ready-made planning/harness.json profiles (Rust, Python/FastAPI, Next.js) for adapting the SDLC pipeline to this project's stack.
doc_id: harness-examples
layer: [factory]
status: active
keywords: [harness.json, stack profiles, Rust, Python, Next.js, validation config]
related: [status, planning-index]
---

# `harness.json` profiles

`planning/harness.json` is the **policy** the SDLC engines read: the validation commands and
whether a UI-test stage exists. The engine code (`.claude/workflows/*.js`) carries the
**mechanism** and ships no stack defaults — so this file is where a project names its real
commands.

Pick the profile that matches this project's stack, paste it into `planning/harness.json`, and
edit the commands to match. Validation `checks[]` run **top-to-bottom**; a check with
`gates: true` blocks the review verdict on failure. Set `uiTest.enabled: true` only for web
projects that have a dev server to smoke-test.

> If `planning/harness.json` is absent, the engines fall back to the spec's
> `## Validation Commands` section and skip the UI-test stage entirely. The file is the
> recommended path, not a hard requirement.

---

## Rust (CLI / TUI / library) — no web server

```json
{
  "$schema": "../.claude/workflows/harness.schema.json",
  "stack": "rust",
  "validation": {
    "checks": [
      { "name": "fmt",    "command": "cargo fmt --check",            "purpose": "Format gate", "gates": true },
      { "name": "clippy", "command": "cargo clippy -- -D warnings",  "purpose": "Lint gate",   "gates": true },
      { "name": "test",   "command": "cargo test",                   "purpose": "Test suite — AUTHORITATIVE for verdict", "gates": true },
      { "name": "build",  "command": "cargo build --release",        "purpose": "Build gate",  "gates": true }
    ]
  },
  "uiTest": { "enabled": false }
}
```

## Python / FastAPI + pydantic — no web UI to smoke-test

```json
{
  "$schema": "../.claude/workflows/harness.schema.json",
  "stack": "python-fastapi",
  "validation": {
    "checks": [
      { "name": "ruff",  "command": "ruff check .",  "purpose": "Lint gate",   "gates": true },
      { "name": "mypy",  "command": "mypy .",        "purpose": "Type gate",   "gates": true },
      { "name": "test",  "command": "pytest",        "purpose": "Test suite — AUTHORITATIVE for verdict", "gates": true }
    ]
  },
  "uiTest": { "enabled": false }
}
```

## Python — rich checks (baseline-diff lint, test-count delta, import warnings, standing-rule scan)

The profile above treats every check as a plain command. A maturing project often wants more: gate
on **net-new** lint only (not pre-existing debt), fail when the **test count regresses**, surface
import-time **warnings** without failing, and scan source for **standing-rule violations**. Those are
the four richer `kind`s. Each still keeps all stack-specific commands and patterns here — the engine
only carries the interpretation. `kind` defaults to `"command"`, so mix plain and rich checks freely.

```json
{
  "$schema": "../.claude/workflows/harness.schema.json",
  "stack": "python-fastapi",
  "validation": {
    "checks": [
      {
        "kind": "forbidden-pattern-scan",
        "name": "standing-rules",
        "purpose": "CLAUDE.md standing-rule scan (non-waivable) — these are rules, not pre-existing debt",
        "gates": true,
        "rules": [
          { "id": "f-string-in-logging", "pattern": "logging\\.[a-z]+\\(.*f[\"']", "paths": "--include='*.py' app/" },
          { "id": "open-without-encoding", "pattern": "open\\(", "paths": "--include='*.py' app/", "allowlistPattern": "encoding=|#|\\.open\\(" },
          { "id": "param-named-id", "pattern": "def [a-zA-Z_]+\\([^)]*\\bid\\b", "paths": "--include='*.py' app/", "allowlistPattern": "obj_id|record_id|node_id|workflow_id|task_id|invalid" }
        ]
      },
      {
        "kind": "warning-scan",
        "name": "app-import",
        "purpose": "App imports cleanly; surface Pydantic field-shadow warnings (advisory)",
        "gates": false,
        "command": "cd app && uv run python -c 'import main'",
        "warningPatterns": ["UserWarning", "shadows an attribute", "field.*shadow"]
      },
      {
        "kind": "warning-scan",
        "name": "worker-import",
        "purpose": "Worker config imports cleanly; surface Pydantic field-shadow warnings (advisory)",
        "gates": false,
        "command": "cd app && uv run python -c 'import worker.config'",
        "warningPatterns": ["UserWarning", "shadows an attribute", "field.*shadow"]
      },
      { "name": "db-session-import",    "command": "cd app && uv run python -c 'import database.session'",    "purpose": "Database session imports", "gates": true },
      { "name": "db-repository-import", "command": "cd app && uv run python -c 'import database.repository'", "purpose": "Repository imports",        "gates": true },
      {
        "kind": "baseline-diff",
        "name": "net-new-lint",
        "purpose": "Ruff — fail only on violations this task introduced (diff vs worktree-creation baseline)",
        "gates": true,
        "baselineCommand": "uv run ruff check app/ --output-format=json",
        "command": "uv run ruff check app/ --output-format=json",
        "compareKeys": ["filename", "code", "message"]
      },
      { "name": "pylint", "command": "uv run pylint app/", "purpose": "Pylint", "gates": true },
      {
        "kind": "count-delta",
        "name": "pytest-count",
        "purpose": "Pytest collection count must not drop vs the previous task (catches silently-removed tests)",
        "gates": true,
        "command": "uv run pytest --collect-only -q",
        "countPattern": "[0-9]+ tests? collected",
        "failOn": "decrease"
      },
      { "name": "pytest", "command": "uv run pytest", "purpose": "Full test suite — AUTHORITATIVE for verdict", "gates": true }
    ]
  },
  "uiTest": { "enabled": false }
}
```

**How each rich kind runs:**

- **`forbidden-pattern-scan`** — each `rule.pattern` is a `grep -rnE` over `rule.paths` (defaults to the
  whole tree), minus `rule.allowlistPattern`. Any match is a violation; the check fails if any rule matches.
  Quote any glob in `paths` (`--include='*.py'`, not `--include=*.py`) so it survives shells that
  glob-expand unquoted args (zsh). Scope patterns to their intent — e.g. exclude method-style `.open(`
  from a builtin-`open()` rule via the allowlist so `fitz.open(` / `Image.open(` are not false positives.
- **`baseline-diff`** — `baselineCommand` runs once at **worktree creation** and is stored as an
  artifact; at test time `command` runs again and the engine fails only on result items whose
  `compareKeys` tuple is absent from the baseline. Pre-existing violations never gate. (Both commands
  must emit the same JSON-array format.)
- **`count-delta`** — `command` runs, the first integer on the line matching `countPattern` is the
  count, and it is compared against the previous task's recorded count. `failOn: "decrease"` fails on a
  drop; `"zero-or-decrease"` also fails when it does not grow. Task 1 (no prior count) is SKIPPED.
- **`warning-scan`** — `command` runs and its **exit code gates as usual**; additionally every
  `warningPatterns` match is recorded. With `gates: false` matches are advisory WARNs; with
  `gates: true` a match also fails the check.

---

## Optional: stub / not-implemented scan (gating companion to the implement-stage self-check)

The implement/fix stages already self-check for left-in placeholders before committing (re-read the
acceptance criteria, confirm no stubs on required paths). That is an *agent instruction* — agnostic,
no config. If you want the same thing as an **authoritative gating test** the Test stage enforces, add
a `forbidden-pattern-scan` check below. The two are complementary: the self-check reasons about intent;
this grep is a hard backstop. Paste the block for your stack into `validation.checks[]`.

Deliberate deferrals (a real Phase-N stub you intend to ship) are carved out with an
`allowlistPattern` — mark the line with a convention comment (e.g. `// stub: phase3`) and the scan
skips it. Keep `gates: true` only once the codebase is actually stub-free, or the first run will block.

**Rust:**
```json
{
  "kind": "forbidden-pattern-scan",
  "name": "no-stubs",
  "purpose": "No unimplemented placeholders left on shipped code paths",
  "gates": true,
  "rules": [
    { "id": "rust-todo-macro",          "pattern": "\\btodo!\\(",          "paths": "--include='*.rs' src/", "allowlistPattern": "// stub:" },
    { "id": "rust-unimplemented-macro", "pattern": "\\bunimplemented!\\(",  "paths": "--include='*.rs' src/", "allowlistPattern": "// stub:" }
  ]
}
```
> `unreachable!()` is intentionally **excluded** — it is a legitimate defensive assertion, not a stub.
> `#[allow(dead_code)]` Phase-N function stubs (real bodies, deferred wiring) are not matched either —
> only the placeholder macros are. Add `unreachable!\\(` to the rules only if your project never uses it
> as a genuine invariant.

**Python / FastAPI:**
```json
{
  "kind": "forbidden-pattern-scan",
  "name": "no-stubs",
  "purpose": "No unimplemented placeholders left on shipped code paths",
  "gates": true,
  "rules": [
    { "id": "py-not-implemented", "pattern": "raise NotImplementedError", "paths": "--include='*.py' app/", "allowlistPattern": "@abstractmethod|# stub:|abc\\.|/interfaces/|/base\\.py" },
    { "id": "py-todo-stub",       "pattern": "# *TODO.*implement",        "paths": "--include='*.py' app/", "allowlistPattern": "# stub:" }
  ]
}
```
> Caveat (honest): `raise NotImplementedError` is the *correct* body of an abstract method, so a flat
> grep false-positives on ABCs / `Protocol`s — the allowlist excludes `@abstractmethod`-adjacent lines
> and common interface paths, but a line-based scan can't see a decorator on the line above. Scope
> `paths` to your concrete-implementation dirs and exclude interface modules. For Python the
> implement-stage self-check (which reasons about intent) is the more reliable catch; treat this scan
> as a coarse backstop, and consider leaving it `gates: false` (advisory) until the allowlist is tuned.

**Next.js / TypeScript:**
```json
{
  "kind": "forbidden-pattern-scan",
  "name": "no-stubs",
  "purpose": "No unimplemented placeholders left on shipped code paths",
  "gates": true,
  "rules": [
    { "id": "ts-not-implemented", "pattern": "throw new Error\\(['\"]not implemented", "paths": "--include='*.ts' --include='*.tsx' src/ app/", "allowlistPattern": "// stub:" },
    { "id": "ts-stub-marker",     "pattern": "// *@stub",                              "paths": "--include='*.ts' --include='*.tsx' src/ app/", "allowlistPattern": "// stub:" }
  ]
}
```
> The `throw new Error('not implemented')` idiom is a clean signal. The `// @stub` rule is opt-in for
> teams that mark placeholder functions with a comment convention — drop it if you don't.

All three follow the `forbidden-pattern-scan` mechanics documented above (each rule is a `grep -rnE`
over `paths` minus `allowlistPattern`; any match fails the check). Quote globs in `paths`
(`--include='*.rs'`) so they survive zsh.

---

## Next.js (web) — UI-test stage enabled

The only profile that exercises the `uiTest` fields. `port` is the base port; in parallel task
runs the engine uses `port + taskNumber` automatically. `routes[]` are smoke-checked once
`readySignal` appears in the dev-server output.

```json
{
  "$schema": "../.claude/workflows/harness.schema.json",
  "stack": "nextjs",
  "validation": {
    "checks": [
      { "name": "lint",   "command": "npm run lint",        "purpose": "Lint gate",  "gates": true },
      { "name": "types",  "command": "npx tsc --noEmit",    "purpose": "Type gate",  "gates": true },
      { "name": "test",   "command": "npm test",            "purpose": "Test suite — AUTHORITATIVE for verdict", "gates": true },
      { "name": "build",  "command": "npm run build",       "purpose": "Build gate", "gates": true }
    ]
  },
  "uiTest": {
    "enabled": true,
    "devServerCommand": "npm run dev",
    "readySignal": "Ready in",
    "port": 3000,
    "routes": ["/", "/about"]
  }
}
```

---

## Optional: `/sdlc-block` policy (`block` block)

`/sdlc-block` reads the `block` section when orchestrating a roadmap as a branch train of PRs.
All profiles above omit it — the defaults are sensible for most projects.

```json
{
  "block": {
    "maxParallelBlocks": 3,
    "autoMerge": false
  }
}
```

**Key-by-key:**

| Key | Type | Default | When to change |
|---|---|---|---|
| `maxParallelBlocks` | integer | `3` | Limits concurrent worktree creation and `/sdlc-flow` runs within a wave. Lower (e.g. `1`) on machines with tight disk/memory. Raise on CI with ample resources. |
| `autoMerge` | boolean | `false` | Set `true` to merge each block's branch into the train automatically on PASS (no PRs, no human review). Leave `false` (recommended) to open one PR per block and use `/merge-train` after `/review-PR`. CLI `--auto-merge` overrides per run. |

Only `/sdlc-block` reads the `block` section; `/sdlc-run`, `/sdlc-task`, and `/sdlc-flow` ignore it.
Merge this block alongside the other top-level keys in any stack profile above.

---

## Optional: `/sdlc-flow` policy (`flow` block)

All the profiles above omit the `flow` block — the stub `harness.json` ships a neutral default
(see below). Adjust these keys when the project's defaults should differ from the engine's
built-in fallbacks.

```json
{
  "flow": {
    "autoMerge": false,
    "testDepth": "fast",
    "prBase": "main",
    "bailReasons": []
  }
}
```

**Key-by-key:**

| Key | Type | Default | When to change |
|---|---|---|---|
| `autoMerge` | boolean | `false` | Set `true` to merge the PR and tear down the worktree automatically on a clean PASS. Leave `false` (recommended) to require a human to merge — the PR is the review checkpoint. CLI `--auto-merge` overrides per run. |
| `testDepth` | `"fast"` or `"full"` | `"fast"` | `"fast"` runs only `gates:true` checks per task (cheap tripwire). Switch to `"full"` if per-task integration breaks are common and the end-review is catching too much. CLI `--test-depth` overrides per run. |
| `prBase` | string | `"main"` | Change to `"develop"` or another branch if the project uses a non-main integration target. |
| `bailReasons` | string[] | `[]` | Append project-specific immediate-bail triggers (plain English). The engine already carries five universal ones; add extras only for patterns unique to this project (e.g. `"Any migration file must be manually reviewed before merging."`). |

Only `/sdlc-flow` reads the `flow` block; `/sdlc-run`, `/sdlc-task`, and `/sdlc-block` ignore it.
Add this block to any of the stack profiles above by merging it alongside the other top-level keys.

---

*The harness carries the mechanism; this file carries the policy. Keep stack facts here, never
in `.claude/workflows/*.js`.*
