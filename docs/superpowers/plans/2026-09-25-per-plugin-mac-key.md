# Per-Plugin Frame-MAC Key Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Local plugins stop holding the master `jwt_secret`. Each one gets a key derived from the master secret and bound to its own `plugin_id`. That key can MAC frames for that one plugin only, and it cannot mint JWTs.

**Architecture:** Today the kernel feeds the master `jwt_secret` into `derive_session_key` as the IKM for every local (non-device) plugin, so every plugin needs the master secret in `VYN_JWT_SECRET`. The same secret signs JWTs, and registration takes `permissions` from JWT claims (`router.rs`, "token fields take precedence"). That means any plugin can mint itself any permission. The fix mirrors the E-01 device path: the IKM for a local plugin becomes `plugin_mac_secret(master, plugin_id)`, which is HKDF-SHA256 output hex-encoded to a string. The supervisor injects that string as `VYN_JWT_SECRET` when it spawns the plugin, so SDKs and plugins do not change. A `legacy_plugin_mac: true` config flag keeps the old behavior for one migration window.

**Tech Stack:** Rust 2021, `hkdf 0.12` + `sha2` (already direct deps of the kernel), tokio, `vynkor-sdk` (dev-dep) for integration tests.

**Spec:** Findings from the 2026-09-25 vynkor-plugins audit session (restated here, no separate spec doc):
- `~/.config/vyn/plugins.d/*.yaml`: all 38 plugin registrations carry `VYN_JWT_SECRET` equal to the master `jwt_secret` (verified by sha256 comparison).
- `src/kernel/orchestrator/mod.rs:147-152`: `mac_secret` is the master `jwt_secret` bytes.
- `src/ipc/protocol/router.rs` (~line 552-565): `ikm = device_secret.unwrap_or(master)`, then `derive_session_key(ikm, nonce, plugin_id)`.
- `src/ipc/protocol/router.rs` (~line 384-400): JWT `claims.permissions` override the manifest's.
- `docs/THREAT_MODEL.md:29`: "The crown jewels are the shared `jwt_secret` (compromise equals forging any identity)". Today every plugin process holds them.

## Global Constraints

- **No changes to `vynkor-wire`, `vynkor-sdk`, `vynkor-sdk-python`, `vynkor-sdk-cpp`, or any plugin.** SDKs read `VYN_JWT_SECRET` as a string and use its **UTF-8 bytes** as the IKM (`VynkorClient::connect_with_secret(path, s.as_bytes())`). The derived secret must therefore be delivered as a string whose bytes the kernel also uses as the IKM. Use lowercase hex, 64 chars.
- **Wire protocol unchanged.** `derive_session_key(ikm, nonce, plugin_id)` stays as is; only the IKM the kernel picks for local plugins changes.
- **Device-scoped connections (non-empty `device_id`, E-01) are untouched.** They keep `device_secret` as the IKM.
- **`src/bridge/` is out of scope.** It uses the device path.
- **JWT signing/validation is unchanged.** Tokens are still HS256 over the master `jwt_secret`; only the kernel and the operator CLI hold that secret.
- **Default is secure.** `legacy_plugin_mac` defaults to `false`.
- Derivation constants, copied verbatim everywhere: HKDF salt `b"vynkor-plugin-mac-v1"`, IKM = master secret bytes, info = `plugin_id` bytes, output 32 bytes → lowercase hex.
- Do **not** edit the working-copy `config.yaml` of the main checkout (the operator has local changes). Work in a git worktree off `origin/develop`; editing `config.yaml` inside the worktree is fine.
- Test commands: `cargo test --test unit`, `cargo test --test integration`, `cargo test --lib`. Run from the repo root.
- Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

