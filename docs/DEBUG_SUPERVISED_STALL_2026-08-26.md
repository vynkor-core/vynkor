# Debug: supervised plugin stall — bisection 2026-08-26 (sherpa TTS)

Branch `debug/sherpa-supervised-stall`. Context: local sherpa synthesis
(tts/speech) under supervisor loads the model and never completes
inference; outside the kernel the same binary/env/rlimit — 80–100 ms. Full matrix —
in vynkor-plugins `plugins/speech/ROADMAP.md` and
`docs/P0_BATCH_2026-08-26.md` (§3).

## What was added in this branch

Env kill-switches on the **kernel** process (not the plugin):

| Variable | Effect |
|---|---|
| `VYN_DEBUG_SKIP_CGROUP=1` | do not create/join per-plugin cgroup v2 (falls back to RLIMIT_NPROC!) |
| `VYN_DEBUG_SKIP_RLIMITS=1` | pre_exec does not call `apply_resource_limits` at all |

Both flags also mirror child stdout/stderr into the kernel log with prefix
`[plugin-stderr]` — otherwise stderr of an unregistered/crashed plugin
is unavailable (`/plugins/{id}/logs` returns 404 before registration).

## Facts obtained via bisection

1. **NPROC fallback breaks plugins (confirmed bug).** When cgroup is skipped,
   `RLIMIT_NPROC = max_procs` (default 64) is set. NPROC counts threads of all
   uid processes; a workstation exceeds 64 immediately ⇒ any
   thread-heavy plugin fails: `pthread_create → EAGAIN` → tokio panic
   `OS can't spawn worker thread` (visible in `[plugin-stderr]`).
   **Fix:** do not apply NPROC from `max_procs` (cgroup already covers), or
   measure from current consumption, or raise default significantly.
2. **`DEFAULT_MAX_VMEM_MB = 512` too small for ONNX plugins**: model fails to
   allocate on load. tts/speech require ≥1536M.
3. **Main open mystery:** with fully skipped limits the handler
   completes instantly (`time-to-first-audio: 0 ms` in mirror), chunks
   sent, but `ActionResponse` never reaches the caller. db_* via the
   same client works. Excluded: rlimits, cgroup, duplicate action_id,
   sandbox/seccomp/cwd/pipes/ONNX threads.

## Resolved 2026-08-26 — fix/supervised-stall-and-limits (PR → develop)

Diagnostic branch closed; fixes merged into `develop` in one PR.

### Problem A — main mystery: `ActionResponse` never arrives

**Root cause:** `src/ipc/protocol.rs` — per-connection error-budget
throttling (`max_conn_errors = 16`). Every denied `tts_speak` audio-chunk
forward (`ipc_targets` does not include `sound` → `ERR_PERMISSION_DENIED`,
`errored = true`) incremented `error_counts[tts_conn]`. After 16 denied
chunks the connection was marked `throttled` and **all** subsequent messages from
the same `conn_id` were silently dropped (`continue` without `send_error`), including
the legitimate final `ActionResponse` from the provider. `db_*` does not send
peer-to-peer frames before responding, so it does not trigger throttling and
works. With fully skipped limits `time-to-first-audio: 0 ms`
confirmed the handler finished, but the response was already in the drop zone.

**Fix:** `is_throttle_exempt()` — `ActionResponse`,
`ActionResponseChunk`, `ActionRequestChunk`, `SessionClose`, `Pong`
bypass the throttling gate and reset the budget (`errored == false →
error_counts.remove`). This preserves amplification protection (VULN-007)
for error-generating messages, but does not block legitimate replies and
watchdog `Pong`.

Live verification (scratch `/tmp/opencode/vynfix`, without `VYN_DEBUG_*`,
cgroup `pids.max` active, `DEFAULT_MAX_VMEM_MB = 2048`):
`tts_speak` (sherpa piper `ru_RU-denis-medium`, "Один два три.")
→ 47 Opus packets, `duration 1.0s`, `TTFA 0 ms`, `elapsed 1633 ms` (cold)
and `0 ms` (warm); `tts_speak_stream` (3 sentences, 162 packets)
→ `TTFA stream 0 ms`, both `status=OK`. `speech` plugin (swap drop-in) —
same `tts_speak` / `tts_speak_stream` with identical TTFA.

### Problem B — `RLIMIT_NPROC` fallback

`RLIMIT_NPROC` counts threads of the whole uid (desktop > 64), so `max_procs`
64 would crash any thread-heavy plugin with `EAGAIN` → tokio panic. Fix:
`apply_resource_limits` no longer sets `RLIMIT_NPROC` at all; when
`!joined_cgroup` only `warn!` ("process accounting degraded"), cgroup
`pids.max` remains the sole limit. Unit tests in `runner.rs`.

### Problem C — `DEFAULT_MAX_VMEM_MB = 512`

ONNX Runtime reserves ~500 MiB VA → model failed to mmap. Default
raised to **2048**, `0` = unlimited (skip `setrlimit`). Updated
`src/utils/config.rs`, `plugins/{tts,speech}/config.example.yaml` and
`README.md`.

Kill-switches `VYN_DEBUG_SKIP_CGROUP` / `VYN_DEBUG_SKIP_RLIMITS` kept
as-is (mission requirement), `[plugin-stderr]` mirroring as well.
