//! `term-core` — tmux session-control and agent-detection primitives.
//!
//! This crate is a straight port of bastion's `src/sessions/{tmux,model,claude_state}.rs`
//! and `src/detect/` modules, carried here so the engine can drive terminals without ever
//! linking the attach path.
//!
//! It is consumed by BOTH a blocking caller (the bastion CLI) and a tokio caller
//! (`engine-core`). The attach path (`attach_session` / `suspend_and_attach`) deliberately
//! lives in a separate crate, `term-attach`, rather than behind a feature flag on this one.
//!
//! Cargo features unify **additively**: `bastion -> term-core` (blocking) and
//! `bastion -> engine-serve -> engine-core -> term-core` (tokio) are both ordinary
//! `[dependencies]` on the same target, so no resolver v2/v3 exemption applies and exactly
//! one rlib gets built with the union of every enabled feature. Under a feature gate,
//! `attach_session` would be `pub` and callable from `engine-core` in the shipped binary —
//! the exact process with no controlling tty. A crate `engine-core` never links (`term-attach`)
//! is the only real guarantee that the attach path cannot be reached from there. Do not
//! "simplify" this back into a feature.
//!
//! Module slots below are filled by later tasks in this spec block (`EN.9.A`); each starts
//! empty so the workspace builds with this crate present.

pub mod claude_state;
pub mod detect;
pub mod model;
pub mod tmux;
