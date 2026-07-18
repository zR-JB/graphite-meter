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

## Breaking migration

- Replace `GM_ENABLE_H1_TLS=true` with a non-empty `GM_H1_TLS_ADDR`.
- Replace `GM_ENABLE_H2=true` with a non-empty `GM_H2_ADDR`.
- Replace `GM_ENABLE_H3=true` with a non-empty `GM_H3_ADDR`.
- Replace `PUBLIC_H*_ORIGIN` with the corresponding `GM_H*_PUBLIC_ORIGIN` only for deterministic native listeners.
- Advertise ordinary reverse proxies with `GM_PUBLIC_ORIGINS`, not one setting per HTTP version.
- `PUBLIC_TLS_ORIGIN` and legacy flag aliases were removed.

See [REVERSE_PROXY.md](REVERSE_PROXY.md) for proxy behavior and examples.
