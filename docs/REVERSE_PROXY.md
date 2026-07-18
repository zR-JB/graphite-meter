# Reverse proxy deployment

A reverse proxy introduces two independent protocol hops:

```text
browser or TUI  <-- H1 / H2 / H3 -->  proxy  <-- usually clear H1 -->  Graphite Meter
```

Graphite Meter advertises the proxy origin once as `negotiated`. The browser reports its actual proxy-facing H1/H2/H3 protocol, while `/probe` separately reports what reached Graphite Meter. A proxy may own TLS, ALPN, HTTP/3 Alt-Svc, and ports 80/443 without changing the clear H1 upstream.

## Proxy-only deployment

```env
GM_ADVERTISED_NATIVE_ENDPOINTS=none
GM_PUBLIC_ORIGINS=self
GM_TRUSTED_PROXIES=172.30.0.0/24
```

Use `GM_PUBLIC_THROUGHPUT_ORIGINS` when the proxy cannot tunnel WebSockets. Use `GM_PUBLIC_LATENCY_ORIGINS` for a separate WebSocket origin. To expose deterministic native protocol tests as well, leave the desired native endpoints advertised and add `GM_PUBLIC_ORIGINS=self`.

The browser discovers everything through same-origin `/preflight`; it does not need a separate preflight URL. An advertised direct native origin is fetched cross-origin using Graphite Meter's CORS policy.

Browser WebSocket validation is based on the actual `HI`/`READY` exchange, not the HTTP version of an unrelated `/probe` request. The native TUI's WebSocket library uses HTTP/1.1 Upgrade, so its public proxy endpoint must accept H1 Upgrade even when normal proxy traffic negotiates H2.

## nginx

```nginx
location = /upload/progress {
    proxy_pass http://graphite-meter:7246;
    proxy_http_version 1.1;
    proxy_set_header Host $host;
    proxy_set_header X-Real-IP $remote_addr;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto $scheme;
    proxy_buffering off;
    proxy_cache off;
    gzip off;
}

location / {
    proxy_pass http://graphite-meter:7246;
    proxy_http_version 1.1;
    proxy_set_header Host $host;
    proxy_set_header X-Real-IP $remote_addr;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto $scheme;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
}
```

## Caddy

```caddyfile
meter.example {
    reverse_proxy graphite-meter:7246
}
```

Caddy can offer public H1/H2/H3 while using clear H1 upstream. Its Alt-Svc header describes Caddy's UDP 443 service; Graphite Meter's native H3 bootstrap remains independent.

## Measurement requirements

- WebSocket Upgrade/CONNECT must reach `/ws/ping`.
- `/upload/progress` is live NDJSON and must not be buffered, cached, compressed, or transformed.
- Preserve `Host` and standard forwarding headers.
- Set `GM_TRUSTED_PROXIES` only to proxy peers as seen by Graphite Meter. Trusted headers affect client identity and admission accounting, not endpoint discovery.
- Apply public connection, handshake, packet-rate, and bandwidth policy at the proxy or firewall. Application bandwidth throttling corrupts measurements.

The selected `/probe` reports normalized client address, address source, and the proxy-to-server protocol. The UI labels this separately from the browser-to-proxy protocol.

See [CONFIGURATION.md](CONFIGURATION.md) for the complete environment reference and native/hybrid examples.
