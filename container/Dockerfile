# Graphite Meter: the server image and the terminal client archives.
#
# Build from the repository root on an amd64 host; the builder cross-compiles arm64:
#   mise run container-build          (target server: the image)
#   mise run tui-package VERSION      (target tui-artifacts: the client archives)
# Target server-artifacts exports the server's third-party source offer. PROFILE=ci builds without LTO or PGO.
ARG VERSION=0.0.0-dev
ARG TARGETARCH
ARG PROFILE=release
# Empty builds every TUI platform; CI and release requests build one per runner.
ARG TUI_PLATFORM

FROM --platform=$BUILDPLATFORM docker.io/library/python:3.15.0-slim-trixie@sha256:d802f03223b9e00604cd54574bcac1a4638914c43683d3f0afd6ebee35f25313 AS python

# The reviewed builder: legal/rust-platform-debian-trixie.json records what it links into each target.
FROM --platform=$BUILDPLATFORM docker.io/library/rust:1.99.1-trixie@sha256:7e9eb23ac694dda816c76933ab63b2703e6becbf3b10da90970124656788d47b AS rust-amd64
COPY --from=python /usr/local /usr/local
ENV LD_LIBRARY_PATH=/usr/local/lib \
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=x86_64-linux-gnu-gcc \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-linux-gnu-gcc \
    CC_x86_64_unknown_linux_musl=x86_64-linux-musl-gcc
# Debian's archive serves one version of each package, so apt reads it as it was when the platform
# records were reviewed: the pinned versions and every dependency stay installable. C dependencies
# use musl headers; Rust's final link still uses its self-contained libc.
RUN printf 'Types: deb\nURIs: https://snapshot.debian.org/archive/%s/20261016T180000Z\nSuites: %s\nComponents: main\nSigned-By: /usr/share/keyrings/debian-archive-keyring.gpg\nCheck-Valid-Until: no\n\n' \
        debian 'trixie trixie-updates' debian-security trixie-security >/etc/apt/sources.list.d/debian.sources \
    && rm -f /etc/apt/sources.list \
    && apt-get update && apt-get install -y --no-install-recommends musl-dev:amd64=1.2.5-3.1~deb13u1 \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
# The pinned toolchain, checked against its channel manifest, with the standard library of every shipped target and
# the LLVM tools that merge PGO profiles.
COPY mise.toml ./
COPY rust/rust-toolchain.toml rust/Cargo.toml rust/
COPY scripts/ci/workspace.py scripts/ci/toolchains.py scripts/ci/
RUN cd rust && rustup toolchain install --no-self-update \
    && rustup component add llvm-tools \
    && python3 ../scripts/ci/toolchains.py verify-rust \
    && rustup target add $( (python3 ../scripts/ci/workspace.py targets tui; \
        python3 ../scripts/ci/workspace.py targets server) | sort -u)

# Debian's arm64 musl wrapper uses the GNU cross compiler with musl target headers.
FROM rust-amd64 AS rust-arm64
RUN dpkg --add-architecture arm64 \
    && apt-get update && apt-get install -y --no-install-recommends \
        gcc-aarch64-linux-gnu=4:14.2.0-1 libc6-dev-arm64-cross=2.41-11cross1 musl-dev:arm64=1.2.5-3.1~deb13u1 \
    && rm -rf /var/lib/apt/lists/*
ENV CC_aarch64_unknown_linux_musl=aarch64-linux-musl-gcc

FROM scratch AS source
COPY LICENSE COPYRIGHT mise.toml ./
COPY scripts/ scripts/
COPY legal/ legal/
COPY rust/ rust/

# A release's PGO profile per architecture in /pgo, trained on that architecture's own code: amd64 natively, arm64
# under QEMU. The ci profile trains none.
FROM rust-amd64 AS pgo-amd64-release
COPY --from=source / /src/
RUN python3 -m scripts.pgo "$(python3 scripts/ci/workspace.py target server linux/amd64)" --profile release \
    && mkdir /pgo && cp rust/target/pgo/*.profdata /pgo/

FROM rust-arm64 AS pgo-arm64-release
RUN apt-get update && apt-get install -y --no-install-recommends qemu-user=1:10.0.13+ds-0+deb13u1 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=source / /src/
RUN python3 -m scripts.pgo "$(python3 scripts/ci/workspace.py target server linux/arm64)" --profile release \
        --runner qemu-aarch64 \
    && mkdir /pgo && cp rust/target/pgo/*.profdata /pgo/

FROM rust-amd64 AS pgo-ci
RUN mkdir /pgo
FROM pgo-ci AS pgo-amd64-ci
FROM pgo-ci AS pgo-arm64-ci

FROM pgo-amd64-${PROFILE} AS pgo-amd64
FROM pgo-arm64-${PROFILE} AS pgo-arm64
FROM pgo-${TARGETARCH} AS pgo-server

# A TUI build reads its TUI_PLATFORM's profile, Windows amd64 amd64's and Windows arm64 none, or with none every one (pgo).
FROM scratch AS pgo
COPY --from=pgo-amd64 /pgo/ /pgo/
COPY --from=pgo-arm64 /pgo/ /pgo/
FROM pgo-amd64 AS pgo-linux-amd64
FROM pgo-arm64 AS pgo-linux-arm64
FROM pgo-amd64 AS pgo-windows-amd64
FROM pgo-ci AS pgo-windows-arm64
FROM pgo${TUI_PLATFORM:+-}${TUI_PLATFORM%%/*}${TUI_PLATFORM:+-}${TUI_PLATFORM##*/} AS pgo-tui

