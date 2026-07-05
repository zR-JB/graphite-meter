# Development

How to build, run, and test everything from source. For a normal deployment of the published
image, see the [README quick start](../README.md#quick-start).

## Prerequisites

- **Go 1.26+** — the measurement server and native TUI client.
- **Bun 1.4+** — the client toolchain.
- **[`just`](https://github.com/casey/just)** — every workflow in this repo goes through the
  `justfile`. Run `just` with no arguments to list every recipe.

A Rust toolchain is **not** required for normal development, building, or running the app — see
["About `crates/rng`"](ARCHITECTURE.md#about-cratesrng).

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

| Recipe | What it does |
| --- | --- |
| `just` (no args) | Lists every recipe. |
| `just dev` | Builds + embeds the dev-profile client, then `go run`s the server on `:8765`. |
| `just prod` | Builds + embeds the prod-profile client, then `go run`s the version-stamped server on `:8765`. |
| `just client-build-dev` | `bun install` + `bun run build` -> `client/dist`, dev profile (Vite's own defaults: dummy engine + dev tools included). |
| `just client-build-prod` | Same, but real-only engine and dev tooling stripped by default — accepts the `GM_CLIENT_*` knobs inline (see below). |
| `just client-watch` | Vite dev server only — hot reload, no Go server, no embedding, no live measurement backend. |
| `just client-check` | Type-checks the client (`svelte-check`). |
| `just client-test` | `bun test` — pure-`.ts`-logic unit tests (no component rendering). |
| `just client-gen-types` | Regenerates `client/src/lib/api/preflight.ts` from `api/preflight.schema.json` (the schema is the source of truth). |
| `just server-build-dev` | Builds + embeds the dev-profile client, then builds `go/graphite-meter` as a persisted, stripped (`-s -w -trimpath`) binary — no version stamp, nothing runs it. |
| `just server-build-prod` | Same, prod profile, plus the ldflags version stamp — the shippable binary for a manual/non-Docker deploy. |
| `just server-test` | `go test ./...` — includes the `/preflight` schema-conformance test. |
| `just goclient-build` | Builds only `go/graphite-meter-client` — does not touch the Svelte client. |
| `just goclient-run` | `go run`s the native TUI client against a running server. |
| `just container-build` | `docker build -f container/Dockerfile -t graphite-meter:latest .` |
| `just test-rng` | Runs the legacy Rust RNG crate's own conformance test. Optional; requires a Rust toolchain only if you run it. |

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

| Variable | Values | `just dev`/`client-build-dev` default | `just prod`/`client-build-prod` default | What it does |
| --- | --- | --- | --- | --- |
| `GM_CLIENT_ENGINE` | `real` / `dummy` | `real` | `real` | Default runner when more than one is compiled in. |
| `GM_CLIENT_ALLOW_DUMMY` | `0` / `1` | `1` | `0` | Compile in the dummy runner and the Developer-tab anomaly-injection cards. |
| `GM_CLIENT_DEV_TOOLS` | `0` / `1` | `1` | `0` | Compile in the whole Developer settings tab (including debug logging). |
| `GM_CLIENT_BUILD_LABEL` | string | `dev` | git short hash | Text shown after `build` in the status bar. Also the label half of the client version `<package.json semver>+<label>`, which is shown in the Endpoint info drawer, written to `dist/version.json`, and sent to the server on preflight as `?client=web&client_version=…`. |

At runtime, when the dummy runner is compiled in, `?engine=dummy` on the URL (or a previously
persisted choice in `localStorage`) switches to it; this check itself compiles away in a
dummy-stripped build, so it can't be re-enabled by URL trickery in a production build.

## Server run-time configuration

Read once at startup; flags (parsed in `cmd/graphite-meter/main.go`) take precedence over
environment variables, which take precedence over defaults.

| Env var | Flag | Default | What it does |
| --- | --- | --- | --- |
| `GM_H1_ADDR` | `-addr` | `:8765` | HTTP/1.1 listen address — the only listener that runs today. |
| `GM_SERVER_NAME` | `-name` | `graphite-meter` | Server identity advertised in `/preflight`. |
| `GM_SERVER_LOCATION` | `-location` | — | Location label advertised in `/preflight` (e.g. `fra`). |
| `GM_VERBOSE` | `-verbose` | off | Per-second throughput + connection-count logging on download/upload (see [Meter](ARCHITECTURE.md#meter-internalendpointmetergo)). |
| `PUBLIC_H1_ORIGIN` | — | derived from request `Host` | Public HTTP/1.1 origin to advertise — set this behind a reverse proxy so the client targets the right URL. |
| `PUBLIC_TLS_ORIGIN` | — | derived from request when forwarded as `https` | Public encrypted origin to advertise (`capabilities.origins.tls`) — the client prefers this for the WebSocket latency bus, mapped to `wss://`. Auto-derived from the request `Host` whenever the server sees `X-Forwarded-Proto: https` (the default from nginx/Caddy/Traefik terminating TLS in front); set explicitly only if that header isn't forwarded, or the public host/port differs from what the proxy sends upstream. This works even though the server itself has no TLS listener — it only affects what origin gets *advertised*. |
| `GM_H3_ADDR`, `GM_TLS_CERT`, `GM_TLS_KEY`, `GM_ADVERTISE_H3`, `PUBLIC_H3_ORIGIN` | — | off / unset | Reserved for the HTTP/2 + HTTP/3 stage. Read at startup but no TLS/H3 listener exists yet, so these currently have no runtime effect. |

## Native TUI client flags

```sh
just goclient-build   # -> go/graphite-meter-client
just goclient-run     # go run ./cmd/graphite-meter-client (against a running server)
```

All flags are editable again inside the TUI before a run starts:

| Flag | Default | Meaning |
| --- | --- | --- |
| `-url` | `http://127.0.0.1:8765` | Server base URL. |
| `-stages` | `latency,download,upload` | Comma list: `latency`/`ping`, `download`/`down`, `upload`/`up`, `bidirectional`/`bidi`. |
| `-warmup` | `800ms` | Per-stage warmup before measurement starts. |
| `-latency-duration` | `4s` | Latency stage window. |
| `-download-duration` | `10s` | Download stage window. |
| `-upload-duration` | `10s` | Upload stage window. |
| `-bidirectional-duration` | `10s` | Bidirectional stage window. |
| `-streams` | `4` | Parallel transfer lanes (clamped 1–128). |
| `-ping` | `medium` | Ping cadence: `instant` (80ms) / `medium` (250ms) / `slow` (600ms), or a raw Go duration. |
| `-loaded-latency` | `true` | Measure RTT while a transfer stage is running. |
| `-insecure` | `false` | Skip TLS certificate verification. |

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
3. **final** (`scratch`) — just the binary. `EXPOSE 8765/tcp` (a commented `8443/tcp` and
   `8443/udp` mark where the future TLS/H3 listener will attach). No entrypoint shell is needed —
   config is read natively from env/flags.

To bake a configurable (non-prod-default) image, pass `--build-arg` for any client knob:

```sh
podman build -f container/Dockerfile -t graphite-meter:dev \
  --build-arg GM_CLIENT_ALLOW_DUMMY=1 --build-arg GM_CLIENT_DEV_TOOLS=1 .
```

`container/docker-compose.build.yml` wraps the same build (context = repo root, `dockerfile:
container/Dockerfile`) with the server env vars pre-wired and commented-out slots for the future
TLS/HTTP/3 ports and the client build knobs. The build-from-source Quadlet variant
(`graphite-meter.build` + `graphite-meter-source.container`) is documented in
[`container/quadlet/README.md`](../container/quadlet/README.md).

> Note: rootless Podman user-mode containers may use pasta for networking, which can significantly
> slow down throughput. To avoid this overhead, enable host networking
> (`Network=host` in the quadlet unit, `network_mode: host` in compose).
