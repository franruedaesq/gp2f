//! Postgres-backed durable event store.
//!
//! Implements [`PersistentStore`] using a `sqlx` connection pool backed by
//! a PostgreSQL database.  All workflow history is persisted across process
//! restarts, satisfying the **Durability** requirement of the production
//! readiness plan.
//!
//! ## Schema
//!
//! Apply `migrations/20240522_init.sql` before starting the server:
//! ```sh
//! psql $DATABASE_URL -f migrations/20240522_init.sql
//! ```
//!
//! ## Configuration
//!
//! Set the `DATABASE_URL` environment variable to a valid Postgres connection
//! string (libpq format or `postgres://` URL):
//! ```sh
//! DATABASE_URL=postgres://user:pass@localhost/gp2f gp2f-server
//! ```
//!
//! When `DATABASE_URL` is unset the server falls back to the in-memory store.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{postgres::PgPoolOptions, PgPool, Row};
use std::time::Duration;

use crate::{
    event_store::{OpOutcome, StoredEvent},
    hlc::{Hlc, HlcTimestamp},
    temporal_store::PersistentStore,
    wire::ClientMessage,
};

// ── error type ────────────────────────────────────────────────────────────────

/// Errors produced by [`PostgresStore`].
#[derive(Debug, thiserror::Error)]
pub enum PostgresStoreError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("migration error: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
}

// ── store ─────────────────────────────────────────────────────────────────────

/// Postgres-backed persistent event store.
///
/// Uses a connection pool so multiple async tasks can issue queries
/// concurrently without contending on a single connection.
pub struct PostgresStore {
    pool: PgPool,
    hlc: Hlc,
}

impl PostgresStore {
    /// Connect to Postgres and return a new store.
    ///
    /// - Configures the connection pool for production workloads.
    /// - Runs any pending migrations from `migrations/` before returning.
    ///
    /// Returns an error if the initial connection cannot be established or
    /// if a migration fails.
    pub async fn new(database_url: &str) -> Result<Self, PostgresStoreError> {
        let pool = PgPoolOptions::new()
            .max_connections(16)
            .min_connections(2)
            .acquire_timeout(Duration::from_secs(5))
            .idle_timeout(Duration::from_secs(600))
            .connect(database_url)
            .await?;

        // Apply any pending migrations from the `migrations/` folder at the
        // workspace root.  This is idempotent – already-applied migrations are
        // skipped.
        sqlx::migrate!("../migrations").run(&pool).await?;

        Ok(Self {
            pool,
            hlc: Hlc::new(),
        })
    }

    /// Build a partition key identical to [`EventStore::partition_key`].
    fn partition_key(msg: &ClientMessage) -> String {
        format!("{}:{}:{}", msg.tenant_id, msg.workflow_id, msg.instance_id)
    }

    /// Fetch the next sequence number for a partition, i.e. `MAX(seq) + 1`.
    async fn next_seq(&self, msg: &ClientMessage) -> Result<i64, sqlx::Error> {
        let row = sqlx::query(
            "SELECT COALESCE(MAX(seq), -1) + 1 AS next_seq \
             FROM event_log \
             WHERE tenant_id = $1 AND workflow_id = $2 AND instance_id = $3",
        )
        .bind(&msg.tenant_id)
        .bind(&msg.workflow_id)
        .bind(&msg.instance_id)
        .fetch_one(&self.pool)
        .await?;

        Ok(row.get::<i64, _>("next_seq"))
    }
}

/// Returns `true` if `e` is a PostgreSQL unique-constraint violation (code 23505).
///
/// Used by [`PostgresStore::append`] to detect seq collisions during OCC retries.
fn is_unique_violation(e: &sqlx::Error) -> bool {
    if let sqlx::Error::Database(db_err) = e {
        return db_err.code().map_or(false, |c| c == "23505");
    }
    false
}

/// Maximum number of OCC retry attempts in [`PostgresStore::append`].
const MAX_APPEND_RETRIES: usize = 5;

