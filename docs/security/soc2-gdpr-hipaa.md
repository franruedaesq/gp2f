# GP2F Security Certification Readiness

> **Status**: Phase 9 – Production Hardening & Pilot Preparation  
> Covers SOC 2 Type II, GDPR, and HIPAA alignment for enterprise deployments.

---

## Table of Contents

1. [Encryption & Key Management](#1-encryption--key-management)
2. [Audit Log Schema & Retention](#2-audit-log-schema--retention)
3. [Data-Flow Diagrams](#3-data-flow-diagrams)
4. [SOC 2 Type II Controls Mapping](#4-soc-2-type-ii-controls-mapping)
5. [GDPR Compliance Controls](#5-gdpr-compliance-controls)
6. [HIPAA Technical Safeguards](#6-hipaa-technical-safeguards)
7. [Penetration-Testing & Vulnerability Management](#7-penetration-testing--vulnerability-management)
8. [Incident Response](#8-incident-response)

---

## 1. Encryption & Key Management

### In-Transit Encryption
| Layer | Algorithm | Notes |
|-------|-----------|-------|
| HTTP/WebSocket | TLS 1.3 | Enforced at the load-balancer; HSTS header required |
| Internal service mesh | mTLS | Zero-trust; certificates rotated every 24 hours by cert-manager |

### At-Rest Encryption
| Storage | Algorithm | Key lifecycle |
|---------|-----------|---------------|
| Event store (PostgreSQL) | AES-256-GCM (column-level) | Master key in cloud KMS; rotated annually |
| Blob/file attachments | AES-256-GCM (envelope) | Data-encryption key per tenant; rotated on tenant request |
| Audit logs | AES-256-GCM | Same as event store; separate KMS key alias |

### Signing Keys
| Purpose | Algorithm | TTL | Storage |
|---------|-----------|-----|---------|
| `op_id` HMAC signing | HMAC-SHA256 | 90 days (auto-rotated) | Cloud KMS / HSM |
| AI ephemeral tokens | 256-bit random | 5 minutes | In-process memory only; never persisted |
| JWT session tokens | RS256 (2048-bit) | 24 hours | Cloud KMS |

### Key Rotation Runbook
1. New key version created in KMS.
2. Server starts dual-validation (old + new) window: **2 hours**.
3. Old key version deactivated (not deleted) after all in-flight ops flush.
4. Old key version deleted after **30 days** (retention period for forensic replay).

---

## 2. Audit Log Schema & Retention

Every operation that passes through the reconciler produces a `StoredEvent` (see
[`server/src/event_store.rs`](../server/src/event_store.rs)).  The JSON representation
below is the canonical audit-log record format.

### Audit Record Fields

```json
{
  "seq": 42,
  "ingested_at": "2025-06-01T12:34:56.789Z",
  "outcome": "ACCEPTED",
  "message": {
    "op_id": "v1.tenant-abc.client-xyz.000001.1748773200000.nonce16.HMAC",
    "ast_version": "1.2.0",
    "action": "update",
    "tenant_id": "tenant-abc",
    "workflow_id": "medical_triage_intake",
    "instance_id": "inst-2025-06-01-001",
    "role": "clinician",
    "client_snapshot_hash": "<blake3-hex>",
    "payload": { "consent_given": true },
    "client_signature": "<hmac-sha256-base64url>",
    "vibe": {
      "intent": "focused",
      "confidence": 0.91,
      "bottleneck": "consent_step"
    }
  }
}
```

### Retention Policies

| Data category | Retention | Legal basis |
|---------------|-----------|-------------|
| Accepted op records | 7 years | SOC 2 / HIPAA audit trail |
| Rejected op records | 90 days | Operational debugging |
| Compacted snapshots | 7 years | Event-sourcing replay |
| AI proposal attempts (rejected) | 30 days | Security forensics |
| Session / JWT logs | 90 days | SOC 2 CC6.1 |
| PHI-touching event records | 6 years | HIPAA §164.530(j) |

> **Implementation note**: The in-memory `EventStore` in the current codebase must be
> replaced by a durable append-only store (e.g., PostgreSQL + WAL archiving) before
> production.  Compaction is logged with a `compacted:<start>..<end>` marker so full
> replay history is preserved.

---

## 3. Data-Flow Diagrams

### 3.1 Normal (Online) Operation

```
┌────────────────────────────────────────────────────────────────────┐
│  CLIENT (Browser / Mobile)                                         │
│                                                                    │
│  ┌──────────────┐  1. Predict locally        ┌──────────────────┐ │
│  │  React UI    │─────────────────────────→  │  WASM evaluator  │ │
│  │  + SDK       │  2. Emit signed op_id       │  (policy-core)   │ │
│  └──────┬───────┘ ←─────────────────────────  └──────────────────┘ │
│         │  (optimistic update)                                      │
└─────────┼──────────────────────────────────────────────────────────┘
          │  TLS 1.3 WebSocket
          ▼
┌─────────────────────────────────────────────────────────────────────┐
│  SERVER (GP2F Axum)                                                 │
│                                                                     │
│  ┌─────────────┐   3. Validate signature    ┌──────────────────┐   │
│  │  Reconciler │──────────────────────────→ │  HMAC verifier   │   │
│  │             │   4. Replay protection     │  Bloom filter    │   │
│  │             │   5. RBAC check            │  RBAC registry   │   │
│  │             │   6. Snapshot hash agree.  └──────────────────┘   │
│  │             │   7. Policy evaluation     ┌──────────────────┐   │
│  │             │──────────────────────────→ │  AST evaluator   │   │
│  │             │                            └──────────────────┘   │
│  │             │   8. Append to event store ┌──────────────────┐   │
│  │             │──────────────────────────→ │  Event store     │   │
│  │             │                            │  (append-only)   │   │
│  │             │   9. Broadcast ACCEPT/REJ  └──────────────────┘   │
│  └─────────────┘──────────────────────────→ WebSocket broadcaster  │
└─────────────────────────────────────────────────────────────────────┘
```

### 3.2 Offline / Reconnect Flow

```
CLIENT (offline)
  │
  ├─ Predicts locally using WASM evaluator
  ├─ Queues signed ops in IndexedDB (encrypted with device key)
  │
(network restored)
  │
  └─ Flushes queue with refreshed snapshot hash
       │
       └──→ SERVER reconciler processes each op
              ├─ ACCEPT: state advanced
              └─ REJECT (stale hash): client receives 3-way patch → merge UI shown
```

### 3.3 AI / Agent Flow

```
LLM / Agent
  │
  ├─ Receives ONLY: VibeVector + ephemeral token + allowed-actions list
  │   (no raw state, no credentials, no PII)
  │
  └─ POSTs to /ai/propose  ──────────────────────────→  SERVER
                                                          │
                                              Same reconciler pipeline
                                              (RBAC + policy + replay protection)
                                                          │
                                              ACCEPT → logged; broadcast
                                              REJECT → silently dropped; logged
```

### 3.4 PHI Data Boundary (HIPAA)

```
┌──────────────────────────────────────────────────────────────────────┐
│  PHI Boundary                                                        │
│                                                                      │
│  ┌──────────────────────┐   column-level AES-256  ┌───────────────┐ │
│  │  medical_triage      │────────────────────────→│  PostgreSQL   │ │
│  │  workflow instances  │                          │  (encrypted)  │ │
│  └──────────────────────┘                          └───────────────┘ │
│                                                                      │
│  VibeVector, op payloads, and audit logs remain inside this boundary │
│  AI tokens: ephemeral only; no PHI ever crosses to the LLM          │
└──────────────────────────────────────────────────────────────────────┘
```

---

## 4. SOC 2 Type II Controls Mapping

| Trust Service Criterion | Control | GP2F Implementation |
|-------------------------|---------|---------------------|
| **CC6.1** Logical access | Authentication | HMAC-signed `op_id`; JWT session tokens; MFA for admin console |
| **CC6.2** Prior to issuing credentials | Credential provisioning | Tenant onboarding workflow; secrets stored in KMS, never in code |
| **CC6.3** Role-based access | Authorization | `RbacRegistry` (see `server/src/rbac.rs`); AST-based `access_policy` per workflow |
| **CC6.6** Restrict access to authorised users | Replay protection | Bloom filter + exact-window replay guard (`server/src/replay_protection.rs`) |
| **CC6.7** Restrict transmission of information | TLS / mTLS | All inter-service communication over TLS 1.3 with pinned certificates |
| **CC7.1** Detect & monitor security events | Audit logging | Append-only `EventStore`; every op, outcome, role, and tenant logged |
| **CC7.2** Monitor for anomalies | Metrics & alerting | Prometheus metrics: `reconciliation_rate`, `eval_latency_p99`, `agent_tool_failure_rate` |
| **CC8.1** Change management | Versioned ASTs | `ast_version` field in every op; `VersionPolicy` allow-list enforces approved versions |
| **A1.2** Availability under load | Backpressure | `LimitsGuard` per-tenant queue and connection limits (`server/src/limits.rs`) |
| **PI1.1** Data integrity | Cryptographic hashing | BLAKE3 snapshot hash on every state document; stored in `AcceptResponse` |

---

## 5. GDPR Compliance Controls

| Article | Requirement | GP2F Implementation |
|---------|-------------|---------------------|
| Art. 5(1)(f) – Integrity & confidentiality | Encryption at rest and in transit | AES-256-GCM at rest; TLS 1.3 in transit (§1) |
| Art. 6 – Lawfulness of processing | Consent gate | `field_is_true("/consent_given")` policy in `medical_triage_intake` workflow; consent stored as immutable op in event log |
| Art. 17 – Right to erasure | Data deletion | Compensation saga (`undo_register_patient`) triggers deletion; `workflow:cancel` RBAC permission for admin |
| Art. 20 – Data portability | Export | Event-store replay CLI (`gp2f replay`) produces full audit JSON; machine-readable |
| Art. 25 – Data protection by design | Minimal data | AI agents receive only VibeVector + ephemeral token; no raw PII |
| Art. 30 – Records of processing | Data-flow documentation | This document + data-flow diagrams (§3) |
| Art. 32 – Security of processing | Encryption + HMAC | §1; all ops cryptographically signed |
| Art. 33 – Breach notification | Incident response | 72-hour notification SLA; see §8 |
| Art. 35 – DPIA | Assessment | DPIA completed for `medical_triage_intake` workflow before pilot go-live |

### Data Sub-Processors

| Sub-processor | Data categories | Transfer basis |
|---------------|-----------------|----------------|
| Cloud provider (compute/storage) | All tenant data | Standard Contractual Clauses (SCCs) |
| KMS provider | Key metadata only | SCCs; keys never leave HSM |
| Observability (metrics/logs) | Aggregated metrics; no PII | SCCs; PII scrubbed before export |

### Retention Periods (GDPR)

See §2 Retention Policies.  For EU residents, all PHI and personal data is deleted
within **30 days** of a valid erasure request, except where a longer retention period
is required by applicable law (e.g., medical records legislation).

---

## 6. HIPAA Technical Safeguards

Reference: 45 CFR §164.312

| Safeguard | Standard | GP2F Control |
|-----------|----------|--------------|
| §164.312(a)(1) – Access control | Unique user identification | `tenant_id` + `role` per op; `instance_id` per workflow run |
| §164.312(a)(2)(i) – Unique user identification | Per-user credentials | JWT issued per authenticated session; `client_id` embedded in `op_id` |
| §164.312(a)(2)(iii) – Automatic logoff | Session TTL | JWT expires after 24 hours; WebSocket disconnects on JWT expiry |
| §164.312(a)(2)(iv) – Encryption/decryption | Data at rest | AES-256-GCM (§1) |
| §164.312(b) – Audit controls | Activity logging | `EventStore` records every PHI-touching op with full `ClientMessage` |
| §164.312(c)(1) – Integrity | Data integrity | BLAKE3 snapshot hash; HMAC-SHA256 per op |
| §164.312(c)(2) – Authentication mechanisms | Signature validation | `verify_signature` in `server/src/signature.rs` |
| §164.312(d) – Person authentication | MFA | MFA enforced at IdP for all users with `workflow:start` or `activity:execute` |
| §164.312(e)(1) – Transmission security | TLS | TLS 1.3; certificate pinning on mobile |
| §164.312(e)(2)(ii) – Encryption | In-transit | TLS 1.3 with AES-256-GCM cipher suite |

### Business Associate Agreement (BAA)

A BAA must be signed with every sub-processor that may touch PHI before the
`medical_triage_intake` pilot goes live.  Current status:

| Party | BAA status |
|-------|-----------|
| Cloud provider | ✅ Signed |
| KMS provider | ✅ Signed |
| Observability provider | ⚠️ Pending (metrics only; no PHI expected) |

---

## 7. Penetration Testing & Vulnerability Management

### Test Schedule
- **Pre-pilot**: Full black-box pen test by accredited third party.
- **Annual**: Full external pen test; scoped to all production endpoints.
- **Continuous**: Automated SAST (CodeQL) on every PR; DAST via OWASP ZAP weekly.

### Known Threat Vectors & Mitigations

| Threat | Mitigation | Status |
|--------|-----------|--------|
| Replay attack | Bloom filter + exact-window replay guard | ✅ Implemented |
| HMAC forgery | 256-bit secret; KMS-managed rotation | ✅ Implemented |
| Tenant data leakage | All queries partitioned by `tenant_id`; no cross-tenant joins | ✅ Implemented |
| AI prompt injection | Agent receives only VibeVector + ephemeral token; no raw state | ✅ Implemented |
| Denial of service | Per-tenant `LimitsGuard` (queue + connection limits) | ✅ Implemented |
| Stale snapshot exploit | BLAKE3 hash mismatch → REJECT + 3-way patch | ✅ Implemented |

---

## 8. Incident Response

### Severity Levels

| Severity | Definition | SLA (detection → notification) |
|----------|------------|--------------------------------|
| P0 – Critical | PHI breach; service down | 1 hour internal; 72 hours regulatory |
| P1 – High | Partial service degradation; suspected breach | 4 hours internal; 72 hours regulatory (if PHI) |
| P2 – Medium | Performance degradation; no breach | 24 hours internal |
| P3 – Low | Minor bug; no data impact | 72 hours internal |

### Response Runbook (P0 / P1)

1. **Detect**: Alert from Prometheus/Grafana or manual report.
2. **Contain**: Rotate affected HMAC keys via KMS runbook (§1).  Revoke affected JWT batch.
3. **Assess**: Replay event log to determine scope of affected ops and tenants.
4. **Notify**: Inform affected tenants within SLA; file regulatory notifications where required.
5. **Remediate**: Deploy patch; replay-validate all affected partitions.
6. **Post-mortem**: 5-day written post-mortem; update threat model.
