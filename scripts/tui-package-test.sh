#!/bin/sh
set -eu

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
archive="$tmp/graphite-meter-client_development_linux_amd64.tar.gz"

./scripts/package-tui.sh development linux amd64 "$tmp"
for entry in \
    graphite-meter-client_development_linux_amd64/graphite-meter-client \
    graphite-meter-client_development_linux_amd64/LICENSE \
    graphite-meter-client_development_linux_amd64/COPYRIGHT \
    graphite-meter-client_development_linux_amd64/THIRD_PARTY_NOTICES.txt \
    graphite-meter-client_development_linux_amd64/SOURCE.txt
do
    tar -tzf "$archive" | grep -Fx "$entry"
done
tar -xOf "$archive" \
    graphite-meter-client_development_linux_amd64/SOURCE.txt | \
    grep -Fx 'https://github.com/zR-JB/graphite-meter'
