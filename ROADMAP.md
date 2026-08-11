# Veyron ROADMAP — Phase 8

**Baseline:** 2026-08-10 · Kernel `0.1.0`
**Branch:** `develop`
**Previous phases:** `docs/archive/` (Phase 1–2: `ROADMAP_phase1.md`/`ROADMAP_v2.md`/`ROADMAP_v3.md` · Phase 3–4: `ROADMAP_v4.md` · Phase 5: `ROADMAP_v5.md` · Phase 6: `ROADMAP_v6.md` · Phase 7 (C++/Python SDK parity): `ROADMAP_v7.md`, all items complete)

---

## Manifesto (non-negotiable)

- Kernel = dumb byte router + process supervisor. Zero business logic. Zero AI. Zero application databases.
- Intra-host IPC = UDS only. No TCP, no Redis, no queues.
- Protocol = single `.proto` file. Changes propagate to all SDKs.
- Plugin = isolated OS process. Cannot bypass kernel. Speaks only UDS.
- External access = WebSocket/HTTP gateway only (Axum).

---

## Current baseline — 2026-08-10

Phase 7 shipped full C++/Python SDK parity with the Rust reference client
(`publish_event`, streaming actions, session close, cross-SDK integration
tests). On the protocol side, kernel support for `PERMISSION_STORAGE` and
`ActionRequest.caller_plugin_id` landed on `develop` (`13274d4`,
`e00ec96`, `6b3691f`) — but the marketplace installer's permission allowlist
(`src/marketplace/installer.rs`) was never updated to match, so installing the
`database` plugin fails with `Plugin 'database' declares unknown permission
'PERMISSION_STORAGE'`. Phase 8 fixes that drift at the root: the known
permission set stops being a hand-maintained list and is derived from the
`PermissionType` proto enum itself, so it can never fall behind the protocol
again.

## Phase 8 — Permission/protocol sync

The proto's `PermissionType` enum is the single source of truth for what a
plugin may declare. Phase 7's protocol work proved the enum can grow
(`PERMISSION_EVENT_PUBLISH`, `PERMISSION_STORAGE`) while the installer's
hand-typed list quietly rots. This phase derives the validation set from the
generated enum, adds drift-detection tests, and lands the follow-ups the
`database` plugin needs across the sibling repos.

- [x] R8-01 — **Installer permission validation derived from the proto enum:**
  replace `const KNOWN_PERMISSIONS` in `src/marketplace/installer.rs` with a
  set derived from `PermissionType::values()` (prost `Enumeration`), accepting
  both the documented lowercase form (`storage`) and the `PERMISSION_`-prefixed
  proto name (`PERMISSION_STORAGE`); unknown permissions keep the exact
  existing error.
  - Files: `src/marketplace/installer.rs`, `tests/unit/test_installer.rs`.
  - Acceptance: `vyn plugin install database` no longer fails with the
    unknown-permission error; `"teleport"` is still rejected.

- [x] R8-02 — **Permission drift-detection tests:** new test walking
  `PermissionType::values()`: every variant except `PERMISSION_UNKNOWN` must
  pass `validate_manifest` in both forms — a future proto permission can never
  silently fail installation again.
  - Files: `tests/unit/test_installer.rs`.
  - Acceptance: `cargo test --test unit manifest_accepts_every_proto_permission` green.

- [x] R8-03 — **Runtime `check_permission` form normalization:**
  `src/auth/permissions.rs::check_permission` compares declared permissions
  against `as_str_name()` with exact equality; normalize the comparison
  (case-insensitive, `PERMISSION_`-prefix-stripped on both sides) so a plugin
  declaring the lowercase form is not denied at runtime.
  - Files: `src/auth/permissions.rs`, unit tests.
  - Acceptance: a manifest declaring `"network"` satisfies a
    `PermissionNetwork` requirement; missing permissions still denied.

- [x] R8-04 — **Registry schema doc alignment:** `docs/PLUGIN_REGISTRY_SCHEMA.md`
  adds `storage`/`event_publish` rows to the permission mapping table and
  notes that the allowed set is derived from the `PermissionType` proto enum
  (both forms accepted).
  - Acceptance: mapping rows present; no schema restructuring.

