//! `DEBRIEF` — a morning brief rendered from a finished campaign's journal
//! rows (`EN.12.G`).
//!
//! Task 1 of this block lands the `JournalReader` seam alone: `engine-core`
//! depends ONLY on `engine-contract` (`crates/engine-core/Cargo.toml`), so
//! it cannot call `engine_store::list_journal_rows_for_campaign` directly —
//! that function lives behind `engine-serve`, which depends on all three
//! crates. The debrief therefore needs an injectable read seam, exactly the
//! reason [`crate::nodes::brain_client::HttpGet`] and
//! [`crate::nodes::http_post::HttpPost`] are injectable rather than direct
//! calls.
//!
//! [`JournalReader`] is the trait; [`StubJournalReader`] is the hermetic
//! test double every debrief test runs against (the gated `cargo nextest`
//! suite never contacts Postgres, mirroring `StubHttpGet`/`StubHttpPost`).
//! The only production implementation lives in `engine-serve`
//! (`crate::journal::journal_reader_live`), wired in by `EN.12.G` task 4's
//! `register_debrief`.
//!
//! `DebriefNode` itself (campaign rows in, `CONTENT_PIPELINE` dispatched
//! over them, brief written back as a journal row) lands in task 3.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use engine_contract::JournalRow;
use uuid::Uuid;

/// The injectable journal-read seam: `rows_for_campaign(campaign_id)` -> the
/// campaign's journal rows on success, or an error string describing the
/// read failure. Patterned on [`crate::nodes::brain_client::HttpGet`]: a
/// trait so production code (`engine-serve`) reaches for a real
/// Postgres-backed reader while `engine-core` tests inject a
/// [`StubJournalReader`] instead.
#[async_trait]
pub trait JournalReader: Send + Sync {
    async fn rows_for_campaign(&self, campaign_id: &Uuid) -> Result<Vec<JournalRow>, String>;
}

/// Test-stub [`JournalReader`]: records the last campaign id it was asked
/// for and returns a configurable success/failure response, mirroring
/// [`crate::nodes::brain_client::StubHttpGet`]. The gated suite never
/// touches Postgres — every debrief test runs on this stub, never on a live
/// reader.
#[derive(Clone)]
pub struct StubJournalReader {
    last_campaign_id: Arc<Mutex<Option<Uuid>>>,
    result: Arc<Mutex<Result<Vec<JournalRow>, String>>>,
}

impl StubJournalReader {
    /// A stub that always succeeds with the given rows.
    #[must_use]
    pub fn succeeding(rows: Vec<JournalRow>) -> Self {
        Self {
            last_campaign_id: Arc::new(Mutex::new(None)),
            result: Arc::new(Mutex::new(Ok(rows))),
        }
    }

    /// A stub that always fails with the given message.
    #[must_use]
    pub fn failing(message: impl Into<String>) -> Self {
        Self {
            last_campaign_id: Arc::new(Mutex::new(None)),
            result: Arc::new(Mutex::new(Err(message.into()))),
        }
    }

    /// The campaign id the most recent call to [`JournalReader::rows_for_campaign`]
    /// was asked for, if any — lets a test assert on the outbound request
    /// shape, not just the returned rows.
    #[must_use]
    pub fn last_campaign_id(&self) -> Option<Uuid> {
        *self.last_campaign_id.lock().expect("stub mutex poisoned")
    }
}

#[async_trait]
impl JournalReader for StubJournalReader {
    async fn rows_for_campaign(&self, campaign_id: &Uuid) -> Result<Vec<JournalRow>, String> {
        *self.last_campaign_id.lock().expect("stub mutex poisoned") = Some(*campaign_id);
        self.result.lock().expect("stub mutex poisoned").clone()
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use engine_contract::JournalDecisionKind;

    use super::*;

    fn sample_row() -> JournalRow {
        JournalRow {
            id: Uuid::new_v4(),
            campaign_id: "campaign-1".to_string(),
            run_id: Uuid::new_v4(),
            step: "build".to_string(),
            kind: JournalDecisionKind::StepIntegrated,
            reason: "ok".to_string(),
            detail: serde_json::json!({}),
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn succeeding_stub_returns_configured_rows_and_records_campaign_id() {
        let campaign_id = Uuid::new_v4();
        let rows = vec![sample_row()];
        let stub = StubJournalReader::succeeding(rows.clone());

        let result = stub.rows_for_campaign(&campaign_id).await;

        assert_eq!(result, Ok(rows));
        assert_eq!(stub.last_campaign_id(), Some(campaign_id));
    }

    #[tokio::test]
    async fn failing_stub_returns_configured_error_and_records_campaign_id() {
        let campaign_id = Uuid::new_v4();
        let stub = StubJournalReader::failing("journal read failed");

        let result = stub.rows_for_campaign(&campaign_id).await;

        assert_eq!(result, Err("journal read failed".to_string()));
        assert_eq!(stub.last_campaign_id(), Some(campaign_id));
    }

    #[tokio::test]
    async fn stub_records_the_most_recent_campaign_id_across_calls() {
        let stub = StubJournalReader::succeeding(vec![]);
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();

        let _ = stub.rows_for_campaign(&first).await;
        let _ = stub.rows_for_campaign(&second).await;

        assert_eq!(stub.last_campaign_id(), Some(second));
    }
}
