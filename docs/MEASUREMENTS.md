# Measurement definitions

Graphite Meter measures an application path: client scheduling, server work, protocol queues and the network all
contribute. Results do not isolate ICMP latency, directional IP loss or a physical link's capacity. Every rule holds
in both clients unless [client differences](#client-differences) says otherwise.

## Reading a result

| Result | Unit and population | Missing evidence |
| --- | --- | --- |
| Download / upload | Payload bytes per second over the headline interval, in the chosen rate unit. | No sufficient interval: no rate. |
| Peak | Highest mean over consecutive windows of the headline interval, each at least 500 ms on every clock, and over the headline window itself, so never below the headline. | No headline: no peak. |
| Latency | Median (P50) RTT of in-window replies, per server and stage; P95 secondary. | No reply, or a failed stage with fewer than three replies and timeouts: "—". |
| Added latency | Loaded median − idle median, per loaded stage and server, in ms; negative values are kept. | Either median missing: no value. |
| Jitter | Mean absolute change between consecutive replies, in ms. | Fewer than two comparable replies: "—". |
| Probe timeouts | `timeouts / (replies + timeouts)`, as a percentage. | No resolved probe: "—", not zero. |
| Paired server timing | Mean raw RTT and server handling over the same valid pairs. | No valid pair: absent. |

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
  headlines do not add up to the Combined value.
- **Zero vs missing:** advancing receiver time with unchanged bytes is measured zero. A missing, stale or
  zero-duration component skips that boundary; the next valid one spans the gap on each receiver's clock. A replaced
  or regressing receiver closes its interval, which no longer counts, and starts an `evidence-resumed` one. A final
  boundary where any server's direction moved no bytes is skipped, so the result ends at the last good one.
- **Dropouts:** a server leaves the stage where its measured bytes stop growing for the silence limit, a lane fails
  for good, or its grant is refused (which asks for sign-in); only the refused grant removes it at the final
  boundary. The interval ends and the survivors start a `dropout` interval. A server that cannot prepare a stage
  leaves the same way, and the run fails only when none survives. A removed server stays out for the rest of the
  run, except that a sole server retries at the next stage. A latency-only failure keeps throughput, except in the
  latency stage: a server lost there (connection lost or timed out) leaves the run while another remains.
- **Headline:** the mean of the latest interval with at least 800 ms of client time and, for upload, 800 ms in
  every receiver clock, whose window moved bytes. After a late dropout the interval before it can hold the headline
  and the stage is Partial. With no such interval the stage fails with a [reason](#failure-reasons) and the run is
  Incomplete. Earlier intervals and removed servers' measurements stay in the per-server results.
- **Gaps:** a client timer read more than 1.5 s late starts an `evidence-resumed` interval, so no headline spans the
  pause; a slow checkpoint alone never does.
- **Byte ledgers** count unique measured bytes once, independent of window selection.

### Stage timing

Stages last 1 s to 5 min. Warmup runs before every stage, including latency: the configured 0–4 s, stretched to
ten idle RTTs but at most 4 s. Stage readiness is bounded per client. Hidden browser pages keep measuring: workers
time bytes and probes while page timers may be throttled, and the schedule never skips past the current segment.

## Latency probing

An RTT is the client's monotonic send-to-receive interval for a matched probe. Warmup probes are excluded. Idle,
download, upload and bidirectional stages keep separate populations; a failed population shows its median only
after three replies and timeouts.

| Behaviour | Rule |
| --- | --- |
| Cadence | Idle default reply-driven; loaded default Medium. Fast 80 ms, Medium 250 ms, Slow 600 ms are start-to-start. |
| Reply-driven | The next probe goes out on the reply; a backup timer covers a missing reply. |
| In-flight window | 16 idle at a fixed cadence, 4 reply-driven, 2 under load. A full window skips the send, never a timeout. |
| Deadline | Fixed at send: `SRTT + 4 × max(RTTVAR, 1 ms)` (RFC 6298), clamped to 250 ms–10 s; 250 ms before the first reply. |
| Stage end | Sending stops; in-window probes drain to their deadlines, at most 10 s. |
| Interruptions | Disconnects leave pending probes unfinished; local send failures are separate; neither is a timeout. |

Cadence is a scheduling policy, not an observed sampling rate: reply-driven density depends on RTT, and no coverage
is inferred from cadence and elapsed time. The idle headline is the full stage median, the base of added latency;
it never falls back to loaded RTTs or preflight hints. A failed stage keeps its measured population, marked
incomplete. The shown server starts at the one with the lowest preparation RTT; switching it never retargets probes
or changes saved statistics.

## Paired server timing

Every reply carries the server's handling time in nanoseconds, from just after its receive call to just before reply
encoding ([wire protocol](../api/wire.md#reflector-handling-time)). The paired population is successful in-window
replies with a valid handling time. A handling time above that reply's raw RTT, or one the client cannot represent
exactly, omits the pair but keeps the raw reply; values are never clamped. Malformed replies are ignored.

Handling time is a diagnostic of the server's own share (about 100 ns by design); raw RTT stays primary for latency,
jitter, deadlines and added latency.

## Client differences

| Rule | Browser | Native |
| --- | --- | --- |
| Skipping a stage | Duration 0 | `--stages` or the setup toggle |
| Early finish | Optional: a stable window can end a stage once it has 800 ms of evidence; that window is then the headline | None: the full window |
| Duration changes | Live: a shortened stage ends at once and keeps its evidence | Fixed at start |
| Stage readiness | 3.5 s: download bytes, a latency reply, a receiver checkpoint | 10 s: lanes open, upload feed advancing, latency can send |
| Upload boundaries | Pushed progress feed, a checkpoint every 250 ms while quiet | A checkpoint batch every 250 ms tick (1.5 s budget, 500 ms at the end) |
| Gap rule | Page-timer lateness; held during preparation and finalization; the interval before the gap still counts | Sampler-tick lateness; the interval before the gap no longer counts |
| Silence limit | 1.5 s of active run time | 2 s, or three missed checkpoints in a row (not at the end) |
| HTTP 429 / 503 | Server at capacity at once | Retried for 2 s, then server at capacity |
| Live rates | Per server and summed; a quiet receiver is bridged by lane completions within 25% of its last rate | Combined boundary rate, eased in the TUI |
| Latency servers | One chosen **Latency server** (default: the first selected) or **Combined** (every server) | Every server; `l` rotates the shown one |
| Reply-driven backup timer | RTT-based, 8 ms–1 s | The probe deadline |
| Reply after the stage end | Resolves the probe, stays out of RTT and jitter | Counts in RTT and jitter if before its deadline |
| Browser only | P10–P90 span, stability, wire-rate estimate, saved history | |

## Run outcomes

| Outcome | Meaning |
| --- | --- |
| Complete | Every planned stage finished with every server. |
| Partial | Every stage has its results, but a server or latency population failed. |
| Incomplete | A planned result is missing after measurement began, including every server failing. |
| Stopped | Cancelled by the user. |
| Failed | Nothing was measured. |

### Failure reasons

Both clients name a failure with one of seven reasons (labels in `vocabulary.ts` and `vocabulary.go`):

| Reason | Label | Typical cause |
| --- | --- | --- |
| `preparation-failed` | Couldn't prepare the connection | A path check or stage preparation failed without a better reason. |
| `connection-lost` | Connection lost | Network error, offline device or server shutdown. |
| `timeout` | Stopped delivering data | A timed-out path, the silence limit, or an idle or lifetime lane ending. |
| `sign-in-required` | Sign-in required | Sign-out or a revoked grant. |
| `server-busy` | Server at capacity | Admission refused with 429 or 503. |
| `protocol-error` | Unexpected server response | An unexpected status or a refused upload owner. |
| `insufficient-evidence` | Too little measured time | No interval with 800 ms of evidence or moved bytes. |

## Saved history

The browser saves history schema 5: `{schemaVersion, id, completedAt, build, engine, result}`, where `result` is
the run's own result and `engine` the latency-focus server's engine version. Schema 4 records are read as a
one-server result without their old grade and stay unchanged in storage. Other or malformed records stay in storage
but are skipped and reported; a database of another version is refused unchanged. Up to 2,000 results are kept;
Complete, Partial and Incomplete runs are saved when saving is on.

A result holds the selected servers and survivors, per-server transport evidence and stage statuses, latency
populations with exact probe counts and accounting completeness, aggregate and component windows (at most 128 recent
intervals, with an omitted count), failures with reasons, unique byte totals, the headline with its peak, stability
and wire estimate model, and the latency focus. Missing measurements stay null. Grants and socket tickets never enter
history or preferences.

Before saving, a result must be coherent: every failure has one of the seven reasons; a failed or partial stage has
a failure in its scope; a complete stage has none, every lane and, for transfers, an interval with 800 ms of evidence;
a skipped stage has no evidence; some stage ran; and the outcome follows the stage statuses (any failed stage:
Incomplete, else any failure: Partial). An incoherent result is logged as an error and saved as Incomplete.
