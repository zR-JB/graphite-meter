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
docs/            ARCHITECTURE.md (the "why"), REAL_RUNNER.md, GLOSSARY.md.
reference-demos/ Proven standalone browser demos (download / ping / upload) — implementation refs.
masterplan.md    Full product/UX blueprint.
```

## Build & run

Requires **Go 1.26+** and **Bun 1.4+**. Convenience targets live in the `justfile`.

```sh
just build-client     # cd client && bun install && bun run build  -> client/dist
just build-server     # copy client/dist into the server and build the Go binary
just dev              # run the Go server locally (serves the built client on http://localhost:8080)
just gen-types        # regenerate api/gen/preflight.ts from preflight.schema.json
just image            # build the production Docker image
```

The server then serves the app and `GET /preflight` on `http://localhost:8080`.

## Status

Backend build is staged (see `docs/ARCHITECTURE.md`). Stage 1: monorepo + server skeleton serving the
client + `/preflight` over HTTP/1.1. Later stages add download, upload, WebSocket latency, and
WebTransport/HTTP/3.

## License

AGPL-3.0-or-later. See `LICENSE`.
