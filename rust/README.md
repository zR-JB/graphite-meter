# Rust workspace

`rust/` holds the server (`graphite-meter-server`) and the terminal client (`graphite-meter-client`), both following
the contracts in [`api/`](../api/). The server ships as a linux/amd64 and linux/arm64 image, the client as archives
for Linux amd64 and arm64 and Windows amd64. Commands, tests and toolchain pins are in the
[development guide](../docs/DEVELOPMENT.md).

| Crate              | Role                                                                                                     |
| ------------------ | -------------------------------------------------------------------------------------------------------- |
| `proto`            | Contracts without IO: routes, wire records, strict JSON, discovery and catalog types, origins and punycode, durations, PKCE and the flag parser both binaries use. |
| `net`              | OS and network glue: the pinned runtime pool, UDP sizing and `SO_REUSEPORT`, dialing, proxies, the trust store, the ring crypto provider and QUIC transport settings. |
| `http3`            | HTTP/3 and WebTransport over the Noq fork, for both roles.                                               |
| `server`, `client` | The two binaries.                                                                                        |
| `legal`            | Notice embedding for the build scripts and the `--legal` printer.                                        |
| `testkit`, `e2e`   | Tests only: TLS identities, a delay and fault relay, and the client against the in-process server.       |

Build variables:

| Variable                         | Meaning                                                                                              |
| -------------------------------- | ---------------------------------------------------------------------------------------------------- |
| `GM_ENGINE_VERSION`              | The version the binaries report; the build tasks set it, plain `cargo build` leaves `0.0.0-dev`.     |
| `GM_LEGAL_DIR`                   | Reviewed notices to embed; without it a build embeds none and `--legal` refuses.                     |
| `GM_ASSET_DIR`                   | The browser app the server embeds; reviewed builds take it from the legal directory.                 |
| `CC_<target>`, `CFLAGS_<target>` | musl cross compilers; flags a caller sets replace `.cargo/config.toml`'s `-DMI_DEFAULT_ALLOW_THP=0`. |

Plain `cargo build` uses the unoptimized debug profile; compare performance only between equal optimized profiles.

## Dependencies

The workspace pins exact revisions of the [Noq](https://github.com/zR-JB/noq) and [h2](https://github.com/zR-JB/h2)
forks, each commit's purpose recorded in [`legal/rust-forks.json`](../legal/rust-forks.json): reliable stream reset
(`RESET_STREAM_AT`, which keeps a canceled WebTransport stream's association header) and buffer-budget hooks. Ring is
the only TLS and QUIC crypto provider. First-party crates forbid `unsafe`; ring and mimalloc contain C and assembly.

## Threading

- The server serves each TCP connection and its streams on one pinned current-thread runtime, one per worker. On
  Linux those runtimes run five nice steps lower than the rest of the process.
- HTTP/3 on Linux runs one endpoint per two runtime threads (two to sixteen, as the budget allows) on one port shared
  through `SO_REUSEPORT`. Connection IDs name their endpoint and a classic BPF program has the kernel deliver each
  datagram there, so a migrating client keeps its connection; a new connection starts on the endpoint holding the
  fewest. Where the kernel refuses the program, an endpoint forwards datagrams that name another. Admission, Retry and
  limits stay server-wide.
- A WebSocket latency bus moves after its upgrade to the multi-thread runtime, which runs no transfer, so its probes
  never queue behind a transfer. An HTTP/1 or HTTP/2 socket yields after moving 256 KiB. A WebTransport bus stays on
  its endpoint's runtime.
- Log lines reach stderr from their own thread through a 1024-line queue; while stderr stalls, lines are dropped and
  counted instead of blocking a runtime.
- The client runs each lane group on one pinned runtime: one HTTP/1.1 connection per lane, one HTTP/2 connection per
  direction, HTTP/3 downloads on the path's connection with a bidirectional upload on its own, and one WebTransport
  session per group of up to 16 lanes. Its prober and latency bus run on a separate runtime; the lane threads run
  five nice steps lower on Linux.

## Memory

One budget, `GM_MAX_BUFFER_BYTES`, covers every HTTP/2 and QUIC buffer, charged as it fills; HTTP/1 holds no floor.
Startup refuses a budget below `GM_MAX_CONNECTIONS` times the larger connection floor (HTTP/2 1.5 MiB, QUIC about
0.5 MiB at default limits) plus the QUIC endpoint buffers and the 256 KiB download block. Receive windows stay 64 KiB
until an admitted upload reads; an admitted HTTP/2 upload then opens its connection's window to 24 MiB, 8 MiB per
stream. From a quarter of the budget or connection capacity, unvalidated QUIC handshakes need Retry; from three
quarters no window grows. A certificate reload that does not fit keeps the previous chain. The budget bounds buffers,
not resident memory. Two seconds after the last connection closes, the server returns its threads' free pages.

## Measured decisions

- Release builds use fat LTO and one codegen unit; the `ci` profile is for local runs and checks only.
- Image and client builds use a PGO profile per architecture, trained in the builder on loopback transfers over every
  listener (`scripts/pgo.py`); it saved 10–17 % CPU per byte on HTTP/3 on amd64 and 2–16 % on arm64.
- Static musl builds link mimalloc with transparent huge pages off (`MIMALLOC_ALLOW_THP=2` restores them): with musl's
  allocator HTTP/2 downloads ran at a sixth of the rate.
- A connection and its streams stay on one pinned runtime on both sides (`e2e/tests/lanes.rs`); two HTTP/3 endpoints
  on four workers cost less CPU per byte than one or four.
- QUIC receive windows autotune from 768 KiB to 48 MiB per connection and 32 MiB per stream, sends from 2 to 16 MiB,
  with receive batches of four GRO messages (`mise run delayed-downloads`).
- Noq's Cubic follows RFC 9438's Reno-friendly region; its queue under load matches bulk TCP on the same link.
- The HTTP/2 server polls its streams before the socket, which keeps an upload's peak RSS at 9.5 MiB instead of
  33–36 MiB.
- Admitted HTTP/2 uploads open a 24 MiB connection window: on a 200 ms, 1 Gbit/s path 16 MiB carried 640 Mbit/s and
  24 MiB 903. The h2 fork keeps a 1 MiB credit refresh, as 256 KiB cost 7–13 % more CPU per byte.
- HTTP/3 replies over 1 MiB wait while the connection holds more than eight congestion windows unacknowledged, so bulk
  replies written ahead cannot starve control replies on slow links.
- Upload control rides the throughput check's connection: a fresh one started bidirectional uploads two round trips
  late, in slow start.
- The client's HTTP/2 windows are 32/64 MiB with 64 KiB frames; a silent QUIC address gets 3 s, an answering one 5 s
  more.
