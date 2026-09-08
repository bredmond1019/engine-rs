//! `progress` — the mid-node progress-reporting seam (`EN.ticket.node-progress-sink`
//! task 1).
//!
//! `Node::process` (`crate::node`) takes `TaskContext` **by value** and returns
//! it only at the end, so a node has no in-band channel to report partial
//! state while it runs. The existing `OnProgress` callback (`crate::workflow`)
//! only fires at node boundaries — entry, then RUNNING/SUCCESS/FAILED — so a
//! node doing several things in sequence (a multi-query capture loop, a whole
//! validation suite) is silent for the entire call.
//!
//! `ProgressSink` closes that gap: an injectable, no-op-by-default seam a
//! node holds and calls as it completes each unit of its own work. It
//! follows this crate's established seam convention (see
//! `crate::nodes::http_post::HttpPost` for the pattern this mirrors): a
//! trait object behind `Arc<dyn ProgressSink>`, a no-op default that keeps
//! every existing run's behavior unchanged, and a recording stub for tests.
//!
//! `emit` is **synchronous and non-blocking** — a bounded try-send that
//! drops on a full channel — because it is called from inside `process` on
//! a blocking-pool thread, and progress is advisory: a dropped progress
//! event must never fail or slow a run. This block ships the seam and its
//! wiring only; no existing node adopts it (see the block record's
//! `out_of_scope`), so every existing test continues to pass unedited with
//! the `NoopProgressSink` default.

use std::sync::{Arc, Mutex};

/// A single mid-node progress update: `done` of `total` units completed by
/// `node`, with an optional human-readable `label` for the current unit
/// (e.g. a query string in a capture loop).
///
/// Deliberately a small typed record, not a `TaskContext` snapshot — the
/// context has not been updated mid-node, so a snapshot would misreport
/// state rather than report progress, and cloning a whole context on every
/// unit is needless work on top of being wrong. See the block's `notes` for
/// the full rationale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeProgress {
    /// The node identity reporting progress (`Node::name()`, or the
    /// instance identity from `NodeExt::with_identity` when the node was
    /// wrapped in `Identified`).
    pub node: String,
    /// Units of the node's own work completed so far.
    pub done: usize,
    /// Total units the node expects to complete. A node that does not know
    /// its total in advance may report `0` and update it on a later call.
    pub total: usize,
    /// Optional description of the current unit (e.g. "query 3: pricing
    /// for SKU-1029").
    pub label: Option<String>,
}

impl NodeProgress {
    /// Construct a `NodeProgress` with no label.
    pub fn new(node: impl Into<String>, done: usize, total: usize) -> Self {
        Self {
            node: node.into(),
            done,
            total,
            label: None,
        }
    }

    /// Attach a label describing the current unit.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

/// The injectable mid-node progress-reporting seam. Object-safe, so it is
/// held as `Arc<dyn ProgressSink>` and shared cheaply across a node and any
/// clones/wrappers of it (mirrors `HttpPost`, `ChannelTransport`, and every
/// other `Send + Sync` trait-object seam in this crate).
///
/// `emit` must never block and must never fail a run: implementors report
/// progress on a best-effort basis (a bounded try-send that drops on a full
/// channel is the reference shape — see `NoopProgressSink` and the live
/// `engine-serve` implementation added in a later task of this block).
pub trait ProgressSink: Send + Sync {
    /// Report one progress update. Must return immediately regardless of
    /// whether the update was actually delivered anywhere.
    fn emit(&self, update: NodeProgress);
}

/// The default `ProgressSink`: discards every update silently. A node that
/// never calls `with_progress(...)` (or is constructed without an explicit
/// sink) behaves exactly as it did before this seam existed — no run's
/// behavior or emitted events change.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopProgressSink;

impl ProgressSink for NoopProgressSink {
    fn emit(&self, _update: NodeProgress) {
        // Deliberately discarded — see the type doc.
    }
}

/// Convenience: `Arc<dyn ProgressSink>` defaulting to [`NoopProgressSink`].
/// Nodes that hold their sink as this alias get behavior-stable-by-default
/// for free from `Default`.
pub fn noop_progress_sink() -> Arc<dyn ProgressSink> {
    Arc::new(NoopProgressSink)
}

/// A test double that records every `NodeProgress` it receives, in call
/// order, behind a `Mutex<Vec<_>>`. Mirrors this crate's other recording
/// stubs (e.g. `HttpPost` test doubles) — no live channel, no async, just an
/// append-only log a test can inspect after the run.
#[derive(Debug, Default)]
pub struct RecordingProgressSink {
    records: Mutex<Vec<NodeProgress>>,
}

impl RecordingProgressSink {
    /// Construct an empty recording sink.
    pub fn new() -> Self {
        Self::default()
    }

