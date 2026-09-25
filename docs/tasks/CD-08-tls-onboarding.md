# CD-08 — TLS Onboarding

*Track A — `vynkor` only (docs + CLI output) · P2 · Source: `CLIENT_DRIVEN_KERNEL_TASKS.md` §8*

> **Decision (2026-09-24):** mechanism is done; remaining is docs + a visible
> fingerprint. Scope: `docs/TLS.md` (self-signed + pinning, ACME via external
> certbot or a reverse proxy), a README TLS section, and the **SHA-256 cert
> fingerprint** printed by `vyn device connect` / `vyn device pair` and by a
> new `vyn tls status`. No ACME client in the kernel, no wire change.
>
> **Status:** DONE 2026-09-25 (kernel, 30dc367).

## Goal

`wss://` out of the box; users never reach for `tls: false`.

## Already Exists

- `tls: true` by default (D-07); `src/utils/tls.rs:resolve_tls_paths()`
  auto-generates a self-signed pair in the per-user private dir when no
  cert/key are configured; `effective_tls_cert_path()` (explicit → auto).
- `src/cli/device.rs` puts `cert_pem` into `PairPayload v=2`; the client pins
  it (trusts only that cert). QR rendering is in `vyn-pair` (K-05) — the
  `vyn` binary only prints the link.
- `src/api/server.rs` — rustls via `axum_server::bind_rustls`.

## Required

- [ ] `docs/TLS.md` — (A) self-signed + QR pinning (LAN/demo, zero setup);
      (B) public host: certbot → `tls_cert_path`/`tls_key_path`, or terminate
      TLS at a reverse proxy; client then uses system roots (no `cert_pem`).
- [ ] README "TLS" section linking it.
- [ ] `cert_fingerprint()` helper (SHA-256 over DER, `AB:CD:…`) in `utils/tls.rs`;
      printed by `vyn device connect` / `vyn device pair` and `vyn tls status`
      (effective cert path, auto-generated vs configured, fingerprint, expiry).

**Acceptance:** docs cover both paths; fingerprint shown in pairing output and
`vyn tls status`; unit test for the fingerprint format.

| | Estimate |
|---|---|
| **Complexity** | XS |
| **Time** | 2–3h |
| **Depends on** | None |