# The TUI archives for every TUI platform of the workspace metadata, or the TUI_PLATFORM's, each with its source offer.
FROM rust-arm64 AS tui-build
RUN apt-get update && apt-get install -y --no-install-recommends \
        gcc-mingw-w64-x86-64-win32=14.2.0-19+27+b1 mingw-w64-x86-64-dev=12.0.0-5 \
    && rm -rf /var/lib/apt/lists/*
# Windows arm64 links with llvm-mingw, appended to PATH so Debian's cross tools keep their names.
ADD --checksum=sha256:5f9c6ed95b2d4bdb2869a488c5fd5857fbdabcf288a0aa3eb1da43f6a08d8ab4 https://github.com/mstorsjo/llvm-mingw/releases/download/20261006/llvm-mingw-20261006-ucrt-ubuntu-22.04-x86_64.tar.xz /tmp/llvm-mingw.tar.xz
RUN mkdir /opt/llvm-mingw && tar -xJf /tmp/llvm-mingw.tar.xz --strip-components=1 -C /opt/llvm-mingw \
    && rm /tmp/llvm-mingw.tar.xz
ENV CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
    PATH=$PATH:/opt/llvm-mingw/bin
COPY --from=source / /src/
COPY --from=pgo-tui /pgo/ /src/rust/target/pgo/
ARG VERSION
ARG PROFILE
ARG TUI_PLATFORM
RUN python3 -m scripts.package "$VERSION" --profile "$PROFILE" --output dist ${TUI_PLATFORM:+--platform "$TUI_PLATFORM"}

FROM scratch AS tui-artifacts
COPY --from=tui-build /src/dist/ /

FROM --platform=$BUILDPLATFORM docker.io/oven/bun:1.4.3@sha256:ec06c3b6cea04192ae6770c434f668ca41d343ad19fa6472216c7b48be39c598 AS browser
ARG VERSION
ARG GM_CLIENT_REVISION=source
# CI sets this to 0 only after the same commit passed the client checks.
ARG GM_CLIENT_VALIDATE=1
ENV VERSION=$VERSION \
    GM_CLIENT_BUILD_PROFILE=prod \
    GM_CLIENT_REVISION=$GM_CLIENT_REVISION \
    GM_LEGAL_SCAN_OUT=/tmp/browser-modules.json
WORKDIR /src/client
COPY client/package.json client/bun.lock client/bunfig.toml ./
RUN bun --bun install --frozen-lockfile
COPY client/ ./
RUN if [ "$GM_CLIENT_VALIDATE" = "1" ]; then bun run build; else bun run build:bundle; fi

# The static musl server with reviewed notices and the browser app.
FROM rust-${TARGETARCH} AS server-build
ARG TARGETARCH
ARG PROFILE
COPY --from=source / /src/
COPY --from=pgo-server /pgo/ /src/rust/target/pgo/
COPY --from=browser /src/client/ /src/client/
# The legal scan names the bundled packages at their paths in Bun's installation cache.
COPY --from=browser /root/.bun/install/cache/ /root/.bun/install/cache/
COPY --from=browser /tmp/browser-modules.json /tmp/browser-modules.json
ARG VERSION
RUN target=$(python3 scripts/ci/workspace.py target server "linux/$TARGETARCH") \
    && python3 -m scripts.legal.rust --package graphite-meter-server --target "$target" --profile "$PROFILE" \
        --version "$VERSION" --out rust/target/server-notices --browser-assets client/dist \
        --browser-scan /tmp/browser-modules.json \
    && cp "rust/target/$target/$PROFILE/graphite-meter-server" /graphite-meter

# The source offer, which the image's SOURCE.txt names, is a release asset, not image content.
FROM scratch AS server-artifacts
COPY --from=server-build /src/rust/target/server-notices/*_third-party-source.tar.gz /

# One static binary, its notices and the CA roots, with no libc, shell or package manager.
FROM scratch AS server
COPY --from=server-build /graphite-meter /graphite-meter
COPY LICENSE COPYRIGHT /usr/share/licenses/graphite-meter/
# The binary's notices followed by the image's own additions.
COPY --from=server-build /src/rust/target/server-notices/IMAGE_NOTICES.txt /usr/share/licenses/graphite-meter/THIRD_PARTY_NOTICES.txt
COPY --from=server-build /src/rust/target/server-notices/SOURCE.txt /usr/share/licenses/graphite-meter/SOURCE.txt
COPY --from=rust-amd64 /usr/share/doc/ca-certificates/copyright /usr/share/licenses/ca-certificates/COPYRIGHT
# Outbound OIDC discovery, token exchange, UserInfo and JWKS requests need the public CA roots.
COPY --from=rust-amd64 /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
EXPOSE 7246/tcp 7247/tcp 7248/tcp 7249/tcp 7249/udp
# Distroless "nonroot"; mounted keys and secrets must be readable by it.
USER 65532:65532
ENTRYPOINT ["/graphite-meter"]
