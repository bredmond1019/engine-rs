---
type: Log
title: engine-rs Development Log
description: Chronological log of work completed for engine-rs.
doc_id: log
layer: [factory]
status: active
timestamp: "2026-07-25T21:30:39Z"
keywords: [work log, session history, development log]
related: [status, context]
---

# Log — engine-rs

*Append-only working log. One dated entry per session. Newest entries at the top.*

---

## [run: 2026-07-25]

### Architecture review of EN.5.A + Phase 6 → five new master-plan blocks (EN.5.D/E/F, EN.6.F/G)
- **What:** Reviewed the just-written `EN.5.A` content-pipeline plan (envelope-based content core) and the new Phase 6 omni-channel blocks against the actual codebase, then folded the findings into `master-plan.md` as new blocks in `/generate-master-plan` format. Three findings drove the changes. **(1) `POST /events/` is synchronous** — `engine-serve/src/http.rs` awaits `workflow.run_with(...)` to completion inside the request handler before returning 202, so the Slack/Telegram/WhatsApp/SendGrid webhook ACK budget (~3s) makes `EN.6.B`–`EN.6.D` unimplementable as specified, and `EN.6.A`'s `WorkflowTriggerDispatch` self-POST would pin two actix workers per chain link. **(2) The local-model swap is unreachable from production** — swapping a stage to a local model requires `graph::registry_for_policy`, but every caller in the repo is a test or an `#[ignore]`d experiment; production registers the policy-blind `graph::workflow`. The cause is structural: `WorkflowFactory = Box<dyn Fn() -> Workflow>` is a zero-arg factory that cannot build a policy-dependent registry. Related defects found in the same sweep: policy resolves per-node (7+ nodes each re-reading `harness.json` from disk), `resolved_policy()` silently returns `Default` on parse failure, `resolve_policy_for_run` requires a worktree path that a channel envelope does not have (a hard blocker for `EN.5.A` as written), and the merge boilerplate is duplicated verbatim across all four workflow policy modules. **(3) Routing on `ChannelType` left the graph open-ended** — routing on `SourcePayload` kind instead collapses 8+ channel types onto three acquisition branches, makes the graph closed under new channels, and removes an unowned `EN.6.E` reversal. Also confirmed the `EN.5.A` graph *does* validate — the `ReviseNode → SelfCriticNode` declared back-edge is legal because the return path runs through a router and `validate.rs:183` skips router edges — so that concern was a false alarm, not a blocker. Authored five new blocks with the full block skeleton: **EN.5.D** (policy/telemetry productionization — policy-aware `WorkflowFactory`, resolve-once, derived `Overlay`, observed tiers), **EN.5.E** (composition primitives — instance identities, input bindings, bounded-loop combinator, `Dispatcher` into engine-core), **EN.5.F** (async run lifecycle — non-blocking `POST /events/`, `GET /events/{event_id}` readback, SSE progress stream), **EN.6.F** (suspend/resume — `run_from` + `SuspendNode` + `POST /events/{event_id}/resume`), **EN.6.G** (schedule source + fan-out/aggregate). Amended the `EN.5.A` and `EN.6.A` blocks in place, added a Phase 5 preamble note explaining D/E/F must land first, and filled the missing `EN.3.C` Quick Reference row (a pre-existing gap) — the table is now 30 rows / 30 block headings. Propagated the amendments into both specs: `EN.5.A-content-pipeline/tasks.json` (tasks 1, 3, 4, 7, 8, 9, 11, 13 amended; task 4 rewritten onto payload-kind routing) + `tasks.md` (AC, new "Blocked by" section, Amendment Log entry), and `EN.6.A-egress-dispatch/tasks.json` (tasks 2, 3, 6 — `X-API-Key`, `parent_run_id`/`envelope_id` correlation, depth cap) + `tasks.md`. Registered all five blocks in `planning/state.json` (EN.5.D/E/F at wave 49, EN.6.F/G at wave 61) with `depends_on` edges added to `EN.5.A` (→ EN.5.D, EN.5.E) and `EN.6.A` (→ EN.5.F).
- **Why:** `EN.5.A` was about to be scheduled as the next real build, and two of its assumptions did not hold against the code as it exists: `resolve_policy_for_run` cannot resolve a policy for a channel envelope (no worktree path), and the local-model rewire it depends on has no production caller. Phase 6's channel adapters had a second, independent blocker — a synchronous trigger endpoint that cannot meet a webhook ACK deadline. Both are infrastructure gaps, not workflow gaps, so they belong in their own blocks ahead of the workflows that need them rather than being smuggled into `EN.5.A`'s task list. Sequencing them explicitly moves `EN.5.A` to `blocked` and surfaces EN.5.D/E/F as the real next work.
- **Refs:** `planning/master-plan.md` (Phase 5 preamble + blocks EN.5.D/E/F, EN.6.F/G; amended EN.5.A + EN.6.A), `planning/EN.5.A-content-pipeline/tasks.{json,md}`, `planning/EN.6.A-egress-dispatch/tasks.{json,md}`, `planning/state.json`
- **Status:** No block completed — this was planning/review work. `mev emit-state --write` moved EN.5.D/E/F into `next` and EN.5.A into `blocked`.
- **Next:** EN.4.D — DELIVERABLE_RENDER, then the EN.5.D/E/F infrastructure trio ahead of EN.5.A.

### `EN.4.C-proposal-generator` done — PROPOSAL_GENERATOR (policy-aware, PersistToBrainNode HTTP push)
- **What:** Ran `/sdlc-flow EN.4.C-proposal-generator` (branch `EN.4.C-proposal-generator-flow`); all 12 tasks passed. Task 1 fixed pre-existing `cargo fmt` violations in an examples file and scaffolded the `proposal_generator` workflow module (mod.rs + eight sibling files, `WORKFLOW_TYPE = "PROPOSAL_GENERATOR"`). Task 2 added a new injectable `HttpPost` seam (trait + reqwest-backed live impl + `StubHttpPost`) in `nodes/http_post.rs` for the engine→brain boundary. Task 3 added `ProposalGeneratorPolicy`/`PartialProposalGeneratorPolicy` on the EN.4.0 `Policy` trait across five stages (research/opportunity/writer/review/revise) with a domain-specific `ReviewMode` (`Full`/`Skip`) knob. Task 4 filled `schema.rs` with the four-section `AutomationRoadmap` deliverable, composite/sort/≤3-profile validators, and `automation_roadmap_json_schema()`. Task 5 added three named policy profiles (`baseline`/`local-judgment`/`skip-review`), `resolve_policy_for_run`, and a `proposal_generator.{policy,profiles}` `harness.json` section. Task 6 built `ProposalCompanyResearchNode` (WebSearch-backed) and `ProposalWriterNode`. Task 7 built `OpportunityIdentifierNode`, scoring from `diagnostic_intake`'s `*_evidence` fields when present with a web-brief fallback, recomputing composite/tier deterministically rather than trusting model arithmetic. Task 8 built the review loop — `ProposalReviewNode` (with a `Skip`-mode short-circuit), `ProposalReviewRouterNode` (`Node`+`Router`, pass/revise branches), and `ProposalReviseNode`. Task 9 built `PersistToBrainNode`, POSTing the finished roadmap (preferring the revise-branch draft) over the `HttpPost` seam to a placeholder `BRAIN_INGEST_URL`. Task 10 assembled the declared `PROPOSAL_GENERATOR` `WorkflowSchema`/`NodeRegistry`/`Workflow` with a Local-tier rewire for opportunity/review/revise (never research/writer), registered in engine-serve. Task 11 added a hermetic e2e integration test driving the full seven-node chain through both router branches, plus decision `D9-engine-brain-boundary.md` recording the engine↔brain HTTP-POST boundary. Task 12 validated the full block (fmt/clippy/test/release-build all green) with no code changes needed.
- **Why:** Closes Block EN.4.C of `master-plan.md` Phase 4 — the largest workflow in the phase, reusing EN.4.A's `CompanyResearchNode` pattern and EN.4.B's `DiagnosticIntake` evidence contract, and the first workflow to POST a finished deliverable across the engine↔brain boundary via the new `HttpPost` seam rather than writing to Postgres/pgvector directly (THE BOUNDARY TEST). `AutomationRoadmap` is exported for reuse by EN.4.D (`DELIVERABLE_RENDER`).
- **Verdict:** PASS (review found no findings). Docs: `docs/proposal-generator-workflow.md` created; `docs/index.md`, `docs/architecture.md`, `docs/research-agent-workflow.md`, `docs/diagnostic-intake-workflow.md`, `docs/data-contract.md` updated.
- **Status:** `planning/state.json` block `EN.4.C` set to `"closed"`; `planning/status.md` Progress Table row added under Phase 4 and flipped to Done.
- **Next:** EN.4.D — DELIVERABLE_RENDER (net-new, roadmap → PDF).

```
954f035 docs: update docs for EN.4.C-proposal-generator
51546f3 feat: implement EN.4.C-proposal-generator-task11
88b6f26 feat: implement EN.4.C-proposal-generator-task10
c8fe13d feat: implement EN.4.C-proposal-generator-task9
17bd333 feat: implement EN.4.C-proposal-generator-task8
f2fa74b feat: implement EN.4.C-proposal-generator-task7
c4508bb feat: implement EN.4.C-proposal-generator-task6
b6e3f64 feat: implement EN.4.C-proposal-generator-task5
```

---

## [run: 2026-07-24]

### `EN.4.B-diagnostic-intake` done — DIAGNOSTIC_INTAKE extractor (net-new, policy-aware)
- **What:** Ran `/sdlc-flow EN.4.B-diagnostic-intake` (branch `EN.4.B-diagnostic-intake-flow`); all 8 tasks passed. Task 1 scaffolded the `diagnostic_intake` workflow module (mod.rs + five sibling stub files) and wired it into `workflows/mod.rs`. Task 2 added `DiagnosticIntakePolicy`/`PartialDiagnosticIntakePolicy` on the EN.4.0 `Policy` trait with a single Local-eligible `extract` stage, proven by a unit test that a Local-tier override survives resolution together with its `LocalConfig`. Task 3 added `DiagnosticIntakeEventSchema`, the `DiagnosticIntake` output type (with `WorkflowCandidate` `*_evidence` fields per `intake.md §3`), and `diagnostic_intake_json_schema()`. Task 4 added four named policy profiles (`baseline`/`cheap-fast`/`thorough`/`local-extract`), `resolve_policy_for_run`, and a matching `diagnostic_intake.{policy,profiles}` section in `harness.json`. Task 5 built `IntakeExtractNode`: wraps `ClaudeCodeStep` with a schema-constrained, tool-free `Config`, ports `intake.md`'s four interview groups + evidence discipline + São Paulo SMB priors into the extraction prompt, and persists `diagnostic-intake-state.json` telemetry. Task 6 assembled the single-node `DIAGNOSTIC_INTAKE` `WorkflowSchema`/`NodeRegistry` with a Local-tier rewire for `IntakeExtractNode`, registered in engine-serve's builtin dispatcher (`register_diagnostic_intake`). Task 7 added a hermetic e2e suite (`crates/engine-core/tests/diagnostic_intake_e2e.rs`) covering extraction with `*_evidence` field integrity through an `EventsRow` round-trip, the Local-tier rewire, dispatcher registration, and a `#[ignore]`-gated four-profile experiment harness. Task 8 validated the full block (fmt/clippy/test/release-build all green) with no code changes needed.
- **Why:** Closes Block EN.4.B of `master-plan.md` Phase 4 — a net-new, policy-aware extraction workflow (no orchestrator source to port) whose sole `extract` stage is Local-tier eligible, the first workflow in this phase to exercise `registry_for_policy`'s Local-transport rewire. `DiagnosticIntake` is exported for reuse by EN.4.C (`PROPOSAL_GENERATOR`).
- **Verdict:** PASS (review found no findings). Docs: `docs/diagnostic-intake-workflow.md` created, `docs/index.md` updated.
- **Status:** `planning/state.json` block `EN.4.B` set to `"closed"`; `planning/status.md` Progress Table row added under Phase 4 and flipped to Done.
- **Next:** EN.4.C — PROPOSAL_GENERATOR (policy-aware, PersistToBrainNode HTTP push).

```
d5959f4 docs: update docs for EN.4.B-diagnostic-intake
12320ed feat: implement EN.4.B-diagnostic-intake-task7
b65ce85 feat: implement EN.4.B-diagnostic-intake-task6
721915a feat: implement EN.4.B-diagnostic-intake-task5
c59e587 feat: implement EN.4.B-diagnostic-intake-task4
dd3e149 feat: implement EN.4.B-diagnostic-intake-task3
3bbe157 feat: implement EN.4.B-diagnostic-intake-task2
b858fb2 feat: implement EN.4.B-diagnostic-intake-task1
```

---

## [run: 2026-07-24]

