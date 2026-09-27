#!/bin/sh
# Print the version a local build stamps: VERSION, else the client revision, else the short HEAD.
set -eu
version=${VERSION:-${GM_CLIENT_REVISION:-$(git rev-parse --short HEAD 2>/dev/null || echo source)}}
case "$version" in
    *[!A-Za-z0-9._+-]*) echo "Invalid build version" >&2; exit 2 ;;
esac
printf '%s\n' "$version"
