#!/bin/sh
set -eu

if [ "$#" -lt 3 ] || [ "$#" -gt 4 ]; then
    echo "usage: $0 VERSION GOOS GOARCH [OUTPUT_DIR]" >&2
    exit 2
fi

version=$1
goos=$2
goarch=$3
repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
output_dir=${4:-"$repo/go/dist"}
binary=graphite-meter-client
[ "$goos" = "windows" ] && binary=graphite-meter-client.exe

case "$goos/$goarch" in
    linux/amd64|linux/arm64|darwin/amd64|darwin/arm64|windows/amd64) ;;
    *) echo "unsupported TUI target: $goos/$goarch" >&2; exit 2 ;;
esac

archive_base="graphite-meter-client_${version}_${goos}_${goarch}"
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
package_dir="$stage/$archive_base"
mkdir -p "$package_dir" "$output_dir"

ldflags="-X github.com/zR-JB/graphite-meter/go/internal/goclient.Version=$version"
(
    cd "$repo/go"
    CGO_ENABLED=0 GOOS="$goos" GOARCH="$goarch" go build \
        -trimpath -ldflags "$ldflags" -o "$package_dir/$binary" ./cmd/graphite-meter-client
)

cp "$repo/LICENSE" "$package_dir/LICENSE"
cp "$repo/COPYRIGHT" "$package_dir/COPYRIGHT"
cp "$repo/legal/generated/tui/THIRD_PARTY_NOTICES.txt" "$package_dir/THIRD_PARTY_NOTICES.txt"
cp "$repo/legal/generated/tui/SOURCE.txt" "$package_dir/SOURCE.txt"

case "$goos" in
    windows) (cd "$stage" && zip -qr "$output_dir/$archive_base.zip" "$archive_base") ;;
    *) tar -czf "$output_dir/$archive_base.tar.gz" -C "$stage" "$archive_base" ;;
esac
