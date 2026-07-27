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

use std::collections::HashMap as StdHashMap;
use std::sync::{OnceLock, RwLock};

use actix_web::{web, HttpRequest, HttpResponse, Responder};
use engine_contract::TaskContext;
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
        })
    })
}

/// Get-or-create the live sender for `run_id`.
fn sender_for(run_id: RunId) -> broadcast::Sender<StreamFrame> {
    {
        let guard = registry().read().expect("stream registry lock poisoned on read");
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

/// Encode one [`StreamFrame`] as a single SSE `data:` event.
fn encode_sse(frame: &StreamFrame) -> web::Bytes {
    let payload = serde_json::to_string(frame).unwrap_or_else(|_| "{}".to_string());
    web::Bytes::from(format!("data: {payload}\n\n"))
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

    let known = state.live.get(event_id).is_some() || state.live.get_record(event_id).is_some();
    if !known {
        return HttpResponse::NotFound()
            .json(serde_json::json!({ "error": "unknown or malformed event_id" }));
    }

    let receiver = subscribe(event_id);
    let body = stream::unfold(receiver, |mut receiver| async move {
        recv_next(&mut receiver)
            .await
            .map(|frame| (Ok::<_, actix_web::Error>(encode_sse(&frame)), receiver))
    });

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
    async fn publish_with_no_subscribers_does_not_panic() {
        let run_id = Uuid::new_v4();
        // No `subscribe` call at all — `send` returning an error (zero
        // receivers) must be swallowed, not propagated as a panic.
        publish(run_id, &fixture_context("nobody listening"));
        publish_terminal(run_id, &fixture_context("nobody listening"));
    }
}
