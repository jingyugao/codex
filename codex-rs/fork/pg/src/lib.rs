//! Fork-only PostgreSQL persistence for Codex threads and cross-pod handoff.
//!
//! [`PgThreadStore`] implements [`codex_thread_store::ThreadStore`] so that all conversation data
//! lives in Postgres. Live writers are fenced by a per-thread `epoch`: opening a thread for writes
//! (create or resume) bumps the epoch, and every subsequent write by that store is conditioned on
//! the epoch it claimed, so a stale pod cannot keep writing after another pod took the thread over.
//!
//! [`PgHandoffStore`] shares the same database and offers an exactly-once claim of a pending
//! unfinished-turn handoff record.

mod db;
mod handoff;
mod projection;
mod store;

pub use handoff::HandoffRecord;
pub use handoff::PgHandoffStore;
pub use store::PgThreadStore;
pub use store::PgThreadStoreConfig;
/// Re-exported so dependents share this crate's driver feature setup.
pub use sqlx;

/// Creates a pool without connecting (usable from sync code). Stores built on it run their
/// migrations lazily before their first query.
#[expect(
    clippy::disallowed_methods,
    reason = "the disallowed pool constructors guard SQLite; this is a Postgres pool"
)]
pub fn connect_lazy(url: &str) -> Result<sqlx::PgPool, codex_thread_store::ThreadStoreError> {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(16)
        .connect_lazy(url)
        .map_err(|err| codex_thread_store::ThreadStoreError::InvalidRequest {
            message: format!("invalid postgres connection url: {err}"),
        })
}

/// Environment variable that overrides the owner id used for ownership fencing.
pub const OWNER_ID_ENV_VAR: &str = "CODEX_FORK_OWNER_ID";

/// Returns the owner id for this process: `CODEX_FORK_OWNER_ID`, else `HOSTNAME`, else a random
/// UUID.
pub fn owner_id_from_env() -> String {
    [OWNER_ID_ENV_VAR, "HOSTNAME"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .map(|value| value.trim().to_string())
        .find(|value| !value.is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
}
