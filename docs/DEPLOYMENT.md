# Deployment and configuration

Graphite Meter runs as one static server binary with the browser client embedded. The default
configuration needs no file and starts a clear HTTP/1.1 service on port 7246. Add native TLS
listeners when you need deterministic HTTP/1.1 TLS, HTTP/2, HTTP/3, or WebTransport.

## Fast local deployment

Run the published container and open <http://localhost:7246>:

```sh
docker run -d --name graphite-meter -p 7246:7246 ghcr.io/zr-jb/graphite-meter:latest
```

This clear HTTP deployment provides fetch-stream throughput and WebSocket latency. Browsers only
expose WebTransport in a secure context, so a remote deployment needs HTTPS plus the native
HTTP/3 listener for WebTransport latency and datagrams.

Pin a release tag instead of `latest` when reproducibility matters.

## Native listeners

Each native listener has a separate address and advertised public origin. Separate ports let a
client select a protocol deterministically instead of relying on negotiation.

| Listener | Default | Protocol and purpose |
| --- | --- | --- |
| `GM_H1_ADDR` | `:7246` | Clear HTTP/1.1 UI, discovery, fetch transfers, progress, and WebSocket latency. Required. |
| `GM_H1_TLS_ADDR` | disabled | Dedicated HTTPS HTTP/1.1 fetch transfers and secure WebSocket latency. |
| `GM_H2_ADDR` | disabled | TLS listener restricted to HTTP/2 for fetch transfers and progress. |
| `GM_H3_ADDR` | disabled | HTTP/3 over UDP for probes, fetch transfers, progress, and WebTransport. A TCP socket on the same address serves the Alt-Svc bootstrap probe. |

The conventional native ports are 7246 through 7249. HTTP/3 needs both TCP and UDP port 7249
through the container, firewall, and network path. Every enabled TLS listener uses the certificate
and key configured by `GM_TLS_CERT` and `GM_TLS_KEY`.

Example native TLS environment:

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

The server hot-reloads a valid replacement PEM pair. It keeps using the previous certificate if a
renewal update is temporarily incomplete or invalid.

## Advertised measurement paths

`/preflight` tells clients which reachable paths can carry throughput and latency. There are two
path types:

- Native endpoints have a known protocol because Graphite Meter owns the listener.
- Public origins are negotiated paths, commonly served through a reverse proxy. The browser
  reports the protocol it reached while the server reports what arrived upstream.

`GM_ADVERTISED_NATIVE_ENDPOINTS` accepts `all`, `none`, or a comma-separated subset of
`http1-clear,http1-tls,http2,http3`. The default is `all`, but only enabled listeners are included.

Native public origins must match their protocol:

| Environment | Flag | Purpose |
| --- | --- | --- |
| `GM_H1_PUBLIC_ORIGIN` | `--h1-public-origin` | Public `http://` origin of the clear HTTP/1.1 listener. |
| `GM_H1_TLS_PUBLIC_ORIGIN` | `--h1-tls-public-origin` | Public `https://` origin of the HTTP/1.1 TLS listener. |
| `GM_H2_PUBLIC_ORIGIN` | `--h2-public-origin` | Public `https://` origin of the HTTP/2 listener. |
| `GM_H3_PUBLIC_ORIGIN` | `--h3-public-origin` | Public `https://` origin of the HTTP/3 listener. |

Public negotiated origins accept `self` or absolute HTTP/HTTPS origins:

| Environment | Flag | Advertised capability |
| --- | --- | --- |
| `GM_PUBLIC_ORIGINS` | `--public-origins` | Fetch throughput and WebSocket latency. |
| `GM_PUBLIC_THROUGHPUT_ORIGINS` | `--public-throughput-origins` | Fetch throughput only. |
| `GM_PUBLIC_LATENCY_ORIGINS` | `--public-latency-origins` | WebSocket latency only. |

An origin cannot be advertised as both deterministic native and negotiated. Use `self` for the
origin that served the current page.

## Server reference

Environment variables load first. A supplied command-line flag overrides the corresponding
value. Inline authentication secrets intentionally have no flag so they do not appear in process
arguments.

### Identity and presentation

