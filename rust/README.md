# Rust workspace

`rust/` holds the server (`graphite-meter-server`) and the terminal client (`graphite-meter-client`). Both follow the
shared contracts in [`api/`](../api/). The server ships as a linux/amd64 and linux/arm64 image, the client as archives
for Linux amd64/arm64 and Windows amd64. Commands, tests and toolchain pins are in the
[development guide](../docs/DEVELOPMENT.md).

Build-time variables:

| Variable | Meaning |
|---|---|
| `GM_ENGINE_VERSION` | The version the binaries report; the build tasks set it, plain `cargo build` leaves `0.0.0-dev`. |
| `GM_LEGAL_DIR` | Reviewed notices to embed; without it a build embeds none. |
| `GM_ASSET_DIR` | The browser app the server embeds; reviewed builds take it from the legal directory. |
| `CC_<target>`, `CFLAGS_<target>` | musl cross compilers; flags a caller sets replace `.cargo/config.toml`'s `-DMI_DEFAULT_ALLOW_THP=0`. |

Plain `cargo build` uses the unoptimized debug profile; compare performance only between equal optimized profiles.
`MIMALLOC_ALLOW_THP=2` restores mimalloc's transparent huge pages, off by default in the static builds.

## Architecture

| Crate | Role |
|---|---|
| `proto` | Contracts without IO: routes, wire records, strict JSON, discovery and server catalog types, origins, durations, PKCE, and the flag parser both binaries use. |
| `net` | OS and network glue: pinned runtime pool, UDP sizing and `SO_REUSEPORT`, dialing, proxies, the trust store, the ring crypto provider and the QUIC transport settings. |
| `http3` | HTTP/3 and WebTransport over the Noq fork for both roles. |
| `server`, `client` | The two binaries. |
| `legal` | Notice embedding for the build scripts and the `--legal` printer. |
| `testkit`, `e2e` | Test only: TLS identities, a delay and fault relay, and the client against the in-process server. |

