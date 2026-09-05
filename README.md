<div align="center">

# Graphite Meter

**How fast is your connection—and how responsive does it stay?**

Self-hosted speed testing for browsers and terminals.\
Download, upload, and latency under load, with the measurement path in plain sight.

[![CI](https://github.com/zR-JB/graphite-meter/actions/workflows/ci.yml/badge.svg)](https://github.com/zR-JB/graphite-meter/actions/workflows/ci.yml) [![Release](https://img.shields.io/github/v/release/zR-JB/graphite-meter?sort=semver)](https://github.com/zR-JB/graphite-meter/releases) [![Container](https://img.shields.io/badge/container-ghcr.io%2Fzr--jb%2Fgraphite--meter-387d91)](https://github.com/zR-JB/graphite-meter/pkgs/container/graphite-meter) [![License](https://img.shields.io/badge/license-AGPL--3.0-blue)](LICENSE)

[Quick start](#quick-start) · [Features](#why-graphite-meter) · [Screenshots](docs/SCREENSHOTS.md) · [Deployment guide](docs/DEPLOYMENT.md)

<img src="docs/assets/hero.png" alt="Completed Graphite Meter test: download and upload results alongside idle and loaded latency profiles" width="1080">

<sub>v0.7.0 · simulated measurements · <a href="docs/SCREENSHOTS.md">explore the interface</a></sub>

</div>

## Quick start

One container. One port. No configuration file or database service.

```sh
docker run -d --name graphite-meter --restart unless-stopped \
  -p 7246:7246 ghcr.io/zr-jb/graphite-meter:latest
```

Open **[localhost:7246](http://localhost:7246)**, or `http://YOUR_SERVER_IP:7246` from another
device, and start a test. Traffic travels between that device and your Graphite Meter server;
choose a server location that includes the network path you want to measure.

The default provides HTTP/1.1 throughput and WebSocket latency for local or trusted-network use.
For public access, follow the [HTTPS and authentication guide](docs/DEPLOYMENT.md#choose-your-deployment).
For HTTP/2, HTTP/3, and WebTransport, enable [native TLS listeners](docs/DEPLOYMENT.md#native-listeners).

> **Upgrading from 0.6?** Update the server and native client together, then reload the browser.
> [The 0.7 upgrade notes](docs/DEPLOYMENT.md#upgrading-to-07) cover protocol changes and older saved history.

Prefer Compose? Save this as `compose.yml`, then run `docker compose up -d`:

```yaml
services:
  graphite-meter:
    image: ghcr.io/zr-jb/graphite-meter:latest
    ports:
      - "7246:7246"
    restart: unless-stopped
```

[Compose with TLS](docs/DEPLOYMENT.md#docker-compose) · [Reverse proxy](docs/DEPLOYMENT.md#reverse-proxies) · [Rootless Podman](container/quadlet/README.md) · [Tailscale](container/quadlet/tailscale-sidecar/README.md)

## Why Graphite Meter?

A throughput number is the beginning. Graphite Meter also shows what happens to responsiveness
while the connection is busy, and gives you control over how it is measured.

| What you get                               | Why it matters                                                                                                                                           |
| ------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Receiver-measured throughput**           | Downloads count bytes received by the client. Upload bytes and timing come from the server, so data waiting in a sender queue does not inflate delivery. |
| **Latency under each kind of load**        | Compare idle, download, upload, and optional simultaneous transfer. Separate profiles show when responsiveness degrades.                                 |
| **Independent connection choices**         | Select throughput and latency paths separately. Compare dedicated HTTP/1.1, HTTP/2, HTTP/3, WebSocket, and WebTransport paths where available.           |
| **Evidence behind the numbers**            | Inspect latency distributions, RTT variation, probe timeouts, and paired server handling time. Missing or interrupted evidence stays explicit.           |
| **A browser and a native terminal client** | Test from a phone or desktop, or use the Go TUI to explore the path without browser runtime limits.                                                      |
| **A small deployment with access control** | One Go binary embeds the web UI. Optional password, OIDC, or hybrid sign-in protects measurement routes as well as the page.                             |

### A useful instrument on every screen

The responsive interface keeps the run controls, throughput results, and latency profile together.
Inspect charts with a pointer, keyboard, or touch; light and dark themes and reduced-motion support
carry across the interface.

- **Make the test yours:** toggle stages, add simultaneous download/upload, choose a duration
  preset or custom timings, and use automatic or fixed stream counts.
- **Read rates your way:** decimal or binary bits/bytes, automatic or fixed gauge scale, and an
  optional, separately labelled wire-rate estimate.
- **Keep a local record:** opt into device-local history for up to 2,000 completed summaries.
  Browse, sort, and inspect saved runs while keeping the live meter available.

<details>
<summary><strong>Connection choices and measurement limits</strong></summary>

Dedicated native listeners make protocol selection explicit. A reverse proxy creates two hops;
endpoint information distinguishes browser-observed and server-observed protocol evidence when
available. There is no universal fastest transport: client, CPU, proxy, and path conditions matter.

WebTransport requires a compatible browser, an HTTPS page, a trusted certificate, and reachable
HTTP/3 over UDP. WebSocket probes remain available without it. Experimental datagram throughput
is useful for investigating browser and QUIC behavior, but can hit API processing limits before
network capacity.

Probe timeouts describe unanswered application probes. They do **not** measure TCP/IP packet loss.
Paired adjusted RTT subtracts only the instrumented server handling interval; raw application RTT
remains the primary latency measurement. Wire rate is an overhead estimate, not packet capture.

See [measurement definitions](docs/MEASUREMENTS.md) for timing, populations, and missing-data behavior,
and [historical benchmarks](docs/BENCHMARKS.md) for software-path performance evidence.

</details>

## From the terminal

Download the matching `graphite-meter-client` archive for your platform from
[Releases](https://github.com/zR-JB/graphite-meter/releases), extract it, and run:

```sh
./graphite-meter-client --url http://YOUR_SERVER_IP:7246
```

The interactive TUI provides stage, duration, stream, and connection choices with live results.
Releases provide Linux and macOS builds for amd64/arm64, plus Windows amd64
(`graphite-meter-client.exe`). For source-built servers, [build the client from the same checkout](docs/DEVELOPMENT.md#development-commands).

On authenticated servers, approve the terminal's short code in your browser. The client keeps its
measurement grant in memory; it does not need your operator password.
[All terminal flags and authentication details →](docs/DEPLOYMENT.md#native-terminal-client)

## Find your next step

| I want to…                                                       | Read                                                                                               |
| ---------------------------------------------------------------- | -------------------------------------------------------------------------------------------------- |
| Deploy, configure TLS, add sign-in, or troubleshoot a connection | [Deployment and configuration](docs/DEPLOYMENT.md)                                                 |
| See settings, endpoint details, mobile, and saved results        | [Screenshot gallery](docs/SCREENSHOTS.md)                                                          |
| Understand exactly what a result means                           | [Measurement definitions](docs/MEASUREMENTS.md)                                                    |
| Explore performance evidence and reproduce a benchmark           | [Benchmarks](docs/BENCHMARKS.md)                                                                   |
| Build, test, or contribute                                       | [Development and architecture](docs/DEVELOPMENT.md)                                                |
| Integrate a client or inspect the contracts                      | [Discovery](api/discovery.md) · [Uploads](api/upload.md) · [Latency and WebTransport](api/wire.md) |

Server selection and parallel tests across independent servers are planned. The current browser
selects among the paths advertised by one deployment.

## Contributing

With [mise installed](docs/DEVELOPMENT.md#prerequisites):

```sh
git clone https://github.com/zR-JB/graphite-meter.git
cd graphite-meter
mise trust
mise run setup
mise run dev
```

Run `mise run check` before submitting a change. The [development guide](docs/DEVELOPMENT.md)
explains the architecture, focused checks, full CI gate, and release process.

## License

Copyright © 2026 zR-JB. Licensed under **AGPL-3.0-or-later**; see [LICENSE](LICENSE) and
[COPYRIGHT](COPYRIGHT). Third-party notices ship in the browser's About/legal view, native-client
archives, and `/usr/share/licenses/graphite-meter/` in the container.
