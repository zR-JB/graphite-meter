# Benchmarks

This document records historical reference testing and the maintained Chromium throughput harness.
The numbers are loopback measurements from specific systems. They show client and server
implementation limits, not the capacity of an arbitrary network.

The historical raw NDJSON rows are not retained. The tables below are the surviving evidence. The
maintained harness under `client/bench` keeps the transport and lane matrix that remains useful for
new browser and protocol versions.

## Reference systems

The reference tests used two Linux systems:

- pass one: 8-core virtual machine with 7 GB RAM;
- pass two: 8-core, 16-thread x86 desktop with 30 GiB RAM, the performance CPU governor, and
  enlarged TCP buffers.

Pinned Chromium and Firefox builds plus a Mozilla-distributed Firefox build were used during the
historical tests. The exact pass-two browser build identifiers were not recorded. The maintained
reproduction harness is Chromium-only and uses Bun.WebView.

## Method

Bun.WebView drives the production transfer workers through the benchmark-only `window.__gmBench`
hook. It does not reimplement the transfer loop.

Each cell contains:

- a 3-second warmup;
- an 8-second measured window;
- teardown and one appended NDJSON result.

Cells run in a fresh deterministic permutation each repeat round. Published values are sustained
window means and use medians across repeats, not short instantaneous peaks.

The loopback rig has no intentional delay, loss, or rate cap. It measures software ceilings. The
optional `client/bench/rig.sh` network namespace uses a veth pair and `netem` for shaped-path
experiments.

Absolute results varied by approximately 6 percent between repeated Chromium runs. HTTP/2
varied by more than 13 percent in some repeated cells. Treat magnitudes accordingly.

## Historical results

### Highest recorded sustained rates

| Configuration                               |          Download |            Upload |
| ------------------------------------------- | ----------------: | ----------------: |
| Chromium, HTTP/1.1 clear, 2 lanes           |  **49.00 Gbit/s** |  **16.95 Gbit/s** |
| Firefox, HTTP/1.1 clear, fresh process      |       9.22 Gbit/s |      not recorded |
| Firefox, HTTP/1.1 clear, settled best lanes |       6.45 Gbit/s |       7.53 Gbit/s |
| Mozilla Firefox build, HTTP/1.1 clear       |      14.52 Gbit/s |      not recorded |
| Native Go client, 1 lane                    |      71.71 Gbit/s |      42.83 Gbit/s |
| Native Go client, 8 lanes                   | **362.59 Gbit/s** | **239.71 Gbit/s** |

The native result is an upper bound for this machine, not a target for browser clients. The native
client can configure socket behavior that browser APIs do not expose.

### Chromium transport matrix

Loopback, five repeats, best measured lane count:

| Transport              |         Download | Lanes |           Upload | Lanes |
| ---------------------- | ---------------: | ----: | ---------------: | ----: |
| HTTP/1.1 clear         | **49.00 Gbit/s** |     2 | **16.95 Gbit/s** |     2 |
| HTTP/1.1 TLS           |     25.45 Gbit/s |     2 |     13.35 Gbit/s |     1 |
| HTTP/2                 |     13.30 Gbit/s |     4 |      9.77 Gbit/s |     4 |
| HTTP/3                 |      2.83 Gbit/s |     1 |      1.66 Gbit/s |     1 |
| WebTransport streams   |      3.01 Gbit/s |     1 |      1.78 Gbit/s |     2 |
| WebTransport datagrams |      1.62 Gbit/s |     1 |      0.36 Gbit/s |     1 |

These results do not define a universal transport ranking. Firefox reached 10.88 Gbit/s in one
fresh HTTP/3 screening run and produced a different ordering. Browser engine, browser state,
proxy behavior, path latency, congestion, encryption, and loss can change the result.

### Shaped loss profile

Chromium, `lan-fast-lossy`, five repeats:

