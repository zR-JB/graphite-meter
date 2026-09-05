# Development

Graphite Meter is a Go monorepo with an embedded Svelte browser client and a native Bubble Tea
client. The Go module owns the server, wire implementation, native client, generated legal data,
and the static browser bundle used by release builds.

## Prerequisites

Install [mise](https://mise.jdx.dev/installing-mise.html) at the exact version in
`mise.toml` (`vars.mise_version`). Mise prepares the pinned Go, Bun, Python and
standalone checkers. Git, Bash and jq are required; browser suites also need the
pinned Chrome for Testing version, and container checks need Docker or Podman.

Run setup once per checkout:

```sh
git clone https://github.com/zR-JB/graphite-meter.git
cd graphite-meter
mise trust
mise run setup
```

Review the repository configuration before trusting it: tasks execute repository
code. Setup installs locked tools and language dependencies, configures the Git
hook, and runs `mise run doctor` to detect toolchain drift. Shell activation is
optional; `mise run` supplies the environment for every task. Use `mise exec --`
for direct commands with the project toolchain, such as `mise exec -- go version`.

`mise prod`, `mise dev` and `mise check` also work interactively. Scripts use
`mise run <task>` to avoid collisions with built-in commands; in particular,
`mise doctor` is mise's own diagnostic, while `mise run doctor` checks this project.
List project commands with `mise tasks`.

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

The native client controller owns preparation, browser-approval polling, and run cancellation.
Preparation tokens bind delayed commands to their original configuration; replacing one cancels
its network work. User cancellation retains final result events, while replacement and shutdown
abandon the old bounded event stream. Shutdown joins started work. Bubble Tea keeps input,
rendering, event batching, and sequence guards for already queued replies.

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
mise run dev                 # build the development browser client and run the server
mise run prod                # build the production browser profile and run the server
mise run client-watch        # run the standalone Vite development server
mise run goclient-run        # run the native client
mise run check               # deterministic developer gate
mise run ci                  # full local CI and release gate
```

`mise run dev` serves Graphite Meter on <http://localhost:7246>. Development builds include diagnostics
and the optional dummy backend. Production builds are real-only unless explicitly overridden.

### Build identities

Untagged builds use `GM_CLIENT_REVISION`, normally the current short Git revision. Release
automation sets `VERSION` and stamps the same public version into the server, browser client, and
native client.

Build a production server binary:

```sh
mise run server-build-prod
```

Build the native client:

```sh
mise run goclient-build
```

Build a release-profile server with the production browser client embedded:

```sh
VERSION=0.6.0 mise run release-build 0.6.0
```

### Browser build flags

| Environment               | Default                    | Meaning                                                      |
| ------------------------- | -------------------------- | ------------------------------------------------------------ |
| `GM_CLIENT_BUILD_PROFILE` | set by the task          | `dev` or `prod` feature profile.                             |
| `GM_CLIENT_ALLOW_DUMMY`   | `0` for production tasks | Compile the dummy backend and development controls when `1`. |
| `GM_CLIENT_REVISION`      | current short revision     | Source identity for an untagged build.                       |
| `VERSION`                 | empty                      | Public release version.                                      |

The real-only production build tree-shakes the dummy backend from the output. Enabling it is for
development and browser tests, not release publication.

## Validation

### Fast gate

```sh
mise run check
```

This runs toolchain validation, workflow policy, generated checks, formatting, type checks, Bun
unit tests, Go vet and tests, staticcheck, and legal validation.

Useful focused commands:

```sh
mise run client-ci
mise run server-check
mise run server-test
mise run client-browser
mise run client-e2e
mise run tui-cross-build
mise run check-generated
mise run legal-check
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

`mise run client-e2e` builds and embeds the current production application, then runs it with a
real server and ephemeral TLS certificate. The transport matrix verifies both transfer directions
over clear and TLS HTTP/1.1, HTTP/2, HTTP/3, and WebTransport streams and datagrams. Protocol probes
check which protocol the listener actually observed. The stubbed browser suite remains the fast
way to reproduce layout, keyboard, history, and failure states deterministically.

### Worktree verification

Run `mise run setup` inside each new worktree. Keep its `node_modules`, generated bundles, embedded
assets, and browser artifacts local to that checkout; Bun's package cache and Go's build cache
can be shared. Do not symlink another worktree's `node_modules`: its installed graph can disagree
with the checked-out lockfile. The pre-commit hook also installs from the exact staged lockfile
inside its disposable worktree before running client or legal checks.

Give simultaneous E2E runs separate port ranges:

```sh
GM_E2E_PORT_BASE=17256 mise run client-e2e
```

The base defaults to 7256 and reserves four consecutive port numbers, including TCP and UDP on
the last port. Readiness requires the identity of the server started by that fixture; an unrelated
server on the same port cannot satisfy it. Manual development servers can use `GM_H1_ADDR`.

In restricted environments, `JUST_TEMPDIR=/tmp` selects a writable directory for Just's scripts.
Go uses its standard build cache, cached per CI job. In a restricted environment,
set `GOCACHE` to a writable directory if needed. Chromium
also needs writable `XDG_CONFIG_HOME` and `XDG_CACHE_HOME` directories on Linux. Set
`BUN_CHROME_PATH` to the browser executable when discovery picks the wrong installation. Real
browser/transport checks require permission to launch browser processes and bind loopback sockets.
If Bun parallel workers stall before printing test results in a sandbox, use a bounded invocation
and run the normal parallel gate in an environment that permits those subprocesses.

### Full gate

```sh
mise run ci
```

The full gate adds Go race tests and coverage, live vulnerability data, dependency audit, secret
scanning, browser integration and E2E suites, cross-builds, release packaging, and container smoke
validation. Some steps need network access, a container engine, or loopback listeners and may need
to run outside a restricted sandbox.

### Generated files

API client types are generated from the JSON schemas. Authentication assets and legal inventories
are also checked for drift.

```sh
mise run client-gen-types
mise run legal-generate
mise run check-generated
mise run legal-check
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
GM_BENCH_SPKI='BASE64_SPKI_PIN' mise run bench-throughput 'h1-clear/down/lanes=2'
```

Run the complete maintained matrix:

```sh
GM_BENCH_SPKI='BASE64_SPKI_PIN' mise run bench-throughput
```

The complete matrix takes hours. See [Benchmarks](BENCHMARKS.md) before comparing new values with
the historical reference results.

## Container build

Build and verify the source image:

```sh
mise run container-build
mise run container-smoke
```

The image is built in stages and finishes from `scratch` with the server binary, public CA roots,
and required license material. The browser client is embedded in the binary.

## Release validation

Release workflows stamp one version across the server and both clients, bind publication to the
authorized source commit, generate checksums and source offers, and verify the OCI image before
promotion.

Run the local release gate with an explicit candidate version:

```sh
VERSION=0.6.0 mise run release-check 0.6.0
```

Publication remains a workflow-controlled operation. Do not create or move release tags as part of
ordinary documentation or feature work.


### Python build tooling

Use the exact Python patch in `mise.toml` for repository tooling locally and in CI.
No pip or Python packages are required.
`mise run setup` prepares the version-pinned standalone `ty` binary. `mise run python-check`
checks all scripts and their tests; `mise run pipeline-test`
combines that check with control-plane and legal regression tests. Legal review,
inventory rendering and source archives run in Python. Only dependency closure
discovery invokes Go metadata commands and the Vite build; no legal helper is
compiled. See `legal/README.md` for the unchanged public legal commands.


### Toolchain ownership and updates

| Pin | Owner | Consumers |
| --- | --- | --- |
| Go, Bun, Python and standalone checkers | `mise.toml` `[tools]` | Local setup/tasks, CI, staged commit checks |
| Tool downloads | `mise.lock` | Exact resolved artifacts and supported checksums |
| Mise bootstrap | `mise.toml` `vars.mise_version` | Validated literal in the SHA-pinned CI action |
| Chrome for Testing | `mise.toml` `vars.browser_chrome` | CI install and browser identity verification |
| Utility container images | `mise.toml` `vars.image_*` | Secret scan and validated build/publication literals |
| Go dependencies | `go/go.mod` and `go/go.sum` | Native module resolution and checksum verification |
| Browser dependencies | `client/package.json` and `client/bun.lock` | Frozen Bun installs |
| External GitHub Actions | Their `uses:` SHA in YAML | GitHub's workflow loader; immutable refs required |

Edit the owning pin, regenerate `mise.lock` with `mise lock`, then run
`mise run toolchain-sync`, `mise run setup` and `mise run check`. Tool downloads
use locked resolution; Go-installed govulncheck retains Go's module checksum
verification. Dependency updates use their native package commands and lockfiles.

Sync updates unavoidable literals: the Go module's language directive, Docker
builder defaults, mise bootstrap versions, and immutable QEMU/Skopeo workflow
references. No-checkout publication jobs retain trusted literal image references.
`mise run toolchain-check` rejects drift, and `mise run doctor` checks runtimes.
Direct `docker build -f container/Dockerfile .` remains supported.

CI explicitly installs each job's required tools and disables automatic tool
installation during tasks. Verified tools are cached separately from the Go
module/build and Bun package caches. Tasks do not cache test results or skip
correctness gates; build, embed, generation and legal checks retain their required
execution order. Release jobs retain their cache isolation.
