---
type: Log
title: engine-rs Development Log
description: Chronological log of work completed for engine-rs.
doc_id: log
layer: [factory]
status: active
timestamp: "2026-08-31T23:56:02Z"
keywords: [work log, session history, development log]
related: [status, context]
---

# Log — engine-rs

*Append-only working log. One dated entry per session. Newest entries at the top.*

## [run: 2026-08-31]

### Run cost was undercounted: a per-invocation ledger, found while porting `workflow_run_id`
- **What:** Started as a port of base-template's caller-stamped `workflow_run_id` and ended as a cost fix. **The port was refused on evidence:** an engine-rs run is 1:N with Claude sessions (every LLM stage is a separate headless `claude` invocation via `ClaudeCodeStep`, no `--resume` across 37 sites), and engine-rs has no `wf_*` id at all — it runs as a workflow graph inside `bastion serve`, not as a Workflow-tool script. Reusing that field name would have made a consumer join under `subagents/workflows/wf_*/agent-*.jsonl`, find nothing, and report the data **absent rather than erroring**. `jynx-2d` verified the four load-bearing claims against source rather than taking them on report, ruled (a) the native shape, and added that the JS engine is 1:N too — a resumed run's state file is written by the *resuming* invocation, so its scalar names only the second segment, which measured as a 34% token loss on `JX.3.B`. Built `crate::sessions`: an append-only, order-preserving per-invocation ledger in `ctx.metadata["claude_sessions"]`, round-tripping onto `SDLCState`. **Then the operator asked why we weren't just reading `total_cost_usd` off the CLI — and we already were, which exposed the real defect.** `policy::telemetry::total_cost_usd` summed each cost-bearing stage's LAST recorded `cost_usd` from `ctx.nodes`, a map keyed by node identity and overwritten on every re-run, so a stage the task loop ran six times contributed one call; `total_tokens` and both cache channels did the same. The error grew with retries and review rounds — worst on exactly the expensive runs. Ledger entries now carry the CLI's own billing (`cost_usd` + all four token channels) and `harvest` sums them, with the old scan kept as the fallback for an empty ledger so no existing caller's numbers moved. Two seams would have silently defeated this and neither was visible from outside: `claude_code_rs::execute` converts an `is_error` envelope into `Error::Api` and **discarded the whole `Outcome`**, losing a billed failure's cost; and `workflow::node_context` **discards a failing node's `TaskContext`**. Fixed by widening `Error::Api` (`session_id`, `cost_usd`, `usage`) and by `NodeError::sessions`, replayed into the reverted context. Dedup was added, then removed entirely — it guarded no real path and, once entries carried money, could only undercount. Gates green in both repos: 3456/3456 and 50/50, fmt/clippy/release clean, corpus 0 errors.
- **Why:** The porting request came with jynx's measurement that `promptTokEst` ran ~113x low. That gap is a JS-engine problem — a Workflow-tool script cannot see a subprocess envelope — and the framing was carried into engine-rs without checking whether it applied here. It did not. The operator's `jq` question is what caught it, and behind it sat a genuine, unnoticed undercount of money: pinned executably by `a_retried_stage_reports_every_attempt_it_was_billed_for` at **$0.52 spent against $0.02 reported**. The general cause — `ctx.nodes` holding one slot per node identity, so any node that runs twice destroys its own history — is unfixed and now carries a priority-1 carryover; `review_verdicts`, the observed transport tier and `CommittedReview.attempts` (hardcoded `1`, with a comment saying the real count is unrecoverable) are still lost the same way.
- **Refs:** `planning/workflow-run-id-join/notes.md`, `docs/data-contract.md` (two 2026-08-31 changelog rows, not a re-pin), carryover `ctx-nodes-holds-one-slot-per-node-so-repeat-invocations-overwrite-history` and `claude-sdk-rs-next-publish-is-semver-major-error-api-gained-fields`.

