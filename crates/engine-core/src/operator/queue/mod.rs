//! The operator queue (`EN.8.B`).
//!
//! Per `planning/8.B-operator-queue/tasks.md`: at most one open
//! operator-facing item at a time, ordered by effective priority rather
//! than arrival, with a digest tail carrying the low-priority remainder as
//! counts and mandatory storm suppression so a restart burst produces one
//! message, not N.
//!
//! Task 1 (this module, plus [`item`]) defines the item shape and its
//! ordering only — pure data and a pure comparator, no I/O, no clock reads.
//! Later tasks in this block add the durable source reader ([`source`],
//! task 2), the depth-limited delivery queue itself (task 3), and the
//! digest/storm-suppression tail (task 4).

pub mod item;

pub use item::{compare_items, ItemSource, OperatorQueueItem};
