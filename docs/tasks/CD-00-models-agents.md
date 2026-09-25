# CD-00 — Models & Agents Announced by Host Plugin

*Track C — `vynkor-plugins/ai` only · P0 · Source: `CLIENT_DRIVEN_KERNEL_TASKS.md` §0*

> **Decision (2026-09-24):** the `ai` plugin's existing `list_models` /
> `list_agents` actions are the contract, and their output schema in
> `vynkor-plugins/plugins/ai/plugin.json` (`action_specs`, ~L328-380) **is**
> the authoritative shape. No proto change, no kernel storage — Option B
> (`ModelInfo`/`AgentInfo` in `PluginManifest`, `GET /models`) is dropped.
> `docs/PLUGIN_REGISTRY_SCHEMA.md` is the wrong home for this (it is the
> marketplace schema, owned by `vynm` now).
>
> **Status:** kernel — nothing to do. Remaining work lives in `vynkor-plugins`.

## Goal

Client must not guess the list of models/agents. Both lists come from the `ai`
plugin on the host, cached per-profile. If host is silent — honest `unavailable`.

## Already Exists

- `vynkor-plugins/plugins/ai/src/main.rs` (~L73-105) — manifest `actions`
  include `list_models`/`list_agents`; dispatch to `handler::handle_list_models`
  / `handle_list_agents` (serialize rows from `AiDb`).
- `vynkor-plugins/plugins/ai/plugin.json` (~L328-380) — `action_specs` for both
  actions, including output schema.
- `vynkor-plugins/plugins/ai/src/db.rs` — `AiDb` (`models`, `agents` tables);
  `discovery.rs` (`GET {base_url}/models`, Ollama `/api/tags`).
- Kernel — generic `action_specs[]` in `PluginManifest`, `get_manifest` /
  `list_plugins`, `system.plugin_joined` carries `action_specs`. Enough for
  discovery; no model-specific surface needed.

## Required (vynkor-plugins)

- [ ] **Strip `api_key_env` from the client-facing `list_models` output** — it
      leaks host env var names to every caller. Keep it in the DB / internal
      resolution only; update the output schema in `plugin.json`.
- [ ] Optional: add `display_name` to model rows (and the output schema).
- [ ] **Contract test** in the `ai` plugin (`test_list_models_contract`):
      output validates against the `plugin.json` output schema and never
      contains `api_key_env`.
- Client: per-profile cache + honest `unavailable` when empty/silent.

**Do not:** guess models on the client; store models in the kernel.

## Complexity / Value / Time

| | Estimate |
|---|---|
| **Complexity** | XS |
| **Value** | High — removes model guessing on client |
| **Time** | ~2h in `vynkor-plugins`, 0h kernel |
| **Depends on** | None |
