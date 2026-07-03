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

# Detect Git commit hash cross-platform, suppressing errors cleanly depending on OS
label       := env("GM_CLIENT_BUILD_LABEL", if os() == "windows" { `cmd.exe /c "git rev-parse --short HEAD 2>nul || echo prod"` } else { `git rev-parse --short HEAD 2>/dev/null || echo prod` })
version     := env("VERSION", label)

# Set OS-specific path for the Go build cache to remain fully cross-platform
GOCACHE     := env("GOCACHE", if os() == "windows" { env("TEMP") / "graphite-meter-go-build" } else { "/tmp/graphite-meter-go-build" })
CGO_ENABLED := "0"

# List available recipes
default:
    @just --list

# --- RNG (Rust generator, crates/rng) ---
# The client no longer bundles a WASM build of this: upload-worker.ts fills its
# payload with crypto.getRandomValues (the buffer is generated once and reused, so
# the RNG is never on the hot path). The crate is kept for the byte-exact
# cross-language conformance pin (api/rng.testvectors.txt) and a possible future
# WebTransport payload path. Legacy/reference only — not part of any other recipe.

# Run the Rust generator's byte-exact conformance test (vs api/rng.testvectors.txt)
test-rng:
    cd crates/rng && cargo test

# --- Client (Svelte/Vite, bun) ---

# Build the client in dev profile (Vite's own defaults: real engine, dummy + dev tools included)
client-build-dev:
    cd client && bun install && bun run build

# Override via GM_CLIENT_* env or `client-build-prod engine=… allow_dummy=… dev_tools=… label=…`.
# Build the client in prod profile: real-only engine, dev tooling stripped by default.
client-build-prod:
    cd client && bun install
    bun -e "process.env.GM_CLIENT_ENGINE='{{engine}}'; process.env.GM_CLIENT_ALLOW_DUMMY='{{allow_dummy}}'; process.env.GM_CLIENT_DEV_TOOLS='{{dev_tools}}'; process.env.GM_CLIENT_BUILD_LABEL='{{label}}'; import { spawnSync } from 'child_process'; spawnSync('bun', ['run', 'build'], { stdio: 'inherit', shell: true, cwd: 'client' });"

# Type-check the client
client-check:
    cd client && bun run check

# Run the Vite dev server standalone (hot reload, no Go server, no embedding)
client-watch:
    cd client && bun run dev

# Output lives inside the client tree so Vite's dev-server fs.allow is happy.
# Regenerate the client's preflight types from the JSON Schema (source of truth).
client-gen-types:
    cd client && bunx json-schema-to-typescript ../api/preflight.schema.json -o src/lib/api/preflight.ts

# --- Embedding (Go module + client, shared by both profiles) ---

# Copy the just-built client/dist into the Go module so //go:embed picks it up.
# Handled cross-platform via Bun's filesystem API to avoid rm/cp syntax differences.
# Profile-agnostic: run this after whichever of client-build-dev/client-build-prod ran.
_embed-client:
    bun -e "import fs from 'fs'; fs.rmSync('go/internal/static/dist', { recursive: true, force: true }); fs.cpSync('client/dist', 'go/internal/static/dist', { recursive: true })"

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

# Run server tests (includes the preflight schema conformance test)
server-test:
    cd go && go test ./...

# --- Go native TUI client (graphite-meter-client) ---
# No dev/prod split: it doesn't embed the Svelte client, so there's nothing to profile.

# Build only the native Go Bubble Tea client. Does not build or stage the Svelte app.
goclient-build:
    cd go && go build -ldflags="-s -w" -trimpath -o graphite-meter-client ./cmd/graphite-meter-client

# Run the native Go client against a running Graphite Meter server.
goclient-run:
    cd go && go run ./cmd/graphite-meter-client

# --- Main entrypoints ---
# Both build the client (in their own profile) and embed it, then `go run` the
# server (in the same profile) — same shape, only the profile differs.

# Dev: build + embed the dev-profile client, then `go run` the server on :8080
dev: client-build-dev _embed-client
    cd go && go run ./cmd/graphite-meter

# Override the GM_CLIENT_* knobs inline exactly like client-build-prod, e.g.
# `just prod allow_dummy=1 dev_tools=1 label=0.2.0`. For a persisted binary
# instead of a live run, use `server-build-prod`.
# Prod: build + embed the prod-profile client, then `go run` the version-stamped server on :8080.
prod: client-build-prod _embed-client
    cd go && go run \
      -ldflags="-X github.com/zR-JB/graphite-meter/go/internal/config.EngineVersion={{version}}" \
      ./cmd/graphite-meter

# --- Container (Docker/Podman) ---

# Build the production image (single static binary)
container-build:
    docker build -f container/Dockerfile -t graphite-meter:latest .
