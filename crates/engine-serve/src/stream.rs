//! SSE progress stream (task 4): a per-run `tokio::sync::broadcast` tee wired
//! into the existing `on_progress` composition in `http.rs`'s `post_events`,
//! plus the `GET /events/{event_id}/stream` endpoint that serves it.
//!
//! This is a **third fan-out** alongside the live-state recorder and the
//! durable writer — not a second progress mechanism. `post_events` calls
//! [`publish`] from inside the same `on_progress` closure at every node
//! transition, and [`publish_terminal`] once on every exit path (success,
//! node error, cancellation, budget halt).
//!
//! Like [`crate::abort::RunRegistry`] and [`crate::live_state::LiveStateStore`],
//! the per-run `broadcast::Sender` registry lives behind a process-global
//! `OnceLock` (mirroring `http.rs`'s `default_budget_from_env` /
//! `live_run_metadata` precedent) rather than a field on
//! [`crate::http::AppState`] — `bastion` constructs that struct as a literal
//! over an unpinned path dependency, so a new public field is an immediate
//! cross-repo compile break for zero gain.
//!
//! **Late subscribers to a terminal run.** Once a run's last frame is
//! published, its live `broadcast::Sender` is retired (dropped) — a
//! `broadcast` channel with no senders left answers every `recv()` with
//! `RecvError::Closed`, so any subscriber still parked on it wakes up and
//! ends its stream cleanly instead of hanging forever. A **terminal cache**
//! (a small side map of the single last frame per finished run) covers a
//! subscriber that connects *after* that point: [`subscribe`] hands back a
//! fresh one-shot channel pre-loaded with the cached terminal frame — one
//! `Ok` frame, then `Closed` — rather than a channel nobody will ever publish
//! to again.
//!
//! **NodeProgress, a second, distinct event shape (`EN.ticket.node-progress-sink`
//! task 3).** [`StreamFrame`] carries a `TaskContext` snapshot at a node
//! *boundary*; `engine_core::progress::NodeProgress` is a small typed
//! mid-node update a node reports as it completes each unit of its own work
//! (task 1/2 of the same block; not yet adopted by any node — see the
//! block's `out_of_scope`). The two are never conflated into one payload
//! shape: a progress update is carried as its own [`ProgressFrame`], on its
//! own per-run broadcast channel (registered in [`Registry`], alongside —
//! not instead of — the state channel), and encoded on the wire as its own
//! SSE `event: progress` frame — a state frame carries no `event:` line at
//! all (its wire shape is unchanged from before this task, since an
//! existing consumer test parses it by stripping a literal `data: `
//! prefix), which is the SSE spec's default event type `message`. A
//! consumer tells the two apart by that `event:` type alone (`progress` vs.
//! the implicit `message`), never by guessing from payload contents.
//! [`stream_event`] tees both channels into one response via
//! [`futures::stream::select`]. Progress is advisory and droppable by design
//! (`engine_core::progress`): unlike the state channel, there is no terminal
//! cache for it — [`subscribe_progress`] gives a reconnecting or
//! late-connecting client an already-closed channel instead of any
//! backfill, and the run's progress sender is retired at the same terminal
//! transitions ([`publish_terminal`], [`publish_suspended`]) that retire the
//! state sender.

use std::collections::HashMap as StdHashMap;
use std::sync::{OnceLock, RwLock};

use actix_web::{web, HttpRequest, HttpResponse, Responder};
use engine_contract::TaskContext;
use engine_core::progress::NodeProgress;
use futures::stream;
use serde::Serialize;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::http::{check_api_key, derive_terminal_status, AppState};
use crate::live_state::RunId;

/// Per-run broadcast channel capacity. Generous enough that a normal
/// node-by-node progress stream never lags a reasonably-fast subscriber;
/// a slower one hits [`broadcast::error::RecvError::Lagged`], handled
/// explicitly rather than left to panic or wedge the stream (see
/// [`recv_next`]).
const CHANNEL_CAPACITY: usize = 256;