### `EN.4.A-research-agent` done — RESEARCH_AGENT (company brief + prospecting mode, policy-aware)
- **What:** Ran `/sdlc-flow EN.4.A-research-agent` (branch `EN.4.A-research-agent-flow`); all 9 tasks passed. Task 1 scaffolded the `research_agent` workflow module (six leaf-file stubs + `WORKFLOW_TYPE = "RESEARCH_AGENT"`) and registered it in `workflows/mod.rs`. Task 2 added `ResearchAgentPolicy`/`PartialResearchAgentPolicy` implementing the EN.4.0 `Policy` trait (research/prospect model tiers, output verbosity, prompt cache, local config). Task 3 added `ResearchAgentEventSchema`, `ResearchMode`, `CompanyBrief`, and `ProspectingResult` with `json_schema()` builders. Task 4 added three named policy profiles (`baseline`/`cheap-fast`/`thorough`), `resolve_policy_for_run`, and a matching `research_agent.{policy,profiles}` section in `harness.json`. Task 5 built `CompanyResearchNode`: wraps `ClaudeCodeStep` with WebSearch/WebFetch tools + a `CompanyBrief` schema, resolves/applies the run policy, parses the reply, and persists `research-agent-state.json` telemetry. Task 6 built the sibling `ProspectingResearchNode` (four-pillar vertical mapping, same structural pattern). Task 7 added `ResearchModeRouterNode` plus graph/registry assembly, registered as `RESEARCH_AGENT` in engine-serve (`register_research_agent` → `register_builtin_workflows`), making it dispatchable and visible in `GET /workflows`. Task 8 added a hermetic e2e suite (`crates/engine-core/tests/research_agent_e2e.rs`) covering both modes' router→terminal-node round-trips against `engine-contract::EventsRow`, a no-Local-rewire assertion on `registry_for_policy`, dispatcher registration, and a `#[ignore]`-gated named-profile experiment harness via `policy::aggregate_state_files`. Task 9 validated the full block (fmt/clippy/test/release-build all green) with no code changes needed.
- **Why:** Closes Block EN.4.A of `master-plan.md` Phase 4 — the first of the diagnostic-funnel-adjacent workflows built on the EN.4.0 shared policy framework, and the source of `CompanyResearchNode` that EN.4.C (`PROPOSAL_GENERATOR`) is expected to reuse.
- **Verdict:** PASS (review found no findings). Docs: `docs/research-agent-workflow.md` created, `docs/index.md` updated.
- **Status:** `planning/state.json` block `EN.4.A` set to `"closed"`; `planning/status.md` Progress Table row added under Phase 4 and flipped to Done.
- **Next:** EN.4.B — DIAGNOSTIC_INTAKE extractor (net-new, policy-aware).

```
f64cded docs: add RESEARCH_AGENT workflow reference for EN.4.A
30b4f31 feat: implement EN.4.A-research-agent-task8
0d09745 feat: implement EN.4.A-research-agent-task7
e3112b2 feat: implement EN.4.A-research-agent-task6
62187f2 feat: implement EN.4.A-research-agent-task5
3ff23d3 feat: implement EN.4.A-research-agent-task4
33f9921 feat: implement EN.4.A-research-agent-task3
416ad99 feat: implement EN.4.A-research-agent-task2
```

---

## [2026-07-24]

