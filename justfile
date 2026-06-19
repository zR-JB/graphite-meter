# Graphite Meter — monorepo task runner.
# Requires: bun 1.4+, go 1.26+. (https://github.com/casey/just)

# List available recipes
default:
    @just --list

# --- RNG (Rust -> WASM payload generator, crates/rng) ---

# Build the canonical xorshift64* generator to WASM into the client tree.
# Requires the Rust toolchain + wasm-pack on PATH (rustup installs to ~/.cargo/bin).
build-wasm:
    cd crates/rng && PATH="$HOME/.cargo/bin:$PATH" wasm-pack build --target web --release --out-dir ../../client/src/lib/wasm/rng --out-name gm_rng

# Run the Rust generator's byte-exact conformance test (vs api/rng.testvectors.txt)
test-rng:
    cd crates/rng && PATH="$HOME/.cargo/bin:$PATH" cargo test

# --- Client (Svelte/Vite, bun) ---

# Install client deps and produce client/dist (WASM generator built first)
build-client: build-wasm
    cd client && bun install && bun run build

# Type-check the client (WASM generator built first so its types resolve)
check: build-wasm
    cd client && bun run check

# Run the Vite dev server (hot reload, no Go server)
dev-client: build-wasm
    cd client && bun run dev

# Regenerate the client's preflight types from the JSON Schema (source of truth).
# Output lives inside the client tree so Vite's dev-server fs.allow is happy.
gen-types:
    cd client && bunx json-schema-to-typescript ../api/preflight.schema.json -o src/lib/api/preflight.ts

# --- Server (Go) ---

# Copy the built client into the server tree so //go:embed picks it up
_stage-client: build-client
    rm -rf server/internal/static/dist
    cp -r client/dist server/internal/static/dist

# Build the Go server binary (embeds the built client)
build-server: _stage-client
    cd server && CGO_ENABLED=0 go build -ldflags="-s -w" -trimpath -o graphite-meter ./cmd/graphite-meter

# Run the server locally; serves the built client + /preflight on :8080
dev: _stage-client
    cd server && go run ./cmd/graphite-meter

# Run server tests (includes the preflight schema conformance test)
test-server:
    cd server && go test ./...

# --- Docker ---

# Build the production image (single static binary)
image:
    docker build -f docker/Dockerfile -t graphite-meter:latest .