/// One SSE frame: the run's `event_id`, its server-derived `status` at this
/// boundary (`"running"` for every non-terminal frame, the terminal status
/// string from [`crate::http::derive_terminal_status`] for the last one),
/// the `TaskContext` snapshot, and whether this is the terminal frame.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StreamFrame {
    pub event_id: Uuid,
    pub status: String,
    pub task_context: TaskContext,
    pub terminal: bool,
}

/// One mid-node progress frame: the run's `event_id` plus the reporting
/// node's identity and `done`/`total` counts (and optional `label`), lifted
/// straight from `engine_core::progress::NodeProgress`. Deliberately a
/// distinct type from [`StreamFrame`] rather than an optional field on it —
/// a progress update never carries a `TaskContext`, and a consumer must be
/// able to tell the two apart from the SSE `event:` type alone, without
/// inspecting payload shape (see [`encode_progress_sse`] / [`encode_sse`]).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProgressFrame {
    pub event_id: Uuid,
    pub node: String,
    pub done: usize,
    pub total: usize,
    pub label: Option<String>,
}

impl ProgressFrame {
    fn from_update(event_id: Uuid, update: &NodeProgress) -> Self {
        Self {
            event_id,
            node: update.node.clone(),
            done: update.done,
            total: update.total,
            label: update.label.clone(),
        }
    }
}

/// Registry of live per-run senders, plus a side cache of the single last
/// (terminal) frame for runs that have already finished — both behind
/// **one** lock. `subscribe`'s "is this run terminal, or should I get/create
/// a live sender" decision and `publish_terminal`'s "cache the frame and
/// retire the live sender" transition must be atomic relative to each
/// other: two separate locks let a `subscribe` straddle the two halves of
/// `publish_terminal`'s transition (its terminal-cache check landing before
/// the insert, its live-sender lookup landing after the removal) and mint a
/// channel nobody will ever publish to again — the exact "orphan channel"
/// hang this registry exists to prevent. A single `RwLock` closes that
/// window entirely: the two operations can never interleave.
struct Registry {
    live: StdHashMap<RunId, broadcast::Sender<StreamFrame>>,
    terminal: TerminalCache,
    /// Per-run progress senders (task 3). No terminal cache here — progress
    /// is advisory and deliberately not replayed to a late subscriber (see
    /// [`subscribe_progress`]) — so this is just a live map, retired at the
    /// same terminal transitions that retire `live`.
    progress: StdHashMap<RunId, broadcast::Sender<ProgressFrame>>,
}

/// Bounded FIFO cache of terminal frames, mirroring
/// [`crate::live_state::LiveStateStore`]'s completed-run ring so a
/// long-lived server process doesn't accumulate one cloned `TaskContext`
/// per completed run forever.
#[derive(Default)]
struct TerminalCache {
    entries: StdHashMap<RunId, StreamFrame>,
    order: std::collections::VecDeque<RunId>,
}

impl TerminalCache {
    fn insert(&mut self, run_id: RunId, frame: StreamFrame) {
        if self.entries.insert(run_id, frame).is_none() {
            self.order.push_back(run_id);
        }
        while self.order.len() > crate::live_state::COMPLETED_RUN_RETENTION {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }

    fn get(&self, run_id: RunId) -> Option<StreamFrame> {
        self.entries.get(&run_id).cloned()
    }
}

fn registry() -> &'static RwLock<Registry> {
    static REGISTRY: OnceLock<RwLock<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        RwLock::new(Registry {
            live: StdHashMap::new(),
            terminal: TerminalCache::default(),
            progress: StdHashMap::new(),
        })
    })
}

/// Get-or-create the live sender for `run_id`.
fn sender_for(run_id: RunId) -> broadcast::Sender<StreamFrame> {
    {
        let guard = registry()
            .read()
            .expect("stream registry lock poisoned on read");
        if let Some(sender) = guard.live.get(&run_id) {
            return sender.clone();
        }
    }
    let mut guard = registry()
        .write()
        .expect("stream registry lock poisoned on write");
    guard
        .live
        .entry(run_id)
        .or_insert_with(|| broadcast::channel(CHANNEL_CAPACITY).0)
        .clone()
}

