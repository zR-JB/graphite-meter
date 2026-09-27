#!/bin/sh
# Build the native TUI archives, the third-party source archive and their checksums.
set -eu
version=${1:?usage: scripts/release-artifacts.sh VERSION}
dist=${RELEASE_DIST:-go/dist}
case "$dist" in /*) ;; *) dist="$PWD/$dist" ;; esac
mkdir -p "$dist"
find "$dist" -maxdepth 1 -type f -delete
LEGAL_THIRD_PARTY_SOURCE_OUT="$dist/graphite-meter_${version}_third-party-source.tar.gz" \
    VERSION=$version mise run _legal-run third-party-source-bundle
while IFS= read -r target; do
    [ -n "$target" ] || continue
    scripts/package-tui.sh "$version" "${target%/*}" "${target#*/}" "$dist"
done < scripts/tui-targets.txt
case ${RUST:-none} in
    none) ;;
    server|tui|both)
        if [ "$RUST" != server ]; then
            python3 scripts/package-rust.py "$version" --container --output "$dist"
        fi
        if [ "$RUST" != tui ]; then
            stage=$(mktemp -d)
            trap 'rm -rf "$stage"' EXIT
            docker buildx build --no-cache --platform linux/amd64 --target server-artifacts \
                -f container/Dockerfile.rust --build-arg VERSION="$version" \
                --output "type=local,dest=$stage" .
            cp "$stage/THIRD_PARTY_SOURCE.tar.gz" \
                "$dist/graphite-meter-server_${version}_linux_amd64_rust_third-party-source.tar.gz"
        fi ;;
    *) echo "invalid Rust release selection" >&2; exit 1 ;;
esac
cd "$dist"
find . -maxdepth 1 -type f ! -name checksums.txt -printf '%f\n' | sort | xargs sha256sum > checksums.txt
