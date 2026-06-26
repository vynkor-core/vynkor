# T-06: Per-Connection Frame MAC (Cryptographic Integrity)

**Date:** 2026-06-26
**Status:** Design approved — pending implementation plan
**Roadmap:** T-06 / VULN-005 (CRC-32 is forgeable; not a MAC)

---

## 1. Goal & Threat Model

CRC-32/ISO-HDLC detects accidental corruption but is not cryptographic — anyone
who can write to a connection can compute a valid CRC for any payload. This spec
adds a cryptographic Message Authentication Code (MAC) over each frame.

**What it defends:** After a plugin completes JWT-authenticated registration, the
kernel and that plugin share a key unique to the connection. Every subsequent
frame in both directions carries an HMAC tag verified with that key. A party that
cannot derive the session key cannot forge or tamper with a frame without
detection. This binds the frame stream to the authenticated identity established
at registration (TLS-like: the handshake establishes keys, then records are
protected).

**What it does NOT do:** It is not a substitute for the JWT handshake (which
authenticates *identity*) nor for UDS `0o600` perms (which limits *who can
connect*). The MAC is the integrity/authenticity layer for frames *after*
identity is established.

**Activation:** The MAC is only active when `jwt_secret` is configured (there is
key material to derive from). Under `allow_no_auth`, frames are unchanged
(CRC-only), exactly as today.

---

## 2. Scope

- **In scope (this pass):** kernel + `sdk/rust` — the complete, tested reference
  implementation. The wire format is specified below as the cross-language
  contract.
- **Out of scope (follow-up):** `sdk/cpp`, `sdk/python`. The protocol is bytes
  over UDS + protobuf, so any SDK implementing this byte format interoperates.
  These adopt the spec later.

---

## 3. Wire Format

The frame layout is extended backward-compatibly using a previously-reserved
`flags` bit.

### 3.1 Flag bit

```
flags bit 0 (0x0001) = MAC_PRESENT
```

- `MAC_PRESENT = 0` → frame is exactly as today: `44-byte header + payload`.
  CRC-32 only. (Used for the registration handshake and for `allow_no_auth`.)
- `MAC_PRESENT = 1` → a 32-byte HMAC-SHA256 tag is appended after the payload.

### 3.2 Layout when MAC_PRESENT = 1

```
┌───────────────┬───────────────────────┬─────────────────────┐
│ 44-byte header│ payload (length bytes)│ MAC tag (32 bytes)   │
└───────────────┴───────────────────────┴─────────────────────┘
```

- The 44-byte header is unchanged (magic, flags, length, target, CRC-32).
  `length` is the payload byte count, **excluding** the tag.
- CRC-32 is retained as a cheap corruption pre-check and is computed over the
  payload exactly as today.
- The MAC tag is **not** counted in `length`. A reader that sees `MAC_PRESENT`
  reads `length` payload bytes, then 32 more tag bytes.

### 3.3 MAC computation

```
tag = HMAC-SHA256(session_key, header[0..44] || payload)
```

- The MAC covers the **entire 44-byte header** (including magic, flags, length,
  target, and the CRC field) concatenated with the payload. This authenticates
  routing (`target`), size (`length`), and content together — none can be
  altered without invalidating the tag.
