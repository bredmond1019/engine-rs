---
type: Reference
title: The ORCHESTRATION workflow
description: How engine-rs sequences SDLC_FLOW and SDLC_TASK runs, plus dispatched in-process workflows, across repos from a lane chain — the gates it applies before each step, the policy it resolves, and the closed engine type that bounds what it can invoke.
doc_id: orchestration-workflow
layer: [engine]
project: engine-rs
status: active
keywords: [orchestration, lane chain, admission control, operator hold, sanctioned engines, worktree isolation, permission profiles, conductor, autonomous run, campaign budget]
related: [architecture, sdlc-flow-workflow, sdlc-flow-policy, terminal-crates, orphan-recovery]
---

# The ORCHESTRATION workflow

`ORCHESTRATION` takes a **lane chain** — a roadmap plus a lane name, or an explicit block list — and
drives one run per block, in order, across more than one repo. Each block's own authored
`sdlc_workflow` field selects whether that run is `SDLC_FLOW` or `SDLC_TASK` — a chain can freely mix
both (see "A chain may mix `task` and `flow` blocks" below).

It exists because the mechanical half of driving a lane is a set of predicates over a graph: resolve
the chain, check dependencies, pick the engine, run one block at a time, verify the state write,
append to the lane log. None of that needs judgement. What needs judgement is what to do when one of
those predicates fails, and that stays with a human.

**Workflow type:** `ORCHESTRATION` · **Node:** `OrchestrationRunNode` ·
**Source:** `crates/engine-core/src/workflows/orchestration/`

## What it actually invokes — read this before extending it

`ORCHESTRATION` calls **native Rust workflows in-process** — `SDLC_FLOW` for a `flow` block,
`SDLC_TASK` for a `task` block. It does not open a Claude Code session and type `/sdlc-flow` or
`/sdlc-task`, and it does not shell out to the JS engines under `.claude/workflows/`.

Per step, `execute.rs` resolves the step's repo slug through the injected `RepoRegistry` to an
absolute path, builds a **fresh** `Workflow` for that block's own engine (policy-aware registry +
schema, registered with that same registry so `SetupWorktreeNode` resolves `event.repo` too), seeds
`event.repo` on the dispatched event, and runs it to completion. Nothing is kept alive or reused
between steps.

Claude Code sessions *do* happen — one layer down. `SDLC_FLOW`'s own model-bearing nodes
(`ClaudeCodeStep` -> `claude_code_rs::execute`, per `D4`) spawn them for the implement, review and
docs stages. So the call stack is:

```
ORCHESTRATION (Rust)
  └─ per block: SDLC_FLOW (Rust, fresh instance, cwd = that block's repo)
       └─ per stage: ClaudeCodeStep -> a Claude Code session
```

Both layers read a repo's harness and `CLAUDE.md` from the **working directory**, which is why a
session cannot span repos but a workflow can. That is what removes the driver as the ceiling on lane
length: a twelve-block chain across four repos is the same shape as a two-block chain in one.

## A chain may mix `task` and `flow` blocks

`EngineKind` has two variants, `Flow` and `Task`, and **both are runnable** (`EN.11.P`). The engine
is resolved per block, from that block's own authored `sdlc_workflow` field, via
`EngineKind::from_sdlc_workflow` — never fixed for the whole chain. A chain can freely interleave
`task` and `flow` blocks; each step in `execute.rs` dispatches to whichever engine its own block
declares.

A block whose authored `sdlc_workflow` falls outside the closed `{task, flow}` vocabulary — absent,
a typo, or a value like `sdlc-run`/`sdlc-block` that this engine deliberately does not support —
still fails loudly with `ExecuteError::UnsupportedEngine` naming the block and repo. It does not
silently fall through to `Flow`, and it does not panic. Check the `sdlc_workflow` field of every
block in a chain if you need to know in advance whether it is runnable — a missing or unsupported
value is the only thing that still stops a block.

Each engine writes and is verified against its own state file, so a `task` step and a `flow` step
against the same `planning/<slug>/sdlc/` directory never collide: `sdlc_flow::DEFAULT_STATE_FILENAME`
(`sdlc-flow-state.json`) for `Flow`, `sdlc_task::DEFAULT_STATE_FILENAME` (`sdlc-task-state.json`) for
`Task`. `integrate.rs`'s `state_path_for` selects the filename from the step's own `EngineKind`
before reading the state write back.

A `task` block whose reconcile failed is a distinct, **terminal** case: `sdlc_task::lean_bookkeep`
writes `status: "reconcile_failed"` to its state file and deliberately skips the block-status flip,
so the block is genuinely not closed. Integrating that as a success would be exactly the silent
unreliability this module's state-write verification exists to prevent, so it instead surfaces
`IntegrateError::ReconcileFailed` and **stops the chain** — the same terminal treatment as any other
integration failure, not a warning that lets the chain continue past an unclosed block.

