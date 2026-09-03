# CLAUDE.md — engine-rs

@AGENTS.md

The file above carries everything that is true for any agent working in this repo: what engine-rs
is, the boundary test, the orientation pointers, the standing rules, build/test/run, the directory
map, the SDLC pipeline notes, the response style and the stopping rule. **Read it as part of these
instructions** — Claude Code loads it automatically through the `@` import.

Only Claude-specific content belongs below.

## Fleet & Core Skills

The harness carries specialized skills in `.claude/skills/` (and `.agents/skills/`). Always consult
the corresponding skill before executing high-stakes fleet operations:

| Skill | Primary Focus | When to consult |
|---|---|---|
| **`commit-in-this-fleet`** | Safe git operations across multi-repo & vault symlinks | BEFORE any `git add`, `commit`, `stash`, `reset`, or `mv` |
| **`derive-state-safely`** | Authored vs derived state and writer execution | BEFORE running `mev emit-state --write`, `set-block-status`, or other state writers |
| **`edit-state-json`** | Canonical `planning/state.json` schema & graph edges | BEFORE hand-editing `state.json` or authoring `depends_on`/`carryover` |
| **`notify-operator`** | Operator alerting discipline via `bastion notify` | BEFORE sending notifications or deciding a lane is blocked |
| **`ping-agent`** | Cross-lane messaging envelopes & registry protocol | BEFORE sending or triaging cross-lane messages |
| **`report-to-the-operator`** | Concise operator reporting ceiling & format | When drafting chat replies, turn outputs, and run reports |
| **`run-the-gates`** | Fleet validation suite & gate diagnostics | BEFORE running `validate-brain` or `harness.json` checks |
| **`stop-or-continue`** | Session restart vs continuation correctness criteria | When an underlying binary/engine changes; never restart for token budget |
| **`write-okf-markdown`** | OKF YAML frontmatter & index.md row maintenance | BEFORE creating or editing any `.md` under `docs/` or `planning/` |
| **`write-repo-doc`** | Reader-first internal documentation standards | BEFORE writing or restructuring docs under `docs/` or guides |

## Available Commands

All harness commands are installed globally in `~/.claude/commands/` via `/sync-global-commands`
(run from base-template). Invoke them with `/<name>` directly. Project-specific commands (if any)
live in `.claude/commands/` and take precedence over global commands on name conflict.

### Session

| Command | What it does |
|---|---|
| `/prime` (global) | Deep session start — reads key docs and summarizes state |
| `/session-recap` (global) | Start-of-session briefing: recent log, current focus, next action |
| `/handoff` (global) | Write handoff.md + log work + commit; hands off to a fresh agent |
| `/wrap-up` (global) | Log work + commit; clean session close without a handoff file |
| `/status` (global) | Quick status snapshot of current focus and momentum |
| `/log-work` (global) | Log a completed work session and update status.md |
| `/archive` (global) | Retire a folder/file — distill durable residue first (D35 gate) |
| `/capture` (global) | Scaffold planning/<slug>/notes.md for pre-plan ideas; adds backlog ticket to brain |

### Planning

| Command | What it does |
|---|---|
| `/plan` (global) | Author a mini-roadmap (phases/blocks) into planning/plan-<slug>/plan.md |
| `/ticket` (global) | Single-block behavior-change spec with observable AC + testing strategy |
| `/chore` (global) | Plan a maintenance or housekeeping task |
| `/breakdown` (global) | Decompose a task spec into agent-executable sub-steps |
| `/generate-tasks` (global) | Generate a task spec for a specified phase and block |
| `/generate-master-plan` (global) | Author the project roadmap as canonical block definitions |

### SDLC

| Command | What it does |
|---|---|
| `/implement` (global) | Execute a plan file against the codebase |
| `/test` (global) | Application validation test suite |
| `/fix` (global) | Make targeted fixes for a FAIL or PARTIAL review verdict |
| `/patch` (global) | Hotfix ladder: small targeted fix routed to lean /sdlc-task |
| `/document` (global) | Update docs to reflect a completed, reviewed implementation |
| `/update-docs` (global) | Documentation health sweep: find stale sections and create missing coverage |
| `/conditional_docs` (global) | Task-type documentation router |
| `/process-tasks` (global) | Process a task list sequentially |
| `/update-task` (global) | Update a task spec after a deviation or completion |
| `/review-task` (global) | Verify a completed task against its spec and acceptance criteria |
| `/review-workflow` (global) | Verify that a completed pipeline executed correctly |
| `/review-PR` (global) | Review a PR against its block spec; post structured verdict |
| `/close-out` (global) | Verify test coverage, patch docs, and hand off cleanly |

### Git

| Command | What it does |
|---|---|
| `/commit` (global) | Stage and commit changes with a conventional message |
| `/init-worktree` (global) | Initialize a new git worktree for isolated work |
| `/clean-worktree` (global) | Merge a completed worktree branch into main and remove it |
| `/start-block` (global) | Start a new spec block: branch, initial commit, worktree setup |
| `/merge-train` (global) | Merge all approved block PRs in dependency order |

### E2E

| Command | What it does |
|---|---|
| `/test_auth_gate` (global) | E2E test template: authentication gate |
| `/test_crud_api` (global) | E2E test template: CRUD API |
| `/test_error_handling` (global) | E2E test template: error handling |
| `/test_ui_form` (global) | E2E test template: UI form |

> `/sync-global-commands` (global) is available in base-template only — it syncs
> these commands to `~/.claude/commands/` and aborts if run outside the base-template root.
