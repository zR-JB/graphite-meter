# Graphite Meter — Message-Bus Wire Protocol (normative)

This spec governs the **message-based channels only**. The WebSocket latency bus (`/ws/ping`)
implements it. WebTransport datagrams (`/wt/ping`) and the `SIZE` control for `/wt/download` are
reserved contract entries; no WebTransport route is mounted or advertised. The plain
request/response HTTP endpoints (`/preflight`, `/probe`, `/download`, `/upload/session`, `/upload`,
`/upload/progress`) are **not** covered here — they use normal HTTP (query params, status codes,
streaming bodies).

The Go and TypeScript implementations MUST agree with the shared conformance corpus
`api/wire.testvectors.txt`; the Rust rewrite must preserve the same contract. The opcode keywords
are additionally pinned as a shared constant table in each implemented language
(`go/internal/wire/opcodes.go`, `client/src/lib/runner/real/wire.ts`).

## Framing

- **One logical message per WS frame / per WT datagram.** WS frames and QUIC datagrams are already
  message-delimited, so there is **no length prefix** and **no trailing newline**.
- **ASCII text.** Format: `OP` or `OP,arg[,arg...]`. The opcode is a fixed uppercase keyword.
- **Parsing** is `indexOf(',')` slicing — never JSON, never regex. The id/number args are plain integers.
- Unknown opcode or malformed args → the receiver replies `ERR,<code>,<text>` (non-fatal) and ignores
  the message; it never tears down the bus for a single bad frame.

## Opcodes

| Dir | Frame | Channel | Meaning |
|---|---|---|---|
| C→S | `HI,<proto>` | ws, wt | Optional hello on bus open. `<proto>` ∈ {`ws`,`wt`}. Lets the bus be primed during warmup without polluting stats. Server MAY reply `READY`. |
| S→C | `READY` | ws, wt | Bus is up; the client may begin the ping chain. |
| C→S | `PING,<id>` | ws, wt-dgram | Latency probe. `<id>` = client-owned monotonic **uint32** counter (see Ids). |
| S→C | `PONG,<id>;TIME,<nanos>` | ws, wt-dgram | Echo. `<id>` copied **verbatim**. `<nanos>` = server monotonic clock (uint64 ns) at receive — **diagnostics/skew only**. RTT is measured purely client-side as `recv − send` using the client's own clock. |
| C→S | `SIZE,<bytes>` | wt (download) | Request `<bytes>` (uint64) on the next opened uni-stream. The WebTransport analogue of `GET /download?bytes=N`. |
| C→S | `BYE` | ws, wt | Graceful bus close (optional; a transport close is equally valid). |
| S→C | `ERR,<code>,<text>` | ws, wt | Non-fatal protocol error. `<code>` is a short token; `<text>` is human detail. |

## Ids (PING/PONG)

- The **client** owns a per-bus monotonic `uint32` counter: `id = (id + 1) >>> 0` (wraps at 2³²).
- The **server keeps zero state** — it copies the id bytes straight back into `PONG`. No allocation,
  no map, no overflow possible server-side.
- Wraparound cannot collide: the live key space is only the in-flight window (a few hundred at most,
  each id removed from the client's pending map within a bounded RTT), so a wrapped value can never
  match a still-pending id.

## RTT, loss, and the instant chain (client behavior)

- On `PING` send, the client records `pending[id] = now()`. On `PONG,<id>` it computes
  `rtt = now() − pending[id]`, deletes the entry, and immediately sends the next `PING` (the
  on-receive→send-next chain).
- A WebSocket ping that exceeds its adaptive timeout represents a stalled reliable channel or queue,
  not physical packet loss because TCP retransmits. The reserved unreliable WT-datagram channel is
  the contract shape that can expose datagram loss directly.
- A small **in-flight window** (1–4 pings) keeps one delayed or missing response from deadlocking
  the chain; an interval pacer is the floor and the on-receive send is the responsive fast path.

## Why text, not binary

The reference demos already sustain thousands of pings/sec at sub-ms latency; the cost is dominated by
the network RTT and the WS/datagram syscall, not the ~8-byte ASCII parse. Text is debuggable in a
packet capture and trivially cross-language. A binary fast-path can hide behind the same `Frame` type
in each language's `wire` module if profiling demands it, without changing this spec's semantics.
