#!/bin/sh
# Print the browser client directory GM_CLIENT selects: v1, the default, or v2.
set -eu
case "${GM_CLIENT:-v1}" in
    v1) echo client ;;
    v2) echo client-v2 ;;
    *) echo "GM_CLIENT must be v1 or v2" >&2; exit 2 ;;
esac