    /// The recorded updates, in the order `emit` was called.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned (i.e. a prior `emit` call
    /// panicked while holding the lock) — that indicates a bug in the sink
    /// itself, not a legitimate empty-records case.
    pub fn records(&self) -> Vec<NodeProgress> {
        self.records
            .lock()
            .expect("RecordingProgressSink mutex poisoned")
            .clone()
    }
}

impl ProgressSink for RecordingProgressSink {
    fn emit(&self, update: NodeProgress) {
        // A poisoned lock here would mean a previous `emit` panicked while
        // holding it, which should never happen in this stub — but per the
        // seam's own contract, `emit` must never panic, so fall back to
        // silently dropping rather than propagating a poison panic.
        if let Ok(mut records) = self.records.lock() {
            records.push(update);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::sync_channel;

    #[test]
    fn noop_progress_sink_discards_silently() {
        let sink = NoopProgressSink;
        // Should not panic, and there is nothing further to observe — the
        // point of this sink is that it does nothing.
        sink.emit(NodeProgress::new("probe", 1, 7));
        sink.emit(NodeProgress::new("probe", 2, 7));
    }

    #[test]
    fn noop_progress_sink_is_the_default_via_helper() {
        let sink = noop_progress_sink();
        sink.emit(NodeProgress::new("probe", 1, 1));
    }

    #[test]
    fn recording_stub_yields_n_records_in_order_with_correct_fields() {
        let sink = RecordingProgressSink::new();
        for i in 1..=7 {
            sink.emit(NodeProgress::new("capture", i, 7).with_label(format!("query {i}")));
        }

        let records = sink.records();
        assert_eq!(records.len(), 7);
        for (i, record) in records.iter().enumerate() {
            let expected_done = i + 1;
            assert_eq!(record.node, "capture");
            assert_eq!(record.done, expected_done);
            assert_eq!(record.total, 7);
            assert_eq!(
                record.label.as_deref(),
                Some(format!("query {expected_done}")).as_deref()
            );
        }
    }

    #[test]
    fn recording_stub_starts_empty() {
        let sink = RecordingProgressSink::new();
        assert!(sink.records().is_empty());
    }

    /// `emit` against a channel-backed sink whose channel is FULL must
    /// return immediately without blocking or erroring. This is the
    /// non-blocking guarantee the block's `why` depends on: `emit` runs on
    /// a blocking-pool thread inside `process`, so a sink that blocks on a
    /// full channel would wedge the node calling it.
    #[test]
    fn emit_against_a_full_channel_does_not_block_or_panic() {
        struct TrySendSink {
            tx: std::sync::mpsc::SyncSender<NodeProgress>,
        }
        impl ProgressSink for TrySendSink {
            fn emit(&self, update: NodeProgress) {
                // Bounded try-send: drop silently if the channel is full or
                // closed, exactly as the live `engine-serve` sink must.
                let _ = self.tx.try_send(update);
            }
        }

        // Capacity 1: the first send fills it, the second must not block.
        let (tx, _rx) = sync_channel(1);
        let sink = TrySendSink { tx };
        sink.emit(NodeProgress::new("gate", 1, 2));
        // Channel is now full (nothing has received yet) — this call must
        // return immediately rather than block on the full bounded channel.
        sink.emit(NodeProgress::new("gate", 2, 2));
    }

    /// `emit` against a CLOSED channel (receiver dropped) must also return
    /// immediately without panicking or erroring the caller.
    #[test]
    fn emit_against_a_closed_channel_does_not_panic_or_error() {
        struct TrySendSink {
            tx: std::sync::mpsc::SyncSender<NodeProgress>,
        }
        impl ProgressSink for TrySendSink {
            fn emit(&self, update: NodeProgress) {
                let _ = self.tx.try_send(update);
            }
        }

        let (tx, rx) = sync_channel(1);
        drop(rx); // close the receiving end
        let sink = TrySendSink { tx };
        // Must not panic even though every send now fails with Disconnected.
        sink.emit(NodeProgress::new("gate", 1, 2));
    }

    #[test]
    fn node_progress_with_label_builder() {
        let update = NodeProgress::new("n", 0, 3).with_label("first");
        assert_eq!(update.label.as_deref(), Some("first"));
    }
}
