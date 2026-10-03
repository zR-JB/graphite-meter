#!/usr/bin/env bash
set -euo pipefail

image=${1:?usage: VERSION=... GM_CLIENT_REVISION=... [ENGINE_VERSION=...] scripts/verify-container.sh IMAGE}
version=${VERSION:?}
revision=${GM_CLIENT_REVISION:?}
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
curl -fsS "$base/preflight" | jq -e --arg origin "$origin" --arg version "${ENGINE_VERSION:-$version}" '
  .engineVersion == $version and (.generation | type == "string" and length > 0)
  and .capabilities.throughput == [{"baseUrl": $origin, "transport": "fetch-stream", "protocol": "http1"}]
  and .capabilities.latency == [{"baseUrl": $origin, "transport": "websocket"}]
  and .server.name == "graphite-meter"'
curl -fsS "$base/probe" | jq -e '
  (.clientIp | type == "string" and length > 0) and (.clientIpVersion == 4 or .clientIpVersion == 6)
  and (.clientIpSource | type == "string" and length > 0) and .protocolNegotiated == "http/1.1"'
curl -fsS "$base/version.json" | jq -e --arg version "$version" --arg revision "$revision" \
    '.version == $version and .label == "prod" and .revision == $revision'

"$engine" export "$container" -o "$tmp/rootfs.tar"
while read -r path marker; do
    tar -xOf "$tmp/rootfs.tar" "$path" | grep -F -- "$marker" >/dev/null
done <<'EOF'
etc/ssl/certs/ca-certificates.crt
usr/share/licenses/graphite-meter/LICENSE GNU AFFERO GENERAL PUBLIC LICENSE
usr/share/licenses/graphite-meter/COPYRIGHT Graphite Meter
usr/share/licenses/graphite-meter/THIRD_PARTY_NOTICES.txt THIRD-PARTY SOFTWARE NOTICES
usr/share/licenses/graphite-meter/SOURCE.txt https://github.com/zR-JB/graphite-meter
EOF
# The Go and Rust images hold the same files: one static binary, the CA roots and the notices;
# no libc, shell or package. The engine adds only its runtime files.
tar -tvf "$tmp/rootfs.tar" | awk '$1 !~ /^d/ { print $6 }' \
    | grep -Evx '\.dockerenv|(dev|proc|sys|run)/.*|etc/(hostname|hosts|resolv\.conf|mtab)' | sort >"$tmp/files"
diff -u - "$tmp/files" <<'FILES'
etc/ssl/certs/ca-certificates.crt
graphite-meter
usr/share/licenses/ca-certificates/COPYRIGHT
usr/share/licenses/graphite-meter/COPYRIGHT
usr/share/licenses/graphite-meter/LICENSE
usr/share/licenses/graphite-meter/SOURCE.txt
usr/share/licenses/graphite-meter/THIRD_PARTY_NOTICES.txt
FILES
licenses='{{ index .Config.Labels "org.opencontainers.image.licenses" }}'
test "$("$engine" inspect -f "$licenses" "$image")" = AGPL-3.0-or-later
test "$("$engine" inspect -f '{{.Config.User}}' "$image")" = 65532:65532
# Relative GM_* paths resolve from /, and an image that names a CA bundle file
# also trusts CA files added to /etc/ssl/certs, as Go's x509 does.
case "$("$engine" inspect -f '{{.Config.WorkingDir}}' "$image")" in
    '' | /) ;;
    *) echo "the image must run from /" >&2; exit 1 ;;
esac
environment=$("$engine" inspect -f '{{range .Config.Env}}{{println .}}{{end}}' "$image")
if grep -q '^SSL_CERT_FILE=' <<<"$environment" && ! grep -qx 'SSL_CERT_DIR=/etc/ssl/certs' <<<"$environment"; then
    echo "an image that sets SSL_CERT_FILE must also set SSL_CERT_DIR=/etc/ssl/certs" >&2
    exit 1
fi
case "${ENGINE_VERSION:-$version}" in
    *-rust)
        "$engine" run --rm -e MIMALLOC_VERBOSE=1 "$image" --version >"$tmp/mimalloc-default" 2>&1
        grep -E "option 'allow_thp': 0([[:space:]]|$)" "$tmp/mimalloc-default"
        "$engine" run --rm -e MIMALLOC_VERBOSE=1 -e MIMALLOC_ALLOW_THP=2 "$image" --version >"$tmp/mimalloc-override" 2>&1
        grep -E "option 'allow_thp': 2([[:space:]]|$)" "$tmp/mimalloc-override"
        ;;
esac

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
# Report sizes the same way for both images, so they compare like for like.
disk=$("$engine" image inspect -f '{{.Size}}' "$image")
compressed=$("$engine" save "$image" | gzip -6 | wc -c)
binary=$(tar -xOf "$tmp/rootfs.tar" graphite-meter | wc -c)
sizes=$(awk -v d="$disk" -v c="$compressed" -v b="$binary" \
    'BEGIN { printf "%.2f MB | %.2f MB | %.2f MB", d / 1e6, c / 1e6, b / 1e6 }')
echo "image sizes (on disk | gzip | server binary): $sizes"
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    printf '### Container image\n\n| Image | On disk | gzip | Server binary |\n|---|---|---|---|\n| `%s` | %s |\n' \
        "$image" "$sizes" >>"$GITHUB_STEP_SUMMARY"
fi
echo "container verification passed: $image"
