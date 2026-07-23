# Podman Quadlet units

Run Graphite Meter as a systemd-managed Podman service. Quadlet turns these
`.container` / `.build` files into generated systemd services.

Two ways to run it:

- **`graphite-meter.container` — the default.** Pulls the published release
  image (`ghcr.io/zr-jb/graphite-meter`); nothing is built locally, no checkout
  needed beyond this one file.
- **`graphite-meter-source.container` + `graphite-meter.build` — build from
  source.** For developers or custom builds (e.g. dummy engine / dev tools
  compiled in). The `.container` references the `.build` unit via
  `Image=graphite-meter.build`, so a start builds first, then runs. Requires
  **Podman 5.0+** (`.build` unit support).

Two complete multi-unit deployments live in subdirectories:

- [`graphite-meter-tls/`](./graphite-meter-tls/) — all four native listeners on
  a public hostname, with a Let's Encrypt certificate issued and renewed by
  certbot over the Cloudflare DNS-01 challenge.
- [`tailscale-sidecar/`](./tailscale-sidecar/) — an isolated server reachable
  only through its own Tailscale identity, with no published ports.

## Default: run the published image

```sh
mkdir -p ~/.config/containers/systemd
cp graphite-meter.container ~/.config/containers/systemd/
# allow the service to run without an active login session (start at boot):
loginctl enable-linger "$USER"
systemctl --user daemon-reload
systemctl --user start graphite-meter.service
```

Then open <http://localhost:7246>.

**Rootful:** copy to `/etc/containers/systemd/`, then
`sudo systemctl daemon-reload && sudo systemctl start graphite-meter.service`.

> The unit is named after the file: `graphite-meter.container` →
> `graphite-meter.service`.

Uncomment `AutoUpdate=registry` in the unit (and enable
`podman-auto-update.timer`) to pull new releases automatically, or pin a
release tag instead of `:latest` for reproducible deploys.

## Alternative: build from source

### 1. Edit the build unit

Quadlet units must live in a systemd search directory, **not** in the repo, so
relative paths to the checkout don't work. Set the context to your checkout's
absolute path in `graphite-meter.build`:

```ini
SetWorkingDirectory=/home/youruser/source/graphite-meter
```

(Keep `File=container/Dockerfile` — it's relative to that context. Also check
`Arch=` — the unit defaults to arm64, e.g. for a Raspberry Pi.)

### 2. Install the units

```sh
mkdir -p ~/.config/containers/systemd
cp graphite-meter.build graphite-meter-source.container ~/.config/containers/systemd/
loginctl enable-linger "$USER"
systemctl --user daemon-reload
systemctl --user start graphite-meter-source.service
```

> The build runs as `graphite-meter-build.service`; the container as
> `graphite-meter-source.service`. Don't install `graphite-meter.container`
> and `graphite-meter-source.container` at the same time — both want the
> container name `graphite-meter` and the same host port.

## Operate

```sh
systemctl --user status graphite-meter.service     # state (or -source)
journalctl --user -u graphite-meter.service -f     # logs
systemctl --user restart graphite-meter.service    # source variant: rebuilds only if inputs changed
```

## Enable authentication

Authentication is off by default. Uncomment the `GM_AUTH_*` block in
`graphite-meter.container`, point `GM_AUTH_PUBLIC_URL` at the exact public HTTPS
origin (no path, and no `:443`), and create the podman secrets the unit mounts.

For the operator password, print the Argon2id hash on a terminal and store the
single line it prints:

```sh
podman run --rm -it ghcr.io/zr-jb/graphite-meter:latest hash-password
printf '%s' 'PASTE_THE_HASH_HERE' | podman secret create gm-auth-password-hash -
```

For OIDC, store the client secret the same way, as
`gm-auth-oidc-client-secret`, and register
`${GM_AUTH_PUBLIC_URL}/auth/oidc/callback` as a confidential authorization-code
client using PKCE S256, `client_secret_basic`, and the scopes
`openid profile groups`. Discovery, token exchange, UserInfo, and JWKS are
outbound HTTPS calls; the published image carries
`/etc/ssl/certs/ca-certificates.crt`, so no CA bundle has to be mounted.

The server refuses to start unless clear HTTP/1.1 is left unadvertised and
every advertised origin is HTTPS on the `GM_AUTH_PUBLIC_URL` hostname. The
commented block covers a reverse-proxy deployment with
`GM_ADVERTISED_NATIVE_ENDPOINTS=none` and `GM_PUBLIC_ORIGINS=self`; serving the
native TLS listeners directly instead means advertising `http1-tls,http2,http3`
and giving each `GM_H*_PUBLIC_ORIGIN` that same hostname.

See [CONFIGURATION.md](../../docs/CONFIGURATION.md) for every variable and the
terminal-client grant flow, and [REVERSE_PROXY.md](../../docs/REVERSE_PROXY.md)
for the headers a trusted proxy must set.

## Notes

- **First source build is slow** on a Pi (bun install + the Go build); later
  starts reuse the built image.
- To rebuild from scratch: `podman rmi localhost/graphite-meter:latest` then
  restart the service.
- Override client build knobs (dummy runner / dev tools / label) by uncommenting
  the `BuildArg=` lines in `graphite-meter.build`.
- On rootless Podman, pasta user-mode networking can significantly limit
  measured throughput — uncomment `Network=host` in the `.container` unit for
  full-rate LAN tests.
