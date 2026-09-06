# Deployment and configuration

Graphite Meter runs as one static server binary with the browser client embedded. The default
configuration needs no file and starts a clear HTTP/1.1 service on port 7246. Add native TLS
listeners when you need deterministic HTTP/1.1 TLS, HTTP/2, HTTP/3, or WebTransport.

## Choose your deployment

| Your setup                       | Start here                                                              | What you need                                                |
| -------------------------------- | ----------------------------------------------------------------------- | ------------------------------------------------------------ |
| Local or trusted LAN             | [One container](#fast-local-deployment)                                 | TCP 7246.                                                    |
| Existing HTTPS ingress           | [Reverse proxy](#reverse-proxies) and [authentication](#authentication) | A hostname, TLS proxy, and its trusted CIDR.                 |
| Direct protocol comparisons      | [Compose with native TLS](#docker-compose)                              | A trusted certificate; TCP 7247–7249 and UDP 7249.           |
| systemd under your own user      | [Quadlet](../container/quadlet/README.md)                               | Linux, Podman, and systemd.                                  |
| Private tailnet                  | [Tailscale sidecar](../container/quadlet/tailscale-sidecar/README.md)   | Tailnet identity, HTTPS certificates, and access policy.     |
| Features in the current checkout | [Build from source](#build-from-source)                                 | Git and Docker Compose, or the pinned development toolchain. |

This guide covers **v0.8.0**. Read [upgrading to 0.8](#upgrading-to-08) when replacing an earlier deployment.
For reproducible deployments, replace `:latest` with `:0.8.0` or a verified image digest.

Jump to [server settings](#server-reference), [terminal flags](#native-terminal-client), or
[troubleshooting](#troubleshooting). Commands using `container/` paths run from the repository root.

## Fast local deployment

Run the published container and open <http://localhost:7246>:

```sh
docker run -d --name graphite-meter --restart unless-stopped \
  -p 7246:7246 ghcr.io/zr-jb/graphite-meter:latest
```

This clear HTTP deployment provides fetch-stream throughput and WebSocket latency. Browsers only
expose WebTransport in a secure context, so a remote deployment needs HTTPS plus the native
HTTP/3 listener for WebTransport latency and datagrams.

## Docker Compose

Clone the repository first if you have not already. The base file pulls the published image;
cloning alone does not build the current source:

```sh
git clone https://github.com/zR-JB/graphite-meter.git
cd graphite-meter
```

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

[The authentication overlay](../container/docker-compose.auth.yml) demonstrates password sign-in
behind an existing HTTPS reverse proxy:

1. Generate a hash using the [password command](#authentication) and save the single hash line to
   `/etc/graphite-meter/auth-password-hash` outside the repository.
2. Edit the overlay's canonical HTTPS URL, trusted proxy CIDR, and secret file path.
3. Configure [proxy forwarding and streaming](#reverse-proxies), then start both Compose files:

```sh
docker compose -f container/docker-compose.yml -f container/docker-compose.auth.yml up -d
```

Open the configured HTTPS URL and sign in. The overlay also documents OIDC and hybrid settings.

### Build from source

From a checkout of the revision you want to run:

```sh
git clone https://github.com/zR-JB/graphite-meter.git
cd graphite-meter
docker compose -f container/docker-compose.build.yml up --build -d
```

Open <http://localhost:7246>. If you already cloned the repository, run only the Compose command.
Stop an existing container bound to port 7246 before starting the source build.

Direct source images use development build identities. Official release automation supplies the
release version and exact source revision.

To build and run the embedded server directly instead of creating an image:

```sh
mise run server-build-prod
./go/graphite-meter
```

## Native listeners

Each native listener has a separate address and advertised public origin. Separate ports let a
client select a protocol deterministically instead of relying on negotiation.

| Listener         | Default  | Protocol and purpose                                                                                                                          |
| ---------------- | -------- | --------------------------------------------------------------------------------------------------------------------------------------------- |
| `GM_H1_ADDR`     | `:7246`  | Clear HTTP/1.1 UI, discovery, fetch transfers, progress, and WebSocket latency. Required.                                                     |
| `GM_H1_TLS_ADDR` | disabled | Dedicated HTTPS HTTP/1.1 fetch transfers and secure WebSocket latency.                                                                        |
| `GM_H2_ADDR`     | disabled | TLS listener restricted to HTTP/2 for fetch transfers and progress.                                                                           |
| `GM_H3_ADDR`     | disabled | HTTP/3 over UDP for probes, fetch transfers, progress, and WebTransport. A TCP socket on the same address serves the Alt-Svc bootstrap probe. |

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

| Environment               | Flag                     | Purpose                                                 |
| ------------------------- | ------------------------ | ------------------------------------------------------- |
| `GM_H1_PUBLIC_ORIGIN`     | `--h1-public-origin`     | Public `http://` origin of the clear HTTP/1.1 listener. |
| `GM_H1_TLS_PUBLIC_ORIGIN` | `--h1-tls-public-origin` | Public `https://` origin of the HTTP/1.1 TLS listener.  |
| `GM_H2_PUBLIC_ORIGIN`     | `--h2-public-origin`     | Public `https://` origin of the HTTP/2 listener.        |
| `GM_H3_PUBLIC_ORIGIN`     | `--h3-public-origin`     | Public `https://` origin of the HTTP/3 listener.        |

Public negotiated origins accept `self` or absolute HTTP/HTTPS origins:

| Environment                    | Flag                          | Advertised capability                   |
| ------------------------------ | ----------------------------- | --------------------------------------- |
| `GM_PUBLIC_ORIGINS`            | `--public-origins`            | Fetch throughput and WebSocket latency. |
| `GM_PUBLIC_THROUGHPUT_ORIGINS` | `--public-throughput-origins` | Fetch throughput only.                  |
| `GM_PUBLIC_LATENCY_ORIGINS`    | `--public-latency-origins`    | WebSocket latency only.                 |

An origin cannot be advertised as both deterministic native and negotiated. Use `self` for the
origin that served the current page.

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

Merge this location and the `map` into your existing HTTPS configuration; the snippet omits
certificate and TLS-listener setup. Request buffering and body-size limits must permit uploads.

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

        proxy_set_header Host $host;
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

## Authentication

Authentication is off by default. When enabled, it covers the UI, discovery, probes, transfers,
progress streams, WebSockets, and WebTransport sessions.

| Environment                       | Flag                             | Default    | Meaning                                                        |
| --------------------------------- | -------------------------------- | ---------- | -------------------------------------------------------------- |
| `GM_AUTH_MODE`                    | `--auth-mode`                    | `off`      | `off`, `password`, `oidc`, or `hybrid`.                        |
| `GM_AUTH_PUBLIC_URL`              | `--auth-public-url`              | empty      | Canonical HTTPS UI origin without a path or explicit `:443`.   |
| `GM_AUTH_PASSWORD_HASH`           | none                             | empty      | Inline Argon2id PHC hash. Prefer the file setting.             |
| `GM_AUTH_PASSWORD_HASH_FILE`      | `--auth-password-hash-file`      | empty      | File containing one Argon2id PHC hash.                         |
| `GM_AUTH_OIDC_ISSUER`             | `--auth-oidc-issuer`             | empty      | HTTPS OIDC issuer URL.                                         |
| `GM_AUTH_OIDC_CLIENT_ID`          | `--auth-oidc-client-id`          | empty      | Confidential client ID.                                        |
| `GM_AUTH_OIDC_CLIENT_SECRET`      | none                             | empty      | Inline client secret. Prefer the file setting.                 |
| `GM_AUTH_OIDC_CLIENT_SECRET_FILE` | `--auth-oidc-client-secret-file` | empty      | File containing the OIDC client secret.                        |
| `GM_AUTH_OIDC_ALLOWED_GROUPS`     | `--auth-oidc-allowed-groups`     | empty      | Required comma-separated, case-sensitive OIDC group allowlist. |
| `GM_AUTH_OIDC_PROVIDER_NAME`      | `--auth-oidc-provider-name`      | `Authelia` | Provider label shown on the sign-in page.                      |

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

## Podman and Quadlet

[The Quadlet guide](../container/quadlet/README.md) contains:

- a published-image unit;
- source-build units for Podman 5 or newer;
- a complete native TLS and Certbot DNS-01 deployment;
- a Tailscale sidecar deployment with no published host ports.

Rootless Podman limits host privilege. Its userspace networking can also limit measured
throughput. `Network=host` avoids that path and is useful for high-rate LAN testing, but it gives
up the container network namespace. Use it deliberately and apply host firewall policy directly.

## Native terminal client

Run `graphite-meter-client` with no arguments for an interactive test against
`http://127.0.0.1:7246`. The default stages are latency, download, and upload with loaded latency
enabled.

| Flag                       | Default                   | Meaning                                                               |
| -------------------------- | ------------------------- | --------------------------------------------------------------------- |
| `--url`                    | `http://127.0.0.1:7246`   | Origin of the operator server catalogue.                               |
| `--server <id>` | operator defaults | Repeatable server IDs; select one to four catalogue entries. |
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

## Upgrading to 0.8

Deploy the server and native client together and reload open browser tabs. Version 0.8
requires fresh upload checkpoints for coordinated measurements and an explicit destination
when minting socket tickets. The 0.7 handling-time probe frames remain in use; 0.6 peers
remain incompatible.

Existing deployments remain singleton by default. Add an [operator catalogue](SERVERS.md)
to offer additional servers; each instance keeps the same image and independent configuration.
No peer connectivity or health dependency is introduced at startup. The browser requires a
trusted HTTPS page to authorize protected remote servers.

New history uses schema version 4. Version 3 records remain readable with their original
singleton meaning; missing server identities and aggregation windows are not fabricated.
Older records remain in storage but are skipped as unsupported. Clearing history remains an
explicit action. See [measurement and history definitions](MEASUREMENTS.md#saved-history).

### Browser preferences

Version 0.8 reads current preference fields only. Obsolete cadence names,
`pingConcurrency`, `parallelStreams`, and old transport-role aliases are ignored;
missing or invalid values use current defaults. Target identifiers are preserved
as stored and resolved against current discovery. The browser storage key is
unchanged, so current preferences and theme choices remain available.

## Troubleshooting

| Symptom                                  | Check                                                                                                                        |
| ---------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| Another device cannot open the page      | Use the server IP rather than `localhost`; publish TCP 7246 and allow it through the host firewall.                          |
| WebTransport is unavailable              | Use an HTTPS page and trusted certificate; verify browser support, advertised H3 origin, and TCP/UDP reachability.           |
| An advertised path fails validation      | Public origins must be reachable from the client, with the correct scheme, port, and certificate hostname.                   |
| Uploads fail behind a proxy              | Remove request buffering and restrictive body-size limits; allow streaming progress and sufficiently long request lifetimes. |
| Throughput is lower than expected        | Check CPU, browser, Wi-Fi, proxy, and container networking; compare the native client and a direct listener.                 |
| Timeouts or a missing value appear       | Inspect the stage evidence. Unresolved probes and missing receiver counters are not zero-valued measurements.                |
| A client stopped working after upgrading | Match server and native-client versions; reload the browser to fetch the matching embedded UI.                               |

## Server reference

Environment variables load first. A supplied command-line flag overrides the corresponding
value. Inline authentication secrets intentionally have no flag so they do not appear in process
arguments.

### Identity and presentation

| Environment                 | Flag                       | Default          | Meaning                                                                   |
| --------------------------- | -------------------------- | ---------------- | ------------------------------------------------------------------------- |
| `GM_SERVER_NAME`            | `--name`                   | `graphite-meter` | Name reported by preflight and shown in clients.                          |
| `GM_SERVER_LOCATION`        | `--location`               | empty            | Optional operator-defined location label.                                 |
| `GM_SERVER_CATALOG` | — | empty | Inline JSON operator catalogue; mutually exclusive with the file option. |
| `GM_SERVER_CATALOG_FILE` | — | empty | Path to a read-only catalogue file, bounded to 64 KiB. |
| `GM_RESULT_HISTORY_DEFAULT` | `--result-history-default` | `false`          | Default browser preference for saving completed summaries on that device. |
| `GM_VERBOSE`                | `--verbose`                | `false`          | Log per-second measurement throughput.                                    |

The browser stores history in its own IndexedDB database. An explicit browser choice overrides
the operator default. Disabling saving retains existing records, and aborted or terminal-error
runs are not stored.

### Listener and endpoint settings

| Environment                      | Flag                            | Default |
| -------------------------------- | ------------------------------- | ------- |
| `GM_H1_ADDR`                     | `--h1-addr`                     | `:7246` |
| `GM_H1_TLS_ADDR`                 | `--h1-tls-addr`                 | empty   |
| `GM_H2_ADDR`                     | `--h2-addr`                     | empty   |
| `GM_H3_ADDR`                     | `--h3-addr`                     | empty   |
| `GM_TLS_CERT`                    | `--tls-cert`                    | empty   |
| `GM_TLS_KEY`                     | `--tls-key`                     | empty   |
| `GM_ADVERTISED_NATIVE_ENDPOINTS` | `--advertised-native-endpoints` | `all`   |

`GM_H1_ADDR` cannot be empty. Listener addresses must differ. Enabling any TLS listener requires
both TLS paths.

### Admission limits

| Environment                             | Flag                                   | Default | Meaning                                                   |
| --------------------------------------- | -------------------------------------- | ------- | --------------------------------------------------------- |
| `GM_MAX_ACTIVE_MEASUREMENTS`            | `--max-active-measurements`            | `256`   | Global measurement handlers.                              |
| `GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT` | `--max-active-measurements-per-client` | `32`    | Measurement handlers per client identity.                 |
| `GM_MAX_ACTIVE_SESSIONS`                | `--max-active-sessions`                | `64`    | WebTransport sessions within the global measurement pool. |
| `GM_MAX_SESSIONS_PER_CLIENT`            | `--max-sessions-per-client`            | `16`    | WebTransport transfer sessions per client identity.       |
| `GM_MAX_CONNECTIONS`                    | `--max-connections`                    | `512`   | Concurrent TCP and QUIC connections.                      |
| `GM_MAX_CONNECTIONS_PER_CLIENT`         | `--max-connections-per-client`         | `64`    | Connections per direct client.                            |
| `GM_MAX_OPERATION_DURATION`             | `--max-operation-duration`             | `5m`    | Maximum request-shaped measurement lifetime.              |
| `GM_MAX_SESSION_DURATION`               | `--max-session-duration`               | `2h`    | Maximum WebTransport session lifetime.                    |

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

Return to the [project overview](../README.md).