The workspace uses exact revisions of the [Noq](https://github.com/zR-JB/noq) and [h2](https://github.com/zR-JB/h2)
forks, recorded with each commit's purpose in [`legal/rust-forks.json`](../legal/rust-forks.json): reliable stream
reset (`RESET_STREAM_AT`, which keeps a cancelled WebTransport stream's association header) and buffer-budget
hooks. Ring is the only TLS and QUIC crypto provider. First-party crates forbid `unsafe`; ring and mimalloc contain
C and assembly.

**Threading.** The server runs one current-thread runtime per worker and serves each TCP connection and its streams
on one of them. On Linux HTTP/3 runs one endpoint per two runtime threads, two to sixteen, as many as the budget
covers, on one port shared through `SO_REUSEPORT`. Connection IDs carry their endpoint, and a classic BPF program on
the port has the kernel deliver each datagram to the endpoint its ID names, so a migrating client keeps its connection
and no endpoint's thread handles another's datagrams. A new connection starts on the endpoint holding the fewest, so
a client's WebTransport bus and its transfers run on different threads while there are threads to spare. Where the
kernel refuses the program, connections stay where they arrive and an endpoint forwards what names another.
Admission, Retry and limits stay server-wide.
After its upgrade a WebSocket bus moves to the multi-thread runtime, which runs no transfer, its socket re-registered
with that runtime's driver, so its probes never queue behind a transfer's task; on Linux the per-worker runtimes run
five nice steps lower. An HTTP/1 or HTTP/2 connection's socket moves at most 256 KiB before its task lets the other
tasks run and its thread offers the CPU to waiting threads, so the bus's thread does not wait out a transfer's scheduler
slice. A WebTransport bus stays on its endpoint's runtime. Log lines reach stderr from a thread of their own through a 1024-line queue; while stderr stalls, lines are
dropped and counted instead of blocking a runtime.
The client dials and runs each lane group on one pinned runtime: one HTTP/1.1 connection per lane, one HTTP/2
connection per direction, HTTP/3 downloads on the path's connection with a bidirectional upload on its own, and one
WebTransport session per group of up to 16 lanes. Its prober and latency bus, WebSocket or WebTransport, run on a
latency runtime of their own, away from the lanes, whose threads run five nice steps lower on Linux. Upload control requests use the throughput check's protocol, on its
connection except over HTTP/3.

**Memory.** One budget (`GM_MAX_BUFFER_BYTES`) covers every HTTP/2 and QUIC buffer, charged as it fills; HTTP/1
holds no floor. Startup refuses a budget below `GM_MAX_CONNECTIONS` times the larger connection floor, the QUIC
endpoint buffers and the 256 KiB download block. A QUIC connection holds a floor of its handshake, certificate
flights and Noq's stream state (481 KiB at default limits, 978 KiB with per-client limits at the totals), about
0.5 MiB in all at default limits; an HTTP/2 connection 1.5 MiB.
Receive windows stay 64 KiB until an admitted upload reads, so a silent peer holds at most 192 KiB of reassembly.
An admitted HTTP/2 upload then opens its connection's window to 24 MiB, 8 MiB per stream, on its client's credit.
From a quarter of the budget or the connection capacity, unvalidated QUIC handshakes need Retry; from three
quarters no window grows. A certificate reload that does not fit keeps the previous chain. This accounting is not a
resident-memory bound.

**Measured decisions**, with their evidence:

- Release builds use fat LTO and one codegen unit; the `ci` profile only for local runs and CI checks.
- Image and TUI builds use a PGO profile per architecture, trained in the builder on loopback transfers over every
  listener (`scripts/pgo.py`): amd64 natively, arm64 under QEMU's user mode, the Windows TUI with amd64's. Against
  the same builds without a profile, the image's server spent 10–17 % less CPU per byte on HTTP/3 on amd64 and 2–16 %
  on arm64, within 2 % on HTTP/2 and HTTP/1.1, and the TUI 9–12 % less on HTTP/3; the amd64 TUI downloaded HTTP/3 4 %
  slower against a server without a profile (release profile, campaign run 38658016677). Local builds take no profile.
- Static musl builds link mimalloc (`rustfs-mimalloc` 0.5.6) with transparent huge pages off: with musl's allocator
  the TUI downloaded HTTP/2 at 3.5 instead of 20 Gbit/s (CI runs 37322843663 and 37326242534). Two seconds after the
  last connection closes, once per idle period, the server returns every runtime thread's free pages.
- A connection and its streams stay on one pinned runtime on both sides (`e2e/tests/lanes.rs` checks the pinning);
  two HTTP/3 endpoints on four workers cost less CPU per byte than one or four.
- QUIC receive windows autotune from 768 KiB to 48 MiB per connection and 32 MiB per stream, sends from 2 to 16 MiB,
  with receive batches of four GRO messages (`mise run delayed-downloads`).
- noq's Cubic follows RFC 9438's Reno-friendly region and K. Under 0.5 % per-packet loss a Reno sender with a 0.7
  backoff, less fair to TCP, reaches 6.4–6.7 against 5.0–5.7 Mbit/s; at 0.1 % the two are within 15 % (campaign run
  38663551289). Cubic's queue raises cable HTTP/3 loaded latency to 85–98 ms, as bulk TCP on the same link does
  (HTTP/1 loaded 110–113 ms), and router AQM bounds it for any sender (CI run 37422155310).
- The HTTP/2 server polls its streams before the socket, which keeps an upload's peak RSS at 9.5 MiB instead of
  33–36 MiB (CI run 37265088955); uploads keep large windows nearly full (`server/tests/sockets/http2.rs`).
- Admitted HTTP/2 uploads open a 24 MiB connection window: on a 200 ms, 1 Gbit/s path 16 MiB carried 640 Mbit/s with
  7 of 29 runs below 500, 24 MiB 903 (`ci` profile, laptop); the h2 fork's 1 MiB credit refresh stays, as 256 KiB
  cost 7–13 % more CPU per byte.
- HTTP/3 replies over 1 MiB wait while the connection holds more than eight congestion windows unacknowledged: Noq
  charges the peer's credit as data is written, so bulk replies written ahead starved control replies and failed
  mobile bidirectional runs (0/5 complete before, 5/5 after).
- Upload control rides the throughput check's connection: a fresh one started cable HTTP/2 bidirectional uploads two
  round trips late, out of slow start (measured in CI run 37365194673).
- The client's HTTP/2 windows are 32/64 MiB with 64 KiB frames; a silent QUIC address gets 3 s, an answering one 5 s
  more (`e2e/tests/lanes.rs`).
