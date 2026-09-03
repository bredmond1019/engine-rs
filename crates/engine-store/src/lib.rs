//! `engine-store` — Postgres read/write for the durable `events` record.
//!
//! `sqlx::PgPool`-backed read/write path for the `events` row (contract §4), per
//! decision D2 — see `planning/decisions/D2-async-runtime-choice.md`.

pub mod postgres;

pub use postgres::{
    connect, get_event, get_task_context, insert_event, insert_journal_row,
    list_journal_rows_for_campaign, list_orphan_candidates, run_migrations, touch, update_event,
    upsert_event,
};

/// Placeholder identifying this crate; exists so the workspace has at least one
/// non-trivial symbol to build and test against before the real types land.
pub fn crate_name() -> &'static str {
    "engine-store"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_name_is_engine_store() {
        assert_eq!(crate_name(), "engine-store");
    }
}
