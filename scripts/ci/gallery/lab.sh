#!/usr/bin/env bash
# Temporary gallery lab: lab.sh PROFILE CMD... runs CMD as the runner's user inside the gm-client namespace, with the
# servers outside it, each behind its own veth whose two directions model one client's path into a home network.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
work=$here/work
profile=$1
shift
ns=gm-client
sudo ip netns add "$ns"
sudo ip netns exec "$ns" ip link set lo up
# netem holds the one-way delay and a token bucket under it the rate, its queue bounded to 4 ms of data, so load
# builds real queueing delay whatever size the kernel's segments are.
shape() { # dev rate-gbit delay-ms [namespace]
  local run=(sudo)
  [ -n "${4:-}" ] && run=(sudo ip netns exec "$4")
  "${run[@]}" tc qdisc replace dev "$1" root handle 1: netem delay "${3}ms"
  "${run[@]}" tc qdisc replace dev "$1" parent 1:1 handle 10: tbf rate "${2}gbit" burst 256kb latency 4ms
}
path() { # n down-gbit up-gbit one-way-delay-ms
  sudo ip link add "gs$1" type veth peer name "gc$1"
  sudo ip link set "gc$1" netns "$ns"
  sudo ip addr add "10.77.$1.1/24" dev "gs$1"
  sudo ip link set "gs$1" up
  sudo ip netns exec "$ns" ip addr add "10.77.$1.2/24" dev "gc$1"
  sudo ip netns exec "$ns" ip link set "gc$1" up
  shape "gs$1" "$2" "$4"
  shape "gc$1" "$3" "$4" "$ns"
}
case $profile in
  laptop) path 1 3.4 1.9 1 ;;
  workstation) path 1 9.4 9.4 0 ;;
  phone) path 1 1.65 0.86 1.5 ;;
  multi) path 1 1.3 0.75 1; path 2 1.1 0.6 1.5; path 3 1.0 0.55 2 ;;
  signin) ;;
  *) echo "unknown profile $profile" >&2; exit 2 ;;
esac
serve() { # n name location [catalog]
  GM_SERVER_NAME=$2 GM_SERVER_LOCATION=$3 GM_TLS_CERT=$work/cert.pem GM_TLS_KEY=$work/key.pem \
    GM_H1_ADDR=10.77.$1.1:8080 GM_H1_TLS_ADDR=10.77.$1.1:8443 GM_H2_ADDR=10.77.$1.1:8444 GM_H3_ADDR=10.77.$1.1:8445 \
    GM_ADVERTISED_NATIVE_ENDPOINTS=http1-tls,http2,http3 GM_SERVER_CATALOG="${4:-[]}" \
    "$work/graphite-meter" >"$work/server-$1.log" 2>&1 &
  echo $! >"$work/server-$1.pid"
}
case $profile in
  multi)
    serve 2 "Office NAS" "Study"
    serve 3 "Mini PC" "Living room"
    serve 1 "Home server" "Basement rack" \
      '{"servers":[{"id":"nas","name":"Office NAS","url":"https://10.77.2.1:8443"},{"id":"mini","name":"Mini PC","url":"https://10.77.3.1:8443"}]}'
    ;;
  signin)
    # The sign-in pages from a private server on loopback.
    hash=$(printf '%s\n%s\n' gallery-password gallery-password | "$work/graphite-meter" hash-password)
    GM_SERVER_NAME="Home server" GM_TLS_CERT=$work/cert.pem GM_TLS_KEY=$work/key.pem GM_H1_TLS_ADDR=127.0.0.1:8898 \
      GM_ADVERTISED_NATIVE_ENDPOINTS=http1-tls,http2,http3 GM_AUTH_MODE=password GM_AUTH_PUBLIC_URL=https://127.0.0.1:8898 \
      GM_AUTH_PASSWORD_HASH="$hash" \
      "$work/graphite-meter" >"$work/server-signin.log" 2>&1 &
    echo $! >"$work/server-1.pid"
    ;;
  *) serve 1 "Home server" "Basement rack" ;;
esac
sleep 1
set +e
if [ "$profile" = signin ]; then
  env GM_LAB_PROFILE="$profile" "$@"
else
  sudo ip netns exec "$ns" sudo -E -u "$USER" env PATH="$PATH" HOME="$HOME" GM_LAB_PROFILE="$profile" "$@"
fi
status=$?
for pid in "$work"/server-*.pid; do
  kill "$(cat "$pid")" 2>/dev/null
  rm -f "$pid"
done
sudo ip netns del "$ns"
exit $status
