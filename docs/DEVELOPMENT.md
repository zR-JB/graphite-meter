# Development

How to build, run, and test everything from source. For a normal deployment of the published
image, see the [README quick start](../README.md#quick-start).

## Prerequisites

- **Go 1.26+** — the measurement server and native TUI client.
- **Bun 1.4+** — the client toolchain.
- **[`just`](https://github.com/casey/just)** — every workflow in this repo goes through the
  `justfile`. Run `just` with no arguments to list every recipe.

## Clone

```sh
git clone https://github.com/zR-JB/graphite-meter.git
cd graphite-meter
```

## Quick start (development)

```sh
just dev          # build the client (dev profile), embed it into the Go server, run it on :8765
```

Open `http://localhost:8765`. `just dev` and `just prod` are the two "do everything" entrypoints
and are deliberately symmetric: each builds the Svelte client in its own profile, stages it into
`go/internal/static/dist` (picked up by `//go:embed`), and `go run`s the server — only the profile
differs. `just dev` includes the dummy runner (`?engine=dummy`) and the Developer settings tab by
default; `just prod` builds a real-only, dev-tooling-stripped, version-stamped run instead (see
below).

## Naming

Recipes follow `<component>-<verb>`: `build` produces an artifact and stops there (a `client/dist`
or a Go binary); `run` executes something already built; `test` runs a component's tests. `dev`
and `prod` are the two config profiles a `-dev`/`-prod`-suffixed recipe can target. The two
recipes with no component prefix, `dev` and `prod`, are the top-level entrypoints described above.
Recipes starting with `_` are private helper steps, not meant to be run directly.

## Just command reference

| Recipe                   | What it does                                                                                                                                                     |
| ------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `just` (no args)         | Lists every recipe.                                                                                                                                              |
| `just dev`               | Builds + embeds the dev-profile client, then `go run`s the server on `:8765`.                                                                                    |
| `just prod`              | Builds + embeds the prod-profile client, then `go run`s the version-stamped server on `:8765`.                                                                   |
| `just client-build-dev`  | `bun install` + `bun run build` -> `client/dist`, dev profile (Vite's own defaults: dummy engine + dev tools included).                                          |
| `just client-build-prod` | Same, but real-only engine and dev tooling stripped by default — accepts the `GM_CLIENT_*` knobs inline (see below).                                             |
| `just client-watch`      | Vite dev server only — hot reload, no Go server, no embedding, no live measurement backend.                                                                      |
| `just client-check`      | Type-checks the client, including Bun test files (`svelte-check`) and the Vite config (`tsc`).                                                                   |
| `just client-test`       | `bun test` — pure-`.ts`-logic unit tests (no component rendering).                                                                                               |
| `just client-ci`         | Runs the fast client CI gates: Prettier check, semantic type check, and Bun tests.                                                                               |
| `just client-gen-types`  | Regenerates TypeScript discovery and probe types from both JSON schemas.                                                                                     |
| `just server-build-dev`  | Builds + embeds the dev-profile client, then builds `go/graphite-meter` as a persisted, stripped (`-s -w -trimpath`) binary — no version stamp, nothing runs it. |
| `just server-build-prod` | Same, prod profile, plus the ldflags version stamp — the shippable binary for a manual/non-Docker deploy.                                                        |
| `just server-check`      | Checks Go formatting and `go vet ./...`.                                                                                                                         |
| `just server-test`       | `go test -race -shuffle=on ./...` — includes the `/preflight` schema-conformance test.                                                                           |
| `just ci`                | Runs the main local CI gates: `client-ci`, `server-check`, and `server-test`.                                                                                    |
| `just goclient-build`    | Builds only `go/graphite-meter-client` — does not touch the Svelte client.                                                                                       |
| `just goclient-run`      | `go run`s the native TUI client against a running server.                                                                                                        |
| `just container-build`   | `docker build -f container/Dockerfile -t graphite-meter:latest .`                                                                                                |

`just prod` and `just client-build-prod` accept the `GM_CLIENT_*` knobs inline to produce a
configurable build instead of the real-only default, e.g.:

```sh
just prod allow_dummy=1 dev_tools=1 label=0.2.0
```

## Build-time feature flags

The dummy engine and the entire Developer tab are compiled out of production builds, not just
hidden — `client/vite.config.ts` reads `GM_CLIENT_*` env vars and injects them via Vite's `define`
as raw literal tokens (not strings), which is what lets Rollup constant-fold the relevant
`if (...)` branches and tree-shake the dead code entirely rather than leaving it reachable behind
a runtime flag.

Vite remains the browser bundler because Svelte's supported non-SvelteKit toolchain is
`@sveltejs/vite-plugin-svelte`, which handles `.svelte` files and Svelte-aware diagnostics. Bun is
still the package manager, script runner, test runner, and runtime used by the client toolchain;
the small custom inline HTML minification step in `vite.config.ts` also uses `Bun.build`, so there
is no direct esbuild dependency in this package. The `check` script is deliberately separate from
the bundler: Bun's bundler transpiles TypeScript, but semantic type checking is handled by
`svelte-check`/`tsc`. `bun run build` is the validated local/default build (`check` plus
`build:bundle`); CI smoke and release image builds pass `GM_CLIENT_VALIDATE=0` to the Dockerfile
because the same commit has already passed the client check/test job.

| Variable                | Values           | `just dev`/`client-build-dev` default | `just prod`/`client-build-prod` default | What it does                                                                                                                                                                                                                                                              |
| ----------------------- | ---------------- | ------------------------------------- | --------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `GM_CLIENT_ENGINE`      | `real` / `dummy` | `real`                                | `real`                                  | Default runner when more than one is compiled in.                                                                                                                                                                                                                         |
| `GM_CLIENT_ALLOW_DUMMY` | `0` / `1`        | `1`                                   | `0`                                     | Compile in the dummy runner and the Developer-tab anomaly-injection cards.                                                                                                                                                                                                |
| `GM_CLIENT_DEV_TOOLS`   | `0` / `1`        | `1`                                   | `0`                                     | Compile in the whole Developer settings tab (including debug logging).                                                                                                                                                                                                    |
| `GM_CLIENT_BUILD_LABEL` | string           | `dev`                                 | git short hash                          | Text shown after `build` in the status bar. Also the label half of the client version `<package.json semver>+<label>`, which is shown in the Endpoint info drawer, written to `dist/version.json`, and sent to the server on preflight as `?client=web&client_version=…`. |

At runtime, when the dummy runner is compiled in, `?engine=dummy` on the URL (or a previously
persisted choice in `localStorage`) switches to it; this check itself compiles away in a
dummy-stripped build, so it can't be re-enabled by URL trickery in a production build.

## Server run-time configuration

Read once at startup; flags (parsed in `cmd/graphite-meter/main.go`) take precedence over
environment variables, which take precedence over defaults.

| Env var                                                                          | Flag        | Default                                                   | What it does                                                                                                                                                                                                                                                                                              |
| -------------------------------------------------------------------------------- | ----------- | --------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `GM_H1_ADDR` | `-h1-addr` (`-addr` legacy alias) | `:8765` | Clear HTTP/1.1 UI and measurement listener. |
| `GM_H1_TLS_ADDR` | `-h1-tls-addr` | `:8445` | Dedicated HTTPS HTTP/1.1 UI, discovery, probe, transfers, and WebSockets. |
| `GM_H2_ADDR` | `-h2-addr` | `:8443` | TCP TLS listener: HTTP/2 transfers plus HTTP/1.1 UI/discovery/probe/WebSockets. |
| `GM_H3_ADDR` | `-h3-addr` | `:8444` | UDP HTTP/3 transfers plus TCP TLS HTTP/1.1 Alt-Svc bootstrap/probe/WebSockets. |
| `GM_ENABLE_H1_TLS` / `GM_ENABLE_H2` / `GM_ENABLE_H3` | matching flags | off | Enable the corresponding native TLS listeners. |
| `GM_TLS_CERT` / `GM_TLS_KEY` | `-tls-cert` / `-tls-key` | — | Matching PEM pair. Invalid dates, hostnames, and pairs fail startup; valid renewals hot-reload. |
| `GM_SERVER_NAME`                                                                 | `-name`     | `graphite-meter`                                          | Server identity advertised in `/preflight`.                                                                                                                                                                                                                                                               |
| `GM_SERVER_LOCATION`                                                             | `-location` | —                                                         | Location label advertised in `/preflight` (e.g. `fra`).                                                                                                                                                                                                                                                   |
| `GM_TRUSTED_PROXIES`                                                             | —           | —                                                         | Comma-separated trusted-proxy CIDRs. Forwarded client addresses and `X-Forwarded-Proto` are ignored unless the socket peer matches one of them. Invalid CIDRs fail startup. See [REVERSE_PROXY.md](REVERSE_PROXY.md).                                                                                     |
| `GM_VERBOSE`                                                                     | `-verbose`  | off                                                       | Per-second throughput + connection-count logging on download/upload (see [Meter](ARCHITECTURE.md#meter-internalendpointmetergo)).                                                                                                                                                                         |
| `PUBLIC_H1_ORIGIN`, `PUBLIC_H1_TLS_ORIGIN`, `PUBLIC_H2_ORIGIN`, `PUBLIC_H3_ORIGIN` | matching flags | request host + listener port | Exact externally reachable transfer origins. Clear H1 must be `http`; all TLS targets must be `https`. |

### Local TLS and HTTP/3 certificates

Use a locally trusted CA for browser testing. A bare self-signed leaf may be
accepted through an HTTPS warning for an ordinary TCP request, but that does
not give an Alt-Svc HTTP/3 connection a usable certificate exception. The leaf
must also cover every name used in the public origins; for the standard local
setup that means `localhost`, `127.0.0.1`, and `::1`.

[`mkcert`](https://github.com/FiloSottile/mkcert) installs a development CA and
can install it into Firefox's NSS store when the platform's NSS tools are
available. Restart browsers after installing the CA.

```sh
mkdir -p .dev-certs
mkcert -install
mkcert -cert-file .dev-certs/localhost.pem \
  -key-file .dev-certs/localhost-key.pem localhost 127.0.0.1 ::1
just prod
cd go
GM_ENABLE_H1_TLS=true GM_ENABLE_H2=true GM_ENABLE_H3=true \
  GM_TLS_CERT=../.dev-certs/localhost.pem \
  GM_TLS_KEY=../.dev-certs/localhost-key.pem \
  PUBLIC_H1_ORIGIN=http://localhost:8765 \
  PUBLIC_H1_TLS_ORIGIN=https://localhost:8445 \
  PUBLIC_H2_ORIGIN=https://localhost:8443 \
  PUBLIC_H3_ORIGIN=https://localhost:8444 \
  ./graphite-meter
```

Firefox has an [additional protection](https://bugzilla.mozilla.org/show_bug.cgi?id=1985341):
by default it disables HTTP/3 when the certificate chain contains a third-party root, even when
that root is trusted for normal HTTPS. For a local development profile, open `about:config`, set
`network.http.http3.disable_when_third_party_roots_found` to `false`, and
restart Firefox. Do not weaken this setting in a normal browsing profile.

The H3 bootstrap still needs `8444/tcp`, and QUIC needs `8444/udp`. A successful
`curl --http3-only` or native-client request proves the server and UDP path, but
not browser certificate policy. In the browser, select HTTP/3 and confirm the
application reports verified browser protocol `h3`; Graphite Meter fails the
run instead of silently measuring its TCP bootstrap.

The `.dev-certs/` directory and common certificate/key extensions are ignored,
and CI rejects tracked TLS material. Never copy a private key into another
tracked path. The mkcert CA key (shown by `mkcert -CAROOT`) is especially
sensitive and must never be shared or committed. Use publicly trusted
certificates for deployed servers; mkcert is development-only.

## Native TUI client flags

```sh
just goclient-build   # -> go/graphite-meter-client
just goclient-run     # go run ./cmd/graphite-meter-client (against a running server)
```

Run settings are editable inside the TUI before a run starts. Channel-id overrides are CLI-only
until discovery-driven channel menus land:

| Flag                      | Default                   | Meaning                                                                                   |
| ------------------------- | ------------------------- | ----------------------------------------------------------------------------------------- |
| `-url`                    | `http://127.0.0.1:8765`   | Server base URL.                                                                          |
| `-transfer-target` (`-protocol` legacy alias) | `auto` | Transfer target: `auto`, `http1`, `http1-clear`, `http1-tls`, `http2`, or direct-QUIC `http3`. |
| `-latency-channel`        | `auto`                    | Advertised channel id for latency, independently of the transfer target.                   |
| `-progress-channel`       | `auto`                    | Advertised channel id for upload progress, independently of the transfer target.           |
| `-stages`                 | `latency,download,upload` | Comma list: `latency`/`ping`, `download`/`down`, `upload`/`up`, `bidirectional`/`bidi`.   |
| `-warmup`                 | `800ms`                   | Per-stage warmup before measurement starts.                                               |
| `-latency-duration`       | `4s`                      | Latency stage window.                                                                     |
| `-download-duration`      | `10s`                     | Download stage window.                                                                    |
| `-upload-duration`        | `10s`                     | Upload stage window.                                                                      |
| `-bidirectional-duration` | `10s`                     | Bidirectional stage window.                                                               |
| `-auto-streams`           | `6`                       | Maximum automatic HTTP/1 streams per direction. Native H2/H3 use one continuous request per direction.        |
| `-streams`                | `0`                       | `0` selects automatic; `1–128` forces an exact count per direction for every protocol.    |
| `-ping`                   | `medium`                  | Ping cadence: `instant` (80ms) / `medium` (250ms) / `slow` (600ms), or a raw Go duration. |
| `-loaded-latency`         | `true`                    | Measure RTT while a transfer stage is running.                                            |
| `-insecure`               | `false`                   | Skip TLS certificate verification.                                                        |

## Building the container image from source

The image is multi-stage (`client` build with `bun` → `server` build with `go` → `scratch`) and
ships a single static binary (no shell, no libc). Base images are fully qualified, so
`podman build` needs no extra config.

```sh
just container-build
podman run -d --name gm --replace -p 8765:8765 graphite-meter:latest
# open http://localhost:8765 ; stop with: podman rm -f gm
```

`container/Dockerfile` stages:

1. **`client`** (`oven/bun:canary`) — installs client deps, builds the Svelte app. Build args
   `GM_CLIENT_ENGINE`/`GM_CLIENT_ALLOW_DUMMY`/`GM_CLIENT_DEV_TOOLS`/`GM_CLIENT_BUILD_LABEL` default
   to production values (`real`/`0`/`0`/`prod`) and are promoted to env vars so `bun run build`'s
   `process.env` (read by `vite.config.ts`) sees them.
2. **`server`** (`golang:1.26`) — `go mod download`, copies `go/` and `api/` (the schema
   conformance test references `api/` by relative path), embeds the client build from stage 1,
   builds a `CGO_ENABLED=0`, stripped, trimmed, ldflags-versioned static binary.
3. **final** (`scratch`) — just the binary. It exposes 8765/tcp, 8445/tcp, 8443/tcp, and 8444/tcp+udp.
   No entrypoint shell is needed —
   config is read natively from env/flags.

To bake a configurable (non-prod-default) image, pass `--build-arg` for any client knob:

```sh
podman build -f container/Dockerfile -t graphite-meter:dev \
  --build-arg GM_CLIENT_ALLOW_DUMMY=1 --build-arg GM_CLIENT_DEV_TOOLS=1 .
```

`container/docker-compose.build.yml` wraps the same build (context = repo root, `dockerfile:
container/Dockerfile`) with the server env vars pre-wired and commented-out slots for the future
TLS ports and the client build knobs. The build-from-source Quadlet variant
(`graphite-meter.build` + `graphite-meter-source.container`) is documented in
[`container/quadlet/README.md`](../container/quadlet/README.md).

> Note: rootless Podman user-mode containers may use pasta for networking, which can significantly
> slow down throughput. To avoid this overhead, enable host networking
> (`Network=host` in the quadlet unit, `network_mode: host` in compose).
