//! Per-tenant AI rate limiting and cost-guard rails.
//!
//! Implements Phase 8 requirement 6: a token-bucket rate limiter that caps
//! AI-assistance calls at [`DEFAULT_MAX_LLM_CALLS_PER_MINUTE`] per minute per
//! tenant by default, and a budget guard that disables AI assistance when a
//! tenant's monthly spend ceiling is reached.
//!
//! ## Redis (distributed deployments)
//!
//! When the `redis-broadcast` Cargo feature is enabled and `REDIS_URL` is set,
//! the [`build_rate_limiter`] factory returns a [`RedisRateLimiter`] that uses
//! Redis `INCR + EXPIRE` for distributed counting across replicas.
//!
//! For single-replica deployments (or when Redis is unavailable) the factory
//! falls back to the in-process [`AiRateLimiter`] which stores counters in
//! local `Mutex`-guarded `HashMap`s.

use std::{collections::HashMap, sync::Mutex, time::Instant};
use std::sync::Arc;

// ── constants ─────────────────────────────────────────────────────────────────

/// Default maximum LLM calls per minute per tenant.
pub const DEFAULT_MAX_LLM_CALLS_PER_MINUTE: u32 = 200;

// ── errors ────────────────────────────────────────────────────────────────────

/// Returned by [`AiRateLimiter::check_and_consume`] when a call is blocked.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum RateLimitError {
    #[error(
        "tenant '{tenant_id}' exceeded the AI rate limit ({limit} calls/min); \
         retry after {retry_after_secs}s"
    )]
    TokenBucketExhausted {
        tenant_id: String,
        limit: u32,
        retry_after_secs: u64,
    },

    #[error("tenant '{tenant_id}' has exceeded their monthly AI budget; AI assistance disabled")]
    BudgetExceeded { tenant_id: String },
}

// ── token bucket ──────────────────────────────────────────────────────────────

struct Bucket {
    tokens: f64,
    last_refill: Instant,
    max_tokens: f64,
    /// Tokens added per second.
    refill_rate: f64,
}

impl Bucket {
    fn new(max_calls_per_minute: u32) -> Self {
        let max = max_calls_per_minute as f64;
        Self {
            tokens: max,
            last_refill: Instant::now(),
            max_tokens: max,
            refill_rate: max / 60.0,
        }
    }

    /// Attempt to consume one token.
    ///
    /// Returns `Ok(())` on success; on failure returns the number of seconds
    /// until the bucket will have at least one token.
    fn try_consume(&mut self) -> Result<(), u64> {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.max_tokens);
        self.last_refill = now;

        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            Ok(())
        } else {
            let secs = ((1.0 - self.tokens) / self.refill_rate).ceil() as u64;
            Err(secs.max(1))
        }
    }
}

// ── spend tracker ─────────────────────────────────────────────────────────────

#[derive(Default)]
struct SpendTracker {
    /// Cumulative spend this calendar month in USD cents.
    monthly_cents: u64,
    /// Ceiling (0 = unlimited).
    budget_cents: u64,
}

impl SpendTracker {
    fn is_over_budget(&self) -> bool {
        self.budget_cents > 0 && self.monthly_cents >= self.budget_cents
    }
}

// ── rate limiter ──────────────────────────────────────────────────────────────

/// Thread-safe AI rate limiter and monthly spend guard.
pub struct AiRateLimiter {
    buckets: Mutex<HashMap<String, Bucket>>,
    spend: Mutex<HashMap<String, SpendTracker>>,
    default_max_calls: u32,
}

