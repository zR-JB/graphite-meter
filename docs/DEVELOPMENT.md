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
- Python 3.14, Bash, and jq for the pipeline regression suite;
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

| Path | Responsibility |
| --- | --- |
| `api/` | Shared schemas and protocol specifications. |
| `client/` | Svelte browser client, workers, and browser verification. |
| `go/cmd/` | Server and native client entry points. |
| `go/internal/` | Server, transport, measurement, and embedded-asset implementation. |
| `container/` | Container build and deployment examples. |
| `legal/` | Reviewed dependency metadata and generated notices. |
| `scripts/ci/` | CI and release verification. |

## Architecture

The Go server, browser client, and native client share routes and wire contracts, while each
client owns its measurement engine. Throughput is receiver-authoritative; latency populations
remain separate by stage. Presentation and animation never define measurement results.

The browser separates connection preparation, measurement execution, and presentation. Workers
isolate transfer and probe hot paths. The interface uses native SVG/CSS for the gauge and canvas
for the timeline chart, with responsive layout, reduced motion, and bounded local history.

The native client provides the same measurement stages through a terminal interface. Browser
and native results reflect different runtime constraints and should not be treated as identical
benchmark targets.

The server owns protocol listeners, authentication, admission limits, and connection lifetimes.
Resource limits bound concurrency without throttling the measured data streams.

See [Measurement definitions](MEASUREMENTS.md) for units, timing, and missing-data behavior,
[Deployment and configuration](DEPLOYMENT.md) for operational settings, and the
[wire specification](../api/wire.md) for protocol details.

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

For a focused browser iteration, build once and select the affected files:

```sh
cd client
bun run build:browser
bun test browser/gauge-lifecycle.test.ts browser/chart-layout.test.ts --no-orphans --timeout 30000
```

Rebuild after changing application source. Set `GM_WEBVIEW_DEBUG=1` for browser process and
request diagnostics; failures are saved under `client/test-results/webview`. Use
`GM_WEBVIEW_ARTIFACTS` to retain them in a different directory.

`just client-e2e` builds and embeds the current production application, then runs it with a
real server and ephemeral TLS certificate. The transport matrix verifies both transfer directions
over clear and TLS HTTP/1.1, HTTP/2, HTTP/3, and WebTransport streams and datagrams. Protocol probes
check which protocol the listener actually observed. The stubbed browser suite remains the fast
way to reproduce layout, keyboard, history, and failure states deterministically.

### Worktree verification

Run `just setup` inside each new worktree. Keep its `node_modules`, generated bundles, embedded
assets, and browser artifacts local to that checkout; Bun's package cache and Go's build cache
can be shared. Do not symlink another worktree's `node_modules`: its installed graph can disagree
with the checked-out lockfile. The pre-commit hook also installs from the exact staged lockfile
inside its disposable worktree before running client or legal checks.

Give simultaneous E2E runs separate port ranges:

```sh
GM_E2E_PORT_BASE=17256 just client-e2e
```

The base defaults to 7256 and reserves four consecutive port numbers, including TCP and UDP on
the last port. Readiness requires the identity of the server started by that fixture; an unrelated
server on the same port cannot satisfy it. Manual development servers can use `GM_H1_ADDR`.

In restricted environments, `JUST_TEMPDIR=/tmp` selects a writable directory for Just's scripts.
`GOCACHE` selects the Go build cache; the recipes already default to a temporary cache. Chromium
also needs writable `XDG_CONFIG_HOME` and `XDG_CACHE_HOME` directories on Linux. Set
`BUN_CHROME_PATH` to the browser executable when discovery picks the wrong installation. Real
browser/transport checks require permission to launch browser processes and bind loopback sockets.
If Bun parallel workers stall before printing test results in a sandbox, use a bounded invocation
and run the normal parallel gate in an environment that permits those subprocesses.

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


### Python build tooling

Use Python 3.14 for repository tooling; no pip or Python packages are required.
`.python-version` selects the minor release locally and in CI; patch updates float.
`just setup` prepares the version-pinned standalone `ty` binary. `just python-check`
checks all scripts and their tests; `just pipeline-test`
combines that check with control-plane and legal regression tests. Legal review,
inventory rendering and source archives run in Python. Only dependency closure
discovery invokes Go metadata commands and the Vite build; no legal helper is
compiled. See `legal/README.md` for the unchanged public legal commands.
