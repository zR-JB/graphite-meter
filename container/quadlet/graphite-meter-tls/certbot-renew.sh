#!/bin/sh
set -eu

# Run immediately at container startup, then twice daily. Certbot only renews
# certificates that are inside their renewal window and updates its lineage
# atomically; it does not replace a healthy certificate on every invocation.
while :; do
  certbot renew \
    --non-interactive \
    --no-random-sleep-on-renew \
    --dns-cloudflare-credentials /run/secrets/cloudflare.ini
  sleep 43200
done