| Environment | Flag | Default | Meaning |
| --- | --- | --- | --- |
| `GM_SERVER_NAME` | `--name` | `graphite-meter` | Name reported by preflight and shown in clients. |
| `GM_SERVER_LOCATION` | `--location` | empty | Optional operator-defined location label. |
| `GM_RESULT_HISTORY_DEFAULT` | `--result-history-default` | `false` | Default browser preference for saving completed summaries on that device. |
| `GM_VERBOSE` | `--verbose` | `false` | Log per-second measurement throughput. |

The browser stores history in its own IndexedDB database. An explicit browser choice overrides
the operator default. Disabling saving retains existing records, and aborted or terminal-error
runs are not stored.

### Listener and endpoint settings

| Environment | Flag | Default |
| --- | --- | --- |
| `GM_H1_ADDR` | `--h1-addr` | `:7246` |
| `GM_H1_TLS_ADDR` | `--h1-tls-addr` | empty |
| `GM_H2_ADDR` | `--h2-addr` | empty |
| `GM_H3_ADDR` | `--h3-addr` | empty |
| `GM_TLS_CERT` | `--tls-cert` | empty |
| `GM_TLS_KEY` | `--tls-key` | empty |
| `GM_ADVERTISED_NATIVE_ENDPOINTS` | `--advertised-native-endpoints` | `all` |

`GM_H1_ADDR` cannot be empty. Listener addresses must differ. Enabling any TLS listener requires
both TLS paths.

### Admission limits

| Environment | Flag | Default | Meaning |
| --- | --- | --- | --- |
| `GM_MAX_ACTIVE_MEASUREMENTS` | `--max-active-measurements` | `256` | Global measurement handlers. |
| `GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT` | `--max-active-measurements-per-client` | `32` | Measurement handlers per client identity. |
| `GM_MAX_ACTIVE_SESSIONS` | `--max-active-sessions` | `64` | WebTransport sessions within the global measurement pool. |
| `GM_MAX_SESSIONS_PER_CLIENT` | `--max-sessions-per-client` | `16` | WebTransport transfer sessions per client identity. |
| `GM_MAX_CONNECTIONS` | `--max-connections` | `512` | Concurrent TCP and QUIC connections. |
| `GM_MAX_CONNECTIONS_PER_CLIENT` | `--max-connections-per-client` | `64` | Connections per direct client. |
| `GM_MAX_OPERATION_DURATION` | `--max-operation-duration` | `5m` | Maximum request-shaped measurement lifetime. |
| `GM_MAX_SESSION_DURATION` | `--max-session-duration` | `2h` | Maximum WebTransport session lifetime. |

All numeric limits must be positive. Per-client limits cannot exceed their global limit. The
session pool is part of the measurement pool, and the session duration must be at least the
operation duration.

Graphite Meter does not throttle measurement bandwidth because doing so would change the result.
An internet-facing deployment should enable authentication or apply appropriate connection and
traffic policy at a trusted proxy or firewall.

### Trusted proxies

`GM_TRUSTED_PROXIES` has no CLI equivalent. It accepts comma-separated proxy CIDRs as Graphite
Meter sees them. Default routes such as `0.0.0.0/0` and `::/0` are rejected.

Only trusted peers may supply forwarding information used for client identity and admission
accounting. Configure the actual proxy network rather than a broad client network.

## Authentication

Authentication is off by default. When enabled, it covers the UI, discovery, probes, transfers,
progress streams, WebSockets, and WebTransport sessions.

