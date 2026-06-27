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
| T-04 | Per-plugin IPC allowlist in manifest | Coarse `PERMISSION_IPC_SEND` allows any target; needs per-target scoping | ✅ Done — `ipc_targets` field in `PluginManifest`; `check_ipc_target()` in `forward()`; JWT claims wire through | — |
| T-05 | Audit logging for security events | Permission denials, CRC errors, oversized frames are unlogged | ✅ Done — denials, CRC/magic/oversized logged + countered (`connection.rs`, `protocol.rs`) | — |
| T-06 | Cryptographic message integrity (MAC) | CRC-32 detects corruption, not tampering | ✅ Done — per-connection HMAC-SHA256 ([design](docs/superpowers/specs/2026-06-26-frame-mac-design.md), [plan](docs/superpowers/plans/2026-06-26-frame-mac.md)) | — |
| T-07 | Fuzz + soak harness | No fuzzing of frame/payload; no 24h soak | ✅ Done — `fuzz/` with 3 libFuzzer targets (`fuzz_frame_parse`, `fuzz_envelope_decode`, `fuzz_router_pipeline`) + seed corpus; soak test in `tests/integration/test_soak.rs` (parameterized via `VEYRON_SOAK_SECS`, default 5 s CI / set 86400 for 24 h) | — |

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
| VULN-004 | Medium | First-claim plugin-ID squatting | Attacker registers `admin` before the real plugin; legit plugin then rejected | ✅ Fixed — `claims.sub != plugin_id` check at registration rejects mismatched tokens (`protocol.rs`) |
| VULN-005 | Low | Non-cryptographic integrity | CRC-32 is forgeable by a socket-level attacker | ✅ Fixed — per-connection HMAC-SHA256 over header+payload, active when `jwt_secret` set; bad tag drops the connection (`frame_mac.rs`, `connection.rs`) |
| VULN-006 | Low | UDS file permissions vs umask | Socket mode depends on umask if explicit chmod regressed | ✅ Mitigated — `0o600` set after bind (`server.rs`) |
| VULN-007 | Low | Error-spam amplification | Malformed/denied frames return errors without closing the connection; plugin can flood | ✅ Fixed — per-connection error budget (16) throttles further messages (`run_with_context`) |
| VULN-008 | Info | HTTP control plane unauthenticated by default | REST endpoints require JWT only when configured | ◐ Mitigated — bound to `127.0.0.1`; enable `jwt_secret` for shared hosts |
| VULN-009 | High | JSON injection via `plugin_id` | Unvalidated `plugin_id` embedded unescaped into `system.plugin_joined/left/died` payloads; a crafted id spoofs/injects fields subscribers parse | ✅ Fixed — `validate_plugin_id()` at registration: `[A-Za-z0-9._-]`, ≤32 bytes, non-reserved |
| VULN-010 | Medium | Plugin logs exposed without auth | `GET /plugins/:id/logs` was public; log output may contain secrets/PII | ✅ Fixed — moved to the auth-protected route group (`server.rs`) |
| VULN-011 | Medium | C++/Python SDK lacks MAC | When kernel runs with `jwt_secret`, HMAC-SHA256 tag is required on every frame; C++ and Python SDKs send CRC-only frames → kernel drops connection | 🔴 Open — workaround: `allow_no_auth: true` in isolated dev environments only |

**Note:** VULN-004 fully fixed: VULN-009 (id validation) blocks reserved/malformed ids;
JWT `sub == plugin_id` check at registration closes identity spoofing when `jwt_secret` set.
Without `jwt_secret` (explicit `allow_no_auth: true`), squatting is a known accepted risk.

**Reporting:** new findings get the next `VULN-NNN` id, a severity, and a row
here before remediation begins. Fixed rows stay for traceability.

---

## Proposed Sprint Goals (Phase 1.3)

Discussion items for the next sprint. Not yet prioritised or assigned.

| ID | Goal | Rationale | Status | Effort est. |
|----|------|-----------|--------|-------------|
| T-08 | Python SDK MAC support | Closes VULN-011 for Python plugins; enables hardened kernel in Python-plugin environments | 🔄 In progress | 2–3 days |
| T-09 | C++ SDK full client + MAC | C++ SDK framing complete but no connection/registration logic or MAC; blocks C++ plugins on hardened kernel | 🔄 In progress | 3–4 days |
| T-10 | SIGTERM graceful shutdown (foreground mode) | `kill -TERM <pid>` in foreground skips graceful shutdown; only Ctrl+C triggers it | ✅ Done — `orchestrator.rs:31-36` installs `signal(SignalKind::terminate())`; `tests/integration/test_sigterm.rs` covers | — |
| T-11 | PID file flock in daemon mode | TOCTOU race on concurrent `vyn start`; foreground already uses flock, daemon does not | ✅ Done — `run_foreground()` in `main.rs:231` acquires `LockExclusiveNonblock`; daemon children inherit this path; `test_kernel::pid_flock_prevents_double_start` covers | — |
| T-12 | Fuzz targets in CI (scheduled job) | Fuzz harness exists but runs manually; nightly CI run catches regressions before releases | ✅ Done — `.github/workflows/fuzz.yml` runs weekly (Mon 03:00 UTC), matrix over all 3 targets, 60 s each; crashes uploaded as artifacts | — |
| T-13 | WebSocket frame MAC | `src/api/websocket.rs` sets `session_key = None` always; WS clients bypass MAC even on hardened kernel | ✅ Done — `parse_frame` reads 32-byte tag when `FLAG_MAC_PRESENT` set; `handle_socket` verifies inbound MAC via `session_key` cell (same path as UDS); `Outbound::EnableMac(k)` enables outbound tagging; `frame_to_bytes` appends tag; 2 integration tests added | — |
| T-14 | Config-driven plugin autoloading | Kernel reads plugin list from `config.yaml`, auto-spawns on start; enables supervised deployments | ✅ Done — `Config.plugins: Vec<PluginDef>` parsed from YAML; `PluginLoader::load_all` called by orchestrator at startup; 8 unit tests + 2 integration tests added; `plugins:` example added to `config.yaml` | — |
| T-15 | Prometheus `/metrics` endpoint | Counters for messages routed, permission denials, MAC failures, error budget hits — needed for prod observability | — | 1–2 days |
| T-16 | SQLite event store (at-least-once delivery) | Fire-and-forget event bus drops events to full channels; Phase 2 reliability target | — | 3–4 days |
| T-17 | KernelCommand dispatch | `reload_config`, `health_check` handlers not implemented; falls to `ErrUnknown` | — | 1–2 days |
