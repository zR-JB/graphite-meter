# Architecture

Graphite Meter is a self-hosted internet speed-test tool. A Svelte 5 browser client (and a
companion native Go terminal client) drive a small, high-throughput measurement server. The
server's job is limited to serving and sinking raw **bytes** and echoing timestamps — every rate,
unit, and statistic is derived on the client side. The explicit design goal is to compare
transports honestly: the client always chooses the transport (HTTP/1.1 streams, WebSocket, and —
once wired up — HTTP/3 / WebTransport), rather than letting the browser negotiate one behind the
scenes.

## Repository layout (monorepo)

```
client/                      Svelte 5 + Tailwind browser client (bun toolchain).
go/                           Go module: the measurement server + a native Bubble Tea TUI client.
  cmd/graphite-meter/         Server entrypoint.
  cmd/graphite-meter-client/  Native terminal (Bubble Tea) client entrypoint.
  internal/config/            Server env-var/flag configuration.
  internal/server/            Listener bootstrap, mux wiring.
  internal/endpoint/          One Go type per HTTP/WS route (preflight, download, upload, ping, ...).
  internal/transport/         The Session abstraction (HTTP vs WebSocket vs, later, WebTransport).
  internal/wire/              Shared wire-protocol types (frames, opcodes, preflight structs).
  internal/goclient/          The native TUI client's measurement engine (shares the wire protocol).
  internal/rng/               The server's own RNG block generator (Go port, still load-bearing).
  internal/static/            //go:embed wrapper that serves the built Svelte client.
api/                          Cross-language contract, source of truth for client/server agreement:
                                 preflight.schema.json / preflight.golden.json  — GET /preflight shape
                                 wire.md / wire.testvectors.txt                — WS/WT message protocol
                                 rng.testvectors.txt                           — byte-exact RNG corpus
crates/rng/                   Legacy/reference Rust port of the RNG (see "About crates/rng" below).
container/                    Deployment: image-based docker-compose.yml + quadlet unit (default),
                               the multi-stage Dockerfile, and build-from-source variants.
```

## How it's built, at a glance

- **Server** (`go/cmd/graphite-meter`): one Go binary. It streams/discards raw bytes on plain
  HTTP/1.1 and runs two small WebSocket message buses for latency and upload progress. It embeds
  the built Svelte client via `//go:embed`, so the whole app ships as a single static binary.
- **Browser client** (`client/`): a Svelte 5 app built around an engine-agnostic core
  (`RunnerCore`) that consumes events from a pluggable backend. The real backend talks to the Go
  server over `fetch`/streamed HTTP and WebSocket, doing all the actual measurement work off the
  main thread in Web Workers. A synthetic "dummy" backend (dev/demo builds only) fabricates
  plausible network behavior for UI work without a live server.
- **Native TUI client** (`go/cmd/graphite-meter-client`): an independent, fully interactive Bubble
  Tea terminal client that speaks the same wire protocol as the browser client and runs the same
  stages (latency, download, upload, bidirectional) against any Graphite Meter server.
- **`api/`**: the schemas and golden/test-vector files both the Go and TypeScript sides are
  checked against, so the server and client(s) can never silently drift apart.

---

## The Go measurement server

Entry point: `go/cmd/graphite-meter/main.go`. `server.Run` (`go/internal/server/listeners.go`)
builds one shared immutable 256 KiB RNG block at startup (`internal/rng`), wires every endpoint
onto a `Registry`, and starts exactly one listener: plain HTTP/1.1 on `Config.H1Addr` (default
`:8765`). `TCP_NODELAY` is forced on every accepted connection so a single ping frame never sits
in Nagle's buffer waiting to coalesce.

### Routes

