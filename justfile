# Graphite Meter — monorepo task runner.
# Requires: bun 1.4+, go 1.26+. (https://github.com/casey/just)

# Default shell for Linux/macOS
set shell := ["sh", "-c"]

# Fallback shell used automatically only when running on Windows (Requires PowerShell 7+)
set windows-shell := ["pwsh", "-NoProfile", "-Command"]

# CRITICAL: Automatically exports all just variables as environment variables 
# to the underlying shell. This eliminates inline "KEY=VALUE command" breaking on Windows.
set export := true

# --- Production build knobs (override via env or `just prod label=… engine=…`) ---
# These feed the client's Vite `define` (see client/vite.config.ts) and the
# server's version ldflag. The `prod` recipe builds a real-only, dev-tooling-free
# bundle by default; flip the knobs to produce a configurable multi-engine build.
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
# WebTransport payload path.

# Run the Rust generator's byte-exact conformance test (vs api/rng.testvectors.txt)
test-rng:
    cd crates/rng && cargo test

# --- Client (Svelte/Vite, bun) ---

# Install client deps and produce client/dist
build-client:
    cd client && bun install && bun run build

# Type-check the client
check:
    cd client && bun run check

# Run the Vite dev server (hot reload, no Go server)
dev-client:
    cd client && bun run dev

# Regenerate the client's preflight types from the JSON Schema (source of truth).
# Output lives inside the client tree so Vite's dev-server fs.allow is happy.
gen-types:
    cd client && bunx json-schema-to-typescript ../api/preflight.schema.json -o src/lib/api/preflight.ts

# --- Go module (server + native client) ---

# Handled cross-platform via Bun filesystem API to avoid rm/cp syntax differences
_stage-client: build-client
    bun -e "import fs from 'fs'; fs.rmSync('go/internal/static/dist', { recursive: true, force: true }); fs.cpSync('client/dist', 'go/internal/static/dist', { recursive: true })"

# Build the Go server binary (embeds the built client)
build-server: _stage-client
    cd go && go build -ldflags="-s -w" -trimpath -o graphite-meter ./cmd/graphite-meter

# Build only the native Go Bubble Tea client. Does not build or stage the Svelte app.
build-go-client:
    cd go && go build -ldflags="-s -w" -trimpath -o graphite-meter-client ./cmd/graphite-meter-client

# Run the native Go client against a running Graphite Meter server.
go-client:
    cd go && go run ./cmd/graphite-meter-client

# Run the server locally; serves the built client + /preflight on :8080
dev: _stage-client
    cd go && go run ./cmd/graphite-meter

# Run server tests (includes the preflight schema conformance test)
test-server:
    cd go && go test ./...

# --- Production ---

# Inline GM_CLIENT_* scopes the knobs to this build only — `dev`/`build-client`
# keep their dev defaults. Defaults: real-only engine, no dev tools, git-hash label.
# Build the client in production mode (real-only, dev tooling stripped)
prod-client:
    cd client && bun install
    bun -e "process.env.GM_CLIENT_ENGINE='{{engine}}'; process.env.GM_CLIENT_ALLOW_DUMMY='{{allow_dummy}}'; process.env.GM_CLIENT_DEV_TOOLS='{{dev_tools}}'; process.env.GM_CLIENT_BUILD_LABEL='{{label}}'; import { spawnSync } from 'child_process'; spawnSync('bun', ['run', 'build'], { stdio: 'inherit', shell: true, cwd: 'client' });"

# Stage the prod browser client into the Go module so //go:embed picks it up
_prod-stage-client: prod-client
    bun -e "import fs from 'fs'; fs.rmSync('go/internal/static/dist', { recursive: true, force: true }); fs.cpSync('client/dist', 'go/internal/static/dist', { recursive: true })"

# Full production build: real-only client embedded in the versioned server binary
prod: _prod-stage-client
    cd go && go build \
      -ldflags="-s -w -X github.com/zR-JB/graphite-meter/go/internal/config.EngineVersion={{version}}" \
      -trimpath -o graphite-meter ./cmd/graphite-meter

# --- Docker ---

# Build the production image (single static binary)
image:
    docker build -f docker/Dockerfile -t graphite-meter:latest .
    