/// Publish a non-terminal frame for `run_id` (status `"running"`). A send
/// with no active subscribers is a no-op — `broadcast::Sender::send` only
/// errors when there are zero receivers, which the tee must never treat as a
/// failure: `post_events`'s run loop cannot be interrupted by nobody
/// listening.
pub fn publish(run_id: RunId, task_context: &TaskContext) {
    let sender = sender_for(run_id);
    let _ = sender.send(StreamFrame {
        event_id: run_id,
        status: "running".to_string(),
        task_context: task_context.clone(),
        terminal: false,
    });
}

/// Publish the terminal frame for `run_id` (status derived by
/// [`crate::http::derive_terminal_status`]) and retire the run: any
/// currently-parked subscriber sees this frame and then `RecvError::Closed`
/// once the sender is dropped; the frame is also cached so a subscriber that
/// connects afterward still gets it (see [`subscribe`]).
pub fn publish_terminal(run_id: RunId, final_context: &TaskContext) {
    let status = derive_terminal_status(final_context).to_string();
    let frame = StreamFrame {
        event_id: run_id,
        status,
        task_context: final_context.clone(),
        terminal: true,
    };

    // One write-lock acquisition for the whole transition: best-effort
    // delivery to whoever is already listening, caching the frame, and
    // retiring the live sender, all atomically relative to `subscribe`. A
    // `subscribe` call can only ever observe this run in the state *before*
    // this transition (live, not yet terminal) or *after* it (terminal,
    // cached) — never the torn state that mints an orphan channel.
    let mut guard = registry()
        .write()
        .expect("stream registry lock poisoned on write");
    if let Some(sender) = guard.live.get(&run_id) {
        let _ = sender.send(frame.clone());
    }
    guard.terminal.insert(run_id, frame);
    guard.live.remove(&run_id);
    // Retire the progress sender at the same transition: no further
    // NodeProgress for a finished run, and this is what lets a
    // `subscribe_progress` call racing this one observe either "live" or
    // "gone", never a channel nobody will ever publish to again.
    guard.progress.remove(&run_id);
}

/// Publish a `status: "suspended"`, `terminal: true` frame and retire the
/// live sender, so a subscriber parked on a now-suspended run closes cleanly
/// instead of hanging. Mirrors [`publish_terminal`]: same single write lock,
/// same terminal-cache insert — a suspended run's stream behaves exactly
/// like a finished run's until [`clear_terminal`] is called on resume.
pub fn publish_suspended(run_id: RunId, snapshot: &TaskContext) {
    let frame = StreamFrame {
        event_id: run_id,
        status: "suspended".to_string(),
        task_context: snapshot.clone(),
        terminal: true,
    };

    let mut guard = registry()
        .write()
        .expect("stream registry lock poisoned on write");
    if let Some(sender) = guard.live.get(&run_id) {
        let _ = sender.send(frame.clone());
    }
    guard.terminal.insert(run_id, frame);
    guard.live.remove(&run_id);
    guard.progress.remove(&run_id);
}

/// Drop `run_id`'s cached terminal frame (and any retired sender) so a
/// resumed run can stream again: the next [`subscribe`] call falls through
/// the now-empty terminal-cache check and mints a fresh live channel. A
/// no-op for a run with no cached frame.
///
/// Without this, [`subscribe`]'s terminal-cache short-circuit (`:194`)
/// would keep handing every post-resume subscriber the stale `suspended`
/// frame followed by `Closed` — a live run with a dead stream.
pub fn clear_terminal(run_id: RunId) {
    let mut guard = registry()
        .write()
        .expect("stream registry lock poisoned on write");
    guard.terminal.entries.remove(&run_id);
    guard.terminal.order.retain(|id| *id != run_id);
    guard.live.remove(&run_id);
}

