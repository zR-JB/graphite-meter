# Benchmark harness

Manual harnesses outside the gate. Loopback measures software limits, not a physical network; interpret results with
the [measurement definitions](MEASUREMENTS.md) and keep reports and raw evidence outside the repository.

| Task | Measures |
| --- | --- |
| `mise run bench-throughput [cell]` | Production transfer workers in Chromium, per origin, direction and lane count. |
| `mise run bench-servers` | One, two and four coordinated servers in isolated Linux network namespaces. |
| `mise run bench-matrix` | Go against Rust servers and clients on shaped paths in the same namespaces. |
| `mise run bench-ui` | UI frame delivery with default stage lengths against the E2E fleet. |
| `mise run bench-wire` | Go and TypeScript wire codec cost. |
| `mise run stress` | Server saturation envelope (Unix). |

## Throughput matrix

Each cell discards a warmup, measures a fixed window and appends an NDJSON result; cell order follows a seed. Cells run
in Chrome for Testing against the E2E fleet's home server, which brings its own certificate and QUIC pin. The fleet
ends a request after 20 s, so warmup plus window must stay under about 16 s.

| Environment | Default | Purpose |
| --- | --- | --- |
| `BUN_CHROME_PATH` | auto-discovered | Chrome for Testing or Chromium executable. |
| `GM_BENCH_ORIGINS` | `h1-clear` | Origins to measure: `h1-clear,h1-tls,h2,h3`. |
| `GM_BENCH_REPS` | `3` | Rounds per cell. |
| `GM_BENCH_WARMUP_MS` / `GM_BENCH_MEASURE_MS` | `3000` / `8000` | Discarded warmup and measured window. |
| `GM_BENCH_SEED` | `1` | Cell-order seed. |

The optional cell argument selects cells whose id contains any of its comma-separated literal terms.

```sh
mise run bench-throughput 'h1-clear/down/lanes=2'                                # one cell
GM_BENCH_ORIGINS=h1-clear,h1-tls,h2,h3 GM_BENCH_REPS=5 mise run bench-throughput  # full matrix
```

The full matrix takes hours. For shaped paths, use the coordinated-server harness below.

## Coordinated servers

`mise run bench-servers` builds the production server, creates a client, a router and four servers in disposable
user, network and PID namespaces with its own certificate, and writes raw JSONL results and logs outside the checkout.
It never touches host interfaces or queue disciplines, and segmentation and receive offloads are off on every link so
netem sees single packets. It needs `iproute2` (`ip`, `tc`), `util-linux` (`unshare`, `nsenter`), ethtool, OpenSSL,
curl and the pinned Chrome for Testing:

```sh
BUN_CHROME_PATH=/path/to/chrome GM_MULTI_BENCH_OUTPUT=/tmp/graphite-meter-servers mise run bench-servers
```

Profiles `server-cap`, `differing-rtt` and `shared-cap` run twice each; narrow them with `GM_MULTI_BENCH_PROFILES`
and `GM_MULTI_BENCH_REPEATS`. Record each server's contribution when comparing runs: a combined rate alone cannot
show which path limited it.

## Go and Rust matrix

`mise run bench-matrix` builds the Go server and client and the release Rust server and client, then runs the rig's
`matrix` profile. Each run starts a fresh server behind the router and 1 or 16 concurrent clients, each on its own
address behind one bridge, for one transport (`h1`, `h2`, `h3`, `wt`) and one direction. Native clients run in report
mode; Chromium runs the shipped UI with product defaults. Every client discards a 4 s warmup, measures 8 s and pings
over WebSocket at the fast cadence. Netem on the router's two egress links adds half the RTT and the loss rate in
each direction at 1 Gbit/s, with one bandwidth-delay product of queue (at least 1,000 packets). Every namespace sets
cubic and 128 MiB TCP buffer limits whatever the host tuned. Stop tuners that override congestion control per
connection, such as bpftune, first: the summary leaves out HTTP/1.1 and HTTP/2 runs whose TCP used another.

Each run appends one row to `matrix.ndjson`: build identity, load average, the server's peak RSS (VmHWM), its CPU time
and the bytes delivered each way while the clients run (setup and warmup included), the TCP congestion control and
buffer limits in use, and every client's own report, whose loaded-latency percentiles are P50 and P95. Logs stay under
`matrix/`. The session ends with `matrix-summary.txt`, the Go and Rust medians and ranges per cell; a metric reads
WORSE when Rust is worse in a one-sided exact Mann-Whitney test (p ≤ 0.05) and by more than 2 %. CPU per delivered
Gbit is compared only when both delivered the same load within 5 %.

| Environment | Default | Purpose |
| --- | --- | --- |
| `GM_MULTI_BENCH_MATRIX` | full matrix | Narrows axes: `server`, `client`, `transport`, `direction`, `rtt` (ms), `loss` (%), `count`, `rate` (Mbit/s). |
| `GM_MULTI_BENCH_REPEATS` | `3` | Rounds; each runs every cell once in its own seeded order. |
| `GM_MULTI_BENCH_SEED` | `1` | Cell order and netem loss pattern. |

```sh
BUN_CHROME_PATH=/path/to/chrome GM_MULTI_BENCH_OUTPUT=/tmp/graphite-meter-matrix mise run bench-matrix
GM_MULTI_BENCH_MATRIX='rtt=0,100 loss=0,1 count=1 transport=h2,h3 client=go,rust' mise run bench-matrix  # a subset
cat /tmp/a/matrix.ndjson /tmp/b/matrix.ndjson | python3 client/bench/server-matrix-summary.py  # sessions together
```

The full matrix (1,440 cells, 3 repeats) takes about 16 hours.
