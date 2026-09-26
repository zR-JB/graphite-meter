# Benchmark harness

Manual harnesses outside the gate. Loopback measures software limits, not a physical network; interpret results with
the [measurement definitions](MEASUREMENTS.md) and keep reports and raw evidence outside the repository.

| Task | Measures |
| --- | --- |
| `mise run bench-throughput [cell]` | Production transfer workers in Chromium, per origin, direction and lane count. |
| `mise run bench-servers` | One, two and four coordinated servers in isolated Linux network namespaces. |
| `mise run bench-ui` | UI frame delivery with default stage lengths against the E2E fleet. |
| `mise run bench-wire` | Go and TypeScript wire codec cost. |
| `mise run stress` | Server saturation envelope (Unix). |

## Throughput matrix

Each cell discards a warmup, measures a fixed window and appends an NDJSON result; cell order follows a seed. The
server starts every native listener, so a [local certificate and SPKI pin](DEVELOPMENT.md#local-tls-and-http3) are
required even for a clear HTTP/1.1 cell.

| Environment | Default | Purpose |
| --- | --- | --- |
| `GM_BENCH_SPKI` | required | Base64 SHA-256 SPKI pin for Chromium QUIC. |
| `BUN_CHROME_PATH` | auto-discovered | Chrome for Testing or Chromium executable. |
| `GM_BENCH_HOST` | `127.0.0.1` | Server bind and browser destination. |
| `GM_BENCH_NETNS` | empty | Run the server through `ip netns exec`. |
| `GM_BENCH_TLS_CERT` / `GM_BENCH_TLS_KEY` | `.dev-certs/localhost.pem` / `-key.pem` | TLS leaf and key. |
| `GM_BENCH_ORIGINS` | `h1-clear` | Origins to measure: `h1-clear,h1-tls,h2,h3`. |
| `GM_BENCH_REPS` | `3` | Rounds per cell. |
| `GM_BENCH_WARMUP_MS` / `GM_BENCH_MEASURE_MS` | `3000` / `8000` | Discarded warmup and measured window. |
| `GM_BENCH_SEED` | `1` | Cell-order seed. |

The optional cell argument selects cells whose id contains any of its comma-separated literal terms.

```sh
GM_BENCH_SPKI='<pin>' mise run bench-throughput 'h1-clear/down/lanes=2'                  # one cell
GM_BENCH_SPKI='<pin>' GM_BENCH_ORIGINS=h1-clear,h1-tls,h2,h3 GM_BENCH_REPS=5 mise run bench-throughput  # full matrix
```

The full matrix takes hours. For a shaped path on a dedicated Linux host (certificate must cover `10.77.0.2`; the
fixture runs `ip netns exec` without elevating itself and starts the server inside `gmbench`):

```sh
sudo client/bench/rig.sh up lan-fast-lossy
GM_BENCH_SPKI='<pin>' GM_BENCH_HOST=10.77.0.2 GM_BENCH_NETNS=gmbench mise run bench-throughput
sudo client/bench/rig.sh down   # always, including after a failure
```

## Coordinated servers

`mise run bench-servers` builds the production server, creates a client, a router and four servers in disposable
user and network namespaces with its own certificate, and writes raw JSONL results and logs outside the checkout. It
never touches host interfaces or queue disciplines. It needs `iproute2` (`ip`, `tc`), `util-linux` (`unshare`,
`nsenter`), OpenSSL, curl and the pinned Chrome for Testing:

```sh
BUN_CHROME_PATH=/path/to/chrome GM_MULTI_BENCH_OUTPUT=/tmp/graphite-meter-servers mise run bench-servers
```

Profiles `server-cap`, `differing-rtt` and `shared-cap` run twice each; narrow them with `GM_MULTI_BENCH_PROFILES`
and `GM_MULTI_BENCH_REPEATS`. Record each server's contribution when comparing runs: a combined rate alone cannot
show which path limited it.
