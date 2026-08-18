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

pub mod identity;
pub mod pane;
pub mod session;

pub use identity::session_name_for;
pub use pane::{
    bound_pane_tail, default_pane_tail_policy, BoundedPane, PaneLimits, PaneTailPolicy,
};
pub use session::TerminalSessionNode;
