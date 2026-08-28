# CD-01 — Pairing Without CLI (Pairing Ticket)

*Track B — `vynkor` + `vynkor-wire` · P0 · Source: `CLIENT_DRIVEN_KERNEL_TASKS.md` §1*

## Goal

"Just give a friend the APK": pairing without a terminal. Single QR `{v:2, ws, ticket}` instead of 4 fields (`host_url`, `device_id`, `jwt_token`, `device_secret`).

**Mechanism:** `vyn device pair [--ttl 5m]` or `POST /devices/pair` → single-use `ticket` (TTL ~5m). On first WS connect with `ticket`, kernel mints device JWT + `device_secret` itself and registers the device. Web/CLI — device list with revocation.

---

## Already Exists

- `src/cli/device.rs:DeviceCmd::Connect` — already does `store.issue()` → `jwt_token`+`device_secret`+`cert_pem` → `PairPayload v=2` → `vynkor://pair?z=1&d=...` (zlib+base64url QR). Client parses (`PairingPayload.parseWithReason`, R-02).
- `src/auth/device_store.rs` — `DeviceStore::issue/get/set_revoked/remove/active_secret`, AES-GCM at-rest, HKDF from `jwt_secret`, file `devices.json` (0600, tmp+rename, re-read per check).
- `src/api/websocket.rs:WsGateway` + `src/ipc/protocol.rs:handle_kernel_message` — checks `active_secret(device_id)` on upgrade and on registration; `derive_session_key` from `device_secret` (E-01).
- `src/api/routes.rs` — `GET /devices` (D-04), `GET /plugins`.
- Client already ready for `ticket` (plan notes).

## Required

- [ ] **CD-01 — ticket mechanism (vynkor+wire, 6–10h):**
  - HTTP: `POST /devices/pair` (auth `PERMISSION_KERNEL_ADMIN`, like `POST /plugins/{id}/start`) → `{ticket: "base64url 32B", ws: "wss://host:port/ws", v:2, ttl_secs: 300, cert_pem?: "..."}`. Generation: 32 CSPRNG bytes → base64url.
  - Storage: new table `tickets` in `DeviceStore` (or separate `TicketStore` beside `devices.json` → `tickets.json`): `{ticket, created_at, expires_at, used_at: Option, created_by: device_id?, device_pubkey?: Ed25519 for E-01}`. Single-use: `consume_ticket(ticket) -> Ok(())` atomically marks `used_at`, repeat → `409 Conflict`.
  - WS handshake: `Sec-WebSocket-Protocol: vynkor, ticket:<base64url>` (or query `?ticket=` — decide; see open question 1) → `device_store.consume_ticket()` → `store.issue(device_id=random_or_ticket_scoped, name, ttl=86400)` + `mint_device_token(...)` → return `device_jwt` and `device_secret` in first WS frame (or directly `PluginRegisterAck.session_nonce` — kernel already issues nonce). Alternative: ticket exchange **before** WS — `POST /devices/consume {ticket}` → `{device_id, jwt_token, device_secret, ws_url, cert_pem}` — simpler, does not touch WS gateway.
  - QR v2: new payload variant `{v:2, ws, ticket, cert_pem?}` — `ticket` field instead of `jwt_token+device_secret`. Old `{v:2, ws, jwt_token, device_secret}` stays valid (backward compat).
  - CLI: `vyn device pair [--ttl 5m] [--name "friend phone"]` — thin client to `POST /devices/pair`, prints QR/link like `vyn device connect`.
  - Web/CLI: `GET /devices` already exists; add `GET /tickets` (optional) and `vyn device tickets list`.

  - **Files:** `src/api/routes.rs` (new handler `pair_device`), `src/api/server.rs` (route `POST /devices/pair`, `POST /devices/consume` if HTTP path chosen), `src/auth/device_store.rs` (structs `TicketRecord` + `issue_ticket/consume_ticket/sweep_expired_tickets`), `src/api/websocket.rs` (branch `ticket` in `extract_ws_token`), `src/cli/device.rs` (`DeviceCmd::Pair`), `../vynkor-wire/proto/vynkor_protocol.proto` — only if ticket enters `PluginRegister` (optional; can be pure HTTP).
  - **Acceptance:** `POST /devices/pair` → `ticket` 43 chars; `POST /devices/consume {ticket}` (or WS with ticket) → `jwt_token`+`device_secret` + `GET /devices` shows `online`; repeat `consume` → `409`; expired (5m) → `410`; old QR (`jwt_token+secret`) still works; test `test_ticket_single_use_and_ttl` green.
  - **Do not:** store ticket in `devices.json` (separate file), do not issue master `jwt_secret` (only per-device), do not break `vyn device connect` (keep for LAN admins).

## Implementation Plan

