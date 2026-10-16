# Development

A Rust workspace for the server and the terminal client, and a TypeScript browser client the server embeds.

[Project overview](../README.md) · [Deployment](DEPLOYMENT.md) · [Measurement definitions](MEASUREMENTS.md)

## Prerequisites

Install [mise](https://mise.jdx.dev/installing-mise.html) at the version in `mise.toml` (`vars.mise_version`); it
provides the pinned Bun, Python and checkers, and rustup installs the Rust release in `rust/rust-toolchain.toml`. You
also need Git, Bash, jq and OpenSSL; browser tests need the pinned Chrome for Testing or Firefox, container checks
Docker or Podman.

```sh
git clone https://github.com/zR-JB/graphite-meter.git
cd graphite-meter
mise run setup      # locked tools and dependencies, the Git hook, then `mise run doctor`
mise run dev        # development build on http://localhost:7246; arguments after -- go to the server
mise run tui        # terminal client; it asks for the server, or add `-- --url <url>`
```

Use `mise run <task>` in scripts (`mise doctor` is mise's own command) and `mise exec -- <cmd>` for tools.
`mise tasks` lists every task with its description. Run `mise run setup` in each new worktree and keep its
`node_modules`, bundles and browser artifacts local.

## Repository layout

| Path         | Responsibility                                                                                 |
| ------------ | ---------------------------------------------------------------------------------------------- |
| `api/`       | Shared contracts, test vectors and protocol specifications.                                    |
| `client/`    | The browser client in plain TypeScript, its workers, unit tests and browser suite.             |
| `rust/`      | The server, the terminal client and their shared crates ([Rust workspace](../rust/README.md)). |
| `container/` | Container build and deployment examples.                                                       |
| `legal/`     | Reviewed dependency metadata ([legal pipeline](../legal/README.md)).                           |
| `scripts/`   | Builds, packaging, CI and release control plane ([CI and release](../scripts/ci/README.md)).   |

## Architecture

The server and both clients share routes and [wire contracts](../api/discovery.md); each client owns its measurement
engine. Throughput is receiver-authoritative, latency populations stay separate per server and stage, and
presentation never defines a result. The server owns listeners, authentication, admission limits and connection
lifetimes; limits bound concurrency without throttling measured traffic.

In both clients one coordinator runs a selection of one to four servers on one stage schedule. It owns preparation,
warmup, boundaries, membership and cancellation; each participant owns its connections, credentials, upload IDs and
receiver observations. Live results and history consume the same summaries ([servers](SERVERS.md)).

The browser separates connection preparation, measurement and presentation; workers own the transfer and probe hot
paths. Browser and native results reflect different runtimes and are not identical benchmark targets
([client differences](MEASUREMENTS.md#client-differences)).

## Commands

```sh
mise run server-build   # rust/target/release/graphite-meter-server; VERSION=1.2.3 stamps a release
mise run tui-build      # rust/target/release/graphite-meter-client
mise run check          # the developer gate; the commit hook runs the parts staged paths touch
mise run ci             # the gates CI runs, including the target checks and the browser suite
```

`dev`, `tui` and `e2e-server` use the `ci` profile (no LTO, 16 codegen units), the build tasks the release profile (fat
LTO, one codegen unit). Local builds embed development notices, so their executables say
`UNREVIEWED DEVELOPMENT BUILD` and are not distributable. Untagged builds identify as `GM_CLIENT_REVISION` (default:
the short Git revision).

## Tests

| Layer           | Command               | Covers                                                       |
| --------------- | --------------------- | ------------------------------------------------------------ |
| Rust            | `mise run rust-check` | Formatting, dependency policy, lints, unit and socket tests. |
| Browser unit    | `mise run client-ci`  | Types, format, measurement math, codecs, state.              |
| Real-server E2E | `mise run e2e`        | Chrome or Firefox against a local fleet of real servers.     |

`mise run e2e-server` builds the server the suite runs by default; `mise run e2e` takes another as its first argument,
then `client/scripts/e2e.ts` starts five servers with an ephemeral certificate: a home server with a catalog, three
public peers (native TLS, plain, second catalog) and a password-protected peer. Tests drive the browser through
`client/e2e/webview.ts`: Chrome over its DevTools Protocol, or with `GM_E2E_BROWSER=firefox` Firefox over WebDriver
BiDi (`GM_FIREFOX_PATH` names another build). Tests written `test.chrome` need the DevTools Protocol and Firefox
skips them.

Failures keep a screenshot, console, page errors and DOM under the client's `test-results/webview`, and server logs
under its `test-results/servers`. Rerun a subset against a built server:

```sh
cd client && GM_E2E_SERVER_BIN=../rust/target/ci/graphite-meter-server \
  bun run e2e bun test ./e2e --no-orphans --timeout=60000 -t "History"
```

`mise run codeql` repeats the CodeQL scan offline with the [bundle](https://github.com/github/codeql-action/releases)
release in `vars.codeql`, installed at `~/.local/share/codeql-bundle` or named by `CODEQL`.

## Local TLS and HTTP/3

E2E creates its own certificates. Manual runs use an untracked `.dev-certs/` covering the hostnames and IPs you use;
Chromium HTTP/3 also needs the leaf SPKI pin:

```sh
openssl x509 -in .dev-certs/localhost.pem -pubkey -noout | openssl pkey -pubin -outform der \
  | openssl dgst -sha256 -binary | openssl enc -base64
```

## Containers and releases

`mise run container-build` and `mise run container-smoke` build and verify the image on an amd64 host: a staged build
ending in `scratch` with the server binary (browser embedded), CA roots and license material.
`mise run tui-package VERSION` builds the client archives in the same pinned builder.

Releases stamp one version across all artifacts, bind publication to the authorized commit and verify every file
before publishing ([CI and release](../scripts/ci/README.md)). Never create or move release tags in ordinary work.

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

Edit the owning pin, run `mise lock`, `mise run toolchain-sync` (updates Dockerfile, mise bootstrap and workflow
literals), `mise run setup` and `mise run check`; `mise run workflow-check` rejects drift. Python tooling uses the
pinned interpreter and the standard library only.
