# Graphite Meter — Message-Bus Wire Protocol (normative)

This spec governs the **message-based channels**: the WebSocket latency bus (`/ws/ping`) and the WebTransport
datagram bus (`/wt/ping`), plus the **WebTransport session routes**, defined by their CONNECT URL (see [WebTransport
routes](#webtransport-routes)). Transfer streams carry raw payload bytes; upload sessions also open a
receiver-progress stream. The plain request/response HTTP endpoints (`/servers`, `/preflight`, `/probe`,
`/download`, `/upload/session`, `/upload`, `/upload/progress`, `/upload/checkpoint`) are **not** covered here —
they use normal HTTP (query params, status codes, streaming bodies).

The Go server/native client and TypeScript browser client share the conformance corpus `api/wire.testvectors.txt`.

Related contracts: [discovery and control responses](discovery.md),
[upload sessions and progress](upload.md), and [measurement definitions](../docs/MEASUREMENTS.md).
For listener setup, use the [deployment guide](../docs/DEPLOYMENT.md#native-listeners).

## Framing and lifecycle

One WebSocket text message or WebTransport datagram carries one ASCII message,
with no length prefix or trailing newline. Only two directional forms exist:

| Direction       | Message                   | Meaning                                                                  |
| --------------- | ------------------------- | ------------------------------------------------------------------------ |
| Client → server | `PING,<id>`               | Client-owned uint32 probe ID.                                            |
| Server → client | `PONG,<id>,<handling-ns>` | Echoed ID and mandatory uint64 server handling duration, in nanoseconds. |

Decimal fields contain only digits: no sign, whitespace, exponent, or fraction.
IDs have at most 10 digits and fit uint32; durations have at most 20 digits and
fit uint64. Zero is valid. Missing, extra, or malformed fields invalidate the
entire message. Receivers ignore malformed messages without replying or closing
the transport, and parsers never invent a zero duration. The server reads a WebSocket
binary message like a text one, and ends the bus with close code `1009` on a message
longer than 2048 bytes; a valid PING is at most 15.

A matching valid probe reply establishes application readiness. Unknown IDs,
duplicate replies, and messages from a replaced connection cannot establish it.
Native preflight sends an actual probe; datagram verification retries within its
bounded deadline. Browser warmup probes retain their existing measurement
exclusion. Transport open/close owns the connection lifetime; there are no hello,
ready, goodbye, capability, or error frames.

Each client owns a per-bus `uint32` counter, `id = (id + 1) >>> 0`; the server keeps no per-probe state and echoes
the parsed id. Only the in-flight window (at most 16 probes) is live, so a wrapped id never matches a pending one.
Cadence, deadlines, the in-flight window and probe timeouts are client behaviour, defined in
[latency probing](../docs/MEASUREMENTS.md#latency-probing).

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

| Route          | Query                               | Streams                                                                                                                                                                                                                    | Datagrams                                                                                                                                  |
| -------------- | ----------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| `/wt/ping`     | `token=`                            | none                                                                                                                                                                                                                       | the message bus above, one frame per datagram                                                                                              |
| `/wt/download` | `bytes=&streams=&datagrams=&token=` | the server opens `streams` (1..16, default 1) unidirectional streams; each writes `bytes`, closes, and is replaced while the session lives. `bytes=0` establishes without serving: the transport check                     | with `datagrams=`, the server floods `bytes` at a time, repeating while the session lives; `datagrams=0` is served the stream form instead |
| `/wt/upload`   | `id=&datagrams=&token=`             | client unidirectional streams are raw upload bytes, up to 16 concurrently; a lane opened past that is refused, not served. The server opens **one** unidirectional stream on establishment carrying the progress feed      | with `datagrams=`, received datagrams count as upload bytes; with `datagrams=0` only the streams are drained                               |

`streams=` and `bytes=` are **clamped, never rejected**: a missing, non-numeric, or sub-1 `streams`
is read as 1 and one above 16 as 16; a missing, non-numeric or negative `bytes` is read as 25 MiB
(26214400) and one above 64 GiB as 64 GiB, the same rule as `GET /download?bytes=`. `/wt/upload`
enforces the stream ceiling from the receiving side — the server stops reading the 17th concurrent
client stream with `STOP_SENDING` code 0, without an error frame, since a byte stream carries no
channel to report one on.

`datagrams=` is **presence-based**: the bare `datagrams=` spelled above is the request, and so is
any value the server cannot read. A spelling of zero or false — `0`, `false`, `off`, `no`, matched
case-insensitively with surrounding whitespace ignored — is a request for **no** datagrams and is
served none, the same rule `bytes=0` follows.

Under authentication a CONNECT must present a credential before the upgrade:
a socket ticket carried as `?token=`, or an `Authorization: Bearer` grant for native
clients. See [socket credentials](#socket-credentials).

The upload `id` is minted by `POST /upload/session` and finalized by `DELETE /upload/progress?id=`
over HTTP; only the measured bytes ride the session. The HTTP/3 origin's TCP side also answers these
control routes, `/upload/checkpoint` and `/wt/session`, since a browser may fetch them there before it
uses QUIC; it serves no transfer. The progress feed carries the same NDJSON
records as `GET /upload/progress`; see the [upload contract](upload.md).

### Lane endings

`GM_MAX_SESSION_DURATION` bounds the two transfer session routes; `/wt/ping` lives under the request bound
(`GM_MAX_OPERATION_DURATION`). A session also ends when the peer closes it, which a client does once the finalizing
DELETE has returned. An establish-only `/wt/download?bytes=0` session, whose answer is the handshake, closes after a
5 s linger; a `/wt/upload` refused at connect sends its `error` record on a server stream and closes after a 2 s
linger.

Every lane shares one 30 s idle bound. WebSocket buses and WebTransport sessions close after 30–45 s without traffic
from the peer (a progress heartbeat or datagram flood is the server's own, so a `/wt/download?datagrams=` session the
peer never speaks on closes too); an HTTP upload that stops sending is answered `408` ([upload](upload.md)); an HTTP
download the peer stops draining is closed. The native client derives its 15 s fixed-cadence ceiling from this bound.
A client treats a bound-driven close as a reconnect, not a stage failure, and redials with the same upload `id`: the
server keeps one aggregate per id, so the counters carry across.

A server-ended bus or session names its cause ([laneendings.txt](laneendings.txt)); clients may treat any close as a
reconnect. The browser acts only on `authentication required` (it asks for sign-in); the native client also redials
idle and lifetime endings. A QUIC connection that carried only WebTransport sessions closes with `H3_NO_ERROR` once
its last session ends (a second later when the server ended it, so the session's code arrives first), so it does not
hold one of the client's connection slots.

| Cause                        | WebSocket close                | WebTransport close          |
| ---------------------------- | ------------------------------ | --------------------------- |
| Peer closed or lane finished | `1000`                         | `0`                         |
| Idle                         | `4001 idle`                    | `1 idle`                    |
| Lifetime bound               | `4002 lifetime`                | `2 lifetime`                |
| Sign-out or grant revocation | `1008 authentication required` | `3 authentication required` |
| Server shutdown              | `1001 shutdown`                | `4 shutdown`                |

## Socket credentials

`POST /wt/session?target=<encoded HTTPS origin and /wt/ route>` mints a
WebTransport ticket. `POST /ws/session?target=<encoded HTTPS origin and /ws/ route>`
mints a WebSocket ticket; the target uses HTTPS even though the eventual socket
URL uses WSS. Targets contain no user information, query, or fragment and must use
the issuing server's configured hostname. The ticket binds the precise origin,
port, route, requesting Origin header, principal and grant lifetime.

Both return `{ "token": "…", "expires": <epoch milliseconds> }`. Tickets are single-use
and short-lived ([limits](discovery.md#browser-measurement-authorization)). Any presentation
spends a ticket; one presented at another destination or from another requesting origin is
refused. Logout cancels associated work. With authentication off,
minting returns exactly `{ "token": "", "expires": 0 }`.

The existing same-origin browser session or an authorized browser measurement grant
may mint a ticket. Cross-origin mint requests use the grant in an Authorization
header with cookies omitted. The reusable grant never enters a URL. Browser socket
constructors carry only the one-use ticket in `?token=`. Native bearer grants remain
eligible for direct authenticated socket connections, under their existing origin
rules; they do not use the browser-grant mint path.
