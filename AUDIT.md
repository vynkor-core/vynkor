# Veyron Kernel — Architectural Audit

**Date:** 2026-07-01
**Auditor:** Lead Systems Architect
**Branch:** `develop` · Commit: `234183a` (Phase 3)
**Method:** Static read of all 42 Rust source files + SDK re-exports. Tests **not** re-run this pass; test-status figures below are carried from the previous audit and are stale.

---

## Executive Summary

The kernel's architecture (dumb byte-router, UDS-only IPC, per-process isolation, default-deny permissions) remains sound. **However, the Phase 3 payload-compression feature (`FLAG_COMPRESSED`, added after the previous audit) introduced two protocol-level regressions that silently corrupt any frame ≥ 64 KiB.** These are not covered by tests and invalidate the previous "all clear" verdict.

The prior AUDIT.md scored this 93/100 with "all 22 VULNs closed." That grade predates the compression code and is no longer accurate.

| Dimension | Prev | Now | Note |
|-----------|------|-----|------|
| Core architecture | 10/10 | 9/10 | Sound; router now mishandles compressed-frame flags |
| Binary framing | 10/10 | **4/10** | BUG-001/002: decompress-in-place leaves flags/length inconsistent |
| Frame MAC | 10/10 | **4/10** | BUG-001: MAC verification fails for every compressed frame |
| Fragmentation / DoS | 9/10 | **6/10** | BUG-003: reassembly has no size or stream-count cap |
| HTTP rate limiting | 10/10 | 6/10 | BUG-004: keyed on unauthenticated, attacker-chosen `sub` |
| Lifecycle / shutdown | 9/10 | 8/10 | BUG-005: grace arg is dead; slowest plugin gates all |
| **Overall** | 93/100 | **~72/100** | Fix BUG-001/002/003 before any promotion |

---

## Bugs Found (this pass)

### BUG-001 — Compressed frames fail MAC verification (Critical)

**Files:** `src/ipc/connection.rs:275-280` (write), `src/ipc/framing.rs:135-166,230-235` (compress/read), `src/api/websocket.rs:94` (WS verify)

On a secured connection the write loop computes the HMAC tag **before** compression:

```
connection.rs write_loop:
  frame.flags |= FLAG_MAC_PRESENT;
  header = serialize_header(&frame);              // length = UNCOMPRESSED, no COMPRESSED bit
  frame.mac = Some(compute_tag(k, &header, &frame.payload));  // payload UNCOMPRESSED
  write_frame_raw(&mut w, &frame).await;          // <-- compresses here, rewrites length + sets COMPRESSED, does NOT re-tag
```

`write_frame_raw` then compresses the payload, sets `FLAG_COMPRESSED`, and rewrites the `length` field to the *compressed* size — but leaves `frame.mac` as the tag computed over the uncompressed header/payload.

On the receiver, `read_frame_body` verifies CRC, decompresses the payload, but **leaves `flags` = COMPRESSED and `length` = compressed size** (framing.rs:230-235). The verifier then does:

```
header = serialize_header(&frame);   // length = COMPRESSED size, COMPRESSED bit SET
verify_tag(&k, &header, &frame.payload /* DECOMPRESSED */, tag)
```

Sender tagged `(header{len=U, no-COMPRESSED} ‖ payload_uncompressed)`; receiver verifies `(header{len=C, COMPRESSED} ‖ payload_uncompressed)`. **The headers never match → `verify_tag` always fails → the connection is dropped** (`connection.rs:140-147`).

**Trigger:** any kernel→plugin (or plugin→plugin forwarded) frame ≥ `COMPRESS_THRESHOLD` (64 KiB) on an auth-enabled deployment. The Rust SDK re-exports the same `write_frame_raw`, so plugin→kernel large frames break identically. Reachable via events, forwarded IPC payloads, and any large action response.

**Why undetected:** the compression tests (`tests/unit/test_framing.rs:284`) never enable MAC, and the MAC tests never exceed the compression threshold. No test exercises the intersection.

