# Graphite Meter

A self-hosted internet speed-test tool: a Svelte 5 browser client driving a high-throughput,
low-footprint measurement server. The server serves/sinks raw **bytes**; the client derives all
rates and units. Designed to compare web transports (HTTP/1.1, HTTP/2, HTTP/3, WebSocket,
WebTransport) honestly — the client always picks the transport, never the browser.

## Repository layout (monorepo)

```
client/          Svelte 5 + Tailwind browser client (bun). The engine-agnostic UI + RunnerCore.
server/          Go measurement server (quic-go / webtransport-go). Serves the embedded client.
server-rs/       RESERVED slot for a future Go->Rust server swap (not created yet).
crates/rng/      Canonical xorshift64* RNG crate -> WASM (browser upload) + native ports (later).
api/             Single source of truth for the cross-language contract:
                   preflight.schema.json  (HTTP JSON API -> generated TS + Go conformance test)
                   wire.md / wire.testvectors.txt  (WS/WT message protocol -> shared golden vectors)
                   rng.testvectors.txt  (byte-exact RNG agreement)
docker/          Multi-stage image: bun client build + go server -> single static binary.
docs/            ARCHITECTURE.md (the "why"), REAL_RUNNER.md, THROUGHPUT_MEASUREMENT.md, GLOSSARY.md.
reference-demos/ Proven standalone browser demos (download / ping / upload) — implementation refs.
masterplan.md    Full product/UX blueprint.
```

## Prerequisites

- **Go 1.26+** — the measurement server.
- **Bun 1.4+** — the client toolchain.
- **Rust toolchain + `wasm-pack`** — builds the upload RNG to WASM (`crates/rng`). Install with
  `rustup` (puts cargo on `~/.cargo/bin`) and `cargo install wasm-pack`. Only needed for local
  builds; the Docker image builds WASM itself in a dedicated stage.

All workflows go through the `justfile` ([casey/just](https://github.com/casey/just)). Run `just`
with no args to list every recipe.

## Quick start (development)

```sh
just dev          # build WASM + client, embed into the Go server, run it on :8080
```

Open **http://localhost:8080**. The server serves the app and `GET /preflight`. `just dev` rebuilds
the WASM RNG, the client, embeds the result via `//go:embed`, and runs the Go binary. By default the
client now uses the **real** runner (talks to the live server). The synthetic **dummy** runner (no
server needed) is still available in dev builds via `?engine=dummy`.

Other dev recipes:

```sh
just dev-client   # Vite dev server only (hot reload, no Go server)
just check        # type-check the client (svelte-check)
just build-wasm   # (re)build the upload RNG -> client/src/lib/wasm/rng
just gen-types    # regenerate client/src/lib/api/preflight.ts from api/preflight.schema.json
just test-server  # Go tests (incl. preflight-schema conformance)
just test-rng     # byte-exact RNG conformance test
```

## Production build

`just prod` builds a **real-only** binary: the dummy runner and the entire Developer settings tab
(debug logging + loss/anomaly injection) are tree-shaken out of the client bundle, and the status
bar shows the git short hash instead of `dev`.

```sh
just prod         # -> server/graphite-meter  (real-only, dev tooling stripped)
```

Override the [build-time knobs](#client-build-time-configuration) inline, e.g. keep dummy + dev
tools in but still version it:

```sh
just prod allow_dummy=1 dev_tools=1 label=0.2.0
```

## Docker / Podman

The image is multi-stage (`wasm` → `client` → `server` → `scratch`) and ships a single ~6.7 MB
static binary. Base images are fully qualified, so **`podman build` works with no extra config**.

```sh
just image                                          # build graphite-meter:latest (prod defaults)
podman run -d --name gm --replace -p 8080:8080 graphite-meter:latest
# open http://localhost:8080  ;  stop with:  podman rm -f gm
```

`just image` wraps `docker build -f docker/Dockerfile -t graphite-meter:latest .` (swap `docker`
for `podman` if that's your runtime). To bake a configurable image, pass `--build-arg` for any
`GM_CLIENT_*` knob:

```sh
podman build -f docker/Dockerfile -t graphite-meter:dev \
  --build-arg GM_CLIENT_ALLOW_DUMMY=1 --build-arg GM_CLIENT_DEV_TOOLS=1 .
```

> If host port 8080 is busy, map another, e.g. `-p 8099:8080`, and open `:8099`.

## Configuration

There are two layers: **client build-time** knobs (baked into the bundle; need a rebuild to change)
and **server run-time** config (env vars / flags read by the running binary).

### Client build-time configuration

Set as env vars for `just prod` / `bun run build`, or as `--build-arg` for Docker. Read by
`client/vite.config.ts` and injected via Vite `define`, so disabled features are tree-shaken out
entirely — not just hidden.

| Variable | Values | Dev default | Prod default | What it does |
| --- | --- | --- | --- | --- |
| `GM_CLIENT_ENGINE` | `real` / `dummy` | `real` | `real` | default runner when more than one is built in |
| `GM_CLIENT_ALLOW_DUMMY` | `0` / `1` | `1` | `0` | include the synthetic dummy runner **and** the loss/anomaly simulation cards |
| `GM_CLIENT_DEV_TOOLS` | `0` / `1` | `1` | `0` | include the whole Developer settings tab (debug logging included) |
| `GM_CLIENT_BUILD_LABEL` | string | `dev` | git short hash | text shown after `build` in the status bar |

### Server run-time configuration

Read at startup; flags take precedence over env vars. All optional with sensible defaults.

| Env var | Flag | Default | What it does |
| --- | --- | --- | --- |
| `GM_H1_ADDR` | `-addr` | `:8080` | HTTP/1.1 listen address |
| `GM_SERVER_NAME` | `-name` | `graphite-meter` | server identity advertised in `/preflight` |
| `GM_SERVER_LOCATION` | `-location` | — | location label advertised in `/preflight` |
| `GM_VERBOSE` | `-verbose` | off | per-second throughput logging on the download/upload endpoints |
| `PUBLIC_H1_ORIGIN` | — | derived from request | public origin to advertise (set behind a reverse proxy) |
| `GM_ADVERTISE_H3`, `GM_H3_ADDR`, `GM_TLS_CERT`, `GM_TLS_KEY`, `PUBLIC_TLS_ORIGIN`, `PUBLIC_H3_ORIGIN` | — | off / — | reserved for the TLS + HTTP/3 (WebTransport) stage |

```sh
podman run -d -p 8080:8080 \
  -e GM_SERVER_NAME=fra-1 -e GM_SERVER_LOCATION=fra -e GM_VERBOSE=1 \
  graphite-meter:latest
```

## Status

Backend build is staged (see `docs/ARCHITECTURE.md`). Stage 1: monorepo + server skeleton serving the
client + `/preflight` over HTTP/1.1. Later stages add download, upload, WebSocket latency, and
WebTransport/HTTP/3.

## License

AGPL-3.0-or-later. See `LICENSE`.
