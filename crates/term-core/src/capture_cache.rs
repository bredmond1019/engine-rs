//! `capture_cache.rs` — the short-TTL, single-flight `capture_pane` cache
//! (`EN.9.B` task 3).
//!
//! Two independent callers legitimately want the same session's pane
//! contents close together in time: the hub's periodic sweep (every ~2s)
//! and a node's own await loop polling for a prompt to settle. Without
//! coalescing, both spawn their own `tmux capture-pane` invocation — twice
//! the process spawns for the same answer. This cache collapses concurrent
//! and closely-spaced callers onto one underlying capture per session.
//!
//! Single-flight is implemented by keying a `tokio::sync::Mutex` per
//! session: the first caller for a cold (or expired) entry holds the lock
//! for the duration of the underlying capture, and every other caller
//! blocks on the SAME lock rather than racing a capture of its own. When
//! the lock is released the entry is fresh, so a caller that was waiting
//! observes a cache hit rather than issuing its own invocation.
//!
//! Behind the non-default `tokio` feature — `bastion` consumes `term-core`
//! blocking-only and must keep paying nothing for this module.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use tokio::sync::Mutex as AsyncMutex;

use crate::tmux::TmuxError;

/// Default TTL for a cached `capture_pane` result. The hub sweep (~2s) and a
/// node's await loop both fall well inside this window, so a captured pane
/// coalesces both without going stale for either.
///
/// Named constant with a per-instance override — see [`CaptureCache::with_ttl`].
pub const DEFAULT_CAPTURE_TTL: Duration = Duration::from_millis(400);

/// One session's cached capture: the last successful pane text plus when it
/// was captured. `None` means cold — never captured, or evicted.
#[derive(Debug)]
struct CacheSlot {
    entry: Option<(String, Instant)>,
}

/// A per-session, short-TTL, single-flight cache over `capture_pane`.
///
/// Cheap to clone — internally an `Arc` over the shared session map, so a
/// `TmuxDriver` and anything else that needs to observe the same cache can
/// hold their own handle.
#[derive(Debug, Clone)]
pub struct CaptureCache {
    ttl: Duration,
    sessions: Arc<StdMutex<HashMap<String, Arc<AsyncMutex<CacheSlot>>>>>,
}

impl Default for CaptureCache {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureCache {
    /// A cache using [`DEFAULT_CAPTURE_TTL`].
    #[must_use]
    pub fn new() -> Self {
        Self::with_ttl(DEFAULT_CAPTURE_TTL)
    }

