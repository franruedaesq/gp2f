# GP2F Wire Protocol

## Overview

Every client action is encoded as a `ClientMessage` and sent to the server over a WebSocket (or HTTP POST `/op`).  
The server responds with either `AcceptResponse` or `RejectResponse`.

---

## Message Schemas (JSON)

### ClientMessage

```json
{
  "opId": "v1.tenant42.client7.0000000001.1700000000000.nonce_b64.hmac_b64",
  "astVersion": "1.0.0",
  "action": "submit_application",
  "payload": { "applicantId": "abc123", "amount": 5000 },
  "clientSnapshotHash": "e3b0c44298fc1c149afbf4c8996fb924..."
}
```

| Field | Type | Description |
|-------|------|-------------|
| `opId` | string | Cryptographic operation ID (see below) |
| `astVersion` | string | Semver of the AST the client is using |
| `action` | string | The action name (must be in the allowed set for the current AST state) |
| `payload` | object | Action-specific data |
| `clientSnapshotHash` | string | BLAKE3 hex of the client's state before this action |

---

### AcceptResponse

```json
{
  "type": "ACCEPT",
  "opId": "v1.tenant42.client7.0000000001...",
  "serverSnapshotHash": "abc123..."
}
```

---

### RejectResponse

```json
{
  "type": "REJECT",
  "opId": "v1.tenant42.client7.0000000001...",
  "reason": "snapshot hash mismatch: client=aaa server=bbb",
  "patch": {
    "baseSnapshot": { ... },
    "localDiff": { ... },
    "serverDiff": { ... },
    "conflicts": [
      {
        "path": "/amount",
        "strategy": "LWW",
        "resolvedValue": 6000
      }
    ]
  }
}
```

---

## op_id Construction

```
op_id = base64url(
  version_byte  ||   // 1 byte:  protocol version (currently 0x01)
  tenant_id     ||   // variable length UTF-8, null-terminated
  client_id     ||   // variable length UTF-8, null-terminated
  counter_u64   ||   // 8 bytes big-endian monotonic counter
  timestamp_ms  ||   // 8 bytes big-endian Unix milliseconds
  nonce_16      ||   // 16 random bytes
  hmac_sha256       // 32 bytes HMAC-SHA256 over all preceding fields
)
```

**Properties**:
- **Globally unique**: counter + nonce prevent collisions even at high concurrency
- **Tamper-evident**: HMAC with a per-tenant secret key
- **Replay-protected**: server stores the last 10 000 op_ids per client in a bloom filter backed by persistent storage
- **Time-sortable**: timestamp_ms allows approximate ordering

---

## Conflict Resolution Strategies

| Strategy | Description |
|----------|-------------|
| `CRDT` | Field is a Yrs (Yjs) CRDT; server returns merged Yrs doc |
| `LWW` | Last-Write-Wins by server timestamp |
| `TRANSACTIONAL` | Entire op is rejected if this field conflicts |

---

## WebSocket Endpoint

```
ws://host:3000/ws
```

Messages are JSON-encoded `ClientMessage` objects. Responses are JSON-encoded `ServerMessage` (tagged union with `"type": "ACCEPT"` or `"type": "REJECT"`).

## HTTP Endpoint

```
POST /op
Content-Type: application/json

<ClientMessage>
```

Response: `ServerMessage` JSON.

---

## Failure Modes

| Condition | Server Response |
|-----------|----------------|
| Duplicate `op_id` | REJECT – "duplicate op_id" |
| Snapshot hash mismatch | REJECT – includes 3-way patch |
| Invalid AST version | REJECT – "version not allowed" |
| Malformed payload | REJECT – "invalid payload" |
| Server error | REJECT – "internal error" |
