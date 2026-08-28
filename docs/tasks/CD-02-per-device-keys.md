# CD-02 — E-01 Per-Device Keys Instead of Host `jwt_secret`

*Track D — cross-repo XL · P0 · Source: `CLIENT_DRIVEN_KERNEL_TASKS.md` §2 + `RFC_E01_PER_DEVICE_KEYS.md`*
*Status in `vynkor`: **DONE** (kernel). Remaining SDK/plugins/client.*

## Goal

Main security blocker for distribution: compromise of phone ≠ compromise of host. Today the phone holds the master `jwt_secret` — cannot give APK to anyone you do not fully trust.

**E-01 mechanism:** Ed25519 keypair on phone, public key travels in `ticket`, kernel verifies `challenge` signature.

---

## Already Exists in `vynkor` — DONE

Full E-01 sprint in kernel closed (2026-08-26):

- `src/auth/device_store.rs` — `DeviceStore`:
  - `HKDF-SHA256(salt="vynkor-device-store-v1", info="aes-256-gcm-key")` from `jwt_secret` → `Aes256Gcm` (AAD `"vynkor-device-secret"`, nonce 12B).
  - `STORE_FILE = "devices.json"` (0600, `tmp+rename`, re-read per check — `vyn device revoke` from another process takes effect without IPC).
  - `issue(device_id, name, ttl)` → 32 CSPRNG bytes → 64 hex `device_secret` (once), `get/list/set_revoked/remove/active_secret`, `DeviceStatus::{Active,Revoked,Expired}` (Expired — computed, not stored), `wrong master → decrypt failed` with hint `rm <data_dir>/devices.json`.
  - Tests: `issue_and_recover_round_trip`, `secrets_are_never_plaintext_on_disk`, `wrong_master_secret_cannot_decrypt`, `revoke_blocks_then_unrevoke_restores`, `expired_rows_are_rejected_like_revoked_ones`, `reissue_rotates_secret_and_clears_revocation` — all green.

- `src/auth/jwt.rs` — `PluginClaims{ sub, permissions, ipc_targets, exp, iat, aud, jti }`, `JwtValidator::with_audience(jwt_audience)` (`required_spec_claims: aud` when `jwt_audience` set, else `validate_aud=false`), `mint_device_token(secret, device_id, perms, ipc_targets, ttl, audience)` → `HS256` + 16B nonce `jti` (hex), `MIN_JWT_SECRET_BYTES=32` (MA-18, checks at boot and mint).

- `src/api/websocket.rs:WsGateway` — on upgrade `validator.validate(token)` → `store.active_secret(&claims.sub)` (revoked/expired → `401`, `ws_connections_rejected_total{reason:"device"}`), else `open_conns` gate (T-09) + `conn_id = WS_CONN_ID_BASE + counter`.
  - Post-register: `Outbound::EnableMac(key, cell)` — inbound `verify_tag` + outbound `compute_tag` (HMAC-SHA256), VULN-020 guard (EnableMac after ack).

- `src/ipc/protocol.rs:handle_kernel_message` — on `PluginRegister` with `device_id` (auth on) → `store.active_secret(&reg.device_id)` → `derive_session_key(IKM=device_secret || master_secret, session_nonce, plugin_id)` (device-scoped vs master), `register_with_device(..., DeviceMeta{device_id, user_id, os, arch, ...})`, `PluginRegisterAck{session_nonce}`.

- `src/cli/device.rs` — `vyn device connect [device] [host] [permissions] [ipc_targets] [ttl] [aud] [qr_out]` → `store.issue` **before** mint (avoid half-pair), `mint_device_token`, `PairPayload{v:2, name, host_url, device_id, jwt_token, device_secret, cert_pem?}` → `zlib+base64url` → `vynkor://pair?z=1&d=...` + QR SVG + `device row` print, `vyn device list|revoke|remove`.

- `src/api/server.rs` — `RouterConfig{device_store, jwt_validator, ws_router_tx, ...}` → `WsGateway{device_store}`, `src/kernel/orchestrator.rs` — wiring.

- `../vynkor-wire/proto v1.7` — `DeviceState::REVOKED`, `DeviceInfo{created, expires}`, `PROTOCOL_VERSION="1.7"`, `DeviceRecord` in `devices.json`.

**Rotation of `jwt_secret`** — intentionally invalidates all paired devices (re-pair), see `RFC E-01 Q3` + `DeviceStore::derive_key` comment.

---

## Remaining (cross-repo)

### `vynkor-wire` + SDKs

- [ ] **Wire — Ed25519 challenge (if RFC requires):** `PluginRegister` + `device_pubkey: bytes(32)` (Ed25519), `challenge: bytes` in `PluginRegisterAck`, kernel `verify(pubkey, challenge)`. Currently `device_secret` is symmetric (HMAC), not Ed25519. RFC describes Ed25519 pair on phone. If keeping symmetric — update RFC; if Ed25519 — wire bump `1.7→1.8` (additive).
  - **Files:** `../vynkor-wire/proto/vynkor_protocol.proto`, `../vynkor-wire/src/lib.rs`, SDKs `vynkor-sdk-{rs,cpp,python}/src`.
  - **Acceptance:** `cargo test` on all SDKs — `register_with_ed25519` green; old `device_secret` tokens still work (backward compat).

### `vynkor-plugins`

- Nothing — plugins already `device_id="local"` (host), no E-01 touch.

### `vynkor-client-android` (`rust/` + `app/`)

- `rust/src/protocol.rs` — already `frame-MAC` with `device_secret`, `PairingPayload v=2` (z=1 inflate), `cert_pem` pinning.
- Remaining — Ed25519 keygen on phone (`ring`/`ed25519-dalek` in `rust/`, `Keystore` in `app/`), send `device_pubkey` in ticket (CD-01), sign `challenge` on `WsGateway` handshake.

---

## Anticipate (verified in code)

- **Symmetric vs Ed25519:** kernel DONE on symmetric `device_secret` HMAC, RFC says Ed25519. Decide before CD-01 — otherwise ticket redesign. Symmetric already safer than master; Ed25519 — `challenge` in `PluginRegisterAck.session_nonce` + `verify(pubkey)` in `WsGateway` + wire bump 1.7→1.8.
- **Master rotation:** `jwt_secret` rotation intentionally invalidates all `devices.json` (re-pair) — document it; `rm <data_dir>/devices.json` hint already in `unseal` error.
- **SDK sync:** `vynkor-sdk-{rs,cpp,python}` + `ai`/`tts` plugins — migration to per-device secret, 6 proto copies if Ed25519.

## Link to CD-01 (ticket)

Design ticket with optional `device_pubkey` field upfront — then E-01 does not require redesigning ticket flow.

## Complexity / Value / Time

| | Estimate (kernel) | Estimate (full XL) |
|---|---|
| **Complexity** | DONE | High (L) — Ed25519 + challenge + SDK sync 3 repos |
| **Value** | **Critical** — cannot distribute APK without it | — |
| **Time** | **0h in kernel** (already DONE) | **+12–20h** cross-repo (wire 2h + SDK 6h + client 6h) if Ed25519 |
| **Risk** | None (kernel DONE) | Medium — key rotation, challenge replay |
| **Dependencies** | CD-01 ticket (for `device_pubkey` transport) | RFC E-01 |

---

## Status

**Kernel — DONE, shippable.** Full Ed25519 — separate RFC sprint after CD-01, does not block ticket (ticket with `device_secret` already safer than master).
