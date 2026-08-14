#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
    echo "usage: $0 VERSION OCI_ARCHIVE" >&2
    exit 2
fi
version=$1
archive=$2
dist=${RELEASE_DIST:-go/dist}
engine=${CONTAINER_ENGINE:-}
if [[ -z "$engine" ]]; then
    if command -v docker >/dev/null 2>&1; then engine=docker; elif command -v podman >/dev/null 2>&1; then engine=podman; else
        echo "release verification requires Docker or Podman; set CONTAINER_ENGINE explicitly" >&2
        exit 2
    fi
fi
command -v "$engine" >/dev/null 2>&1 || { echo "release verification engine '$engine' was not found" >&2; exit 2; }
: "${SKOPEO_IMAGE:?SKOPEO_IMAGE must be an immutable Skopeo image reference}"
: "${SKOPEO_VERSION:?SKOPEO_VERSION must be set}"
test -s "$archive"
test -d "$dist"

tmp=$(mktemp -d)
server_pid=
cleanup() {
    if [[ -n "$server_pid" ]]; then kill "$server_pid" 2>/dev/null || true; fi
    rm -rf "$tmp"
}
trap cleanup EXIT
mkdir -p "$tmp/local-copy"

skopeo_version=$("$engine" run --rm --entrypoint skopeo "$SKOPEO_IMAGE" --version)
echo "$skopeo_version"
grep -F "skopeo version $SKOPEO_VERSION " <<<"$skopeo_version" >/dev/null

raw=$("$engine" run --rm --entrypoint skopeo -v "$archive:/work/release.oci.tar:ro" "$SKOPEO_IMAGE" inspect --raw oci-archive:/work/release.oci.tar)
jq -e '
  (.manifests | map(select(.platform.os == "linux" and .platform.architecture == "amd64")) | length) == 1
  and (.manifests | map(select(.platform.os == "linux" and .platform.architecture == "arm64")) | length) == 1
' <<<"$raw" >/dev/null

labels=$("$engine" run --rm --entrypoint skopeo -v "$archive:/work/release.oci.tar:ro" "$SKOPEO_IMAGE" inspect --format '{{json .Labels}}' oci-archive:/work/release.oci.tar)
jq -e --arg version "$version" '
  .["org.opencontainers.image.version"] == $version
  and .["org.opencontainers.image.licenses"] == "AGPL-3.0-or-later"
  and (.["org.opencontainers.image.source"] | test("github.com/zR-JB/graphite-meter$"))
' <<<"$labels" >/dev/null

"$engine" run --rm --entrypoint skopeo \
    -v "$archive:/work/release.oci.tar:ro" \
    -v "$tmp/local-copy:/work/local-copy" \
    "$SKOPEO_IMAGE" copy --all oci-archive:/work/release.oci.tar dir:/work/local-copy
test -s "$tmp/local-copy/manifest.json" || test -s "$tmp/local-copy/oci-layout"

(cd "$dist" && sha256sum -c checksums.txt)
test -s "$dist/graphite-meter_${version}_corresponding-source.tar.gz"
forbidden='(^|/)(\.dev-certs|certs?|certificates?|letsencrypt)(/|$)|\.(pem|key|crt|cer|der|csr|p12|pfx|pkcs8|jks|keystore)$'
if tar -tzf "$dist/graphite-meter_${version}_corresponding-source.tar.gz" | grep -Eiq "$forbidden"; then
    echo "release verification failed: corresponding-source archive contains certificate material" >&2
    exit 1
fi

while IFS= read -r target; do
    [[ -n "$target" ]] || continue
    goos=${target%/*}; goarch=${target#*/}
    base="graphite-meter-client_${version}_${goos}_${goarch}"
    case "$goos" in
        windows)
            file="$dist/$base.zip"
            unzip -l "$file" | grep -F "$base/LICENSE" >/dev/null
            unzip -l "$file" | grep -F "$base/COPYRIGHT" >/dev/null
            unzip -l "$file" | grep -F "$base/THIRD_PARTY_NOTICES.txt" >/dev/null
            unzip -l "$file" | grep -F "$base/SOURCE.txt" >/dev/null
            if unzip -Z1 "$file" | grep -Eiq "$forbidden"; then
                echo "release verification failed: $file contains certificate material" >&2
                exit 1
            fi
            ;;
        *)
            file="$dist/$base.tar.gz"
            tar -tzf "$file" | grep -Fx "$base/LICENSE" >/dev/null
            tar -tzf "$file" | grep -Fx "$base/COPYRIGHT" >/dev/null
            tar -tzf "$file" | grep -Fx "$base/THIRD_PARTY_NOTICES.txt" >/dev/null
            tar -tzf "$file" | grep -Fx "$base/SOURCE.txt" >/dev/null
            if tar -tzf "$file" | grep -Eiq "$forbidden"; then
                echo "release verification failed: $file contains certificate material" >&2
                exit 1
            fi
            ;;
    esac
    test -s "$file"
done < scripts/tui-targets.txt

if [[ -f client/dist/version.json ]]; then
    jq -e --arg expected "$version+prod" '.version == $expected and .label == "prod"' client/dist/version.json >/dev/null
fi
if [[ -f go/graphite-meter ]]; then
    server_log="$tmp/server.log"
    GM_H1_ADDR=127.0.0.1:7246 GM_H1_TLS_ADDR= GM_H2_ADDR= GM_H3_ADDR= \
        ./go/graphite-meter >"$server_log" 2>&1 &
    server_pid=$!
    ready=false
    for _ in $(seq 1 30); do
        if curl -fsS http://127.0.0.1:7246/preflight >"$tmp/preflight.json"; then ready=true; break; fi
        sleep 1
    done
    if [[ "$ready" != true ]]; then
        cat "$server_log" >&2
        exit 1
    fi
    jq -e --arg version "$version" '.engineVersion == $version' "$tmp/preflight.json" >/dev/null
    kill "$server_pid" 2>/dev/null || true
fi
echo "release verification passed: $version"
