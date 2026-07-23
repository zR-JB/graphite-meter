# Configuration

Graphite Meter reads environment variables at startup. Matching command-line flags override the environment. Environment variables are the canonical interface for Docker, Compose, Quadlet, and Kubernetes deployments.

## Native listeners

| Environment | Flag | Default | Meaning |
| --- | --- | --- | --- |
| `GM_H1_ADDR` | `-h1-addr` | `:7246` | Clear HTTP/1.1 UI, API, transfers, and WebSockets. |
| `GM_H1_TLS_ADDR` | `-h1-tls-addr` | empty | Native HTTPS/WSS HTTP/1.1 listener. |
| `GM_H2_ADDR` | `-h2-addr` | empty | Native deterministic HTTP/2 measurement listener. |
| `GM_H3_ADDR` | `-h3-addr` | empty | Native deterministic HTTP/3 UDP listener and TCP bootstrap. |
| `GM_TLS_CERT` | `-tls-cert` | empty | Certificate PEM for enabled native TLS listeners. |
| `GM_TLS_KEY` | `-tls-key` | empty | Private-key PEM for enabled native TLS listeners. |

A non-empty listener address enables that listener. There are no separate enable switches.

`GM_H1_PUBLIC_ORIGIN`, `GM_H1_TLS_PUBLIC_ORIGIN`, `GM_H2_PUBLIC_ORIGIN`, and `GM_H3_PUBLIC_ORIGIN` override the externally reachable origin of the corresponding native listener. Their flags use the same names without `GM_` and with lowercase dashes. They are for host/port remapping only; the advertised protocols remain deterministic.

`GM_ADVERTISED_NATIVE_ENDPOINTS` controls which enabled native listeners appear in `/preflight`:

- `all` (default): every enabled native listener
- `none`: no native listener
- A comma list of `http1-clear`, `http1-tls`, `http2`, and `http3`

## Reverse-proxy endpoints

| Environment | Flag | Capability |
| --- | --- | --- |
| `GM_PUBLIC_ORIGINS` | `-public-origins` | Throughput and WebSocket latency |
| `GM_PUBLIC_THROUGHPUT_ORIGINS` | `-public-throughput-origins` | Throughput only |
| `GM_PUBLIC_LATENCY_ORIGINS` | `-public-latency-origins` | WebSocket latency only |

Values are comma-separated HTTP(S) origins. `self` means the origin used to fetch `/preflight`, which is the usual reverse-proxy setting. Public endpoints are negotiated: clients detect the protocol used to reach the proxy instead of assuming the protocol used between the proxy and Graphite Meter.

Common deployments:

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

| Environment | Default | Meaning |
| --- | --- | --- |
| `GM_SERVER_NAME` | `graphite-meter` | Server name in `/preflight`. |
| `GM_SERVER_LOCATION` | empty | Optional location label. |
| `GM_TRUSTED_PROXIES` | empty | Comma-separated proxy CIDRs allowed to supply client-address headers. |
| `GM_MAX_ACTIVE_MEASUREMENTS` | `256` | Global concurrent measurement handlers. |
| `GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT` | `32` | Per-client measurement handlers. |
| `GM_MAX_CONNECTIONS` | `512` | Global TCP/QUIC connections. |
| `GM_MAX_CONNECTIONS_PER_CLIENT` | `64` | Per-direct-client connections. |
| `GM_MAX_OPERATION_DURATION` | `5m` | Maximum operation lifetime. |
| `GM_VERBOSE` | false | Per-second server measurement logs. |

## Optional authentication

Authentication is off by default. When enabled, every UI asset, discovery request, probe, transfer, progress stream, and WebSocket requires a browser session or terminal grant.

| Environment | Flag | Meaning |
| --- | --- | --- |
| `GM_AUTH_MODE` | `-auth-mode` | `off`, `password`, `oidc`, or `hybrid`. |
| `GM_AUTH_PUBLIC_URL` | `-auth-public-url` | Canonical HTTPS UI origin, without a path. |
| `GM_AUTH_PASSWORD_HASH` | none | Inline Argon2id PHC hash. Prefer a secret file in containers. |
| `GM_AUTH_PASSWORD_HASH_FILE` | `-auth-password-hash-file` | File containing the Argon2id PHC hash. |
| `GM_AUTH_OIDC_ISSUER` | `-auth-oidc-issuer` | Exact OIDC issuer URL. |
| `GM_AUTH_OIDC_CLIENT_ID` | `-auth-oidc-client-id` | Confidential-client ID. |
| `GM_AUTH_OIDC_CLIENT_SECRET` | none | Inline client secret. Prefer a secret file in containers. |
| `GM_AUTH_OIDC_CLIENT_SECRET_FILE` | `-auth-oidc-client-secret-file` | File containing the client secret. |
| `GM_AUTH_OIDC_ALLOWED_GROUPS` | `-auth-oidc-allowed-groups` | Comma-separated, case-sensitive group allowlist. |
| `GM_AUTH_OIDC_PROVIDER_NAME` | `-auth-oidc-provider-name` | Login/provider label; default `Authelia`. |

