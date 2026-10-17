<div align="center">

<img src="client/public/favicon.svg" width="64" alt="">

# Graphite Meter

A self-hosted speed and latency test, in one container, for the browser and the terminal.

[![CI](https://img.shields.io/github/actions/workflow/status/zR-JB/graphite-meter/ci.yml?branch=main&label=CI&logo=githubactions&logoColor=white)](https://github.com/zR-JB/graphite-meter/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/zR-JB/graphite-meter?sort=semver&label=release&logo=github)](https://github.com/zR-JB/graphite-meter/releases/latest)
[![Image](https://img.shields.io/badge/ghcr.io-zr--jb%2Fgraphite--meter-2496ED?logo=docker&logoColor=white)](https://github.com/zR-JB/graphite-meter/pkgs/container/graphite-meter)
[![License](https://img.shields.io/github/license/zR-JB/graphite-meter?label=license)](LICENSE)

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/hero-dark.png">
  <img src="docs/assets/hero-light.png" width="100%" alt="A completed test on a desktop, 9.40 Gbit/s down and 9.39 up with 0.4 ms idle latency and 1.3 ms added under load, and the same app on a phone at 1197 Mbit/s">
</picture>

</div>

## Install

```sh
docker run -d --name graphite-meter --restart unless-stopped -p 7246:7246 ghcr.io/zr-jb/graphite-meter:latest
```

Open `http://SERVER_IP:7246`. The image holds one static binary, for amd64 and arm64, with no database behind it.

| Setup                        | Guide                                                                                                                              |
| ---------------------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| HTTP/2, HTTP/3, WebTransport | These need TLS, from the [native listeners](docs/DEPLOYMENT.md#native-listeners) or a [reverse proxy](docs/DEPLOYMENT.md#reverse-proxies). |
| Compose, Podman Quadlet      | See [Docker Compose](docs/DEPLOYMENT.md#docker-compose) and [Quadlet](container/quadlet/README.md).                              |
| Sign-in                      | A password or your OIDC provider protects the app and every measurement route; see [authentication](docs/DEPLOYMENT.md#authentication). |
| More than one test server    | List them in a [server catalog](docs/DEPLOYMENT.md#server-catalog).                                                               |

> [!NOTE]
> If you upgrade from 0.12 or earlier, `latest` now runs the Rust server. See [upgrading](docs/DEPLOYMENT.md#upgrading).

## Measurements

| Result     | Method                                                                                                                  |
| ---------- | ----------------------------------------------------------------------------------------------------------------------- |
| Throughput | The receiver times each transfer, so the browser times downloads and the server times uploads.                        |
| Latency    | Idle latency and latency under load are separate populations, each with median, P95, jitter and timeouts. Their difference is the queueing that load adds. |
| Servers    | A test runs against one to four servers on one schedule.                                                                |
| Paths      | Throughput and latency travel on separate paths over HTTP/1.1, HTTP/2, HTTP/3, WebSocket or WebTransport, picked automatically or by hand. |

The [measurement definitions](docs/MEASUREMENTS.md) define each result. The app reports numbers and grades nothing.

## Terminal

<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/terminal-dark.png">
  <img src="docs/assets/terminal-light.png" width="100%" alt="The terminal client after a completed test: 2.35 Gbit/s each way, 0.4 ms idle latency, 2.7 ms added under load">
</picture>

</div>

```sh
# The console, against a server on the LAN
graphite-meter-client --url http://192.168.1.20:7246

# One test against two catalog servers, printed as a report; the exit status says how it ended
graphite-meter-client --url https://meter.example.net --server frankfurt --server amsterdam --report
```

| Client     | Detail                                                                                                                          |
| ---------- | ------------------------------------------------------------------------------------------------------------------------------- |
| Platforms  | Every [release](https://github.com/zR-JB/graphite-meter/releases) attaches one static executable for Linux amd64, Linux arm64 and Windows amd64. |
| Sign-in    | The client approves a short code in the browser and never sees the password.                                                   |
| Guide      | The [terminal client guide](docs/TERMINAL.md) lists the flags, keys, report mode and exit codes.                                |

## Settings and History

<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/phones-dark.png">
  <img src="docs/assets/phones-light.png" width="700" alt="Two phone screens: Settings with three test servers and the throughput paths, and History with four results">
</picture>

</div>

| Sheet    | Content                                                                                  |
| -------- | ---------------------------------------------------------------------------------------- |
| Settings | Settings picks the test servers, the throughput and latency paths, the stages and the advanced options. |
| History  | History keeps up to 2,000 results and shows the columns you choose.                       |

## Privacy

The server makes no outbound connection unless OIDC is configured, and the browser app loads nothing from a third
party. Results stay in the browser's History; the server stores none.

## Documentation

- [Deployment](docs/DEPLOYMENT.md) covers Compose, Quadlet, reverse proxies, TLS, sign-in, the server catalog and
  every setting.
- [Terminal client](docs/TERMINAL.md) covers installing, the flags, the keys, report mode and the exit codes.
- [Measurement definitions](docs/MEASUREMENTS.md) say what each result means and how it is computed.
- [Development](docs/DEVELOPMENT.md) covers building, testing, the workspace and releasing.

## Contributing

`mise run setup` installs the toolchain, `mise run dev` serves a development build on port 7246, and
`mise run check` runs the gate a pull request must pass. The [development guide](docs/DEVELOPMENT.md) has the rest.

## License

Copyright 2026 zR-JB, AGPL-3.0-or-later; see [LICENSE](LICENSE) and [COPYRIGHT](COPYRIGHT). Every image and archive
ships its third-party notices: the browser shows them in its legal view, the client archives carry them as files, and
the image keeps them under `/usr/share/licenses/graphite-meter/`. Every release attaches the third-party source.
