//! Tower middleware: cryptographic op_id validation + replay protection.
//!
//! ## Op-ID format
//!
//! Each op carries an `X-Op-Id` HTTP header with the format:
//! ```text
//! {timestamp_secs}.{nonce_base64url}
//! ```
//! and a companion `X-Op-Signature` header containing the base64url-encoded
//! Ed25519 signature over the raw `X-Op-Id` value concatenated with the
//! client identifier (`X-Client-Id` header):
//! ```text
//! signing_message = op_id_header_value || ":" || client_id
//! signature       = ed25519_sign(private_key, signing_message)
//! X-Op-Signature  = base64url(signature)
//! ```
//!
//! ## Checks performed (in order)
//!
//! 1. Parse `X-Op-Id` → extract `timestamp_secs` and `nonce`.
//! 2. Reject with **409** if `|now - timestamp_secs| > 30 s`.
//! 3. Reject with **409** if the `nonce` is already in the bloom filter (replay).
//! 4. Verify the Ed25519 signature using the per-client public key from the
//!    [`PublicKeyStore`].  Reject with **401** on failure.
//! 5. Insert the nonce into the bloom filter.
//!
//! Requests that do not carry `X-Op-Id` / `X-Op-Signature` headers are passed
//! through unchanged (unauthenticated / dev mode, mirrors the existing HMAC
//! behaviour).

use axum::http::{Request, Response, StatusCode};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::{Signature, VerifyingKey};
use rand::Rng;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, RwLock},
    task::{Context, Poll},
    time::{SystemTime, UNIX_EPOCH},
};
use tower::{Layer, Service};

// ── bloom filter ──────────────────────────────────────────────────────────────

/// Bits in the nonce bloom filter.  2 000 000 bits gives FPR < 0.001 at
/// ~100 000 nonces with 7 hash functions.
const BLOOM_BITS: usize = 2_000_000;
const BLOOM_HASHES: usize = 7;

fn bloom_positions(item: &[u8]) -> [usize; BLOOM_HASHES] {
    let mut h1: u64 = 14_695_981_039_346_656_037;
    for &b in item {
        h1 ^= u64::from(b);
        h1 = h1.wrapping_mul(1_099_511_628_211);
    }
    let mut h2 = h1 ^ (h1 >> 30);
    h2 = h2.wrapping_mul(0xbf58476d1ce4e5b9);
    h2 ^= h2 >> 27;
    h2 = h2.wrapping_mul(0x94d049bb133111eb);
    h2 ^= h2 >> 31;
    let mut out = [0usize; BLOOM_HASHES];
    for (i, pos) in out.iter_mut().enumerate() {
        *pos = (h1.wrapping_add((i as u64).wrapping_mul(h2)) as usize) % BLOOM_BITS;
    }
    out
}

/// Shared nonce bloom filter + exact set for the sliding window.
struct NonceStore {
    bloom: Box<[u64; BLOOM_BITS / 64]>,
    exact: HashSet<String>,
}

impl NonceStore {
    fn new() -> Self {
        Self {
            bloom: Box::new([0u64; BLOOM_BITS / 64]),
            exact: HashSet::new(),
        }
    }

    fn contains(&self, nonce: &str) -> bool {
        if self.exact.contains(nonce) {
            return true;
        }
        for bit in bloom_positions(nonce.as_bytes()) {
            if self.bloom[bit / 64] & (1u64 << (bit % 64)) == 0 {
                return false;
            }
        }
        true
    }

    fn insert(&mut self, nonce: String) {
        for bit in bloom_positions(nonce.as_bytes()) {
            self.bloom[bit / 64] |= 1u64 << (bit % 64);
        }
        self.exact.insert(nonce);
    }
}

// ── public key store ──────────────────────────────────────────────────────────

/// Registry of per-client Ed25519 verifying keys.
///
/// In production this is backed by Redis; this in-memory implementation is
/// used for tests and standalone deployments.
///
/// **Production path**: replace [`InMemoryPublicKeyStore`] with a
/// `RedisPublicKeyStore` that calls
/// `redis GET pubkey:{client_id}` for each request.
pub trait PublicKeyStore: Send + Sync {
    /// Look up a client's [`VerifyingKey`] by `client_id`.
    fn get(&self, client_id: &str) -> Option<VerifyingKey>;
}

