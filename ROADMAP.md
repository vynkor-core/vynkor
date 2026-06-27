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
| VULN-012 | Critical | Broadcast target `"*"` bypasses `ipc_targets` allowlist | `broadcast()` checks `PERMISSION_IPC_SEND` but never calls `check_ipc_target`; plugin with empty `ipc_targets` (deny-all) can still fan-out to every registered plugin via `"*"` | ✅ Fixed — per-recipient `check_ipc_target` added to `broadcast()` (T-18) |
| VULN-013 | High | Poisoned mutex silently disables inbound MAC verification | `protocol.rs` uses `if let Ok(...) = msg.session_key.lock()` — on mutex poison the key is never installed; connection proceeds with `session_key = None`, all subsequent frames skip MAC check | ✅ Fixed — `unwrap_or_else(|p| p.into_inner())` always installs the key (T-19) |
| VULN-014 | High | `.lock().unwrap()` in hot frame-read path → Panic DoS | `connection.rs:73`, `websocket.rs:87`, and five `store.rs` call sites unwrap a `std::sync::Mutex`; any panic while the lock is held poisons the mutex and crashes the next frame-read task | ✅ Fixed — all 7 sites use `unwrap_or_else(|p| p.into_inner())` (T-19) |
| VULN-015 | High | Broadcast propagates `FLAG_MAC_PRESENT` with no MAC bytes | Broadcast clones `flags` (including `FLAG_MAC_PRESENT`) from sender frame but sets `mac: None`; a recipient whose write-loop has no session key sends the flag without appending 32 tag bytes, corrupting the stream | ✅ Fixed — `FLAG_MAC_PRESENT` stripped from cloned frame flags in `broadcast()` (T-18) |
| VULN-016 | High | WS malformed-frame loop — no error budget, no disconnect | Bad `parse_frame` in WS handler logs a warning and continues; unlike UDS path, there is no `MAX_CONN_ERRORS` budget; attacker streams infinite invalid binary frames consuming CPU and log I/O indefinitely | 🔴 Open — tracked T-20 |
| VULN-017 | Medium | UDS socket bind-before-chmod TOCTOU | `UnixListener::bind()` creates the socket with umask-derived permissions; `set_permissions(0o600)` is called in a separate step; another local process can connect in the gap | 🔴 Open — tracked T-21 |
| VULN-018 | Medium | Dead plugin entries persist in supervisor — `is_running()` returns stale `true` | `monitor_loop` leaves `entries` intact when max_restarts is exceeded or policy is `Never`; `is_running()`, REST status endpoints, and `stop_plugin` (which calls `SIGTERM` on the PID) all see a ghost entry | 🔴 Open — tracked T-22 |
| VULN-019 | Medium | No pre-registration connection rate limit | UDS accept loop spawns a task per connection unconditionally; before registration (and MAC enforcement) an attacker opens arbitrarily many connections and floods with 1 MB frames | 🔴 Open — tracked T-20 |
| VULN-020 | Medium | MAC activation races plugin pipelining | Inbound `session_key` is set synchronously before the register-ack is delivered to the network; a pipelined plugin that sends any frame between issuing `PluginRegister` and receiving the ack will have it rejected as MAC-invalid | 🔴 Open — tracked T-21 |
| VULN-021 | Low | Watchdog resets pong timer after SIGKILL | `watchdog_loop` calls `registry.record_pong()` immediately after sending SIGKILL; a process stuck in D-state (unkillable) resets its own deadline each cycle and is never escalated | 🔴 Open — tracked T-22 |
| VULN-022 | Low | Non-UTF8 frame target silently becomes empty string | `target_as_str` returns `""` on invalid UTF-8; frame falls through to the unknown-plugin arm with a generic error and no log of the raw bytes; masks SDK misconfiguration | 🔴 Open — tracked T-21 |

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
| T-15 | Prometheus `/metrics` endpoint | Counters for messages routed, permission denials, MAC failures, error budget hits — needed for prod observability | ✅ Done — `GET /metrics` returns Prometheus text (content-type `text/plain; version=0.0.4`); counters instrumented throughout kernel (`messages_routed_total`, `ipc_send_denied_total`, `ipc_frame_errors_total`, `plugins_registered_total`, `plugin_restarts_total`, etc.); 4 unit tests + 5 integration tests added | — |
| T-16 | SQLite event store (at-least-once delivery) | Fire-and-forget event bus drops events to full channels; Phase 2 reliability target | ✅ Done — `src/events/store.rs` SQLite backend; `EventBus::with_store` persists on publish; retry worker redelivers stale pending every 5s, marks dead after 5 retries; `EventAck` handler calls `mark_delivered`; 6 unit tests + 2 integration tests added | — |
| T-17 | KernelCommand dispatch | `reload_config`, `health_check` handlers not implemented; falls to `ErrUnknown` | ✅ Done — `CommandHandler::dispatch` in `src/kernel/commands.rs`; `send_command` added to Rust SDK; 6 unit tests + 4 integration tests added | — |

---

## Sprint Goals — Phase 1.4 (Audit Hardening)

Findings from the 2026-06-27 static-analysis + threat-model audit. All map to a `VULN-NNN` above.
Tasks are ordered by severity; each targets a single subsystem to keep diffs reviewable.