| Environment | Flag | Default | Meaning |
| --- | --- | --- | --- |
| `GM_AUTH_MODE` | `--auth-mode` | `off` | `off`, `password`, `oidc`, or `hybrid`. |
| `GM_AUTH_PUBLIC_URL` | `--auth-public-url` | empty | Canonical HTTPS UI origin without a path or explicit `:443`. |
| `GM_AUTH_PASSWORD_HASH` | none | empty | Inline Argon2id PHC hash. Prefer the file setting. |
| `GM_AUTH_PASSWORD_HASH_FILE` | `--auth-password-hash-file` | empty | File containing one Argon2id PHC hash. |
| `GM_AUTH_OIDC_ISSUER` | `--auth-oidc-issuer` | empty | HTTPS OIDC issuer URL. |
| `GM_AUTH_OIDC_CLIENT_ID` | `--auth-oidc-client-id` | empty | Confidential client ID. |
| `GM_AUTH_OIDC_CLIENT_SECRET` | none | empty | Inline client secret. Prefer the file setting. |
| `GM_AUTH_OIDC_CLIENT_SECRET_FILE` | `--auth-oidc-client-secret-file` | empty | File containing the OIDC client secret. |
| `GM_AUTH_OIDC_ALLOWED_GROUPS` | `--auth-oidc-allowed-groups` | empty | Required comma-separated, case-sensitive OIDC group allowlist. |
| `GM_AUTH_OIDC_PROVIDER_NAME` | `--auth-oidc-provider-name` | `Authelia` | Provider label shown on the sign-in page. |

Create an operator password hash interactively:

```sh
docker run --rm -it ghcr.io/zr-jb/graphite-meter:latest hash-password
```

Password mode requires exactly one password-hash source. OIDC mode requires issuer, client ID,
one secret source, and at least one allowed group. Hybrid mode requires both complete methods and
keeps the operator password available if the identity provider cannot be reached.

Register `${GM_AUTH_PUBLIC_URL}/auth/oidc/callback` as a confidential authorization-code client
using PKCE S256, `client_secret_basic`, and the `openid profile groups` scopes.

Authenticated browser sessions are HTTPS-only, absolute eight-hour sessions. Advertised origins
must use the canonical authentication hostname, although their ports may differ. Clear HTTP/1.1
cannot be advertised while authentication is enabled.

### Terminal client authorization

The TUI does not request or store the operator password. It creates a verifier in memory and
shows a short code plus a browser approval URL. After the user confirms the matching code, the
client receives a measurement-only bearer grant bound to the approving browser session and exact
HTTPS origin. Closing the client discards the grant; browser sign-out revokes it.

The client refuses authenticated operation over HTTP and does not combine authentication with
`--insecure`.

## Docker Compose

The base file pulls the published image:

```sh
docker compose -f container/docker-compose.yml up -d
```

### Native TLS

The TLS overlay deliberately does not issue certificates. Supply an existing Let's Encrypt-style
tree whose `live/` entries link into the same mounted `archive/` tree:

```sh
export GM_PUBLIC_HOST=meter.example.com
export GM_CERT_NAME=meter.example.com
export GM_CERTIFICATE_TREE=/etc/letsencrypt
docker compose -f container/docker-compose.yml -f container/docker-compose.tls.yml up -d
```

This publishes TCP ports 7247, 7248, and 7249 plus UDP port 7249. The certificate tree is mounted
read-only. The rootless Quadlet example under `container/quadlet/graphite-meter-tls` provides a
complete Cloudflare DNS-01 issuance and renewal workflow.

### Authentication overlay

`container/docker-compose.auth.yml` demonstrates password authentication behind a TLS reverse
proxy. Create the hash file outside the repository, configure the canonical URL and trusted proxy
CIDR, then combine it with the base file. The overlay also documents the OIDC and hybrid settings.

### Build from source

```sh
docker compose -f container/docker-compose.build.yml up --build
```

Direct source images use development build identities. Official release automation supplies the
release version and exact source revision.

To build and run the embedded server directly instead of creating an image:

```sh
just server-build-prod
./go/graphite-meter
```

## Podman and Quadlet

`container/quadlet` contains:

- a published-image unit;
- source-build units for Podman 5 or newer;
- a complete native TLS and Certbot DNS-01 deployment;
- a Tailscale sidecar deployment with no published host ports.

Rootless Podman limits host privilege. Its userspace networking can also limit measured
throughput. `Network=host` avoids that path and is useful for high-rate LAN testing, but it gives
up the container network namespace. Use it deliberately and apply host firewall policy directly.

## Reverse proxies

A proxy creates two protocol hops:

```text
browser or TUI  <->  proxy  <->  Graphite Meter
```

The browser may negotiate HTTP/2 or HTTP/3 with the proxy while Graphite Meter receives clear
HTTP/1.1 upstream. Configure this as a negotiated public origin:

