#!/usr/bin/env bash
# The server image of CI and release requests, one OCI archive per platform of SERVER_PLATFORMS, which assets.py stage
# joins into the multi-platform image. CI builds them one after the other; a release builds each on its own runner.
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

mkdir -p "$OUT_DIR"
for platform in $SERVER_PLATFORMS; do
  name=${platform//\//_}
  supplied=()
  # A stable release builds from the profile it exports, as a rebuild does (legal/README.md).
  if [[ "$SOURCE_OFFERS" == true && "$PROFILE" == release ]]; then
    build --target "pgo-${platform#*/}-trained" --platform "$platform" --provenance false \
      --output "type=local,dest=$CONTEXT/pgo"
    supplied=(--build-arg PGO=supplied)
  fi
  if [[ "$PROFILE" == ci ]]; then
    # Docker's image store cannot load the attestation manifests.
    build --target server --platform "$platform" --provenance false --load --tag "graphite-meter:${platform#*/}"
  fi
  build --target server --platform "$platform" "${supplied[@]}" --provenance mode=max \
    --output "type=oci,dest=$OUT_DIR/graphite-meter_$name.oci.tar"
  # A local export writes the build's file tree as it is, so only a stable release's own source exports one.
  if [[ "$SOURCE_OFFERS" == true ]]; then
    build --target server-artifacts --platform "$platform" "${supplied[@]}" --provenance false \
      --output "type=local,dest=$OUT_DIR/server-export/$name"
  fi
done
