# Configuration

The server reads environment variables at startup; matching command-line flags override them.
Environment variables are the canonical interface for Docker, Compose, Quadlet, and Kubernetes.
This page lists every runtime option. Defaults need no configuration.

## Native listeners

A non-empty listener address enables that listener. There are no separate enable switches.

| Environment      | Flag           | Default | Meaning                                                     |
| ---------------- | -------------- | ------- | ----------------------------------------------------------- |
| `GM_H1_ADDR`     | `-h1-addr`     | `:7246` | Clear HTTP/1.1 UI, API, transfers, and WebSockets.          |
| `GM_H1_TLS_ADDR` | `-h1-tls-addr` | empty   | Native HTTPS/WSS HTTP/1.1 listener.                         |
| `GM_H2_ADDR`     | `-h2-addr`     | empty   | Native deterministic HTTP/2 measurement listener.           |
| `GM_H3_ADDR`     | `-h3-addr`     | empty   | Native deterministic HTTP/3 UDP listener and TCP bootstrap. |
| `GM_TLS_CERT`    | `-tls-cert`    | empty   | Certificate PEM for all enabled native TLS listeners.       |
| `GM_TLS_KEY`     | `-tls-key`     | empty   | Private-key PEM for all enabled native TLS listeners.       |

Ports 7246–7249 are the convention used by the container image's `EXPOSE` set and every example
in this repository:

| Port       | Enabled by       | Serves                                                    |
| ---------- | ---------------- | --------------------------------------------------------- |
| `7246/tcp` | `GM_H1_ADDR`     | Clear HTTP/1.1 UI, API, transfers, and WebSocket latency. |
| `7247/tcp` | `GM_H1_TLS_ADDR` | HTTPS UI, API, transfers, and WSS latency.                |
| `7248/tcp` | `GM_H2_ADDR`     | HTTP/2 measurement only.                                  |
| `7249/tcp` | `GM_H3_ADDR`     | HTTP/3 Alt-Svc bootstrap probe only.                      |
| `7249/udp` | `GM_H3_ADDR`     | HTTP/3 QUIC measurement.                                  |