### TestTaskNode harness check-kind parity + live-tested critical write-permission gap in ImplementTaskNode/ConsolidatedReviewNode
- **What:** Ported the four missing harness check kinds (`forbidden-pattern-scan`, `baseline-diff`, `count-delta`, `warning-scan`) into `TestTaskNode` from the Python reference, with 10 new unit tests — fixes the gate that made real per-project harnesses (e.g. orchestrator's) unpassable through `/sdlc-flow` regardless of code correctness. Also root-caused a deeper, more serious gap while live-testing against orchestrator's `or-y-event-read-api` spec through `bastion serve`: `ImplementTaskNode`/`ConsolidatedReviewNode` build their `claude-code-rs` `Config` from `Config::default()` (`dangerously_skip_permissions: false`, no `allowed_tools` grant), and `graph.rs` registers them plain with no `.with_config()` override — the exact wiring `bastion serve`'s engine mount uses. Confirmed live: two full `SDLC_FLOW` triggers both reported `ImplementTaskNode` success with a plausible written-files summary, but orchestrator's working tree stayed completely clean — no files were ever actually written. Every `/sdlc-flow` run through `bastion serve` today is a no-op on the real codebase despite reporting success.
- **Why:** The four missing check kinds were blocking real per-project harnesses from ever passing through `/sdlc-flow`, independent of code correctness — needed fixing to unblock genuine SDLC validation. The deeper write-permission gap surfaced only because of live end-to-end testing against a real orchestrator spec through `bastion serve`, and is a materially more serious finding: it means the engine has been silently no-op'ing on real codebases while reporting success.
- **Status:** Did not fix the write-permission gap — captured as carryover `sdlc-flow-implement-node-no-write-permission` (already recorded in `planning/state.json` `carryover[]`; also notes the secondary branch-staleness issue where a long-running flow's end-review diffs against a moving `main`). It needs a human call on the safety tradeoff of granting an autonomous, network-triggered node blanket write/skip-permissions.

---

## [run: 2026-07-24]

### `EN.4.0-shared-policy-framework` done — shared run-policy/observability framework + model-node seam hoist + SDLC refactor
- **What:** Ran `/sdlc-flow EN.4.0-shared-policy-framework` (branch `EN.4.0-shared-policy-framework-flow`); all 8 tasks passed. Task 1 added the new generic `crates/engine-core/src/policy/` module family (`ModelTier`/`LocalConfig`/`OutputVerbosity`, tier→model-string mapping, and the shaping fns `apply_model_tier`/`apply_prompt_cache`/`apply_verbosity_directive`, factored out of the old monolithic `apply_policy`). Task 2 added generic telemetry, aggregation, `EmitStateNode`, and resolved-policy plumbing + profile lookup to the framework. Task 3 added `crates/engine-core/tests/policy_framework.rs`, proving the framework is reusable via a standalone `SamplePolicy` outside `SdlcPolicy`. Task 4 hoisted the model-node seams (`ModelTransport`, `put_result`/`get_result`, `strip_json_fence`, and a newly-factored `parse_structured_or_fenced`) from `sdlc_flow/mod.rs` up to `workflows/mod.rs`, with `sdlc_flow` re-exporting them for back-compat (`CommandRunner`/`default_command_runner` stayed in `sdlc_flow` as specified). Task 5 refactored SDLC-flow's policy/telemetry/aggregation/emit-state/profile-lookup machinery to delegate onto the generic framework (`SdlcPolicy` implements the `Policy` trait) while keeping every serialized shape and the pre-existing SDLC test suite byte-identical. Task 6 wired `cost_usd` (read generically from each node's own `ctx.nodes[identity]["cost_usd"]`) into `Workflow::run_with`'s `BudgetLedger`, so `Budget.max_cost_usd` actually gates a run. Task 7 added decision doc `planning/decisions/D7-shared-policy-framework.md` + its index row. Task 8 validated the full block (fmt/clippy/test/release-build all green, `SdlcPolicy` behavior-preservation confirmed).
- **Why:** Closes Block EN.4.0 of `master-plan.md` Phase 4 — the shared policy/telemetry framework that Blocks EN.4.A–D (RESEARCH_AGENT, DIAGNOSTIC_INTAKE, PROPOSAL_GENERATOR, DELIVERABLE_RENDER) and EN.5.B1/B2 (eval slice runner, regression-history gate) all depend on, per the block→project-doc crosswalk in `master-plan.md`.
- **Verdict:** PASS (review found no findings). Docs patched: `docs/sdlc-flow-policy.md`, `docs/architecture.md`.
- **Status:** `planning/state.json` block `EN.4.0` set to `"closed"`; `planning/status.md` Progress Table flipped to Done with a new Phase 4 sub-table.
- **Next:** Plan and pick up one of the newly-unblocked EN.4.A–D / EN.5.B1 blocks.

```
38cb87d docs: update docs for EN.4.0-shared-policy-framework
c268b5a feat: implement EN.4.0-shared-policy-framework-task6
eb0a5b7 feat: implement EN.4.0-shared-policy-framework-task5
25eb516 feat: implement EN.4.0-shared-policy-framework-task4
6e445e0 feat: implement EN.4.0-shared-policy-framework-task3
c3d58a7 feat: implement EN.4.0-shared-policy-framework-task2
32e225c fix: fix pass 1 for EN.4.0-shared-policy-framework-task1
33ec137 feat: implement EN.4.0-shared-policy-framework-task1
```

---

## [run: 2026-07-20]

### `plan-sdlc-policy-profiles-E` (Block EN.3-plan.E) done — docs, index, and research-note wrap-up; plan complete
- **What:** Ran `/sdlc-task plan-sdlc-policy-profiles-E` (lean single-unit engine — implement →
  fast-test → fix, committed directly to `main`; no branch/PR/review, since `sdlc-task` doesn't do
  those). All 5 tasks passed: (1) hardened `docs/sdlc-flow-policy.md` against the shipped profile
  surface — the `profile` event field, the four named profiles, the 4-layer precedence, and
  structured-output/`constrained_json` — commit `9323257`; (2) added `profile` to the workflow
  event-field docs in `docs/sdlc-flow-workflow.md` — commit `e4eabe3`; (3) refreshed index rows in
  `docs/index.md`/`planning/index.md` for the files delivered across Blocks A–D (standing rule 2) —
  commit `f343bdf`; (4) flipped `planning/sdlc-flow-policy-research/notes.md` from `draft` to
  `active` and cross-linked the delivered tests/harness deliverables — touched only the gitignored,
  brain-vaulted `planning/` tree, so produced no repo commit (expected); (5) validation — ran the
  full check suite (fmt/clippy/test/build), confirmed all green on a doc-only block — also
  planning-only, no repo commit.
- **Why:** Closes Block EN.3-plan.E, the last remaining block of the ad-hoc
  `plan-sdlc-policy-profiles` plan (see D34) — depended on Blocks A–D (structured-output hardening,
  named profiles, deterministic plumbing tests, and the real-CLI experiment harness merged via PR
  #10) all being closed first, per `planning/plan-sdlc-policy-profiles/plan.md`. With EN.3-plan.E
  closed, all five blocks (A–E) of the plan are now `closed` in `planning/state.json`, so
  `plan-sdlc-policy-profiles` is fully complete.
- **Status:** Block EN.3-plan.E closed (`planning/state.json` id `EN.3-plan.E` set to `"closed"`).
  Parent plan `plan-sdlc-policy-profiles` complete — no blocks remain open. `master-plan.md`'s own
  sequence (Phases 0–3) was already fully `Done` before this ad-hoc plan started; nothing else is
  queued there, so the next overall focus is open — to be planned.
- **Refs:** `planning/plan-sdlc-policy-profiles/plan.md`, `planning/plan-sdlc-policy-profiles-E/tasks.json`,
  commits `9323257`, `e4eabe3`, `f343bdf`

### PR #10 merged — `plan-sdlc-policy-profiles-D` (Block EN.2-plan.D) closed
- **What:** Merged PR #10 (https://github.com/bredmond1019/engine-rs/pull/10), branch
  `plan-sdlc-policy-profiles-D-flow`, into `main`. The PR carried the `sdlc-flow` run for spec
  `plan-sdlc-policy-profiles-D`: 3 tasks (all passed, attempt 1 each), a consolidated end-of-flow
  review (PASS, no findings on first attempt), and a docs update (`docs/sdlc-flow-policy.md`).
  Changed files: new `crates/engine-core/tests/sdlc_flow_experiment.rs` (505 lines) — the real-CLI
  experiment harness described in the session entry directly below — plus
  `docs/sdlc-flow-policy.md`.
- **Why:** Finalizes Block EN.2-plan.D (Real-CLI experiment harness, Part B) of the ad-hoc
  `plan-sdlc-policy-profiles` plan — the implementation/review work was done in this same session
  (see the sub-entry directly below); this entry is the PR merge that closes the block out,
  unblocking EN.3-plan.E (docs, index, and research-note wrap-up, the plan's last remaining block).
- **Status:** Block EN.2-plan.D is closed (`planning/state.json` already had `status: "closed"`
  for this block). The parent plan `plan-sdlc-policy-profiles` is *not* fully done — Block E
  remains.
- **Refs:** PR #10 (https://github.com/bredmond1019/engine-rs/pull/10),
  `planning/plan-sdlc-policy-profiles-D/tasks.md`

### `plan-sdlc-policy-profiles-D` (Block EN.2-plan.D) done — real-CLI experiment harness (Part B)
- **What:** Ran `/sdlc-flow` for spec `plan-sdlc-policy-profiles-D` on branch
  `plan-sdlc-policy-profiles-D-flow`, 3 tasks, all passed on attempt 1, consolidated review PASS
  with no findings. Added `crates/engine-core/tests/sdlc_flow_experiment.rs`: an
  `ExperimentSetupNode` that fixes `sdlc_flow_live.rs`'s policy-stamping shortcut (the fixture
  setup node there inserts the `SetupWorktreeNode` result directly, skipping
  `resolve_policy_for_run`, so no policy gets stamped) by calling `resolve_policy_for_run` and
  inserting `RESOLVED_POLICY_IDENTITY` into `ctx.nodes` directly (since `put_result` is
  `pub(crate)` and unreachable from an external `tests/` file). A synthetic 3-task fixture
  (happy path / first-fail-then-pass retry / trivial one-liner) drives a `#[ignore]`-gated test
  that runs all four named profiles (`baseline`, `cheap-fast`, `pragmatist`, `batch-reviewer`)
  through the full `SDLC_FLOW` graph, aggregates their `sdlc-flow-state.json` files via
  `aggregate::aggregate_state_files`, and prints a ranked table (sorted by ascending
  `avg_cost_usd`) covering cost/time/tokens/attempts/pass-rate, asserting more than one distinct
  resolved-policy row appears. Task 1 also added a non-`#[ignore]` scaffolding unit test
  (`experiment_scaffolding_builds_workflow_and_fixtures`) so the harness's construction is itself
  verified in the default gated suite. Task 3 was validation-only (fmt/clippy/test/build all
  green, `--test sdlc_flow_experiment -- --list` confirms both tests are present with the real-CLI
  one `#[ignore]`-gated) — no code changes, no commit.
- **Why:** Closes Block EN.2-plan.D of the ad-hoc `plan-sdlc-policy-profiles` plan (see D34),
  unblocking EN.3-plan.E (docs/index/research-note wrap-up), the plan's last remaining block.
- **Status:** Block EN.2-plan.D closed (`planning/state.json` id `EN.2-plan.D` flipped to
  `"closed"`). The parent plan `plan-sdlc-policy-profiles` is *not* fully done — Block E remains.
- **Refs:** `planning/plan-sdlc-policy-profiles-D/tasks.md`,
  `crates/engine-core/tests/sdlc_flow_experiment.rs`
- **Next:** `/generate-tasks` (or `/sdlc-flow`) against EN.3-plan.E — docs, index, and
  research-note wrap-up

```
414923c docs: update docs for plan-sdlc-policy-profiles-D
cbd3dda feat: implement plan-sdlc-policy-profiles-D-task2
2626b22 feat: implement plan-sdlc-policy-profiles-D-task1
d8544a9 docs: log-work for plan-sdlc-policy-profiles-C PR #9 merge
4b70a7e Merge pull request #9 from bredmond1019/plan-sdlc-policy-profiles-C-flow
75c4ff8 chore: wrap up plan-sdlc-policy-profiles-C
eb20351 feat: implement plan-sdlc-policy-profiles-C-task3
831da39 feat: implement plan-sdlc-policy-profiles-C-task2
```

---

## [run: 2026-07-19]

### PR #9 merged — `plan-sdlc-policy-profiles-C` (Block EN.2-plan.C) closed
- **What:** Merged PR #9 (https://github.com/bredmond1019/engine-rs/pull/9), branch
  `plan-sdlc-policy-profiles-C-flow`, into `main`. The PR carried the `sdlc-flow` run for spec
  `plan-sdlc-policy-profiles-C`: 4 tasks (all passed, attempt 1 each), a consolidated end-of-flow
  review (PASS, no findings on first attempt), and no docs changes needed. Changed files: new
  `crates/engine-core/tests/sdlc_flow_profiles.rs` (680 lines) — hermetic, deterministic plumbing
  tests proving profile resolution, inline-`policy`-over-`profile` precedence, unknown-profile
  errors, and policy-driven graph routing (trivial-skip review bypass, local-tier transport
  rewiring).
- **Why:** Finalizes Block EN.2-plan.C (Deterministic plumbing tests, Part A) of the ad-hoc
  `plan-sdlc-policy-profiles` plan — the implementation/review work was done in this same session
  (see the sub-entry directly below); this entry is the PR merge that closes the block out,
  unblocking EN.2-plan.D (real-CLI experiment harness, Part B).
- **Status:** Block EN.2-plan.C is closed (`planning/state.json` already had `status: "closed"`
  for this block). The parent plan `plan-sdlc-policy-profiles` is *not* fully done — Blocks D/E
  remain, in that dependency order.
- **Refs:** PR #9 (https://github.com/bredmond1019/engine-rs/pull/9),
  `planning/plan-sdlc-policy-profiles-C/tasks.md`,
  `crates/engine-core/tests/sdlc_flow_profiles.rs`
- **Next:** `/generate-tasks` (or `/sdlc-flow`) against EN.2-plan.D — real-CLI experiment harness
  (Part B).

### `plan-sdlc-policy-profiles-C` (Block EN.2-plan.C) — deterministic plumbing tests, all 4 tasks PASS
- **What:** Ran `/sdlc-flow plan-sdlc-policy-profiles-C` on branch `plan-sdlc-policy-profiles-C-flow`.
  Added `crates/engine-core/tests/sdlc_flow_profiles.rs`, a hermetic, in-suite test file proving:
  each of the four named profiles (`baseline`, `cheap-fast`, `pragmatist`, `batch-reviewer`)
  resolves to its documented `SdlcPolicy` (Task 1); inline-`policy` precedence over `profile` holds
  and an unknown profile name errors out of `resolve_policy_for_run` (Task 2); and the assembled
  `SDLC_FLOW` graph actually routes on policy — `cheap-fast`'s `TrivialSkip` mode skips
  `ConsolidatedReviewNode` on a trivial diff (contrasted with `baseline` still reaching it), and a
  `local`-tier review policy gets its transport genuinely rewired to a loopback HTTP stub, observed
  via a request-count spy and the `local/<model>` usage marker (Task 3). Task 4 ran the full gated
  validation suite (`cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`,
  `cargo build --release`) confirming all pass, including the 11 new tests.
- **Why:** Closes Block EN.2-plan.C (Deterministic plumbing tests, Part A) of the ad-hoc
  `plan-sdlc-policy-profiles` plan, unblocking EN.2-plan.D (real-CLI experiment harness, Part B).
- **Verdict:** PASS (consolidated end-of-flow review, no findings).
- **Notable decisions:** built a minimal in-process HTTP/1.1 stub server on `127.0.0.1:0` (tokio
  `TcpListener`, hand-rolled response) to exercise the real reqwest-based local transport
  hermetically rather than reimplementing it; reached the run-level unknown-profile error path via
  the public `engine_core::workflows::sdlc_flow::setup::resolve_policy_for_run` rather than only
  re-asserting `profile_by_name(...) == None`; introduced a `NamedProfileCtor` type alias to satisfy
  `clippy::type_complexity` in the profile round-trip test's case table.
- **Refs:** `planning/plan-sdlc-policy-profiles-C/tasks.md`,
  `planning/plan-sdlc-policy-profiles-C/sdlc/sdlc-flow-state.json`,
  `crates/engine-core/tests/sdlc_flow_profiles.rs`.
- **Next:** EN.2-plan.D — real-CLI experiment harness (Part B).

```
eb20351 feat: implement plan-sdlc-policy-profiles-C-task3
831da39 feat: implement plan-sdlc-policy-profiles-C-task2
6c5b615 feat: implement plan-sdlc-policy-profiles-C-task1
```

---

## [run: 2026-07-19]

### PR #8 merged — `plan-sdlc-policy-profiles-B` (Block EN.1-plan.B) closed
- **What:** Merged PR #8 (https://github.com/bredmond1019/engine-rs/pull/8), branch
  `plan-sdlc-policy-profiles-B-flow`, into `main`. The PR carried the `sdlc-flow` run for spec
  `plan-sdlc-policy-profiles-B`: 6 tasks (all passed, attempt 1 each), a consolidated end-of-flow
  review (PASS, no findings after one review-fix pass), and a docs patch
  (`docs/sdlc-flow-policy.md`, `docs/sdlc-flow-workflow.md`). Changed files: new
  `crates/engine-core/src/workflows/sdlc_flow/profiles.rs` (the four named `PartialPolicy`
  profiles + `profile_by_name` lookup), plus `mod.rs`, `policy.rs`, `schema.rs`, `setup.rs`.
- **Why:** Finalizes Block EN.1-plan.B (named profiles + first-class `profile:` field) of the
  ad-hoc `plan-sdlc-policy-profiles` plan — the implementation/review/docs work was done in this
  same session (see the sub-entry directly below); this entry is the PR merge that closes the
  block out, unblocking EN.2-plan.C (deterministic plumbing tests) and EN.2-plan.D (real-CLI
  experiment harness).
- **Status:** Block EN.1-plan.B is closed (`planning/state.json` already had `status: "closed"`
  for this block). The parent plan `plan-sdlc-policy-profiles` is *not* fully done — Blocks C/D/E
  remain, in that dependency order.
- **Refs:** PR #8 (https://github.com/bredmond1019/engine-rs/pull/8),
  `planning/plan-sdlc-policy-profiles-B/tasks.md`, `planning/plan-sdlc-policy-profiles-B/tasks.json`
- **Next:** `/generate-tasks` (or `/sdlc-flow`) against EN.2-plan.C / EN.2-plan.D — deterministic
  plumbing tests + real-CLI experiment harness (both now unblocked).

### `plan-sdlc-policy-profiles-B` (Block EN.1-plan.B) — Named profiles + first-class `profile:` field
- **What:** Ran the `sdlc-flow` workflow for spec `plan-sdlc-policy-profiles-B` on branch
  `plan-sdlc-policy-profiles-B-flow`, executing all 6 tasks (all passed, attempt 1 each) plus a
  consolidated review (PASS, no findings after one review-fix pass) and a docs patch
  (`docs/sdlc-flow-policy.md`, `docs/sdlc-flow-workflow.md`). Task 1 added
  `crates/engine-core/src/workflows/sdlc_flow/profiles.rs` with `baseline`/`cheap-fast`/`pragmatist`/
  `batch-reviewer` `PartialPolicy` constructors and a `profile_by_name` lookup. Task 2 added an
  additive `#[serde(default)] profile: Option<String>` field to `SDLCFlowEventSchema`. Task 3 gave
  `policy::resolve` a fourth profile layer between harness defaults and the event override
  (builtin → harness_defaults → profile → event_override). Task 4 wired
  `resolve_policy_for_run` to resolve `event.profile` via `harness.json`'s `sdlc.profiles` map first,
  then the built-in `profiles::profile_by_name`, erroring on unknown names. Task 5 added the
  `sdlc.profiles` map (all four named profiles) to `planning/harness.json` alongside the existing
  `sdlc.policy` no-op block, plus a documented example in `planning/harness.examples.md`. Task 6
  confirmed all four gated validation commands (`cargo fmt --check`, `cargo clippy -D warnings`,
  `cargo test`, `cargo build --release`) pass.
- **Why:** Closes out Block EN.1-plan.B of the ad-hoc `plan-sdlc-policy-profiles` plan, unblocking
  EN.2-plan.C (deterministic plumbing tests) and EN.2-plan.D (real-CLI experiment harness), both of
  which depended on the named-profile plumbing landing first.
- **Notable decisions:** `baseline()` spells out all model tiers + `review_mode: per_task` +
  `llm_triage: false` explicitly (rather than an all-`None` `PartialPolicy`) so selecting
  `profile: "baseline"` is a legible, self-documenting no-op; `resolve_profile` in `setup.rs` keeps
  the harness-profiles-then-builtin lookup and unknown-name error logic in one place;
  `planning/harness.json`/`harness.examples.md` edits landed via the company-brain symlink (not
  tracked by this repo's git, per the Symlink warning standing rule).
- **Status:** Block EN.1-plan.B is closed (`planning/state.json` flipped to `status: "closed"`).
  The parent plan `plan-sdlc-policy-profiles` is *not* fully done — Blocks C/D/E remain.

Next: EN.2-plan.C / EN.2-plan.D — deterministic plumbing tests + real-CLI experiment harness.

```
8858b34 docs: update docs for plan-sdlc-policy-profiles-B
56578bb fix: review pass 1 for plan-sdlc-policy-profiles-B
6a36828 feat: implement plan-sdlc-policy-profiles-B-task4
c944ce3 feat: implement plan-sdlc-policy-profiles-B-task3
d2cbcfc feat: implement plan-sdlc-policy-profiles-B-task2
7cb3e0b feat: implement plan-sdlc-policy-profiles-B-task1
b13ad69 docs: log-work for plan-sdlc-policy-profiles PR #7 merge
e93c13a Merge pull request #7 from bredmond1019/plan-sdlc-policy-profiles-flow
```

---

## [run: 2026-07-19]

### PR #7 merged — `plan-sdlc-policy-profiles` (Block EN.1-plan.A) closed
- **What:** Ran the `sdlc-flow` workflow for spec `plan-sdlc-policy-profiles` on branch
  `plan-sdlc-policy-profiles-flow`. It executed 4 tasks (all implemented and passed fast-tests),
  a consolidated review (PASS, no findings), and a docs patch (updated `docs/architecture.md`
  and `docs/sdlc-flow-workflow.md`). This produced PR #7
  (https://github.com/bredmond1019/engine-rs/pull/7), which has now been merged into `main`
  (merge commit fast-forwarded locally; `main` is up to date with `origin/main` plus the merge).
  Changed files in the merge: `crates/engine-core/src/nodes/claude_code_step.rs`,
  `crates/engine-core/src/nodes/openai_compat_transport.rs`,
  `crates/engine-core/src/workflows/sdlc_flow/docs.rs`,
  `crates/engine-core/src/workflows/sdlc_flow/setup.rs`,
  `crates/engine-core/src/workflows/sdlc_flow/task_loop.rs`, plus new/updated tests in
  `crates/engine-core/tests/` (`claude_code_step.rs`, `sdlc_flow_e2e.rs`, `sdlc_flow_live.rs`,
  `sdlc_flow_task_loop.rs`), and `docs/architecture.md`, `docs/sdlc-flow-workflow.md`.
- **Why:** This closes out Block EN.1-plan.A (structured-output hardening) of the ad-hoc
  `plan-sdlc-policy-profiles` plan — the implementation/review/docs work was done in the prior
  session (see the `[run: 2026-07-18]` entry below); this session is the PR merge that finalizes
  the block.
- **Status:** Block EN.1-plan.A is closed (`planning/state.json` already had `status: "closed"`
  for this block). The parent plan `plan-sdlc-policy-profiles` is **not** fully done — Blocks B
  (named profiles), C/D (deterministic tests + real-CLI experiment harness), and E (docs
  wrap-up) remain, in that dependency order.
- **Refs:** PR #7 (https://github.com/bredmond1019/engine-rs/pull/7),
  `planning/plan-sdlc-policy-profiles/plan.md`, `planning/plan-sdlc-policy-profiles/tasks.md`
- **Next:** `/generate-tasks` (or `/sdlc-flow`) against Block B — EN.1-plan.B, named profiles +
  first-class `profile:` field.

## [run: 2026-07-18]

### EN.1-plan.A — Structured-output hardening (`/sdlc-flow plan-sdlc-policy-profiles`)
- **What:** Surfaced `claude-code-rs`'s new `Outcome.structured_output` through the shared
  `ClaudeCodeStep` seam (`crates/engine-core/src/nodes/claude_code_step.rs`), writing it into
  `ctx.nodes[name]["structured"]` alongside `content`/`cost_usd`/`model`, and fixed every
  `Outcome { .. }` literal crate-wide (9 files) that would otherwise fail to compile against the
  new struct field. All five model-JSON parse sites in `sdlc_flow` — `ImplementTaskNode`,
  `TriageTaskNode`'s llm branch, `ConsolidatedReviewNode` (`task_loop.rs`), `GenerateTasksNode`
  (`setup.rs`), and `PatchDocsNode` (`docs.rs`) — now set `config.json_schema` matching their
  output struct and prefer the pre-parsed `structured` value, falling back to the existing
  `strip_json_fence` + `serde_json::from_str` path when structured output is absent or null.
  Verdict routing's `.trim().to_uppercase()` normalization was retained unchanged. Ran the full
  gated validation suite (`cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`,
  `cargo build --release`) clean. All 4 tasks passed on first attempt; end-of-flow review verdict
  PASS with no findings; docs stage patched `docs/architecture.md` and `docs/sdlc-flow-workflow.md`.
- **Why:** This is Block A of the `plan-sdlc-policy-profiles` ad-hoc plan
  (`planning/plan-sdlc-policy-profiles/plan.md`) — two of the real production bugs already fixed
  in the SDLC-flow port (JSON-fence wrapping, lowercase `"pass"` verdicts) are exactly what
  schema-constrained output prevents at the source, so this had to land before the Phase 2
  experiments (Blocks C/D) measure *policy* effects rather than parse flakiness.
- **Decisions:** A shared `parse_structured_or_fenced<T>` helper was added per-file (duplicated
  into `task_loop.rs`, `setup.rs`, and `docs.rs` rather than promoted to `mod.rs`) to keep each
  task's file-touch scope narrow. Task 1 also fixed 5 `Outcome{..}` literals beyond the 4 files
  named in the spec's file list, since the acceptance criteria required crate-wide compilation.
- **Status:** Block A (EN.1-plan.A) is Done. The parent plan `plan-sdlc-policy-profiles` is
  **not** fully done — Blocks B (named profiles), C/D (deterministic tests + real-CLI experiment
  harness), and E (docs wrap-up) remain, in that dependency order.
- **Refs:** `planning/plan-sdlc-policy-profiles/plan.md`, `planning/plan-sdlc-policy-profiles/tasks.md`,
  `planning/plan-sdlc-policy-profiles/sdlc/sdlc-flow-state.json`
- **Next:** `/generate-tasks` (or `/sdlc-flow`) against Block B — EN.1-plan.B, named profiles +
  first-class `profile:` field.

```
6151b37 docs: update docs for plan-sdlc-policy-profiles
d48ffc4 feat: implement plan-sdlc-policy-profiles-task4
272cf10 feat: implement plan-sdlc-policy-profiles-task3
1b6a8c8 feat: implement plan-sdlc-policy-profiles-task2
bc386ab feat: implement plan-sdlc-policy-profiles-task1
```

### SDLC Flow Policy Research + Test Planning
- **What:** Reviewed the new SDLC Flow policy and workflow documentation (`docs/sdlc-flow-policy.md`, `docs/sdlc-flow-workflow.md`). Summarized the available configuration knobs for adjusting cost, time, and quality. Designed four initial test profiles (Baseline, Cheap & Fast, Pragmatist, Batch Reviewer) to determine the ideal configuration. Created a research note at `planning/sdlc-flow-policy-research/notes.md` to hold these test profiles and discussed next steps with the user regarding test execution methods and optimization metrics. Wrote `planning/handoff.md` and committed changes to cleanly hand off for test creation.
- **Why:** The `SDLC_FLOW` now has a tunable `SdlcPolicy`. We need to empirically test these settings to find the optimal configuration for the agentic loops.
- **Refs:** `planning/sdlc-flow-policy-research/notes.md`, `planning/handoff.md`

### SDLC Flow reference docs + live integration test (bugfixes)
- **What:** Wrote SDLC Flow reference docs (`docs/sdlc-flow-policy.md`, `docs/sdlc-flow-workflow.md`)
  and a live integration test (`crates/engine-core/tests/sdlc_flow_live.rs`) exercising real Claude
  Code CLI calls through `ImplementTaskNode`, `TriageTaskNode`'s `llm_triage` branch, and
  `ConsolidatedReviewNode`. Writing that test surfaced and fixed 5 real production bugs:
  1. `ImplementTaskNode`/`ConsolidatedReviewNode` were missing `Config.cwd` (real calls ran in the
     wrong directory).
  2. No headless tool-permission wiring — added `Config.dangerously_skip_permissions` to
     `claude-code-rs`.
  3. Real replies wrap JSON in markdown fences — added `strip_json_fence` at all 5 model-output
     parse sites.
  4. Case-sensitive verdict routing would silently dead-end a run on a lowercase `"pass"`/`"fail"`
     reply — normalized to uppercase after parsing.

  (Narrative counts "5 real production bugs" but only 4 are enumerated above — logging the
  discrepancy as-is rather than inventing a fifth.) All fixes are covered by new hermetic unit
  tests; the full gated suite is green in both engine-rs and claude-code-rs (155 lib tests, up
  from 147). Cleared 3 stale `state.json` carryover entries whose conditions had already resolved
  (`en3a-retry-loop-no-bail`, `sdlc-flow-seam-duplication`, `local-llm-tier-investigation`) and
  added one new one: `review-retry-no-attempt-cap` — a discovered-but-unfixed gap where the
  review↔implement retry loop has no attempt cap independent of `TriageTaskNode`'s test-failure
  path.
- **Why:** Needed real (non-mocked) CLI-call coverage for the SDLC Flow task loop to validate the
  policy/routing work landed in EN.3.C actually holds up against a live Claude Code CLI, and to
  document the workflow + policy surface for future blocks.
- **Refs:** `docs/sdlc-flow-policy.md`, `docs/sdlc-flow-workflow.md`,
  `crates/engine-core/tests/sdlc_flow_live.rs`, `planning/state.json` (carryover updates)

### EN.3.C — Tunable run-policy config + experiment telemetry (PASS)
Implemented the three-layer `SdlcPolicy` (event override > `harness.json` `sdlc.policy` defaults >
built-in default, precedence-tested) in a new `policy.rs`, and wired its resolution into
`SetupWorktreeNode` (stamped into ctx as `ResolvedPolicy`). `ImplementTaskNode`, `TriageTaskNode`'s
llm-triage branch, and `ConsolidatedReviewNode` now consume the resolved policy for per-stage model
tiers, `output_verbosity` prompt directives, and a `prompt_cache` system-prompt anchor, falling back
to today's hardcoded defaults when no policy is present. Added deterministic trivial-task
classification (`git diff --numstat` against `review_skip_max_files`/`review_skip_max_diff_lines`)
and made `TriageRouterNode` honor `review_mode` (`per_task` | `trivial_skip` | `end_only`) to decide
whether a passing task still routes to `ConsolidatedReviewNode`. Added `openai_compat_transport` — a
local (Ollama-style) `ModelTransport` for the `local` tier that POSTs to an OpenAI-compatible
endpoint, synthesizes a zero-cost `Outcome`, and fails fast + falls back to the cloud transport on
error — wired via a new `registry_for_policy(&SdlcPolicy)` that rewires triage/review (never
`ImplementTaskNode`) when their resolved tier is `Local`. Extended `SDLCState` with a resolved-policy
snapshot + `RunOutcomes` (wall-clock, attempt/retry counts, pass/fail, review verdicts, tokens, cost,
per-stage tier used), finalized deterministically by `WrapUpNode`. Added `aggregate.rs`, a cross-run
aggregator grouping state files by policy and tabulating cost/tokens/time/attempts/pass-rate per
distinct policy. The first review pass caught a real gap: `WrapUpNode` computed the policy/outcomes
blocks but only stamped them into the transient ctx output — `SaveStateNode`, which runs earlier in
the graph and never re-runs after `WrapUpNode`, is the only node that persists `SDLCState` to the
on-disk `sdlc-flow-state.json`, so the durable file never received the new telemetry. This was fixed
in a review-fix pass; task 8's full gated suite (fmt/clippy/test/release build) is green with no
further code changes needed. Final verdict: PASS (all 8 tasks passed). EN.3.C's block entry doesn't
yet exist in `planning/state.json` (only through EN.3.B), so no authored block flip was made there.
Next: pick the next Phase 3/4 block per `master-plan.md`.

```
4ce86ac fix: review pass 1 for EN.3.C-tunable-run-policy-telemetry
e78ae78 feat: implement EN.3.C-tunable-run-policy-telemetry-task7
da568bb feat: implement EN.3.C-tunable-run-policy-telemetry-task6
16f512f feat: implement EN.3.C-tunable-run-policy-telemetry-task5
039dd0d feat: implement EN.3.C-tunable-run-policy-telemetry-task4
60e41f7 feat: implement EN.3.C-tunable-run-policy-telemetry-task3
5b88e53 feat: implement EN.3.C-tunable-run-policy-telemetry-task2
efaa644 feat: implement EN.3.C-tunable-run-policy-telemetry-task1
```

### EN.3.B — SDLC-flow docs/wrapup/PR port merged
- **What:** Closed out EN.3.B-sdlc-flow-docs-wrapup-pr: docs step required no changes (nothing
  stale after the 8-task implementation + PASS review). Opened PR #5
  (https://github.com/bredmond1019/engine-rs/pull/5). CI on the PR failed on the fmt/clippy/test/
  build job, but investigation confirmed this is pre-existing and unrelated to this PR — the Cargo
  workspace carries a path dependency on `../claude-code-rs` that isn't checked out in CI, and the
  last 2 CI runs on `main` failed with the identical error before this PR existed. Squash-merged
  PR #5 into main as commit `c5bddce`, deleted the remote branch, then rebased local `main` onto
  `origin/main`, resolving a `log.md` conflict by keeping both the new EN.3.B entry and the
  previously-committed economics-analysis entry (EN.3.B listed first, newer). Local `main` is now
  clean and in sync with `origin/main`.
- **Why:** Continuing EN.3.A's SDLC-flow port work; EN.3.B was the last block before EN.3.C
  (tunable run-policy config + experiment telemetry), which is next per the master-plan.
- **Refs:** PR #5 (https://github.com/bredmond1019/engine-rs/pull/5), commit `c5bddce`, spec
  `EN.3.B-sdlc-flow-docs-wrapup-pr`

### EN.3.B — SDLC-flow docs/wrap-up/PR port + parity acceptance (PASS)
Ported the bottom half of the SDLC-flow pipeline into `engine-core::workflows::sdlc_flow` across
8 tasks, all passing first-attempt. Tasks 1-2 hoisted the duplicated `put_result`/`get_result` +
`CommandOutput`/`CommandRunner`/`ModelTransport`/`default_command_runner` seams into `mod.rs` and
built `PatchDocsNode` (Sonnet-backed, parses model JSON for stale-docs patches). Tasks 3-4 added the
deterministic tail nodes `WrapUpNode` (template-rendered PASS/PARTIAL-FAIL report via an injectable
clock seam, no model call), `PullRequestNode` (git push + `gh pr create` via the runner seam, D25
no-auto-merge, no-op when `auto_pr` is false), and `EmitStateNode` (`mev emit-state --write` via the
runner seam). Task 5 fixed the EN.3.A retry-bail known-issue: added `IncrementAttemptNode` as the
real target of both retry back-edges (`TriageRouterNode` RETRYABLE, `ReviewRouterNode` minor
FAIL/PARTIAL) and fixed a latent bug where `TriageTaskNode` read a frozen dispatch-time attempt
count instead of live state, so the bail gate had never actually fired. Task 6 assembled the full
`SDLC_FLOW` graph in `graph.rs` (17 node identities, declared-acyclic per D42, back-edges runtime-
only), replacing the old terminal `PatchDocsNode` stub. Task 7 added a hermetic end-to-end
integration test (`tests/sdlc_flow_e2e.rs`) driving the whole assembled graph — happy path (both
`auto_pr` values), the never-passing-task retry-bail path, and durable `EventsRow` round-trip
parity — and surfaced (but did not fix, out of scope) a related finding: `WrapUpNode`'s own
`latest_state` doesn't consider `IncrementAttemptNode`'s output, so it renders a stale PASS-looking
report on the MAJOR_BAIL path. Task 8 ran the full validation suite (fmt/clippy/test/build) clean.
Final verdict: PASS. Both EN.3.A review findings from the prior session (retry-bail, seam
duplication) are now resolved. Next: EN.3.C — tunable run-policy config + experiment telemetry.

```
e48bb38 feat: implement EN.3.B-sdlc-flow-docs-wrapup-pr-task7
b23c8d0 feat: implement EN.3.B-sdlc-flow-docs-wrapup-pr-task6
55dd7ec feat: implement EN.3.B-sdlc-flow-docs-wrapup-pr-task5
0916345 feat: implement EN.3.B-sdlc-flow-docs-wrapup-pr-task4
1f6315a feat: implement EN.3.B-sdlc-flow-docs-wrapup-pr-task3
5ce2060 feat: implement EN.3.B-sdlc-flow-docs-wrapup-pr-task2
29a9c30 feat: implement EN.3.B-sdlc-flow-docs-wrapup-pr-task1
f6bab3c docs: log SDLC economics analysis + EN.3.A review findings
```

### SDLC token/time economics analysis + EN.3.A review findings; EN.3.B/EN.3.C scoping
- **What:** Ran a token/cost/time economics analysis of the SDLC pipeline (engine-rs
  deterministic-node port vs. the 100%-agent-led `sdlc-flow.js`), grounded in real telemetry from
  66 past flow runs — captured in `planning/sdlc-token-time-economics/notes.md`. Ranked improvement
  levers (#2a coding-output terseness ~$14.6/66runs is the biggest cost+time lever; #1 close-out
  redundancy dedup ~$10; #3 tier/skip gates; #2b prompt caching secondary). Designed the run-tail
  structured-output approach (push judgment into loop-node structured output so wrap-up/PR/handoff/
  emit-state are 0-model-token template renders) and confirmed `claude-code-rs` has no native
  schema-constrained output (must prompt-and-parse). EN.3.A shipped via `/sdlc-flow` (tasks 1-7 all
  PASS first-attempt); reviewed the committed code and found two issues: the retry loop has no
  attempt-based bail (`attempt_count` never increments on the RETRYABLE back-edge), and the
  `put_result`/runner/transport seams are duplicated across `setup.rs`/`task_loop.rs`. Updated
  master-plan: added a deterministic `EmitStateNode` to EN.3.B + logged the retry-bail fix there,
  and added a new EN.3.C block (tunable run-policy config + experiment telemetry — levers #1-#3 as
  per-run dials). Wrote handoff prompting the next agent to review this work and investigate a
  local LLM on a 32GB M2 as a model tier vs. dropping to Haiku.
- **Why:** The user asked how much the deterministic-node port saves and how to push cost/time/
  limits further; the analysis reframed the endgame (the money+time live in the expensive Sonnet
  coding path + close-out redundancy, not the cheap Haiku stages the deterministic nodes replace).
  The review findings and new blocks turn that into actionable roadmap work.
- **Refs:** `planning/sdlc-token-time-economics/notes.md`, `master-plan.md` EN.3.B/EN.3.C,
  `planning/handoff.md`

### Ported the SDLC-flow top half (setup + task loop) into engine-core
- **What:** Ran EN.3.A-sdlc-flow-setup-task-loop through `/sdlc-flow` (tasks 1-7, all passed on
  first attempt, PASS review). Task 1 ported the SDLC schema types (`SDLCTaskStatus`/
  `SDLCTriageVerdict`/`SDLCReviewVerdict`/`SDLCTask`/`SDLCFlowEventSchema`/`SDLCTelemetry`/
  `SDLCState` + `parse_task_range`) into `crates/engine-core/src/workflows/sdlc_flow/schema.rs`
  and scaffolded the module tree. Task 2 implemented the setup-half nodes (`SetupWorktreeNode`,
  `SpecExistsRouterNode`, `GenerateTasksNode`, `LoadTaskStateNode`) in `setup.rs`. Task 3
  implemented the task-loop nodes/routers (`TaskQueueRouterNode`, `ImplementTaskNode`,
  `TestTaskNode`, `TriageTaskNode`, `TriageRouterNode`, `ConsolidatedReviewNode`,
  `ReviewRouterNode`, `UpdateTaskStatusNode`, `SaveStateNode`) in `task_loop.rs`, ported from the
  Python orchestrator with the deterministic-by-default / few-model-calls split (only Implement +
  Review always call a model; Triage's model branch gates on `event.llm_triage`; Generate is
  fallback-path only). Task 4 assembled the `SDLC_FLOW` `WorkflowSchema` + `NodeRegistry` in
  `graph.rs`, wiring all 13 top-half nodes plus a terminal `PatchDocsNode` stub, passing
  `WorkflowValidator` (declared-acyclic, runtime back-edges supplied by `Router::route`). Task 5
  registered the schema+workflow under `workflow_type = "SDLC_FLOW"` in engine-serve's dual
  `Dispatcher` registry. Task 6 added a hermetic integration test
  (`crates/engine-core/tests/sdlc_flow_task_loop.rs`) driving a fail-then-pass retry back-edge
  through stubbed transports/runners, with an amendment documenting that
  `SDLCTelemetry.total_attempts` increments once per completed task (not per retry) — verified
  against both the Rust and Python node logic. Task 7 confirmed the full validation suite
  (`cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, `cargo build --release`)
  green with no further changes needed.
- **Why:** Continues the Phase 3 SDLC-flow port (EN.3.A) per `master-plan.md`, porting the
  top half of the 16-node Python `sdlc_flow_workflow` pipeline into the native Rust engine ahead
  of EN.3.B (docs/wrap-up/PR bottom half + parity acceptance). Next: EN.3.B.
- **Refs:** `planning/EN.3.A-sdlc-flow-setup-task-loop/tasks.md`,
  `planning/EN.3.A-sdlc-flow-setup-task-loop/sdlc/worklog.md`, PR #4
  (https://github.com/bredmond1019/engine-rs/pull/4)

```
3dcd50f feat: implement EN.3.A-sdlc-flow-setup-task-loop-task6
0c6cddb feat: implement EN.3.A-sdlc-flow-setup-task-loop-task5
111118f feat: implement EN.3.A-sdlc-flow-setup-task-loop-task4
62c578c feat: implement EN.3.A-sdlc-flow-setup-task-loop-task3
07eb735 feat: implement EN.3.A-sdlc-flow-setup-task-loop-task2
d5b4fea feat: implement EN.3.A-sdlc-flow-setup-task-loop-task1
1f18ee3 docs: close-out ticket-engine-store-round-trip-false-green
472c492 feat: implement ticket-engine-store-round-trip-false-green-task2
```

---

## [run: 2026-07-17]

### Fixed engine-store timestamp decode and killed the false-green Postgres round-trip test
- **What:** Ran `ticket-engine-store-round-trip-false-green` through `/sdlc-task` (authoring
  `breakdown.md` + `tasks.json` first, since the ticket had only a `/ticket`-authored `tasks.md`).
  Task 1 fixed `engine_store::get_event`'s `ColumnDecode` failure by reading `created_at`/`updated_at`
  as `NaiveDateTime` and converting with `.and_utc()`, since the orchestrator's `events` table columns
  are `timestamp without time zone` (contract §4) while `EventsRow`'s public fields stay `DateTime<Utc>`
  — the D20 contract surface is untouched, the fix lands purely on the consumer side. Added a non-live
  unit test covering the conversion. Task 2 killed the false green in
  `crates/engine-store/tests/postgres_round_trip.rs`: added `#[ignore]` to
  `insert_then_read_round_trips_an_events_row` and hardened the `DATABASE_URL` guard from a silent
  early `return` (which reported `1 passed` even though the test never ran) to a hard `.expect(...)`
  failure, and rewrote the header comment to document the `--ignored` contract. Ran `/close-out`:
  all gates green, `cargo test` with `DATABASE_URL` unset now reports the round-trip as `ignored`
  (never `passed`), coverage scan found no blocking gaps, `/code-review low` found no findings,
  `/update-docs --patch` made one surgical fix to `docs/architecture.md`'s module map. Wrote
  `planning/handoff.md` pointing at EN.3.A as the next unblocked block.
- **Why:** `get_event` could not decode the real orchestrator-owned `events` table at all (a live
  `ColumnDecode` failure against `orchestration_dev` on both sqlx 0.8 and 0.9), and the test that
  should have caught this had never actually run in CI since EN.0.B — an accepted block's acceptance
  criterion was silently unverified. Found during BA.7.C's decomposition; not a blocker for EN.3.A/EN.3.B.
- **Refs:** `planning/ticket-engine-store-round-trip-false-green/tasks.md` (+ `breakdown.md`/`tasks.json`
  committed to the brain repo), `orchestrator/docs/data-contract.md` §4, `planning/handoff.md`.

```
472c492 feat: implement ticket-engine-store-round-trip-false-green-task2
d30a43a feat: implement ticket-engine-store-round-trip-false-green-task1
```

---

## [run: 2026-07-16]

Implemented EN.2.B — cancellation token, abort endpoint, and cost/token budget gate — across 7 tasks, PASS review. Task 1 added `CancellationToken` (backed by `tokio::sync::watch`, using `send_replace` rather than `send` to avoid a silent no-op/deadlock when `cancel()` races a subscriber, decision D6) and `stamp_cancelled` metadata merging; promoted tokio to a real `[dependencies]` entry in `engine-core`. Task 2 added `Budget`/`BudgetLedger` (`crates/engine-core/src/budget.rs`) as a pre-dispatch check gate accumulating tokens/cost, absent-config always-allow. Task 3 wired both into a new `Workflow::run_with(event, on_progress, RunOptions)` entry point that checks cancellation and budget at each node boundary before dispatch and stamps the halt reason into `TaskContext::metadata`, leaving `run()` unchanged for existing callers. Task 4 gave `ClaudeCodeStep` an optional token raced via `tokio::select!` against its transport future, so mid-flight cancel drops the future and returns `Ok(ctx)` unchanged rather than a `NodeError`. Task 5 added an authenticated `POST /events/{run_id}/abort` endpoint backed by a new per-run `RunRegistry`, covering 401/404/success plus a concurrency race test. Task 6 registered both surfaces in the canonical orchestrator data contract at v1.1.0 and re-pinned bastion's consumer doc from 1.0.0 straight to 1.1.0 (resolving prior drift), reconciling the "observers, never writers" prose with D25's write-side abort trigger — no engine-rs source changed. Task 7 confirmed all four gates (fmt, clippy `-D warnings`, test, release build) green on the branch. Notable decisions: `NodeRunStatus` stays `pending|running|success|failed` per the contract's minor-bump constraint — cancellation is spelled in `TaskContext::metadata`, not a new status variant; a budget cap is enforced reached-before-dispatch (>=, not strictly >). Next: EN.3.A — SDLC-flow setup + task loop port.

```
a769526 docs: update docs for EN.2.B-cancellation-abort-budget
3997922 feat: implement EN.2.B-cancellation-abort-budget-task5
68184cf feat: implement EN.2.B-cancellation-abort-budget-task4
fceb544 feat: implement EN.2.B-cancellation-abort-budget-task3
dcd876b feat: implement EN.2.B-cancellation-abort-budget-task2
fefe24b fix: fix pass 1 for EN.2.B-cancellation-abort-budget-task1
4c92dbe feat: implement EN.2.B-cancellation-abort-budget-task1
```

### Close-out: EN.2.B cancellation-abort-budget
- **What:** validation suite green (fmt/clippy/test/build/emoji), coverage scan clean, code-review low found no findings, docs audit confirmed already current, handoff.md written pointing at EN.3.A.
- **Why:** close-out gate for the just-completed EN.2.B block, to safely hand off to the next session/block.
- **Refs:** PR #3, planning/handoff.md.

---

## [2026-07-16]

### Closed carryover `orchestrator-contract-conformance` Gap 1 — round_trip.rs now asserts against a real orchestrator-emitted fixture
- **What:** Repointed `crates/engine-contract/tests/round_trip.rs` at a real, orchestrator-emitted `task_context` fixture instead of one this crate had hand-authored about itself during EN.0.B. Test (a) (`fixture_round_trips_with_no_field_or_casing_drift`) now deserializes the fixture as `TaskContext` (not `EventsRow` — the orchestrator emits/owns a `task_context` value, not a full DB row; the row-shape assertion is test (b), unchanged and still synthetic-data-only). The old hand-authored fixture (`tests/fixtures/python_task_context.json` — hand-typed UUID, round-number timestamps, a `ticket_id`/`title` naming EN.0.B itself) was already gone from this repo's history (removed by an unrelated prior commit `b057dae`, an automated harness sync — not part of this session's work, just confirmed absent). Added `tests/fixtures/research_agent_task_context.json` — a checked-in copy of the real fixture the orchestrator repo emits via its `scripts/emit_task_context_fixture.py` (captured from an actual `ResearchAgentWorkflow` run, not hand-authored) — plus `tests/fixtures/README.md` as the provenance record: what it replaced and why, and the "owner-local + checked-in copy" pattern this repo and orchestrator settled on. Added a third test, `fixture_matches_orchestrator_owned_original_when_sibling_checkout_present`, implementing that pattern: diffs this crate's copy against `../orchestrator/tests/fixtures/task_context/research_agent_task_context.json` (a sibling checkout under the same parent directory) byte-for-byte, and skips silently if the sibling isn't present, so this crate stays standalone-clonable. Full `cargo test` (all crates, 23 tests including the 3 in `round_trip.rs`) verified green. Committed as `33dca04`.
- **Why:** This closes Gap 1 of the `orchestrator-contract-conformance` carryover (`planning/orchestrator-contract-conformance/notes.md`) — this crate's byte-for-byte conformance claim against the orchestrator data contract (`orchestrator/docs/data-contract.md` v1.0.1) was asserted but unverified, since the fixture it tested against was written by this crate about itself rather than captured from the orchestrator. Structurally identical to the `claude-code-rs` CLI schema drift fixed via that repo's D2. The carryover's `clears_when` condition — "round_trip.rs asserts against a task_context fixture emitted by a real orchestrator run rather than one authored by engine-rs" — is now met. **Gap 2 remains open** — it's separate, lower-priority docs debt (engine-rs has no `docs/data-contract.md` consumer re-pin doc mirroring bastion's) and was explicitly out of scope this session.
- **Refs:** `planning/orchestrator-contract-conformance/notes.md`; `core/_planning/orchestrator/task-context-fixture/notes.md` (producer half); `orchestrator/docs/data-contract.md` v1.0.1; commit `33dca04`

---

### Fixed the claude-code-rs parser against real captured CLI fixtures — EN.2.A's PARTIAL resolved, EN.2.B unblocked
- **What:** Took the priority handoff asking for a versioned D20-style data-contract between `engine-rs` and `core/claude-code-rs`, and **rejected its framing** after investigation — then fixed the actual bug. Three findings drove the reversal. (1) **The handoff aimed at the wrong boundary.** `engine-rs` consumes `claude-code-rs` via a Cargo *path dependency* and builds `Outcome` as a struct literal in three places, so `rustc` already enforces that seam more strictly than prose could — and you cannot pin a path dep, which makes a "re-pin doc" incoherent. The boundary that silently rotted is `claude-code-rs` ↔ the **vendor CLI**: unowned, no compiler, runtime JSON. (2) **Documentation was never the missing artifact — ground truth was.** The CLI schema was asserted in six places (parse.rs module doc, 3 unit tests, `tests/parse_schema.rs`, `docs/api.md`, `docs/architecture.md`, `knowledge.md`); all six agreed with each other, all six were wrong, because every fixture had been hand-written to match the parser rather than captured from the CLI. The tests were tautologies. A seventh document would have rotted identically. (3) **A `mev --pins` validator would have caught neither bug** — it checks that two documents agree on a version string, and both bugs were documents agreeing with each other while disagreeing with reality.
  **Phase 0 (the gate):** captured real `claude` 2.1.211 output by hand before writing any code. Confirmed the core assumptions (no top-level `model`; no top-level `content`; text in `result`; `modelUsage` keyed by model) and caught three things no hand-written fixture would have surfaced: **`subtype` lies** (the error envelope reports `subtype: "success"` alongside `is_error: true`, so the planned `subtype != "success"` check was wrong — `is_error` is the only trustworthy signal); **two distinct failure modes** (a CLI failure leaves stdout empty with the message on stderr; an API failure returns a well-formed envelope with an **empty stderr** and the message inside `result` — so the planned `Error::Cli { stderr }` would have surfaced an empty string, and the exit code cannot distinguish them); and **`modelUsage` carries per-model `costUSD`**, a better attribution tiebreak than output tokens and free input for EN.2.B's budget gate.
  **Phase 1 (`claude-code-rs`, commit `7daab1c`):** committed both captures as fixtures (only `session_id`/`uuid` redacted) plus `tests/fixtures/README.md` as the provenance record — capture command, redaction list, re-capture procedure, and a "we depend on / we deliberately ignore" table (the knowledge that lives nowhere in code). Rewrote `parse.rs`: deleted `ContentBlock` + its helper + custom `Deserialize` (~50 lines and 3 tests defending a shape the CLI **never emitted** — top-level `content` was invented by the first fixture author); `Outcome` now mirrors the wire with `model_usage: BTreeMap` (`BTreeMap`, not `HashMap` — `HashMap`'s per-process iteration randomization would make the tiebreak silently flaky), required `text` from `result`, `is_error`, `api_error_status`; `primary_model()` is a documented *heuristic* (cost → output tokens → key order), not a field disguised as CLI ground truth. Established a leniency rule: **required when absence is indistinguishable from a legitimate value** (`text` — a default would render its removal as an empty reply, i.e. silent data loss), **defaulted when absence merely costs detail** (`api_error_status`). Split error handling into `Error::Cli`/`Error::Api` per the real capture. Fixed 4 sleeper tests that smuggled assertions through `outcome.model` (a field that never existed) plus 2 more in `tests/isolation.rs` that neither exploration had found. Replaced `tests/parse_schema.rs` wholesale with conformance tests over the real fixtures + an `#[ignore]`d **drift canary** that diffs live CLI output against the fixture, failing in both directions — and **verified the canary actually fires** by doctoring the fixture (a canary that can't fail is the very sin being fixed). Decision `D2-cli-schema-provenance.md`; purged the fabricated schema from `docs/api.md`, `docs/architecture.md`, `knowledge.md`.
  **Phase 2 (`engine-rs`, commit `4c0a950`):** consumer update, run back-to-back because the path dep meant Phase 1 broke `engine-rs`'s build the instant it landed (a worktree would have made this *worse* — `../claude-code-rs` resolves to the main checkout regardless, so the consumer would be unvalidatable until merge; hence the deliberate deviation from the approved plan's `/sdlc-flow` + PR). `text_output()` collapsed away; `model` now comes from `primary_model().unwrap_or(UNKNOWN_MODEL)` — the fallback lives at the seam rather than loosening `engine_contract::Usage::model` (a required `String` mirroring orchestrator's contract v1.0.1) for a vendor quirk. Added tests for the `unknown` fallback and multi-model attribution. **All four gates green (fmt, clippy `-D warnings`, 72 tests, release build), and EN.2.A's live acceptance test `live_claude_code_step_produces_populated_usage` now passes against a real Claude Code session** — the failure was never an engine-rs defect, exactly as D4's transport boundary predicted.
  Also **found the same disease in the orchestrator seam** and ticketed it: `engine-contract/tests/round_trip.rs` asserts engine-rs's "byte-for-byte v1.0.1" claim against `tests/fixtures/python_task_context.json`, which despite its name was hand-authored by the Rust side during EN.0.B (hand-typed UUID, round-number timestamps, and `ticket_id: "T-142"`/`title: "Add data-contract serde types"` — the name of EN.0.B itself). The orchestrator has no such fixture and no Python test asserts the shape, so the test proves engine-rs is self-consistent, not that it matches Python.
- **Why:** EN.2.B is a cost/token budget gate that reads cost and usage straight off `Outcome`, so it could not be built correctly on a seam whose parser hard-failed — contract-first was right even though the handoff reached that conclusion via imprecise reasoning. The durable output is not a document but a **convention**: *the counterparty produces the fixture, the consumer's test parses it, the doc records provenance.* Applied to the CLI seam now; ticketed for the orchestrator seam. Deliberately **not** built: a `cli-contract.md` (a contract needs two consenting parties; Anthropic never agreed to one, so semver/changelog/re-pin are inert, and a version number would imply verification the doc cannot perform), a `core/docs/contracts/` registry (at 5 docs it is a hand-maintained cache of a `grep` — a new drift surface on a drift-prevention project), and the `mev --pins` pass (per the reasoning above). Rationale for each is recorded in D2's Rejected Alternatives.
- **Refs:** `claude-code-rs` commit `7daab1c` + `planning/decisions/D2-cli-schema-provenance.md` + `tests/fixtures/README.md`; `engine-rs` commit `4c0a950`; `planning/state.json` (both carryovers resolved — note `claude-code-rs-engine-rs-data-contract-design` was resolved by *rejecting its premise* per D2, not by satisfying its `clears_when`; new carryover `orchestrator-owned-task-context-fixture`); brain `planning/backlog.md` (2 new tickets); `planning/handoff.md` consumed and deleted

---

### Investigated EN.2.A's PARTIAL root cause — claude-code-rs schema drift, priority handoff for a data-contract redesign
- **What:** Investigated the root cause of the upstream `claude-code-rs` parser bug behind EN.2.A's PARTIAL verdict. Ran `claude -p "reply with the single word: ok" --output-format json` directly to see the real CLI output shape, and compared it against `core/claude-code-rs/src/parse.rs`'s `Outcome` struct (which expects `total_cost_usd`, `usage`, `model: String`, `content: Vec<ContentBlock>`). Found the CLI's JSON schema has drifted in two ways since that parser was written: (1) no top-level `model` field anymore — the model name is now a key inside a `modelUsage: {"<model>": {...}}` object; (2) no top-level `content` blocks array anymore — response text now lives in a top-level `result: String` field instead. The second drift is the more dangerous one: it silently degrades to empty output even once the `model` field is fixed, because `content` carries `#[serde(default)]` and swallows the mismatch rather than failing loudly. Expanded the existing `planning/state.json` carryover entry `claude-code-rs-parser-missing-model-field` with these full findings, and added a new carryover entry `claude-code-rs-engine-rs-data-contract-design` (kind: `deferred`, `cross_repo: true`) capturing the ask to design a versioned data-contract between `engine-rs` and `core/claude-code-rs`, mirroring the D20/D47 orchestrator↔bastion pattern. Rewrote `planning/handoff.md` in full, framed as a priority handoff for an Opus-tier agent to design that contract before picking up EN.2.B. Re-ran `mev emit-state --write` as an idempotency safety check.
- **Why:** EN.2.A's PARTIAL verdict was provisionally attributed to a narrow upstream parser bug (a single missing `model` field); this session's direct CLI probe revealed the actual problem is broader schema drift with no versioned contract governing the `claude-code-rs`↔CLI boundary, and — worse — a second, currently-masked drift (`content` vs. `result`) that would silently break output once the first fix landed. That combination makes an ad hoc field patch unsafe; a real data-contract design (in the spirit of D20/D47) needs to happen and be reviewed before EN.2.B builds further on top of this seam.
- **Refs:** `planning/handoff.md`, `planning/state.json` (carryover: `claude-code-rs-parser-missing-model-field` expanded, `claude-code-rs-engine-rs-data-contract-design` added), `docs/decisions/D20-shared-data-contract.md`, `docs/decisions/D47-workspace-contract.md` (pattern referenced, not modified), `core/claude-code-rs/src/parse.rs`

---

### EN.2.A-claude-code-step-node closed out PARTIAL — ClaudeCodeStep node shipped, docs patched
- **What:** Ran `/sdlc-run EN.2.A-claude-code-step-node` (implement → test → review [PARTIAL x2] → fix → wrap-up-partial-failure-due-to-session-limit), then ran `/close-out` manually to finish the loop: re-verified all four gating checks (`cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, `cargo build --release`) green, ran `/code-review low` on the diff (zero findings), and patched `docs/architecture.md` (added `ClaudeCodeStep` to Module Map / Build & CI / Core Types). `ClaudeCodeStep` node implemented, tested, and reviewed clean; the sole gap is an upstream `core/claude-code-rs` parser bug (missing `"model"` field) blocking the live `#[ignore]` acceptance test, confirmed out of `engine-rs`'s scope per the D4 transport boundary. Shipped in commit `364d8cf` on `main`: `crates/engine-core/src/nodes/claude_code_step.rs` (new), `crates/engine-core/src/nodes/mod.rs` (new), `crates/engine-core/src/lib.rs`, `crates/engine-core/Cargo.toml`, root `Cargo.toml`, `crates/engine-core/tests/claude_code_step.rs` (new). Flipped `EN.2.A` to `closed` in `planning/state.json`, added a carryover entry `claude-code-rs-parser-missing-model-field` (kind: `known_issue`, scope: `engine-rs`) tracking the upstream bug, and regenerated derived state via `mev emit-state --write` (focus now surfaces `EN.2.B` as next).
- **Why:** EN.2.A's implementation and review were functionally complete, but the review loop hit a session limit before a formal close-out; this session finished that loop — re-confirming the gate is green, closing the block cleanly, documenting the upstream blocker so it isn't mistaken for an engine-rs defect, and handing off with `EN.2.B` as the next action.
- **Refs:** `planning/handoff.md`, `planning/state.json` (carryover: `claude-code-rs-parser-missing-model-field`), commit `364d8cf`, `docs/architecture.md`, D4 (transport boundary decision)

---

## 2026-07-03

### Merged EN.2.0 into main, closed the block, wrote handoff for EN.2.A
- **What:** Ran `/code-review low` on the full EN.2.0-async-node-trait diff — zero findings, nothing to fix. Merged `EN.2.0-async-node-trait-flow` into `main` via `/clean-worktree` (fast-forward, pushed to `origin/main`; GitHub auto-marked PR #2 as MERGED); removed the worktree and deleted the local branch. Flipped `EN.2.0`'s block status from `open` to `closed` in `planning/state.json`, ran `mev emit-state --write` to regenerate `focus` (EN.2.A now blocked only by the external SDK/D4 transport dependency, no longer by EN.2.0), and ran `mev validate-brain --state` — 0 errors, 2 pre-existing unrelated warnings. Wrote a fresh `planning/handoff.md` pointing the next agent at EN.2.A, still blocked on the D4 transport decision (see carryover entries `transport-decision-uses-d4-not-d3`, `claude-sdk-rs-not-on-disk`, `en2a-transport-decision-options` in `planning/state.json`).
- **Why:** EN.2.0 (async `Node` trait) was implemented and reviewed in a prior turn this session; this follow-up closes it out cleanly — merge, worktree cleanup, state reconciliation, fresh handoff — so the next session can pick up EN.2.A with no loose state, still gated on the outstanding D4 transport decision.
- **Refs:** `planning/handoff.md`, `planning/state.json` (carryover: `transport-decision-uses-d4-not-d3`, `claude-sdk-rs-not-on-disk`, `en2a-transport-decision-options`), PR #2

---

## [run: 2026-07-03]

Implemented EN.2.0-async-node-trait end to end via `/sdlc-flow`: Task 1 added `async-trait` and `futures` as workspace dependencies (wired into `engine-core` for real use, `async-trait` only into `engine-serve`); Task 2 converted `Node::process` to `async fn` via `#[async_trait::async_trait]`, made `Workflow::run`/`node_context` async, switched `ParallelNode`'s fan-out from `std::thread::scope` to `futures::future::join_all`, and removed `engine-serve`'s `web::block` wrapper so `post_events` awaits `workflow.run` directly; Task 3 confirmed all four gated checks (`cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, `cargo build --release`) pass against the converted codebase with `web::block` no longer live anywhere in `http.rs`. All three tasks passed on the first attempt, the review verdict was PASS with zero findings, and `docs/architecture.md` was updated to reflect the async runner. Notable decisions: `Router::route`/`OnProgress` stayed synchronous per the spec (D5), and two validator tests that never call `.process()`/`.run()` were left as plain `#[test]` rather than `tokio::test` for a minimal diff. Next: EN.2.A — Claude Code step node; first command `/generate-tasks EN.2.A`.

```
de07253 chore: flow state — docs
b226d0b docs: update docs for EN.2.0-async-node-trait
1c5afb7 chore: flow state — task 3 passed
93af3ed chore: flow state — task 2 passed
23cf690 feat: implement EN.2.0-async-node-trait-task2
11421ae chore: flow state — task 1 passed
63c06be feat: implement EN.2.0-async-node-trait-task1
e251560 chore: reconcile state for EN.2.0 — focus + block, clear async-node carryover
```

---

## 2026-07-03

### Audited Claude SDKs and paused for transport decision
- **What:** Audited homegrown `claude-sdk-rs`, the official Python SDK (`claude-agent-sdk-python`), and the native Rust SDK (`claude-agent-sdk-rust`). Discovered the Python SDK uses a Keychain interception trick to authenticate the CLI subprocess without API credits. Logged findings and options in `planning/claude-sdk/notes.md`. Recorded transport options in `state.json` carryover. Wrote `planning/handoff.md`.
- **Why:** To figure out how to leverage the flat-rate Claude Code Subscription programmatically for the ClaudeCodeStep node (EN.2.A).
- **Refs:** `planning/claude-sdk/notes.md`, `planning/handoff.md`, `planning/state.json`

### Async-node question captured; handoff refreshed pointing at two now-related gating decisions
- **What:** Answered a series of questions comparing engine-rs to the Python orchestrator (Execution Core scope, integration test coverage, async/concurrency posture), captured the async-node question in `planning/async-node/notes.md` with full file/struct/function detail for both codebases, added a carryover entry, and wrote a fresh handoff pointing at two now-related decisions (transport + async trait) both gating EN.2.A.

  1. Answered a sequence of user questions comparing engine-rs (Rust) to the Python orchestrator:
     - What "Execution Core" (Phase 1: EN.1.A/B/C) encompasses and how it relates to the Python orchestrator (D42 parallel-pilot rewrite, per-workflow graduation, byte-for-byte data-contract seam).
     - Confirmed engine-rs already has an end-to-end integration test: `crates/engine-serve/tests/dispatch_integration.rs` — three tests covering dispatch -> HTTP -> workflow execution -> live-state recording -> durable-write `EventsRow` mapping, all exercised together (no live Postgres — self-skips with `pool: None`).
     - A feature-for-feature comparison: orchestrator has 7 real workflows, richer node types (`AgentNode` w/ 8 model providers, `ToolUseNode`, `RouterNode`), Celery+Redis async dispatch, Brain/RAG wired in (pgvector) — vastly more built. engine-rs's one already-realized advantage: in-memory `LiveStateStore` with zero Postgres polling for the local Console read path.
     - The async/concurrency question: user asked "where do we stand on getting things async, the real leverage of Rust, which Python already has." Two background research agents confirmed via direct file reads that **neither** codebase does real async/await concurrency at the node/workflow level. Rust: `Node::process` (`crates/engine-core/src/node.rs:49`) is a plain sync fn; `Workflow::run`'s pointer-walk loop (`engine-core/src/workflow.rs`) has no `.await`; `ParallelNode` (`engine-core/src/parallel.rs:53`) fans out via `std::thread::scope` (real OS threads). Async only exists at the infra edges: actix-web handlers, sqlx Postgres I/O, and the durable-writer's `tokio::spawn` background task (`engine-serve/src/durable.rs`). Python: `Workflow.run`, `Node.process`, and `AgentNode.process` (via `agent.run_sync`) are all plain sync `def`; `ParallelNode` uses `concurrent.futures.ThreadPoolExecutor` (thread-based, not `asyncio.gather`); even the Celery task and the FastAPI `POST /events/` route are plain sync `def`; Python's actual concurrency comes from Celery worker processes (process-level) and `ThreadPoolExecutor` (thread-level), not asyncio. Conclusion: Rust's fully-sync node core is a faithful port, not a regression — but making `Node::process` genuinely `async fn` is an unexploited opportunity Python can't easily retrofit (would require reworking `pydantic_ai`'s sync integration), and it's directly relevant to EN.2.A since spawning a Claude Code subprocess is inherently an async operation.

  2. Created `planning/async-node/notes.md` (via `/capture async-node`, then populated with real verified content — file paths, struct/function signatures, line numbers for both Rust and Python — rather than the skill's default empty scaffold, at the user's explicit request for full detail "so the next agent knows exactly where to look"). Includes a side-by-side concurrency-model comparison table and open questions (should `Node::process` become `async fn` before EN.2.A's spec is written; does `ParallelNode`'s fan-out change from thread-based to `tokio::spawn` if so; does the `web::block` wrapper in `engine-serve/src/http.rs` retire; is this its own decision file D5 or folds into EN.2.A's scope).

  3. Added a pointer entry to the brain's `planning/backlog.md` (Active section, dated 2026-07-03, `repo:engine-rs type:research status:idea`) linking to the notes file. Note: that edit lives in the brain repo (`agentic-portfolio/`), not in engine-rs itself, and is not part of engine-rs's own git commit. Already done in a prior step of this session.

  4. Added a third carryover entry to `planning/state.json`: slug `resolve-async-node-question-before-en2a` (kind: `deferred`) — decide `Node::process` sync-vs-async before EN.2.A's spec, pointing at `planning/async-node/notes.md` for full context. The other two existing carryover entries (`transport-decision-uses-d4-not-d3`, `claude-sdk-rs-not-on-disk`) are unchanged/still open — nothing new resolved them this session. Already done in a prior step.

  5. Hit and fixed a schema violation while adding that carryover entry: the first draft used `"related": ["async-node"]` (a bare string), but per `core/planning/state-schema.md`, `carryover[].related` must be `depends_on`-style edge objects (`{type:"block",...}`/`{type:"external",...}`) or omitted entirely. Since the entry doesn't point at a block/external dependency, `related` was omitted (correct per schema). Re-ran `mev emit-state --write` afterward to confirm engine-rs's own `state.json` is now schema-clean — it is. Already run twice this session.

  6. `mev emit-state --write` surfaced a pre-existing, unrelated error this session: `core/orchestrator/planning/state.json` is malformed JSON. Not caused by this session, not touched, flagged as a heads-up only.

  7. Confirmed the prior session's `planning/handoff.md` (which had pointed the next agent at reviewing a Claude Code Rust SDK on GitHub before deciding EN.2.A's transport) had already been consumed/deleted before this session started — its content is fully preserved in this log's prior "Paused EN.2.A spec generation..." entry and in the two pre-existing carryover entries, so nothing was lost. A fresh `planning/handoff.md` was written this session pointing at BOTH now-related open decisions (transport choice + async-node question) that gate EN.2.A, with first command: read `planning/async-node/notes.md` in full, then ask the user for the GitHub SDK URL. Already done in a prior step.

  8. No architectural decision was settled this session (the async-node question and transport question are both still open) — no `planning/decisions/` file authored.

  No code was changed this session — pure research/synthesis plus planning-doc updates. `planning/state.json`'s block statuses are unchanged this session (no block closed) — EN.2.A remains `open` and `focus.next` already correctly points at it.
- **Why:** The user was assessing engine-rs's real-world maturity relative to the Python orchestrator (feature completeness, test coverage, and — critically — whether Rust's async/concurrency advantage was actually being exploited). Surfacing that neither codebase does true node-level async surfaced a concrete, EN.2.A-relevant design question that needed to be captured durably before it's lost, rather than answered ephemerally in chat.
- **Refs:** `planning/async-node/notes.md`, `planning/state.json` (carryover: `resolve-async-node-question-before-en2a`, plus pre-existing `transport-decision-uses-d4-not-d3` and `claude-sdk-rs-not-on-disk`), `planning/handoff.md`, brain `planning/backlog.md`

### Paused EN.2.A spec generation at the transport-decision clarify gate
- **What:** Started `/generate-tasks EN.2.A` (Claude Code step node) but paused at the transport-decision clarify gate **without writing a spec** — the working tree is clean of any EN.2.A files. EN.1.C is fully merged, so **Phase 1 (Execution Core) is Done**. Two blockers surfaced at the gate: (1) a **decision-number collision** — `master-plan.md` names the transport decision **D3**, but D3 is now the HTTP-framework decision (recorded during EN.1.C), so the transport decision must be recorded as **D4**; (2) **`claude-sdk-rs` is not on disk** in this environment, so the native transport for `ClaudeCodeStep` can't be built here. The user wants to first **review a Claude Code Rust SDK on GitHub and compare it against `claude-sdk-rs`** before deciding `ClaudeCodeStep`'s transport. Recorded two carryover entries in `planning/state.json` (`transport-decision-uses-d4-not-d3`, `claude-sdk-rs-not-on-disk`) and rewrote `planning/handoff.md` to refocus the next session on the external SDK review.
- **Why:** The transport choice is a load-bearing decision for the whole Phase 2 Claude Code node; making it blind (wrong decision number, and without the actual SDK on disk to compare) would bake in rework. Pausing at the gate and pointing the next session at the SDK review keeps the decision honest and preserves clean state (no half-written spec).
- **Refs:** `planning/handoff.md`, `planning/state.json` (carryover: `transport-decision-uses-d4-not-d3`, `claude-sdk-rs-not-on-disk`), `planning/master-plan.md` (EN.2.A)

### Merged EN.1.C into main, cleaned up worktree, reconciled state.json, wrote handoff for EN.2.A
- **What:** Ran `/code-review low` on the EN.1.C source diff (tests excluded) — no findings. Verified `docs/architecture.md`'s EN.1.C update (module map, dependency list, key types, data-flow narrative) was accurate — no further doc edits needed. Merged `EN.1.C-trigger-dispatch-serve-embedding-flow` into `main` via `/clean-worktree`: the first `--ff-only` attempt failed because `main` had advanced by one commit (routine harness sync from base-template, `2d25df2`); rebased the worktree branch onto `main` (clean, 17 commits, no conflicts) and retried `--ff-only`, which succeeded (merge commit `2248d5a`). Removed the worktree and deleted the branch. Reconciled `planning/state.json`: flipped `EN.1.C` from `"open"` to `"closed"`, moved `focus.next` from `EN.1.C` to `EN.2.A` (now unblocked), confirmed `EN.2.B`/`EN.3.A`/`EN.3.B` remain correctly blocked. Ran `mev emit-state --write` — clean, only informational `W_EMIT_NO_SENTINEL` warnings (expected/pre-existing). Wrote a fresh `planning/handoff.md` pointing at `EN.2.A` ("Claude Code step node") as next, first command `/generate-tasks EN.2.A`.
- **Why:** Phase 1 (Execution Core) is now fully Done (`EN.1.A`, `EN.1.B`, `EN.1.C` all closed); closing out EN.1.C cleanly — merge, worktree cleanup, reconciled state, fresh handoff — lets the next session start EN.2.A (Phase 2) with no loose state.
- **Refs:** `planning/master-plan.md`, `planning/handoff.md`, `planning/state.json`, PR #1 (`https://github.com/bredmond1019/engine-rs/pull/1`, on branch `EN.1.C-trigger-dispatch-serve-embedding-flow` — merged locally via `--ff-only` rather than through the PR; open question carried into the handoff on whether to push local `main` to `origin/main` and reconcile/close the PR)

---

### Completed EN.1.C-trigger-dispatch-serve-embedding end to end (6 tasks, PASS)
Ran `/sdlc-flow EN.1.C-trigger-dispatch-serve-embedding` to completion across 6 tasks, embedding the execution engine in `bastion serve`. Task 1 added a `Dispatcher` in `crates/engine-serve/src/dispatch.rs` implementing dual-registry (`workflow_registry` + `schema_registry`) dispatch keyed by `workflow_type`, rejecting unregistered types with `DispatchError::UnknownWorkflowType`. Task 2 added an in-memory `LiveStateStore` (`Arc<RwLock<HashMap<RunId, TaskContext>>>`) giving the local Console a no-DB-poll read path for live run state. Task 3 added `crates/engine-serve/src/durable.rs` — an mpsc-bridged async durable-write seam (`DurableHandle`/`spawn_durable_writer`/`durable_on_progress`) mapping `on_progress` snapshots to `engine_contract::EventsRow`, inserting the first (all-PENDING) snapshot and updating subsequent ones via `engine_store`, self-skipping Postgres I/O (not failing) with no `DATABASE_URL`. Task 4 built the four-endpoint `actix-web` HTTP surface (`POST /events/` with `X-API-Key` gating, `GET /health`, `GET /workflows`, `GET /workflows/{type}/graph`), wiring the D3 HTTP-framework decision (recorded earlier this run) into the dispatch/live-state/durable modules from tasks 1–3. Task 5 added the headline integration test (`crates/engine-serve/tests/dispatch_integration.rs`) covering live-state reads with no DB query, byte-identical durable `EventsRow` mapping, and a 422 for an unregistered `workflow_type`. Task 6 confirmed all four validation gates pass clean (fmt, clippy `-D warnings`, `cargo test` — 22+19+3+1+1 tests green across the workspace, `cargo build --release`) with zero further code changes. Review verdict: **PASS** — no findings. Notable decisions: kept `Dispatcher::register` generic via a boxed `WorkflowFactory` closure rather than a `NodeRegistry`-sharing convenience method; used `uuid::Uuid` as the `RunId` type to match `EventsRow.id`'s existing type; built the `OnProgress` trait-object closure inside `web::block`'s blocking closure to keep the outer closure `Send` for actix; did not edit `workflow.rs` in task 3 since no genuine signature gap surfaced (per the spec's own guidance) — no deviations from the spec surfaced across the six tasks. Next: merge `EN.1.C-trigger-dispatch-serve-embedding-flow` into `main` and define the next Phase 1/2 block.

```
2c810dc chore: flow state — docs
c6bd73a docs: update docs for EN.1.C-trigger-dispatch-serve-embedding
a7164db chore: flow state — task 6 passed
e523911 chore: flow state — task 5 passed
96ea35c feat: implement EN.1.C-trigger-dispatch-serve-embedding-task5
b9461a1 chore: flow state — task 4 passed
fb8ae82 feat: implement EN.1.C-trigger-dispatch-serve-embedding-task4
```

---

## 2026-07-03

### Created GitHub remote, merged EN.1.B, cleaned up worktree, reconciled state.json, wrote handoff for EN.1.C
- **What:** Created this repo's first GitHub remote — `bredmond1019/engine-rs` (private) — matching the naming convention of sibling repos (bastion, bella, mev: plain name, private), and pushed `main` plus the feature branch. Ran `/code-review low` on the EN.1.B source diff (tests excluded) — no findings. Merged `EN.1.B-router-parallel-nodes-validator-flow` into `main` via `git merge --ff-only` (commit `43637e2`) and pushed the merge commit to `origin/main`. Removed the worktree and deleted the branch via `/clean-worktree`. Reconciled `planning/state.json`, which had an uncommitted, half-finished edit left over from a prior session (it closed EN.0.A/EN.0.B/EN.1.A but not EN.1.B, and still listed EN.1.B in `focus.next` even though it was now done): closed the EN.1.B block entry, removed it from `focus.next`/`focus.blocked`, and promoted EN.1.C to `focus.next` (no longer blocked by EN.1.B). Wrote a fresh `planning/handoff.md` pointing the next agent at EN.1.C (first command: `/generate-tasks EN.1.C`).
- **Why:** EN.1.B (Router trait + `as_router()` hook + `dispatch_route()`, `ParallelNode` fan-out/merge via `std::thread::scope` with deterministic last-write-wins merge, `WorkflowValidator` with BFS reachability + DFS cycle detection skipping router edges + non-router fan-out arity guard, and `Workflow::run` wired to dispatch through routers plus the new fallible `Workflow::new_validated()`) had just landed via `/sdlc-flow` and needed a clean merge to `main`, a real GitHub remote for the repo, and reconciled planning state before the next block could start.
- **Refs:** `planning/master-plan.md`, `planning/handoff.md`, `planning/state.json`

---

### Completed EN.1.B-router-parallel-nodes-validator end to end (5 tasks, PASS)
Ran `/sdlc-flow EN.1.B-router-parallel-nodes-validator` to completion across 5 tasks. Task 1 added a `Router` trait (supertrait of `Node`) with `route(ctx)` for runtime next-node selection, a `Node::as_router()` registry hook, and a `dispatch_route(&dyn Router, &TaskContext)` dispatch helper in `engine-core`. Task 2 added `ParallelNode` — fan-out over branch nodes via `std::thread::scope`, deep-copying `TaskContext` per branch, with deterministic last-write-wins merge of `nodes`/`node_runs` keyed by declared branch order — plus unit and integration tests. Task 3 added `WorkflowValidator` (BFS reachability from `start_node`, DFS cycle detection that skips edges declared out of router nodes, and a non-router fan-out arity guard) with a `ValidationError` enum and six unit tests covering valid and rejected schemas. Task 4 wired `Workflow::run` to call `Router::route(ctx)` for router nodes (supporting undeclared runtime back-edges) while plain nodes still walk `connections[0]`, and added a new fallible `Workflow::new_validated(registry, schema)` that runs the validator first — `Workflow::new` stayed infallible and unchanged, keeping the EN.1.A `tests/workflow_runner.rs` passing unmodified. Task 5 confirmed `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, and `cargo build --release` all pass clean with no further changes. Review verdict: **PASS** — no findings. Notable decisions: router classification is purely via `NodeRegistry` lookup + `Node::as_router().is_some()`; validation runs arity → reachability → cycles in that order over sorted node keys for reproducible error reporting; DFS cycle detection skips walking connections declared out of a router node entirely (not just back-edges), matching the spec's "skips edges out of router nodes" language. No genuine deviations from the spec surfaced across the five tasks. Next: merge `EN.1.B-router-parallel-nodes-validator-flow` into `main` and define the next Phase 1 block.

```
6dd6ce5 docs: update docs for EN.1.B-router-parallel-nodes-validator
aa39cc0 chore: flow state — task 5 passed
92a0f36 chore: flow state — task 4 passed
45efffe feat: implement EN.1.B-router-parallel-nodes-validator-task4
cff7b3d chore: flow state — task 3 passed
a6180bc feat: implement EN.1.B-router-parallel-nodes-validator-task3
90bac7b chore: flow state — task 2 passed
301d926 feat: implement EN.1.B-router-parallel-nodes-validator-task2
```

---

## 2026-07-03

### Merged EN.1.A-node-trait-workflow-runner, cleaned up worktree, wrote handoff for EN.1.B
- **What:** Ran three SDLC pipeline blocks in sequence: `/sdlc-run EN.0.A-cargo-workspace-ci` (PASS — workspace scaffold, CI, D2 tokio+sqlx decision), `/sdlc-run EN.0.B-data-contract-postgres` (PASS — engine-contract serde types, engine-store Postgres layer), and `/sdlc-flow EN.1.A-node-trait-workflow-runner` (PASS, 5 tasks — `Node` trait, `NodeRegistry`, `WorkflowSchema`/`NodeConfig`, `Workflow` pointer-walk runner with `on_progress` seam). Discussed sqlx vs. Diesel along the way; kept D2 as-is, no new decision needed. Added `/trees` to `.gitignore` (commit `414b353`). Caught and fixed a docs bug before merging: `docs/architecture.md`'s Module Map / Build & CI sections still described `engine-contract`/`engine-store` as stubs even though EN.0.B gave them real types — corrected in the worktree (commit `bc2bd67`, "docs: correct engine-contract/engine-store stub description in architecture.md"). Ran `/code-review low` on the EN.1.A diff (source only) — no findings. Merged `EN.1.A-node-trait-workflow-runner-flow` into `main` via `git merge --no-ff` (merge commit `a7906cc`), deliberately choosing `--no-ff` over the skill's default `--ff-only` because the branch carried meaningful intermediate wrap-up/state commits worth preserving in history. Verified `cargo test --workspace` passes clean on `main` post-merge. Removed the worktree at `trees/EN.1.A-node-trait-workflow-runner-flow` and deleted the branch. Wrote `planning/handoff.md` for the next agent (first command: `/generate-tasks EN.1.B`). Added a `carryover[]` entry to `planning/state.json` (slug `state-json-block-status-stale`, kind `known_issue`): the `tracks[].blocks[]` status fields for EN.0.A/EN.0.B/EN.1.A still read `"open"` even though `planning/status.md`'s Progress Table marks all three Done — flagged so the next agent trusts `status.md` over `state.json`'s per-block status until reconciled.
- **Why:** Continuing the sequential SDLC drive through engine-rs's Phase 0/Phase 1 blocks per `master-plan.md`; the merge, worktree cleanup, and handoff close out EN.1.A cleanly so the next session can pick up EN.1.B with no loose state.
- **Refs:** `planning/master-plan.md`, `planning/handoff.md`, `planning/state.json` (carryover: `state-json-block-status-stale`)

---

## 2026-07-03

Completed EN.1.A-node-trait-workflow-runner end to end (implement → test → review → document → wrap-up) across 5 tasks. Task 1 added the `Node` trait (`process`/`name`, `Send + Sync`) and a `NodeRegistry` (`HashMap<String, Box<dyn Node>>`) in `engine-core`, backed by `engine-contract`'s `TaskContext`. Task 2 added `WorkflowSchema`/`NodeConfig` with helpers to resolve the start node and each node's `connections[0]` next-node. Task 3 added the `Workflow` pointer-walk runner (`crates/engine-core/src/workflow.rs`) that seeds all nodes PENDING before the walk, stamps RUNNING → SUCCESS/FAILED with timing on each `NodeRun`, invokes the `on_progress` persistence seam at every node boundary, and halts on node failure. Task 4 added a fixture 3-node linear integration test (`workflow_runner.rs`) covering full-success transitions, the initial PENDING `on_progress` snapshot, and a middle-node failure halting the walk. Task 5 confirmed all four validation commands (`cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, `cargo build --release`) pass clean with no further changes needed. Review verdict: PASS — all acceptance criteria MET, all gating checks green. Notable decisions: `NodeError` is a simple string-wrapping struct rather than an enum (minimal failure carrier for this block's scope); a node's own runtime failure is captured in its `NodeRun` and does not short-circuit `Workflow::run` with an `Err` — only unregistered-node graph-shape issues do. No genuine deviations from the spec — router/parallel-node branching and the acyclic validator remain out of scope for EN.1.B as planned. Next: define and run EN.1.B-router-parallel-nodes-validator (router + parallel nodes + validator).

```
add6862 chore: flow state — docs
c7e473c docs: update docs for EN.1.A-node-trait-workflow-runner
db55e50 chore: flow state — task 5 passed
96df0cd chore: flow state — task 4 passed
d022eec feat: implement EN.1.A-node-trait-workflow-runner-task4
4a169e0 chore: flow state — task 3 passed
67ef4bf feat: implement EN.1.A-node-trait-workflow-runner-task3
5be0e37 chore: flow state — task 2 passed
```

---

## 2026-07-02

Completed EN.0.B-data-contract-postgres end to end (implement → test → review → document → wrap-up). Implemented the preserved data-contract seam in `engine-contract` — `NodeRunStatus` (lowercase `pending|running|success|failed`), `Usage`, `NodeRun` (always-present-but-nullable `started_at`/`completed_at`/`error`/`input`/`usage`), `TaskContext`, and `EventsRow` (`id`, `workflow_type`, `data`, `task_context`, `created_at`, `updated_at`) — matching `orchestrator/docs/data-contract.md` v1.0.1 field-for-field. Added a byte-for-byte round-trip test against a captured Python-shaped fixture plus a Rust-constructed shape assertion, both passing with no field/casing/type drift. Implemented `engine-store`'s Postgres layer (`connect`, `insert_event`, `update_event`, `get_event`) on the D2-pinned `sqlx::PgPool` stack, with a live round-trip test that self-skips (not fails) when `DATABASE_URL` is unset so EN.0.A's Postgres-less CI stays green. Review verdict: PASS — all 6 acceptance criteria MET, all 4 gating checks (fmt, clippy, test, build --release) green, 16 tests total. `docs/architecture.md` was flagged NEEDS_REVIEW (module map / Core Types / Build & CI sections still describe stubs) rather than edited directly, since it's a top-level architecture doc. No genuine deviations from the spec — the always-present-but-null `NodeRun` field serialization was in-scope work needed to satisfy the byte-for-byte acceptance criterion, not a scope change. Next: define and run EN.1.A-node-trait-workflow-runner (Node trait + Workflow runner).

```
9347681 docs: update docs for EN.0.B-data-contract-postgres
a7cbb55 feat: implement EN.0.B-data-contract-postgres
63f6996 chore: add spec for EN.0.B-data-contract-postgres
f2bb90c chore: wrap up EN.0.A-cargo-workspace-ci
9f7f1b8 docs: update docs for EN.0.A-cargo-workspace-ci
```

---

## 2026-07-02

Completed EN.0.A-cargo-workspace-ci end to end (implement → test → review → document → wrap-up). Stood up the `engine-rs` Cargo workspace with four member crates (`engine-core`, `engine-contract`, `engine-store`, `engine-serve`), each carrying a compiling `src/lib.rs` stub with a trivial passing test. Added `.github/workflows/ci.yml` running fmt/clippy/test/build on push and pull_request, matching `planning/harness.json`'s validation gates exactly. Recorded the async-runtime + persistence stack as decision `D2-async-runtime-choice.md` (tokio + sqlx with postgres/runtime-tokio/tls-rustls features), linked from `planning/decisions/index.md`. Review verdict: PASS — all 6 acceptance criteria MET, all 4 gating checks (fmt, clippy, test, build --release) green. `docs/architecture.md` patched with the confirmed Module Map and a new Build & CI section documenting D2 and the CI gates; no NEEDS_REVIEW flags. No genuine deviations from the spec — the async-runtime decision was in-scope work, not a scope change. Next: define and run EN.0.B-data-contract-postgres (data-contract serde types + Postgres round-trip).

```
9f7f1b8 docs: update docs for EN.0.A-cargo-workspace-ci
1a59a44 feat: implement EN.0.A-cargo-workspace-ci
cdc9133 chore: add spec for EN.0.A-cargo-workspace-ci
```

---

## 2026-07-02

Project initialized from `base-template` (commit `7f2cbada68bdb0433133cf213777994030f7b7d6`) via `/new-project`.
Planning infrastructure scaffolded: `planning/context.md`, `planning/status.md`,
`planning/master-plan.md`, `planning/index.md`, `planning/harness.json`, `planning/decisions/`,
and the root `CLAUDE.md` / `README.md`. Concept folders (`planning/<concept>/`) are created on
demand by the SDLC pipeline. Curated SDLC harness (`.claude/`) in place.

Next step: run `/generate-tasks` for the first Phase 0 block to begin the pipeline.

```diff
(no code changes — planning files only)
```
