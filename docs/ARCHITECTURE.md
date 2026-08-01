# Architecture

Graphite Meter is a self-hosted internet speed-test tool. A Svelte 5 browser client (and a
companion native Go terminal client) drive a small, high-throughput measurement server. The
server's job is limited to serving and sinking raw **bytes** and echoing timestamps — every rate,
unit, and statistic is derived on the client side. The explicit design goal is to compare
transports honestly: discovery identifies one logical server, then the client independently
freezes a throughput target and a latency target for the run. Fetch throughput may use HTTP/1.1,
HTTP/2, or HTTP/3; WebTransport carries transfers over QUIC streams and the ping bus over
datagrams; the HTTP/1.1 WebSocket bus carries latency over TCP. Upload progress is part of the
selected throughput target and is never coupled to latency.

## Repository layout (monorepo)

```
client/                      Svelte 5 + Tailwind browser client (bun toolchain).
go/                           Go module: the measurement server + a native Bubble Tea TUI client.
  cmd/graphite-meter/         Server entrypoint.
  cmd/graphite-meter-client/  Native terminal (Bubble Tea) client entrypoint.
  internal/config/            Server env-var/flag configuration.
  internal/server/            Listener bootstrap, mux wiring.
  internal/endpoint/          One Go type per HTTP/WS/WT route (preflight, download, upload, ping, ...).
  internal/transport/         The Session abstraction (HTTP, WebSocket, and WebTransport).
  internal/wire/              Shared wire-protocol types (frames, opcodes, preflight structs).
  internal/goclient/          The native TUI client's measurement engine (shares the wire protocol).
  internal/static/            //go:embed wrapper that serves the built Svelte client.
api/                          Cross-language contract, source of truth for client/server agreement:
                                 preflight schema/golden — logical discovery
                                 probe schema/golden     — selected-path evidence
                                 wire.md / wire.testvectors.txt                — the message-bus protocol
container/                    Deployment: image-based docker-compose.yml + quadlet unit (default),
                               the multi-stage Dockerfile, and build-from-source variants.
```

## How it's built, at a glance

- **Server** (`go/cmd/graphite-meter`): one Go binary. It streams/discards raw bytes and serves
  authoritative NDJSON upload progress over the selected H1/H2/H3 throughput listener, while a
  ping bus — the H1 WebSocket route or WebTransport datagrams — handles latency. It embeds the
  built Svelte client via `//go:embed`, so the whole app ships as a single static binary.
- **Browser client** (`client/`): a Svelte 5 app built around an engine-agnostic core
  (`RunnerCore`) that consumes events from a pluggable backend. The real backend talks to the Go
  server over `fetch`/streamed HTTP, WebSocket, and WebTransport sessions, doing all the actual
  measurement work off the main thread in Web Workers. A synthetic "dummy" backend (dev/demo
  builds only) fabricates plausible network behavior for UI work without a live server.
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
| `:7249/udp` | HTTP/3                 | H3 probe, transfers, upload progress, and WebTransport sessions.                                    |
| `:7249/tcp` | HTTP/1.1 TLS bootstrap | Alt-Svc bootstrap probe only; no UI, discovery, transfers, progress, or WebSockets.                 |

Discovery separates `capabilities.throughput` from `capabilities.latency`. Each entry is a base URL
plus the transport that reaches it, and for throughput the protocol: `http1`, `http2`, `http3`, or
`negotiated`. Stable API routes and scheme-derived facts are not duplicated in discovery. A run
freezes one endpoint for each role and verifies each endpoint independently. Their probes are
separate connections and can therefore select different IPv4/IPv6 paths;
the UI reports both instead of presenting the latency probe as a throughput fallback. The browser
always fetches `/preflight` from the page origin; it never reconstructs target ports locally,
and every subsequent HTTP, WebSocket, or WebTransport URL comes from that discovery document.
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