/// Subscribe to `run_id`'s tee. If the run has already gone terminal,
/// returns a fresh one-shot channel pre-loaded with the cached terminal
/// frame (one `Ok`, then `Closed`) instead of a live channel nobody will
/// ever publish to again. Otherwise returns (creating if necessary) a
/// receiver on the run's live broadcast channel.
///
/// Takes the write lock unconditionally (even on the cache-hit path) so
/// this check-then-act is atomic relative to `publish_terminal`'s own
/// single-lock transition — see [`Registry`]'s docs.
pub fn subscribe(run_id: RunId) -> broadcast::Receiver<StreamFrame> {
    let mut guard = registry()
        .write()
        .expect("stream registry lock poisoned on write");

    if let Some(frame) = guard.terminal.get(run_id) {
        let (one_shot, receiver) = broadcast::channel(1);
        let _ = one_shot.send(frame);
        // Drop the sender immediately: the receiver already has its one
        // frame queued, and dropping closes the channel so the next `recv()`
        // after it is `Closed` rather than pending forever.
        drop(one_shot);
        return receiver;
    }

    guard
        .live
        .entry(run_id)
        .or_insert_with(|| broadcast::channel(CHANNEL_CAPACITY).0)
        .subscribe()
}

/// Get-or-create the live progress sender for `run_id`. Mirrors
/// [`sender_for`] exactly — same lazy get-or-create shape, same "a stray
/// send after the run has already gone terminal just re-mints an entry
/// nobody retires" acceptance that the state channel's `sender_for`/`publish`
/// pair already lives with (task 3 does not change that convention, only
/// adds a second channel following it).
fn progress_sender_for(run_id: RunId) -> broadcast::Sender<ProgressFrame> {
    {
        let guard = registry()
            .read()
            .expect("stream registry lock poisoned on read");
        if let Some(sender) = guard.progress.get(&run_id) {
            return sender.clone();
        }
    }
    let mut guard = registry()
        .write()
        .expect("stream registry lock poisoned on write");
    guard
        .progress
        .entry(run_id)
        .or_insert_with(|| broadcast::channel(CHANNEL_CAPACITY).0)
        .clone()
}

/// Publish one [`NodeProgress`] update for `run_id`. A send with no active
/// subscribers is a no-op, exactly like [`publish`] — the tee must never
/// treat "nobody is listening right now" as a failure the caller has to
/// handle.
pub fn publish_progress(run_id: RunId, update: &NodeProgress) {
    let sender = progress_sender_for(run_id);
    let _ = sender.send(ProgressFrame::from_update(run_id, update));
}

/// Subscribe to `run_id`'s progress tee. Unlike [`subscribe`], there is
/// nothing to replay: progress is advisory and droppable by design, so a
/// client that connects after the run has already gone terminal gets an
/// **already-closed** channel (zero senders) rather than a live one nobody
/// will ever publish to again, and never a backfill of updates it missed
/// while it was away.
///
/// Takes the write lock unconditionally, mirroring [`subscribe`]'s own
/// atomicity argument: this check-then-act must be atomic relative to
/// [`publish_terminal`]'s/[`publish_suspended`]'s single-lock retirement of
/// `guard.progress`, or a subscribe landing in that window could mint a
/// fresh live entry immediately after the run retired it.
pub fn subscribe_progress(run_id: RunId) -> broadcast::Receiver<ProgressFrame> {
    let mut guard = registry()
        .write()
        .expect("stream registry lock poisoned on write");

    if guard.terminal.entries.contains_key(&run_id) {
        let (one_shot, receiver) = broadcast::channel(1);
        drop(one_shot);
        return receiver;
    }

    guard
        .progress
        .entry(run_id)
        .or_insert_with(|| broadcast::channel(CHANNEL_CAPACITY).0)
        .subscribe()
}

/// Pull the next frame off `receiver`, transparently skipping past a
/// [`broadcast::error::RecvError::Lagged`] gap rather than panicking or
/// treating it as end-of-stream — a slow SSE client falls behind, not the
/// whole stream.
async fn recv_next(receiver: &mut broadcast::Receiver<StreamFrame>) -> Option<StreamFrame> {
    loop {
        match receiver.recv().await {
            Ok(frame) => return Some(frame),
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return None,
        }
    }
}

/// [`recv_next`]'s counterpart for the progress channel.
async fn recv_next_progress(
    receiver: &mut broadcast::Receiver<ProgressFrame>,
) -> Option<ProgressFrame> {
    loop {
        match receiver.recv().await {
            Ok(frame) => return Some(frame),
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return None,
        }
    }
}

