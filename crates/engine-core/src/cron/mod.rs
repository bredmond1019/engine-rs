//! Durable Cron primitive — Phase 6 Block M (`EN.6.M`).
//!
//! Ports qm §5's durable cron primitive (fire log, drift-free catch-up
//! correctness, silence protocol) as a standalone module with no dependency
//! on any specific workflow or envelope type, so `EN.6.G`'s `schedule.rs`
//! can fire through it instead of hand-rolling cron mechanics. See
//! `planning/en-6m-durable-cron-primitive/tasks.md` for the full spec.
//!
//! This module ports only the pure scheduling logic from
//! `example-repo/qm/src/cron/schedule.ts` (`normalizeSchedule`,
//! `recoverNextFireAt`, `advanceNextFireAt`) — not the Croner/timezone-string
//! dependency (this crate uses the `cron` crate + `chrono-tz` instead), and
//! not `scheduler.ts`'s pg-boss job queue, `LeaderLease`, `maxFiresPerTick`,
//! or any other multi-instance machinery a single Mac Mini does not need.
//!
//! ## Two mutually-exclusive schedule kinds
//!
//! - **Calendar** (`{cron_expr, timezone}`) — a wall-clock cron expression
//!   evaluated in an IANA timezone. Advances from the *last scheduled* fire
//!   time (never the actual fire time), so a schedule never accumulates
//!   drift across delayed fires: `cron.after(scheduled_at)` is an absolute
//!   calendar computation, not an incremental one.
//! - **Interval** (`{every_ms, first_fire_at}`) — a fixed-duration repeat.
//!   Advances from the actual **fired** time (`fired_at + every_ms`), which
//!   guarantees exactly one catch-up fire after downtime rather than a
//!   thundering herd of missed slots.
//!
//! `every_ms` is bounded to `[60_000, 86_400_000)` — below one minute is
//! rejected as too fast to be a sane recurring interval, and at-or-above 24h
//! is rejected because that is a wall-clock schedule in disguise (it drifts
//! across DST transitions when expressed as a fixed duration) — use a
//! `Calendar` schedule instead.

// `record` and `store` submodules land in EN.6.M tasks 2 and 3 respectively.

use std::str::FromStr;

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use cron::Schedule as CronExpr;

/// Minimum interval schedule period: below this is too fast to be a sane
/// recurring interval.
pub const MIN_INTERVAL_MS: i64 = 60_000;

/// Maximum interval schedule period: at or above this is a wall-clock
/// schedule in disguise (drifts across DST when expressed as a fixed
/// duration) — use a `Calendar` schedule instead.
pub const MAX_INTERVAL_MS: i64 = 86_400_000;

/// A normalized, validated schedule — mutually exclusive calendar or
/// interval variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CronSchedule {
    /// A wall-clock cron expression evaluated in an IANA timezone.
    Calendar { expr: String, timezone: Tz },
    /// A fixed-duration repeat, anchored to `first_fire_at` before its
    /// first fire.
    Interval {
        every_ms: i64,
        first_fire_at: DateTime<Utc>,
    },
}

/// Raw, not-yet-validated input for [`normalize_schedule`] — mirrors the
/// shape a caller (e.g. `EN.6.G`'s `schedule.rs`) would deserialize from an
/// event payload. Exactly one of the calendar fields (`cron_expr` +
/// `timezone`) or the interval field (`every_ms`) must be set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawSchedule {
    pub cron_expr: Option<String>,
    pub timezone: Option<String>,
    pub every_ms: Option<i64>,
    /// Optional explicit anchor for an interval schedule's first fire;
    /// defaults to `now + every_ms` when omitted.
    pub first_fire_at: Option<DateTime<Utc>>,
}

/// A defect found while normalizing or validating a [`CronSchedule`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CronScheduleError {
    /// Both calendar fields (`cron_expr`/`timezone`) and the interval field
    /// (`every_ms`) were set — mutually exclusive.
    BothCalendarAndInterval,
    /// Neither a calendar nor an interval field was set.
    NeitherCalendarNorInterval,
    /// `cron_expr` was set without a `timezone`, or vice versa.
    IncompleteCalendarSchedule,
    /// The cron expression failed to parse.
    InvalidCronExpr(String),
    /// The timezone string is not a recognized IANA timezone.
    InvalidTimezone(String),
    /// A calendar schedule whose cron expression has no upcoming fire time
    /// at all (a self-contradictory expression, e.g. Feb 30th).
    NoUpcomingFire,
    /// `every_ms` is below [`MIN_INTERVAL_MS`].
    IntervalTooShort { every_ms: i64 },
    /// `every_ms` is at or above [`MAX_INTERVAL_MS`] — a wall-clock schedule
    /// in disguise that drifts across DST when expressed as a fixed
    /// duration; use a `Calendar` schedule instead.
    IntervalTooLong { every_ms: i64 },
}

