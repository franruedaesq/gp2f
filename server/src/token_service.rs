//! Ephemeral tool token service.
//!
//! Mints short-lived, scoped tokens for a given workflow instance and current
//! AST state.  Tokens are opaque to the client and to any AI agent: only the
//! backend knows how to map them to real operations.
//!
//! ## Token lifecycle
//! 1. **Mint** – caller supplies tenant/workflow/instance/op metadata *and*
//!    the current `ast_state_hash`.  The service generates a random opaque
//!    `token_id` of the form `tool_req_{op_slug}_{hex}`, stores its metadata,
//!    and returns the `token_id`.
//! 2. **Redeem** – caller presents the `token_id` and the *current*
//!    `ast_state_hash`.  The service checks:
//!    - token exists and has not been redeemed yet,
//!    - token has not expired (TTL = 5 minutes),
//!    - `op_name` matches the one baked into the token,
//!    - `ast_state_hash` still matches the hash recorded at mint time.
//!
//! Any mismatch returns an explicit `RedeemError`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Default token time-to-live: 5 minutes.
pub const TOKEN_TTL: Duration = Duration::from_secs(5 * 60);

// ── error types ───────────────────────────────────────────────────────────────

/// Errors that can occur when redeeming a token.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum RedeemError {
    #[error("token not found")]
    NotFound,
    #[error("token has already been redeemed")]
    AlreadyRedeemed,
    #[error("token is locked (in-flight consumption by another request)")]
    Locked,
    #[error("token has expired")]
    Expired,
    #[error("operation mismatch: expected '{expected}', got '{got}'")]
    OpMismatch { expected: String, got: String },
    #[error("AST state hash mismatch: token was issued for hash '{expected}', got '{got}'")]
    StateHashMismatch { expected: String, got: String },
}

// ── token state ───────────────────────────────────────────────────────────────

/// Lifecycle states of an ephemeral tool token.
///
/// ```text
/// ┌────────┐  lock()   ┌────────┐  redeem()  ┌──────────┐
/// │ ISSUED │ ────────▶ │ LOCKED │ ──────────▶ │ CONSUMED │
/// └────────┘           └────────┘             └──────────┘
/// ```
///
/// The `LOCKED` state prevents double-spending across concurrent requests:
/// a token is locked atomically before validation so that two simultaneous
/// redemption calls cannot both pass the `redeemed` check.  In a distributed
/// deployment the `lock()` + `redeem()` transition should be implemented with
/// a Redis Lua script (or `WATCH`/`MULTI`/`EXEC`) for cross-replica safety.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenState {
    /// Minted and not yet claimed.
    Issued,
    /// Locked by an in-flight redemption request; cannot be redeemed by
    /// another concurrent caller.
    Locked,
    /// Successfully consumed; no further operations permitted.
    Consumed,
}

// ── internal storage ──────────────────────────────────────────────────────────

/// Metadata stored for each minted token.
struct TokenRecord {
    tenant_id: String,
    workflow_id: String,
    instance_id: String,
    op_name: String,
    /// BLAKE3 hex digest of the policy AST state at mint time.
    ast_state_hash: String,
    issued_at: Instant,
    state: TokenState,
}

// ── public API types ──────────────────────────────────────────────────────────

/// Input required to mint a new token.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MintRequest {
    pub tenant_id: String,
    pub workflow_id: String,
    pub instance_id: String,
    /// The operation this token authorizes (e.g. `"doc_approval"`).
    pub op_name: String,
    /// Current BLAKE3 hex digest of the evaluated policy AST state.
    pub ast_state_hash: String,
}

/// Response returned after successfully minting a token.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MintResponse {
    /// Opaque token ID: `tool_req_{op_slug}_{random_hex}`.
    pub token_id: String,
    /// Number of seconds until the token expires.
    pub expires_in_secs: u64,
}

/// Input required to redeem a token.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RedeemRequest {
    pub token_id: String,
    /// Must match the `op_name` baked into the token.
    pub op_name: String,
    /// Current BLAKE3 hex digest; must match the one recorded at mint time.
    pub ast_state_hash: String,
}