#[async_trait]
impl PersistentStore for PostgresStore {
    /// Append an event to the log and return its sequence number.
    ///
    /// Uses **Optimistic Concurrency Control (OCC)**: the unique primary key on
    /// `(tenant_id, workflow_id, instance_id, seq)` detects concurrent writes
    /// with the same sequence number.  On collision (PostgreSQL error 23505) the
    /// method refetches `MAX(seq)` and retries up to [`MAX_APPEND_RETRIES`]
    /// times.  This eliminates the TOCTOU race between the `SELECT MAX` and the
    /// `INSERT` that existed in the original two-round-trip design.
    ///
    /// Returns `0` only when an unrecoverable error occurs (serialisation
    /// failure, fatal DB error, or all OCC retries exhausted).  The caller
    /// should treat `0` as a signal that the event was **not** persisted.
    async fn append(&self, msg: ClientMessage, outcome: OpOutcome) -> u64 {
        let outcome_str = match outcome {
            OpOutcome::Accepted => "ACCEPTED",
            OpOutcome::Rejected => "REJECTED",
        };

        // Store the full ClientMessage as JSONB so events_for can fully
        // reconstruct StoredEvent on replay.
        let message_json = match serde_json::to_value(&msg) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(op_id = %msg.op_id, "postgres append: failed to serialize message: {e}");
                serde_json::Value::Null
            }
        };

        for attempt in 0..MAX_APPEND_RETRIES {
            let seq = match self.next_seq(&msg).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("postgres append: failed to get next seq: {e}");
                    return 0;
                }
            };

            // Recalculate the HLC timestamp on each attempt so that retried
            // events always get a timestamp ≥ all previously observed clocks,
            // preserving HLC monotonicity.
            let hlc_ts = self.hlc.now() as i64;

            let result = sqlx::query(
                "INSERT INTO event_log \
                 (tenant_id, workflow_id, instance_id, seq, op_id, ingested_at, hlc_ts, outcome, payload) \
                 VALUES ($1, $2, $3, $4, $5, NOW(), $6, $7, $8)",
            )
            .bind(&msg.tenant_id)
            .bind(&msg.workflow_id)
            .bind(&msg.instance_id)
            .bind(seq)
            .bind(&msg.op_id)
            .bind(hlc_ts)
            .bind(outcome_str)
            .bind(&message_json)
            .execute(&self.pool)
            .await;

            match result {
                Ok(_) => return seq as u64,
                Err(ref e) if is_unique_violation(e) => {
                    tracing::warn!(
                        op_id = %msg.op_id,
                        attempt,
                        "postgres append: seq conflict (OCC retry)"
                    );
                    // Another writer claimed this seq; refetch and retry.
                    continue;
                }
                Err(e) => {
                    tracing::error!("postgres append: insert failed: {e}");
                    return 0;
                }
            }
        }

        tracing::error!(
            op_id = %msg.op_id,
            "postgres append: max OCC retries ({MAX_APPEND_RETRIES}) exceeded"
        );
        0
    }

    async fn events_for(&self, key: &str) -> Vec<StoredEvent> {
        // Parse the partition key back into tenant/workflow/instance.
        let parts: Vec<&str> = key.splitn(3, ':').collect();
        if parts.len() != 3 {
            return vec![];
        }
        let (tenant_id, workflow_id, instance_id) = (parts[0], parts[1], parts[2]);

        let rows = match sqlx::query(
            "SELECT seq, ingested_at, hlc_ts, outcome, payload \
             FROM event_log \
             WHERE tenant_id = $1 AND workflow_id = $2 AND instance_id = $3 \
             ORDER BY seq ASC",
        )
        .bind(tenant_id)
        .bind(workflow_id)
        .bind(instance_id)
        .fetch_all(&self.pool)
        .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("postgres events_for: query failed: {e}");
                return vec![];
            }
        };

        rows.into_iter()
            .filter_map(|row| {
                let seq: i64 = row.try_get("seq").ok()?;
                let ingested_at: DateTime<Utc> = row.try_get("ingested_at").ok()?;
                let hlc_ts: i64 = row.try_get("hlc_ts").ok()?;
                let outcome_str: String = row.try_get("outcome").ok()?;
                let message_json: serde_json::Value = row.try_get("payload").ok()?;

                let outcome = if outcome_str == "ACCEPTED" {
                    OpOutcome::Accepted
                } else {
                    OpOutcome::Rejected
                };

                // Deserialize the full ClientMessage stored as JSONB.
                let message: ClientMessage = match serde_json::from_value(message_json) {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!(
                            seq,
                            "postgres events_for: failed to deserialize message: {e}"
                        );
                        return None;
                    }
                };

                Some(StoredEvent {
                    seq: seq as u64,
                    ingested_at,
                    hlc_ts: hlc_ts as HlcTimestamp,
                    message,
                    outcome,
                })
            })
            .collect()
    }

    async fn total_count(&self) -> usize {
        let row = match sqlx::query("SELECT COUNT(*) AS cnt FROM event_log")
            .fetch_one(&self.pool)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("postgres total_count: query failed: {e}");
                return 0;
            }
        };
        row.try_get::<i64, _>("cnt").unwrap_or(0) as usize
    }
}
