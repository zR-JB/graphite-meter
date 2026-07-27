# Development

How to build, run, and test everything from source. For a normal deployment of the published
image, see the [README quick start](../README.md#quick-start).

## Prerequisites

- **Go 1.26+** — the measurement server and native TUI client.
- **Bun 1.4+** — the client toolchain.
- **[`just`](https://github.com/casey/just)** — every workflow in this repo goes through the
  `justfile`. Run `just` with no arguments to list every recipe.

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

## Naming

Recipes follow `<component>-<verb>`: `build` produces an artifact and stops there (a `client/dist`
or a Go binary); `run` executes something already built; `test` runs a component's tests. `dev`
and `prod` are the two config profiles a `-dev`/`-prod`-suffixed recipe can target. The two
recipes with no component prefix, `dev` and `prod`, are the top-level entrypoints described above.
Recipes starting with `_` are private helper steps, not meant to be run directly.

## Just command reference

| Recipe                   | What it does                                                                                                                                                     |
| ------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `just` (no args)         | Lists every recipe.                                                                                                                                              |
| `just hooks`             | Points git at `.githooks` (one-time per clone); the pre-commit hook mirrors the CI gates.                                                                        |
| `just dev`               | Builds + embeds the dev-profile client, then `go run`s the server on `:7246`.                                                                                    |
| `just prod`              | Builds + embeds the prod-profile client, then `go run`s the version-stamped server on `:7246`.                                                                   |
| `just client-build-dev`  | `bun install` + `bun run build` -> `client/dist`, dev profile (Vite's own defaults: dummy engine + dev tools included).                                          |
| `just client-build-prod` | Same, but real-only engine and dev tooling stripped by default — accepts the `GM_CLIENT_*` knobs inline (see below).                                             |
| `just client-watch`      | Vite dev server only — hot reload, no Go server, no embedding, no live measurement backend.                                                                      |
| `just client-check`      | Type-checks the client, including Bun test files (`svelte-check`) and the Vite config (`tsc`).                                                                   |
| `just client-test`       | `bun test` — pure-`.ts`-logic unit tests (no component rendering).                                                                                               |
| `just client-ci`         | Runs the fast client CI gates: the schema-type drift check, Prettier check, semantic type check, and Bun tests.                                                  |
| `just client-browser`    | The stubbed browser suite in Chromium and Firefox: accessibility, panel behaviour, presentation. Serves the bundle alone, so nothing here reaches a backend. Slow (~45s), so it is outside `just ci`.               |
| `just client-e2e`        | End to end: boots the measurement server and moves bytes over every real transport from Chromium, through the production lanes. The only check where both ends are real. Needs Go and openssl; the certificate is generated per run. ~6s.               |
| `just client-gen-types`  | Regenerates TypeScript discovery and probe types from both JSON schemas.                                                                                         |
| `just client-check-generated` | Regenerates those types and fails if they drift from `api/*.schema.json`. Hangs off `client-ci` rather than `check-generated`: the generator needs bun and `client/node_modules`, and `ci.yml` calls `check-generated` from the Go job, which sets up neither. |
| `just auth-preview`      | Serves the real server-rendered login page on `127.0.0.1:4174`; `mode=` and `oidc_ready=` pick the variant.                                                      |
| `just server-build-dev`  | Builds + embeds the dev-profile client, then builds `go/graphite-meter` as a persisted, stripped (`-s -w -trimpath`) binary — no version stamp, nothing runs it. |
| `just server-build-prod` | Same, prod profile, plus the ldflags version stamp — the shippable binary for a manual/non-Docker deploy.                                                        |
| `just server-check`      | Checks Go formatting, `go vet ./...`, and `go vet -tags stress ./internal/server/` so the saturation harness cannot rot outside the gate.                        |
| `just server-test`       | `go test -race -shuffle=on ./...` — includes the `/preflight` schema-conformance test — then fails if total statement coverage is below the **75% floor**.       |
| `just check-generated`   | Regenerates the embedded auth assets and fails if `go/internal/auth/assets_generated.go` drifts from source. Called by both the pre-commit hook and `ci.yml`. Covers the auth assets only; the schema-derived client types are gated by `client-check-generated`. |
| `just go-lint`           | Pinned `staticcheck` and `govulncheck` over the Go module. `ci.yml` calls this exact recipe; the pre-commit hook is the one gate that skips it.                  |
| `just ci`                | The fast local gate, in order: `check-generated`, `client-ci`, `server-check`, `go-lint`, `server-test`. Same recipes `ci.yml` runs.                             |
| `just ci-full`           | `ci` plus `client-browser` and `client-e2e`. The Docker smoke job and the cross-build matrix stay CI-only (they need a container runtime or other toolchains).                        |
| `just goclient-build`    | Builds only `go/graphite-meter-client` — does not touch the Svelte client.                                                                                       |
| `just goclient-run`      | `go run`s the native TUI client against a running server.                                                                                                        |
| `just container-build`   | `docker build -f container/Dockerfile -t graphite-meter:latest .`                                                                                                |
| `just stress`            | Server saturation envelope: observer RTT under growing loader concurrency. Measurement only, never in CI.                                                        |
| `just bench-wire`        | Both halves of the ping-bus encoding evidence, Go and TypeScript. Excluded from CI.                                                                              |
| `just bench-throughput`  | Browser throughput matrix against a real server; takes an optional Playwright `-g` filter and an optional project, e.g. `just bench-throughput 'h1-clear/down/lanes=2' chromium`. Hours long, never in CI — see [BENCHMARKS.md](BENCHMARKS.md). |

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
just prod allow_dummy=1 dev_tools=1 label=0.2.0
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
`svelte-check`/`tsc`. `bun run build` is the validated local/default build (`check` plus
`build:bundle`).

| Variable                | Values           | `just dev`/`client-build-dev` default | `just prod`/`client-build-prod` default | What it does                                                                                                                                                                                                                                                              |
| ----------------------- | ---------------- | ------------------------------------- | --------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `GM_CLIENT_ENGINE`      | `real` / `dummy` | `real`                                | `real`                                  | Default runner when more than one is compiled in.                                                                                                                                                                                                                         |
| `GM_CLIENT_ALLOW_DUMMY` | `0` / `1`        | `1`                                   | `0`                                     | Compile in the dummy runner and the Developer-tab anomaly-injection cards.                                                                                                                                                                                                |
| `GM_CLIENT_DEV_TOOLS`   | `0` / `1`        | `1`                                   | `0`                                     | Compile in the whole Developer settings tab (including debug logging).                                                                                                                                                                                                    |
| `GM_CLIENT_BUILD_LABEL` | string           | `dev`                                 | git short hash                          | Text shown after `build` in the status bar. Also the label half of the client version `<package.json semver>+<label>`, which is shown in the Endpoint info drawer, written to `dist/version.json`, and sent to the server on preflight as `?client=web&client_version=…`. |
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
against an ordinary dev server, which `playwright.bench.config.ts` starts.

```sh
cd client
GM_BENCH_SPKI=<base64 SHA-256 of the dev leaf's SPKI> \
  GM_BENCH_ORIGINS=h1-clear GM_BENCH_REPS=5 \
  bunx playwright test -c playwright.bench.config.ts --project=chromium
```

Always pass `--project`. Without it every browser project runs, which is how a previous session
exhausted a machine's memory. `GM_BENCH_SPKI` pins the development certificate for QUIC, which
`ignoreHTTPSErrors` does not cover; the config's own comment gives the `openssl` pipeline that
derives it, and the `chromium` project is skipped rather than run against a guessed pin when it is
unset. `GM_BENCH_FIREFOX` behaves the same way for the `firefox-stock` project.

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

1. **`client`** (`oven/bun:canary`) — installs client deps, builds the Svelte app. Build args
   `GM_CLIENT_ENGINE`/`GM_CLIENT_ALLOW_DUMMY`/`GM_CLIENT_DEV_TOOLS`/`GM_CLIENT_BUILD_LABEL` default
   to production values (`real`/`0`/`0`/`prod`) and are promoted to env vars so `bun run build`'s
   `process.env` (read by `vite.config.ts`) sees them.
2. **`server`** (`golang:1.26`) — `go mod download`, copies `go/` and `api/` (the schema
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

`container/docker-compose.build.yml` wraps the same build (context = repo root, `dockerfile:
container/Dockerfile`) with the server env vars pre-wired, a complete commented native TLS
listener example, and the client build knobs. The build-from-source Quadlet variant
(`graphite-meter.build` + `graphite-meter-source.container`) is documented in
[`container/quadlet/README.md`](../container/quadlet/README.md).

> Note: rootless Podman user-mode containers may use pasta for networking, which can significantly
> slow down throughput. To avoid this overhead, enable host networking
> (`Network=host` in the quadlet unit, `network_mode: host` in compose).