- The tag itself is obviously excluded from its own input.
- Verification uses constant-time comparison (`hmac` crate's `Mac::verify_slice`).

### 3.4 Size limit interaction

`MAX_PAYLOAD_SIZE` (1 MiB) continues to bound the **payload** (the `length`
field). The 32-byte tag is additional and fixed; readers must allow `length +
32` bytes for a MAC'd frame.

---

## 4. Handshake (Key Establishment)

```
Plugin                                  Kernel
  │── connect ───────────────────────────►│
  │── PluginRegister{ jwt_token } ────────►│   (MAC_PRESENT = 0)
  │                                        │   validate JWT
  │                                        │   nonce = random 16 bytes
  │◄── PluginRegisterAck{ ..., session_nonce } ─┤   (MAC_PRESENT = 0)
  │                                        │
  │  both derive session_key               │
  │── Subscribe / messages (MAC_PRESENT=1)►│   verify tag
  │◄── events / responses (MAC_PRESENT=1) ─┤
```

- The `PluginRegister` and `PluginRegisterAck` frames are **not** MAC'd — neither
  side has the session key yet. Registration is authenticated by the JWT, which
  is self-signed.
- On a successful, JWT-validated registration, the kernel generates a random
  16-byte `session_nonce` and returns it in the `PluginRegisterAck`.
- After the ack, **every** frame in both directions sets `MAC_PRESENT = 1` and
  carries a tag.

### 4.1 Key derivation

```
session_key = HKDF-SHA256(
    ikm  = jwt_secret_bytes,
    salt = session_nonce,             // 16 random bytes from the ack
    info = "veyron-frame-mac-v1|" + plugin_id,
)                                     // 32-byte output
```

- All three inputs are known to both sides: `jwt_secret` (shared config),
  `session_nonce` (from the ack), `plugin_id` (the registered id). Note the
  kernel-internal `conn_id` is deliberately **not** used (the plugin does not
  know it).
- A fresh random nonce per registration means the session key rotates on every
  (re)connection, so a captured tag is useless against a later session.
- `plugin_id` in `info` domain-separates plugins sharing the same `jwt_secret`.

---

## 5. Enforcement Rules (kernel)

When `jwt_secret` is configured:

| Frame | Expectation |
|-------|-------------|
| `PluginRegister` (pre-registration) | `MAC_PRESENT` may be 0; JWT authenticates it |
| Any frame from a **registered** connection | MUST have `MAC_PRESENT = 1` and a valid tag |
| Registered frame with `MAC_PRESENT = 0` | Rejected — `ErrorCode::ErrMacMissing` |
| Registered frame with bad tag | **Immediate connection drop** — `ErrorCode::ErrMacInvalid`. A well-formed frame with an invalid tag is active tampering/forgery, not a transient error, so it bypasses the per-connection error budget and tears down the connection at once (error sent best-effort, then the read loop closes). Counted via metrics. |

When `allow_no_auth` (no `jwt_secret`): MAC is never expected; all frames are
CRC-only (`MAC_PRESENT = 0`). A frame that arrives with `MAC_PRESENT = 1` in this
mode is rejected (no key to verify with).

The kernel stores the derived `session_key` in the connection/registry state so
the router can verify inbound frames and tag outbound ones for that connection.

---

## 6. Components & Changes

### 6.1 Proto (`proto/veyron_protocol.proto`)
- Add `bytes session_nonce = 4;` to `PluginRegisterAck` (additive; current fields
  are 1–3). Bump the version comment.
- Add `ERR_MAC_MISSING = 6;` and `ERR_MAC_INVALID = 7;` to the `ErrorCode` enum
  (current codes are 0–5).
- Regenerate Rust bindings (build.rs). Regenerate Python `pb2` in a follow-up.

### 6.2 Crypto module (new, e.g. `src/auth/frame_mac.rs`)
- `derive_session_key(jwt_secret: &[u8], nonce: &[u8], plugin_id: &str) -> [u8; 32]`
  (HKDF-SHA256).
- `compute_tag(key, header, payload) -> [u8; 32]` and
  `verify_tag(key, header, payload, tag) -> bool` (HMAC-SHA256, constant-time).
- Pure functions, unit-testable in isolation, identical logic the Rust SDK reuses.

### 6.3 Framing (`src/ipc/framing.rs`)
- `Frame` gains an optional `mac: Option<[u8; 32]>` (or the tag is handled at
  read/write boundaries). Reading: if `flags & MAC_PRESENT`, read 32 trailing
  bytes after the payload. Writing: if a tag is present, append it.
- Framing stays key-agnostic: it serializes/deserializes the tag bytes. Tag
  **computation/verification** lives in the router/connection layer that holds
  the session key. (Keeps framing a pure transport concern.)

### 6.4 Connection / router (`src/ipc/connection.rs`, `src/ipc/protocol.rs`)
- On successful registration, derive and store the connection's `session_key`.
- Inbound: for registered connections (auth on), verify the tag before
  dispatch; reject per Section 5.
- Outbound: tag frames sent to a registered connection with `MAC_PRESENT = 1`.
- Reuse the existing per-connection error budget for MAC failures.

### 6.5 Rust SDK (`sdk/rust`)
- After receiving the ack, derive the session key (same `frame_mac` logic).
- Tag all outbound frames and verify all inbound frames post-registration.

### 6.6 Cargo
- Add `hmac`, `sha2`, `hkdf` (RustCrypto) to workspace deps (kernel + sdk/rust).

---

## 7. Backward Compatibility

- A kernel with `allow_no_auth` and a no-MAC SDK: unchanged (flag 0 throughout).
- A kernel with `jwt_secret` requires MAC from registered connections, so an SDK
  that does not implement the MAC cannot complete a session against a secured
  kernel — this is intended (security cannot be optional once auth is on).
- The flag-bit approach means a single SDK build can talk to both secured and
  `allow_no_auth` kernels by following the ack: derive a key and switch on
  MAC only when the kernel ran with auth. (The SDK learns the mode by whether a
  `session_nonce` is present in the ack.)

---

## 8. Testing

**Unit — `frame_mac`:**
- HKDF determinism: same inputs → same key; different nonce/plugin_id → different key.
- HMAC round-trip: `verify_tag` accepts a genuine tag.
- Tamper detection: flipping a payload byte, a header byte (target/length), or a
  tag byte → `verify_tag` rejects.

**Unit — framing:**
- Round-trip a `MAC_PRESENT` frame: tag bytes preserved; `length` excludes tag.
- `MAC_PRESENT = 0` frame still round-trips unchanged.

**Unit — router:**
- Registered connection sending an unMAC'd frame (auth on) → `ErrMacMissing`.
- Registered connection sending a bad tag → `ErrMacInvalid`, connection dropped
  immediately (does not consume/await the error budget).
- Valid MAC'd frame routes normally.

**Integration — kernel ↔ Rust SDK:**
- With `jwt_secret`: full handshake; both derive matching keys; a unicast/ping
  round-trips with valid tags both directions.
- With `allow_no_auth`: no MAC, existing behavior.

---

## 9. Risks & Notes

- **Performance:** HMAC-SHA256 per frame adds CPU. SHA-256 is fast (hardware on
  most targets); the router is already the single-threaded bottleneck, so this is
  a real but bounded cost. A benchmark should measure it (ROADMAP §10
  "Performance benchmarks"); not blocking.
- **Key in memory:** `session_key` lives in connection state for the connection's
  lifetime. Acceptable; it is derived, per-connection, and rotates on reconnect.
- **jwt_secret as IKM:** reuses the existing shared secret as HKDF input material,
  domain-separated by the `info` string. No new secret to distribute.
- **Replay within a session:** the MAC authenticates integrity/origin, not
  freshness — it does not prevent replay of a captured frame within the same
  session. Out of scope here; message-level `message_id`/sequence would address
  replay separately if needed.

---

## 10. Out of Scope

- C++ / Python SDK implementations (follow the wire spec later).
- Replay protection / per-frame sequence numbers.
- Asymmetric signatures or per-plugin public keys.
- Rekeying mid-session.
