# Development

Graphite Meter is a Go monorepo with an embedded Svelte browser client and a native Bubble Tea
client. The Go module owns the server, wire implementation, native client, generated legal data,
and the static browser bundle used by release builds.

## Prerequisites

Use the versions pinned by the repository:

- Go from `go/go.mod`;
- Bun from `.bun-version`;
- Just from `.just-version`;
- Git;
- Chrome for Testing for browser and benchmark suites;
- Docker or Podman for container validation.

Run the setup once:

```sh
git clone https://github.com/zR-JB/graphite-meter.git
cd graphite-meter
just setup
```

`just setup` installs locked dependencies and repository tools, configures the Git hook, and runs
`just doctor` to detect toolchain drift.

## Repository layout

| Path                           | Responsibility                                                               |
| ------------------------------ | ---------------------------------------------------------------------------- |
| `api/`                         | JSON schemas and the normative message-bus protocol.                         |
| `client/`                      | Svelte 5 browser client, workers, browser harness, and throughput benchmark. |
| `go/cmd/graphite-meter`        | Server entry point and CLI flags.                                            |
| `go/cmd/graphite-meter-client` | Native Bubble Tea client.                                                    |
| `go/internal/config`           | Server configuration and validation.                                         |
| `go/internal/endpoint`         | Measurement, probe, upload progress, and WebTransport handlers.              |
| `go/internal/server`           | Listener ownership, admission, TLS lifecycle, and routing.                   |
| `go/internal/transport`        | HTTP protocol evidence, message channels, and QUIC I/O cancellation.                          |
| `go/internal/static`           | Embedded browser assets and index metadata injection.                        |
| `container/`                   | OCI image, Compose examples, and Quadlet units.                              |
| `legal/`                       | Reviewed dependency metadata and generated notices.                          |
| `scripts/ci/`                  | Workflow policy, release verification, and CI helpers.                       |

## Measurement architecture

The server and both clients share routes and wire contracts but retain separate measurement
engines. The protocol definitions in `api/` are the boundary between them.

HTTP routes receive the request and response writer directly. WebSocket and WebTransport
adapters own connection lifetime and supply message channels or byte readers/writers to shared
measurement operations. Upload adapters pass the authenticated request or CONNECT owner explicitly;
refused lanes are rejected before their bytes are read. The shared upload loop retains receiver-side
chunk accounting, while each adapter owns deadlines, stream closure, and transport-specific responses.

### Authoritative accounting

- Download bytes are counted after the receiving browser worker or native client consumes them.
- Upload bytes and elapsed time come from the receiving server and are returned through the upload
  progress stream or the WebTransport session.
- Client upload completion and animated values are presentation hints only. They do not enter the
  final result reducer.
- Each final rate uses bytes and elapsed time from one clock domain.
- Wire-rate estimates are presentation data derived after measurement. They never change the raw
  application rate.

### Application latency

The latency worker owns ping pacing and observation timestamps outside the browser main thread.
WebSocket pings provide RTT through a reliable TCP stream. WebTransport pings use unreliable QUIC
datagrams, so missing application datagrams remain observable instead of being repaired by TCP.

RTT is measured by the client clock from send to response receipt. It includes the complete
application path, browser or native scheduling, server work, and transport behavior.

Latency continues during transfer stages when loaded latency is enabled. Idle and loaded samples
are summarized separately.

### Measurement stages

- Latency establishes the idle RTT profile.
- Download consumes server payload on one or more lanes.
- Upload sends bounded incompressible payloads while the server reports received progress.
- Bidirectional runs the existing upload and download mechanisms concurrently.
- Loaded latency runs the selected latency channel during transfer stages.

The client probes and commits selected paths before the measured timeline begins. Connection
preparation and warmup do not consume the configured measurement window.

### Browser client

`RunnerCore` owns the stage timeline, monotonic measurement clock, adaptive finish decision,
recovery deadline, and final result reducer. `RealBackend` supplies production samples through a
three-step stage lifecycle: prepare, measure, and end.

Workers isolate the hot paths:

| Worker                      | Responsibility                                                       |
| --------------------------- | -------------------------------------------------------------------- |
| `download-worker.ts`        | Consume and count one fetch download lane.                           |
| `upload-worker.ts`          | Size and submit finite incompressible upload requests.               |
| `upload-progress-worker.ts` | Parse authoritative server upload progress.                          |
| `ping-worker.ts`            | Pace WebSocket or WebTransport pings and retain RTT/loss timestamps. |
| `wt-transfer-worker.ts`     | Own a WebTransport session, streams, datagrams, and finalization.    |

The main-thread scheduler is invalidation-driven, visibility-aware, reduced-motion-aware, and
capped at 30 frames per second. Presentation interpolation never becomes measurement evidence.

History code and IndexedDB are loaded only when History is visited or an enabled run completes.
Only validated completed summaries are stored, capped at 2,000 records. Raw graph points and
aborted runs are not retained.

### Native client

The native client uses the same discovery document, routes, and wire protocol. Its Bubble Tea
model redraws on state changes and uses no independent animation clock. It supports interactive
configuration, repeatable setup through flags, browser-approved authentication grants, and the
same latency, download, upload, bidirectional, and loaded-latency stages.