Generate a local operator hash interactively; the password is never accepted as a command-line argument:

```sh
graphite-meter hash-password
```

Authenticated deployments must advertise only HTTPS origins on the canonical hostname. Set `GM_ADVERTISED_NATIVE_ENDPOINTS` to omit `http1-clear`. For OIDC, register `${GM_AUTH_PUBLIC_URL}/auth/oidc/callback` as a confidential authorization-code client using PKCE S256, `client_secret_basic`, and scopes `openid profile groups`.

### Authorizing the terminal client

The terminal client has no password prompt and stores nothing. When it meets an authenticated server it asks a browser to vouch for it, once per launch:

1. The client generates a random verifier, keeps it in memory, and derives a challenge from it.
2. It opens `${GM_AUTH_PUBLIC_URL}/auth/cli?challenge=…` in your browser and prints a short verification code.
3. The browser page shows the same code. Confirm they match, then approve. If they differ, something else asked for the approval — refuse it.
4. The client exchanges its verifier for a grant. Approval expires **two minutes** after the page is opened.

The grant lives in the client's memory only. It is never written to disk, is bound to the browser session that approved it and to the exact HTTPS origin it was issued for, is refused on any redirect, and reaches measurement routes only — never the UI or the auth surface. Signing out of the browser session revokes it immediately, and closing the client discards it, so every launch needs a fresh approval.

The client refuses to authenticate over anything but HTTPS and refuses `-insecure` entirely: a grant is never sent to an unverified server.

### Hybrid mode with a private identity provider

In `hybrid` mode the login page offers both the identity provider and the operator password. The browser, not the server, reaches the identity provider — so if the provider is only routable on a private network (a VPN or overlay such as Tailscale), visitors outside it cannot complete an OIDC sign-in and use the operator password instead. That is the intended fallback, not a misconfiguration. If you want OIDC to be the only way in, use `oidc` mode, which starts only once discovery succeeds.

### Bounded stores and the availability trade-off

Every authentication store is bounded in memory, and the bounds are deliberately chosen over unbounded growth: an anonymous caller can be refused, but cannot make the process allocate without limit.

| Bound | Limit |
| --- | --- |
| Sessions | 1024 total, 8 per subject, 8-hour lifetime |
| Password attempts | 5 per client address per minute, 60 across all addresses per minute |
| OIDC token exchanges | 10 per client address per minute |
| OIDC transactions | 256 total, 8 per client address, 10-minute lifetime |
| Terminal approvals | 256 total, 8 per session, 2-minute lifetime |
| Tracked addresses | 2048 per budget |

Per-address budgets are keyed by the full IPv4 address and by the IPv6 **/64**, because a single IPv6 allocation routinely covers 2^64 addresses.

The consequence is that a determined anonymous attacker can hold a global ceiling engaged and keep others from signing in for as long as they keep it saturated. Existing sessions are unaffected — only new sign-ins are refused. When a global ceiling engages the server logs it once per minute:

```
[gm:auth] global password-attempt ceiling engaged; further attempts are refused until the window drains
```

Treat those lines as the signal that the ceiling, not the service, is refusing work. Put the deployment behind a network-level rate limit if you expect sustained hostile traffic.

## Breaking migration

- Replace `GM_ENABLE_H1_TLS=true` with a non-empty `GM_H1_TLS_ADDR`.
- Replace `GM_ENABLE_H2=true` with a non-empty `GM_H2_ADDR`.
- Replace `GM_ENABLE_H3=true` with a non-empty `GM_H3_ADDR`.
- Replace `PUBLIC_H*_ORIGIN` with the corresponding `GM_H*_PUBLIC_ORIGIN` only for deterministic native listeners.
- Advertise ordinary reverse proxies with `GM_PUBLIC_ORIGINS`, not one setting per HTTP version.
- `PUBLIC_TLS_ORIGIN` and legacy flag aliases were removed.

See [REVERSE_PROXY.md](REVERSE_PROXY.md) for proxy behavior and examples.
