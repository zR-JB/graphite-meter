# Measurement definitions

Graphite Meter measures an application path. Browser or native-client scheduling,
server work, protocol queues, and the network all affect that path. Results do not
isolate ICMP latency, directional IP loss, or a physical link's capacity.

## Throughput

Download counts payload bytes consumed by the receiving client. Upload counts
bytes and elapsed receiver time at the server. Sender-queued bytes are not proof
of delivery. Full-window average and an adaptive stable-window headline remain
distinct results; chart smoothing does not determine either.

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

Version 1 saved browser results remain readable. They retain their stored values
and are marked as legacy calculations because profiles used bucket-derived
estimates and some timeout populations were selectively sampled. They cannot be
reconstructed into version 2 raw measurements.

Early Version 2 snapshots without accounting-completeness metadata remain
readable. They are shown as partial accounting with unknown exact timeout counts;
a stored percentage is never used to reconstruct a supposedly exact count.

## Native summaries

Native stages separate transport preparation, warmup, and measurement. Preparation
is bounded to ten seconds. Every download lane must have an accepted HTTP response
or WebTransport receive stream; upload lanes must have written request headers or
opened their streams, with the receiver progress feed subscribed and advancing.
The latency bus must be open and able to send. A successful latency reply is not a
readiness requirement, so silent paths can still produce timeout observations.
All selected directions and loaded-latency participants pass this gate before the
configured/adaptive warmup starts. Warmup data is excluded from every result.

The shared gate opens the measured phase. Download and latency use client monotonic
time; upload takes its baseline from the first fresh receiver report after that
gate. Acquiring that receiver baseline has a separate deadline: the shorter of
the configured measured duration and ten seconds. If no fresh report arrives,
the stage fails without an upload summary; baseline acquisition time never
enters a receiver window. Its exact counter window follows the server progress cadence and server
clock, rather than mixing client timestamps with server byte counts. Download
and upload full-window means are received bytes divided by their attributable
elapsed seconds. A direction failure or caller cancellation stops its siblings;
nonempty measured throughput windows survive with the stage's original failure
and an incomplete label. Setup-only bytes never become a partial result, and
empty interrupted transfer windows do not produce numeric summaries. Cleanup
finishes before the run's terminal outcome is emitted.

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
