#!/usr/bin/env bash
set -euo pipefail

image=${1:?usage: scripts/verify-container.sh IMAGE}
version=${GM_VERIFY_VERSION:-0.0.0-ci}
label=${GM_VERIFY_LABEL:-prod}
revision=${GM_VERIFY_REVISION:-local}
engine=${CONTAINER_ENGINE:-$(command -v docker || command -v podman || true)}
if ! command -v "$engine" >/dev/null 2>&1; then
    echo "container verification requires Docker or Podman" >&2
    exit 2
fi
container="graphite-meter-verify-$$"
tmp=$(mktemp -d)

cleanup() {
    status=$?
    [ "$status" -eq 0 ] || "$engine" logs "$container" 2>&1 || true
    "$engine" rm -f "$container" >/dev/null 2>&1 || true
    rm -rf "$tmp"
    exit "$status"
}
trap cleanup EXIT

refute() {
    if grep -Eqi "$1" "$2"; then
        echo "unexpected '$1' in $2" >&2
        exit 1
    fi
}

"$engine" run -d --name "$container" -p 127.0.0.1::7246 "$image" >/dev/null
port=$("$engine" port "$container" 7246/tcp | sed -n 's/.*:\([0-9][0-9]*\)$/\1/p')
base="http://127.0.0.1:${port:?container port is not published}"
for _ in $(seq 30); do
    curl -fsS -o /dev/null "$base/preflight" && break
    sleep 1
done

origin="http://127.0.0.1:7246"
curl -fsS "$base/preflight" | jq -e --arg origin "$origin" --arg version "$version" '
  .engineVersion == $version and (.generation | type == "string" and length > 0)
  and .capabilities.throughput == [{"baseUrl": $origin, "transport": "fetch-stream", "protocol": "http1"}]
  and .capabilities.latency == [{"baseUrl": $origin, "transport": "websocket"}]
  and .server.name == "graphite-meter"'
curl -fsS "$base/probe" | jq -e '
  (.clientIp | type == "string" and length > 0) and (.clientIpVersion == 4 or .clientIpVersion == 6)
  and (.clientIpSource | type == "string" and length > 0) and .protocolNegotiated == "http/1.1"'
curl -fsS "$base/version.json" | jq -e --arg version "$version" --arg label "$label" \
    --arg revision "$revision" '.version == $version and .label == $label and .revision == $revision'

"$engine" export "$container" -o "$tmp/rootfs.tar"
while read -r path marker; do
    tar -xOf "$tmp/rootfs.tar" "$path" | grep -F -- "$marker" >/dev/null
done <<'EOF'
etc/ssl/certs/ca-certificates.crt
usr/share/licenses/ca-certificates/COPYRIGHT
usr/share/licenses/graphite-meter/LICENSE GNU AFFERO GENERAL PUBLIC LICENSE
usr/share/licenses/graphite-meter/COPYRIGHT Graphite Meter
usr/share/licenses/graphite-meter/THIRD_PARTY_NOTICES.txt THIRD-PARTY SOFTWARE NOTICES
usr/share/licenses/graphite-meter/SOURCE.txt https://github.com/zR-JB/graphite-meter
EOF
licenses='{{ index .Config.Labels "org.opencontainers.image.licenses" }}'
test "$("$engine" inspect -f "$licenses" "$image")" = AGPL-3.0-or-later

curl -fsS "$base/" -o "$tmp/index.html"
grep -qi '<script[^>]*type="module"' "$tmp/index.html"
grep -qi '<div id="app"' "$tmp/index.html"
refute '/src/main.ts' "$tmp/index.html"
grep -oE '(href|src)="/[^"]*"' "$tmp/index.html" | cut -d'"' -f2 | sort -u >"$tmp/assets"
grep -Eq '^/assets/.+\.js$' "$tmp/assets"
grep -Eq '^/assets/.+\.css$' "$tmp/assets"
while IFS= read -r path; do
    type=$(curl -fsS -o "$tmp/body" -w '%{content_type}' "$base$path")
    test -s "$tmp/body"
    refute '<!doctype html|<html' "$tmp/body"
    case "$path" in
        *.js) grep -qi javascript <<<"$type" ;;
        *.css) grep -qi '^text/css' <<<"$type" ;;
        *.woff2) grep -Eqi 'font/woff2|application/octet-stream' <<<"$type" ;;
        *.svg) grep -Eqi 'image/svg\+xml|text/xml' <<<"$type" ;;
    esac
done <"$tmp/assets"

for path in /settings/ /assets/missing.js; do
    test "$(curl -sS -o "$tmp/missing" -w '%{http_code}' "$base$path")" = 404
    refute '<!doctype html|<html|<div id="app"' "$tmp/missing"
done
echo "container verification passed: $image"
