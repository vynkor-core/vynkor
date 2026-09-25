# CD-06 — Version Negotiation in Handshake

*Track B — `vynkor` only (wire fields deferred) · P2 · Source: `CLIENT_DRIVEN_KERNEL_TASKS.md` §6*

> **Decision (2026-09-24):** minimum supported = **1.5** (v1.5 renumbered the
> zero-value enums — anything older is wire-incompatible). The kernel compares
> **major.minor** and rejects anything outside `[1.5, PROTOCOL_VERSION]` with
> an explicit error naming the supported range. Empty `protocol_version`
> stays accepted. **No proto change now** — `PluginRegisterAck`
> `negotiated_version` / `min_supported_version` (free tags 5, 6) are deferred
> to the next wire release.
>
> **Status:** decided, in progress (kernel branch).

## Goal

Predictable upgrade path: a client sending an unsupported `protocol_version`
gets a clear rejection that says what the kernel supports.

## Already Exists

- `vynkor_wire::PROTOCOL_VERSION = "1.7"`.
- `src/ipc/protocol/router.rs` ~L358-373 (D-03): rejects on **major** mismatch
  only; minor/patch accepted; empty version accepted.

**Bug in the original spec:** it proposed `plugin_major < min_major ||
plugin_major > wire_major` — a major-only compare cannot express a range like
1.5–1.7 (or later 1.6–1.7). The compare must be on `(major, minor)`.

## Required (kernel)

- [ ] Kernel-local `MIN_SUPPORTED_PROTOCOL_VERSION = "1.5"` (move to
      `vynkor-wire` with the next wire release).
- [ ] Parse `major.minor` (ignore patch); reject if `< 1.5` or `> PROTOCOL_VERSION`
      with `reject_reason` like `"kernel supports protocol 1.5–1.7, got 1.4"`;
      unparsable → reject with the same message.
- [ ] Tests (`tests/unit/test_router.rs`): `""`, `1.5`, `1.7`, `1.7.3` accepted;
      `1.4`, `1.8`, `2.0`, garbage rejected with the range in the reason.

## Deferred (next wire release)

- `PluginRegisterAck.negotiated_version = 5`, `min_supported_version = 6`;
  sync the 3 proto copies (`vynkor-wire`, `vynkor-sdk-cpp`, `vynkor-sdk-python`)
  per the T-17 drift check.

| | Estimate |
|---|---|
| **Complexity** | S |
| **Time** | 2–3h kernel |
| **Depends on** | None |
