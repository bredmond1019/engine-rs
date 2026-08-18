//! `terminal` (`EN.9.D`) — the Phase-2 read-only terminal nodes: create/
//! observe a tmux session, no sends, no waits.
//!
//! `identity` (task 1) is the pure session-naming helper
//! [`identity::session_name_for`] plus the per-struct `session_input`
//! [`crate::node::InputBinding`] field convention every node in this module
//! follows — `TerminalSessionNode`/`TerminalObserveNode` hold the field
//! directly (following `revise.rs:59-69`'s shape), never
//! `crate::node::NodeExt::with_input_from`'s [`crate::node::WithInput`]
//! wrapper, which is inert passthrough for a node whose `process` never
//! reads it (block record `N4`).
//!
//! `session` (task 2) is `TerminalSessionNode`. `pane` (task 3) is the pure
//! pane-bounding/redaction/hashing helpers. `observe` (task 4) is
//! `TerminalObserveNode`.
//!
//! `predicate` (`EN.9.E` task 1) is the Phase-3 write/await pair's pure
//! `AwaitPredicate` enum and its evaluation — the marker semantics, and
//! the `Detect`/`Regex`/`Silence`/`ExitCode` alternatives, that
//! `TerminalAwaitNode` (task 3) polls against.
//!
//! `send` (`EN.9.E` task 2) is `TerminalSendNode` — the guarded,
//! floor-checked write node: `sdlc_flow::command_floor`, `send_id`
//! back-edge idempotency, and lease re-verification, all under a
//! per-session mutex.
//!
//! `await_node` (`EN.9.E` task 3) is `TerminalAwaitNode` — the bounded,
//! cancellable poll over `predicate::AwaitPredicate`: its OWN timeout
//! (`RunOptions` has no deadline field), a `CancellationToken` taken
//! through its own builder and `select!`ed on every poll tick (the runner
//! only observes cancellation between nodes), and a resolved
//! poll-interval/timeout policy stamped into its `ctx.nodes` result.
//!
//! `manifest_source` (`EN.9.F` task 1) is `ManifestSource` — the runtime
//! override for the detect manifest around `term_core::detect`'s
//! compile-time `include_str!` consts: resolved from
//! `ENGINE_TERMINAL_MANIFEST_OVERRIDE`, re-read/re-compiled on mtime
//! change, cached between calls, with a malformed override kept on the
//! last-good compile (never a silent fall-back) rather than taking
//! detection down.

pub mod await_node;
pub mod identity;
pub mod manifest_source;
pub mod observe;
pub mod pane;
pub mod predicate;
pub mod send;
pub mod session;

pub use await_node::{AwaitPolicy, PartialAwaitPolicy, TerminalAwaitNode};
pub use identity::session_name_for;
pub use manifest_source::{ManifestOrigin, ManifestSource, ResolvedManifest};
pub use observe::TerminalObserveNode;
pub use pane::{
    bound_pane_tail, default_pane_tail_policy, BoundedPane, PaneLimits, PaneTailPolicy,
};
pub use predicate::{
    evaluate, marker_path, AwaitPredicate, MarkerObservation, Observation, PredicateOutcome,
};
pub use send::TerminalSendNode;
pub use session::TerminalSessionNode;
