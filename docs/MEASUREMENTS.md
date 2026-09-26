# Measurement definitions

Graphite Meter measures an application path: client scheduling, server work, protocol queues and the network all
contribute. Results do not isolate ICMP latency, directional IP loss or a physical link's capacity.

## Reading a result

| Result | Unit and population | Missing evidence |
| --- | --- | --- |
| Download / upload | Payload bytes per second over a receiver window, in the chosen rate unit. | No valid receiver window: no rate. |
| Peak | Highest mean over consecutive windows of the latest interval, each at least 500 ms on every clock. | Shorter evidence: no peak. |
| Latency | Median (p50) RTT of in-window replies, per server and stage; p95 secondary. | No eligible reply: "—". |
| Added latency | Loaded median − idle median, per stage and server, in ms; negative values are kept. | Either median missing: "—". |
| Jitter | Mean absolute change between consecutive replies, in ms. | Fewer than two comparable replies: "—". |
| Probe timeouts | `timeouts / (replies + timeouts)`, as a percentage. | No resolved probe: "—", not zero. |
| Paired server timing | Mean raw RTT, server handling and adjusted RTT over the same valid pairs. | No valid pair: absent. |

Definitions used throughout:

- **Percentiles** cover replies received within the stage's measured window. P50 is the midpoint median; P10, P90
  and P95 use nearest rank.
- **Jitter** (RTT variation) takes successful replies in receive order within one continuous segment. Timeouts are
  skipped; a stage boundary or reconnect breaks adjacency. Identical replies give zero.
- **Probe timeout**: the reply deadline expired. A late reply cannot erase it. Interrupted probes (no verdict) and
  local send failures are counted separately and excluded from the ratio. Timeouts are application observations on
  WebSocket (reliable, so retransmission and head-of-line blocking delay replies) or WebTransport datagrams (which
  queues may delay or drop); neither is TCP/IP packet loss.

## Throughput

Download counts payload bytes consumed by the client. Upload counts bytes and elapsed time at the server's receiver
(receiver-timed); sender-queued bytes are not delivery. The headline is an adaptive stable window where configured,
otherwise the full-window mean; both and the peak stay distinct results, and chart smoothing affects none of them.
Warmup bytes are excluded.

### Coordinated servers

One schedule owns stages, readiness, warmup, boundaries, cancellation and membership for one to four servers; a
single server is the same run with one participant. Each server keeps its own resources and credentials. Per-server contributions share the client's
connection and are not independent capacity tests. The browser uses adaptive completion where configured; the
native client measures the full window.

- **Intervals** have fixed membership. Downloads sum client-consumed byte deltas over a common client monotonic
  window. Uploads use each receiver's latest record (browser: pushed progress feed, backed by
  `POST /upload/checkpoint` while quiet; native: checkpoints; the final boundary always requests fresh ones),
  divide each receiver's byte delta by its own elapsed time and add the rates. Receiver durations are never added;
  each stays in the result. The receiver clock is authoritative: bytes growing without receiver time are stale.
- **Live rates** are shown per server and summed, so an irregular receiver never freezes the others; while it is
  quiet, lane completions bridge its shown rate within 25% of its last receiver rate. Saved results stay
  receiver-timed.
- **Headline and peak** come from the combined boundary samples, never from independently chosen per-server
  windows. Per-server headlines must not be summed to rebuild the aggregate.
- **Zero vs missing:** advancing receiver time with unchanged bytes is measured zero. A missing, stale or
  zero-duration component skips that boundary; the next valid boundary spans the gap on each receiver's clock. A
  replaced or regressing receiver starts a fresh interval.
- **Dropouts:** a single missed checkpoint is tolerated; three consecutive misses, measured evidence that stops
  for 1.5 s (browser) or 2 s (native), or a refused grant (which asks for sign-in) remove that server in the stage
  where it happened. A missed final boundary alone never does. The interval ends, survivors start a new one and
  stability resets. A server that cannot prepare a stage is removed the same way; the run fails only when no server
  survives. A removed server stays out for the rest of the run, except that a sole server retries at the next
  stage. A latency-only failure keeps throughput.
- **Final headline** needs at least 800 ms of client evidence in the latest interval and, for upload, 800 ms in
  every receiver clock, and a window that moved no bytes has none; otherwise the stage fails with a stated reason
  and the run is Incomplete. Stages last 1 s to 5 min and early finish waits for that evidence. Earlier
  intervals and failed servers' measurements remain in per-server results.
- **Live duration changes** set the active stage's end: a shortened stage ends at once and keeps its evidence.
- **Byte ledgers** count unique measured bytes once, independent of window selection.
- **Wire-rate estimates** are computed per component from the chosen window's protocol and IP-family evidence;
  missing evidence makes the estimate unavailable.

Latency stays keyed by server and stage. The browser probes one chosen **Latency server** (default: the first
selected) or **Every server**; the choice is fixed before the run, and unprobed servers have no latency, not zero.
With Every server, the headline comes from the server with the lowest preparation RTT. Switching the displayed
server never retargets probes or changes saved statistics. The native client probes every server.

### Hidden pages

