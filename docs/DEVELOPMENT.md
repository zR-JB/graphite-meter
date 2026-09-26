# Development

A Go monorepo with an embedded Svelte browser client and a native Bubble Tea client. The Go module owns the server,
wire implementation, native client, generated legal data and the embedded browser bundle.

[Project overview](../README.md) · [Deployment](DEPLOYMENT.md) · [Measurement definitions](MEASUREMENTS.md)

## Prerequisites

Install [mise](https://mise.jdx.dev/installing-mise.html) at the version in `mise.toml` (`vars.mise_version`); it
provides the pinned Go, Bun, Python and checkers. You also need Git, Bash and jq; browser tests need the pinned
Chrome for Testing, container checks Docker or Podman. On Windows, put Git for Windows' `bash.exe` and `sh.exe` on
`PATH` (task execution there is unverified).

```sh
git clone https://github.com/zR-JB/graphite-meter.git
cd graphite-meter
mise run setup      # locked tools and dependencies, the Git hook, then `mise run doctor`
mise run dev        # development build on http://localhost:7246
```

mise trusts the project configuration automatically; in paranoid mode run
[`mise trust`](https://mise.jdx.dev/cli/trust.html) once. Use `mise run <task>` in scripts (`mise doctor` is mise's
own command) and `mise exec -- <cmd>` for tools. `mise tasks` lists every task with its description.

Run `mise run setup` in each new worktree and keep its `node_modules`, bundles and browser artifacts local; do not
symlink another worktree's `node_modules`. Bun's package cache and Go's build cache can be shared.

## Repository layout

| Path | Responsibility |
| --- | --- |
| `api/` | Shared schemas and protocol specifications. |
| `client/` | Svelte browser client, workers, unit tests and the E2E runner. |
| `go/cmd/` | Server and native client entry points. |
| `go/internal/` | Server, transport, measurement, native client and embedded assets. |
| `container/` | Container build and deployment examples. |
| `legal/` | Reviewed dependency metadata and generated notices ([legal pipeline](../legal/README.md)). |
| `scripts/ci/` | CI and release control plane ([CI and release](../scripts/ci/README.md)). |

## Architecture

The server and both clients share routes and [wire contracts](../api/discovery.md); each client owns its measurement
engine. Throughput is receiver-authoritative, latency populations stay separate per server and stage, and
presentation never defines a result. The server owns listeners, authentication, admission limits and connection
lifetimes; limits bound concurrency without throttling measured traffic.

In both clients one coordinator runs a selection of one to four servers on one stage schedule. It owns preparation,
warmup, boundaries, membership and cancellation; each participant owns its connections, credentials, upload IDs and
receiver observations. Live results and history consume the same summaries ([servers](SERVERS.md)).

The browser separates connection preparation, measurement and presentation; workers own the transfer and probe hot
paths, the gauge is SVG/CSS and the timeline a canvas. In the native client a controller owns preparation, sign-in
polling and cancellation, and Bubble Tea owns input and rendering. Browser and native results reflect different
runtimes and are not identical benchmark targets.

## Commands

```sh
mise run dev                # development browser build + server
mise run prod               # production browser build + server
mise run client-watch       # standalone Vite dev server
cd go && go run ./cmd/graphite-meter-client  # native client
mise run check              # deterministic gate; the commit hook runs the parts staged paths touch
mise run ci                 # everything CI runs, job by job
```

| Build | Command |
| --- | --- |
| Production server | `mise run server-build-prod` (release stamp: `VERSION=0.9.0 mise run server-build-prod`) |
| Native client | `mise run goclient-build` (all release archives: `mise run release-artifacts`) |

Untagged builds identify as `GM_CLIENT_REVISION` (default: the short Git revision); `GM_CLIENT_BUILD_PROFILE` is
`dev` or `prod`; release automation sets `VERSION` for the server and both clients.

## Tests

| Layer | Command | Covers |
| --- | --- | --- |
| Go unit and integration | `mise run server-test` (race + coverage: `server-race`) | Server, wire, auth, native client, TUI. |
| Browser unit | `mise run client-ci` | Types, format, measurement math, codecs, state. |
| Real-server E2E | `mise run e2e` | Pinned Chromium against a local fleet of real servers. |

`mise run e2e` builds the production client and server, then `client/scripts/e2e.ts` starts five servers with an
ephemeral certificate: a home server with a catalogue, three public peers (native TLS, plain, second catalogue) and
a password-protected peer. Tests seed settings through `localStorage`, spawn their own server when they stop one,
and drive Chrome through `client/e2e/webview.ts`. Servers listen on `127.0.0.1` at TCP+UDP ports drawn from a
per-process slice of 20000–31999; the harness uses an OS-assigned port.

Failures keep a screenshot, console, page errors and DOM under `client/test-results/webview`, and server logs under
`client/test-results/servers`. Rerun a subset against the built binary, and set `GM_WEBVIEW_DEBUG=1` for Chrome
output:

```sh
cd client && bun run scripts/e2e.ts bun test ./e2e --no-orphans --timeout=60000 -t "History"
```

In a restricted sandbox set writable `GOCACHE`, `XDG_CONFIG_HOME` and `XDG_CACHE_HOME`, point `BUN_CHROME_PATH` at
the pinned browser, and allow browser processes and loopback sockets. If Bun's parallel workers stall there, use
`bun test src --parallel=1` locally and rely on CI for the parallel gate.

`mise run codeql` repeats the CodeQL scan offline with the [bundle](https://github.com/github/codeql-action/releases)
release in `vars.codeql`, installed at `~/.local/share/codeql-bundle` or named by `CODEQL`.

`mise run legal-check` detects drift in legal inventories.
Regenerate legal outputs (`mise run legal-generate`) only after an intentional dependency or artifact change.

## Local TLS and HTTP/3

E2E creates its own certificate. Manual runs and benchmarks use an untracked `.dev-certs/` covering the hostnames
and IPs you use; Chromium HTTP/3 also needs the leaf SPKI pin:

```sh
openssl x509 -in .dev-certs/localhost.pem -pubkey -noout | openssl pkey -pubin -outform der \
  | openssl dgst -sha256 -binary | openssl enc -base64
```

Benchmarks are described in the [benchmark harness](BENCHMARKS.md). Keep reports and raw results outside the
repository.

## Containers and releases

`mise run container-build` and `mise run container-smoke` build and verify the image: a staged build ending in
`scratch` with the server binary (browser embedded), CA roots and license material.

Releases stamp one version across all artifacts, bind publication to the authorized commit and verify the OCI image
before promotion ([CI and release](../scripts/ci/README.md)). Run the local release gate in a disposable checkout,
since generating versioned legal outputs changes tracked files:

```sh
VERSION=0.9.0 mise run legal-generate
mise run release-check 0.9.0
```

Never create or move release tags in ordinary work.

## Toolchain pins

| Pin | Owner | Consumers |
| --- | --- | --- |
| Go, Bun, Python, standalone checkers | `mise.toml` `[tools]` | Local tasks, CI, commit hook |
| Tool downloads | `mise.lock` | Exact artifacts and checksums |
| mise bootstrap | `mise.toml` `vars.mise_version` | The SHA-pinned CI action |
| Chrome for Testing | `mise.toml` `vars.browser_chrome` | CI install and identity check |
| CodeQL bundle | `mise.toml` `vars.codeql` | `mise run codeql` |
| Utility container images | `mise.toml` `vars.image_*` | Secret scan, build and publication |
| Go dependencies | `go/go.mod`, `go/go.sum` | Module resolution and checksums |
| Browser dependencies | `client/package.json`, `client/bun.lock` | Frozen Bun installs |
| External GitHub Actions | Their `uses:` SHA | Immutable workflow refs |

Edit the owning pin, run `mise lock`, `mise run toolchain-sync` (updates the Go directive, Docker builder defaults,
mise bootstrap and workflow literals), `mise run setup` and `mise run check`; `mise run workflow-check` rejects
drift. Python tooling uses the pinned interpreter and the standard library only.
