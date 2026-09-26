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

// ResolveClientAddress uses the peer, or a trusted proxy's single X-Real-IP; ok reports usable evidence.
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
