# Measurement definitions

Graphite Meter measures an application path, so client scheduling, server work, protocol queues and the network all
contribute to a result. Results do not isolate ICMP latency, directional IP loss or a physical link's capacity. Every
rule holds in both clients unless [client differences](#client-differences) says otherwise.

## Reading a result

| Result                      | Definition                                                                                                                                                                                               | When it is missing                                                                                   |
| --------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------- |
| Download / upload           | The payload bytes per second over the headline interval, shown in the rate unit you chose.                                                                                                               | There is no rate when no interval was long enough.                                                   |
| Peak                        | The highest mean over the headline window and over consecutive ≥ 500 ms windows of its interval, on every clock.                                                                                         | There is no peak when there is no headline.                                                          |
| Latency                     | The median (P50) RTT of in-window replies, per server and stage. P95 is secondary.                                                                                                                       | It is missing when there is no reply, or when a failed stage has fewer than three replies and timeouts. |
| Added latency               | The loaded median − idle median, per loaded stage and server, in ms. Negative values are kept.                                                                                                            | There is no value when either median is missing.                                                     |
| Jitter                      | The mean absolute change between consecutive replies, in ms.                                                                                                                                             | It is missing when fewer than two replies are comparable.                                             |
| Timeouts                    | The ratio `timeouts / (replies + timeouts)`, as a percentage.                                                                                                                                            | It is missing, not zero, when no probe was resolved.                                                  |
| Paired server timing        | The mean raw RTT and server handling over the same valid pairs.                                                                                                                                          | It is absent when there is no valid pair.                                                            |
| Rate stability (browser)    | The score is `100 × (1 − CV)` over every 250 ms rate bucket in the headline window, holes included, and it is floored at 0 %.                                                                            | It is 0 % when there are fewer than two buckets.                                                     |
| No data (browser)           | The time in the headline window when no server moved that direction. It is client time for download and the shortest receiver clock for upload, and it counts in the mean.                               | It is shown from 0.5 s, and older saved results have none.                                           |
| Latency stability (browser) | The score is `100 × (1 − jitter / max(median, 1 ms))` on the stage's own population, floored at 0 %.                                                                                                     | It is absent when there is no jitter.                                                                |
| Wire rate (browser)         | The headline times a modeled overhead. The model adds HTTP/2, HTTP/3 or WebTransport framing, TLS 1.3 records, TCP (with timestamps) or UDP + QUIC headers, IPv4 or IPv6 and Ethernet framing, and assumes a 1,500 B MTU. Several servers each count with their own protocol and IP family, weighted by their bytes. | It is shown only from 0.5 % overhead.                                                                |

- Percentiles cover replies within the stage's measured window. P50 is the midpoint median; P95 and the
  browser's P10 to P90 span use nearest rank.
- Jitter (RTT variation) takes successful replies in receive order within one continuous segment. Timeouts are
  skipped, and a stage boundary or reconnect breaks adjacency. Identical replies give zero.
- A probe times out when its reply deadline expires, and a late reply cannot erase the timeout. Unfinished probes
  have no verdict. They and local send failures are counted separately and left out of the ratio. Timeouts are
  application observations, not TCP/IP packet loss. WebSocket is reliable, so retransmission and head-of-line
  blocking delay replies. On WebTransport, queues may delay or drop datagrams.

Results are plain values, and nothing is graded. Both clients show a missing value as a dash, never as zero.

## Throughput

Download counts payload bytes the client consumed. Upload counts bytes and elapsed time at the server's receiver, so
it is receiver-timed. Sender-queued bytes are not delivery. Warmup bytes are excluded, and chart smoothing changes no
result. A browser can hold a lane's arrived bytes for 60 to 100 ms before scripts see them and then release them at
once, so a short early-finish window can read up to about 2 % high or low in a single run; the median is unaffected.

### Coordinated servers

One schedule owns stages, readiness, warmup, boundaries, cancellation and membership for one to four servers; a
single server is the same run with one participant. Each server keeps its own resources and credentials. Their
contributions share the client's connection and are not independent capacity tests.

- Intervals have fixed membership. Downloads sum client-consumed byte deltas over one client monotonic window.
  Uploads take each receiver's latest observation, divide its byte delta by its own elapsed time and add the rates;
  receiver durations are never added. The final boundary always requests fresh receiver checkpoints, and in the
  browser asks every download lane what it had counted by one cutoff, waiting up to 250 ms for the answers. Each
  lane answers from readings of its running total, taken at most once a millisecond and at every report, so however late its answer
  arrives it counts to the same moment as every other lane, and the boundary closes at that cutoff; a lane that
  answered counts nothing more. A final boundary in the same clock reading as the last tick replaces that tick's
  download counts. The browser's lanes count from the moment the window opens, however late they hear of it, and an
  early finish's steady window starts from what every lane had counted by its first boundary, asked the same way.
- Headline and peak come from the combined boundary samples, never from per-server windows; per-server
  headlines do not add up to the all-servers value.
- Zero differs from missing. Advancing receiver time with unchanged bytes is a measured zero. A missing, stale or
  zero-duration component skips that boundary, and the next valid one spans the gap on each receiver's clock. A
  replaced or regressing receiver closes its interval, which no longer counts, and starts an `evidence-resumed` one.
  A final boundary where any server's direction moved no bytes is skipped, so the result ends at the last good one.
- A server drops out of a stage in three cases: its measured bytes stop growing for the silence limit while another
  server's still grow, a lane fails for good, or its grant is refused, which asks for sign-in. Silence that every
  server shares belongs to the link, so nobody leaves for it, the run shows it as Recovering and the stage keeps its
  planned time. The browser times an upload receiver that still reports on that receiver's clock. It times one that
  sends no record at all only after 4 s, since its feed can lag a busy uplink. It reconnects once a progress feed
  that sends nothing, heartbeats included, for 1.5 s. The terminal client times every direction on its own clock from
  the boundary where the server's counted bytes last grew. At the final boundary a refused grant removes a server. So
  does a direction still silent past the limit, or a stream still retrying a failure while that direction's bytes
  have not moved for 500 ms. The stage then records that failure. The interval ends at the boundary where the
  departing server's bytes last moved, so its silence is never measured, and the survivors' `dropout` interval starts
  there. In a bidirectional stage that boundary is where its first direction stopped. If its bytes never moved, the
  interval ends at its first boundary without a window. A server that cannot prepare a stage leaves the same way, and
  the run fails only when none survives. A removed server stays out for the rest of the run, except that a sole
  server retries at the next stage. Its own statuses show the stages it never began as not run. A latency-only
  failure keeps throughput, except in the latency stage. A server whose connection is lost or timed out there leaves
  the run while another remains.
- The headline is the mean of the latest interval whose window moved bytes and that has at least 800 ms of client
  time and, for upload, 800 ms in every receiver clock. Time in it when no bytes moved lowers the mean. After a late
  dropout the interval before it can hold the headline and the stage is Partial. With no such interval the stage
  fails with a [reason](#failure-reasons) and the run is Incomplete. Earlier intervals and removed servers'
  measurements stay in the per-server results.
- A client timer read more than 1.5 s late is a gap and starts an `evidence-resumed` interval, so no headline spans
  the pause; a slow checkpoint alone never does.
- Byte ledgers count unique measured bytes once, independent of window selection.

### Stage timing

Stages last from 1 s up to the smallest stage limit among the selected servers. That limit is 5 min by default, and
`GM_MAX_STAGE_DURATION` sets it from 1 s to 24 h. A longer plan blocks the start and names the server, and a server
that reports no limit fails its check. A quiet link never ends a stage. A stage runs its planned time unless an early
finish, a live change or a failure no server survives ends it sooner. The browser never finishes a stage early once a
server moved no bytes in a direction for 500 ms, the run stalled, a server left or evidence resumed. Warmup runs
before every stage, including latency. It lasts the configured 0 to 4 s, stretched to ten idle RTTs but at most 4 s.
Stage readiness is bounded per client. Hidden browser pages keep measuring. Workers time bytes and probes, the
browser's run clock ticks from a worker so a page hidden for hours keeps its pace, and the schedule never skips past
the current segment.

## Latency probing

An RTT is the client's monotonic send-to-receive interval for a matched probe. Warmup probes are excluded. Idle,
download, upload and bidirectional stages keep separate populations; a failed population shows its median only
after three replies and timeouts.

| Behavior         | Rule                                                                                                             |
| ---------------- | ---------------------------------------------------------------------------------------------------------------- |
| Cadence          | The idle default is reply-driven and the loaded default is Medium. Fast 80 ms, Medium 250 ms and Slow 600 ms are start-to-start. |
| Reply-driven     | The latest probe's reply sends the next at once, a late one never; a backup timer covers a missing reply.        |
| In-flight window | 16 probes idle at a fixed cadence, 4 reply-driven and 2 under load. A full window skips the send, never a timeout. |
| Deadline         | Fixed at send: `SRTT + 4 × max(RTTVAR, 1 ms)` (RFC 6298), clamped to 250 ms to 10 s; 250 ms before the first reply. |
| Stage end        | Sending stops; in-window probes drain to their deadlines, at most 10 s.                                          |
| Interruptions    | Disconnects and freezes over 1.5 s leave pending probes unfinished; send failures are separate; no timeouts.     |

Cadence is a scheduling policy, not an observed sampling rate. Reply-driven density follows RTT alone and reaches
tens of thousands of probes a second on loopback, and no coverage is inferred from cadence and elapsed time. The idle
headline is the full stage median and the base of added latency. It never falls back to loaded RTTs or preflight
hints. A failed stage keeps its measured population, marked incomplete. Every selected server is probed. The run's
latency belongs to the server shown when the run starts, or to a survivor once that server leaves. The shown server
is the first selected unless the browser was switched to another. Switching the shown server never retargets probes
or changes saved statistics.

The terminal client probes from threads of its own, apart from the threads that carry transfers, which run five nice
steps lower on Linux.

## Paired server timing

Every reply carries the server's handling time in nanoseconds, from just after its receive call to just before reply
encoding ([wire protocol](../api/wire.md#reflector-handling-time)). The paired population is successful in-window
replies with a valid handling time. A handling time above that reply's raw RTT, or one the client cannot represent
exactly, omits the pair but keeps the raw reply; values are never clamped. Malformed replies are ignored.

Handling time is a diagnostic of parsing and handling alone. The server's receive queue, its wait for a thread, TLS
and the reply's encoding and write fall outside it, so under load the server's share of an RTT can exceed it. Raw
RTT stays primary for latency, jitter, deadlines and added latency.

## Client differences

| Rule                      | Browser                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  | Terminal                                                                                                                                                      |
| ------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Skipping a stage          | Duration 0                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               | `--stages` or the setup toggle                                                                                                                                |
| Early finish              | Optional, also for the idle latency stage: after 52 % of the stage, a stability score of at least 0.86 over the last 4 s held for 1.1 s with enough samples ends it; that window is then the headline                                                                                                                                                                                                                                                                                                                                                                                                                                                    | None: the full window                                                                                                                                         |
| Duration changes          | Live: a shortened stage ends at once and keeps its evidence                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              | Fixed at start                                                                                                                                                |
| Stage readiness           | 3.5 s from preparation: download bytes, a receiver checkpoint, then a latency reply; loaded latency that is not ready fails only its population                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          | 10 s: lanes open, upload feed advancing, latency can send                                                                                                     |
| Upload boundaries         | Pushed progress feed on the server's discovery origin; over HTTP it rides that origin's WebSocket when it answered over confirmed HTTP/1.1 and offers one, else an HTTP request, with a checkpoint every 250 ms while it is quiet (1.5 s timeout); over WebTransport the session's feed alone; the final checkpoint retries a miss every 100 ms within 1.5 s                                                                                                                                                                                                                                                                                                                | A checkpoint batch every 250 ms tick (1.5 s budget, 500 ms at the end), retrying a miss every 100 ms                                                          |
| Gap rule                  | Page-timer lateness; held during preparation and finalization; the interval before the gap still counts                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  | Sampler-tick lateness; the interval before the gap no longer counts                                                                                           |
| Silence limit             | 1.5 s of active run time while another server moves, else at the stage end; an upload receiver on its own clock, or 4 s without a record                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 | 2 s of client time without counted bytes or three missed checkpoints (not at the end), while another server moves, else at the end; failing lanes quit at 2 s |
| HTTP 429 / 503            | A start takes each server's lease to the most the run holds there; a server it does not fit leaves before anything is measured, at the address or sign-in limit (429) or as server at capacity (503). Past the lease, a refused lane or upload feed is asked again once, after 300 ms or Retry-After within 1.2 s, and a second refusal fails the stage alike; Retry-After in whole seconds                                                                                                                                                                                                                                                                                                                                                                                                                        | Retried for 2 s, then the same reasons                                                                                                                        |
| Exact stream count        | Up to 128 per direction, 16 per WebTransport session; over HTTP/1.1 only what the origin's six connections can run gates readiness; within the server's per-client measurement limit                                                                                                                                                                                                                                                                                                                                                                                                                                                                     | At most 14 per direction                                                                                                                                      |
| Automatic streams         | HTTP/1.1 up to the HTTP/1.1 stream limit (default 4) per direction, trimmed to the origin's six connections, less on the discovery origin one kept for control requests while a stage probes or uploads and one for an upload's HTTP progress feed (WebSockets take none); HTTP/2 1 down / 4 up; HTTP/3 and WebTransport 1                                                                                                                                                                                                                                                                                                                                                        | `--auto-streams`, default 6                                                                                                                                   |
| Path freshness            | A verified path older than 2 min, since its check or the last run that measured over it without failing it, is checked again before a run                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                | Preparation is reused for 30 s                                                                                                                                |
| Between runs              | Each selected server keeps one latency bus and its transfer workers across checks, idle time and runs; deselecting it ends both, turning latency off ends the bus, a bus that goes quiet between runs closes until a check proves its path again, and a page hidden for 30 s lets both go. A start renews the bus's connection, so a run meets the server's lifetime bound only if it outlasts it. Its lease, its uploads' control requests and the HTTP probes before a reopen or redial go to its discovery origin; the lease holds the bus, and deselecting the server or 30 s hidden ends it. Discovery is read again each minute. A path a run found wrong, or a failed path check, waits for Retry, a start, a network change or a new server generation | Opened per run                                                                                                                                                |
| Latency recovery          | A stalled ping channel redials once per stage, once the server answers an HTTP probe, asked every half second, and at once at the server's idle or lifetime bound; a population fails after 7.3 s without replies, or when its stage ends while it is still down                                                                                                                                                                                                                                                                                                                                                                                         | Redials within 2 s, capped at the stage end; fails before the first reply, or when its stage ends while it is still down                                      |
| Broken lanes              | A lane, feed or WebTransport session reopens once per stage, once the server answers an HTTP probe within 3 s, and freely at the server's idle or lifetime bound; its second other ending fails the stage with its cause. After 4 failed WebTransport handshakes in 5 min, lapsed or abandoned dials included, the page dials none until the oldest of them is 5 min old: Automatic says why it used another path, and a chosen WebTransport fails with that reason                                                                                                                                                                                      | Retried for 2 s without moving bytes, then failed                                                                                                             |
| Transfer endings          | Over HTTP/2 and HTTP/3 a download body ends on the server's clock (`ms=`): after 250 ms while the stage prepares, then 250 ms past its planned end, a live change included; a stage that runs its planned time waits for it, an early finish or a shortened stage cuts it. HTTP/1.1, which a proxy would re-chunk, keeps one sized body, cut at the stage end like an upload POST. A stage starting within 90 s of the run uses an upload id minted at its start                                                                                                                                                                                         | Cut at the stage end                                                                                                                                          |
| Warmup RTT                | The latency focus server's path-check RTT                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                | The highest RTT among active servers, updated to latency-stage medians                                                                                        |
| Live rates                | Per server and summed; a quiet receiver is bridged by lane completions within 25% of its last rate; 500 ms without bytes anywhere fades the sum to zero; a stage's warmup shows its ramp, muted and never counted, and runs on into the window                                                                                                                                                                                                                                                                                                                                                                                                           | All-servers boundary rate, eased in the TUI                                                                                                                   |
| Latency cadence           | Reply-driven, Fast, Medium or Slow                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       | Also a custom spacing from 80 ms to 15 s                                                                                                                      |
| Reply-driven backup timer | RTT-based, 8 ms to 1 s                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   | The probe deadline                                                                                                                                            |
| Reply after the stage end | Resolves the probe, stays out of RTT and jitter                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          | Counts in RTT and jitter if before its deadline                                                                                                               |
| Browser only              | P10 to P90 span, stability, wire-rate estimate, saved history                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |                                                                                                                                                               |

## Run outcomes

| Outcome    | Meaning                                                                              |
| ---------- | ------------------------------------------------------------------------------------ |
| Complete   | Every planned stage finished with every server.                                      |
| Partial    | Every stage has its results, but a server or latency population failed.              |
| Incomplete | A planned result is missing after measurement began, including every server failing. |
| Stopped    | Canceled by the user; work a failure cancels carries that failure, never "stopped". |
| Failed     | Nothing was measured.                                                                |

The latency result is the latency-focus server's population. If that server leaves, the focus moves to a surviving
server that measured latency. The latency stage is Incomplete only when no focus population has a median; latency
failures on other servers make the run Partial. A population or stage that ends without a result records
`insufficient-evidence`.

### Failure reasons

Both clients name a failure with one of eight reasons, whose labels [`api/failurereasons.txt`](../api/failurereasons.txt)
pins:

| Reason                  | Label                                     | Typical cause                                                                                                                                                                                                                                                                         |
| ----------------------- | ----------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `path-failed`           | Path unavailable                          | A path check or stage preparation failed: nothing arrived within 3.5 s, a WebTransport session or upload id did not open, TLS or a proxy refused the connection, a lane reached the HTTP/3 port over TCP (`409 wrong-protocol`), or WebTransport is paused after 4 failed handshakes. |
| `connection-lost`       | Connection lost                           | Network error, offline device or server shutdown, or a channel that worked whose one redial did not open.                                                                                                                                                                             |
| `timeout`               | Stopped delivering data                   | A channel that worked went quiet: the silence limit, a lane that moves nothing for 2 s, or an idle or lifetime lane ending.                                                                                                                                                           |
| `sign-in-required`      | Sign-in required                          | Sign-out or a revoked grant.                                                                                                                                                                                                                                                          |
| `client-limit`          | Limit reached for your address or sign-in | Refused with `429 client-limit`: this address or sign-in already holds its share of the server, named at the start when its lease does not fit; past the lease, one retry waits 300 ms, or Retry-After up to 1.2 s.                                                                                                                                   |
| `server-capacity`       | Server at capacity                        | Refused with `503 server-capacity`: the server holds all the measurements it admits, named at the start when its lease does not fit; past the lease, one retry waits as for `client-limit`.                                                                                                                                                               |
| `protocol-error`        | Unexpected server response                | An unexpected status, a refused upload owner, or an upload id the server still does not know after one replacement receiver.                                                                                                                                                          |
| `insufficient-evidence` | Too little measured time                  | No interval with 800 ms of evidence or moved bytes; in the browser, also intervals spanning too little of the planned time.                                                                                                                                                           |

## Saved history

When saving is on, the browser keeps the newest 2,000 Complete, Partial and Incomplete results in its own IndexedDB.
A record holds the build and the result. The result covers the selected servers and survivors, each server's engine
version, path evidence and stage statuses, latency populations with exact probe counts, at most 128 recent intervals,
failures with reasons, byte totals, and the headline with its peak, stability and wire estimate. Missing
measurements stay null, and grants and leases are never saved. Records that earlier releases saved at
schemas 4 and 5 are upgraded to the current schema 6 once when the database opens, and records that cannot be read
stay in storage, are skipped and are reported. A failure earlier releases saved as `preparation-failed` reads as
`path-failed`, and one saved as `server-busy` as `server-capacity`.

Before saving, a result must be coherent: every failure has one of the eight reasons; a failed or partial stage has
a failure in its scope; a complete stage has none, every lane and, for transfers, an interval with 800 ms of evidence;
a skipped stage has no evidence; some stage ran; a complete transfer stage's intervals span at least 75 % of its
planned time (52 % with early finish); and the outcome follows the stage statuses, so any failed stage makes it
Incomplete and otherwise any failure makes it Partial. The run applies the same span rule as each stage ends, so
History agrees with what was shown. An incoherent result is logged as an error and saved as Incomplete.
