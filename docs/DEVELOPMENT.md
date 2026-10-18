# Development

A Rust workspace holds the server and the terminal client. A TypeScript browser client, built with Bun, is embedded
in the server. Everything runs through `mise`.

## Setup

Install [mise](https://mise.jdx.dev/installing-mise.html) at the version in `mise.toml` (`vars.mise_version`). It
installs the pinned Bun, Python and checkers, and rustup installs the Rust release in `rust/rust-toolchain.toml`.
You also need Git, Bash, jq and OpenSSL. Browser tests need the pinned Chrome for Testing or Firefox, and container
checks need Docker or Podman.

```sh
git clone https://github.com/zR-JB/graphite-meter.git
cd graphite-meter
mise run setup      # locked tools and dependencies, the Git hook, then `mise run doctor`
mise run dev        # development build on http://localhost:7246; arguments after -- go to the server
mise run tui        # terminal client; it asks for the server, or add `-- --url <url>`
```

`mise tasks` lists every task. Run `mise run setup` in each new worktree; `node_modules`, bundles and browser
artifacts stay local to it.

## Commands

```sh
mise run check          # the developer gate; the commit hook runs the parts the staged paths touch
mise run ci             # the gates CI runs, including the target checks and the browser suite
mise run server-build   # rust/target/release/graphite-meter-server; VERSION=1.2.3 stamps a release
mise run tui-build      # rust/target/release/graphite-meter-client
```

| Layer           | Command               | Covers                                                       |
| --------------- | --------------------- | ------------------------------------------------------------ |
| Rust            | `mise run rust-check` | Formatting, dependency policy, lints, unit and socket tests. |
| Browser unit    | `mise run client-ci`  | Types, format, measurement math, codecs, state.              |
| Real-server E2E | `mise run e2e`        | Chrome or Firefox against a local fleet of real servers.     |

`dev`, `tui` and `e2e-server` use the `ci` profile, which has no LTO and 16 codegen units. The build tasks use the
release profile, which has fat LTO and one codegen unit. Local builds embed development notices, so their executables
say `UNREVIEWED DEVELOPMENT BUILD` and are not distributable. Untagged builds identify as `GM_CLIENT_REVISION`, which
is the short Git revision by default.

`mise run e2e-server` builds the server the suite runs by default, and `mise run e2e` takes another as its first
argument. `client/scripts/e2e.ts` then starts five servers with an ephemeral certificate. They are a home server with
a catalog, three public peers and a password-protected peer. The script drives the browser through
`client/e2e/webview.ts`. It drives Chrome over its DevTools Protocol, or with `GM_E2E_BROWSER=firefox` it drives
Firefox over WebDriver BiDi, where `GM_FIREFOX_PATH` names another build. Tests written `test.chrome` need the
DevTools Protocol, so Firefox skips them. Failures keep a screenshot, console, page errors and DOM under
`client/test-results/webview` and server logs under `client/test-results/servers`. To rerun a subset against a built
server:

```sh
cd client && GM_E2E_SERVER_BIN=../rust/target/ci/graphite-meter-server \
  bun run e2e bun test ./e2e --no-orphans --timeout=60000 -t "History"
```

`GM_WEBVIEW_DEBUG=1` passes Chrome's own stderr through. Two manual Chrome benchmarks run on the same fleet.
`mise run bench-ui` checks frame delivery during a run: `GM_PERF_SERVERS` lists the server counts (default `1,4`),
`GM_PERF_BIDIRECTIONAL=1` adds that stage, `GM_PERF_CPU` throttles the CPU by a factor, `GM_PERF_VIEWPORT` (`WxH`)
and `GM_PERF_DPR` emulate a screen, `GM_PERF_TARGET_MS`, `GM_PERF_P99_MS` and `GM_PERF_MAX_MS` set the frame budget
(16.7, 16.7 or 33.4 when throttled, and 50 ms), and `GM_PERF_PROFILE` and `GM_PERF_METRICS` name file prefixes for a
CPU profile and the measurements. `mise run bench-session` checks that repeated tests release their resources:
`GM_PERF_RUNS` sets the count (12), `GM_PERF_BACKGROUND=1` minimizes the window during every tenth, and
`GM_PERF_METRICS` and `GM_PERF_HEAP` name files for the samples and a final heap snapshot.

For manual HTTPS and HTTP/3 runs, put certificates for the names you use in an untracked `.dev-certs/`. Chromium
also needs the leaf's SPKI pin:

```sh
openssl x509 -in .dev-certs/localhost.pem -pubkey -noout | openssl pkey -pubin -outform der \
  | openssl dgst -sha256 -binary | openssl enc -base64
```

`mise run codeql` repeats the CodeQL scan offline with the [bundle](https://github.com/github/codeql-action/releases)
release in `vars.codeql`, installed at `~/.local/share/codeql-bundle` or named by `CODEQL`.

## Layout

| Path         | Holds                                                                                          |
| ------------ | ---------------------------------------------------------------------------------------------- |
| `api/`       | The wire contracts both clients and the server follow, with test vectors.                      |
| `client/`    | The browser client, its workers, unit tests and browser suite.                                 |
| `rust/`      | The server, the terminal client and their shared crates.                                       |
| `container/` | The image build and the Compose and Quadlet examples.                                          |
| `legal/`     | Reviewed dependency records and notice sources ([legal pipeline](../legal/README.md)).         |
| `scripts/`   | Builds, packaging, CI policy and release control ([CI and release](../scripts/ci/README.md)).  |

The server and both clients share routes and the [wire contracts](../api/discovery.md), and each client owns its
measurement engine. Throughput is receiver-authoritative, latency populations stay separate per server and stage,
and presentation never defines a result. In both clients one coordinator runs one to four servers on one stage
schedule and owns preparation, warmup, boundaries, membership and cancellation. Each participant owns its
connections, credentials, upload IDs and receiver observations. Browser and native results reflect different
runtimes ([client differences](MEASUREMENTS.md#client-differences)).

### Rust workspace

| Crate              | Role                                                                                                     |
| ------------------ | -------------------------------------------------------------------------------------------------------- |
| `proto`            | Contracts without IO: routes, wire records, strict JSON, discovery and catalog types, origins, durations, PKCE and the flag parser. |
| `net`              | OS and network glue: the pinned runtime pool, UDP sizing and `SO_REUSEPORT`, dialing, proxies, the trust store, the ring crypto provider and QUIC settings. |
| `http3`            | HTTP/3 and WebTransport over the Noq fork, for both roles.                                               |
| `server`, `client` | The two binaries.                                                                                        |
| `legal`            | Notice embedding for the build scripts and the `--legal` printer.                                        |
| `testkit`, `e2e`   | Tests only: TLS identities, a delay and fault relay, and the client against the in-process server.       |

| Build variable                   | Meaning                                                                                              |
| -------------------------------- | ---------------------------------------------------------------------------------------------------- |
| `GM_ENGINE_VERSION`              | The version the binaries report. The build tasks set it, and plain `cargo build` leaves `0.0.0-dev`. |
| `GM_LEGAL_DIR`                   | Reviewed notices to embed; without it a build embeds none and `--legal` refuses.                     |
| `GM_ASSET_DIR`                   | The browser app the server embeds; reviewed builds take it from the legal directory.                 |
| `CC_<target>`, `CFLAGS_<target>` | musl cross compilers; flags a caller sets replace `.cargo/config.toml`'s `-DMI_DEFAULT_ALLOW_THP=0`. |

The workspace pins exact revisions of the [Noq](https://github.com/zR-JB/noq) and [h2](https://github.com/zR-JB/h2)
forks, and [`legal/rust-forks.json`](../legal/rust-forks.json) records each commit's purpose. Ring is the only TLS
and QUIC crypto provider, and first-party crates forbid `unsafe`.

Each TCP connection and its streams live on one pinned current-thread runtime. There is one per worker, and on Linux
it runs five nice steps lower than the rest of the process. HTTP/3 on Linux runs one endpoint per two runtime threads
on one port shared through `SO_REUSEPORT`. Connection IDs name their endpoint and a classic BPF program routes each
datagram there, so a migrating client keeps its connection. A WebSocket latency bus moves after its upgrade to the
multi-thread runtime, which runs no transfer, so probes never queue behind a transfer. An HTTP/1 or HTTP/2 socket
yields after 256 KiB. Log lines leave through a 1024-line queue and are dropped and counted while stderr stalls. The
client runs each lane group on one pinned runtime and its prober on another.

One budget, `GM_MAX_BUFFER_BYTES`, covers every HTTP/2 and QUIC buffer and is charged as each buffer fills. Startup
refuses a budget below `GM_MAX_CONNECTIONS` times the larger connection floor plus the QUIC endpoint buffers and the
256 KiB download block. The floor is 1.5 MiB for HTTP/2 and about 0.5 MiB for QUIC. Receive windows stay 64 KiB
until an admitted upload reads. An admitted HTTP/2 upload then opens a 24 MiB connection window with 8 MiB per
stream. From a quarter of the budget unvalidated QUIC handshakes need Retry, and from three quarters no window
grows. The budget bounds buffers, not resident memory. Two seconds after the last connection closes the server
returns its threads' free pages.

The following choices were measured, and this list keeps them because they are easy to undo by accident. Release
builds use fat LTO and one codegen unit. Image and client builds use a PGO profile per architecture trained on
loopback transfers (`scripts/pgo.py`), which costs 10 to 17 % less CPU per byte on HTTP/3 on amd64. Static musl
builds link mimalloc with transparent huge pages off, since musl's allocator ran HTTP/2 downloads at a sixth of the
rate. QUIC receive windows autotune from 768 KiB to 48 MiB per connection. Admitted HTTP/2 uploads get a 24 MiB
window, which reached 903 Mbit/s against 640 at 16 MiB on a 200 ms, 1 Gbit/s path. Upload control rides the
throughput check's connection, since a fresh one started bidirectional uploads two round trips late.

### Browser client

The browser client is plain TypeScript with no runtime dependencies. Each folder imports only from the rows above
it. `src/app/app.ts`, mounted by `src/main.tsx`, is the one place that wires across them.

| Folder         | Owns                                                                                                                      |
| -------------- | ------------------------------------------------------------------------------------------------------------------------- |
| `auth`         | Sources of the sign-in pages; `bun run build:auth` writes `rust/server/src/auth/assets`, which the server embeds.         |
| `src/lib`      | Signals, a JSX runtime that builds real DOM, keyed lists, the frame clock, motion, decoders.                               |
| `src/protocol` | Server documents and their decoders, routes, wire and progress records, failure reasons, worker messages.                 |
| `src/engine`   | Measurement without a DOM: `net` (catalog, selection, credentials, checks, sign-in), `lanes` and `workers`, `measure`, `run`. |
| `src/app`      | Preferences, the run model the views read, History storage, the composition root.                                        |
| `src/view`     | Every figure, unit, scale and word the views show.                                                                       |
| `src/styles`   | The design tokens and base styles.                                                                                       |
| `src/ui`       | Views: `shell`, `measure`, `settings`, `details`, `history`, `legal`, `account` and the `kit` they share.                |
| `public`       | Fonts, the favicon and the legal notices, copied into the build as they are.                                             |

Saved data and links from earlier releases keep working, so these are compatibility boundaries: `localStorage`
`graphite-meter:v1` and `graphite-meter:server-selection:v1`, the IndexedDB database `graphite-meter` at version 4
with schema-6 records, the BroadcastChannel `graphite-meter-history`, and the hash routes (Details is `endpoint` in
URLs). The page keeps the server's shell contract, which `build.ts` checks: one `</head>`, one attribute-less inline
script and style hashed into the content security policy, the `graphite-meter-trusted-types` meta naming the
Trusted Types policy `src/lib/trusted.ts` creates, `<div id="app">` and hashed assets under `/assets/`.

`bun build.ts` bundles the workers, then the app with their hashed URLs, minifies the shell, copies `public` and writes
compressed siblings. `bun check` type-checks with the TypeScript port Bun ships and formats with Biome. `bun test src`
covers pure logic and the engine against fake clocks.

## Containers and releases

`mise run container-build` and `mise run container-smoke` build and verify the image on an amd64 host. The image is a
staged build ending in `scratch`, and it holds the server binary, CA roots and license material.
`mise run tui-package VERSION` builds the client archives in the same pinned builder. Releases stamp one version across
every artifact, bind publication to the authorized commit and verify every file before publishing ([CI and
release](../scripts/ci/README.md)). Never create or move release tags in ordinary work.

Before each release, `mise run screenshots vX.Y.Z` renders the README images in `docs/assets` from the working tree,
and the result merges like any change. The browser app runs on the E2E fleet with fakes for the network and the run's
clock (`client/scripts/screenshots`), and the terminal client draws a simulated run
(`rust/client/examples/screenshot.rs`), so the same code in the same browser gives the same pixels.
`docs/assets/screenshots.json` records the version and a hash of the sources that draw them, and the same stamp writes
that version into the README captions. A stable release request refuses screenshots of another version, of changed
sources or under captions naming another version (`python3 scripts/ci/screenshots.py check vX.Y.Z`).

## Toolchain pins

| Pin                              | Owner                                                   | Consumers                                 |
| -------------------------------- | ------------------------------------------------------- | ----------------------------------------- |
| Bun, Python, standalone checkers | `mise.toml` `[tools]`                                   | Local tasks, CI, commit hook              |
| Tool downloads                   | `mise.lock`                                             | Exact artifacts and checksums             |
| Rust                             | `rust/rust-toolchain.toml`, `vars.rust_manifest_sha256` | rustup, the image builder                 |
| mise bootstrap, Buildx           | `vars.mise_version`, `vars.buildx_version`              | The SHA-pinned CI actions                 |
| Chrome for Testing, Firefox      | `vars.browser_*`                                        | CI install and identity check             |
| CodeQL bundle                    | `vars.codeql`                                           | `mise run codeql`                         |
| Container images                 | `vars.image_*`                                          | Image build, verification and publication |
| Rust dependencies                | `rust/Cargo.toml`, `rust/Cargo.lock`                    | Cargo, `cargo deny`                       |
| Browser dependencies             | `client/package.json`, `client/bun.lock`                | Frozen Bun installs                       |
| External GitHub Actions          | Their `uses:` SHA                                       | Immutable workflow refs                   |

To change a pin, edit its owner, then run `mise lock`, `mise run toolchain-sync`, `mise run setup` and `mise run check`.
`mise run toolchain-sync` updates the Dockerfile, mise bootstrap and workflow literals, and `mise run workflow-check`
rejects drift. Python tooling uses the pinned interpreter and the standard library only.
