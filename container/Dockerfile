# Graphite Meter — production image.
# Multi-stage: build the Svelte client (bun), then build the Go server with the
# client embedded, then ship a single static binary on scratch.
#
# Build from the REPO ROOT:
#   docker build -f container/Dockerfile -t graphite-meter:latest .
#
# Reserved for a future Go->Rust server swap:
ARG SERVER_IMPL=go
ARG BUN_VERSION=canary
# Stamped into both EngineVersion (server) and the client bundle (see the
# client stage's VERSION ARG below) — one value, two consumers. This default
# only applies to a manual `docker build` with no --build-arg; CI (ci.yml's
# Smoke job, release.yml) always passes VERSION explicitly, so it never
# relies on this fallback.
ARG VERSION=0.1.0
ARG CLIENT_VERSION

# ---------------------------------------------------------------------------
# Stage: client — build the Svelte app with bun.
# ---------------------------------------------------------------------------
FROM docker.io/oven/bun:${BUN_VERSION} AS client
# Build-time client config (override with --build-arg). Defaults = production:
# real-only engine, dev tooling stripped. ARG must be promoted to ENV so it
# reaches `bun run build`'s process.env, where vite.config.ts reads it.
ARG GM_CLIENT_ENGINE=real
ARG GM_CLIENT_ALLOW_DUMMY=0
ARG GM_CLIENT_DEV_TOOLS=0
ARG GM_CLIENT_BUILD_PROFILE=prod
ARG GM_CLIENT_REVISION=source
# Keep standalone/local Docker builds self-validating. CI sets this to 0 only
# after the same commit has already passed the client check/test job.
ARG GM_CLIENT_VALIDATE=1
ARG VERSION
ARG CLIENT_VERSION
ENV GM_CLIENT_ENGINE=$GM_CLIENT_ENGINE \
    GM_CLIENT_ALLOW_DUMMY=$GM_CLIENT_ALLOW_DUMMY \
    GM_CLIENT_DEV_TOOLS=$GM_CLIENT_DEV_TOOLS \
    GM_CLIENT_BUILD_PROFILE=$GM_CLIENT_BUILD_PROFILE \
    GM_CLIENT_REVISION=$GM_CLIENT_REVISION \
    VERSION=$CLIENT_VERSION
WORKDIR /app/client
# Install deps first for layer caching. Every official build uses the
# repository's declared Bun channel and the committed lockfile.
COPY client/package.json client/bun.lock ./
RUN bun --bun install --frozen-lockfile
# Then the source, then build.
COPY client/ ./
RUN if [ "$GM_CLIENT_VALIDATE" = "1" ]; then bun run build; else bun run build:bundle; fi

# ---------------------------------------------------------------------------
# Stage: server — build the Go binary with the client embedded.
# ---------------------------------------------------------------------------
# Pin the patch release used by CI and the checked-in Go legal snapshot. The
# floating 1.26 tag can lag the go.mod minimum and fail before the image's
# legal files are even staged.
FROM docker.io/library/golang:1.26.6 AS server
ARG VERSION
WORKDIR /src/go
# Module graph first for caching.
COPY go/go.mod go/go.sum ./
RUN go mod download
# Source + the api/ tree (the schema-conformance test references ../../../api).
COPY go/ ./
COPY api/ /src/api/
# Legal assets are generated from the same runtime closure used by the
# server/browser and container inventories.
COPY LICENSE COPYRIGHT legal/generated/container/THIRD_PARTY_NOTICES.txt legal/generated/container/SOURCE.txt /legal/graphite-meter/
# Embed the built client.
COPY --from=client /app/client/dist ./internal/static/dist
RUN CGO_ENABLED=0 go build \
      -ldflags="-s -w -X github.com/zR-JB/graphite-meter/go/internal/config.EngineVersion=${VERSION}" \
      -trimpath \
      -o /graphite-meter ./cmd/graphite-meter
RUN set -eux; test -f /usr/share/doc/ca-certificates/copyright; mkdir -p /legal/ca-certificates; cp /usr/share/doc/ca-certificates/copyright /legal/ca-certificates/COPYRIGHT

# ---------------------------------------------------------------------------
# Final: a single static binary. The Svelte client is embedded inside it.
# Config comes from env (GM_*, PUBLIC_*) and/or flags — read natively by the
# binary, so no shell entrypoint is needed (scratch has no shell anyway).
# ---------------------------------------------------------------------------
FROM scratch
COPY --from=server /graphite-meter /graphite-meter
COPY --from=server /legal/graphite-meter /usr/share/licenses/graphite-meter
COPY --from=server /legal/ca-certificates/COPYRIGHT /usr/share/licenses/ca-certificates/COPYRIGHT
# Outbound OIDC discovery, token exchange, UserInfo, and JWKS requests need the
# public CA roots. A scratch image does not provide them on its own.
COPY --from=server /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
EXPOSE 7246/tcp
EXPOSE 7247/tcp
EXPOSE 7248/tcp
EXPOSE 7249/tcp
EXPOSE 7249/udp
ENTRYPOINT ["/graphite-meter"]