- [x] R8-05 — **Proto-copy byte-identity drift test:** test asserting the three
  vendored `veyron_protocol.proto` copies (`wire/proto`, `sdk/python/proto`,
  `sdk/cpp/proto`) stay byte-identical.
  - Files: `tests/unit/test_proto_sync.rs` (new), `tests/unit/mod.rs`.
  - Acceptance: test passes; editing one copy makes it fail.

- [x] R8-06 — **(cross-repo) Land the `database` plugin in veyron-plugins:**
  merge `worktree-database-plugin` into `veyron-plugins` `main`
  (`plugins/database/`), add the registry entry via `scripts/package.sh`
  (permissions normalized to `["storage"]`), build `dist/database-0.1.0.zip`,
  move `database` from Planned to Shipped in `veyron-plugins/ROADMAP.md`.
  - Tracked in: `veyron-plugins/ROADMAP.md`.
  - Acceptance: `vyn plugin install database` against the fixed kernel succeeds
    and the plugin registers.

- [x] R8-07 — **(cross-repo) Publish `veyron-wire` 0.2.0, drop patch override:**
  `cargo publish` `veyron-wire` 0.2.0 (kernel already depends on it by
  path + `[patch.crates-io]` override whose comment says it is a no-op once
  0.2.0 is published); remove the override and verify the workspace still
  builds green.
- Tracked in: `veyron-wire/`.
- Acceptance: `cargo search veyron-wire` shows 0.2.0; no `patch.crates-io`
  block in `Cargo.toml`; full test suite green without it.

---

## Immediate — Audit findings (nearest)

Findings from the 2026-08-11 audit reconciliation (see `AUDIT.md`). All are
kernel-local, independent of the cross-repo R8 items, and land before any
Phase 9 work. N5 restores the DoD gates; N2 closes the last
permission-surface inconsistency; N1 is the largest single hot-path win.

> **STATUS (2026-08-11): N1–N5 all shipped on `develop`.** N1 closed as
> non-issue — wire v0.2.0 already shares the payload via `Arc<[u8]>`; sharing
> locked in by regression tests. N2–N5 fixed with regression coverage (see
> each item). N5 restores the DoD `fmt` gate.

- [x] N1 — **Router hot path clones every forwarded payload (Moderate):**
  `forward` (`protocol.rs:995`) does `payload: msg.frame.payload.clone()`
  — a full `Vec<u8>` heap copy per frame (up to 1 MiB); `broadcast` has the
  same pattern. The event path already shares bytes via `Arc<[u8]>`
  (`events/bus.rs:121`). Contradicts the README "zero copies" claim and is
  the dominant router cost at high throughput.
  - Files: `src/ipc/protocol.rs`.
  - Acceptance: `forward`/`broadcast` route a payload-sharing frame
    (`Arc<[u8]>`) without per-hop clone, mirroring `EventBus::deliver`;
    README §3 updated only if the sharing semantics change.
  - **Status (2026-08-11): CLOSED — non-issue.** `Frame.payload` is already
    `Arc<[u8]>` in wire v0.2.0 (`wire/src/framing.rs:69-81`), so the clone is
    a refcount bump. Regression tests `forward_shares_payload_without_copy`
    / `broadcast_shares_payload_without_copy` (`tests/unit/test_router.rs`)
    assert `Arc::ptr_eq` on a 64 KiB payload.

- [x] N2 — **Permission comparison is form-sensitive in the clamp + config
      cross-check (Low, fails-closed):** the T-04 registration clamp
      (`protocol.rs:336`) and `validate_plugin_def` (`loader.rs:249`)
      compare permission strings with exact equality, while runtime
      `check_permission` normalizes (`permissions.rs:21-25,36`). A config
      `permissions: [network]` with a token claiming `PERMISSION_NETWORK`
      silently strips the permission at registration (warn only) or
      refuses boot.
  - Files: `src/ipc/protocol.rs`, `src/plugins/loader.rs`, `src/auth/permissions.rs`.
  - Acceptance: normalize both sides; extend
    `registration_clamps_jwt_permissions_to_config_allowlist`
    (`tests/unit/test_router.rs:948`) to cover both forms.
  - **Status (2026-08-11): FIXED** — `normalize_permission` is `pub(crate)`;
    the clamp builds a normalized `HashSet` (`protocol.rs:336-343`);
    `validate_plugin_def` uses a normalized `any` match (`loader.rs:250-260`).
    Covered by the parametrized clamp test (both forms) and
    `config_lowercase_perm_matches_manifest_proto_form` (with negative control).