/// Simple in-memory public key store suitable for tests and dev deployments.
///
/// # Deprecation
///
/// **Do not use in production.**  Use [`EnvVarKeyProvider`] (populated from a
/// Kubernetes Secret or AWS Secrets Manager environment injection) or
/// [`PollingKeyProvider`] (which also supports live key rotation) instead.
#[deprecated(
    since = "0.2.0",
    note = "InMemoryPublicKeyStore is for tests and dev only; \
            use EnvVarKeyProvider or PollingKeyProvider in production"
)]
#[derive(Default)]
pub struct InMemoryPublicKeyStore {
    keys: Mutex<HashMap<String, VerifyingKey>>,
}

#[allow(deprecated)]
impl InMemoryPublicKeyStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a client's verifying key.
    pub fn insert(&self, client_id: impl Into<String>, key: VerifyingKey) {
        self.keys.lock().unwrap().insert(client_id.into(), key);
    }
}

#[allow(deprecated)]
impl PublicKeyStore for InMemoryPublicKeyStore {
    fn get(&self, client_id: &str) -> Option<VerifyingKey> {
        self.keys.lock().unwrap().get(client_id).copied()
    }
}

// ── EnvVarKeyProvider ─────────────────────────────────────────────────────────

/// Public key provider that reads keys from the `KEYS_JSON` environment
/// variable (suitable for Kubernetes Secrets / Docker env injection).
///
/// `KEYS_JSON` must be a JSON object mapping `client_id` → 32-byte hex-encoded
/// Ed25519 verifying key:
///
/// ```json
/// {"client-a": "0102...1f20", "client-b": "abcd...ef01"}
/// ```
///
/// Keys are loaded once at construction time.  To rotate keys, redeploy the
/// server with the updated `KEYS_JSON` value.
pub struct EnvVarKeyProvider {
    keys: HashMap<String, VerifyingKey>,
}

impl EnvVarKeyProvider {
    /// Parse `KEYS_JSON` and return a provider loaded with those keys.
    ///
    /// Returns an empty provider if the env var is not set or if any key
    /// fails to parse (with a warning log per failed entry).
    pub fn from_env() -> Self {
        Self {
            keys: load_keys_from_env(),
        }
    }
}

impl PublicKeyStore for EnvVarKeyProvider {
    fn get(&self, client_id: &str) -> Option<VerifyingKey> {
        self.keys.get(client_id).copied()
    }
}

// ── shared key-loading helper ─────────────────────────────────────────────────

/// Load the Ed25519 verifying key map from the `KEYS_JSON` environment
/// variable.  Returns an empty map when the variable is absent or invalid.
///
/// This is the shared loading logic used by both [`EnvVarKeyProvider`] and
/// [`PollingKeyProvider`].
fn load_keys_from_env() -> HashMap<String, VerifyingKey> {
    let raw = match crate::secrets::resolve_secret("KEYS_JSON") {
        Some(v) => v,
        None => return HashMap::new(),
    };
    let map: HashMap<String, String> = match serde_json::from_str(&raw) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("KEYS_JSON is not valid JSON: {e}");
            return HashMap::new();
        }
    };
    let mut keys = HashMap::new();
    for (client_id, hex_key) in map {
        match parse_verifying_key(&hex_key) {
            Ok(vk) => {
                keys.insert(client_id, vk);
            }
            Err(e) => {
                tracing::warn!(client_id = %client_id, "KEYS_JSON: skipping invalid key: {e}");
            }
        }
    }
    keys
}

// ── PollingKeyProvider ────────────────────────────────────────────────────────

/// A key provider that periodically reloads keys from the `KEYS_JSON`
/// environment variable to support **key rotation without a server restart**.
///
/// At construction time keys are loaded immediately.  A background task then
/// wakes every `interval` and replaces the in-memory map with a fresh load.
/// This enables live rotation:
///
/// 1. Update the Kubernetes Secret (or AWS Secrets Manager value) that
///    injects `KEYS_JSON` into the pod environment.
/// 2. The pod's `/proc/self/environ` or mounted-file watcher reflects the new
///    value; the next poll picks it up automatically.
///
/// For Kubernetes, mount the secret as a file and read it in the reload
/// callback instead of using `std::env::var` if you prefer inotify-style
/// updates over polling.
pub struct PollingKeyProvider {
    keys: Arc<RwLock<HashMap<String, VerifyingKey>>>,
}