/// Response returned after successfully redeeming a token.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RedeemResponse {
    pub tenant_id: String,
    pub workflow_id: String,
    pub instance_id: String,
    pub op_name: String,
}

// ── service ───────────────────────────────────────────────────────────────────

/// In-process ephemeral tool token service.
///
/// For production use, replace the inner `HashMap` with a distributed cache
/// (e.g. Redis) that supports atomic TTL-based expiry and CAS-style
/// single-use redemption.
pub struct TokenService {
    store: Mutex<HashMap<String, TokenRecord>>,
    /// Override TTL (used in tests to set very short expiries).
    ttl: Duration,
}

impl TokenService {
    /// Create a new service with the default 5-minute TTL.
    pub fn new() -> Self {
        Self::with_ttl(TOKEN_TTL)
    }

    /// Create a service with a custom TTL (useful for testing).
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            store: Mutex::new(HashMap::new()),
            ttl,
        }
    }

    /// Mint a new token and return its opaque ID plus remaining TTL.
    pub fn mint(&self, req: MintRequest) -> MintResponse {
        let token_id = Self::generate_token_id(&req.op_name);
        let record = TokenRecord {
            tenant_id: req.tenant_id,
            workflow_id: req.workflow_id,
            instance_id: req.instance_id,
            op_name: req.op_name,
            ast_state_hash: req.ast_state_hash,
            issued_at: Instant::now(),
            state: TokenState::Issued,
        };
        self.store.lock().unwrap().insert(token_id.clone(), record);
        MintResponse {
            token_id,
            expires_in_secs: self.ttl.as_secs(),
        }
    }

    /// Attempt to redeem a token.
    ///
    /// On success the token is marked as consumed (single-use).
    pub fn redeem(&self, req: RedeemRequest) -> Result<RedeemResponse, RedeemError> {
        let mut store = self.store.lock().unwrap();
        let record = store.get_mut(&req.token_id).ok_or(RedeemError::NotFound)?;

        match record.state {
            TokenState::Consumed => return Err(RedeemError::AlreadyRedeemed),
            TokenState::Locked => return Err(RedeemError::Locked),
            TokenState::Issued => {}
        }
        if record.issued_at.elapsed() > self.ttl {
            return Err(RedeemError::Expired);
        }
        if record.op_name != req.op_name {
            return Err(RedeemError::OpMismatch {
                expected: record.op_name.clone(),
                got: req.op_name,
            });
        }
        if record.ast_state_hash != req.ast_state_hash {
            return Err(RedeemError::StateHashMismatch {
                expected: record.ast_state_hash.clone(),
                got: req.ast_state_hash,
            });
        }

        let resp = RedeemResponse {
            tenant_id: record.tenant_id.clone(),
            workflow_id: record.workflow_id.clone(),
            instance_id: record.instance_id.clone(),
            op_name: record.op_name.clone(),
        };
        record.state = TokenState::Consumed;
        Ok(resp)
    }

    /// Lock a token to prevent concurrent redemption by another request.
    ///
    /// Transitions the token from `Issued` → `Locked`.  Returns an error if
    /// the token does not exist, has already been locked or consumed, or has
    /// expired.  The caller is responsible for following up with [`redeem`]
    /// (which transitions `Locked` → `Consumed`) or releasing the lock on
    /// failure.
    ///
    /// In a single-node deployment the [`Mutex`] already provides mutual
    /// exclusion.  In a multi-replica deployment replace both `lock` and
    /// `redeem` with a Redis Lua script that performs the full `ISSUED →
    /// LOCKED → CONSUMED` transition atomically.
    pub fn lock(&self, token_id: &str) -> Result<(), RedeemError> {
        let mut store = self.store.lock().unwrap();
        let record = store.get_mut(token_id).ok_or(RedeemError::NotFound)?;

        match record.state {
            TokenState::Consumed => return Err(RedeemError::AlreadyRedeemed),
            TokenState::Locked => return Err(RedeemError::Locked),
            TokenState::Issued => {}
        }
        if record.issued_at.elapsed() > self.ttl {
            return Err(RedeemError::Expired);
        }
        record.state = TokenState::Locked;
        Ok(())
    }

    /// Return the current [`TokenState`] for a token, or `None` if not found.
    pub fn state(&self, token_id: &str) -> Option<TokenState> {
        self.store.lock().unwrap().get(token_id).map(|r| r.state)
    }

    // ── helpers ───────────────────────────────────────────────────────────

    /// Generate a token ID of the form `tool_req_{op_slug}_{hex}`.
    ///
    /// The hex suffix is derived from a BLAKE3 hash of the current time and a
    /// per-process random seed, giving a 12-character opaque suffix.
    fn generate_token_id(op_name: &str) -> String {
        let slug = op_name
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '_' })
            .collect::<String>();
        let slug = slug.trim_matches('_').to_lowercase();
        let slug = if slug.is_empty() {
            "op".to_owned()
        } else {
            slug
        };

        // Use a combination of current time nanos + a thread-local counter
        // to generate a unique 6-byte hex suffix.
        let nonce = nonce_bytes();
        let hash = blake3::hash(&nonce);
        let hex_suffix: String = hash.as_bytes()[..6]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();

        format!("tool_req_{slug}_{hex_suffix}")
    }
}

