# Veyron Hardening Roadmap

Living tracker for security/robustness hardening of the Veyron kernel. Phase-1.1
feature planning lives in [`docs/ROADMAP.md`](docs/ROADMAP.md); the ecosystem
roadmap is [`docs/ROADMAP_v2.md`](docs/ROADMAP_v2.md). Design specs live under
[`docs/superpowers/specs/`](docs/superpowers/specs/).

---

## Targets

Concrete, scoped deliverables. Each maps to a known gap in the post-Phase-1.1
audit (`AUDIT.md`). Status reflects work completed to date.

| ID | Target | Rationale | Status | Effort |
|----|--------|-----------|--------|--------|
| T-01 | Move kernel-command semantics out of the IPC router | Transport layer must not hold business logic (`KernelCommand` dispatch) | ✅ Done — `src/kernel/commands.rs` | — |
| T-02 | Default-deny peer-to-peer IPC via `PERMISSION_IPC_SEND` | Any registered plugin could unicast to any other | ✅ Done — gated in `forward()` | — |
| T-03 | Permission-check broadcast (`target = "*"`) | Broadcast path was unchecked | ✅ Done — gated in `broadcast()` | — |
| T-04 | Per-plugin IPC allowlist in manifest | Coarse `PERMISSION_IPC_SEND` allows any target; needs per-target scoping | ☐ Open | 2 days |
| T-05 | Audit logging for security events | Permission denials, CRC errors, oversized frames are unlogged | ✅ Done — denials, CRC/magic/oversized logged + countered (`connection.rs`, `protocol.rs`) | — |
| T-06 | Cryptographic message integrity (MAC) | CRC-32 detects corruption, not tampering | ◐ Spec'd — [design](docs/superpowers/specs/2026-06-26-frame-mac-design.md); impl pending | 3 days |
| T-07 | Fuzz + soak harness | No fuzzing of frame/payload; no 24h soak | ☐ Open | 3 days |

---

## Known Vulnerabilities

Tracked security weaknesses. Severity is qualitative (impact × exploitability in
the intended single-host, trusted-process deployment). "Fixed" entries are
retained for the audit trail.

| ID | Severity | Vulnerability | Vector | Status / Mitigation |
|----|----------|---------------|--------|---------------------|
| VULN-001 | High | Unauthenticated peer-to-peer IPC | Any registered plugin unicasts arbitrary `Envelope` to any other plugin | ✅ Fixed — default-deny, requires `PERMISSION_IPC_SEND` (`forward()`) |
| VULN-002 | Medium | Unchecked broadcast | `target = "*"` reaches all plugins with no permission check | ✅ Fixed — default-deny, requires `PERMISSION_IPC_SEND` (`broadcast()`) |
| VULN-003 | Medium | No socket-level authentication | Any local process can connect to UDS and claim any `plugin_id` | ✅ Mitigated — kernel refuses to start without `jwt_secret` unless `allow_no_auth: true` is set deliberately (secure by default) |
| VULN-004 | Medium | First-claim plugin-ID squatting | Attacker registers `admin` before the real plugin; legit plugin then rejected | ⚠ Open — needs identity binding (JWT `sub` enforced only when auth on) |
| VULN-005 | Low | Non-cryptographic integrity | CRC-32 is forgeable by a socket-level attacker | ◐ Spec'd — see T-06 |
| VULN-006 | Low | UDS file permissions vs umask | Socket mode depends on umask if explicit chmod regressed | ✅ Mitigated — `0o600` set after bind (`server.rs`) |
| VULN-007 | Low | Error-spam amplification | Malformed/denied frames return errors without closing the connection; plugin can flood | ✅ Fixed — per-connection error budget (16) throttles further messages (`run_with_context`) |
| VULN-008 | Info | HTTP control plane unauthenticated by default | REST endpoints require JWT only when configured | ◐ Mitigated — bound to `127.0.0.1`; enable `jwt_secret` for shared hosts |
| VULN-009 | High | JSON injection via `plugin_id` | Unvalidated `plugin_id` embedded unescaped into `system.plugin_joined/left/died` payloads; a crafted id spoofs/injects fields subscribers parse | ✅ Fixed — `validate_plugin_id()` at registration: `[A-Za-z0-9._-]`, ≤32 bytes, non-reserved |
| VULN-010 | Medium | Plugin logs exposed without auth | `GET /plugins/:id/logs` was public; log output may contain secrets/PII | ✅ Fixed — moved to the auth-protected route group (`server.rs`) |

**Note:** VULN-004 (plugin-id squatting) is partly mitigated by VULN-009's id
validation (reserved names `kernel`/`*` and malformed ids can no longer be
claimed); full identity binding still requires `jwt_secret` so the token `sub` is
enforced against the declared id.

**Reporting:** new findings get the next `VULN-NNN` id, a severity, and a row
here before remediation begins. Fixed rows stay for traceability.
