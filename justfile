# Graphite Meter — monorepo task runner.
# Requires: bun 1.4+, go 1.26+. (https://github.com/casey/just)

# --- Production build knobs (override via env or `just prod label=… engine=…`) ---
# These feed the client's Vite `define` (see client/vite.config.ts) and the
# server's version ldflag. The `prod` recipe builds a real-only, dev-tooling-free
# bundle by default; flip the knobs to produce a configurable multi-engine build.
engine      := env("GM_CLIENT_ENGINE", "real")
allow_dummy := env("GM_CLIENT_ALLOW_DUMMY", "0")
dev_tools   := env("GM_CLIENT_DEV_TOOLS", "0")
label       := env("GM_CLIENT_BUILD_LABEL", `git rev-parse --short HEAD 2>/dev/null || echo prod`)
version     := env("VERSION", label)
go_cache    := env("GOCACHE", "/tmp/graphite-meter-go-build")

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
    cd crates/rng && PATH="$HOME/.cargo/bin:$PATH" cargo test

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

# Copy the built browser client into the Go module so //go:embed picks it up
_stage-client: build-client
    rm -rf go/internal/static/dist
    cp -r client/dist go/internal/static/dist

# Build the Go server binary (embeds the built client)
build-server: _stage-client
    cd go && GOCACHE="{{go_cache}}" CGO_ENABLED=0 go build -ldflags="-s -w" -trimpath -o graphite-meter ./cmd/graphite-meter

# Build only the native Go Bubble Tea client. Does not build or stage the Svelte app.
build-go-client:
    cd go && GOCACHE="{{go_cache}}" CGO_ENABLED=0 go build -ldflags="-s -w" -trimpath -o graphite-meter-client ./cmd/graphite-meter-client

# Run the native Go client against a running Graphite Meter server.
go-client:
    cd go && GOCACHE="{{go_cache}}" go run ./cmd/graphite-meter-client

# Run the server locally; serves the built client + /preflight on :8080
dev: _stage-client
    cd go && GOCACHE="{{go_cache}}" go run ./cmd/graphite-meter

# Run server tests (includes the preflight schema conformance test)
test-server:
    cd go && GOCACHE="{{go_cache}}" go test ./...

# --- Production ---

# Inline GM_CLIENT_* scopes the knobs to this build only — `dev`/`build-client`
# keep their dev defaults. Defaults: real-only engine, no dev tools, git-hash label.
# Build the client in production mode (real-only, dev tooling stripped)
prod-client:
    cd client && bun install && \
      GM_CLIENT_ENGINE="{{engine}}" \
      GM_CLIENT_ALLOW_DUMMY="{{allow_dummy}}" \
      GM_CLIENT_DEV_TOOLS="{{dev_tools}}" \
      GM_CLIENT_BUILD_LABEL="{{label}}" \
      bun run build

# Stage the prod browser client into the Go module so //go:embed picks it up
_prod-stage-client: prod-client
    rm -rf go/internal/static/dist
    cp -r client/dist go/internal/static/dist

# Full production build: real-only client embedded in the versioned server binary
prod: _prod-stage-client
    cd go && GOCACHE="{{go_cache}}" CGO_ENABLED=0 go build \
      -ldflags="-s -w -X github.com/zR-JB/graphite-meter/go/internal/config.EngineVersion={{version}}" \
      -trimpath -o graphite-meter ./cmd/graphite-meter

# --- Docker ---

# Build the production image (single static binary)
image:
    docker build -f docker/Dockerfile -t graphite-meter:latest .
