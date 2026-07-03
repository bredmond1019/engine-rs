//! `engine-store` — Postgres read/write for the durable `events` record.
//!
//! Stub crate for EN.0.A (Cargo workspace + CI). The `sqlx::PgPool`-backed read/write path
//! for the `events` row lands in EN.0.B per decision D2 — see
//! `planning/decisions/D2-async-runtime-choice.md` and `docs/architecture.md`.

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
