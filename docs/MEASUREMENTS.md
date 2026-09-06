# Measurement definitions

Graphite Meter measures an application path. Browser or native-client scheduling,
server work, protocol queues, and the network all affect that path. Results do not
isolate ICMP latency, directional IP loss, or a physical link's capacity.

## Reading a result

| Result                 | Unit and population                                                                                         | When evidence is missing                                                   |
| ---------------------- | ----------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------- |
| Download / upload      | Payload bytes per second over a receiver measurement window, displayed in your chosen rate unit.            | No valid receiver window means no rate.                                    |
| Idle / loaded latency  | Milliseconds for successful in-window replies, separately for each stage.                                   | No eligible replies means no RTT descriptor.                               |
| RTT variation (jitter) | Mean absolute change between consecutive comparable replies, in milliseconds.                               | Fewer than two comparable replies means no estimate.                       |
| Probe timeouts         | Expired probes divided by replies plus timeouts, shown as a percentage.                                     | No resolved probes means unavailable; unresolved attempts remain separate. |
| Paired server timing   | Raw RTT, server handling, and adjusted RTT means over the same valid pairs, in milliseconds in the browser. | No valid pair means no diagnostic.                                         |

Start with throughput and the difference between idle and loaded latency. Use counts and
completeness to judge the available evidence. Full definitions follow:
[throughput](#throughput), [browser summaries](#browser-summaries-version-2),
[server timing](#paired-server-timing-diagnostics), and [native summaries](#native-summaries).

## Throughput

Download counts payload bytes consumed by the receiving client. Upload counts
bytes and elapsed receiver time at the server. Sender-queued bytes are not proof
of delivery. Full-window average and an adaptive stable-window headline remain
distinct results; chart smoothing does not determine either.

### Coordinated server windows

A selection contains one to four explicitly chosen servers. One coordinator owns
the stage schedule, readiness, warmup, measurement boundaries, cancellation, and
membership changes. Server resources and credentials remain separate. Per-server
contributions describe those paths while sharing the client connection; they are
not independent server-capacity tests. The browser uses adaptive completion where
configured; the native client retains its full-window duration policy.

Each aggregation interval has fixed membership. Downloads sum client-consumed
payload-byte deltas over a common client monotonic window. Uploads take fresh
`POST /upload/checkpoint` snapshots concurrently at common boundary requests, retain
each server's byte/time pair, divide each byte delta by that receiver's elapsed
seconds, then add the resulting bytes-per-second rates. This is a sum of coordinated
receiver-window means. It is not an exactly synchronized global-clock sample:
request/response timing and each actual receiver duration remain in the result.
Receiver durations are never added.

Browser stable-window boundaries are chosen centrally. A headline never adds
independently selected stable windows or independent per-server peaks. Peak rates
are taken from the combined boundary samples. Presentation buckets, smoothing and
latency focus cannot change any of these reductions. Mixed transport wire estimates
are computed component by component from the chosen window's protocol and IP-family
evidence; missing evidence makes the estimate unavailable.

Advancing receiver time with unchanged bytes establishes measured zero delivery.
A missing, stale, or zero-duration component does not establish a zero rate. Missing
checkpoint evidence interrupts the aggregate interval; recovering evidence starts
a fresh interval and stability confirmation. Unique measured-byte ledgers are
independent of headline-window selection and do not count overlapping checkpoints,
feed reports or interval boundaries twice. Warmup bytes are excluded.

Before any measurement, all selected servers must be ready. After measurement has
started, a terminal throughput failure removes that server for the remaining run
after bounded transport recovery. Healthy connections keep running. The current
interval ends, fresh survivor baselines start a new interval, and stability resets.
The final headline requires the latest interval to contain at least 800 ms of client
evidence and, for upload, at least 800 ms in every component's receiver clock. If it
does not, the headline is unavailable. Earlier valid intervals and failed-server
measurements remain available in the affected server's individual results. All servers failing
ends with an incomplete result. A latency-only failure does not remove throughput.

Latency remains keyed by both server and stage. Added latency compares a loaded
population only with that server's idle baseline. The named latency focus selects
one population for presentation; there is no averaged multi-server ping or blended
responsiveness grade. The browser can probe one explicitly selected primary server
(the default) or every selected server. Primary selection is fixed before the run;
other servers still generate throughput load, and their unmeasured latency fields
remain null. Switching the displayed server never starts, stops or retargets probes. [Server controls and deployment](SERVERS.md) describe selection,
authorization, shared-origin stream budgets and result details.

## Round-trip latency and probe timeouts

An RTT is the client's monotonic send-to-receive interval for a matched probe.
Warmup probes do not enter measurement results. Idle, download, upload, and
bidirectional stages have separate populations, even when they use the same server.

A probe timeout means its reply deadline expired. The timeout percentage is
`timeouts / (replies + timeouts)`. Interrupted probes without a verdict and local
send failures are separate counts and are excluded from that denominator. With
no resolved probes the percentage is unavailable, not zero. Timeouts have no RTT.
A late reply cannot erase an already recorded timeout.

WebSocket runs over a reliable stream, so retransmission and head-of-line blocking
can delay replies. WebTransport uses unreliable datagrams; network or endpoint
queues can delay or drop them. Neither outcome identifies physical or directional
packet loss. Transport labels describe how the application probe travelled.

## Browser summaries, version 2

The runner reduces raw outcomes into stage summaries. The UI and saved history
consume those summaries; graph buckets are presentation data only. Live summaries
update at most once per second and are finalized after the stage's terminal probe
outcomes have been collected.

At the stage boundary, the browser stops submitting probes and ends the transfer
load. The worker drains probes submitted on or before that boundary to their
original deadlines, with a maximum of ten seconds. Deadlines are fixed when each
probe is submitted using the current adaptive RTT estimate, with a 250 ms floor
and a ten-second ceiling. A reply received at or after its deadline remains a
timeout, even if the periodic timeout sweep has not yet run.

Replies received after the stage boundary resolve probes for the timeout
percentage but do not enter that stage's RTT or jitter population: their RTT may
include time after the transfer load ended. Probes submitted after the boundary
while a stop message was in transit are excluded. The boundary uses comparable
worker/window performance-clock coordinates, so batching cannot change membership.

Disconnects leave pending probes unresolved. Rejected local sends are recorded as
send failures. Neither becomes timeout evidence. Buffered replies are delivered
before a later interruption so RTT variation never crosses that interruption.
The worker flushes resolved outcomes and interruption counts before acknowledging its stop; only then does
the owner terminate it and finalize the stage. Cancellation can stop this drain
immediately. If the worker crashes or fails to acknowledge within the bounded
wait, the owner reports an interruption without inventing outcomes for the
unknown pending population.
Its stage summary retains `accountingComplete: false`, including when no known
outcome was delivered, so the missing population cannot look like a complete
zero-timeout measurement. A failed stage that must discard its active worker
reports incomplete accounting before reducing the stage's retained partial result.

Configured probe cadence is a scheduling policy, not an observed sampling rate.
Fixed cadence does not send early after a reply; a saturated in-flight window
waits for a slot without a catch-up burst. Reply-driven mode has RTT-dependent
sampling density. Pauses, scheduler delays, and saturation leave unsent
opportunities that are neither timeout nor unresolved attempts. Reported counts
cover actual attempted probes; no coverage percentage is inferred from requested
cadence and elapsed time alone.

- Min, max, mean, and percentiles cover replies received within the measured stage.
- P50 is the midpoint median. P10, P90, and P95 use nearest rank.
- RTT variation (jitter) is the mean absolute difference of consecutive successful
  replies in observation order within a continuous measurement segment. Timeouts
  are skipped; stage or connection interruptions break adjacency. Fewer than two
  comparable replies gives no variation estimate; identical replies give zero.
- The unloaded headline may use an adaptive stable window. Its full-stage
  descriptors remain separate and never fall back to loaded RTTs or preflight hints.
- Added latency under load compares each available transfer-stage median with the
  full idle median. The largest increase determines the summary grade, so a good
  download stage cannot hide an impaired upload stage. The increase is clamped at
  zero; thresholds are A ≤5 ms, B ≤30 ms, C ≤60 ms, D ≤200 ms, otherwise F. This is
  an application responsiveness descriptor, not proof of a particular queueing cause.

## Saved history

Graphite Meter 0.8 writes saved history schema version 4 and still reads version 3.
Version 3 records remain original singleton results, with no invented server or
interval metadata. Versions 1 and 2
are unsupported: existing entries remain in browser storage but are skipped and
reported as unsupported or malformed. They are neither migrated nor reinterpreted;
clearing history remains an explicit user action. New results retain the usual
2,000-entry limit.

Version 4 adds selected server identities, actual surviving participants, per-server
transport evidence, stage latency populations, aggregate and component windows,
structured failures, unique byte totals and the presentation-only latency focus.
The live result and history detail consume the same summaries. At most 128 recent
aggregation intervals are retained; an omitted count is explicit and byte ledgers
still cover the full measured run. An incomplete all-server failure is also saved
when result saving is enabled; user-aborted runs are not saved. Grants and socket
tickets never enter history or saved preferences.

Version 3 stores one selected throughput headline, `reportedBytesPerSec`, alongside
the distinct full-window average and peak. Each saved latency lane requires exact
known probe counts and accounting-completeness metadata. Missing measurements stay
null; incomplete accounting remains explicit. Optional paired reflector timing is
absent when no valid diagnostic pairs were measured, independently of format version.

## Paired server timing diagnostics

Version 0.7 requires reflector handling time on every valid wire reply, in nanoseconds.
The measured server interval runs from immediately after its message receive call
to immediately before reply encoding. Browser clients convert an exactly
representable duration to milliseconds and reject diagnostics that exceed that
reply's raw RTT. Malformed wire fields invalidate the reply. A clock-quantized or unrepresentable
pair is omitted from the diagnostic, never corrected
by clamping the adjusted value to zero.

Each stage may expose `reflectorTiming` with `sampleCount`, `meanRawRttMs`,
`meanHandlingMs`, and `meanAdjustedRttMs`. All three means use the **same paired
population**: successful replies received within the measurement window with valid
handling time. Timeout, interrupted, failed-send, late, post-cutoff,
and invalid clock-pair outcomes do not enter this diagnostic population. A measured
zero duration is valid. No valid pairs means the entire diagnostic is absent.
The paired raw mean may differ from the stage's full-population mean or median.

Adjusted RTT subtracts only this instrumented server handling interval. It retains
network delays, receive and send queues outside that interval, browser scheduling,
and other endpoint delays. Raw application RTT remains primary for latency,
variation, confidence, loaded responsiveness, and timeout estimation. History retains the paired summary when valid pairs are available.

## Native summaries

Native stages separate transport preparation, warmup, and measurement. Preparation
is bounded to ten seconds. Every download lane must have an accepted HTTP response
or WebTransport receive stream; upload lanes must have written request headers or
opened their streams, with the receiver progress feed subscribed and advancing.
The latency bus must be open and able to send. A successful latency reply is not a
readiness requirement, so silent paths can still produce timeout observations.
All selected directions and loaded-latency participants pass this gate before the
configured/adaptive warmup starts. Warmup data is excluded from every result.

The shared gate opens the measured phase after fresh initial upload checkpoints.
Download and latency use client monotonic time; uploads use each server's own
receiver checkpoints, with each concurrent checkpoint batch bounded to 1.5 seconds.
The coordinator samples boundaries about every 250 ms. Full-window means follow
the component rules above. A terminal direction failure stops that participant's
resources and leaves survivors running; caller cancellation stops all resources.
Setup-only bytes never become a partial result. Earlier measured bytes and windows
remain in details when the final headline lacks evidence. Cleanup joins resources
and checkpoint requests before the run's terminal outcome is emitted.

Native latency summaries use received application replies within each measured stage. P50 is the
midpoint median; P10/P90/P95 use nearest rank. RTT variation is the mean absolute difference between
consecutive successful replies in receive order, skipping timeout outcomes and starting a new
sequence after a reconnect. One reply cannot establish variation; repeated identical replies can
establish zero variation.

Native probe deadlines are `max(4 * PingInterval, 250ms)`, measured from the client send attempt.
Replies after that deadline count as probe timeouts, even if the periodic timeout sweep has not
run. Timeout ratios use only successful replies plus expired probes. At a stage cutoff or channel
interruption, pending probes whose deadlines have not elapsed are reported as unresolved; local
send failures are separate. This native cutoff does not add a post-stage drain interval. An empty
resolved population has no timeout ratio, and timeout-only loaded stages still produce a result.
Failed stages retain their measured latency population with an incomplete marker and the original
failure; the elapsed window records only the measured portion. Failures before any probe was measured
produce an error without a numeric summary. These are application probe observations over WebSocket
or WebTransport, not TCP/IP packet loss.

Native raw RTT ends immediately after the message adapter receives the reply,
before wire decoding, so metadata parsing cannot inflate it. A stage's
`ReflectorTiming` contains paired count and mean raw RTT, server handling, and
adjusted RTT as nanosecond-resolution durations. Only successful in-window
replies with valid clock pairs enter those means; the native cutoff policy above
remains unchanged. Unrepresentable or greater-than-RTT durations leave the raw
result intact and omit only the paired diagnostic. Missing or malformed wire
fields invalidate the reply as a protocol error. Version 0.6 and 0.7 peers must
not be mixed.

Return to the [project overview](../README.md) or [deployment guide](DEPLOYMENT.md).