impl PollingKeyProvider {
    /// Create a new provider and spawn a background reload task.
    ///
    /// `interval` controls how often `KEYS_JSON` is re-read.  A value of
    /// 60 seconds is a reasonable production default.
    ///
    /// **Must be called inside a Tokio runtime.**
    pub fn new(interval: std::time::Duration) -> Self {
        let keys = Arc::new(RwLock::new(load_keys_from_env()));
        let keys_clone = keys.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                let updated = load_keys_from_env();
                tracing::debug!(
                    count = updated.len(),
                    "PollingKeyProvider: reloaded KEYS_JSON"
                );
                match keys_clone.write() {
                    Ok(mut guard) => *guard = updated,
                    Err(poisoned) => {
                        // RwLock is poisoned (a previous writer panicked).  Recover by
                        // overwriting the poisoned data with the fresh snapshot so the
                        // server keeps serving valid keys rather than crashing.
                        tracing::error!(
                            "PollingKeyProvider: RwLock poisoned; recovering with fresh keys"
                        );
                        *poisoned.into_inner() = updated;
                    }
                }
            }
        });
        Self { keys }
    }
}

impl PublicKeyStore for PollingKeyProvider {
    fn get(&self, client_id: &str) -> Option<VerifyingKey> {
        self.keys
            .read()
            .ok()
            .and_then(|guard| guard.get(client_id).copied())
    }
}

/// Parse a 64-character hex string into an Ed25519 [`VerifyingKey`].
fn parse_verifying_key(hex: &str) -> Result<VerifyingKey, String> {
    if hex.len() != 64 {
        return Err(format!("expected 64 hex chars, got {}", hex.len()));
    }
    let mut bytes = [0u8; 32];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        let s = std::str::from_utf8(chunk).map_err(|e| e.to_string())?;
        bytes[i] = u8::from_str_radix(s, 16).map_err(|e| e.to_string())?;
    }
    VerifyingKey::from_bytes(&bytes).map_err(|e| e.to_string())
}

// ── middleware state ──────────────────────────────────────────────────────────

/// Shared state for [`OpIdLayer`].
#[derive(Clone)]
pub struct OpIdState {
    /// Per-client public keys.
    key_store: Arc<dyn PublicKeyStore>,
    /// Nonce replay protection.
    nonce_store: Arc<Mutex<NonceStore>>,
}

impl OpIdState {
    pub fn new(key_store: Arc<dyn PublicKeyStore>) -> Self {
        Self {
            key_store,
            nonce_store: Arc::new(Mutex::new(NonceStore::new())),
        }
    }
}

/// Generate a fresh op_id header value of the form `{ts_secs}.{nonce_base64url}`.
pub fn generate_op_id() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let nonce_bytes: [u8; 16] = rand::thread_rng().gen();
    let nonce = URL_SAFE_NO_PAD.encode(nonce_bytes);
    format!("{ts}.{nonce}")
}

// ── layer ─────────────────────────────────────────────────────────────────────

/// Tower [`Layer`] that wraps every service with op_id verification.
#[derive(Clone)]
pub struct OpIdLayer {
    state: OpIdState,
}

impl OpIdLayer {
    pub fn new(key_store: Arc<dyn PublicKeyStore>) -> Self {
        Self {
            state: OpIdState::new(key_store),
        }
    }
}

impl<S> Layer<S> for OpIdLayer {
    type Service = OpIdMiddleware<S>;

    fn layer(&self, inner: S) -> Self::Service {
        OpIdMiddleware {
            inner,
            state: self.state.clone(),
        }
    }
}

// ── service ───────────────────────────────────────────────────────────────────

/// Tower [`Service`] that enforces op_id verification before forwarding.
#[derive(Clone)]
pub struct OpIdMiddleware<S> {
    inner: S,
    state: OpIdState,
}

impl<S, ReqBody, ResBody> Service<Request<ReqBody>> for OpIdMiddleware<S>
where
    S: Service<Request<ReqBody>, Response = Response<ResBody>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    ReqBody: Send + 'static,
    ResBody: Default + Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        let state = self.state.clone();

        let headers = req.headers();
        let op_id_hdr = headers
            .get("x-op-id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_owned());
        let sig_hdr = headers
            .get("x-op-signature")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_owned());
        let client_id = headers
            .get("x-client-id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_owned());

        // If no op-id header is present, pass through (dev/unauthenticated mode).
        if op_id_hdr.is_none() {
            let fut = self.inner.call(req);
            return Box::pin(fut);
        }

        if let Some(err) = validate_op_id(
            op_id_hdr.as_deref().unwrap_or(""),
            sig_hdr.as_deref(),
            client_id.as_deref(),
            &state,
        ) {
            let status = match err {
                ValidationError::TimestampOutOfRange | ValidationError::ReplayDetected => {
                    StatusCode::CONFLICT
                }
                ValidationError::MissingSignature
                | ValidationError::InvalidSignatureFormat
                | ValidationError::UnknownClient
                | ValidationError::SignatureInvalid => StatusCode::UNAUTHORIZED,
                ValidationError::BadOpIdFormat => StatusCode::BAD_REQUEST,
            };
            return Box::pin(async move {
                let mut resp = Response::new(ResBody::default());
                *resp.status_mut() = status;
                Ok(resp)
            });
        }

        let fut = self.inner.call(req);
        Box::pin(fut)
    }
}

