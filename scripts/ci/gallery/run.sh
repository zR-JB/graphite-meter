#!/usr/bin/env bash
# Temporary gallery run: the production server with the browser client, every scene over its shaped path, the hero.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../.." && pwd)
mkdir -p "$here/work" "$here/out"
cd "$root"
mise run server-build-prod
cp go/graphite-meter "$here/work/graphite-meter"
openssl req -x509 -newkey rsa:2048 -nodes -days 7 -keyout "$here/work/key.pem" -out "$here/work/cert.pem" \
  -subj /CN=graphite-meter -addext "subjectAltName=IP:10.77.1.1,IP:10.77.2.1,IP:10.77.3.1,IP:127.0.0.1" 2>/dev/null
bun="$(command -v bun)"
for profile in laptop multi workstation phone signin; do
  "$here/lab.sh" "$profile" "$bun" "$here/capture.ts"
done
"$bun" "$here/render.ts"
ls -la "$here/out"