| Path | Method | Transport | Purpose |
| --- | --- | --- | --- |
| `/preflight` | GET | HTTP/1.1, JSON | Server identity, negotiated protocol, and a `capabilities` block (advertised origins/transports/endpoint paths) the client negotiates against instead of hardcoding. |
| `/download` | GET | HTTP/1.1, streamed body | Streams `?bytes=N` bytes (default 25 MiB, clamped to 64 GiB) sliced from the one shared RNG block — never regenerated per request. |
| `/upload/session` | POST | HTTP/1.1, JSON | Mints a short-lived, crypto-random `gmu_...` token (`{"uploadId": "..."}`) that correlates one upload stage's parallel POST lanes with its progress socket. |
| `/upload` | POST | HTTP/1.1, streamed body | Drains and counts an uploaded body via a pooled 256 KiB buffer; with a valid `?id=`, folds every drained chunk into a shared per-id aggregate (see below). |
| `/ws/ping` | WS upgrade | WebSocket | Stateless `PING,<id>` → `PONG,<id>;TIME,<nanos>` echo. The server keeps zero per-ping state; RTT is computed entirely client-side. |
| `/ws/upload` | WS upgrade | WebSocket | Pushes the server-measured cumulative byte count for a `?id=` (`BYTES_RECEIVED`, then one final `UPLOAD_COMPLETE`) so upload throughput is judged by what the server actually received, not what the browser thinks it sent. |
| `/` (anything unmatched) | GET | HTTP/1.1 | The embedded Svelte SPA, with SPA-aware fallback (a missing extensionless path serves `index.html`; a missing path that looks like a hashed asset 404s cleanly instead of serving HTML for it). |

The `/preflight` response also advertises `/wt/ping`, `/wt/download`, `/wt/upload` as endpoint
paths and a `webtransport` capability flag — those are placeholders for the not-yet-implemented
WebTransport stage (see Roadmap); the flag is always `false` and no route is actually mounted.

### Server-authoritative upload accounting

`internal/endpoint/upload_store.go` is the interesting piece of the upload path: a 32-shard,
mostly lock-free per-id aggregate (`bytes`, and an `activeNanos` clock that only counts time
bytes were *actually* flowing — a lane reconnect, an idle stall, or connection setup is excluded
via a 250 ms gap cap). Upload throughput is derived client-side as `Δbytes / Δserver-active-time`
from this clock, not from wall-clock time or the client's own write progress, so a stall or a lane
reconnect can't distort the reported rate. IDs must have been minted by `/upload/session` — a
flood of forged IDs on this otherwise auth-less, cookie-less bus creates no state. A background
sweeper reaps idle aggregates after 30s.

### Wire protocol (message buses only)