- [x] N3 — **`load_config` performs no numeric bounds validation (Low):**
  `config.rs:318` accepts `router_channel_capacity: 0`, `max_connections: 0`,
  and negative watchdogs silently.
  - Files: `src/utils/config.rs`.
  - Acceptance: out-of-range numerics clamp to defaults or error loudly.
  - **Status (2026-08-11): FIXED** — `clamp_invalid_numerics` clamps the four
    zero-invalid fields to defaults with `warn!` (fields are unsigned, so
    negatives already fail serde). Covered by
    `load_config_clamps_zero_numerics_to_defaults` /
    `load_config_preserves_sane_numerics`.

- [x] N4 — **Daemon start reports success before the child holds the
      pid-file lock (Low, TOCTOU):** `daemonize_and_run` (`main.rs:213-236`)
      writes the pid and returns success; the re-exec'd child acquires the
      exclusive flock only later in `run_foreground` (`main.rs:238-264`) —
      a competing instance winning the lock first aborts the child after
      the parent already reported "started".
  - Files: `src/main.rs`.
  - Acceptance: readiness handshake — the child reports success (exit
    status or explicit ready line) before the parent confirms.
  - **Status (2026-08-11): FIXED** — `UnixStream::pair()` handshake via
    `VEYRON_READY_FD` (CLOEXEC cleared in `pre_exec`): the child emits
    `"{pid}\n"` only after flock + pid write; the parent publishes the pid
    file and reports success only after the matching line, else SIGKILLs +
    reaps + cleans up and errors out. Smoke-verified happy and failure paths.

- [x] N5 — **`cargo fmt --check` fails on `tests/unit/test_proto_sync.rs:43`
      (Low, DoD violation):** unformatted closure (introduced `61aec96`);
      the only offender against the Definition of Done gate.
  - Files: `tests/unit/test_proto_sync.rs`.
  - Acceptance: `cargo fmt --check` exits 0.
  - **Status (2026-08-11): FIXED** — `cargo fmt` run tree-wide; `fmt --check` exits 0.

> Deferred audit items M7 (C++/Python fuzz harness) and M9 (zero-value enum
> renumber, wire-breaking) remain open — tracked in the Task Summary below
> and in `AUDIT.md`. M7 is the last substantive coverage gap; M9 rides the
> protocol v1.4 bump (P11-03).

---

## Phase 9 — Hard isolation (deferred)

Deferred until the R8 cross-repo items ship. The current sandbox (`sandbox:
true`) isolates via user + network namespaces plus rlimits only — plugins
share the host PID space (they can enumerate every host process via
`/proc`), read/write host files as the real uid, and their `max_procs`
budget is *shared with every other process of the same uid* (RLIMIT_NPROC
is checked at each fork/clone against the real-uid thread count, walked up
the entire user-namespace tree to `init_user_ns`). Phase 9 replaces the
shared-uid rlimit accounting with cgroup accounting and closes the
visibility/file-system gaps.

- [x] R9-01 — **Per-plugin process accounting via cgroup v2 `pids.max`
      instead of RLIMIT_NPROC:** today a plugin's thread budget is the
      host-wide real-uid count (desktop sessions routinely run ~700+
      threads, so `max_procs` is a *shared* budget, not per-plugin
      isolation — a thread storm in one plugin or in the desktop starves
      the other). The cgroup v2 `pids` controller counts tasks *inside the
      cgroup only*: supervisor creates a `veyron/<plugin_id>.scope` cgroup,
      writes the child PID into `cgroup.procs` in `pre_exec`, and sets
      `pids.max = max_procs`. Requires cgroup v2 (systemd default) and
      either root or a delegated `user@1000.service` subtree.
  - Files: `src/plugins/runner.rs` (`sandbox_pre_exec`), `src/plugins/supervisor.rs`.
  - Acceptance: a plugin with `max_procs: 64` runs on a host whose session
    already uses 700+ threads (currently a hard EAGAIN boundary); a
    thread-storm in one plugin does not consume another plugin's budget.
  - Done: per-plugin scopes land in the first writable ancestor that has
    the `pids` controller (root-first walk, falls back to RLIMIT_NPROC
    when none); scope reaped on exit. Covered by
    `tests/unit/test_supervisor.rs`: `pids_cgroup_accounts_plugin_threads_per_plugin`,
    `sandboxed_plugin_still_joins_its_pids_cgroup` (join from inside the
    user namespace), `thread_storm_in_one_plugin_does_not_consume_another_budget`.

