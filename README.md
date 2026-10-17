<div align="center">

<img src="client/public/favicon.svg" width="64" alt="">

# Graphite Meter

Self-hosted speed and latency test. One container. Browser or terminal.

[![CI](https://img.shields.io/github/actions/workflow/status/zR-JB/graphite-meter/ci.yml?branch=main&label=CI&logo=githubactions&logoColor=white)](https://github.com/zR-JB/graphite-meter/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/zR-JB/graphite-meter?sort=semver&label=release&logo=github)](https://github.com/zR-JB/graphite-meter/releases/latest)
[![Image](https://img.shields.io/badge/ghcr.io-zr--jb%2Fgraphite--meter-2496ED?logo=docker&logoColor=white)](https://github.com/zR-JB/graphite-meter/pkgs/container/graphite-meter)
[![License](https://img.shields.io/github/license/zR-JB/graphite-meter?label=license)](LICENSE)

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/hero-dark.png">
  <img src="docs/assets/hero-light.png" width="100%" alt="A completed test on a desktop, 9.40 Gbit/s down and 9.39 up with 0.4 ms idle latency and 1.3 ms added under load, and the same app on a phone at 1197 Mbit/s">
</picture>

<sub>0.13.0 in Chrome. Desktop: path shaped to 9.8 Gbit/s. Phone: 1.25 Gbit/s down, 0.9 up. The app sets no rate limit.</sub>

</div>

## Run it

```sh
docker run -d --name graphite-meter --restart unless-stopped -p 7246:7246 ghcr.io/zr-jb/graphite-meter:latest
```

Open `http://SERVER_IP:7246`. One static binary in a scratch image, amd64 and arm64, no database.

| You want                         | Read                                                                                                                   |
| -------------------------------- | ---------------------------------------------------------------------------------------------------------------------- |
| HTTP/2, HTTP/3, WebTransport     | They need TLS: [native listeners](docs/DEPLOYMENT.md#native-listeners) or a [reverse proxy](docs/DEPLOYMENT.md#reverse-proxies) |
| Compose, Podman Quadlet          | [Docker Compose](docs/DEPLOYMENT.md#docker-compose), [Quadlet](container/quadlet/README.md)                           |
| Sign-in, password or OIDC        | [Authentication](docs/DEPLOYMENT.md#authentication)                                                                   |
| More than one test server        | [Server catalog](docs/DEPLOYMENT.md#server-catalog)                                                                   |

## What it measures

| Result     | How                                                                                                                 |
| ---------- | ------------------------------------------------------------------------------------------------------------------- |
| Throughput | Download and upload, timed by the receiver: the browser for downloads, the server for uploads.                     |
| Latency    | Idle and under load as separate populations: median, P95, jitter, timeouts. The difference is the queueing under load. |
| Servers    | One to four test servers in one test, on one schedule.                                                              |
| Paths      | Throughput and latency on separate paths: HTTP/1.1, HTTP/2, HTTP/3, WebSocket, WebTransport. Automatic or chosen.    |

Every number is defined in [measurement definitions](docs/MEASUREMENTS.md). Nothing is graded.

## Terminal

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/terminal-dark.png">
  <img src="docs/assets/terminal-light.png" width="100%" alt="The terminal client after a completed test: 2.35 Gbit/s each way, 0.4 ms idle latency, 2.7 ms added under load">
</picture>

<sub>graphite-meter-client 0.13.0, same server, path shaped to 2.46 Gbit/s.</sub>

```sh
graphite-meter-client --url http://192.168.1.20:7246
graphite-meter-client --url https://meter.example --server frankfurt --server amsterdam --report
```

| Client        | Detail                                                                                            |
| ------------- | ------------------------------------------------------------------------------------------------- |
| Platforms     | Linux amd64, Linux arm64, Windows amd64. One static executable per [release](https://github.com/zR-JB/graphite-meter/releases). |
| `--report`    | One test without the console; the report on stdout, the outcome in the exit status.               |
| Guide         | [Flags, keys, report mode, exit codes](docs/TERMINAL.md)                                           |

## Settings and History

<div align="center">
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/phones-dark.png">
  <img src="docs/assets/phones-light.png" width="700" alt="Two phone screens: Settings with three test servers and the throughput paths, and History with four results">
</picture>
</div>

History keeps up to 2,000 results in the browser. The server stores none.

## Trust

| Area     | What the code does                                                                                           |
| -------- | ------------------------------------------------------------------------------------------------------------ |
| Sign-in  | Off, a password, or your OIDC provider. It covers the app and every measurement route. The terminal client approves a code in the browser and never sees the password. |
| Network  | No outbound connection unless OIDC is configured. The browser app loads nothing from a third party.          |
| License  | AGPL-3.0. Every image and archive ships its license notices; every release attaches the third-party source. |

> [!NOTE]
> Upgrading from 0.12 or earlier: `latest` is now the Rust server. See [upgrading](docs/DEPLOYMENT.md#upgrading).

## Documentation

- [Deployment](docs/DEPLOYMENT.md): Compose, Quadlet, reverse proxies, TLS, sign-in, the server catalog, every
  setting.
- [Terminal client](docs/TERMINAL.md): install, flags, keys, report mode, exit codes.
- [Measurement definitions](docs/MEASUREMENTS.md): what each result means and how it is computed.
- [Development](docs/DEVELOPMENT.md): building, testing, the workspace, releasing.

## Contributing

`mise run setup`, then `mise run dev` on port 7246, then `mise run check` before a pull request. The rest is in the
[development guide](docs/DEVELOPMENT.md).

## License

Copyright 2026 zR-JB. AGPL-3.0-or-later: [LICENSE](LICENSE), [COPYRIGHT](COPYRIGHT). Third-party notices ship in
the browser's legal view, in the client archives and under `/usr/share/licenses/graphite-meter/` in the image.
