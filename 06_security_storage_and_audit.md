# 06_security_storage_and_audit.md

## Architecture Review: Security, Storage & Audit

### Overview
This domain ensures that data is stored securely at rest (client and server), communications are authenticated, and all actions are auditable. It leverages standard browser APIs (IndexedDB, WebCrypto) for local storage and enterprise-grade KMS/HSM for server-side key management to meet compliance standards.

### Pros
*   **Local Encryption:** Using AES-GCM for IndexedDB storage ensures that if a device is stolen or inspected, the cached data is inaccessible without the key (managed via proper authentication flows).
*   **Tamper-Proof History:** The use of Blake3 hashing for snapshot verification and Ed25519 for operation signing creates an immutable chain of custody for all state changes.
*   **Auditability:** Because the system is event-sourced and deterministic, the entire history can be replayed to prove exactly *why* a state exists, satisfying strict compliance requirements (e.g., "Why did the AI reject this loan?").
*   **Key Management:** Integration with AWS KMS / on-prem HSM ensures that master signing keys are never exposed in plaintext and are protected by hardware security modules.

### Cons & Risks
*   **Key Management Complexity:** Rotating keys for millions of client devices and re-encrypting local data is complex and prone to "lockout" bugs where users lose access to offline work.
*   **Browser Storage Limits:** IndexedDB quotas vary by browser and disk space; hitting a quota can cause silent data loss or application failure if not handled proactively.
*   **Performance:** Extensive hashing (Blake3) and encryption/decryption on the main thread (if not offloaded to Web Workers) can cause UI stutter during heavy sync operations.
*   **XSS Vulnerabilities:** If an attacker gains XSS execution, they can theoretically access the decrypted data in memory or use the handle to sign operations, bypassing storage encryption.

### Single Points of Failure (SPOF)
*   **KMS:** If the Key Management Service is unreachable, the server cannot verify signatures or sign new authoritative blocks, halting all write operations.
*   **Audit Log Integrity:** If the append-only log (in Postgres/Temporal) is compromised or deleted, the ability to prove historical state and debug issues is permanently lost.

## Testing Strategy

### Security & Crypto Testing
We must verify that the cryptographic promises hold true under attack and operational stress.

*   **Local Extraction:** Attempt to extract data from the IndexedDB files directly on the filesystem (e.g., using browser dev tools or OS file access). Verify that it is unreadable ciphertext without the correct AES-GCM key.
*   **Signature Forgery:** Attempt to submit an `op_id` with a modified payload (e.g., changing a value from $10 to $1000) but the original signature. Verify the server rejects it with a signature mismatch error.
*   **Key Rotation:** Trigger a key rotation event. Verify that the client can still read old data (using the old key or after re-encryption) and that new data uses the new key, with no data loss.
*   **Replay Debugger:** Use the `gp2f replay --op-id xxx` tool to replay a sequence of ops from production. Verify that the resulting state hash matches the production state hash exactly (bit-for-bit), proving determinism.

### Specific Test Cases & Scenarios
*   **Audit Log Immutability:** Attempt to manually modify a row in the Temporal history or Postgres events table. Run the verification tool to detect the hash mismatch in the chain (merkle tree or hash chain).
*   **XSS Simulation:** Inject a mock malicious script that attempts to export the WebCrypto `CryptoKey` object. Verify that `extractable: false` prevents this (if configured correctly).
*   **Weak Randomness:** Verify that nonces and IVs are generated using `crypto.getRandomValues()` and not `Math.random()`, to prevent prediction attacks.
*   **Storage Quota:** Fill the local IndexedDB to its limit with dummy data. Verify the application handles the `QuotaExceededError` gracefully (e.g., by evicting old caches or notifying the user) rather than crashing.

### Tools
*   **SubtleCrypto Test Suite:** Standard tests to verify browser implementation of AES-GCM/Ed25519 (sanity check across different browsers).
*   **OWASP ZAP / Burp Suite:** For intercepting traffic and attempting to replay or modify signed WebSocket frames.
*   **KMS Simulator:** For testing key rotation and API failures without racking up AWS bills (e.g., using LocalStack).
*   **Custom Audit Verifier:** A Rust tool that walks the event log, re-calculates the Blake3 hashes for every step, and asserts the chain integrity against the stored snapshots.
