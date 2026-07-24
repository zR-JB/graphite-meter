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
  internal/transport/         The Session abstraction (HTTP and WebSocket; WebTransport shape reserved).
  internal/wire/              Shared wire-protocol types (frames, opcodes, preflight structs).
  internal/goclient/          The native TUI client's measurement engine (shares the wire protocol).
  internal/static/            //go:embed wrapper that serves the built Svelte client.
api/                          Cross-language contract, source of truth for client/server agreement:
                                 preflight schema/golden — logical discovery
                                 probe schema/golden     — selected-path evidence
                                 wire.md / wire.testvectors.txt                — message protocol + reserved WT shapes
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
builds the random block, upload store, meters, and endpoint implementations once. Each listener
gets its own mux, so a surface never leaks across protocols: the H3 TCP bootstrap carries only
`/probe`, and transfers and latency cannot silently fall back to H1. QUIC 0-RTT is
disabled to prevent POST replay.

| Listener    | Protocol               | Owned surface                                                                                       |
| ----------- | ---------------------- | --------------------------------------------------------------------------------------------------- |
| `:7246/tcp` | HTTP/1.1 clear         | UI, discovery, probe, transfers, upload progress, and clear WebSocket latency.                      |
| `:7247/tcp` | HTTP/1.1 TLS only      | HTTPS UI, discovery, probe, transfers, upload progress, and WSS latency. ALPN offers only HTTP/1.1. |
| `:7248/tcp` | HTTP/2 only            | H2 probe, transfers, and upload progress only. No UI, discovery, H1 ALPN, or WebSocket route.       |
| `:7249/udp` | HTTP/3                 | H3 probe, transfers, and upload progress.                                                           |
| `:7249/tcp` | HTTP/1.1 TLS bootstrap | Alt-Svc bootstrap probe only; no UI, discovery, transfers, progress, or WebSockets.                 |

