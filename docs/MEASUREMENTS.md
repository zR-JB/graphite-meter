# Measurement definitions

Graphite Meter measures an application path: client scheduling, server work, protocol queues and the network all
contribute. Results do not isolate ICMP latency, directional IP loss or a physical link's capacity. Every rule holds
in both clients unless [client differences](#client-differences) says otherwise.

## Reading a result

| Result                      | Unit and population                                                                                                                                                                                      | Missing evidence                                                             |
| --------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------- |
| Download / upload           | Payload bytes per second over the headline interval, in the chosen rate unit.                                                                                                                            | No sufficient interval: no rate.                                             |
| Peak                        | Highest mean over the headline window and consecutive ≥ 500 ms windows of its interval, on every clock.                                                                                                  | No headline: no peak.                                                        |
| Latency                     | Median (P50) RTT of in-window replies, per server and stage; P95 secondary.                                                                                                                              | No reply, or a failed stage with fewer than three replies and timeouts: "—". |
| Added latency               | Loaded median − idle median, per loaded stage and server, in ms; negative values are kept.                                                                                                               | Either median missing: no value.                                             |
| Jitter                      | Mean absolute change between consecutive replies, in ms.                                                                                                                                                 | Fewer than two comparable replies: "—".                                      |
| Probe timeouts              | `timeouts / (replies + timeouts)`, as a percentage.                                                                                                                                                      | No resolved probe: "—", not zero.                                            |
| Paired server timing        | Mean raw RTT and server handling over the same valid pairs.                                                                                                                                              | No valid pair: absent.                                                       |
| Rate stability (browser)    | `100 × (1 − CV)` of every 250 ms rate bucket in the headline window, holes included, floored at 0 %.                                                                                                     | Fewer than two buckets: 0 %.                                                 |
| No data (browser)           | Time in the headline window when no server moved that direction: client time for download, the shortest receiver clock for upload. It counts in the mean.                                                | Shown from 0.5 s; older saved results have none.                             |
| Latency stability (browser) | `100 × (1 − jitter / max(median, 1 ms))`, floored at 0 %, on the stage's own population.                                                                                                                 | No jitter: absent.                                                           |
| Wire rate (browser)         | The headline times a modelled overhead: HTTP/2, HTTP/3 or WebTransport framing, TLS 1.3 records, TCP (with timestamps) or UDP + QUIC headers, IPv4 or IPv6 and Ethernet framing, assuming a 1,500 B MTU. | Shown only from 0.5 % overhead.                                              |

- **Percentiles** cover replies within the stage's measured window. P50 is the midpoint median; P95 and the
  browser's P10–P90 span use nearest rank.
- **Jitter** (RTT variation) takes successful replies in receive order within one continuous segment. Timeouts are
  skipped; a stage boundary or reconnect breaks adjacency. Identical replies give zero.
- **Probe timeout**: the reply deadline expired; a late reply cannot erase it. Unfinished probes (no verdict) and
  local send failures are counted separately and left out of the ratio. Timeouts are application observations on
  WebSocket (reliable, so retransmission and head-of-line blocking delay replies) or WebTransport datagrams (which
  queues may delay or drop); neither is TCP/IP packet loss.

Results are plain values; nothing is graded.

## Throughput

Download counts payload bytes the client consumed. Upload counts bytes and elapsed time at the server's receiver
(receiver-timed); sender-queued bytes are not delivery. Warmup bytes are excluded, and chart smoothing changes no
result.

### Coordinated servers

One schedule owns stages, readiness, warmup, boundaries, cancellation and membership for one to four servers; a
single server is the same run with one participant. Each server keeps its own resources and credentials. Their
contributions share the client's connection and are not independent capacity tests.

- **Intervals** have fixed membership. Downloads sum client-consumed byte deltas over one client monotonic window.
  Uploads take each receiver's latest observation, divide its byte delta by its own elapsed time and add the rates;
  receiver durations are never added. The final boundary always requests fresh receiver checkpoints.
- **Headline and peak** come from the combined boundary samples, never from per-server windows; per-server
  headlines do not add up to the all-servers value.
- **Zero vs missing:** advancing receiver time with unchanged bytes is measured zero. A missing, stale or
  zero-duration component skips that boundary; the next valid one spans the gap on each receiver's clock. A replaced
  or regressing receiver closes its interval, which no longer counts, and starts an `evidence-resumed` one. A final
  boundary where any server's direction moved no bytes is skipped, so the result ends at the last good one.
- **Dropouts:** a server leaves the stage where its measured bytes stop growing for the silence limit while another
  server's still grow, a lane fails for good, or its grant is refused (which asks for sign-in). Silence that every
  server shares is the link's: nobody leaves for it, the run shows it as Recovering and the stage keeps its planned
  time. An upload receiver that still reports is silent once its own clock shows no new bytes for the limit; one that
  sends no record at all only after 4 s, since its feed can lag a busy uplink. At the final boundary a refused grant
  removes a server, and so does a direction still silent past the limit or a stream still retrying a failure while
  that direction's bytes have not moved for 500 ms; the stage then records that failure. The interval ends at the
  boundary where the departing server's bytes last moved (in a bidirectional stage, where its first direction
  stopped; at its first boundary, without a window, if they never moved), so its silence is never measured, and the
  survivors' `dropout` interval starts there. A server that cannot prepare a stage leaves the same way, and the run
  fails only when none survives. A removed server stays out for the rest of the run, except that a sole server
  retries at the next stage. A latency-only failure keeps throughput, except in the latency stage: a server lost
  there (connection lost or timed out) leaves the run while another remains.