### Step 1 — HTTP before WS (recommended, simpler)

1. `src/auth/device_store.rs` — `TicketStore` (file `tickets.json`, same HKDF+0600 or plaintext — tickets are single-use, not long-term secrets; plaintext JSON is fine). Methods `issue_ticket(ttl) -> Ticket`, `consume_ticket(ticket) -> Option<Ticket>`, `sweep_expired()`.
2. `src/api/routes.rs` — `pair_device(State) -> Json<TicketView>` (generates ticket, writes, returns `ws_url` from `config` — `resolve_advertise_url` already in `cli/device.rs`, move to `utils/url.rs`).
3. `src/api/routes.rs` — `consume_ticket(Json{ ticket }) -> Json<PairPayload>` (validates TTL+unused, atomically `used_at=now`, calls `device_store.issue(device_id=random_device_id(), name="ticket-pair", ttl=86400)` + `mint_device_token` → returns same `PairPayload v=2` as `vyn device connect` but already with JWT).
4. `src/api/server.rs` — `POST /devices/pair`, `POST /devices/consume` (both behind `require_kernel_admin`? `consume` — no auth, it's public ticket exchange).
5. `src/cli/device.rs` — `DeviceCmd::Pair{ ttl, name, host }` → `POST /devices/pair` → QR.

### Step 2 — WS path (alternative, if open question 1 chooses WS)

- `src/api/websocket.rs:extract_ws_token` — if `token.starts_with("ticket:")` → `ticket = token[7..]` → `store.consume_ticket(ticket)` → mint+issue → continue as normal `validate`.

### Proto

- Not required. QR version `v` already exists (`PairPayload.v=2`). If ticket enters `PluginRegister.jwt_token` as `ticket:<b64>` — wire unchanged. If a separate `PluginRegister.ticket` is desired — bump `v1.7→1.8` (additive, one commit).

## Open Question 1 — Which to Choose?

- **HTTP `POST /devices/consume` before WS** — easier to test (`curl`), does not touch WS gateway, explicit JSON response with `jwt_token`.
- **WS `ticket:<b64>` in `Sec-WebSocket-Protocol`** — one fewer round-trip, but mixes HTTP and WS auth.

Recommendation: **HTTP** for MVP (1 PR), WS variant — later if 1-RTT is needed.

## Link to E-01

Design ticket with `device_pubkey` (Ed25519, 32B hex) upfront — optional field `ticket.device_pubkey`. When E-01 goes live, kernel will verify `challenge` signature instead of checking `jwt_secret`. Field is ignored for now.

## Anticipate (verified in code)

- **Race single-use:** two `consume` at once — `read→write` without lock yields duplicate JWT. Fix with `Mutex` in `TicketStore` + `sweep_expired` in `prune_tick` (like `error_counts` 60s in `protocol.rs`). Verified: `DeviceStore` already re-reads per check but without lock.
- **TTL vs restart:** store `expires_at` as unix secs, not `Instant` — kernel restart does not reset timer. File `tickets.json` separate from `devices.json`.
- **Compatibility:** old QR `{jwt_token,device_secret}` must stay valid — branch `if ticket:` else `validate(jwt)`. Verified: `pair` currently only old format.
- **Choice HTTP vs WS:** HTTP `POST /devices/consume` easier to `curl` and does not touch `WsGateway`; WS `ticket:` saves 1 RTT. Start with HTTP.
- **Security:** do not issue master `jwt_secret`, only per-device; `tickets.json` 0600 if plaintext.

## Complexity / Value / Time

| | Estimate |
|---|---|
| **Complexity** | Medium (M) — new store + 2 HTTP handlers + QR branch, no crypto |
| **Value** | **Very high** — unblocks "give APK to a friend" (main P0) |
| **Time** | **6–10h** (2h store, 3h HTTP, 2h CLI/QR, 2h tests) |
| **Risk** | Medium — single-use race (needs atomic `consume`), solved with `TicketStore` lock |
| **Dependencies** | Only `vynkor` + optional `wire` (QR `v`); client already ready — zero work |
| **Depends on** | None; but design with `device_pubkey` for E-01 |

---

## Acceptance Checklist

- [ ] `POST /devices/pair` → ticket, TTL 300s, single-use
- [ ] `POST /devices/consume {ticket}` → `jwt_token`+`device_secret` (or WS ticket handshake)
- [ ] Repeat `consume` → 409, expired → 410
- [ ] `vyn device pair` prints QR v2 `{v:2, ws, ticket}`
- [ ] Old QR (`jwt_token+secret`) still works
- [ ] `GET /devices` shows new device, `vyn device revoke` revokes
- [ ] `cargo test` — `test_ticket_single_use`, `test_ticket_ttl_expiry`, `clippy -D warnings`
