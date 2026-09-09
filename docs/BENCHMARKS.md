# Benchmark harness

The maintained Chromium harness drives the production transfer workers through
Bun.WebView. Each cell discards a warmup, measures a fixed window, and appends an
NDJSON result. Cell order uses a deterministic seed; repeated runs should compare
sustained window rates under the same browser, machine and network conditions.

Loopback measures software limits, not the capacity of a physical network. Use the
[current measurement definitions](MEASUREMENTS.md) to interpret throughput,
latency and receiver evidence. Keep run reports and raw evidence outside the
repository; this guide describes the maintained harness.

## Running the harness

The harness starts every native listener, so a local certificate and its SPKI pin are required even
when filtering to a clear HTTP/1.1 cell. See [Development](DEVELOPMENT.md#local-tls-and-http3).

| Environment           | Default                        | Purpose                                           |
| --------------------- | ------------------------------ | ------------------------------------------------- |
| `GM_BENCH_SPKI`       | required                       | Base64 SHA-256 SPKI pin for Chromium QUIC.        |
| `BUN_CHROME_PATH`     | auto-discovered                | Chrome for Testing or Chromium executable.        |
| `GM_BENCH_HOST`       | `127.0.0.1`                    | Server bind and browser destination address.      |
| `GM_BENCH_NETNS`      | empty                          | Run the server through `ip netns exec`.           |
| `GM_BENCH_TLS_CERT`   | `.dev-certs/localhost.pem`     | TLS leaf certificate.                             |
| `GM_BENCH_TLS_KEY`    | `.dev-certs/localhost-key.pem` | TLS private key.                                  |
| `GM_BENCH_ORIGINS`    | `h1-clear`                     | Comma-separated origins to measure.               |
| `GM_BENCH_REPS`       | `3`                            | Repeat rounds per cell. |
| `GM_BENCH_WARMUP_MS`  | `3000`                         | Discarded warmup before each cell.                |
| `GM_BENCH_MEASURE_MS` | `8000`                         | Measured window per cell.                         |
| `GM_BENCH_SEED`       | `1`                            | Deterministic cell-order seed.                    |

Run one maintained cell:

```sh
GM_BENCH_SPKI='<pin>' mise run bench-throughput 'h1-clear/down/lanes=2'
```

Run the full Chromium matrix:

```sh
GM_BENCH_SPKI='<pin>' GM_BENCH_ORIGINS=h1-clear,h1-tls,h2,h3 GM_BENCH_REPS=5 mise run bench-throughput
```

Create a shaped path on a dedicated Linux benchmark host. The benchmark process must have
permission to enter the network namespace: the fixture invokes `ip netns exec` without elevating
itself. Its TLS certificate must cover `10.77.0.2`.

```sh
sudo client/bench/rig.sh up lan-fast-lossy
GM_BENCH_SPKI='<pin>' GM_BENCH_HOST=10.77.0.2 GM_BENCH_NETNS=gmbench mise run bench-throughput
sudo client/bench/rig.sh down
```

The fixture starts and stops the server inside `gmbench`; do not start a second server manually.
Always tear down the rig after the run, including after a failed benchmark.
`mise run bench-wire` measures wire-codec cost, and `mise run stress` measures the server saturation
envelope.

## Coordinated servers

The [development guide](DEVELOPMENT.md#throughput-benchmark) describes the
one-, two-, and four-server namespace harness. Its profiles isolate server caps,
different RTTs and a shared bottleneck. Record the selected identities and each
server's contribution when comparing runs; a combined rate alone cannot explain
which path limited the result.

Return to the [development guide](DEVELOPMENT.md).
