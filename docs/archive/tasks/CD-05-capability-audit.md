# CD-05 — Capability Call Audit on Device

*Track A — client + kernel regression test · P1 · Source: `CLIENT_DRIVEN_KERNEL_TASKS.md` §5*

> **Decision (2026-09-24):** the audit recipient is the **target/provider
> device** — the data owner ("who requested *my* location"). **Kernel change:
> none.** The router already stamps a non-spoofable `caller_plugin_id` on
> every forwarded `ActionRequest` (`src/ipc/protocol/router.rs` ~L788-791 —
> the inbound value is overwritten with the authenticated sender id), so the
> device sees the requester on each incoming request and keeps its own local
> audit log. No `capability_used` event, no event-bus filter.
> The kernel only adds a regression test guaranteeing the stamping for
> device targets.
>
> **Status:** kernel DONE 2026-09-25 (19ef1c9, regression test). Remaining:
> client-side audit log (`vynkor-client-android`).

## Goal

Phone shows "who requested my location, and when". Sensitive permissions stop
being a black box.

## How it works

- A phone registers as a plugin with `plugin_id = device_id` and actions
  `{device_id}.{cap}` (e.g. `{device_id}.geo`).
- A host plugin calls `{device_id}.geo`; the router forwards the
  `ActionRequest` with `caller_plugin_id` = the authenticated sender.
- The phone records `{cap, ts, caller_plugin_id}` locally on every incoming
  request and renders the audit screen from that log.

## Required

- [ ] **Kernel (regression test only):** an `ActionRequest` forwarded to a
      device-owned action carries `caller_plugin_id` = real sender, even when
      the sender set a forged value.
- [ ] **Client:** local audit log + screen (retention is the client's call).

**Do not:** store history in the kernel; publish per-call events (mic chunks
at 50/s would flood the device channel); interpret `cap`.

| | Estimate |
|---|---|
| **Complexity** | XS kernel (test) / S client |
| **Value** | High — unblocks granting geo/mic/contacts |
| **Time** | ~1h kernel, client separate |
| **Depends on** | None |