- [ ] R9-02 — **PID-namespace isolation via shim supervisor:** the current
      spawn path cannot combine `unshare(CLONE_NEWPID)` with threading —
      the exec'd plugin would inherit a pending `pid_for_children`
      namespace and every thread spawn fails with EINVAL (documented in
      `runner.rs`). Correct approach: a shim process breaks the coupling —
      kernel forks a tiny wrapper, the wrapper unshares `CLONE_NEWPID` and
      forks the plugin (which is *born* into the namespace as PID 1, where
      threads work), then forwards signals and exit status. Effect: the
      plugin sees only its own processes (`/proc` shows just itself), and
      can no longer enumerate or signal host/other-plugin processes.
  - Files: new `src/plugins/shim.rs`, `src/plugins/supervisor.rs`.
  - Acceptance: a plugin's `/proc` lists only its own tasks; `ps` inside
    the plugin shows one process; supervisor signal/exit forwarding still
    works (restart, SIGTERM shutdown).

- [ ] R9-03 — **Filesystem isolation (Landlock LSM first, minimal rootfs
      later):** plugins currently read/write the host filesystem with the
      real uid's credentials. Landlock (Linux 5.13+, unprivileged,
      `no_new_privs`-compatible) lets `pre_exec` restrict file access
      declaratively — read-only access to declared dirs, write access only
      to the plugin's data dir — with no CAP_SYS_ADMIN requirement.
      Heavier alternative if stricter containment is needed: mount
      namespace + `chroot`/`pivot_root` into a minimal rootfs (plugin
      binary + runtime libs + data dir only), which the userns already
      grants CAP_SYS_ADMIN for.
  - Files: `src/plugins/runner.rs`, config schema (`max_fs_access` /
    `readonly_paths` / `writable_paths`).
  - Acceptance: a plugin denied read access to `~` gets EACCES on open;
    plugin writes are confined to its declared writable dirs.

- [ ] R9-04 — **seccomp syscall filter in `pre_exec`:** default-deny
      allowlist (or a tight denylist of kernel-escape-capable syscalls —
      `ptrace`, `bpf`, `keyctl`, `reboot`, `kexec_load`, `mount`/`umount`
      while R9-03 is unlanded) applied before exec, so a compromised
      plugin cannot use exotic syscalls to attack the kernel. Needs a
      runtime profiling pass per SDK (tokio/Python/CPP baseline) so the
      allowlist doesn't break legitimate plugins.
  - Files: `src/plugins/runner.rs`.
  - Acceptance: a plugin calling a denied syscall gets `EPERM`/`SIGSYS`;
      all three SDK example plugins still pass their integration tests.

- [ ] R9-05 — **Interim process-visibility hardening (until R9-02):** in a
      new mount namespace (`CLONE_NEWNS`, permitted inside the userns),
      remount `/proc` with `hidepid=2` so plugins cannot read other
      processes' cmdlines/environ while the shared-PID-space limitation is
      still in force.
  - Files: `src/plugins/runner.rs`.
  - Acceptance: `/proc/<other-pid>/` resolves to ENOENT for non-namespace
      processes; plugin functionality unaffected.

- [ ] R9-06 — **Docs: fix stale isolation references + record exact rlimit
      semantics:** README §5's "tracked in `AUDIT.md`" for the shim
      supervisor is stale — the item lives here (Phase 9), fix the pointer.
      Document that `max_procs` is a *shared real-uid* budget, not
      per-plugin (RLIMIT_NPROC is checked per fork/clone against the
      real-uid thread count and the check walks the userns tree up to
      `init_user_ns`), and that the sandbox does not yet hide host
      processes or restrict file access. Fold R9-01's cgroup migration in
      as the eventual correct accounting.
  - Files: `README.md` §5, `src/plugins/runner.rs` doc comments,
    `config.yaml` (Veyron repo) `max_procs` comment.
  - Acceptance: no stale `AUDIT.md` pointers; README accurately states
    what the sandbox does and does not isolate today.