| Path                     | Method       | Transport                            | Purpose                                                                                                                                                                                                                         |
| ------------------------ | ------------ | ------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `/preflight`             | GET          | UI origins, JSON                     | Logical server identity, process generation, and independent throughput and latency target catalogs. Refreshed by the bounded preparation lifecycle.                                                                            |
| `/probe`                 | GET          | selected H1/H2/H3                    | Client IP/source and server-observed protocol for the actual selected path. H3 TCP also returns `Alt-Svc` and closes.                                                                                                           |
| `/download`              | GET          | selected fetch target, streamed body | Streams `?bytes=N` bytes (default 25 MiB, clamped to 64 GiB) sliced from the one shared random block — never regenerated per request.                                                                                           |
| `/upload/session`        | POST         | selected throughput target, JSON     | Mints a short-lived `gmu_...` token correlating one upload stage's POST lanes and progress stream.                                                                                                                              |
| `/upload`                | POST         | selected fetch target, streamed body | Drains and counts an uploaded body via a pooled 256 KiB buffer; with a valid `?id=`, folds every drained chunk into a shared per-id aggregate (see below).                                                                      |
| `/ws/ping`               | WS upgrade   | WebSocket                            | Stateless `PING,<id>` → `PONG,<id>;TIME,<nanos>` echo. The server keeps zero per-ping state; RTT is computed entirely client-side.                                                                                              |
| `/wt/ping`               | CONNECT      | HTTP/3 WebTransport                  | The same echo over session datagrams, where a ping that never returns is real packet loss rather than a stalled queue.                                                                                                          |
| `/wt/download`           | CONNECT      | HTTP/3 WebTransport                  | The CONNECT query names everything (`bytes=&streams=&datagrams=`): the server opens the lanes, each writing `bytes` and replaced when exhausted, or floods datagrams while the session lives. `bytes=0` is the transport check. |
| `/wt/upload`             | CONNECT      | HTTP/3 WebTransport                  | Client unidirectional streams are upload bytes (`?id=` names the upload); the server opens one stream at establishment carrying the `/upload/progress` records, so the counter rides the connection under test.                 |
| `/wt/session`            | POST         | selected throughput target, JSON     | Mints the single-use 30 s token an authenticated browser CONNECT carries in its URL, since a WebTransport CONNECT can send neither cookies nor headers. Empty token when auth is off.                                           |
| `/upload/progress`       | GET / DELETE | selected throughput target, NDJSON   | GET flushes `ready`, then server-timed `progress`, `complete`, or terminal `error` objects; blank lines are heartbeats. DELETE explicitly finalizes the stage after POST lanes stop.                                            |
| `/` (anything unmatched) | GET          | H1/H1-TLS UI listeners               | The embedded Svelte SPA, with SPA-aware fallback (a missing extensionless path serves `index.html`; a missing path that looks like a hashed asset 404s cleanly instead of serving HTML for it).                                 |

WebTransport is mounted and advertised wherever HTTP/3 is configured. Under authentication the
boundary authenticates the extended CONNECT before any upgrade runs: native clients send their
bearer grant, and browsers carry a session-linked single-use token minted at `/wt/session`;
revoking the auth session unwinds live WebTransport sessions. One session holds one
request-admission slot for its whole life, where a fetch target takes one per request.

### Server-authoritative upload accounting

`internal/endpoint/upload_store.go` is the interesting piece of the upload path: a 32-shard,
mostly lock-free per-id aggregate (`bytes` plus a monotonic first-byte time anchor). Upload
throughput is derived client-side as `Δbytes / Δserver-elapsed-time`; clients baseline both values
when measurement begins, excluding warmup while retaining stalls, reconnects and lane turnaround.
This avoids client event-loop timing artifacts without inflating results by removing pauses. IDs
are short-lived HMAC-authenticated tokens minted without server-side session state; forged IDs and
`/upload/session` floods therefore allocate no aggregate state. A background sweeper reaps idle
aggregates after 90s — twice the WebTransport idle bound plus a reconnect grace, so a counter
outlives the session death it has to survive.

### Wire protocol (message buses only)

Plain request/response endpoints (`/preflight`, `/probe`, `/download`, `/upload`, and
`/upload/progress`) are normal HTTP and not covered by a message protocol. The latency ping bus
speaks a tiny ASCII protocol — one message per WebSocket frame or WebTransport datagram, no
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
| `BYE`   | C→S       | `BYE`                    | Graceful bus close.                                                         |
| `ERR`   | S→C       | `ERR,<code>,<text>`      | Non-fatal protocol error.                                                   |

### Admission guardrails (`internal/server`)

One shared controller bounds established TCP/QUIC connections before TLS or QUIC setup and another
bounds active download, upload, progress, and latency handlers across every listener. HTTP clients
use the trusted-proxy-aware address resolver; direct sockets use the peer address. IPv4 is keyed by
address and IPv6 by `/64`; under authentication the key is the subject instead, so a user behind a
changing address keeps one budget. Request slots carry a hard lifetime and are released on every completion
or cancellation path. Per-client exhaustion returns `429`, global exhaustion returns `503`, and
verbose mode reports active, peak, and rejected counts without logging every hostile request.