- **Headline:** the mean of the latest interval with at least 800 ms of client time and, for upload, 800 ms in
  every receiver clock, whose window moved bytes; time in it when no bytes moved lowers the mean. After a late
  dropout the interval before it can hold the headline and the stage is Partial. With no such interval the stage
  fails with a [reason](#failure-reasons) and the run is Incomplete. Earlier intervals and removed servers'
  measurements stay in the per-server results.
- **Gaps:** a client timer read more than 1.5 s late starts an `evidence-resumed` interval, so no headline spans the
  pause; a slow checkpoint alone never does.
- **Byte ledgers** count unique measured bytes once, independent of window selection.

### Stage timing

Stages last from 1 s up to the smallest stage limit among the selected servers: 5 min unless an operator raises
`GM_MAX_STAGE_DURATION`, at most 24 h; a server that predates the limit counts as 5 min. A longer plan blocks the
start and names the server. A quiet link never ends a stage: it runs its planned time unless an early finish, a
live change or a failure no server survives ends it sooner, and the browser never finishes a stage early once a
server moved no bytes in a direction for 500 ms, the run stalled, a server left or evidence resumed. Warmup runs
before every stage, including latency: the configured 0–4 s, stretched to ten idle RTTs but at most 4 s. Stage
readiness is bounded per client. Hidden browser pages keep measuring: workers time bytes and probes, the browser's
run clock ticks from a worker so a page hidden for hours keeps its pace, and the schedule never skips past the
current segment.

## Latency probing

An RTT is the client's monotonic send-to-receive interval for a matched probe. Warmup probes are excluded. Idle,
download, upload and bidirectional stages keep separate populations; a failed population shows its median only
after three replies and timeouts.

| Behaviour        | Rule                                                                                                             |
| ---------------- | ---------------------------------------------------------------------------------------------------------------- |
| Cadence          | Idle default reply-driven; loaded default Medium. Fast 80 ms, Medium 250 ms, Slow 600 ms are start-to-start.     |
| Reply-driven     | The next probe goes out on the reply; a backup timer covers a missing reply.                                     |
| In-flight window | 16 idle at a fixed cadence, 4 reply-driven, 2 under load. A full window skips the send, never a timeout.         |
| Deadline         | Fixed at send: `SRTT + 4 × max(RTTVAR, 1 ms)` (RFC 6298), clamped to 250 ms–10 s; 250 ms before the first reply. |
| Stage end        | Sending stops; in-window probes drain to their deadlines, at most 10 s.                                          |
| Interruptions    | Disconnects leave pending probes unfinished; local send failures are separate; neither is a timeout.             |

Cadence is a scheduling policy, not an observed sampling rate: reply-driven density depends on RTT, and no coverage
is inferred from cadence and elapsed time. The idle headline is the full stage median, the base of added latency;
it never falls back to loaded RTTs or preflight hints. A failed stage keeps its measured population, marked
incomplete. Every selected server is probed. The run's latency is the first selected server's, or a survivor's once
it leaves; the shown server starts there, and switching it never retargets probes or changes saved statistics.

## Paired server timing

Every reply carries the server's handling time in nanoseconds, from just after its receive call to just before reply
encoding ([wire protocol](../api/wire.md#reflector-handling-time)). The paired population is successful in-window
replies with a valid handling time. A handling time above that reply's raw RTT, or one the client cannot represent
exactly, omits the pair but keeps the raw reply; values are never clamped. Malformed replies are ignored.

Handling time is a diagnostic of the server's own share (about 100 ns by design); raw RTT stays primary for latency,
jitter, deadlines and added latency.

## Client differences

| Rule                      | Browser                                                                                                                                                                                                      | Native                                                                                                                   |
| ------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------ |
| Skipping a stage          | Duration 0                                                                                                                                                                                                   | `--stages` or the setup toggle                                                                                           |
| Early finish              | Optional, also for the idle latency stage: after 52 % of the stage, a stability score of at least 0.86 over the last 4 s held for 1.1 s with enough samples ends it; that window is then the headline        | None: the full window                                                                                                    |
| Duration changes          | Live: a shortened stage ends at once and keeps its evidence                                                                                                                                                  | Fixed at start                                                                                                           |
| Stage readiness           | 3.5 s from preparation: download bytes, a receiver checkpoint, then a latency reply; loaded latency that is not ready fails only its population                                                              | 10 s: lanes open, upload feed advancing, latency can send                                                                |
| Upload boundaries         | Pushed progress feed; over HTTP a checkpoint every 250 ms while the feed is quiet (1.5 s timeout); over WebTransport the session's feed alone; the final checkpoint retries a miss every 100 ms within 1.5 s | A checkpoint batch every 250 ms tick (1.5 s budget, 500 ms at the end), retrying a miss every 100 ms                     |
| Gap rule                  | Page-timer lateness; held during preparation and finalization; the interval before the gap still counts                                                                                                      | Sampler-tick lateness; the interval before the gap no longer counts                                                      |
| Silence limit             | 1.5 s of active run time while another server moves, else at the stage end; an upload receiver sending no record gets 4 s                                                                                    | 2 s, or three missed checkpoints in a row (not at the end)                                                               |
| HTTP 429 / 503            | Lanes and the upload feed retry until silence or the readiness budget lapses, then server at capacity; Retry-After in whole seconds                                                                          | Retried for 2 s, then server at capacity                                                                                 |
| Forced streams            | At most 14 per direction over HTTP/2 and HTTP/3 and 16 per WebTransport session, within a server's 32 measurements per client                                                                                | At most 14 per direction                                                                                                 |
| Automatic streams         | HTTP/1.1 up to 4 per direction, trimmed to the origin's six-connection budget; HTTP/2 1 down / 4 up; HTTP/3 and WebTransport 1                                                                               | `--auto-streams`, default 6                                                                                              |
| Path freshness            | A verified path older than 2 min is checked again before a run                                                                                                                                               | Preparation is reused for 30 s                                                                                           |
| Latency recovery          | The ping channel reconnects with 100 ms–2 s backoff; a population fails after 7.3 s without replies, or when its stage ends while it is still down                                                           | Redials within 2 s, capped at the stage end; fails before the first reply, or when its stage ends while it is still down |
| Warmup RTT                | The latency focus server's path-check RTT                                                                                                                                                                    | The highest RTT among active servers, updated to latency-stage medians                                                   |
| Live rates                | Per server and summed; a quiet receiver is bridged by lane completions within 25% of its last rate; 500 ms without bytes anywhere fades the sum to zero; a new stage shows none before its evidence          | All-servers boundary rate, eased in the TUI                                                                              |
| Latency cadence           | Reply-driven, Fast, Medium or Slow                                                                                                                                                                           | Also a custom spacing from 80 ms to 15 s                                                                                 |
| Reply-driven backup timer | RTT-based, 8 ms–1 s                                                                                                                                                                                          | The probe deadline                                                                                                       |
| Reply after the stage end | Resolves the probe, stays out of RTT and jitter                                                                                                                                                              | Counts in RTT and jitter if before its deadline                                                                          |
| Browser only              | P10–P90 span, stability, wire-rate estimate, saved history                                                                                                                                                   |                                                                                                                          |

## Run outcomes

| Outcome    | Meaning                                                                              |
| ---------- | ------------------------------------------------------------------------------------ |
| Complete   | Every planned stage finished with every server.                                      |
| Partial    | Every stage has its results, but a server or latency population failed.              |
| Incomplete | A planned result is missing after measurement began, including every server failing. |
| Stopped    | Cancelled by the user; work a failure cancels carries that failure, never "stopped". |
| Failed     | Nothing was measured.                                                                |

The latency result is the latency-focus server's population. If that server leaves, the focus moves to a surviving
server that measured latency. The latency stage is Incomplete only when no focus population has a median; latency
failures on other servers make the run Partial. A population or stage that ends without a result records
`insufficient-evidence`.

### Failure reasons

Both clients name a failure with one of seven reasons (labels in `vocabulary.ts` and `vocabulary.go`):

| Reason                  | Label                           | Typical cause                                                                                                                |
| ----------------------- | ------------------------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| `preparation-failed`    | Couldn't prepare the connection | A path check or stage preparation failed without a better reason.                                                            |
| `connection-lost`       | Connection lost                 | Network error, offline device or server shutdown.                                                                            |
| `timeout`               | Stopped delivering data         | A timed-out path, the silence limit, a lane that moves nothing for 2 s, or an idle or lifetime lane ending.                  |
| `sign-in-required`      | Sign-in required                | Sign-out or a revoked grant.                                                                                                 |
| `server-busy`           | Server at capacity              | Admission refused with 429 or 503; retries wait 300 ms doubling, or Retry-After, up to 1.2 s.                                |
| `protocol-error`        | Unexpected server response      | An unexpected status, a refused upload owner, or an upload id the server still does not know after one replacement receiver. |
| `insufficient-evidence` | Too little measured time        | No interval with 800 ms of evidence or moved bytes; in the browser, also intervals spanning too little of the planned time.  |

## Saved history

The browser saves history schema 5: `{schemaVersion, id, completedAt, build, engine, result}`, where `result` is
the run's own result and `engine` the latency-focus server's engine version. Schema 4 records are read with the
servers they saved (one when they saved none), without their old grade, and stay unchanged in storage. Other or
malformed records stay in storage but are skipped and reported; a database of another version is refused unchanged.
Up to 2,000 readable results are kept, and unreadable records never count toward that or get pruned; Complete,
Partial and Incomplete runs are saved when saving is on.

A result holds the selected servers and survivors, per-server transport evidence and stage statuses, latency
populations with exact probe counts and accounting completeness, aggregate and component windows (at most 128 recent
intervals, with an omitted count), failures with reasons, unique byte totals, the headline with its peak, stability
and wire estimate model, and the latency focus. Missing measurements stay null. Grants and socket tickets never enter
history or preferences.

Before saving, a result must be coherent: every failure has one of the seven reasons; a failed or partial stage has
a failure in its scope; a complete stage has none, every lane and, for transfers, an interval with 800 ms of evidence;
a skipped stage has no evidence; some stage ran; a complete transfer stage's intervals span, from first to last,
at least 75 % of its planned time (52 % with early finish), so a hidden-page gap between them still counts; and the
outcome follows the stage statuses (any failed stage: Incomplete, else any failure: Partial). The run applies the
same span rule as each stage ends (52 % only after an early finish), so a stage below it settles Partial with
`insufficient-evidence` and History agrees with what was shown. An incoherent result is logged as an error and saved
as Incomplete.