---

## Phase 10 — Plugin config & marketplace state (deferred)

Deferred until the R8/N items ship. Today a plugin's runtime settings live
inline in `config.yaml` (`plugins:` list), the installer edits that one shared
file with marker-comment blocks (`# veyron install:` /
`append_config_example` / `remove_config_example`), and marketplace state is
split between the TTL `registry.json` cache and whatever happens to exist on
disk under `~/.local/lib/veyron/plugins/`. Fine for 5 plugins, the bottleneck
once the fleet grows: one shared file owned by nobody in particular, text-block
surgery, and no record of what is installed, from where, or at what version.
Phase 10 gives each plugin its own config file and gives the marketplace an
explicit state store. Independent of Phase 9 — can land before or after it.

- [ ] R10-01 — **Per-plugin config via `plugins.d/` drop-in directory:** the
      `plugins:` list leaves `config.yaml` for a `plugins.d/*.yaml` directory
      (new `plugins_dir` config key, default `<config_dir>/plugins.d/`); each
      file carries exactly one plugin entry (id, binary, restart, sandbox,
      limits, env). `load_config` globs and merges the files — order = filename
      sort, duplicate `id` across files = boot error, SIGHUP reload merges the
      same way. `vyn plugin install` writes `plugins.d/<slug>.yaml`,
      `vyn plugin remove` deletes it — the marker-block machinery
      (`append_config_example`/`remove_config_example`) is deleted with them.
  - Files: `src/utils/config.rs`, `src/marketplace/installer.rs`,
    `src/cli/plugin.rs`, `config.yaml` (Veyron repo).
  - Acceptance: install/remove never touch `config.yaml`; a hand-written
    entry in `plugins.d/` boots; duplicate ids across files fail loudly.

- [ ] R10-02 — **Explicit installed-plugin state store:** replace
      filesystem-sniffing `~/.local/lib/veyron/plugins/<slug>` with
      `~/.local/share/veyron/installed.json` recording slug, version, sha256,
      install time, source registry URL. Enables `vyn plugin list --installed`
      offline (no registry fetch), upgrade detection (installed vs registry
      version), and a `remove` that works even when the plugin dir is missing
      or half-deleted.
  - Files: `src/marketplace/installer.rs` (new state module),
    `src/cli/plugin.rs`.
  - Acceptance: `vyn plugin list --installed` shows versions offline;
    reinstalling the same version warns instead of re-extracting; remove
    tolerates a missing dir.

- [ ] R10-03 — **`registry.json` cache rework:** the TTL cache at
      `~/.cache/veyron/registry.json` is a raw mirror of the remote registry
      document. Move it under the marketplace state dir, version the schema,
      persist per-plugin `installed_version`/`last_check`, and make
      signature/revocation handling explicit (stale entry policy decided and
      tested) instead of implicit TTL-only expiry.
  - Files: `src/marketplace/registry.rs`, `src/marketplace/state.rs` (new).
  - Acceptance: cache file carries a schema version; revoked-entry handling
    is explicit and covered by tests.

- [ ] R10-04 — **`vyn plugin enable|disable <slug>`:** toggling a per-plugin
      file (rename/comment-out) replaces hand-editing `config.yaml` when an
      operator wants a plugin kept on disk but not auto-spawned — today that
      means uncommenting/commenting the installer's block by hand, which
      `remove_config_example` then deliberately refuses to touch.
  - Files: `src/cli/plugin.rs`, `src/plugins/loader.rs`.
  - Acceptance: `disable` stops auto-spawn on boot without uninstalling;
    `enable` restores it; the state survives SIGHUP reload.

---

## Phase 11 — Protocol v1.4: permission additions (deferred)

Deferred until the veyron-plugins fleet needs them — `secrets` is the first
plugin that cannot ship without one of these values, so P11 is the gate for
the whole next plugin batch. Tracked from the plugin side in
`veyron-plugins/ROADMAP.md` ("Kernel-side changes needed"); this section is
the kernel's half of the same work. Purely an enum addition + regeneration +
copy sync — no new Envelope payloads, no IPC/framing/orchestrator changes
(the existing `ActionRequest`/`Event`/`EventPublish`/IPC/streaming/WS
surfaces cover every planned plugin).

