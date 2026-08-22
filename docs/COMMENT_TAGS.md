# Comment Tags Glossary

**Date:** 2026-08-20

In-code audit tags (`T-11`, `S1`, `VULN-020`, `BUG-006`, `R9-02`, …) are opaque
to a newcomer. This file maps every tag to its issue and primary source file.
Sources: `AUDIT.md` (security/robustness findings), `ROADMAP.md` (roadmap items,
incl. MA-*), `docs/DUMB_CORE_AUDIT.md` (DC-* dumb-core findings + F1–F6 fixes).

| Tag | Meaning | Source | Files |
|-----|---------|--------|-------|
| T-01 | unverified/unchecked JWT claim; lifecycle routes need `PERMISSION_KERNEL_ADMIN` | AUDIT | `api/middleware.rs`, `api/server.rs` |
| T-03 | blocking `.send().await` on the shared router task stalls all IPC | AUDIT | `ipc/protocol.rs` |
| T-04 | per-target IPC allowlist; clamp JWT-claimed perms to `config.yaml` allowlist | AUDIT | `auth/permissions.rs`, `ipc/protocol.rs`, `kernel/orchestrator.rs` |
| T-06 | raw binary (audio) frames require `PERMISSION_AUDIO_STREAM` | AUDIT | `ipc/protocol.rs` |
| T-07 | per-plugin resource metrics (Linux) | AUDIT | `plugins/supervisor.rs` |
| T-08 | unregistered connection never routes frames | AUDIT | `ipc/protocol.rs` |
| T-09 | `max_connections` slot reservation before WS upgrade | AUDIT | `api/websocket.rs`, `utils/config.rs` |
| T-10 | prefer explicit contract over implicit behavior | AUDIT | `api/routes.rs` |
| T-11 | registry maintainer signature is detached; verify against pinned key | AUDIT | `marketplace/registry.rs`, `marketplace/installer.rs`, `utils/config.rs` |
| T-12 | minimum `jwt_secret` length (HS256 brute-force) | AUDIT | `kernel/orchestrator.rs` |
| T-19 | action permission must be declared by requester (anti-laundering); provider-only check insufficient | AUDIT | `auth/permissions.rs`, `ipc/protocol.rs`, `marketplace/installer.rs` |
| S1 | signature must bind the full registry entry, not a subset | AUDIT | `marketplace/installer.rs` |
| S2 | `data_dir` must not default to world-writable `/tmp` | AUDIT | `events/store.rs`, `utils/config.rs` |
| BUG-003 | stream_id / fragmentation must respect `MAX_PAYLOAD_SIZE` and stream cap | AUDIT | `ipc/connection.rs` |
| BUG-004 | never key on a self-decoded unverified `sub`; quota bypass | AUDIT | `api/middleware.rs`, `api/rate_limit.rs`, `api/server.rs` |
| BUG-005 | one plugin's shutdown delay must not delay SIGKILL for others | AUDIT | `plugins/supervisor.rs` |
| BUG-006 | never blindly unlink whatever sits at `socket_path` | AUDIT | `ipc/server.rs` |
| VULN-007 | cap amplification a misbehaving plugin can cause | AUDIT | `ipc/protocol.rs` |
| VULN-017 | set socket perms via explicit `set_permissions()`, not a separate `chmod()` | AUDIT | `ipc/server.rs` |
| VULN-018 | restart budget exhaustion; preserve final `restart_count` | AUDIT | `plugins/supervisor.rs` |
| VULN-020 | registration deadline / ack armed before the ack is written | AUDIT | `ipc/protocol.rs`, `ipc/connection.rs` |
| VULN-021 | do not reset pong on the deadline path (stuck D-state) | AUDIT | `plugins/supervisor.rs` |
| R5-06 | TLS required against a secured kernel | ROADMAP | `cli/plugin.rs` |
| R5-07 | legacy string-form action → fallback permission map | ROADMAP | `auth/permissions.rs` |
| R5-12 | (roadmap Phase 5 item) | ROADMAP | — |
| R6-02/03/04 | (roadmap Phase 6 items) | ROADMAP | — |
| R9-01 | per-plugin process accounting via cgroup v2 `pids.max` | ROADMAP | `plugins/runner.rs`, `plugins/supervisor.rs` |
| R9-02 | PID-namespace isolation via shim process (`vyn __shim`) | ROADMAP | `plugins/shim.rs`, `plugins/supervisor.rs`, `cli/mod.rs` |
| R9-03 | Landlock filesystem restriction (`max_fs_access`) | ROADMAP | `plugins/shim.rs`, `plugins/supervisor.rs` |
| R9-04 | seccomp syscall denylist (ptrace, bpf, mount, …) | ROADMAP | `plugins/seccomp.rs`, `plugins/shim.rs` |
| D-02 | device identity / offline-on-last-plugin-unregister | ROADMAP | `plugins/registry.rs`, `api/routes.rs` |
| D-03 | device-scoped token (`sub == device_id`); same-user IPC; wire `DeviceInfo` | ROADMAP | `auth/permissions.rs`, `ipc/protocol.rs`, `plugins/registry.rs` |
| D-04 | discovery surface (device map as serializable view) | ROADMAP | `api/routes.rs`, `kernel/commands.rs`, `cli/devices.rs`, `plugins/registry.rs` |
| D-06 | `role: client` bridge relays unresolvable targets to host | ROADMAP | `ipc/protocol.rs`, `kernel/orchestrator.rs`, `cli/mod.rs` |
| D-07 | TLS on by default; JWT audience claim + per-mint `jti` nonce | ROADMAP | `auth/jwt.rs`, `api/websocket.rs`, `kernel/orchestrator.rs`, `cli/token.rs`, `cli/plugin.rs` |
| D-08 | tool-calling surface: `action_specs` / `get_manifest` served to the AI | ROADMAP | `kernel/commands.rs`, `events/bus.rs`, `plugins/registry.rs` |
| D-10 | process-unique trace id / hop-0 trace log | ROADMAP | `ipc/protocol.rs`, `events/bus.rs` |
| D-14 | QR pairing companion for remote device agent | ROADMAP | `cli/mod.rs` |
| M-01 | evict idle keys to bound memory bloat | AUDIT | `api/server.rs`, `ipc/protocol.rs` |
| M-02 | re-sending a sequence must replace its bytes | AUDIT | `ipc/connection.rs` |
| M-03 | sandbox every spawned plugin regardless of `sandbox` flag | AUDIT | `plugins/runner.rs` |
| M-05 | (robustness item) | AUDIT | `ipc/connection.rs` |
| M-08 | reserve both slots via `entry()` to avoid double-insert | AUDIT | `plugins/registry.rs` |
| M-09 | private per-user runtime dir, never shared `/tmp` | AUDIT | `main.rs`, `marketplace/installer.rs`, `marketplace/state.rs`, `utils/config.rs` |
| PERF-1 | router kernel replies block on `.send().await` | AUDIT | `ipc/protocol.rs` |
| PERF-2 | sync SQLite + `std::sync::Mutex` on the async runtime | AUDIT | `events/store.rs`, `events/bus.rs` |
| PERF-3 | per-message `PluginEntry` clones + O(n) registry scans | AUDIT | `plugins/registry.rs` |
| PERF-4 | hot-path constant-factor costs (double CRC, sync zstd, `/proc`, WS copies) | AUDIT | `ipc/`, `events/` |
| UX-1 | body-less REST errors, 200-on-failure | AUDIT | `api/` |
| UX-2 | Debug repr leaks into public API shapes | AUDIT | `api/` |
| UX-3 | config validation gaps + silent parse-error swallowing | AUDIT | `utils/config.rs` |
| UX-4 | CLI polish | AUDIT | `cli/` |
| MA-01 | split `ipc/protocol.rs` + `marketplace/registry.rs` monoliths | ROADMAP | `ipc/protocol.rs`, `marketplace/registry.rs` |
| MA-02 | extract duplicated `target_bytes`/`build_frame` + `resolve_*_url` helpers | ROADMAP | `ipc/`, `marketplace/` |
| MA-03 | unify error system on `VeyronError` | ROADMAP | `utils/errors.rs` |
| MA-04 | replace deprecated `rand::thread_rng()` | ROADMAP | `auth/jwt.rs` |
| MA-05 | add this glossary + reduce comment duplication + consistent style | ROADMAP | `docs/COMMENT_TAGS.md` |
| MA-06 | `create_router_full` → `RouterConfig` struct | ROADMAP | `ipc/` |
| MA-07 | `Config::Default` dedup + clamp zero-invalid numerics | ROADMAP | `utils/config.rs` |
| MA-08 | `reset_for_test()` for global atomics | ROADMAP | `ipc/` |
| MA-09 | split `plugins/supervisor.rs` (933 L) | ROADMAP | `plugins/supervisor.rs` |
| MA-10 | split `kernel/orchestrator.rs` (470 L) | ROADMAP | `kernel/orchestrator.rs` |
| MA-11 | move `drain_to_log`/`proc_resource_usage` → `plugins/metrics.rs` | ROADMAP | `plugins/` |
| MA-12 | log mutex poison instead of silently swallowing | ROADMAP | `utils/` |
| MA-13 | reuse `veyron_wire` framing in WS gateway | ROADMAP | `api/websocket.rs` |
| MA-14 | `utils/logging.rs` dedup + `try_init()` | ROADMAP | `utils/logging.rs` |
| MA-15 | `veyron-wire` dead-code clippy check | ROADMAP | `../vynkor-wire` |
| MA-16 | separate tests from prod code in `registry.rs` | ROADMAP | `plugins/registry.rs` |
| MA-17 | unify `validate_slug`/`validate_plugin_id` regex | ROADMAP | `utils/` |
| MA-18 | `mint_device_token` length-checks `jwt_secret` | ROADMAP | `auth/jwt.rs` |
| MA-19 | `debug_assert!` + SAFETY comment on `unsafe` in `main.rs:391` | ROADMAP | `main.rs` |
| DC-1 | marketplace / app-store client embedded in the kernel | DUMB_CORE | `marketplace/` |
| DC-2 | device-fleet domain model (D-01…D-14) | DUMB_CORE | `plugins/registry.rs`, `bridge/` |
| DC-3 | AI tool-calling surface in protocol + kernel | DUMB_CORE | `kernel/commands.rs`, `events/bus.rs` |
| DC-4 | hardcoded action→permission policy | DUMB_CORE | `auth/permissions.rs` |
| DC-5 | events SQLite DB vs "no databases" clause | DUMB_CORE | `events/store.rs` |
| F1–F6 | dumb-core fix plan items (F1=DC-1, F2/F3=DC-2, F4=DC-3, F5=DC-4, F6=DC-5) | DUMB_CORE | see `docs/DUMB_CORE_AUDIT.md` §6 |

Tags are lowercase terse per `CLAUDE.md` — search `grep -rn "T-11" src/` to find
all sites.
