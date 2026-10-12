#!/bin/sh
# Print the browser client directory GM_CLIENT selects: v2, the default, or v1.
set -eu
case "${GM_CLIENT:-v2}" in
    v1) echo client ;;
    v2) echo client-v2 ;;
    *) echo "GM_CLIENT must be v1 or v2" >&2; exit 2 ;;
esac