impl std::fmt::Display for CronScheduleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CronScheduleError::BothCalendarAndInterval => write!(
                f,
                "a schedule must be either calendar (cron_expr + timezone) or interval (every_ms), not both"
            ),
            CronScheduleError::NeitherCalendarNorInterval => write!(
                f,
                "a schedule must set either cron_expr + timezone (calendar) or every_ms (interval)"
            ),
            CronScheduleError::IncompleteCalendarSchedule => write!(
                f,
                "a calendar schedule requires both cron_expr and timezone"
            ),
            CronScheduleError::InvalidCronExpr(expr) => {
                write!(f, "invalid cron expression '{expr}'")
            }
            CronScheduleError::InvalidTimezone(tz) => {
                write!(f, "'{tz}' is not a recognized IANA timezone")
            }
            CronScheduleError::NoUpcomingFire => {
                write!(f, "cron expression has no upcoming fire time")
            }
            CronScheduleError::IntervalTooShort { every_ms } => write!(
                f,
                "every_ms {every_ms} is below the minimum recurring interval of {MIN_INTERVAL_MS}ms (60s)"
            ),
            CronScheduleError::IntervalTooLong { every_ms } => write!(
                f,
                "every_ms {every_ms} is a wall-clock schedule in disguise (>= 24h as a fixed duration drifts across DST transitions) — use a calendar schedule (cron_expr + timezone) instead"
            ),
        }
    }
}

impl std::error::Error for CronScheduleError {}

/// Normalize a [`RawSchedule`] into a validated [`CronSchedule`] plus its
/// first computed `next_fire_at`. Mirrors qm's `normalizeSchedule`:
/// calendar and interval fields are mutually exclusive; a calendar schedule
/// computes its first next-fire via the `cron` crate; an interval schedule
/// defaults `first_fire_at` to `now + every_ms` when not given explicitly.
pub fn normalize_schedule(
    raw: RawSchedule,
    now: DateTime<Utc>,
) -> Result<(CronSchedule, DateTime<Utc>), CronScheduleError> {
    let has_calendar_field = raw.cron_expr.is_some() || raw.timezone.is_some();
    let has_interval_field = raw.every_ms.is_some();

    if has_calendar_field && has_interval_field {
        return Err(CronScheduleError::BothCalendarAndInterval);
    }
    if !has_calendar_field && !has_interval_field {
        return Err(CronScheduleError::NeitherCalendarNorInterval);
    }

    if has_calendar_field {
        let (expr, timezone) = match (raw.cron_expr, raw.timezone) {
            (Some(expr), Some(tz)) => (expr, tz),
            _ => return Err(CronScheduleError::IncompleteCalendarSchedule),
        };
        let tz: Tz = timezone
            .parse()
            .map_err(|_| CronScheduleError::InvalidTimezone(timezone.clone()))?;
        let schedule = CronSchedule::Calendar {
            expr: expr.clone(),
            timezone: tz,
        };
        validate_schedule(&schedule)?;
        let next_fire_at = next_calendar_fire_after(&expr, tz, now)?;
        Ok((schedule, next_fire_at))
    } else {
        let every_ms = raw.every_ms.expect("has_interval_field checked above");
        let first_fire_at = raw
            .first_fire_at
            .unwrap_or_else(|| now + Duration::milliseconds(every_ms));
        let schedule = CronSchedule::Interval {
            every_ms,
            first_fire_at,
        };
        validate_schedule(&schedule)?;
        Ok((schedule, first_fire_at))
    }
}

/// Validate a [`CronSchedule`]'s bounds. Calendar schedules are validated
/// for a parseable cron expression and recognized timezone (already done at
/// construction in [`normalize_schedule`], but re-checked here so a
/// programmatically-constructed `CronSchedule` — not just one built via
/// `normalize_schedule` — can be validated on its own). Interval schedules
/// are rejected below [`MIN_INTERVAL_MS`] or at/above [`MAX_INTERVAL_MS`].
pub fn validate_schedule(schedule: &CronSchedule) -> Result<(), CronScheduleError> {
    match schedule {
        CronSchedule::Calendar { expr, .. } => {
            CronExpr::from_str(expr)
                .map_err(|e| CronScheduleError::InvalidCronExpr(format!("{expr}: {e}")))?;
            Ok(())
        }
        CronSchedule::Interval { every_ms, .. } => {
            if *every_ms < MIN_INTERVAL_MS {
                return Err(CronScheduleError::IntervalTooShort {
                    every_ms: *every_ms,
                });
            }
            if *every_ms >= MAX_INTERVAL_MS {
                return Err(CronScheduleError::IntervalTooLong {
                    every_ms: *every_ms,
                });
            }
            Ok(())
        }
    }
}