Discovery separates `capabilities.throughput` from `capabilities.latency`. Each entry is only a
base URL plus, for throughput, `http1`, `http2`, `http3`, or `negotiated`. Stable API routes and
scheme-derived facts are not duplicated in discovery. A run freezes one endpoint for each role
and verifies each endpoint independently. Their probes are separate connections and can therefore select different IPv4/IPv6 paths;
the UI reports both instead of presenting the latency probe as a throughput fallback. The browser
always fetches `/preflight` from the page origin; it never reconstructs target
ports locally, and every subsequent HTTP or WebSocket URL comes from that discovery document.
The browser joins discovery, selection, and probe evidence in one connection model. It validates
both roles at startup, revalidates only the changed role for ordinary selection changes, and
keeps successful evidence for at most 30 seconds. A discovery generation change, connection
failure, reset, explicit retry, or relevant network/visibility transition invalidates that
evidence. Start reuses a fresh unchanged preparation; run inputs are snapshotted at that boundary,
so settings remain editable while a run consumes the frozen values.
Native latency is advertised on deterministic H1 clear/TLS origins. A reverse proxy may advertise
WebSocket latency independently of throughput, and the browser verifies the actual `HI`/`READY`
exchange without claiming a handshake HTTP version. WebSockets over H2 or H3 Extended CONNECT are specified by
[RFC 8441](https://www.rfc-editor.org/rfc/rfc8441) and
[RFC 9220](https://www.rfc-editor.org/rfc/rfc9220), but the current implementation uses the widely
interoperable HTTP/1.1 Upgrade.

The dedicated native H1-TLS listener is a real deterministic transfer target, not a control
fallback. Reverse-proxy origins are instead advertised once as negotiated because browsers cannot
be forced to use H1 on an origin that also offers H2 or H3.
HTTP/1.1 remains an active Internet Standard, and TLS security is independent of selecting H2;
ALPN lets a browser negotiate the protocol the origin offers. Keeping the native SPA on H1 makes
the control-plane entrypoint distinct from the H2 measurement target. A reverse proxy may still
serve the H1 Go origin to browsers over H2 or H3, which is why the client records the actual page
hop. Native H3 deliberately has no UI: browsers normally discover HTTP/3 from an existing HTTPS
origin through Alt-Svc or HTTPS DNS records rather than treating a QUIC listener like a directly
navigable symmetric replacement. See [RFC 9112](https://www.rfc-editor.org/rfc/rfc9112.html),
[RFC 7301](https://www.rfc-editor.org/rfc/rfc7301.html), and
[RFC 9114 section 3.1](https://www.rfc-editor.org/rfc/rfc9114.html#section-3.1).

### Routes

| Path                     | Method       | Transport                            | Purpose                                                                                                                                                                                         |
| ------------------------ | ------------ | ------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `/preflight`             | GET          | UI origins, JSON                     | Logical server identity, process generation, and independent throughput and latency target catalogs. Refreshed by the bounded preparation lifecycle.                                            |
| `/probe`                 | GET          | selected H1/H2/H3                    | Client IP/source and server-observed protocol for the actual selected path. H3 TCP also returns `Alt-Svc` and closes.                                                                           |
| `/download`              | GET          | selected fetch target, streamed body | Streams `?bytes=N` bytes (default 25 MiB, clamped to 64 GiB) sliced from the one shared random block — never regenerated per request.                                                           |
| `/upload/session`        | POST         | selected throughput target, JSON     | Mints a short-lived `gmu_...` token correlating one upload stage's POST lanes and progress stream.                                                                                              |
| `/upload`                | POST         | selected fetch target, streamed body | Drains and counts an uploaded body via a pooled 256 KiB buffer; with a valid `?id=`, folds every drained chunk into a shared per-id aggregate (see below).                                      |
| `/ws/ping`               | WS upgrade   | WebSocket                            | Stateless `PING,<id>` → `PONG,<id>;TIME,<nanos>` echo. The server keeps zero per-ping state; RTT is computed entirely client-side.                                                              |
| `/upload/progress`       | GET / DELETE | selected throughput target, NDJSON   | GET flushes `ready`, then server-timed `progress`, `complete`, or terminal `error` objects; blank lines are heartbeats. DELETE explicitly finalizes the stage after POST lanes stop.            |
| `/` (anything unmatched) | GET          | H1/H1-TLS UI listeners               | The embedded Svelte SPA, with SPA-aware fallback (a missing extensionless path serves `index.html`; a missing path that looks like a hashed asset 404s cleanly instead of serving HTML for it). |

No WebTransport channel is advertised and no route is mounted. Its dependency, schema variants,
wire opcodes, and commented HTTP/3 configuration are retained as inactive contract surface; they
do not describe a runtime capability.

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
bus speaks a tiny ASCII protocol — one message per frame, no
length prefix, `OP` or `OP,arg[,arg...]`, parsed by `indexOf(',')` slicing, never JSON. An unknown
opcode or malformed frame gets a non-fatal `ERR,<code>,<text>` reply; the bus is never torn down
for one bad frame. Full spec: [`api/wire.md`](../api/wire.md); shared byte-exact conformance corpus:
`api/wire.testvectors.txt` (every language's encoder/decoder must match it).

| Opcode  | Direction | Shape                    | Meaning                                                                     |
| ------- | --------- | ------------------------ | --------------------------------------------------------------------------- |
| `HI`    | C→S       | `HI,<proto>`             | Optional hello (`proto` ∈ `ws`/`wt`); lets the bus be primed during warmup. |
| `READY` | S→C       | `READY`                  | Bus is up.                                                                  |
| `PING`  | C→S       | `PING,<id>`              | Latency probe; `id` is a client-owned monotonic uint32.                     |
| `PONG`  | S→C       | `PONG,<id>;TIME,<nanos>` | Echo; `id` verbatim, server clock is diagnostics-only.                      |
| `SIZE`  | C→S       | `SIZE,<bytes>`           | Reserved WebTransport download-size request; no runtime consumer.           |
| `BYE`   | C→S       | `BYE`                    | Graceful bus close.                                                         |
| `ERR`   | S→C       | `ERR,<code>,<text>`      | Non-fatal protocol error.                                                   |

### Admission guardrails (`internal/server`)

One shared controller bounds established TCP/QUIC connections before TLS or QUIC setup and another
bounds active download, upload, progress, and latency handlers across every listener. HTTP clients
use the trusted-proxy-aware address resolver; direct sockets use the peer address. IPv4 is keyed by
address and IPv6 by `/64`. Request slots carry a hard lifetime and are released on every completion
or cancellation path. Per-client exhaustion returns `429`, global exhaustion returns `503`, and
verbose mode reports active, peak, and rejected counts without logging every hostile request.

Upload aggregates retain their separate 1,000-entry global cap, add a 32-entry per-client cap, and
allow only one progress stream per id. Progress heartbeats do not extend aggregate TTL.

### Meter (`internal/endpoint/meter.go`)

Not a scoring or unit-conversion system — a purely optional, `GM_VERBOSE`/`--verbose`-gated,
nil-safe per-second logger of server-observed throughput and live connection count
(`[gm:server:download] 9.41 Gbit/s · 4 conns · 1.18 GB this window`). It exists so an operator can
sanity-check the server's own drained/served rate against kernel counters or the client's
reported numbers — it never influences what the client reports.

### Session abstraction (`internal/transport`)

Every endpoint is written once against a `Session` interface (`Context`, `Query`, `Proto`, `HTTP`,
`OpenDownloadSink`/`OpenUploadSource`, `Bus`), so it doesn't need to know whether it's
running over HTTP or a WebSocket bus. Two concrete sessions exist — `httpSession` (H1/H2/H3
request/response) and `websocketSession` (message bus). The interface retains WebTransport-shaped
stream and bus seams, but there is no WebTransport session implementation.

`internal/config`, `internal/transport`, `internal/server`, `internal/static`, and
`internal/endpoint/registry.go` have unit tests alongside the rest of `internal/endpoint`
and `internal/wire` — run with `just server-test`.

---

## The native Go terminal client

`go/cmd/graphite-meter-client` is an interactive Bubble Tea client that shares `/preflight` and
the wire protocol with the browser. Measurement events are drained in bounded batches, and the
terminal redraws only for state changes; there is no application animation clock.

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

The TUI has five configuration sections (Server — three built-in presets plus a custom URL; Run
setup; Timing; Connections — stream count and TLS verification; Start) and a live telemetry view
(session panel, stage timeline, ASCII throughput bars, a running results log). Keys: `tab`/arrows
to move between sections and rows, `enter`/`space` to toggle or edit, `r` to run or run again, `v`
to recheck the connection, `esc` to cancel a run (confirmed by a second `esc`) or to leave a
finished one, `?` to expand the key list, `q` to quit; the footer lists every binding the screen on
show accepts. CLI flags (all editable again inside the TUI before a run starts) are listed in
[CONFIGURATION.md](CONFIGURATION.md#native-tui-client-flags).

`internal/goclient` (stats, config normalization, preflight, the adaptive-warmup/lane-stagger
runner, and the per-stage transfer lanes) and the TUI's pure helpers and `model` state machine in
`cmd/graphite-meter-client/model.go` have unit tests — also run with `just server-test` (one Go
module covers both the server and the TUI client).

---

## The Svelte browser client

The UI (`client/src/`) is deliberately engine-agnostic: it only ever talks to `RunnerCore`
(`src/lib/runner/core.ts`), which owns the phase timeline, a deadline scheduler, a measured clock that
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
  release build. Negotiates a transport per stage: advertised targets resolve to `fetch`
  streaming for transfer and WebSocket for latency. It spawns one Web Worker per parallel stream
  and keeps an idle keepalive ping
  running between runs for the connectivity indicator.
- **`DummyBackend`** (`src/lib/runner/dummy.ts`) — a synthetic engine for UI development and
  demos, with five canned network profiles (fiber/cable/lte/satellite/throttled) and support for
  injecting live anomalies. Present only in dev/dummy-enabled builds; tree-shaken entirely out of
  a real-only production bundle (see [DEVELOPMENT.md](DEVELOPMENT.md#build-time-feature-flags)).

### Measurement stages

- **Latency** owns its WebSocket, pacing, RTT, and loss state in `ping-worker.ts`.
- **Download** runs one streaming fetch worker per lane and reuses its read buffer.
- **Upload** uses finite requests from a bounded shared payload pool; `/upload/progress` remains
  the sole authoritative byte and rate source.
- **Bidirectional** runs the download and upload pools together. Automatic stream policy accounts
  for multiplexing and the HTTP/1.1 connection budget; forced policy uses the requested lane count.

### Settings

The responsive settings panel groups test, result, and advanced controls. It independently selects
verified throughput and latency targets; unavailable targets remain visible with their discovery
reason. Ping cadence applies to the selected measurement channel, while probe and between-run
keepalive traffic retain their internal cadence.

Wire estimates stop at the browser's first hop. Browser Resource Timing, server probe evidence,
and trusted-proxy address evidence stay separate because they describe different network
boundaries. Dev-tooling builds add diagnostics and dummy-backend anomaly controls.

### The Endpoint info drawer

The right-side drawer reads the live connection model. Its default view shows server identity,
the independently selected throughput and latency paths, readiness, relevant browser-facing and
server-observed protocol evidence, client address evidence, and pre-test RTT. Raw target IDs,
origins, routes, discovery generation, stream policy, engine capabilities, and compensation
assumptions are available in an expandable, copyable diagnostic report rather than repeated in
the primary summary.

### Web Workers

| Worker                      | Role                                                                                                                                                 |
| --------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------- |
| `download-worker.ts`        | One per download lane; streams and discards bytes, reports periodic byte/time deltas.                                                                |
| `upload-worker.ts`          | One per upload lane; builds and POSTs the incompressible payload, reports only liveness.                                                             |
| `upload-progress-worker.ts` | The authoritative upload byte/rate source, parsing NDJSON from the selected throughput target.                                                       |
| `ping-worker.ts`            | Owns the `/ws/ping` connection and the entire RTT/loss algorithm, off the main thread.                                                               |
| `autosize.ts`               | Shared helper (not a worker): EWMA-smoothed, step-clamped transfer sizing used by the upload worker and the chunked-download path.                   |

### Testing

Pure logic uses Bun unit tests; browser behavior uses Playwright in Chromium and Firefox. Commands
and contributor workflows live in [DEVELOPMENT.md](DEVELOPMENT.md).

---

## Reserved contract surface

- **Reserved WebTransport contract** is inactive in this release. It is neither mounted nor
  advertised; H3 throughput and upload progress use fetch over QUIC, while latency independently
  uses a separately advertised WebSocket endpoint. Activating it for both lanes is on the
  [roadmap](#roadmap).

## Roadmap

- **WebTransport for latency and throughput (one of the next releases)** — activate the reserved
  contract so both lanes run over QUIC. Bidirectional streams give throughput a symmetric path
  instead of today's fetch-stream down and POST up, and datagrams make packet loss directly
  measurable, which a TCP-backed WebSocket cannot report because retransmits hide it.
- **Multi-server testing** — select one configured server or run against several servers in one
  pass. Protocol targets in one discovery document currently remain listeners of one logical
  server, not independent servers.
- **Rust rewrite (speculative far future)** — replace the Go server and native terminal client
  while preserving the shared schemas, wire vectors, runtime behavior, and container interface.
  Promising experiments used s2n-quic with a custom H3 adapter.

## TLS security and lifecycle

H1-TLS, H2, and H3 are enabled by non-empty listener addresses and fail before binding when the PEM pair is missing, mismatched,
outside its validity window, or incompatible with a configured public TLS hostname. Private-key
permissions and certificates expiring within 30 days produce warnings. Files are checked every
minute and a complete valid renewal is swapped atomically; a partial or invalid renewal leaves the
last valid certificate serving. No private-key bytes are logged. Deployment settings for the pair
are in [CONFIGURATION.md](CONFIGURATION.md#native-listeners).
