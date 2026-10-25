# Server reference

Every server command, setting and limit. The [deployment guide](DEPLOYMENT.md) shows how to use them, and the
terminal client has [its own flags](TERMINAL.md#flags).

## Commands

The server executable is `/graphite-meter` in the image and `graphite-meter-server` in a release archive or a
source build. Besides serving, it takes these commands:

| Command                | Effect                                                                                              |
| ---------------------- | --------------------------------------------------------------------------------------------------- |
| `version`, `--version` | Print the version.                                                                                  |
| `hash-password`        | Read a password twice on stdin (1 to 1024 bytes, hidden on a terminal) and print its Argon2id hash. |
| `--legal`              | Print the project and dependency license notices.                                                   |
| `--help`               | Print every flag with its variable and default.                                                     |

## Settings

The environment loads first, and a flag overrides it. Rows marked _env only_ have no flag, so secrets stay out of
process arguments. Listen addresses take `:port` or `host:port`, durations take units such as `90s`, `5m` or `2h`,
and booleans take `true`, `false`, `1` or `0`. A configuration error stops the server with exit status 1 and names
the setting.

| Environment                             | Flag                                   | Default          | Meaning                                                                                |
| --------------------------------------- | -------------------------------------- | ---------------- | -------------------------------------------------------------------------------------- |
| `GM_H1_ADDR`                            | `--h1-addr`                            | `:7246`          | Clear HTTP/1.1 listen address; required.                                               |
| `GM_H1_TLS_ADDR`                        | `--h1-tls-addr`                        | empty            | HTTPS HTTP/1.1 address; empty disables.                                                |
| `GM_H2_ADDR`                            | `--h2-addr`                            | empty            | HTTP/2 TLS address; empty disables.                                                    |
| `GM_H3_ADDR`                            | `--h3-addr`                            | empty            | HTTP/3 UDP and bootstrap TCP address; empty disables.                                  |
| `GM_TLS_CERT` / `GM_TLS_KEY`            | `--tls-cert` / `--tls-key`             | empty            | PEM paths; required when any TLS listener is enabled. The certificate must be current and name the host of every HTTPS public origin. |
| `GM_H1_PUBLIC_ORIGIN`                   | `--h1-public-origin`                   | empty            | Public `http://` origin of the clear listener.                                         |
| `GM_H1_TLS_PUBLIC_ORIGIN`               | `--h1-tls-public-origin`               | empty            | Public `https://` origin of the HTTPS HTTP/1.1 listener.                               |
| `GM_H2_PUBLIC_ORIGIN`                   | `--h2-public-origin`                   | empty            | Public `https://` origin of the HTTP/2 listener.                                       |
| `GM_H3_PUBLIC_ORIGIN`                   | `--h3-public-origin`                   | empty            | Public `https://` origin of the HTTP/3 listener.                                       |
| `GM_ADVERTISED_NATIVE_ENDPOINTS`        | `--advertised-native-endpoints`        | `all`            | `all`, `none` or a subset of `http1-clear,http1-tls,http2,http3`.                      |
| `GM_PUBLIC_ORIGINS`                     | `--public-origins`                     | empty            | Negotiated origins (or `self`) for throughput and latency.                             |
| `GM_PUBLIC_THROUGHPUT_ORIGINS`          | `--public-throughput-origins`          | empty            | Negotiated throughput-only origins (or `self`).                                        |
| `GM_PUBLIC_LATENCY_ORIGINS`             | `--public-latency-origins`             | empty            | WebSocket latency-only origins (or `self`).                                            |
| `GM_SERVER_NAME`                        | `--name`                               | `graphite-meter` | Name in `/preflight` and clients; at most 256 bytes, no control characters.            |
| `GM_SERVER_LOCATION`                    | `--location`                           | empty            | Location label, with the same limits.                                                  |
| `GM_RESULT_HISTORY_DEFAULT`             | `--result-history-default`             | `false`          | Default for saving completed browser results on the device.                            |
| `GM_VERBOSE`                            | `--verbose`                            | `false`          | Log throughput every second, admission counters every 30 s and sign-in debug lines.    |
| `GM_MAX_ACTIVE_MEASUREMENTS`            | `--max-active-measurements`            | `256`            | Measurements running at once: downloads, uploads, latency connections and WebTransport sessions. |
| `GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT` | `--max-active-measurements-per-client` | `32`             | Measurements per client identity, its leases' included; the most one lease holds.      |
| `GM_MAX_ACTIVE_SESSIONS`                | `--max-active-sessions`                | `64`             | WebTransport sessions, which also count as measurements.                               |
| `GM_MAX_SESSIONS_PER_CLIENT`            | `--max-sessions-per-client`            | `8`              | WebTransport sessions per client identity; the most one lease holds.                   |
| `GM_MAX_BUFFER_BYTES`                   | `--max-buffer-bytes`                   | `8589934592`     | Bytes of HTTP/2 and QUIC connection buffers ([resources](#resources)).                 |
| `GM_MAX_CONNECTIONS`                    | `--max-connections`                    | `4096`           | Concurrent TCP and QUIC connections.                                                   |
| `GM_MAX_CONNECTIONS_PER_CLIENT`         | `--max-connections-per-client`         | `64`             | Connections per direct client; a trusted proxy counts only toward `GM_MAX_CONNECTIONS`. |
| `GM_MAX_OPERATION_DURATION`             | `--max-operation-duration`             | `6m`             | Longest HTTP download or upload and longest latency connection; unset or empty, at least `GM_MAX_STAGE_DURATION` plus 1 minute. |
| `GM_MAX_SESSION_DURATION`               | `--max-session-duration`               | `2h`             | Longest WebTransport download or upload session; unset or empty, at least the operation lifetime. |
| `GM_MAX_STAGE_DURATION`                 | `--max-stage-duration`                 | `5m`             | Longest stage clients may plan, `1s` to `24h`; sent in `/preflight`.                   |
| `GM_TRUSTED_PROXIES`                    | _env only_                             | empty            | Proxy CIDRs allowed to supply `X-Real-IP`, `X-Forwarded-Proto` and `X-Forwarded-Host`. |
| `GM_AUTH_MODE`                          | `--auth-mode`                          | `off`            | `off`, `password`, `oidc` or `hybrid`.                                                 |
| `GM_AUTH_PUBLIC_URL`                    | `--auth-public-url`                    | empty            | Canonical HTTPS UI origin, no path or `:443`.                                          |
| `GM_AUTH_PASSWORD_HASH`                 | _env only_                             | empty            | Inline Argon2id PHC hash; prefer the file.                                             |
| `GM_AUTH_PASSWORD_HASH_FILE`            | `--auth-password-hash-file`            | empty            | File with one Argon2id PHC hash.                                                       |
| `GM_AUTH_OIDC_ISSUER`                   | `--auth-oidc-issuer`                   | empty            | HTTPS issuer URL.                                                                      |
| `GM_AUTH_OIDC_CLIENT_ID`                | `--auth-oidc-client-id`                | empty            | Confidential client ID.                                                                |
| `GM_AUTH_OIDC_CLIENT_SECRET`            | _env only_                             | empty            | Inline client secret; prefer the file.                                                 |
| `GM_AUTH_OIDC_CLIENT_SECRET_FILE`       | `--auth-oidc-client-secret-file`       | empty            | File with the client secret.                                                           |
| `GM_AUTH_OIDC_ALLOWED_GROUPS`           | `--auth-oidc-allowed-groups`           | empty            | Groups allowed to sign in, comma-separated and case-sensitive; required for OIDC.      |
| `GM_AUTH_OIDC_PROVIDER_NAME`            | `--auth-oidc-provider-name`            | `Authelia`       | Sign-in page label, at most 64 bytes.                                                  |
| `GM_SERVER_CATALOG`                     | _env only_                             | empty            | [Server catalog](#server-catalog) JSON.                                                |
| `GM_SERVER_CATALOG_FILE`                | _env only_                             | empty            | Absolute catalog file path without `..`; exclusive with the inline form.               |
| `TZ`                                    | _env only_                             | UTC              | IANA zone name for [log times](#logs), from zone data built into the release.          |
| `HTTPS_PROXY`, `HTTP_PROXY`, `NO_PROXY` | _env only_                             | empty            | Proxy for OIDC requests, read as the [terminal client](TERMINAL.md#environment) reads them. |
| `SSL_CERT_FILE`, `SSL_CERT_DIR`         | _env only_                             | the system's     | CA roots for OIDC requests. `SSL_CERT_FILE` replaces the system bundle and `SSL_CERT_DIR`, a `:`-separated list, the system directories; each one unset keeps the system's. |

- Listener addresses must differ. Numeric limits must be positive, per-client limits may not exceed their global
  limit, sessions may not exceed measurements, and the session duration must be at least the operation duration.
  An unset or empty operation lifetime is at least the stage limit plus a minute, so a server that allows
  multi-hour stages never cuts their transfers short, and an unset or empty session lifetime is at least the
  operation lifetime. Set them to cut transfers sooner, and clients reconnect across each cut. Something must carry
  throughput, so with no native endpoint advertised, set `GM_PUBLIC_ORIGINS` or `GM_PUBLIC_THROUGHPUT_ORIGINS`, or
  startup fails with "configuration advertises no throughput endpoint".
- A native listener is advertised on the host the client asked for, at the listener's own port. Set its
  `GM_H*_PUBLIC_ORIGIN` when clients reach it on another host or port, such as a remapped container port. HTTP/3
  uses that port for both TCP and UDP, so a remap must publish both protocols on it.
- A client identity is a login or measurement grant, and its subject shares twice its limit. Every password login
  is the one operator subject. Without a login, the identity is an IPv4 address or an IPv6 /64, and its /56 and
  /48 share two and four times its limit. Upload receivers are counted the same way, and connections are counted
  by address.
- Connections count TCP and QUIC alike. An idle QUIC connection ends without a close at its 30 s idle timeout, so
  Firefox keeps the server's HTTP/3.
- Graphite Meter never throttles measured traffic. Public deployments need authentication or connection policy at
  a trusted proxy or firewall.
- `GM_TRUSTED_PROXIES` rejects the default routes `0.0.0.0/0` and `::/0`. A trusted peer names its client with
  exactly one `X-Real-IP`, and any `X-Forwarded-For` must end in that address. Measurement admission refuses any
  other request from the peer with HTTP 400, the sign-in page says the proxy names no client, and the server logs
  the fault under `proxy` at most once a minute.
- The browser keeps History in its own IndexedDB, and its own choice overrides `GM_RESULT_HISTORY_DEFAULT`. Stopped
  and Failed runs are not saved.

## Sign-in limits

Browser sessions are HTTPS-only and last eight hours. A subject holds at most 8 sessions, and the server holds at
most 1024.

Sign-in is rate limited per client address. An IPv6 client counts as its /64, and its /56 and /48 share two and
four times the limit. Each address gets 5 password attempts, 10 OIDC code exchanges, 10 OIDC sign-in starts and 10
sign-in approval pages per minute, and all clients together get at most 60 wrong passwords per minute. A password
sign-in also leaves a 30-day device cookie. The cookie is signed with a key derived from the password hash, so
changing the password forgets every device. A browser holding the cookie keeps its own address's limit but skips
the bounds all clients share, so others' attempts cannot lock a known operator out.

## Server catalog

`GM_SERVER_CATALOG` takes inline JSON and `GM_SERVER_CATALOG_FILE` an absolute path; the
[deployment guide](DEPLOYMENT.md#server-catalog) has examples. Either form is a list of origins or an object:

| Field               | Rule                                                                                    |
| ------------------- | --------------------------------------------------------------------------------------- |
| `servers`           | Holds up to 31 entries besides the synthesized `self`, which you omit. IDs and origins must be unique. |
| `id`                | Has 1 to 64 ASCII letters, digits, `.`, `_` or `-`.                                     |
| `url`               | Is an HTTP(S) origin of at most 2048 bytes with no credentials, path, query or fragment. |
| `name`, `location`  | Are fallbacks of at most 256 UTF-8 bytes without control characters; discovery supplies current values. |
| `additionalOrigins` | Lists up to 32 exact origins on other hosts; ports on the entry's own hostname need none. |
| `defaultSelection`  | Names one to four existing IDs; omitted means `self`, and empty is invalid.             |

The raw configuration is limited to 64 KiB. The normalized catalog adds this server's own entry and is limited to
48 KiB. IDs derive from the canonical origin, so reordering keeps them and changing the origin creates a new one.

## Resources

`GM_MAX_BUFFER_BYTES` defaults to 8 GiB and bounds the connection buffers the HTTP/2 and HTTP/3 listeners share.
It must cover every allowed connection's floor. That floor is 1.5 MiB with the HTTP/2 listener on, about 0.5 MiB
with HTTP/3 alone and none for HTTP/1.1. The default `GM_MAX_CONNECTIONS` of 4096 therefore needs up to 6 GiB. A
small host lowers both, for example to `GM_MAX_BUFFER_BYTES=1073741824` with `GM_MAX_CONNECTIONS=512`. A budget
that falls short stops the server at start with the values that fit.

Connections run on one thread per runtime worker, and by default there is one worker per CPU. Tokio's own
`TOKIO_WORKER_THREADS` sets that count on a host shared with other services. On Linux those threads run five nice
steps lower, so transfers yield to the host's other services and to the server's own probes.

## Logs

The server logs to stderr and writes one line per event in UTC, or in the zone `TZ` names:

```text
2026-10-06T14:31:02Z INFO  server:    ready
2026-10-06T14:31:09Z WARN  tls:       certificate expires in 72 h; renew it (transport::tls)
```

Each line has a level of `DEBUG`, `INFO`, `WARN` or `ERROR`, then a topic and the message. Warnings and errors end
with the module that wrote them. Lines a peer can repeat, such as failed handshakes, appear at most once a minute
with a count of those held back. Connections refused at a client's limit are logged once a minute per client, so an
address many users share shows up by name. On a terminal the lines are colored unless `NO_COLOR` is set or
`TERM=dumb`, and `FORCE_COLOR` colors them anywhere.

`TZ` takes an IANA zone name such as `Europe/Berlin`, and each line then carries the offset in effect at that time,
such as `2026-10-06T16:31:02+02:00`. The zone data is built into the server, so it needs no mounts and is as
current as the release. The server refuses to start on any other `TZ` value, such as a POSIX rule or a path. Only
log lines change; cookies, HTTP dates and certificates stay UTC.