### JS stage-prompt port — all four SDLC prompts, each verified live
- **What:** Closed `EN.ticket.prompt-parity-with-the-js-engines` by porting all four JS engine stage prompts into the Rust engine, each as a run-invariant constant prepended to the per-call prompt body: `TRIAGE_STABLE_PROMPT` (bail taxonomy, cost asymmetry, evidence clause, the R4 self-inflicted-environment caution), `REVIEW_STABLE_PROMPT` (shared by `ConsolidatedReviewNode` and `EndReviewNode` — per-criterion MET/PARTIAL/NOT_MET, index-not-evidence, and an anti-paraphrase clause NOT in the JS, added because JX.3.A's criterion was passed by reinterpretation), `DOCS_STABLE_PROMPT` (surgical rules, BOOTSTRAP MODE, the `write-repo-doc` gaps), and `IMPLEMENT_STANDARDS_PREAMBLE` (CLAUDE.md authority, harness rules, scope containment, the D8 completeness self-check). Schema additions — triage's `evidence`/`base_state_checked`/`same_failure_as_before`, review's `localized`, docs' `created`/`flagged` — are all `serde(default)` and outside `required`, so an omitting model still parses and every port is behavior-stable. **Two of the four content bins were deliberately NOT ported:** the gating-suite re-run, the commit sequences, the work assertion and the vault recipe are all already enforced in Rust (`run_checks`, `commit_all`, `verify_claimed_writes`), and moving them into prose would be a downgrade wearing the costume of parity; a test per stage now fails if someone ports them back. The TEST stage was re-read in full and skipped entirely for the same reason — all 21 lines are bin 2. **Each prompt was then verified against real model calls, not only unit tests.** `smoke-sdlc-flow` cannot reach triage's model branch or either reviewer, so two reusable fixtures were built: `smoke-sdlc-triage` (returned `MAJOR_BAIL` with all three new fields, `base_state_checked: false` and no fabricated pre-existence claim — the R4 defect not recurring) and `smoke-sdlc-review` (both review nodes returned `FAIL` with every check green, one issue reading "uses only a generic phrase" — the anti-paraphrase clause firing). 27 new unit tests; four byte-identical change-detectors updated deliberately rather than deleted. Close-out added a *Stage prompts* section to `docs/workflows/sdlc-flow.md` and corrected three stale output shapes.
- **Why:** The Rust stage prompts were 4–6 lines against the JS engines' 37–88, and the thinness was not cosmetic — it had produced at least two measured misclassifications (R4's `MAJOR_BAIL` on a run whose only real problem was a write escape; JX.3.A's acceptance criterion accepted as a weaker paraphrase). The operator waived the ticket's one-stage-per-pin-advance constraint, judging that these prompts should have existed before the jynx dogfooding runs began; the cost — jynx loses per-stage attribution — is recorded in the classification docs rather than glossed.
- **Refs:** `planning/EN.ticket.prompt-parity-with-the-js-engines/` (four-bin classification, both stages), `planning/smoke-sdlc-triage/`, `planning/smoke-sdlc-review/`, commits `fd27c38`, `7d99520`, `788c1ee`.

### engine-updates-and-fixes chain (2nd run) — five jynx-filed tickets plus a generic HTTP node
- **What:** Renamed the orchestration-run directory `en-three-tickets` → `engine-updates-and-fixes` and reopened it for five more jynx-filed tickets plus one operator-queued addition, via `/orchestrate` driving `sdlc-task` for each, in place on `main`. Two of the five block records pointed at the wrong files — jynx's traces named where the error TEXT surfaced, not where the bug was — and both were amended (D18) before task authoring: `EN.ticket.sdlc-task-resolved-policy-writer-reader-mismatch`'s real bug was `engine-serve`'s `register_sdlc_task_with_registry` seeding the raw, unprojected `SdlcTaskPolicy` instead of projecting through `.to_sdlc_policy()` — the only factory a served `bastion serve` dispatch uses, so every live dispatch failed while the hermetic suite stayed green (`75d608c`, `c08ce77`); `EN.ticket.failed-run-leaves-worktree-and-branch-behind`'s premise ("teardown only runs on the success path") was wrong — there was no engine-level teardown on any path — fixed by hooking `SetupWorktreeNode::remove_worktree_and_branch` into `engine-serve`'s `spawn_run`, gated on whether `LoadTaskStateNode` ever established real task-level state so a resumable run's worktree survives (`9f78092`, `c6d0652`). `EN.ticket.unknown-review-verdict-must-not-hard-bail`: an unrecognized review verdict now gets the same bounded `max_review_attempts` retry as FAIL/PARTIAL instead of an instant terminal bail, the prompt + JSON schema now constrain the verdict vocabulary to PASS/PARTIAL/FAIL, and a dedicated test confirmed the ticket's counter-honesty concern was already resolved as a side effect of the router fix — no separate defect (`de092da`, `52da44e`, `b4e24bc`, `eac3bb8`). `EN.ticket.triage-cannot-see-which-test-failed`: `TestTaskNode` now compacts raw check output — keeping failure-signal lines + context over passing-test noise — before the existing hard-truncation cap runs, and the exhausted-attempts bail reason now names the failing test (`1529024`). `EN.ticket.generic-http-request-node`: a general-purpose `HttpRequestNode` reusing the existing injectable `HttpPost` seam, no new trait or stub infrastructure (`effb4c5`, `dcf97f3`). `EN.ticket.micro-spec-fixture-for-engine-seam-comparison` stayed HELD on its unmet dependency, never started. All five PASS, `bastion validate-brain --state`/`--graph`/`--links`/`--structure` all 0 errors. Full workspace re-run at chain close: 3007/3007 tests passed. `/close-out` afterward: all 5 gating checks green (fmt/clippy/nextest 3412 passed/build/hang-terminate script), coverage adequate across all 16 changed source files (every ticket authored test-first), two docs patched (`architecture.md`'s Injectable Seams table gained `HttpRequestNode`; `sdlc-flow.md`'s Restart-vs-resume section gained the `retry_task` fifth case) — `fed8cff`.
- **Why:** Five more tickets jynx's engine-comparison lane filed 2026-08-31 at pin `ea50741`, plus `EN.ticket.generic-http-request-node` queued mid-run by the operator. A light review of chain 1's three tickets ran in parallel (a forked subagent, read-only) — all three called solid, one low-severity undocumented edge case noted in `retry_task`/`task_range` interaction (recorded as a new carryover, not fixed this session).
- **Refs:** `planning/orchestration-run/engine-updates-and-fixes/{notes.md,review.md}`; `planning/handoff.md`; blocks `EN.ticket.sdlc-task-resolved-policy-writer-reader-mismatch` / `EN.ticket.unknown-review-verdict-must-not-hard-bail` / `EN.ticket.failed-run-leaves-worktree-and-branch-behind` / `EN.ticket.triage-cannot-see-which-test-failed` / `EN.ticket.generic-http-request-node`.

### engine-updates-and-fixes chain (1st run) — retry-one-exhausted-task, abort-interrupts-agent-node, run-telemetry's structural zeros
- **What:** Closed the three tickets left open by the 2026-08-30 jynx defect sweep, via `/orchestrate` driving `sdlc-task` for each, in place on `main`. `LoadTaskStateNode` gained a fifth restart-vs-resume case — an additive `retry_task: Option<u32>` event field that resets one named task's status/attempt_count/review_attempt_count against the existing committed state while leaving every other task and the resume:true/false paths untouched (`23fa483`, `8334ec0`); the three agent nodes in `sdlc_flow`/`sdlc_task` gained a cancellation-token builder, minted and published by the SDLC factories and threaded through `engine-serve`'s suspend/dispatch path, so an abort request can now interrupt a node mid-agent-call (`0f483cd`, `fc6f3e7`); and `run_telemetry`'s four permanently-zero counters (`total_attempts`/`total_retries`/`tasks_passed`/`tasks_failed`) were documented at their origin and at `docs/architecture.md`, with a new pinning test (`stamp_run_telemetry_counters_stay_structurally_zero_on_a_successful_run`) forcing the docs to be revisited if a future change starts populating them (`ea50741`, `d5fe94e`, `00bed02`). All three PASS, `bastion validate-brain --state` 0 errors after each. Full workspace re-run at chain close: 2985/2985 tests passed.
- **Why:** These were the three remaining open tickets from the prior session's jynx-driven defect sweep, queued for a follow-up `/orchestrate` run rather than closed inline that session.
- **Refs:** `planning/orchestration-run/engine-updates-and-fixes/{notes.md,review.md}` (renamed 2026-08-31 from `en-three-tickets` and reopened for a second run of jynx-filed tickets — see that session's log entry below); blocks `EN.ticket.retry-one-exhausted-task-without-restarting-the-spec` / `EN.ticket.abort-must-interrupt-an-in-flight-agent-node` / `EN.ticket.run-telemetry-publishes-four-permanently-zero-counters`.

## [run: 2026-08-30]

### jynx defect sweep — eight engine fixes, four tickets, and a stale-harness sync
- **What:** Fixed eight defects the jynx lane found driving Phase 3 specs through `bastion serve`: a bailed run reporting `succeeded` over HTTP (`e8ad0ef`); implement work escaping the worktree (`9933f98`); `verify_claimed_writes` short-circuiting on the model's own empty self-report (`e5908ee`); passing reviews spending later tasks' review budget (`b451c85`); attempts counted at their outcome so a bail counted none (`f4d57bf`); the worktree planning guard accepting a stale directory and `SpecExistsRouterNode` fabricating a task list for a named spec (`3bdb46a`, `07c8057`); an undeclared router edge (`7fa2629`); and a task marked done whose commit silently failed (`2d9985b`). Plus `engine_build_sha` on bastion's own `/health` (`efb46bf`). Ticketed the four that remained, three still open. Synced 12 stale harness files from base-template (`4b76f6a`).
- **Why:** The jynx lane pins a daemon build and advances it deliberately, so it needed each run labelled with the build that produced it — and while proving that out it kept finding layers of this engine reporting success on work that never happened. The sweep is those reports worked through. The harness sync came last because a tests-first ticket bailed and left `main` red: engine-rs's engines had no `expect_red`, which base-template has had for a while. Not a missing feature, a sync gap.
- **Refs:** `planning/orchestration-run/engine-tickets/{notes.md,review.md}`; `planning/handoff.md`; blocks `EN.ticket.bailed-run-reports-succeeded-over-http` and seven siblings.

## [run: 2026-08-30]

### engine-tickets chain — the build-SHA stamp and the close-block commit manifest
- **What:** Closed `EN.ticket.stamp-engine-sha-on-every-run` and `EN.ticket.close-block-node-leaves-derived-output-uncommitted` via `/orchestrate`, both `/sdlc-task` in place on `main`, PASS 4/4 each. The first compiles the build commit SHA into the binary behind one accessor, `engine_core::engine_build_sha()`, which both the run artifact (`wrap_up.rs`) and `GET /health` (`http.rs`) read, with a dirty tree reporting `<sha>-dirty`. The second widens `close_block.rs`'s existing `I_EMIT_WROTE` read into a full write manifest and stages each path one at a time behind a `realpath`, with the result surfaced on `CloseOutcome::Closed`. Followed by `/close-out`: 5/5 gating checks, no blocking coverage gaps, and two stale docs patched (the `GET /health` contract row, and `CloseBlockNode`'s behaviour in the SDLC_TASK doc).
- **Why:** A run artifact recorded nothing about which build produced it, and because `bastion serve` is a launch-time snapshot that label cannot be backfilled — every unattended run made before this is permanently unlabelled. Requested by the jynx lane as the prerequisite for its engine-comparison phase. The second was reported by the mev lane: closing a block regenerates derived surfaces fleet-wide and committed none of them, which cost 24 uncommitted files across four repos and surfaced later as a red gate in a lane that never touched them — a defect this chain reproduced live, twice, while closing its own blocks.
- **Refs:** `planning/orchestration-run/engine-tickets/{notes.md,review.md}`; `planning/handoff.md`; blocks `EN.ticket.stamp-engine-sha-on-every-run` / `EN.ticket.close-block-node-leaves-derived-output-uncommitted`.

## [run: 2026-08-29]

### EN.12 lane — the unattended-run trio (EN.12.L, EN.12.G, EN.12.C)
- **What:** Closed all three blocks of `/orchestrate EN.12.C EN.12.G EN.12.L` (PRs #61, #62, #63, all merged). `EN.12.L` typed the `GET /recall` envelope and made recall reachable as a `RECALL` dispatch step that branches the chain and journals the branch; `EN.12.G` shipped the `JournalReader` seam and `DebriefNode` rendering a morning brief from a campaign's journal; `EN.12.C` shipped permission-profile enforcement — closed `GatedAction`/`PermissionProfile` enums, `ClearOperatorGate` denied by an early-return guard, the profile stamped into every run record, and narrowing-only inheritance into child runs. Also added the missing `docs/workflows/recall.md` and its catalogue rows.
- **Why:** These three are what make an overnight run trustworthy rather than merely unattended — context, a voice, and boundaries. The operator cut `EN.12.F` (CONDUCTOR) mid-run and asked for the debrief standalone, so `EN.12.G` was decoupled from it and takes a campaign id alone, triggerable from `routine.sh`.
- **Refs:** `planning/orchestration-run/orchestration-extensions/{notes.md,review.md}`; `planning/decisions/D23-brain-read-seam.md`; blocks `EN.12.L`/`EN.12.G`/`EN.12.C`.

## [run: 2026-08-29]

`EN.12.C` — Permission-profile enforcement: the profile in force is stamped into every run record and gates graded actions — done via `/sdlc-flow` on branch `EN.12.C-flow`. All 7 tasks passed, PASS review. Added a closed `GatedAction`/`PermissionProfile` enum pair and a `decide()` implementing the permission-grading matrix (locked/standard/unrestricted x clear-operator-gate/install-on-mini/push-to-main/cross-repo-write), with `clear-operator-gate` denied at every profile via an early-return before the grading match — structurally impossible to flip via one profile's row — and `DEFAULT_PROFILE = Standard` (task 1); the profile now resolves from `brain.toml`'s `[permission_profiles]` table via `mev::brain::config::load_brain_config`, failing closed to `Locked` with a typed `ProfileResolutionError` on any malformed or absent config shape, validating every declared level against the closed vocabulary rather than only the active default (task 2); every `LaneLogEntry` append site in `integrate_chain_impl` now stamps a resolved `profile` wire identifier, with the closed-step write refused via a standalone, unit-testable `require_profile_stamp` guard if the stamp is ever missing — matching the module's pre-existing convention that only the closed-step write is non-swallowable (task 3); `FlowInvocation` gained a required (no-Default) `permission_profile`, and `execute_step` resolves a child step's profile from the parent plus an optional per-step request via `resolve_child_permission_profile`, erroring with `ExecuteError::ProfileWidening` — never a silent clamp — if the request would widen it; the resolved profile is now seeded into both `sdlc_flow_event` and `sdlc_task_event` (task 4); `gates.rs` gained `check_permission_gate`, which consults `permission::decide` before a graded action and, on denial, calls an injected `author_operator_edge` closure to raise a stable-slug `{"type":"operator"}` edge and refuse the step — never an in-process state.json write; a source-scan test proves the module contains no reference at all to mev's gate-closing verb (task 5); task 6 fixed pre-existing rustfmt/clippy drift the shared worktree had accumulated and added an integration test composing the real production functions (`resolve_permission_profile`, `check_permission_gate`, `integrate_chain`, `execute_step`) end-to-end; task 7 (validate-only) surfaced and fixed one real regression an EN.11.A-era lane-log shape test that rejected task 3's new `profile` key as unrecognized, then ran the full harness clean: fmt, clippy `-D warnings`, `nextest --workspace --all-features`, release build, and the hang-termination script. Closes `EN.12.C`. Docs: `docs/workflows/orchestration.md` updated. Next: see `planning/status.md`.

```
56bd94e docs: update docs for EN.12.C
207cb09 test: allow EN.12.C task 6's profile stamp in lane-log shape assertion
c2f20e6 fix: fix pass 1 for EN.12.C-task6
538939a feat: implement EN.12.C-task6
2de5a5a feat: implement EN.12.C-task5
064d55a feat: implement EN.12.C-task4
849306a feat: implement EN.12.C-task3
a175d54 feat: implement EN.12.C-task2
```

## [run: 2026-08-29]

`EN.12.L` — Brain read client (engine half): `HttpGet` trait and the recall consumer node — done via `/sdlc-flow` on branch `EN.12.L-flow`. All 7 tasks passed, PASS review. `RecallNode::process` now deserializes the full `GET /recall` body through a typed `RecallResponse` envelope (query/count/results) instead of an untyped `body.get("results")` pull, pinned by a 5-test fixture-conformance suite proving failure on rename/add/remove and tolerance of an added field (task 1); the `RECALL` workflow (single-node `RecallNode` graph) registered in both dispatcher registries so a `kind: dispatch` chain step naming `"RECALL"` resolves and runs, resolving `BrainConfig::from_env()` per event rather than at startup so a missing `BRAIN_API_URL` surfaces as `DispatchError::PolicyResolutionFailed` (task 2); `JournalDecisionKind::RecallConsulted` (wire: `recall_consulted`) added, rendering "recall consulted" through `engine-serve`'s exhaustive `ledger_label`/`kind_title` matches with no DB migration needed (task 3); `integrate_chain_inner`'s Dispatch arm now sets a loop-local `pending_skip` on a `count==0` RECALL result (next step skipped with no lane-log/checkpoint entry) and emits a `RecallConsulted` row (query/count/top_score/branch) in place of the generic `StepIntegrated` row — a failing RECALL step still bails the chain (task 4); a real `[dispatch RECALL, block]` integration test drives two different stubbed recall bodies through `integrate_chain_with_dispatch`, asserting the branch differs and precedes the block step's row, and a failing `StubHttpGet` bails via `IntegrateError::Dispatch`, using a test-only `QuerySeedNode` to exercise the real `RecallNode`/`StubHttpGet` path since the chain always passes an empty event to a dispatch step's factory (task 5); `journal_integration.rs` proves both `render_notes_md` and `render_review_md` emit the substring "recall" for a `RecallConsulted` row, standing in for the un-gateable `bastion journal | grep -q recall` DoD line (task 6); task 7 validated the full harness — fmt, clippy `-D warnings`, nextest `--workspace` (3222/3222 passed), release build, hang-terminate script all green, no source changes needed. This lands the engine's half of the D9/D51/D53 boundary decision: a chain step can now visibly branch on what the Brain already knows, with the branch recorded in the run journal. Closes `EN.12.L`. Docs: `docs/architecture.md`, `docs/data-contract.md`, `docs/coming-soon.md` updated. Next: see `planning/status.md`.

```
5f65830 docs: update docs for EN.12.L
fb27a50 feat: implement EN.12.L-task6
39d7dea feat: implement EN.12.L-task5
18e1223 feat: implement EN.12.L-task4
ecee03a feat: implement EN.12.L-task3
b4edb3c feat: implement EN.12.L-task2
451da26 feat: implement EN.12.L-task1
```

## [run: 2026-08-28]

`EN.11.C` — Chains compose: block N+1's tree contains block N's work — done via `/sdlc-flow` on branch `EN.11.C-flow`. All 4 tasks passed, PASS review. `PullRequestNode` now stamps `branch_name` on both the `auto_pr:false` short-circuit (falling back to `null` when `SetupWorktreeNode` hasn't run) and the success path, via a new `setup_branch_name()` helper distinct from the error-returning `setup_output()` (task 1); `integrate_chain_impl` merges a step's branch into `main` and pushes it after its state write verifies and before its lane-log closed line, through a new `merge_step_branch`/`resolve_merge_branch` pair and `IntegrateError::StepMergeFailed` carrying git stderr, run through the existing `default_command_runner()` seam — `PullRequestNode` itself still never auto-merges (task 2); EN.11.B's composition test now asserts the composed (fixed) tree, plus a new gate-capable-of-failing test, a merge-failure test, and a real-symlink planning-preservation test over the merge stage (task 3); task 4 (validate-only) confirmed the full harness green — fmt, clippy `--all-features -D warnings`, `nextest --workspace --all-features` (3189 passed, +21 over the 3168 baseline, none moved to skipped), release build, cargo audit (pre-existing h2 advisory only, non-gating), no code changes needed. This closes the "composition" leg of `sequence.md`'s sequencing rule — a two-block chain's second block now actually branches from a tree containing the first block's work, closing red-team finding G1. Docs updated: `docs/orchestration-workflow.md`, `docs/sdlc-flow-workflow.md`. Closes `EN.11.C`. Next: see `planning/status.md`.

```
6550a6f docs: update docs for EN.11.C
f756e5f feat: implement EN.11.C-task3
d25b3b4 feat: implement EN.11.C-task2
1aff6b4 feat: implement EN.11.C-task1
```

## [run: 2026-08-27]

`EN.11.A` — Artifact identity: build stamp, writer, run_id and host — done via `/sdlc-flow` on branch `EN.11.A-flow`. All 6 tasks passed, PASS review. `engine-core` gained `build_info::{GIT_SHA, BUILT_AT, WRITER, host_stamp()}` sourced from a new `build.rs` (git sha + RFC3339 build timestamp via `cargo:rustc-env`, falling back to `"unknown"` outside a git checkout — task 1); `SDLCState::to_committed_state_json` now stamps additive top-level `build.git_sha`/`build.built_at`, `writer`, and `host.{hostname,pid}` keys derived internally from `build_info::host_stamp()` rather than a 7th tail parameter, with `from_committed_state_json` still parsing files missing all three keys (task 2); `committed_artifact_is_stale(value, current_run_id)` added, keying only on `run_id` (never `final_validation` presence, since `SaveStateNode` writes that `null` on every save), backed by a 6-test `tests/it/artifact_identity.rs` suite including a dedicated negative-control test proving the stale assertion is real per D68 constraint 4 (task 3); `GET /health` now returns `build.git_sha`/`build.built_at` (task 4); `LaneLogEntry` gained optional `run_id`/`writer`/`build_sha`, stamped via a new `with_identity()` at every `append_lane_log_line` call site in `integrate_chain_impl` — closed lines get the real `step_run_id`, no-child-ran bail lines get `None` but still stamp `writer`/`build_sha` (task 5); task 6 ran the full validation suite, fixing a pre-existing lane-log exact-shape test broken by task 5's additive fields rather than reverting the design. Full gate green — fmt, clippy `-D warnings`, `nextest --workspace` (3185 passed, 0 failed, 21 skipped), release build. Closes `EN.11.A`. Docs updated: `docs/data-contract.md`, `docs/sdlc-flow-workflow.md`, `docs/orchestration-workflow.md`. Next: `EN.ticket.vault-dependent-tests-must-skip-not-fail`.

```
75d95ec docs: update docs for EN.11.A
a732ca9 test: allow additive identity keys in fixed-shape lane-log test (EN.11.A task 6)
5034f3d feat: implement EN.11.A-task5
86080e1 feat: implement EN.11.A-task4
a76e25a feat: implement EN.11.A-task3
ff9f252 feat: implement EN.11.A-task2
4874418 feat: implement EN.11.A-task1
```

---

## [run: 2026-08-27]

### Security fix: `event-listener` RustSec bump

`cargo update -p event-listener` (`5.4.1` -> `5.4.2`), fixing `RUSTSEC-2026-0221` (unsound
`Send`/`Sync` impl on `StackSlot`, reachable via `sqlx-core`). No `Cargo.toml` edit. Part of a
fleet-wide `cargo audit` pass this session (see HQ's `docs/rust-dependency-audit.md`) — this repo's
build-speed fix (dead `sccache` removal, `[profile.dev]`, single `tests/it.rs` binary) already
landed 2026-07-29, so today's change here is the dependency bump only. `cargo audit` is down to two
upstream-blocked findings: `h2` 0.3.x (pinned by `actix-http`'s `h2 = "^0.3"`, no compatible upgrade
exists) and `spin` (yanked, via `flume`/`sqlx-sqlite` — possibly unreachable in the actual build).
Full gate chain green: fmt, clippy `--all-features`, nextest `--workspace --all-features` (3168
passed), release build.

```
6892297 security(deps): bump event-listener for RustSec fix
```

## [run: 2026-08-24]

Implemented `EN.12.E` — `WORKFLOW_DISPATCH`: a chain step can run a registered workflow, not just a block — across 7 tasks via `/sdlc-flow` on branch `EN.12.E-flow`, PASS verdict. `ChainStep` gains `kind: StepKind` (block/dispatch/command, default block) plus forward-compatible `LaneDirectives`/`LaneBudget` parsing (`deny_unknown_fields` removed from both structs, positively controlled live against a real deny-unknown-fields failure) (task 1); two guard tests pin `EngineKind`'s closed two-variant set and confirm no `From<&str>`/`From<String>` impl exists via source-scan tests matching the module's existing pattern (task 2); new `dispatch.rs` resolves a dispatch step's `block_id` as a `Dispatcher` registry key and runs it in-process, stopping the chain with a named `UnknownWorkflowKey` diagnostic on an unregistered key or `ChildFailed` on a failed node, never selecting an `EngineKind` or falling through to a block invocation (task 3); `execute_step` now refuses non-Block kinds via a new `ExecuteError::WrongStepKind`, keeping dispatch routing entirely out of the SDLC engine path (task 4); `integrate_chain_with_dispatch` routes dispatch steps to `execute_dispatch_step` and records the outcome as a journal row (via EN.12.D's journal seam) with no `lane-log.jsonl` line, added as a new entry point so `integrate_chain`/`integrate_chain_with_journal` stay byte-identical (task 5); mixed `[research, block]` chain integration coverage (journal ordering, single lane-log line), an unregistered-dispatch-key-stops-the-chain case, and an end-to-end forward-compatible lane-segment field parse test added to `tests/it/orchestration.rs` (task 6); full gate green — fmt, clippy `--all-features -D warnings`, nextest `--workspace --all-features` (3168/3168 passed, 21 skipped), release build (task 7). Notable decisions: `kind` sits adjacent to `directives` on `RawBlockPosition` (sibling field, not nested), per the block spec's wording; `execute_dispatch_step` routing lives in `integrate.rs`'s per-step loop, not inside `execute_step`, per `dispatch.rs`'s own "never through `execute::execute_step`" doc contract. Closes `EN.12.E`. `docs/orchestration-workflow.md` updated. Next: pick up from `planning/status.md`'s `next` frontmatter list (`EN.ticket.vault-dependent-tests-must-skip-not-fail`, `EN.5.B2`, `EN.5.C`, `EN.6.L`, and others).

```
250fa58 docs: update docs for EN.12.E
830364c feat: implement EN.12.E-task6
260ab3e feat: implement EN.12.E-task5
3943f16 feat: implement EN.12.E-task4
d372aba feat: implement EN.12.E-task3
abd2fd3 feat: implement EN.12.E-task2
bacc7a1 feat: implement EN.12.E-task1
```

---

## [run: 2026-08-24]

Implemented `EN.12.D` — a durable run journal — across 8 tasks via `/sdlc-flow` on branch `EN.12.D-flow`, PASS verdict. `engine-contract::journal` adds `JournalRow`/`JournalDecisionKind` (StepIntegrated, StepBailed, GateRefused, StateWriteVerificationFailed, BudgetHalted, ResolvedPolicy) with serde round-trip tests (task 1); `engine-store` gains `insert_journal_row`/`list_journal_rows_for_campaign` following the `events` table's sqlx shape and self-skip discipline (task 2); `durable.rs`'s mpsc bridge widens to a `DurableItem::{Snapshot,Journal}` enum so journal writes ride the existing pool-is-None self-skip off the run's hot path (task 3); `integrate_chain_with_journal` emits a journal item at every decision point — bail, gate refusal, state-write verification failure, integrated step, and budget halt — plus one `ResolvedPolicy` item per step carrying the model tier/transport actually used rather than the configured value, added as a new entry point behind a byte-identical `integrate_chain` wrapper so ~20 existing call sites needed no change (task 4); `GET /campaigns/{id}/journal` registered in `http::configure`'s route table (served inside bastion's `/api` scope), self-skipping to 404 with no `DATABASE_URL` (task 5); a D57 `notes.md`/`review.md` renderer in `engine-serve/src/journal.rs`, golden-tested field-by-field against `roadmap_status_discovery.py` and `/consolidate-run`'s actual parsing (task 6); route-level integration coverage for bailed, budget-halted, and repo-less campaigns, `#[ignore]`-gated like `postgres_round_trip.rs` for the three Postgres-backed cases (task 7); full gate green — fmt, clippy `--all-features -D warnings`, nextest `--workspace --all-features` (3148 passed, 21 skipped, 0 failed), release build (task 8). Notable decisions: `JournalRow` carries no telemetry fields by design (decisions are sparse/semantic, telemetry is dense/numeric — kept out per the block's explicit UNGROUNDED-vs-settled split); the renderer takes a caller-supplied `RunRecordMeta` rather than deriving repo/roadmap from rows, since journal rows don't carry them. Docs patched: `docs/architecture.md`, `docs/data-contract.md`. Closes `EN.12.D`. Next: pick up from `planning/status.md`'s `next` frontmatter list (`EN.ticket.vault-dependent-tests-must-skip-not-fail`, `EN.5.B2`, `EN.5.C`, `EN.6.L`, `EN.12.B`, and others).

```
e1b13be docs: update docs for EN.12.D
e182b33 feat: implement EN.12.D-task7
6429863 feat: implement EN.12.D-task6
82425c1 feat: implement EN.12.D-task5
457fe31 feat: implement EN.12.D-task4
c173d7d feat: implement EN.12.D-task3
ed8d75b feat: implement EN.12.D-task2
8ba4579 feat: implement EN.12.D-task1
```

---

## [run: 2026-08-24]

Implemented `EN.5.G` — `LINKEDIN_POST`, drafting LinkedIn post candidates from the week's real work — via `/sdlc-flow` on branch `EN.5.G-flow`, all 8 tasks passed, PASS review. Task 1 added `LinkedInPostEventSchema` plus traceable `PostCandidate`/`WorkSource` types, enforcing a non-empty `sources` invariant at the deserialization boundary rather than by convention. Task 2 added `WorkSourceNode`, gathering git commits, `log.md` entries, and `planning/decisions/` files for a date range over an injectable `CommandRunner`/`FileReader`/`DirReader` seam. Task 3 added the four-layer `LinkedInPostPolicy` (draft/critic/translate tiers, `max_critic_iterations`, `candidate_count`, `translate_enabled`) with `baseline`/`cheap-fast`/`thorough` profiles. Task 4 added `PostDraftNode`, a `ClaudeCodeStep` model node proposing candidates that drops any whose sources come back empty and surfaces model-flagged `unsupported_claims` rather than emitting them. Task 5 added `BrandCriticNode` (a deterministic pre-scan for three of `brand.md`'s six anti-slop checks, model judgment for the rest) and `ReviseNode`. Task 6 assembled and registered the full graph — `WorkSourceNode -> PostDraftNode -> PostCandidateSelectNode -> BrandCriticNode -> CriticRouterNode -> {TranslateGateNode | ReviseNode -> BrandCriticNode}` — as `LINKEDIN_POST` in `engine-serve`, writing a local `CriticRouterNode`/`TranslateGateNode` rather than reusing `content_pipeline`'s literally (their hardcoded upstream reads don't match this workflow's shape). Task 7 added the `linkedin_post_e2e.rs` integration suite (traceable candidates, a revise-then-pass loop, the iteration-cap exit, an empty-range no-fabrication case), building a fresh registry with small local replicas of the three private adapter/router nodes. Task 8 validated the full gate on the integrated tree — fmt, clippy `-D warnings`, `nextest --workspace --all-features` (3119 passed), `cargo build --release`, all clean, no code changes needed. Notable decision: scoped without a new okf-core doc model — drafts are run-result-only — since the spec's `EN.6.H1` doc-model dependency is dead (that block is `wontfix` and was never built); documented in the spec's own Notes before implementation started. Closes `EN.5.G`. `docs/linkedin-post-workflow.md` added. Next: `EN.ticket.vault-dependent-tests-must-skip-not-fail`, `EN.5.B2`, `EN.5.C`.

```
7110b7e docs: update docs for EN.5.G
fa8d59f feat: implement EN.5.G-task7
1570747 alias claude-code-rs path dep to claude-sdk-rs
403df12 feat: implement EN.5.G-task6
5575c5f feat: implement EN.5.G-task5
951d57b feat: implement EN.5.G-task4
9b87d35 feat: implement EN.5.G-task3
35baf30 feat: implement EN.5.G-task2
c92d8e9 feat: implement EN.5.G-task1
```

---

## [run: 2026-08-24]

Implemented `EN.6.K` — Brain read-client seam (GET /recall) + ingest-client hardening — via `/sdlc-flow` on branch `en-6k-brain-read-client-flow`, all 4 tasks passed, PASS review. Task 1 added the injectable `HttpGet` seam (`ReqwestHttpGet`/`StubHttpGet`) and `BrainConfig::from_env` (`BRAIN_API_URL` required, `BRAIN_API_KEY` optional) in a new `nodes/brain_client.rs`, consuming the already-settled `D23-brain-read-seam.md` decision rather than re-authoring it. Task 2 added `RecallNode` — GET `/recall`, query sourced from `ctx.event` or a bound upstream, `limit`/`hybrid` builder args, `X-API-Key` auth, stamping `{query, count, results}` per data-contract v1.6.0. Task 3 re-pointed both `PersistToBrainNode` impls and `HarvestApproveNode` at `BrainConfig` instead of a hardcoded `localhost:8000`, and re-pointed `content_pipeline`'s persist node from the nonexistent `/ingest/learning` route to the real `POST /ingest/artifact`, mapping the `LearningArtifact` payload into that route's generic envelope — noted in the spec's Amendment Log as a scope extension beyond the read-client title. Task 4 added the `tests/it/brain_client.rs` integration suite (RecallNode composed in a real workflow, auth-header and 401-halt cases) plus `X-API-Key` header assertions in the `content_pipeline`/`proposal_generator` e2e fixtures; full gate (fmt/clippy/nextest --workspace/release build) green. Notable decisions: `PersistToBrainNode::new()`/`HarvestApproveNode::new()` stayed zero-arg/infallible, resolving `BrainConfig` lazily at process()-time to avoid touching ~10 out-of-scope call sites; `docs/data-contract.md` updated with a non-re-pin changelog row (Pinned Contract Version stays 1.8.0). Closes `EN.6.K`. Next: `EN.5.B2` (regression history + blind judge + change gate), `EN.5.C` (EXTERNAL_INTEL), `EN.5.G` (CONTENT_DRAFT).

```
0b13939 docs: update docs for en-6k-brain-read-client
3edcb07 feat: implement en-6k-brain-read-client-task4
2a3bcdf feat: implement en-6k-brain-read-client-task3
4f9b20a feat: implement en-6k-brain-read-client-task2
67f7ee4 feat: implement en-6k-brain-read-client-task1
```

---

## [run: 2026-08-23]

`EN.11.H` — Crash recovery — resume a campaign instead of hand-cleaning branches — done via `/sdlc-flow` on branch `EN.11.H-flow`, PASS review, all 7 tasks passed. Added a `checkpoint` module (`Checkpoint`/`CheckpointStep`, atomic temp+rename write, a reader distinguishing Found/Absent) at `orchestration/checkpoint.rs` (task 1); `integrate_chain` now writes/extends the per-chain checkpoint after each step's lane-log line on the success path (errors propagate via a new `IntegrateError::CheckpointWriteFailed`) and best-effort flushes it on every cancel/budget/bail path without adding a new parameter — reading any existing checkpoint at the top so a second call against the same `campaign_id` extends rather than clobbers (task 2); worktree-setup's failure path now deletes the branch (`git branch -D`) as well as the worktree, best-effort and non-fatal, so a retry after a failed `git worktree add` no longer fails against an already-existing branch — the pre-change red state was verified by manually stripping the fix and watching the new test fail before restoring it, satisfying base-template D68 constraint 4 (task 3); `engine-serve/resume.rs` gained `plan_campaign_resume()` (NoCheckpoint/AlreadyComplete/Aborted/Plan(resume_at_index, remaining), telling a kill-9 crash apart from an EN.11.F operator abort via the lane-log's terminal Cancelled/BudgetHalted line) and `reconcile_stale_branch()` (best-effort worktree/branch cleanup before a resumed step redispatches) — both kept as free functions with no new AppState field or HTTP route, deliberately avoiding EN.11.E's two-day undetected bastion build break (task 4); the un-gateable live-crash-resume AC was recorded with a verbatim kill-9/resume recipe and an explicit not-run status — no live `bastion serve` reachable from this session, only a local `cargo build` on `core/bastion` as a buildability-only signal (task 5); `docs/suspend-resume.md` gained a "Campaign-level crash recovery (EN.11.H)" section (checkpoint contents, block-boundary-only resume, abort-vs-crash distinction, single-host invariant, out-of-scope limits) plus a `docs/index.md` description update (task 6); task 7 ran the full harness suite over the integrated tree with no code changes needed. Closes EN.11.H. Next: `EN.ticket.vault-dependent-tests-must-skip-not-fail`.

```
cb955e1 docs: update docs for EN.11.H
58d8182 feat: implement EN.11.H-task6
b6c35e0 feat: implement EN.11.H-task4
878ba36 feat: implement EN.11.H-task3
c997f62 feat: implement EN.11.H-task2
dfd86a6 feat: implement EN.11.H-task1
```

---

## [run: 2026-08-23]

`EN.11.F` — The stop button — abort a campaign, and a ceiling that stops it unattended — done via `/sdlc-flow` on branch `EN.11.F-flow`, PASS review, all 7 tasks passed. `budget.rs` gained a `CampaignLedger` that accumulates cost/tokens across a chain's block boundaries, sharing a private `evaluate_budget` helper with the existing per-node `BudgetLedger` so the halt/allow decision is not forked between the two ledger types (task 1); `engine-serve` gained a `CampaignRegistry` mirroring `RunRegistry` plus `POST /campaigns/{id}/abort` returning 401/404/202 on the existing per-run abort contract, without touching `RunRegistry`/`abort_run` (task 2); `FlowInvocation` now carries `cancellation_token`/`budget`, threaded into `default_flow_runner`'s `RunOptions` in place of the prior `RunOptions::default()` (task 3); `integrate_chain` checks the campaign budget ceiling and re-checks cancellation at every block boundary, recording new `Cancelled`/`BudgetHalted` lane-log terminal states distinct from `Closed`/`Bailed` so a clean abort, a budget halt, and a failure are distinguishable in the chain's own record — in-flight/completed blocks are never touched (task 4); integration coverage in `orchestration_chain.rs` proves abort-between-blocks leaves block 1 committed and block 2 unstarted, and a cost cap set to exactly block 1's cost halts only after both boundary checks ran (task 5); the un-gateable "no orphaned `claude` subprocess" AC is recorded explicitly not-run — no live `bastion serve`/Postgres deployment in this sandbox and `bastion abort` doesn't yet call the new campaign route — in `planning/orchestration-run/autonomous-foundation/notes.md`, rather than claimed satisfied by the task 5 fixture alone (task 6); task 7 ran the full harness suite over the integrated tree — fmt, clippy `--all-features -D warnings`, nextest `--workspace --all-features` (2881 passed), release build, cargo audit (1 pre-existing non-gating RUSTSEC advisory, `h2` v0.3.27). This closes the abort half of the split `EN.ticket.orchestration-abort-and-progress` ticket (Wave 0 item SQ-11); the per-step-progress half (SQ-06) stays open under the original ticket ID. `docs/architecture.md`/`docs/orchestration-workflow.md` updated. Next: `EN.ticket.vault-dependent-tests-must-skip-not-fail`.

```
908a898 fix: review pass 1 for EN.11.F
1074255 feat: implement EN.11.F-task5
e65bc40 feat: implement EN.11.F-task4
803e2a3 feat: implement EN.11.F-task3
da0ccc3 feat: implement EN.11.F-task2
d69f51e feat: implement EN.11.F-task1
ad5cb16 docs: update docs for EN.11.F
```

---

## [run: 2026-08-23]

`EN.11.P` — `task` blocks are orchestratable — done via `/sdlc-flow` on branch `EN.11.P-flow`, PASS review, all 7 tasks passed. `EngineKind` now dispatches through `execute_step`/`default_flow_runner` for both `Task` and `Flow`, with an `engine` field threaded end-to-end through `FlowInvocation`/`ExecutionOutcome` and the old `task_engine_is_unsupported` test flipped to assert dispatch instead of rejection (task 1); `integrate.rs`'s `state_path_for` is now engine-aware (selects `sdlc-flow-state.json` vs `sdlc-task-state.json`), and a `reconcile_failed` state surfaces the new terminal `IntegrateError::ReconcileFailed` instead of the generic mismatch (task 2); `engine-serve` registers `SDLC_TASK` (`register_sdlc_task`/`_with_registry`, mirroring the `SDLC_FLOW` factory) into `register_builtin_workflows_with_registry`, preserving the `task/` branch prefix and `sdlc_task` policy resolver on the served path — `sdlc_task_policy_resolver` made `pub` for reuse (task 3); the pre-flight 422 gate and the suspend terminal-state guard widen from `SDLC_FLOW`-only to also cover `SDLC_TASK`, with a suspended/failed `SDLC_TASK` run writing its terminal blocked state to `sdlc-task-state.json` (task 4); `sdlc_task_e2e.rs` pins `ORCHESTRATION`'s mixed task+flow chain — `steps_integrated == 2`, a task block's failed reconcile stopping the chain before the next step dispatches, and a flow-only regression guard (task 5); `docs/orchestration-workflow.md`/`docs/index.md` rewritten off the stale "Only Flow runs today" claim, now documenting per-engine state filenames and terminal `ReconcileFailed` (task 6); task 7 ran the full harness suite over the integrated tree — fmt, clippy `--all-features -D warnings`, nextest `--workspace --all-features` (2862 passed, 17 skipped), release build, cargo audit (1 pre-existing non-gating RUSTSEC advisory, `h2` v0.3.27). This closes `EN.11.P` — the payoff line of the SDLC_TASK port: 349 corpus blocks already carrying `task` are now drivable by ORCHESTRATION, which previously rejected every one of them at `execute.rs:215`. Next: `EN.ticket.vault-dependent-tests-must-skip-not-fail`.

```
0e7f748 docs: update docs for EN.11.P
e792bd4 feat: implement EN.11.P-task6
fb15929 feat: implement EN.11.P-task5
e3937bb feat: implement EN.11.P-task4
d323b72 feat: implement EN.11.P-task3
0fba037 feat: implement EN.11.P-task2
c98fd93 feat: implement EN.11.P-task1
```

---

## [run: 2026-08-23]

`EN.11.O` — SDLC_TASK policy and profiles — done via `/sdlc-flow` on branch `EN.11.O-flow`, PASS review, all 8 tasks passed. Added `crates/engine-core/src/workflows/sdlc_task/{policy,profiles}.rs`: `SdlcTaskPolicy`/`PartialSdlcTaskPolicy` with the four-layer resolve shim and a `to_sdlc_policy()` projection into `SdlcPolicy`, deliberately omitting the six review/docs knobs SDLC_TASK's registry never reads (task 1); `WORKFLOW_KEY` plus the baseline/cheap-fast/thorough profiles required by standing rule 6, and event-aware `resolve_policy_for_run_from` shims (task 2); `SetupWorktreeNode` gained a `with_policy_resolver` seam so SDLC_TASK's registry now resolves `SdlcTaskPolicy` via `sdlc_task.{policy,profiles}` and projects it into the stamped `SdlcPolicy` — live config instead of dead config — with the event schema's `policy` field narrowed to `Option<PartialSdlcTaskPolicy>` (task 3); `planning/harness.json` gained a `sdlc_task.{policy,profiles}` section mirroring the struct's field set exactly, documenting the `test_depth:Full` reconcile-skip trap (task 4); Guard A (advertised knob set == struct field set, derived mechanically) and Guard B (every knob projects to a shared-node-read field), both demonstrated capable of failing by a temporary sabotage-then-revert (task 5); `sdlc_task_e2e.rs` rewired off a stale `sdlc.policy` harness key onto the real `sdlc_task.policy` seam and extended to prove each profile changes a real observable — reconcile `CommandRunner` calls, `ImplementTaskNode` retry counts — through the actual registry, plus a dedicated pin for the `test_depth:Full` reconcile-skip trap (task 6); `docs/sdlc-task-workflow.md` documents the full knob table, four-layer resolution, the three profiles, and the reconcile-skip caveat, with `docs/index.md` updated (task 7); full validation gate green across the integrated tree — fmt, clippy `--all-features -D warnings`, nextest `--workspace --all-features` (2839 tests), release build, cargo audit (1 pre-existing RUSTSEC advisory in a transitive dep, non-gating) (task 8). Closes `EN.11.O`. Next: `EN.ticket.vault-dependent-tests-must-skip-not-fail`.

```
3d86358 feat: implement EN.11.O-task8
ea59e80 feat: implement EN.11.O-task7
986dc57 feat: implement EN.11.O-task6
7dabcdd feat: implement EN.11.O-task5
4cdaada feat: implement EN.11.O-task3
fffcc20 feat: implement EN.11.O-task2
a3f85d7 feat: implement EN.11.O-task1
```

---

## [run: 2026-08-22]

`EN.11.N` — SDLC_TASK graph and schema — done via `/sdlc-flow` on branch `EN.11.N-flow`, PASS review, all 7 tasks passed. Ported base-template's `sdlc-task.js` into `crates/engine-core/src/workflows/sdlc_task/`: the module root, event schema, and `TerminalSignal::ReconcileFailed` with its `derive_committed_status` arm plus a `CloseBlockNode` skip on `reconcile_failed` per D56 (task 1); `TaskTriageRouterNode` — the three-arm deterministic fork (PASS → `UpdateTaskStatusNode`, RETRYABLE-under-budget → `IncrementAttemptNode`, MAJOR_BAIL/exhausted/unknown → `LeanBookkeepNode`), fail-closed on missing upstream results (task 2); `SpecExistsRouterNode`/`LoadTaskStateNode` promoted to real structs with `with_state_filename` builders and `SetupWorktreeNode` gained `with_branch_prefix`, so SDLC_TASK reuses SDLC_FLOW's setup nodes under its own filename/branch prefix without forking them (task 3); the D56 reconcile scope (`select_reconcile_checks`, `FinalValidationNode::with_scope`) verified already complete from a prior pass (task 4); `LeanBookkeepNode` — the lean close-out, widening `wrap_up`'s durable state helpers to `pub(crate)` for reuse, deriving bailed/reconcile_failed/done status, enforcing the fullRun guard from `event.task_range`, with `CloseBlockNode::with_state_source` reading its stamp instead of forking a second closer (task 5); `graph.rs` assembling `WORKFLOW_TYPE`/`schema()`/`registry()`/`registry_for_policy()`/`workflow()` into a `Workflow::new_validated` graph — the first point SDLC_TASK validates end to end — verified already complete from an earlier interrupted attempt (task 6); `docs/sdlc-task-workflow.md` added with its `docs/index.md` row (task 7). Full validation gate green throughout (fmt, clippy `-D warnings`, `nextest --workspace --all-features`, release build). Closes `EN.11.N` — `EngineKind::Task` is now representable AND runnable — unblocking `EN.11.O` (SDLC_TASK policy and profiles) and contributing to `EN.11.P` (task blocks are orchestratable). Next: `EN.11.O`.

```
b1666cf feat: implement EN.11.N-task7
6e82871 style: cargo fmt after the EN.11.N bail
f77dc68 feat: implement EN.11.N-task6
b85b585 feat: implement EN.11.N-task5
8ed7cae chore(harness): pull base-template dfbb9a6 — state skills cover mev's other write verbs
0efb146 feat: implement EN.11.N-task4
f43b816 feat: implement EN.11.N-task3
d56787b feat: implement EN.11.N-task2
```

---

## [run: 2026-08-21]

### Structured logging — one greppable line per node, with run and campaign ids — closes `EN.11.I`

- **What:** `tracing`/`tracing-subscriber` added to the workspace dependency table, consumed only by
  `engine-core` and `engine-serve`; `engine-serve` gained an idempotent `init_tracing()` (JSON layer,
  `flatten_event(true)`, `ENGINE_LOG` env-var filter knob defaulting to `info`) (task 1). `Workflow::
  walk` and the `node_context` dispatch site carry `#[instrument]` spans recording `run_id`/
  `campaign_id`, propagated across `OrchestrationRunNode`'s `spawn_blocking` boundary by explicitly
  re-capturing the current span and dispatcher inside the closure (task 2). All real production
  `eprintln!` call sites in `engine-core`'s 5 named src files migrated to structured `tracing` events,
  and a `tracing::error!` naming the node and failure added at the dispatch `Err` branch to satisfy
  the "a failing node emits a structured event" acceptance criterion, since no prior call site covered
  it (task 3). `engine-serve`'s 4 src `eprintln!` sites migrated to structured events; `term-core`'s
  sole site was removed rather than routed, since `parse_sessions` has no in-workspace caller (task 4).
  The JSON wire shape was pinned — `node_context` now emits an explicit `run_id`/`campaign_id`/`node`
  event per dispatch (span fields alone don't flatten to event top-level) — via a new
  `crates/engine-core/tests/it/structured_logging.rs` suite covering dispatch order, failure events,
  `spawn_blocking` propagation over the real JSON writer, and a self-verifying zero-`eprintln!` count
  check (task 5). Full validation gate green — fmt, clippy `--all-features -D warnings`, nextest
  `--workspace --all-features` (2749/2749 passed, 17 skipped), release build, cargo audit (task 6,
  validate-only, no code changes). All 6 tasks passed, PASS review. This closes `EN.11.I` — the
  observability gap seams.md flagged as the binding constraint on Wave 2-3 orchestration gates is now
  closed: every node dispatch and failure produces one greppable structured line carrying its run and
  campaign identity. Docs updated: `docs/deployment-launchd.md`. Next: `EN.5.B2`, `EN.5.C`, `EN.6.K`,
  `EN.5.G`, `EN.4.D`, `EN.11.M`, `EN.12.A`, `EN.12.B`, `EN.12.D` per the `next` frontmatter list.

```
54fe8de feat: implement EN.11.I-task5
4e0c875 feat: implement EN.11.I-task4
dab7455 feat: implement EN.11.I-task3
3a2595d feat: implement EN.11.I-task2
a68757e feat: implement EN.11.I-task1
e830f03 docs: update docs for EN.11.I
f4a54f9 chore: init worktree EN.11.I-flow
```

---

## [run: 2026-08-21]

### Campaign identity — a chain of runs has an address — closes `EN.11.E`

- **What:** `docs/data-contract.md` re-framed as the canonical data contract per D78, absorbing
  the outgoing 1.7.0 canonical content before authoring anything new (task 1); `campaign_id:
  Option<String>` threaded through `execute.rs`/`graph.rs`/`integrate.rs`'s `FlowInvocation`/child
  run seam so a run knows which campaign it belongs to (tasks 2-3); `crates/engine-serve/src/
  live_state.rs` gained `RunRecord.campaign_id` and `list_campaign_runs`, merging the live map and
  completed ring with deterministic ordering and a `possibly_truncated` flag for ring eviction
  (task 4); `GET /campaigns/{id}` registered in `crates/engine-serve/src/http.rs`, returning the
  campaign's runs plus a cost/token rollup read from `OrchestrationRunNode`'s `campaign_members`,
  verified by 5 HTTP-level `actix_web::test` cases against the real router (task 5); `docs/
  data-contract.md` bumped to Contract Version 1.8.0 documenting campaign identity and the new
  route, plus a Consumer re-pin obligations section naming orchestrator's and bastion's observed
  versions (task 6); full validation gate green — fmt, clippy `--all-features -D warnings`,
  `cargo nextest run --workspace --all-features`, release build, cargo audit (task 7). All 7 tasks
  passed, PASS review. This closes `EN.11.E` and unblocks `EN.11.G`'s campaign-wide cost rollup
  (previously scoped out for want of an identity to roll up to). The campaign id is a first-class
  field on the wire, never hidden in free-form `metadata`, per seams.md seam 11. Cross-repo re-pin
  in `orchestrator`/`bastion` is out of scope for this block — filed as follow-ups on those lanes
  per D78's Consequences section. Docs updated: `docs/architecture.md`, `docs/
  orchestration-workflow.md`. Next: `EN.11.F` (the stop button) and `EN.11.H` (crash recovery),
  both previously blocked, are candidates now that a campaign has an address.

```
07cc84b docs: update docs for EN.11.E
738a2ff feat: implement EN.11.E-task6
15d45e0 feat: implement EN.11.E-task5
fdf8da8 feat: implement EN.11.E-task4
29cd67e fix: fix pass 1 for EN.11.E-task3
fbc7320 feat: implement EN.11.E-task3
28870aa fix: fix pass 1 for EN.11.E-task2
8e4f6a0 feat: implement EN.11.E-task2
```

---

## [run: 2026-08-19]

### OTel telemetry on the pane-launch command — closes `EN.ticket.otel-pane-telemetry`

- **What:** Cleared the ticket's external OTLP-collector-spike dependency by running it locally
  (Docker `otel/opentelemetry-collector-contrib`, this machine, not the Mini) — confirmed
  `claude_code.cost.usage` exports over OTLP when `CLAUDE_CODE_ENABLE_TELEMETRY=1` +
  `OTEL_METRICS_EXPORTER=otlp` are set on the `claude` launch, with `OTEL_RESOURCE_ATTRIBUTES`
  (`run_id`, `node.identity`) attached to the metric's resource. Authored the ticket spec
  (`planning/EN.ticket.otel-pane-telemetry/tasks.json`), correcting a stale fact along the way —
  the pane-launch command actually lives in `EN.10.A`'s `LiveClaudeSessionNode`
  (`crates/engine-core/src/nodes/terminal/live_claude.rs`), not `EN.9.D`'s `TerminalSessionNode`
  (which only creates/leases the tmux session and never types a command into it). Ran `/sdlc-task`;
  hit and fixed one spec bug of my own mid-run (a breaking `build_command_line` signature change
  split across two tasks left the repo non-compiling between them — merged into one compilable
  unit) and one over-broad grep check (matched the tests' own negative assertions). Final
  implementation: `build_command_line` now prepends `CLAUDE_CODE_ENABLE_TELEMETRY=1
  OTEL_METRICS_EXPORTER=otlp OTEL_RESOURCE_ATTRIBUTES='run_id=...,node.identity=...'` ahead of the
  resolved `claude` binary/argv, value-only shell-quoted. Full validation suite passed (fmt,
  clippy `-D warnings`, `cargo nextest run --workspace`, release build). Corrected
  `docs/terminal-nodes.md`'s `LiveClaudeSessionNode` row (wrongly claimed the launch goes through
  `claude_code_rs::execute`) during `/close-out`'s doc-patch step. Filed carryover
  `h2-dos-advisory-unfixable-without-actix-web-major-upgrade` for a pre-existing `cargo audit`
  gate failure (RUSTSEC-2026-0258, transitively via `actix-web`, no fix short of a major upgrade)
  found while running close-out's validation suite, and fixed an unrelated pre-existing `cargo fmt`
  violation in `crates/engine-serve/src/orphan.rs`.
- **Why:** N7 — `/usage` scraping was rejected (not deferred) as a cost-correlation source: its
  totals reset on `/clear`, `/cost` doesn't exist, and a pane render has no stability contract.
  OTel resource attributes give free run/session correlation since the design already controls the
  launch command.
- **Refs:** `crates/engine-core/src/nodes/terminal/live_claude.rs`,
  `planning/blocks/EN.ticket.otel-pane-telemetry.json`, `docs/terminal-nodes.md`

### Orphan-reconciled runs 404 on GET /events/{id} — LiveStateStore seed fix in `orphan.rs`

- **What:** Fixed `crates/engine-serve/src/orphan.rs`: `reconcile_orphans` now takes a `&LiveStateStore` and calls `live.mark_terminal(row.id, &row.task_context, row.workflow_type, row.created_at, row.updated_at)` for each row it reconciles, right after `persist_reconciled`. `get_event` (`http.rs`) serves reads only from `LiveStateStore` — no Postgres fallback, by design, since CI has no `DATABASE_URL` — so a run the boot sweep reconciles in Postgres was previously never seeded into the calling process's in-memory store and 404'd forever (idempotency means a later sweep never re-lists it, so there was no second chance). Routing the reconciled row through the existing `mark_terminal` hook also means a boot-reconciled crash now fires the same terminal-run failure notification a live failure would. Updated all 7 existing `orphan.rs` test call sites to pass a `LiveStateStore::new()`, and added a regression assertion (`reconciles_every_candidate_and_names_the_stuck_node`) that `live.get_record(id)` returns a terminal record post-sweep. Full workspace suite: 2239 passed. Left a prompt for a follow-up `bastion` agent — its boot-sweep call site (`src/serve/mod.rs` ~line 516) and 4 tests need the same `&live_store` argument threaded through; `live_store` is already hoisted in scope there, so it's a one-line production fix plus test updates, not new plumbing.
- **Why:** Reported directly against the Mac Mini production `engine-serve` — confirmed 2026-08-18 by killing `engine-serve` mid-run, letting launchd restart it, and observing the boot orphan sweep correctly mark the run terminal in Postgres while `GET /events/{id}` still 404'd. Breaks any HTTP poller checking terminal status post-crash-recovery, including `scripts/health_check.sh --full`'s workflow-trigger check and `bastion-ui`'s run-status screen.
- **Refs:** `crates/engine-serve/src/orphan.rs`, `crates/engine-serve/src/live_state.rs`, `crates/engine-serve/src/http.rs::get_event`

## [run: 2026-08-18]

### Autonomy research dossier — nine documents, nine tickets, no implementation

- **What:** Produced `planning/orchestration-extensions/` — 9 documents, ~3,500 lines — as research input for a planning session on autonomous multi-repo orchestration. `index.md` routes; `research-overview.md` summarises; `autonomy-brief.md` carries the argument plus seven appendices including a full code map; `sdlc-flow-parity.md`, `sdlc-task-review.md` and `sdlc-task-port-design.md` cover the JS engines and the `SDLC_TASK` port; `leverage-inventory.md` inventories what mev and bastion already do; `corpus-review.md` (5 parts) tests the whole thing against 165 carryovers and 62 backlog items fleet-wide. Filed and registered nine defect tickets with block records and task specs, all under `epics: ["engine-orchestration"]`. Marked one stale `memory.md` entry superseded and deleted one resolved carryover.
- **Why:** The operator wants an orchestration system that runs campaigns overnight, creates its own follow-on work, and blocks on a human when it must. Before planning that, we needed to know what actually exists. Two P0 defects surfaced that were not previously known: no Rust node ever closes a block (so a chain's second block can never become ready), and a failed final gate still writes `"status": "done"` (so a chain advances on a red build). Also found HQ **D74**, which already governs this programme and which the initial research had not cited.
- **Refs:** `planning/orchestration-extensions/index.md`, HQ `docs/decisions/D74-orchestration-is-a-workflow.md`, `planning/handoff.md`

## [run: 2026-08-18]

### ORCHESTRATION workflow — sequence SDLC_FLOW runs across repos from a lane chain closes EN.10.B

Drove `EN.10.B` via `/sdlc-flow` on branch `EN.10.B-flow`, all 7 tasks passed, PASS review.
`chain.rs` (`crates/engine-core/src/workflows/orchestration/chain.rs`) resolves a lane chain from
mev's `planning/lane-segments.json` — sorted by `(segment, position)`, refusing to start via a
`ChainError::Held` when a HELD-UNTIL target is still open (an injectable `is_block_open` closure),
and failing loudly (`ChainError::ParseFailed`, via `#[serde(deny_unknown_fields)]`) on any malformed
directive shape; `resolve_explicit_chain` bypasses lane-file parsing for an explicit block list
(task 1). `gates.rs` adds `check_dependencies` (unmet `depends_on` edges never start a block, naming
the edge + repo) and `AdmissionGate` (wraps the `EN.9.F` `AdmissionControl` so a saturated ceiling
queues rather than proceeds or fails) (task 2). `execute.rs`'s `execute_step` invokes `SDLC_FLOW`
(never reimplemented) per chain step via an injectable `FlowRunner`, selecting
`EngineKind::Task`/`Flow` from an injectable `resolve_engine` closure and resolving each step's repo
to an absolute cwd via `RepoRegistry`; `default_flow_runner` builds a fresh policy-aware `SDLC_FLOW`
`Workflow` per invocation, mirroring `engine-serve`'s `register_sdlc_flow_with_registry` factory
(task 3). `integrate.rs` adds operator-hold pause-and-resume (`HoldSource`/`wait_for_clearance`),
state-write verification against `sdlc-flow-state.json` `status=="done"`, roadmap-dir resolution per
`/begin-orchestration` Step 1C, an exactly-one-line `lane-log.jsonl` append, and the
`integrate_chain` loop tying gates+execute+hold+verify+log together (task 4). The `ORCHESTRATION`
workflow (`graph.rs`) assembles tasks 1-4 into a single `OrchestrationRunNode`, bridging its
deliberately non-`Send` future into `Node::process`'s `Send` bound via `tokio::task::spawn_blocking`
running a fresh current-thread runtime; one `hold_poll_interval_ms` policy knob resolved through the
standard four-layer precedence with baseline/cheap-fast/thorough profiles, registered in
`engine-serve`'s dispatcher via `register_orchestration` (task 5). A two-repo end-to-end integration
suite (`crates/engine-core/tests/it/orchestration.rs`) covers per-step cwd, unmet-dependency
refusal, admission waiting at capacity, HELD-UNTIL refusal, operator-hold pause/resume,
one-lane-log-line-per-block, and loud failure on a corrupted state write — using
`tokio::task::LocalSet`/`spawn_local` for the two concurrency-sensitive tests since `integrate_chain`
is `!Send` (task 6). Task 7 ran the full validation suite (fmt, clippy `--all-features -D warnings`,
`nextest --workspace --all-features`, release build, `cargo audit`) on the integrated tree — all
green, no code changes needed. Closes `EN.10.B`. Docs: `docs/architecture.md` updated.

Next: `EN.10.C` — Sanctioned-engine guard — a block node can only reach `/sdlc-task` or `/sdlc-flow`.

```
4dc4682 docs: update docs for EN.10.B
668fc0a feat: implement EN.10.B-task6
22ef1c6 feat: implement EN.10.B-task5
0b1b52d feat: implement EN.10.B-task4
1b5a4af feat: implement EN.10.B-task3
63ba566 feat: implement EN.10.B-task2
cd99839 feat: implement EN.10.B-task1
ff08479 Merge pull request #48 from bredmond1019/EN.10.A-flow
```

---

## [run: 2026-08-18]

### Terminal node holds a tmux session and can open a live Claude Code session mid-workflow closes EN.10.A

Drove `EN.10.A` via `/sdlc-flow` on branch `EN.10.A-flow`, all 5 tasks passed, PASS review.
`HeldSessionNode` (`crates/engine-core/src/nodes/terminal/held_session.rs`) extends the Phase 9
scripted terminal nodes to a HELD session: it acquires a tmux session once per run under the
`EN.9.B` lease and carries it across node boundaries via a process-global registry keyed by
session name, spawning a background renewal loop with a `lease_ttl_ms`/`renew_interval_ms` policy
resolved through the standard four-layer precedence and three named profiles (task 1). That
renewal loop now checks tmux liveness before every renew tick and publishes a distinguishable
`HeldSessionFailure` (`ExternallyKilled` vs `LeaseLost`) over a `tokio::sync::watch` channel, so a
re-entrant `process()` call surfaces the loss as a typed `NodeError` instead of hanging or silently
succeeding (task 2). `LiveClaudeSessionNode` (`crates/engine-core/src/nodes/terminal/live_claude.rs`)
launches an interactive `claude` CLI session inside the held tmux pane by typing the resolved
command via `TerminalDriver::send_keys` — deliberately not `claude_code_rs::execute`, which runs
headless and is never attached to a pty, so it would be invisible to `bastion sessions` — reusing
`claude_code_rs::Config` for model/continue/resume and reading the target session name from the
upstream `HeldSessionNode`'s `ctx.nodes` entry via the existing `session_input`/`InputBinding`
convention (task 3). A real-tmux integration suite (`crates/engine-core/tests/it/held_session.rs`,
wired into `tests/it/main.rs`) covers session identity across two node boundaries, lease renewal
over a compressed TTL, orphan reconcile of an abandoned lease via `steal_after`, and error-not-hang
on external kill — no mocking, per the block's testing strategy (task 4). Task 5 confirmed the full
gate on the integrated tree: fmt, clippy, nextest --workspace (2455 passed), release build, cargo
audit, and `cargo tree -i term-attach` confirming `engine-core` stays absent from that dependency
graph. Docs updated: `docs/architecture.md`, `docs/terminal-crates.md`. This closes `EN.10.A`.
Next: `EN.10.B` — ORCHESTRATION workflow — sequence SDLC_FLOW runs across repos from a lane chain.

```
58006b5 docs: update docs for EN.10.A
7505cd8 feat: implement EN.10.A-task4
3598be4 feat: implement EN.10.A-task3
80ea228 feat: implement EN.10.A-task2
c2c055b feat: implement EN.10.A-task1
d226bf1 Merge pull request #47 from bredmond1019/EN.9.G-flow
73fa14e chore: wrap up EN.9.G
cf34f1b docs: update docs for EN.9.G
```

### Operator-hold policy surface + the Blocked-edge bridge receiver close EN.9.G

Drove `EN.9.G` via `/sdlc-flow` on branch `EN.9.G-flow`, all 4 tasks passed, PASS review.
`HoldPolicyNode` (`crates/engine-core/src/nodes/terminal/hold_policy.rs`) is the per-workflow
operator-hold policy surface over the `EN.9.B` session lease: 60s default grace, a doubly-optional
`steal_after_ms` (custom serde so an explicit `null` — fail-closed — is distinct from a layer
leaving the key untouched), resolved through the standard four-layer precedence
(event override > profile > `harness.json` > built-in default) with baseline/cheap-fast/thorough
profiles, stamping its resolved values into `ctx.nodes` (task 1). `engine-serve::blocked_bridge`
is the receiving half of the Blocked-edge bridge: it re-evaluates a live level predicate
(current state == Blocked) on every trigger via injectable `LevelSource`/`Notifier` traits before
delivering into an `EN.8.B` `OperatorQueue`, exiting silently on a stale trigger; a deterministic
`item_id` (`blocked-edge:<session>`) collapses repeated triggers for one session onto the queue's
single delivery slot (task 2). `crates/engine-core/tests/it/blocked_bridge.rs` reproduces the
fire-then-resolve-within-one-tick race with a fixed injected clock rather than real sleeps,
asserting no notification fires on the stale trigger (task 3). Task 4 (validation-only) confirmed
the full gate green — fmt, clippy `-D warnings`, nextest --workspace (2426 passed), release build,
cargo audit clean of vulnerabilities (pre-existing advisory warnings only). Docs updated:
`docs/architecture.md`, `docs/terminal-crates.md`, `docs/operator-payload-contract.md`. This closes
`EN.9.G`. Next: `EN.10.A` — Terminal node holds a tmux session and can open a live Claude Code
session mid-workflow.

```
cf34f1b docs: update docs for EN.9.G
ab61251 feat: implement EN.9.G-task3
bc84479 feat: implement EN.9.G-task2
271f68a feat: implement EN.9.G-task1
0f5f210 feat: implement EN.9.F-task4
e19fab3 feat: implement EN.9.F-task3
6711879 feat: implement EN.9.F-task2
54404b5 feat: implement EN.9.F-task1
```

---

## [run: 2026-08-18]

### Write and await nodes land at contract v0.2.0, closing Phase 3 of the terminal-node work

- **What:** Drove `EN.9.E` via `/sdlc-flow` on branch `EN.9.E-flow`, all 5 tasks passed, PASS
  review. `crates/engine-core/src/nodes/terminal/predicate.rs` adds `AwaitPredicate`
  {Marker,Detect,Regex,Silence,ExitCode} and a pure `evaluate()` over an already-collected
  `Observation`, covering the four load-bearing marker rules — `{out}.{nonce}.done` path,
  content equal to the nonce, never `remove_file`, and `out` mtime postdating the send — with
  `marker_path(out, nonce)` exported as the single source of truth shared with the sender.
  `send.rs` adds `TerminalSendNode`: refuses org-floor-denied commands via a typed `NodeError`,
  re-verifies the session lease with `SessionLease::renew` immediately before every send, holds
  a per-session `tokio::sync::Mutex<()>` across the check+send, and dedupes back-edge re-entries
  by a `send_id` recorded in a tmux user-option (driver-observable, survives a fresh `ctx`).
  `await_node.rs` adds `TerminalAwaitNode`: a bounded, cancellable poll over `AwaitPredicate`
  with its own timeout (the runner has no deadline field), a `CancellationToken` taken through
  its own builder and `select!`ed every tick, and a four-layer-resolved `AwaitPolicy`
  (poll_interval_ms/timeout_ms) with baseline/cheap-fast/thorough profiles stamped into
  `ctx.nodes` on every non-cancelled return. `tests/it/terminal_send_await.rs` (declared in
  `tests/it/main.rs` per standing rule 8) drives real `Workflow::run_with` walks against
  `StubTerminalDriver` covering stale-marker rejection, send_id idempotency across two
  fresh-ctx `Workflow` instances sharing one driver, and cancellation bounded within 5 seconds.
  Full validation gate green: fmt, clippy `--all-features -D warnings`, nextest
  `--workspace --all-features` (2370/2370, 17 skipped), release build, cargo audit — no code
  changes needed at the validation task. `docs/architecture.md` updated. Closes `EN.9.E`,
  completing Phase 3 of the terminal-node work atop `EN.9.D`.
- Next: pick up the next queued block per `planning/status.md`'s Current focus / next pointer.

```
6614eea docs: update docs for EN.9.E
b241938 feat: implement EN.9.E-task4
d7ea34b feat: implement EN.9.E-task3
9d7a385 feat: implement EN.9.E-task2
0a43fab feat: implement EN.9.E-task1
```

---

## [run: 2026-08-18]

### Read-only terminal nodes land, TERMINAL_PROBE proven against the real Mac Mini tmux

- **What:** Drove `EN.9.D` via `/sdlc-flow` on branch `EN.9.D-flow`, all 7 tasks passed, PASS
  review. `TerminalSessionNode` and `TerminalObserveNode` (`crates/engine-core/src/nodes/terminal/`)
  give the engine its first terminal-facing nodes: session-ensure stamps
  `@engine_run_id`/`@engine_created_at` before any fallible work and acquires the `EN.9.B` lease
  with a deterministic nonce so a back-edge re-entry is a no-op reuse rather than a foreign-lease
  collision; observe resolves the upstream session via a `session_input: InputBinding` field
  (reused across future terminal nodes through a `HasSessionInput`/`WithSessionInput` trait pair),
  captures the pane once, and runs it through new pure `bound_pane_tail`/`PaneTailPolicy` helpers
  that bound, redact, and hash — with HashOnly vs. full-content policy resolved from whether this
  run created the session or adopted an existing one. The `TERMINAL_PROBE` workflow chains both
  nodes and is registered in `engine-serve`'s dispatcher alongside the other builtins.
- **Proven live, not just hermetically:** task 6 built and ran a `terminal_probe` example against
  the Mac Mini's actual tmux (3.5a), pinned `#{session_attached}` golden bytes, and exercised the
  orphan kill-restart recipe against the live `:8090` `bastion serve` instance — recording the
  evidence in `planning/EN.9.D/artifacts/mini-probe-run.txt`. That live run surfaced two real
  term-core defects — `SessionLease::read()` hard-fails on a never-set lease option, and
  `show_option_args` omits tmux's `-v` flag, corrupting `Lease::parse`'s `run_id` field — neither
  fixed in-scope; both recorded as follow-up-ticket recommendations.
- **Full gate green:** fmt, clippy `--all-features -D warnings`, nextest
  `--workspace --all-features` (2319 passed, 17 skipped), release build, cargo audit (advisory
  warnings only, no blocking vulnerabilities).
- **Closes** `EN.9.D`, unblocking `EN.9.E` (write/await nodes at contract v0.2.0, Phase 3).
  `docs/architecture.md` and `docs/terminal-crates.md` updated.
- **Refs:** `planning/EN.9.D/tasks.md`; `planning/EN.9.D/artifacts/mini-probe-run.txt`.

```
4ca4389 docs: update docs for EN.9.D
f3f6690 feat: implement EN.9.D-task6
401e8fb feat: implement EN.9.D-task5
c56d63b feat: implement EN.9.D-task4
f420eb2 feat: implement EN.9.D-task3
edbdde0 feat: implement EN.9.D-task2
dba021a feat: implement EN.9.D-task1
15b0dfc docs: log the approval-ledger read endpoint and its downstream bastion wiring block
```

## [run: 2026-08-17]

### The approval ledger is readable over HTTP, and bastion now owns the one line that turns it on
- **What:** Drove `EN.ticket.approval-ledger-read-endpoint` via `/sdlc-task`, in place on `main`,
  all 5 tasks passed. `crates/engine-serve/src/approvals.rs` adds `list_ledger` and `ledger_stats`
  over `EN.8.C`'s append-only JSONL ledger — newest-first rows with `total` counted before
  `limit`/`offset`, an `item_id` filter, `limit` defaulting to 100 and **clamped** to 1000 rather
  than rejected, and stats splitting the two populations deliberately (`Requeued` excluded from
  median/max, included in `decisions_per_day`). Both routes are registered in `crate::http::configure`
  with `/approvals/ledger/stats` **before** `/approvals/ledger`, first-registration-wins. The
  blocking file read runs inside `web::block`; no synchronous `std::fs` read is reachable from a
  handler body. Nine in-module tests, all driven through the real route table.
- **Why:** `EN.8.C` shipped the ledger as a file behind the `ApprovalLedger` trait with no HTTP
  surface, and `bastion-web:BW.ticket.approval-ledger-view` cannot open a file on another host.
  Carryover `approval-ledger-has-no-engine-read-path` had tracked that since 2026-08-12; it is now
  cleared.
- **Shape, per D15:** the ledger arrives as `Option<web::Data<Arc<dyn ApprovalLedger>>>`, not an
  `AppState` field, so bastion compiles untouched and the routes answer **503** with a stable JSON
  body until wired. The D62 downstream check against `core/bastion` is the only evidence that claim
  holds, and it ran green.
- **Tests are route-table tests on purpose:** every case builds
  `App::new().configure(crate::http::configure)` over a real `FileApprovalLedger` in a tempdir.
  A handler-level test would pass even if the routes were never registered — the exact shape
  carryover `gate-scope-must-be-shown-capable-of-failing` describes. Task 5 additionally ran a D68
  gate-capability check confirming nextest discovers the 9 approvals tests rather than silently
  matching none.
- **Cross-repo:** injected `BA.ticket.approval-ledger-read-wiring` into `core/bastion`'s
  `state.json` (`tracks[7]`, `focus.next`, P2) rather than leaving the follow-up as prose here. Its
  note carries the trap: the reader must be handed the **same** `Arc` the writer builds at
  `src/serve/mod.rs:587` — a second `FileApprovalLedger` resolving `default_ledger_path`
  independently reads an empty file while the writer appends elsewhere, and **neither side errors**.
  That bastion file now has four engine seams waiting on it (`BA.ticket.spawn-schedule-loop`,
  `BA.ticket.orphan-reconcile-wiring`, the approve-and-run seams, and this); take them together.
- **Still unpushable:** `main` is 27 commits ahead of `origin` — `engine-rs-main-ahead-of-origin-unpushable`
  is unchanged and nothing in this repo can clear it.
- **Refs:** `planning/EN.ticket.approval-ledger-read-endpoint/tasks.md`; `docs/approval-ledger.md`;
  carryover `approval-ledger-reader-unwired-in-bastion`.

### Harness pulled up to D67/D66, and the ledger read path finally has an owner

- **What:** Cleared both open items from the engine-lane handoff. (1) Ran
  `sync_downstream_harness.py --repo engine-rs` — 9 files, deferred all of last session because the
  sync copies `.claude/workflows/` and an engine was always mid-run. Brings D67's **mandatory**
  `mev validate-state` after any `state.json` write, the `defect` carryover kind and the
  `reference[]`-vs-`carryover[]` routing rule, D66's tiered heavy-lane concurrency, and a fix to the
  engines' parse-time safety gate: `renderEngineParseChecks` now filters to `.js`, because
  `node --check` throws `ERR_UNKNOWN_FILE_EXTENSION` on a `.md`/`.json` path regardless of content —
  a false positive, and one this repo's specs hit routinely since tasks name `tasks.md`. (2) Authored
  `EN.ticket.approval-ledger-read-endpoint`, 5 tasks: two authenticated GETs
  (`/approvals/ledger`, `/approvals/ledger/stats`) over `EN.8.C`'s ledger.
- **Why:** The harness pull was P2-first in the handoff precisely because no engine was running; it
  is the only safe window. On the ticket: `EN.8.C` shipped the ledger as a JSONL file behind the
  `ApprovalLedger` trait with no HTTP surface, and the operator-surface roadmap splits writer
  (`EN.8.C`) from reader (`bastion-web:BW.ticket.approval-ledger-view`) — but nobody owned the
  engine-side read path, and a browser cannot open a file on another host. Carryover
  `approval-ledger-has-no-engine-read-path` had tracked that since 2026-08-12.
- **The load-bearing design call, recorded because it is reversible:** the ledger reaches the
  handlers as `Option<web::Data<Arc<dyn ApprovalLedger>>>`, **not** as an `AppState` field.
  `AppState`'s fields are public and it is struct-literal-constructed in bastion
  (`src/serve/mod.rs`) plus five engine-serve test files, so a required field would be a cross-repo
  break for a surface bastion is not ready to wire. Additive instead: the routes register
  unconditionally and answer **503** until one `.app_data(..)` line lands in bastion reusing the
  `Arc` it already builds at `src/serve/mod.rs:587`. Same shape as `spawn_schedule_loop` and
  `reconcile_orphans` — an engine seam callable before its host wires it. That makes a **fourth**
  seam waiting on that one bastion file; whoever next works there should take all four together.
- **Two ACs declared un-gateable (D64)** rather than written as if a green suite proved them: the
  additive-seam claim (only evidence is the D62 downstream build of another repo) and reader/writer
  sharing one `Arc` (the call site is in another repo and does not exist yet). The second names its
  failure mode explicitly — a second `FileApprovalLedger` resolving `default_ledger_path`
  independently reads an empty file while the writer appends elsewhere, and **neither side errors**.
- **Not done, and not ours:** the 8 blocks held on `bastion:BA.18.F` (`EN.9.D`->`EN.9.H`,
  `EN.10.A/B/C`, `EN.ticket.otel-pane-telemetry`). Untouched deliberately.
- **Decision recorded:** `D15` — cross-repo-visible serve seams are additive extractors, not
  `AppState` fields. Generalizes the call above past this one ticket, and carries the shared-`Arc`
  requirement plus the reason the D62 downstream check is load-bearing here rather than routine.
- **Refs:** `planning/decisions/D15-additive-seams-over-appstate-fields.md`;
  `planning/blocks/EN.ticket.approval-ledger-read-endpoint.json`;
  `planning/EN.ticket.approval-ledger-read-endpoint/tasks.md`; carryover
  `approval-ledger-has-no-engine-read-path` (now has an owning block, cleared on implement);
  carryover `engine-rs-harness-stale-vs-base-template-d67-d66` (cleared).

### Engine lane closed four blocks; three were found by the consumer, not by our gates
- **What:** Drove the `engine` lane of roadmap `engine-orchestration`. `EN.9.B` (`/sdlc-flow`, 9/9,
  PASS) shipped the async driver seam, session lease, operator hold and capture cache into
  `term-core` behind a non-default `tokio` feature. Three further tickets were **adopted mid-run**,
  each raised by the `bastion` WIRE lane while it prepared `BA.18.F`:
  `EN.ticket.term-core-port-gaps` (5/5 — `send_keys_no_enter`, the whole `BlockedReason`
  sub-classification, and the `awaiting_question` manifest rule `EN.9.A` had dropped outright, so
  term-core could not detect the AskUserQuestion blocked state at all),
  `EN.ticket.term-core-embedded-asset-consts` (3/3 — publishes the `include_str!` targets a
  `pub use` shim structurally cannot re-export), and `EN.ticket.tmux-error-root-cause` (3/3 —
  `TmuxError::root_cause()`, without which the naive fix to bastion's exhaustive status match turns
  503/C001 into 500/C010 with a green suite). `BA.18.F` is fully unblocked. Also fixed a blind
  harness gate: `test`/`fastCommand`/`clippy` now carry `--all-features`.
- **Why:** `EN.9.A` closed PASS on an incomplete extraction, and none of it was catchable here. The
  session surfaced six instances of one pattern — a check whose inputs both come from the artifact
  under test, returning the same green a real check returns: two manifests that both parse while
  one lost a rule; a port matching a planning inventory that had already dropped 11 tests; a
  "verified 257" that is the sum of its own two enumerations; a feature-gated module the standing
  gate never compiled; and the two consumer-found gaps above. Two lanes converged on four
  constraints, the general one being *a gate must be shown to be capable of failing on the block's
  own deliverable*.
- **Correction worth recording:** this lane initially reported `EN.9.B`'s 39 feature-gated tests as
  "never ran". Too strong — its task 9 did run them (164 tests) because the spec's Validate task
  called the feature out explicitly. The gap was ongoing coverage, not an untested block.
- **Refs:** `planning/orchestration-run/engine-orchestration/{notes.md,review.md}`;
  carryover `gate-scope-must-be-shown-capable-of-failing`.

Implemented EN.9.B — Async driver seam and the session lease (Phase 1) — across 9 tasks, all passed, PASS review. Task 1 added a non-default `tokio` feature to `term-core` with `run_tmux_async` sharing one `classify_output` with the sync path, plus `TmuxError::Timeout` that kills a wedged child via `kill_on_drop` instead of leaking it. Task 2 added an object-safe `TerminalDriver` trait with `TmuxDriver` (delegating only to existing tmux.rs argv builders + `run_tmux_async`) and a `StubTerminalDriver` recording exact argv sequences with configurable per-op outcomes. Task 3 added a per-session, ~400ms-TTL, single-flight `CaptureCache` so the hub sweep and a node's await loop coalesce onto one underlying tmux invocation; a failed capture is never cached. Task 4 added the advisory, fail-closed `SessionLease` in `lease.rs` — write-then-read-back arbitration (acquire/renew/release) over tmux user-options, since tmux has no CAS primitive, with jittered backoff derived from `SystemTime` rather than a new `rand` dependency. Task 5 added `OperatorHold` — `@operator_hold` + `#{session_attached}` fallback signals, sends-pause/reads-continue asymmetry, and an injectable-clock 60s detach grace, gating sends via `guard_send`; this required a new `TerminalDriver::display_message` method not in the original file list. Task 6 added `GuardedSender` — a per-session mutex around the literal+Enter send pair with `C-u` line-clear recovery on Enter failure (original error preserved), requiring `send_keys` to split into `send_literal`/`send_enter` so the caller can tell which half failed. Task 7 added `docs/terminal-driver.md` with pointers from `docs/index.md`/`docs/architecture.md`, deferring live-tmux evidence to task 8. Task 8 captured verbatim live-tmux evidence (tmux 3.7b) proving `#{session_attached}` emits `0`/`1`/`0` across detached/attached/detached-again, using a nested-tmux-attach trick, and pinned those bytes as a golden test in `hold.rs`; no contradiction with task 5's design was found. Task 9 validated the full gate: fmt, clippy `-D warnings`, nextest --workspace (2202/2202), build --release, plus `term-core` with and without the `tokio` feature (164/122 tests). Closes `EN.9.B`, unblocking `EN.9.D` (read-only terminal nodes + TERMINAL_PROBE, Phase 2).

Next: EN.9.D — read-only terminal nodes + TERMINAL_PROBE (Phase 2), building on the driver/lease/hold seams shipped here.

```
898593b docs: update docs for EN.9.B
583b085 feat: implement EN.9.B-task8
51f7042 docs: document the terminal driver, session lease, and operator hold
f550a18 feat: implement EN.9.B-task6
5aa978a feat: operator hold with @operator_hold + session_attached fallback (EN.9.B task 5)
8da86ac feat: implement EN.9.B-task4
43e41f2 feat: implement EN.9.B-task3
46e7d85 feat(term-core): TerminalDriver trait, TmuxDriver, StubTerminalDriver
```

## [run: 2026-08-13]

### `EN.9.C` closed — the engine can finally see runs a crash stranded
- **What:** Drove `EN.9.C` via `/sdlc-task`, in place on `main`, all 8 tasks passed. Task 1 added
  `engine_core::completion` (`stamp_completion`/`is_complete`), a run-level `metadata.completion`
  annotation mirroring `cancellation`/`budget`/`suspension`. Task 2 stamped it at **both** terminal
  exits in `spawn_run` (`suspend.rs:504` eviction branch, `:526` main) before the durable write, and
  deliberately not on the plain suspend path. Task 3 added
  `engine_store::list_orphan_candidates` — the first non-by-id select in that crate — as a JSONB
  predicate over the marker's absence plus an `updated_at` cutoff and a hard `limit`. Task 4 added
  `OrphanPolicy` across the four resolution layers. Task 5 added `engine_serve::orphan` with an
  injectable `OrphanLister` seam and `reconcile_orphans`, which fails orphans loudly and never
  resumes them. Task 6 added the stale-run alarm on aged `running`/`suspended` runs, de-duplicated
  so one stuck run produces one item. Task 7 documented it in `docs/orphan-recovery.md`. Task 8
  validated the full gate.
- **Why:** Both Mini plists set `KeepAlive=true`/`ThrottleInterval=10`, so launchd restarts the
  engine within ~10s of a crash — and until now that **hid the evidence**, because a run that
  crashed after node 1 of 5 has no failure marker and `derive_terminal_status` reports it as
  `succeeded`, indistinguishable from a clean finish.
- **Design note:** the block asked for a `SELECT ... WHERE status` query. **There is no `status`
  column** — contract §4's `events` schema is externally owned and this repo has no migrations — so
  the discriminator is the new metadata annotation instead. Operator-confirmed before implementation.
- **Not yet live:** nothing calls the sweep at boot; that call site is in bastion. Carryover
  `orphan-reconcile-unwired-in-bastion`.
- **Refs:** `planning/EN.9.C/tasks.md`; roadmap `operator-surface`, lane `terminal`.

### Lane `terminal` section 1 closed — two blocks, and four defects found in passing
- **What:** Closed `EN.9.C` and `EN.9.A` (PR #45). Along the way: completed `EN.9.A`'s integration
  by hand after `/sdlc-flow` returned `stranded: true`; resolved a two-file merge conflict between
  the unpushed `EN.9.C` commits and the squash-landed `EN.9.A`; repaired a stale spec `Status` line;
  corrected two wrong acceptance criteria in `master-plan.md` at source; and promoted three new
  carryovers.
- **Why the strand matters:** the PR failure had nothing to do with the code — all five repo gates
  passed. The push was blocked by HQ's pre-push `E_SYNC_DRIFT` because the wrap-up bumped
  `status.md`'s timestamp without re-syncing the brain project cache, and the engine reported a bare
  PR failure without the pre-push stderr. Any brain-vaulted repo running `/sdlc-flow` can hit this.
- **Refs:** `planning/orchestration-run/operator-surface/notes.md` + `review.md`.

### `EN.9.A` closed — term-core and term-attach extracted from bastion
- **What:** Drove `EN.9.A` via `/sdlc-flow`, in place on branch `EN.9.A-flow`, all 8 tasks passed,
  PASS review. Tasks 1–4 scaffolded `crates/term-core` and ported bastion's terminal-control code
  into it: tmux argv builders + execution (typed `TmuxError`, `anyhow` fully retired), the
  manifest/golden-test agent-detection engine with its manifests and fixtures, and
  `model.rs`/`claude_state.rs` (session/pane parsing, workspace-trust observer). `AgentDetection`
  now derives `Serialize`/`Deserialize`, and new `set_option_args`/`show_option_args` builders lay
  the groundwork for `EN.9.B`'s session lease. Task 6 added `crates/term-attach` as a second,
  separate crate (never a feature — Cargo features unify additively, and `bastion`'s blocking and
  `engine-core`'s tokio dependency edges on the same target would otherwise link one rlib with both
  present in the shipped binary) holding `attach_session`/`suspend_and_attach`, fixing a bug where
  attach failures fabricated their stderr text instead of surfacing tmux's real error. Task 5
  reconciled a spec-authoring discrepancy (114 ported tests vs. 116 actual — 2 net-new tests for
  the option builders — recorded in the spec's Amendment Log) and confirmed both crates are absent
  from `engine-core`/`engine-serve`/`engine-store`'s dependency graphs. Task 7 documented the split
  (`docs/terminal-crates.md`, `architecture.md`, `docs/index.md`) and mechanically re-confirmed the
  isolation. Task 8 validated the full gate green.
- **Why:** Closes Phase 9's first block — the engine can now build on terminal-control primitives
  without ever linking the interactive-attach path into its own binary. Unblocks `EN.9.B` (the
  async driver seam and session lease).
- **Refs:** `planning/EN.9.A/tasks.md`; roadmap Phase 9 "Terminal nodes"; HQ lane
  `planning/operator-surface/lane-terminal.txt`.

```
29b1282 docs: document term-core/term-attach split and prove isolation
1a89c23 feat: implement EN.9.A-task6
721299f feat: implement EN.9.A-task4
8d88dba feat: implement EN.9.A-task3
e439281 feat: implement EN.9.A-task2
8d942eb feat: implement EN.9.A-task1
```

Next: `EN.9.B` — async driver seam and the session lease (Phase 1), pending the tmux user-option
spike on the Mini.

---

## [run: 2026-08-13]

### `EN.ticket.run-failure-notification` closed — terminal run failures reach the operator channel
- **What:** Drove `ticket-run-failure-notification` via `/sdlc-task`, in place on `main`, all 6 tasks
  passed. Task 1 added `operator::failure` with a pure notify-or-not decision over
  `derive_terminal_status`'s four outcomes and two policy knobs (`notify_on_statuses`,
  `failure_item_priority`) resolving through the standard four layers across
  `baseline`/`cheap-fast`/`thorough`, documented in `planning/harness.json`. Task 2 added the pure
  renderer — workflow type, run id, first failing node and its error, `budget_halted` distinguished
  from `failed` in the text, deterministic marked truncation rather than failing to notify. Task 3
  hooked `live_state::mark_terminal`, the single point every run exits through, so a run that
  failed three nodes emits one notification, not three; `cancelled` deliberately emits none. Task 4
  added the integration suite (`tests/it/run_failure_notification.rs`, 6 tests) covering the burst
  case, the single-failure path and both negative cases. Task 5 carried the documentation as an
  explicit task, because `/sdlc-task` ships no docs stage. Task 6 validated the full gate.
- **Why:** Closes the last block of engine-rs's `operator-surface` section. Without it the first
  thing the operator learns about a failed run is a log line nobody reads.
- **Refs:** `planning/ticket-run-failure-notification/tasks.md`; roadmap `operator-surface`, lane
  `surface`.

### Lane close — `operator-surface` section complete, one correction worth keeping
- **What:** Closed `EN.8.D` (PR #44) and the ticket above, ran the downstream consumer check against
  `core/bastion` (clean, `--locked`, 1m01s), cleared two carryovers on that single piece of
  evidence, promoted `approve-and-run-seams-unwired-in-bastion` (P1), and wrote
  `BA.ticket.approve-and-run-seams` into bastion at the operator's request. `/close-out` found
  `docs/architecture.md` stale on the policy-resolving registration list and fixed it (`d3c7252`).
  Repaired one partial state write: `/sdlc-task` set `state.json` to `closed` but left the spec's
  own Status line reading "Not started".
- **Why:** The Telegram operator gate cleared between runs, so the two blocks left HELD on
  2026-08-12 became runnable. **The correction matters more than the closes:** this lane initially
  recorded bastion's operator seams as stubbed `|_| None`, which is wrong — `notify-send-trigger`
  already made them real, and the actual gap is narrower (the lookup resolves against a registry fed
  only by `POST /api/notify/test`, so engine-queued items are invisible to it). The error came from
  reading bastion's doc comments instead of its wiring; the comments describe authoring-time state
  and a later ticket moved the code without moving the prose.
- **Refs:** `planning/orchestration-run/operator-surface/notes.md` + `review.md`;
  `planning/handoff.md`.

---

`EN.8.D` — APPROVE_AND_RUN — shipped across 8 tasks, PASS review. Built the operator-approval POC engine-rs's half of `business/docs/profile-and-pitch.md:128`'s pitch composes: `render`/`render_and_validate` turn a pending-harvest record into a `ValidatedOperatorPayload` with a deterministic gate_id and a fixed approve/skip/open_session option set (task 1); `ApproveAndRunPolicy` (drain_batch_max/harvest_item_priority/session_fallback_slug) resolves through the standard four-layer precedence with baseline/cheap-fast/thorough profiles (task 2); the drain renders and validates pending-harvest records into `EN.8.B`'s `OperatorQueue`, routing non-conforming records to `session-<slug>` and bounded by `drain_batch_max` (task 3); the verdict path maps a tapped option key to a `LedgerDecision`, records it via `EN.8.C`'s `operator::ledger::record_decision`, re-queues on digest mismatch, and authorizes execution only on a matched Approved verdict (task 4); the declared `APPROVE_AND_RUN` graph composes `HarvestApproveNode` behind `ApproveAndRunExecuteNode` over the injectable `HttpPost` seam (task 5); `ApproveAndRunSeams` (lookup_pending/resolve_verdict, Send+Sync, no DB/network) exposes the two bastion-shaped seams and `engine-serve` registers the workflow with per-event policy resolution (task 6); a hermetic end-to-end suite covers the POC scenario, a 60-item storm, digest-mismatch requeue, and non-conforming session-routing (task 7); the full gate (fmt, clippy `-D warnings`, nextest --workspace, release build) validated green — 1995/1995 tests, no code changes needed (task 8). Notable decisions: `ApproveAndRunExecuteNode` composes rather than reimplements `HarvestApproveNode`'s POST; an engine-side `ApproveAndRunVerdict` struct stands in for bastion's `telegram::ResponseVerdict` since this crate can't name bastion's type; the integration suite uses `FileApprovalLedger` over a tempdir since `InMemoryApprovalLedger` is test-only and invisible from the `tests/it` binary. The block is complete and testable in engine-rs alone; live-on-a-phone still depends on bastion wiring the two open seams (`PendingLookup`/`VerdictSink`) into `run_server`, a separate bastion block. Docs: `docs/operator-payload-contract.md` updated, `docs/approve-and-run-workflow.md` added. Next: `EN.ticket.run-failure-notification`, `EN.9.C`, or `EN.5.B2`.

```
9042bcc docs: update docs for EN.8.D
a29f99f feat: implement EN.8.D-task7
df30fe8 feat: expose APPROVE_AND_RUN's bastion seams and register the workflow
8f8bb61 feat: implement EN.8.D-task5
84709b8 feat: implement EN.8.D-task4
9126202 feat: implement EN.8.D-task3
468fc2d feat: implement EN.8.D-task2
0c5d3fa feat: render pending-harvest records into validated operator payloads (EN.8.D task 1)
```

## [run: 2026-08-12]

### EN.8.B-operator-queue closed — operator queue depth limit, priority ordering, digest tail, storm suppression
- **What:** Drove `8.B-operator-queue` via `/sdlc-flow` on branch `8.B-operator-queue-flow`, all 6
  tasks passed, PASS review. Task 1 added `OperatorQueueItem` and a deterministic `compare_items`
  comparator (priority desc, enqueued_at asc, item_id asc), wired as `operator::queue` with no I/O
  or clock reads. Task 2 added a `QueueSource` trait plus a file-backed `BlockedEdgeSource` reading
  bastion's blocked-edge sink JSONL (missing file → empty, malformed lines skipped, no held handle
  or writer-process dependency). Task 3 made `OperatorQueue` enforce a policy-resolved depth limit
  (default 1 open item, §7.5 Invariant 3), release the open item on answer or on an unanswered
  timeout (re-queuing, never dropping it), and drop items whose level predicate no longer holds at
  selection time — `operator_queue_depth`/`answer_timeout_secs` resolve through the standard four
  policy layers across `baseline`/`cheap-fast`/`thorough` profiles, stamped via `policy_state()`.
  Task 4 added storm suppression and a digest tail (`build_digest`/`storm_digest`), with
  `suppression_window_secs`/`digest_schedule_secs` policy knobs documented in `planning/harness.json`.
  Task 5 added the 60-item, restart-storm, wedge, deterministic-ordering, and stale-item integration
  tests in `crates/engine-core/tests/it/operator_queue.rs`. Task 6 ran the full authoritative
  validation suite (fmt, clippy `-D warnings`, nextest --workspace, build --release) — all green, no
  code changes needed.
- **Why:** Closes `EN.8.B`, giving the operator surface its Invariant-3 delivery discipline (at most
  one open item at a time, priority- not arrival-ordered, with a digest tail so a restart burst
  produces one message, not N) — unblocking `EN.8.D` (APPROVE_AND_RUN) alongside `EN.8.C`.
- Docs: `docs/operator-payload-contract.md`, `docs/index.md` updated.

Next: `EN.8.D` — APPROVE_AND_RUN — the POC that makes the pitch true, now unblocked alongside `EN.5.B2`.

```
d24354d docs: update docs for 8.B-operator-queue
d99bf6e feat: implement 8.B-operator-queue-task5
ee31038 feat: implement 8.B-operator-queue-task4
ceaffdc feat: implement 8.B-operator-queue-task3
fbe833b feat: implement 8.B-operator-queue-task2
8fec05d feat: implement 8.B-operator-queue-task1
16c8db93 chore: wrap up 8.B-operator-queue (vault)
```

### EN.8.A-operator-payload-contract closed — operator payload contract + two-channel router
- **What:** Drove `8.A-operator-payload-contract` via `/sdlc-flow` on branch
  `8.A-operator-payload-contract-flow`, all 6 tasks passed, PASS review. Confirmed WhatsApp Cloud
  API limits against Meta's developer docs (3 reply buttons, 20-char labels, 1024-char body) and
  landed the configurable `OperatorPayloadLimits` in `engine-core::operator::limits`, plus a
  non-platform floor `OPERATOR_MIN_RESPONSE_OPTIONS = 2` (task 1). Added `OperatorPayload`/
  `OperatorResponseOption` with a sha256 digest computed over the rendered summary+options only
  (deliberately excluding `gate_id`), giving later re-queue logic a way to detect a payload
  mutated after rendering (task 2). Added `validate.rs` returning a `ValidatedOperatorPayload`
  only on success, four distinct typed rejection errors, enforcing "a failing gate cannot reach
  the notification channel" at the type level via a private-field newtype with no public
  constructor other than `validate()` (task 3). `HarvestGate` now carries a declared
  `OperatorChannel` (notification default, or session-<slug> via `with_channel`), readable off the
  gate definition without executing the workflow (task 4). Added a cross-module hermetic test
  suite (`operator::tests`) proving all rejection paths force session routing, the gate-channel
  readability property, and both digest-change re-queue scenarios (task 5). Full validation gate
  green — fmt, clippy `-D warnings`, `nextest --workspace`, `build --release`, no code changes
  needed (task 6). Docs: `docs/operator-payload-contract.md` created; `docs/harvest-gate.md`,
  `docs/index.md`, `docs/architecture.md` updated.
- **Why:** Closes `EN.8.A`, unblocking `EN.ticket.run-failure-notification` (same payload schema)
  and clearing the way for `EN.8.B`/`EN.8.C` (operator queue, approval ledger).
- **Next:** `EN.5.B2` — regression history + blind judge + change gate, per the `next` frontmatter
  list.

```
a1a7f27 docs: update docs for 8.A-operator-payload-contract
c519f06 feat: implement 8.A-operator-payload-contract-task5
9b84377 feat: implement 8.A-operator-payload-contract-task4
f4a4eed feat: implement 8.A-operator-payload-contract-task3
7d205e0 feat: implement 8.A-operator-payload-contract-task2
0b94c03 feat: implement 8.A-operator-payload-contract-task1
```

---

## [run: 2026-08-09]

### C5 substrate lane closed (stale-generate-timeout-caveat, implement-node-transport-retry) + close-out
- **What:** Drove `planning/close-the-loop/roadmap.md`'s substrate lane, engine-rs section (C5), via
  `/begin-orchestration` + two `/sdlc-task` runs, then `/close-out`.
  `EN.ticket.stale-generate-timeout-caveat` closed (4/4 tasks): fixed a stale doc comment on
  `CallTimeouts` claiming `generate` wasn't consumed by `GenerateTasksNode` when it had been for
  five days, corrected `docs/sdlc-flow-workflow.md`'s policy-reader rows, and filled two real gaps
  in `docs/sdlc-flow-policy.md` (missing `docs` knob, undocumented `timeouts.*` family, wrong
  `harness.json` example default). Zero `.rs` behavior change.
  `EN.ticket.implement-node-transport-retry` closed (5/5 tasks): decided and recorded shape (A) —
  bounded in-node retry with backoff — in the ticket's own Amendment Log before writing code, per
  its own AC1. Landed `TransportRetry`/`PartialTransportRetry` on `SdlcPolicy` (full four-layer
  resolution) and wrapped all four transport call sites in `ClaudeCodeStep::process` with
  retry-with-backoff, cancellation re-checked between attempts. Confirmed the blast radius: all
  five `ClaudeCodeStep` consumers (implement/triage/review/generate/docs) share one retry budget
  today. This clears the `sdlc-flow-implement-transport-failures-not-retried` carryover (deleted).
  `/close-out` then ran the full gate clean (fmt/clippy/nextest 1799/1799/build --release), found
  and filled one real coverage gap — `merge_transport_retry`'s four-layer resolution had zero
  direct tests, unlike every sibling policy knob — and added 5 tests mirroring the existing
  pattern.
- **Why:** The C5 lane was queued substrate work: one doc/code reconciliation ticket, and one
  real reliability defect (a transient transport blip — timeout, spawn failure — was killing an
  entire `SDLC_FLOW` run outright, losing all completed task progress, root-caused to a real 2026
  -08-01 production failure at exactly 300.181s wall clock). Landing both keeps the pipeline's own
  docs honest and stops future automated runs from dying on network hiccups they should just
  retry past.
- **Refs:** `planning/close-the-loop/lane-substrate.txt` (C5 section), `planning/state.json`
  (carryover: added `transport-retry-policy-not-wired-to-call-sites`, deleted
  `sdlc-flow-implement-transport-failures-not-retried`), `planning/handoff.md`. New idea captured
  in the handoff (not yet researched): whether `claude-code-rs` could expose the 5-hour/weekly
  rate-limit window (distinct from the existing `headless-cli-no-usage-cost-data` dollar-cost
  carryover) so `TransportRetry` could back off correctly on a rate-limit hit instead of guessing.

---

## [run: 2026-08-07]

### Lane C substrate (C5) — both engine-rs blocks closed via `/begin-orchestration` + `/sdlc-task`
- **What:** Drove the demand-ready roadmap's substrate lane, engine-rs section, in place on `main`.
  `EN.ticket.call-timeout-policy-knob` closed as a **no-op re-validation** — its 5 tasks were already
  implemented and committed in prior sessions (`26b1d02`, `b653e52`) while `state.json` still read
  `open`, so the run changed zero files. `EN.ticket.cron-schedule-startup-wiring` closed with real
  work across 4 commits: `ScheduleEntry.next_fire_at` (the loader had been discarding
  `normalize_schedule`'s first-fire anchor, exactly what seeding needs), `build_seeded_registry()`
  (the seeding caller the module doc claimed existed but did not), `spawn_schedule_loop()` (a
  `tokio` interval driver that spawns nothing when no entries are configured), restart-safe
  re-seeding, and two `harness.json` knobs (`schedule.poll_interval_ms` = 15000,
  `schedule.store_path`). Then a close-out pass: filled the `read_schedule_loop_config` coverage gap
  and corrected `cron-primitive.md`/`architecture.md`, which described the scheduler as already
  polling in production when nothing has ever called it.
- **Why:** The engine had a complete, tested cron primitive that was reachable only from test code —
  a configured schedule entry would have sat silently and never fired. This block built the missing
  plumbing. **It still does not run:** the call site belongs in `bastion`'s `serve/mod.rs` beside
  `spawn_durable_writer`, a different repo, so it is a separate block. The roadmap's claim that this
  block alone unblocks the newsletter digest is therefore wrong.
- **Refs:** `planning/orchestration-run/notes.md` (attention items + mid-run decisions),
  `planning/orchestration-run/review.md` (plain-English overview + manual verification),
  `planning/handoff.md`, lane log `planning/demand-ready/lane-log.jsonl`.

---

## [run: 2026-08-04]

### `en-5b1-eval-slice-runner` — Eval slice runner (first half of the `OR.U` port) — done via `/sdlc-flow`
Implemented across 4 tasks, PASS review. Task 1 ported Synapse's deterministic/structural/
reference-based scorer library into `engine-core` as pure functions over generic
`serde_json::Value`/`&str` inputs (`evals::scorers::{score_deterministic, score_structural,
score_reference_based}` returning `ScoreResult`), with no embedding or corpus access — the
reference-based scorer uses a containment short-circuit then a token-overlap ratio against a fixed
0.5 pass threshold, and the structural scorer checks key presence plus JSON type match with partial
credit, deliberately leaving out Synapse's retrieval-specific stopword/sentence-splitting machinery
as out of scope. Task 2 added `EvalCase` (scorer kind + dot-path selector + expected value) and
`EvalSlice` (named case collection grouped by domain/model/profile, mirroring `PolicyAggregate`'s
own grouping shape), producing per-case results and an overall pass-rate; `ScoreResult` gained
`Serialize` as a minimal, behavior-preserving addition so slice/case reports could serialize it.
Task 3 landed the eval slice runner itself — `run_slice` imports `EN.4.0`'s
`aggregate_state_files`/`extract_policy_telemetry` directly (no second aggregation path, true by
construction) via a field-less `UnitPolicy` that collapses every state file into one
`PolicyAggregate` row, reduces that to a single JSON record, and scores it against an `EvalSlice`;
shipped alongside a concrete `coding_slice()` and an integration test proving the runner against a
real fixture `*-state.json`. Task 4 ran the full validation gate (fmt --check, clippy -D warnings,
`nextest --workspace`, `build --release`) — 1781/1781 tests green, no warnings, no code changes
needed. This closes the first half of the `OR.U` port and unblocks `EN.5.B2` (regression history +
blind judge + keep-if-better/revert-if-worse change gate), which depends on this block's scorer
library and slice runner. `docs/architecture.md` updated.

```
440ac5b docs: update docs for en-5b1-eval-slice-runner
502cd70 feat: implement en-5b1-eval-slice-runner-task3
d9e792b feat: implement en-5b1-eval-slice-runner-task2
02fd3e2 feat: implement en-5b1-eval-slice-runner-task1
```

Next: `EN.5.B2` — regression history + blind judge + keep-if-better/revert-if-worse change gate,
now unblocked, per the `next` frontmatter list.

### `EN.ticket.wire-meta-transport-telemetry` — Wire local-eligible stages through `with_meta_transport` — done via `/sdlc-flow`
- **What:** Closed the D13 follow-up known issue where `model_tier_used` telemetry could never show
  `"local"` for any of the 4 workflows, because `registry_for_policy` wired every local-eligible
  node via `ClaudeCodeStep::with_transport` (plain), which always stamps a generic `"cloud"`-tier
  `TransportInfo` regardless of what actually ran. Task 1 landed the shared `TransportSlot` helper
  (`workflows/transport_slot.rs`) — a plain-or-meta transport override with meta-wins precedence,
  matching `ClaudeCodeStep`'s own rule — unit-tested for meta-wins, plain-only, and neither-set.
  Tasks 2-5 gave all 10 local-eligible nodes across `sdlc_flow` (`TriageTaskNode`,
  `ConsolidatedReviewNode`), `content_pipeline` (`SummarizeNode`, `SelfCriticNode`, `ReviseNode`,
  `TranslateNode`), `proposal_generator` (`OpportunityIdentifierNode`, `ProposalReviewNode`,
  `ProposalReviseNode`), and `diagnostic_intake` (`IntakeExtractNode`) a `with_meta_transport`
  passthrough onto `TransportSlot`, and rewired every corresponding `registry_for_policy` call site
  from `openai_compat_transport_live` to `openai_compat_meta_transport_live`, one workflow per task
  with tests proving `"local"` on a successful local call and `"cloud"` on a simulated local-call
  failure. Along the way, each task discovered and fixed the same adjacent defect: every node's
  final `put_result` call was silently overwriting the whole `ctx.nodes[identity]` entry and
  dropping the `"transport"` key `ClaudeCodeStep::process` had just stamped — exactly what
  `RunTelemetry`'s `observed_model_tiers` reads back — so the `TransportSlot` wiring alone would not
  have been sufficient without also preserving that stamp through each node's own result-building
  step. Task 6 ran the full validation gate (fmt, clippy `-D warnings`, `nextest run --workspace`,
  `build --release`) — all green — fixed 2 pre-existing clippy lints uncovered only under
  `-D warnings`, fixed a real e2e regression in `proposal_generator_e2e.rs` caused by the transport
  stamp now appearing in `ReviseNode`'s output, and grep-confirmed exactly 10 production
  `with_meta_transport(openai_compat_meta_transport_live(...))` call sites with zero remaining
  plain rewires. PASS review. Docs updated: `docs/sdlc-flow-policy.md`,
  `docs/diagnostic-intake-workflow.md`, `docs/proposal-generator-workflow.md`,
  `docs/content-pipeline-workflow.md`.
- **Why:** `model_tier_used` telemetry drives cost-tracking and the planned cloud-vs-local
  comparison (`EN.5.B1`/`EN.5.B2`); until this landed it silently reported a generic cloud stamp for
  every run regardless of whether a stage actually ran locally, fell back to cloud, or was never
  local-eligible in the first place — corrupting that data source unnoticed since each workflow's
  local-rewire first shipped.
- Next: `EN.6.I` — LEAD_INGEST (write an inbound form lead to the brain as an opportunity),
  `EN.5.B1` — Eval slice runner (scorers + EvalCase/EvalSlice on EN.4.0 telemetry, first half of the
  OR.U port).

```
2c98017 docs: update docs for ticket-wire-meta-transport-telemetry
c7d1ef6 feat: implement ticket-wire-meta-transport-telemetry-task6
dbb2af5 feat: implement ticket-wire-meta-transport-telemetry-task5
5d33093 feat: implement ticket-wire-meta-transport-telemetry-task4
c886a91 feat: implement ticket-wire-meta-transport-telemetry-task3
5b76898 feat: implement ticket-wire-meta-transport-telemetry-task2
e8b806f feat: implement ticket-wire-meta-transport-telemetry-task1
6d2cff0 feat: implement ticket-local-schema-constrained-json-task3
88d0fc8 feat: implement ticket-local-schema-constrained-json-task2
```

---

## [run: 2026-08-03]

### `EN.6.G` schedule source + fan-out/aggregate — done via `/sdlc-flow`
- **What:** Shipped the two primitives a scheduled multi-source digest needs. Task 1 landed
  `FanOutNode` (`crates/engine-core/src/nodes/fan_out.rs`) — builds N `with_identity`-wrapped
  instances of one node type via a builder closure and runs them through `ParallelNode` — and
  `AggregateNode` (`aggregate.rs`) — joins N `ctx.nodes` entries into one deterministically-ordered
  array by declared identity order, not `HashMap` iteration order; a reproducing test confirmed
  `EN.5.E`'s `with_identity`/`Identified` wrapper already prevents `ParallelNode`'s last-write-wins
  collision for same-type branches, so `parallel.rs` needed no change, per the ticket's
  investigate-first guidance. Task 2 landed `crates/engine-serve/src/schedule.rs` —
  `ScheduleEntry`/`ScheduleRegistry` as a thin adapter over `engine_core::cron`'s `tick()`, a
  `harness.json` `schedule.entries` loader/normalizer, and `dispatch_scheduled_entry` building a
  `Schedule`-typed `IngressEnvelope` dispatched in-process via `dispatch_with_event` + `spawn_run`
  (no self-directed HTTP call), covered by 10 unit tests. Task 3 added end-to-end integration tests
  proving `FanOutNode`/`AggregateNode` survive a real `Workflow::run` with no last-write-wins
  collision, and a single `ScheduleRegistry.tick()` fire dispatching one `PersistToBrainNode`-shaped
  digest payload plus one `OutboundAction`-shaped record through the non-blocking `spawn_run` path.
  Task 4 ran the full validation gate — fmt, clippy `-D warnings`, `nextest run --workspace` (1726
  passed, 16 skipped), `build --release` — all green, no code changes needed.
- **Why:** Completes the omni-channel ingress story with a cron-fired `Schedule` source and the
  generic fan-out/aggregate pair a multi-source digest run needs, built on `EN.6.M`'s durable cron
  substrate per the `D2` stacking decision.
- **Decisions:** `ScheduleRegistry` is deliberately not an `AppState` field — follows the existing
  `default_budget_from_env`/`live_run_metadata` process-global precedent in `http.rs` to avoid an
  immediate cross-repo compile break for bastion, which constructs `AppState` with a literal over an
  unpinned path dependency. `planning/harness.json`'s new knob is `schedule.entries` (an array with
  a sibling `_comment`), matching the file's existing `<workflow_key>.policy`/`.profiles`
  sibling-`_comment` convention. `dispatch_scheduled_entry` always returns `FireOutcome::Reported`
  (never `Silent`) since a dispatch attempt always has something to report.
- **Verdict:** PASS. This closes `EN.6.G`.
- Next: `EN.5.B1` (eval slice runner), `EN.5.C` (EXTERNAL_INTEL), `EN.6.C/D` (Slack,
  Telegram/WhatsApp adapters), `EN.6.I` (LEAD_INGEST), `EN.4.D` (DELIVERABLE_RENDER).

```
c7eff3c docs: update docs for en-6g-schedule-source-fan-out-aggregate
5a82e44 feat: implement en-6g-schedule-source-fan-out-aggregate-task3
ff8b6f2 feat: implement en-6g-schedule-source-fan-out-aggregate-task2
8d57267 feat: implement en-6g-schedule-source-fan-out-aggregate-task1
```

### `EN.6.M` durable background/cron primitive — done via `/sdlc-flow`
- **What:** Ported qm §5's durable Cron primitive as a standalone module with no dependency on any
  specific workflow or envelope type. Task 1 (`crates/engine-core/src/cron/mod.rs`): `CronSchedule`
  (Calendar `{cron_expr, timezone}` / Interval `{every_ms, first_fire_at}`, mutually exclusive and
  validated as such), `RawSchedule`, `CronScheduleError`, and `normalize_schedule`/
  `validate_schedule`/`recover_next_fire_at`/`advance_next_fire_at` — calendar schedules advance
  drift-free from the last *scheduled* time via the `cron` crate's `Schedule::after`, interval
  schedules advance catch-up-safe from the actual *fired* time (exactly one catch-up after
  downtime, never a thundering herd), `every_ms` bounded to `[60_000, 24h)`. Task 2
  (`cron/record.rs`): `CronRecord`, `FireOutcome` (`Reported`/`Silent`), and `CronFireLogEntry` with
  a `From<&FireOutcome>` conversion that structurally prevents a `Silent` outcome from ever
  carrying a note — the silence protocol is enforced by the type, not just documented. Task 3
  (`cron/store.rs`): the injectable `CronStore` trait, a restart-durable `FileCronStore`
  (whole-object JSON persistence, `chrono-tz`'s `serde` feature enabled workspace-wide for the
  `Tz` field), and a `tick()` driver that fires due/enabled records with the correct per-variant
  advance anchor (record's pre-fire `next_fire_at` for Calendar, `now` for Interval) and durable
  fire-log recording. Task 4: full validation gate — fmt, clippy `-D warnings`, `nextest run
  --workspace` (1706/1706), `build --release` — all green with no further code changes needed.
  PASS review (no findings). Docs: `docs/cron-primitive.md` added, `docs/index.md` updated.
- **Why:** `EN.6.G` (Schedule source + fan-out/aggregate) needs a real cron substrate to fire
  through instead of hand-rolling cron mechanics, per the `D2` stacking decision in
  `planning/orchestrate-2026-08-03-log.md` — this block ships that primitive standalone first.
- **Decisions:** Added the `cron` crate (chrono-compatible, no async runtime coupling) rather than
  hand-rolling a cron-expression parser, as pre-specified. Kept the fire-log store JSON-file-backed
  via the repo's `CommandRunner`-style injectable-seam convention, not `engine-serve/durable.rs`'s
  Postgres pattern — no new externally-provisioned schema for a single-Mac-Mini scheduler. Scope
  boundary held: no HTTP endpoint, no dynamic create/list/patch API — entries are constructed
  programmatically by whatever calls the primitive next (`EN.6.G`).
- **Verdict:** PASS. This closes `EN.6.M`.
- Next: `EN.6.G` — wire `schedule.rs` to fire through this primitive.

```
d2b368b docs: update docs for en-6m-durable-cron-primitive
9bf20f1 feat: implement en-6m-durable-cron-primitive-task3
48e89bc feat: implement en-6m-durable-cron-primitive-task2
4442c7f feat: implement en-6m-durable-cron-primitive-task1
9b17ea7 feat: implement ticket-sdlc-command-policy-floor-task2
5c3c618 fix: adapt opportunity stage-vocab tests to mev D58 (parse_stages, brain.toml+pipeline.md fixture)
088d061 feat: implement ticket-sdlc-command-policy-floor-task1
3eead6a chore(harness): pull base-template b410add — gate-skip-count-regression + triage-verify-pre-existing-claims
```

---

## [run: 2026-08-02]

### Live review path verified — carryover cleared, hardening train archived
- **What:** Consumed the handoff and closed its items. (1) **Verified the live review path**:
  rebuilt the stale `bastion` release binary (it predated `7247f40`/`3b5d33c`), started `bastion
  serve` with the engine mount (`DATABASE_URL=…/orchestration_dev`, `ENGINE_BRAIN_ROOT` set), and
  triggered `SDLC_FLOW` run `049b5fc0` (`smoke-sdlc-flow`, `repo: engine-rs`, `use_worktree`,
  `profile: cheap-fast`, per-run override `policy.review_mode: per_task`). Run reached `status:
  done`; `ConsolidatedReviewNode` returned a **live haiku PASS verdict over the real committed
  diff** — `{"verdict": "PASS", "summary": "SMOKE.md created at root with ENGINE-SMOKE content.
  Both acceptance criteria met.", "review_diff_truncated": false}` with `ResolvedPolicy` confirming
  `review_mode: per_task` + `review: haiku`. Evidence pulled from the durable Postgres `events`
  row. Deleted carryover `sdlc-live-review-never-observed` (its `clears_when` is met); smoke
  worktree/branch/state cleaned. (2) Committed C1's launchd section in the HQ repo
  (`docs/infrastructure.md`, `133bb563`). (3) **Archived `planning/sdlc-flow-hardening/`** via
  /archive: residue distilled to `knowledge.md` (3 entries) + `memory.md` (5 entries) + new
  decision `D14-dirty-tree-abort-safety-invariant`; graph check net-clean (also fixed 3
  pre-existing dangling `related:` edges). (4) Deleted the consumed `planning/handoff.md`.
  **`sdlc-flow.js` retirement is now unblocked but deliberately NOT decided/executed** — the JS
  harness stays the production driver until an explicit decision is logged.
- **Why:** The handoff's first item — the last unverified property of the hardening train. Until a
  live model was observed reviewing a real non-empty diff, "the Rust SDLC_FLOW is proven end to
  end" could not be claimed for the review path, and the JS engine could not be considered
  retirable.
- **Caveat:** the serve process used for the verification was reaped by the session harness
  (~SIGTERM at turn end) *after* the run completed — the run itself finished cleanly; for
  longer-lived serves, launch fully detached (`setsid`) rather than as a session background task.
- **Refs:** `planning/archive/sdlc-flow-hardening/`, `planning/decisions/D14-dirty-tree-abort-safety-invariant.md`,
  HQ `133bb563`; carryovers remaining: `sdlc-review-routed-to-cloud-pending-local-model`,
  `eval-cloud-vs-local-comparison`, `en7d-brain-root-not-set-in-deployment` (Mac Mini human step).

### Review routed to cloud, local->cloud fallback bug fixed, handoff written
- **What:** Following the hardening train, routed `SDLC_FLOW`'s review stage off the unprovisioned
  `local` tier onto cloud models (`7247f40`) — `cheap-fast*` review+triage -> haiku, `pragmatist*`
  review -> sonnet — after a live `review_mode: per_task` run died on `qwen2.5:3b` not being pulled.
  The triage flip is behavior-stable (`TriageTaskNode` returns before any model call when
  `llm_triage: false`, which both cheap-fast profiles set). Then root-caused and fixed a real bug
  the tier move exposed (`3b5d33c`): both `openai_compat_transport` and
  `openai_compat_meta_transport` handed the incoming `Config` to `cloud_fallback` unchanged, but
  `apply_model_tier` had already written the LOCAL model name into `config.model` — so the cloud
  `claude` CLI was invoked with `qwen2.5:3b` and 404'd, making the fallback useless for the single
  most likely local-side failure. Both sites now route through `clear_local_model()`. Verified by
  mutation: reverting the fix fails exactly the 4 new tests while all 8 pre-existing tests still
  pass, proving the old suite was blind to it. Deliberately did NOT add a `LocalConfig.fallback_model`
  knob (disproportionate for an error path) and left the fallback quiet-but-attributable via its
  existing `tier: "cloud"` stamp. Declined to switch the six opt-in `local-*` profiles to cloud —
  they are the control group for the planned comparison. Captured
  `planning/eval-cloud-vs-local/notes.md` recording that the cloud-vs-local comparison maps onto the
  existing `EN.5.B1`/`EN.5.B2` eval blocks rather than a one-off script.
- **Why:** The owner is about to run `SDLC_FLOW` regularly on this MacBook Pro, and every
  `review_mode: per_task` run failed on a model that isn't installed. Fixing the tier exposed that
  the documented "automatic cloud fallback" had never actually worked — `docs/sdlc-flow-policy.md`
  was promising resilience the code could not deliver, and a silent-but-broken fallback would also
  have corrupted the future cloud-vs-local eval by hard-failing runs instead of degrading them.
- **Refs:** `planning/handoff.md`, `planning/eval-cloud-vs-local/notes.md`,
  `planning/sdlc-flow-hardening/plan.md`; carryovers `sdlc-live-review-never-observed`,
  `sdlc-review-routed-to-cloud-pending-local-model`, `eval-cloud-vs-local-comparison`.

### SDLC_FLOW hardening train executed — 9 tickets + 3 follow-ups merged, smoke re-run
- **What:** Drove the entire `sdlc-flow-hardening` ticket train to completion as orchestrator,
  each ticket run by a subagent through the JS harness in its own worktree, with every merge to
  `main` serialized and re-validated by me. Landed T0 `additive-tolerant-goldens` (`c2125c8`),
  T1 `policy-path-generate-docs-nodes` (`6fbf988`, the P0), T1b (T1's deferred task 3, `e2fb496`),
  T2 `commit-task-work-real-diffs` (`6b5fe23`, the flagship), T2b (T2's deferred task 5 + a
  safety fix, `ee699d5`), T2c (new, `f9a685c`), T3 `wrapup-outcome-truth` (`52a4711`),
  T4 `resume-reset-semantics` (`1e3870a`), T5 `triage-failure-output` (`bf6fc5b`),
  T6 `restamp-attempt-count` (`8caabbf`), T7 `watch-script-run-id-guard` (`cba644d`), and
  C1 `chore-engine-brain-root-deployment` (`dd52784`, repo half only). Final suite: 1671 passed,
  0 failed. **Two follow-ups the train itself surfaced, both owner-approved:** (a) T2's tree-wide
  `git add -A` was a live-repo hazard because `use_worktree` defaults to `false` and that path
  resolves `worktree_path` to `"."` — a run there would sweep unrelated dirty files into a
  `feat(sdlc):` commit; T2b added a dirty-tree abort mirroring `sdlc-flow.js` STEP 3a, scoped to
  the whole `!use_worktree` branch so a `repo`-slug run against a registry-resolved root is
  covered too, and deliberately NOT made a policy knob (safety invariant, not a cost trade);
  (b) T2 made the reviewer prompt unbounded, so T2c added `review_diff_max_chars` (default
  120_000) across all six profiles with a VISIBLE truncation banner telling the reviewer to
  return `PARTIAL` rather than `PASS` on a partial diff. Cleared the inherited carryover
  `worktree-uncommitted-run-2d46b140` by committing its 75 lines to
  `sdlc/ticket-expose-live-run-workflow-type` (`7a82673`) and stopping the stray serve.
  Ran the post-T2 smoke twice through `bastion serve`, both succeeded.
- **Why:** The 2026-08-01 live runs proved the Rust `SDLC_FLOW` walked end to end but verified
  nothing — reviews ran against an empty diff (making every past PASS a rubber stamp), bails
  recorded as passes, `auto_pr` PRs shipped without code, doc patches never reached the branch,
  and a served run could aim a skip-permissions writer at the primary checkout. All five are now
  closed, and the pipeline can be trusted to carry feature work again.
- **Caveat — one acceptance criterion is NOT met by the smoke.** The smoke spec is a one-line
  marker write, so `TriageTaskNode` correctly returned `trivial: true` and routed past
  `ConsolidatedReviewNode` on both runs (a legitimate skip, not defect 0d). A third run forcing
  `review_mode: per_task` DID reach the review node — proving it is no longer unconditionally
  skipped — but failed inside it because the `cheap-fast` local review tier (`qwen2.5:3b`) is not
  pulled in Ollama. "Reviewer sees a non-empty diff" is instead proven deterministically by the
  hermetic suite, including `real_git_intent_add_surfaces_untracked_content_in_the_review_prompt`
  which drives real git. **A live model reviewing a real diff has still not been observed**, so
  retiring `.claude/workflows/sdlc-flow.js` should wait for that.
- **Refs:** `planning/sdlc-flow-hardening/{plan,notes}.md` (plan carries the full execution record
  + smoke evidence); smoke runs `b395313a` (cwd-target) and `59f1670a` (registry-resolved, serve
  outside the brain tree, absolute `worktree_path`).

### SDLC_FLOW hardening — audit, plan, and 9 ticket specs authored
- **What:** Ran a three-way parallel read-only code audit of the SDLC_FLOW workflow (graph +
  state, policy + seams, trigger boundary) verifying every 2026-08-01 defect anchor at
  `533b28e`; found three NEW defects sharing 0b's root (0d TrivialSkip always skips review,
  0e every auto_pr PR is code-free, 0f doc patches never committed); a dedicated design pass
  settled commit-per-task + working-tree-vs-HEAD diffs as the fix. Authored
  `planning/sdlc-flow-hardening/plan.md` (sequenced orchestration: T0 goldens → T1
  rogue-nodes P0 → T2 commit-the-work → tail) plus nine full spec dirs
  (`ticket-additive-tolerant-goldens`, `ticket-policy-path-generate-docs-nodes`,
  `ticket-commit-task-work-real-diffs`, `ticket-wrapup-outcome-truth`,
  `ticket-resume-reset-semantics`, `ticket-triage-failure-output`,
  `ticket-restamp-attempt-count`, `ticket-watch-script-run-id-guard`,
  `chore-engine-brain-root-deployment`). Flipped all notes.md statuses to
  ticketed/wontfix/done. Coordinated with the brain-quality track (no sequencing gate —
  everything runs via the JS harness; merge serialization only; amendment written into its
  runbook). Authored the engine-rs briefing for the upcoming bastion-web architecture review
  (`core/_planning/bastion-web/architecture-improvement/engine-rs.md`). Deleted both consumed
  handoffs; wrote a fresh handoff for the orchestrating agent.
- **Why:** The 2026-08-01 live runs proved the Rust SDLC_FLOW walks but does not verify —
  review rubber-stamps on an empty diff, and recorded history lies on bails. This session
  converts that defect tab into an executable, small-diff ticket train before any further
  feature work rides the pipeline.
- **Refs:** `planning/sdlc-flow-hardening/{plan,notes}.md`; the nine spec dirs;
  `core/_planning/orchestrator/brain-quality-orchestration.md` (2026-08-02 amendment).

## [run: 2026-08-01]

### First real bastion-web-triggered SDLC_FLOW trial, timeout root-caused, two follow-up tickets authored, handoff written
- **What:** Ran the first real `SDLC_FLOW` trial triggered from `bastion-web`'s QuickLaunch UI
  against a locally-served `bastion serve` pinned to this repo's cwd (targeting
  `ticket-expose-live-run-workflow-type`). Built and hardened `scripts/dev_with_bastion_web.sh`
  (starts both servers with full preflight — repo layout, env files, required vars, Postgres,
  ports — verifies the engine actually mounted via `GET /workflows` not just `/health`,
  process-tree-aware shutdown via `kill_tree`, a `--stop` escape hatch) and
  `scripts/watch_sdlc_flow.sh` (watches an already-triggered run by `run_id` alone, combining
  `core/scripts/run-sdlc-flow.sh`'s per-node polling with `sdlc_smoke.sh`'s exit-code contract,
  auto-surfacing the failing node's error on a terminal failure). The trial run itself failed:
  `ImplementTaskNode` timed out at exactly 300.181s. Root-caused to `claude-code-rs`'s
  `execute.rs:21` hardcoded `DEFAULT_TIMEOUT` constant with zero override path anywhere in
  either repo — confirmed by reading the source, not inferred. Found a second, compounding
  issue while tracing it: a raw `execute()` failure inside `ImplementTaskNode` is not routed
  through the existing `max_attempts`/`IncrementAttemptNode` retry loop at all — it halts the
  whole run unconditionally (`workflow.rs`'s `run_halts_walk_on_failure` confirms). Authored two
  tickets: `CC.ticket.configurable-call-timeout` (claude-code-rs — add `Config.timeout:
  Option<Duration>`) and `EN.ticket.call-timeout-policy-knob` (this repo — thread it through
  `SdlcPolicy` as a per-stage knob mirroring the existing `ModelTiers` pattern; depends on the
  first). Deleted a fully-resolved worktree-sibling-path-deps ticket earlier in the session
  (implemented, tested, merged) and the previous stale `EN.3.J` handoff. Wrote a fresh handoff
  for an Opus review pass on both new tickets before implementation.
- **Why:** Proving the Rust `SDLC_FLOW` works end-to-end from the actual UI a human will use
  (not just `scripts/sdlc_smoke.sh`'s synthetic harness) surfaced a real, previously-invisible
  infra ceiling — every prior smoke/trial run happened to finish inside 300s. The retry-loop gap
  found alongside it is architecturally significant enough (halt semantics, not just a knob) to
  warrant a deliberate design call rather than folding it into the timeout fix.
- **Refs:** `planning/handoff.md`, `planning/ticket-call-timeout-policy-knob/tasks.md`,
  `core/claude-code-rs/planning/ticket-configurable-call-timeout/tasks.md`,
  `planning/state.json` (carryover: `sdlc-flow-implement-transport-failures-not-retried`)

---

## [run: 2026-07-31]

### EN.3.J-sdlc-flow-smoke tasks 6-10 — both live runs executed, a real EN.3.K wiring gap found and fixed
- **What:** Completed the remaining `EN.3.J` tasks: task 6 added the `--repo <slug>` flag to
  `scripts/sdlc_smoke.sh` (byte-identical default body, `docs/sdlc-flow-smoke.md` updated).
  Tasks 7-9, normally strictly human-in-the-loop, were executed directly this session with the
  repo owner's explicit prior agreement (real triggered runs against a live `bastion serve`, real
  agentic writes, real token spend). Run 1 (engine acceptance, no `repo`, served from
  `core/engine-rs`) and Run 2 (deployment acceptance, `--repo engine-rs`, served from `$HOME` with
  `ENGINE_BRAIN_ROOT` exported) both reached `status: "succeeded"` with the marker file written and
  validated; Run 2's `worktree_path` came back absolute and registry-rooted
  (`.../core/engine-rs/trees/sdlc/smoke-sdlc-flow`), proving the target no longer depends on the
  serve process's cwd. Task 8's two 422 pre-flight probes (bogus `repo`, bogus `spec_slug`) both
  rejected cleanly with no `run_id` minted. Task 10 (Validate) confirmed all evidence recorded, the
  cross-repo `health_check.sh` fix already committed, and skipped the Rust gate (no `.rs` files in
  this block's own diff).
  - **A genuine `EN.3.K` production bug was found and fixed along the way, in the sibling
    `core/bastion` repo:** `engine_serve::workflows::init_repo_registry_from_env()` was fully
    implemented but never called anywhere in `bastion serve`'s startup path, so the repo registry
    stayed empty regardless of `ENGINE_BRAIN_ROOT` — every `repo`-bearing event 422'd with "no
    registry available," even for a valid slug. This was a hard blocker for Run 2 specifically;
    without it the deployment-acceptance run could never pass. Fixed with a single call to
    `init_repo_registry_from_env()` before `build_engine_dispatcher()` registers `SDLC_FLOW`,
    committed directly to `core/bastion` `main` (`1a5a455`), full `cargo nextest run --lib` green
    (43/43).
  - **A second, separate defect was found and left open, out of this block's scope:**
    `FinalValidationNode` (`EN.3.E`'s always-on run-level gate) fails all four harness checks
    (`fmt`/`clippy`/`test`/`build`) inside any `--repo`-targeted or `--worktree` `SDLC_FLOW` run —
    `Cargo.toml`'s `claude-code-rs = { path = "../claude-code-rs" }` can't resolve from a nested
    worktree (the real sibling checkout is one level up from the *main* repo, not from
    `trees/sdlc/<branch>`). Doesn't fail the run itself (`WrapUpNode` reports `done` with a "needs
    follow-up" warning), but affects every real worktree-based run in this repo, not just the
    smoke. Recommend a follow-up ticket.
  - **Operational lesson:** the installed `~/.local/bin/bastion` binary doesn't auto-rebuild when
    `engine-rs` (a path dependency) changes, and carries no staleness check. Testing against a
    stale pre-`EN.3.K` binary produced two confusing false findings (a spurious task-loop bail,
    both 422 checks silently no-op'ing) that fully resolved after a fresh `cargo build --release`
    in `core/bastion`. Rebuild `bastion` immediately before any live verification run.
- **Why:** `EN.3.J`'s whole purpose is proving the Rust `SDLC_FLOW` works end to end before
  `.claude/workflows/sdlc-flow.js` is retired — that proof was the only thing left blocking the
  Mac Mini launchd deployment.
- **Refs:** `planning/EN.3.J-sdlc-flow-smoke/tasks.md` (Run Evidence + 2026-08-01 Amendment Log
  entry), `core/bastion` commit `1a5a455`

### EN.3.K landed; EN.3.J amended to a two-run acceptance
- **What:** `EN.3.K-dispatch-target-resolution` merged (PR #35) — a `brain.toml`-backed
  repo-slug registry, `repo` on the event schema, and dispatch-time 422s for an unknown
  slug or an absent spec dir. Then amended `EN.3.J`'s spec: it predated `EN.3.K` and its
  script posts no `repo` field, so the smoke exercised only the pre-`EN.3.K` cwd-fallback
  path. Restructured into a `--repo` flag plus three human-in-the-loop tasks — run 1
  (engine acceptance, no `repo`), a 422 pre-flight, and run 2 (deployment acceptance,
  server started outside the brain tree). Tasks 1-5 left byte-untouched so the work
  already on draft PR #34 still maps.
- **Why:** The always-on `bastion serve` on the Mac Mini can only serve one repo while the
  target is the process's working directory. `EN.3.K` fixes that; the smoke had to be
  updated to actually prove it, otherwise the deployment's core property would ship
  unverified.
- **Refs:** `planning/EN.3.K-dispatch-target-resolution/`, `planning/EN.3.J-sdlc-flow-smoke/`
  (see its Amendment Log), `docs/deployment-launchd.md`


### EN.3.K-dispatch-target-resolution — repo slug registry + dispatch-time 422 validation
- **What:** `/sdlc-flow` on branch `EN.3.K-dispatch-target-resolution-flow`, all 10 tasks passed,
  PASS review. `SDLCFlowEventSchema` gained `repo: Option<String>` — a `brain.toml`-backed registry
  **slug**, never a raw path — and a new `RepoRegistry` (`crates/engine-core/src/repo_registry.rs`)
  resolves it to an absolute root, rejecting escaping/nonexistent entries and honoring an optional
  `ENGINE_REPO_ALLOWLIST` narrowing filter (tasks 1-2). `SetupWorktreeNode` anchors `worktree_path`
  and every git invocation (worktree add/remove, checkout) to the resolved root via an optional
  injected registry, staying byte-identical when `repo` is absent (task 3). `engine-serve` gained a
  process-global repo-registry seam (`set_repo_registry`/`repo_registry`/
  `init_repo_registry_from_env`) plus explicit-registry factory entry points, with bastion's
  existing one-argument call left untouched (task 4). `post_events` now pre-flight-rejects, with a
  422 naming the offending value and before minting a `run_id`, both an unknown repo slug and an
  absent `spec_slug` directory — a spec dir that exists but lacks `tasks.json` still dispatches
  (202) so `GenerateTasksNode`'s legitimate "author a missing plan" path is untouched (task 5).
  Hermetic integration coverage: a new `sdlc_flow_repo_resolution.rs` suite proving worktree
  creation/git cwds/per-run policy anchor to the resolved root (task 6), and 5 new tests extending
  `engine-serve`'s `dispatch_integration.rs` covering both 422 cases, the tasks.json-absent
  non-regression, the no-`repo`-field default, and the EN.5.F `run_id`==`event_id`
  contract (task 7). `planning/ticket-local-policy-harness-file` — the Mac Mini's single-target
  `ENGINE_HARNESS_PATH` proposal — was confirmed never implemented and marked Superseded by EN.3.K
  in place, no code change (task 8). Docs: `docs/deployment-launchd.md` added (launchd
  `EnvironmentVariables` checklist, `ENGINE_BRAIN_ROOT` soft-to-loud failure note), plus updates to
  `sdlc-flow-workflow.md`, `sdlc-flow-smoke.md`, `architecture.md`, `docs/index.md` (task 9). Full
  validation gate green — fmt, clippy `-D warnings`, `cargo nextest run --workspace` (1563 passed),
  release build, cross-repo bastion `cargo check` (task 10, no code changes needed).
- **Why:** A single always-on `bastion serve` under launchd has exactly one `WorkingDirectory`, so
  before this block it could only ever drive `SDLC_FLOW` against one of the fleet's 11+ repos, and
  an unknown `spec_slug` was silently routed to `GenerateTasksNode` instead of rejected — together
  meaning a network-triggered, `dangerously_skip_permissions: true` agentic run could write in an
  unintended directory or invent a plan for a spec that does not exist. `repo` is deliberately a
  registry slug, not a path, so the reachable set stays "the ~20 repos in `brain.toml`" rather than
  "the filesystem," even if the caller's API key leaks.
- **Refs:** `planning/EN.3.K-dispatch-target-resolution/`, decision `D8-autonomous-node-write-permission.md`,
  superseded ticket `planning/ticket-local-policy-harness-file/`.
- **Next:** Pick up `EN.5.B1` (eval slice runner), `EN.5.C` (EXTERNAL_INTEL), `EN.6.C`/`EN.6.D`
  (Slack/Telegram-WhatsApp adapter skeletons), `EN.6.G` (schedule source), `EN.6.I` (LEAD_INGEST),
  or `EN.4.D` (DELIVERABLE_RENDER) per the `next` frontmatter list. `EN.3.J`'s human-in-the-loop
  smoke run (PR #34) is now fully unblocked on the engine-rs side (`EN.3.D`/`EN.3.E`/`EN.3.G`/`EN.3.K`
  all Done).

```
bcf872e feat: implement EN.3.K-dispatch-target-resolution-task9
b0bcc29 feat: implement EN.3.K-dispatch-target-resolution-task7
65e93bc feat: implement EN.3.K-dispatch-target-resolution-task6
a0a2402 feat: implement EN.3.K-dispatch-target-resolution-task5
ae4b281 feat: implement EN.3.K-dispatch-target-resolution-task4
1166211 feat: implement EN.3.K-dispatch-target-resolution-task3
f2b88e7 feat: implement EN.3.K-dispatch-target-resolution-task2
99d7754 feat: implement EN.3.K-dispatch-target-resolution-task1
```

---

### SDLC_FLOW hardening — check-selection parity, final gate, terminal paths, hermetic tests
- **What:** Merged `EN.3.D` (PR #31), `EN.3.E` (PR #32), `EN.3.G` (PR #33) and
  `EN.ticket.hermetic-test-temp-dirs`. The Rust engine now honours `fastCommand`,
  `perTask: false` and per-task `validation_commands`; a new unconditional
  `FinalValidationNode` runs the full suite once on the drain branch; no run can end
  without a terminal state; and PID-keyed test temp dirs are hermetic (a recycled PID
  was inheriting a populated dir, producing a false FAIL that bailed `EN.3.J`).
  Merged the Mac Mini's local-model work into the brain — the D12 collision resolved by
  renumbering theirs to D13 — amended and promoted `ticket-wire-meta-transport-telemetry`,
  and specced `EN.3.K` for multi-repo dispatch. `EN.3.J` smoke apparatus is on draft PR #34
  awaiting a human run.
- **Why:** Preparing to retire the JS `sdlc-flow.js` engine and run everything through
  engine-rs, triggered from bastion-web against an always-on `bastion serve`. Every Rust
  task attempt was paying a full `nextest --workspace` plus a release build (2m44s vs 6.4s
  per CLAUDE.md), which made the engine impractical for daily use.
- **Refs:** `planning/EN.3.D-check-selection-parity/`, `EN.3.E-final-validation-node/`,
  `EN.3.G-terminal-path-robustness/`, `EN.3.K-dispatch-target-resolution/`,
  decision D12-per-task-vs-final-check-depth


### `EN.3.J-sdlc-flow-smoke` — BAILED (tasks 1-5 passed, review FAIL)
- **What:** Ran `/sdlc-flow EN.3.J-sdlc-flow-smoke` on branch `EN.3.J-sdlc-flow-smoke-flow`, scoped to
  tasks 1-5. Authored the minimal one-task smoke spec (`planning/smoke-sdlc-flow/tasks.json` +
  OKF-frontmattered `tasks.md`) whose only job is to make an agentic node write `SMOKE.md` containing
  `ENGINE-SMOKE` at the worktree root (task 1); added `scripts/sdlc_smoke.sh`, an executable
  trigger-and-watch harness that POSTs the `SDLC_FLOW` smoke event, extracts `event_id` from the 202,
  polls `GET /events/{event_id}` printing status transitions, and exits 0/1/2 on
  succeeded/failed-cancelled-budget_halted/timeout, never touching the SSE stream endpoint (task 2);
  gave `--clean` its real implementation — removes the smoke worktree, deletes the branch, and
  `rm -rf`'s the leftover `sdlc-flow-state.json` dir, each step tolerant of the resource being absent,
  with an inline comment explaining the `SpecExistsRouterNode` resume hazard (task 3); fixed
  `agentic-portfolio/scripts/health_check.sh --full` (committed in the parent repo) to recognize the
  real terminal status vocabulary (`succeeded|failed|cancelled|budget_halted`) instead of the
  nonexistent `"completed"` (task 4); added `docs/sdlc-flow-smoke.md` documenting the six operational
  prerequisites, the event-flag rationale, the QuickLaunch watch limitation, the run/cleanup
  procedure, and the real status vocabulary, registered in `docs/index.md` (task 5). All five tasks
  passed on first attempt. The run then bailed at review: `cargo nextest run --workspace` (a gating
  check) fails on two pre-existing tests in `sdlc_flow::setup`
  (`spec_exists_routes_to_generate_when_absent`, `spec_exists_ignores_state_file_at_old_flat_path`) —
  confirmed via `git diff main..HEAD` to touch zero `.rs` files and to leave `setup.rs` untouched, so
  the failure predates this branch and is out of scope to fix under EN.3.J, but per review protocol a
  fresh gating-check failure still blocks PASS regardless of attribution. Separately, task 6 (the
  human-in-the-loop real-run evidence acceptance criterion) is explicitly forbidden to an agent by the
  spec's own Notes — "an agent reaching it must stop and hand off" — so it remains unmet by design at
  this point in the spec, not a defect in tasks 1-5. Both issues require human handoff rather than
  another automated retry: the setup-test regression needs triage as its own fix (likely from another
  block or environment drift), and task 6 needs an actual human-triggered run with real evidence
  recorded in `tasks.md`'s Notes. No production Rust code changed on this branch. Next: triage and fix
  the two pre-existing `sdlc_flow::setup` test failures (likely as a follow-up chore, not under
  EN.3.J), then have a human trigger the real smoke run from bastion-web and record the evidence
  before EN.3.J can close.

```
808c2be feat: implement EN.3.J-sdlc-flow-smoke-task5
f315814 feat: implement EN.3.J-sdlc-flow-smoke-task3
038002b feat: implement EN.3.J-sdlc-flow-smoke-task2
e10818a Merge pull request #33 from bredmond1019/EN.3.G-terminal-path-robustness-flow
26f20ff chore: wrap up EN.3.G-terminal-path-robustness
f03d08c docs: update docs for EN.3.G-terminal-path-robustness
7bd83c9 feat: implement EN.3.G-terminal-path-robustness-task8
da808b1 feat: implement EN.3.G-terminal-path-robustness-task7
```

### `EN.3.G-terminal-path-robustness` — PASS (all 9 tasks)
- **What:** Ran `/sdlc-flow EN.3.G-terminal-path-robustness` on branch
  `EN.3.G-terminal-path-robustness-flow`. Made it structurally impossible for an `SDLC_FLOW` run to
  end without a terminal state on disk, plus six correctness defects found alongside that
  investigation. `TriageRouterNode::route`/`ReviewRouterNode::route` now fall back to `WrapUpNode`
  instead of `None` on any unrecognized verdict, stamping `unrecognized_verdict` on the upstream
  node's result and surfacing it into the terminal state's `bail_reason` — closing the one remaining
  way a run could leave a permanently `running` state file, since a `None` route ends the walk `Ok`
  with no failed node for `write_terminal_blocked_state`'s net to catch (task 1). Unified the two
  divergent `latest_state` implementations onto `task_loop.rs`'s (the one that already considers
  `IncrementAttemptNode`), fixing `WrapUpNode`'s prior under-reporting of `attempt_count`/
  `total_retries` on a post-retry `MAJOR_BAIL` (task 2). Extracted `SaveStateNode`'s git-add/commit
  tail into a shared `commit_state_file` helper and gave `WrapUpNode` its own `CommandRunner` so its
  terminal write is now committed — a `--worktree` run's PR previously did not contain its own final
  `done`/`blocked` state (task 3). `log_noop_commit` now classifies a non-zero git commit exit as a
  quiet no-op vs. a genuine `eprintln!` warning via a new pure `is_noop_commit` helper (task 4).
  `run_forbidden_pattern_scan` now invokes `grep` directly via its own argv entry instead of
  interpolating into `sh -c`, closing a shell-injection hole, with a documented quote-escaped fallback
  retained only for glob paths (task 5). `EmitStateNode` patches the committed state's `pr` block
  (url + number parsed from the PR URL's trailing segment) in place after `PullRequestNode` runs, via
  `commit_state_file`, as a silent best-effort no-op on any missing precondition and with the declared
  graph unchanged (task 6). `stream_event` now uses the same three-tier lookup as `get_event` (live
  map → terminal record ring → `live_run_metadata()`), closing the SSE 404 race for a run registered
  but not yet snapshotted (task 7). A new hermetic `sdlc_flow_terminal_paths.rs` integration suite
  proves a garbage triage/review verdict still reaches a terminal on-disk state naming the offending
  string, `pr` populated only under `auto_pr:true`, and the happy path still reaching `status:"done"`
  (task 8). Full validation gate green — fmt, clippy `-D warnings`, workspace `nextest`, release
  build, cross-repo bastion check — with zero code changes needed (task 9). PASS review. This closes
  `EN.3.G`, unblocking `EN.3.J` (SDLC_FLOW e2e smoke run) on this half of its dependencies. Next: pick
  up `EN.5.B1`/`EN.5.C`/`EN.6.C`/`EN.6.D`/`EN.6.I`/`EN.4.D`/`EN.ticket.expose-live-run-workflow-type`/
  `EN.chore.master-plan-hygiene` per the `next` frontmatter list.

```
f03d08c docs: update docs for EN.3.G-terminal-path-robustness
7bd83c9 feat: implement EN.3.G-terminal-path-robustness-task8
da808b1 feat: implement EN.3.G-terminal-path-robustness-task7
b939e1a feat: implement EN.3.G-terminal-path-robustness-task6
5333746 feat: implement EN.3.G-terminal-path-robustness-task5
2f31a5b feat: implement EN.3.G-terminal-path-robustness-task4
94be7e7 feat: implement EN.3.G-terminal-path-robustness-task3
295e000 feat: implement EN.3.G-terminal-path-robustness-task2
39d5b66 feat: implement EN.3.G-terminal-path-robustness-task1
```

### `EN.3.E-final-validation-node` — PASS (all 7 tasks)
- **What:** Ran `/sdlc-flow EN.3.E-final-validation-node` on branch
  `EN.3.E-final-validation-node-flow`. Gave the Rust `SDLC_FLOW` graph a second check-running
  site: `FinalValidationNode`, an unconditional, full-depth, unfiltered harness gate that runs
  exactly once per run on the task-loop drain branch, restoring `cargo nextest run --workspace`
  and `cargo build --release` — both of which EN.3.D's cheap per-task tripwire (`fastCommand`
  substitution + `perTask:false` exclusion) had left unexercised. `FinalValidationNode` shares
  `TestTaskNode`'s `run_checks`/`select_task_checks` machinery via a widened `pub(crate)` surface
  and a new `apply_per_task_filter: bool` parameter on `select_task_checks`, so it can select
  checks without dropping `perTask:false` entries while `TestTaskNode`'s own call site stays
  byte-identical (task 1). It's wired onto the drain branch —
  `TaskQueueRouterNode(no pending) -> FinalValidationNode -> PatchDocsNode` — registered
  unconditionally (no policy gate, per CLAUDE.md standing rule 6) in `registry()`; total node
  count is now eighteen (task 2). Its outcome folds into `WrapUpNode`'s committed state as a
  new additive `CommittedFinalValidation` — a sixth transient parameter on
  `to_committed_state_json`, threaded through every call site including `setup.rs`,
  `engine-serve/http.rs`, and the golden fixture; a failing gate is reported as a degraded
  terminal status, not converted into a bail (task 3, one fix cycle for a pre-existing,
  unrelated `content_pipeline::profiles` test). `sdlc_flow_e2e.rs` gained tail/drain assertions,
  a "runs exactly once per run" multi-task test, and a failing-gate degraded-result test, plus
  two `sdlc_flow_task_loop.rs` integration tests were repaired against the new graph identity
  (task 4). Decision D12 recorded — per-task check depth is a legitimate policy knob, but
  whether the authoritative suite runs at all is the run's correctness contract and must stay
  an unconditional node (task 5). Docs updated across `sdlc-flow-workflow.md` (graph diagram,
  node table, two-check-site subsection), `architecture.md` (pointer note), and
  `sdlc-flow-policy.md` (corrected a stale claim that only "the end-of-run Validate task and CI"
  ran the full suite) (task 6). Full authoritative validation gate green — fmt, clippy
  `-D warnings`, workspace `nextest` (1491 tests passed), release build (task 7). PASS review.
  This closes `EN.3.E`, completing the EN.3.D/EN.3.E pair that gives Rust `SDLC_FLOW` the same
  two-check-site model the JS engine has always had. Next: `EN.3.G` (terminal-path robustness),
  or `EN.5.B1`/`EN.5.C`/`EN.6.C`/`EN.6.D`/`EN.6.I`/`EN.4.D` per the frontmatter `next` list.

```
e5dff68 feat: implement EN.3.E-final-validation-node-task6
04965ce feat: implement EN.3.E-final-validation-node-task4
744d4cd feat: implement EN.3.E-final-validation-node-task3
a69ea36 feat: implement EN.3.E-final-validation-node-task2
82fb2b1 feat: implement EN.3.E-final-validation-node-task1
9cbee4d Merge pull request #31 from bredmond1019/EN.3.D-check-selection-parity-flow
c96cebb chore: wrap up EN.3.D-check-selection-parity
4ff8602 fix: review pass 1 for EN.3.D-check-selection-parity
```

### `EN.3.D-check-selection-parity` — PASS (all 8 tasks)
- **What:** Ran `/sdlc-flow EN.3.D-check-selection-parity` on branch
  `EN.3.D-check-selection-parity-flow`. Gave the Rust `SDLC_FLOW` the three per-task
  test-selection behaviors the JS engine already had and the Rust engine implemented none of:
  `fastCommand` substitution, `perTask: false` exclusion, and a task's own `validation_commands`
  overriding the project-wide harness suite. `SdlcPolicy` gained a `test_depth: TestDepth`
  (`full`/`fast`, built-in default `full`) knob resolving through all four policy layers (task 1),
  set explicitly in all four named profiles — `baseline` -> `full`, `cheap-fast`/`pragmatist`/
  `batch-reviewer` -> `fast` (task 2). A pure `select_task_checks` free function + `CheckSelection`
  telemetry struct landed in `task_loop.rs`, matching the JS engine's precedence exactly:
  non-empty `validation_commands` wins verbatim; otherwise harness checks minus `enabled:false`
  minus `perTask:false`, with `fastCommand` substituted at `fast` depth (task 3). `TestTaskNode`
  was wired to call it via the strict stamped policy read, additively stamping
  `test_depth`/`check_source`/`excluded_checks` while leaving `run_checks`/`all_passed`/
  `check_results`/`failure_summary` unchanged in shape, and fixing the harness-missing auto-pass
  bug on the same code path into a real gating failure (task 4, one fix cycle for a missing
  policy stamp in a shared test helper). Task 5 repaired the resulting test churn across two
  integration fixtures without weakening any assertion. Task 6 added a new hermetic
  `sdlc_flow_check_selection.rs` integration module driving the real `TestTaskNode` with a
  recording `CommandRunner`, asserting exact command strings at both depths, the per-task
  override, and the harness-missing gate. Task 7 documented the knob and precedence table in
  `planning/harness.json`/`docs/sdlc-flow-policy.md`. Task 8 ran the full validation gate — fmt,
  clippy `-D warnings`, `cargo nextest run --workspace` (1476 passed), release build — all green
  with no changes needed. PASS review (2 attempts, no findings survived). This closes `EN.3.D`,
  unblocking `EN.3.E` (FinalValidationNode). Next: `EN.3.E`, or pick up `EN.5.B1`/`EN.5.C`/
  `EN.6.C`/`EN.6.D`/`EN.6.I`/`EN.4.D` per the status frontmatter.
```
4ff8602 fix: review pass 1 for EN.3.D-check-selection-parity
5c88235 feat: implement EN.3.D-check-selection-parity-task7
8912fd9 feat: implement EN.3.D-check-selection-parity-task6
ccd6df6 feat: implement EN.3.D-check-selection-parity-task5
56bf8af fix: fix pass 1 for EN.3.D-check-selection-parity-task4
da1deea feat: implement EN.3.D-check-selection-parity-task4
cca8e45 feat: implement EN.3.D-check-selection-parity-task3
db84cce feat: implement EN.3.D-check-selection-parity-task2
78764e1 feat: implement EN.3.D-check-selection-parity-task1
```

### `EN.6.J-flow-state-run-id` — PASS (all 8 tasks)
- **What:** Ran `/sdlc-flow EN.6.J-flow-state-run-id` on branch `EN.6.J-flow-state-run-id-flow`.
  `RunMeta`/`SDLCState` gained `run_id: Option<String>`, emitted/round-tripped as a top-level
  `"run_id"` JSON key in `to_committed_state_json`/`from_committed_state_json`, tolerating both an
  absent key (the JS `sdlc-flow.js` engine's shape) and explicit null (task 1); `RunOptions` now
  carries `run_id: Option<Uuid>`, stamped into `ctx.metadata` by both `Workflow::run_with` and
  `run_from` before the walk starts, with `read_run_id`/`RUN_ID_METADATA_KEY` re-exported from
  `lib.rs` (task 2); `SaveStateNode`'s per-task `build_run_meta` now reads the run id back off
  `ctx.metadata` so every intermediate `sdlc-flow-state.json` write carries it (task 3); `WrapUpNode`
  stamps `run_id` into `RunMeta` on the run's tail write, and a new public
  `write_terminal_blocked_state(ctx, reason)` writes a `"blocked"` status + `bail_reason` for a
  failed walk — a safe no-op with no worktree, no loaded state, or no pre-existing `sdlc/` directory
  (task 4); `suspend::spawn_run` stamps `run_id` into `RunOptions` and, on a failed (non-suspended)
  `SDLC_FLOW` walk, calls the terminal writer — the run-result match was restructured so a plain
  `Ok(Ok(ctx))` whose `node_runs` recorded a `Failed` node is treated as a failure too, not just the
  `Ok(Err)`/panic branches (task 5); a hermetic `sdlc_flow_run_id_terminal.rs` e2e suite (a module of
  the single `engine-core` `it` binary) covers a forced node error halting a real `Workflow::run_with`
  walk and the failure-path writer producing `blocked` + `bail_reason` + `run_id`, a clean walk
  reaching `WrapUpNode` and writing `done` + the same `run_id`, and JS-engine-shape (no `run_id` key)
  committed JSON parsing to `None` while preserving other D31 fields on rewrite (task 6);
  `docs/sdlc-flow-workflow.md`/`docs/architecture.md` updated to document the `run_id` key and the
  failure-path terminal write (task 7); full validation gate green — `fmt`, `clippy -D warnings`,
  workspace `nextest` (1450 passed, 16 skipped), `build --release`, no code changes needed (task 8).
  PASS review. This closes the engine-rs half of `EN.6.J`; the `bastion`
  `WorkflowStateDto`/`docs/serve-api.md` follow-on is explicitly out of scope per the spec's Notes
  (a separate repo, a separate spec). Next: pick up `EN.5.B1`/`EN.5.C`/`EN.6.C`/`EN.6.D`/`EN.6.I`/
  `EN.4.D` per the `next` frontmatter list.

```
58ffe70 docs: document run_id stamp and terminal-blocked failure write (EN.6.J task 7)
c0f685b feat: implement EN.6.J-flow-state-run-id-task6
e2aef53 feat: implement EN.6.J-flow-state-run-id-task5
4681089 feat: implement EN.6.J-flow-state-run-id-task4
bea9af4 feat: stamp run_id in SaveStateNode per-task writes (EN.6.J task 3)
3fe5889 feat: implement EN.6.J-flow-state-run-id-task2
00ef137 feat: implement EN.6.J-flow-state-run-id-task1
```

---

## [run: 2026-07-30]

### `EN.6.B-email-adapter` — PASS (all 8 tasks)
- **What:** Ran `/sdlc-flow EN.6.B-email-adapter` on branch `EN.6.B-email-adapter-flow`. `OutboundAction`
  gained an additive, defaulted, omit-when-empty `metadata: BTreeMap<String,String>` field, with
  `new()`/`with_metadata()`/`metadata_value()` helpers and byte-identical serialization for empty
  metadata (task 1). `EmailChannelTransport` was added as the live `ChannelTransport` impl sending
  through the Resend HTTP API over the injectable `HttpPost` seam — env-only `RESEND_API_KEY`,
  `ReplyContext` threaded into `In-Reply-To`/`References`, and `metadata["opportunity_slug"]` echoed
  as a Resend `tags` entry for bounce correlation (task 2). `LiveChannelTransport` now routes
  `ChannelType::Email` to it instead of `UnwiredChannelTransport`, and `unwired_channel_error`'s
  stale block attributions were corrected (Slack → EN.6.C, Telegram/WhatsApp → EN.6.D) (task 3).
  `parse_inbound_email` was added as a pure, deterministic Resend inbound-mail → `IngressEnvelope`
  parser (message-id or v5-uuid-derived `envelope_id`, text/html fallback, attachments, reply
  context, RFC 3339 timestamp fallback) (task 4), and `map_delivery_event` as a pure Resend
  delivery/bounce webhook → `AddOpportunityActionEvent` mapper that skips untagged/unrecognized
  events explicitly rather than erroring (task 5). `engine-serve` gained `POST
  /webhooks/email/inbound` (dispatches `CONTENT_PIPELINE`, returns `202 {run_id, event_id,
  envelope_id}`) and `POST /webhooks/email/events` (dispatches `OPPORTUNITY_ADD_ACTION` via the tag
  echo, or `202 {skipped, reason}`), both `X-API-Key` gated, via a self-contained dispatch helper
  that left `post_events` untouched as the regression gate (task 6). `docs/email-adapter.md` was
  added (OKF-framed: env vars, tag-echo correlation, both routes, the D51/D53 boundary, the
  no-policy-surface rationale) and cross-referenced from `docs/index.md`/`harness.json` (task 7).
  Task 8, the designated full-suite validation task, found and fixed one stale integration-test
  assertion in `action_dispatch_e2e.rs` left over from task 3's routing change (it still expected
  email to hit `UnwiredChannelTransport`); all four validation commands (fmt, clippy `-D warnings`,
  `nextest --workspace` with `RESEND_API_KEY` unset, release build) then passed clean — 1428 tests,
  16 skipped. Review verdict: PASS, no findings.
- **Decision:** Bounce → opportunity correlation stays a pure tag echo on the send (no corpus scan,
  no address index) — this block only plumbs and tests the `metadata` field; the sender that
  populates `opportunity_slug` is `EN.6.H2`'s work. Svix signature verification on the webhook
  routes is deliberately deferred, noted alongside the retry/queue durability `EN.6.A` also
  deferred. This closes `EN.6.B`, giving `EN.6.H` (OUTREACH workflow) its first live channel
  transport — it previously needed only `EN.6.B` and `EN.7.B` (already Done) to unblock.
- Next: `EN.5.B1`/`EN.5.C`/`EN.6.C`/`EN.6.D`/`EN.6.G`/`EN.4.D` per `status.md`'s `next` frontmatter
  list.

```
a6e1acb docs: update docs for EN.6.B-email-adapter
e27736e feat: implement EN.6.B-email-adapter-task8
5bdef47 feat: implement EN.6.B-email-adapter-task7
077c567 feat: implement EN.6.B-email-adapter-task6
0a0c0d6 feat: implement EN.6.B-email-adapter-task5
66f83bd feat: implement EN.6.B-email-adapter-task4
827da60 feat: implement EN.6.B-email-adapter-task3
```

### `EN.4.G-needs-further-research` — PASS (all 8 tasks)
- **What:** Ran `/sdlc-flow EN.4.G-needs-further-research` on branch `EN.4.G-needs-further-research-flow`.
  `CompanyBrief`/`ProspectLead` gained `needs_further_research: Vec<String>` (`#[serde(default)]`,
  always serialized, additive to both JSON schemas, never `required`) (task 1). A new `grounding`
  policy knob (`GroundingDepth::Standard`/`Strict`, deliberately no `off` per Rule 5) was added to
  `ResearchAgentPolicy`, resolving through all four layers with `baseline`/`cheap-fast` at `standard`
  and `thorough` at `strict` (task 2). `CompanyResearchNode` now appends a depth-aware
  `grounding_directive()` (naming FAR/DFARS and Brazilian data-residency examples, kept-not-deleted
  framing, `strict` adding a per-claim pass over `pain_points`/`outreach_hooks`) and stamps
  `grounding_depth` plus a derived, never-model-trusted `validation_required` onto its result (task 3);
  `ProspectingResearchNode` mirrors the directive per-lead and additionally stamps a sweep-level,
  order-stable, deduped `needs_further_research` union (task 4). `okf-core`'s `Opportunity` gained the
  same field end to end — always emitted in frontmatter, `validation_required` always re-derived —
  mapped from both `from_company_brief`/`from_prospecting_result` and recovered by `from_frontmatter`,
  committed separately in `core/okf-core` (task 5). A hermetic `research_agent_grounding_e2e.rs`
  proves the flag survives a real `Workflow::run` into the written Opportunity's frontmatter for both
  company and prospecting modes (task 6). `docs/research-agent-workflow.md`/`docs/index.md` were
  updated with the grounding contract and the knob (task 7). Full validation (engine-rs fmt/clippy
  `-D warnings`/nextest/release build, plus okf-core's and mev's own suites) was green with no code
  changes needed (task 8). Review verdict: PASS, no findings.
- **Decision:** `validation_required` is derived everywhere (node stamp, `Opportunity` method), never
  independently model- or user-settable, so a document can never disagree with its own flagged-claims
  list. This closes the `research-agent-needs-further-research-flag` carryover in `planning/state.json`
  and unblocks `EN.6.H1` (OUTREACH_DRAFT), which reads the field unconditionally.
- Next: `EN.5.B1`/`EN.5.C`/`EN.6.C`/`EN.6.D`/`EN.4.D` per `status.md`'s `next` frontmatter list.

```
e4105a0 feat: implement EN.4.G-needs-further-research-task7
499fbb4 feat: implement EN.4.G-needs-further-research-task6
acc9b08 feat: implement EN.4.G-needs-further-research-task4
6e2da0c feat: implement EN.4.G-needs-further-research-task3
9c2e121 feat: implement EN.4.G-needs-further-research-task2
a3a1103 feat: implement EN.4.G-needs-further-research-task1
```

### `EN.4.E-contact-enrichment` re-run — PASS (all 11 tasks)
- **What:** Completed the `/sdlc-flow EN.4.E-contact-enrichment` re-run on branch
  `EN.4.E-contact-enrichment-flow` that had previously bailed at Task 11. Tasks 1, 3–10 remained
  no-ops (already implemented and merged in the spec's original PR #22, `5cfa09e`) — each
  re-verified against every acceptance criterion with no code changes. Task 2 needed a trivial
  fmt/import-ordering fix in `suspend_resume_postgres_restart.rs` (`d67354e`, unrelated to EN.4.E's
  own scope). Task 11 (full-suite validation, the designated owner of `cargo test`/
  `cargo build --release` per the spec's Validation Commands) hit the same regression that bailed
  the prior attempt — several `crates/engine-serve` suspend/resume tests failing with "run never
  landed in the suspended index" plus a body-not-empty assertion. Root-caused this time: those
  tests race on process-global `OnceLock`-backed registries (the suspended-run index and pause
  signals from `EN.6.F-suspend-resume`) and only ran safely under `cargo nextest run`'s
  one-process-per-test isolation — plain `cargo test`'s shared-process threading let concurrent
  tests interfere. Fixed with a `#[cfg(test)]` shared mutex serializing the affected tests in
  `suspend.rs`/`resume.rs`/`http.rs` — a test-only change, no production suspend/resume behavior
  altered. Full validation (fmt, clippy `-D warnings`, workspace `cargo test`/`nextest`,
  `cargo build --release`, plus okf-core's and mev's own suites) green; review PASS; docs
  (`docs/testing.md`) updated to note the shared-registry test-isolation caveat.
- **Decision:** This closes `EN.4.E` for good — the earlier bail was a test-infrastructure gap
  in an adjacent block (`EN.6.F`), not a defect in this spec's own deliverables, and it is now
  fixed without touching production code.
- Next: `EN.5.B1`/`EN.5.C`/`EN.6.C`/`EN.6.D`/`EN.4.D` per `status.md`'s `next` frontmatter list.

```
5b722e5 docs: update docs for EN.4.E-contact-enrichment
7eb2a1a fix: fix pass 1 for EN.4.E-contact-enrichment-task11
11173d4 chore: wrap up EN.4.E-contact-enrichment
d67354e fix: fix pass 1 for EN.4.E-contact-enrichment-task2
74ebe29 test: cover corrupt/malformed persisted suspension state
7c8680a test: cover Postgres restart-durability resume path (rehydrate_from_store)
b95b4be Merge pull request #26 from bredmond1019/EN.6.F-suspend-resume-flow
206ee71 chore: wrap up EN.6.F-suspend-resume
```

### `EN.4.E-contact-enrichment` re-run — BAILED at Task 11 (resume-module regression)
- **What:** Ran `/sdlc-flow EN.4.E-contact-enrichment` on branch `EN.4.E-contact-enrichment-flow`
  against a spec that had already fully shipped in a prior session (PR #22, `5cfa09e`, all 11
  tasks PASS-reviewed 2026-07-28). Tasks 1, 3–10 verified as already-implemented no-ops (working
  tree clean, every acceptance criterion re-checked against the existing code, no commits made).
  Task 2 needed a trivial fmt/import-ordering fix in `suspend_resume_postgres_restart.rs`
  (committed as `d67354e`, unrelated to EN.4.E's own scope). Task 11 (full-suite validation) then
  hit a real, reproducible failure: `task_validation_3`/`cargo test` fails across multiple
  resume/suspend tests with the signature "run never landed in the suspended index" plus a
  body-not-empty assertion, on two consecutive attempts with no evidence of progress between
  them — a structural bug in the suspend/resume state transition (`EN.6.F-suspend-resume`'s
  primitive stack), not a flake and not in scope for this spec to fix. The run bailed rather than
  attempting an unbounded fix inside an unrelated block's territory.
- **Decision:** Left `EN.4.E`'s own deliverables untouched (they are correct and already merged);
  did not force a third fix attempt on the resume module from within this spec's flow. The
  resume/suspend regression needs its own triage against `EN.6.F`'s test suite before any further
  `EN.4.E`-branch validation can go green.
- Next: triage the suspend/resume "run never landed in the suspended index" failure directly
  (likely in `crates/engine-serve` or `engine-core::suspend`) as its own fix pass, independent of
  `EN.4.E`; once green, either re-run this spec's validation or fold the fix into a follow-on
  ticket against `EN.6.F`.

```
d67354e fix: fix pass 1 for EN.4.E-contact-enrichment-task2
74ebe29 test: cover corrupt/malformed persisted suspension state
7c8680a test: cover Postgres restart-durability resume path (rehydrate_from_store)
b95b4be Merge pull request #26 from bredmond1019/EN.6.F-suspend-resume-flow
206ee71 chore: wrap up EN.6.F-suspend-resume
90d7d24 feat: implement EN.6.F-suspend-resume-task14
cc0245c feat: implement EN.6.F-suspend-resume-task13
908a64f feat: implement EN.6.F-suspend-resume-task12
```

---

## [run: 2026-07-30]

### `EN.6.F-suspend-resume` — human-in-the-loop approval gate: run_from, SuspendNode, pause/resume/suspended-list HTTP surface
- **What:** Ran `/sdlc-flow EN.6.F-suspend-resume` on branch `EN.6.F-suspend-resume-flow`. All 15
  tasks passed, PASS review. Landed the full suspend/resume primitive stack: `PauseSignal` (a
  two-way, clearable sibling of `CancellationToken`) plus the `metadata.suspension` marker
  read/write API in `engine-core::suspend` (task 1); `BudgetLedger::from_parts`/`from_context` for
  ledger rehydration on resume (task 2); `Workflow::run_with` split into `seed_context`/`walk` so a
  fresh run and a resumed run share one pointer-walk loop (task 3); `Workflow::walk` gained a
  loop-top `PauseSignal` check and post-node `SuspendNode` finalization, plus
  `Workflow::run_from`/`ResumeState`/`without_seeded_nodes` for rehydrating a suspended
  `TaskContext` at its saved pointer (task 4); `SuspendNode` — the workflow-authored suspension
  request node, default-off, in-place no-op — added (task 5); a 15-test hermetic `engine-core`
  integration suite covering marker semantics, loop-mid-suspend, and rehydration (task 6);
  `engine-store` gained `upsert_event` (idempotent) + `get_task_context`, `engine-serve`'s durable
  writer always upserts (task 7); the suspended-run index (`TakeForResume`,
  `AlreadyResuming`/`NotFound` semantics, one lint-fix cycle) (task 8); `publish_suspended`/
  `clear_terminal` on the SSE stream registry so a resumed run streams fresh (task 9);
  `spawn_run`/`SpawnedRun`/`RunStart` extracted so trigger and resume share one
  terminal-vs-suspended exit fork (task 10); the HTTP surface — `POST /events/{run_id}/pause`,
  `POST /events/{event_id}/resume`, `GET /events/suspended` — with full Postgres-fallback
  rehydration for restart survival (task 11); `GET /events/{event_id}` now reports `status:
  "suspended"` for a live run with an open marker (task 12); a 13-test hermetic `engine-serve` HTTP
  suite covering the full pause/resume/list surface, plus a required production fix to `abort_run`
  so a suspended run stays killable (task 13, logged in the spec's Amendment Log); `docs/
  suspend-resume.md` added (task 14); full validation gate green — fmt, clippy `-D warnings`,
  workspace `nextest` (1342/1342, `DATABASE_URL` unset), release build, cross-repo bastion `cargo
  check` clean (task 15). Two genuine spec deviations, both logged in the spec's Amendment Log:
  task 11 added a `pool`/`pool()` accessor to `engine-serve`'s `DurableHandle` (outside its
  declared `files`) because the spec's own Postgres-fallback rehydration algorithm required it;
  task 13 fixed `abort_run` to fall back to the suspended index when a run has no live
  `CancellationToken`, because the spec's own coverage bullet ("a suspended run must still be
  killable") could not pass without it. This closes `EN.6.F`, giving Phase 6 its human-in-the-loop
  approval gate — `EN.6.H` (OUTREACH workflow) now needs only `EN.6.B` (email adapter) to unblock.
  Next: `EN.5.B1`/`EN.5.C`/`EN.6.C`/`EN.6.D`/`EN.4.D` per the `next` frontmatter list.

```
90d7d24 feat: implement EN.6.F-suspend-resume-task14
cc0245c feat: implement EN.6.F-suspend-resume-task13
908a64f feat: implement EN.6.F-suspend-resume-task12
4d02b2f feat: implement EN.6.F-suspend-resume-task11
6c60370 feat: implement EN.6.F-suspend-resume-task10
0975429 feat: implement EN.6.F-suspend-resume-task9
0848926 fix: fix pass 1 for EN.6.F-suspend-resume-task8
4fe00b4 feat: implement EN.6.F-suspend-resume-task8
```

## [run: 2026-07-29]

### `EN.6.E-research-ingress-dispatch` — self-feeding RESEARCH_AGENT → CONTENT_PIPELINE dispatch, review-fixed, docs patched
- **What:** Ran `/sdlc-task EN.6.E-research-ingress-dispatch` in place on `main` (no worktree/
  branch). All 7 tasks passed (task 3 needed one triage→fix cycle). Added
  `ResearchIngressDispatchNode` as the new terminal `RESEARCH_AGENT` node — gated by a default-off
  `ingress_dispatch` policy knob, it wraps a finished run's research output as an
  `IngressEnvelope` and sends a `TriggerWorkflow{CONTENT_PIPELINE}` action over the existing
  `ChannelTransport` egress seam (`EN.6.A`), with a deterministic `envelope_id` and propagated
  `chain_depth`. `/code-review med --fix` then surfaced 6 findings but did not actually apply any
  of them (verified via a clean `git status`); fixed 4 by hand: (1)
  `ingress_dispatch.rs:175` — a materialize that legitimately planned zero actions now soft-skips
  instead of hard-failing the run via `?`; (2) `engine-serve/src/workflows.rs:102` —
  `register_research_agent` now rewires the node's transport to `ENGINE_EVENTS_URL`, mirroring
  `register_content_pipeline`'s `ActionDispatchNode` override (previously hardcoded to
  `localhost:8080` in any deployment); (3) `resolve_dispatch` now propagates a corrupted/
  unparsable resolved-policy stamp instead of silently falling back, matching
  `CompanyResearchNode`/`ProspectingResearchNode`; (4) extracted `DEFAULT_EVENTS_URL` +
  `receipt_from_send_result` into `channel_transport.rs`, shared by `ActionDispatchNode` and the
  new node instead of duplicated. Deliberately left two findings unfixed — `thorough()` enabling
  `ingress_dispatch` and `registry_for_policy`'s registry-then-override pattern — both are the
  task's own intentional, test-covered design, not bugs. Added 3 new tests for the two behavioral
  fixes. `/close-out --no-review` then ran the full validation suite (fmt/clippy/`nextest
  --workspace` 1258 tests/`build --release`, all green), found no blocking coverage gaps, and
  `/update-docs --patch` updated `docs/research-agent-workflow.md` (new "Self-feeding dispatch"
  section, six-node graph shape, policy/profile tables) plus stale terminal-node references in
  `docs/index.md`/`docs/architecture.md`/`docs/content-pipeline-workflow.md`.
- **Why:** `EN.6.E` closes the omni-channel Phase 6 self-feeding loop — a `RESEARCH_AGENT` run can
  now automatically chain into `CONTENT_PIPELINE` at the `thorough` quality tier, without a human
  manually re-triggering content generation off research output. The review-fix pass matters
  because `--fix` silently not applying its own findings would otherwise have shipped a hard
  run-failure bug (soft-skip case) and a wrong-URL production bug (missing `ENGINE_EVENTS_URL`
  wiring) straight to `main`.
- **Refs:** `planning/EN.6.E-research-ingress-dispatch/tasks.md`; `docs/research-agent-workflow.md`
  § Self-feeding dispatch; commits `10c349a`..`be167c2`.

### `EN.7.C-materialize-harvest-gate` — the materialize→harvest gate; Phase 7 complete
- **What:** Ran `/sdlc-flow` for `EN.7.C-materialize-harvest-gate` on branch
  `EN.7.C-materialize-harvest-gate-flow` (built on top of the already-merged `EN.7.D`). All 10
  tasks passed, PASS review. Added the generic `HarvestMode`/`HarvestGate` primitive
  (`off`/`in_process`/`approval`, snake_case serde) under `nodes/harvest_gate.rs`, with a
  deliberate default of `off` — a real behavior change, since `PersistToBrainNode` previously
  POSTed to Synapse's ingest endpoint unconditionally (task 1). Added a `harvest` policy knob
  (`HarvestConfig`/`PartialHarvestConfig`) resolved through the standard four-layer `Overlay`
  merge (task 2), set explicitly in all three existing named `CONTENT_PIPELINE` profiles plus a
  new `curated-harvest` profile selecting `in_process` (task 3). `PersistToBrainNode` gained
  `.with_harvest(HarvestGate)`, branching `process()` into Post/Skip/Defer while always building
  the payload first and stamping one stable `{posted, skipped, harvest_mode, status, artifact_id,
  response, pending}` key set across all three modes — the Defer branch builds a
  `pending_harvest_record` from `MaterializeDocNode`'s stamped paths (task 4). The graph now
  constructs `PersistToBrainNode` via a single `persist_to_brain_node(&HarvestConfig)` site shared
  by `registry()`/`registry_for_policy()` so the two paths can never drift (task 5). Added
  `HarvestApproveNode` (a generic `HttpPost` seam over a pending-harvest record) and the
  single-node `HARVEST_APPROVE` micro-workflow completing the human-approval hand-off — it
  re-POSTs the same payload to the same URL the in-process path would have, so there is still only
  one route into the index (task 6), registered in `engine-serve`'s `register_builtin_workflows`
  as a model-free workflow like the opportunity-edit micro-workflows (task 7). A hermetic
  `harvest_gate_e2e` suite (a module in the single `engine-core` `tests/it` binary, not a new test
  binary) drives a real `CONTENT_PIPELINE` run through all three `HarvestMode` paths plus the
  `HARVEST_APPROVE` hand-off and a failure case, verifying byte-identical POST payloads and
  identical materialized `.md` output across modes (task 8). `docs/harvest-gate.md` added
  (OKF-fronted) and cross-referenced from `docs/index.md`/`architecture.md`/
  `content-pipeline-workflow.md`/`materialize-doc-node.md`/`opportunity-edit-workflows.md` (task
  9). Full validation green: fmt, clippy `-D warnings`, full `nextest` workspace suite (1226
  passed), release build, hermetic e2e (tempdir + `StubHttpPost`, no real network), cross-repo
  bastion consumer check (task 10). Notable decision: the built-in default for the new knob is
  `off` rather than the usual "behavior-stable" default CLAUDE.md rule 6 calls for — called out
  explicitly in the spec as a deliberate operator-directed change, not a side effect, since the
  block's whole point is to stop the unconditional push and rely on the existing manifest/
  `index_brain` freshness reindex unless a run explicitly opts in. This closes `EN.7.C` and
  completes Phase 7 (brain-write loop): `EN.7.A`/`EN.7.B`/`EN.7.D`/`EN.7.C` are all Done.
  Next: open the `EN.7.C` PR / merge the branch, then pick up `EN.5.B1`, `EN.5.C`, `EN.6.C`/`EN.6.D`,
  or `EN.4.D` per `planning/status.md`'s `next` list.

```
46051d1 docs: update docs for EN.7.C-materialize-harvest-gate
c784aeb feat: implement EN.7.C-materialize-harvest-gate-task9
a2f07fc feat: implement EN.7.C-materialize-harvest-gate-task8
a1fe70b feat: implement EN.7.C-materialize-harvest-gate-task7
e7136ba feat: implement EN.7.C-materialize-harvest-gate-task6
093c3f7 docs: cargo clean measurement — a rotten target/ was the largest lever
40ae190 docs: testing.md — test layout, commands, hermetic conventions (D57)
0725c14 perf(harness): cut sdlc-flow iteration cost — one test binary, no sccache, per-task validation
```

---

### Traced the per-task/review test-relink slowdown to base-template; wrote its fix as a ticket there; applied a local link-time mitigation here
- **What:** `/prime` surfaced the still-open `EN.7.D` review/PR handoff plus carryover
  `harness-per-task-relinks-all-test-binaries`. Reviewed that carryover in depth against the real
  code (`sdlc-flow.js`/`sdlc-task.js`'s duplicated `renderCheckList`, `harness.schema.json`) and
  confirmed the root cause: `testDepth: "fast"` filters to `gates: true` checks, not to a cheap
  subset, and `engine-core`'s 25 integration-test binaries each relink the whole crate regardless
  of whether `cargo test` is filtered. The user then reported the *same* slowdown hit an ad hoc
  `cargo test engine-core` during an earlier review session — confirming a package-scoped filtered
  invocation still builds every target before filtering, so the review tool's own verification
  step pays the same cost the SDLC per-task loop does. Wrote and registered a ticket in
  `base-template` (`planning/ticket-per-task-fast-checks/{tasks.md,tasks.json}`,
  `BT.ticket.per-task-fast-checks`, wave 26): optional `perTask`/`fastCommand` fields on the check
  schema (default-preserving), wired through both engines' `renderCheckList` + harness-config
  loader, plus a default `"perTask": false"` on the Rust/Next.js `build` checks in
  `harness.examples.md` so new projects get the safe win for free. That ticket is now running via
  `/sdlc-task` in `base-template`, independently of this repo. In parallel, applied a local,
  base-template-independent mitigation: `[profile.dev]` in the workspace root `Cargo.toml`
  (`debug = "line-tables-only"` + `split-debuginfo = "unpacked"`), which cuts per-binary link cost
  for any `cargo test` invocation (the `test` profile inherits `dev`) regardless of command shape,
  without touching `cargo build --release` or `harness.json`.
- **Why:** The `EN.7.D` review that should have taken minutes took roughly an hour across two
  separate attempts (the earlier `/sdlc-flow` run and, per the user, an ad hoc review-time
  `cargo test engine-core`), and this project's test suite will only keep growing — this needed a
  real fix, not a one-off workaround, and the fix belongs in `base-template` since every downstream
  project using this harness will eventually hit the same wall as its own suite grows.
- **Refs:** `base-template/planning/ticket-per-task-fast-checks/tasks.md`, carryover
  `harness-per-task-relinks-all-test-binaries` (updated, now points at the real ticket).
- **Verdict:** Ticket written and running in `base-template`; `Cargo.toml` change applied locally,
  uncommitted. `EN.7.D`'s review/PR is still the open item — untouched this session.
- **Next:** Finish the `EN.7.D` code review and open its PR; decide whether the `Cargo.toml` change
  rides in that PR or its own; once `BT.ticket.per-task-fast-checks` lands, sync it down and edit
  this repo's `planning/harness.json` (`test.fastCommand` + `build.perTask: false`).

---

## [run: 2026-07-28]

### `EN.7.D` implemented inline after the sdlc-flow proved too slow — learning-artifact materialization, plus a masked production bug
- **What:** Authored the `EN.7.D` spec (`/generate-tasks`, 9 tasks) from the brain program plan `core/planning/mev-write-loop-master-plan.md` § Phase 4 `EN.4.A` (program letters do not match local block ids). A `/sdlc-flow` run got through tasks 1–3 in ~an hour, because `testDepth: "fast"` means "only `gates: true` checks" (`sdlc-flow.js:1161`, `:527`) and all four of this repo's checks are gating — so every per-task tripwire ran the full suite plus a release build. Drove tasks 4–9 inline instead: targeted tests per task, full gate paid once at the end. Landed the materialize tail `DigestRenderNode -> LearningArtifactPayloadNode -> MaterializeDocNode -> PersistToBrainNode -> ActionDispatchNode` (materialize deliberately BEFORE the Synapse push, which halts the run on a non-2xx), a `materialize {enabled, corpus_root, write}` policy knob resolving through all four layers and stated in all three named profiles, a `with_enabled` in-place no-op on the shared node so the declared node set never varies by policy, and `build_learning_artifact_payload` extracted from `PersistToBrainNode` so the written document and the ingested payload cannot drift. Full gate green: 38 suites / 1168 tests / 0 failures, plus fmt, clippy `-D warnings`, and `build --release`; `okf-core` and `mev` trees clean. An interrupted `/code-review --fix` left two fixes behind, both verified and committed as `d1a8787` — the important one being `ensure_plan_parents()`: `mev`'s `apply_plan` calls `std::fs::write` and never creates directories, so a first-ever write into a brain root lacking `docs/content/learning-corpus/` failed and halted the run. That directory does not exist in the real corpus, so the first real run would have hit it; every e2e test pre-created it (copying `opportunity_loop_e2e.rs`), which masked it. Added a regression test that deliberately does not pre-create the subtree, confirmed failing without the fix.
- **Why:** `EN.7.D` is the block that proves the `MaterializeDocNode` writer is generic rather than an opportunity-specific tool — a generic abstraction with one live instance is unproven. It cost a model string and a source node: zero edits to `okf-core`, `mev`, or the seam's model dispatch, which already carried `"learning-artifact"` from `EN.7.A`. The inline run was chosen over restarting the flow because the 9 tasks form one causal chain (policy -> payload -> graph -> e2e) that one context holds comfortably, and because the per-task full-suite cost bought little when tasks 1–6 are only meaningfully verifiable by the task-7 e2e anyway.
- **Refs:** `planning/EN.7.D-learning-artifact-materialization/tasks.md`, `core/planning/mev-write-loop-master-plan.md` (Phase 4 `EN.4.A`), D53, commits `e4305ee`..`d1a8787`.
- **Verdict:** All 9 tasks green and fully gated. Code review NOT completed (stopped early); no PR opened yet.
- **Next:** `/code-review` the branch and open the PR; implement the `perTask`/`fastCommand` harness-schema fields in base-template so per-task tripwires stop relinking all 25 integration-test binaries; decide the `ENGINE_BRAIN_ROOT` deployment question before any served content run.

---


### `EN.4.F-locale-rate-card` done — locale threaded through the diagnostic funnel, pricing off a firewalled two-sheet rate card
- **What:** Resumed `/sdlc-flow EN.4.F-locale-rate-card` on branch `EN.4.F-locale-rate-card-flow` after two prior bails on the `MoneyRange` string-vs-struct mismatch, and ran tasks 1–10 through to a PASS review. Task 4 resolved the mismatch by making `Investment` a `MoneyRange` type alias with `authored_locale` stamped on `AutomationRoadmap`. Task 5 fixed the real bug behind the earlier bails: `ProposalWriterNode`/`ProposalReviseNode` had been merging a redundant sibling `"locale"` key into `ctx.nodes[NODE_NAME]`, which `PersistToBrainNode`'s strict re-parse through `AutomationRoadmap` silently dropped, breaking the round trip the e2e tests asserted on — removing the extra key and relying solely on `authored_locale` fixed it (mirrored onto `ProposalReviseNode`, outside the task's declared files, since a reviewer-rejected draft would otherwise lose its locale stamp and price). Task 6 spliced a new `language_directive(Locale)` helper into `CompanyResearchNode`/`ProspectingResearchNode`/`IntakeExtractNode` prompt bodies while keeping each `STABLE_SYSTEM_PROMPT` byte-identical across locales. Task 7 added a currency-aware `format_money` helper to `PersistToBrainNode`'s plain-language rendering. Task 8 added a hermetic `locale_rate_card.rs` e2e suite (per-locale pricing, no cross-sheet leakage, floor compliance, byte-identical prompts, a real firewall grep guard). Task 9 documented the block across `data-contract.md`/`proposal-generator-workflow.md`/`research-agent-workflow.md`/`diagnostic-intake-workflow.md`/`architecture.md`/`index.md`, explicitly noting the `investment`-shape change is not a Pinned Contract Version bump since `AutomationRoadmap` is opaque to the canonical contract. Task 10 validated the full suite (fmt, clippy `-D warnings`, `cargo test`, `cargo build --release`) green with no further code changes. Notable decisions: `Locale`'s `PtBr` default uses `#[derive(Default)]`/`#[default]` per `clippy::derivable_impls` rather than a manual `impl Default`; `hourly_floor` stayed a plain `f64` (internal scoping guidance, not a client-facing engagement, so no `MoneyRange`); `RateSheet::validate()` enforces the firewall invariant that every `MoneyRange`'s currency matches its sheet's currency.
- **Verdict:** PASS review, all 10 tasks green.
- **Next:** `EN.4.D` (DELIVERABLE_RENDER, roadmap → PDF) is now unblocked; otherwise proceed per `master-plan.md` sequence (`EN.5.B1` eval slice runner, or the Phase 6 omni-channel skeletons).

```
642b6a3 feat: implement EN.4.F-locale-rate-card-task9
becda15 feat: implement EN.4.F-locale-rate-card-task8
9aaf5ee feat: implement EN.4.F-locale-rate-card-task7
b80afa9 feat: implement EN.4.F-locale-rate-card-task6
a6a7a0d fix: fix pass 1 for EN.4.F-locale-rate-card-task5
2d39b74 feat: implement EN.4.F-locale-rate-card-task5
e104ccc fix: drop stale string investment field from proposal_generator_e2e fixture
317daff chore: wrap up EN.4.F-locale-rate-card (attempt 2 bail)
```

### `EN.4.F-locale-rate-card` BAILED (attempt 2) — MoneyRange string-vs-struct mismatch persists in proposal_generator_e2e, tasks 5–10 still not attempted
- **What:** Continued `/sdlc-flow EN.4.F-locale-rate-card` on branch `EN.4.F-locale-rate-card-flow` after the prior wrap-up. A first fix pass (`18d240e`) dropped the stale string `investment` field from the `graph.rs`/`revise.rs` test fixtures, resolving the earlier `loop_behavior` assertion failures at lines 668/636. A second fix pass (`978a838`, "fix pass 1 for task4") targeted the remaining failures directly but hit the same root cause as attempt 1 with no progress: `MoneyRange` deserialization expects a structured `{min, max}` object, but the writer/model still emits a plain locale-formatted string (e.g. `"R$8,000-12,000 fixed fee"`) in `proposal_generator_e2e`. This is a structural data-contract defect — schema and producer disagree on the wire shape — not a flaky or incrementally-closing test failure, so the run bailed again rather than attempting a third pass on the same approach. Tasks 1–3 remain the only passed work; tasks 4 (partial) through 10 remain outstanding.
- **Why it bailed:** Same root cause recurring across attempts 1 and 2 with no progress: `MoneyRange` deserialization mismatch — tests/schema expect a structured `{min, max}` object but the writer/model emits a plain string like `"R$8,000-12,000 fixed fee"`. This is a structural data-contract defect (schema vs. producer disagreement) in `proposal_generator_e2e`, not a flaky or incrementally-closing test failure.
- **Decisions:** None new this pass beyond what attempt 1 already recorded (`Investment` as a `MoneyRange` type alias, `hourly_floor` as a plain `f64`); the `graph.rs`/`revise.rs` fixture fix (`18d240e`) removed the stale string `investment` field rather than reconciling it with the new structured shape, since those fixtures are outside task 4's owned files.
- **Next:** Before resuming EN.4.F, resolve the `MoneyRange` wire-format question at the schema/prompt level — either add a custom `Deserialize` for `MoneyRange` that accepts a plain locale-formatted string in addition to the structured form, or tighten `ProposalWriterNode`'s prompt/JSON schema so the model is forced to emit structured `{min, max, currency}` for `investment` — then re-run tasks 4–10.

```
978a838 fix: fix pass 1 for EN.4.F-locale-rate-card-task4
18d240e fix: drop stale string investment field from graph.rs/revise.rs test fixtures
b29f571 chore: wrap up EN.4.F-locale-rate-card
4e7995a feat: implement EN.4.F-locale-rate-card-task4
feb6c89 feat: implement EN.4.F-locale-rate-card-task3
```

### `EN.4.F-locale-rate-card` BAILED — MoneyRange/schema contract mismatch on Task 4, tasks 5–10 not attempted
- **What:** Ran `/sdlc-flow EN.4.F-locale-rate-card` (branch `EN.4.F-locale-rate-card-flow`). Tasks 1–3 passed cleanly: Task 1 added `Locale`/`Currency` (`locale.rs`) with BCP-47 wire tags, default pt-BR, `currency()`/`language_name()` helpers, 7 unit tests. Task 2 added the two-sheet firewalled `RateCard` (`EngagementKind`, `EngagementBasis`, `MoneyRange`, `RateSheet`, `RateCard`) with a strict `harness.json`-backed loader (`RateCard::load_from`) that defaults on an absent section but errors on malformed or currency-mismatched data, plus a `rate_card` section in `planning/harness.json` carrying the ported `rates.md` numbers. Task 3 added a `#[serde(default)]` `locale: Locale` field (default pt-BR) to all three event schemas (`ResearchAgentEventSchema`, `DiagnosticIntakeEventSchema`, `ProposalGeneratorEventSchema`), deliberately kept off the policy surface, with 9 new unit tests plus 12 collateral struct-literal fixes across other workflow files required for compilation. Task 4 (structured `Investment`/`authored_locale` on `AutomationRoadmap`) was implemented as a type alias `Investment = MoneyRange`, but hit a recurring, non-progressing failure across 2 attempts: the deliverable schema expects `MoneyRange` to deserialize from a structured `{min, max}` shape, but the writer model/upstream fixture still returns a plain string (e.g. `"R$8,000-12,000 fixed fee"`), and the same `graph.rs` `loop_behavior` assertion failures recurred at lines 668/636 with no delta between attempt 1 and attempt 2 — 14 failures each time, same modules. This indicates a structural contract mismatch between the deliverable JSON schema and what the model/writer prompt actually emits (a schema/prompt design gap), not a bounded code fix, so the run bailed rather than burning further attempts. Tasks 5–10 (writer/revise/persist/graph wiring, docs) were never attempted.
- **Why it bailed:** Same failure recurring across attempts (14 failures, same modules) — `MoneyRange` deserialization expects a struct but the model/upstream returns a plain string, plus the same `graph.rs` `loop_behavior` assertion failures — no progress between attempts, indicating the fix needs schema/prompt-level design work (e.g. a custom deserializer accepting both string and structured forms, or tightening the writer's schema/prompt to force structured min/max output) rather than another attempt at the same approach.
- **Decisions:** `Investment` modeled as a type alias over `locale::MoneyRange` rather than a duplicate struct; `hourly_floor` kept as a plain `f64` (not a `MoneyRange`) since it's internal scoping guidance rather than a client-facing engagement.
- **Next:** Before resuming EN.4.F, resolve the MoneyRange wire-format question — either add a custom `Deserialize` for `MoneyRange` that accepts a plain locale-formatted string (parsing `"R$8,000-12,000 fixed fee"`-style text) in addition to the structured form, or tighten `ProposalWriterNode`'s prompt/JSON schema so the model is forced to emit structured `{min, max, currency}` for `investment` — then re-run tasks 4–10.

### `EN.4.E-contact-enrichment` done — RESEARCH_AGENT extracts reachable contacts and lifts source links into the materialized opportunity
- **What:** Ran `/sdlc-flow EN.4.E-contact-enrichment` (branch `EN.4.E-contact-enrichment-flow`); all 11 tasks passed. Task 1 gave `Opportunity::from_company_brief`/`from_prospecting_result` (okf-core, committed separately in that repo as `b41c462`) order-stable/deduped lifting of `company_url`→`url` and `sources[]`→`links[]`, via new `json_str_opt`/`json_str_array_deduped` helpers. Task 2 added `ResearchContact` (`{name, role, emails[], whatsapp[], phones[], links[], note}`, field-for-field with okf-core's `Contact`) plus `contacts[]`/`company_url` on `CompanyBrief`/`ProspectLead`, with a shared invariant JSON sub-schema never listed in `required`. Task 3 added a `contact_enrichment` policy knob (`{research, prospect, max_fetches}`, depths `off`/`standard`/`deep`) to `ResearchAgentPolicy`, wired through all four resolution layers, with the three named profiles set (`baseline` standard/standard/4, `cheap-fast` off/off/0, `thorough` deep/standard/8) and documented in `harness.json`. Task 4 gave `CompanyResearchNode`'s prompt a policy-driven acquisition + anti-fabrication directive (contact/about/team page, footer, `mailto:`/`wa.me` links, plus LinkedIn/Instagram/Facebook and a named-decision-maker ask at `deep`), deterministically stamped the trigger's `company_url` onto the parsed brief (event always wins over the model), and stamped the resolved depth into node telemetry, with `STABLE_SYSTEM_PROMPT` staying byte-identical across depths. Task 5 gave `ProspectingResearchNode` the equivalent per-lead directive — one cheap attempt per identifiable business, explicit skip-pseudonymous-individuals rule. Task 6 added `OpportunityEdit::MergeContacts` to the `DocMaterializer` seam, routed to `mev::doc::opportunity::plan_merge_contacts` with unknown-slug handled as an Ok+diagnostic outcome (not a hard failure), and replaced the stale "not shipped" doc comment this block was named in. Task 7 added `MergeContactsNode`, the new terminal node collecting a run's company/prospecting contacts and merging them via the seam, with a clean no-op when zero contacts are found (mirroring mev's `detect_kind`/title-derivation rules locally since they're private to mev's crate). Task 8 wired the `RESEARCH_AGENT` declared graph to terminate in the shared `MergeContactsNode`, reached from `MaterializeDocNode` on both branches, registered via engine-serve's existing `registry_for_policy` delegation, and updated the EN.7.B `opportunity_loop_e2e.rs` fixture's hand-built registry to keep passing `WorkflowValidator`. Task 9 added a hermetic 8-test e2e suite (`research_agent_contacts_e2e.rs`) proving the real `Workflow::run` end-to-end — company/prospecting contact lifting, byte-idempotent re-runs, zero-contact success paths, the `cheap-fast`/`off` no-op path, and contact-channel unioning via mev's real `plan_merge_contacts` — all reconstructed through okf-core's real frontmatter parser. Task 10 updated `docs/research-agent-workflow.md`, `docs/materialize-doc-node.md`, `docs/architecture.md`, and `docs/index.md` to document the five-node graph, the `ResearchContact` shape, the `contact_enrichment` knob's per-profile table, the anti-fabrication contract, and the two-step ingest→merge write. Task 11 (validation-only) confirmed all 8 Validation Commands green (engine-rs fmt/clippy/test/build-release; okf-core fmt/clippy/test; mev test) with both sibling repos left clean.
- **Why:** Closes the merge-contacts gap `EN.7.B` explicitly parked (`doc_materializer.rs`'s comment named this block as the driver), turning the previously hand-run `mev doc opportunity merge-contacts` step into part of the automated `RESEARCH_AGENT` workflow — giving downstream outreach work (`EN.6.H`) a real contact channel to dispatch against.
- **Verdict:** PASS (review found no findings). Docs: `docs/research-agent-workflow.md`, `docs/materialize-doc-node.md`, `docs/architecture.md`, `docs/index.md` updated.
- **Status:** `planning/state.json` block `EN.4.E` set to `"closed"`; `planning/status.md` flipped `EN.4.E` to Done and refreshed Momentum/Current focus.
- **Next:** EN.6.C/D (Slack, Telegram/WhatsApp adapter skeletons — need detailing before scheduling), EN.4.D (DELIVERABLE_RENDER, net-new), or EN.7.C (Materialize→harvest gate — in-process vs human-approval → Synapse ingest), independently available.

```
d79dc1f feat: implement EN.4.E-contact-enrichment-task10
ddddac2 feat: implement EN.4.E-contact-enrichment-task9
662a6e5 feat: implement EN.4.E-contact-enrichment-task8
a967fd3 feat: implement EN.4.E-contact-enrichment-task7
bc4cfac feat: implement EN.4.E-contact-enrichment-task6
d9866b2 feat: implement EN.4.E-contact-enrichment-task5
4579130 feat: implement EN.4.E-contact-enrichment-task4
0a00b69 feat: implement EN.4.E-contact-enrichment-task3
```

## [run: 2026-07-27]

### `EN.7.B-research-opportunity-loop` done — RESEARCH_AGENT terminates in MaterializeDocNode, set-stage/add-action micro-workflows
- **What:** Ran `/sdlc-flow EN.7.B-research-opportunity-loop` (branch `EN.7.B-research-opportunity-loop-flow`); all 9 tasks passed. Task 1 gave `MaterializeDocNode` an ordered `with_source_nodes` upstream-identity preference list (first-present-wins), keeping `with_source_node`'s prior single-identity behavior as a one-element-vec convenience. Task 2 added `edit_opportunity` (set-stage/add-action) to the `DocMaterializer` seam via a new `OpportunityEdit` enum, live-implemented over mev's `plan_set_stage`/`plan_add_action` + `apply_plan` inside `spawn_blocking`, with `StubDocMaterializer` recording edit calls. Task 3 added `OpportunityEditNode` (`nodes/opportunity_edit.rs`), reading its arguments off `ctx.event` via a `require_str` helper and stamping a `MaterializeOutcome`-shaped result under `self.name()`. Task 4 wired `RESEARCH_AGENT`'s declared graph to terminate in a shared `MaterializeDocNode` — both `CompanyResearchNode` and `ProspectingResearchNode` connect to it via the ordered preference list — passing `WorkflowValidator::validate`. Task 5 declared the `OPPORTUNITY_SET_STAGE`/`OPPORTUNITY_ADD_ACTION` single-node micro-workflows (schema/graph/registration), model-free, backed by two distinctly-identified `OpportunityEditNode` instances via `NodeExt::with_identity`. Task 6 registered both as model-free workflows in `engine-serve`'s `register_builtin_workflows`, populating both the workflow and schema registries with no policy resolution. Task 7 added a hermetic `tests/opportunity_loop_e2e.rs` driving the real `Workflow::run` for `RESEARCH_AGENT` (stubbed model transports, real `MevDocMaterializer` against a tempdir) and both new micro-workflows, covering all 7 required scenarios: company/prospecting branches closing the loop with valid Opportunity frontmatter, byte-idempotent research re-runs, set-stage change + idempotent repeat, add-action append + idempotent repeat, invalid-stage failure naming `VALID_STAGES` with the file unchanged, and unknown-slug failure for both edit workflows with no file created — a real hermeticity leak (a stray `research-agent-state.json` landing under the crate directory) was caught and fixed by seeding `SetupWorktreeNode` before finalizing. Task 8 replaced a stray Unicode `⇄` in `docs/data-contract.md`'s frontmatter title with ASCII `<->` to satisfy the `emoji_gate` check (1 fix attempt) and added/updated docs for the closed loop and the two new workflow entry points. Task 9 (validation-only) ran the full gated suite — `fmt`/`clippy -D warnings`/`test`/`build --release` all green with zero stray corpus writes; `cargo check --manifest-path ../bastion/Cargo.toml` fails on a pre-existing, unrelated cross-repo drift (bastion's `brainval/mod.rs:186` calling mev's `emit_state` with a stale 2-arg signature, predating and untouched by this spec) — left unfixed as out of scope for engine-rs, with bastion's incidental `Cargo.lock` diff reverted.
- **Why:** Closes the RESEARCH→opportunity loop per D53's fourth boundary-test channel — a company/prospecting run now *ends* by writing or updating its opportunity file — and gives bastion-web's `BW.7.A` the two `POST /events/` entry points (`OPPORTUNITY_SET_STAGE`/`OPPORTUNITY_ADD_ACTION`) it will trigger for stage/action edits.
- **Verdict:** PASS (review found no findings). Docs: `docs/opportunity-edit-workflows.md` created; `docs/research-agent-workflow.md`, `docs/architecture.md`, `docs/data-contract.md`, `docs/index.md` updated.
- **Status:** `planning/state.json` block `EN.7.B` set to `"closed"`; `planning/status.md` flipped `EN.7.B` to Done and refreshed Momentum.
- **Next:** EN.6.C/D (Slack, Telegram/WhatsApp adapter skeletons — need detailing), EN.4.D (DELIVERABLE_RENDER, net-new), or EN.7.C (Materialize→harvest gate — in-process vs human-approval → Synapse ingest, now unblocked but forward-looking).

```
e2c324e docs: update docs for EN.7.B-research-opportunity-loop
fd64379 fix: fix pass 1 for EN.7.B-research-opportunity-loop-task8
181a073 feat: implement EN.7.B-research-opportunity-loop-task8
e2b7ba1 feat: implement EN.7.B-research-opportunity-loop-task7
5745cce feat: implement EN.7.B-research-opportunity-loop-task6
70e5d48 feat: implement EN.7.B-research-opportunity-loop-task5
4a4172d feat: implement EN.7.B-research-opportunity-loop-task4
c2deeb1 feat: implement EN.7.B-research-opportunity-loop-task3
```

### `EN.7.A-materialize-doc-node` done — MaterializeDocNode, DocMaterializer seam, mev/okf-core link
- **What:** Ran `/sdlc-flow EN.7.A-materialize-doc-node` (branch `EN.7.A-materialize-doc-node-flow`); all 7 tasks passed. Task 1 had `engine-core` declare path dependencies on `mev` and `okf-core`, bridging the edition 2021↔2024 workspace split, with `cargo build --release` and `cargo tree -p engine-core` confirming an acyclic dependency graph. Tasks 2 and 3 were found already fully implemented and committed on this branch before this agent invocation started — brain-root/target-corpus resolution (`brain_root.rs`, `ENGINE_BRAIN_ROOT` env precedence over `mev::brain::config::find_brain_root`, typed `BrainRootError`, 4 unit tests) and the `DocMaterializer` seam (live `mev`-backed impl + recording stub, mirroring `HttpPost`'s trait/live-impl/stub shape, 6 unit tests) — both verified against the tasks spec rather than redone. Task 4 added `MaterializeDocNode` (`nodes/materialize_doc.rs`) implementing `Node` with `with_materializer`/`with_brain_root`/`with_source_node`/`with_write` builders, stamping `{materialized, dry_run, model, paths, warnings}` under its own name, surfacing the first error-severity mev diagnostic as a `NodeError`. Task 5 added hermetic `tests/materialize_doc.rs` driving the real `MevDocMaterializer` against a tempdir corpus — write, idempotency, dry-run, unknown-model, missing-upstream, identity-override, and `ENGINE_BRAIN_ROOT` resolution — plus a copied `CompanyBrief` fixture for hermeticity (idempotency asserted via on-disk bytes rather than the `paths` stamp, since mev's own idempotency guard zero-stamps `paths` on unchanged content). Task 6 added `docs/materialize-doc-node.md`, a new "Injectable Seams" inventory table (`HttpPost`/`ChannelTransport`/`DocMaterializer`) in `architecture.md`, a `docs/index.md` row, and verified `CLAUDE.md`'s THE BOUNDARY TEST block stayed byte-identical to `orchestrator/CLAUDE.md`'s copy. Task 7 ran the full gated suite (fmt/clippy/test/build --release/`cargo tree` acyclicity) plus a bastion cross-repo spot-check, all green with no code changes needed (bastion's incidentally regenerated `Cargo.lock` reverted).
- **Why:** Gives `EN.7.B` (closing the RESEARCH→opportunity loop) a ready-made, generic, identity-overridable writer node that bridges engine-rs to mev/okf-core in-process, per D53's fourth boundary-test channel — engine-rs executes the write, mev/okf-core own the document format.
- **Verdict:** PASS (review found no findings). Docs: `docs/materialize-doc-node.md` created; `docs/architecture.md`, `docs/index.md` updated.
- **Status:** `planning/state.json` block `EN.7.A` set to `"closed"`; `planning/status.md` gained a new Phase 7 section with `EN.7.A` flipped to Done.
- **Next:** `EN.7.B` — close the RESEARCH→opportunity loop + stage/action micro-workflows on top of `MaterializeDocNode`, or independently: `EN.6.C/D` (Slack/Telegram/WhatsApp adapter skeletons, need detailing) or `EN.4.D` (DELIVERABLE_RENDER, net-new).

```
531a8a3 feat: implement EN.7.A-materialize-doc-node-task6
cc97533 feat: implement EN.7.A-materialize-doc-node-task5
b39cb25 feat: implement EN.7.A-materialize-doc-node-task4
e078630 feat: implement EN.7.A-materialize-doc-node-task3
a6eb616 feat: implement EN.7.A-materialize-doc-node-task2
0a183c7 feat: implement EN.7.A-materialize-doc-node-task1
```

### `EN.6.A-egress-dispatch` done — ChannelTransport seam, ActionDispatchNode, workflow-trigger dispatch
- **What:** Ran `/sdlc-flow EN.6.A-egress-dispatch` (branch `EN.6.A-egress-dispatch-flow`); all 7 tasks passed. Task 1 landed the `ChannelTransport` egress seam in `engine-core` — `OutboundAction`/`OutboundBody`/`ChannelSendReceipt` types, the trait, `StubChannelTransport`, and `UnwiredChannelTransport` (mirroring `HttpPost`). Task 2 gave `WorkflowTriggerDispatch` a working POST to `/events/` — `{workflow_type, data}` carrying an `X-API-Key` header (via an additive `HttpPost::post_with_headers`, default-delegating so every existing call site stayed untouched), an 8-hop chain-depth cap, and a preference for an injected in-process `Dispatcher` (fire-and-forget via `tokio::task::spawn_blocking`, since `Workflow::run`'s `OnProgress` isn't `Send` and can't cross an await point inside the Send-bound `ChannelTransport::send`) over the HTTP loopback fallback; `channel_transport_live()` now routes `WorkflowTrigger` through it and every other channel to `UnwiredChannelTransport`. Task 3 added `ActionDispatchNode`, the deterministic terminal `CONTENT_PIPELINE` egress node — a digest reply `OutboundAction` when `reply_context` is present, a `TriggerWorkflow` action when the raw event carries a `trigger` request, sent over the injectable seam, never failing the run on a transport error (`delivered:false` receipt instead), with every stored receipt stamped with the run's `envelope_id`. Task 4 wired `ActionDispatchNode` into the declared graph after `PersistToBrainNode`, added a non-model `dispatch_verbosity` policy stage that never rewires under Local, and gave `ContentPipelineInput` a typed `trigger` field — plus a collateral fix to the pre-existing `content_pipeline_e2e.rs` fixture, which needed `ActionDispatchNode` registered to keep `Workflow::new_validated` satisfied. Task 5 wired `engine-serve`'s `register_content_pipeline` to re-register `ActionDispatchNode` with `channel_transport_live` pointed at a new `ENGINE_EVENTS_URL` env var (falling back to the prior localhost placeholder), updated `harness.json`'s `dispatch_verbosity` default, and brought `architecture.md` §3.3/§5/§7.2 back in sync with the shipped API. Task 6 added the hermetic `action_dispatch_e2e.rs` suite driving the real graph through `PersistToBrainNode → ActionDispatchNode`, covering reply-digest matching, fire-and-forget, `TriggerWorkflow` dispatch (URL/payload/`X-API-Key` via `StubHttpPost::last_call`), chain-depth-cap refusal, unwired-channel `delivered=false` naming EN.6.D, failing-transport resilience, `EventsRow` round-trip with receipts, and Local-profile rewire leaving `ActionDispatchNode` untouched. Task 7 ran the full workspace gate (`fmt`/`clippy -D warnings`/`test --workspace`/`build --release`) clean, with the architecture doc already in sync — no changes needed.
- **Why:** Gives every EN.6.B–D channel adapter a single already-tested `ChannelTransport` seam to implement against, and closes the loop for workflow chaining (a triggered child run can itself trigger further runs, capped and correlated) now that `EN.5.F` made `/events/` non-blocking.
- **Verdict:** PASS (review found no findings). Docs: `docs/content-pipeline-workflow.md`, `docs/index.md` updated.
- **Status:** `planning/state.json` block `EN.6.A` set to `"closed"`; `planning/status.md` Progress Table gained a new Phase 6 section with `EN.6.A` flipped to Done.
- **Next:** EN.6.C/D — Slack/Telegram/WhatsApp adapter skeletons (need detailing before scheduling) now that the `ChannelTransport` seam exists, or EN.4.D — DELIVERABLE_RENDER (net-new, roadmap → PDF), independently available.

```
4bd6865 chore: flow state — docs
0cdd99a docs: update docs for EN.6.A-egress-dispatch
26f4633 feat: implement EN.6.A-egress-dispatch-task6
24c969e feat: implement EN.6.A-egress-dispatch-task5
befc20a feat: implement EN.6.A-egress-dispatch-task4
c9d7746 feat: implement EN.6.A-egress-dispatch-task3
4c2bf3f feat: implement EN.6.A-egress-dispatch-task2
40a7a82 feat: implement EN.6.A-egress-dispatch-task1
```

## [run: 2026-07-27]

### `EN.5.F-async-run-lifecycle` done — non-blocking trigger, run readback, SSE progress stream
- **What:** Ran `/sdlc-flow EN.5.F-async-run-lifecycle` (branch `EN.5.F-async-run-lifecycle-flow`); all 7 tasks passed. Task 1 gave `LiveStateStore` bounded terminal-run retention — `mark_terminal`/`get_record` move a finished run out of the live map into a 100-entry completed ring (carrying `workflow_type`/`created_at`/`updated_at`), while `record`/`get`/`list_active`/`remove` kept their exact prior signatures so `http.rs`/`abort.rs` and bastion's `GET /api/runs/{id}` projection stayed unchanged. Task 2 flipped `POST /events/` from awaiting the run inline to spawning it via `actix_web::rt::spawn` (the current-thread arbiter, required because `engine_core::workflow::OnProgress` carries no `Send` bound) and returning `202 {run_id, event_id}` immediately — seeded with a default `Budget` read from `ENGINE_RUN_MAX_COST_USD`/`ENGINE_RUN_MAX_TOKENS` via a memoized `OnceLock` helper rather than a new `AppState` field (bastion constructs `AppState` as a struct literal over an unpinned path dep, so any new field is a cross-repo compile break), with the spawned task marking the run terminal and deregistering the `RunRegistry` token on every exit path; the `500` failure arm is gone entirely. Task 3 added the canonical `GET /events/{event_id}` readback — `{event_id, workflow_type, status, created_at, updated_at, task_context}`, server-derived status (`running`/`succeeded`/`cancelled`/`budget_halted`/`failed`), `X-API-Key` gated, 404 on unknown/malformed ids, served DB-free from `LiveStateStore` plus a module-local live-run-metadata side table for still-running workflow_type/created_at. Task 4 added `crates/engine-serve/src/stream.rs` and `GET /events/{event_id}/stream` — a per-run `tokio::sync::broadcast` tee with a terminal-frame cache so a late subscriber still gets one terminal frame, wired as a third fan-out inside `post_events`'s existing `on_progress` closure. Task 5 wrote the hermetic `tests/async_lifecycle.rs` (5 tests) proving the block's acceptance surface end to end: sub-100ms 202 against a slow node, readback's running→terminal transition, one SSE frame per node transition plus a terminal frame, abort of a spawned run reading back cancelled, and a run exceeding the $5 default budget halting with the budget marker. Task 6 updated `docs/data-contract.md` — the readback and stream routes marked ported/extension, the 500-removal semantic change and default-budget env knobs documented, a dated changelog row added with the Pinned Contract Version held at 1.3.0. Task 7 validated the full suite — `fmt`/`clippy -D warnings`/`cargo test` (91+ tests across `engine-serve`/`engine-store`)/`cargo build --release`, plus `cargo check --manifest-path ../bastion/Cargo.toml`, confirming `AppState` gained no public field and bastion's tree stayed clean after reverting the incidental `Cargo.lock` regeneration from the new `futures` workspace dep.
- **Why:** Closes the last of the four EN.5.D/E/A/F Phase-5 substrate blocks — `EN.6.A` (egress seam: `ChannelTransport` + `ActionDispatchNode` + workflow-trigger dispatch) and the whole Phase 6 channel/adapter fan-out behind it were blocked on a non-blocking trigger path, since a Slack ACK needs ~3s and a pipeline run can take minutes.
- **Verdict:** PASS (review found no findings). Docs: `docs/data-contract.md` updated.
- **Status:** `planning/state.json` block `EN.5.F` set to `"closed"`; `planning/status.md` Progress Table row added under Phase 5 and flipped to Done. `EN.6.A` is now unblocked.
- **Next:** EN.4.D — DELIVERABLE_RENDER (net-new, roadmap → PDF), or EN.6.A — the egress seam now that EN.5.D/E/A/F are all done.

```
c8e861e docs: update docs for EN.5.F-async-run-lifecycle
f7c6ab1 feat: implement EN.5.F-async-run-lifecycle-task6
e0705d7 chore: flow state — task 5 passed
fb59383 feat: implement EN.5.F-async-run-lifecycle-task5
6fb8f63 feat: implement EN.5.F-async-run-lifecycle-task4
27cfcbf feat: implement EN.5.F-async-run-lifecycle-task3
afb5ea9 feat: implement EN.5.F-async-run-lifecycle-task2
574f69d feat: implement EN.5.F-async-run-lifecycle-task1
```

## [run: 2026-07-26]

### `EN.5.A-content-pipeline` done — envelope-based content core (bounded self-critic loop, translate, digest, PersistToBrain)
- **What:** Ran `/sdlc-flow EN.5.A-content-pipeline` (branch `EN.5.A-content-pipeline-flow`); all 14 tasks passed. Task 1 added the channel-agnostic `IngressEnvelope`/`SourcePayload`/`ChannelType` (including `Web`/`Tui`/`Schedule`/`Api`) contract to `engine-contract` with full round-trip coverage. Task 2 scaffolded the `content_pipeline` workflow module and its schema. Task 3 built the EN.4.0-framework policy surface (`baseline`/`local-drafting`/`fast-summarize` profiles on EN.5.D's worktree-independent `PolicyConfigSource`), rejecting out-of-range `max_critic_iterations`/`critic_confidence_threshold` rather than silently clamping. Task 4 wrote `SourceRouterNode`, routing purely on `SourcePayload` kind (`Url`/`VideoId`/everything else) per the 2026-07-25 architecture amendment that collapses 8+ channels onto three branches. Task 5 added the three fetch/normalize nodes converging on one `{title, text, source_ref}` shape. Task 6 added `SummarizeNode`. Tasks 7–9 built the bounded self-critic loop — `SelfCriticNode`, then `CriticRouterNode`/`IncrementCriticIterationNode` hand-rolled (not the EN.5.E `build_loop` combinator, whose static graph-build-time cap can't express a per-run policy-resolved bound) with the iteration cap pinned to fire at exactly N critic passes not N+1, then `ReviseNode` closing the back-edge — all wired via EN.5.E's `InputBinding`. Task 10 added `TranslateSkipRouterNode`/`TranslateNode` (pt-BR default). Task 11 added `DigestRenderNode` (UUID-v5 `artifact_id` derived from `envelope_id` for webhook-retry idempotency) and `PersistToBrainNode` POSTing a `LearningArtifact` to Synapse's ingest endpoint over the EN.4.C `HttpPost` seam. Task 12 assembled the declared `CONTENT_PIPELINE` `WorkflowSchema`/registry, registered it in engine-serve, and added `harness.json` defaults. Task 13 added the hermetic e2e suite driving the real `Workflow::run` walk loop across every branch — and in doing so discovered and fixed a real framework bug in `workflow.rs::run_with`: router dispatch was computed on the pre-process `ctx`, so a self-referential router (`SourceRouterNode`) whose own `process()` writes what its own `route()` reads always saw `None` on first entry, halting the walk after one node; moved dispatch to post-process `ctx`, verified behavior-preserving workspace-wide. Task 14 validated the full suite (fixing two residual pre-existing clippy failures in `proposal_generator/graph.rs` along the way) and patched an `architecture.md` doc gap (the `ChannelType` sample was missing the Web/Tui/Schedule/Api variants).
- **Why:** Closes `EN.5.A`, the content core Synapse's `CONTENT_PIPELINE` divested to engine-rs per D50/D51 — one graph now serves web articles, YouTube transcripts, and every Phase 6 channel (Slack/Telegram/WhatsApp/Email/ResearchAgent/workflow triggers) through the same `SourcePayload`-kind routing, with the write boundary enforced at `PersistToBrainNode`'s HTTP seam (no embedding/pgvector in this repo, per D51). This is the last of the three Phase-5 content/composition/lifecycle blocks alongside `EN.5.D`/`EN.5.E` that the 2026-07-25 architecture review split out.
- **Verdict:** PASS (review found no findings). Docs: `docs/architecture.md` (`ChannelType` sample) updated.
- **Status:** `planning/state.json` block `EN.5.A` set to `"closed"`; `planning/status.md` Progress Table row added under Phase 5 and flipped to Done. `EN.5.F` (async run lifecycle) is the one remaining infrastructure block still gating `EN.6.A`.
- **Next:** EN.4.D — DELIVERABLE_RENDER (net-new, roadmap → PDF), or EN.5.F — async run lifecycle (non-blocking POST /events/, GET /events/{event_id} readback, SSE progress stream) ahead of EN.6.A.

```
21e2219 docs: update docs for EN.5.A-content-pipeline
0547285 feat: implement EN.5.A-content-pipeline-task14
f43e253 feat: implement EN.5.A-content-pipeline-task13
dfe196b feat: implement EN.5.A-content-pipeline-task12
68ea3be feat: implement EN.5.A-content-pipeline-task11
3a2ed89 feat: implement EN.5.A-content-pipeline-task10
ede45dc feat: implement EN.5.A-content-pipeline-task9
8dc3aa9 feat: implement EN.5.A-content-pipeline-task8
```

### `EN.5.E-composition-primitives` done — instance identities, input bindings, bounded-loop combinator, Dispatcher into engine-core
- **What:** Ran `/sdlc-flow EN.5.E-composition-primitives` (branch `EN.5.E-composition-primitives-flow`); all 6 tasks passed. Task 1 added instance-backed node identity — a delegating `Identified<N>` wrapper constructed via a blanket `NodeExt::with_identity` extension method (not a per-struct field), so none of the 43 existing `impl Node` blocks needed editing — and declarative input bindings (`InputBinding`, `with_input_from`, `WithInput<N>`), both exported from `engine-core`'s `lib.rs` with full unit coverage. Task 2 added `crates/engine-core/src/loop_combinator.rs` — `build_loop(LoopSpec)` emitting a `{guard router, increment node, back-edge}` cluster with both nodes real `Router` impls (so `WorkflowValidator`'s DFS cycle-skip covers both hops of the back-edge per D42), covered by 5 unit tests. Task 3 moved `Dispatcher`/`DispatchError`/`WorkflowFactory` verbatim into `engine-core::dispatch` (with all 8 unit tests), reducing `engine-serve::dispatch` to a re-export so every existing import site — the bastion path dep, `http.rs`, `workflows.rs`, `dispatch_integration`/`abort_integration`, and the four engine-core e2e tests — kept resolving unchanged. Task 4 rebuilt `PROPOSAL_GENERATOR`'s review→revise cluster on the loop combinator (bounded back-edge revise→review, capped at 3 iterations, exits on a `pass` verdict routed straight to `PersistToBrainNode`), with `revise.rs`/`review_router.rs` reading their upstream/downstream via `InputBinding` instead of cross-module `NODE_NAME` consts, while `proposal_generator_e2e.rs` stayed byte-identical and green; `graph::revise_loop_cluster()` was made `pub` so `tests/policy_dispatch_e2e.rs`'s hermetic registry could register the same guard/increment nodes. Task 5 added a hermetic `crates/engine-core/tests/composition.rs` proving duplicate node identities with independent input bindings, an identity-overridden router + loop-combinator cluster validating clean, and in-process sub-workflow dispatch via `engine_core::dispatch::Dispatcher` with zero HTTP calls — driven through the real `Workflow` runner (using `futures::executor::block_on` for the inner sub-workflow run to sidestep a non-`Send` `OnProgress` future). Task 6 validated the full suite — `fmt --check`, `clippy -D warnings`, `cargo test` (workspace), `cargo build --release`, plus the spec's spot-check binaries (composition, proposal_generator_e2e, validator, engine-serve) — all green, and confirmed `proposal_generator_e2e.rs`, `sdlc_flow/`, `engine-contract`, and `router.rs` are byte-identical to the block's base commit.
- **Why:** Closes the second of the three EN.5.D/E/F substrate blocks `EN.5.A` (CONTENT_PIPELINE) was blocked on — nodes can now appear twice in one graph under independent instance identities, bind their inputs declaratively instead of hardcoded `NODE_NAME` consts, compose a bounded critic/revise loop from a spec instead of hand-writing one, and dispatch a sub-workflow in-process rather than over HTTP. `EN.5.A`'s `SourceRouterNode` and critic loop are specified directly on top of this block's `with_input_from` and combinator surfaces; `EN.6.C`'s fan-out depends on its instance identities.
- **Verdict:** PASS (review found no findings). Docs: `docs/architecture.md`, `docs/proposal-generator-workflow.md` updated.
- **Status:** `planning/state.json` block `EN.5.E` set to `"closed"`; `planning/status.md` Progress Table row added under Phase 5 and flipped to Done. `EN.5.A` is now unblocked (both EN.5.D and EN.5.E are closed); `EN.5.F` is the one remaining infrastructure block, gating `EN.6.A`.
- **Next:** EN.4.D — DELIVERABLE_RENDER (net-new, roadmap → PDF), EN.5.A — CONTENT_PIPELINE (now unblocked), or EN.5.F — async run lifecycle (non-blocking POST /events/, SSE progress stream) ahead of EN.6.A.

```
c903b14 docs: update docs for EN.5.E-composition-primitives
8be0f6f feat: implement EN.5.E-composition-primitives-task5
f0211a4 feat: implement EN.5.E-composition-primitives-task4
0645d51 feat: implement EN.5.E-composition-primitives-task3
9161fc5 feat: implement EN.5.E-composition-primitives-task2
f0ca0a4 feat: implement EN.5.E-composition-primitives-task1
d162e8b test: add happy-path fixture for sdlc-flow experiment harness
5053fec chore: wrap up EN.5.D-policy-dispatch-seam
```

## [run: 2026-07-25]

### `EN.5.D-policy-dispatch-seam` done — policy-aware WorkflowFactory, resolve-once, derived Overlay, observed tiers
- **What:** Ran `/sdlc-flow EN.5.D-policy-dispatch-seam` (branch `EN.5.D-policy-dispatch-seam-flow`); all 12 tasks passed. Task 1 added `policy::overlay` (an `Overlay` trait + `merge_opt`/`PartialLocalConfig`/`merge_local`) as the shared merge surface. Task 2 migrated all four workflows' `policy.rs` modules (`sdlc_flow`, `research_agent`, `diagnostic_intake`, `proposal_generator`) onto it, deleting every hand-written `merge_opt`/`merge_local`/`apply_override`. Task 3 added `PolicyConfigSource` (Worktree/HarnessFile/Builtin) decoupling `harness.json` lookup from a worktree path, plus `resolved_policy_strict` — an error instead of a silent `Default` on an absent/unparsable stamp. Task 4 gave each workflow a `resolve_policy_for_run_from(ctx, &PolicyConfigSource)` built on task 3, with the existing worktree-taking function reduced to a thin wrapper. Task 5 made `WorkflowFactory` event-aware: `dispatch_with_event(workflow_type, &Value)` returns `Result<Workflow, String>`, with a new `DispatchError::PolicyResolutionFailed` distinct from `UnknownWorkflowType`. Task 6 added `Workflow::with_seeded_nodes` so a dispatch-resolved policy stamp is visible to a run's first node. Task 7 wired all four `register_*` factories in engine-serve to resolve policy once at dispatch time via `resolve_policy_for_run_from` + `registry_for_policy` + a seeded `RESOLVED_POLICY_IDENTITY` stamp, with `POST /events/` now dispatching via `dispatch_with_event` and returning a 4xx naming the offending profile on resolution failure. Task 8 migrated every node across all four workflows off per-node `resolve_policy_for_run` calls onto the single strict stamped read, deleting the lenient `resolved_policy` entirely and updating 7 integration-test fixture files whose hand-rolled setup previously relied on per-node resolution or the deleted default. Task 9 made `ClaudeCodeStep` stamp `ctx.nodes[stage]["transport"] = {tier, model, endpoint}` on every run, with `openai_compat_transport` gaining a `_meta_transport` variant that records `"local"`+endpoint on success and the cloud-fallback tier with no endpoint on any local-side failure. Task 10 derived `RunTelemetry.model_tier_used` from those stamped transport tiers (overriding caller-supplied intent), and made `Workflow::run_with` stamp a workflow-agnostic `RunTelemetry` into `ctx.metadata` at every exit path — success, cancelled, and budget-halted. Task 11 added a hermetic `policy_dispatch_e2e.rs` suite (4 tests: profile-driven local dispatch, unknown-profile 4xx, no-worktree resolution, RunTelemetry/EventsRow round-trip) plus decision `D11-policy-dispatch-seam.md` and doc updates (`docs/architecture.md`, `docs/sdlc-flow-policy.md`). Task 12 validated the full suite (fmt/clippy/test/release-build) green with no code changes needed.
- **Why:** Closes the structural gap the 2026-07-25 architecture review found blocking `EN.5.A`: the local-model swap (`graph::registry_for_policy`) had no production caller because `WorkflowFactory` was a zero-argument closure that could not see the resolved policy, `resolve_policy_for_run` required a worktree path a channel envelope does not have, policy resolved per-node from disk with a silent `Default` fallback on parse failure, and the merge boilerplate was duplicated verbatim across all four workflow policy modules. This block makes the local-model swap reachable from `POST /events/`, resolves policy once per run, fails loudly on an unknown profile, and makes telemetry record the transport a stage *actually called* rather than the tier it intended to call — closing the gap between `EN.4.0`'s policy framework and what a served run does.
- **Verdict:** PASS (review found no findings). Docs: `docs/architecture.md`, `docs/sdlc-flow-policy.md` updated; decision `D11-policy-dispatch-seam.md` recorded and indexed.
- **Status:** `planning/state.json` block `EN.5.D` set to `"closed"`; `planning/status.md` Progress Table gained a new Phase 5 section with the `EN.5.D` row, flipped to Done.
- **Next:** EN.4.D — DELIVERABLE_RENDER (net-new, roadmap → PDF), then EN.5.E / EN.5.F — the remaining infrastructure pair still gating EN.5.A and Phase 6.

```
56c9732 docs: update docs for EN.5.D-policy-dispatch-seam
2d6e67a feat: implement EN.5.D-policy-dispatch-seam-task11
9c8be61 feat: implement EN.5.D-policy-dispatch-seam-task10
fb01177 feat: implement EN.5.D-policy-dispatch-seam-task9
bfb269e feat: implement EN.5.D-policy-dispatch-seam-task8
1426404 feat: implement EN.5.D-policy-dispatch-seam-task7
b89ad0e feat: implement EN.5.D-policy-dispatch-seam-task6
d3a5893 feat: implement EN.5.D-policy-dispatch-seam-task5
```

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