**Scope actually exercised so far** (`EN.11.P`, tests added `crates/engine-core/tests/it/sdlc_task_e2e.rs`):
a two-block chain of one `task` block plus one `flow` block completing with `steps_integrated == 2`;
a `task` block with a failed reconcile stopping the chain; a `flow`-only chain unchanged. All three
run against **tempdir fixture repos** — see "Status: not yet exercised on a real chain" below, which
still applies to a *mixed* chain exactly as it does to a flow-only one. No corpus-wide percentage of
"how much is now drivable" is restated here; the figure would need to be re-measured against the
current corpus at the time it's cited, and that re-measurement is out of scope for this rewrite.

## A chain may also mix `block` and `dispatch` steps (`EN.12.E`)

Every step in a chain also carries a `kind`, separate from the `EngineKind` question above.
`kind` answers "is this step an SDLC spec at all, or a registered in-process workflow?" — while
`EngineKind` (previous section) only ever answers "which SDLC engine" for a step that already is
one. A `ChainStep`'s `kind` is `StepKind::Block` by default, so an existing chain with no `kind`
field behaves exactly as before this feature; `StepKind::Dispatch` opts one step into the
behavior below (`StepKind::Command` is reserved for a future block).

- **A `block` step** is everything described above — one `SDLC_FLOW`/`SDLC_TASK` run against a
  corpus block, gated, executed, and integrated the normal way.