A hidden run continues: workers keep timing bytes and probes while page timers may be throttled. The schedule still
enters every warmup and stage in order; one late tick never skips past the current segment. A timer gap over 1.5 s
starts a new interval and restarts stability confirmation, so no headline or early finish spans the gap; a stage
whose remaining evidence is too short fails.

## Latency probing

An RTT is the client's monotonic send-to-receive interval for a matched probe. Warmup probes are excluded. Idle,
download, upload and bidirectional stages keep separate populations.

| Behaviour | Both clients |
| --- | --- |
| Cadence | Idle default reply-driven; loaded default Medium. Fast 80 ms, Medium 250 ms, Slow 600 ms are start-to-start. |
| Reply-driven | The next probe goes out on the reply; a backup timer covers a missing reply (browser: RTT-based, 8 ms–1 s; native: the probe deadline). |
| In-flight window | 16 idle at a fixed cadence, 4 reply-driven, 2 under load. A full window leaves an unsent opportunity, never a timeout. |
| Deadline | Fixed at send: `SRTT + 4 × RTTVAR` (RFC 6298), clamped to 250 ms–10 s; 250 ms before the first reply. |
| Stage end | Sending stops; in-window probes drain to their deadlines. Replies after the boundary resolve timeouts but do not enter RTT or jitter. |
| Interruptions | Disconnects leave pending probes unresolved; local send failures are separate; neither is a timeout. |

Cadence is a scheduling policy, not an observed sampling rate: reply-driven density depends on RTT, and no coverage
percentage is inferred from cadence and elapsed time.

**Browser.** Live summaries update at most once per second and are final once the stage's terminal outcomes are in.
The drain lasts at most ten seconds; a fixed cadence waits for a free slot without a catch-up burst. The worker
flushes outcomes before acknowledging its stop; if it crashes or misses the bounded wait, the stage keeps
`accountingComplete: false` rather than inventing outcomes. The idle headline is the full stage median, as in the
native client and in added latency; the adaptive stable window only decides early finish. It never falls back to
loaded RTTs or preflight hints.

**Native.** Raw RTT ends when the adapter receives the reply, before decoding. A fixed cadence skips a send when the
window is full. A failed stage keeps its measured population with an incomplete marker; a failure before any probe
produces an error without a summary.

Browser results show and save the signed added latency of each loaded stage as "Added latency", with an A–F
grade of the largest increase as a secondary label (A ≤ 5, B ≤ 30, C ≤ 60, D ≤ 200 ms, else F; a negative
increase grades A). Records saved before per-stage values show only their grade.

## Paired server timing

Every reply carries the server's handling time in nanoseconds, from just after its receive call to just before reply
encoding ([wire protocol](../api/wire.md#reflector-handling-time)). The paired population is successful in-window
replies with a valid handling time; timeouts, interrupted, failed, late and post-cutoff outcomes are excluded. A
handling time above that reply's raw RTT, or one the client cannot represent exactly, omits the pair but keeps the
raw reply; values are never clamped. Malformed fields invalidate the reply.

Adjusted RTT subtracts only the instrumented handling interval and keeps network delay, queues outside it and client
scheduling. Raw RTT stays primary for latency, jitter, deadlines and added latency. Browser stages expose
`reflectorTiming` (`sampleCount`, `meanRawRttMs`, `meanHandlingMs`, `meanAdjustedRttMs`); native stages expose
`ReflectorTiming` durations.

## Native stages

Stage readiness is bounded to ten seconds: every download lane has a response or WebTransport stream, every upload
lane has sent headers or opened its stream with the progress feed advancing, and the latency channel can send. A
reply is not required, so silent paths still yield timeouts. Warmup then lasts the configured value or ten idle
RTTs, whichever is longer, up to 4 s. The measured phase opens on fresh upload checkpoints; the coordinator samples
about every 250 ms, each checkpoint batch bounded to 1.5 s (500 ms at the final boundary), retrying refusals every
100 ms. A final boundary where any server's direction moved no bytes is skipped, so the result ends at the last good
one; a gap over 1.75 s between sampled boundaries starts a new interval. A lane with no bytes for two seconds ends
with its last error; a lost progress feed is reopened within two seconds. Before the next stage, upload waits until
the receiver is quiet (250 ms, at most 4 s). Cleanup joins all resources before the outcome is emitted.

## Run outcomes

| Outcome | Meaning |
| --- | --- |
| Complete | Every stage finished with every server. |
| Partial | Every stage finished after a server or latency population left. |
| Incomplete | A planned result is missing after measurement began, including every server failing. |
| Stopped | Cancelled by the user. |
| Failed | Nothing was measured. |

## Saved history

The browser accepts only history schema 4 with wire estimates version 2. Other or malformed records stay in storage
but are skipped and reported; a database of another version is refused unchanged. Nothing is migrated. Up to 2,000
results are kept; Complete, Partial and Incomplete runs are saved when saving is on.

A record holds the selected servers and survivors, per-server transport evidence, stage latency populations with
exact probe counts and accounting completeness, aggregate and component windows (at most 128 recent intervals,
with an explicit omitted count), structured failures, unique byte totals, the headline (`reportedBytesPerSec`) with
the full-window average and peak, and the latency focus. Missing measurements stay null. Grants and socket tickets
never enter history or preferences.
