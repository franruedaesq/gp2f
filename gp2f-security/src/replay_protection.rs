//! Replay protection using a per-client sliding-window exact set + bloom filter.
//!
//! **Design**
//! - The **exact ring buffer** (backed by a `HashSet` + eviction queue) gives
//!   zero false-negatives for the most recent `WINDOW` op_ids per client.
//! - A **bloom filter pair** (active + previous) extends detection to the
//!   preceding `2 × ROTATE_AT` op_ids with a small false-positive rate.
//!   In production this layer is backed by a persistent DB to eliminate FPs.
//!
//! Capacity choices (ADR-003):
//! - `WINDOW` = 10 000 op_ids per client (exact)
//! - Bloom filter: 10 000 bits per filter word set, ~0.1 % FPR after 5 000 items
//!
//! ## Distributed deployment (Redis)
//!
//! When the `redis-broadcast` Cargo feature is enabled and `REDIS_URL` is set,
//! [`build_replay_store`] returns a [`RedisReplayGuard`] backed by Redis Sets.
//!
//! Key layout: `replay:{client_id}` – Redis Set of seen `op_id`s.
//! TTL is refreshed on every insert (`EXPIRE replay:{client_id} {window_secs}`).
//! Uses `SISMEMBER` for duplicate checks and `SADD` + `EXPIRE` for insertion,
//! providing cross-replica duplicate detection without shared in-process state.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

/// Size of the exact window (zero false-negatives within this many recent ops).
pub const WINDOW: usize = 10_000;

/// Number of bits per bloom filter (must be a multiple of 64).
/// 1_000_000 bits → FPR < 0.0001% at 10 000 items with 7 hashes.
const BLOOM_BITS: usize = 1_000_000; // 15_625 u64 words

/// Number of bloom hash functions.
const BLOOM_HASHES: usize = 7;

/// Rotate the active filter into `previous` after this many insertions.
const ROTATE_AT: usize = WINDOW / 2;

// ── per-client state ──────────────────────────────────────────────────────────

struct ClientEntry {
    /// Exact set for the most recent `WINDOW` op_ids.
    exact_set: HashSet<String>,
    /// FIFO order of exact-set entries for eviction.
    exact_order: VecDeque<String>,
    /// Active bloom filter (current generation).
    bloom_active: Box<[u64; BLOOM_BITS / 64]>,
    /// Previous bloom filter (one generation behind).
    bloom_previous: Box<[u64; BLOOM_BITS / 64]>,
    /// Number of insertions into the active filter.
    bloom_active_count: usize,
}

impl ClientEntry {
    fn new() -> Self {
        Self {
            exact_set: HashSet::with_capacity(WINDOW),
            exact_order: VecDeque::with_capacity(WINDOW),
            bloom_active: Box::new([0u64; BLOOM_BITS / 64]),
            bloom_previous: Box::new([0u64; BLOOM_BITS / 64]),
            bloom_active_count: 0,
        }
    }

    /// `true` iff `op_id` is a (probable) duplicate.
    fn contains(&self, op_id: &str) -> bool {
        // Exact check (authoritative for the recent window).
        if self.exact_set.contains(op_id) {
            return true;
        }
        // Bloom check for older items that fell off the exact window.
        bloom_contains(&self.bloom_active, op_id) || bloom_contains(&self.bloom_previous, op_id)
    }

    /// Record `op_id` as seen.
    fn insert(&mut self, op_id: String) {
        // Evict oldest if window is full.
        if self.exact_order.len() == WINDOW {
            if let Some(evicted) = self.exact_order.pop_front() {
                self.exact_set.remove(&evicted);
                // Evicted item enters bloom filter territory.
                bloom_insert(&mut self.bloom_active, &evicted);
                self.bloom_active_count += 1;
            }
        }
        self.exact_order.push_back(op_id.clone());
        self.exact_set.insert(op_id);

        // Rotate bloom filters when active is full.
        if self.bloom_active_count >= ROTATE_AT {
            std::mem::swap(&mut self.bloom_previous, &mut self.bloom_active);
            *self.bloom_active = [0u64; BLOOM_BITS / 64];
            self.bloom_active_count = 0;
        }
    }
}

// ── bloom helpers ─────────────────────────────────────────────────────────────

/// Double-hashing: generate k independent positions using two base hashes.
/// `h_i(x) = (h1(x) + i * h2(x)) % BLOOM_BITS`
fn bloom_positions(item: &[u8]) -> [usize; BLOOM_HASHES] {
    // h1: FNV-1a 64-bit
    let mut h1: u64 = 14_695_981_039_346_656_037;
    for &b in item {
        h1 ^= u64::from(b);
        h1 = h1.wrapping_mul(1_099_511_628_211);
    }
    // h2: SplitMix64 of h1
    let mut h2 = h1 ^ (h1 >> 30);
    h2 = h2.wrapping_mul(0xbf58476d1ce4e5b9);
    h2 ^= h2 >> 27;
    h2 = h2.wrapping_mul(0x94d049bb133111eb);
    h2 ^= h2 >> 31;

    let mut positions = [0usize; BLOOM_HASHES];
    for (i, pos) in positions.iter_mut().enumerate() {
        *pos = ((h1.wrapping_add((i as u64).wrapping_mul(h2))) as usize) % BLOOM_BITS;
    }
    positions
}

