// Package transport holds client attribution, QUIC defaults and WebTransport stream unblocking.
package transport

import (
	"net"
	"net/http"
	"net/netip"
	"slices"
	"strings"
	"sync/atomic"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/logx"
)

type ClientIPSource string

const (
	ClientIPSocket    ClientIPSource = "socket"
	ClientIPForwarded ClientIPSource = "forwarded"
)

type ClientAddress struct {
	Addr    netip.Addr
	Version int
	Source  ClientIPSource
}

// RefusedAddress answers a trusted proxy's request that names no client; the server log says why.
const RefusedAddress = "client address unknown: the trusted reverse proxy's X-Real-IP is missing or does not match " +
	"its X-Forwarded-For; the server log names the fault"

// ResolveClientAddress uses the peer, or a trusted proxy's single X-Real-IP; ok reports usable evidence.
func ResolveClientAddress(r *http.Request, trusted []netip.Prefix) (ClientAddress, bool) {
	peer, ok := Peer(r.RemoteAddr)
	if !ok {
		return ClientAddress{Source: ClientIPSocket}, false
	}
	socket := clientAddress(peer, ClientIPSocket)
	if !Trusted(peer, trusted) {
		return socket, true
	}
	addr, fault := forwardedClient(r.Header)
	if fault != "" {
		warnRefusal(peer, fault)
		return socket, false
	}
	return clientAddress(addr, ClientIPForwarded), true
}

// forwardedClient reads a trusted proxy's single X-Real-IP. X-Forwarded-For, when sent, must end in that address: a
// proxy appends the peer it saw, so a different last hop means the X-Real-IP came from further out, such as a client
// whose own header the proxy passed on. Forwarded is ignored: no common proxy writes it, and some pass a client's on.
func forwardedClient(h http.Header) (netip.Addr, string) {
	real := h.Values("X-Real-IP")
	if len(real) != 1 {
		return netip.Addr{}, "the proxy must send X-Real-IP once"
	}
	addr, err := netip.ParseAddr(strings.TrimSpace(real[0]))
	if err != nil {
		return netip.Addr{}, "X-Real-IP is not one IP address"
	}
	addr = addr.Unmap()
	if chain := h.Values("X-Forwarded-For"); len(chain) > 0 {
		hops := strings.Split(chain[len(chain)-1], ",")
		if last := strings.TrimSpace(hops[len(hops)-1]); last != "" {
			if hop, ok := hopAddress(last); !ok || hop != addr {
				return netip.Addr{}, "X-Forwarded-For ends in " + last + ", not X-Real-IP " + addr.String()
			}
		}
	}
	return addr, ""
}

// hopAddress reads an X-Forwarded-For entry, which some proxies write with a port.
func hopAddress(hop string) (netip.Addr, bool) {
	if addr, err := netip.ParseAddr(hop); err == nil {
		return addr.Unmap(), true
	}
	if addrPort, err := netip.ParseAddrPort(hop); err == nil {
		return addrPort.Addr().Unmap(), true
	}
	return netip.Addr{}, false
}

// The last refusal warning's Unix second; a misconfigured proxy refuses every request, so one line a minute is enough.
var refusalWarned atomic.Int64

func warnRefusal(proxy netip.Addr, fault string) {
	now, last := time.Now().Unix(), refusalWarned.Load()
	if now-last < 60 || !refusalWarned.CompareAndSwap(last, now) {
		return
	}
	logx.Warnf("proxy", "request from trusted proxy %s names no client: %s; set the proxy to overwrite X-Real-IP "+
		"with the address it accepted the connection from (docs/DEPLOYMENT.md, Reverse proxies)", proxy, fault)
}

func Peer(addr string) (netip.Addr, bool) {
	host, _, err := net.SplitHostPort(addr)
	if err != nil {
		host = addr
	}
	ip, err := netip.ParseAddr(strings.Trim(host, "[]"))
	return ip.Unmap(), err == nil
}

// Trusted reports whether addr is one of the operator's proxies.
func Trusted(addr netip.Addr, trusted []netip.Prefix) bool {
	addr = addr.Unmap()
	return slices.ContainsFunc(trusted, func(p netip.Prefix) bool { return p.Contains(addr) })
}

// ShareFull reports whether any key holds its share: limit for the first, doubling for each wider aggregate.
func ShareFull(keys []string, limit int, held func(key string) int) bool {
	for i, key := range keys {
		if held(key) >= limit<<i {
			return true
		}
	}
	return false
}

// AddressKeys keys a client's budgets: an IPv4 address, or an IPv6 /64 and the /56 and /48 that hold it.
func AddressKeys(addr netip.Addr) []string {
	addr = addr.Unmap()
	switch {
	case !addr.IsValid():
		return []string{"unknown"}
	case addr.Is4():
		return []string{addr.String()}
	}
	var keys []string
	for _, bits := range []int{64, 56, 48} {
		keys = append(keys, netip.PrefixFrom(addr, bits).Masked().String())
	}
	return keys
}

func clientAddress(addr netip.Addr, source ClientIPSource) ClientAddress {
	version := 6
	if addr.Is4() {
		version = 4
	}
	return ClientAddress{Addr: addr, Version: version, Source: source}
}