Upload aggregates retain their separate 1,000-entry global cap, add a 32-entry per-client cap, and
allow one live progress feed per id, taken over by the newest same-owner feed on reconnect.
Progress heartbeats do not extend aggregate TTL.

A WebTransport session holds one slot for a whole test rather than one request, so it draws on a
separate 16-per-client budget: without one, a client's request allowance could be held for the
session lifetime, which is orders of magnitude longer. Under authentication that budget is keyed by
login where a request budget is keyed by subject, so sessions held on one device cannot starve the
same user's others; with authentication off both fall back to the address key. Admitting a session
therefore spends two global counters — a slot in the measurement pool and a slot in the 64-slot
session budget carved out of it — plus its per-client session bucket.
Without that carve-out a handful of clients' sessions would hold the whole pool for hours, and
every request-shaped route would be refused behind them. The carve-out is a ceiling, not a
reservation: it caps what sessions may occupy and holds nothing open for them, so a pool filled by
request-shaped routes — the two ping buses among them, since neither is a session route — refuses
every session while the session budget is untouched. With the defaults (64 global, 16 per client)
four distinct logins or address buckets can occupy the whole session budget, which is acceptable
because WebTransport exhaustion degrades rather than denies: discovery advertises fetch streams and
WebSocket alongside, and a client falls back per role, where an unbounded session budget could deny
every route instead. An operator expecting many distinct clients should lower
`GM_MAX_SESSIONS_PER_CLIENT`. Within a session each direction carries at most 16 lanes — the server
clamps the download lanes it opens and resets an upload lane offered past the ceiling — so one
admitted session cannot fan out without bound. A session's lane count costs no additional admission,
which is what makes a sixteen-lane ceiling safe to publish.

A session is also bounded by its own traffic: a transfer session that carries nothing the **peer**
sent for about 30 seconds is closed. A transfer session always has bytes moving while it is real, so
silence means the peer is gone without having said so, which a terminated worker, a reloaded page
and a dropped network all look like. The slot returns without the client's help. Only what the peer
sent counts, so neither the server-generated progress heartbeat nor a server-generated datagram
flood keeps a session alive — a `/wt/download?datagrams=` session the peer never speaks on closes on
the same bound. `api/wire.md` states the rule clients hold the server to.

### Lifetime bounds and long tests

Routes are bounded by what they carry, not by the mechanism carrying them. The two ping buses
(`/ws/ping` and `/wt/ping`) exchange messages and reconnect, so both live under
`GM_MAX_OPERATION_DURATION` (default 5 m). The WebTransport transfer sessions (`/wt/download`,
`/wt/upload`) accumulate a whole test, so they live under `GM_MAX_SESSION_DURATION` (default 2 h)
and carry a per-client budget of their own (`GM_MAX_SESSIONS_PER_CLIENT`). Neither bounds a test: both clients
resume every channel across a kill — fetch lanes and the progress feed reopen against the same
upload aggregate, WebTransport stages re-dial their session, and the ping bus reconnects — with the
gap priced into the measured rate as a pause. A multi-hour or multi-day run therefore needs no
server configuration; the bounds exist so an abandoned connection cannot hold an admission slot
past its cap. Reconnection is not unconditional: a session that dies as fast as it dials is a
server refusing the stage, not a gap to ride out, and the stage fails rather than re-dialling for
its whole window.

### Saturation envelope (`just stress`)

The loopback harness (`saturation_stress_test.go`, build constraint `stress && unix` — the CPU
column reads `getrusage`) measures observer latency clients — one per bus, WebSocket and
WebTransport datagrams — against growing background load:
transfer loaders alternating download and upload (two forced lanes each) over kernel TCP or
userspace QUIC, and reply-driven ping-chain spammers, alone and combined. One run on an 8-core x86
desktop, server and clients sharing the process, produced this shape: observer RTT stayed
sub-millisecond while CPU had headroom and inflated roughly linearly with concurrency once the cores
saturated — p50 ≈ 5 ms at 8 loaders and ≈ 20 ms at 32, about 2.5× that confined to 2 cores — and
reported loss was zero in every scenario of that run, so the contamination it showed was CPU
scheduling rather than queue drops, visible in the percentiles rather than silently corrupting them.
QUIC loaders moved about a tenth of the TCP loaders' loopback goodput at comparable CPU. The figures
are one machine and are not kept in the repository; `just stress` re-takes them, and the ordering
rather than the magnitudes is what should be expected to carry. `/probe` reports the admission
occupancy (`load: {active, max}`), the endpoint panel surfaces it past half occupancy as a caution,
and admission's `429`/`503` remains the hard refusal.

