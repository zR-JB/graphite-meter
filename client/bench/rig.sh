#!/usr/bin/env bash
# Rig B: the server in a network namespace, the browser on the host, and netem
# on the veth ends. WebView control traffic stays on unshaped loopback, which
# is what shaping `lo` directly cannot give.
#
#   sudo ./rig.sh up lan-1g     # create and shape
#   sudo ./rig.sh shape wifi-poor
#   sudo ./rig.sh show
#   sudo ./rig.sh down
#
# Needs the ip, tc and ethtool binaries. Their packages are named differently
# per distribution: iproute2 or iproute plus iproute-tc, and ethtool.
set -euo pipefail

NS=gmbench
HOST_IF=veth-gm
NS_IF=veth-gm-ns
HOST_IP=10.77.0.1
NS_IP=10.77.0.2
PREFIX=24
# netem drops silently once its queue fills, which would turn a clean profile
# into a loss profile without saying so.
LIMIT=200000

# rate delay jitter loss gso
profile() {
  case "$1" in
  lan-1g)         echo "1gbit   0.15ms 0     0     on" ;;
  lan-2.5g)       echo "2500mbit 0.15ms 0    0     on" ;;
  lan-10g)        echo "10gbit  0.1ms  0     0     on" ;;
  lan-fast)       echo "-       0.1ms  0     0     on" ;;
  lan-fast-lossy) echo "-       0.1ms  0     0.01% off" ;;
  wifi-good)      echo "1200mbit 1.5ms 0.5ms 0.1%  off" ;;
  wifi-poor)      echo "300mbit 4ms    2.5ms 2%    off" ;;
  none)           echo "-       -      0     0     on" ;;
  *) echo "unknown profile: $1" >&2; exit 2 ;;
  esac
}

require() {
  command -v "$1" >/dev/null || {
    echo "missing $1: install iproute2/iproute-tc and ethtool (Fedora: sudo dnf install iproute-tc ethtool)" >&2
    exit 1
  }
}

up() {
  require ip
  ip netns add "$NS" 2>/dev/null || true
  ip link add "$HOST_IF" type veth peer name "$NS_IF" 2>/dev/null || true
  # After a dirty exit the peer is already inside the namespace, where moving it
  # again fails. Only move the end that is still on the host.
  if ip link show "$NS_IF" >/dev/null 2>&1; then
    ip link set "$NS_IF" netns "$NS"
  fi
  ip addr add "$HOST_IP/$PREFIX" dev "$HOST_IF" 2>/dev/null || true
  ip link set "$HOST_IF" up
  ip netns exec "$NS" ip addr add "$NS_IP/$PREFIX" dev "$NS_IF" 2>/dev/null || true
  ip netns exec "$NS" ip link set "$NS_IF" up
  ip netns exec "$NS" ip link set lo up
  shape "${1:-none}"
}

# One qdisc per direction: the namespace side shapes download, the host side
# shapes upload, so the two are set independently.
shape() {
  require tc
  read -r rate delay jitter loss gso <<<"$(profile "$1")"

  for spec in "host:$HOST_IF:" "ns:$NS_IF:ip netns exec $NS"; do
    IFS=: read -r _ iface prefix <<<"$spec"
    # shellcheck disable=SC2086
    $prefix tc qdisc del dev "$iface" root 2>/dev/null || true
    if command -v ethtool >/dev/null; then
      # netem drops whole 64 KiB super-packets with GSO on, so one drop stands
      # in for ~44 real ones. Only the loss profiles pay the CPU to turn it off.
      # shellcheck disable=SC2086
      $prefix ethtool -K "$iface" tso "$gso" gso "$gso" gro "$gso" 2>/dev/null || true
    fi
    [ "$delay" = "-" ] && continue
    args=(delay "$delay")
    [ "$jitter" != "0" ] && args+=("$jitter")
    [ "$loss" != "0" ] && args+=(loss "$loss")
    [ "$rate" != "-" ] && args+=(rate "$rate")
    # shellcheck disable=SC2086
    $prefix tc qdisc add dev "$iface" root netem "${args[@]}" limit "$LIMIT"
  done
  echo "shaped: $1"
}

show() {
  echo "# host $HOST_IF"; tc qdisc show dev "$HOST_IF" || true
  echo "# ns $NS_IF"; ip netns exec "$NS" tc qdisc show dev "$NS_IF" || true
}

down() {
  ip link del "$HOST_IF" 2>/dev/null || true
  ip netns del "$NS" 2>/dev/null || true
  echo "rig removed"
}

case "${1:-}" in
up) up "${2:-none}" ;;
shape) shape "${2:?profile}" ;;
show) show ;;
down) down ;;
*) echo "usage: $0 {up [profile]|shape <profile>|show|down}" >&2; exit 2 ;;
esac