Only 7246 and 7247 serve a browser; 7248 and 7249 are strict measurement targets. The routes each
listener owns are in [ARCHITECTURE.md](ARCHITECTURE.md#the-go-measurement-server).

`GM_TLS_CERT`/`GM_TLS_KEY` are one PEM pair shared by every enabled native TLS listener. The pair
is validated before binding, and a complete valid renewal is picked up without a restart. Mount
the whole Let's Encrypt tree rather than one `live/` directory: its entries are symlinks into
`archive/`. Development certificates:
[DEVELOPMENT.md](DEVELOPMENT.md#local-tls-and-http3-certificates).

## Advertised endpoints and public origins

What `/preflight` offers clients to measure against:

| Environment                       | Flag                           | Default | Meaning                                                                                                        |
| --------------------------------- | ------------------------------ | ------- | -------------------------------------------------------------------------------------------------------------- |
| `GM_ADVERTISED_NATIVE_ENDPOINTS`  | `-advertised-native-endpoints` | `all`   | Which enabled native listeners `/preflight` advertises: `all`, `none`, or a comma list of `http1-clear`, `http1-tls`, `http2`, `http3`. |
| `GM_H1_PUBLIC_ORIGIN`             | `-h1-public-origin`            | empty   | Externally reachable origin of the native clear-H1 listener, for host/port remapping.                           |
| `GM_H1_TLS_PUBLIC_ORIGIN`         | `-h1-tls-public-origin`        | empty   | Externally reachable origin of the native HTTPS H1 listener.                                                    |
| `GM_H2_PUBLIC_ORIGIN`             | `-h2-public-origin`            | empty   | Externally reachable origin of the native H2 listener.                                                          |
| `GM_H3_PUBLIC_ORIGIN`             | `-h3-public-origin`            | empty   | Externally reachable origin of the native H3 listener.                                                          |
| `GM_PUBLIC_ORIGINS`               | `-public-origins`              | empty   | Comma-separated negotiated (reverse-proxy) origins offering throughput and WebSocket latency.                   |
| `GM_PUBLIC_THROUGHPUT_ORIGINS`    | `-public-throughput-origins`   | empty   | Negotiated origins offering throughput only.                                                                    |
| `GM_PUBLIC_LATENCY_ORIGINS`       | `-public-latency-origins`      | empty   | Negotiated origins offering WebSocket latency only.                                                             |

Native listeners are deterministic: each advertises exactly one protocol, and the `*_PUBLIC_ORIGIN`
overrides remap host/port only. Negotiated origins are the reverse-proxy path: clients detect the
protocol they actually used to reach the proxy instead of assuming the protocol behind it. The
value `self` means the origin the client fetched `/preflight` from — the usual proxy setting.
Proxy examples and header requirements: [REVERSE_PROXY.md](REVERSE_PROXY.md).

### Examples

```env
# Proxy-only UI, throughput, and WebSocket latency
GM_ADVERTISED_NATIVE_ENDPOINTS=none
GM_PUBLIC_ORIGINS=self
```

```env
# HTTPS offers everything; HTTP offers throughput only
GM_ADVERTISED_NATIVE_ENDPOINTS=none
GM_PUBLIC_ORIGINS=https://meter.example
GM_PUBLIC_THROUGHPUT_ORIGINS=http://meter.example
```

```env
# Hybrid: proxy ergonomics plus deterministic native protocol endpoints
GM_H1_TLS_ADDR=:7247
GM_H2_ADDR=:7248
GM_H3_ADDR=:7249
GM_TLS_CERT=/run/secrets/fullchain.pem
GM_TLS_KEY=/run/secrets/privkey.pem
GM_PUBLIC_ORIGINS=self
```

## Identity, proxy trust, and limits

| Environment                             | Flag                                  | Default          | Meaning                                                                          |
| --------------------------------------- | ------------------------------------- | ---------------- | -------------------------------------------------------------------------------- |
| `GM_SERVER_NAME`                        | `-name`                               | `graphite-meter` | Server name in `/preflight`.                                                     |
| `GM_SERVER_LOCATION`                    | `-location`                           | empty            | Optional location label.                                                         |
| `GM_TRUSTED_PROXIES`                    | none                                  | empty            | Comma-separated proxy CIDRs allowed to supply client-address headers. Invalid CIDRs fail startup. |
| `GM_MAX_ACTIVE_MEASUREMENTS`            | `-max-active-measurements`            | `256`            | Global concurrent measurement handlers.                                          |
| `GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT` | `-max-active-measurements-per-client` | `32`             | Per-client measurement handlers.                                                 |
| `GM_MAX_CONNECTIONS`                    | `-max-connections`                    | `512`            | Global TCP/QUIC connections.                                                     |
| `GM_MAX_CONNECTIONS_PER_CLIENT`         | `-max-connections-per-client`         | `64`             | Per-direct-client connections.                                                   |
| `GM_MAX_OPERATION_DURATION`             | `-max-operation-duration`             | `5m`             | Maximum operation lifetime.                                                      |
| `GM_VERBOSE`                            | `-verbose`                            | `false`          | Per-second server measurement logs.                                              |

## Authentication

Off by default. When enabled, every UI asset, discovery request, probe, transfer, progress
stream, and WebSocket requires a browser session or terminal grant.

| Environment                       | Flag                            | Default    | Meaning                                                              |
| --------------------------------- | ------------------------------- | ---------- | -------------------------------------------------------------------- |
| `GM_AUTH_MODE`                    | `-auth-mode`                    | `off`      | `off`, `password`, `oidc`, or `hybrid`.                              |
| `GM_AUTH_PUBLIC_URL`              | `-auth-public-url`              | empty      | Canonical HTTPS UI origin, without a path.                           |
| `GM_AUTH_PASSWORD_HASH`           | none                            | empty      | Inline Argon2id PHC hash. Prefer the file variant in containers.     |
| `GM_AUTH_PASSWORD_HASH_FILE`      | `-auth-password-hash-file`      | empty      | File containing the Argon2id PHC hash.                               |
| `GM_AUTH_OIDC_ISSUER`             | `-auth-oidc-issuer`             | empty      | Exact OIDC issuer URL.                                               |
| `GM_AUTH_OIDC_CLIENT_ID`          | `-auth-oidc-client-id`          | empty      | Confidential-client ID.                                              |
| `GM_AUTH_OIDC_CLIENT_SECRET`      | none                            | empty      | Inline client secret. Prefer the file variant in containers.         |
| `GM_AUTH_OIDC_CLIENT_SECRET_FILE` | `-auth-oidc-client-secret-file` | empty      | File containing the client secret.                                   |
| `GM_AUTH_OIDC_ALLOWED_GROUPS`     | `-auth-oidc-allowed-groups`     | empty      | Comma-separated, case-sensitive group allowlist. Required with OIDC. |
| `GM_AUTH_OIDC_PROVIDER_NAME`      | `-auth-oidc-provider-name`      | `Authelia` | Provider label on the login page.                                    |

Generate the operator hash interactively — the password is never accepted as a command-line
argument:

```sh
graphite-meter hash-password
```

Authenticated deployments must advertise only HTTPS origins on the canonical hostname: set
`GM_ADVERTISED_NATIVE_ENDPOINTS` to omit `http1-clear`. For OIDC, register
`${GM_AUTH_PUBLIC_URL}/auth/oidc/callback` as a confidential authorization-code client using PKCE
S256, `client_secret_basic`, and scopes `openid profile groups`. Behind a reverse proxy, the
required forwarding headers are in
[REVERSE_PROXY.md](REVERSE_PROXY.md#measurement-requirements).

### Authorizing the terminal client

The terminal client has no password prompt and stores nothing. Against an authenticated server it
asks a browser to vouch for it, once per launch:

1. The client generates a random verifier, keeps it in memory, and derives a challenge from it.
2. It opens `${GM_AUTH_PUBLIC_URL}/auth/cli?challenge=…` in your browser and prints a short
   verification code.
3. The browser page shows the same code. Confirm they match, then approve. If they differ,
   something else asked for the approval — refuse it.
4. The client exchanges its verifier for a grant. Approval expires **two minutes** after the
   page is opened.

The grant lives in the client's memory only: bound to the approving browser session and the
exact HTTPS origin, refused on any redirect, valid for measurement routes only. Signing out of
the browser session revokes it; closing the client discards it. The client refuses to
authenticate over anything but HTTPS and refuses `-insecure` entirely.

### Hybrid mode with a private identity provider

In `hybrid` mode the login page offers both the identity provider and the operator password. The
browser, not the server, reaches the identity provider — so visitors who cannot route to a
private provider (VPN, Tailscale) sign in with the operator password instead. That is the
intended fallback. If OIDC must be the only way in, use `oidc` mode, which starts only once
issuer discovery succeeds.

### Bounded stores and the availability trade-off

Every authentication store is bounded in memory: an anonymous caller can be refused, but cannot
make the process allocate without limit.

| Bound                | Limit                                                     |
| -------------------- | --------------------------------------------------------- |
| Sessions             | 1024 total, 8 per subject, 8-hour lifetime                |
| Password attempts    | 5 per client address per minute, 60 total per minute      |
| OIDC token exchanges | 10 per client address per minute                          |
| OIDC transactions    | 256 total, 8 per client address, 10-minute lifetime       |
| Terminal approvals   | 256 total, 8 per session, 2-minute lifetime               |
| Tracked addresses    | 2048 per budget                                           |

Per-address budgets are keyed by the full IPv4 address and by the IPv6 **/64**. A sustained
anonymous flood can hold a global ceiling engaged and refuse new sign-ins (existing sessions are
unaffected); the server then logs once per minute:

```
[gm:auth] global password-attempt ceiling engaged; further attempts are refused until the window drains
```

That line means the ceiling, not the service, is refusing work. Put the deployment behind a
network-level rate limit if you expect sustained hostile traffic.

## Native TUI client flags

`graphite-meter-client` is configured by flags only. Every run setting below is also editable
inside the TUI before a run starts.

| Flag                     | Default                 | Meaning                                                                                        |
| ------------------------ | ----------------------- | ---------------------------------------------------------------------------------------------- |
| `-url`                   | `http://127.0.0.1:7246` | Server base URL.                                                                               |
| `-throughput-origin`     | `auto`                  | Discovered throughput origin.                                                                  |
| `-throughput-protocol`   | `auto`                  | `auto`, `http1`, `http2`, or `http3`; fixed native endpoints reject mismatches.                |
| `-latency-origin`        | `auto`                  | Discovered WebSocket latency origin.                                                           |
| `-stages`                | `latency,download,upload` | Comma list: `latency`/`ping`, `download`/`down`, `upload`/`up`, `bidirectional`/`bidi`.      |
| `-warmup`                | `800ms`                 | Per-stage warmup before measurement starts.                                                    |
| `-latency-duration`      | `4s`                    | Latency stage window.                                                                          |
| `-download-duration`     | `10s`                   | Download stage window.                                                                         |
| `-upload-duration`       | `10s`                   | Upload stage window.                                                                           |
| `-bidirectional-duration`| `10s`                   | Bidirectional stage window.                                                                    |
| `-auto-streams`          | `6`                     | Maximum automatic HTTP/1 streams per direction. Native H2/H3 use one request per direction.    |
| `-streams`               | `0`                     | `0` selects automatic; `1–128` forces an exact count per direction for every protocol.         |
| `-ping`                  | `medium`                | Ping cadence: `instant` (80ms) / `medium` (250ms) / `slow` (600ms), or a raw Go duration.      |
| `-loaded-latency`        | `true`                  | Measure RTT while a transfer stage is running.                                                 |
| `-insecure`              | `false`                 | Skip TLS certificate verification. Refused against an authenticated server.                    |
| `-version`               | `false`                 | Print the client version and exit.                                                             |

## Deployment scaffolding

`GM_PUBLIC_HOST`, `GM_CERT_NAME`, and `CERTBOT_EMAIL` appear in the Compose and Quadlet examples
but are never read by the server. Compose interpolates the first two into
`GM_H*_PUBLIC_ORIGIN`, `GM_TLS_CERT`, and `GM_TLS_KEY`; the certbot units read all three to issue
and renew the certificate.
