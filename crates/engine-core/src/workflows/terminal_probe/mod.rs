//! `TERMINAL_PROBE` (`EN.9.D` task 5) — the one-node-per-stage probe
//! workflow that exercises the Phase-2 read-only terminal nodes end to
//! end: [`crate::nodes::terminal::TerminalSessionNode`] ensures/creates a
//! tmux session and acquires its lease, then
//! [`crate::nodes::terminal::TerminalObserveNode`] captures the pane once
//! and detects the agent state — no sends, no waits.
//!
//! Composes existing primitives; it invents no new one. Module layout
//! mirrors `harvest_approve`'s micro-workflow shape:
//! - [`graph`] — the declared `WorkflowSchema` / `NodeRegistry` /
//!   `Workflow` assembly, built via `Workflow::new_validated` so a
//!   structurally unsound graph fails loudly at assembly.
//!
//! **No policy module, no profiles module.** Neither node calls a model;
//! `TerminalObserveNode`'s only configurable knob (`PaneTailPolicy`) is
//! resolved internally from the upstream session's `adopted` state, not
//! from a `harness.json` policy layer, so there is nothing for a
//! `Policy`/`profiles` surface to override here (CLAUDE.md standing rule
//! 6's "where feasible" carve-out — the value is derived, not a cost/
//! latency/quality knob a run would want overridden).

pub mod graph;

pub use graph::{registry, registry_with, schema, workflow, TERMINAL_PROBE_WORKFLOW_TYPE};
