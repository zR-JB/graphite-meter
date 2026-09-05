# Graphite Meter — Message-Bus Wire Protocol (normative)

This spec governs the **message-based channels**: the WebSocket latency bus (`/ws/ping`) and the
WebTransport datagram bus (`/wt/ping`), plus the **WebTransport session routes**, whose streams
carry no messages at all and are defined by their CONNECT URL (see [WebTransport
routes](#webtransport-routes)). The plain request/response HTTP endpoints (`/preflight`, `/probe`,
`/download`, `/upload/session`, `/upload`, `/upload/progress`) are **not** covered here — they use
normal HTTP (query params, status codes, streaming bodies).

The Go server/native client and TypeScript browser client share the conformance
corpus `api/wire.testvectors.txt`. Version 0.7 is a protocol break: version 0.6
clients and servers cannot be mixed with 0.7 peers. Deploy matching versions.

## Framing and lifecycle

One WebSocket text message or WebTransport datagram carries one ASCII message,
with no length prefix or trailing newline. Only two directional forms exist:

| Direction | Message | Meaning |
|---|---|---|
| Client → server | `PING,<id>` | Client-owned uint32 probe ID. |
| Server → client | `PONG,<id>,<handling-ns>` | Echoed ID and mandatory uint64 server handling duration, in nanoseconds. |

Decimal fields contain only digits: no sign, whitespace, exponent, or fraction.
IDs have at most 10 digits and fit uint32; durations have at most 20 digits and
fit uint64. Zero is valid. Missing, extra, or malformed fields invalidate the
entire message. Receivers ignore malformed messages without replying or closing
the transport, and parsers never invent a zero duration.

A matching valid probe reply establishes application readiness. Unknown IDs,
duplicate replies, and messages from a replaced connection cannot establish it.
Native preflight sends an actual probe; datagram verification retries within its
bounded deadline. Browser warmup probes retain their existing measurement
exclusion. Transport open/close owns the connection lifetime; there are no hello,
ready, goodbye, capability, or error frames.

## Reflector handling time

The server interval begins immediately after the message adapter's `Recv` returns
and ends immediately before PONG encoding. It includes probe parsing and
application handling, and excludes receive queues, earlier adapter work, reply
encoding, transport submission, and delivery. No absolute server timestamp is sent.

Raw application RTT uses the client's own receive-minus-send clock and remains
primary. After strict decoding, clients pair the handling duration with that
same reply's raw RTT. An imprecise or impossible clock pair (including handling
greater than raw RTT due to clock quantization) retains the raw reply but is
omitted from the paired diagnostic, never clamped. This validation does not alter
probe outcomes, timeout deadlines, or in-flight ownership.

This application measurement is not TWAMP interoperable and does not isolate
network RTT. See [measurement definitions](../docs/MEASUREMENTS.md) for populations.

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
clients. `/wt/session` requires both `token` and `expires`; expiry is a finite epoch-millisecond
value for a nonempty token. With authentication off it returns exactly an empty token and
`expires: 0`. Clients mint unconditionally.

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

**Discovery contract.** `transport` is required on every throughput and latency target.
A missing value is invalid; clients never infer a transport from its absence. Upgrade
clients and servers together for the 0.7 contract.

## Ids (PING/PONG)

- The **client** owns a per-bus monotonic `uint32` counter: `id = (id + 1) >>> 0` (wraps at 2³²).
- The **server keeps no per-probe state** — it echoes the parsed id in `PONG`. There is no
  server-side id map or per-bus capability state.
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

## Text representation

The two small messages are directly inspectable in packet captures and share
strict numeric validation across the Go and browser implementations.