**Fix:** compute the MAC over the *wire* bytes. Either (a) compress first, then tag the final on-wire header+payload, or (b) after decompression on read, clear `FLAG_COMPRESSED` and reset `length` to the decompressed size *before* reconstructing the header for verification — and correspondingly tag over the pre-compression form on both ends. Pick one canonical "what the MAC covers" and apply it symmetrically.

---

### BUG-002 — Forwarding/broadcasting a compressed frame corrupts it (Critical)

**Files:** `src/ipc/framing.rs:230-235`, `src/ipc/protocol.rs:492-515` (forward), `src/ipc/protocol.rs:556-587` (broadcast)

Same root cause as BUG-001, independent of MAC. After `read_frame_body` decompresses, the returned `Frame` has `payload = plaintext` but `flags` still carries `FLAG_COMPRESSED`. `test_framing.rs:302` asserts this on purpose (`FLAG_COMPRESSED must be set on received frame`), so it is intended behavior — and it is wrong.

The router forwards this frame verbatim to the target's write loop. `write_frame_raw` sees `flags & FLAG_COMPRESSED != 0` (framing.rs:136) and therefore **skips re-compression**, writing the *plaintext* payload on the wire while `FLAG_COMPRESSED` is still set and `length`/`crc` are recomputed over the plaintext. The receiving peer sees `FLAG_COMPRESSED`, calls `zstd::bulk::decompress` on plaintext bytes → decompression error → `VeyronError::Internal` → frame read fails → **connection dropped** (framing.rs:231-232, connection.rs:255).

**Trigger:** any plugin-to-plugin unicast or broadcast of a payload ≥ 64 KiB.

**Fix:** clear `FLAG_COMPRESSED` and reset `length` to the decompressed length in `read_frame_body` once the payload has been decompressed, so the in-memory `Frame` invariant is "payload is always plaintext; flags describe the plaintext." Update the assertion in `test_framing.rs:302` to match (it currently encodes the bug).

---

### BUG-003 — Fragment reassembly has no size or stream-count cap (High, DoS)

**File:** `src/ipc/connection.rs:180-205` (`run`), `:47-62` (prune)

`ReassemblyBuf` accumulates fragments with only a 30 s idle timeout. There is **no** cap on:

- **Reassembled payload size.** `total` is a `u16` (up to 65 535) and each fragment frame may carry up to `MAX_PAYLOAD_SIZE` (1 MiB) of chunk data. A completed reassembly therefore has no 1 MiB ceiling — it bypasses the frame size limit entirely, and `length: payload.len() as u32` (connection.rs:200) **truncates** if the total exceeds 4 GiB, producing a length field that disagrees with the payload.
- **Concurrent streams.** `reassembly_map` is keyed by attacker-chosen `stream_id` (`u32`) with no entry cap. A peer can open unbounded incomplete streams and buffer memory up to the 30 s prune window (≈ line-rate × 30 s) with the kernel holding all of it.

The previous audit marked "Fragment-based memory exhaustion ✅ PASS — pruned after 30 s." 30 s of buffering with no cap is not a mitigation.

**Fix:** cap the number of concurrent reassembly streams per connection; cap cumulative buffered bytes per stream and per connection; reject reassembled payloads that would exceed `MAX_PAYLOAD_SIZE`; validate that `frag_hdr.total` is consistent across a stream.

---

### BUG-004 — HTTP rate limit keyed on unauthenticated `sub` (Medium)

**File:** `src/api/rate_limit.rs:48-63`

`extract_sub` decodes the bearer JWT with `insecure_disable_signature_validation()` and `validate_exp = false`, then rate-limits on the resulting `sub`. Because the signature is never checked at this layer, `sub` is fully attacker-controlled:

- **Bypass:** an attacker sends a fresh forged token with a random `sub` per request → each request lands in a distinct bucket → never rate-limited. The subsequent auth middleware rejects the request, but the rate limiter provides no protection against an unauthenticated request flood — which is exactly what it is supposed to blunt.
- **Targeted exhaustion:** an attacker sets `sub` to a legitimate plugin's id and burns that victim's bucket, causing 429s for the real token.

