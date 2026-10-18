# Podman Quadlet

Quadlet turns the `.container` files here into systemd services. This directory holds three layouts. The first
runs the published image on one port, the second runs the same image on every native TLS listener with a Let's
Encrypt certificate, and the third runs an isolated server that is reachable only over Tailscale. The
[deployment guide](../../docs/DEPLOYMENT.md) covers variables, sign-in and reverse proxies.

Generated Quadlet services are started, never enabled, because their `[Install]` sections already apply at
generation. `loginctl enable-linger "$USER"` keeps a user's services running after logout.

## The published image

```sh
mkdir -p ~/.config/containers/systemd
cp container/quadlet/graphite-meter.container ~/.config/containers/systemd/
loginctl enable-linger "$USER"
systemctl --user daemon-reload
systemctl --user start graphite-meter.service
```

Open <http://localhost:7246>. For a rootful service, copy the unit to `/etc/containers/systemd/` and use
`sudo systemctl` without `--user`.

On rootless Podman, pasta user-mode networking limits measured throughput, so uncomment `Network=host` in the unit
for LAN tests. Host networking gives up the container's network namespace, so apply firewall policy on the host.

To require sign-in, uncomment the `GM_AUTH_*` block in the unit, set `GM_AUTH_PUBLIC_URL` to the exact public HTTPS
origin (no path, no `:443`) and create the secrets the unit mounts:

```sh
podman run --rm -it ghcr.io/zr-jb/graphite-meter:latest hash-password
printf '%s' 'PASTE_THE_HASH_HERE' | podman secret create gm-auth-password-hash -
printf '%s' 'OIDC_CLIENT_SECRET' | podman secret create gm-auth-oidc-client-secret -  # OIDC or hybrid
```

The block assumes a reverse proxy, with `GM_ADVERTISED_NATIVE_ENDPOINTS=none` and `GM_PUBLIC_ORIGINS=self`. Publish
7246 on `127.0.0.1` only, or use `Network=host`, and list only the proxy's own address in `GM_TRUSTED_PROXIES`,
because a trusted peer names the client and the HTTPS origin. The image carries CA roots for the outbound OIDC
calls.

```sh
systemctl --user status graphite-meter.service --no-pager
journalctl --user -u graphite-meter.service -f
```

## Native TLS with certbot

`graphite-meter-tls/` holds three units. `certbot-bootstrap` issues a Let's Encrypt certificate once over the
Cloudflare DNS-01 challenge, `certbot-renew` keeps it fresh, and `graphite-meter` serves every native listener
with it on the host's network. You need a public hostname, an API token that may edit its Cloudflare zone
(`Zone:DNS:Edit`, see the [certbot plugin](https://certbot-dns-cloudflare.readthedocs.io/en/stable/#credentials)),
and TCP 7247 to 7249 plus UDP 7249 open. The DNS record must be DNS only (gray cloud), because Cloudflare's proxy
does not pass these ports. Do not install these units beside another Quadlet named `graphite-meter`.

Quadlet discovers units recursively and the units reference their siblings by relative path, so install the whole
directory and keep the populated files in the installed copy, not in the checkout:

```sh
install -d -m 700 ~/.config/containers/systemd
cp -r container/quadlet/graphite-meter-tls ~/.config/containers/systemd/
cd ~/.config/containers/systemd/graphite-meter-tls
mkdir -p letsencrypt log
cp secrets/cloudflare.ini.example secrets/cloudflare.ini
chmod 600 secrets/cloudflare.ini
$EDITOR secrets/cloudflare.ini graphite-meter.env   # change every hostname-derived line together
loginctl enable-linger "$USER"
systemctl --user daemon-reload
systemctl --user start certbot-bootstrap.service
systemctl --user start certbot-renew.service graphite-meter.service
```

Verify with `systemctl --user status certbot-bootstrap certbot-renew graphite-meter`,
`podman exec graphite-meter-certbot-renew certbot certificates` and `curl -v https://HOST:7247/`.
`curl --http3-only -v https://HOST:7249/probe` checks HTTP/3 from a capable curl.

`certbot-bootstrap` uses a stable `--cert-name` with `--keep-until-expiring`, so restarting it reuses the lineage.
`certbot-renew` runs `certbot renew` at start and every 12 hours and updates the `live/` symlinks atomically. The
server reloads the certificate and key itself, so there is no deploy hook. To test renewal against Let's Encrypt's
staging environment without using up rate limits, run:

```sh
podman exec graphite-meter-certbot-renew certbot renew --dry-run --dns-cloudflare-credentials /run/secrets/cloudflare.ini
```

`graphite-meter.env` advertises only the TLS listeners, so 7246/tcp can stay closed, and with `Network=host` there is
no `PublishPort=`. To change the hostname, stop all three units, update the env file, and either keep the old
certificate lineage or delete it with `certbot delete --cert-name OLD_NAME`. Never edit `letsencrypt/renewal`, `live`
or `archive` by hand.

## Tailscale sidecar

`tailscale-sidecar/` runs the image inside a Tailscale container's network namespace. The sidecar owns the tailnet
identity and the HTTPS certificate, nothing is published on the host, and the listeners take the standard web ports
instead of 7246 to 7249.

| Traffic                  | Listener   |
| ------------------------ | ---------- |
| HTTP/1.1 clear           | `80/tcp`   |
| HTTP/1.1 TLS, UI and WSS | `443/tcp`  |
| HTTP/2 measurement       | `8443/tcp` |
| HTTP/3 bootstrap         | `8444/tcp` |
| HTTP/3 measurement       | `8444/udp` |

You need Linux with `/dev/net/tun`, systemd, a recent Podman, tailnet HTTPS certificates and policy access to those
ports. Set `TS_AUTHKEY`, `TS_HOSTNAME` and `TS_CERT_DOMAIN` in the Tailscale env file, put the same certificate
domain in the Graphite Meter env file in place of `graphite-meter.example-tailnet.ts.net`, and adjust or drop the
example tag to match the tailnet policy. Populated `.env` files are ignored by Git.

```sh
cd container/quadlet/tailscale-sidecar
cp graphite-meter-tailnet.env.example graphite-meter-tailnet.env
cp graphite-meter-tailnet-tailscale.env.example graphite-meter-tailnet-tailscale.env
chmod 600 graphite-meter-tailnet*.env
$EDITOR graphite-meter-tailnet*.env
install -d -m 700 ~/.config/containers/systemd
install -m 644 *.container *.volume ~/.config/containers/systemd/
install -m 600 graphite-meter-tailnet*.env ~/.config/containers/systemd/
loginctl enable-linger "$USER"
systemctl --user daemon-reload
systemctl --user start graphite-meter-tailnet-tailscale.service
```

Starting the sidecar starts Graphite Meter after the certificate check. `podman port graphite-meter-tailnet` must print
nothing. From another tailnet device, `curl -v https://HOST/preflight`, `curl -v --http2 https://HOST:8443/probe` and
`curl -v --http3-only https://HOST:8444/probe` exercise the three listeners. Results include the tunnel's path and
overhead, so they are not a physical link's capacity ([measurement definitions](../../docs/MEASUREMENTS.md)).
