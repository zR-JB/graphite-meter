# Architecture

Graphite Meter is a self-hosted internet speed-test tool. A Svelte 5 browser client (and a
companion native Go terminal client) drive a small, high-throughput measurement server. The
server's job is limited to serving and sinking raw **bytes** and echoing timestamps — every rate,
unit, and statistic is derived on the client side. The explicit design goal is to compare
transports honestly: discovery identifies one logical server, then the client independently
freezes a throughput target and a latency target for the run. Fetch throughput may use HTTP/1.1,
HTTP/2, or HTTP/3; WebSockets remain a dedicated HTTP/1.1 latency transport. Upload progress is
part of the selected throughput target and is never coupled to latency.

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
  internal/static/            //go:embed wrapper that serves the built Svelte client.
api/                          Cross-language contract, source of truth for client/server agreement:
                                 preflight schema/golden — logical discovery
                                 probe schema/golden     — selected-path evidence
                                 wire.md / wire.testvectors.txt                — WS/WT message protocol
container/                    Deployment: image-based docker-compose.yml + quadlet unit (default),
                               the multi-stage Dockerfile, and build-from-source variants.
```

## How it's built, at a glance

- **Server** (`go/cmd/graphite-meter`): one Go binary. It streams/discards raw bytes and serves
  authoritative NDJSON upload progress over the selected H1/H2/H3 throughput listener, while a
  dedicated H1 WebSocket bus handles latency. It embeds the built Svelte client via `//go:embed`,
  so the whole app ships as a single static binary.
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
builds the random block, upload store, meters, and endpoint implementations once. Listener-specific
muxes expose clear H1 on 7246/tcp, optional TLS-only H1 on 7247/tcp, optional H2 on 7248/tcp,
and optional H3 on 7249/udp with a TLS H1 bootstrap on 7249/tcp. The H3 TCP surface has only
`/probe`, so transfers and latency cannot silently fall back to H1. QUIC 0-RTT is
disabled to prevent POST replay.

| Listener | Protocol | Owned surface |
| --- | --- | --- |
| `:7246/tcp` | HTTP/1.1 clear | UI, discovery, probe, transfers, upload progress, and clear WebSocket latency. |
| `:7247/tcp` | HTTP/1.1 TLS only | HTTPS UI, discovery, probe, transfers, upload progress, and WSS latency. ALPN offers only HTTP/1.1. |
| `:7248/tcp` | HTTP/2 only | H2 UI, discovery, probe, transfers, and upload progress. No H1 ALPN or WebSocket route. |
| `:7249/udp` | HTTP/3 | H3 probe, transfers, and upload progress. |
| `:7249/tcp` | HTTP/1.1 TLS bootstrap | Alt-Svc bootstrap probe only; no UI, discovery, transfers, progress, or WebSockets. |