| ID | Target | Closes VULNs | Status | Effort est. |
|----|--------|--------------|--------|-------------|
| T-18 | Broadcast security: enforce `ipc_targets` + strip `FLAG_MAC_PRESENT` | VULN-012 (Critical), VULN-015 (High) | ✅ Done — `broadcast()` in `src/ipc/protocol.rs`; 2 unit tests added | — |
| T-19 | Mutex poison hardening across hot paths | VULN-013 (High), VULN-014 (High) | ✅ Done — `unwrap_or_else(|p| p.into_inner())` in `protocol.rs`, `connection.rs`, `websocket.rs`, `store.rs` (×5); 1 unit test added | — |
| T-20 | WS error budget + pre-registration connection rate limit | VULN-016 (High), VULN-019 (Medium) | 🔴 Open | 1–2 days |
| T-21 | Transport-layer correctness: UDS TOCTOU, MAC activation race, target UTF-8 | VULN-017 (Medium), VULN-020 (Medium), VULN-022 (Low) | 🔴 Open | 1–2 days |
| T-22 | Supervisor lifecycle cleanup: dead entries + watchdog pong after SIGKILL | VULN-018 (Medium), VULN-021 (Low) | 🔴 Open | 1 day |

### T-18 — Broadcast Security

**Files:** `src/ipc/protocol.rs`

1. In `broadcast()`, add a per-target `check_ipc_target` gate inside the send loop (mirrors `forward()`). Closes VULN-012.
2. In the broadcast frame clone, strip `FLAG_MAC_PRESENT` from `flags` before enqueuing — the recipient's `write_loop` re-adds it with a fresh tag computed under the recipient's session key. Closes VULN-015.
3. Add unit tests: (a) plugin with empty `ipc_targets` cannot broadcast; (b) broadcast to a non-MAC recipient does not include the flag.

### T-19 — Mutex Poison Hardening

**Files:** `src/ipc/connection.rs`, `src/api/websocket.rs`, `src/events/store.rs`, `src/ipc/protocol.rs`

1. Replace all `std::sync::Mutex::lock().unwrap()` with `lock().unwrap_or_else(|p| p.into_inner())` at: `connection.rs:73`, `websocket.rs:87`, and the five `store.rs` call sites. This recovers the inner value without panicking. Closes VULN-014.
2. In `protocol.rs`, replace `if let Ok(mut cell) = msg.session_key.lock()` with an unwrap-or-recover call that **always** installs the key — silent swallow of poisoning is a security bypass. Closes VULN-013.
3. Add a unit test: poison the `SessionKeyCell` mutex externally, then verify that a subsequent registration still correctly installs the MAC key.

### T-20 — WS Error Budget + Connection Rate Limit

**Files:** `src/api/websocket.rs`, `src/ipc/server.rs`, `src/utils/config.rs`, `config.yaml`

1. Add a `ws_parse_errors: u32` counter in `handle_socket`; break the socket loop after `MAX_WS_PARSE_ERRORS` (16, matching UDS). Closes VULN-016.
2. Add `max_connections: usize` to `Config` (default `1024`). In `UdsServer::start`, track open connections with `Arc<AtomicUsize>`; reject new accepts when the limit is reached and log the event. Closes VULN-019.
3. Add unit tests: (a) WS connection is closed after 16 bad frames; (b) 1025th UDS connection is rejected when limit is 1024.

### T-21 — Transport-Layer Correctness

**Files:** `src/ipc/server.rs`, `src/ipc/protocol.rs`, `src/ipc/framing.rs`

1. **UDS TOCTOU (VULN-017):** Before `UnixListener::bind()`, set `umask(0o177)` via `libc::umask` and restore the old mask after. Keep the explicit `set_permissions(0o600)` call as defence-in-depth. Gate behind `#[cfg(unix)]`.
2. **MAC activation race (VULN-020):** Move `*cell = Some(key)` out of `protocol.rs` and into the `write_loop` — have `Outbound::EnableMac(k)` both enable outbound tagging and install the inbound key on the same `SessionKeyCell` reference. This ensures inbound MAC verification is never active before the ack has been written to the socket.
3. **target_as_str (VULN-022):** Change return type to `Option<&str>` (returns `None` on non-UTF-8). Update the router call site to log the raw hex bytes and return an error frame. Add a unit test for non-UTF-8 input.

### T-22 — Supervisor Lifecycle Cleanup

**Files:** `src/plugins/supervisor.rs`

1. **Dead entries (VULN-018):** In `monitor_loop`, call `self.entries.remove(&event.plugin_id)` in the `None` branch (policy = Never or max_restarts exceeded). Add `PluginState::Stopped` variant to the registry or a separate `stopped_ids` set if historical lookup is needed. Verify `is_running()` returns `false` for terminated plugins. Closes VULN-018.
2. **Watchdog pong (VULN-021):** Remove the `registry.record_pong(&plugin_id)` call that follows `SIGKILL`. Let the pong deadline continue running; if the process is truly dead the exit event will arrive; if it's in D-state the watchdog will SIGKILL again next interval and escalate via `warn!`. Closes VULN-021.
3. Add unit tests: (a) `is_running()` returns `false` after max_restarts exceeded; (b) watchdog does not reset pong after kill.