/// Encode one [`StreamFrame`] as a single SSE `data:` event — byte-identical
/// to this function's shape before task 3 (no explicit `event:` line, so it
/// carries the SSE default event type `message`). Existing consumers
/// (`crates/engine-serve/tests/async_lifecycle.rs`'s
/// `stream_delivers_one_frame_per_node_transition_then_a_terminal_frame`
/// strips a literal `"data: "` prefix off every frame) depend on this shape
/// staying exactly as it was — see [`encode_progress_sse`] for how the two
/// event shapes are still distinguished.
fn encode_sse(frame: &StreamFrame) -> web::Bytes {
    let payload = serde_json::to_string(frame).unwrap_or_else(|_| "{}".to_string());
    web::Bytes::from(format!("data: {payload}\n\n"))
}

/// Encode one [`ProgressFrame`] as a single SSE event, typed `event:
/// progress` — the explicit type is what makes it distinguishable from a
/// [`StreamFrame`] (which carries no `event:` line, so it is the SSE
/// default type `message`) by event type alone, without a consumer ever
/// needing to inspect payload contents. Never a `TaskContext` — see the
/// module's NodeProgress doc section for why the two payload shapes are
/// kept distinct.
fn encode_progress_sse(frame: &ProgressFrame) -> web::Bytes {
    let payload = serde_json::to_string(frame).unwrap_or_else(|_| "{}".to_string());
    web::Bytes::from(format!("event: progress\ndata: {payload}\n\n"))
}

