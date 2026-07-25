# Graphite Meter — monorepo task runner.
# Requires: bun 1.4+, go 1.26+. (https://github.com/casey/just)
#
# Naming: `<component>-<verb>`, where verb is one of:
#   build  — produce an artifact (client/dist, or a go binary) and stop there.
#   run    — execute something that's already runnable (a binary, a dev server).
#   test   — run that component's test suite.
# `dev` and `prod` are the two config profiles; a recipe suffixed `-dev`/`-prod`
# only differs in which profile it targets. The two top-level recipes without a
# component prefix, `dev` and `prod`, are the "do everything" entrypoints: each
# builds the client in its own profile, embeds it into the Go server, and runs
# the server — same shape both ways, only the profile differs. Anything with a
# leading underscore is a private helper step, not meant to be run directly.

# Default shell for Linux/macOS
set shell := ["sh", "-c"]

# Fallback shell used automatically only when running on Windows (Requires PowerShell 7+)
set windows-shell := ["pwsh", "-NoProfile", "-Command"]

# CRITICAL: Automatically exports all just variables as environment variables
# to the underlying shell. This eliminates inline "KEY=VALUE command" breaking on Windows.
set export := true

# --- Production build knobs (override via env or `just prod label=… engine=…`) ---
# These feed the client's Vite `define` (see client/vite.config.ts) and the
# server's version ldflag. `client-build-prod` / `prod` / `server-build-prod`
# all build a real-only, dev-tooling-free bundle by default; flip the knobs to
# produce a configurable multi-engine build instead.
engine      := env("GM_CLIENT_ENGINE", "real")
allow_dummy := env("GM_CLIENT_ALLOW_DUMMY", "0")
dev_tools   := env("GM_CLIENT_DEV_TOOLS", "0")

# Detect Git commit hash cross-platform, suppressing errors cleanly depending on OS.
# `label` is the status-bar label and the label half of the client version
# <semver>+<label> (Endpoint info drawer, dist/version.json, preflight query).
# `version` feeds BOTH the client build's VERSION env (see client-build-prod
# below) and the Go ldflag, identically, so a `just` build always agrees with
# itself. Three fallback tiers, low to high precedence:
#   1. no VERSION, no `just` at all (raw `go build`/`bun run build`) — the
#      compiled-in sentinels: go/internal/config.EngineVersion's "0.0.0-dev"
#      and client/package.json's "0.0.0". Never bumped by hand.
#   2. `just` run with no VERSION set — falls back to the git short hash
#      (this line), a real build identity, distinct from tier 1's sentinel.
#   3. `VERSION=x.y.z just ...` (or CI/release.yml, which always sets it from
#      the git tag) — the authoritative release version.
label       := env("GM_CLIENT_BUILD_LABEL", if os() == "windows" { `cmd.exe /c "git rev-parse --short HEAD 2>nul || echo prod"` } else { `git rev-parse --short HEAD 2>/dev/null || echo prod` })
version     := env("VERSION", label)

# Set OS-specific path for the Go build cache to remain fully cross-platform
GOCACHE     := env("GOCACHE", if os() == "windows" { env("TEMP") / "graphite-meter-go-build" } else { "/tmp/graphite-meter-go-build" })
CGO_ENABLED := "0"

# List available recipes
default:
    @just --list

# Point git at the tracked hooks (one-time per clone); pre-commit mirrors the CI gates
hooks:
    git config core.hooksPath .githooks

# Run the same fast client gates used by CI.
client-ci:
    cd client && bun run format:check
    cd client && bun run check
    cd client && bun test

# --- Client (Svelte/Vite, bun) ---

# Build the client in dev profile (Vite's own defaults: real engine, dummy + dev tools included)
client-build-dev:
    cd client && bun install && bun run build

