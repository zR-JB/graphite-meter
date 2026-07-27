# Configuration

The server reads environment variables at startup; matching command-line flags override them.
Environment variables are the canonical interface for Docker, Compose, Quadlet, and Kubernetes.
This page lists every runtime option. Defaults need no configuration.

## Native listeners

A non-empty listener address enables that listener. There are no separate enable switches.

| Environment      | Flag            | Default | Meaning                                                     |
| ---------------- | --------------- | ------- | ----------------------------------------------------------- |
| `GM_H1_ADDR`     | `--h1-addr`     | `:7246` | Clear HTTP/1.1 UI, API, transfers, and WebSockets.          |
| `GM_H1_TLS_ADDR` | `--h1-tls-addr` | empty   | Native HTTPS/WSS HTTP/1.1 listener.                         |
| `GM_H2_ADDR`     | `--h2-addr`     | empty   | Native deterministic HTTP/2 measurement listener.           |
| `GM_H3_ADDR`     | `--h3-addr`     | empty   | Native deterministic HTTP/3 UDP listener and TCP bootstrap. |
| `GM_TLS_CERT`    | `--tls-cert`    | empty   | Certificate PEM for all enabled native TLS listeners.       |
| `GM_TLS_KEY`     | `--tls-key`     | empty   | Private-key PEM for all enabled native TLS listeners.       |

Ports 7246–7249 are the convention used by the container image's `EXPOSE` set and every example
in this repository:

| Port       | Enabled by       | Serves                                                    |
| ---------- | ---------------- | --------------------------------------------------------- |
| `7246/tcp` | `GM_H1_ADDR`     | Clear HTTP/1.1 UI, API, transfers, and WebSocket latency. |
| `7247/tcp` | `GM_H1_TLS_ADDR` | HTTPS UI, API, transfers, and WSS latency.                |
| `7248/tcp` | `GM_H2_ADDR`     | HTTP/2 measurement only.                                  |
| `7249/tcp` | `GM_H3_ADDR`     | HTTP/3 Alt-Svc bootstrap probe only.                      |
| `7249/udp` | `GM_H3_ADDR`     | HTTP/3 QUIC measurement, plus WebTransport sessions.      |

