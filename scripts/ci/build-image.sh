#!/usr/bin/env bash
# The server image of CI and release requests. Two concurrent cargo builds on one runner take longer than two in a row,
# so each architecture builds and loads alone; the OCI archive and source offers then reuse this builder's layers.
set -euo pipefail

build() {
  docker buildx build --file container/Dockerfile \
    --build-arg VERSION="$VERSION" --build-arg PROFILE="$PROFILE" \
    --build-arg GM_CLIENT_REVISION="$GM_CLIENT_REVISION" --build-arg GM_CLIENT_VALIDATE="$GM_CLIENT_VALIDATE" \
    --label "org.opencontainers.image.source=https://github.com/$REPOSITORY" \
    --label "org.opencontainers.image.revision=$REVISION" \
    --label "org.opencontainers.image.version=$VERSION" \
    --label org.opencontainers.image.licenses=AGPL-3.0-or-later "$@" "$CONTEXT"
}

# Docker's image store cannot load the attestation manifests.
for arch in amd64 arm64; do
  build --target server --platform "linux/$arch" --provenance false --load --tag "graphite-meter:$arch"
done
mkdir -p "$OUT_DIR"
build --target server --platform linux/amd64,linux/arm64 --provenance mode=max \
  --output "type=oci,dest=$OUT_DIR/graphite-meter.oci.tar"
# A local export writes the build's file tree as it is, so only a stable release's own source exports one.
if [[ "$SOURCE_OFFERS" == true ]]; then
  build --target server-artifacts --platform linux/amd64,linux/arm64 --provenance false \
    --output "type=local,dest=$OUT_DIR/server-export"
fi
