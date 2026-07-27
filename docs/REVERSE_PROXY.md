# Reverse proxy deployment

A reverse proxy introduces two independent protocol hops:

```text
browser or TUI  <-- H1 / H2 / H3 -->  proxy  <-- usually clear H1 -->  Graphite Meter
```

Graphite Meter advertises the proxy origin once as `negotiated`. The browser reports its actual
proxy-facing H1/H2/H3 protocol, while `/probe` separately reports what reached Graphite Meter. A
proxy may own TLS, ALPN, HTTP/3 Alt-Svc, and ports 80/443 without changing the clear H1 upstream.

The server-side setting is small (full reference:
[CONFIGURATION.md](CONFIGURATION.md#advertised-endpoints-and-public-origins)):

```env
GM_ADVERTISED_NATIVE_ENDPOINTS=none
GM_PUBLIC_ORIGINS=self
GM_TRUSTED_PROXIES=172.30.0.0/24
```

Use `GM_PUBLIC_THROUGHPUT_ORIGINS` when the proxy cannot tunnel WebSockets, and
`GM_PUBLIC_LATENCY_ORIGINS` for a separate WebSocket origin. To expose deterministic native
protocol tests as well, leave the desired native endpoints advertised alongside
`GM_PUBLIC_ORIGINS=self`.

The browser discovers everything through same-origin `/preflight`. The native TUI's WebSocket
library uses HTTP/1.1 Upgrade, so its proxy endpoint must accept H1 Upgrade even when normal
proxy traffic negotiates H2.

WebTransport rides HTTP/3 extended CONNECT over UDP, which a TCP reverse proxy cannot carry.
Deployments that want WebTransport leave the native H3 endpoint advertised alongside
`GM_PUBLIC_ORIGINS=self`, exactly like the other native protocol tests, and open `7249/udp` to
the internet rather than to the proxy. Nothing breaks without it: discovery advertises mechanisms
side by side, and clients fall back per role to WebSocket latency and fetch throughput.

Browsers additionally need the page itself in a secure context before they expose the API at all,
so the proxy origin serving the UI has to be HTTPS — which it normally is, since the proxy is
usually what owns TLS. A proxy terminating TLS while serving the UI over plain http leaves
browsers with no WebTransport whatever the H3 endpoint advertises; the native client is
unaffected.

## nginx

Every directive below changes an nginx default that would otherwise break measurement or the
trust model; nothing is decorative.

```nginx
# Forward WebSocket upgrades; close upstream connections otherwise.
map $http_upgrade $connection_upgrade {
    default upgrade;
    ''      close;
}

server {
    # listen / TLS as usual …

    location / {
        proxy_pass http://graphite-meter:7246;

        # nginx speaks HTTP/1.0 upstream by default; WebSockets and streamed
        # responses need 1.1.
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection $connection_upgrade;

        # nginx sends $proxy_host as Host and no forwarding headers by default.
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header X-Forwarded-Host $http_host;

        # Client-supplied forwarding headers must not reach the server: its
        # trust model reads X-Real-IP from the proxy alone.
        proxy_set_header Forwarded "";
        proxy_set_header X-Forwarded-For "";

        # Response buffering is on by default and would batch the live
        # /upload/progress NDJSON stream.
        proxy_buffering off;
    }
}
```

Not needed on stock nginx: `proxy_cache` is off unless a cache zone is configured, and proxied
responses are not gzipped unless `gzip_proxied` says so.

## Caddy

Caddy streams responses, proxies WebSockets, and (since 2.5) sets `X-Forwarded-Proto` and
`X-Forwarded-Host` itself while distrusting incoming values from untrusted peers. What it does
not do by default:

```caddyfile
meter.example {
    reverse_proxy graphite-meter:7246 {
        # The address header the server trusts.
        header_up X-Real-IP {remote_host}
        # Caddy would append to X-Forwarded-For; the server wants client
        # forwarding headers removed entirely.
        header_up -Forwarded
        header_up -X-Forwarded-For
    }
}
```

Caddy's Alt-Svc header describes Caddy's own UDP 443 HTTP/3 service; Graphite Meter's native H3
bootstrap remains independent.

## Measurement requirements

- WebSocket Upgrade/CONNECT must reach `/ws/ping`.
- `/upload/progress` is live NDJSON and must not be buffered, cached, compressed, or transformed.
- Preserve `Host` and set the forwarding headers as above.
- Set `GM_TRUSTED_PROXIES` only to proxy peers as Graphite Meter sees them. Trusted headers
  affect client identity and admission accounting, not endpoint discovery.
- Apply connection, handshake, packet-rate, and bandwidth policy at the proxy or firewall.
  Application bandwidth throttling corrupts measurements.

With authentication enabled, the trusted proxy must set `X-Real-IP` from its connection peer,
remove `Forwarded` and `X-Forwarded-For`, and overwrite `X-Forwarded-Proto` and
`X-Forwarded-Host` — exactly what the examples above do. Its socket CIDR must be in
`GM_TRUSTED_PROXIES`, and the reconstructed origin must match `GM_AUTH_PUBLIC_URL` exactly.
Redact the `/auth/oidc/callback` query string from access and error logs. Do not add provider
`forward_auth`; Graphite Meter owns its OIDC redirect and session boundary.
