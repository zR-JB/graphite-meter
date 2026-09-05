# Tailscale sidecar deployment

Run the published Graphite Meter image in a Tailscale container's network
namespace without host networking or published ports. The sidecar owns the
tailnet identity and TLS certificate.

[Deployment overview](../../../docs/DEPLOYMENT.md) · [Basic Quadlet](../README.md) · [Public native TLS](../graphite-meter-tls/README.md)

| Traffic                  | Listener   |
| ------------------------ | ---------- |
| HTTP/1.1 clear           | `80/tcp`   |
| HTTP/1.1 TLS, UI and WSS | `443/tcp`  |
| HTTP/2 measurement       | `8443/tcp` |
| HTTP/3 bootstrap         | `8444/tcp` |
| HTTP/3 measurement       | `8444/udp` |

This deployment deliberately drops the usual 7246-7249 scheme
([DEPLOYMENT.md](../../../docs/DEPLOYMENT.md#native-listeners)): the
sidecar owns the tailnet address and publishes nothing on the host, so the
listeners are free to take the standard web ports.

## Configure

Requirements are Linux with `/dev/net/tun`, systemd, a recent Podman release,
tailnet HTTPS certificates, and policy access to the ports above.

From the repository root, enter this example directory before preparing the files:

```sh
cd container/quadlet/tailscale-sidecar
cp graphite-meter-tailnet.env.example graphite-meter-tailnet.env
cp graphite-meter-tailnet-tailscale.env.example graphite-meter-tailnet-tailscale.env
chmod 600 graphite-meter-tailnet*.env
```

Set `TS_AUTHKEY`, `TS_HOSTNAME`, and `TS_CERT_DOMAIN` in the Tailscale file.
Replace `graphite-meter.example-tailnet.ts.net` in the Graphite Meter file with
the same certificate domain. Change or remove the example advertised tag to
match the tailnet policy. Populated `.env` files are ignored by Git.

## Install rootless

From this directory:

```sh
install -d -m 700 ~/.config/containers/systemd
install -m 644 *.container *.volume ~/.config/containers/systemd/
install -m 600 graphite-meter-tailnet*.env ~/.config/containers/systemd/

loginctl enable-linger "$USER"
systemctl --user daemon-reload
systemd-analyze --user verify graphite-meter-tailnet-tailscale.service graphite-meter-tailnet.service
systemctl --user start graphite-meter-tailnet-tailscale.service
```

Starting the sidecar starts Graphite Meter after the certificate health check.

## Verify

```sh
systemctl --user status graphite-meter-tailnet-tailscale.service --no-pager
systemctl --user status graphite-meter-tailnet.service --no-pager
podman exec graphite-meter-tailnet.tailscale tailscale status
podman port graphite-meter-tailnet
```

The final command must print nothing. From another tailnet device:

```sh
curl -v http://graphite-meter.example-tailnet.ts.net/preflight
curl -v --http1.1 https://graphite-meter.example-tailnet.ts.net/preflight
curl -v --http2 https://graphite-meter.example-tailnet.ts.net:8443/probe
curl -v --http1.1 https://graphite-meter.example-tailnet.ts.net:8444/probe
curl -v --http3-only https://graphite-meter.example-tailnet.ts.net:8444/probe
```

The last command requires an HTTP/3-capable curl. If curl stalls while a browser
reports `protocolNegotiated: h3`, cross-check with another HTTP/3 client because
implementations can behave differently on the same path.

## Updates and measurement scope

Update the Graphite Meter image and native clients together for 0.7; see the
[upgrade notes](../../../docs/DEPLOYMENT.md#upgrading-to-07). The Tailscale identity and certificate
state live in the supplied volumes and are separate from browser-local result history.

Results include the tailnet path and its tunnel overhead. They should not be read as direct
physical-link capacity; the optional wire estimate cannot account for unknown encapsulation.
See [measurement definitions](../../../docs/MEASUREMENTS.md).
