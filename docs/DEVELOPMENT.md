# Development

How to build, run, and test everything from source. For a normal deployment of the published
image, see the [README quick start](../README.md#quick-start).

## Prerequisites

- **Go 1.27.0** — use the exact release declared by `go/go.mod`.
- **Bun 1.4.0** — use the exact version declared by `.bun-version`.
- **Python 3** — `python3` must be available for the typed CI policy, release verification, and Git hook.
- **[`just`](https://github.com/casey/just)** — use the version declared by `.just-version`.
- **Docker or Podman and Chrome/Chromium** — required for the complete `just ci` workflow. Set
  `BUN_CHROME_PATH` when Bun cannot discover the executable.

## Clone

```sh
git clone https://github.com/zR-JB/graphite-meter.git
cd graphite-meter
```

## Quick start (development)

```sh
just dev          # build the client (dev profile), embed it into the Go server, run it on :7246
just prod         # same local workflow with the production client profile
```

Open `http://localhost:7246`. `just dev` and `just prod` are the two "do everything" entrypoints
and are deliberately symmetric: each builds the Svelte client in its own profile, stages it into
`go/internal/static/dist` (picked up by `//go:embed`), and `go run`s the server — only the profile
differs. `just dev` includes the dummy runner (`?engine=dummy`) and the Developer settings tab by
default; `just prod` builds a real-only, dev-tooling-stripped, version-stamped run instead (see
below).

Both commands are local source runs. Use `just dev` while developing UI behavior, `just prod` to
exercise the real production browser bundle, and `just ci` to run the local validation used by CI.
For an installed deployment, use the published container commands in the
[README](../README.md#quick-start) or build a persistent binary with `just server-build-prod`.

## Setup

```sh
just setup
just doctor
```

`setup` performs the frozen Bun install, downloads Go modules, prepares the
pinned development tools, and enables `.githooks`. Network access is expected
for setup. `doctor` is mutation-free and reports toolchain drift.

## Develop

```sh
just dev
just prod
just client-watch
```

## Validate

```sh
just check
just ci
```

`check` is the fast deterministic developer gate after setup. `ci` is the
complete local CI-equivalent workflow: race and coverage tests, the Chromium
WebView suite, real E2E, TUI cross-builds, release checks, container smoke, and the
networked security scan. `just ci` therefore requires Docker or Podman,
installed Chrome/Chromium, and network access for vulnerability advisories.
CI prepares only the executable analysis tool needed by each job; it does not
promote `.tools` binaries from pull-request runs into a shared cache.

The authoritative unit command is `bun test src --parallel
--timings=test-timings.json`. Bun 1.4 process isolation removed the old ordering
dependency and reduced the 66-file suite from roughly 15.4 seconds serial to 6.4
seconds parallel on the reference machine. `just client-test-changed` is a local
iteration aid only; deterministic and pre-commit gates always execute the full
suite. Refresh scheduling metadata deliberately with `just client-test-timings`.

Go's test runner already schedules independent packages concurrently. The two
slowest packages additionally opt isolated, listener-owning integration tests
into `t.Parallel()`, reducing their combined wall time from about 59 seconds to
38 seconds on the reference machine. Keep tests that mutate process environment
or package globals sequential, and validate any new parallel candidate with
`go test -race -shuffle=on` rather than adding blanket concurrency or retries.

`just client-browser` runs the Chromium-only Bun.WebView suite serially on the
main thread. Browser, E2E, and benchmark commands use Bun's `--no-orphans`
cleanup guard in addition to explicit WebView, server, and subprocess teardown.
Each page closes in a `finally` block, and a process-exit guard stops the shared
ephemeral server and browser. On failure, screenshots plus URL, console/error,
and compact DOM diagnostics are written under `client/test-results/webview`;
failed CI jobs upload that directory.

The authoritative command reference is always:

```sh
just --list
```

## Release operations

Release publication is request-driven and all write-capable consumers execute
from trusted default-branch workflow definitions. Do not create or push release
tags manually: tag pushes are intentionally not release triggers.

For a stable release, first wait for the exact `main` commit's `Gate` and
`CodeQL` checks. Then open **Actions → Request stable release**, select `main`,
enter the stable tag (`vMAJOR.MINOR.PATCH`), and choose `validate`. The
zero-write request workflow hands a small request artifact to the trusted
`Release` `workflow_run` consumer. Validation builds and verifies the exact
native artifacts and OCI archive but stops before environment approval or any
registry/GitHub Release write.

After a validation run succeeds, start a fresh **Request stable release** run
from `main` with the same tag and `publish`. The trusted consumer rebinds the
request to exact current `main`, Gate, and CodeQL; builds and verifies the exact
payload; waits for the `ghcr-release` environment approval; rechecks mutable
trust; then publishes the immutable version image, the GitHub Release/tag, and
finally the monotonic series/`latest` aliases. If `main` advances or the guarded
CI/CodeQL state changes before publication, start a fresh request rather than
forcing the old transaction through.

For a PR prerelease, wait for the PR's exact head to pass both `Gate` and
PR-bound `CodeQL`, and ensure the PR contains current `main`. Open **Actions →
Request PR prerelease**, select `main`, and provide the PR number, exact
40-character head SHA, and a strict `vMAJOR.MINOR.PATCH-{alpha,beta,rc}.N` tag.
The request job has no write permission and never checks PR files out on the
runner; trusted tooling gives the validated commit directly to BuildKit as a
public remote Git context. The trusted default-branch publisher later treats the
OCI handoff as untrusted data, revalidates PR/Gate/CodeQL provenance, waits for
the protected environment, rechecks again, and publishes only the exact
prerelease image tag. It creates no Git tag, GitHub Release, series alias, or
`latest`.

Do not use a PAT, manually log into GHCR, hand-upload release artifacts, or
construct a GitHub Release by hand. The `ghcr-release` environment is an
approval/policy boundary, not a credential store. Repository **Actions → General →
Artifact and log retention** must permit at least 35 days: trusted publication
handoffs use that lifetime so a protected-environment approval can wait without
losing the already-verified payload. For a transient failure after a trusted
transaction has started, prefer GitHub's **Re-run failed jobs** so the run retains
its original immutable source; if a trust recheck refuses the rerun, start a
fresh request.


## Exceptional maintenance

```sh
just legal-generate
just legal-review template
just release-check
```

Use `legal-generate` only after an approved legal/dependency change. The
review command is for new or changed legal facts; `release-check` validates
distributable artifacts without publishing them. Private underscore-prefixed
recipes are implementation details and are not part of the developer API.

## Browser ping-cadence capture

Use a loopback or quiet LAN target to verify application-level `/ws/ping` pacing independently of
worker reporting and chart updates:

1. Run `just dev`, open the browser client, disable adaptive early finish, and set the latency
   duration to exactly 4000ms. Leave warmup enabled so the capture also shows that the same socket
   and cadence continue across the warmup-to-measurement boundary.
2. In browser developer tools, open Network, select the `/ws/ping` WebSocket, and view its Messages
   or Frames. Clear the frame list at the latency measurement boundary, or count only outbound
   `PING,<id>` frames whose timestamps fall inside that four-second measured window. Exclude `HI`
   and the preceding warmup PINGs.
3. Repeat with Unloaded ping cadence set to Fast (80ms), Medium (250ms), and Slow (600ms).
   Expect roughly 50, 16, and 6–7 outbound PING frames respectively. A one-timer-boundary difference
   is normal when a frame lands exactly on a window edge.
4. Repeat the fixed-window count during download or upload with loaded latency enabled, changing
   only Loaded ping cadence. Expect the same three counts. This validates that the transfer-stage
   warmup and measurement use the loaded selector while the unloaded selector remains independent.

Immediate loopback PONGs must not increase the fixed-cadence counts. Reply-driven should send the
next PING immediately after each PONG; if a reply is missing, its RTT-derived backup may open up to
four unloaded requests or two loaded requests before pausing. Pending-window saturation or RTT longer
than the cadence may reduce them, but recovery must not produce early sends or catch-up bursts.
Pre-test probes and the idle connectivity keepalive are intentionally outside both controls.

`just prod` and `just client-build-prod` accept the `GM_CLIENT_*` knobs inline to produce a
configurable build instead of the real-only default, e.g.:

```sh
just prod allow_dummy=1 dev_tools=1
```

## Build-time feature flags

The dummy engine and the entire Developer tab are compiled out of production builds, not just
hidden — `client/vite.config.ts` reads `GM_CLIENT_*` env vars and injects them via Vite's `define`
as raw literal tokens (not strings), which is what lets Rollup constant-fold the relevant
`if (...)` branches and tree-shake the dead code entirely rather than leaving it reachable behind
a runtime flag.

Vite remains the browser bundler because Svelte's supported non-SvelteKit toolchain is
`@sveltejs/vite-plugin-svelte`, which handles `.svelte` files and Svelte-aware diagnostics. Bun is
still the package manager, script runner, test runner, and runtime used by the client toolchain;
the small custom inline HTML minification step in `vite.config.ts` also uses `Bun.build`, so there
is no direct esbuild dependency in this package. The `check` script is deliberately separate from
the bundler: Bun's bundler transpiles TypeScript, but semantic type checking is handled by
`svelte-check` and TypeScript's native checker. Full removal of TypeScript 6 is not yet possible:
Svelte and Vite integrations still import TypeScript's JavaScript compiler API, which the native
TypeScript 7 package does not provide as a drop-in replacement. Therefore `typescript@^6.0.3`
remains the compiler-API dependency, while `@typescript/native` aliases exactly TypeScript 7.0.2
and its executable checks non-Svelte config/build scripts. Bun runs `svelte-check --tsgo`, which
transforms Svelte components and delegates their TypeScript diagnostics to the TS7 native CLI.
TS6 remains installed only for the Svelte/Vite JavaScript compiler API. The experimental TS7 API
mode is not used because its synchronous RPC wrapper currently accesses Node-private stream
handles that Bun does not expose. There is no environment-dependent TS6 diagnostic fallback.
The two checks run concurrently, while bundling starts only after both succeed. `bun run build` is
the validated local/default build (`check` plus `build:bundle`).

The Go module targets 1.27.0. That release supplies the faster small-object allocator and the
v2-backed `encoding/json` implementation without application-level switches. The source also uses
the release-aware `go fix` modernizations where they reduce allocation or concurrency ceremony,
including `strings.SplitSeq`, `sync.WaitGroup.Go`, typed `errors.AsType`, and built-in `min`/`max`.
Experimental SIMD and new cryptographic or UUID APIs are intentionally not enabled because the
current server has no workload or protocol requirement that benefits from them.
Staticcheck is pinned to `2026.2.1`, which supports Go 1.27 export data and language features.
Repository tool installation runs from inside `go/` so analyzers are compiled with the same exact
Go toolchain as the application.

| Variable                | Values           | `just dev`/`client-build-dev` default | `just prod`/`client-build-prod` default | What it does                                                                                                                                                                                                                                                              |
| ----------------------- | ---------------- | ------------------------------------- | --------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `GM_CLIENT_ENGINE`      | `real` / `dummy` | `real`                                | `real`                                  | Default runner when more than one is compiled in.                                                                                                                                                                                                                         |
| `GM_CLIENT_ALLOW_DUMMY` | `0` / `1`        | `1`                                   | `0`                                     | Compile in the dummy runner and the Developer-tab anomaly-injection cards.                                                                                                                                                                                                |
| `GM_CLIENT_DEV_TOOLS`   | `0` / `1`        | `1`                                   | `0`                                     | Compile in the whole Developer settings tab (including debug logging).                                                                                                                                                                                                    |
| `GM_CLIENT_BUILD_PROFILE` | string         | `dev`                                 | `prod`                                  | Build profile shown in the footer and diagnostics. |
| `GM_CLIENT_REVISION`      | string         | `source`                              | git short hash                          | Source revision shown for untagged builds and written to `dist/version.json`. |
| `VERSION`                 | string         | unset                                 | unset unless releasing                  | Validated release version. Untagged builds keep `version.json.version` null. |
| `GM_CLIENT_VALIDATE`    | `0` / `1`        | image build arg only                  | image build arg only                    | `1` runs `bun run build` (type check + bundle) inside the Dockerfile; `0` runs `build:bundle` alone. CI smoke and release image builds pass `0` because the same commit already passed the client check/test job.                                                         |

At runtime, when the dummy runner is compiled in, `?engine=dummy` on the URL (or a previously
persisted choice in `localStorage`) switches to it; this check itself compiles away in a
dummy-stripped build, so it can't be re-enabled by URL trickery in a production build.

## Local TLS and HTTP/3 certificates

Every runtime option — server environment variables, server flags, and the TUI client's flags —
lives in [CONFIGURATION.md](CONFIGURATION.md). This section covers only development certificates.

Use a locally trusted CA for browser testing. A bare self-signed leaf may be
accepted through an HTTPS warning for an ordinary TCP request, but that does
not give an Alt-Svc HTTP/3 connection a usable certificate exception. The leaf
must also cover every name used in the public origins; for the standard local
setup that means `localhost`, `127.0.0.1`, and `::1`.

[`mkcert`](https://github.com/FiloSottile/mkcert) installs a development CA into supported system
and browser trust stores, including Firefox, Chrome, and Chromium. The exact stores depend on the
platform and available NSS tools. Restart browsers after installing the CA.

```sh
mkdir -p .dev-certs
mkcert -install
mkcert -cert-file .dev-certs/localhost.pem \
  -key-file .dev-certs/localhost-key.pem localhost 127.0.0.1 ::1
GM_H1_TLS_ADDR=:7247 GM_H2_ADDR=:7248 GM_H3_ADDR=:7249 \
  GM_TLS_CERT=../.dev-certs/localhost.pem \
  GM_TLS_KEY=../.dev-certs/localhost-key.pem \
  just prod
```

That single command starts all four native listeners on their standard ports. Open the UI on
`http://localhost:7246` or `https://localhost:7247`.

Both page origins expose `WebTransport`, since a browser gates it on a secure context and loopback
counts as one; whether a session then establishes is the H3 certificate question below. Reaching
the same dev server on a LAN address over plain http is the case that has no API at all — the path
cards report that rather than a missing server transport. To exercise the browser's WebTransport
paths off loopback, serve the UI from `https://<host>:7247` with a certificate covering that name.

Browsers can apply additional certificate and root-policy checks to HTTP/3 beyond their normal
HTTPS trust decision. Firefox has a confirmed
[additional protection](https://bugzilla.mozilla.org/show_bug.cgi?id=1985341): by default it
disables HTTP/3 when the certificate chain contains a third-party root, even when that root is
trusted for normal HTTPS. For a local development profile, open `about:config`, set
`network.http.http3.disable_when_third_party_roots_found` to `false`, and restart Firefox. Do not
weaken this setting in a normal browsing profile. Chrome, Chromium, and other browsers perform
similar policy checks; no browser-specific workaround is documented here without a confirmed path.

The H3 bootstrap still needs `7249/tcp`, and QUIC needs `7249/udp`. A successful
`curl --http3-only` or native-client request proves the server and UDP path, but
not browser certificate policy. In the browser, select HTTP/3 and confirm the
application reports verified browser protocol `h3`; Graphite Meter fails the
run instead of silently measuring its TCP bootstrap.

The `.dev-certs/` directory and common certificate/key extensions are ignored,
and CI rejects tracked TLS material. Never copy a private key into another
tracked path. The mkcert CA key (shown by `mkcert -CAROOT`) is especially
sensitive and must never be shared or committed. Use publicly trusted
certificates for deployed servers; mkcert is development-only.

## The throughput benchmark

`client/bench/` drives the production workers against a real server and appends one NDJSON row per
run; `rig.sh` in the same directory puts the server in a network namespace behind a shaped `veth`
pair. Neither is part of CI: they take hours and measure the machine rather than the code. It runs
against a prebuilt static harness and real server owned by the Bun.WebView fixture.

```sh
GM_BENCH_SPKI=<base64 SHA-256 of the dev leaf's SPKI> \
  GM_BENCH_ORIGINS=h1-clear GM_BENCH_REPS=5 \
  just bench-throughput
```

`GM_BENCH_SPKI` pins the development certificate for QUIC, which ordinary HTTPS error bypasses do
not cover. The WebView fixture refuses to run without it. Use the recipe's optional test-name filter
to select a cell; there is no browser-project selector because coverage is Chromium-only.

Every finding — per-transport figures, tuning verdicts, shaped-path results, Firefox's memory
behavior, and the limits of all of it — is in [BENCHMARKS.md](BENCHMARKS.md).

## Building the container image from source

The image is multi-stage (`client` build with `bun` → `server` build with `go` → `scratch`) and
ships a single static binary (no shell, no libc). Base images are fully qualified, so
`podman build` needs no extra config.

```sh
just container-build
podman run -d --name gm --replace -p 7246:7246 graphite-meter:latest
# open http://localhost:7246 ; stop with: podman rm -f gm
```

`container/Dockerfile` stages:

1. **`client`** (`oven/bun:1.4.0`) — installs client deps, builds the Svelte app. Build args
   `GM_CLIENT_ENGINE`/`GM_CLIENT_ALLOW_DUMMY`/`GM_CLIENT_DEV_TOOLS`/`GM_CLIENT_BUILD_PROFILE` default
   to production values (`real`/`0`/`0`/`prod`) and are promoted to env vars so `bun run build`'s
   `process.env` (read by `vite.config.ts`) sees them.
2. **`server`** (`golang:1.27.0`) — `go mod download`, copies `go/` and `api/` (the schema
   conformance test references `api/` by relative path), embeds the client build from stage 1,
   builds a `CGO_ENABLED=0`, stripped, trimmed, ldflags-versioned static binary.
3. **final** (`scratch`) — just the binary. It exposes 7246/tcp, 7247/tcp, 7248/tcp, and 7249/tcp+udp.
   No entrypoint shell is needed —
   config is read natively from env/flags.

To bake a configurable (non-prod-default) image, pass `--build-arg` for any client knob:

```sh
podman build -f container/Dockerfile -t graphite-meter:dev \
  --build-arg GM_CLIENT_ALLOW_DUMMY=1 --build-arg GM_CLIENT_DEV_TOOLS=1 .
```

The direct Dockerfile and source Compose defaults stamp `0.1.0` into both the server and
client. `just container-build` and the source-build Quadlet instead derive the server identity
from the checkout revision; they leave `CLIENT_VERSION` empty for an untagged client and set
`GM_CLIENT_REVISION` to that revision. Release and prerelease image builds pass the validated
release version to both `VERSION` and `CLIENT_VERSION`.

`container/docker-compose.build.yml` wraps the same build (context = repo root, `dockerfile:
container/Dockerfile`) with the server env vars pre-wired, a complete commented native TLS
listener example, and the client build knobs. The build-from-source Quadlet variant
(`graphite-meter.build` + `graphite-meter-source.container`) is documented in
[`container/quadlet/README.md`](../container/quadlet/README.md).

> Note: rootless Podman user-mode containers may use pasta for networking, which can significantly
> slow down throughput. To avoid this overhead, enable host networking
> (`Network=host` in the quadlet unit, `network_mode: host` in compose).
