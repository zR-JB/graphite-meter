# Rust implementation

`rust/` holds a second implementation of the server (`graphite-meter-server`) and of the native terminal client
(`graphite-meter-client`). Both follow the shared contracts in [`api/`](../api/), Go's flags, usage text, `GM_*`
settings, report output and exit codes, and the Go TUI's look and keys; either client works with either server.

Go stays the default. The Rust builds are opt-in: stable releases can add a linux/amd64 + linux/arm64 server image
tagged `X.Y.Z-rust` (with `X.Y-rust` and `latest-rust` following the newest such release) and TUI archives for Linux
amd64/arm64 and Windows amd64 named like Go's with a `_rust` marker; prereleases can add the image alone, tagged
`X.Y.Z-{alpha,beta,rc}.N-rust`. There is no Rust build for macOS.
[Deployment](../docs/DEPLOYMENT.md#experimental-rust-image) covers the image and its tags.

## Building and running

The workspace pins Rust 1.99.0 (`rust-toolchain.toml`); mise pins cargo-deny and cargo-nextest. Tests need OpenSSL;
the interoperability tasks also need Go and `musl-tools`. From the repository root:

```sh
GM_IMPLEMENTATION=rust mise run dev          # Rust server with the development browser app
GM_IMPLEMENTATION=rust mise run tui -- --url https://meter.example
mise run rust-server-run -- --h1-addr :8080  # arguments after -- go to the server
mise run rust-client-run -- --report --url https://meter.example
```

| Task | Does |
|---|---|
| `rust-server-run`, `rust-client-run` | Build with the `ci` profile (no LTO, 16 codegen units) and run. |
| `rust-server-build`, `rust-client-build` | Build `target/release/graphite-meter-{server,client}` with the release profile (fat LTO, one codegen unit). |
| `rust-check` | Formatting, fork pins, legal reviews and crate budgets, `cargo deny --locked check`, clippy, nextest. |
| `rust-check-targets` | Check the shipped musl and Windows targets (needs mingw-w64, musl-tools and an aarch64 GNU compiler). |
| `rust-format` | `cargo fmt --all`. |
| `rust-interop` | Go's QUIC, HTTP/3 and WebTransport peers and the Go TUI against the static Rust server. |
| `rust-client-interop` | The Rust TUI against Go's server, signed in through Go's approval pages. |
| `rust-delayed-downloads` | The delayed-link HTTP/3 and WebTransport download gate; a run that measures nothing fails. |
| `rust-e2e SERVER` | The browser suite against a Rust server executable. |
| `rust-container-build` | The server image `graphite-meter:latest-rust` (amd64 build host). |
| `rust-client-package` | The Linux and Windows TUI archives and their source offers in the pinned builder (amd64 build host). |

`GM_IMPLEMENTATION=rust` switches `dev`, `prod` and `tui` to the Rust server or TUI; `prod` uses the release
profile, `dev` and `tui` the `ci` profile. Plain `cargo build` uses the unoptimized debug profile; compare
performance only between equal optimized profiles.

Local builds embed development notices, so their executables say `UNREVIEWED DEVELOPMENT BUILD` and are not
distributable; the pinned builder in `container/Dockerfile.rust` makes reviewed release builds
([legal pipeline](../legal/README.md#rust-notices)). The server's browser app is built once per profile into
`target/browser/` and reused until a browser input changes.

Build-time variables:

| Variable | Meaning |
|---|---|
| `GM_ENGINE_VERSION` | The version the binaries report; the build tasks set `VERSION-rust`, plain `cargo build` leaves `0.0.0-rust-dev`. |
| `GM_RUST_LEGAL_DIR` | Reviewed notices to embed; without it a build embeds none. |
| `GM_RUST_ASSET_DIR` | The browser app the server embeds; reviewed builds take it from the legal directory. |
| `CC_<target>`, `CFLAGS_<target>` | musl cross compilers; flags a caller sets replace `.cargo/config.toml`'s `-DMI_DEFAULT_ALLOW_THP=0`. |

Run-time settings beyond Go's:

| Setting | Meaning |
|---|---|
| `GM_MAX_BUFFER_BYTES` / `--max-buffer-bytes` | Buffer budget of the HTTP/2 and QUIC listeners, default 8 GiB (below). |
| `TOKIO_WORKER_THREADS` | Worker threads, default the available CPUs; each adds one pinned connection thread. |
| `MIMALLOC_ALLOW_THP=2` | Restores mimalloc's transparent huge pages, off by default in musl builds. |
| `--legal` (server) | Prints the project's copyright, source and reviewed third-party notices, as Go's TUI does. |

## Architecture

| Crate | Role |
|---|---|
| `proto` | Contracts without IO: routes, wire records, strict JSON, discovery and catalogue types, origins and punycode, Go durations, PKCE, and the Go-style flag parser both binaries use. |
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
covers, on one port shared through `SO_REUSEPORT`; connection IDs carry their endpoint, which forwards short-header
packets naming another, so a migrating client keeps its connection. Admission, Retry and limits stay server-wide.
The client dials and runs each lane group on one pinned runtime: one HTTP/1.1 connection per lane, one HTTP/2
connection per direction, HTTP/3 downloads on the path's connection with a bidirectional upload on its own, and one
WebTransport session per group of up to 16 lanes.

**Memory.** One budget (`GM_MAX_BUFFER_BYTES`) covers every HTTP/2 and QUIC buffer, charged as it fills; HTTP/1
holds no floor. Startup refuses a budget below `GM_MAX_CONNECTIONS` times the larger connection floor, the QUIC
endpoint buffers and the 256 KiB download block. A QUIC connection holds a floor of its handshake, certificate
flights and Noq's stream state (481 KiB at default limits), about 0.5 MiB in all; an HTTP/2 connection 1.5 MiB.
Receive windows stay 64 KiB until an admitted upload reads, so a silent peer holds at most 192 KiB of reassembly.
From a quarter of the budget or the connection capacity, unvalidated QUIC handshakes need Retry; from three
quarters no window grows. A certificate reload that does not fit keeps the previous chain. This accounting is not a
resident-memory bound.

**Measured decisions**, with their evidence:

- Release builds use fat LTO and one codegen unit; the `ci` profile only for local runs and CI checks
  (PR #210's profile comparison).
- Static musl builds link mimalloc (`rustfs-mimalloc` 0.5.6) with transparent huge pages off: with musl's allocator
  the TUI downloaded HTTP/2 at 3.5 instead of 20 Gbit/s (CI runs 37322843663 and 37326242534). Two seconds after the
  last connection closes, once per idle period, the server returns every runtime thread's free pages.
- A connection and its streams stay on one pinned runtime on both sides (`e2e/tests/lanes.rs` checks the pinning);
  two HTTP/3 endpoints on four workers cost less CPU per byte than one or four (PR #210).
- QUIC receive windows autotune from 768 KiB to 48 MiB per connection and 32 MiB per stream, sends from 2 to 16 MiB,
  with receive batches of four GRO messages (`mise run rust-delayed-downloads`).
- The HTTP/2 server polls its streams before the socket, which keeps an upload's peak RSS at 9.5 MiB instead of
  33–36 MiB (CI run 37265088955); uploads keep large windows nearly full (`server/tests/sockets/http2.rs`).
- The client's HTTP/2 windows are 32/64 MiB with 64 KiB frames; a silent QUIC address gets 3 s, an answering one 5 s
  more (`e2e/tests/net.rs`).

## Differences from Go

User-visible behaviour that deliberately differs from Go's server and TUI.

**Origins, discovery and flags**

- Origins compare parsed: `https://h:0443` is `https://h`, and port 0 is refused in any spelling.
- IPv4-mapped IPv6 hosts print as `[::ffff:127.0.0.1]`, so a grant request from `https://[::ffff:7f00:1]` is refused.
- Discovery refuses `maxStageMs: 0`, `null` in string, boolean or list members, and a missing `server.name` or
  `engineVersion`.
- Targets of an unknown transport count toward a discovery list's limit of 32.
- An invalid catalogue entry is left out alone, and the default selection drops it; selecting it names its fault.
  Go refuses the whole catalogue.
- International hosts convert to punycode only for common-script letters IDNA keeps unchanged; `--url` and server
  origins take ASCII hosts.
- Durations whose parts overflow are refused; Go wraps them around.
- Arguments must be UTF-8.

**Server**

- A failed listen reads `listen tcp :7246: address already in use`, without Go's `bind: `.
- Log lines use Go's `log` format in UTC; accept-retry and TLS handshake lines carry Rust's error texts.
- Failed QUIC handshakes log `[gm:h3] QUIC handshake error from ADDR`, rate-limited; `[gm:memory]` lines report
  endpoints the budget does not cover and when window growth is held or resumed.
- `version` and `--version` print `X.Y.Z-rust`, which `/preflight` and `/servers` also report; `--legal` prints
  the notices.
- A path with an empty segment, such as `//api/download`, gets 404 where Go answers 307 to the cleaned path.
- HTTP/1 reads request heads up to 128 KiB and refuses those over 32 KiB with 431; Go stops reading at 36 KiB.
- The upload progress feed sends a heartbeat after 1 s without a line; Go sends one every second.
- A reply write blocked for 30 s ends the reply on every listener, progress streams included.
- HTTP/2 replies that end unfinished reset their stream with CANCEL, where Go sends INTERNAL_ERROR.
- HTTP/2 streams past 250 concurrent get REFUSED_STREAM.
- A connection with a raised receive window goes away after 15 s without admitted work; idle HTTP/3 then closes at
  once, HTTP/2 after 5 s.
- A request or session the budget cannot hold gets `H3_REQUEST_REJECTED`; refused control metadata past a
  connection's floor closes only that connection with `INTERNAL_ERROR`.
- HTTP/3 SETTINGS over 8 KiB or with more than 64 identifiers close with `H3_EXCESSIVE_LOAD`.
- A QUIC handshake has 10 s in all, and a silent client holds it that long; quic-go ends one after 5 s without a
  packet.
- The server follows the client's TLS 1.3 suite order, so OpenSSL clients get AES-256-GCM (about 3% more CPU per
  byte); Go prefers AES-128-GCM.
- The WebSocket close handshake has 5 s in all; refused upgrades add `Allow: GET` for HEAD and
  `Upgrade: websocket` for HTTP/1.0, with empty bodies.
- Shutdown drains for 5 s; HTTP/3 then takes up to 1 s more to send its closes.
- With `GM_VERBOSE` a WebTransport session counts as one transfer however many streams it uses.

**Authentication**

- `GM_AUTH_PUBLIC_URL` serves canonically: `HTTPS://Meter.Example:08443` as `https://meter.example:8443`.
- `GM_AUTH_OIDC_ISSUER` needs an ASCII host and path, a nonzero port and no `?`.
- The `[gm:auth] mode=` line prints the canonical origin before OIDC discovery runs.
- Sign-in forms need URL-encoded bodies with unique fields; another body shows the "failed" notice.
- Passwords, challenges and CSRF proofs are read only from form bodies, never from the URL query.
- The device cookie must be canonical unpadded base64url.
- A CLI approval page another login opened is refused at once.
- `expires` in `/auth/session` and CLI token answers is a whole-second UTC time.
- A socket ticket's `target` must spell its route's path exactly, and a ticket minted without `Origin` is refused
  to a request with an empty one.
- 303 redirects from GET `/auth/cli`, `/auth/browser` and `/auth/oidc/callback` carry no body.
- Every OIDC callback answer clears the transaction cookie.
- OIDC sign-in stops waiting for the provider 3 s before the callback's 15 s exchange bound.
- OIDC verifies RS256–512, PS256–512, ES256, ES384 and EdDSA with RSA keys of 2048–8192 bits; a provider advertising
  only ES512 is refused at discovery.
- Discovery refuses endpoints with a fragment, port 0 or non-ASCII text, a `null` algorithm list and a non-boolean
  issuer-parameter flag.
- Provider JSON matches member names case-sensitively and refuses a repeated member; ID token times must be numbers.
- The token endpoint must answer 200 with JSON, whatever its content type.
- ID tokens must be compact JWS of at most 16 KiB without `cty`, `crit` or `enc`, typed JWT or JOSE if at all;
  signed user information must name the issuer and the client.
- A key whose `use` is not `sig`, whose `key_ops` lacks `verify` or whose `alg` is not the token's verifies nothing.
- A key set naming `keys` twice is refused; the first 64 usable keys are kept, an unreadable key is skipped, padded
  members are read and a key repeating a member keeps the last.

**Client networking**

- A proxy variable the client cannot use fails each request it would carry, naming the variable.
- NO_PROXY ports compare canonically: a `host:080` entry matches nothing, and `host:80` also bypasses `host:080`.
- TLS to an `https://` proxy offers no ALPN, so the proxy hop is always HTTP/1.1.
- Proxy TLS, SOCKS5 and CONNECT share one 60 s bound; CONNECT sends no User-Agent, caps the answer's head at 64 KiB
  and fails as `proxy refused CONNECT with STATUS`.
- TLS sessions resume across connections to a server; only TLS 1.3 suites follow Go's order.
- A trusted root serving as its own certificate passes whatever its EKU; a CA certificate that only chains to a
  root is refused.
- TCP dials, name lookup included, time out after 9 s (Go 10 s); HTTP/3 tries each resolved address, IPv4 first.
- A control request has 10 s in all, connecting and reading its body included; one without response headers in
  10 s retires its HTTP/2 connection.
- HTTP/2 connections idle 30 s are pinged and closed after 20 s unanswered.
- A bodyless request that fails on a reused connection, POST included, is sent once more on a new one.
- Idle HTTP/1.1 connections stay until unusable, at most 32 per origin; each HTTP/1.1 lane attempt dials anew.
- Requests a grant may not accompany (cleartext, `--insecure`, another host) go without it; Go fails them.
- A target enrolled by two servers carries the first server's grant.
- Sign-in failures do not include the login URL.

**Client runs**

- Invalid `--url`, `--server`, `--throughput-origin` or `--latency-origin` values are usage errors (status 2), and
  the origin flags take no target IDs.
- `--ping` and `--loaded-ping` refuse negative durations; Go reads `-1ns` as reply-driven.
- `--streams` and `--auto-streams` take decimal counts only.
- Automatic paths try the next transport after any fault except a sign-in request.
- In the TUI a sign-in request over `http://` or `--insecure` fails the server as preparation-failed, naming why.
- A server whose catalogue origin changed since the last check is checked again instead of refused.
- The datagram latency check sends each repeat under a new ID, so only the latest probe's reply counts.
- A sign-in refusal of the first upload checkpoint fails the server as sign-in-required.
- A 4xx other than 429 or sign-in is final for upload sessions, progress feeds and checkpoints.
- WebTransport downloads use 64 MiB streams: a longer one fails the lane, a shorter one redials.
- One WebTransport session or upload-control attempt may take 10 s; Go cuts each at its 2 s redial window.
- A receiver checkpoint reporting 0 ns counts; upload totals ignore counts naming a replaced receiver.
- A latency channel lost within 500 ms of opening redials after 500 ms, unless that reaches the window's end.
- Probe replies arriving while a window drains count in its results but not on the live trace.
- A stopped stage records no insufficient-evidence failures.

**Report and TUI**

- The TUI shows Recovering while every server's bytes in a direction stand still for 2 s.
- The report header's duration starts with the stages; Go's includes the path check.
- A latency note gives its stage's measured time; throughput notes leave out the sample count.
- Details keep the latest 128 aggregation intervals of each stage; Go keeps 128 for the whole run.
- The server chooser shows each entry's resolved origin.
- The setting editor moves with arrows, Home and End only.
- A test stopped before it starts finishes the path check it replaced.
- Colour depth comes from the environment only, so 24-bit colour that only terminfo or tmux reports gets 16 or 256
  colours; 16-colour frames write `38;5;0`–`15`.
- On Windows the palette assumes a dark background.
- On exit the TUI restores the terminal's previous window title where supported.
- In the TUI the first interrupt or termination sets the exit status.
- A second signal in report mode, or output into a closed pipe, exits with 130, 143 or 141 instead of dying of the
  signal; the shell sees the same status.
- A build without reviewed notices refuses `--legal` with status 1.
