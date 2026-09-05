#!/usr/bin/env bash
set -euo pipefail

image=${1:?usage: scripts/verify-container.sh IMAGE}
container="graphite-meter-verify-$$"
tmp=$(mktemp -d)
status=1
expected_version=${GM_VERIFY_VERSION:-0.0.0-ci}
expected_label=${GM_VERIFY_LABEL:-prod}
expected_revision=${GM_VERIFY_REVISION:-local}
if [ -n "${CONTAINER_ENGINE:-}" ]; then
    engine=$CONTAINER_ENGINE
elif command -v docker >/dev/null 2>&1; then
    engine=docker
elif command -v podman >/dev/null 2>&1; then
    engine=podman
else
    echo "container verification requires Docker or Podman; set CONTAINER_ENGINE explicitly if needed" >&2
    exit 2
fi
if ! command -v "$engine" >/dev/null 2>&1; then
    echo "container verification engine '$engine' was not found" >&2
    exit 2
fi

cleanup() {
    status=$?
    if [ "$status" -ne 0 ]; then
        "$engine" logs "$container" 2>&1 || true
    fi
    "$engine" rm -f "$container" >/dev/null 2>&1 || true
    rm -rf "$tmp"
    exit "$status"
}
trap cleanup EXIT

"$engine" run -d --name "$container" -p 127.0.0.1::7246 "$image" >/dev/null
port=$("$engine" port "$container" 7246/tcp | sed -n 's/.*:\([0-9][0-9]*\)$/\1/p')
test -n "$port"
base="http://127.0.0.1:$port"

for _ in $(seq 1 30); do
    if curl -fsS -o /dev/null "$base/preflight"; then
        break
    fi
    sleep 1
done
curl -fsS -o /dev/null "$base/preflight"

curl -fsS "$base/preflight" | tee "$tmp/preflight.json" >/dev/null
expected_origin="http://127.0.0.1:7246"
jq -e --arg expected_origin "$expected_origin" --arg expected_version "$expected_version" '
  .engineVersion == $expected_version
  and (.generation | type == "string" and length > 0)
  and .capabilities.throughput == [{"baseUrl": $expected_origin, "transport": "fetch-stream", "protocol": "http1"}]
  and .capabilities.latency == [{"baseUrl": $expected_origin, "transport": "websocket"}]
  and .server.name == "graphite-meter"
' "$tmp/preflight.json"

curl -fsS "$base/probe" | tee "$tmp/probe.json" >/dev/null
jq -e '(.clientIp | type == "string" and length > 0) and (.clientIpVersion == 4 or .clientIpVersion == 6) and (.clientIpSource | type == "string" and length > 0) and .protocolNegotiated == "http/1.1"' "$tmp/probe.json"

curl -fsS "$base/version.json" | tee "$tmp/version.json" >/dev/null
jq -e --arg expected_version "$expected_version" --arg expected_label "$expected_label" --arg expected_revision "$expected_revision" \
    '.version == $expected_version and .label == $expected_label and .revision == $expected_revision' "$tmp/version.json"

"$engine" export "$container" -o "$tmp/rootfs.tar"
tar -tf "$tmp/rootfs.tar" > "$tmp/files"
for path in \
    etc/ssl/certs/ca-certificates.crt \
    usr/share/licenses/graphite-meter/LICENSE \
    usr/share/licenses/graphite-meter/COPYRIGHT \
    usr/share/licenses/graphite-meter/THIRD_PARTY_NOTICES.txt \
    usr/share/licenses/graphite-meter/SOURCE.txt \
    usr/share/licenses/ca-certificates/COPYRIGHT
do
    grep -Fx "$path" "$tmp/files" >/dev/null
done
tar -xOf "$tmp/rootfs.tar" usr/share/licenses/graphite-meter/LICENSE > "$tmp/LICENSE"
tar -xOf "$tmp/rootfs.tar" usr/share/licenses/graphite-meter/COPYRIGHT > "$tmp/COPYRIGHT"
tar -xOf "$tmp/rootfs.tar" usr/share/licenses/graphite-meter/THIRD_PARTY_NOTICES.txt > "$tmp/THIRD_PARTY_NOTICES.txt"
tar -xOf "$tmp/rootfs.tar" usr/share/licenses/graphite-meter/SOURCE.txt > "$tmp/SOURCE.txt"
tar -xOf "$tmp/rootfs.tar" usr/share/licenses/ca-certificates/COPYRIGHT > "$tmp/CA_COPYRIGHT"
grep -F 'GNU AFFERO GENERAL PUBLIC LICENSE' "$tmp/LICENSE" >/dev/null
grep -F 'Graphite Meter' "$tmp/COPYRIGHT" >/dev/null
grep -F 'THIRD-PARTY SOFTWARE NOTICES' "$tmp/THIRD_PARTY_NOTICES.txt" >/dev/null
grep -F 'https://github.com/zR-JB/graphite-meter' "$tmp/SOURCE.txt" >/dev/null
test -s "$tmp/CA_COPYRIGHT"
test "$("$engine" inspect -f '{{ index .Config.Labels "org.opencontainers.image.licenses" }}' "$image")" = 'AGPL-3.0-or-later'

curl -fsS "$base/" -o "$tmp/index.html"
grep -qi '<script[^>]*type="module"' "$tmp/index.html"
! grep -q '/src/main.ts' "$tmp/index.html"

python3 - "$tmp/index.html" > "$tmp/client-assets.txt" <<'PY'
from html.parser import HTMLParser
import sys

class AssetParser(HTMLParser):
    def __init__(self):
        super().__init__()
        self.urls = set()

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        for key in ("href", "src"):
            value = attrs.get(key)
            if value and value.startswith("/"):
                self.urls.add(value)

parser = AssetParser()
with open(sys.argv[1], encoding="utf-8") as f:
    parser.feed(f.read())
for url in sorted(parser.urls):
    print(url)
PY

grep -Eq '^/assets/.+\.js$' "$tmp/client-assets.txt"
grep -Eq '^/assets/.+\.css$' "$tmp/client-assets.txt"
while IFS= read -r path; do
    headers=$(mktemp)
    body=$(mktemp)
    curl -fsS -D "$headers" -o "$body" "$base$path"
    test "$(wc -c < "$body")" -gt 0
    content_type=$(tr -d '\r' < "$headers" | awk 'BEGIN { IGNORECASE = 1 } /^content-type:/ { print $2; exit }')
    case "$path" in
        *.js) echo "$content_type" | grep -Eqi 'javascript' ;;
        *.css) echo "$content_type" | grep -Eqi '^text/css' ;;
        *.woff2) echo "$content_type" | grep -Eqi 'font/woff2|application/octet-stream' ;;
        *.svg) echo "$content_type" | grep -Eqi 'image/svg\+xml|text/xml' ;;
    esac
    ! grep -Eqi '<!doctype html|<html' "$body"
    rm -f "$headers" "$body"
done < "$tmp/client-assets.txt"

curl -fsS "$base/#/settings" -o "$tmp/hash-route.html"
grep -qi '<div id="app"' "$tmp/hash-route.html"
direct_route_status=$(curl -sS -o "$tmp/direct-route.txt" -w '%{http_code}' "$base/settings/")
test "$direct_route_status" = 404
! grep -Eqi '<!doctype html|<html|<div id="app"' "$tmp/direct-route.txt"
missing_status=$(curl -sS -o "$tmp/missing-asset.txt" -w '%{http_code}' "$base/assets/missing.js")
test "$missing_status" = 404
! grep -Eqi '<!doctype html|<html' "$tmp/missing-asset.txt"

echo "container verification passed: $image"
