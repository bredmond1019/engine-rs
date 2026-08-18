//! The single integration-test binary for `engine-core` — every file under `tests/it/` is a
//! module of THIS binary, not a binary of its own.
//!
//! Why: cargo builds one test binary per `tests/*.rs` file, and each one statically links the
//! whole crate plus its ~345-crate dependency graph. At 25 files that was ~25 x 20MB of linking
//! on every full test run — measured on 2026-07-29 as the single largest cost in this repo's
//! SDLC loop, dwarfing the 1.8s it takes to actually RUN the tests. One binary, one link.
//!
//! Test ISOLATION is unaffected: `cargo nextest run` (this repo's mandated runner — CLAUDE.md
//! standing rule 7) executes every test in its own process regardless of how many binaries the
//! tests are packed into. That is what makes this collapse safe here when it would not be under
//! plain `cargo test`.
//!
//! Adding an integration test: create `tests/it/<name>.rs` and add one `mod <name>;` line below.
//! Do NOT add a new `tests/*.rs` file at this level — that silently reintroduces a second binary.

mod action_dispatch_e2e;
mod approve_and_run;
mod budget;
mod cancellation;
mod claude_code_step;
mod composition;
mod content_pipeline_e2e;
mod content_pipeline_materialize_e2e;
mod diagnostic_intake_e2e;
mod evals_slice;
mod fan_out_aggregate;
mod harvest_gate_e2e;
mod locale_rate_card;
mod materialize_doc;
mod operator_queue;
mod opportunity_loop_e2e;
mod parallel;
mod policy_dispatch_e2e;
mod policy_framework;
mod proposal_generator_e2e;
mod research_agent_contacts_e2e;
mod research_agent_e2e;
mod research_agent_grounding_e2e;
mod research_ingress_dispatch_e2e;
mod run_failure_notification;
mod sdlc_flow_check_selection;
mod sdlc_flow_e2e;
mod sdlc_flow_experiment;
mod sdlc_flow_live;
mod sdlc_flow_profiles;
mod sdlc_flow_repo_resolution;
mod sdlc_flow_retry_feedback;
mod sdlc_flow_run_id_terminal;
mod sdlc_flow_task_loop;
mod sdlc_flow_terminal_paths;
mod sdlc_flow_write_permission;
mod suspend_resume;
mod terminal_admission;
mod terminal_send_await;
mod validator;
mod workflow_runner;
