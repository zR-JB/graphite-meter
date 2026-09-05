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
