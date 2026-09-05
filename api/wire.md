# Graphite Meter — Message-Bus Wire Protocol (normative)

This spec governs the **message-based channels**: the WebSocket latency bus (`/ws/ping`) and the
WebTransport datagram bus (`/wt/ping`), plus the **WebTransport session routes**, whose streams
carry no messages at all and are defined by their CONNECT URL (see [WebTransport
routes](#webtransport-routes)). The plain request/response HTTP endpoints (`/preflight`, `/probe`,
`/download`, `/upload/session`, `/upload`, `/upload/progress`) are **not** covered here — they use
normal HTTP (query params, status codes, streaming bodies).

The Go and TypeScript implementations MUST agree with the shared conformance corpus
`api/wire.testvectors.txt`; every implementation must preserve the same contract. The opcode keywords
are additionally pinned as a shared constant table in each implemented language
(`go/internal/wire/opcodes.go`, `client/src/lib/runner/real/wire.ts`).

## Framing

- **One logical message per WS frame / per WT datagram.** WS frames and QUIC datagrams are already
  message-delimited, so there is **no length prefix** and **no trailing newline**. WebTransport byte
  streams carry no messages at all: every parameter rides the session's CONNECT URL.
- **ASCII text.** Format: `OP` or `OP,arg[,arg...]`. The opcode is a fixed uppercase keyword.
- **Parsing** is `indexOf(',')` slicing — never JSON, never regex. The id/number args are plain integers.
- Unknown opcode or malformed core args → the receiver replies `ERR,<code>,<text>` (non-fatal) and ignores
  the message; it never tears down the bus for a single bad frame.

## Opcodes

| Dir | Frame | Channel | Meaning |
|---|---|---|---|
| C→S | `HI,<proto>` | ws, wt | Optional hello on bus open. `<proto>` ∈ {`ws`,`wt`}. Lets the bus be primed during warmup without polluting stats. Server MAY reply `READY`. |
| S→C | `READY` | ws, wt | Bus is up; the client may begin the ping chain. |
| C→S | `PING,<id>` | ws, wt-dgram | Latency probe. `<id>` = client-owned monotonic **uint32** counter (see Ids). |
| S→C | `PONG,<id>;TIME,<nanos>` | ws, wt-dgram | Echo. `<id>` copied **verbatim**. `<nanos>` = server monotonic clock (uint64 ns) at receive — **diagnostics/skew only**. RTT is measured purely client-side as `recv − send` using the client's own clock. |
| C→S | `BYE` | ws, wt | Graceful bus close (optional; a transport close is equally valid). |
| S→C | `ERR,<code>,<text>` | ws, wt | Non-fatal protocol error. `<code>` is a short token; `<text>` is human detail. |

## Optional reflector handling time

A client requests application timing on each new bus with `HI,ws;TIMING,1` or
`HI,wt;TIMING,1`. A supporting server acknowledges with `READY,TIMING,1` and adds
`;HANDLING,<nanos>` to each subsequent PONG on that bus. `<nanos>` is an unsigned
64-bit duration in nanoseconds, including zero. An ordinary HI, an unsupported
version, or a new bus retains the legacy READY/PONG shapes. Existing servers
accept the extended HI as an opaque protocol label and answer ordinary READY;
clients continue measuring raw RTT when timing is unacknowledged or absent.
The capability is connection-local and a client must discard it on reconnect.
Repeated acknowledgements are idempotent. Datagrams may reorder, so a reply
arriving before its capability acknowledgement has no timing diagnostic.

The interval begins immediately after the server message adapter's `Recv` returns
and ends immediately before encoding the PONG. It includes parsing and application
handling. It excludes receive queues and adapter work before that boundary,
reply encoding, transport submission, and delivery after it. The existing TIME
field is the server monotonic timestamp at this receive boundary; its absolute
value does not participate in RTT subtraction.

A valid PONG ID and TIME remain a raw reply even if an optional suffix is unknown,
malformed, duplicated, or out of uint64 range: receivers omit the diagnostic.
Malformed core ID/TIME still invalidates the reply. Clients use HANDLING only after
timing was negotiated on that same bus, on the same on-time reply as its raw RTT.
An imprecise or impossible duration (including handling greater than raw RTT due
to clock quantization) is omitted, never clamped. Missing diagnostics do not change
raw RTT, probe outcomes, timeout deadlines, or in-flight ownership.

This is an application protocol extension inspired by reflector residence-time
measurement. It is not TWAMP interoperable and does not measure isolated network RTT.

## WebTransport routes

Sessions are opened with extended CONNECT on the HTTP/3 origin. A stream carries no metadata of
its own, so the CONNECT URL query carries every parameter and the server opens the streams whose
content it defines. Streams are raw bytes end to end.

| Route | Query | Streams | Datagrams |
|---|---|---|---|
| `/wt/ping` | `token=` | none | the message bus above, one frame per datagram |
| `/wt/download` | `bytes=&streams=&datagrams=&token=` | the server opens `streams` (1..16, default 1) unidirectional streams; each writes `bytes`, closes, and is replaced while the session lives. `bytes=0` establishes without serving: the transport check | with `datagrams=`, the server floods `bytes` at a time, repeating while the session lives; `datagrams=0` is served the stream form instead |
| `/wt/upload` | `id=&datagrams=&token=` | client unidirectional streams are raw upload bytes, up to 16 concurrently; a lane opened past that is reset rather than served. The server opens **one** unidirectional stream on establishment carrying the progress feed | with `datagrams=`, received datagrams count as upload bytes; with `datagrams=0` only the streams are drained |

`streams=` is **clamped, never rejected**: a missing, non-numeric, or sub-1 value is read as 1, and
a value above 16 is read as 16. `/wt/upload` enforces the same ceiling from the receiving side —
the 17th concurrent client stream is reset without an error frame, since a byte stream carries no
channel to report one on.

`datagrams=` is **presence-based**: the bare `datagrams=` spelled above is the request, and so is
any value the server cannot read. A spelling of zero or false — `0`, `false`, `off`, `no`, matched
case-insensitively with surrounding whitespace ignored — is a request for **no** datagrams and is
served none, the same rule `bytes=0` follows.

Under authentication a CONNECT must present a credential before the upgrade: a single-use,
short-lived, session-linked token minted by `POST /wt/session` and carried as `?token=` (a browser
CONNECT can send neither cookies nor headers), or an `Authorization: Bearer` grant for native
clients. `/wt/session` answers with an empty token when authentication is off, so clients mint
unconditionally.

The upload `id` is minted by `POST /upload/session` and finalized by `DELETE /upload/progress?id=`
over HTTP; only the measured bytes ride the session. The progress feed carries the same NDJSON
records as `GET /upload/progress`.

The `GM_MAX_SESSION_DURATION` bound covers the two **transfer** session routes. `/wt/ping` is not
one: it lives under the ordinary request bound (`GM_MAX_OPERATION_DURATION`), while sharing the
inactivity bound below — which is what the native client's ping-cadence ceiling is derived from.

A session ends on its lifetime bound, when the peer closes it — which a client does once the
finalizing DELETE has returned — or on either of two server-side inactivity bounds: a session that
carries nothing the **peer** sent for about **30 seconds** is closed, ping buses included (a
server-generated progress heartbeat is not traffic, and neither is a server-generated datagram
flood, so a `/wt/download?datagrams=` session the peer never speaks on closes on the same bound),
and an establish-only `/wt/download?bytes=0` session — which serves nothing, so its answer is the
handshake — is closed after a **30-second linger**. A client MUST treat a bound-driven close as a
reconnect rather than a stage failure, and re-dial against the same upload `id`: the server keeps
one aggregate per id, so the counters carry across.

**Discovery compatibility.** `transport` is a required field on both target lists. A client that
predates it reads every target as its own default (`fetch-stream` / `websocket`), which on an
HTTP/3 origin turns one advertised origin into several identical ones and can make automatic
selection ambiguous. Clients built before this field must be updated alongside the server.

## Ids (PING/PONG)

- The **client** owns a per-bus monotonic `uint32` counter: `id = (id + 1) >>> 0` (wraps at 2³²).
- The **server keeps no per-probe state** — it echoes the parsed id in `PONG`. Its only
  per-bus protocol state is the optional timing capability; there is no server-side id map.
- Wraparound cannot collide: the live key space is only the in-flight window (a few hundred at most,
  each id removed from the client's pending map within a bounded RTT), so a wrapped value can never
  match a still-pending id.

## RTT, probe timeouts, and reply-driven pacing (client behavior)

- On `PING` send, the client records `pending[id] = now()`. On `PONG,<id>` it computes
  `rtt = now() − pending[id]` and resolves that probe once. Sending policy is separate:
  browser fixed cadence is start-to-start and a reply never advances the next scheduled send.
  A full in-flight window waits for a slot, then sends once without a catch-up burst.
- Browser reply-driven pacing sends immediately after its chain-head reply and uses a bounded
  RTT-based backup timer when that reply does not arrive. Its reply-dependent sampling density
  differs from a fixed cadence; neither mode changes which attempted probes enter accounting.
- A WebSocket ping that exceeds its adaptive timeout represents a stalled reliable channel or queue,
  not physical packet loss because TCP retransmits. A timeout on the WT-datagram channel also
  includes possible endpoint queueing or drops; neither identifies physical or directional IP loss.
  See [measurement definitions](../docs/MEASUREMENTS.md) for populations and statistics.
- A bounded **in-flight window** (16 idle at a fixed cadence, 4 reply-driven, 2 under load) limits
  pending work. Pauses, a saturated window, and scheduling gaps create unsent opportunities,
  not timeout or unresolved attempts. Requested cadence alone cannot establish a coverage percentage.

## Why text, not binary

The reference demos already sustain thousands of pings/sec at sub-ms latency; the cost is dominated by
the network RTT and the WS/datagram syscall, not the ~8-byte ASCII parse. Text is debuggable in a
packet capture and trivially cross-language. A binary fast-path can hide behind the same `Frame` type
in each language's `wire` module if profiling demands it, without changing this spec's semantics.
