//! op_id signature validation.
//!
//! Each `ClientMessage` may carry a `clientSignature` field: a base64url
//! HMAC-SHA256 computed over the canonical op fields using a per-tenant secret.
//!
//! Construction (matches `docs/wire-protocol.md`):
//! ```text
//! message = op_id || ":" || tenant_id || ":" || workflow_id || ":" ||
//!           instance_id || ":" || ast_version || ":" || action || ":" ||
//!           client_snapshot_hash
//! tag     = HMAC-SHA256(key = tenant_secret, message)
//! signature = base64url(tag)
//! ```

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use hmac_sha256::HMAC;

use crate::wire::ClientMessage;

/// Verify the `client_signature` carried by `msg` against `tenant_secret`.
///
/// Returns `Ok(())` when:
/// - The message has no `client_signature` (unauthenticated / dev mode), or
/// - The signature is present and valid.
///
/// Returns `Err(reason)` when the signature is present but invalid.
pub fn verify_signature(msg: &ClientMessage, tenant_secret: &[u8]) -> Result<(), String> {
    let Some(sig_b64) = msg.client_signature.as_deref() else {
        // No signature – allowed in development / unauthenticated mode.
        return Ok(());
    };

    let canonical = build_canonical(msg);
    let expected_tag = HMAC::mac(canonical.as_bytes(), tenant_secret);
    let expected_b64 = URL_SAFE_NO_PAD.encode(expected_tag);

    if sig_b64 == expected_b64 {
        Ok(())
    } else {
        Err(format!("invalid client signature for op_id={}", msg.op_id))
    }
}

/// Build the canonical signing string for a [`ClientMessage`].
pub fn build_canonical(msg: &ClientMessage) -> String {
    format!(
        "{}:{}:{}:{}:{}:{}:{}",
        msg.op_id,
        msg.tenant_id,
        msg.workflow_id,
        msg.instance_id,
        msg.ast_version,
        msg.action,
        msg.client_snapshot_hash,
    )
}

/// Sign a [`ClientMessage`] using `tenant_secret` and return the base64url tag.
pub fn sign_message(msg: &ClientMessage, tenant_secret: &[u8]) -> String {
    let canonical = build_canonical(msg);
    let tag = HMAC::mac(canonical.as_bytes(), tenant_secret);
    URL_SAFE_NO_PAD.encode(tag)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::ClientMessage;

    fn test_msg(sig: Option<String>) -> ClientMessage {
        ClientMessage {
            op_id: "op-1".into(),
            ast_version: "1.0.0".into(),
            action: "submit".into(),
            payload: serde_json::Value::Null,
            client_snapshot_hash: "aaaa".into(),
            tenant_id: "tenant42".into(),
            workflow_id: "wf1".into(),
            instance_id: "inst1".into(),
            client_signature: sig,
        }
    }

    #[test]
    fn no_signature_passes() {
        let msg = test_msg(None);
        assert!(verify_signature(&msg, b"secret").is_ok());
    }

    #[test]
    fn valid_signature_accepted() {
        let msg = test_msg(None);
        let sig = sign_message(&msg, b"secret");
        let msg_signed = test_msg(Some(sig));
        assert!(verify_signature(&msg_signed, b"secret").is_ok());
    }

    #[test]
    fn tampered_signature_rejected() {
        let msg = test_msg(Some("bogus_sig".into()));
        assert!(verify_signature(&msg, b"secret").is_err());
    }

    #[test]
    fn wrong_key_rejected() {
        let msg = test_msg(None);
        let sig = sign_message(&msg, b"correct_key");
        let msg_signed = test_msg(Some(sig));
        assert!(verify_signature(&msg_signed, b"wrong_key").is_err());
    }
}
