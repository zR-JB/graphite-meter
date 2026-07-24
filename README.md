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

Built to measure the link, not the tool: in Chrome it sustains up to **60 Gbit/s down and
18 Gbit/s up**, and the native terminal client pushes **hundreds of Gbit/s**.\*

## Features

- **Bufferbloat, measured properly** — idle latency is profiled first, then the same ping loop
  keeps running _during_ transfers, so you see how latency degrades under load: average, jitter,
  and range per stage, idle vs. loaded.
- **Honest numbers** — upload throughput is what the **server** received, streamed back live,
  never what the browser thinks it sent. Payloads are incompressible; stalls and reconnects
  can't inflate a result.
- **Optional wire-rate estimates** — opt-in Ethernet accounting for IP, TCP/UDP, TLS/QUIC, and
  HTTP framing; unobservable details are shown as a range instead of guessed.
- **Stages you choose** — latency, download, upload, and a bidirectional stage that saturates
  both directions at once. Adaptive early stopping ends a stage once its result is stable.
- **Configurable from the UI** — durations, parallel streams, ping cadence, units, gauge
  scaling. No config files.
- **Deploys the way you already deploy** — direct with native HTTP/1.1, HTTP/2, and HTTP/3
  listeners, behind nginx or Caddy, or both at once; clients measure the protocol they actually
  reached, so a proxy in front doesn't falsify results.
- **Optional private access** — operator password, OIDC with a group allowlist, or both,
  covering every asset, transfer, and WebSocket.
- **Featherweight** — a single static Go binary (~10 MB, browser client embedded,
  `FROM scratch` image, no shell, no libc) with low CPU and memory draw while sinking gigabits.
- **Free and open source** — AGPL-3.0.

## Quick start

Run the published image (multi-arch: amd64 + arm64) and open **http://localhost:7246**:

```sh
docker run -d --name graphite-meter -p 7246:7246 ghcr.io/zr-jb/graphite-meter:latest
```

That is the whole default deployment. The Compose overlays add TLS and authentication on top of
the same image:

| Deployment                     | Command                                                                                                                    |
| ------------------------------ | -------------------------------------------------------------------------------------------------------------------------- |
| Clear HTTP/1.1                 | `docker run -d --name graphite-meter -p 7246:7246 ghcr.io/zr-jb/graphite-meter:latest`                                      |
| Native TLS with H1, H2, and H3 | `GM_PUBLIC_HOST=meter.example.com docker compose -f container/docker-compose.yml -f container/docker-compose.tls.yml up -d` |
| With authentication            | `docker compose -f container/docker-compose.yml -f container/docker-compose.auth.yml up -d`                                 |

Ports 7246–7249 and what each one serves are in
[Native listeners](docs/CONFIGURATION.md#native-listeners). For Podman + systemd, ready-made
Quadlet units — including a rootless Let's Encrypt variant and a Tailscale sidecar — live in
[container/quadlet](container/quadlet/README.md). To serve behind nginx or Caddy, see
[docs/REVERSE_PROXY.md](docs/REVERSE_PROXY.md).

## Native terminal client

The same measurement engine without a browser: `graphite-meter-client` is an interactive TUI
that runs the same stages over the same wire protocol against any Graphite Meter server — and
pushes rates a browser can't.

- Full run setup in the terminal: server presets, stage selection, timings, transport and
  stream choices, with live bars, loaded latency, and per-stage progress.
- Throughput and latency targets are chosen independently, so you can pin a protocol
  (HTTP/1.1, HTTP/2, HTTP/3) instead of trusting negotiation.
- Works against authenticated servers: it shows a verification code and opens the approval page
  in your browser when you press `enter`, then holds the grant in memory only.

Prebuilt binaries for Linux, macOS, and Windows are attached to every
[release](https://github.com/zR-JB/graphite-meter/releases); the
[flags reference](docs/CONFIGURATION.md#native-tui-client-flags) covers scripted use.

## Docs

Defaults need no configuration. Everything beyond them:

| Document                                       | Covers                                                                              |
| ---------------------------------------------- | ----------------------------------------------------------------------------------- |
| [docs/CONFIGURATION.md](docs/CONFIGURATION.md) | Every runtime option on one page: listeners, TLS, origins, limits, auth, TUI flags. |
| [docs/REVERSE_PROXY.md](docs/REVERSE_PROXY.md) | nginx and Caddy deployments, and the headers measurement and auth require.          |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)   | How the server, both clients, and the `api/` contract fit together; the roadmap.    |
| [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md)     | Toolchain, `just` recipes, build flags, local TLS certs, building the image.        |

## Contributing

```sh
git clone https://github.com/zR-JB/graphite-meter.git
cd graphite-meter
just dev     # build the browser client, embed it, run the server on :7246
just ci      # the same checks CI runs
```

Prerequisites and everything else are in [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md).

## License

[AGPL-3.0-or-later](LICENSE).

---

<sub>\* Peak figures measured with Chrome (browser client) and the native TUI client on Linux
against a localhost server — a hardware-unconstrained best case that shows the tool won't be your
bottleneck. Across a real network, expect results limited by your NIC, path, and browser.</sub>