# Override via GM_CLIENT_* env or `client-build-prod engine=… allow_dummy=… dev_tools=… label=…`.
# Build the client in prod profile: real-only engine, dev tooling stripped by default.
client-build-prod:
    cd client && bun install
    bun -e "process.env.GM_CLIENT_ENGINE='{{engine}}'; process.env.GM_CLIENT_ALLOW_DUMMY='{{allow_dummy}}'; process.env.GM_CLIENT_DEV_TOOLS='{{dev_tools}}'; process.env.GM_CLIENT_BUILD_LABEL='{{label}}'; process.env.VERSION='{{version}}'; import { spawnSync } from 'child_process'; spawnSync('bun', ['run', 'build'], { stdio: 'inherit', shell: true, cwd: 'client', env: process.env });"

# Type-check the client, including Bun test files
client-check:
    cd client && bun run check

# Run the client's unit tests (bun:test)
client-test:
    cd client && bun test

# Run the Vite dev server standalone (hot reload, no Go server, no embedding)
client-watch:
    cd client && bun run dev

# Preview the real no-JavaScript login page on loopback only.
auth-preview mode="hybrid" oidc_ready="true":
    cd go && go run ./internal/auth/cmd/authpreview --mode {{quote(mode)}} --oidc-ready={{quote(oidc_ready)}}

# Output lives inside the client tree so Vite's dev-server fs.allow is happy.
# Regenerate client discovery and path-probe types from the JSON Schemas.
client-gen-types:
    cd client && bunx json-schema-to-typescript ../api/preflight.schema.json -o src/lib/api/preflight.ts
    cd client && bunx json-schema-to-typescript ../api/probe.schema.json -o src/lib/api/probe.ts

# --- Embedding (Go module + client, shared by both profiles) ---

# Copy the just-built client/dist into the Go module so //go:embed picks it up.
# Handled cross-platform via Bun's filesystem API to avoid rm/cp syntax differences.
# Profile-agnostic: run this after whichever of client-build-dev/client-build-prod ran.
_embed-client:
    bun -e "import fs from 'fs'; fs.rmSync('go/internal/static/dist', { recursive: true, force: true }); fs.cpSync('client/dist', 'go/internal/static/dist', { recursive: true }); fs.writeFileSync('go/internal/static/dist/.gitkeep', '')"

# --- Go server ---
# build-* recipes produce a standalone binary and stop (nothing runs it) — that's
# what a persisted artifact is for, so both strip debug info (-s -w -trimpath).
# The dev/prod entrypoints below use `go run` instead: there's no artifact to
# strip when the binary is executed once and discarded.

# Build the dev-profile server binary: dev client embedded, no version stamp
server-build-dev: client-build-dev _embed-client
    cd go && go build -ldflags="-s -w" -trimpath -o graphite-meter ./cmd/graphite-meter

# This is the shippable artifact (what a manual/non-Docker deploy would run).
# Build the prod-profile server binary: real-only client embedded, version-stamped.
server-build-prod: client-build-prod _embed-client
    cd go && go build \
      -ldflags="-s -w -X github.com/zR-JB/graphite-meter/go/internal/config.EngineVersion={{version}}" \
      -trimpath -o graphite-meter ./cmd/graphite-meter

# Check Go formatting and vet diagnostics.
server-check:
    cd go && unformatted=$(gofmt -l .); if [ -n "$unformatted" ]; then echo "$unformatted"; gofmt -d .; exit 1; fi
    cd go && go vet ./...

# Regenerate the embedded auth assets and fail if they drift from source. The
# pre-commit hook and ci.yml both call this recipe.
check-generated:
    #!/usr/bin/env sh
    set -e
    (cd go/internal/auth && go run ./cmd/authassets)
    if ! git diff --quiet -- go/internal/auth/assets_generated.go; then
        echo "assets_generated.go is stale; run 'go generate ./internal/auth' and commit the result"
        exit 1
    fi

# Pinned lint tools. Their install path carries the version, so a bump here
# rebuilds instead of reusing the cached binary. govulncheck reads the live
# vulnerability database, which its own version does not freeze.
staticcheck_version := "2025.1.1"
govulncheck_version := "v1.6.0"

