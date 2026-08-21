# Veyron Threat Model

Date: **2026-08-15** · Phase 12 remote devices (D-01…D-10 shipped, baseline
proto v1.6). Residual updated: S2 fixed (PR #35, 2026-08-18), S3 fixed (PR #20,
2026-08-14). Consolidates §10 (auth/channel security), §19 (host reachability)
and §21 (AI tool-calling safety) of `docs/REMOTE_DEVICES_PLAN.md` into one
focused threat model, so the security posture lives in a single place.
Companion files: `AUDIT.md` (findings ledger) and
`docs/REMOTE_DEVICES_ROADMAP.md` (task status).

Scope: the kernel plus the remote-devices deployment (WS gateway, TLS, JWT,
frame-MAC, device agents/bridge, AI tool surface). Every control listed is
**SHIPPED** unless tagged **DEFERRED** with its roadmap id. This is a record of
what is actually implemented, not a design wish-list.

## Assets

| Asset | Where it lives | Whose trust it carries |
|---|---|---|
| Plugin processes | separate OS processes, sandboxed on Linux | every plugin's code and secrets |
| Wire protocol (frames, HMAC tags) | UDS + WS, `FLAG_MAC_PRESENT` | integrity of all IPC |
| JWTs + shared HS256 `jwt_secret` | kernel config + issued tokens | all identity and authorization |
| Plugin registry + devices map | kernel memory (`DashMap`) | plugin topology and permissions |
| Event bus + event store | kernel + SQLite `events.db` | at-least-once delivery state |
| Plugin configs + credentials | `plugins.d/*.yaml`, env (`VEYRON_JWT_TOKEN`/`VEYRON_JWT_SECRET`) | the operator's grants |
| AI tool-calling surface (`action_specs`) | registry-served manifest data (D-08) | what the model can reach |
| User data in flight | protobuf payloads through plugins | end-user content |

The crown jewels are the shared `jwt_secret` (compromise equals forging any
identity) and the credentials a compromised plugin holds.

## Actors

### 1. external attacker

Reaches the kernel only over the network face: the WS gateway and the HTTP
API. They can probe endpoints, attempt JWT forgery or replay, feed malformed
frames, and brute-force the shared secret offline if it ever leaks.

Denied by:

- TLS (rustls) on by default since D-07; `tls: false` is an explicit opt-out.
- JWT validated before the WS upgrade completes (token rides
  `Sec-WebSocket-Protocol`, never the URL; access-log hygiene).
- Per-device JWT with `aud`/`jti`/`exp` validation (D-07).
- Per-session HMAC-SHA256 frame MAC; one tampered byte kills the connection.
- Register-or-drop: an authenticated WS connection that does not finish
  registration within `ws_register_timeout_secs` (default 10) is dropped
  (D-07), closing the pre-MAC window.
- Bind policy: loopback unless `role: host` + auth configured; local `vyn`
  clients pin the exact served cert.
- Rate limits (`max_ws_connections` default 1024, error-budget throttling,
  rate limit keyed on the verified `sub`).
- Protocol-version major-mismatch reject (`ERR_PROTOCOL_MISMATCH`, D-03).

