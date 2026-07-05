# Graphite Meter — production image.
# Multi-stage: build the Svelte client (bun), then build the Go server with the
# client embedded, then ship a single static binary on scratch.
#
# Build from the REPO ROOT:
#   docker build -f container/Dockerfile -t graphite-meter:latest .
#
# Reserved for a future Go->Rust server swap:
ARG SERVER_IMPL=go
# Stamped into both EngineVersion (server) and the client bundle (see the
# client stage's VERSION ARG below) — one value, two consumers. This default
# only applies to a manual `docker build` with no --build-arg; CI (ci.yml's
# Smoke job, release.yml) always passes VERSION explicitly, so it never
# relies on this fallback.
ARG VERSION=0.1.0

# ---------------------------------------------------------------------------
# Stage: client — build the Svelte app with bun.
# ---------------------------------------------------------------------------
FROM docker.io/oven/bun:canary AS client
# Build-time client config (override with --build-arg). Defaults = production:
# real-only engine, dev tooling stripped. ARG must be promoted to ENV so it
# reaches `bun run build`'s process.env, where vite.config.ts reads it.
# GM_CLIENT_BUILD_LABEL is the status-bar label AND the label half of the
# client version <semver>+<label> (Endpoint info drawer, dist/version.json,
# preflight ?client_version=). VERSION (the semver half) is the same build-arg
# the server stage stamps into EngineVersion below — one source of truth for
# both halves of the binary, so client and server can never disagree.
ARG GM_CLIENT_ENGINE=real
ARG GM_CLIENT_ALLOW_DUMMY=0
ARG GM_CLIENT_DEV_TOOLS=0
ARG GM_CLIENT_BUILD_LABEL=prod
ARG VERSION
ENV GM_CLIENT_ENGINE=$GM_CLIENT_ENGINE \
    GM_CLIENT_ALLOW_DUMMY=$GM_CLIENT_ALLOW_DUMMY \
    GM_CLIENT_DEV_TOOLS=$GM_CLIENT_DEV_TOOLS \
    GM_CLIENT_BUILD_LABEL=$GM_CLIENT_BUILD_LABEL \
    VERSION=$VERSION
WORKDIR /app/client
# Install deps first for layer caching. (Not --frozen-lockfile: the committed
# bun.lock may be a newer lockfile version than the pinned bun image; bun then
# resolves from package.json's pinned ranges, which is fine for a build image.)
COPY client/package.json client/bun.lock ./
RUN bun --bun install
# Then the source, then build.
COPY client/ ./
RUN bun run build   # -> /app/client/dist

# ---------------------------------------------------------------------------
# Stage: server — build the Go binary with the client embedded.
# ---------------------------------------------------------------------------
FROM docker.io/library/golang:1.26 AS server
ARG VERSION
WORKDIR /src/go
# Module graph first for caching.
COPY go/go.mod go/go.sum ./
RUN go mod download
# Source + the api/ tree (the schema-conformance test references ../../../api).
COPY go/ ./
COPY api/ /src/api/
# Embed the built client.
COPY --from=client /app/client/dist ./internal/static/dist
RUN CGO_ENABLED=0 go build \
      -ldflags="-s -w -X github.com/zR-JB/graphite-meter/go/internal/config.EngineVersion=${VERSION}" \
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
EXPOSE 8765/tcp
# EXPOSE 8443/tcp
# EXPOSE 8443/udp
ENTRYPOINT ["/graphite-meter"]
