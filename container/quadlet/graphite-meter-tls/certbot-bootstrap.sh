#!/bin/sh
set -eu

: "${GM_PUBLIC_HOST:?GM_PUBLIC_HOST is required}"
: "${GM_CERT_NAME:?GM_CERT_NAME is required}"
: "${CERTBOT_EMAIL:?CERTBOT_EMAIL is required}"

exec certbot certonly \
  --non-interactive \
  --agree-tos \
  --email "$CERTBOT_EMAIL" \
  --dns-cloudflare \
  --dns-cloudflare-credentials /run/secrets/cloudflare.ini \
  --dns-cloudflare-propagation-seconds 30 \
  --cert-name "$GM_CERT_NAME" \
  --keep-until-expiring \
  --key-type ecdsa \
  -d "$GM_PUBLIC_HOST"
