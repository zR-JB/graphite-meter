# Podman Quadlet units

Run Graphite Meter as a systemd-managed Podman service. Quadlet turns these
`.container` / `.build` files into generated systemd services.

[Deployment overview](../../docs/DEPLOYMENT.md) · [Native TLS](graphite-meter-tls/README.md) · [Tailscale](tailscale-sidecar/README.md)

Use the published image for a normal installation. Choose the source build only when you need
changes from a checkout. Commands below run from `container/quadlet`:

```sh
cd container/quadlet
```

Two ways to run it:

- **`graphite-meter.container` - the default.** Pulls the published release
  image (`ghcr.io/zr-jb/graphite-meter`); nothing is built locally, no checkout
  needed beyond this one file.
- **`graphite-meter-source.container` + `graphite-meter.build` - build from
  source.** For developers or custom builds. The `.container` references the
  `.build` unit via
  `Image=graphite-meter.build`, so a start builds first, then runs. Requires
  **Podman 5.0+** (`.build` unit support).

Two complete multi-unit deployments live in subdirectories:

- [`graphite-meter-tls/`](./graphite-meter-tls/) - all four native listeners on
  a public hostname, with a Let's Encrypt certificate issued and renewed by
  certbot over the Cloudflare DNS-01 challenge.
- [`tailscale-sidecar/`](./tailscale-sidecar/) - an isolated server reachable
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

## Alternative: build from source

### 1. Edit the build unit

Quadlet units live in a systemd search directory, not in the repo, so replace
both `/path/to/graphite-meter` occurrences in `graphite-meter.build` with your
checkout's absolute path: `SetWorkingDirectory=` (the build context) and the
`cd` in `ExecStartPre=` (which stamps the source revision). Keep
`File=container/Dockerfile`, and check `Arch=` (default arm64).

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
> and `graphite-meter-source.container` at the same time - both want the
> container name `graphite-meter` and the same host port.

## Enable authentication

Authentication is off by default. Uncomment the `GM_AUTH_*` block in
`graphite-meter.container`, point `GM_AUTH_PUBLIC_URL` at the exact public HTTPS
origin (no path, and no `:443`), and create the podman secrets the unit mounts.

```sh
podman run --rm -it ghcr.io/zr-jb/graphite-meter:latest hash-password
printf '%s' 'PASTE_THE_HASH_HERE' | podman secret create gm-auth-password-hash -
printf '%s' 'OIDC_CLIENT_SECRET' | podman secret create gm-auth-oidc-client-secret -  # OIDC or hybrid
```

The commented block assumes a reverse proxy (`GM_ADVERTISED_NATIVE_ENDPOINTS=none`,
`GM_PUBLIC_ORIGINS=self`); for direct native TLS advertise `http1-tls,http2,http3`
with each `GM_H*_PUBLIC_ORIGIN` on the same hostname. The image carries CA roots
for outbound OIDC calls. OIDC client registration, every variable and the proxy
headers are in [DEPLOYMENT.md](../../docs/DEPLOYMENT.md#authentication).

## Verify and maintain

```sh
systemctl --user status graphite-meter.service --no-pager
journalctl --user -u graphite-meter.service -f
```

For the source variant, substitute `graphite-meter-source.service`. The supplied `[Install]`
sections make the generated services start with the user manager; lingering keeps that manager
available after logout. Do not run `systemctl enable` on generated Quadlet services.
See [Podman's Quadlet documentation](https://docs.podman.io/en/latest/markdown/podman-systemd.unit.5.html#enabling-unit-files).

Upgrade native clients with the server and reload browser tabs; see
[upgrading](../../docs/DEPLOYMENT.md#upgrading).

## Build and networking

- Override the build identity (`VERSION`, `CLIENT_VERSION`,
  `GM_CLIENT_REVISION`) with the commented `BuildArg=` lines in
  `graphite-meter.build`.
- On rootless Podman, pasta user-mode networking can significantly limit
  measured throughput - uncomment `Network=host` in the `.container` unit for
  LAN tests that need to avoid that overhead. Host networking gives up the container network
  namespace; apply firewall policy on the host.
