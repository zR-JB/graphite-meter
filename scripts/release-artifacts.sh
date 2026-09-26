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
cd "$dist"
find . -maxdepth 1 -type f ! -name checksums.txt -printf '%f\n' | sort | xargs sha256sum > checksums.txt
