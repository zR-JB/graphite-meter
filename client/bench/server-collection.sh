#!/usr/bin/env bash
set -euo pipefail

# Called by mise with its pinned runtimes and already built servers and clients.
test -n "${BUN_CHROME_PATH:?Set BUN_CHROME_PATH to the pinned Chrome for Testing binary}"
for program in unshare nsenter ip tc ethtool openssl curl; do
  command -v "$program" >/dev/null
done
root=$(cd "$(dirname "$0")/../.." && pwd)
certs=$(mktemp -d)
trap 'rm -rf "$certs"' EXIT
export GM_E2E_SERVER_BIN="$root/go/graphite-meter"
export GM_E2E_TLS_CERT="$certs/cert.pem" GM_E2E_TLS_KEY="$certs/key.pem" SSL_CERT_FILE="$certs/ca.pem"
openssl req -x509 -newkey rsa:2048 -nodes -days 1 -keyout "$certs/ca.key" -out "$SSL_CERT_FILE" \
  -subj /CN=graphite-meter-benchmark-ca -addext basicConstraints=critical,CA:TRUE \
  -addext keyUsage=critical,keyCertSign 2>/dev/null
openssl req -new -newkey rsa:2048 -nodes -keyout "$GM_E2E_TLS_KEY" -subj /CN=graphite-meter-benchmark 2>/dev/null |
  openssl x509 -req -CA "$SSL_CERT_FILE" -CAkey "$certs/ca.key" -set_serial 1 -days 1 -out "$GM_E2E_TLS_CERT" \
    -extfile <(printf '%s\n' basicConstraints=critical,CA:FALSE keyUsage=critical,digitalSignature,keyEncipherment \
      extendedKeyUsage=serverAuth subjectAltName=IP:10.81.1.2,IP:10.81.2.2,IP:10.81.3.2,IP:10.81.4.2) 2>/dev/null
GM_E2E_SPKI=$(openssl x509 -in "$GM_E2E_TLS_CERT" -pubkey -noout |
  openssl pkey -pubin -outform der | openssl dgst -sha256 -binary | openssl enc -base64)
export GM_E2E_SPKI
export GM_MULTI_BENCH_OUTPUT="${GM_MULTI_BENCH_OUTPUT:-$(mktemp -d -t graphite-meter-servers.XXXXXX)}"
cd "$root/client"
printf 'Writing measurement evidence to %s\n' "$GM_MULTI_BENCH_OUTPUT"
# The PID namespace ends every server, client and browser together with the rig.
unshare --user --map-root-user --net --pid --fork --kill-child --mount-proc python3 bench/server-collection-rig.py
