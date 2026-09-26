// Package transport holds client attribution, QUIC defaults and WebTransport stream unblocking.
package transport

import (
	"net"
	"net/http"
	"net/netip"
	"slices"
	"strings"
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

// ResolveClientAddress attributes a request to its socket peer or, behind a trusted proxy, to the one X-Real-IP the
// proxy set. ok is false when a trusted peer's evidence is missing or ambiguous; the address is then the peer's own.
func ResolveClientAddress(r *http.Request, trusted []netip.Prefix) (ClientAddress, bool) {
	host, _, err := net.SplitHostPort(r.RemoteAddr)
	if err != nil {
		host = r.RemoteAddr
	}
	peer, err := netip.ParseAddr(strings.Trim(host, "[]"))
	if err != nil {
		return ClientAddress{Source: ClientIPSocket}, false
	}
	socket := clientAddress(peer.Unmap(), ClientIPSocket)
	if !Trusted(peer, trusted) {
		return socket, true
	}
	values := r.Header.Values("X-Real-IP")
	if len(values) != 1 || r.Header.Get("Forwarded") != "" || r.Header.Get("X-Forwarded-For") != "" {
		return socket, false
	}
	addr, err := netip.ParseAddr(strings.TrimSpace(values[0]))
	if err != nil {
		return socket, false
	}
	return clientAddress(addr.Unmap(), ClientIPForwarded), true
}

// Trusted reports whether addr is one of the operator's proxies.
func Trusted(addr netip.Addr, trusted []netip.Prefix) bool {
	addr = addr.Unmap()
	return slices.ContainsFunc(trusted, func(p netip.Prefix) bool { return p.Contains(addr) })
}

// AddressBucket keys a per-client budget: an IPv4 address or an IPv6 /64.
func AddressBucket(addr netip.Addr) string {
	addr = addr.Unmap()
	switch {
	case !addr.IsValid():
		return "unknown"
	case addr.Is6():
		return netip.PrefixFrom(addr, 64).Masked().String()
	default:
		return addr.String()
	}
}

func clientAddress(addr netip.Addr, source ClientIPSource) ClientAddress {
	version := 6
	if addr.Is4() {
		version = 4
	}
	return ClientAddress{Addr: addr, Version: version, Source: source}
}
