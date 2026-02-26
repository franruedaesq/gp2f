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
    temporal_store::{PersistenceError, PersistentStore},
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
}

// ── advisory lock helpers ─────────────────────────────────────────────────────

/// Derive a stable i64 advisory lock key from a string partition key using
/// FNV-1a 64-bit hash (offset basis 14695981039346656037, prime 1099511628211,
/// as per https://www.isthe.com/chongo/tech/comp/fnv/#FNV-1a).
///
/// FNV-1a is chosen for its simplicity, speed, and good avalanche properties.
/// Postgres `pg_advisory_xact_lock` accepts a single `bigint` (i64), so the
/// 64-bit FNV result is reinterpreted as i64 via bitcast.  Different partition
/// keys will produce different lock keys with overwhelming probability.
fn advisory_lock_key(partition_key: &str) -> i64 {
    let mut hash: u64 = 14_695_981_039_346_656_037;
    for byte in partition_key.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    // Fold into i64 by reinterpreting the bit pattern.
    hash as i64
}

#[async_trait]
impl PersistentStore for PostgresStore {
    /// Append an event to the log and return its sequence number.
    ///
    /// Uses a per-instance PostgreSQL advisory lock (`pg_advisory_xact_lock`)
    /// to serialise concurrent writes from multiple actors to the same
    /// `(tenant_id, workflow_id, instance_id)` partition, preventing interleaved
    /// events and broken causal chains when split-brain actors race to write.
    ///
    /// The advisory lock is held for the duration of the transaction so the
    /// sequence number is assigned atomically and the causal chain is preserved.
    ///
    /// Returns `Ok(seq)` on success or a structured [`PersistenceError`] when
    /// the event could not be persisted.
    async fn append(
        &self,
        msg: ClientMessage,
        outcome: OpOutcome,
    ) -> Result<u64, PersistenceError> {
        let outcome_str = match outcome {
            OpOutcome::Accepted => "ACCEPTED",
            OpOutcome::Rejected => "REJECTED",
        };

        // Store the full ClientMessage as JSONB so events_for can fully
        // reconstruct StoredEvent on replay.
        let message_json = serde_json::to_value(&msg).map_err(|e| {
            tracing::warn!(op_id = %msg.op_id, "postgres append: serialization failed: {e}");
            PersistenceError::Serialization(e.to_string())
        })?;

        let hlc_ts = self.hlc.now() as i64;

        // Derive a stable 64-bit advisory lock key from the partition key so
        // that concurrent writes for the same (tenant, workflow, instance)
        // triple are serialised by the database.
        let partition_key = Self::partition_key(&msg);
        let lock_key = advisory_lock_key(&partition_key);

        let mut tx = self.pool.begin().await.map_err(|e| {
            tracing::error!(op_id = %msg.op_id, "postgres append: begin transaction failed: {e}");
            PersistenceError::Database(e.to_string())
        })?;

        // Acquire per-instance advisory lock for the duration of the transaction.
        // `pg_advisory_xact_lock` is automatically released at transaction end
        // (commit or rollback) – no explicit unlock is needed.
        // This serialises all writes to the same instance, preserving causal order.
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(lock_key)
            .execute(&mut *tx)
            .await
            .map_err(|e| {
                tracing::error!(op_id = %msg.op_id, "postgres append: advisory lock failed: {e}");
                PersistenceError::Database(e.to_string())
            })?;

        let row = sqlx::query(
            "INSERT INTO event_log \
             (tenant_id, workflow_id, instance_id, op_id, ingested_at, hlc_ts, outcome, payload) \
             VALUES ($1, $2, $3, $4, NOW(), $5, $6, $7) \
             RETURNING seq",
        )
        .bind(&msg.tenant_id)
        .bind(&msg.workflow_id)
        .bind(&msg.instance_id)
        .bind(&msg.op_id)
        .bind(hlc_ts)
        .bind(outcome_str)
        .bind(&message_json)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| {
            tracing::error!(op_id = %msg.op_id, "postgres append: insert failed: {e}");
            // Unique-constraint violations (PostgreSQL error code 23505) indicate
            // a duplicate op_id or seq conflict – surface as Conflict rather than
            // a generic database error so callers can apply idempotency logic.
            match &e {
                sqlx::Error::Database(db_err) if db_err.code().as_deref() == Some("23505") => {
                    PersistenceError::Conflict(format!(
                        "duplicate key on insert for op_id={}: {e}",
                        msg.op_id
                    ))
                }
                _ => PersistenceError::Database(e.to_string()),
            }
        })?;

        tx.commit().await.map_err(|e| {
            tracing::error!(op_id = %msg.op_id, "postgres append: commit failed: {e}");
            PersistenceError::Database(e.to_string())
        })?;

        let seq: i64 = row.get("seq");
        Ok(seq as u64)
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