The harness is loopback-only: it prices CPU and scheduling contention, not a shaped or lossy path,
and it records process CPU rather than scheduler or Go-runtime pressure. A bandwidth-shaped server
envelope is unmeasured.

### Meter (`internal/endpoint/meter.go`)

Not a scoring or unit-conversion system — a purely optional, `GM_VERBOSE`/`--verbose`-gated,
nil-safe per-second logger of server-observed throughput and live connection count
(`[gm:server:download] 9.41 Gbit/s · 4 conns · 1.18 GB this window`). It exists so an operator can
sanity-check the server's own drained/served rate against kernel counters or the client's
reported numbers — it never influences what the client reports.

### Session abstraction (`internal/transport`)

Every endpoint is written once against a `Session` interface (`Context`, `Query`, `Proto`, `HTTP`,
`OpenDownloadSink`/`OpenUploadSource`, `Bus`), so it doesn't need to know whether it's
running over HTTP, a WebSocket bus, or a WebTransport session. Three concrete sessions exist —
`httpSession` (H1/H2/H3 request/response), `websocketSession` (message bus), and
`webtransportSession`, which wraps either one accepted stream or the session's datagram channel.
A WebTransport session hosts many logical requests, so each is dispatched into the same endpoints
HTTP serves.

`internal/config`, `internal/transport`, `internal/server`, `internal/static`, and
`internal/endpoint/registry.go` have unit tests alongside the rest of `internal/endpoint`
and `internal/wire` — run with `just server-test`.

---

## The native Go terminal client

`go/cmd/graphite-meter-client` is an interactive Bubble Tea client that shares `/preflight` and
the wire protocol with the browser. Measurement events are drained in bounded batches, and the
terminal redraws only for state changes; there is no application animation clock.

Stages (independently toggleable): **latency** (idle RTT baseline over `/ws/ping` or `/wt/ping`),
**download**, **upload**, **bidirectional** (download and upload lanes run concurrently — this is
purely client-side orchestration; the server has no special bidirectional mode, it's just the
existing `/download` and `/upload` routes hit at the same time), and **loaded latency** (the same
ping loop run concurrently during a transfer stage, to measure RTT-under-load / bufferbloat
separately from the idle baseline).

Transport: protocol-specific streamed HTTP GET/POST clients for throughput and its NDJSON upload
progress, plus an independently selected latency channel. Latency runs over WebTransport datagrams
where the server advertises them, since an unanswered ping there is real packet loss; throughput
prefers fetch streams, which still win raw rate over TCP. Both fall back to the other mechanism,
and `--throughput-transport` / `--latency-transport` name one explicitly.

To reduce measurement noise, the runner adaptively stretches warmup to roughly 10x the measured
idle RTT (floor = configured warmup, ceiling 4s) so TCP slow start finishes before the measured
window opens, and staggers parallel lane starts by up to 75ms so congestion windows don't ramp in
lockstep.

Stats: effective throughput as receiver-counted bytes over the measured window, plus a running
peak across ~100ms samples (upload uses the server receiver, download the local client receiver);
latency as min/mean/P50/P95 (linear-interpolated percentiles), jitter (mean absolute deviation),
and loss ratio.