Only 7246 and 7247 serve a browser; 7248 and 7249 are strict measurement targets. The routes each
listener owns are in [ARCHITECTURE.md](ARCHITECTURE.md#the-go-measurement-server).

WebTransport lives on 7249/udp, but a browser reaches it only from a page in a secure context:
`WebTransport` is a `[SecureContext]` interface, so a UI served over plain HTTP has no such API at
all. Serve the UI from 7247 (or an HTTPS proxy origin) if browsers should use it. Loopback counts
as secure, so the API is present over `http://localhost:7246` in local development and absent over
the same server's LAN address. The native client is not subject to any of this. Where the API is
missing the browser's path picker names which of the two reasons applies — an insecure page, or a
browser that never shipped it — and each role falls back rather than the run failing.

`GM_TLS_CERT`/`GM_TLS_KEY` are one PEM pair shared by every enabled native TLS listener. The pair
is validated before binding, and a complete valid renewal is picked up without a restart. Mount
the whole Let's Encrypt tree rather than one `live/` directory: its entries are symlinks into
`archive/`. Development certificates:
[DEVELOPMENT.md](DEVELOPMENT.md#local-tls-and-http3-certificates).

## Advertised endpoints and public origins

What `/preflight` offers clients to measure against:

| Environment                       | Flag                            | Default | Meaning                                                                                                        |
| --------------------------------- | ------------------------------- | ------- | -------------------------------------------------------------------------------------------------------------- |
| `GM_ADVERTISED_NATIVE_ENDPOINTS`  | `--advertised-native-endpoints` | `all`   | Which enabled native listeners `/preflight` advertises: `all`, `none`, or a comma list of `http1-clear`, `http1-tls`, `http2`, `http3`. |
| `GM_H1_PUBLIC_ORIGIN`             | `--h1-public-origin`            | empty   | Externally reachable origin of the native clear-H1 listener, for host/port remapping.                           |
| `GM_H1_TLS_PUBLIC_ORIGIN`         | `--h1-tls-public-origin`        | empty   | Externally reachable origin of the native HTTPS H1 listener.                                                    |
| `GM_H2_PUBLIC_ORIGIN`             | `--h2-public-origin`            | empty   | Externally reachable origin of the native H2 listener.                                                          |
| `GM_H3_PUBLIC_ORIGIN`             | `--h3-public-origin`            | empty   | Externally reachable origin of the native H3 listener.                                                          |
| `GM_PUBLIC_ORIGINS`               | `--public-origins`              | empty   | Comma-separated negotiated (reverse-proxy) origins offering throughput and WebSocket latency.                   |
| `GM_PUBLIC_THROUGHPUT_ORIGINS`    | `--public-throughput-origins`   | empty   | Negotiated origins offering throughput only.                                                                    |
| `GM_PUBLIC_LATENCY_ORIGINS`       | `--public-latency-origins`      | empty   | Negotiated origins offering WebSocket latency only.                                                             |

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

| Environment                             | Flag                                   | Default          | Meaning                                                                          |
| --------------------------------------- | -------------------------------------- | ---------------- | -------------------------------------------------------------------------------- |
| `GM_SERVER_NAME`                        | `--name`                               | `graphite-meter` | Server name in `/preflight`.                                                     |
| `GM_SERVER_LOCATION`                    | `--location`                           | empty            | Optional location label.                                                         |
| `GM_TRUSTED_PROXIES`                    | none                                   | empty            | Comma-separated proxy CIDRs allowed to supply client-address headers. List the proxy's actual CIDR — a default route (`0.0.0.0/0`, `::/0`) is rejected, since trusting every caller's headers lets any client spoof its address and dodge the rate limits. Invalid CIDRs fail startup. |
| `GM_MAX_ACTIVE_MEASUREMENTS`            | `--max-active-measurements`            | `256`            | Global concurrent measurement handlers.                                          |
| `GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT` | `--max-active-measurements-per-client` | `32`             | Per-client measurement handlers.                                                 |
| `GM_MAX_ACTIVE_SESSIONS`                | `--max-active-sessions`                | `64`             | Ceiling on how much of the measurement pool WebTransport sessions may occupy. The session routes (`/wt/download`, `/wt/upload`) hold their slot for the session duration rather than the operation duration, so capping them — at a quarter of the pool by default — keeps the request-shaped routes from being refused behind them. It is a ceiling, not a reservation: nothing holds slots open for sessions, so a pool filled by request-shaped routes (the two ping buses among them) refuses every session while none is active. Must be greater than zero, and must not exceed the global measurement limit. |
| `GM_MAX_SESSIONS_PER_CLIENT`            | `--max-sessions-per-client`            | `16`             | Per-login WebTransport transfer sessions, which hold a measurement slot for a whole test and so carry their own budget. Under authentication it is keyed by login where the request budget is keyed by subject, so one account's phone and desktop hold separate budgets and one device's held sessions cannot starve another's; with authentication off there is no login and it falls back to the same address bucket the request budget uses. The datagram ping bus is not one: it is bounded like the WebSocket bus. Several tabs share one login, so leave room. With the defaults four distinct logins or address buckets can occupy the whole session budget; that is tolerable because a refused session degrades rather than denies — discovery advertises fetch streams and WebSocket alongside, and a client falls back per role — so an operator expecting many distinct clients should lower this rather than raise the global budget. Must not exceed the per-client measurement limit, nor the global session budget one client's sessions draw from. |
| `GM_MAX_CONNECTIONS`                    | `--max-connections`                    | `512`            | Global TCP/QUIC connections.                                                     |
| `GM_MAX_CONNECTIONS_PER_CLIENT`         | `--max-connections-per-client`         | `64`             | Per-direct-client connections.                                                   |
| `GM_MAX_OPERATION_DURATION`             | `--max-operation-duration`             | `5m`             | Maximum lifetime of one measurement request.                                     |
| `GM_MAX_SESSION_DURATION`               | `--max-session-duration`               | `2h`             | Maximum lifetime of one WebTransport session, which hosts a whole test. Must be at least the operation duration. |
| `GM_VERBOSE`                            | `--verbose`                            | `false`          | Per-second server measurement logs.                                              |

The measurement endpoints are meant to move data fast: a single `/download` streams up to
64 GiB and the server applies no bandwidth cap of its own (throttling would corrupt the
measurement). A deployment reachable from the open internet should therefore either enable
[authentication](#authentication) or sit behind a proxy/firewall that caps per-client bandwidth and
connections — otherwise an anonymous client can pull sustained traffic at your expense.

## Authentication

Off by default. When enabled, every UI asset, discovery request, probe, transfer, progress
stream, WebSocket, and WebTransport session requires a browser session or terminal grant. A
WebTransport CONNECT can send neither cookies nor headers from a browser, so the client mints a
single-use 30-second token at `POST /wt/session` and carries it in the CONNECT URL; the native
client sends its grant as a header. Revoking a session ends its live WebTransport sessions.

| Environment                       | Flag                             | Default    | Meaning                                                              |
| --------------------------------- | -------------------------------- | ---------- | -------------------------------------------------------------------- |
| `GM_AUTH_MODE`                    | `--auth-mode`                    | `off`      | `off`, `password`, `oidc`, or `hybrid`.                              |
| `GM_AUTH_PUBLIC_URL`              | `--auth-public-url`              | empty      | Canonical HTTPS UI origin, without a path.                           |
| `GM_AUTH_PASSWORD_HASH`           | none                             | empty      | Inline Argon2id PHC hash. Prefer the file variant in containers.     |
| `GM_AUTH_PASSWORD_HASH_FILE`      | `--auth-password-hash-file`      | empty      | File containing the Argon2id PHC hash.                               |
| `GM_AUTH_OIDC_ISSUER`             | `--auth-oidc-issuer`             | empty      | Exact OIDC issuer URL.                                               |
| `GM_AUTH_OIDC_CLIENT_ID`          | `--auth-oidc-client-id`          | empty      | Confidential-client ID.                                              |
| `GM_AUTH_OIDC_CLIENT_SECRET`      | none                             | empty      | Inline client secret. Prefer the file variant in containers.         |
| `GM_AUTH_OIDC_CLIENT_SECRET_FILE` | `--auth-oidc-client-secret-file` | empty      | File containing the client secret.                                   |
| `GM_AUTH_OIDC_ALLOWED_GROUPS`     | `--auth-oidc-allowed-groups`     | empty      | Comma-separated, case-sensitive group allowlist. Required with OIDC. |
| `GM_AUTH_OIDC_PROVIDER_NAME`      | `--auth-oidc-provider-name`      | `Authelia` | Provider label on the login page.                                    |

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

### Sessions, cookies, and what they bind to

A browser sign-in creates a session that lasts **8 hours, absolute**. Nothing extends it: no
request, no open page, no measurement renews the clock, so a tab left open overnight is signed
out in the morning and lands back on the login page.

**Signing in again** in the same browser rotates the old session out — the session it replaces,
and every bearer grant issued from it, is revoked immediately rather than left live for the rest
of its lifetime. **Sign out** ends the current session (and its grants). **Sign out everywhere**
(the second account-strip button) ends *every* session for the account at once, which is how you
revoke a session or terminal grant whose token you no longer hold — a shared device, a stolen
laptop, a leaked grant. A plain sign-out does not touch your other devices; sign-out-everywhere
does.

The server sets four cookies, all `__Host-` prefixed — which browsers accept only with `Secure`,
`Path=/`, and **no `Domain`**:

| Cookie              | Lifetime | Readable by JS | Purpose                                        |
| ------------------- | -------- | -------------- | ---------------------------------------------- |
| `__Host-gm_session` | 8h       | no             | The session. Only its SHA-256 is stored.       |
| `__Host-gm_csrf`    | 8h       | yes            | Mirrored into `X-CSRF-Token` on unsafe methods. |
| `__Host-gm_login`   | 10 min   | no             | Pre-session token for the sign-in form.        |
| `__Host-gm_oidc`    | 10 min   | no             | Binds an OIDC round trip to this browser.      |

`__Host-gm_csrf` is deliberately readable: the browser client echoes it in the `X-CSRF-Token`
header, and the server compares that header against the token it holds for the session. The
session cookie proves who you are; the header proves the request came from a page that could read
the value. A cookie alone would still be attached to requests another site triggered.

An identity provider sets its own cookie on its own host — Authelia's `authelia_session`, for
instance. Graphite Meter never sees it.

What `__Host-` costs you, concretely:

- **One hostname.** Cookies set on `meter.example.com` never reach `example.com` or a sibling
  subdomain, and no subdomain can overwrite them.
- **Any port on that hostname.** Cookies ignore ports, so `https://meter.example.com` and
  `https://meter.example.com:7248` share one session. This is what makes cross-port native
  measurement work while signed in.
- **HTTPS only**, which is why an authenticated deployment cannot advertise clear HTTP/1.1.

That is also the rule for advertised measurement origins: scheme must be `https` and the
**hostname must equal** `GM_AUTH_PUBLIC_URL`'s; the port may differ freely. A measurement origin
on a different hostname is refused at startup. The terminal client is exempt because it does not
use cookies at all — it carries a bearer grant, accepted only on measurement routes.

### Authorizing the terminal client

The terminal client has no password prompt and stores nothing. Against an authenticated server it
asks a browser to vouch for it, once per launch:

1. The client generates a random verifier, keeps it in memory, and derives a challenge from it.
2. It shows a short verification code and the approval URL
   (`${GM_AUTH_PUBLIC_URL}/auth/cli?challenge=…`), and waits. Press `enter` to open that page in
   your browser — nothing opens on its own, so the code can be read first, and the URL can be
   opened by hand instead, on this machine or another one.
3. The browser page shows the same code. Confirm they match, then approve. If they differ,
   something else asked for the approval — refuse it.
4. The client exchanges its verifier for a grant. Approval expires **two minutes** after the
   page is opened.

The grant lives in the client's memory only: bound to the approving browser session and the
exact HTTPS origin, refused on any redirect, valid for measurement routes only. Signing out of
the browser session revokes it; closing the client discards it. The client refuses to
authenticate over anything but HTTPS and refuses `--insecure` entirely.

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
inside the TUI before a run starts, though the two halves of a path are one row there: the
Connections section walks the advertised (origin, transport) pairs per role rather than offering
`--throughput-origin` and `--throughput-transport` as independent cycles, so a combination no
server advertises cannot be assembled by hand. The flags stay independent, and a pair the server
does not offer is refused by the connection check with the reason.

| Flag                       | Default                 | Meaning                                                                                        |
| -------------------------- | ----------------------- | ---------------------------------------------------------------------------------------------- |
| `--url`                    | `http://127.0.0.1:7246` | Server base URL. Nothing is dialled until it is picked in the TUI or rechecked with `v`.       |
| `--throughput-origin`      | `auto`                  | Discovered throughput origin.                                                                  |
| `--throughput-protocol`    | `auto`                  | `auto`, `http1`, `http2`, or `http3`; fixed native endpoints reject mismatches, so it decides anything only on a `negotiated` (reverse-proxy) origin. The TUI's version row goes inert on a path that fixes its own. |
| `--throughput-transport`   | `auto`                  | `auto`, `fetch-stream`, or `webtransport`; automatic prefers fetch streams. Datagram throughput is a browser-only mode and this client refuses `webtransport-datagram`. |
| `--latency-origin`         | `auto`                  | Discovered latency origin.                                                                     |
| `--latency-transport`      | `auto`                  | `auto`, `websocket`, or `webtransport`; datagrams measure loss a WebSocket cannot show. Only a run that commits to WebTransport is bound by the `--ping` ceiling below, so `websocket` is exempt and so is an `auto` run that resolves to, or falls back to, the WebSocket bus. |
| `--stages`                 | `latency,download,upload` | Comma list: `latency`/`ping`, `download`/`down`, `upload`/`up`, `bidirectional`/`bidi`.      |
| `--warmup`                 | `800ms`                 | Per-stage warmup before measurement starts.                                                    |
| `--latency-duration`       | `4s`                    | Latency stage window.                                                                          |
| `--download-duration`      | `10s`                   | Download stage window.                                                                         |
| `--upload-duration`        | `10s`                   | Upload stage window.                                                                           |
| `--bidirectional-duration` | `10s`                   | Bidirectional stage window.                                                                    |
| `--auto-streams`           | `6`                     | Maximum automatic HTTP/1 streams per direction. A multiplexed connection resolves its own count per direction — H2 one download and four upload, H3 one either way — and ignores this. |
| `--streams`                | `0`                     | `0` selects automatic; `1–128` forces an exact count per direction. WebTransport clamps a forced count to 16 per direction, the session's own ceiling. |
| `--ping`                   | `medium`                | Ping cadence: `instant` (80ms) / `medium` (250ms) / `slow` (600ms), or a raw Go duration. Over the WebTransport latency bus the cadence is capped at `15s` — half the server's 30-second WebTransport idle bound, past which the bus is reaped between pings. `auto` is not checked at parse time: the bus is unresolved until Prepare commits one, and rejecting early would refuse a cadence a WebSocket fallback would have taken. The bound is applied once the run has committed to WebTransport. The WebSocket bus has no idle timer and takes any positive cadence. |
| `--loaded-latency`         | `true`                  | Measure RTT while a transfer stage is running.                                                 |
| `--insecure`               | `false`                 | Skip TLS certificate verification. Refused against an authenticated server.                    |
| `--version`                | `false`                 | Print the client version and exit.                                                             |

## Deployment scaffolding

`GM_PUBLIC_HOST`, `GM_CERT_NAME`, and `CERTBOT_EMAIL` appear in the Compose and Quadlet examples
but are never read by the server. Compose interpolates the first two into
`GM_H*_PUBLIC_ORIGIN`, `GM_TLS_CERT`, and `GM_TLS_KEY`; the certbot units read all three to issue
and renew the certificate.