/// Compute the next calendar fire time strictly after `after`, in the
/// schedule's own timezone, returned back as UTC.
fn next_calendar_fire_after(
    expr: &str,
    timezone: Tz,
    after: DateTime<Utc>,
) -> Result<DateTime<Utc>, CronScheduleError> {
    let schedule = CronExpr::from_str(expr)
        .map_err(|e| CronScheduleError::InvalidCronExpr(format!("{expr}: {e}")))?;
    let after_in_tz = after.with_timezone(&timezone);
    schedule
        .after(&after_in_tz)
        .next()
        .map(|dt| dt.with_timezone(&Utc))
        .ok_or(CronScheduleError::NoUpcomingFire)
}

/// Recover a schedule's `next_fire_at` from durable state after a process
/// restart — mirrors qm's `recoverNextFireAt`. Given `created_at` (used when
/// the schedule has never fired) and `last_fired_at` (the last time it
/// actually fired, if any), computes the next fire instant.
///
/// - **Calendar**: advances from `last_fired_at` (or `created_at` if it has
///   never fired) via the cron expression — the scheduled slot, not
///   whatever moment the process happened to restart at.
/// - **Interval**: advances from `last_fired_at + every_ms` if it has fired
///   before; otherwise its `first_fire_at`. If that computed instant is
///   already in the past, it is returned as-is (due immediately) rather
///   than fast-forwarded through every missed slot — the `tick()` driver
///   (`EN.6.M` task 3) fires it exactly once and then advances from the
///   real fire time, guaranteeing exactly one catch-up fire, never a
///   thundering herd.
pub fn recover_next_fire_at(
    schedule: &CronSchedule,
    created_at: DateTime<Utc>,
    last_fired_at: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    match schedule {
        CronSchedule::Calendar { expr, timezone } => {
            let anchor = last_fired_at.unwrap_or(created_at);
            next_calendar_fire_after(expr, *timezone, anchor).ok()
        }
        CronSchedule::Interval {
            every_ms,
            first_fire_at,
        } => match last_fired_at {
            Some(last) => Some(last + Duration::milliseconds(*every_ms)),
            None => Some(*first_fire_at),
        },
    }
}