The TUI has five configuration sections (Server — the local-dev preset plus a custom URL, taken as
typed: a missing scheme becomes `http://`, and a missing port is the scheme's own; Run
setup; Timing; Connections — one path selector per role, walking the advertised (origin, transport)
pairs so the endpoint and the mechanism are never chosen apart into a combination no server offers,
each named by its mechanism, the HTTP version it carries and whether it is encrypted, plus the HTTP
version where the selected path leaves one open, stream count and TLS verification; Start) and a
live telemetry view
(session panel, stage timeline, ASCII throughput bars, a running results log). A launched client
checks nothing: `--url` is a default, not a destination, so the connection checklist stays idle
until a server is picked with `enter` (or a check is asked for with `v`). Keys: `tab`/arrows
to move between sections and rows, `enter`/`space` to toggle or edit, `r` to run or run again, `v`
to recheck the connection, `esc` to cancel a run (confirmed by a second `esc`) or to leave a
finished one, `?` to expand the key list, `q` to quit; the footer lists every binding the screen on
show accepts. Navigation is keyboard-only by design: the client never turns on mouse reporting, so
the terminal keeps its own text selection and copy. CLI flags (all editable again inside the TUI
before a run starts) are listed in [CONFIGURATION.md](CONFIGURATION.md#native-tui-client-flags).

`internal/goclient` (stats, config normalization, preflight, the adaptive-warmup/lane-stagger
runner, and the per-stage transfer lanes) and the TUI's pure helpers and `model` state machine in
`cmd/graphite-meter-client/model.go` have unit tests — also run with `just server-test` (one Go
module covers both the server and the TUI client).

The two clients share the wire protocol, the route table, and the lane tables for multiplexed and
session transports. They are not the same measurement engine. The browser resolves its automatic
HTTP/1 lane count out of a shared six-connection budget, so a bidirectional stage opens fewer lanes
than a single-direction one — 2 per direction against this client's 6 — where the native client
opens its ceiling in both directions. The browser adds adaptive early-finish, overhead compensation,
a device-memory-tiered upload reservoir and a stall watchdog, none of which exist natively; the
native client alone accepts `--insecure` and a raw ping duration; datagram throughput is
browser-only. The two also compute jitter differently — consecutive-sample variation in the browser,
deviation from the mean natively — and percentiles differ likewise, nearest-rank against
linear-interpolated, so the two clients' latency summaries are not directly comparable.

---

## The Svelte browser client

The UI (`client/src/`) is deliberately engine-agnostic: it only ever talks to `RunnerCore`
(`src/lib/runner/core.ts`), which owns the phase timeline, a deadline scheduler, a measured clock that
retains stalls in effective throughput while bounding recovery with a max timeout, an
adaptive early-finish confirmation, and the one live-throughput presentation path. Exact byte/time
observations independently feed a growing time-weighted display window, fixed 250ms confidence
buckets, and the final reducer. A sustained fast-window change starts a new display regime and
revokes stability; a brief dip does neither. Confirmation never accelerates the measured clock: it
closes a stable stage at the real boundary and shifts only the remaining schedule.

Adaptive evidence targets are resolved from one phase policy rather than assuming a particular test
preset. The desired sample floor is capped by the actual phase budget after reserving confirmation
time and by the fixed confidence horizon. Latency also uses the selected fixed ping interval from the
same cadence table as the real and dummy engines. This keeps short/medium/long tests eligible at every
fixed cadence without weakening reply-driven evidence merely because the measured path itself is slow.
At the warmup-to-measure boundary, the ping worker re-anchors fixed cadence and attempts one measured
PING immediately. Pending PINGs retain their send-time warmup/measurement attribution, so a warmup
reply arriving after that boundary cannot become measured evidence. The fixed-cadence feasibility
policy can therefore count the synchronized boundary send instead of assuming an accidental timer
alignment.

Stall presentation is also runner-owned. The core emits the shared transition to zero consumed by
the gauge, cards, and chart, while synthetic transition values never enter confidence or result
accounting. When a transfer first becomes stable, its result window begins at the exact source
observation that supplied the opening evidence, not at that observation's end boundary; a final or
delayed report can therefore open a non-empty stable window for both one-way and bidirectional
lanes. Latency follows the same single-path rule: every raw outcome stays in `RunAccumulator`,
while the UI receives deadline-closed, phase-aligned 200ms median/P95/max/loss buckets. Each bucket
also retains scalar first/last and consecutive-delta evidence, so rolling connectivity jitter stays
exact without retaining a second raw history or inventing a delta across a phase/stall continuity
break. The ping worker stamps every success and declared loss with its monotonic observation time;
`latencyChannel.ts` translates that cross-realm coordinate once, and `RunnerCore` accepts only the
resulting required `LatencyObservation`. Worker batching and main-thread delivery delay therefore do
not collapse outcomes or make old evidence look new. Closed presentation windows remain revisionable
for the same bounded history the UI retains: an observation delivered behind the runner timer
updates its original event-time bucket, and the store replaces that bucket by phase, start, and
continuity identity instead of appending a duplicate or moving the observation forward. Pure tail
appends keep the chart's phase index incremental; replacements, ordered insertions, and bounded
history shifts increment an explicit history revision that wakes and rebuilds every indexed chart
consumer, keeping both rendered lines and hover lookup on the same bucket objects as the store.

Gauge and live chart share one robust recent latency scale with shrink dwell. Before the first live
bucket, the gauge derives its scale from the displayed pre-test fallback. Terminal gauge and chart
both recompute the same robust domain from successful whole-run bucket history; if a completed run
has only fallback/loss evidence, the gauge instead scales that displayed fallback directly. A value
is therefore never paired with a scale from another time domain. Isolated tails remain explicit
clipping markers. Chart lines are
straight, render the first closed bucket as a point, and break at phase,
loss, stall, and gap boundaries rather than adding another smoothing curve. The one live stability
snapshot gates adaptive completion but remains a control signal while a phase is running. Compact
results show only the measured value, so ordinary ramp-up is not presented as a premature
low-stability verdict. Finalized stability appears once in the completed result card; the UI never
manufactures a second confidence state.

Wire-rate calculation is centralized in the store. It independently models each measured direction
from canonical goodput and keeps the profile that produced its assumptions, so later settings edits
cannot alter a completed result. The optional estimate is default-on but intentionally absent from
the live gauge and compact stage rows: those surfaces show only canonical measured goodput. A concise
secondary line appears once in each completed one-way result card, with the existing short wire-rate
explanation on its tag. It never drives the headline, dial, chart, confidence, or result reducer.
Loopback is explicitly identified as having no physical wire, and opting out hides only that final
secondary line.

Each run enters a visible `connecting` phase while the selected target is probed and verified.
Only after that succeeds does the measurement timeline start. Stage preparation may also be
asynchronous: `RunnerCore` holds that stage at zero until `onStageBegin` resolves, then gives the
primed connection the full configured warmup interval. Discovery, upload-session allocation, and
other setup work therefore cannot silently consume measured or warmup time.

A pluggable `RunnerBackend` supplies the actual samples via a 3-call-per-stage lifecycle
(`onStageBegin` → prime/open connections and signal readiness, `onStageMeasure` → start pushing
real samples on the _same_ primed connection, `onStageEnd`). Two backends exist:

- **`RealBackend`** (`src/lib/runner/RealRunner.ts`) — the production engine, always used in a
  release build. Resolves per-role transports at probe time — from the four peers described
  below — and commits them for the run. It drives lanes in Web Workers and keeps an idle
  keepalive ping running between runs for the connectivity indicator.
- **`DummyBackend`** (`src/lib/runner/dummy.ts`) — a synthetic engine for UI development and
  demos, with five canned network profiles (fiber/cable/lte/satellite/throttled) and support for
  injecting live anomalies. Present only in dev/dummy-enabled builds; tree-shaken entirely out of
  a real-only production bundle (see [DEVELOPMENT.md](DEVELOPMENT.md#build-time-feature-flags)).

### Transports and how a path is checked

Four transports are peers: `fetch-stream`, one streaming request per lane over whichever of
HTTP/1.1, HTTP/2 or HTTP/3 the target advertises; `websocket`, the ping bus on an HTTP/1.1 Upgrade;
and `webtransport` streams and `webtransport-datagram` over QUIC. `real/transports.ts` is the one
table describing each — the role it can serve (`throughput`, `latency`, or both), whether the
browser can drive it, whether its bytes ride a session, and its picker order — and a
`Record<TransportKind, TransportSpec>` makes a missing row a build error. The WebTransport lane ceiling is not a spec field: it is a separate constant in
the same module (`WT_MAX_LANES`, 16), re-exported through `real/streamPolicy.ts`. Adding a fifth
transport is a row plus a `ByteLane` implementation.

A transport is a field on a target, never a suffix on an id. Selection ids are matched whole and
never taken apart, so an IPv6 literal carrying `::` of its own resolves like any other origin.

`webtransport-datagram` is the one transport the picker hides by default, behind the "Datagram
throughput" setting. It is a loss diagnostic rather than a speed test: nothing is retransmitted, so
missing goodput is packet loss that really happened — which is the measurement — and the rate is
not comparable to the stream card, because browsers hand over one datagram per call and on a fast
link that cost bounds the upload direction rather than the link doing so. The setting adds the
card; a card already selected stays visible when the setting goes back off, since a run is still
about to happen over datagrams, and the caution carries the selection rather than the toggle. The
native client refuses the mechanism outright.

"Whether the browser can drive it" is one bit for selection and two for the reader. `WebTransport`
is a `[SecureContext]` interface, so a page served over plain http has no such global — by the
presence check alone indistinguishable from a browser that never shipped it, and the two have
different answers: reopen the page over https, or use another browser. `webTransportGap()` in the
same module reads `isSecureContext` to tell them apart, and the path cards name the one that
applies. Loopback is a secure context, so local development over `http://localhost` is unaffected.

Every transport shares one establish budget, one restart cadence and one early-fail deadline
(`real/budgets.ts`), so a stage that cannot carry bytes is skipped in the same time whichever
mechanism was selected. The early fail is a deadline rather than an attempt count: a lane that
refuses instantly and one that times out reach it together.

A path is checked on: boot; an explicit Retry; a selection change resolving to a different target; a
role the run will open that is not verified; `online`; becoming visible after 30 s hidden; a
preflight generation change; and Engage when the last check is older than 30 s. Nothing else,
notably not a stage toggle or any other setting, and not re-selecting the target `auto` already
picked. Each role is checked independently, has its own Retry, and reports one of four states:
`Ready`, `Checking`, `Failed`, `Stale`. A failure names itself rather than reading as a check still
in flight.

### Measurement stages

- **Latency** owns its ping bus (`/ws/ping` or `/wt/ping`), pacing, RTT, and loss state in
  `ping-worker.ts`.
- **Download** runs one streaming fetch worker per lane (each reusing its read buffer) or one
  session worker reading the server-opened WebTransport lanes.
- **Upload** uses finite requests from a bounded shared payload pool, or client-opened
  WebTransport streams writing the same source block; the `/upload/progress` records remain the
  sole authoritative byte and rate source, fetched or carried on the session under test.
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

| Worker                      | Role                                                                                                                                                                      |
| --------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `download-worker.ts`        | One per download lane; streams and discards bytes, reports periodic byte/time deltas.                                                                                     |
| `upload-worker.ts`          | One per upload lane; builds and POSTs the incompressible payload, reports only liveness.                                                                                  |
| `upload-progress-worker.ts` | The authoritative upload byte/rate source, parsing NDJSON from the selected throughput target.                                                                            |
| `ping-worker.ts`            | Owns the ping bus, `/ws/ping` or `/wt/ping`, and the entire RTT/loss/timestamp algorithm off the main thread; batched outcomes retain their individual observation times. |
| `wt-transfer-worker.ts`     | Owns one WebTransport session per direction: reads the server-opened download lanes and progress feed, opens the upload lanes, and finalizes with DELETE.                 |
| `autosize.ts`               | Shared helper (not a worker): EWMA-smoothed, step-clamped transfer sizing used by the upload worker and the chunked-download path.                                        |
| `payload.ts`                | Shared helper (not a worker): the memoised incompressible source block both upload paths write.                                                                           |

Byte lanes reach workers through `real/byteLane.ts`: a transfer direction drives lanes as
`start`/`measure`/`stop`/`discard` and never learns which transport is underneath; `fetchLane` wraps
a download or upload worker and `sessionLane` owns a WebTransport session. The ping and upload
progress feeds have their own seams, `real/latencyChannel.ts` and `real/uploadProgress.ts`. All
three construct their workers through `real/workerPool.ts`, and nothing above that seam references
`Worker`.

The two paths differ in worker count for a reason that is not a preference. A `WebTransport` object
is neither serializable nor transferable, so it cannot cross a worker boundary: one worker per lane
would mean one session per lane. Transferring the individual streams instead does not help, because
a transferred `ReadableStream` leaves its underlying source in the owning realm and pipes chunks
over a `MessagePort`, adding a hop rather than removing one. Keeping the session whole also makes a
stage's admission cost independent of its lane count — a forced 16 lanes per direction spends two of
a client's sixteen session slots, where a session per lane would spend thirty-two and could not be
admitted at all. The concentration this creates is bounded by the transport rather than by the
thread: payload bytes never leave the worker, and every lane shares one 50 ms delta window, so the
session path posts fewer messages to the main thread than the fetch path does, not more. No
benchmark cell varies worker topology, and none could without also varying the session count.

### Testing

Pure logic uses Bun unit tests; browser behavior uses Playwright in Chromium and Firefox. Commands
and contributor workflows live in [DEVELOPMENT.md](DEVELOPMENT.md).

---

## Roadmap

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
