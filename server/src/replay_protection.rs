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

use std::collections::{HashMap, HashSet, VecDeque};

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
