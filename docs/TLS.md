# TLS for the gateway

The HTTP/WebSocket gateway (`/ws`, REST, `/devices/*`) is **TLS by default**
(D-07). Pick one of three setups; all of them give phones `wss://` without
anyone setting `tls: false` on the app side.

| Setup | When | Phone trusts the cert by |
|---|---|---|
| [Self-signed (default)](#1-self-signed-default) | LAN / Tailscale, no domain | pinning the exact cert shipped in the pairing QR |
| [Your own cert](#2-your-own-certificate) | you already have a cert/key (internal CA, wildcard) | pinning, or system roots if publicly trusted |
| [ACME via reverse proxy](#3-acme-lets-encrypt-via-a-reverse-proxy) | public domain, Let's Encrypt | system roots — no pinning needed |

Check what the kernel serves at any time:

```console
$ vyn tls status
tls: on
cert: /run/user/1000/vyn-tls/cert.pem (auto-generated self-signed)
sha256: B8:23:5C:…:97:20
```

The `sha256` line is the same format as
`openssl x509 -in cert.pem -noout -fingerprint -sha256`. `vyn device connect`
and `vyn device pair` print it next to the pairing link, so you can match it
against what the phone shows.

## 1. Self-signed (default)

Nothing to configure. With `tls: true` (the default) and neither
`tls_cert_path` nor `tls_key_path` set, the first `vyn start` generates an
ECDSA pair into `<private runtime dir>/vyn-tls/` (0700) and reuses it on
every restart.

Pairing carries the cert to the phone: the QR/link from `vyn device connect`
or `vyn device pair` includes `cert_pem`, and the app pins that exact
certificate for `wss://`. Pinning is what makes a self-signed cert safe here.
The phone does not trust "any cert for this host", only the one it scanned.

**Rotating** the self-signed cert (delete `vyn-tls/` and restart) changes the
fingerprint, so every paired phone must be re-paired. Do it deliberately, not
as a troubleshooting step.

## 2. Your own certificate

```yaml
tls: true
tls_cert_path: /etc/vyn/tls/cert.pem   # PEM, leaf first if it is a chain
tls_key_path:  /etc/vyn/tls/key.pem
```

Set **both or neither**. Setting only one is a boot error, because
half-configured TLS would otherwise silently downgrade. The pairing payload
carries the cert file as-is. `vyn tls status` fingerprints its first (leaf)
certificate, so keep the leaf first in a chain file.

## 3. ACME (Let's Encrypt) via a reverse proxy

The kernel does not speak ACME itself (dumb core: certificate lifecycle is
not kernel business). Let a proxy terminate TLS and forward to a
loopback-only plaintext gateway:

```yaml
# config.yaml
tls: false
bind: 127.0.0.1      # REQUIRED: with role: host + jwt_secret the default bind is 0.0.0.0
port: 8080
```

Caddy (obtains and renews certificates automatically):

```caddyfile
vyn.example.com {
    reverse_proxy 127.0.0.1:8080
}
```

nginx + certbot:

```nginx
server {
    listen 443 ssl;
    server_name vyn.example.com;
    ssl_certificate     /etc/letsencrypt/live/vyn.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/vyn.example.com/privkey.pem;

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;       # websocket upgrade for /ws
        proxy_set_header Connection "upgrade";
        proxy_set_header Sec-WebSocket-Protocol $http_sec_websocket_protocol;
        proxy_read_timeout 1h;                        # long-lived device sessions
    }
}
```

Pair with the public URL so the QR points at the proxy. An explicit
`wss://` (or `https://`) scheme is kept even though the kernel itself runs
`tls: false`:

```console
$ vyn device pair --host wss://vyn.example.com/ws
```

With `tls: false` the pairing payload carries no `cert_pem`, so the app falls
back to system roots, which is correct for a publicly-trusted certificate.

The proxy must not log request bodies. `POST /devices/consume` carries the
single-use pairing ticket in its body. Tickets are useless once consumed or
expired (default 5 min), but a body-logging proxy still widens the window.

## Why not `tls: false` on its own

`tls: false` without a TLS-terminating proxy sends JWTs, the pairing
response (`device_secret`) and every frame in cleartext. Per-frame HMAC stops
tampering but not reading. The only supported plaintext setups are the
loopback-only proxy above and local development.