The browser and native clients are not identical benchmark targets. They have different runtime
constraints, buffer policies, percentile implementations, and browser-only presentation features.

### Server

Endpoints are implemented against a shared session interface and can run through HTTP,
WebSockets, or WebTransport without duplicating measurement logic. Separate native listeners make
HTTP/1.1 clear, HTTP/1.1 TLS, HTTP/2, and HTTP/3 selectable paths.

Admission limits bound active handlers, sessions, and connections. WebTransport sessions consume
part of the global measurement pool rather than extending it. The server intentionally does not
rate-limit measurement bodies because that would alter throughput results.

See [Deployment and configuration](DEPLOYMENT.md) for listener, authentication, proxy, and limit
settings. See [the wire specification](../api/wire.md) for message framing and opcodes.

## Development commands

```sh
just dev                 # build the development browser client and run the server
just prod                # build the production browser profile and run the server
just client-watch        # run the standalone Vite development server
just goclient-run        # run the native client
just check               # deterministic developer gate
just ci                  # full local CI and release gate
```

`just dev` serves Graphite Meter on <http://localhost:7246>. Development builds include diagnostics
and the optional dummy backend. Production builds are real-only unless explicitly overridden.

### Build identities

Untagged builds use `GM_CLIENT_REVISION`, normally the current short Git revision. Release
automation sets `VERSION` and stamps the same public version into the server, browser client, and
native client.

Build a production server binary:

```sh
just server-build-prod
```

Build the native client:

```sh
just goclient-build
```

Build a release-profile server with the production browser client embedded:

```sh
VERSION=0.6.0 just release-build 0.6.0
```

### Browser build flags

| Environment               | Default                    | Meaning                                                      |
| ------------------------- | -------------------------- | ------------------------------------------------------------ |
| `GM_CLIENT_BUILD_PROFILE` | set by the recipe          | `dev` or `prod` feature profile.                             |
| `GM_CLIENT_ALLOW_DUMMY`   | `0` for production recipes | Compile the dummy backend and development controls when `1`. |
| `GM_CLIENT_REVISION`      | current short revision     | Source identity for an untagged build.                       |
| `VERSION`                 | empty                      | Public release version.                                      |

The real-only production build tree-shakes the dummy backend from the output. Enabling it is for
development and browser tests, not release publication.

## Validation

### Fast gate

```sh
just check
```

This runs toolchain validation, workflow policy, generated checks, formatting, type checks, Bun
unit tests, Go vet and tests, staticcheck, and legal validation.

Useful focused commands:

```sh
just client-ci
just server-check
just server-test
just client-browser
just client-e2e
just tui-cross-build
just check-generated
just legal-check
```

The Bun unit suite is designed for parallel execution. On a constrained sandbox, use
`bun test src --parallel=1` only as an environmental workaround and still rely on hosted CI for
the normal parallel gate.

Browser tests use the repository's Bun.WebView harness and pinned Chrome for Testing. Failed runs
retain a screenshot, URL, console log, page errors, and a compact DOM snapshot.

### Full gate

```sh
just ci
```

The full gate adds Go race tests and coverage, live vulnerability data, dependency audit, secret
scanning, browser integration and E2E suites, cross-builds, release packaging, and container smoke
validation. Some steps need network access, a container engine, or loopback listeners and may need
to run outside a restricted sandbox.

### Generated files

API client types are generated from the JSON schemas. Authentication assets and legal inventories
are also checked for drift.

```sh
just client-gen-types
just legal-generate
just check-generated
just legal-check
```

Only regenerate reviewed legal outputs after an intentional dependency or distributed-artifact
change.

## Local TLS and HTTP/3

The E2E suite creates an ephemeral certificate automatically. Manual development and throughput
benchmarks use `.dev-certs` in the repository root. Keep private development keys untracked.

A local certificate must cover the hostnames or IP addresses used by the selected origins.
Chromium HTTP/3 tests also need the leaf certificate SPKI pin. Derive it with:

```sh
openssl x509 -in .dev-certs/localhost.pem -pubkey -noout | openssl pkey -pubin -outform der | openssl dgst -sha256 -binary | openssl enc -base64
```

## Throughput benchmark

The maintained Chromium matrix drives the production transfer workers through Bun.WebView.
`.dev-certs` and `GM_BENCH_SPKI` are required because the benchmark server starts every native
listener.

Run one cell:

```sh
GM_BENCH_SPKI='BASE64_SPKI_PIN' just bench-throughput 'h1-clear/down/lanes=2'
```

Run the complete maintained matrix:

```sh
GM_BENCH_SPKI='BASE64_SPKI_PIN' just bench-throughput
```

The complete matrix takes hours. See [Benchmarks](BENCHMARKS.md) before comparing new values with
the historical reference results.

## Container build

Build and verify the source image:

```sh
just container-build
just container-smoke
```

The image is built in stages and finishes from `scratch` with the server binary, public CA roots,
and required license material. The browser client is embedded in the binary.

## Release validation

Release workflows stamp one version across the server and both clients, bind publication to the
authorized source commit, generate checksums and source offers, and verify the OCI image before
promotion.

Run the local release gate with an explicit candidate version:

```sh
VERSION=0.6.0 just release-check 0.6.0
```

Publication remains a workflow-controlled operation. Do not create or move release tags as part of
ordinary documentation or feature work.
