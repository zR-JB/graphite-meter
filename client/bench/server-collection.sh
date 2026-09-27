#!/usr/bin/env bash
set -euo pipefail

# Called by mise with its pinned runtimes and an already built production server.
test -n "${BUN_CHROME_PATH:?Set BUN_CHROME_PATH to the pinned Chrome for Testing binary}"
for program in unshare nsenter ip tc ethtool openssl curl; do
  command -v "$program" >/dev/null
done
root=$(cd "$(dirname "$0")/../.." && pwd)
certs=$(mktemp -d)
trap 'rm -rf "$certs"' EXIT
export GM_E2E_SERVER_BIN="$root/go/graphite-meter"
export GM_E2E_TLS_CERT="$certs/cert.pem" GM_E2E_TLS_KEY="$certs/key.pem"
openssl req -x509 -newkey rsa:2048 -nodes -days 1 \
  -keyout "$GM_E2E_TLS_KEY" -out "$GM_E2E_TLS_CERT" \
  -subj /CN=graphite-meter-benchmark 2>/dev/null
GM_E2E_SPKI=$(openssl x509 -in "$GM_E2E_TLS_CERT" -pubkey -noout |
  openssl pkey -pubin -outform der | openssl dgst -sha256 -binary | openssl enc -base64)
export GM_E2E_SPKI
export GM_MULTI_BENCH_OUTPUT="${GM_MULTI_BENCH_OUTPUT:-$(mktemp -d -t graphite-meter-servers.XXXXXX)}"
cd "$root/client"
printf 'Writing measurement evidence to %s\n' "$GM_MULTI_BENCH_OUTPUT"
# The PID namespace ends every server and browser together with the rig.
unshare --user --map-root-user --net --pid --fork --kill-child --mount-proc python3 bench/server-collection-rig.py
