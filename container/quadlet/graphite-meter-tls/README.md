# Graphite Meter TLS rootless Quadlet

Three units that serve the published image on all four native listeners with a
Let's Encrypt certificate obtained over the Cloudflare DNS-01 challenge:
`certbot-bootstrap` issues it once, `certbot-renew` keeps it fresh, and
`graphite-meter` serves with it.

[Deployment overview](../../../docs/DEPLOYMENT.md) · [Basic Quadlet](../README.md) · [Tailscale](../tailscale-sidecar/README.md)

Before starting, have a public hostname, permission to edit its Cloudflare DNS zone, and a
rootless Podman/systemd installation. Allow TCP 7247–7249 and UDP 7249 through your firewall.
The supplied units use host networking and the published image; do not install them alongside
another Quadlet using the `graphite-meter` name.

## Install

Copy the whole directory into the rootless Quadlet search path - modern Quadlet
discovers units recursively, and the unit files reference their siblings by
relative path.

```sh
install -d -m 700 ~/.config/containers/systemd
cp -r container/quadlet/graphite-meter-tls ~/.config/containers/systemd/
cd ~/.config/containers/systemd/graphite-meter-tls
mkdir -p letsencrypt log
```

## Configure

Keep the credentials and the populated environment file in the installed copy,
not in the repository checkout.

```sh
cp secrets/cloudflare.ini.example secrets/cloudflare.ini
chmod 600 secrets/cloudflare.ini
$EDITOR secrets/cloudflare.ini graphite-meter.env
# Update every hostname/certificate-derived line in graphite-meter.env together.
```

The Cloudflare DNS record for `GM_PUBLIC_HOST` must be **DNS only** (gray cloud),
not proxied. Graphite Meter uses non-standard TCP ports 7247/7248 and TCP+UDP
7249; Cloudflare's normal HTTP proxy does not transparently pass this native
multi-port measurement traffic.

Scope the API token to `Zone:DNS:Edit` for the relevant zone, following the
[Certbot Cloudflare credential guide](https://certbot-dns-cloudflare.readthedocs.io/en/stable/#credentials).

## Validate and start

```sh
loginctl enable-linger "$USER"
systemctl --user daemon-reload
/usr/lib/systemd/system-generators/podman-system-generator --user --dryrun
systemctl --user start certbot-bootstrap.service
systemctl --user start certbot-renew.service graphite-meter.service
```

The units already declare `WantedBy=default.target`; Quadlet applies that at generation time.
Do not enable the generated services with `systemctl enable`
([Quadlet documentation](https://docs.podman.io/en/latest/markdown/podman-systemd.unit.5.html#enabling-unit-files)).

Some distributions install the generator at `/usr/libexec/podman/quadlet`; if
the dry-run path above does not exist, skip that command and inspect generated
units with `systemctl --user cat graphite-meter.service` after daemon-reload.

## Verify

Set the hostname in your verification shell as well as in the service environment:

```sh
export GM_PUBLIC_HOST=meter.example.com
systemctl --user status certbot-bootstrap certbot-renew graphite-meter
journalctl --user -u certbot-bootstrap -u certbot-renew -u graphite-meter -f
podman exec graphite-meter-certbot-renew certbot certificates
curl -v "https://${GM_PUBLIC_HOST}:7247/"
```

Test HTTP/3 from a capable client:

```sh
curl --http3-only -v "https://${GM_PUBLIC_HOST}:7249/"
```

## Renewal behavior

`certbot-bootstrap` is a one-shot prerequisite. It uses a stable `--cert-name`
and `--keep-until-expiring`, so repeated starts reuse the existing lineage
instead of continuously issuing or overwriting certificates.

`certbot-renew` stays running, invokes `certbot renew` immediately and every 12
hours, and is restarted by systemd if it exits. Certbot only renews when the
certificate enters its renewal window and atomically updates the `live/`
symlinks. Graphite Meter hot-reloads the PEM pair, so no deploy hook or server
restart is configured.

To force a safe staging test without consuming production rate limits:

```sh
podman run --rm -v "$PWD/letsencrypt:/etc/letsencrypt:z" -v "$PWD/log:/var/log/letsencrypt:z" -v "$PWD/secrets/cloudflare.ini:/run/secrets/cloudflare.ini:ro,z" docker.io/certbot/dns-cloudflare:latest renew --dry-run --dns-cloudflare-credentials /run/secrets/cloudflare.ini
```

## Ports with host networking

`graphite-meter.env` enables all four native listeners on the standard ports
listed in [DEPLOYMENT.md](../../../docs/DEPLOYMENT.md#native-listeners),
and `Network=host` binds them directly on the host. Keep 7246/tcp blocked in
the firewall if you want TLS-only external access, and do not add
`PublishPort=` while `Network=host` is set.

## Changing hostname or certificate name

Treat this as a deliberate migration. Stop all three units, update
`graphite-meter.env`, and either keep the existing certificate lineage or remove
only the old lineage with `certbot delete --cert-name OLD_NAME`. Do not manually
edit files under `letsencrypt/renewal`, `live`, or `archive`.