1. **The operator's YAML still sets `VYN_JWT_SECRET=<master>`** (all 38 files do today). The supervisor must override it with the derived key, not let the YAML win, and log a warning once per spawn. Pinned in Task 3 by testing `mac_env_override` together with the env-application order.
2. **A plugin uses its derived key but registers under another `plugin_id`** (telegram's key, `plugin_id = "agent"`). The MAC must fail and the kernel must drop the connection. Pinned in Task 2 (`derived_key_of_other_plugin_is_rejected`).
3. **A client still presents the master secret on a secure-default kernel** (externally launched plugin, old harness). It must be rejected, not silently accepted. Pinned in Task 2 (`master_secret_client_rejected_by_default`).
4. **`legacy_plugin_mac: true`**: the master-secret client keeps working, so the migration can roll back. Pinned in Task 2 (`legacy_flag_accepts_master_secret`).
5. **Auth off (`jwt_secret: None`)**: nothing is injected, no MAC, behavior identical to today. Pinned in Task 3 (`no_master_means_no_injection`).

---

## File Structure

- Create `src/auth/plugin_key.rs`: the one derivation function. Pure, no I/O.
- Modify `src/auth/mod.rs`: `pub mod plugin_key;`.
- Modify `src/utils/config.rs`: the `legacy_plugin_mac: bool` config field.
- Modify `src/ipc/protocol/router.rs`: IKM selection for local plugins; thread the flag.
- Modify `src/kernel/orchestrator/mod.rs`: pass the flag to the router; hand master + flag to the supervisor.
- Modify `src/plugins/supervisor/mod.rs` and `src/plugins/supervisor/spawn.rs`: store master + flag; inject the derived `VYN_JWT_SECRET` after operator env.
- Modify `src/cli/token.rs`: `vyn token plugin-secret --plugin <id>` for plugins the kernel does not spawn.
- Tests: `tests/unit/test_plugin_key.rs` (new, registered in `tests/unit/mod.rs`) and `tests/integration/test_mac.rs` (extend).
- Docs: `docs/THREAT_MODEL.md` and `config.yaml` (commented example).

---

### Task 1: Derivation function

**Files:**
- Create: `src/auth/plugin_key.rs`
- Modify: `src/auth/mod.rs`
- Test: `tests/unit/test_plugin_key.rs`, `tests/unit/mod.rs`

**Interfaces:**
- Produces: `pub fn vynkor::auth::plugin_key::plugin_mac_secret(master: &[u8], plugin_id: &str) -> String`. Returns 64 lowercase hex chars, deterministic.

- [x] **Step 1: Write the failing test** in `tests/unit/test_plugin_key.rs`:

```rust
use vynkor::auth::plugin_key::plugin_mac_secret;

const MASTER: &[u8] = b"unit-test-master-secret-at-least-32-bytes";

#[test]
fn derived_secret_is_64_lowercase_hex() {
    let s = plugin_mac_secret(MASTER, "telegram");
    assert_eq!(s.len(), 64);
    assert!(s.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)), "{s}");
}

#[test]
fn derived_secret_is_deterministic() {
    assert_eq!(plugin_mac_secret(MASTER, "agent"), plugin_mac_secret(MASTER, "agent"));
}

#[test]
fn derived_secret_differs_per_plugin() {
    assert_ne!(plugin_mac_secret(MASTER, "agent"), plugin_mac_secret(MASTER, "telegram"));
}

#[test]
fn derived_secret_differs_per_master() {
    assert_ne!(
        plugin_mac_secret(MASTER, "agent"),
        plugin_mac_secret(b"another-master-secret-at-least-32-bytes!!", "agent")
    );
}

#[test]
fn derived_secret_never_equals_or_contains_master() {
    let s = plugin_mac_secret(MASTER, "agent");
    assert!(!s.as_bytes().windows(MASTER.len()).any(|w| w == MASTER));
}

#[test]
fn derived_secret_known_answer() {
    // Pins the constants (salt "vynkor-plugin-mac-v1", info = plugin_id) so a
    // silent change breaks every deployed plugin loudly here instead.
    // Filled in Step 4 from the first run's actual value.
    assert_eq!(plugin_mac_secret(b"k", "p"), KAT_K_P);
}
```

Add `mod test_plugin_key;` to `tests/unit/mod.rs` next to the other `mod test_*;` lines. For the KAT, temporarily declare `const KAT_K_P: &str = "";` at the top of the test file (it gets filled in Step 4).

- [x] **Step 2: Run to verify it fails**

Run: `cargo test --test unit test_plugin_key`
Expected: compile error `unresolved import vynkor::auth::plugin_key`.

- [x] **Step 3: Implement** `src/auth/plugin_key.rs`:

```rust
//! Per-plugin frame-MAC secret. A local plugin must not hold the master
//! `jwt_secret`: that secret also signs JWTs, and registration trusts JWT
//! `permissions` claims, so holding it means minting any permission. Instead
//! the kernel hands each spawned plugin HKDF(master, plugin_id) as
//! `VYN_JWT_SECRET`, and uses the same bytes as the frame-MAC IKM for that
//! plugin_id. The value is hex text because SDKs feed the env string's bytes
//! straight into `derive_session_key`.

use hkdf::Hkdf;
use sha2::Sha256;

const SALT: &[u8] = b"vynkor-plugin-mac-v1";

pub fn plugin_mac_secret(master: &[u8], plugin_id: &str) -> String {
    let hk = Hkdf::<Sha256>::new(Some(SALT), master);
    let mut okm = [0u8; 32];
    hk.expand(plugin_id.as_bytes(), &mut okm)
        .expect("HKDF expand of 32 bytes is always valid");
    okm.iter().map(|b| format!("{b:02x}")).collect()
}
```

Add `pub mod plugin_key;` to `src/auth/mod.rs` (alphabetical, after `pairing`).

- [x] **Step 4: Fill the KAT.** Run `cargo test --test unit derived_secret_known_answer`. The failure prints the actual value. Paste it into `KAT_K_P`. This is intentional: the value pins the implementation just written, and Steps 5 onward guard against it drifting.

- [x] **Step 5: Run all of them**

Run: `cargo test --test unit test_plugin_key`
Expected: 6 passed.

- [x] **Step 6: Commit**

```bash
git add src/auth/plugin_key.rs src/auth/mod.rs tests/unit/test_plugin_key.rs tests/unit/mod.rs
git commit -m "feat(auth): derive a per-plugin frame-MAC secret from jwt_secret"
```

---

### Task 2: Kernel uses the derived key for local plugins (+ config flag)

**Files:**
- Modify: `src/utils/config.rs` (add field near `allow_no_auth`, ~line 113)
- Modify: `src/ipc/protocol/router.rs` (router struct/constructor near line 82; `handle_kernel_message` signature ~line 320; call site ~line 258; IKM selection ~line 552)
- Modify: `src/kernel/orchestrator/mod.rs` (~line 147-205, where `mac_secret` is built and passed)
- Modify: `tests/integration/helpers.rs` (`test_config` must set the new field if `Config` is built with a struct literal)
- Test: `tests/integration/test_mac.rs`

**Interfaces:**
- Consumes: `plugin_mac_secret` (Task 1).
- Produces: `Config.legacy_plugin_mac: bool` (serde default `false`). Task 3 reads it.

- [x] **Step 1: Write failing integration tests.** Append to `tests/integration/test_mac.rs`. Use sockets and ports unique to this file (19502–19505); check for collisions with `command grep -rn '1950[2-5]' tests`.

```rust
use vynkor::auth::plugin_key::plugin_mac_secret;

/// Sends one MAC'd ping and reports whether a Pong came back within 2s.
async fn ping_gets_pong(client: &mut VynkorClient) -> bool {
    let env = Envelope {
        payload: Some(envelope::Payload::Ping(Ping { timestamp: 9 })),
        ..Default::default()
    };
    let mut buf = vec![];
    prost::Message::encode(&env, &mut buf).unwrap();
    if client.send_raw("kernel", buf).await.is_err() {
        return false;
    }
    for _ in 0..2 {
        match timeout(Duration::from_secs(2), client.recv()).await {
            Ok(Ok(e)) if matches!(e.payload, Some(envelope::Payload::Pong(_))) => return true,
            Ok(Ok(_)) => continue, // error frame before drop
            _ => return false,
        }
    }
    false
}

#[tokio::test]
async fn derived_plugin_secret_completes_handshake() {
    let secret = "integration-mac-secret-3-32-bytes-min";
    let sock = "/tmp/vynkor_mac_derived.sock";
    let (_s, _r, _b) = start_kernel_secured(sock, 19502, secret).await;
    let token = create_test_token("tg", vec![], secret.as_bytes(), 3600);
    let derived = plugin_mac_secret(secret.as_bytes(), "tg");
    let mut c = VynkorClient::connect_with_secret(sock, derived.as_bytes()).await.unwrap();
    let ack = c.register_with_token("tg", PluginManifest::default(), &token).await.unwrap();
    assert!(ack.accepted);
    assert!(ping_gets_pong(&mut c).await, "derived key must MAC-verify");
}

#[tokio::test]
async fn master_secret_client_rejected_by_default() {
    let secret = "integration-mac-secret-4-32-bytes-min";
    let sock = "/tmp/vynkor_mac_master.sock";
    let (_s, _r, _b) = start_kernel_secured(sock, 19503, secret).await;
    let token = create_test_token("tg", vec![], secret.as_bytes(), 3600);
    let mut c = VynkorClient::connect_with_secret(sock, secret.as_bytes()).await.unwrap();
    let _ = c.register_with_token("tg", PluginManifest::default(), &token).await;
    assert!(!ping_gets_pong(&mut c).await, "master secret must no longer MAC a local plugin");
}

#[tokio::test]
async fn derived_key_of_other_plugin_is_rejected() {
    let secret = "integration-mac-secret-5-32-bytes-min";
    let sock = "/tmp/vynkor_mac_cross.sock";
    let (_s, _r, _b) = start_kernel_secured(sock, 19504, secret).await;
    let token = create_test_token("agent", vec![], secret.as_bytes(), 3600);
    let telegram_key = plugin_mac_secret(secret.as_bytes(), "telegram");
    let mut c = VynkorClient::connect_with_secret(sock, telegram_key.as_bytes()).await.unwrap();
    let _ = c.register_with_token("agent", PluginManifest::default(), &token).await;
    assert!(!ping_gets_pong(&mut c).await, "telegram's key must not MAC as agent");
}

#[tokio::test]
async fn legacy_flag_accepts_master_secret() {
    let secret = "integration-mac-secret-6-32-bytes-min";
    let sock = "/tmp/vynkor_mac_legacy.sock";
    let mut cfg = super::helpers::test_config(sock, 19505);
    cfg.allow_no_auth = false;
    cfg.jwt_secret = Some(secret.to_string());
    cfg.legacy_plugin_mac = true;
    let (_s, _r, _b) = super::helpers::start_kernel_with_config(cfg).await;
    let token = create_test_token("tg", vec![], secret.as_bytes(), 3600);
    let mut c = VynkorClient::connect_with_secret(sock, secret.as_bytes()).await.unwrap();
    let ack = c.register_with_token("tg", PluginManifest::default(), &token).await.unwrap();
    assert!(ack.accepted);
    assert!(ping_gets_pong(&mut c).await, "legacy mode keeps master-secret MAC");
}
```

Also update the existing `secured_kernel_completes_mac_handshake_and_pings`: replace `secret.as_bytes()` in its `connect_with_secret` call with `plugin_mac_secret(secret.as_bytes(), "mac-plugin").as_bytes()`. Keep `create_test_token(..., secret.as_bytes(), ...)` on the master: JWTs are still signed by the master. Check that `start_kernel_with_config` is `pub` in `helpers.rs` and returns the same tuple; if it is private, make it `pub`.

- [x] **Step 2: Run to verify failure**

Run: `cargo test --test integration test_mac`
Expected: compile error "no field `legacy_plugin_mac`". Once the field exists, `derived_plugin_secret_completes_handshake`, `master_secret_client_rejected_by_default` and the updated handshake test must FAIL. The other two may pass for the wrong reason.

- [x] **Step 3: Add the config field** in `src/utils/config.rs`, right after `allow_no_auth`:

```rust
    /// Migration escape hatch: when true, local plugins MAC their frames with
    /// the master `jwt_secret` (pre-2026-09-25 behavior) instead of the
    /// per-plugin key `plugin_mac_secret(jwt_secret, plugin_id)`. Insecure —
    /// every plugin then holds a secret that can mint any JWT. Remove once all
    /// externally launched plugins use `vyn token plugin-secret`.
    #[serde(default)]
    pub legacy_plugin_mac: bool,
```

If `Config` has a manual `Default` impl or struct literals (`test_config` in `tests/integration/helpers.rs`, and any others: `command grep -rn 'allow_no_auth:' src tests`), add `legacy_plugin_mac: false` next to each `allow_no_auth:`.

- [x] **Step 4: Thread the flag into the router.** The router receives `mac_secret: Option<Arc<Vec<u8>>>` in its run/constructor fn (`router.rs` ~line 82) and passes `&mac_secret` into `handle_kernel_message` (~line 266). Add a `legacy_plugin_mac: bool` parameter right after `mac_secret` at the constructor, at the `handle_kernel_message` signature (~line 328) and at every call site (`command grep -n 'handle_kernel_message(' src`). In `src/kernel/orchestrator/mod.rs`, pass `config.legacy_plugin_mac` right after `mac_secret` (~line 202).

- [x] **Step 5: Change the IKM selection** in `router.rs`. It currently reads:

```rust
                    let ikm: &[u8] = match &device_secret {
                        Some(s) => s.as_slice(),
                        None => secret.as_slice(),
                    };
```

Replace with:

```rust
                    // local plugins MAC with a key bound to their own
                    // plugin_id, never the master secret that signs JWTs
                    // (legacy_plugin_mac keeps the old behavior for migration)
                    let plugin_secret;
                    let ikm: &[u8] = match &device_secret {
                        Some(s) => s.as_slice(),
                        None if legacy_plugin_mac => secret.as_slice(),
                        None => {
                            plugin_secret = crate::auth::plugin_key::plugin_mac_secret(
                                secret.as_slice(),
                                &plugin_id,
                            );
                            plugin_secret.as_bytes()
                        }
                    };
```

Also update the comment in `orchestrator/mod.rs` above `let mac_secret` ("the same secret used for JWT") so it says local plugins get a per-plugin key derived from it.

- [x] **Step 6: Run tests**

Run: `cargo test --test integration test_mac`
Expected: all MAC tests pass (the 2 existing + 4 new).
Then `cargo test --test integration` and `cargo test --test unit` must stay green. Any other test that connects with the raw master secret now fails. Fix each one by switching it to `plugin_mac_secret(master, <its plugin_id>)`; do not flip it to legacy mode. List what you changed in the commit body. To find them: `command grep -rn 'connect_with_secret\|VYN_JWT_SECRET' tests`. `sdk_harness.rs`, `test_sdk_python.rs` and `test_sdk_cpp.rs` may set `VYN_JWT_SECRET` on a child process; give them the derived value for the plugin_id they register.

- [x] **Step 7: Commit**

```bash
git add -A src tests
git commit -m "fix(auth): MAC local plugin frames with a per-plugin key, not jwt_secret"
```

---

### Task 3: Supervisor injects the derived key into spawned plugins

**Files:**
- Modify: `src/plugins/supervisor/mod.rs` (struct `PluginSupervisor` ~line 108; add a setter next to `set_data_dir` ~line 151)
- Modify: `src/plugins/supervisor/spawn.rs` (env block ~line 90-101)
- Modify: `src/kernel/orchestrator/mod.rs` (~line 255, next to `supervisor.set_data_dir(...)`)
- Test: unit tests inside `src/plugins/supervisor/spawn.rs` (`#[cfg(test)] mod tests`, create it if absent)

**Interfaces:**
- Consumes: `plugin_mac_secret` (Task 1), `Config.legacy_plugin_mac` (Task 2).
- Produces: `PluginSupervisor::set_plugin_mac(&mut self, master: Option<Arc<Vec<u8>>>, legacy: bool)` and `pub(crate) fn mac_env_override(master: Option<&[u8]>, legacy: bool, plugin_id: &str) -> Option<String>`.

- [x] **Step 1: Write the failing unit tests** at the bottom of `src/plugins/supervisor/spawn.rs`:

```rust
#[cfg(test)]
mod mac_env_tests {
    use super::mac_env_override;
    use crate::auth::plugin_key::plugin_mac_secret;

    const M: &[u8] = b"supervisor-test-master-secret-32-bytes!!";

    #[test]
    fn injects_derived_key_by_default() {
        assert_eq!(
            mac_env_override(Some(M), false, "telegram"),
            Some(plugin_mac_secret(M, "telegram"))
        );
    }

    #[test]
    fn legacy_mode_injects_nothing() {
        // legacy: operator YAML keeps supplying VYN_JWT_SECRET as before
        assert_eq!(mac_env_override(Some(M), true, "telegram"), None);
    }

    #[test]
    fn no_master_means_no_injection() {
        assert_eq!(mac_env_override(None, false, "telegram"), None);
    }

    #[test]
    fn override_wins_over_operator_env() {
        let operator_env = vec!["VYN_JWT_SECRET=the-master".to_string(), "X=1".to_string()];
        let merged = merged_env(&operator_env, mac_env_override(Some(M), false, "p"));
        let v: Vec<_> = merged.iter().filter(|(k, _)| k == "VYN_JWT_SECRET").collect();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].1, plugin_mac_secret(M, "p"));
        assert!(merged.iter().any(|(k, v)| k == "X" && v == "1"));
    }

    use super::merged_env;
}
```

- [x] **Step 2: Run to verify failure**

Run: `cargo test --lib mac_env_tests`
Expected: compile error: `mac_env_override` / `merged_env` not found.

- [x] **Step 3: Implement the pure helpers** in `spawn.rs` (module level):

```rust
/// The `VYN_JWT_SECRET` value the kernel forces on a spawned plugin: its
/// per-plugin MAC key. `None` = inject nothing (auth off, or legacy mode
/// where the operator's YAML still supplies the master secret).
pub(crate) fn mac_env_override(master: Option<&[u8]>, legacy: bool, plugin_id: &str) -> Option<String> {
    match master {
        Some(m) if !legacy => Some(crate::auth::plugin_key::plugin_mac_secret(m, plugin_id)),
        _ => None,
    }
}

/// Operator `KEY=VALUE` env, then the kernel's MAC key on top: the override
/// replaces any operator-supplied `VYN_JWT_SECRET` (today every plugins.d
/// file carries the master secret there).
pub(crate) fn merged_env(operator_env: &[String], mac_override: Option<String>) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = operator_env
        .iter()
        .filter_map(|kv| kv.split_once('='))
        .filter(|(k, _)| mac_override.is_none() || *k != "VYN_JWT_SECRET")
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    if let Some(key) = mac_override {
        out.push(("VYN_JWT_SECRET".to_string(), key));
    }
    out
}
```

- [x] **Step 4: Run the helper tests**

Run: `cargo test --lib mac_env_tests`
Expected: 4 passed.

- [x] **Step 5: Wire it into the supervisor.**

In `src/plugins/supervisor/mod.rs`, add fields to `PluginSupervisor` and initialize them in the constructor that sets `data_dir: None` (~line 167):

```rust
    /// Master jwt_secret + legacy flag, used only to derive each spawned
    /// plugin's per-plugin MAC key (`VYN_JWT_SECRET`). Never passed through.
    pub(crate) mac_master: Option<std::sync::Arc<Vec<u8>>>,
    pub(crate) legacy_plugin_mac: bool,
```

(init: `mac_master: None, legacy_plugin_mac: false,`), plus the setter next to `set_data_dir`:

```rust
    pub fn set_plugin_mac(&mut self, master: Option<std::sync::Arc<Vec<u8>>>, legacy: bool) {
        self.mac_master = master;
        self.legacy_plugin_mac = legacy;
    }
```

In `spawn.rs`, replace:

```rust
        for kv in &config.env {
            if let Some((k, v)) = kv.split_once('=') {
                cmd.env(k, v);
            }
        }
```

with:

```rust
        let mac_override = mac_env_override(
            self.mac_master.as_deref().map(|v| v.as_slice()),
            self.legacy_plugin_mac,
            &config.plugin_id,
        );
        if mac_override.is_some() && config.env.iter().any(|kv| kv.starts_with("VYN_JWT_SECRET=")) {
            warn!(
                plugin_id = %config.plugin_id,
                "plugin config sets VYN_JWT_SECRET; ignored — the kernel injects a per-plugin key. Remove it from plugins.d"
            );
        }
        for (k, v) in merged_env(&config.env, mac_override) {
            cmd.env(k, v);
        }
```

In `src/kernel/orchestrator/mod.rs`, next to `supervisor.set_data_dir(config.data_dir.clone());` (~line 255):

```rust
        supervisor.set_plugin_mac(
            config.jwt_secret.as_ref().map(|s| Arc::new(s.as_bytes().to_vec())),
            config.legacy_plugin_mac,
        );
```

(Reuse the existing `mac_secret` Arc via `.clone()` if it is still in scope at that point; otherwise build it as above.)

- [x] **Step 6: Run the full suite**

Run: `cargo test --lib && cargo test --test unit && cargo test --test integration`
Expected: all green. `test_autoload.rs` / `test_shim.rs` spawn real plugins: if one runs on a secured kernel and its child connects with a secret, it now receives the derived key automatically. That is the point, and it must pass unchanged.

- [x] **Step 7: Commit**

```bash
git add src/plugins/supervisor src/kernel/orchestrator/mod.rs
git commit -m "feat(supervisor): inject the per-plugin MAC key as VYN_JWT_SECRET"
```

---

### Task 4: CLI for externally launched plugins + docs

**Files:**
- Modify: `src/cli/token.rs`
- Modify: `docs/THREAT_MODEL.md` (lines ~20-30 asset table + crown-jewels paragraph; "Residual risk" list ~line 162)
- Modify: `config.yaml` (commented example next to `jwt_secret`, ~line 11)
- Test: `tests/unit/test_plugin_key.rs` (CLI output helper)

**Interfaces:**
- Consumes: `plugin_mac_secret`.
- Produces: `TokenCmd::PluginSecret { plugin: String }`, which prints `plugin_mac_secret(jwt_secret, plugin)`.

- [ ] **Step 1: Add the subcommand** to `TokenCmd` in `src/cli/token.rs`:

```rust
    /// Print the per-plugin frame-MAC secret for a plugin the kernel does not
    /// spawn itself (dev runs, external harnesses). Set it as VYN_JWT_SECRET.
    /// Supervised plugins get it injected automatically.
    PluginSecret {
        /// The plugin_id the process registers as.
        #[arg(long)]
        plugin: String,
    },
```

and a match arm in `handle`:

```rust
        TokenCmd::PluginSecret { plugin } => {
            println!("{}", crate::auth::plugin_key::plugin_mac_secret(secret.as_bytes(), &plugin));
            Ok(())
        }
```

- [ ] **Step 2: Verify manually**

Run: `cargo run -q -- token --help` (check how `vyn token` is wired in `src/cli/mod.rs` ~line 49 for the exact config flag) and then `cargo run -q -- token plugin-secret --plugin demo` with a temp config holding `jwt_secret: "cli-test-master-secret-at-least-32-bytes"`.
Expected: 64 hex chars, equal to `plugin_mac_secret(b"cli-test-master-secret-at-least-32-bytes", "demo")`. Assert that with a one-off `#[test]` in `tests/unit/test_plugin_key.rs` if the CLI exposes a callable fn; otherwise record the manual output in the commit body.

- [ ] **Step 3: Docs.** In `docs/THREAT_MODEL.md`:
  - asset row "Plugin configs + credentials": env is now `VYN_JWT_TOKEN` + a **per-plugin** `VYN_JWT_SECRET` injected by the supervisor;
  - after "The crown jewels are the shared `jwt_secret`…" add: "Since 2026-09-25 local plugins never receive it: each gets `plugin_mac_secret(jwt_secret, plugin_id)` (HKDF-SHA256, salt `vynkor-plugin-mac-v1`), which MACs frames for that plugin_id only and cannot sign JWTs. `legacy_plugin_mac: true` restores the old exposure for migration.";
  - Residual risk: add "Plugins' `VYN_JWT_TOKEN`s are long-lived (exp ~2036 in the reference deployment). A stolen token is still replayable by a process that also has that plugin's derived key."

  In `config.yaml` below the commented `jwt_secret` line:

```yaml
#   # Local plugins get a per-plugin frame-MAC key injected as VYN_JWT_SECRET;
#   # do NOT put the master jwt_secret in plugins.d. Migration only:
#   # legacy_plugin_mac: true
```

- [ ] **Step 4: Full suite + clippy**

Run: `cargo test --lib && cargo test --test unit && cargo test --test integration && cargo clippy --all-targets -- -D warnings`
Expected: green. If clippy fails on code this plan did not touch, note it in the report instead of fixing it.

- [ ] **Step 5: Commit**

```bash
git add src/cli/token.rs docs/THREAT_MODEL.md config.yaml tests/unit
git commit -m "feat(cli): vyn token plugin-secret; document per-plugin MAC keys"
```

---

## Operator follow-up (NOT part of this plan's execution — the human does this after merge + deploy)

1. Deploy the new kernel. Supervised plugins keep working: the kernel overrides the YAML `VYN_JWT_SECRET` and logs a warning per plugin.
2. Remove `VYN_JWT_SECRET=` lines from `~/.config/vyn/plugins.d/*.yaml` **and** the `*.bak*` copies.
3. **Rotate `jwt_secret`.** The old value sat in 38+ files, so treat it as leaked. Then re-mint every plugin's `VYN_JWT_TOKEN` and re-pair devices (the device store key derives from `jwt_secret`; see `device_store.rs:134`).
