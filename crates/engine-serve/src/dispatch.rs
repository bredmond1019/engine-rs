//! Re-export of `engine-core`'s dispatch module.
//!
//! `Dispatcher` / `DispatchError` / `WorkflowFactory` moved down into
//! `engine-core` in `EN.5.E` task 3 (nothing about them was HTTP-specific —
//! see `engine_core::dispatch`'s module docs for the full rationale). This
//! module re-exports the moved types at their original public path so
//! `bastion`'s path dependency and every existing `engine_serve::dispatch::*`
//! / `crate::dispatch::*` import site keeps resolving unchanged.

pub use engine_core::dispatch::{DispatchError, Dispatcher, WorkflowFactory};