**Fix:** rate-limit unauthenticated traffic by source (connection/IP) before auth, and apply the per-`sub` limit only *after* signature verification (i.e. inside/after auth middleware using validated claims).

---

### BUG-005 — Shutdown grace: passed argument is dead; slowest plugin gates all (Low)

**Files:** `src/plugins/supervisor.rs:269-297`, `src/kernel/orchestrator.rs:281`

`graceful_shutdown(&self, _default_grace_seconds: u32)` ignores its argument (note the leading underscore) and instead uses `max()` of every plugin's `grace_seconds`. Orchestrator passes a hardcoded `GRACE_SECONDS = 5` that has no effect. Consequences: a single plugin configured with a large `grace_seconds` delays SIGKILL for *all* plugins by that amount; and the "default 5 s" the caller thinks it is passing is never honored (the real default only applies when *every* plugin has `grace_seconds == 0`).

**Fix:** either honor the passed default as the floor, or SIGKILL each plugin on its own per-plugin deadline rather than one shared max.

---

### BUG-006 — Predictable world-directory socket fallback (Low)

**File:** `src/utils/config.rs:89-95`, `src/ipc/server.rs:29`

When `XDG_RUNTIME_DIR` is unset, the socket falls back to `/tmp/veyron.sock` (predictable, world-writable dir). `UdsServer::start` unconditionally `remove_file`s the path before bind. The 0o177-umask + `set_permissions(0o600)` protects the socket itself, but the predictable path in a shared `/tmp` invites pre-creation / squatting nuisance. Prefer failing closed or using a per-user dir when `XDG_RUNTIME_DIR` is absent.

---

## What still checks out

These prior-audit claims were re-verified against the code and hold:

- **UDS-only IPC**, loopback-only HTTP bind (`api/server.rs:152`), 0o600 socket via pre-bind umask (`ipc/server.rs:37-51`).
- **Default-deny peer IPC**: `check_ipc_send` + `check_ipc_target` enforced on both `forward` and `broadcast` (`protocol.rs:449-474,527-565`); empty `ipc_targets` = deny-all.
- **JWT `sub == plugin_id`** at registration (`protocol.rs:222`); token perms override manifest (`:231-232`).
- **Registration integrity**: one plugin per conn_id, reserved-id/charset/length validation (`registry.rs:47-59,128-152`).
- **Zip-slip**: `..`/root/prefix/absolute rejected, symlinks skipped, post-create canonical containment check (`installer.rs:233-277`).
- **SHA-256 archive gate** and atomic install with `.bak` rollback (`installer.rs:104-171`).
- **Broadcast strips `FLAG_MAC_PRESENT`** so each recipient re-tags with its own key (`protocol.rs:566-576`) — correct in principle, though it inherits BUG-002 for compressed payloads.
- **Mutex poison recovery** on hot paths (`connection.rs:127,269`; `events/store.rs`).
- **Watchdog** does not reset pong after SIGKILL (`supervisor.rs:410-414`).

---

## Recommended order of work

1. **BUG-001 + BUG-002** — fix together; both are the "flags/length not normalized after decompress" invariant break. Add a test that sends a ≥64 KiB payload over a MAC-enabled connection and one that forwards a ≥64 KiB payload plugin→plugin. Correct `test_framing.rs:302`.
2. **BUG-003** — add reassembly caps + reject over-size reassembled frames.
3. **BUG-004** — move per-token rate limiting behind signature verification.
4. **BUG-005 / BUG-006** — cleanups.
5. Re-run `cargo test --all --all-features` and refresh the (currently stale) test baseline before re-scoring.

---

## Superseded prior claims

The previous AUDIT.md (`bead6b5`) stated framing/MAC = 10/10 and "all VULN-001–022 resolved." That assessment predates the Phase 3 compression code (`234183a`) and is retracted for the framing/MAC/fragmentation dimensions per BUG-001/002/003 above. VULN-005 (per-frame MAC) is effectively regressed for all compressed frames.
