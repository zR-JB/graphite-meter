<div align="center">

# Graphite Meter

Self-hosted network throughput, loaded latency, and datagram-loss measurement for browsers and terminals.

[![CI](https://github.com/zR-JB/graphite-meter/actions/workflows/ci.yml/badge.svg)](https://github.com/zR-JB/graphite-meter/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/zR-JB/graphite-meter?sort=semver)](https://github.com/zR-JB/graphite-meter/releases)
[![Image](https://img.shields.io/badge/ghcr.io-zr--jb%2Fgraphite--meter-2496ed?logo=docker&logoColor=white)](https://github.com/zR-JB/graphite-meter/pkgs/container/graphite-meter)
[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue)](LICENSE)

<img src="docs/assets/hero.png" alt="Graphite Meter browser client showing a completed multi-gigabit test on desktop and mobile" width="920">

<sub>Demo data.</sub>

</div>

Graphite Meter combines a server, a responsive browser client, and a native terminal client. Open
the browser UI from any device, select the stages and transports you want to test, and inspect
throughput and latency under load. The server is just as quick to start: one container command
provides a usable local instance with no configuration file.

The measurement paths are explicit. Uploads are timed where the server receives them, latency can
use unreliable WebTransport datagrams, and native listeners on separate ports let clients select
HTTP/1.1, HTTP/2, or HTTP/3 instead of depending only on protocol negotiation.

## Measurement model

| Capability              | What Graphite Meter reports                                                                                                | Boundary                                                                                           |
| ----------------------- | -------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------- |
| Download                | Payload bytes consumed by the receiving client over measured time.                                                         | Browser, host, server, and path limits can all constrain the result.                               |
| Upload                  | Payload bytes received and timed by the server, so bytes merely queued by the sender are not counted as delivered.         | A proxy or server that terminates or buffers the measured request becomes part of the path.        |
| Idle and loaded latency | Client-clock application send-to-receive time before and during each selected transfer stage, with percentiles and jitter. | It includes browser, server, transport, and scheduler time. It is not ICMP latency.                |
| Probe timeouts          | Resolved application probes whose reply deadline expired, labelled by WebSocket or datagram transport.                     | Network and endpoint queues can cause timeouts; this does not identify physical or directional IP loss. |
| Wire-rate estimate      | A separately labelled estimate derived from payload rate and observed protocol evidence.                                   | It is not packet capture and cannot include unknown tunnel, encapsulation, VLAN, or path overhead. |

WebSocket latency remains available when WebTransport is unavailable. It measures application RTT,
including TCP retransmission and head-of-line blocking. WebTransport datagrams avoid stream
retransmission, but their timeouts also include endpoint queueing and drops.
See [measurement definitions](docs/MEASUREMENTS.md) for exact populations, missing evidence, and statistics.

## Start a server

Run the multi-architecture image and open <http://localhost:7246>:

```sh
docker run -d --name graphite-meter -p 7246:7246 ghcr.io/zr-jb/graphite-meter:latest
```

This default starts a clear HTTP/1.1 service with fetch-stream throughput and WebSocket latency.
It is useful for quick local or trusted-network testing and requires only port 7246. It does not
provide native TLS, HTTP/2, HTTP/3, or WebTransport. Native TLS listeners add those dedicated paths.
WebTransport in a remote browser also requires an HTTPS page, a trusted certificate, reachable
HTTP/3 over UDP, and compatible browser support.

See [Deployment and configuration](docs/DEPLOYMENT.md) for Docker Compose, native TLS, reverse
proxies, authentication, Podman, Quadlet, every server setting, and every terminal-client flag.

## Browser client

The main screen keeps routine test choices close to the run button:

- toggle idle latency, download, and upload stages independently;
- enable the optional bidirectional stage in Settings;
- keep latency probes active during transfers to profile responsiveness under load;
- choose a duration preset or custom stage timings and let estimated-stable stages finish early;
- select throughput and latency paths independently from the server's advertised endpoints;
- choose automatic or fixed stream policy and automatic or fixed gauge scale;
- display rates as decimal or binary bits or bytes throughout the application;
- show protocol-overhead wire estimates beside measured application rates;
- keep up to 2,000 completed summaries in optional device-local history.

The connection picker exposes fetch streams over negotiated or dedicated HTTP listeners,
WebSocket latency, and WebTransport streams or datagrams where the server advertises them.
Experimental datagram throughput is available for comparing browser and QUIC API behavior, but it
commonly measures browser datagram-processing limits rather than line capacity. Parallel clear
HTTP/1.1 fetch streams remain the practical high-throughput browser path on the reference system.

Graphite Meter records both browser-observed and server-observed protocol evidence when available.
That matters behind a reverse proxy, where the browser-facing protocol can differ from the upstream
protocol. Some proxies perform better with multiplexed HTTP/2 than with separate HTTP/1.1 requests,
so there is no universal transport ranking. An ordinary TCP reverse proxy does not carry native
WebTransport; expose the HTTP/3 UDP listener separately when that path is required.

More browser views are collected in the [screenshot gallery](docs/SCREENSHOTS.md).

## Native terminal client

`graphite-meter-client` uses the same server contracts without a browser. Its TUI selects the
server, stages, timing, independent connection paths, and stream policy, then shows live progress
and the completed latency and throughput results.

<div align="center">
<img src="docs/assets/tui.png" alt="Graphite Meter native terminal client showing a completed test" width="920">
<br><sub>Demo data.</sub>
</div>

Current [releases](https://github.com/zR-JB/graphite-meter/releases) attach prebuilt native client
archives for Linux, macOS, and Windows. Authenticated servers use a short
verification code and browser approval; the client keeps the resulting measurement grant in memory
only. Command-line configuration and all flags are documented in
[Deployment and configuration](docs/DEPLOYMENT.md#native-terminal-client).

## Deployment options

The memory-safe Go server embeds the production Svelte client in one static binary. The published
container is built from `scratch` and contains no shell or libc. Runtime admission limits bound
active measurements, sessions, and connections without throttling the traffic being measured.

| Deployment                               | Use case                                                                        |
| ---------------------------------------- | ------------------------------------------------------------------------------- |
| Clear HTTP/1.1 container                 | Fast local startup with fetch throughput and WebSocket latency.                 |
| Native TLS listeners                     | Deterministic HTTP/1.1 TLS, HTTP/2, HTTP/3, and WebTransport on separate ports. |
| Reverse proxy                            | Existing HTTPS ingress with protocol evidence for both sides of the proxy.      |
| Password, OIDC, or hybrid authentication | Restrict the UI and all measurement transports on public deployments.           |
| Rootless Podman and Quadlet              | Run under a user account, with complete native-TLS and Tailscale examples.      |

Host networking can avoid rootless userspace-network throughput limits, but it also removes the
container network namespace. The deployment guide describes that tradeoff and the firewall rules
needed for TCP and UDP listeners.

## Reference performance

Reference testing on one x86 system measured these sustained loopback medians:

| Client and path                   |      Download |        Upload |
| --------------------------------- | ------------: | ------------: |
| Chromium, clear HTTP/1.1, 2 lanes |  49.00 Gbit/s |  16.95 Gbit/s |
| Native Go client, 8 lanes         | 362.59 Gbit/s | 239.71 Gbit/s |

These are software-path results from one machine, not expected rates for a real network. Browser
version, CPU, memory, transport, proxy, latency, loss, and the network path can change the ordering
and the ceiling. The method, transport matrix, shaped-path results, and limitations are in
[Benchmarks](docs/BENCHMARKS.md).

## Documentation

| Document                                            | Contents                                                                                                                       |
| --------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------ |
| [Deployment and configuration](docs/DEPLOYMENT.md)  | Listeners, TLS, advertised origins, authentication, limits, containers, proxies, Podman, and all CLI and environment settings. |
| [Development and architecture](docs/DEVELOPMENT.md) | Data flow, authoritative boundaries, repository layout, toolchain, tests, builds, and releases.                                |
| [Benchmarks](docs/BENCHMARKS.md)                    | Historical evidence, maintained reproduction commands, and interpretation limits.                                              |
| [Screenshots](docs/SCREENSHOTS.md)                  | Current browser settings, endpoint evidence, and local history surfaces.                                                       |
| [Wire protocol](api/wire.md)                        | Normative server and client measurement contracts.                                                                             |

## Roadmap

1. Let the browser select independent Graphite Meter servers. Authentication and operator policy
   for remote targets still need a defined contract.
2. Build on that work to measure several servers at once, using independent origins and
   connections to load paths beyond one browser origin's limits.

## Contributing

```sh
git clone https://github.com/zR-JB/graphite-meter.git
cd graphite-meter
just dev
just ci
```

The required toolchain, focused test commands, generated artifacts, and release workflow are in
[Development and architecture](docs/DEVELOPMENT.md).

## License

Graphite Meter - Copyright © 2026 zR-JB

Licensed under AGPL-3.0-or-later. See [LICENSE](LICENSE) and [COPYRIGHT](COPYRIGHT). Third-party
notices are generated for each distributed artifact and are available in the browser's About and
legal view, in the container under `/usr/share/licenses/graphite-meter/`, and in native-client
release archives.
