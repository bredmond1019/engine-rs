---
type: Reference
title: engine-rs Durable Cron Primitive
description: The standalone CronSchedule/CronRecord/CronStore/tick() cron primitive (EN.6.M) — drift-free calendar and catch-up-safe interval scheduling, the silence protocol, and restart-durable file persistence.
doc_id: cron-primitive
layer: [engine]
project: engine-rs
status: active
keywords: [cron, schedule, CronSchedule, CronRecord, CronStore, tick, silence protocol, drift-free, catch-up, FileCronStore]
related: [architecture, docs-index]
---

# engine-rs — Durable Cron Primitive (`EN.6.M`)

`crates/engine-core/src/cron/` is a standalone scheduling primitive with no dependency on any
specific workflow or envelope type, so a future caller (`EN.6.G`'s `schedule.rs`) can fire through
it instead of hand-rolling cron mechanics. It ports the pure scheduling logic of qm §5's durable
cron design (fire log, drift-free catch-up correctness, silence protocol) using the `cron` crate +
`chrono-tz` in place of qm's Croner/timezone-string dependency, and omits qm's pg-boss job queue,
`LeaderLease`, and `maxFiresPerTick` — multi-instance machinery a single Mac Mini operator does not
need.

## Module layout

| File | Contents |
|---|---|
| `cron/mod.rs` | `CronSchedule` (Calendar/Interval), `RawSchedule`, `CronScheduleError`, and the pure scheduling functions: `normalize_schedule`, `validate_schedule`, `recover_next_fire_at`, `advance_next_fire_at` |
| `cron/record.rs` | `CronRecord` (durable per-entry state), `FireOutcome` (`Reported(String)`/`Silent`), `CronFireLogEntry`, and the `From<&FireOutcome>` conversion that enforces the silence protocol |
| `cron/store.rs` | The injectable `CronStore` trait, the restart-durable `FileCronStore` (whole-object JSON persistence), and the `tick()` driver |

## Two mutually-exclusive schedule kinds

`CronSchedule` is an enum with exactly two variants:

- **`Calendar { expr, timezone }`** — a wall-clock cron expression (via the `cron` crate)
  evaluated in an IANA timezone (`chrono_tz::Tz`). Advances from the *last scheduled* fire time
  (never the actual fire time), so a schedule never accumulates drift across delayed fires —
  `cron.after(scheduled_at)` is an absolute calendar computation, not an incremental one.
- **`Interval { every_ms, first_fire_at }`** — a fixed-duration repeat. Advances from the actual
  *fired* time (`fired_at + every_ms`), guaranteeing exactly one catch-up fire after downtime
  rather than a thundering herd of missed slots.

`every_ms` is bounded to `[MIN_INTERVAL_MS, MAX_INTERVAL_MS)` = `[60_000, 86_400_000)`: below one
minute is rejected as too fast (`CronScheduleError::IntervalTooShort`); at or above 24h is rejected
(`CronScheduleError::IntervalTooLong`) because a fixed duration that long drifts across DST
transitions — use a `Calendar` schedule instead.

## Core functions (`cron/mod.rs`)

- `normalize_schedule(raw: RawSchedule, now: DateTime<Utc>) -> Result<(CronSchedule, DateTime<Utc>), CronScheduleError>`
  — validates a `RawSchedule` (mirrors an event-payload shape: `cron_expr`/`timezone` XOR
  `every_ms`, with an optional `first_fire_at` for interval schedules, defaulting to
  `now + every_ms`) into a `CronSchedule` plus its first computed `next_fire_at`.
- `validate_schedule(schedule: &CronSchedule) -> Result<(), CronScheduleError>` — re-validates an
  already-constructed `CronSchedule` (parseable cron expression, `every_ms` bounds).
- `recover_next_fire_at(schedule, created_at, last_fired_at) -> Option<DateTime<Utc>>` — recomputes
  `next_fire_at` from durable state after a process restart. Calendar advances from
  `last_fired_at` (or `created_at` if never fired) via the cron expression; interval advances from
  `last_fired_at + every_ms` (or `first_fire_at` if never fired), returned as-is even if already
  due — `tick()` fires it exactly once and then advances from the real fire time.
- `advance_next_fire_at(schedule, fired_at) -> Option<DateTime<Utc>>` — advances `next_fire_at`
  immediately after a fire. **Callers must pass the correct anchor per variant**: the fire's
  *scheduled* instant for Calendar (drift-free), the *actual* fire instant for Interval (exactly
  one catch-up). `tick()` (below) is the reference caller that gets this right.

`CronScheduleError` covers: `BothCalendarAndInterval`, `NeitherCalendarNorInterval`,
`IncompleteCalendarSchedule`, `InvalidCronExpr`, `InvalidTimezone`, `NoUpcomingFire`,
`IntervalTooShort`, `IntervalTooLong` — manual `Display` + `std::error::Error`, matching this
repo's existing error-enum convention (no `thiserror`).

## Fire log + silence protocol (`cron/record.rs`)

`CronRecord` is the durable state of one scheduled entry: `id`, `schedule`, `created_at`,
`enabled`, `last_fired_at`, `next_fire_at`.

A fire produces a `FireOutcome`:

- `Reported(String)` — something happened; carries a human-readable note.
- `Silent` — the **silence protocol**: nothing to report, so no note or downstream output is
  produced.

`CronFireLogEntry` (`fired_at`, `scheduled_at`, `outcome_kind: &'static str`, `note: Option<String>`)
is the append-only per-fire record. The `From<&FireOutcome>` conversion (and the
`fire_outcome_kind_and_note` helper it shares with `CronFireLogEntry::new`) is the single
enforcement point: a `Silent` outcome structurally cannot produce a log entry carrying a note,
since the variant itself carries no message. `outcome_kind` stays a fixed `&'static str`
(`"reported"`/`"silent"`) in memory; a manual `Serialize`/`Deserialize` pair (via a private
`CronFireLogEntryWire` mirror with an owned `String`) handles the wire round-trip without changing
the in-memory field's type.

## `CronStore` seam + `FileCronStore` + `tick()` (`cron/store.rs`)

`CronStore` is the injectable seam (mirrors this repo's established trait/live-impl/stub
convention, e.g. `sdlc_flow`'s `CommandRunner`):

```rust
trait CronStore: Send + Sync {
    fn list(&self) -> Vec<CronRecord>;
    fn get(&self, id: &str) -> Option<CronRecord>;
    fn due(&self, now: DateTime<Utc>) -> Vec<CronRecord>;
    fn record_fire(&self, id: &str, entry: CronFireLogEntry, next_fire_at: Option<DateTime<Utc>>);
    fn fire_log(&self, id: &str) -> Vec<CronFireLogEntry>;
}
```

`FileCronStore` is the default implementation: it persists the whole store (every record plus its
fire log) as a single JSON file, rewritten whole-object on every mutation — matching the repo's
existing whole-object JSON persistence convention (see `repo_registry.rs`) rather than an
append-only log format. `FileCronStore::open(path)` reads the file if it exists (this is what
makes the fire log survive a process restart) or starts empty. `FileCronStore::upsert(record)` is
a concrete inherent method (not part of the `CronStore` trait) for seeding records, since a caller
holding only `&dyn CronStore` only ever needs to read/tick, never seed.

This is deliberately **not** the Postgres pattern used by `engine-serve/src/durable.rs`: the
`events` table is the orchestrator's pre-existing, externally-provisioned schema with no migration
tooling in this repo to add a table alongside it, and a single Mac Mini operator needs nothing more
than "survives a process restart."

`tick(store, now, fire) -> usize` is the driver a caller polls on an interval: it fires every due
(`next_fire_at <= now`), enabled record via the supplied `fire` closure, advances `next_fire_at`
using the anchor contract described above (computed explicitly per-variant inside `tick()` rather
than always passing `now`), records the fire in the durable log, and returns the count of records
fired.

## Dependencies

Added at the workspace level (`Cargo.toml`) and to `engine-core`: `cron = "0.12"` (cron expression
parsing/evaluation) and `chrono-tz = { version = "0.10", features = ["serde"] }` (IANA timezone
handling with `Tz` deriving `Serialize`/`Deserialize` for `CronSchedule::Calendar`).

## Status

Pure primitive only — no live caller yet. `EN.6.G`'s `schedule.rs` is the intended consumer (see
the module-level doc comment in `cron/mod.rs`); until that block lands, `CronStore`/`tick()` are
exercised only by this module's own unit tests.
