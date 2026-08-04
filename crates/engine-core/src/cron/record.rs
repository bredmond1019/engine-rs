//! `CronRecord` + fire log + silence protocol — Phase 6 Block M (`EN.6.M`)
//! task 2.
//!
//! A [`CronRecord`] is the durable state of one scheduled entry: its
//! [`CronSchedule`] (see [`super::CronSchedule`]), when it was created,
//! whether it is enabled, and its last/next fire times. A fire produces a
//! [`FireOutcome`] — either [`FireOutcome::Reported`] (something happened,
//! carrying a note describing what) or [`FireOutcome::Silent`] (the
//! *silence protocol*: nothing to report, so no note or downstream output
//! is produced). The [`From<&FireOutcome>`] conversion into
//! [`CronFireLogEntry`] is the enforcement point: a `Silent` outcome can
//! never produce a log entry carrying a note, by construction.

use chrono::{DateTime, Utc};

use super::CronSchedule;

/// The durable state of one scheduled cron entry.
#[derive(Debug, Clone, PartialEq)]
pub struct CronRecord {
    pub id: String,
    pub schedule: CronSchedule,
    pub created_at: DateTime<Utc>,
    pub enabled: bool,
    pub last_fired_at: Option<DateTime<Utc>>,
    pub next_fire_at: Option<DateTime<Utc>>,
}

/// The result of one fire attempt.
///
/// The silence protocol: a fire that has nothing to report is `Silent` and
/// must not surface any note or downstream output — this is enforced at the
/// `CronFireLogEntry` conversion, not left to callers to remember.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FireOutcome {
    /// Something happened; carries a human-readable note describing it.
    Reported(String),
    /// Nothing to report this fire — the silence protocol. No note, no
    /// downstream output.
    Silent,
}

/// One append-only entry in a cron id's durable fire log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronFireLogEntry {
    pub fired_at: DateTime<Utc>,
    pub scheduled_at: Option<DateTime<Utc>>,
    /// `"reported"` or `"silent"` — a stable, distinguishable tag rather
    /// than re-deriving it from `note`'s presence at every read site.
    pub outcome_kind: &'static str,
    pub note: Option<String>,
}

impl CronFireLogEntry {
    /// Build a fire log entry from a computed [`FireOutcome`] plus the
    /// fire's timing. This is the single seam through which an outcome
    /// becomes a durable record — see [`From<&FireOutcome>`] for the
    /// silence-protocol enforcement.
    pub fn new(
        outcome: &FireOutcome,
        fired_at: DateTime<Utc>,
        scheduled_at: Option<DateTime<Utc>>,
    ) -> Self {
        let (outcome_kind, note) = fire_outcome_kind_and_note(outcome);
        CronFireLogEntry {
            fired_at,
            scheduled_at,
            outcome_kind,
            note,
        }
    }
}

/// Maps a [`FireOutcome`] to its `(outcome_kind, note)` pair. `Silent`
/// always maps to `("silent", None)` — there is no code path by which a
/// silent fire's note (there isn't one) could leak into the log, since the
/// variant itself carries no message.
fn fire_outcome_kind_and_note(outcome: &FireOutcome) -> (&'static str, Option<String>) {
    match outcome {
        FireOutcome::Silent => ("silent", None),
        FireOutcome::Reported(msg) => ("reported", Some(msg.clone())),
    }
}

impl From<&FireOutcome> for CronFireLogEntry {
    /// Convert an outcome into a log entry stamped at "now", with no known
    /// scheduled time. Prefer [`CronFireLogEntry::new`] when the fire's
    /// actual/scheduled timestamps are available (the normal `tick()`
    /// path); this impl exists for call sites that only have the outcome
    /// itself.
    fn from(outcome: &FireOutcome) -> Self {
        CronFireLogEntry::new(outcome, Utc::now(), None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    #[test]
    fn silent_outcome_produces_log_entry_with_no_note() {
        let entry = CronFireLogEntry::new(
            &FireOutcome::Silent,
            utc(2026, 1, 1, 0, 0, 0),
            Some(utc(2026, 1, 1, 0, 0, 0)),
        );
        assert_eq!(entry.outcome_kind, "silent");
        assert_eq!(entry.note, None);
    }

    #[test]
    fn reported_outcome_round_trips_its_message() {
        let outcome = FireOutcome::Reported("3 new items".to_string());
        let entry = CronFireLogEntry::new(&outcome, utc(2026, 1, 1, 0, 0, 0), None);
        assert_eq!(entry.outcome_kind, "reported");
        assert_eq!(entry.note, Some("3 new items".to_string()));
    }

    #[test]
    fn silent_and_reported_are_distinguishable_by_outcome_kind() {
        let silent = CronFireLogEntry::new(&FireOutcome::Silent, Utc::now(), None);
        let reported = CronFireLogEntry::new(
            &FireOutcome::Reported("hello".to_string()),
            Utc::now(),
            None,
        );
        assert_ne!(silent.outcome_kind, reported.outcome_kind);
        assert_eq!(silent.outcome_kind, "silent");
        assert_eq!(reported.outcome_kind, "reported");
    }

    #[test]
    fn from_ref_conversion_preserves_silence_protocol() {
        let entry: CronFireLogEntry = (&FireOutcome::Silent).into();
        assert_eq!(entry.outcome_kind, "silent");
        assert_eq!(entry.note, None);
    }

    #[test]
    fn cron_record_holds_expected_fields() {
        let schedule = CronSchedule::Interval {
            every_ms: 60_000,
            first_fire_at: utc(2026, 1, 1, 0, 0, 0),
        };
        let record = CronRecord {
            id: "test-cron".to_string(),
            schedule: schedule.clone(),
            created_at: utc(2025, 12, 31, 0, 0, 0),
            enabled: true,
            last_fired_at: None,
            next_fire_at: Some(utc(2026, 1, 1, 0, 0, 0)),
        };
        assert_eq!(record.id, "test-cron");
        assert!(record.enabled);
        assert_eq!(record.schedule, schedule);
    }
}
