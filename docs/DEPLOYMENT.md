# Deployment and configuration

One static server binary with the browser client embedded. With no configuration it serves clear HTTP/1.1 on port
7246. Native TLS listeners add deterministic HTTP/1.1 TLS, HTTP/2, HTTP/3 and WebTransport.

| Your setup | Start here | What you need |
| --- | --- | --- |
| Local or trusted LAN | [One container](#fast-local-deployment) | TCP 7246. |
| Existing HTTPS ingress | [Reverse proxy](#reverse-proxies) and [authentication](#authentication) | A hostname, TLS proxy and its CIDR. |
| Direct protocol comparisons | [Compose with native TLS](#native-tls) | A trusted certificate; TCP 7247–7249, UDP 7249. |
| systemd under your own user | [Quadlet](../container/quadlet/README.md) | Linux, Podman and systemd. |
| Private tailnet | [Tailscale sidecar](../container/quadlet/tailscale-sidecar/README.md) | Tailnet identity, HTTPS certificates, policy. |
| Current checkout | [Build from source](#build-from-source) | Git and Docker Compose, or the pinned toolchain. |

Pin `:X.Y.Z` or an image digest instead of `:latest` for reproducible deployments. Jump to the
[server reference](#server-reference), [terminal client](#native-terminal-client) or
[troubleshooting](#troubleshooting). `container/` paths are relative to the repository root.

## Fast local deployment

```sh
docker run -d --name graphite-meter --restart unless-stopped \
  -p 7246:7246 ghcr.io/zr-jb/graphite-meter:latest
```

Open <http://localhost:7246>. Clear HTTP gives fetch-stream throughput and WebSocket latency. Browsers expose
WebTransport only in a secure context, so remote WebTransport needs HTTPS and the native HTTP/3 listener.

## Docker Compose

The Compose files pull the published image; `docker-compose.build.yml` builds the checkout instead.

```sh
git clone https://github.com/zR-JB/graphite-meter.git
cd graphite-meter
docker compose -f container/docker-compose.yml up -d
```

### Native TLS

The TLS overlay does not issue certificates. Supply a Let's Encrypt-style tree whose `live/` entries link into the
same mounted `archive/` tree:

```sh
export GM_PUBLIC_HOST=meter.example.com GM_CERT_NAME=meter.example.com GM_CERTIFICATE_TREE=/etc/letsencrypt
docker compose -f container/docker-compose.yml -f container/docker-compose.tls.yml up -d
```

It publishes TCP 7247–7249 and UDP 7249 and mounts the tree read-only. For issuance and renewal, see the
[TLS Quadlet](../container/quadlet/graphite-meter-tls/README.md) (Cloudflare DNS-01).

### Authentication overlay

[`docker-compose.auth.yml`](../container/docker-compose.auth.yml) shows password sign-in behind an HTTPS proxy
(its comments cover OIDC and hybrid):

1. Save one [password hash](#authentication) line to `/etc/graphite-meter/auth-password-hash`.
2. Edit the overlay's public URL, trusted proxy CIDR and secret path.
3. Configure [proxy forwarding](#reverse-proxies), then start both files:

```sh
docker compose -f container/docker-compose.yml -f container/docker-compose.auth.yml up -d
```

### Build from source

```sh
docker compose -f container/docker-compose.build.yml up --build -d   # image from the checkout
mise run server-build-prod && ./go/graphite-meter                      # or the binary directly
```

Stop any container already bound to 7246 first. Source builds carry a development identity; release automation
stamps the version and source revision.

## Native listeners

Each listener has its own address and advertised origin, so a client can select a protocol deterministically.

| Listener | Default | Serves |
| --- | --- | --- |
| `GM_H1_ADDR` | `:7246` | Clear HTTP/1.1: UI, discovery, fetch transfers, progress, WebSocket latency. Required. |
| `GM_H1_TLS_ADDR` | disabled | HTTPS HTTP/1.1 fetch transfers and secure WebSocket latency. |
| `GM_H2_ADDR` | disabled | TLS restricted to HTTP/2: fetch transfers and progress. |
| `GM_H3_ADDR` | disabled | HTTP/3 over UDP: probes, fetch transfers, progress, WebTransport; TCP on the same address serves the Alt-Svc bootstrap. |

HTTP/3 needs TCP and UDP on its port end to end. All TLS listeners share `GM_TLS_CERT` and `GM_TLS_KEY`; a valid
replacement PEM pair is hot-reloaded, and an incomplete renewal keeps the previous pair.

```env
GM_H1_TLS_ADDR=:7247
GM_H2_ADDR=:7248
GM_H3_ADDR=:7249
GM_TLS_CERT=/etc/letsencrypt/live/meter.example.com/fullchain.pem
GM_TLS_KEY=/etc/letsencrypt/live/meter.example.com/privkey.pem
GM_H1_TLS_PUBLIC_ORIGIN=https://meter.example.com:7247
GM_H2_PUBLIC_ORIGIN=https://meter.example.com:7248
GM_H3_PUBLIC_ORIGIN=https://meter.example.com:7249
```

## Advertised measurement paths

`/preflight` lists the paths that carry throughput and latency:

- **Native endpoints** (`GM_ADVERTISED_NATIVE_ENDPOINTS`, `GM_H*_PUBLIC_ORIGIN`): Graphite Meter owns the listener, so
  the protocol is known. Only enabled listeners are advertised; each public origin must match its scheme.
- **Negotiated origins** (`GM_PUBLIC_ORIGINS` for both roles, `GM_PUBLIC_THROUGHPUT_ORIGINS`,
  `GM_PUBLIC_LATENCY_ORIGINS`): usually a reverse proxy. The browser reports the protocol it reached, the server what
  arrived upstream. `self` is the origin that served that server's discovery request.

An origin cannot be both native and negotiated. Clear HTTP loopback from an HTTPS page is browser-dependent; advertise
HTTPS paths for HTTPS deployments, including on `localhost`.

## Reverse proxies

A proxy adds a second protocol hop: the browser may reach the proxy over HTTP/2 or HTTP/3 while Graphite Meter sees
clear HTTP/1.1. Advertise it as a negotiated origin, alongside native endpoints if users should choose both:

```env
GM_ADVERTISED_NATIVE_ENDPOINTS=none
GM_PUBLIC_ORIGINS=self
GM_TRUSTED_PROXIES=172.30.0.0/24
```

WebTransport is HTTP/3 extended CONNECT over UDP, which a TCP proxy cannot carry; expose the native H3 endpoint
directly when it is required.

### nginx

Merge into your HTTPS server (certificate setup omitted):

```nginx
map $http_upgrade $connection_upgrade {
    default upgrade;
    ''      close;
}

server {
    location / {
        proxy_pass http://graphite-meter:7246;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection $connection_upgrade;

        proxy_set_header Host $http_host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header X-Forwarded-Host $http_host;
        proxy_set_header Forwarded "";
        proxy_set_header X-Forwarded-For "";

        proxy_buffering off;
        proxy_request_buffering off;
        proxy_read_timeout 300s;
        proxy_send_timeout 300s;
        client_max_body_size 0;
    }
}
```

### Nginx Proxy Manager

Tested with 2.15.1: create a Proxy Host with WebSocket support on and caching off, and put a complete
`location / { ... }` block with the nginx directives above in **Advanced**, using
`proxy_set_header Connection $http_connection;` (the `map` cannot go there). Add no `/` Custom Location; NPM then
omits its default location, whose `$host` would drop a nonstandard port. Check the generated file with `nginx -t`.

### Caddy

```caddyfile
meter.example {
    reverse_proxy graphite-meter:7246 {
        header_up X-Real-IP {remote_host}
        header_up -Forwarded
        header_up -X-Forwarded-For
    }
}
```

### Proxy requirements

- Preserve `Host`; overwrite `X-Forwarded-Proto` and `X-Forwarded-Host`; set `X-Real-IP` from the connection peer.
- Remove client-supplied `Forwarded` and `X-Forwarded-For`; set `GM_TRUSTED_PROXIES` to the proxy peers only.
- Allow WebSocket Upgrade to `/ws/ping`; do not buffer, cache, compress or transform `/upload/progress`.
- Expire idle upstream connections within 15 s; Graphite Meter closes them then.
- Keep the whole route family on one backend; do not add `forward_auth`; Graphite Meter owns authentication.
- Redact the `/auth/oidc/callback` query string from logs; keep bandwidth policy off measurement routes.

## Authentication

Off by default. When enabled it covers the UI, discovery, probes, transfers, progress, WebSockets and WebTransport;
settings are in the [server reference](#server-reference). Password mode needs one hash source; OIDC needs issuer,
client ID, one secret source and at least one allowed group; hybrid needs both and keeps the password usable when
the provider is down.

```sh
docker run --rm -it ghcr.io/zr-jb/graphite-meter:latest hash-password
```

Register `${GM_AUTH_PUBLIC_URL}/auth/oidc/callback` as a confidential authorization-code client with PKCE S256,
`client_secret_basic` and scopes `openid profile groups`. Browser sessions are HTTPS-only and last eight hours.
Advertised origins must use the authentication hostname (ports may differ); clear HTTP/1.1 cannot be advertised.

**Terminal clients** never see the operator password: the client shows a short code and an approval URL, and after
browser approval receives an in-memory, measurement-only grant bound to that session and HTTPS origin. Sign-out
revokes it. The client refuses authenticated operation over HTTP or with `--insecure`.

## Podman and Quadlet

[The Quadlet guide](../container/quadlet/README.md) has a published-image unit, source-build units (Podman 5+), a
[native TLS + Certbot DNS-01](../container/quadlet/graphite-meter-tls/README.md) deployment and a
[Tailscale sidecar](../container/quadlet/tailscale-sidecar/README.md). Rootless userspace networking can cap
throughput; `Network=host` avoids it but gives up the network namespace, so apply host firewall policy.

## Native terminal client

`graphite-meter-client` with no arguments tests `http://127.0.0.1:7246` interactively. Releases attach archives for
Linux and macOS (amd64/arm64) and Windows (amd64); the server ships as the container image or a
[source build](#build-from-source).

| Flag | Default | Meaning |
| --- | --- | --- |
| `--url` | `http://127.0.0.1:7246` | Origin of the operator server catalogue. |
| `--server <id>` | operator default | Repeatable; one to four catalogue IDs. |
| `--throughput-origin` / `--latency-origin` | `auto` | Discovered origin, or `auto`. |
| `--throughput-protocol` | `auto` | `http1`, `http2` or `http3` for a negotiated origin. |
| `--throughput-transport` | `auto` | `fetch-stream` or `webtransport`. |
| `--latency-transport` | `auto` | `websocket` or `webtransport`. |
| `--stages` | `latency,download,upload` | Comma-separated; add `bidirectional`. Unknown tokens are ignored. |
| `--warmup` | `800ms` | Per transfer stage, 0–4 s. |
| `--latency-duration` | `4s` | Measured windows, 500 ms–5 min. |
| `--download-/--upload-/--bidirectional-duration` | `10s` | |
| `--auto-streams` | `6` | Maximum HTTP/1.1 streams per direction. |
| `--streams` | `0` | Exact streams per server and direction; `0` keeps automatic. |
| `--ping` | `reply-driven` | Idle cadence: `reply-driven`, `fast` (80 ms), `medium` (250 ms), `slow` (600 ms) or a duration ≥ 80 ms. |
| `--loaded-ping` | `medium` | Cadence during transfers, same values. |
| `--loaded-latency` | `true` | Measure latency during transfer stages. |
| `--insecure` | `false` | Skip TLS verification; refuses sign-in. |
| `--report` | `false` | Run once without the interface; automatic when stdout is not a terminal. |
| `--version` / `--legal` | | Print the version or third-party notices and exit. |

Fixed cadences are capped at 15 s (half the server's idle bound). Headless runs print stage
progress to stderr and the plain report to stdout; an interactive run prints the same report on exit.

| Exit | Meaning |
| --- | --- |
| 0 | Complete, or quit before a run. |
| 1 | Any other outcome (Partial, Incomplete, Stopped, Failed) or a runtime error. |
| 2 | Invalid flags or arguments. |
| 130 / 143 | Stopped by SIGINT (or ctrl+c) / SIGTERM. |

Setup has the browser's Settings sections: **Connection paths**, **Duration & stages** and **Advanced**. `?` shows
the keys for the current screen.

| Key | Where | Action |
| --- | --- | --- |
| tab ←/→, ↑/↓, enter | setup | Section, row, change. |
| r | setup / finished | Start test / Run again. |
| v, s, u, a | setup | Recheck paths, choose servers, keep available servers, Automatic paths. |
| space, enter, esc | server chooser | Toggle, apply, cancel. |
| esc | running / finished | Stop test (asks to confirm) / back to setup. |
| d, l | running / finished | Details (servers, intervals, failures); rotate the latency server. |
| ↑/↓, pgup/pgdn, home/end | any | Scroll the body. |
| q, ctrl+c | any | Quit. |

## Upgrading

Upgrade the server and native clients together and reload open tabs; mixed versions are refused by the wire and
discovery contracts. Existing deployments stay single-server until you add a [catalogue](SERVERS.md). Browser
history reads [schema 4](MEASUREMENTS.md#saved-history) only; older records stay in storage but are skipped. Unknown
or obsolete browser preferences fall back to defaults.

## Troubleshooting

| Symptom | Check |
| --- | --- |
| Another device cannot open the page | Use the server IP, not `localhost`; publish and allow TCP 7246. |
| WebTransport is unavailable | HTTPS page, trusted certificate, browser support, advertised H3 origin, TCP+UDP reachability. |
| An advertised path fails validation | The origin must be reachable with the right scheme, port and certificate hostname. |
| A local peer fails only from a hosted page | Local-network permission and HTTPS; see [browser reachability](SERVERS.md#local-network-browser-permission). |
| A browser IPv6 peer needs a hostname | Use a DNS name for IPv6 outside the page's origin; the native client accepts literals. |
| Uploads fail behind a proxy | Disable request buffering and body-size limits; allow streaming progress and long requests. |
| Throughput is lower than expected | CPU, browser, Wi-Fi, proxy and container networking; compare the native client on a direct listener. |
| Timeouts or "—" appear | Inspect stage evidence: unresolved probes and missing receiver counters are not zero. |
| A client stopped working after upgrading | Match server and client versions; reload the browser. |

## Server reference

Environment loads first; a flag overrides it. `graphite-meter -h` lists every flag with its variable. Rows marked
*env only* have no flag, so secrets stay out of process arguments.

| Environment | Flag | Default | Meaning |
| --- | --- | --- | --- |
| `GM_H1_ADDR` | `--h1-addr` | `:7246` | Clear HTTP/1.1 listen address; required. |
| `GM_H1_TLS_ADDR` | `--h1-tls-addr` | empty | HTTPS HTTP/1.1 address; empty disables. |
| `GM_H2_ADDR` | `--h2-addr` | empty | HTTP/2 TLS address; empty disables. |
| `GM_H3_ADDR` | `--h3-addr` | empty | HTTP/3 UDP and bootstrap TCP address; empty disables. |
| `GM_TLS_CERT` / `GM_TLS_KEY` | `--tls-cert` / `--tls-key` | empty | PEM paths; required when any TLS listener is enabled. |
| `GM_H1_PUBLIC_ORIGIN` | `--h1-public-origin` | empty | Public `http://` origin of the clear listener. |
| `GM_H1_TLS_PUBLIC_ORIGIN` | `--h1-tls-public-origin` | empty | Public `https://` origin of the HTTPS HTTP/1.1 listener. |
| `GM_H2_PUBLIC_ORIGIN` | `--h2-public-origin` | empty | Public `https://` origin of the HTTP/2 listener. |
| `GM_H3_PUBLIC_ORIGIN` | `--h3-public-origin` | empty | Public `https://` origin of the HTTP/3 listener. |
| `GM_ADVERTISED_NATIVE_ENDPOINTS` | `--advertised-native-endpoints` | `all` | `all`, `none` or a subset of `http1-clear,http1-tls,http2,http3`. |
| `GM_PUBLIC_ORIGINS` | `--public-origins` | empty | Negotiated origins (or `self`) for throughput and latency. |
| `GM_PUBLIC_THROUGHPUT_ORIGINS` | `--public-throughput-origins` | empty | Negotiated throughput-only origins. |
| `GM_PUBLIC_LATENCY_ORIGINS` | `--public-latency-origins` | empty | WebSocket latency-only origins. |
| `GM_SERVER_NAME` | `--name` | `graphite-meter` | Name in `/preflight` and clients. |
| `GM_SERVER_LOCATION` | `--location` | empty | Location label. |
| `GM_RESULT_HISTORY_DEFAULT` | `--result-history-default` | `false` | Default for saving completed browser results on the device. |
| `GM_VERBOSE` | `--verbose` | `false` | Log per-second throughput. |
| `GM_MAX_ACTIVE_MEASUREMENTS` | `--max-active-measurements` | `256` | Concurrent measurement handlers. |
| `GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT` | `--max-active-measurements-per-client` | `32` | Handlers per client identity. |
| `GM_MAX_ACTIVE_SESSIONS` | `--max-active-sessions` | `64` | WebTransport sessions, a share of the handler pool. |
| `GM_MAX_SESSIONS_PER_CLIENT` | `--max-sessions-per-client` | `8` | WebTransport sessions per client identity. |
| `GM_MAX_CONNECTIONS` | `--max-connections` | `512` | Concurrent TCP and QUIC connections. |
| `GM_MAX_CONNECTIONS_PER_CLIENT` | `--max-connections-per-client` | `64` | Connections per direct client. |
| `GM_MAX_OPERATION_DURATION` | `--max-operation-duration` | `5m` | Request-shaped measurement lifetime. |
| `GM_MAX_SESSION_DURATION` | `--max-session-duration` | `2h` | WebTransport transfer session lifetime. |
| `GM_TRUSTED_PROXIES` | *env only* | empty | Proxy CIDRs allowed to supply `X-Real-IP`. |
| `GM_AUTH_MODE` | `--auth-mode` | `off` | `off`, `password`, `oidc` or `hybrid`. |
| `GM_AUTH_PUBLIC_URL` | `--auth-public-url` | empty | Canonical HTTPS UI origin, no path or `:443`. |
| `GM_AUTH_PASSWORD_HASH` | *env only* | empty | Inline Argon2id PHC hash; prefer the file. |
| `GM_AUTH_PASSWORD_HASH_FILE` | `--auth-password-hash-file` | empty | File with one Argon2id PHC hash. |
| `GM_AUTH_OIDC_ISSUER` | `--auth-oidc-issuer` | empty | HTTPS issuer URL. |
| `GM_AUTH_OIDC_CLIENT_ID` | `--auth-oidc-client-id` | empty | Confidential client ID. |
| `GM_AUTH_OIDC_CLIENT_SECRET` | *env only* | empty | Inline client secret; prefer the file. |
| `GM_AUTH_OIDC_CLIENT_SECRET_FILE` | `--auth-oidc-client-secret-file` | empty | File with the client secret. |
| `GM_AUTH_OIDC_ALLOWED_GROUPS` | `--auth-oidc-allowed-groups` | empty | Required comma-separated, case-sensitive groups. |
| `GM_AUTH_OIDC_PROVIDER_NAME` | `--auth-oidc-provider-name` | `Authelia` | Sign-in page label, ≤ 64 bytes. |
| `GM_SERVER_CATALOG` | *env only* | empty | [Server catalogue](SERVERS.md#operator-catalogue) JSON. |
| `GM_SERVER_CATALOG_FILE` | *env only* | empty | Absolute catalogue file path without `..` (≤ 64 KiB); exclusive with the inline form. |

- Listener addresses must differ. Numeric limits are positive, per-client limits ≤ their global limit, sessions ≤
  handlers, and session duration ≥ operation duration.
- A client identity is a login or measurement grant, whose subject shares twice its limit (every password login is
  the one operator subject); otherwise an IPv4 address or IPv6 /64 whose /56 and /48 share two and four times its
  limit. Upload receivers are counted the same way, and connections by address.
- A direct client address holds at most 8 QUIC connections and a browser opens one per WebTransport session, so a
  larger per-client session share only helps a login that spans addresses.
- Graphite Meter never throttles measured traffic. Public deployments need authentication or connection policy at a
  trusted proxy or firewall.
- `GM_TRUSTED_PROXIES` rejects default routes (`0.0.0.0/0`, `::/0`). A trusted peer names its client with exactly one
  `X-Real-IP`; a missing or repeated header, or one with `Forwarded`/`X-Forwarded-For`, is refused by sign-in and
  measurement admission.
- The browser keeps history in its own IndexedDB; its own choice overrides `GM_RESULT_HISTORY_DEFAULT`. Stopped and
  Failed runs are not saved.
