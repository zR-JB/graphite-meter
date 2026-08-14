#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 3 ]]; then
    echo "usage: $0 VERSION REVISION OCI_ARCHIVE" >&2
    exit 2
fi
version=$1
revision=$2
archive=$3

engine=${CONTAINER_ENGINE:-}
if [[ -z "$engine" ]]; then
    if command -v docker >/dev/null 2>&1; then engine=docker
    elif command -v podman >/dev/null 2>&1; then engine=podman
    else echo "OCI verification requires Docker or Podman" >&2; exit 2
    fi
fi
command -v "$engine" >/dev/null 2>&1 || { echo "container engine '$engine' was not found" >&2; exit 2; }
: "${SKOPEO_IMAGE:?SKOPEO_IMAGE must be immutable}"
: "${SKOPEO_VERSION:?SKOPEO_VERSION must be set}"
: "${REPOSITORY:?REPOSITORY must be set}"
test -s "$archive"

skopeo_version=$("$engine" run --rm --entrypoint skopeo "$SKOPEO_IMAGE" --version)
echo "$skopeo_version"
grep -F "skopeo version $SKOPEO_VERSION " <<<"$skopeo_version" >/dev/null

raw=$("$engine" run --rm --entrypoint skopeo -v "$archive:/work/candidate.oci.tar:ro" \
    "$SKOPEO_IMAGE" inspect --raw oci-archive:/work/candidate.oci.tar)
jq -e '
    (.manifests | map(.platform | {os, architecture}) | sort)
    == [{"os":"linux","architecture":"amd64"},{"os":"linux","architecture":"arm64"}]
' <<<"$raw" >/dev/null || { echo "candidate OCI archive has the wrong platform set" >&2; exit 1; }

expected_source="https://github.com/$REPOSITORY"
for arch in amd64 arm64; do
    labels=$("$engine" run --rm --entrypoint skopeo \
        -v "$archive:/work/candidate.oci.tar:ro" "$SKOPEO_IMAGE" \
        inspect --override-os linux --override-arch "$arch" \
        --format '{{json .Labels}}' oci-archive:/work/candidate.oci.tar)
    jq -e --arg source "$expected_source" --arg revision "$revision" --arg version "$version" '
        .["org.opencontainers.image.source"] == $source
        and .["org.opencontainers.image.revision"] == $revision
        and .["org.opencontainers.image.version"] == $version
        and .["org.opencontainers.image.licenses"] == "AGPL-3.0-or-later"
    ' <<<"$labels" >/dev/null || { echo "candidate OCI labels are invalid for linux/$arch" >&2; exit 1; }
done

echo "candidate OCI verification passed: $version"