// ── validation logic ──────────────────────────────────────────────────────────

/// Errors that can occur during op_id validation.
#[derive(Debug, PartialEq, Eq)]
pub enum ValidationError {
    BadOpIdFormat,
    TimestampOutOfRange,
    ReplayDetected,
    MissingSignature,
    InvalidSignatureFormat,
    UnknownClient,
    SignatureInvalid,
}

/// Validate an op_id header and (optionally) its signature.
///
/// Returns `None` on success; `Some(err)` on failure.
/// On success the nonce is inserted into the bloom filter.
pub fn validate_op_id(
    op_id: &str,
    signature: Option<&str>,
    client_id: Option<&str>,
    state: &OpIdState,
) -> Option<ValidationError> {
    // Parse "{ts}.{nonce}".
    let mut parts = op_id.splitn(2, '.');
    let ts_str = match parts.next() {
        Some(s) => s,
        None => return Some(ValidationError::BadOpIdFormat),
    };
    let nonce = match parts.next() {
        Some(s) if !s.is_empty() => s,
        _ => return Some(ValidationError::BadOpIdFormat),
    };

    let ts_secs: u64 = match ts_str.parse() {
        Ok(v) => v,
        Err(_) => return Some(ValidationError::BadOpIdFormat),
    };

    // Timestamp check: ±30 s.
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if now.abs_diff(ts_secs) > 30 {
        return Some(ValidationError::TimestampOutOfRange);
    }

    // Nonce replay check.
    {
        let nonce_store = state.nonce_store.lock().unwrap();
        if nonce_store.contains(nonce) {
            return Some(ValidationError::ReplayDetected);
        }
    }

    // Ed25519 signature verification (only when signature header is present).
    if let Some(sig_b64) = signature {
        let cid = client_id.unwrap_or("");
        if cid.is_empty() {
            return Some(ValidationError::UnknownClient);
        }

        match state.key_store.get(cid) {
            None => return Some(ValidationError::UnknownClient),
            Some(key) => {
                let sig_bytes = match URL_SAFE_NO_PAD.decode(sig_b64) {
                    Ok(b) => b,
                    Err(_) => return Some(ValidationError::InvalidSignatureFormat),
                };
                if sig_bytes.len() != 64 {
                    return Some(ValidationError::InvalidSignatureFormat);
                }
                let sig_arr: [u8; 64] = sig_bytes.try_into().unwrap();
                let sig = Signature::from_bytes(&sig_arr);
                let message = format!("{op_id}:{cid}");
                use ed25519_dalek::Verifier;
                if key.verify(message.as_bytes(), &sig).is_err() {
                    return Some(ValidationError::SignatureInvalid);
                }
            }
        }
    } else if client_id.is_some() {
        // client_id provided but no signature → require signature.
        return Some(ValidationError::MissingSignature);
    }

    // All checks passed → insert nonce.
    state.nonce_store.lock().unwrap().insert(nonce.to_owned());
    None
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    #[allow(deprecated)]
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use rand::rngs::OsRng;

    #[allow(deprecated)]
    fn make_state(keys: &[(&str, VerifyingKey)]) -> OpIdState {
        let store = Arc::new(InMemoryPublicKeyStore::new());
        for (id, key) in keys {
            store.insert(*id, *key);
        }
        OpIdState::new(store)
    }

    fn ts_now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    fn fresh_op_id() -> String {
        let nonce_bytes: [u8; 16] = rand::thread_rng().gen();
        let nonce = URL_SAFE_NO_PAD.encode(nonce_bytes);
        format!("{}.{}", ts_now(), nonce)
    }

    #[test]
    fn valid_op_id_without_signature_passes() {
        let state = make_state(&[]);
        let op_id = fresh_op_id();
        assert!(validate_op_id(&op_id, None, None, &state).is_none());
    }

    #[test]
    fn stale_timestamp_is_rejected() {
        let state = make_state(&[]);
        let stale_ts = ts_now() - 31;
        let nonce = URL_SAFE_NO_PAD.encode([1u8; 16]);
        let op_id = format!("{stale_ts}.{nonce}");
        assert_eq!(
            validate_op_id(&op_id, None, None, &state),
            Some(ValidationError::TimestampOutOfRange)
        );
    }

    #[test]
    fn future_timestamp_is_rejected() {
        let state = make_state(&[]);
        let future_ts = ts_now() + 31;
        let nonce = URL_SAFE_NO_PAD.encode([2u8; 16]);
        let op_id = format!("{future_ts}.{nonce}");
        assert_eq!(
            validate_op_id(&op_id, None, None, &state),
            Some(ValidationError::TimestampOutOfRange)
        );
    }

    #[test]
    fn replay_is_detected() {
        let state = make_state(&[]);
        let op_id = fresh_op_id();
        // First call succeeds and inserts nonce.
        assert!(validate_op_id(&op_id, None, None, &state).is_none());
        // Second call must be rejected.
        assert_eq!(
            validate_op_id(&op_id, None, None, &state),
            Some(ValidationError::ReplayDetected)
        );
    }

    #[test]
    fn valid_ed25519_signature_passes() {
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = signing_key.verifying_key();
        let state = make_state(&[("client-1", verifying_key)]);

        let op_id = fresh_op_id();
        let message = format!("{op_id}:client-1");
        let sig = signing_key.sign(message.as_bytes());
        let sig_b64 = URL_SAFE_NO_PAD.encode(sig.to_bytes());

        assert!(validate_op_id(&op_id, Some(&sig_b64), Some("client-1"), &state).is_none());
    }

    #[test]
    fn invalid_signature_is_rejected() {
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = signing_key.verifying_key();
        let state = make_state(&[("client-1", verifying_key)]);

        let op_id = fresh_op_id();
        let bad_sig = URL_SAFE_NO_PAD.encode([0u8; 64]);

        assert_eq!(
            validate_op_id(&op_id, Some(&bad_sig), Some("client-1"), &state),
            Some(ValidationError::SignatureInvalid)
        );
    }

    #[test]
    fn unknown_client_is_rejected() {
        let state = make_state(&[]);
        let op_id = fresh_op_id();
        let sig_b64 = URL_SAFE_NO_PAD.encode([0u8; 64]);
        assert_eq!(
            validate_op_id(&op_id, Some(&sig_b64), Some("ghost"), &state),
            Some(ValidationError::UnknownClient)
        );
    }

    #[test]
    fn bad_op_id_format_rejected() {
        let state = make_state(&[]);
        assert_eq!(
            validate_op_id("not-valid-at-all", None, None, &state),
            Some(ValidationError::BadOpIdFormat)
        );
    }

    #[test]
    fn generate_op_id_has_correct_format() {
        let id = generate_op_id();
        let mut parts = id.splitn(2, '.');
        let ts: u64 = parts.next().unwrap().parse().unwrap();
        let nonce = parts.next().unwrap();
        assert!(ts > 0);
        assert!(!nonce.is_empty());
    }

    // ── EnvVarKeyProvider tests ───────────────────────────────────────────

    #[test]
    fn env_var_key_provider_empty_when_no_env_var() {
        // Ensure the env var is not set.
        std::env::remove_var("KEYS_JSON");
        let provider = EnvVarKeyProvider::from_env();
        assert!(provider.get("any-client").is_none());
    }

    #[test]
    fn env_var_key_provider_loads_valid_key() {
        use ed25519_dalek::SigningKey;
        let signing_key = SigningKey::generate(&mut OsRng);
        let hex_key: String = signing_key
            .verifying_key()
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let json = format!(r#"{{"client-a": "{hex_key}"}}"#);
        std::env::set_var("KEYS_JSON", &json);
        let provider = EnvVarKeyProvider::from_env();
        assert!(provider.get("client-a").is_some());
        assert!(provider.get("other-client").is_none());
        std::env::remove_var("KEYS_JSON");
    }

    #[test]
    fn env_var_key_provider_skips_invalid_hex() {
        std::env::set_var("KEYS_JSON", r#"{"bad-client": "notvalidhex"}"#);
        let provider = EnvVarKeyProvider::from_env();
        assert!(provider.get("bad-client").is_none());
        std::env::remove_var("KEYS_JSON");
    }

    #[test]
    fn env_var_key_provider_invalid_json_returns_empty() {
        std::env::set_var("KEYS_JSON", "not-json");
        let provider = EnvVarKeyProvider::from_env();
        assert!(provider.get("any").is_none());
        std::env::remove_var("KEYS_JSON");
    }
}
