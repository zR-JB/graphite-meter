# Deployment

Graphite Meter ships as one static server binary with the browser app embedded. With no configuration it serves
clear HTTP/1.1 on port 7246, and native TLS listeners add HTTP/1.1 over TLS, HTTP/2, HTTP/3 and WebTransport.

| Setup                       | Start here                                                              | You need                                         |
| --------------------------- | ----------------------------------------------------------------------- | ------------------------------------------------ |
| Local or trusted LAN        | [One container](#one-container)                                         | TCP 7246.                                        |
| Linux without containers    | [Without Docker](#without-docker)                                       | Linux on amd64 or arm64, and systemd for a service. |
| Windows                     | [On Windows](#on-windows)                                               | Docker Desktop or WSL2.                          |
| Existing HTTPS ingress      | [Reverse proxies](#reverse-proxies) and [authentication](#authentication) | A hostname, a TLS proxy and its address.       |
| Direct protocol comparisons | [Native TLS](#native-tls)                                               | A trusted certificate; TCP 7247 to 7249, UDP 7249. |
| systemd under your own user | [Podman Quadlet](../container/quadlet/README.md)                        | Linux, Podman and systemd.                       |
| Private tailnet             | [Tailscale sidecar](../container/quadlet/README.md#tailscale-sidecar)   | A tailnet identity and HTTPS certificates.       |
| Current checkout            | [Build from source](#build-from-source)                                 | Git and Docker Compose, or the pinned toolchain. |

Pin `:X.Y.Z` or an image digest instead of `:latest` for reproducible deployments. The
[server reference](REFERENCE.md) lists every setting and limit; the terminal client has [its own guide](TERMINAL.md).
`container/` paths are relative to the repository root.

## One container

```sh
docker run -d --name graphite-meter --restart unless-stopped -p 7246:7246 ghcr.io/zr-jb/graphite-meter:latest
```

Open <http://localhost:7246>, or `http://SERVER_IP:7246` from another device. Over clear HTTP the test measures
HTTP/1.1 throughput and WebSocket latency. Browsers expose WebTransport only in a secure context, so remote
WebTransport needs HTTPS and the native HTTP/3 listener.

## Without Docker

Every release since 0.13.1 also attaches the server alone for Linux as
`graphite-meter-server_X.Y.Z_linux_amd64.tar.gz` and `graphite-meter-server_X.Y.Z_linux_arm64.tar.gz`. Each holds the
same static executable as the image, with its license notices, and needs nothing else installed. Download one with
`checksums.txt`, check it and start it:

```sh
curl -fLO https://github.com/zR-JB/graphite-meter/releases/download/vX.Y.Z/graphite-meter-server_X.Y.Z_linux_amd64.tar.gz
curl -fLO https://github.com/zR-JB/graphite-meter/releases/download/vX.Y.Z/checksums.txt
sha256sum --check --ignore-missing checksums.txt
tar -xzf graphite-meter-server_X.Y.Z_linux_amd64.tar.gz
./graphite-meter-server_X.Y.Z_linux_amd64/graphite-meter-server
```

It listens on port 7246 on every address and takes the same `GM_*` variables and flags as the image, listed in the
[server reference](REFERENCE.md#settings). For OIDC sign-in it finds the system CA bundle on Debian, Ubuntu, Fedora,
RHEL, openSUSE, Alpine and Arch without configuration.

To keep it running, install it, create an unprivileged user and a settings file, which may stay empty:

```sh
sudo install -m 755 graphite-meter-server_X.Y.Z_linux_amd64/graphite-meter-server /usr/local/bin/
sudo useradd --system graphite-meter
sudo install -d /etc/graphite-meter && sudo touch /etc/graphite-meter/env
```

Save this unit as `/etc/systemd/system/graphite-meter.service`, put `GM_*` lines in the settings file, and start it
with `sudo systemctl enable --now graphite-meter`:

```ini
[Unit]
Description=Graphite Meter
Wants=network-online.target
After=network-online.target

[Service]
User=graphite-meter
EnvironmentFile=/etc/graphite-meter/env
ExecStart=/usr/local/bin/graphite-meter-server
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

`journalctl -u graphite-meter` shows the log. The server writes no files, so the user needs only read access to the
TLS key and secret files you name. The default ports are above 1024 and need no privileges; only a port below 1024
needs `AmbientCapabilities=CAP_NET_BIND_SERVICE` in the unit. For HTTP/3 above about 1 Gbit/s, raise the
[UDP buffers](#udp-buffers).

### On Windows

The server ships for Linux only. On Windows, run the image with Docker Desktop, or run the Linux archive above under
WSL2. The [terminal client](TERMINAL.md#install) ships natively for Windows on amd64 and arm64.

## Docker Compose

The Compose files pull the published image; the `docker-compose.build.yml` overlay builds the checkout instead.

```sh
git clone https://github.com/zR-JB/graphite-meter.git
cd graphite-meter
docker compose -f container/docker-compose.yml up -d
```

### Native TLS

The TLS overlay issues no certificates. Supply a Let's Encrypt-style tree whose `live/` entries link into the same
mounted `archive/` tree:

```sh
export GM_PUBLIC_HOST=meter.example.com GM_CERT_NAME=meter.example.com GM_CERTIFICATE_TREE=/etc/letsencrypt
docker compose -f container/docker-compose.yml -f container/docker-compose.tls.yml up -d
```

It publishes TCP 7247 to 7249 and UDP 7249 and mounts the tree read-only. The key must be readable by the
[container user](#container-user). For issuance and renewal, see the
[TLS Quadlet](../container/quadlet/README.md#native-tls-with-certbot), which uses Cloudflare DNS-01.

Prefer an ECDSA P-256 key, which the TLS Quadlet issues with `certbot --key-type ecdsa`. Every TLS and QUIC
handshake signs with the key, and an RSA-2048 key costs about four times the CPU per handshake.

### Authentication overlay

[`docker-compose.auth.yml`](../container/docker-compose.auth.yml) shows password sign-in behind an HTTPS proxy on
the host, and its comments cover OIDC, hybrid and a proxy container. It needs Compose 2.24.4 or later. It publishes
7246 on `127.0.0.1` only and trusts only the network gateway the proxy's connections arrive from. Stack no public
ports on it, because loopback and IPv6 clients of a published port arrive from that gateway too.

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
mise run server-build && rust/target/release/graphite-meter-server                                # binary
```

Stop any container already bound to 7246 first. Source builds report a development version, and the binary build
says `UNREVIEWED DEVELOPMENT BUILD` because its license notices are unreviewed, so do not redistribute it. The
image's builder needs an amd64 host and cross-compiles arm64, so on arm64 hosts run a published tag.

### Container user

The image runs as the unprivileged user `65532:65532` and writes nothing. Mounted keys and secrets must be readable
by it:

- With Docker and native TLS, certbot keys are root `0600`. Give the group read once, and certbot keeps it on
  renewal.

  ```sh
  sudo chgrp -R 65532 /etc/letsencrypt/live /etc/letsencrypt/archive
  sudo chmod -R g+rX /etc/letsencrypt/live /etc/letsencrypt/archive
  ```

- Docker secrets need `sudo chown 65532:65532 FILE && sudo chmod 0400 FILE`.
- Quadlet needs nothing. Podman secrets are world-readable inside the container. Your user owns the TLS and
  Tailscale keys, and those units map it to the image user with `UserNS=keep-id:uid=65532,gid=65532`.

## Native listeners

Each listener has its own address and advertised origin, so a client picks a protocol by picking an address. The
clear HTTP/1.1 listener on `GM_H1_ADDR` is required and also serves the UI and discovery. The other listeners stay
off until they get an address. HTTP/3 serves WebTransport and needs its port open for both TCP and UDP end to
end. All TLS listeners share `GM_TLS_CERT` and `GM_TLS_KEY`. The server reloads a valid replacement certificate and
key on its own and keeps the previous pair through an incomplete renewal.

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

### Advertised measurement paths

`/preflight` lists the paths that carry throughput and latency:

- Native endpoints come from `GM_ADVERTISED_NATIVE_ENDPOINTS` and `GM_H*_PUBLIC_ORIGIN`. Graphite Meter owns the
  listener, so it knows the protocol. You can advertise only enabled listeners. Naming a disabled one is a startup
  error, and each public origin must match its scheme.
- Negotiated origins come from `GM_PUBLIC_ORIGINS` for both roles, `GM_PUBLIC_THROUGHPUT_ORIGINS` and
  `GM_PUBLIC_LATENCY_ORIGINS`, and they usually name a reverse proxy. The browser reports the protocol it reached,
  and the server reports what arrived upstream. `self` is the origin that served that server's discovery request.

An origin cannot be both native and negotiated, except as a latency-only origin. Browsers differ on clear HTTP
loopback from an HTTPS page, so advertise HTTPS paths for HTTPS deployments, including on `localhost`.

## Reverse proxies

A proxy adds a second protocol hop. The browser may reach the proxy over HTTP/2 or HTTP/3 while Graphite Meter sees
clear HTTP/1.1. Advertise the proxy as a negotiated origin, and keep native endpoints beside it if users should
choose both:

```env
GM_ADVERTISED_NATIVE_ENDPOINTS=none
GM_PUBLIC_ORIGINS=self
GM_TRUSTED_PROXIES=172.30.0.2/32
```

`GM_TRUSTED_PROXIES` is the proxy's own address as Graphite Meter sees it. Here it is a proxy container with a fixed
address. A trusted peer names the client, and with authentication it also names the HTTPS origin. So trust no
address that other clients share, because loopback and IPv6 clients of a published container port arrive from the
network gateway. `GM_ADVERTISED_NATIVE_ENDPOINTS=none` only stops discovery from offering the native listeners.
They still listen, so publish no ports for them when the proxy should be the only way in.

WebTransport is HTTP/3 extended CONNECT over UDP, which a TCP proxy cannot carry. Expose the native HTTP/3
endpoint directly when WebTransport is required.

### nginx

Merge this into your HTTPS server. The example omits certificate setup:

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

        proxy_buffering off;
        proxy_request_buffering off;
        proxy_read_timeout 300s;
        proxy_send_timeout 300s;
        client_max_body_size 0;
    }
}
```

In Nginx Proxy Manager, create a Proxy Host with WebSocket support on and caching off, and put a complete
`location / { ... }` block with the directives above under Advanced. Write
`proxy_set_header Connection $http_connection;` there, because the `map` cannot go there. Add no `/` Custom Location.
NPM then omits its default location, and that location's `$host` would drop a nonstandard port. Check the generated file
with `nginx -t`.

### Caddy

```caddyfile
meter.example {
    reverse_proxy graphite-meter:7246 {
        header_up X-Real-IP {remote_host}
    }
}
```

### Traefik

[`docker-compose.traefik.yml`](../container/docker-compose.traefik.yml) runs Traefik v3 in front of the default
Compose file. Traefik holds a Let's Encrypt certificate, and Graphite Meter is reachable only through it:

```sh
docker compose -f container/docker-compose.yml -f container/docker-compose.traefik.yml up -d
```

Traefik needs no header middleware. For a client outside `forwardedHeaders.trustedIPs` it drops the client's
`X-Real-Ip` and `X-Forwarded-*` headers, sets `X-Real-Ip` to the connection peer and appends the same address to
`X-Forwarded-For`. It sends a WebSocket upgrade's protocol as `wss`, which counts as HTTPS. Two settings exist only
in Traefik's static configuration, never in labels, and the overlay sets both as flags:

- `--entryPoints.websecure.transport.respondingTimeouts.readTimeout=0` is needed because Traefik's 60 s default
  cuts an upload stage that streams one request body for longer.
- `--serversTransport.forwardingTimeouts.idleConnTimeout=10s` is needed because Traefik keeps idle upstream
  connections for 90 s by default, but Graphite Meter closes them after 15 s.

With authentication, stack `docker-compose.auth.yml` before the Traefik overlay, and Traefik's address replaces the
gateway as the trusted proxy. Do not list Traefik's clients in `forwardedHeaders.trustedIPs` and do not set
`insecure`. Their own `X-Real-Ip` would then pass through, and Graphite Meter refuses those requests.

### Proxy requirements

- Preserve `Host`, overwrite `X-Forwarded-Proto` and `X-Forwarded-Host`, and set `GM_TRUSTED_PROXIES` to the proxy
  peers only. Without it every client counts as the proxy, and with authentication no request counts as HTTPS, so
  sign-in fails.
- Set `X-Real-IP` to the connection peer and replace any the client sent. `X-Forwarded-For` may stay, but its last
  entry must be the same address, as Traefik, Caddy and nginx's `$proxy_add_x_forwarded_for` write it. A different
  last entry shows the header was forged, and Graphite Meter refuses the request rather than attribute it to an
  address the client chose. The server ignores `Forwarded`.
- Allow WebSocket Upgrade to `/ws/ping`, and do not buffer, cache, compress or transform `/upload/progress`.
- Expire idle upstream connections within 15 s, because Graphite Meter closes them then.
- Keep the whole route family on one backend and add no `forward_auth`, because Graphite Meter owns authentication.
- Redact the `/auth/oidc/callback` query string from logs, and keep bandwidth policy off measurement routes.

## Authentication

Authentication is off by default. When enabled, it covers the UI, discovery, probes, transfers, progress,
WebSockets and WebTransport. Password mode needs one hash source. OIDC needs an issuer, a client ID, one secret
source and at least one allowed group. Hybrid needs both and keeps the password usable while the provider is down.

```sh
docker run --rm -it ghcr.io/zr-jb/graphite-meter:latest hash-password   # or: graphite-meter-server hash-password
```

Register `${GM_AUTH_PUBLIC_URL}/auth/oidc/callback` as a confidential authorization-code client with PKCE S256,
`client_secret_basic` and scopes `openid profile groups`. OIDC mode refuses to start until issuer discovery
succeeds, and hybrid retries it in the background.

Advertised origins must use the authentication hostname, but their ports may differ. Clear HTTP/1.1 cannot be
advertised. The default `GM_ADVERTISED_NATIVE_ENDPOINTS=all` includes it, so set `none` behind a proxy or
`http1-tls,http2,http3`, or the server refuses to start.

Sessions, device cookies and rate limits are listed under [sign-in limits](REFERENCE.md#sign-in-limits).

The [terminal client](TERMINAL.md#sign-in) never sees the operator password. An operator approves its short code in
the browser, and the client receives an in-memory, measurement-only grant bound to that login and HTTPS origin.
Sign-out revokes the grant.

## Server catalog

A test runs against one to four selected test servers on one stage schedule, and the default is the server that
serves the page. Every server runs the same software and configures itself. Servers never contact each other.
Clients connect directly to each selected server and never import a peer's catalog, so there are no shared keys,
database or discovery service.

Set `GM_SERVER_CATALOG` to inline JSON, or set `GM_SERVER_CATALOG_FILE` to an absolute path that is normally
mounted read-only, and restart. Usually a list of the other servers' origins is enough:

```sh
export GM_SERVER_CATALOG='["https://fra.example.net","https://ams.example.net"]'
```

Each server's name, location and paths come from its own `/preflight`, and until discovery succeeds the client
shows the hostname. Do not list `/preflight` or `/probe` paths. The object form adds named IDs, a default selection
and extra measurement hosts, within the [catalog rules](REFERENCE.md#server-catalog):

```json
{
  "defaultSelection": ["self"],
  "servers": [
    { "id": "frankfurt", "url": "https://fra.example.net", "name": "Frankfurt" },
    {
      "id": "amsterdam",
      "url": "https://ams.example.net",
      "name": "Amsterdam",
      "additionalOrigins": ["https://transfer.ams.example.net:8443"]
    }
  ]
}
```

The page derives its browser connection policy from these destinations, so discovery cannot authorize an
unrelated host. Proxies must keep that policy and allow streaming and `Authorization` headers. Protected servers
accept measurement grants only on their own hostname. For Compose, edit
[the example](../container/server-catalog.example.json), save it as `container/server-catalog.json` and add
[the catalog overlay](../container/docker-compose.catalog.yml).

Clients use the catalog as follows:

- Browsers save a choice as ID plus canonical origin. A removed or changed entry needs the user to apply a new
  selection, and an ID never silently redirects to another server. Listing a server does not make it reachable,
  because a catalog may mix public, intranet and LAN servers. Unselected unavailable entries do not block a test,
  and an unavailable selected entry fails by name until access returns or the user deselects it.
- HTTP and HTTPS servers may be mixed, and Automatic resolves each server's advertised paths separately. From an
  HTTPS page the browser blocks non-loopback HTTP paths as mixed content, and protected servers need HTTPS on both
  the page and the server. The terminal client has no mixed-content rule.
- When a hosted page reaches a private-address server, or an intranet page reaches loopback, the browser may ask
  for local-network permission. Chrome requires a secure page for it. Chromium's connection policy cannot match literal
  IPv6 hosts, so use a DNS name for a separate IPv6 origin. The terminal client accepts literals.
- In the browser, Settings lists the test servers with their status and discovery time, and you select one to four.
  Each selected server is probed for latency. The latency lanes show one server at a time and switch mid-run, and
  the results selector shows all servers or one. A protected server shows Sign in, which opens that server's own
  login in a popup followed by an approval page. Approve only if its eight-character code matches the one in
  Settings. The grant lives in page memory and is bound to that server, the requesting origin and the login.
  Reloading the page asks again, and signing out on the server revokes the grant. A server that fails is dropped
  while the others continue, as [coordinated servers](MEASUREMENTS.md#coordinated-servers) explains.
- The terminal client takes `--url` as the catalog and repeatable `--server ID`; see its
  [flags](TERMINAL.md#flags).

## Podman and Quadlet

See the [Quadlet guide](../container/quadlet/README.md). Rootless userspace networking can cap throughput.
`Network=host` avoids that but gives up the network namespace, so apply host firewall policy.

## Troubleshooting

| Symptom                                    | Check                                                                                       |
| ------------------------------------------ | ------------------------------------------------------------------------------------------- |
| Another device cannot open the page        | Use the server IP, not `localhost`; publish and allow TCP 7246.                             |
| WebTransport is unavailable                | Check for an HTTPS page, a trusted certificate, browser support, an advertised HTTP/3 origin and TCP and UDP reachability. |
| An advertised path fails validation        | The origin must be reachable with the right scheme, port and certificate hostname.          |
| A local peer fails only from a hosted page | Check local-network permission and HTTPS; see the [server catalog](#server-catalog).        |
| A browser IPv6 peer needs a hostname       | Use a DNS name for IPv6 outside the page's origin; the terminal client accepts literals.     |
| Connections from one address are refused   | The `admission` warning names it; set `GM_TRUSTED_PROXIES` for a proxy, or raise `GM_MAX_CONNECTIONS_PER_CLIENT` for clients sharing an address. |
| Uploads fail behind a proxy                | Disable request buffering and body-size limits; allow streaming progress and long requests. |
| Throughput is lower than expected          | Check CPU, browser, Wi-Fi, proxy and container networking, and compare the terminal client on a direct listener. |
| Timeouts or missing values appear          | Inspect stage evidence, because unfinished probes and missing receiver counters are not zero. |

### UDP buffers

The HTTP/3 listener asks the kernel for 7 MiB of UDP buffer per direction, split among its sockets with at least
2 MiB each. Linux caps an unprivileged request at `net.core.rmem_max` and `net.core.wmem_max`, and the server then
logs one `udp` warning at startup that reads `UDP buffer too small for QUIC above about 1 Gbit/s`. Below about
1 Gbit/s of QUIC the smaller buffer is enough. Above it, bursts can overflow the socket and lower HTTP/3 and
WebTransport results. Raise the caps on the host, which also covers containers, and add both keys to a file under
`/etc/sysctl.d/` to keep them after a reboot:

```sh
sysctl --write net.core.rmem_max=7500000 net.core.wmem_max=7500000
```
