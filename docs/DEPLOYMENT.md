# Deployment and configuration

One static server binary with the browser client embedded. With no configuration it serves clear HTTP/1.1 on port 7246. Native TLS listeners add deterministic HTTP/1.1 TLS, HTTP/2, HTTP/3 and WebTransport.

| Your setup                  | Start here                                                              | What you need                                    |
| --------------------------- | ----------------------------------------------------------------------- | ------------------------------------------------ |
| Local or trusted LAN        | [One container](#fast-local-deployment)                                 | TCP 7246.                                        |
| Existing HTTPS ingress      | [Reverse proxy](#reverse-proxies) and [authentication](#authentication) | A hostname, TLS proxy and its CIDR.              |
| Direct protocol comparisons | [Compose with native TLS](#native-tls)                                  | A trusted certificate; TCP 7247–7249, UDP 7249.  |
| systemd under your own user | [Quadlet](../container/quadlet/README.md)                               | Linux, Podman and systemd.                       |
| Private tailnet             | [Tailscale sidecar](../container/quadlet/tailscale-sidecar/README.md)   | Tailnet identity, HTTPS certificates, policy.    |
| Current checkout            | [Build from source](#build-from-source)                                 | Git and Docker Compose, or the pinned toolchain. |

Pin `:X.Y.Z` or an image digest instead of `:latest` for reproducible deployments. Jump to the
[server reference](#server-reference), [terminal client](#native-terminal-client) or
[troubleshooting](#troubleshooting). `container/` paths are relative to the repository root.

## Fast local deployment

Start the [quick start](../README.md#quick-start) container and open <http://localhost:7246>. Clear HTTP gives
fetch-stream throughput and WebSocket latency. Browsers expose WebTransport only in a secure context, so remote
WebTransport needs HTTPS and the native HTTP/3 listener.

## Docker Compose

The Compose files pull the published image; the `docker-compose.build.yml` overlay builds the checkout instead.

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

It publishes TCP 7247–7249 and UDP 7249 and mounts the tree read-only; the key must be readable by the
[container user](#container-user). For issuance and renewal, see the
[TLS Quadlet](../container/quadlet/graphite-meter-tls/README.md) (Cloudflare DNS-01).

### Authentication overlay

[`docker-compose.auth.yml`](../container/docker-compose.auth.yml) shows password sign-in behind an HTTPS proxy on
the host (its comments cover OIDC, hybrid and a proxy container). It needs Compose 2.24.4 or later, publishes 7246
on `127.0.0.1` only and trusts only the network gateway the proxy's connections arrive from; stack no public ports
on it, because loopback and IPv6 clients of a published port arrive from that gateway too.

1. Save one [password hash](#authentication) line to `/etc/graphite-meter/auth-password-hash`, owned by the
   [container user](#container-user).
2. Edit the overlay's public URL and secret path.
3. Configure [proxy forwarding](#reverse-proxies), then start both files:

```sh
docker compose -f container/docker-compose.yml -f container/docker-compose.auth.yml up -d
```

### Build from source

```sh
docker compose -f container/docker-compose.yml -f container/docker-compose.build.yml up --build -d  # image
mise run server-build-prod && ./go/graphite-meter                                                  # binary
```

Stop any container already bound to 7246 first. Source builds carry a development identity; release automation
stamps the version and source revision.

### Container user

The image runs as the unprivileged user `65532:65532` and writes nothing. Mounted keys and secrets must be readable
by it:

- **Docker native TLS**: certbot keys are root `0600`; give the group read once and certbot keeps it on renewal.

  ```sh
  sudo chgrp -R 65532 /etc/letsencrypt/live /etc/letsencrypt/archive
  sudo chmod -R g+rX /etc/letsencrypt/live /etc/letsencrypt/archive
  ```

- **Docker secrets**: `sudo chown 65532:65532 FILE && sudo chmod 0400 FILE`.
- **Quadlet**: nothing. Podman secrets are world-readable inside the container, and the TLS and Tailscale units
  map your user, which owns their keys, to the image user with `UserNS=keep-id:uid=65532,gid=65532`.

Rootful Docker gives container root the host's root. _Rootless Podman already maps root to my user, so why a
non-root user?_ Defence in depth: an escape from the default unit lands on a subordinate UID with no access to your
files; the keep-id units run as your user, as root did before.

## Native listeners

Each listener has its own address and advertised origin, so a client can select a protocol deterministically. The
clear HTTP/1.1 listener (`GM_H1_ADDR`) also serves the UI and discovery and is required; the others are off until
given an address ([server reference](#server-reference)). HTTP/3 serves WebTransport and needs TCP (the Alt-Svc
bootstrap) and UDP on its port end to end. All TLS listeners share `GM_TLS_CERT` and `GM_TLS_KEY`; a valid
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
  the protocol is known. Only enabled listeners can be advertised, and naming a disabled one is a startup error; each
  public origin must match its scheme.
- **Negotiated origins** (`GM_PUBLIC_ORIGINS` for both roles, `GM_PUBLIC_THROUGHPUT_ORIGINS`,
  `GM_PUBLIC_LATENCY_ORIGINS`): usually a reverse proxy. The browser reports the protocol it reached, the server what
  arrived upstream. `self` is the origin that served that server's discovery request.

An origin cannot be both native and negotiated, except as a latency-only origin. Clear HTTP loopback from an HTTPS
page is browser-dependent; advertise HTTPS paths for HTTPS deployments, including on `localhost`.

## Reverse proxies

A proxy adds a second protocol hop: the browser may reach the proxy over HTTP/2 or HTTP/3 while Graphite Meter sees
clear HTTP/1.1. Advertise it as a negotiated origin, alongside native endpoints if users should choose both:

```env
GM_ADVERTISED_NATIVE_ENDPOINTS=none
GM_PUBLIC_ORIGINS=self
GM_TRUSTED_PROXIES=172.30.0.2/32
```

`GM_TRUSTED_PROXIES` is the proxy's own address as Graphite Meter sees it, here a proxy container with a fixed
address. A trusted peer names the client and, for authentication, the HTTPS origin, so trust no address that other
clients share: a published container port's loopback and IPv6 clients arrive from the network gateway.

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
- Remove client-supplied `Forwarded` and `X-Forwarded-For`; set `GM_TRUSTED_PROXIES` to the proxy peers only. Without
  it every client counts as the proxy, and with authentication no request counts as HTTPS, so sign-in fails.
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
`client_secret_basic` and scopes `openid profile groups`. OIDC mode refuses to start until issuer discovery succeeds;
hybrid retries it in the background. Browser sessions are HTTPS-only and last eight hours; a subject holds at most 8
and the server 1024.

Advertised origins must use the authentication hostname (ports may differ), and clear HTTP/1.1 cannot be advertised:
the default `GM_ADVERTISED_NATIVE_ENDPOINTS=all` includes it, so set `none` behind a proxy or
`http1-tls,http2,http3`, or the server refuses to start.

Sign-in is rate limited per client address (an IPv6 /64 whose /56 and /48 share two and four times the limit): 5
password attempts per minute, at most 60 wrong passwords per minute across all clients, and 10 OIDC code exchanges
and 10 sign-in approval pages per minute. A password sign-in also leaves a 30-day device cookie (signed with the
password hash, so changing the password forgets every device); a browser holding it keeps its own address's limit but
skips the bounds all clients share, so others' attempts cannot lock a known operator out.

**Terminal clients** never see the operator password: the client shows a short code and an approval URL, and after
browser approval receives an in-memory, measurement-only grant bound to that session and HTTPS origin. Sign-out
revokes it. The client refuses authenticated operation over HTTP or with `--insecure`, and signs in only in its
interactive interface: a headless run against a protected server exits 1 with "Sign-in required".

## Podman and Quadlet

See the [Quadlet guide](../container/quadlet/README.md). Rootless userspace networking can cap throughput;
`Network=host` avoids it but gives up the network namespace, so apply host firewall policy.

## Native terminal client

`graphite-meter-client` with no arguments tests `http://127.0.0.1:7246` interactively. Releases attach archives for
Linux and macOS (amd64/arm64) and Windows (amd64); the server ships as the container image or a
[source build](#build-from-source).

| Flag                                             | Default                   | Meaning                                                                                         |
| ------------------------------------------------ | ------------------------- | ----------------------------------------------------------------------------------------------- |
| `--url`                                          | `http://127.0.0.1:7246`   | Origin of the operator server catalogue.                                                        |
| `--server <id>`                                  | operator default          | Repeatable; one to four catalogue IDs.                                                          |
| `--throughput-origin` / `--latency-origin`       | `auto`                    | Discovered origin, or `auto`.                                                                   |
| `--throughput-protocol`                          | `auto`                    | `http1`, `http2` or `http3` for a negotiated origin.                                            |
| `--throughput-transport`                         | `auto`                    | `fetch-stream` or `webtransport`.                                                               |
| `--latency-transport`                            | `auto`                    | `websocket` or `webtransport`.                                                                  |
| `--stages`                                       | `latency,download,upload` | Comma-separated; add `bidirectional` (aliases `ping`, `down`, `up`, `bidi`).                    |
| `--warmup`                                       | `800ms`                   | Before every stage, 0–4 s; stretched to ten idle RTTs, at most 4 s.                             |
| `--latency-duration`                             | `4s`                      | Measured window, 1 s–24 h and within every selected server's limit (5 min unless raised).       |
| `--download-/--upload-/--bidirectional-duration` | `10s`                     | Same bounds.                                                                                    |
| `--auto-streams`                                 | `6`                       | Maximum HTTP/1.1 streams per direction, 1–14.                                                   |
| `--streams`                                      | `0`                       | Exact streams per server and direction, at most 14; `0` keeps automatic.                        |
| `--ping`                                         | `reply-driven`            | Idle cadence: `reply-driven`, `fast` (80 ms), `medium` (250 ms), `slow` (600 ms) or 80 ms–15 s. |
| `--loaded-ping`                                  | `medium`                  | Cadence during transfers, same values.                                                          |
| `--loaded-latency`                               | `true`                    | Measure latency during transfer stages.                                                         |
| `--insecure`                                     | `false`                   | Skip TLS verification; refuses sign-in.                                                         |
| `--report`                                       | `false`                   | Run once without the interface, and without sign-in; automatic when stdout is not a terminal.   |
| `--version` / `--legal`                          |                           | Print the version or third-party notices and exit.                                              |

Fixed cadences are capped at 15 s, half the server's idle bound. Headless runs print stage progress to stderr and
the report to stdout; an interactive run prints the same report on exit. It is plain text unless stdout is a
terminal and `NO_COLOR` is unset.

| Exit      | Meaning                                                                               |
| --------- | ------------------------------------------------------------------------------------- |
| 0         | Complete, or quit before a run.                                                       |
| 1         | Any other outcome (Partial, Incomplete, Stopped, Failed) or a runtime error.          |
| 2         | Invalid flags or arguments.                                                           |
| 130 / 143 | A run stopped by SIGINT (or ctrl+c) / SIGTERM; after a finished run they exit like q. |

Setup is one list: **Start test** (focused at launch), then connection paths, stages and a collapsed **Advanced**
group. The footer explains the focused row and its steps, then names what enter does; `?` shows every key for the
current screen. Every selected server is probed for latency; the run's latency is the first selected server's, and
if it leaves the test a surviving one takes over. `l` switches the server shown; the printed report keeps the run's.

| Key                       | Where              | Action                                                                                                                                                       |
| ------------------------- | ------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| ↑/↓ (k/j, tab), ←/→       | setup              | Move; change the focused value.                                                                                                                              |
| enter, space              | setup              | Start test on **Start test**, else open the row; space turns a stage on or off.                                                                              |
| r, v, s, u, a             | setup              | Start test, recheck paths, test servers, use available servers, automatic paths.                                                                             |
| ←/→, home/end, enter, esc | editing a value    | Move the cursor, apply, cancel.                                                                                                                              |
| space, enter, esc         | server chooser     | Select, apply, cancel.                                                                                                                                       |
| enter, space (o), esc     | sign-in            | Open the approval page, cancel.                                                                                                                              |
| esc                       | running            | Stop test; a second esc confirms.                                                                                                                            |
| enter (r), esc            | finished           | Run again; back to setup.                                                                                                                                    |
| d, l                      | running / finished | Details (servers, intervals, failures; esc closes); with several servers, the latency server.                                                                |
| ↑/↓, pgup/pgdn, home/end  | any                | Scroll the body.                                                                                                                                             |
| ?, q, ctrl+c              | any                | Keys for this screen; quit. While editing, ? and q are typed; ctrl+c quits. A running test stops first and prints its report; a second ctrl+c quits at once. |

## Upgrading

Upgrade the server and native clients together and reload open tabs; wire changes stay additive, and discovery
treats the engine version as metadata, not a compatibility test. Existing deployments stay single-server until you
add a [catalogue](SERVERS.md). Browser history saves [schema 5](MEASUREMENTS.md#saved-history) and still reads
schema 4; older records stay in storage but are skipped. Unknown or obsolete browser preferences fall back to
defaults.

The image now runs as [`65532:65532`](#container-user). Before pulling it, make Docker-mounted keys and secrets
readable by that user and reinstall the TLS and Tailscale Quadlet units; their old copies, auto-updated or not,
cannot read their keys.

## Troubleshooting

| Symptom                                    | Check                                                                                                        |
| ------------------------------------------ | ------------------------------------------------------------------------------------------------------------ |
| Another device cannot open the page        | Use the server IP, not `localhost`; publish and allow TCP 7246.                                              |
| WebTransport is unavailable                | HTTPS page, trusted certificate, browser support, advertised H3 origin, TCP+UDP reachability.                |
| An advertised path fails validation        | The origin must be reachable with the right scheme, port and certificate hostname.                           |
| A local peer fails only from a hosted page | Local-network permission and HTTPS; see [browser reachability](SERVERS.md#local-network-browser-permission). |
| A browser IPv6 peer needs a hostname       | Use a DNS name for IPv6 outside the page's origin; the native client accepts literals.                       |
| Uploads fail behind a proxy                | Disable request buffering and body-size limits; allow streaming progress and long requests.                  |
| Throughput is lower than expected          | CPU, browser, Wi-Fi, proxy and container networking; compare the native client on a direct listener.         |
| Timeouts or "—" appear                     | Inspect stage evidence: unfinished probes and missing receiver counters are not zero.                        |

## Server reference

Environment loads first; a flag overrides it. `graphite-meter -h` lists every flag with its variable. Rows marked
_env only_ have no flag, so secrets stay out of process arguments.

| Environment                             | Flag                                   | Default          | Meaning                                                                                |
| --------------------------------------- | -------------------------------------- | ---------------- | -------------------------------------------------------------------------------------- |
| `GM_H1_ADDR`                            | `--h1-addr`                            | `:7246`          | Clear HTTP/1.1 listen address; required.                                               |
| `GM_H1_TLS_ADDR`                        | `--h1-tls-addr`                        | empty            | HTTPS HTTP/1.1 address; empty disables.                                                |
| `GM_H2_ADDR`                            | `--h2-addr`                            | empty            | HTTP/2 TLS address; empty disables.                                                    |
| `GM_H3_ADDR`                            | `--h3-addr`                            | empty            | HTTP/3 UDP and bootstrap TCP address; empty disables.                                  |
| `GM_TLS_CERT` / `GM_TLS_KEY`            | `--tls-cert` / `--tls-key`             | empty            | PEM paths; required when any TLS listener is enabled.                                  |
| `GM_H1_PUBLIC_ORIGIN`                   | `--h1-public-origin`                   | empty            | Public `http://` origin of the clear listener.                                         |
| `GM_H1_TLS_PUBLIC_ORIGIN`               | `--h1-tls-public-origin`               | empty            | Public `https://` origin of the HTTPS HTTP/1.1 listener.                               |
| `GM_H2_PUBLIC_ORIGIN`                   | `--h2-public-origin`                   | empty            | Public `https://` origin of the HTTP/2 listener.                                       |
| `GM_H3_PUBLIC_ORIGIN`                   | `--h3-public-origin`                   | empty            | Public `https://` origin of the HTTP/3 listener.                                       |
| `GM_ADVERTISED_NATIVE_ENDPOINTS`        | `--advertised-native-endpoints`        | `all`            | `all`, `none` or a subset of `http1-clear,http1-tls,http2,http3`.                      |
| `GM_PUBLIC_ORIGINS`                     | `--public-origins`                     | empty            | Negotiated origins (or `self`) for throughput and latency.                             |
| `GM_PUBLIC_THROUGHPUT_ORIGINS`          | `--public-throughput-origins`          | empty            | Negotiated throughput-only origins.                                                    |
| `GM_PUBLIC_LATENCY_ORIGINS`             | `--public-latency-origins`             | empty            | WebSocket latency-only origins.                                                        |
| `GM_SERVER_NAME`                        | `--name`                               | `graphite-meter` | Name in `/preflight` and clients; at most 256 bytes, no control characters.            |
| `GM_SERVER_LOCATION`                    | `--location`                           | empty            | Location label, with the same limits.                                                  |
| `GM_RESULT_HISTORY_DEFAULT`             | `--result-history-default`             | `false`          | Default for saving completed browser results on the device.                            |
| `GM_VERBOSE`                            | `--verbose`                            | `false`          | Log per-second throughput, admission counters and authentication debug lines.          |
| `GM_MAX_ACTIVE_MEASUREMENTS`            | `--max-active-measurements`            | `256`            | Concurrent measurement handlers.                                                       |
| `GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT` | `--max-active-measurements-per-client` | `32`             | Handlers per client identity.                                                          |
| `GM_MAX_ACTIVE_SESSIONS`                | `--max-active-sessions`                | `64`             | WebTransport sessions, a share of the handler pool.                                    |
| `GM_MAX_SESSIONS_PER_CLIENT`            | `--max-sessions-per-client`            | `8`              | WebTransport sessions per client identity.                                             |
| `GM_MAX_CONNECTIONS`                    | `--max-connections`                    | `4096`           | Concurrent TCP and QUIC connections.                                                   |
| `GM_MAX_CONNECTIONS_PER_CLIENT`         | `--max-connections-per-client`         | `64`             | Connections per direct client.                                                         |
| `GM_MAX_OPERATION_DURATION`             | `--max-operation-duration`             | stage + 1m       | Request-shaped measurement lifetime; unset, at least `5m`.                             |
| `GM_MAX_SESSION_DURATION`               | `--max-session-duration`               | `2h`             | WebTransport session lifetime; unset, at least the operation's.                        |
| `GM_MAX_STAGE_DURATION`                 | `--max-stage-duration`                 | `5m`             | Longest stage clients may plan, `1s`–`24h`; sent in `/preflight`.                      |
| `GM_TRUSTED_PROXIES`                    | _env only_                             | empty            | Proxy CIDRs allowed to supply `X-Real-IP`, `X-Forwarded-Proto` and `X-Forwarded-Host`. |
| `GM_AUTH_MODE`                          | `--auth-mode`                          | `off`            | `off`, `password`, `oidc` or `hybrid`.                                                 |
| `GM_AUTH_PUBLIC_URL`                    | `--auth-public-url`                    | empty            | Canonical HTTPS UI origin, no path or `:443`.                                          |
| `GM_AUTH_PASSWORD_HASH`                 | _env only_                             | empty            | Inline Argon2id PHC hash; prefer the file.                                             |
| `GM_AUTH_PASSWORD_HASH_FILE`            | `--auth-password-hash-file`            | empty            | File with one Argon2id PHC hash.                                                       |
| `GM_AUTH_OIDC_ISSUER`                   | `--auth-oidc-issuer`                   | empty            | HTTPS issuer URL.                                                                      |
| `GM_AUTH_OIDC_CLIENT_ID`                | `--auth-oidc-client-id`                | empty            | Confidential client ID.                                                                |
| `GM_AUTH_OIDC_CLIENT_SECRET`            | _env only_                             | empty            | Inline client secret; prefer the file.                                                 |
| `GM_AUTH_OIDC_CLIENT_SECRET_FILE`       | `--auth-oidc-client-secret-file`       | empty            | File with the client secret.                                                           |
| `GM_AUTH_OIDC_ALLOWED_GROUPS`           | `--auth-oidc-allowed-groups`           | empty            | Required comma-separated, case-sensitive groups.                                       |
| `GM_AUTH_OIDC_PROVIDER_NAME`            | `--auth-oidc-provider-name`            | `Authelia`       | Sign-in page label, ≤ 64 bytes.                                                        |
| `GM_SERVER_CATALOG`                     | _env only_                             | empty            | [Server catalogue](SERVERS.md#operator-catalogue) JSON.                                |
| `GM_SERVER_CATALOG_FILE`                | _env only_                             | empty            | Absolute catalogue file path without `..`; exclusive with the inline form.             |

- Listener addresses must differ. Numeric limits are positive, per-client limits ≤ their global limit, sessions ≤
  handlers, and session duration ≥ operation duration. Lifetimes left unset cover the stage limit plus a minute, so a
  server allowing multi-hour stages never cuts their lanes on a short default; set them to keep lanes shorter, and
  clients reconnect across each cut. Something must carry throughput: with no native endpoint
  advertised, set `GM_PUBLIC_ORIGINS` or `GM_PUBLIC_THROUGHPUT_ORIGINS`, or startup fails with "configuration
  advertises no throughput endpoint".
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