fn bloom_insert(filter: &mut [u64; BLOOM_BITS / 64], item: &str) {
    for bit in bloom_positions(item.as_bytes()) {
        filter[bit / 64] |= 1u64 << (bit % 64);
    }
}

fn bloom_contains(filter: &[u64; BLOOM_BITS / 64], item: &str) -> bool {
    for bit in bloom_positions(item.as_bytes()) {
        if filter[bit / 64] & (1u64 << (bit % 64)) == 0 {
            return false;
        }
    }
    true
}

// ── public API ────────────────────────────────────────────────────────────────

/// Server-wide replay-protection store.
///
/// Call [`ReplayGuard::check_and_insert`] for every incoming op_id.
pub struct ReplayGuard {
    clients: HashMap<String, ClientEntry>,
}

impl ReplayGuard {
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
        }
    }

    /// Returns `true` if `op_id` is a duplicate for `client_id`.
    /// If not a duplicate, records the op_id and returns `false`.
    pub fn check_and_insert(&mut self, client_id: &str, op_id: &str) -> bool {
        let entry = self
            .clients
            .entry(client_id.to_owned())
            .or_insert_with(ClientEntry::new);
        if entry.contains(op_id) {
            return true; // duplicate
        }
        entry.insert(op_id.to_owned());
        false
    }
}

impl Default for ReplayGuard {
    fn default() -> Self {
        Self::new()
    }
}

// ── async replay store trait ──────────────────────────────────────────────────

/// Type alias for a thread-safe, dynamically dispatched replay store.
pub type DynReplayStore = Arc<dyn ReplayStore>;

/// Async replay-protection trait.
///
/// Implemented by [`std::sync::Mutex<ReplayGuard>`] (in-memory, single-replica)
/// and by [`RedisReplayGuard`] (distributed, requires `redis-broadcast` feature).
#[async_trait::async_trait]
pub trait ReplayStore: Send + Sync + 'static {
    /// Returns `true` if `op_id` is a (probable) duplicate for `client_id`.
    /// If not a duplicate, records the op_id and returns `false`.
    async fn check_and_insert(&self, client_id: &str, op_id: &str) -> bool;
}

/// Wrap the synchronous [`ReplayGuard`] behind the async [`ReplayStore`] trait.
#[async_trait::async_trait]
impl ReplayStore for std::sync::Mutex<ReplayGuard> {
    async fn check_and_insert(&self, client_id: &str, op_id: &str) -> bool {
        self.lock().unwrap().check_and_insert(client_id, op_id)
    }
}

/// Build the best available replay store at startup.
///
/// When `REDIS_URL` is set and the `redis-broadcast` feature is enabled,
/// connects to Redis; otherwise falls back to the in-process replay guard.
pub async fn build_replay_store() -> DynReplayStore {
    #[cfg(feature = "redis-broadcast")]
    if let Some(url) = crate::secrets::resolve_secret("REDIS_URL") {
        match RedisReplayGuard::connect(&url) {
            Ok(guard) => {
                tracing::info!(url = %url, "Redis replay protection connected");
                return Arc::new(guard);
            }
            Err(e) => {
                tracing::warn!("Redis replay guard failed ({e}); falling back to in-memory");
            }
        }
    }
    tracing::info!("Using in-memory replay protection");
    Arc::new(std::sync::Mutex::new(ReplayGuard::new()))
}

// ── Redis replay guard ────────────────────────────────────────────────────────

/// Lua script for atomic replay check-and-insert using a Sorted Set.
///
/// KEYS[1] = `replay:{client_id}`
/// ARGV[1] = `op_id`, ARGV[2] = `window_ttl_secs`, ARGV[3] = current Unix timestamp (secs)
///
/// The sorted set uses the insertion timestamp as the score.  On each call:
/// 1. Remove all members whose score is older than `now - ttl` (sliding window eviction).
/// 2. Check for the `op_id` member.
/// 3. If new, add it with score = now.
/// 4. Refresh the key TTL so inactive clients eventually get their key expired.
///
/// Returns: `1` if duplicate, `0` if new (op recorded atomically).
#[cfg(feature = "redis-broadcast")]
const REPLAY_LUA: &str = r#"
local key = KEYS[1]
local op_id = ARGV[1]
local ttl = tonumber(ARGV[2])
local now = tonumber(ARGV[3])
local cutoff = now - ttl
redis.call('ZREMRANGEBYSCORE', key, '-inf', cutoff)
if redis.call('ZSCORE', key, op_id) then
    return 1
end
redis.call('ZADD', key, now, op_id)
redis.call('EXPIRE', key, ttl)
return 0
"#;

