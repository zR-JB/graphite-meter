# Graphite Meter - production image.
# Multi-stage: build the Svelte client (bun), then build the Go server with the
# client embedded, then ship a single static binary on scratch.
#
# Build from the REPO ROOT:
#   docker build -f container/Dockerfile -t graphite-meter:latest .
#
# Direct source builds are explicitly development builds. Release and
# prerelease automation supplies the public server and client versions.
ARG VERSION=0.0.0-dev
ARG CLIENT_VERSION

# ---------------------------------------------------------------------------
# Stage: client - build the Svelte app with bun.
# ---------------------------------------------------------------------------
FROM docker.io/oven/bun:1.4.2@sha256:9114c058aeae42162ee16dd5084b95fe9473970bb6bcb5b232ab1630f0546895 AS client
# Build-time client config (override with --build-arg). ARG must be promoted
# to ENV so it reaches `bun run build`'s process.env.
ARG GM_CLIENT_BUILD_PROFILE=prod
ARG GM_CLIENT_REVISION=source
# Keep standalone/local Docker builds self-validating. CI sets this to 0 only
# after the same commit has already passed the client check/test job.
ARG GM_CLIENT_VALIDATE=1
ARG VERSION
ARG CLIENT_VERSION
ENV GM_CLIENT_BUILD_PROFILE=$GM_CLIENT_BUILD_PROFILE \
    GM_CLIENT_REVISION=$GM_CLIENT_REVISION \
    VERSION=$CLIENT_VERSION \
    GM_LEGAL_SCAN_OUT=/tmp/browser-modules.json
WORKDIR /app/client
# Install deps first for layer caching. Every official build uses the
# repository's exact Bun release and the committed lockfile.
COPY client/package.json client/bun.lock client/bunfig.toml ./
RUN bun --bun install --frozen-lockfile
# Then the source, then build.
COPY client/ ./
COPY api/ /app/api/
COPY go/internal/auth/assets/ /app/go/internal/auth/assets/
RUN if [ "$GM_CLIENT_VALIDATE" = "1" ]; then bun run build; else bun run build:bundle; fi

# ---------------------------------------------------------------------------
# Stage: server - build the Go binary with the client embedded.
# ---------------------------------------------------------------------------
FROM docker.io/library/golang:1.27.1@sha256:3680233e3204827fbdc66088528ae6d4b3d034f51d03a99d454f6de034888244 AS server
ARG VERSION
WORKDIR /src/go
# Module graph first for caching.
COPY go/go.mod go/go.sum ./
RUN go mod download
# Server source; contract fixtures are only needed by tests.
COPY go/ ./
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
# Stage: third-party source - the source of the image's components, which a
# prerelease exports (--target third-party-source) and attaches as a stable
# release attaches the one scripts/release-artifacts.sh builds.
# ---------------------------------------------------------------------------
FROM docker.io/library/python:3.14.7-slim-bookworm@sha256:82bc3c539b8813ada9d68c63b40158fa002f7f33de9bf3312a3dfdc0620dff56 AS source-offer
ARG VERSION
ENV GOPATH=/go GOTOOLCHAIN=local PATH=/usr/local/go/bin:$PATH
COPY --from=server /usr/local/go /usr/local/go
COPY --from=server /go/pkg/mod /go/pkg/mod
# The scan names the bundled packages where the client stage installed them.
COPY --from=client /app/client/ /app/client/
COPY --from=client /root/.bun/install/cache/ /root/.bun/install/cache/
COPY --from=client /tmp/browser-modules.json /tmp/browser-modules.json
WORKDIR /src
COPY scripts/ scripts/
COPY legal/ legal/
COPY client/public/fonts/ client/public/fonts/
COPY go/ go/
COPY --from=client /app/client/dist go/internal/static/dist
RUN python3 -m scripts.legal --mode third-party-source-bundle --version "$VERSION" \
      --browser-scan /tmp/browser-modules.json

FROM scratch AS third-party-source
COPY --from=source-offer /src/go/dist/ /

# ---------------------------------------------------------------------------
# Final: a single static binary. The Svelte client is embedded inside it.
# Config comes from GM_* env and/or flags - read natively by the
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
# Distroless "nonroot"; mounted keys and secrets must be readable by it.
USER 65532:65532
ENTRYPOINT ["/graphite-meter"]