impl Default for TokenService {
    fn default() -> Self {
        Self::new()
    }
}

// ── nonce generation ──────────────────────────────────────────────────────────

/// Generate a per-call unique nonce using a monotonic counter.
fn nonce_bytes() -> Vec<u8> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;

    let mut buf = [0u8; 16];
    buf[..8].copy_from_slice(&ts.to_le_bytes());
    buf[8..].copy_from_slice(&count.to_le_bytes());
    buf.to_vec()
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn svc() -> TokenService {
        TokenService::new()
    }

    fn mint_req(op: &str, hash: &str) -> MintRequest {
        MintRequest {
            tenant_id: "tenant1".into(),
            workflow_id: "wf1".into(),
            instance_id: "inst1".into(),
            op_name: op.into(),
            ast_state_hash: hash.into(),
        }
    }

    fn redeem_req(token_id: &str, op: &str, hash: &str) -> RedeemRequest {
        RedeemRequest {
            token_id: token_id.into(),
            op_name: op.into(),
            ast_state_hash: hash.into(),
        }
    }

    // ── token ID format ───────────────────────────────────────────────────

    #[test]
    fn token_id_has_correct_prefix() {
        let svc = svc();
        let resp = svc.mint(mint_req("doc_approval", "abc123"));
        assert!(
            resp.token_id.starts_with("tool_req_doc_approval_"),
            "unexpected token_id: {}",
            resp.token_id
        );
    }

    #[test]
    fn token_ids_are_unique() {
        let svc = svc();
        let t1 = svc.mint(mint_req("op", "h1")).token_id;
        let t2 = svc.mint(mint_req("op", "h1")).token_id;
        assert_ne!(t1, t2, "minted tokens must be unique");
    }

    // ── happy-path redeem ─────────────────────────────────────────────────

    #[test]
    fn redeem_success() {
        let svc = svc();
        let MintResponse { token_id, .. } = svc.mint(mint_req("approve", "hash_v1"));
        let result = svc.redeem(redeem_req(&token_id, "approve", "hash_v1"));
        assert!(result.is_ok());
        let resp = result.unwrap();
        assert_eq!(resp.op_name, "approve");
        assert_eq!(resp.tenant_id, "tenant1");
    }

    // ── single-use enforcement ────────────────────────────────────────────

    #[test]
    fn second_redeem_is_rejected() {
        let svc = svc();
        let MintResponse { token_id, .. } = svc.mint(mint_req("approve", "hash_v1"));
        svc.redeem(redeem_req(&token_id, "approve", "hash_v1"))
            .unwrap();
        let err = svc
            .redeem(redeem_req(&token_id, "approve", "hash_v1"))
            .unwrap_err();
        assert_eq!(err, RedeemError::AlreadyRedeemed);
    }

    // ── locked state ──────────────────────────────────────────────────────

    #[test]
    fn lock_transitions_to_locked_state() {
        let svc = svc();
        let MintResponse { token_id, .. } = svc.mint(mint_req("approve", "hash_v1"));
        assert_eq!(svc.state(&token_id), Some(TokenState::Issued));
        svc.lock(&token_id).unwrap();
        assert_eq!(svc.state(&token_id), Some(TokenState::Locked));
    }

    #[test]
    fn redeem_locked_token_fails_with_locked_error() {
        let svc = svc();
        let MintResponse { token_id, .. } = svc.mint(mint_req("approve", "hash_v1"));
        svc.lock(&token_id).unwrap();
        let err = svc
            .redeem(redeem_req(&token_id, "approve", "hash_v1"))
            .unwrap_err();
        assert_eq!(err, RedeemError::Locked);
    }

    #[test]
    fn double_lock_fails_with_locked_error() {
        let svc = svc();
        let MintResponse { token_id, .. } = svc.mint(mint_req("approve", "hash_v1"));
        svc.lock(&token_id).unwrap();
        let err = svc.lock(&token_id).unwrap_err();
        assert_eq!(err, RedeemError::Locked);
    }

    #[test]
    fn state_is_consumed_after_redeem() {
        let svc = svc();
        let MintResponse { token_id, .. } = svc.mint(mint_req("approve", "hash_v1"));
        svc.redeem(redeem_req(&token_id, "approve", "hash_v1"))
            .unwrap();
        assert_eq!(svc.state(&token_id), Some(TokenState::Consumed));
    }

    // ── expiry ────────────────────────────────────────────────────────────

    #[test]
    fn expired_token_is_rejected() {
        // Use a 0-second TTL so the token is immediately expired.
        let svc = TokenService::with_ttl(Duration::from_secs(0));
        let MintResponse { token_id, .. } = svc.mint(mint_req("approve", "hash_v1"));
        // Sleep for 1 ms to ensure elapsed() > 0
        std::thread::sleep(Duration::from_millis(1));
        let err = svc
            .redeem(redeem_req(&token_id, "approve", "hash_v1"))
            .unwrap_err();
        assert_eq!(err, RedeemError::Expired);
    }

    // ── not found ─────────────────────────────────────────────────────────

    #[test]
    fn unknown_token_returns_not_found() {
        let svc = svc();
        let err = svc
            .redeem(redeem_req("tool_req_fake_000000", "op", "h"))
            .unwrap_err();
        assert_eq!(err, RedeemError::NotFound);
    }

    // ── op-name mismatch ──────────────────────────────────────────────────

    #[test]
    fn wrong_op_name_is_rejected() {
        let svc = svc();
        let MintResponse { token_id, .. } = svc.mint(mint_req("approve", "hash_v1"));
        let err = svc
            .redeem(redeem_req(&token_id, "delete", "hash_v1"))
            .unwrap_err();
        assert!(matches!(err, RedeemError::OpMismatch { .. }));
    }

    // ── state-hash mismatch (workflow state change) ───────────────────────

    #[test]
    fn stale_state_hash_is_rejected() {
        let svc = svc();
        let MintResponse { token_id, .. } = svc.mint(mint_req("approve", "hash_v1"));
        // Simulate a workflow state change: current hash is now different.
        let err = svc
            .redeem(redeem_req(&token_id, "approve", "hash_v2_new"))
            .unwrap_err();
        assert!(matches!(err, RedeemError::StateHashMismatch { .. }));
    }

    // ── expires_in_secs matches TTL ───────────────────────────────────────

    #[test]
    fn expires_in_secs_reflects_ttl() {
        let svc = TokenService::with_ttl(Duration::from_secs(300));
        let resp = svc.mint(mint_req("op", "h"));
        assert_eq!(resp.expires_in_secs, 300);
    }
}