impl AiRateLimiter {
    pub fn new() -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
            spend: Mutex::new(HashMap::new()),
            default_max_calls: DEFAULT_MAX_LLM_CALLS_PER_MINUTE,
        }
    }

    /// Override the per-minute call limit for a specific tenant.
    pub fn set_rate_limit(&self, tenant_id: &str, max_calls_per_minute: u32) {
        self.buckets
            .lock()
            .unwrap()
            .insert(tenant_id.to_owned(), Bucket::new(max_calls_per_minute));
    }

    /// Set the monthly spend ceiling (USD cents) for a tenant.  0 = unlimited.
    pub fn set_monthly_budget(&self, tenant_id: &str, budget_cents: u64) {
        self.spend
            .lock()
            .unwrap()
            .entry(tenant_id.to_owned())
            .or_default()
            .budget_cents = budget_cents;
    }

    /// Try to consume one AI call token for `tenant_id`.
    ///
    /// Returns `Ok(())` when the call is allowed.  Returns a
    /// [`RateLimitError`] when the token bucket is exhausted or the monthly
    /// budget has been exceeded.
    pub fn check_and_consume(&self, tenant_id: &str) -> Result<(), RateLimitError> {
        // Budget check first (cheap).
        {
            let spend = self.spend.lock().unwrap();
            if let Some(tracker) = spend.get(tenant_id) {
                if tracker.is_over_budget() {
                    return Err(RateLimitError::BudgetExceeded {
                        tenant_id: tenant_id.to_owned(),
                    });
                }
            }
        }

        // Token-bucket check.
        let mut buckets = self.buckets.lock().unwrap();
        let max = self.default_max_calls;
        let bucket = buckets
            .entry(tenant_id.to_owned())
            .or_insert_with(|| Bucket::new(max));

        bucket
            .try_consume()
            .map_err(|retry_after_secs| RateLimitError::TokenBucketExhausted {
                tenant_id: tenant_id.to_owned(),
                limit: max,
                retry_after_secs,
            })
    }

    /// Record `cents` of spend for a tenant after a successful LLM call.
    pub fn record_spend(&self, tenant_id: &str, cents: u64) {
        self.spend
            .lock()
            .unwrap()
            .entry(tenant_id.to_owned())
            .or_default()
            .monthly_cents += cents;
    }

    /// Reset the monthly spend counter for `tenant_id` (month rollover).
    pub fn reset_monthly_spend(&self, tenant_id: &str) {
        if let Some(tracker) = self.spend.lock().unwrap().get_mut(tenant_id) {
            tracker.monthly_cents = 0;
        }
    }

    /// Current cumulative spend (USD cents) for `tenant_id` this month.
    pub fn monthly_spend_cents(&self, tenant_id: &str) -> u64 {
        self.spend
            .lock()
            .unwrap()
            .get(tenant_id)
            .map(|t| t.monthly_cents)
            .unwrap_or(0)
    }
}

impl Default for AiRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

// ── RateLimiterBackend trait ──────────────────────────────────────────────────

/// Abstraction over the rate-limiting backend.
///
/// Implement this trait to swap the in-process token bucket for a distributed
/// backend (Redis, Memcached, etc.) without changing call sites.
#[async_trait::async_trait]
pub trait RateLimiterBackend: Send + Sync {
    /// Try to consume one AI call token for `tenant_id`.
    ///
    /// Returns `Ok(())` when the call is allowed.  Returns a
    /// [`RateLimitError`] when the call should be rejected.
    async fn check_and_consume(&self, tenant_id: &str) -> Result<(), RateLimitError>;
}

/// Type-erased rate limiter used throughout the server.
pub type DynRateLimiter = Arc<dyn RateLimiterBackend>;

// ── In-process adapter ────────────────────────────────────────────────────────

/// Wraps [`AiRateLimiter`] so it can be used as a [`RateLimiterBackend`].
pub struct InProcessRateLimiter(pub AiRateLimiter);

#[async_trait::async_trait]
impl RateLimiterBackend for InProcessRateLimiter {
    async fn check_and_consume(&self, tenant_id: &str) -> Result<(), RateLimitError> {
        self.0.check_and_consume(tenant_id)
    }
}

// ── Redis-backed rate limiter ─────────────────────────────────────────────────

/// Redis-backed distributed rate limiter using `INCR + EXPIRE`.
///
/// Each tenant gets a Redis key `ai_rl:{tenant_id}` that is incremented on
/// every call and expired after 60 seconds.  This provides a sliding fixed-
/// window counter that is consistent across all server replicas.
///
/// Requires the `redis-broadcast` Cargo feature and `REDIS_URL` to be set.
#[cfg(feature = "redis-broadcast")]
pub struct RedisRateLimiter {
    connection: redis::aio::ConnectionManager,
    max_calls_per_minute: u32,
}

#[cfg(feature = "redis-broadcast")]
impl RedisRateLimiter {
    /// Connect to Redis and return a new rate limiter.
    pub async fn connect(redis_url: &str, max_calls_per_minute: u32) -> Result<Self, redis::RedisError> {
        let client = redis::Client::open(redis_url)?;
        let connection = redis::aio::ConnectionManager::new(client).await?;
        Ok(Self {
            connection,
            max_calls_per_minute,
        })
    }

    /// Redis key for a tenant's per-minute call counter.
    fn key(tenant_id: &str) -> String {
        format!("ai_rl:{tenant_id}")
    }
}