Discovery separates `capabilities.throughputTargets` from `capabilities.latencyTargets`. A run
freezes one target for each role and verifies each target independently. Fetch throughput targets
own probe, transfer, session, and NDJSON progress routes. Latency targets own a probe and ping
route. The browser always fetches `/preflight` from the page origin; it never reconstructs target
ports locally, and every subsequent HTTP or WebSocket URL comes from that discovery document.
Today only WebSocket over dedicated H1 clear/TLS origins is advertised for latency;
H2/H3 WebSockets and WebTransport remain unadvertised. WebSockets over H2 or H3 Extended CONNECT are specified by
[RFC 8441](https://www.rfc-editor.org/rfc/rfc8441) and
[RFC 9220](https://www.rfc-editor.org/rfc/rfc9220), but the current implementation uses the widely
interoperable HTTP/1.1 Upgrade.

The dedicated H1-TLS listener is a real transfer target, not a control fallback. It exists because
browsers cannot be forced to use H1 on an origin that advertises H2 through ALPN. Operators using a
reverse proxy must likewise give H1-TLS its own origin/port or disable H2 on that virtual host.

### Routes

| Path                     | Method     | Transport               | Purpose                                                                                                                                                                                                                      |
| ------------------------ | ---------- | ----------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `/preflight`             | GET        | UI origins, JSON  | Logical server identity plus independent throughput and latency target catalogs. Refreshed before every run. |
| `/probe`                 | GET        | selected H1/H2/H3       | Client IP/source and server-observed protocol for the actual selected path. H3 TCP also returns `Alt-Svc` and closes. |
| `/download`              | GET        | selected fetch target, streamed body | Streams `?bytes=N` bytes (default 25 MiB, clamped to 64 GiB) sliced from the one shared random block — never regenerated per request.                                                                                        |
| `/upload/session`        | POST       | selected throughput target, JSON | Mints a short-lived `gmu_...` token correlating one upload stage's POST lanes and progress stream. |
| `/upload`                | POST       | selected fetch target, streamed body | Drains and counts an uploaded body via a pooled 256 KiB buffer; with a valid `?id=`, folds every drained chunk into a shared per-id aggregate (see below).                                                                   |
| `/ws/ping`               | WS upgrade | WebSocket               | Stateless `PING,<id>` → `PONG,<id>;TIME,<nanos>` echo. The server keeps zero per-ping state; RTT is computed entirely client-side.                                                                                           |
| `/upload/progress`       | GET / DELETE | selected throughput target, NDJSON | GET flushes `ready`, then server-timed `progress`, `complete`, or terminal `error` objects; blank lines are heartbeats. DELETE explicitly finalizes the stage after POST lanes stop. |
| `/` (anything unmatched) | GET        | H1/H1-TLS/H2 UI listeners | The embedded Svelte SPA, with SPA-aware fallback (a missing extensionless path serves `index.html`; a missing path that looks like a hashed asset 404s cleanly instead of serving HTML for it).                              |

No WebTransport channel is advertised and no route is mounted. The `webtransport-go` dependency
and contract variants remain reserved for Stage 5; its HTTP/3 server wiring stays commented out
until actual WebTransport endpoints exist and are advertised.

### Server-authoritative upload accounting

`internal/endpoint/upload_store.go` is the interesting piece of the upload path: a 32-shard,
mostly lock-free per-id aggregate (`bytes` plus a monotonic first-byte time anchor). Upload
throughput is derived client-side as `Δbytes / Δserver-elapsed-time`; clients baseline both values
when measurement begins, excluding warmup while retaining stalls, reconnects and lane turnaround.
This avoids client event-loop timing artifacts without inflating results by removing pauses. IDs
are short-lived HMAC-authenticated tokens minted without server-side session state; forged IDs and
`/upload/session` floods therefore allocate no aggregate state. A background sweeper reaps idle
aggregates after 30s.

### Wire protocol (message buses only)

Plain request/response endpoints (`/preflight`, `/probe`, `/download`, `/upload`, and
`/upload/progress`) are normal HTTP and not covered by a message protocol. The WebSocket latency
bus (and future WebTransport latency counterparts) speaks a tiny ASCII protocol — one message per frame, no
length prefix, `OP` or `OP,arg[,arg...]`, parsed by `indexOf(',')` slicing, never JSON. An unknown
opcode or malformed frame gets a non-fatal `ERR,<code>,<text>` reply; the bus is never torn down
for one bad frame. Full spec: `api/wire.md`; shared byte-exact conformance corpus:
`api/wire.testvectors.txt` (every language's encoder/decoder must match it).

| Opcode            | Direction | Shape                              | Meaning                                                                     |
| ----------------- | --------- | ---------------------------------- | --------------------------------------------------------------------------- |
| `HI`              | C→S       | `HI,<proto>`                       | Optional hello (`proto` ∈ `ws`/`wt`); lets the bus be primed during warmup. |
| `READY`           | S→C       | `READY`                            | Bus is up.                                                                  |
| `PING`            | C→S       | `PING,<id>`                        | Latency probe; `id` is a client-owned monotonic uint32.                     |
| `PONG`            | S→C       | `PONG,<id>;TIME,<nanos>`           | Echo; `id` verbatim, server clock is diagnostics-only.                      |
| `SIZE`            | C→S       | `SIZE,<bytes>`                     | WebTransport download-size request — reserved, no consumer yet.             |
| `BYE`             | C→S       | `BYE`                              | Graceful bus close.                                                         |
| `ERR`             | S→C       | `ERR,<code>,<text>`                | Non-fatal protocol error.                                                   |

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
`internal/endpoint/registry.go` have unit tests alongside the rest of `internal/endpoint`
and `internal/wire` — run with `just server-test`.

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

Transport: protocol-specific streamed HTTP GET/POST clients for throughput and its NDJSON upload
progress, plus an independently selected HTTP/1.1 WebSocket for latency. WebTransport is not
attempted — the `SIZE` opcode exists in the shared wire package but this client never sends it.

To reduce measurement noise, the runner adaptively stretches warmup to roughly 10x the measured
idle RTT (floor = configured warmup, ceiling 4s) so TCP slow start finishes before the measured
window opens, and staggers parallel lane starts by up to 75ms so congestion windows don't ramp in
lockstep.

Stats: effective throughput as receiver-counted bytes over the measured window, plus a running
peak across ~100ms samples (upload uses the server receiver, download the local client receiver);
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
(`src/lib/runner/core.ts`), which owns the phase timeline, a 20ms tick loop, a measured clock that
retains stalls in effective throughput while bounding recovery with a max timeout, an
adaptive early-finish ("glide" toward the phase boundary once a stage's confidence stabilizes),
and dual exponential-moving-average smoothing (a fast ~700ms constant for the displayed number, a
slower ~1800ms constant for stability judgments) — both fed from the same raw samples, so the
exact byte totals a run reports never drift from what smoothing displays.

Each run enters a visible `connecting` phase while the selected target is probed and verified.
Only after that succeeds does the measurement timeline start. Stage preparation may also be
asynchronous: `RunnerCore` holds that stage at zero until `onStageBegin` resolves, then gives the
primed connection the full configured warmup interval. Discovery, upload-session allocation, and
other setup work therefore cannot silently consume measured or warmup time.

A pluggable `RunnerBackend` supplies the actual samples via a 3-call-per-stage lifecycle
(`onStageBegin` → prime/open connections and signal readiness, `onStageMeasure` → start pushing
real samples on the _same_ primed connection, `onStageEnd`). Two backends exist:

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
  this is the actual read-side ceiling at multi-gigabit rates). Automatic stream policy uses the
  browser's per-origin connection budget, bounded by the configured H1 ceiling, and one request
  stream for HTTP/2/HTTP/3.
- **Upload** — `upload-worker.ts` builds one incompressible Blob "pool" via
  `crypto.getRandomValues` (generated once, then sliced with zero-copy views — never
  regenerated per request), and POSTs adaptively-sized slices toward a 500ms target
  (`autosize.ts`). Automatic H2/H3 uses three overlapping POST lanes so the connection keeps
  carrying data while another finite request waits for its response. The shared 64 MiB reservoir
  is divided across those lanes. The reported throughput number is **server-authoritative**: the worker only
  reports lane liveness, and a separate dedicated `/upload/progress` fetch
  (`upload-progress-worker.ts`) is the sole source of the byte count and rate, exactly mirroring
  the server's elapsed-time clock described above.
- **Bidirectional** — download and upload lanes run concurrently on `RealBackend`, each with its
  own worker pool, aggregation cadence, and stall tracking. Automatic HTTP/1.1 splits the
  available connection budget between directions after reserving the progress request and latency socket and applies the
  configured ceiling to each share; automatic HTTP/2 and HTTP/3 use one download request and
  three overlapping upload requests on one multiplexed connection. Forced policy starts the exact configured request count independently for each
  direction and protocol. HTTP/1.1 requests over the browser's connection limit can be queued.

### Settings

Settings use a docked panel on wide layouts, a side flyout on smaller desktops, and a draggable
bottom sheet on mobile. There are up to two tabs — Setup, plus Developer in dev-tooling builds.
A production build has only Setup, so no tab bar is rendered at all.

**Setup — Test tier**

| Setting         | Default | Notes                                                                                                                                            |
| --------------- | ------- | ------------------------------------------------------------------------------------------------------------------------------------------------ |
| Duration preset | Medium  | Short / Medium / Long apply warmup and every enabled stage duration together. Custom exposes the individual millisecond fields.                  |
| Bidirectional   | off     | Adds concurrent download + upload. Its individual duration is shown only for Custom; named presets supply their matching bidirectional duration. |

**Setup — Results tier**

| Setting                                     | Default | Notes                                                                                                                                                                                                                                          |
| ------------------------------------------- | ------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Rate unit                                   | Bits    | Bits or Bytes.                                                                                                                                                                                                                                 |
| Prefix scale                                | Decimal | Decimal (SI) or Binary (IEC).                                                                                                                                                                                                                  |
| Auto throughput ceiling                     | on      | When off, a manual max-scale value can be set.                                                                                                                                                                                                 |
| Include wire-rate estimates in result cards | off     | Estimates forward-direction physical Ethernet occupancy. Resource Timing detects the browser-facing HTTP protocol; expert settings cover MTU, IP version, TCP-option range, VLAN, QUIC connection-ID range, and explicit tunnel encapsulation. |

**Setup — Advanced tier**

| Setting                                       | Default   | Notes                                                                                                    |
| --------------------------------------------- | --------- | -------------------------------------------------------------------------------------------------------- |
| Adaptive early finish                         | on        | Plus Min coverage (0.52), Stability threshold (0.86), Glide window (1100ms).                             |
| Ping velocity                                 | Medium    | Instant / Medium / Slow pacer.                                                                           |
| Transfer stream policy                        | Automatic | H1 derives from the connection pool with a configurable ceiling (default 6); H2/H3 use one download and three overlapping upload requests. Forced uses the configured count exactly for every protocol. |
| Skip loaded latency when latency stage is off | on        |                                                                                                          |
| Chunked download (experimental)               | off       | See Experimental features.                                                                               |

Wire estimates deliberately stop at the browser's first hop. Behind a terminating reverse proxy,
`PerformanceResourceTiming.nextHopProtocol` describes browser→proxy while the selected `/probe`'s
`protocolNegotiated` describes what Go observed; the two are retained separately. The model composes
application framing, TLS/QUIC protection, packetization, optional tunnel encapsulation, and
Ethernet framing once. Reverse ACKs belong to the other full-duplex direction, while browser CPU,
stability, ramp-up, ping timeouts, and proxy buffering describe achieved goodput or measurement
quality rather than invisible protocol bytes, so none of them increases the estimate.
The IP-family input defaults to the normalized client address reported by `/probe`; forwarding
headers contribute only through a peer in `GM_TRUSTED_PROXIES`. The UI shows whether that evidence
came from the socket or a trusted proxy and retains IPv4/IPv6 overrides because translation,
overlays, and upstream load balancers can make the presented family ambiguous.

**Developer tab** (only in dev-tooling builds) — a debug-logging switch (verbose per-worker
console diagnostics, meant to pair with the server's `-verbose`/`GM_VERBOSE` logging) and, when
the dummy engine is also compiled in, four live anomaly-injection controls usable mid-run: latency
spike, packet loss, throughput drop, and connection drop (a full stall-then-resume).

### The Endpoint info drawer

The right-side drawer is a responsive read-only card grid: **Client** (normalized IP, address
family and detection source from `/probe`, plus client build version), **Engine** (the wired
runner's name, per-runner version, and its supported transports per role — latency vs throughput,
from `runner.describe()`), **Server** (node, location, endpoint, and server build version from
`/preflight`), and **Connection** (selected target, browser-verified and server-observed protocols, transfer/latency
transports, resolved automatic or forced stream policy, and pre-test ping). The
`capabilities.throughputTargets` and `capabilities.latencyTargets` are resolved independently.
The drawer shows both frozen ids and both verified protocols; upload progress is shown as part of
the throughput path.

### Web Workers

| Worker                      | Role                                                                                                                                                 |
| --------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------- |
| `download-worker.ts`        | One per download lane; streams and discards bytes, reports periodic byte/time deltas.                                                                |
| `upload-worker.ts`          | One per upload lane; builds and POSTs the incompressible payload, reports only liveness.                                                             |
| `upload-progress-worker.ts` | The authoritative upload byte/rate source, parsing NDJSON from the selected throughput target.                                                         |
| `ping-worker.ts`            | Owns the `/ws/ping` connection and the entire RTT/loss algorithm, off the main thread.                                                               |
| `autosize.ts`               | Shared helper (not a worker): EWMA-smoothed, step-clamped transfer sizing used by both the upload worker and the experimental chunked-download path. |

### Testing

Unit tests (`bun:test`, run via `just client-test`) cover pure `.ts` logic only — no component
rendering (no jsdom/happy-dom/`@testing-library/svelte` in this repo). Covered so far:
`compensation.ts`, `format.ts`, `runner/adaptive.ts`, `runner/core.ts` (via a fake `RunnerBackend`
test double, exercising the full phase-lifecycle/stall/early-finish/EMA behavior without a real
network or worker), `runner/dummy.ts`, `state/persistence.ts`, `runner/workers/autosize.ts`,
`runner/workers/backoff.ts`, `runner/workers/rttEstimator.ts`, `runner/real/streamPolicy.ts`,
`runner/real/wire.ts`, `runner/real/backendPure.ts` (URL/median/ping-need/lane-stagger helpers
split out of `RealRunner.ts`, so they're testable without pulling in
`RealRunner.ts`'s build-time `BUILD` defines), `canvas/hoverInterp.ts` and `canvas/gaugeSweep.ts`
(pure interpolation/mapping math split out of `ChartEngine.ts`/`GaugeEngine.ts`), `runner/evaluation.ts`,
`runner/schedule.ts`, and `state/stageGuards.ts`. Follow `state/stageGuards.test.ts` as the style
model for new pure-logic tests; extract logic out of `.svelte`/rune-bearing files or classes
entangled with I/O the same way `stageGuards.ts`/`backendPure.ts`/`hoverInterp.ts` were extracted,
if it needs to be unit-tested in isolation.

Client type checking is a separate gate from test execution. `just client-check` runs
`svelte-check` with Bun's test globals enabled, so test files are semantically checked instead of
only transpiled by `bun test`; it also runs `tsc` over the Vite config.

---

## Experimental and roadmap

- **Chunked download** is an opt-in adaptive-chunk alternative to long streams.
- **WebTransport** remains future work. It is neither mounted nor advertised; H3 throughput and
  upload progress use fetch over QUIC, while latency independently uses an H1-TLS WebSocket.
- **Server selection and simultaneous multi-server testing** remain future work. Protocol targets
  in one discovery document are listeners of one logical server, not independent servers.

## TLS security and lifecycle

H1-TLS, H2, and H3 are opt-in and fail before binding when the PEM pair is missing, mismatched,
outside its validity window, or incompatible with a configured public TLS hostname. Private-key
permissions and certificates expiring within 30 days produce warnings. Files are checked every
minute and a complete valid renewal is swapped atomically; a partial or invalid renewal leaves the
last valid certificate serving. No private-key bytes are logged. The Compose overlay mounts the
complete Let's Encrypt tree read-only so `live/` symlinks into `archive/` remain usable.
