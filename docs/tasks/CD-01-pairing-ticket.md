# CD-01 — Pairing Without CLI (Pairing Ticket)

*Track B — `vynkor` only · P0 · Source: `CLIENT_DRIVEN_KERNEL_TASKS.md` §1*

> **Decision (2026-09-24):** HTTP path.
> `POST /devices/pair` (kernel-admin) issues a single-use ticket;
> unauthenticated, rate-limited `POST /devices/consume {ticket}` exchanges it
> for per-device credentials. **The WS gateway is untouched** — a ticket in the
> WS subprotocol/query string leaks into proxy logs and mixes a second auth
> path into the main entry point; a JSON body over TLS, single-use, does not.
> Tickets are stored **hashed (SHA-256)** in a separate `tickets.json` (0600).
> No `device_pubkey`/Ed25519 field — the E-01 RFC chose a symmetric
> per-device secret (see CD-02). `vyn device pair` prints the link; QR
> rendering is `vyn-pair`'s job (K-05).
>
> **Status:** DONE 2026-09-25 (kernel, d677a02).

## Goal

"Just give a friend the APK": pairing without a terminal. One QR/link
`{v:2, ws, ticket}` instead of 4 fields (`host_url`, `device_id`, `jwt_token`,
`device_secret`).

## Already Exists

- `src/cli/device.rs` — `DeviceCmd::Connect`: `store.issue()` → `jwt_token` +
  `device_secret` + `cert_pem` → `PairPayload v=2` → `vynkor://pair?z=1&d=...`;
  `resolve_advertise_url` (~L413). QR is rendered by the separate `vyn-pair`
  binary (`src/bin/vyn-pair.rs`, K-05).
- `src/auth/device_store.rs` — `DeviceStore::issue/get/set_revoked/remove/active_secret`,
  AES-GCM at rest, `devices.json` (0600, tmp+rename, re-read per check).
- `src/api/websocket.rs` (`WsGateway`) + `src/ipc/protocol/router.rs`
  (`handle_kernel_message`) — per-device secret check on upgrade and on
  registration; `derive_session_key` from `device_secret` (E-01).
- `src/api/routes.rs` — `GET /devices`.

## Required

- [ ] `POST /devices/pair` (auth: `PERMISSION_KERNEL_ADMIN`) →
      `{v:2, ticket, ws, ttl_secs, cert_pem?}`; ticket = 32 CSPRNG bytes, base64url.
- [ ] `POST /devices/consume {ticket}` — no auth, rate-limited; validates
      TTL + unused, atomically marks used, `device_store.issue(...)` +
      `mint_device_token(...)` → same `PairPayload v=2` as `vyn device connect`.
      Repeat → `409`, expired → `410`, unknown → `404`/`401`.
- [ ] `TicketStore` — `tickets.json` (0600, tmp+rename) beside `devices.json`;
      stores `sha256(ticket)`, `expires_at` (unix secs), `used_at`; mutex
      around read-modify-write (single-use race); expired rows swept on prune.
- [ ] `vyn device pair [--ttl 5m] [--name ...]` → calls `POST /devices/pair`,
      prints the link (pipe to `vyn-pair` for a QR).
- [ ] Old QR `{jwt_token, device_secret}` / `vyn device connect` keep working.

**Files:** `src/auth/device_store.rs` (or new `src/auth/ticket_store.rs`),
`src/api/routes.rs`, `src/api/server.rs`, `src/cli/device.rs`. No wire change.

**Do not:** store tickets in `devices.json` or in plaintext; hand out the master
`jwt_secret`; touch the WS gateway.

## Anticipate

- **Single-use race:** two concurrent `consume` → lock the store; test it.
- **TTL across restart:** unix-secs `expires_at`, not `Instant`.
- **Brute force:** 256-bit ticket + rate limit on the unauthenticated route.

## Acceptance Checklist

- [ ] `POST /devices/pair` → ticket, TTL 300s default, single-use
- [ ] `POST /devices/consume {ticket}` → `jwt_token` + `device_secret`; device appears in `GET /devices`
- [ ] Repeat `consume` → 409, expired → 410
- [ ] `tickets.json` holds only hashes, mode 0600
- [ ] `vyn device pair` prints `{v:2, ws, ticket}` link
- [ ] Old QR still works; `vyn device revoke` revokes ticket-paired devices
- [ ] Tests: single-use, TTL expiry, concurrent consume; `clippy -D warnings`

| | Estimate |
|---|---|
| **Complexity** | M — new store + 2 HTTP handlers + CLI |
| **Value** | Very high — unblocks "give APK to a friend" |
| **Time** | 6–10h |
| **Depends on** | None |