/// Window TTL for the per-client Redis Sets (1 hour).
///
/// After this many seconds of inactivity the set expires automatically,
/// preventing unbounded memory growth on the Redis server.
#[cfg(feature = "redis-broadcast")]
pub const REPLAY_WINDOW_SECS: u64 = 3_600;

/// Redis-backed replay-protection store.
///
/// Uses a Sorted Set keyed `replay:{client_id}` where each member is an
/// `op_id` and its score is the Unix timestamp (seconds) at which the op was
/// received.  On every insert:
/// 1. Members older than `window_secs` are evicted with `ZREMRANGEBYSCORE`
///    (sliding-window eviction), bounding the set size to at most
///    `window_secs` worth of op_ids rather than all op_ids ever sent.
/// 2. `ZSCORE` is used for the duplicate check (O(log N)).
/// 3. `ZADD` records the new op with the current timestamp as the score.
/// 4. `EXPIRE` is refreshed so inactive client keys are eventually deleted.
///
/// Across all replicas, every `check_and_insert` call hits the same Redis
/// shard, providing consistent cross-replica duplicate detection without
/// unbounded memory growth.
#[cfg(feature = "redis-broadcast")]
pub struct RedisReplayGuard {
    client: redis::Client,
    /// Window TTL in seconds (refreshed on every insert).
    window_secs: u64,
}

#[cfg(feature = "redis-broadcast")]
impl RedisReplayGuard {
    /// Connect to Redis and return a new replay guard.
    pub fn connect(redis_url: &str) -> Result<Self, redis::RedisError> {
        let client = redis::Client::open(redis_url)?;
        Ok(Self {
            client,
            window_secs: REPLAY_WINDOW_SECS,
        })
    }

    /// Create with a custom window TTL (useful for testing).
    pub fn with_window(client: redis::Client, window_secs: u64) -> Self {
        Self {
            client,
            window_secs,
        }
    }

    fn set_key(client_id: &str) -> String {
        format!("replay:{client_id}")
    }
}

#[cfg(feature = "redis-broadcast")]
#[async_trait::async_trait]
impl ReplayStore for RedisReplayGuard {
    async fn check_and_insert(&self, client_id: &str, op_id: &str) -> bool {
        let key = Self::set_key(client_id);
        let mut conn = match self.client.get_multiplexed_async_connection().await {
            Ok(c) => c,
            Err(e) => {
                let is_production = std::env::var("APP_ENV")
                    .map(|v| v.eq_ignore_ascii_case("production"))
                    .unwrap_or(false);

                if is_production {
                    tracing::error!(
                        client_id = %client_id,
                        error = %e,
                        "Redis replay guard connection error; denying op (fail-closed)"
                    );
                    return true; // fail closed: treat as duplicate/deny if Redis is unreachable
                }

                tracing::warn!(
                    client_id = %client_id,
                    error = %e,
                    "Redis replay guard connection error; allowing op (fail-open)"
                );
                return false; // fail open: allow op if Redis is unreachable (dev/test)
            }
        };

        // Current Unix timestamp in seconds used as the ZSET member score.
        // Log a warning if the system clock appears to be set before the Unix epoch.
        let now_secs: u64 = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        {
            Ok(d) => d.as_secs(),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Redis replay guard: system clock is before Unix epoch; \
                     replay protection may evict entries prematurely"
                );
                0
            }
        };

        // Atomic check-and-insert via Lua: ZREMRANGEBYSCORE (evict old) + ZSCORE + ZADD + EXPIRE.
        let result: i64 = match redis::Script::new(REPLAY_LUA)
            .key(&key)
            .arg(op_id)
            .arg(self.window_secs)
            .arg(now_secs)
            .invoke_async(&mut conn)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    client_id = %client_id,
                    op_id = %op_id,
                    error = %e,
                    "Redis replay guard script error; allowing op (fail-open)"
                );
                return false; // fail open
            }
        };

        result == 1 // 1 = duplicate, 0 = new
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_op_not_duplicate() {
        let mut guard = ReplayGuard::new();
        assert!(!guard.check_and_insert("client-1", "op-1"));
    }

    #[test]
    fn repeated_op_is_duplicate() {
        let mut guard = ReplayGuard::new();
        guard.check_and_insert("client-1", "op-1");
        assert!(guard.check_and_insert("client-1", "op-1"));
    }

    #[test]
    fn different_clients_are_isolated() {
        let mut guard = ReplayGuard::new();
        guard.check_and_insert("client-1", "op-1");
        // Same op_id from a different client must NOT be flagged as duplicate
        assert!(!guard.check_and_insert("client-2", "op-1"));
    }

    #[test]
    fn many_ops_no_false_negatives_within_window() {
        let mut guard = ReplayGuard::new();
        let n = WINDOW;
        for i in 0..n {
            assert!(
                !guard.check_and_insert("c", &format!("op-{i}")),
                "op-{i} should not be a duplicate on first insert"
            );
        }
        // All ops within the exact window must be detected as duplicates.
        for i in 0..n {
            assert!(
                guard.check_and_insert("c", &format!("op-{i}")),
                "op-{i} should be detected as duplicate within the exact window"
            );
        }
    }
}