```env
GM_ADVERTISED_NATIVE_ENDPOINTS=none
GM_PUBLIC_ORIGINS=self
GM_TRUSTED_PROXIES=172.30.0.0/24
```

Use `GM_PUBLIC_THROUGHPUT_ORIGINS` or `GM_PUBLIC_LATENCY_ORIGINS` when an origin provides only one
role. Keep deterministic native endpoints advertised alongside the proxy origin when users should
be able to select both.

WebTransport is HTTP/3 extended CONNECT over UDP. A normal TCP reverse proxy cannot carry it.
Expose the native H3 endpoint separately when WebTransport is required. Without it, clients can
still use fetch throughput and WebSocket latency.

### nginx

```nginx
map $http_upgrade $connection_upgrade {
    default upgrade;
    ''      close;
}

server {
    # Configure listen and TLS as usual.

    location / {
        proxy_pass http://graphite-meter:7246;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection $connection_upgrade;

        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header X-Forwarded-Host $http_host;
        proxy_set_header Forwarded "";
        proxy_set_header X-Forwarded-For "";

        proxy_buffering off;
    }
}
```

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

- Preserve `Host` and overwrite `X-Forwarded-Proto` and `X-Forwarded-Host`.
- Set `X-Real-IP` from the proxy connection peer.
- Remove client-supplied `Forwarded` and `X-Forwarded-For` values.
- Allow WebSocket Upgrade to `/ws/ping`.
- Do not buffer, cache, compress, or transform `/upload/progress`.
- Set `GM_TRUSTED_PROXIES` only to the proxy peers.
- Redact the `/auth/oidc/callback` query string from logs.
- Do not add provider `forward_auth`; Graphite Meter owns its authentication boundary.
- Apply bandwidth policy outside measurement routes only when accurate throughput is required.

## Native terminal client

Run `graphite-meter-client` with no arguments for an interactive test against
`http://127.0.0.1:7246`. The default stages are latency, download, and upload with loaded latency
enabled.

| Flag                       | Default                   | Meaning                                                               |
| -------------------------- | ------------------------- | --------------------------------------------------------------------- |
| `--url`                    | `http://127.0.0.1:7246`   | Graphite Meter UI and discovery origin.                               |
| `--throughput-origin`      | `auto`                    | Discovered throughput origin or `auto`.                               |
| `--throughput-protocol`    | `auto`                    | `auto`, `http1`, `http2`, or `http3` for a negotiated origin.         |
| `--throughput-transport`   | `auto`                    | `auto`, `fetch-stream`, or `webtransport`.                            |
| `--latency-origin`         | `auto`                    | Discovered latency origin or `auto`.                                  |
| `--latency-transport`      | `auto`                    | `auto`, `websocket`, or `webtransport`.                               |
| `--stages`                 | `latency,download,upload` | Comma-separated stages, including `bidirectional`.                    |
| `--warmup`                 | `800ms`                   | Per-transfer-stage warmup.                                            |
| `--latency-duration`       | `4s`                      | Idle latency duration.                                                |
| `--download-duration`      | `10s`                     | Download duration.                                                    |
| `--upload-duration`        | `10s`                     | Upload duration.                                                      |
| `--bidirectional-duration` | `10s`                     | Bidirectional duration.                                               |
| `--auto-streams`           | `6`                       | Maximum automatic HTTP/1 streams per direction.                       |
| `--streams`                | `0`                       | Forced streams per active direction; zero keeps automatic policy.     |
| `--ping`                   | `medium`                  | `instant` (80 ms), `medium` (250 ms), `slow` (600 ms), or a duration. |
| `--loaded-latency`         | `true`                    | Measure latency while transfer stages run.                            |
| `--insecure`               | `false`                   | Skip TLS verification for unauthenticated testing.                    |
| `--version`                | `false`                   | Print the client version and exit.                                    |
| `--legal`                  | `false`                   | Print licenses and notices and exit.                                  |

WebTransport limits custom ping intervals to half the server session idle bound. Invalid stage
tokens are ignored; at least one valid stage must remain for a useful run.

Releases attach native client archives for Linux amd64/arm64, macOS amd64/arm64, and Windows
amd64. The server is distributed through the multi-architecture container image; a standalone
server binary can be built from source but is not attached to GitHub Releases.
