# Network Plugin Design

Date: 2026-07-05

## Goal

Give Veyron plugins/kernel a way to make outbound HTTP requests, via a
dedicated plugin (`network`) that declares `PERMISSION_NETWORK` and exposes
an `http_request` action. WebSocket support is deferred to a follow-up.

## Scope

- v1: HTTP only (GET/POST/PUT/DELETE/etc via one generic action).
- Out of scope for v1: WebSocket (ws_connect/ws_send/ws_close + event
  forwarding), any protocol beyond HTTP(S).

## Location

New crate: `veyron-plugins/plugins/network/`, mirroring the existing
`plugins/ping-pong-rs/` layout:

```
plugins/network/
  Cargo.toml       # depends on veyron-sdk + veyron via git, reqwest
  plugin.json      # registry metadata (permissions: ["PERMISSION_NETWORK"])
  src/
    main.rs        # NetworkPlugin (Plugin trait impl)
    ssrf.rs         # is_blocked_ip() guardrail
```

Binary name: `network`. Plugin id: `"network"`.

## Manifest

```rust
PluginManifest {
    permissions: vec!["PERMISSION_NETWORK".into()],
    actions: vec!["http_request".into()],
    events: vec![],
    ipc_targets: vec![],
    ..Default::default()
}
```

## `http_request` action

Request (`ActionRequest.params_json`, JSON):

```json
{
  "method": "GET",
  "url": "https://api.example.com/thing",
  "headers": { "Accept": "application/json" },
  "body": "optional string body",
  "timeout_ms": 5000
}
```

- `method` required, one of the standard HTTP verbs.
- `url` required, must parse and have scheme `http` or `https`.
- `headers` optional, string→string map.
- `body` optional, sent as-is (UTF-8 string); omitted for bodyless verbs.
- `timeout_ms` optional; capped at 30_000 (kernel's default action timeout).
  A caller may specify less, never more — a request-supplied value above the
  cap is clamped down, not rejected.

Response (`ActionResponse.data_json`, JSON) on success:

```json
{
  "status": 200,
  "headers": { "content-type": "application/json" },
  "body": "response body as UTF-8 (lossy)"
}
```

`ActionResponse.status = ACTION_OK`.

### Error handling

All failures (bad JSON, missing/invalid `url`, blocked scheme, blocked IP,
timeout, connection error, response body over the size cap) map to
`ActionStatus::ACTION_ERROR` with a human-readable message in
`ActionResponse.error`. `ACTION_PERMISSION_DENY` is not set by this plugin —
it's the kernel's status for routing a request to a plugin that hasn't
declared the required permission.

### Guardrails

- **Scheme allowlist:** only `http://` and `https://`. Anything else
  (`file://`, `ftp://`, ...) is rejected before any network I/O.
- **SSRF blocklist (`ssrf.rs::is_blocked_ip`):** resolve the request host,
  reject if any resolved IP falls in loopback, private (RFC1918), link-local,
  or cloud-metadata (`169.254.169.254`) ranges. This function's range list is
  a deliberate security decision left to the plugin author (scaffolded with
  signature + TODO; not fully authored by the assistant).
- **Response size cap:** 10 MiB. Bodies larger than this are truncated to an
  error (`ACTION_ERROR`, not a partial success) rather than silently
  truncated data.
- **Timeout cap:** 30s hard ceiling regardless of caller-requested
  `timeout_ms`.

### HTTP client

`reqwest` with `rustls-tls` (avoids an OpenSSL system dependency, keeping the
plugin a self-contained static-ish binary consistent with the other SDK
plugins).

## Operator note

This plugin needs actual network egress. In the kernel's `config.yaml`
`plugins:` entry for it, set `sandbox: false` — `sandbox: true` puts the
plugin in an isolated PID+net namespace with no route out (see
`src/plugins/runner.rs`), which would make every `http_request` fail.
Document this in the plugin's README.

## Testing

- Unit tests for `ssrf.rs::is_blocked_ip` (once authored): loopback, RFC1918
  ranges, link-local, metadata IP, and a couple of public IPs that must NOT
  be blocked.
- Unit test for JSON request parsing / validation (missing url, bad scheme,
  bad method).
- Integration-style test using a local mock HTTP server (e.g.
  `tokio::net::TcpListener` loopback, or a lightweight crate already used
  elsewhere in the repo if one exists) to exercise a full `http_request`
  round-trip through `on_message`, without needing a live kernel.

## Non-goals / follow-ups

- WebSocket actions (ws_connect/ws_send/ws_close, inbound messages as
  kernel Events) — separate design, needs a session/connection-id model and
  a custom serve loop (default `Plugin::serve()` doesn't support pushing
  unsolicited Events from a background task).
- Publishing to `registry.json` / building a release archive (`dist/`,
  `package.sh` flow) — left for whenever the plugin is ready to ship, not
  part of this initial implementation.
