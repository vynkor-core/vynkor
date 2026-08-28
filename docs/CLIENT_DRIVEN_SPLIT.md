# Client-Driven Kernel Tasks — Split by Repository

*Source: `../vynkor-client-android/docs/CLIENT_DRIVEN_KERNEL_TASKS.md` (2026-08-26) · kernel audit `vynkor` 2026-08-28*
*Branch: `feat/client-driven-kernel-tasks` · Base protocol `v1.7` (`vynkor-wire 0.0.2`)*

This document splits the 10 client-wave tasks by repository so each can be built in isolation.

---

## Dependency Matrix

| # | Task | `vynkor` | `vynkor-wire` | `vynkor-plugins` (`ai`) | SDK / client | Kernel status |
|---|---|---|---|---|---|---|
| **0** | Models & agents announced by host plugin | `registry.rs` + `routes.rs`/`commands.rs` | **yes** (new `PluginManifest` fields or `ModelInfo`/`AgentInfo`) | **ai** — announces | client ready | Partial (generic `action_specs`) |
| **1** | Pairing ticket (P0) | **yes** (`routes.rs`, `websocket.rs`, `device_store.rs`, `cli/device.rs`) | **yes** (QR `v` field, optional `ticket` vs `jwt_token+secret`) | — | client ready | **No** (only `vyn device connect`) |
| **2** | E-01 per-device keys | **DONE** | **DONE** (`v1.7`) | migrate `ai`/`tts`/others to per-device secret | Ed25519 in SDK | **DONE in kernel** |
| **3** | AI response streaming `{id}.chat` | **generic DONE** (chunks) | **yes** (`CHAT_DELTA` / `chat.cancel` vs reuse `ActionResponseChunk`) | **ai** — token stream + cancel | 1h client adapt | Generic DONE, chat semantics — no |
| **4** | Assistant session | **yes** (`bus.rs`, `protocol.rs`) | **yes** (`partial_transcript`/`turn_end`/`tts_interrupt`) | **ai+stt+tts** — pipeline | wake-word on phone | **No** |
| **5** | `capability_used` audit | **vynkor-only** | — | — | channel exists | **No** |
| **6** | Version negotiation | **yes** (`protocol.rs`) | **yes** (`min/max` in handshake) | — | — | Partial (major-reject) |
| **7** | Offline device command fate | **vynkor-only** | — (new `ErrorCode`) | — | — | **No** |
| **8** | TLS onboarding | **DONE** (auto-gen + `cert_pem` pinning) | — | — | pinning ready | **DONE (needs ACME docs)** |
| **9** | Per-device quota on `ai.chat` | **vynkor-only** | — | — | — | Partial (generic limits exist) |

### Execution Tracks (isolated)

- **Track A — `vynkor`-only** (no wire bump, no `ai`): **5, 7, 9** (+ fixes 6/8). Shippable in one PR.
- **Track B — `vynkor` + `vynkor-wire`**: **1, 6** (and 3 if `CHAT_DELTA` as new frame).
- **Track C — `vynkor` + `wire` + `vynkor-plugins/ai`**: **0, 3, 4**. Requires protocol + `ai` logic coordination.
- **Track D — cross-repo XL**: **2** (already DONE in kernel, remaining SDK/plugins/client).

---

## Recommended Order (from source doc, confirmed by audit)

```
1. Pairing ticket      P0  6–10h   Track B   unblocks demo
2. Chat streaming      P0  8–12h   Track C   biggest UX win
3. E-01 (parallel)     P0* XL      Track D   can run with 2, design ticket with pubkey upfront
4. Capability audit    P1  4–6h    Track A   cheap, big trust win
5. Assistant session   P1 12–20h   Track C   before wake-word wave
6..9                  P2  2–6h ea Track A/B as feedback arrives
```

*E-01 can start after RFC discussion; #1 does not block it but ticket should be designed with `device_pubkey` in mind.*

---

## Open Questions (from source doc)

1. Ticket exchange via existing WS handshake or separate `HTTP POST /devices/pair` before WS?
2. Streaming format: new `CHAT_DELTA` frames vs reusing event bus?
3. Assistant pipeline owner — kernel or separate `assistant` plugin?

---

## Details

Each file below is one task with plan, files, complexity and acceptance:

- **Track A (vynkor-only):**
  - `docs/tasks/CD-05-capability-audit.md` — `capability_used` audit
  - `docs/tasks/CD-07-offline-commands.md` — offline command fate
  - `docs/tasks/CD-09-per-device-quota.md` — quota on `ai.chat`
  - `docs/tasks/CD-06-version-negotiation.md` — min/max
  - `docs/tasks/CD-08-tls-onboarding.md` — TLS (almost DONE)

- **Track B (vynkor+wire):**
  - `docs/tasks/CD-01-pairing-ticket.md` — ticket without CLI

- **Track C (vynkor+wire+ai):**
  - `docs/tasks/CD-00-models-agents.md` — models/agents
  - `docs/tasks/CD-03-chat-streaming.md` — streaming
  - `docs/tasks/CD-04-assistant-session.md` — assistant session

- **Track D (cross-repo):**
  - `docs/tasks/CD-02-per-device-keys.md` — E-01 (DONE in kernel)

> All tasks follow `REMOTE_DEVICES_ROADMAP.md` / `ROADMAP.md` style: checkbox, `Files:`, `Acceptance:`, estimate.