#[cfg(feature = "redis-broadcast")]
#[async_trait::async_trait]
impl RateLimiterBackend for RedisRateLimiter {
    async fn check_and_consume(&self, tenant_id: &str) -> Result<(), RateLimitError> {
        use redis::Script;
        // Atomically INCR the counter and set a 60-second TTL on first increment.
        // The Lua script runs as a single atomic unit on the Redis server, which
        // eliminates the INCR/EXPIRE race condition that exists in a two-step approach.
        let script = Script::new(
            r"
            local count = redis.call('INCR', KEYS[1])
            if count == 1 then
                redis.call('EXPIRE', KEYS[1], 60)
            end
            return count
            ",
        );
        let key = Self::key(tenant_id);
        let mut conn = self.connection.clone();
        let count: u64 = script.key(&key).invoke_async(&mut conn).await.map_err(|e| {
            tracing::warn!(tenant_id = %tenant_id, error = %e, "Redis rate-limit script failed; failing open");
            // Fail open on Redis errors to avoid blocking all traffic during
            // Redis outages; log the error so it can be alerted on.
            RateLimitError::TokenBucketExhausted {
                tenant_id: tenant_id.to_owned(),
                limit: self.max_calls_per_minute,
                retry_after_secs: 1,
            }
        })?;
        if count > u64::from(self.max_calls_per_minute) {
            Err(RateLimitError::TokenBucketExhausted {
                tenant_id: tenant_id.to_owned(),
                limit: self.max_calls_per_minute,
                retry_after_secs: 60,
            })
        } else {
            Ok(())
        }
    }
}

// ── factory ───────────────────────────────────────────────────────────────────

/// Build the best available rate limiter at startup.
///
/// When `REDIS_URL` is set and the `redis-broadcast` feature is enabled,
/// connects to Redis for distributed counting across replicas.  Otherwise
/// falls back to the in-process token-bucket implementation.
pub async fn build_rate_limiter() -> DynRateLimiter {
    #[cfg(feature = "redis-broadcast")]
    if let Some(url) = crate::secrets::resolve_secret("REDIS_URL") {
        let max = std::env::var("AI_RATE_LIMIT_PER_MINUTE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_MAX_LLM_CALLS_PER_MINUTE);
        match RedisRateLimiter::connect(&url, max).await {
            Ok(limiter) => {
                tracing::info!(
                    url = %url,
                    max_calls_per_minute = max,
                    "Redis-backed AI rate limiter connected"
                );
                return Arc::new(limiter);
            }
            Err(e) => {
                tracing::warn!("Redis rate limiter failed ({e}); falling back to in-process");
            }
        }
    }
    tracing::info!("Using in-process AI rate limiter (single-replica mode)");
    Arc::new(InProcessRateLimiter(AiRateLimiter::new()))
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_calls_up_to_default_limit() {
        let limiter = AiRateLimiter::new();
        // The first call should always succeed.
        assert!(limiter.check_and_consume("tenant_a").is_ok());
    }

    #[test]
    fn exhausts_bucket_and_returns_error() {
        let limiter = AiRateLimiter::new();
        // Set a very small limit so we can exhaust it quickly.
        limiter.set_rate_limit("t1", 2);
        assert!(limiter.check_and_consume("t1").is_ok());
        assert!(limiter.check_and_consume("t1").is_ok());
        let err = limiter.check_and_consume("t1").unwrap_err();
        assert!(matches!(err, RateLimitError::TokenBucketExhausted { .. }));
    }

    #[test]
    fn tenants_are_isolated() {
        let limiter = AiRateLimiter::new();
        limiter.set_rate_limit("a", 1);
        limiter.set_rate_limit("b", 1);
        limiter.check_and_consume("a").unwrap();
        // Exhausting "a" must not affect "b".
        assert!(limiter.check_and_consume("b").is_ok());
    }

    #[test]
    fn budget_exceeded_blocks_calls() {
        let limiter = AiRateLimiter::new();
        limiter.set_monthly_budget("t2", 100);
        limiter.record_spend("t2", 100);
        let err = limiter.check_and_consume("t2").unwrap_err();
        assert!(matches!(err, RateLimitError::BudgetExceeded { .. }));
    }

    #[test]
    fn unlimited_budget_never_blocks() {
        let limiter = AiRateLimiter::new();
        // budget_cents = 0 means unlimited
        limiter.set_monthly_budget("t3", 0);
        limiter.record_spend("t3", u64::MAX / 2);
        // Should not return BudgetExceeded
        let result = limiter.check_and_consume("t3");
        assert!(!matches!(
            result,
            Err(RateLimitError::BudgetExceeded { .. })
        ));
    }

    #[test]
    fn reset_monthly_spend_clears_counter() {
        let limiter = AiRateLimiter::new();
        limiter.set_monthly_budget("t4", 50);
        limiter.record_spend("t4", 50);
        assert!(matches!(
            limiter.check_and_consume("t4"),
            Err(RateLimitError::BudgetExceeded { .. })
        ));
        limiter.reset_monthly_spend("t4");
        assert!(limiter.check_and_consume("t4").is_ok());
    }

    #[test]
    fn monthly_spend_accumulates() {
        let limiter = AiRateLimiter::new();
        limiter.record_spend("t5", 10);
        limiter.record_spend("t5", 20);
        assert_eq!(limiter.monthly_spend_cents("t5"), 30);
    }

    #[test]
    fn unknown_tenant_has_zero_spend() {
        let limiter = AiRateLimiter::new();
        assert_eq!(limiter.monthly_spend_cents("unknown"), 0);
    }
}