Residual: brute force of a weak or leaked shared secret (no per-device
revocation until D-18); low-severity dependency advisories (S4) and internal
detail in plugin-facing errors (S5) aid recon. S2 (events DB in `/tmp/veyron`)
and S3 (`crossbeam-epoch` RUSTSEC) are closed: the event DB now lives in the
per-user private runtime dir (0o700, PR #35, 2026-08-18) and `crossbeam-epoch`
is at 0.9.20 (PR #20, 2026-08-14). All open items tracked in `AUDIT.md`.

### 2. compromised plugin

A plugin whose code or process is attacker-controlled. It runs as a separate
OS process and speaks the frame protocol like any other plugin.

Denied by:

- Sandbox: private user/network/PID/mount namespaces via the `vyn __shim`,
  RLIMIT caps (`NPROC=1024`, `AS=512MiB`), Landlock `max_fs_access`, and a
  seccomp denylist (ptrace, bpf, keyrings, module loading, mount-escape,
  `open_by_handle_at`, ...). A violation kills the plugin (`SIGSYS`); a plugin
  that cannot be restricted is killed, never run unrestricted.
- Permissions default-deny + `ipc_targets` allowlist: it reaches only
  explicitly named targets.
- Same-user IPC (D-03): cross-user sends denied.
- Frame MAC: it cannot forge frames under another plugin's identity.
- Zero-parse routing: the kernel never deserializes payloads to route, and no
  plugin sees another plugin's frame contents.
- Registry/event-store writes are kernel-side; a plugin cannot corrupt them
  from the wire.

Residual: it can still exercise every permission it legitimately holds. A
compromised high-privilege plugin (e.g. one holding `PERMISSION_KERNEL_ADMIN`)
is a high-privilege attacker; sandboxing is the boundary, not a cure. C++/Python
framing still lacks fuzz coverage (M7, deferred), so a memory bug there is the
softest local target.

### 3. compromised device

A device is a full peer on the network path: its agent connects over WS(TLS),
registers `<device_id>.<cap>` mirrors, and holds a per-device JWT. "Compromised
device" means that JWT, or the agent process, is attacker-controlled.

Denied by:

- The device ceiling: a device-scoped JWT (`sub == device_id`) carries
  restricted claims that override the manifest at registration (D-03 clamp).
- `ipc_targets` allowlist: the device reaches only allowlisted targets.
- Same-user IPC (D-03): cross-user sends denied.
- Short `exp` plus `aud`/`jti` validation bounds token reuse.
- The channel itself is TLS + frame-MAC, so a compromised device cannot replay
  another device's traffic.

Residual: a stolen device token's blast radius is bounded by per-device
permissions + short exp, but revocation is per-secret, not per-device: every
device shares the one HS256 `jwt_secret`, so a leaked secret can mint for any
device until D-18 (ed25519 enrollment + per-device revocation, deferred).
Multi-user enforcement is deferred (D-23); today a co-user's plugin could
subscribe to `*` events.

### 4. malicious prompt

The prompt-injection actor on the AI plugin (Kairo). The model's output is
untrusted; an injected prompt tries to make the model call tools the user did
not authorize. The kernel stays dumb, it never interprets tool schemas; the
defense is structural and layered per §21.

Denied by:

- Least privilege: Kairo runs with a restricted JWT + `ipc_targets` allowlist
  (minted with `vyn token mint --device … --permissions … --ipc-targets …`,
  D-07), so even a fully injected prompt reaches only allowlisted low-risk
  tools.
- Confirmation gate (D-09): high-risk actions (`requires_confirmation`) split
  into `request_*` (any caller; params stored pending, nothing executes) and
  `confirm_*` (allowlisted callers only; executes the params stored at request
  time). The AI's JWT cannot call `confirm_*`, so the model cannot self-approve;
  pending requests expire (default 5 min). Enforcement keys on the
  kernel-stamped, unspoofable `caller_plugin_id`; there is no kernel gate (dumb
  core, §21.2).
- Args validated against `params_schema` before execution.
- Data vs authority: tool descriptions come from the trusted manifest; web/email
  content and tool args are untrusted data, never instructions.
- Rate/spend caps (`ACTION_QUOTA_EXCEEDED` + rate limiting).
- Audit log: every AI-initiated action persists caller + args + result,
  traceable via `message_id` (D-10).

Residual: an injected prompt can still drive any low-risk tool the model
legitimately holds, which is the point of least privilege, not a failure. The
gate is opt-in: a plugin that skips the SDK's `ConfirmationGate` has no
confirmation step, and the model plugin is responsible for honoring the tool
schema it reads.

## Controls

| Control | What it does | Status | Ref |
|---|---|---|---|
| TLS (rustls) | encrypts the network path; self-signed auto-cert in `<private dir>/veyron-tls/`; local clients pin the cert; `tls: false` explicit opt-out | **SHIPPED** | D-07 |
| JWT (HS256) | per-device mint (`vyn token mint --device`); `sub=device_id`, restricted claims, `aud`/16-byte `jti`/short `exp`; `jwt_audience` required when set; min-secret enforced at boot | **SHIPPED** | D-07, M5 |
| Frame MAC | HMAC-SHA256 per-session tag after registration; constant-time compare across SDKs; WS pre-MAC gap closed by register-or-drop | **SHIPPED** | D-07 |
| Per-device permissions | device ceiling (claims clamp the manifest at registration); `ipc_targets` allowlist; default-deny; same-user IPC | **SHIPPED** | D-02/D-03 |
| Overlay | Tailscale today: E2E-encrypted, zero kernel/protocol change, coordination server sees device metadata, not frames | **SHIPPED** (deployment layer) | §19 |
| Confirmation gate | `request_*`/`confirm_*` permission separation; confirm allowlist incl. `prefix.*` globs; pending TTL; kernel-stamped `caller_plugin_id` | **SHIPPED** | D-09 |
| Least-privilege AI | restricted JWT + `ipc_targets` allowlist; `params_schema` validation; rate/spend caps; audit log | **SHIPPED** | D-07/D-08/D-09, §21 |
| Sandbox (supporting) | namespaces, Landlock, seccomp denylist, RLIMIT caps, `pids.max` | **SHIPPED** | R9-03/R9-04 |
| Protocol versioning (supporting) | major-mismatch reject with `ERR_PROTOCOL_MISMATCH` | **SHIPPED** | D-03 |
| ed25519 enrollment + per-device revocation | replaces the shared HS256 secret for device identity | **DEFERRED** | D-18 |
| Multi-user enforcement | event-bus user-scoping, per-user AI sessions; the `user_id` seam is already in place | **DEFERRED** | D-23 |

## Residual risk / deferred

- **Shared-secret device identity.** All devices share one HS256 `jwt_secret`;
  there is no per-device revocation. A leaked secret, or a stolen token used
  inside its short `exp`, can impersonate any device. Bounded today by per-device
  claims + short exp; the real fix is D-18 (ed25519), deferred.
- **Single-user enforcement.** Same-user IPC is live (D-03), but event-bus
  user-scoping is not (D-23 deferred). With co-users on one kernel, a plugin
  could subscribe to `*` events.
- **Open audit items** (tracked in `AUDIT.md`/`ROADMAP.md`): S4
  `anyhow`/`number_prefix` advisories (Low, P3), S5 internals leak into
  plugin-facing errors (Low, P2), PERF-1/PERF-2 (Medium, P1),
  PERF-3/UX-1/UX-2 (Low-Med, P2), PERF-4/UX-3/UX-4 (Low, P3), M7 C++/Python
  fuzz harness (deferred), MA-01..MA-19 (2026-08-20 maintainability), and
  DC-1..DC-5 (dumb-core). S2 (events DB in `/tmp/veyron`) and S3
  (`crossbeam-epoch` RUSTSEC) are closed — see the external-attacker residual.
  None of these changes the controls above; they are hardening debt, not new
  exposures.
- **Operator opt-outs.** `allow_no_auth: true` and `tls: false` exist for
  explicit downgrade; running either is a deliberate, documented posture change.