    /// A cache with an explicit TTL — the override a test (or a future
    /// policy knob) reaches for instead of the named default.
    #[must_use]
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            ttl,
            sessions: Arc::new(StdMutex::new(HashMap::new())),
        }
    }

    /// Fetch `session_name`'s cached capture if it exists and is still
    /// fresh, otherwise run `capture` to produce a fresh one and cache it.
    ///
    /// Concurrent callers for the SAME session on a cold (or expired) entry
    /// serialize on that session's lock: the first caller performs the
    /// underlying capture and populates the entry; every other caller then
    /// observes the fresh entry and returns it without calling `capture`
    /// itself. Concurrent callers for DIFFERENT sessions never contend —
    /// each session has its own lock.
    ///
    /// A failed capture is never cached — the next caller retries rather
    /// than being pinned to a stale error.
    pub async fn get_or_capture<F, Fut>(
        &self,
        session_name: &str,
        capture: F,
    ) -> Result<String, TmuxError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<String, TmuxError>>,
    {
        let slot = {
            let mut sessions = self.sessions.lock().unwrap();
            Arc::clone(
                sessions
                    .entry(session_name.to_string())
                    .or_insert_with(|| Arc::new(AsyncMutex::new(CacheSlot { entry: None }))),
            )
        };

        // Held across the (possible) await below — this IS the single-flight
        // arbitration. A concurrent caller for this session blocks here
        // rather than racing its own `capture()`.
        let mut slot = slot.lock().await;

        if let Some((value, captured_at)) = &slot.entry {
            if captured_at.elapsed() < self.ttl {
                return Ok(value.clone());
            }
        }

        let fresh = capture().await?;
        slot.entry = Some((fresh.clone(), Instant::now()));
        Ok(fresh)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn hit_inside_ttl_performs_no_second_capture() {
        let cache = CaptureCache::with_ttl(Duration::from_secs(10));
        let calls = Arc::new(AtomicUsize::new(0));

        for _ in 0..3 {
            let calls = Arc::clone(&calls);
            let result = cache
                .get_or_capture("proj-abc", || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok("pane text".to_string())
                })
                .await
                .unwrap();
            assert_eq!(result, "pane text");
        }

        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "expected exactly one underlying capture for three calls inside the TTL"
        );
    }

    #[tokio::test]
    async fn entry_past_ttl_recaptures() {
        let cache = CaptureCache::with_ttl(Duration::from_millis(20));
        let calls = Arc::new(AtomicUsize::new(0));

        let mk_capture = |calls: Arc<AtomicUsize>, text: &'static str| {
            move || {
                let calls = Arc::clone(&calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, TmuxError>(text.to_string())
                }
            }
        };

        let first = cache
            .get_or_capture("proj-abc", mk_capture(Arc::clone(&calls), "first"))
            .await
            .unwrap();
        assert_eq!(first, "first");

        tokio::time::sleep(Duration::from_millis(40)).await;

        let second = cache
            .get_or_capture("proj-abc", mk_capture(Arc::clone(&calls), "second"))
            .await
            .unwrap();
        assert_eq!(second, "second");

        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "expected a re-capture once the entry aged past the TTL"
        );
    }

    #[tokio::test]
    async fn n_concurrent_cold_callers_yield_one_capture_and_identical_results() {
        let cache = CaptureCache::with_ttl(Duration::from_secs(10));
        let calls = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..8 {
            let cache = cache.clone();
            let calls = Arc::clone(&calls);
            handles.push(tokio::spawn(async move {
                cache
                    .get_or_capture("proj-abc", || async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        // Give other spawned callers a chance to queue up
                        // behind the per-session lock while this "capture"
                        // is in flight.
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        Ok("coalesced".to_string())
                    })
                    .await
            }));
        }

        let mut results = Vec::new();
        for h in handles {
            results.push(h.await.unwrap().unwrap());
        }

        assert_eq!(calls.load(Ordering::SeqCst), 1, "expected single-flight");
        assert!(
            results.iter().all(|r| r == "coalesced"),
            "expected every caller to observe the identical result: {results:?}"
        );
        assert_eq!(results.len(), 8);
    }

    #[tokio::test]
    async fn different_sessions_do_not_share_an_entry() {
        let cache = CaptureCache::with_ttl(Duration::from_secs(10));

        let a = cache
            .get_or_capture("session-a", || async { Ok("a-text".to_string()) })
            .await
            .unwrap();
        let b = cache
            .get_or_capture("session-b", || async { Ok("b-text".to_string()) })
            .await
            .unwrap();

        assert_eq!(a, "a-text");
        assert_eq!(b, "b-text");
    }

    #[tokio::test]
    async fn a_failed_capture_is_never_cached() {
        let cache = CaptureCache::with_ttl(Duration::from_secs(10));
        let calls = Arc::new(AtomicUsize::new(0));

        let first = {
            let calls = Arc::clone(&calls);
            cache
                .get_or_capture("proj-abc", || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Err::<String, _>(TmuxError::NoServer)
                })
                .await
        };
        assert!(matches!(first, Err(TmuxError::NoServer)));

        let second = {
            let calls = Arc::clone(&calls);
            cache
                .get_or_capture("proj-abc", || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok("recovered".to_string())
                })
                .await
        };
        assert_eq!(second.unwrap(), "recovered");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "a failed capture must not be cached — the next caller must retry"
        );
    }

    #[test]
    fn default_ttl_is_the_named_constant() {
        // `CaptureCache::new()` uses DEFAULT_CAPTURE_TTL rather than a bare
        // literal, and DEFAULT_CAPTURE_TTL itself sits comfortably under the
        // hub's ~2s sweep interval so the sweep and a node await loop
        // coalesce rather than each seeing a stale-past-TTL entry.
        assert_eq!(DEFAULT_CAPTURE_TTL, Duration::from_millis(400));
        assert!(DEFAULT_CAPTURE_TTL < Duration::from_secs(2));
    }
}