- [ ] P11-01 — **Add five `PermissionType` values (15–19, contiguous):**
  `PERMISSION_SECRETS` (15), `PERMISSION_CLIPBOARD` (16),
  `PERMISSION_LAUNCH` (17), `PERMISSION_SCREEN` (18), `PERMISSION_HOME`
  (19); bump the `// v 1.3` header to `// v 1.4`. Contiguity is
  load-bearing: `known_permissions()` (`src/marketplace/installer.rs:23`)
  probes enum codes and stops after 4 consecutive misses, so a gap ≥4
  silently rejects plugins declaring later values. Regenerate the
  `veyron-wire` prost types — `known_permissions()` (R8-01) and the JWT
  `permissions` claims (free-form strings) adopt the new values with no
  Rust source change. Bump `veyron-wire` to 0.3.0 and drop the crates-io
  patch override if one is in effect, per the R8-07 precedent.
  - Files: `wire/proto/veyron_protocol.proto`, `veyron-wire/` (regenerate
    + publish).
  - Acceptance: a plugin.json declaring `"secrets"` passes
    `validate_manifest`; R8-02's drift test stays green.

- [ ] P11-02 — **Sync all six proto copies (fixes pre-existing v1.2
  drift):** the three in-repo copies (`wire/proto`,
  `sdk/python/proto`, `sdk/cpp/proto`) are R8-05-guarded and move
  together; the standalone `veyron-sdk-python`/`veyron-sdk-cpp` repos are
  **already behind on v1.2** — missing `PERMISSION_EVENT_PUBLISH`,
  `PERMISSION_STORAGE`, the R6 streaming messages
  (`ActionRequestChunk`/`ActionResponseChunk`/`ActionStreamAbort`/
  `SessionClose`), `EventPublish*`, and `ActionRequest.caller_plugin_id` —
  and have no drift guard. Sync all six to v1.4 in one pass; extend R8-05
  (or add a release-time check) to cover the standalone copies so they
  can't drift again.
  - Files: all six `veyron_protocol.proto` copies,
    `tests/unit/test_proto_sync.rs`.
  - Acceptance: Python/C++ SDK examples exercise `publish_event` and
    storage-permission manifests against a v1.4 kernel.

- [ ] P11-03 — **Land M9 (zero-value enum renumber) on the same bump:**
  M9 is wire-breaking and gated on the next protocol version bump — this
  is that bump. Renumber `PERMISSION_UNKNOWN = 0` per the `AUDIT.md` plan
  while the enum is already being edited, and update
  `known_permissions()`'s exclusion accordingly.
  - Files: `wire/proto/veyron_protocol.proto` (+ all copies, same sync as
    P11-02).
  - Acceptance: no real permission value collides with `PERMISSION_UNKNOWN`;
    R8-02/R8-05 tests still pass.

---

## Cross-repo coordination

- **veyron-plugins** (`veyron-plugins/ROADMAP.md`): database plugin landing
  (R8-06) depends on R8-01/R8-02 landing here first — the kernel must accept
  `PERMISSION_STORAGE` before the registry entry is installable. The
  protocol v1.4 permission additions (Phase 11) are likewise tracked from
  the plugin side in its "Kernel-side changes needed" section — `secrets`
  is the first plugin blocked on P11.
- **veyron-wire** (`veyron-wire/`): 0.2.0 publish (R8-07) is release-process
  work; kernel changes here only consume it. P11-01 bumps it again to 0.3.0.
- **veyron-sdk-python / veyron-sdk-cpp** (standalone repos): proto copies
  have already drifted to v1.2 (no guard exists for them) — P11-02 syncs
  them to v1.4 and adds drift protection.
- **sdk/python/proto**, **sdk/cpp/proto** vendored copies: guarded by the R8-05
  drift test, which reads them via sibling-repo paths (`../veyron-sdk-python`,
  `../veyron-sdk-cpp`) after the submodule removal below.
