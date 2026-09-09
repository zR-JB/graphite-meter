#!/bin/sh
set -eu

# Each suite owns its Chrome process and server fleet. In particular, the dropout
# test stops a server, and performance measurements need an otherwise idle runner.
case "${GM_E2E_SUITE:-all}" in
  connections) set -- e2e/transports.test.ts e2e/connections.test.ts "$@" ;;
  measurement) set -- e2e/multi-server.test.ts "$@" ;;
  authentication)
    # Five password logins consume the per-address minute budget. Peer approval
    # owns a fresh fleet rather than depending on delays in unrelated tests.
    bun test e2e/authenticated-home.test.ts "$@"
    set -- e2e/authentication.test.ts "$@"
    ;;
  performance) set -- e2e/client-performance.test.ts "$@" ;;
  all)
    for suite in connections measurement authentication performance; do
      GM_E2E_SUITE="$suite" sh scripts/e2e.sh "$@"
    done
    exit 0
    ;;
  *) echo "Unknown GM_E2E_SUITE: $GM_E2E_SUITE" >&2; exit 2 ;;
esac
exec bun test "$@"