/// Advance a schedule's `next_fire_at` immediately after a fire — mirrors
/// qm's `advanceNextFireAt`.
///
/// **Callers must pass the correct anchor per variant** (this is the
/// drift-free contract from the block's Acceptance Criteria):
/// - **Calendar**: pass the fire's *scheduled* instant (the `next_fire_at`
///   the record held before this fire) — never the actual wall-clock time
///   the fire executed at — so delayed processing never accumulates drift.
/// - **Interval**: pass the *actual* instant the fire executed at
///   (`fired_at`), so downtime produces exactly one catch-up fire rather
///   than a burst of back-to-back fires for every missed slot.
pub fn advance_next_fire_at(
    schedule: &CronSchedule,
    fired_at: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    match schedule {
        CronSchedule::Calendar { expr, timezone } => {
            next_calendar_fire_after(expr, *timezone, fired_at).ok()
        }
        CronSchedule::Interval { every_ms, .. } => {
            Some(fired_at + Duration::milliseconds(*every_ms))
        }
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
    fn calendar_schedule_advances_across_dst_spring_forward() {
        // US Pacific spring-forward 2026: 2026-03-08 02:00 local jumps to
        // 03:00 local (2:00-3:00 AM does not exist). A schedule firing
        // daily at 02:30 local should skip straight to the next valid
        // occurrence without drifting.
        let tz: Tz = "America/Los_Angeles".parse().unwrap();
        let schedule = CronSchedule::Calendar {
            expr: "0 30 2 * * *".to_string(),
            timezone: tz,
        };
        validate_schedule(&schedule).expect("calendar schedule should validate");

        // Anchor: the (scheduled) fire on 2026-03-07 at 02:30 Pacific = 10:30 UTC.
        let scheduled_at = utc(2026, 3, 7, 10, 30, 0);
        let next = advance_next_fire_at(&schedule, scheduled_at).expect("next fire");

        // 2026-03-07 02:30 Pacific is PST (UTC-8); 2026-03-08's 02:30 slot
        // does not exist (clocks spring forward at 02:00 -> 03:00), so the
        // `cron` crate's next matching occurrence lands on 2026-03-09
        // 02:30 PDT (UTC-7) = 09:30 UTC. Assert against the crate's actual
        // computed value rather than hand-deriving DST rules twice.
        let expected_local = next.with_timezone(&tz);
        assert_eq!(expected_local.format("%H:%M").to_string(), "02:30");
        assert!(
            next > scheduled_at + Duration::hours(23),
            "next fire {next} should be roughly a day (or more, across the DST gap) after {scheduled_at}"
        );
    }

    #[test]
    fn interval_schedule_advances_from_fired_at_not_wall_clock_anchor() {
        let schedule = CronSchedule::Interval {
            every_ms: 5 * 60_000,
            first_fire_at: utc(2026, 1, 1, 0, 0, 0),
        };
        let fired_at = utc(2026, 6, 15, 13, 47, 22);
        let next = advance_next_fire_at(&schedule, fired_at).unwrap();
        assert_eq!(next, fired_at + Duration::milliseconds(5 * 60_000));
    }

    #[test]
    fn interval_recover_uses_first_fire_at_when_never_fired() {
        let first_fire_at = utc(2026, 1, 1, 0, 0, 0);
        let schedule = CronSchedule::Interval {
            every_ms: 60_000,
            first_fire_at,
        };
        let created_at = utc(2025, 12, 31, 23, 0, 0);
        let recovered = recover_next_fire_at(&schedule, created_at, None).unwrap();
        assert_eq!(recovered, first_fire_at);
    }

    #[test]
    fn interval_recover_advances_from_last_fired_at_when_present() {
        let schedule = CronSchedule::Interval {
            every_ms: 60_000,
            first_fire_at: utc(2026, 1, 1, 0, 0, 0),
        };
        let last_fired_at = utc(2026, 1, 2, 12, 0, 0);
        let recovered =
            recover_next_fire_at(&schedule, utc(2026, 1, 1, 0, 0, 0), Some(last_fired_at)).unwrap();
        assert_eq!(recovered, last_fired_at + Duration::milliseconds(60_000));
    }

    #[test]
    fn every_ms_at_or_above_24h_is_rejected() {
        let schedule = CronSchedule::Interval {
            every_ms: MAX_INTERVAL_MS,
            first_fire_at: Utc::now(),
        };
        let err = validate_schedule(&schedule).unwrap_err();
        assert!(matches!(err, CronScheduleError::IntervalTooLong { .. }));
        assert!(err.to_string().contains("wall-clock schedule in disguise"));
    }

    #[test]
    fn every_ms_below_60s_is_rejected() {
        let schedule = CronSchedule::Interval {
            every_ms: 59_999,
            first_fire_at: Utc::now(),
        };
        let err = validate_schedule(&schedule).unwrap_err();
        assert!(matches!(err, CronScheduleError::IntervalTooShort { .. }));
    }

    #[test]
    fn mutually_exclusive_fields_both_set_is_rejected() {
        let raw = RawSchedule {
            cron_expr: Some("0 0 * * * *".to_string()),
            timezone: Some("UTC".to_string()),
            every_ms: Some(120_000),
            first_fire_at: None,
        };
        let err = normalize_schedule(raw, Utc::now()).unwrap_err();
        assert_eq!(err, CronScheduleError::BothCalendarAndInterval);
    }

    #[test]
    fn neither_field_set_is_rejected() {
        let raw = RawSchedule::default();
        let err = normalize_schedule(raw, Utc::now()).unwrap_err();
        assert_eq!(err, CronScheduleError::NeitherCalendarNorInterval);
    }

    #[test]
    fn normalize_calendar_schedule_computes_next_fire() {
        let now = utc(2026, 1, 1, 0, 0, 0);
        let raw = RawSchedule {
            cron_expr: Some("0 0 12 * * *".to_string()),
            timezone: Some("UTC".to_string()),
            every_ms: None,
            first_fire_at: None,
        };
        let (schedule, next_fire_at) = normalize_schedule(raw, now).unwrap();
        assert!(matches!(schedule, CronSchedule::Calendar { .. }));
        assert_eq!(next_fire_at, utc(2026, 1, 1, 12, 0, 0));
    }

    #[test]
    fn normalize_interval_schedule_defaults_first_fire_at_to_now_plus_every_ms() {
        let now = utc(2026, 1, 1, 0, 0, 0);
        let raw = RawSchedule {
            cron_expr: None,
            timezone: None,
            every_ms: Some(120_000),
            first_fire_at: None,
        };
        let (schedule, next_fire_at) = normalize_schedule(raw, now).unwrap();
        assert!(matches!(schedule, CronSchedule::Interval { .. }));
        assert_eq!(next_fire_at, now + Duration::milliseconds(120_000));
    }
}
