# CD-08 — TLS Onboarding

*Track A — `vynkor`-only (docs) · P2 · Source: `CLIENT_DRIVEN_KERNEL_TASKS.md` §8*

## Goal

`wss://` out of the box: self-signed CA + certificate fingerprint in QR v2 (client ready to pin) **or** ACME instructions. APK user must not set `tls: false`.

---

## Already Exists — Almost DONE

- `src/utils/config.rs:default_tls = true` — TLS enabled by default (D-07).
- `src/utils/tls.rs:resolve_tls_paths()` — if `tls_cert_path/key_path` not set → generates self-signed ECDSA `rcgen` in `default_tls_dir()/vyn-tls/{cert.pem,key.pem}` (per-user private dir, not `/tmp`).
- `src/utils/config.rs:effective_tls_cert_path()` — priority: explicit `tls_cert_path` → auto-gen.
- `src/cli/device.rs:206` — when `cfg.tls` reads `effective_tls_cert_path()` and puts `cert_pem` in `PairPayload v=2` (in QR). ~800B PEM, compressed `z=1` — fits QR v33.
- Client (`vynkor-client-android/rust`) — `rustls` root pinning: if `cert_pem` in payload → trusts **only** it (self-signed works without disabling verification).
- `src/api/server.rs` — `RustlsConfig::from_pem_file` → `axum_server::bind_rustls`.

## Required

- [ ] **CD-08 — docs + UX (vynkor-only, 1–2h):**
  - Update `README.md` + `config.yaml` example: `tls: true` default, where auto-gen cert lives, how client pins.
  - Add to `docs/THREAT_MODEL.md` or new `docs/TLS.md` — two production paths:
    - **A. Self-signed (LAN / demo):** auto-gen + QR pinning (already works) — zero actions.
    - **B. ACME / Let's Encrypt (public host):** instructions: `certbot` → `tls_cert_path/key_path` in `config.yaml` → restart; client then uses system roots (no `cert_pem` in QR → fallback `webpki-roots`).
  - Optional: `vyn tls status` — show `effective_tls_cert_path` + fingerprint (SHA256) for manual verification.

  - **Files:** `README.md`, `config.yaml`, `docs/TLS.md` (new), optional `src/cli/device.rs` (fingerprint in `vyn device connect` output), `src/utils/tls.rs` (helper `cert_fingerprint`).
  - **Acceptance:** `README` describes both paths; `vyn device connect` on `tls:true` prints `cert fingerprint: ab:cd:...` next to QR; `cargo test` — `test_effective_tls_cert_path_*` already green.
  - **Do not:** embed ACME client in kernel (external `certbot`), do not change wire.

## Anticipate (verified in code)

- **Auto-gen already exists:** `utils/tls.rs:resolve_tls_paths()` + `default_tls_dir()/vyn-tls` per-user private (not `/tmp`), `effective_tls_cert_path()` explicit→auto. Verified: `default_tls=true`.
- **`tls:false` habit:** new users set `tls:false` out of habit — docs must shout `tls:true` default + fingerprint in `vyn device connect` output.
- **ACME outside kernel:** do not embed ACME client; external `certbot` → `tls_cert_path/key_path`.

## Implementation Plan

1. Write `docs/TLS.md` (1 page, 2 paths).
2. In `src/cli/device.rs:connect()` after `cert_pem` — compute `SHA256(cert_pem)` → `println!("cert fingerprint: {}", hex)`.
3. Update `README.md` — "TLS" section (3 paragraphs).

## Complexity / Value / Time

| | Estimate |
|---|---|
| **Complexity** | Very low (XS) — docs + 5 lines code |
| **Value** | Medium — removes "why wss won't connect" for new users |
| **Time** | **1–2h** |
| **Risk** | None — docs/output only |
| **Dependencies** | None |

---

## Status

**95% DONE.** Mechanism works; remaining is docs + fingerprint in CLI. Can close in same PR as CD-05/CD-07/CD-09.
