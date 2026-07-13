<div align="center">

# Graphite Meter

**A self-hosted, open-source internet speed test that keeps up with modern networks.**

One ~10 MB static binary. Multi-gigabit honest measurements. Beautiful on every screen.

[![CI](https://github.com/zR-JB/graphite-meter/actions/workflows/ci.yml/badge.svg)](https://github.com/zR-JB/graphite-meter/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/zR-JB/graphite-meter?sort=semver)](https://github.com/zR-JB/graphite-meter/releases)
[![Image](https://img.shields.io/badge/ghcr.io-zr--jb%2Fgraphite--meter-2496ed?logo=docker&logoColor=white)](https://github.com/zR-JB/graphite-meter/pkgs/container/graphite-meter)
[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue)](LICENSE)

<img src="docs/assets/hero.png" alt="Graphite Meter after a finished run: 13.98 Gbit/s down, 11.08 Gbit/s up, latency-under-load profiles and per-stage throughput charts — desktop and mobile, dark mode" width="920">

</div>

Graphite Meter is built to measure the link, not the tool: in Chrome it sustains up to **60 Gbit/s down and 18 Gbit/s up**, and the native terminal client pushes **hundreds of Gbit/s**.\*

## Features

- **Bufferbloat, measured properly** — idle latency is profiled first, then the same ping loop
  keeps running _during_ download and upload, so you see exactly how your latency degrades under
  load (loaded latency), with detailed per-stage latency plots: average, jitter, and range for
  idle vs. loaded, down and up.
- **Honest numbers** — upload throughput is what the **server** actually received (byte counts
  over an elapsed clock streamed back live), never what the browser thinks it sent. All
  payloads are incompressible; stalls and reconnects can't inflate a result.
- **Every stage is optional** — latency, download, upload, and an optional **bidirectional**
  stage that saturates both directions at once.
- **Adaptive early stopping** — a stage ends as soon as its result is statistically stable, so a
  steady fiber line finishes in seconds while a jittery link gets the full window.
- **Honest wire-rate estimates** — opt-in forward-direction Ethernet accounting for IP,
  TCP/UDP, TLS/QUIC, HTTP framing, and explicit tunnels. The browser-facing protocol is detected
  automatically; unobservable packet details are shown as a range instead of guessed from runtime
  variance, ping timeouts, or ramp-up.
- **Highly configurable** — durations, parallel streams, ping cadence, units (bits/bytes,
  SI/IEC), manual or auto gauge scaling — from the UI, no config files.
- **Featherweight server** — a single static Go binary (~10 MB, client embedded, `FROM scratch`
  image, no shell, no libc) with low CPU and memory draw even while sinking gigabits.
- **Native TUI client** — a full interactive terminal client
  (`graphite-meter-client`) that speaks the same wire protocol and runs the same stages against
  any Graphite Meter server.
- **Modern, responsive UI** — dark and light themes, equally at home on a phone and a
  desktop.
- **Selectable HTTP/1.1, HTTP/2, and HTTP/3** — one logical server and shared measurement core;
  browser and native clients freeze one verified target for every transfer in a run.
- **Free and open source** — AGPL-3.0.

## Quick start

Run the published image (multi-arch: amd64 + arm64):

```sh
docker run -d --name graphite-meter -p 8765:8765 ghcr.io/zr-jb/graphite-meter:latest
```

Open **http://localhost:8765** — that's it for the default HTTP/1.1 deployment. Optional native
H2 and H3 listeners require a valid certificate; use the Compose TLS overlay below.
For local browser testing, including Firefox's stricter handling of private-root
HTTP/3 certificates, see [Local TLS and HTTP/3 certificates](docs/DEVELOPMENT.md#local-tls-and-http3-certificates).

### docker compose

Use [`container/docker-compose.yml`](container/docker-compose.yml), or:

```yaml
services:
  graphite-meter:
    image: ghcr.io/zr-jb/graphite-meter:latest
    ports:
      - "8765:8765"
    restart: unless-stopped
```

Enable all protocol targets with `container/docker-compose.tls.yml`. Set `GM_PUBLIC_HOST` and
mount/provision a certificate in the overlay's complete `/etc/letsencrypt` volume (the complete
tree is required because `live/` contains symlinks into `archive/`). H3 needs both TCP and UDP
8444 reachable.

### Podman + systemd (Quadlet)

A ready-made unit that pulls the image and runs it as a systemd service lives at
[`container/quadlet/graphite-meter.container`](container/quadlet/graphite-meter.container) — see
[`container/quadlet/README.md`](container/quadlet/README.md).

### Configuration

Everything is optional; the defaults just work. The common knobs:

| Env var              | Default                                 | What it does                                                                                                                                 |
| -------------------- | --------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| `GM_H1_ADDR`         | `:8765`                                 | Clear HTTP/1.1 listen address.                                                                                                               |
| `GM_ENABLE_H2`       | off                                     | Enable HTTP/2 on `GM_H2_ADDR` (`:8443`).                                                                                                     |
| `GM_ENABLE_H3`       | off                                     | Enable HTTP/3 UDP plus its H1 TLS bootstrap on `GM_H3_ADDR` (`:8444`).                                                                       |
| `GM_TLS_CERT` / `GM_TLS_KEY` | —                              | Matching, currently valid PEM pair required by H2/H3; renewed files hot-reload.                                                             |
| `GM_SERVER_NAME`     | `graphite-meter`                        | Server name shown in the client.                                                                                                             |
| `GM_SERVER_LOCATION` | —                                       | Location label shown in the client (e.g. `fra`).                                                                                             |
| `GM_TRUSTED_PROXIES` | —                                       | Comma-separated proxy CIDRs allowed to supply client IP and scheme forwarding headers.                                                       |
| `PUBLIC_H1_ORIGIN`   | derived from request                    | Public origin to advertise — set behind a reverse proxy.                                                                                     |
| `PUBLIC_H2_ORIGIN` / `PUBLIC_H3_ORIGIN` | derived from request host | Exact public TLS origins. Setting one advertises that external target even when its native listener is disabled.                         |
| `PUBLIC_TLS_ORIGIN` | — | Legacy alias for `PUBLIC_H2_ORIGIN`; the explicit H2 variable takes precedence. |
| `GM_VERBOSE`         | off                                     | Per-second server-side throughput/connection logging.                                                                                        |

Forwarding headers are ignored by default. See [Reverse proxy deployment](docs/REVERSE_PROXY.md)
for nginx, Caddy, and Traefik examples and the trust-chain rules.

Full reference (flags, reserved TLS/HTTP-3 variables): [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md#server-run-time-configuration).

> **Measuring multi-gigabit through the container?** Rootless Podman's user-mode networking
> (pasta/slirp) can cap throughput well below your NIC — use host networking for full-rate LAN
> tests.

### Native terminal client

```sh
just goclient-build            # -> go/graphite-meter-client
./go/graphite-meter-client -url http://your-server:8765 -protocol auto
```

An interactive TUI with the same stages (latency, download, upload, bidirectional, loaded
latency), server presets, and live telemetry — ideal for headless boxes and for pushing rates a
browser can't.

## Building from source & contributing

- **[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md)** — toolchain, `just` recipes, build flags,
  building the image from source.
- **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)** — how the server, the browser client, the TUI
  client, and the cross-language `api/` contract fit together, plus the roadmap (HTTP/3 /
  WebTransport, multi-server testing).

The short version: `git clone`, then `just dev`, then open http://localhost:8765.

## License

[AGPL-3.0-or-later](LICENSE).

---

<sub>\* Peak figures measured with Chrome (browser client) and the native TUI client on Linux
against a localhost server — a hardware-unconstrained best case that shows the tool won't be your
bottleneck. Across a real network, expect results limited by your NIC, path, and browser.</sub>
