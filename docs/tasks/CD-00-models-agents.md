# CD-00 — Models & Agents Announced by Host Plugin

*Track C — `vynkor` + `vynkor-wire` + `vynkor-plugins/ai` · P0 · Source: `CLIENT_DRIVEN_KERNEL_TASKS.md` §0*

## Goal

Client must not guess the list of models/agents. Both lists come from the `ai` plugin on the host, cached per-profile. If host is silent — honest `unavailable`.

---

## Already Exists

- `../vynkor-plugins/plugins/ai/src/db.rs` — `AiDb` (SQLite `~/.local/share/vyn/ai.db`): `models(id, provider, base_url, api_key_env, is_default)`, `agents(id, name, model_id, system_prompt, goal, description, is_default)`, `discovery.rs` → `GET {base_url}/models` + `GET /api/tags` (Ollama).
- `../vynkor-plugins/plugins/ai/src/handler.rs` — `handle_list_models`/`handle_list_agents` (return `serde_json::to_vec` from DB).
- `../vynkor-plugins/plugins/ai/src/main.rs` — `manifest.actions = ["chat_completion","embedding","list_models","list_agents","refresh_models","usage_stats"]`, registration without `device_id` (host plugin → `"local"`).
- Kernel (`vynkor`) — generic `action_specs[]` in `PluginManifest` (D-01, `v1.6`), `get_manifest`/`list_plugins` (READONLY_COMMANDS), `system.plugin_joined` carries `action_specs`.
- Client already fetches `models[]/agents[]` on connect (`ChatSettings`).

## Required

- [ ] **CD-00 — stable read-only endpoints `models[]`/`agents[]` (vynkor+wire+ai, 4–8h):**
  - **Option A (recommended) — no new wire field:** `ai` already serves `list_models`/`list_agents` as `ActionRequest`. Kernel already has `get_manifest`/`list_plugins` for discovery. Enough to **document** that `list_models`/`list_agents` is the public contract of the `ai` plugin, not a naming convention, and add per-profile caching in client. Zero kernel work — just **authoritative contract**.
    - **Files:** `../vynkor-plugins/plugins/ai/README.md` or `docs/PLUGIN_REGISTRY_SCHEMA.md` — section "AI plugin contract: `list_models`/`list_agents`"; `vynkor/docs/CLIENT_DRIVEN_SPLIT.md` — link.
    - **Acceptance:** `ai` answers `list_models` → `[{id:"gpt-4o", displayName:"GPT-4o", context:128000}]` stably; client shows honest `unavailable` when empty; test `test_list_models_contract` green in `ai`.

  - **Option B — with wire bump (if kernel discovery needed):** add to `PluginManifest` fields `models: repeated ModelInfo{id, displayName, context, provider}` and `agents: repeated AgentInfo{id, name, description, allowedModels[]}` or separate `ModelInfo`/`AgentInfo` messages; kernel stores as `registry.get_model_info(plugin_id)` and serves via `GET /models`/`GET /agents` (REST) and `system.plugin_joined` payload.
    - **Files:** `../vynkor-wire/proto/vynkor_protocol.proto` (`ModelInfo`, `AgentInfo`, fields in `PluginManifest`), `src/plugins/registry.rs` (storage), `src/api/routes.rs` (`GET /models`, `GET /agents`), `src/events/bus.rs` (enrich joined event), `../vynkor-plugins/plugins/ai/src/main.rs` (fill manifest on `register_full`).
    - **Acceptance:** `GET /models` → `models[]` from `ai` manifest; `GET /agents` → `agents[]`; `plugin_joined` carries both lists; `cargo test` — drift test on 6 proto copies.

  - **Do not:** guess models on client (fallback `unavailable`), do not store models in kernel (pass-through only).

## Anticipate (verified in code)

- **Wire `reserved`:** `PluginManifest` already `reserved 4,5,6 ("needs_ai","needs_gpu","priority")` — for `models`/`agents` use 10/11 (as in plan), verify `sdk-python/proto` sync of 6 copies.
- **Generic already exists:** `action_specs[]` in `PluginManifest` (D-01 v1.6) already serves discovery — do not duplicate without need. Verified: `ai` already `list_models` in `handler.rs`.
- **Cache:** per-profile cache in client, `unavailable` when host silent — do not guess.

## Implementation Plan

### Option A (MVP, 0 kernel work)

1. In `../vynkor-plugins/plugins/ai` add `docs/AI_CONTRACT.md` — stable shape of `list_models`/`list_agents` (id, displayName, context, allowedModels).
2. In `vynkor/docs/PLUGIN_REGISTRY_SCHEMA.md` — link to contract.
3. Client — already ready; add per-profile cache (`vynkor-client-android`).

### Option B (if kernel should be source of truth)

1. `vynkor-wire/proto` — `message ModelInfo {string id, displayName; uint32 context; string provider}` + `message AgentInfo {string id, name, description; repeated string allowedModels}`, fields `repeated ModelInfo models = 10; repeated AgentInfo agents = 11` in `PluginManifest` (additive, check `reserved` 4/5/6 busy — use 10/11).
2. `vynkor/src/plugins/registry.rs` — `PluginEntry.manifest.models/agents` already as `PluginManifest` (prost), store — zero code.
3. `vynkor/src/api/routes.rs` — `GET /models` (aggregates `models` of all `ai` plugins), `GET /agents`.
4. `vynkor/src/events/bus.rs:plugin_lifecycle_payload` — add `models`/`agents` to joined payload (as already for `action_specs`).

## Complexity / Value / Time

| | Estimate |
|---|---|
| **Complexity** | XS (Option A) / S (Option B — only additive proto) |
| **Value** | High — removes model guessing on client, honest `unavailable` |
| **Time** | **A: 0h in kernel / B: 4–8h** (2h proto+registry, 2h REST+event, 2h ai manifest) |
| **Risk** | Low — additive, does not break `action_specs` |
| **Dependencies** | `ai` plugin already has DB — just declare contract |
| **Depends on** | None |

---

## Recommendation

Ship **Option A** now (0 kernel work, docs only), **Option B** — when core-level discovery is needed (e.g. `vyn models list` CLI).