- **A `dispatch` step** runs a **registered in-process workflow** instead — one of the workflows
  already registered with the same `Dispatcher` `engine-serve::workflows` populates (for example
  `RESEARCH_AGENT` or `CONTENT_PIPELINE`; see `docs/architecture.md`'s `Dispatcher` entry). It
  never opens a Claude Code session, never selects an `EngineKind`, and never falls through to a
  block invocation — those are separate code paths (`dispatch.rs`'s `execute_dispatch_step`, not
  `execute.rs`'s `execute_step`).

A dispatch step reuses `ChainStep::block_id` as the `Dispatcher` registry key (`workflow_type`,
e.g. `"RESEARCH_AGENT"`) rather than adding a new field — `dispatch.rs`'s `workflow_key` accessor
names that reuse so a reader never has to infer it from the field name. An unregistered key stops
the chain loudly with `DispatchStepError::UnknownWorkflowKey`, naming the step's `block_id` and
the key it resolved to; it never silently proceeds to the next step or falls back to a block run.

**A dispatch step's outcome is recorded in the journal, not `lane-log.jsonl`.** `integrate.rs`
routes a `Dispatch` step to `execute_dispatch_step` and records the result as a `JournalRow` (the
same `StepIntegrated`/`StepBailed` decision kinds a block step's integration uses — see
`docs/architecture.md`'s Journal entry) through a new opt-in entry point,
`integrate_chain_with_dispatch`. It does not push an `ExecutionOutcome`, write a checkpoint entry,
or call `step_observer` — there is no SDLC state write to make resumable and no `ExecutionOutcome`
shape to fabricate for a workflow that was never an SDLC run. Calling a chain with a dispatch step
through the older `integrate_chain`/`integrate_chain_with_journal` entry points (no `Dispatcher`
supplied) fails loudly with `IntegrateError::NoDispatcherConfigured` rather than silently skipping
the step.

## What happens per block

| Stage | Module | What it does |
|---|---|---|
| Resolve | `chain.rs` | Turns a (roadmap, lane) pair or an explicit block list into ordered `(repo, block_id)` steps. Reads mev's structured `HELD-UNTIL` / `BUDGET` / `EXCLUSIVE-REPOS` directives and `planning/lane-segments.json` — it does not re-derive segments. |
| Gate | `gates.rs` | Resolves every `depends_on` edge against the **live graph** (backed by `corpus_gates.rs`, which reads each repo's real `planning/state.json` through `okf_core::load_state`) and refuses to start a block with an unmet edge, naming the edge and its repo. `DependencyEdge` is an enum — `Block` · `Operator { slug }` · `Approval { slug }` · `External { what }` — so an **operator gate is always unmet while present** and clears only by removal from the corpus (mev is the single writer); the engine can never self-clear one. Also reads mev's `lane-frontier.json` for lane-head startability, but `startable: true` never short-circuits the per-edge check. Then consults admission control: **at capacity the run waits** — it does not proceed and does not fail, and a block parked on an operator hold releases its permit rather than starving the ceiling. `gates.rs` also exposes `check_permission_gate` (`EN.12.C`, see "Permission profiles" below), a separate check from the `depends_on` gate above — it denies a graded action outright rather than waiting on one. |
| Execute | `execute.rs` | For a `block`-kind step: builds and runs a **fresh in-process Rust `Workflow`** for whichever engine the block's own authored `sdlc_workflow` field names — `SDLC_FLOW` or `SDLC_TASK` — with the repo resolved through `RepoRegistry` and `event.repo` seeded. An unsupported or absent `sdlc_workflow` value errors (see above). A non-`Block` `kind` reaching `execute_step` is refused with `ExecuteError::WrongStepKind` — a `dispatch` step is routed elsewhere (below), never through this path. Every `FlowInvocation` also carries a required `permission_profile` (`EN.12.C`); `execute_step` resolves the child step's effective profile from the parent's plus an optional per-step request and errors with `ExecuteError::ProfileWidening` rather than silently widening it — see "Permission profiles" below. |
| Dispatch | `dispatch.rs` | For a `dispatch`-kind step (`EN.12.E`, see "A chain may also mix `block` and `dispatch` steps" below): resolves the step's `block_id` as a `Dispatcher` registry key and runs the registered in-process workflow to completion, never selecting an `EngineKind`. |
| Integrate | `integrate.rs` | Verifies the state write after a `block` step's engine returns and **fails the run loudly on a mismatch** — including a `status: "done"` run whose `final_validation.all_passed` is `false`, and a state file whose `block_id` does not match the executed block. After the state write verifies and before the step's `closed` lane-log line is appended, **merges that step's branch into `main` and pushes it** (`EN.11.C`) — this is what makes block N+1's tree actually contain block N's work; without it every step cuts its branch from `origin/main` and is missing everything before it (sequence.md finding G1). The branch to merge is resolved from `PullRequestNode`'s own stamped `branch_name` (authoritative even on the `auto_pr: false` short-circuit) with a fallback to `SetupWorktreeNode`'s branch for a run that never reached `PullRequestNode`; `None` from both is a documented no-op skip, not an error. This lives in the chain, not in `SDLC_FLOW`/`PullRequestNode` — `PullRequestNode`'s "never auto-merges" contract (human review gate, D25) is unchanged for a standalone run; only the chain, integrating steps back-to-back with nobody in the loop, has grounds to merge automatically. A merge that can't complete (conflict, rejected push) is `IntegrateError::StepMergeFailed` (carrying the failing git command's stderr) and the step is never recorded as `closed`. Before each block, re-checks the run's `cancellation_token` and a campaign-scoped `CampaignLedger` against an optional `campaign_budget` ceiling (`EN.11.F`) — both checked again at every block boundary, not just once at the start. Appends exactly one `lane-log.jsonl` line per `block` step in the on-disk contract shape `{ts, lane, repo, block, status, note}` with `status` a typed `closed` \| `bailed` \| `held` \| `cancelled` \| `budget_halted`; a **failed** step appends a `bailed` line before the error propagates, so an attempted block is never silent. Every line also stamps the resolved `permission_profile` wire identifier (`EN.12.C`, see "Permission profiles" below); the `closed` write refuses loudly (`IntegrateError::MissingPermissionProfileStamp`) if that stamp is ever absent, while the other outcome lines keep the module's existing best-effort write semantics. A `dispatch` step's outcome is journaled instead, not appended to `lane-log.jsonl` (see below). An operator hold pauses and resumes without re-running completed blocks, under a deadline rather than an unbounded poll. |

Readiness always comes from the graph, never from a roadmap's hand-written wave table. A roadmap is
an authored snapshot and has been wrong; the `depends_on` edges are the fact.

## The lane-log contract

Exactly one line per integrated block — not zero, not two. The log is the cross-lane channel, so a
missing or duplicated line is how a sibling lane reads the wrong state. The roadmap directory is
resolved by the two-location rule (`planning/roadmaps/<slug>/` first, then legacy `planning/<slug>/`;
a slug present in both is an error, never a silent preference).

Every line also carries optional `run_id`/`writer`/`build_sha` identity fields (`EN.11.A`,
additive — skip-if-`None`, so an older reader parsing the fixed `{ts, lane, repo, block, status,
note}` shape still round-trips). `writer`/`build_sha` are stamped on every line regardless of
outcome. `run_id` is the executed step's engine run UUID when a child workflow actually ran for
that step (a `closed` line, or a `bailed` line from a post-execution failure such as a state-write
verification mismatch); it is `None` when the step never got that far — cancelled, budget-halted,
or held/bailed before execution started, so there is no run to name.

A fourth identity field, `profile` (`EN.12.C`), carries the resolved `PermissionProfile` wire
identifier (`locked` \| `standard` \| `unrestricted`) and is likewise additive on read — an older
reader ignores it — but is **required on write** for a `closed` line: `integrate.rs` refuses the
write rather than append a `closed` line with no profile stamp. See "Permission profiles" below.

A clean abort, a budget halt, and a node/state-write failure are three distinguishable terminal
states in that log, not one undifferentiated stop (`EN.11.F`): a chain halted by
`POST /campaigns/{id}/abort` (an explicit human request) appends a `cancelled` line; a chain
halted because the campaign-scoped `CampaignLedger` tripped its `Budget` ceiling appends a
`budget_halted` line naming the tripped cap; either way, blocks already integrated keep their
`closed` line and no block still running or not yet started is touched. See "Campaign identity"
below for how a campaign's abort token and cost/token ceiling are threaded through a chain.

## Campaign identity

Every `ORCHESTRATION` run resolves a `campaign_id` (`EN.11.E`) — the event's own `campaign_id` when
present (so a resumed or operator-restarted chain rejoins the same campaign instead of minting a
new identity indistinguishable from a fresh one), else a fresh v4 UUID minted at run start. Each
`execute.rs` step threads that same `campaign_id` onto the child `SDLC_FLOW` event it dispatches
(`event.campaign_id`), and stamps it back onto its own step record so the step is attributable to
its campaign without re-reading the child's `TaskContext`. The parent run additionally stamps
`campaign_members` — the per-step roster — into `ctx.nodes[OrchestrationRunNode]`, next to the
existing `steps_integrated`/`blocks`/`policy`/`cancellation` fields.

`GET /campaigns/{id}` (`engine-serve`, task 5) reads this identity back: it resolves every run —
live or completed — carrying the given `campaign_id` (via `LiveStateStore::list_campaign_runs`) and
rolls up their cost/tokens from the parent run's `campaign_members` entry. See `docs/architecture.md`
(HTTP surface, `LiveStateStore`) for the endpoint and store shape, and `docs/data-contract.md` §8 for
the canonical wire shape.

`POST /campaigns/{id}/abort` (`engine-serve`, `EN.11.F` task 2) gives a human a way to stop a whole
campaign — every block in the chain, not just the block currently running. A campaign-scoped
`CancellationToken`, registered in `CampaignRegistry` under the campaign's id, is what
`integrate.rs`'s per-boundary check (above) observes. See `docs/architecture.md`'s "Campaign abort
endpoint" entry for the route contract.

## `DEBRIEF`'s two outputs: the ops digest and `POST_DRAFT` (`EN.12.M`)

`DebriefNode` (see [`debrief.md`](debrief.md) for its full mechanics) renders **two** separately-
shaped outputs from a completed campaign's journal rows, both produced by one node — no second node
was added; standing rule 6 forbids a policy knob or an optional output changing a declared graph's
node set, and the draft needs no input the ops digest hasn't already fetched:

- **The ops digest** (`render_brief`, unchanged by this block) — every step in order, every bail
  named with its reason. Written back as a `DebriefRendered` journal row with `step: "DebriefNode"`.
- **`POST_DRAFT`** (`render_post_draft` / `build_post_draft_payload`,
  `crates/engine-core/src/workflows/orchestration/post_draft.rs`) — a publishable draft: a thesis
  line, the measured numbers the run actually produced, and the evidence paths behind them. Written
  back as its own `DebriefRendered` row with `step: "PostDraft"`, and dispatched as a
  `LearningArtifact` payload whose `channel_type` is the literal `"post_draft"` — distinguishing it
  from any other `LearningArtifact` shape without parsing `digest_markdown` prose.

Both outputs are dispatched to `CONTENT_PIPELINE` over the same fire-and-forget `ChannelTransport`
seam the ops digest already used, and both are written back **synchronously**, byte-identical to
what was dispatched — the same "nothing lost to an unawaited child run" guarantee `debrief.md`
documents for the ops digest, now covering the draft too.

**The bar a draft must clear (D79).** A draft is only produced when the campaign's journal rows
collectively carry **at least one measured number AND at least one evidence path** — a numeric JSON
leaf under some row's `detail`, and a path-shaped token (something containing `/`, e.g.
`planning/harness.json` or `debrief.rs:208`) in some row's `reason` or `detail`. This is the same
bar `docs/content/queue.md` (D79) holds every hand-written entry to: a measured fact or a named
failure, checkable from an evidence path — never a claim with nothing behind it.

**No draft, rather than an empty one, when the bar isn't cleared.** A campaign whose journal has
zero rows, whose `JournalReader` fails, or whose rows carry neither a number nor a path produces
**no** `POST_DRAFT` output at all — never an empty or stub draft. This is deliberate and the
opposite of the ops digest's own rule (which always renders *something*, even "No steps ran for
this campaign"): a queue that fills up regardless of whether a run had anything worth writing up
trains the operator to stop reading it, which is the exact failure this block exists to end. Each
refusal is journaled with a named reason (zero rows / reader failure / missing number / missing
path / missing both) rather than silently dropped, so a refusal is always distinguishable from a
run nobody debriefed.

**Where a draft lands, and who decides it.** A cleared draft's `LearningArtifact` payload
materializes into `docs/content/drafts/` — but that directory is **not** a literal engine-rs (or
mev) chose. It falls out of `okf_core::BrainDocModel::index_intent()` on the payload: the same
`index_path`/`link_target` resolution `mev doc materialize --model learning-artifact` uses
internally. **okf-core owns the target directory, not mev and not engine-rs** — if the drafts
directory ever needs to change, that's an okf-core change, not a materializer flag or an engine-rs
constant.

**engine-rs writes no markdown on this path.** `DebriefNode` builds the payload, dispatches it, and
writes the journal row — it never calls `mev` and never performs an `fs::write` to a `.md` path (a
module-scoped test in `post_draft.rs` asserts this by construction). `mev doc materialize --model
learning-artifact [--write]` is the only writer, and dry-run is its default. Because there is no
synchronous path back from the dispatch, `DebriefNode` cannot observe what `mev` actually did with
the draft — so it reports what `mev` **would** write instead, before anything is applied:
`post_draft.materialize_intent.would_write` on the node's own result carries the resolved
`docs/content/drafts/<file>` path, computed the identical way (`would_write_path`, mirroring
`index_intent()`) without invoking `mev`. That is this block's dry-run observability requirement —
proposing, not writing.

## `CONDUCTOR`: picking tonight's chain (`EN.12.F`)

Every chain documented above still needs a human to name it — an explicit `blocks` list, or a
`roadmap`+`lane` pair. `CONDUCTOR` is the seam that fills in the one case that used to be a hard
refusal: **no `blocks` and no `roadmap`/`lane` at all.** With no conductor wired
(`OrchestrationRunNode::new()`'s default), that event shape still refuses exactly as before, with the
same "needs either `blocks` or `roadmap`+`lane`" diagnostic. With one wired
(`OrchestrationRunNode::with_conductor`), it proposes tonight's chain itself — and the proposal flows
through the SAME `ChainStep` shape `resolve_explicit_chain` produces, so nothing downstream of it can
tell a conductor-proposed chain from an authored one.

**Two inputs, never a third.** `conductor.rs`:

- **The operator-written weekly objective** — `read_objective`, plain prose at
  `ConductorConfig::objective_path()` (default `planning/objective.md` at the HQ/brain root, **not**
  this repo's own vaulted `planning/`). The conductor reads it; it never writes it. **With no
  objective file present, the conductor refuses to propose anything** — it never falls back to an
  inferred goal.
- **mev's computed candidate slate** — `fetch_frontier_slate`, `mev frontier --json`, shelled out
  through the SAME `CommandRunner` convention `policy::emit_state::EmitStateNode` already uses for
  `mev emit-state --write`. `frontier` is a READ verb and is exempt from the `--agent` quiesce rule a
  write verb must pass (see `conductor.rs`'s module doc); a future write call from this module would
  not be.

**Subset-only, never invented.** `propose_chain` validates a caller-supplied, ordered
`(repo, block_id)` pick list against the slate and refuses, in order: (1) any proposed block absent
from the slate — the WHOLE proposal is rejected, never silently trimmed — with
`ConductorProposalError::NotInSlate`; (2) a block id that does not exist in the corpus at all, same
refusal shape; (3) a slate candidate with no `tasks.json` yet —
`ConductorProposalError::MissingTasksJson`, naming `/generate-tasks` rather than dispatching into a
run that has nothing to execute.

**The `git log -S` pre-flight.** The corpus graph lags reality by days — measured directly against
this fleet: two blocks were fully implemented and merged while `state.json` still read `open`, because
their bookkeeping emits were silently refused by the lane's own quiesce lease. Before the proposal is
finalised, `git_log_dash_s_preflight` runs `git log -S<block_id> --oneline` (through the same injected
`Runner` convention, never a real shell-out in a test) for every surviving candidate; one with at least
one matching commit is DROPPED — not refused, the rest of the proposal still goes ahead — with the
reason recorded on `DroppedCandidate` and journalled (see below). A candidate the pre-flight cannot
even check (spawn failure, non-zero exit) is a hard refusal
(`ConductorProposalError::GitPreflightFailed`): an inconclusive check must never read as "history is
clean".

**Journalled to `EN.12.D`, retrievable per campaign.** `OrchestrationRunNode::process` writes one
`JournalRow` (`kind: JournalDecisionKind::ConductorProposed`, `step: "CONDUCTOR"`) once the run's
`campaign_id` is resolved — a human-scannable `reason` (how many blocks proposed, how many the
pre-flight or the caps below dropped) plus the full per-block detail (`proposed[]`/`dropped[]`, each
drop naming why) on `detail`. `OrchestrationRunNode::with_journal_sink` wires the sink; with none
attached (the default) a conductor-resolved chain still runs, it simply journals nothing —
`DebriefNode`'s own no-op convention.

### Constraints on the first autonomous runs (Task 5)

The operator's weekly objective (`agentic-portfolio/planning/objective.md`) bounds the *first*
`CONDUCTOR`-proposed runs so a wrong call is cheap to unwind. All three constraints are
`OrchestrationPolicy` knobs — resolved through the same four layers as every other knob in this
workflow — and, unlike most of this repo's knobs, their **built-in defaults are the caps
themselves**, not a behavior-stable no-op: a deliberate exception to CLAUDE.md standing rule 6, the
same exception `default_use_worktree` already documents, because an unbounded first unattended run was
never a cost/quality trade to begin with. All three apply **only** to a chain `CONDUCTOR` itself
proposed — an explicit `blocks` chain or a `roadmap`+`lane` chain is operator-directed, not
autonomous, and is unaffected by any of them.

| Knob | Built-in default | What it enforces |
|---|---|---|
| `conductor_max_chain_blocks` | `Some(3)` | The most blocks a conductor proposal may dispatch in one run — the upper end of the objective's "two or three blocks" cap. |
| `conductor_single_repo_only` | `true` | Trims a proposal to its first proposed block's repo before dispatch — the objective's "single repo, not a cross-repo lane" cap. A safety invariant: it stays `true` on every named profile, never relaxed by a cheaper or more thorough one, mirroring how `default_use_worktree` is never relaxed by profile either. |
| `campaign_max_cost_usd_cents` | `Some(5_000)` ($50.00) | Wired straight into `integrate::integrate_chain`'s existing `campaign_budget: Option<&budget::Budget>` parameter — `EN.11.F` task 4 already checks it at every block boundary and halts the chain with the `budget_halted` terminal state; until this task the call site always passed `None`. Kept as integer cents, not `f64`, so `OrchestrationPolicy` can still derive `Eq`. |
| `campaign_max_total_tokens` | `None` | The same ceiling expressed as a token count. Left unset on every profile — cost is the primary guardrail for the first unattended nights. |

`apply_conductor_caps` (`graph.rs`) applies the first two, in order — single-repo trim, then
chain-length trim — to the conductor's `ProposalOutcome` BEFORE the chain is finalised, appending a
`DroppedCandidate` (same shape and journalling path as a `git log -S` drop) for every block either cap
removes, so the journalled proposal always reflects what was actually dispatched.

**The permission profile is not widened by any of this.** `resolve_child_permission_profile`
(`execute.rs`, see "Permission profiles" below) already narrows-but-never-widens a per-step request
against its parent, and `permission::decide` already denies `ClearOperatorGate` for every profile via
an early return no profile row can flip — both pre-date `CONDUCTOR` and needed no new code here.
`CONDUCTOR` requests no `permission_profile` of its own, so a conductor-proposed chain runs at
whatever profile the dispatching event already carries, at most `standard`, with `ClearOperatorGate`
staying denied exactly as it is for every other chain.

**Why the caution is specific, not general.** `CONDUCTOR` selects work by reading two verdicts this
fleet has measured reliability problems in: 17 committed `gates: true` checks across 7 repos that
cannot go red (M1), and 12 forms across 6 repos of an engine writing a terminal state it has no
evidence for (M2). Autonomy layered on top of unreliable verdicts compounds overnight instead of
stalling, so the caps above stay in force — revisited only once M2's remediation closes, not by
default. Evidence: `agentic-portfolio/planning/open-work/orchestration-runs/retros/pattern-analysis-2026-09-02.md`.

Named profiles carry all four knobs explicitly (CLAUDE.md standing rule 6):

- **`baseline`** — restates the built-in defaults verbatim: `$50.00` ceiling, no token ceiling,
  3-block cap, single-repo.
- **`cheap-fast`** — a tighter `$25.00` ceiling and a 2-block cap, the low end of the objective's
  "two or three" range; still single-repo.
- **`thorough`** — a more generous `$100.00` ceiling and the full 3-block cap; still single-repo.

## Policy

Resolved through the standard four layers (per-run event override > named profile >
`planning/harness.json` > built-in default):

| Knob | Default | What it trades |
|---|---|---|
| `hold_poll_interval_ms` | `2000` | How often a paused run checks whether an operator hold has cleared. Lower notices a clearance sooner at the cost of more wake-ups. |
| `default_use_worktree` | `true` | Whether a block with no per-run policy override runs in its own worktree (isolated) or directly in the repo's shared checkout (in-place). **This is a correctness precondition, not a cost/quality knob** — see below. |
| `hold_deadline_ms` | `None` (unbounded) | The total time a single operator hold may consume before the chain fails loudly with `IntegrateError::HoldDeadlineExceeded`, instead of waiting forever. |
| `default_auto_pr` | `true` | Whether a `flow` step's `SDLC_FLOW` child event opens a real PR through `PullRequestNode`. `false` seeds `auto_pr: false` on the child event, which `PullRequestNode` short-circuits on — stamping `{"pr_url": null, "skipped": true, "branch_name": ...}` without ever shelling out to `gh`. **`SDLC_TASK` steps are unaffected**: `SdlcTaskEventSchema` drops the field entirely because `SDLC_TASK` ships no PR ceremony, so there is nothing to seed. |
| `conductor_max_chain_blocks` | `Some(3)` | `CONDUCTOR`-only (`EN.12.F` Task 5) — the most blocks a conductor proposal may dispatch. See "Constraints on the first autonomous runs" above. |
| `conductor_single_repo_only` | `true` | `CONDUCTOR`-only — trims a proposal to its first block's repo. Never varies by profile. |
| `campaign_max_cost_usd_cents` | `Some(5_000)` | `CONDUCTOR`-only — the campaign cost ceiling in USD cents, wired into `integrate_chain`'s `campaign_budget` and enforced via the `budget_halted` terminal state (`EN.11.F`). |
| `campaign_max_total_tokens` | `None` | `CONDUCTOR`-only — the same ceiling by token count. Unset on every named profile. |

Named profiles (`crates/engine-core/src/workflows/orchestration/graph.rs`):

- **`baseline`** — `2000ms` poll, `default_use_worktree: true`, no hold deadline,
  `default_auto_pr: true`. Spelled out explicitly rather than left empty, so selecting it is a
  legible, self-documenting no-op against the built-in default.
- **`cheap-fast`** — `10000ms` poll (fewer wake-ups; a cleared hold is noticed later),
  `default_use_worktree: true`, a bounded 15-minute hold deadline, `default_auto_pr: false`.
  Cheapness on this profile applies to poll intervals, hold deadlines, and now PR creation — the
  axes where getting it wrong costs a slightly later reaction or a review ceremony nobody asked
  for — never to sharing a working tree with other processes, where getting it wrong costs
  someone else's commits.
- **`thorough`** — `500ms` poll (a cleared hold is noticed almost immediately; more wake-ups),
  `default_use_worktree: true`, no hold deadline, `default_auto_pr: true`.

Defaults are also written into `planning/harness.json` under `orchestration`, so the knobs are
discoverable without reading the Rust.

**Why `default_auto_pr` exists.** The fleet sandbox at `/Users/brandon/Dev/engine-rs-sandbox/`
exists so `ORCHESTRATION` can be exercised without risking the live, concurrently-used checkouts.
Its `origin` is a local bare repo — enough for `merge_step_branch`'s `git push origin main` at
integration time, but not for `gh`, so a chain run there used to die at the PR step with
`gh pr create failed: none of the git remotes configured for this repository point to a known
GitHub host`. Passing `"policy": {"default_auto_pr": false}` (or the `cheap-fast` profile) lets a
`flow` step run to completion without ever calling `gh`. The knob is complementary to, not a
substitute for, giving the sandbox a real GitHub remote: with `auto_pr: false`,
`PullRequestNode` is never exercised, so a run that must actually test the PR path still needs a
real GitHub-hosted remote.

### `ORCHESTRATION` isolates by default

`OrchestrationPolicy::default_use_worktree` is `true`: a dispatch that carries no `policy` field —
and every named profile — runs each block in its own worktree, not in the repo's shared checkout.

This changed 2026-09-02 (`EN.ticket.orchestration-worktree-by-default`) after an incident where an
`ORCHESTRATION` dispatch with no `policy` override ran in-place while a concurrent Claude Code
session ran `git checkout` in the same tree mid-run, silently stealing `HEAD` and landing three of
the orchestrated run's commits on the other lane's branch — recovered only by a seven-commit
cherry-pick and a dead PR. It is a deliberate, named exception to this repo's standing rule 6
(a new knob must not change existing behavior): the *knob* is unchanged, only which way the
*unstated* case falls, because in this environment's routine 10+ concurrent sessions a hazard that
requires every caller to remember a policy field is a hazard that will recur.

**To opt back into in-place execution** — for example the fleet sandbox at
`/Users/brandon/Dev/engine-rs-sandbox/`, which an operator owns exclusively — pass an explicit
override on the dispatch event:

```json
{"workflow_type": "ORCHESTRATION", "policy": {"default_use_worktree": false}, ...}
```

Two contract rows sit ahead of this knob and are unreachable through it whatever it is set to
(`resolve_isolation`, `crates/engine-core/src/workflows/orchestration/execute.rs:315-334`):
`base-template` is always isolated, and the brain root is never isolated, regardless of
`default_use_worktree`.

**What this does not fix.** Even with isolation on, `merge_step_branch`
(`crates/engine-core/src/workflows/orchestration/integrate.rs:341-370`) still runs
`git checkout main`, `git merge --no-ff <branch>` and `git push origin main` against the repo's
**shared** checkout at integration time. Isolation narrows the collision window to the merge step;
it does not close it. Real mutual exclusion around that window is separate, still-open work — see
the carryover `orchestration-default-use-worktree-false-collides-with-concurrent-lanes`, which
stays open (`clears_when: null`) until it lands.

**Verifying the installed binary, not just the source tree.** The repo's gated checks (`cargo fmt`,
`cargo clippy`, `cargo nextest`, `cargo build --release`) compile and test the **source tree**; they
cannot observe what the fleet's already-running `bastion serve` actually does, because that process
is an installed binary built at some earlier commit. A green suite here is not evidence the deployed
default flipped. The standing verification recipe:

1. Rebuild the installed binaries: `agentic-portfolio/scripts/sync/rebuild_binaries.sh`.
2. Restart the sandbox instance (port 4318).
3. Dispatch an `ORCHESTRATION` event with **no** `policy` field.
4. Confirm the step result reports `use_worktree: true`, and that `main`'s `HEAD` did not move
   during the block (it should have moved only via the post-integration merge, not mid-run).

This is the standing evidence for the acceptance criterion "the deployed `bastion serve` actually
exhibits the new default" — marked `gateable: false` in the block record precisely because no
automated suite here can observe an installed binary. Run this recipe and record the outcome
(step result and `HEAD` observation) whenever the installed default needs re-confirming, rather
than inferring it from a green source-tree suite.

## Permission profiles (`EN.12.C`)

Every chain runs under a `PermissionProfile` — a closed, three-level enum (`locked` \|
`standard` \| `unrestricted`, wire-serialized snake_case) that grades what a step is allowed to do
without an operator in the loop, defined alongside a closed `GatedAction` enum
(`crates/engine-core/src/policy/permission.rs`) covering `ClearOperatorGate`,
`InstallOnMini`, `PushToMain`, and `CrossRepoWrite`. `permission::decide(profile, action)` is a
pure grading matrix over those two enums; `ClearOperatorGate` is denied for every profile via an
early return before the matrix is even consulted — no profile row can flip it, so an operator gate
can never be cleared from inside a chain run, matching the Gate stage's rule above that only mev
can clear one.

**Resolving the active profile.** `resolve_permission_profile` (and its config-only sibling
`resolve_permission_profile_from_config`) reads the `[permission_profiles]` table from a repo's
`brain.toml` (via `mev::brain::config::load_brain_config`, resolved through
`RepoRegistry::brain_root()`) and **fails closed to `Locked`** — with a typed
`ProfileResolutionError` explaining why — on any absent table, empty level set, or unknown level
id, rather than silently defaulting to something more permissive.

**Enforcing it.** `gates.rs`'s `check_permission_gate` consults `permission::decide` before a
graded action runs. On denial it calls an injected `author_operator_edge` closure to raise a
`{"type": "operator"}` edge (a stable slug of the form `permission-<action>`, so every block
denied the same action shares one operator gate to clear) and refuses the step —
`PermissionGateError::Denied` or `::EdgeAuthorFailed`. It never writes `state.json` in-process;
authoring the edge is delegated the same way `gates.rs`'s existing `depends_on` machinery treats
mev as the single writer.

**Threading it through a chain.** `execute_step` resolves each child step's effective profile from
`resolve_child_permission_profile(parent_profile, requested_profile)` — a per-step request may
*narrow* the parent's profile but never *widen* it; a widening request is refused with
`ExecuteError::ProfileWidening` rather than silently clamped. The resolved profile is seeded as a
named `permission_profile` key in both `sdlc_flow_event` and `sdlc_task_event`, and `integrate.rs`
stamps the same resolved profile onto every `lane-log.jsonl` line (see "The lane-log contract"
above) via `resolved_permission_profile_identifier`, refusing the write outright
(`IntegrateError::MissingPermissionProfileStamp`) if a `closed` line would otherwise go out with
none.

## Only sanctioned engines are reachable

The block-execution seam takes a **closed two-variant type**, `EngineKind::{Task, Flow}` — not a
command string, not a validated `&str`. Any other runner is *structurally unrepresentable*.

A block whose authored `sdlc_workflow` falls outside `{task, flow}` produces a diagnostic and does
not run. It never silently defaults. `sdlc-run` and `sdlc-block` are deliberately unsupported here:
they have different isolation and merge semantics than a chain can safely assume.

This is enforced as code rather than convention because the failure it prevents is invisible. A block
built outside the engines has no spec, no gate, no review and no honest state write — and the chain's
own verification still looks fine, because the state write looks fine. A guard test scans every file
in the module and fails if a string-typed runner is reintroduced anywhere in it.

> **Note on that guard's history.** It originally scanned only its own file, so an escape added to
> `execute.rs` — the actual block-execution seam — passed clean. It now covers the whole module, with
> a per-file allowlist of legitimate string-taking entry points. The lesson generalises: a gate must
> be shown failing *for the surface its criterion names*, not merely shown failing.

## Status: first real-repo chain has run; cross-repo remains unexercised

Every acceptance criterion is covered by integration tests against **tempdir fixture repos**. As of
2026-08-18 `ORCHESTRATION` had never sequenced a real block in a real repo. That has since changed,
but only partially — read the scope carefully before citing this as more than it is.

On 2026-09-02, `ORCHESTRATION` sequenced two fixture specs single-repo, inside engine-rs, from
block `EN.ticket.micro-spec-fixture-for-engine-seam-comparison`. The run is recorded in
`planning/roadmaps/jynx-orchestration-smoke/lane-log.jsonl` (`"writer": "engine-rs"`, the only
Rust-written lane-log in the corpus):

- `2026-09-02T18:57:11Z` — block `micro-spec-small`, `bailed` — `node 'PullRequestNode' did not
  succeed` (build_sha `2aebc8ebb9e4e7898ffb807677c5a89c8f25b2a0`).
- `2026-09-02T19:20:08Z` — block `micro-spec-small`, `closed` via `SDLC_FLOW` (run_id
  `96e6826b-a0d4-4142-b9e8-b22dd3a27cb0`, build_sha `74375e76e49f4df3f2bd43a9fa778b75dad87f67`,
  profile `standard`).
- `2026-09-02T19:28:03Z` — block `micro-spec-large`, `closed` via `SDLC_FLOW` (run_id
  `f0dd2d18-5813-4290-bd57-6867dcf459a5`, same build_sha, profile `standard`).

**What this does and does not establish.** These were fixture specs, not corpus blocks, and the
chain ran against a single repo — it is evidence that `ORCHESTRATION` can sequence a real
`SDLC_FLOW` run end to end (including a real bail-and-recover), not evidence about a cross-repo
lane. **A cross-repo chain over real corpus blocks remains unexercised.** The posture below still
applies in full to that case.

Treat the first real cross-repo run as a test, not as routine — the same posture the brain root's
`CLAUDE.md` prescribes for the first `/orchestrate` run in HQ, and with more force here because this
one drives other engines. A short **two-block, single-repo** chain where a failure is cheap to
unwind was, and remains, the right first target before a cross-repo lane; the run above is that
first target, exercised.
