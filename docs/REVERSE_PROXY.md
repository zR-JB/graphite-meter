# Reverse proxy deployment

Graphite Meter accepts proxy forwarding headers only when the request's socket
peer matches `GM_TRUSTED_PROXIES`. The variable is a comma-separated list of
CIDRs and defaults to empty. Use the smallest network containing the proxy;
never add a public client network just to make forwarding headers work.

```sh
GM_TRUSTED_PROXIES=172.30.0.0/24,127.0.0.1/32,::1/128
```

Invalid CIDRs stop the server at startup. For a trusted peer, the server uses
`Forwarded`, then `X-Forwarded-For`, then `X-Real-IP`. It walks an address chain
from right to left across trusted hops and selects the first untrusted address.
Malformed, `unknown`, and obfuscated values fall back to the socket peer. IPv4,
IPv6, optional ports, quoted/bracketed values, and IPv4-mapped IPv6 are
normalized with Go's `net/netip` package.

The resolved address is diagnostic evidence only. It is never used for
authentication or authorization. The selected target's `/probe` reports the normalized address,
its family, and whether it came from the socket or a trusted header. A proxy can
translate between IPv4 and IPv6 or traverse an overlay/load balancer, so the
reported family is not guaranteed to describe every physical segment.

`/preflight` is logical discovery and does not claim which protocol a later target uses. Set
`PUBLIC_H1_ORIGIN`, `PUBLIC_H1_TLS_ORIGIN`, `PUBLIC_H2_ORIGIN`, and `PUBLIC_H3_ORIGIN` to origins
the browser can reach. Any explicit public origin is advertised even when its corresponding native
listener is disabled: the public target may be a proxy forwarding to the server's H1 listener.
Native listener flags control local binds, not external target discovery.

An advertised H1-TLS target must negotiate HTTP/1.1 at the browser-facing hop. Give it a dedicated
port or virtual host whose ALPN policy does not offer H2. Advertising the same H2-enabled origin as
both `PUBLIC_H1_TLS_ORIGIN` and `PUBLIC_H2_ORIGIN` does not let JavaScript force H1; Graphite Meter
will correctly reject the H1-TLS selection when Resource Timing reports `h2`.
A conventional HTTP reverse proxy changes the measured hop: Resource Timing then describes the
browser-to-proxy protocol, while `/probe` describes the proxy-to-server protocol. Directly expose
the native H2 TCP and H3 TCP+UDP ports when the goal is to compare those server listeners.

`GET /upload/progress` is a live `application/x-ndjson` response. Proxies must flush it as records
arrive and must not compress, transform, cache, or buffer it. The server sends `Cache-Control:
no-store, no-transform` and `X-Accel-Buffering: no`, but the proxy
configuration remains authoritative. A deployment is not measurement-safe until `ready` and
roughly 100 ms progress records are observable immediately at the public origin.

## nginx

```nginx
location = /upload/progress {
    proxy_pass http://graphite-meter:8765;
    proxy_http_version 1.1;
    proxy_buffering off;
    proxy_cache off;
    gzip off;
}

location / {
    proxy_pass http://graphite-meter:8765;
    proxy_http_version 1.1;
    proxy_set_header Host $host;
    proxy_set_header X-Real-IP $remote_addr;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto $scheme;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
}
```

Set `GM_TRUSTED_PROXIES` to the nginx container/host CIDR as seen by Graphite
Meter, not to the browser's network.

## Caddy

```caddyfile
speed.example.com {
    reverse_proxy graphite-meter:8765
}
```

Caddy supplies `X-Forwarded-For`, `X-Forwarded-Proto`, and `X-Forwarded-Host`
to its upstream. Trust only the Caddy container/host CIDR in Graphite Meter.

## Traefik

```yaml
labels:
  - traefik.enable=true
  - traefik.http.routers.graphite-meter.rule=Host(`speed.example.com`)
  - traefik.http.services.graphite-meter.loadbalancer.server.port=8765
```

Traefik supplies the standard `X-Forwarded-*` headers to its upstream. Trust
only the Traefik container/host CIDR. If another load balancer sits before it,
add that balancer's CIDR only when Traefik is also configured to trust and
sanitize that hop.
