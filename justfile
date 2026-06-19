# Graphite Meter — monorepo task runner.
# Requires: bun 1.4+, go 1.26+. (https://github.com/casey/just)

# List available recipes
default:
    @just --list

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

# Regenerate api/gen/preflight.ts from the JSON Schema (source of truth)
gen-types:
    cd client && bunx json-schema-to-typescript ../api/preflight.schema.json -o ../api/gen/preflight.ts

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