Plain request/response endpoints (`/preflight`, `/download`, `/upload`) are normal HTTP and not
covered by a message protocol. The two WebSocket buses (and their future WebTransport
counterparts) speak a tiny ASCII, comma-delimited protocol — one logical message per frame, no
length prefix, `OP` or `OP,arg[,arg...]`, parsed by `indexOf(',')` slicing, never JSON. An unknown
opcode or malformed frame gets a non-fatal `ERR,<code>,<text>` reply; the bus is never torn down
for one bad frame. Full spec: `api/wire.md`; shared byte-exact conformance corpus:
`api/wire.testvectors.txt` (every language's encoder/decoder must match it).

| Opcode | Direction | Shape | Meaning |
| --- | --- | --- | --- |
| `HI` | C→S | `HI,<proto>` | Optional hello (`proto` ∈ `ws`/`wt`); lets the bus be primed during warmup. |
| `READY` | S→C | `READY` | Bus is up. |
| `PING` | C→S | `PING,<id>` | Latency probe; `id` is a client-owned monotonic uint32. |
| `PONG` | S→C | `PONG,<id>;TIME,<nanos>` | Echo; `id` verbatim, server clock is diagnostics-only. |
| `SIZE` | C→S | `SIZE,<bytes>` | WebTransport download-size request — reserved, no consumer yet. |
| `BYTES_RECEIVED` | S→C | `BYTES_RECEIVED,<n>;TIME,<activeNanos>` | Running server-measured upload total + active-time clock. |
| `UPLOAD_COMPLETE` | S→C | `UPLOAD_COMPLETE,<n>;TIME,<activeNanos>` | Final upload total, sent exactly once. |
| `BYE` | C→S | `BYE` | Graceful bus close. |
| `ERR` | S→C | `ERR,<code>,<text>` | Non-fatal protocol error. |

### Meter (`internal/endpoint/meter.go`)

Not a scoring or unit-conversion system — a purely optional, `GM_VERBOSE`/`-verbose`-gated,
nil-safe per-second logger of server-observed throughput and live connection count
(`[gm:server:download] 9.41 Gbit/s · 4 conns · 1.18 GB this window`). It exists so an operator can
sanity-check the server's own drained/served rate against kernel counters or the client's
reported numbers — it never influences what the client reports.

### Session abstraction (`internal/transport`)

Every endpoint is written once against a `Session` interface (`Context`, `Query`, `ClientIP`,
`Proto`, `OpenDownloadSink`/`OpenUploadSource`, `Bus`), so it doesn't need to know whether it's
running over HTTP or a WebSocket bus. Today two concrete sessions exist — `httpSession` (h1
request/response) and `websocketSession` (message bus). A WebTransport session is named in the
interface's doc comments as the intended third implementation but does not exist yet.

`internal/config`, `internal/transport`, `internal/server`, `internal/static`, and
`internal/endpoint/registry.go` have unit tests alongside the rest of `internal/endpoint`,
`internal/wire`, and `internal/rng` — run with `just server-test`.

---

## The native Go terminal client

`go/cmd/graphite-meter-client` is a separate binary: a fully interactive Bubble Tea TUI, not a
one-shot printer. It shares the wire protocol and `/preflight` contract with the browser client
and runs the same measurement stages against any Graphite Meter server.

Stages (independently toggleable): **latency** (idle RTT baseline over `/ws/ping`), **download**,
**upload**, **bidirectional** (download and upload lanes run concurrently — this is purely
client-side orchestration; the server has no special bidirectional mode, it's just the existing
`/download` and `/upload` routes hit at the same time), and **loaded latency** (the same ping loop
run concurrently during a transfer stage, to measure RTT-under-load / bufferbloat separately from
the idle baseline).

Transport: plain streamed HTTP GET/POST for bulk transfer (HTTP/2 is explicitly disabled on this
client's `http.Transport`) and WebSocket for the ping and upload-progress buses. WebTransport is
not attempted — the `SIZE` opcode exists in the shared wire package but this client never sends
it.

To reduce measurement noise, the runner adaptively stretches warmup to roughly 10x the measured
idle RTT (floor = configured warmup, ceiling 4s) so TCP slow start finishes before the measured
window opens, and staggers parallel lane starts by up to 75ms so congestion windows don't ramp in
lockstep.

Stats: throughput as a flat mean + running peak across ~100ms samples (upload throughput is
derived from the server's authoritative byte/active-time counters, same as the browser client);
latency as min/mean/P50/P95 (linear-interpolated percentiles), jitter (mean absolute deviation),
and loss ratio.

The TUI has five configuration sections (Servers — three built-in presets plus a custom URL;
Stages; Timing; Network — stream count and TLS verification; Run) and a live telemetry view
(session panel, ASCII throughput bars, a running results log). Keys: `tab`/arrows to navigate,
`enter`/`space` to toggle or edit, `r` to run, `c`/`esc` to cancel, `q` to quit. CLI flags (all
editable again inside the TUI before a run starts) are listed in
[DEVELOPMENT.md](DEVELOPMENT.md#native-tui-client-flags).

`internal/goclient` (stats, config normalization, preflight, the adaptive-warmup/lane-stagger
runner, and the per-stage transfer lanes) and the TUI's pure helpers and `model` state machine in
`cmd/graphite-meter-client/main.go` have unit tests — also run with `just server-test` (one Go
module covers both the server and the TUI client).

---

## The Svelte browser client

The UI (`client/src/`) is deliberately engine-agnostic: it only ever talks to `RunnerCore`
(`src/lib/runner/core.ts`), which owns the phase timeline, a 20ms tick loop, a "measured
test-time" clock that freezes during a stall (so dead air doesn't count against the run), an
adaptive early-finish ("glide" toward the phase boundary once a stage's confidence stabilizes),
and dual exponential-moving-average smoothing (a fast ~700ms constant for the displayed number, a
slower ~1800ms constant for stability judgments) — both fed from the same raw samples, so the
exact byte totals a run reports never drift from what smoothing displays.

A pluggable `RunnerBackend` supplies the actual samples via a 3-call-per-stage lifecycle
(`onStageBegin` → prime/open connections, `onStageMeasure` → start pushing real samples on the
*same* primed connection, `onStageEnd`). Two backends exist:

- **`RealBackend`** (`src/lib/runner/RealRunner.ts`) — the production engine, always used in a
  release build. Negotiates a transport per stage (today this always resolves to `fetch`
  streaming for transfer and WebSocket for latency, since the server never advertises
  `webtransport`), spawns one Web Worker per parallel stream, and keeps an idle keepalive ping
  running between runs for the connectivity indicator.
- **`DummyBackend`** (`src/lib/runner/dummy.ts`) — a synthetic engine for UI development and
  demos, with five canned network profiles (fiber/cable/lte/satellite/throttled) and support for
  injecting live anomalies. Present only in dev/dummy-enabled builds; tree-shaken entirely out of
  a real-only production bundle (see [DEVELOPMENT.md](DEVELOPMENT.md#build-time-feature-flags)).

### Measurement stages

- **Latency** — a dedicated ping WebSocket run entirely inside `ping-worker.ts`, off the main
  thread so JS jank on the page never pollutes RTT. Implements an adaptive RFC-6298-style loss
  timeout, a "late-pong graveyard" so one delayed reply doesn't falsely register as loss, an
  on-receive fast path for idle sampling, and a sparser pacer under load so pings don't compete
  with an in-flight transfer.
- **Download** — `download-worker.ts`, one per parallel lane: a `fetch` GET with a streamed
  response read via a BYOB reader that reuses a single 1 MiB buffer (no per-chunk allocation —
  this is the actual read-side ceiling at multi-gigabit rates). Lane count is auto-derived from
  the browser's per-origin connection budget, capped by the user's configured ceiling.
- **Upload** — `upload-worker.ts` builds one incompressible Blob "pool" via
  `crypto.getRandomValues` (generated once, then sliced with zero-copy views — never
  regenerated per request), and POSTs adaptively-sized slices toward a 500ms target
  (`autosize.ts`). The reported throughput number is **server-authoritative**: the worker only
  reports lane liveness, and a separate dedicated `/ws/upload` connection
  (`upload-progress-worker.ts`) is the sole source of the byte count and rate, exactly mirroring
  the server's active-time clock described above.
- **Bidirectional** — download and upload lanes run concurrently on `RealBackend`, each with its
  own worker pool, aggregation cadence, and stall tracking. The browser's per-origin connection
  budget is split between the two directions (after reserving buses for the ping/upload-progress
  channels), with the user's ceiling applied per direction exactly as it is for a standalone
  download or upload stage.

### Settings

Settings live in a docked panel with up to two tabs — Setup, plus Developer in dev-tooling
builds. A production build has only Setup, so no tab bar is rendered at all.

**Setup — Run tier**

| Setting | Default | Notes |
| --- | --- | --- |
| Duration preset | Medium | Short / Medium / Long / Custom; each scales warmup + per-stage duration together. |
| Bidirectional | off | Plus its own duration field (default 10s). |
| Auto throughput ceiling | on | When off, a manual max-scale value can be set. |
| Rate unit | Bits | Bits or Bytes. |
| Prefix scale | Decimal | Decimal (SI) or Binary (IEC). |

**Setup — Tuning tier**

| Setting | Default | Notes |
| --- | --- | --- |
| Adaptive early finish | on | Plus Min coverage (0.52), Stability threshold (0.86), Glide window (1100ms). |
| Chunked download (experimental) | off | See Experimental features. |
| Ping velocity | Medium | Instant / Medium / Slow pacer. |
| Max parallel streams | 6 | Ceiling only — actual lane count is auto-derived. |
| Skip loaded latency when latency stage is off | on | |
| Include wire-rate estimates in result cards | off | Opt-in overhead-compensation display, expandable into a nested model: connection profile (LAN / Loopback / VPN tunnel / Internet), a transport & security preset (HTTP/1.1 cleartext / HTTPS / HTTP/2 / HTTP/3 — see Roadmap), per-layer framing toggles (Ethernet/IP/transport, VPN encapsulation, TLS records, HTTP/WS/QUIC framing), path-behavior toggles (ACK/control traffic, loss/retransmission), and an advanced raw byte-accounting section (MTU, TCP options, TLS record size, AEAD tag size, QUIC connection-ID length, max loss ratio, VLAN tagging). |

**Developer tab** (only in dev-tooling builds) — a debug-logging switch (verbose per-worker
console diagnostics, meant to pair with the server's `-verbose`/`GM_VERBOSE` logging) and, when
the dummy engine is also compiled in, four live anomaly-injection controls usable mid-run: latency
spike, packet loss, throughput drop, and connection drop (a full stall-then-resume).

### The Endpoint info drawer

The right-side drawer is a read-only card grid: **Client** (public IP from `/preflight`, client
build version), **Engine** (the wired runner's name, per-runner version, and its supported
transports per role — latency vs throughput, from `runner.describe()`), **Server** (name/host/
port/location, server build version, negotiated protocol — all from `/preflight`), and
**Transport** (the active transfer/latency transports, stream ceiling, pre-test ping). The
`capabilities` object the server advertises (per-transport availability booleans, per-transport
origin URLs, and every stable endpoint path) is richer than what's rendered — it's consumed
internally for transport negotiation but not yet shown as a full matrix in the UI.

### Web Workers

| Worker | Role |
| --- | --- |
| `download-worker.ts` | One per download lane; streams and discards bytes, reports periodic byte/time deltas. |
| `upload-worker.ts` | One per upload lane; builds and POSTs the incompressible payload, reports only liveness. |
| `upload-progress-worker.ts` | The authoritative upload byte/rate source, over its own `/ws/upload` connection. |
| `ping-worker.ts` | Owns the `/ws/ping` connection and the entire RTT/loss algorithm, off the main thread. |
| `autosize.ts` | Shared helper (not a worker): EWMA-smoothed, step-clamped transfer sizing used by both the upload worker and the experimental chunked-download path. |

### Testing

Unit tests (`bun:test`, run via `just client-test`) cover pure `.ts` logic only — no component
rendering (no jsdom/happy-dom/`@testing-library/svelte` in this repo). Covered so far:
`compensation.ts`, `format.ts`, `runner/adaptive.ts`, `runner/core.ts` (via a fake `RunnerBackend`
test double, exercising the full phase-lifecycle/stall/early-finish/EMA behavior without a real
network or worker), `runner/dummy.ts`, `state/persistence.ts`, `runner/workers/autosize.ts`,
`runner/workers/backoff.ts`, `runner/workers/rttEstimator.ts`, `runner/real/laneBudget.ts`,
`runner/real/wire.ts`, `runner/real/backendPure.ts` (URL/median/ping-need/lane-stagger helpers
split out of `RealRunner.ts`, mirroring `laneBudget.ts`, so they're testable without pulling in
`RealRunner.ts`'s build-time `BUILD` defines), `canvas/hoverInterp.ts` and `canvas/gaugeSweep.ts`
(pure interpolation/mapping math split out of `ChartEngine.ts`/`GaugeEngine.ts`), `runner/evaluation.ts`,
`runner/schedule.ts`, and `state/stageGuards.ts`. Follow `state/stageGuards.test.ts` as the style
model for new pure-logic tests; extract logic out of `.svelte`/rune-bearing files or classes
entangled with I/O the same way `stageGuards.ts`/`backendPure.ts`/`hoverInterp.ts` were extracted,
if it needs to be unit-tested in isolation.

---

## Experimental / not-yet-usable-end-to-end features

- **Chunked download** — an opt-in "Chunked download (experimental)" setting that requests a
  sequence of adaptively-sized chunks on one connection instead of one long stream. Implemented
  and usable today; explicitly labeled experimental because it's newer and less exercised than
  the default long-stream path.
- **WebTransport** — modeled as a transport option throughout the client's negotiation logic and
  the `/preflight` capability schema (server and client both), but not implemented on either end:
  the server never opens an HTTP/3 listener and always advertises `webtransport: false`; the
  browser client throws on that transport kind if it's ever selected. See Roadmap.
- **Server-selection seam** — both the runner contract and the real backend carry a stubbed
  `listServers()` method for a future multi-server picker (see Roadmap); it throws today and no
  UI consumes it yet.
- **TLS / HTTP/2 / HTTP/3 server config fields** — `GM_H3_ADDR`, `GM_TLS_CERT`, `GM_TLS_KEY`,
  `GM_ADVERTISE_H3`, `PUBLIC_TLS_ORIGIN`, `PUBLIC_H3_ORIGIN` all exist in `internal/config` and
  are read at startup, but no TLS or HTTP/3 listener is ever started — setting them today has no
  effect.

---

## About `crates/rng`

`crates/rng` is a Rust port of the server's scrambled-counter xorshift64* generator, kept
byte-exact against `api/rng.testvectors.txt` alongside the Go port in `go/internal/rng`. It
predates the current upload path: the browser client used to fill its upload payload with a WASM
build of this generator. It no longer does — `upload-worker.ts` now fills a reusable payload block
with `crypto.getRandomValues` once and slices it, since a reused buffer is never regenerated on
the hot path anyway, and CSPRNG bytes are just as incompressible as the xorshift output. The crate
is kept in the repository purely as a reference and as the byte-exact conformance pin for the Go
port (and as a possible starting point for a future WebTransport payload path — see Roadmap); it
is not part of the client build, the Docker image, or any required dependency of the standard
`just` workflow. `just test-rng` still runs its own conformance test if you have a Rust toolchain
and want to check it, but nothing else in the repository depends on it.

The Go server's own RNG package (`go/internal/rng`), by contrast, is not legacy — it generates the
one shared immutable block every `/download` response is sliced from, at server startup.

---

## Roadmap

Everything below is planned, in roughly increasing order of how far out it is; none of it is
implemented yet unless a section above says otherwise.

1. **Multi-protocol server.** Today the server runs a single plain HTTP/1.1 listener. Planned work
   adds native TLS termination and native HTTP/2 and HTTP/3 (QUIC) listeners as their own server
   handler stacks/endpoints, addressable independently rather than only through an external
   reverse proxy. The config surface for this already exists (`GM_H3_ADDR`, `GM_TLS_CERT`,
   `GM_TLS_KEY`, `GM_ADVERTISE_H3`, `PUBLIC_TLS_ORIGIN`, `PUBLIC_H3_ORIGIN`) but is currently
   inert.
2. **A client-side protocol/transport selector.** Once the server speaks more than HTTP/1.1, the
   browser client is planned to expose an explicit choice of which HTTP version to measure over —
   HTTP/1.1, HTTP/2, or HTTP/3 — instead of only negotiating it implicitly from what the server
   advertises. When HTTP/3 is selected, latency and throughput are planned to be configurable
   *separately*, each independently able to choose WebTransport unreliable datagrams (for
   loss-tolerant, minimal-overhead probing) instead of the existing reliable channel (WebSocket for
   latency, fetch streams for throughput). Two seams for this already exist: the Settings
   "Transport & security" preset (HTTP/1.1 / HTTPS / HTTP/2 / HTTP/3), which today only feeds the
   overhead-compensation byte math, and the runner contract's `EngineInfo` (`runner.describe()`),
   which carries per-role supported-transport lists (latency vs throughput) rendered in the
   Endpoint info drawer — the selection UI will offer exactly those lists.
3. **A server-selection UI.** Planned so the primary server — the one that serves the web page —
   can be configured, via a TOML file or environment variables, with a list of other Graphite
   Meter server instances running elsewhere. The browser client would then let the user pick which
   configured server to actually run the test against, instead of always testing the server that
   served the page. The runner contract already carries a stubbed `listServers()` method as a seam
   for this; no UI consumes it yet.
4. **Far future, speculative: simultaneous multi-server testing.** Running a test against several
   configured servers at once, in a single pass, to compare routes or providers directly.
5. **Far future, speculative: a Rust server implementation.** An alternative to the Go server,
   using `s2n-quic-h3` for the QUIC/HTTP/3/WebTransport side. No `server-rs/` directory exists in
   this checkout yet.
6. **Far future, speculative: a native Rust TUI client**, as a Rust-side counterpart to the
   existing Go Bubble Tea client.