| Transport              |    Best download |       One lane |      Best upload |       One lane |
| ---------------------- | ---------------: | -------------: | ---------------: | -------------: |
| HTTP/1.1 clear         | 8.12 Gbit/s at 2 |           6.98 | 8.71 Gbit/s at 1 |           8.71 |
| HTTP/1.1 TLS           | 6.62 Gbit/s at 2 |           5.42 | 7.60 Gbit/s at 1 |           7.60 |
| HTTP/2                 | 4.02 Gbit/s at 1 |           4.02 | 5.90 Gbit/s at 4 |           5.36 |
| HTTP/3                 | 2.75 Gbit/s at 1 |           2.75 | 1.55 Gbit/s at 1 |           1.55 |
| WebTransport streams   | 2.95 Gbit/s at 1 |           2.95 | 1.62 Gbit/s at 1 |           1.62 |
| WebTransport datagrams |      1.54 Gbit/s | not applicable |      0.36 Gbit/s | not applicable |

Parallel HTTP/1.1 lanes use separate connections and congestion windows. Multiplexed streams share
one connection, so additional streams do not provide the same loss resilience. Firefox again
showed different lane behavior.

The shaping rig's configured loss rate did not equal its delivered loss rate, so the loss figures
are evidence for that named profile rather than calibrated physical-link loss percentages.

## Server resource observations

Across the historical 67-cell Chromium matrix at five repeats, the server stayed below 1.73 CPU
cores and 27 MiB resident memory while delivering up to 49.00 Gbit/s on the reference machine.

These values include one browser, server, and loopback environment. They are not deployment sizing
guarantees. HTTP/3 and WebTransport performed less throughput per server CPU than the kernel TCP
paths in these tests.

## Browser limits

The native client moved substantially more data than either browser on the same host. A Chromium
single-process diagnostic raised one sustained download from 47.6 to 69.2 Gbit/s, indicating that
browser process boundaries contributed to the ceiling. That diagnostic is unstable and is not a
supported configuration.

Firefox results depended strongly on process age. The first substantial download in a fresh
process was faster than later runs, and settled results varied between processes. Firefox also
retained substantial memory during large responses on the reference system. This behavior was
reproduced with another browser speed-test implementation, so it was not isolated to Graphite
Meter.

For high-rate testing, treat browser choice, available memory, and test duration as part of the
measurement environment. Use the native client when the browser becomes the limiting component.

## Findings reflected in current defaults

- Chromium HTTP/1.1 reached its loopback plateau with a small number of parallel connections.
- Separate HTTP/1.1 connections helped on the shaped loss profile.
- Multiplexed transports did not benefit from the same lane policy in Chromium.
- WebTransport uses one continuous stream per direction automatically.
- Browser upload payload memory is bounded and scaled down on devices reporting less memory.
- The upload result remains server-received bytes and time regardless of the browser's payload
  strategy.

These are client implementation choices rather than new server configuration requirements.

## Reproduction

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
| `GM_BENCH_REPS`       | `3`                            | Repeat rounds per cell. Historical tables used 5. |
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

Create a shaped path:

```sh
sudo client/bench/rig.sh up lan-fast-lossy
sudo ip netns exec gmbench <server-binary-bound-to-10.77.0.2>
GM_BENCH_SPKI='<pin>' GM_BENCH_HOST=10.77.0.2 mise run bench-throughput
sudo client/bench/rig.sh down
```

Set `GM_BENCH_NETNS` instead when the fixture should start the server inside the namespace.
`mise run bench-wire` measures wire-codec cost, and `mise run stress` measures the server saturation
envelope.

## Limitations

- Absolute rates are machine-specific and browser-version-specific.
- The historical raw rows and exact pass-two browser build identifiers are unavailable.
- Most historical tuning sweeps used Chromium, clear HTTP/1.1, and loopback.
- HTTP/2 showed enough repeated-run variation that lane conclusions are weak.
- QUIC upload rates changed materially between the two machines and remain unexplained.
- Firefox shaped-path coverage was limited and does not support Chromium-wide conclusions.
- No historical reference test covered real internet paths, Wi-Fi hardware, mobile devices,
  Safari, or shaped capacity above 10 Gbit/s.
- Loopback results demonstrate software headroom. They do not prove equivalent performance across
  a real NIC or network.
