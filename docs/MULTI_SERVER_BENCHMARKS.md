# Coordinated server experiment — 2026-09-06

These short experiments exercise the production browser coordinator against independent
Go servers. They verify how combined throughput behaves under controlled server and shared
bottlenecks; they do not establish a physical line's maximum or an internet-wide speedup.

## Method

The host was an AMD Ryzen 7 7800X3D (8 cores, 16 threads), Linux 7.2.3, with the repository's
Go 1.27.1, Bun 1.4.2 and Chrome for Testing 152.0.7977.82. The production UI ran in headless
Chrome through the maintained browser harness. No other repository test suite ran concurrently.

Disposable unprivileged user/network namespaces contained one client, one forwarding router,
and four separate server processes, with distinct upload stores and transport origins. All
measurement traffic traversed virtual Ethernet links; browser automation stayed on unshaped
client loopback. Linux netem supplied per-path delay and rate limits. Its deliberately generous
queue was not an AQM or a model of a particular router.

Each cell selected the first one, two or four catalogue entries. It used one HTTPS HTTP/1
download stream per server, WebSocket latency probes every 250 ms, 750 ms warmup, 1.5 seconds
of idle latency, four seconds of measured download, and fixed-duration completion. Two
repetitions used the orders 4/1/2 and 2/4/1. These experiments cover download aggregation;
receiver-authoritative uploads, simultaneous upload/download, and failure isolation are
covered by the deterministic and real-transport acceptance tests.

Profiles:

- **Server caps:** 40, 60, 80 and 100 Mbit/s on the respective server links, about 2 ms RTT.
- **Different RTTs:** 80 Mbit/s per server, with nominal RTTs of 5, 20, 50 and 100 ms.
- **Shared cap:** 200 Mbit/s per server and a common 100 Mbit/s router-to-client egress cap.

All profiles also included 0.2 ms of shared router egress delay. The four-server different-RTT
cells recorded idle medians of approximately 5.4, 20.4, 50.4 and 100.4 ms, retained as separate
server populations. Reported throughput is application payload, so TLS, HTTP and network
framing keep it below the configured link rate.

## Results

Each rate below is the mean of the two recorded headline rates, in decimal Mbit/s.

| Profile | One server | Two servers | Four servers |
| --- | ---: | ---: | ---: |
| Server caps (combined caps 40 / 100 / 280) | 37.9 | 94.8 | 265.2 |
| Different RTTs (combined caps 80 / 160 / 320) | 75.7 | 151.9 | 303.2 |
| Shared cap (100 for every selection) | 94.6 | 95.1 | 95.0 |

The shared cap stayed effectively unchanged when servers were added. Independent server
caps allowed larger combined rates. Neither behavior requires assuming that selected servers
have independent paths all the way to the client.

## Browser resource observations

The harness sampled all descendant Chrome processes every 50 ms. CPU seconds include all
threads in those processes. The measurement starts immediately before Start and ends after
completion, history persistence and collection of frame statistics, about 7.2–8.3 seconds.
It includes warmup and result rendering. RSS is the sum across processes and can count shared
pages more than once; it is not JavaScript heap size or unique physical memory.

The server-cap cells show the resource change as useful delivered traffic increases:

| Selected servers | Mean Chrome CPU seconds | Peak summed RSS range, MiB | RSS increase from prepared state, MiB |
| --- | ---: | ---: | ---: |
| 1 | 1.38 | 1365–1373 | 143–148 |
| 2 | 1.59 | 1371–1391 | 149–155 |
| 4 | 1.84 | 1416–1428 | 186–191 |

Across all 18 cells, Chrome consumed 1.35–1.99 CPU seconds. Both the 95th-percentile and
largest observed animation-frame gaps were at most 16.8 ms in this 60 Hz harness. No cell
lost a participant or failed to persist its result. This supports responsiveness for these
short, single-stream, at-most-304-Mbit/s cases; it is not a high-rate or long-duration memory
stress result, and headless frame timing does not replace checking the visible interface.

## Reproduce

Run `mise run bench-servers` as described in [Development](DEVELOPMENT.md#throughput-benchmark).
The rig writes one raw JSONL row per cell, including actual component windows, separate idle
and loaded latency summaries, frame gaps, process CPU, and RSS measurements. Preserve that
output when comparing revisions. Use a fresh output directory because rows are appended.

The scripts are [the namespace rig](../client/bench/server-collection-rig.py) and
[the production UI driver](../client/bench/server-collection.bench.ts). They do not modify host
interfaces, queue disciplines, or server installations.
