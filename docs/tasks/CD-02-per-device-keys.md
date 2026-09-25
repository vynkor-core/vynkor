# CD-02 — E-01 Per-Device Keys Instead of Host `jwt_secret`

*Track D — cross-repo · P0 · Source: `CLIENT_DRIVEN_KERNEL_TASKS.md` §2 + `vynkor-client-android/docs/RFC_E01_PER_DEVICE_KEYS.md`*

> **Decision (2026-09-24):** kernel side **DONE** (confirmed). The E-01 RFC
> (status: implemented) chose a **symmetric per-device secret** for v1 and
> rejected Ed25519 — the Ed25519 / wire-1.8 challenge item is removed here and
> parked as "future v3". Plugins: nothing to do.
>
> **Status:** kernel DONE. Remaining: SDKs + client naming/support (below).

## Goal

Compromise of a phone ≠ compromise of the host. The phone holds only its own
`device_secret`, never the master `jwt_secret`.

## Done in `vynkor` (kernel)

- `src/auth/device_store.rs` — `DeviceStore`: HKDF-SHA256 from `jwt_secret` →
  AES-256-GCM at rest; `devices.json` (0600, tmp+rename, re-read per check so
  `vyn device revoke` from another process takes effect without IPC);
  `issue/get/list/set_revoked/remove/active_secret`; `Active/Revoked/Expired`.
- `src/auth/jwt.rs` — `PluginClaims`, `JwtValidator::with_audience`,
  `mint_device_token(...)` (HS256, `jti` nonce), `MIN_JWT_SECRET_BYTES=32`.
- `src/api/websocket.rs` (`WsGateway`) — validate token → `active_secret(sub)`
  (revoked/expired → 401); post-register `EnableMac` (HMAC-SHA256 frame tags).
- `src/ipc/protocol/router.rs` (`handle_kernel_message`) — on `PluginRegister`
  with `device_id`: `active_secret` → `derive_session_key(device_secret || master,
  session_nonce, plugin_id)`, `register_with_device(...)`.
- `src/cli/device.rs` — `vyn device connect|list|revoke|remove`; QR via
  `vyn-pair` (K-05).
- Wire v1.7 — `DeviceState::REVOKED`, `DeviceInfo{created, expires}`.
- Rotating `jwt_secret` intentionally invalidates all paired devices (re-pair).

## Remaining (cross-repo)

- [ ] **vynkor-sdk-cpp:** `resolve_jwt_secret` (`include/vynkor/env.hpp:21`,
      used at `include/vynkor/plugin.hpp:75,88`) — for remote devices resolve
      the per-device secret under device-secret naming (e.g. `VYN_DEVICE_SECRET`),
      not `jwt_secret`.
- [x] **vynkor-sdk (Rust):** `VynkorClient::connect_ws_device` /
      `with_device_id` send `PluginRegister.device_id`; `Plugin::run_ws` reads
      `VYN_DEVICE_ID` + `VYN_DEVICE_SECRET`. Env policy is strict: a half-set
      pair, or `VYN_JWT_SECRET` next to a device pair, is an error (vynkor-sdk
      PR #16). C++/Python should mirror the same names and policy.
- [ ] **vynkor-sdk-python:** first-class `device_secret` support for
      remote-device clients (session-key derivation + frame MAC).
- [ ] **vynkor-client-android:** rename `HostProfile.jwtSecret` → `deviceSecret`.
- `vynkor-plugins`: nothing — host plugins register as `device_id="local"`.

No proto change: all 3 proto copies (`vynkor-wire`, `vynkor-sdk-cpp`,
`vynkor-sdk-python`) stay as-is.

## Future (v3, not scheduled)

Ed25519 device keypair + challenge signature (`device_pubkey` in
`PluginRegister`, challenge in `PluginRegisterAck`, wire bump). Only if an
asymmetric trust model is needed; out of scope for v1 per the RFC.

| | Estimate |
|---|---|
| **Kernel** | DONE |
| **Remaining** | ~6–10h across SDKs + client (naming + device-secret plumbing) |
| **Depends on** | None; CD-01 builds on this |
