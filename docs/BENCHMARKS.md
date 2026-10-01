# Benchmark harness

Manual harnesses outside the gate. Loopback measures software limits, not a physical network; interpret results with
the [measurement definitions](MEASUREMENTS.md) and keep reports and raw evidence outside the repository.

| Task                               | Measures                                                                       |
| ---------------------------------- | ------------------------------------------------------------------------------ |
| `mise run bench-throughput [cell]` | Production transfer workers in Chromium, per origin, direction and lane count. |
| `mise run bench-servers`           | One, two and four coordinated servers in isolated Linux network namespaces.    |
| `mise run bench-ui`                | UI frame delivery with default stage lengths against the E2E fleet.            |
| `mise run bench-wire`              | Go and TypeScript wire codec cost.                                             |
| `mise run stress`                  | Server saturation envelope (Unix).                                             |

## Throughput matrix

Each cell discards a warmup, measures a fixed window and appends an NDJSON result; cell order follows a seed. Cells run
in Chrome for Testing against the E2E fleet's home server, which brings its own certificate and QUIC pin. The fleet
ends a request after 20 s, so warmup plus window must stay under about 16 s.

| Environment                                  | Default         | Purpose                                      |
| -------------------------------------------- | --------------- | -------------------------------------------- |
| `BUN_CHROME_PATH`                            | auto-discovered | Chrome for Testing or Chromium executable.   |
| `GM_BENCH_ORIGINS`                           | `h1-clear`      | Origins to measure: `h1-clear,h1-tls,h2,h3`. |
| `GM_BENCH_REPS`                              | `3`             | Rounds per cell.                             |
| `GM_BENCH_WARMUP_MS` / `GM_BENCH_MEASURE_MS` | `3000` / `8000` | Discarded warmup and measured window.        |
| `GM_BENCH_SEED`                              | `1`             | Cell-order seed.                             |

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

## Native terminal

```sh
cd go && go test ./cmd/graphite-meter-client -run '^$' -bench '^BenchmarkTUI' -benchmem
```

The frame workload retains four servers and 480 points per trace at 80×24, 120×40 and 160×50. Animation frames reuse
unchanged samples; sample frames update throughput and latency. The chart workload measures braille rasterization
separately. These measure Go rendering time and allocations, excluding terminal-emulator drawing and network traffic.
