# Graphite Meter — production image.
# Multi-stage: build the Svelte client (bun), then build the Go server with the
# client embedded, then ship a single static binary on scratch.
#
# Build from the REPO ROOT:
#   docker build -f docker/Dockerfile -t graphite-meter:latest .
#
# Reserved for a future Go->Rust server swap:
ARG SERVER_IMPL=go
ARG VERSION=0.1.0

# ---------------------------------------------------------------------------
# Stage: wasm (reserved). The browser upload RNG (crates/rng -> WASM) is wired
# in at Stage 3. Until then this is a no-op passthrough so the stage exists in
# the pipeline; the client stage copies from it but it carries nothing yet.
# ---------------------------------------------------------------------------
FROM scratch AS wasm
# (Stage 3: build crates/rng with wasm-pack into /wasm and COPY it into client.)

# ---------------------------------------------------------------------------
# Stage: client — build the Svelte app with bun.
# ---------------------------------------------------------------------------
FROM oven/bun:1 AS client
WORKDIR /app/client
# Install deps first for layer caching. (Not --frozen-lockfile: the committed
# bun.lock may be a newer lockfile version than the pinned bun image; bun then
# resolves from package.json's pinned ranges, which is fine for a build image.)
COPY client/package.json client/bun.lock ./
RUN bun install
# Then the source + build.
COPY client/ ./
# COPY --from=wasm / ./src/lib/wasm   # (Stage 3)
RUN bun run build   # -> /app/client/dist

# ---------------------------------------------------------------------------
# Stage: server — build the Go binary with the client embedded.
# ---------------------------------------------------------------------------
FROM golang:1.26 AS server
ARG VERSION
WORKDIR /src/server
# Module graph first for caching.
COPY server/go.mod server/go.sum ./
RUN go mod download
# Source + the api/ tree (the schema-conformance test references ../../../api).
COPY server/ ./
COPY api/ /src/api/
# Embed the built client.
COPY --from=client /app/client/dist ./internal/static/dist
RUN CGO_ENABLED=0 go build \
      -ldflags="-s -w -X github.com/zR-JB/graphite-meter/server/internal/config.EngineVersion=${VERSION}" \
      -trimpath \
      -o /graphite-meter ./cmd/graphite-meter

# ---------------------------------------------------------------------------
# Final: a single static binary. The Svelte client is embedded inside it.
# Config comes from env (GM_*, PUBLIC_*) and/or flags — read natively by the
# binary, so no shell entrypoint is needed (scratch has no shell anyway).
# ---------------------------------------------------------------------------
FROM scratch
COPY --from=server /graphite-meter /graphite-meter
# HTTP/1.1 (default). TLS + HTTP/3 (WebTransport) land in Stage 5:
EXPOSE 8080/tcp
# EXPOSE 8443/tcp
# EXPOSE 8443/udp
ENTRYPOINT ["/graphite-meter"]