- **Submodules removed (temporary decision, 2026-08-11):** `sdk/*` and `wire/`
  are no longer git submodules of this repo. The kernel consumes `veyron-wire`
  and `veyron-sdk` from crates.io; cross-SDK integration tests and the proto
  drift guard read sources from the sibling repos (`../veyron-wire`,
  `../veyron-sdk-cpp`, `../veyron-sdk-python`), which CI checks out itself.
  **Revisit in the future:** decide between a true monorepo, restored
  submodules, or published-artifact-only consumption — see the trade-offs
  (lockstep proto iteration vs. release cadence) recorded at removal time.

## Task Summary

| Item | Scope | Depends on |
|------|-------|------------|
| R8-01 | installer permissions derived from `PermissionType` enum | none |
| R8-02 | permission drift-detection tests | R8-01 |
| R8-03 | runtime `check_permission` normalization | none |
| R8-04 | registry schema doc alignment | none |
| R8-05 | proto-copy byte-identity test | none |
| R8-06 | `database` plugin landing (veyron-plugins) | R8-01, R8-02 |
| R8-07 | `veyron-wire` 0.2.0 publish + patch removal | none |
| N1 | router payload-sharing (`Arc<[u8]>`) in `forward`/`broadcast` — closed as non-issue; `Arc::ptr_eq` regression tests | none |
| N2 | permission form normalization in clamp + config cross-check — shipped, tests for both forms | none |
| N3 | config numeric bounds validation — shipped, zero-clamp + warn + tests | none |
| N4 | daemon-start readiness handshake (pid-file TOCTOU) — shipped, smoke-verified | none |
| N5 | `cargo fmt` fix for `test_proto_sync.rs` (DoD gate) — shipped, gate green | none |
| M7 | C++/Python framing fuzz harness (deferred) | none |
| M9 | zero-value enum renumber (deferred) | next protocol bump |
| R9-01 | cgroup v2 `pids.max` per-plugin accounting (replaces shared-uid RLIMIT_NPROC) | R8 + N ship gate |
| R9-02 | PID namespace via shim supervisor | R9-01 |
| R9-03 | filesystem isolation (Landlock / minimal rootfs) | R9-02 |
| R9-04 | seccomp syscall filter | R9-03 |
| R9-05 | `/proc` `hidepid=2` interim visibility hardening | R8 + N ship gate |
| R9-06 | docs: fix stale `AUDIT.md` pointer, record exact rlimit semantics | none |
| R10-01 | plugin settings out of `config.yaml` → `plugins.d/` drop-in dir | R8 + N ship gate |
| R10-02 | installed-plugin state store (`installed.json`) | none |
| R10-03 | `registry.json` cache rework (schema version, revocation policy) | R10-02 |
| R10-04 | `vyn plugin enable\|disable` toggle | R10-01 |
| P11-01 | protocol v1.4 — `PermissionType` additions 15–19 (`SECRETS`/`CLIPBOARD`/`LAUNCH`/`SCREEN`/`HOME`), header bump, wire regeneration | `secrets` plugin (veyron-plugins) needs it |
| P11-02 | proto-copy sync — 6 files / 4 repos; fixes standalone SDK v1.2 drift, adds drift guard | P11-01 |
| P11-03 | M9 zero-value enum renumber on the v1.4 bump (wire-breaking) | P11-01 |

**Ship gate:** R8-01..R8-05 are kernel-local and land together on `develop`;
R8-06/R8-07 are cross-repo coordination items shipped from their own repos.
The Immediate N1–N5 items shipped (2026-08-11) — all kernel-local, independent
of the cross-repo items; N5 restored the DoD `fmt` gate. Phase 9 is explicitly
deferred — no R9 item is scheduled until R8 ships. M7/M9 remain
deferred by decision. R9-01/R9-05 are Linux-cgroup/mount-namespace work and
require a delegated cgroup v2 subtree or root. Phase 10 (plugin config +
marketplace state) is likewise deferred and independent of Phase 9 — it can
land before or after hard isolation.

## Definition of Done

- `cargo test --all --all-features` exits 0; new behavior has regression tests.
- `cargo clippy --all-targets --all-features -- -D warnings` clean; `cargo fmt --check` clean.
- C++: existing CMake test targets stay green; new tests follow the
  `sdk/cpp/tests/test_*.cpp` naming/registration pattern in `CMakeLists.txt`.
- Python: new tests follow the `tests/python/test_*.py` pattern.
- Docs updated in the same PR (README for operator-visible changes; no
  `docs/FRAMING.md` changes expected since the wire format doesn't change).
