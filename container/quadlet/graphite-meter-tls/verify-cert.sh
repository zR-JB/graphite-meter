#!/bin/sh
set -eu
DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
# shellcheck disable=SC1091
. "$DIR/graphite-meter.env"
certdir="$DIR/letsencrypt/live/$GM_CERT_NAME"
test -s "$certdir/fullchain.pem"
test -s "$certdir/privkey.pem"
