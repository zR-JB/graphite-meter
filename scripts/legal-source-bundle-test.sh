#!/bin/sh
set -eu

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
archive="$tmp/source.tar.gz"

VERSION=development LEGAL_SOURCE_OUT="$archive" just _legal-run source-bundle
tar -tzf "$archive" | grep -Fx \
    graphite-meter_development_corresponding-source/project/LICENSE
tar -tzf "$archive" | grep -Fx \
    graphite-meter_development_corresponding-source/LEGAL_INVENTORY.json
tar -tzf "$archive" | grep -q '/third_party/go/'
tar -tzf "$archive" | grep -q '/third_party/npm/'
if tar -tzf "$archive" | grep -q '.dev-certs'; then
    echo 'development certificates leaked into source bundle' >&2
    exit 1
fi