/// `GET /events/{event_id}/stream` — the SSE progress stream. `X-API-Key`
/// gated (401 without it); 404 for an unknown or malformed id (never a
/// hanging connection). Serves `text/event-stream`: one frame per node
/// transition, then a terminal frame, then the stream ends — including for a
/// run that is already terminal by the time the client connects.
pub async fn stream_event(
    path: web::Path<String>,
    req: HttpRequest,
    state: web::Data<AppState>,
) -> impl Responder {
    if !check_api_key(&req, &state.api_key) {
        return HttpResponse::Unauthorized().finish();
    }

    let raw_id = path.into_inner();
    let event_id = match Uuid::parse_str(&raw_id) {
        Ok(id) => id,
        Err(_) => {
            return HttpResponse::NotFound()
                .json(serde_json::json!({ "error": "unknown or malformed event_id" }));
        }
    };

    // Three-tier lookup, mirroring `get_event` (`http.rs:465`): live map, then
    // the terminal record ring, then `live_run_metadata()`. That third tier
    // covers the window between `post_events` registering the run_id and the
    // first `on_progress` snapshot landing in `LiveStateStore` — without it,
    // a client that opens the stream in that window gets 404'd here while
    // `GET /events/{id}` correctly reports `running` for the same id
    // (`http.rs:517-540`). Change one, change the other.
    let known = state.live.get(event_id).is_some()
        || state.live.get_record(event_id).is_some()
        || crate::http::live_run_metadata()
            .read()
            .expect("live run metadata lock poisoned on read")
            .contains_key(&event_id);
    if !known {
        return HttpResponse::NotFound()
            .json(serde_json::json!({ "error": "unknown or malformed event_id" }));
    }

    let receiver = subscribe(event_id);
    let state_stream = stream::unfold(receiver, |mut receiver| async move {
        recv_next(&mut receiver)
            .await
            .map(|frame| (Ok::<_, actix_web::Error>(encode_sse(&frame)), receiver))
    });

    // Tee the progress channel in alongside the state channel: two distinct
    // SSE event types on the one connection (see the module's NodeProgress
    // doc section). `subscribe_progress` retires with the run exactly when
    // `subscribe`'s state channel does (both cleared under one write lock in
    // `publish_terminal`/`publish_suspended`), so `select` below completes
    // once both sides have closed rather than hanging on a progress channel
    // nobody will ever publish to again.
    let progress_receiver = subscribe_progress(event_id);
    let progress_stream = stream::unfold(progress_receiver, |mut receiver| async move {
        recv_next_progress(&mut receiver).await.map(|frame| {
            (
                Ok::<_, actix_web::Error>(encode_progress_sse(&frame)),
                receiver,
            )
        })
    });

    let body = stream::select(state_stream, progress_stream);

    HttpResponse::Ok()
        .content_type("text/event-stream")
        .streaming(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap as StdHashMap2;

    fn fixture_context(marker: &str) -> TaskContext {
        TaskContext {
            event: serde_json::json!({ "marker": marker }),
            nodes: StdHashMap2::new(),
            metadata: serde_json::json!({}),
            node_runs: StdHashMap2::new(),
        }
    }

    #[tokio::test]
    async fn publish_delivers_one_frame_per_snapshot() {
        let run_id = Uuid::new_v4();
        let mut receiver = subscribe(run_id);

        publish(run_id, &fixture_context("first"));
        publish(run_id, &fixture_context("second"));

        let first = recv_next(&mut receiver).await.expect("frame 1");
        assert_eq!(first.task_context, fixture_context("first"));
        assert!(!first.terminal);
        assert_eq!(first.status, "running");

        let second = recv_next(&mut receiver).await.expect("frame 2");
        assert_eq!(second.task_context, fixture_context("second"));
        assert!(!second.terminal);
    }

    #[tokio::test]
    async fn subscriber_receives_frames_in_order_then_a_terminal_frame_then_closes() {
        let run_id = Uuid::new_v4();
        let mut receiver = subscribe(run_id);

        publish(run_id, &fixture_context("a"));
        publish(run_id, &fixture_context("b"));
        publish_terminal(run_id, &fixture_context("done"));

        let frame_a = recv_next(&mut receiver).await.expect("frame a");
        assert_eq!(frame_a.task_context, fixture_context("a"));
        let frame_b = recv_next(&mut receiver).await.expect("frame b");
        assert_eq!(frame_b.task_context, fixture_context("b"));
        let terminal = recv_next(&mut receiver).await.expect("terminal frame");
        assert!(terminal.terminal);
        assert_eq!(terminal.task_context, fixture_context("done"));
        assert_eq!(terminal.status, "succeeded");

        assert!(
            recv_next(&mut receiver).await.is_none(),
            "stream should end after the terminal frame"
        );
    }

    #[tokio::test]
    async fn a_lagged_subscriber_recovers_instead_of_panicking() {
        let run_id = Uuid::new_v4();
        let mut receiver = subscribe(run_id);

        // Publish well past the channel capacity before the subscriber ever
        // reads, forcing a `Lagged` gap on the first `recv()`.
        for i in 0..(CHANNEL_CAPACITY + 10) {
            publish(run_id, &fixture_context(&format!("frame-{i}")));
        }
        publish_terminal(run_id, &fixture_context("done"));

        // `recv_next` must swallow the `Lagged` gap and keep delivering
        // frames without panicking, eventually reaching the terminal one.
        let mut saw_terminal = false;
        while let Some(frame) = recv_next(&mut receiver).await {
            if frame.terminal {
                saw_terminal = true;
                break;
            }
        }
        assert!(
            saw_terminal,
            "lagged subscriber should still reach a terminal frame"
        );
    }

    #[tokio::test]
    async fn subscribing_after_terminal_yields_the_terminal_frame_and_closes() {
        let run_id = Uuid::new_v4();
        publish_terminal(run_id, &fixture_context("already-done"));

        let mut receiver = subscribe(run_id);

        let frame = recv_next(&mut receiver)
            .await
            .expect("late subscriber should still get the terminal frame");
        assert!(frame.terminal);
        assert_eq!(frame.task_context, fixture_context("already-done"));

        assert!(
            recv_next(&mut receiver).await.is_none(),
            "late subscriber's stream should close after the terminal frame"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn subscribers_racing_publish_terminal_never_hang() {
        // Regression test: `publish_terminal` used to retire the live
        // sender (making `sender_for` mint a brand-new one) before caching
        // the terminal frame, so a `subscribe` landing in that window got a
        // receiver on a channel nobody would ever publish to again — an
        // unbounded hang. Race many subscribers against the publish across
        // many iterations to catch a reordering regression.
        for _ in 0..200 {
            let run_id = Uuid::new_v4();
            let ctx = fixture_context("racing");

            let publisher = tokio::task::spawn_blocking(move || {
                publish_terminal(run_id, &ctx);
            });

            let mut subscriber_tasks = Vec::new();
            for _ in 0..8 {
                subscriber_tasks.push(tokio::spawn(async move {
                    let mut receiver = subscribe(run_id);
                    tokio::time::timeout(std::time::Duration::from_secs(2), async move {
                        loop {
                            match recv_next(&mut receiver).await {
                                Some(frame) if frame.terminal => break,
                                Some(_) => continue,
                                None => break,
                            }
                        }
                    })
                    .await
                    .expect("subscriber hung across the publish_terminal race window")
                }));
            }

            publisher.await.expect("publisher thread should not panic");
            for task in subscriber_tasks {
                task.await.expect("subscriber task should not panic");
            }
        }
    }

    #[tokio::test]
    async fn publish_suspended_delivers_one_terminal_frame_then_closes() {
        let run_id = Uuid::new_v4();
        let mut receiver = subscribe(run_id);

        publish_suspended(run_id, &fixture_context("paused"));

        let frame = recv_next(&mut receiver)
            .await
            .expect("subscriber should get the suspended frame");
        assert!(frame.terminal);
        assert_eq!(frame.status, "suspended");
        assert_eq!(frame.task_context, fixture_context("paused"));

        assert!(
            recv_next(&mut receiver).await.is_none(),
            "stream should close after the suspended frame"
        );
    }

    #[tokio::test]
    async fn late_subscriber_after_publish_suspended_gets_the_cached_frame() {
        let run_id = Uuid::new_v4();
        publish_suspended(run_id, &fixture_context("already-suspended"));

        let mut receiver = subscribe(run_id);
        let frame = recv_next(&mut receiver)
            .await
            .expect("late subscriber should still get the suspended frame");
        assert!(frame.terminal);
        assert_eq!(frame.status, "suspended");

        assert!(recv_next(&mut receiver).await.is_none());
    }

    #[tokio::test]
    async fn clear_terminal_lets_a_resumed_run_stream_fresh_frames() {
        let run_id = Uuid::new_v4();
        publish_suspended(run_id, &fixture_context("paused"));

        clear_terminal(run_id);

        // A fresh subscribe must NOT see the stale suspended frame — it
        // should mint a brand-new live channel instead.
        let mut receiver = subscribe(run_id);
        publish(run_id, &fixture_context("resumed"));

        let frame = recv_next(&mut receiver)
            .await
            .expect("resumed run should deliver a fresh live frame");
        assert!(!frame.terminal);
        assert_eq!(frame.status, "running");
        assert_eq!(frame.task_context, fixture_context("resumed"));
    }

    #[tokio::test]
    async fn clear_terminal_on_unknown_run_is_a_no_op() {
        let run_id = Uuid::new_v4();
        // No prior publish/subscribe for this run_id at all.
        clear_terminal(run_id);

        // Registry should still behave normally afterward.
        let mut receiver = subscribe(run_id);
        publish(run_id, &fixture_context("still-works"));
        let frame = recv_next(&mut receiver).await.expect("frame");
        assert_eq!(frame.task_context, fixture_context("still-works"));
    }

    #[tokio::test]
    async fn publish_with_no_subscribers_does_not_panic() {
        let run_id = Uuid::new_v4();
        // No `subscribe` call at all — `send` returning an error (zero
        // receivers) must be swallowed, not propagated as a panic.
        publish(run_id, &fixture_context("nobody listening"));
        publish_terminal(run_id, &fixture_context("nobody listening"));
    }

    // --- Task 3: NodeProgress as a distinct SSE event type ------------

    fn fixture_progress(node: &str, done: usize, total: usize) -> NodeProgress {
        NodeProgress::new(node, done, total)
    }

    #[test]
    fn state_and_progress_frames_are_distinguishable_by_event_type_alone() {
        // The acceptance criterion is literal: a consumer must be able to
        // tell the two SSE event shapes apart by their `event:` type, never
        // by inspecting or guessing from the `data:` payload. `encode_sse`
        // (state) carries no `event:` line at all — it must stay
        // byte-identical to its pre-task-3 shape, since an existing
        // consumer (`async_lifecycle.rs`) strips a literal `"data: "`
        // prefix off every frame — so its SSE event type is the spec
        // default, `message`. `encode_progress_sse` carries an explicit
        // `event: progress` line. A consumer distinguishes the two by
        // whether an `event:` line is present/what it names, never by
        // parsing the JSON payload.
        let state_frame = StreamFrame {
            event_id: Uuid::new_v4(),
            status: "running".to_string(),
            task_context: fixture_context("marker"),
            terminal: false,
        };
        let progress_frame = ProgressFrame {
            event_id: Uuid::new_v4(),
            node: "capture".to_string(),
            done: 3,
            total: 7,
            label: None,
        };

        let state_bytes = encode_sse(&state_frame);
        let progress_bytes = encode_progress_sse(&progress_frame);
        let state_text = String::from_utf8(state_bytes.to_vec()).expect("utf8");
        let progress_text = String::from_utf8(progress_bytes.to_vec()).expect("utf8");

        assert!(
            !state_text.contains("event:"),
            "state frames carry no event: line (default SSE type `message`), got: {state_text}"
        );
        assert!(
            progress_text.starts_with("event: progress\n"),
            "got: {progress_text}"
        );
        assert_ne!(state_text, progress_text);

        // The progress payload never carries a `task_context` key — a
        // consumer distinguishing by event type never needs to check this,
        // but it also rules out a payload-shape ambiguity between the two.
        assert!(!progress_text.contains("task_context"));
    }

    #[tokio::test]
    async fn publish_progress_delivers_records_in_order_to_a_subscriber() {
        let run_id = Uuid::new_v4();
        let mut receiver = subscribe_progress(run_id);

        publish_progress(run_id, &fixture_progress("capture", 1, 7));
        publish_progress(run_id, &fixture_progress("capture", 2, 7));

        let first = recv_next_progress(&mut receiver).await.expect("frame 1");
        assert_eq!(first.event_id, run_id);
        assert_eq!(first.node, "capture");
        assert_eq!(first.done, 1);
        assert_eq!(first.total, 7);

        let second = recv_next_progress(&mut receiver).await.expect("frame 2");
        assert_eq!(second.done, 2);
    }

    #[tokio::test]
    async fn progress_publish_with_no_subscribers_does_not_panic() {
        let run_id = Uuid::new_v4();
        publish_progress(run_id, &fixture_progress("capture", 1, 1));
    }

    #[tokio::test]
    async fn a_reconnecting_client_after_terminal_gets_no_progress_backfill() {
        let run_id = Uuid::new_v4();
        let mut receiver = subscribe_progress(run_id);
        publish_progress(run_id, &fixture_progress("capture", 1, 2));
        publish_terminal(run_id, &fixture_context("done"));

        // The one update published before the run finished still reaches an
        // already-connected subscriber (this isn't a "no progress at all"
        // guarantee, only "no backfill for a NEW subscriber") — drain it.
        let already_seen = recv_next_progress(&mut receiver)
            .await
            .expect("the pre-terminal update should still reach an already-listening subscriber");
        assert_eq!(already_seen.done, 1);

        // Nothing further follows it — the progress channel closes with the
        // run rather than delivering a terminal frame of its own.
        assert!(
            recv_next_progress(&mut receiver).await.is_none(),
            "progress channel should close, not deliver a terminal frame"
        );

        // A client that only connects *after* the run has gone terminal
        // must not receive the earlier progress update — no backfill.
        let mut late_receiver = subscribe_progress(run_id);
        assert!(
            recv_next_progress(&mut late_receiver).await.is_none(),
            "late progress subscriber must see an already-closed channel, not a backfill"
        );
    }

    #[tokio::test]
    async fn progress_channel_closes_when_run_goes_suspended() {
        let run_id = Uuid::new_v4();
        let mut receiver = subscribe_progress(run_id);
        publish_suspended(run_id, &fixture_context("paused"));

        assert!(
            recv_next_progress(&mut receiver).await.is_none(),
            "progress channel should close when the run suspends"
        );
    }
}