# Static analysis + vulnerability scan. ci.yml calls this exact recipe, so the
# local gate and GitHub CI cannot describe different checks.
go-lint:
    #!/usr/bin/env sh
    set -e
    tools="$(go env GOPATH)/bin/gm-lint"
    staticcheck_dir="$tools/staticcheck-{{staticcheck_version}}"
    govulncheck_dir="$tools/govulncheck-{{govulncheck_version}}"
    test -x "$staticcheck_dir/staticcheck" || GOBIN="$staticcheck_dir" go install honnef.co/go/tools/cmd/staticcheck@{{staticcheck_version}}
    test -x "$govulncheck_dir/govulncheck" || GOBIN="$govulncheck_dir" go install golang.org/x/vuln/cmd/govulncheck@{{govulncheck_version}}
    cd go
    "$staticcheck_dir/staticcheck" ./...
    "$govulncheck_dir/govulncheck" ./...

# Race-detector tests plus the coverage floor. ci.yml calls this recipe, so the
# floor is enforced identically locally and in CI. Raise the floor as coverage
# climbs; never lower it.
server-test:
    #!/usr/bin/env sh
    set -e
    cd go
    CGO_ENABLED=1 go test -race -shuffle=on -coverprofile=cover.out ./...
    total=$(go tool cover -func=cover.out | awk '/^total:/ {print $3}' | tr -d '%')
    echo "total statement coverage: ${total}%"
    awk -v t="$total" 'BEGIN { exit (t + 0 >= 75.0) ? 0 : 1 }' \
        || { echo "coverage ${total}% is below the 75% floor"; exit 1; }

# Playwright browser tests (chromium + firefox). ci.yml calls this recipe after
# installing the browsers. Slow (~45s), so it is not in the pre-commit hook;
# run it explicitly or via `just ci-full`. The bundle is rebuilt by test:e2e,
# since playwright's webServer previews dist.
client-e2e:
    cd client && bun run test:e2e

# The fast local gate. ci.yml runs these same recipes; the pre-commit hook runs
# all but go-lint, and only those matching the staged files.
# Loopback saturation harness (issue #44): observer RTT percentiles under
# growing loader concurrency, on kernel TCP and userspace QUIC, plus a
# CPU-constrained pass. Measurement only; not part of ci. Unix only: the CPU
# column reads getrusage.
stress:
    cd go && go test -tags stress -run TestSaturationEnvelope -v -timeout 30m -count=1 ./internal/server/

ci: check-generated client-ci server-check go-lint server-test

# Everything CI runs that is meaningful on a workstation: the fast gate plus the
# browser E2E. The Docker smoke job and the cross-build matrix stay CI-only
# infrastructure (a container runtime / other toolchains).
ci-full: ci client-e2e

# --- Go native TUI client (graphite-meter-client) ---
# No dev/prod split: it doesn't embed the Svelte client, so there's nothing to profile.

# Build only the native Go Bubble Tea client. Does not build or stage the Svelte app.
# Stamped with the same `version` as server-build-prod (see the fallback tiers above).
goclient-build:
    cd go && go build \
      -ldflags="-s -w -X github.com/zR-JB/graphite-meter/go/internal/goclient.Version={{version}}" \
      -trimpath -o graphite-meter-client ./cmd/graphite-meter-client

# Run the native Go client against a running Graphite Meter server.
goclient-run:
    cd go && go run ./cmd/graphite-meter-client

# --- Main entrypoints ---
# Both build the client (in their own profile) and embed it, then `go run` the
# server (in the same profile) — same shape, only the profile differs.

# Dev: build + embed the dev-profile client, then `go run` the server on :7246
dev: client-build-dev _embed-client
    cd go && go run ./cmd/graphite-meter

# Override the GM_CLIENT_* knobs inline exactly like client-build-prod, e.g.
# `just prod allow_dummy=1 dev_tools=1 label=0.2.0`. For a persisted binary
# instead of a live run, use `server-build-prod`.
# Prod: build + embed the prod-profile client, then `go run` the version-stamped server on :7246.
prod: client-build-prod _embed-client
    cd go && go run \
      -ldflags="-X github.com/zR-JB/graphite-meter/go/internal/config.EngineVersion={{version}}" \
      ./cmd/graphite-meter

# --- Container (Docker/Podman) ---

# Build the production image (single static binary)
container-build:
    docker build -f container/Dockerfile -t graphite-meter:latest .
