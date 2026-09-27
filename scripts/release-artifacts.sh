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
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
while IFS='/ ' read -r goos goarch _; do
    [ -n "$goos" ] || continue
    base="graphite-meter-client_${version}_${goos}_${goarch}"
    binary=graphite-meter-client
    [ "$goos" != windows ] || binary=graphite-meter-client.exe
    mkdir "$stage/$base"
    (cd go && CGO_ENABLED=0 GOOS=$goos GOARCH=$goarch go build -trimpath \
        -ldflags "-X github.com/zR-JB/graphite-meter/go/internal/goclient.Version=$version" \
        -o "$stage/$base/$binary" ./cmd/graphite-meter-client)
    cp LICENSE COPYRIGHT legal/generated/tui/THIRD_PARTY_NOTICES.txt legal/generated/tui/SOURCE.txt \
        "$stage/$base/"
    case "$goos" in
        windows) (cd "$stage" && zip -qr "$dist/$base.zip" "$base") ;;
        *) tar -czf "$dist/$base.tar.gz" -C "$stage" "$base" ;;
    esac
done < scripts/tui-targets.txt
cd "$dist"
find . -maxdepth 1 -type f ! -name checksums.txt -printf '%f\n' | sort | xargs sha256sum > checksums.txt
