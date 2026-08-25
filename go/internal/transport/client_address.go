package transport

import (
	"net/http"
	"net/netip"
	"strconv"
	"strings"
)

// ClientIPSource names the origin of a resolved client address.
type ClientIPSource string

const (
	// ClientIPSocket is the peer address of the connection itself.
	ClientIPSocket ClientIPSource = "socket"
	// ClientIPForwarded is an address taken from a proxy header.
	ClientIPForwarded ClientIPSource = "forwarded"
)

// ClientAddress is the address a request is attributed to, with its provenance.
type ClientAddress struct {
	Addr    netip.Addr
	Version int
	Source  ClientIPSource
}

// ResolveClientAddress attributes a request to a client IP. Proxy headers count
// only when the socket peer is itself trusted, so a client-supplied prefix
// cannot spoof its own address. A malformed or obfuscated chain falls back to
// the socket peer rather than guessing.
func ResolveClientAddress(r *http.Request, trusted []netip.Prefix) ClientAddress {
	peer, ok := parseAddress(r.RemoteAddr)
	if !ok {
		return ClientAddress{Source: ClientIPSocket}
	}
	fromSocket := clientAddress(peer, ClientIPSocket)
	if !contains(trusted, peer) {
		return fromSocket
	}

	chain, ok := forwardedChain(r.Header)
	if !ok {
		return fromSocket
	}
	return clientAddress(firstUntrustedHop(peer, chain, trusted), ClientIPForwarded)
}

// firstUntrustedHop walks the chain right to left starting at peer and returns
// the first entry outside trusted. An all-trusted chain yields its leftmost
// entry.
func firstUntrustedHop(peer netip.Addr, chain []netip.Addr, trusted []netip.Prefix) netip.Addr {
	current := peer
	for i := len(chain) - 1; i >= 0 && contains(trusted, current); i-- {
		current = chain[i]
	}
	return current
}

func clientAddress(addr netip.Addr, source ClientIPSource) ClientAddress {
	version := 6
	if addr.Is4() {
		version = 4
	}
	return ClientAddress{Addr: addr, Version: version, Source: source}
}

// forwardedChain returns the proxy chain in client-to-proxy order. One header
// wins outright: a malformed one is never rescued by another that a nearer hop
// may write. X-Real-IP is consulted first because it is the header the trusted
// proxy is configured to overwrite from its own peer (docs/REVERSE_PROXY.md);
// ranking a client-settable Forwarded above it would let a failed strip
// upstream override the one value the proxy actually vouches for.
func forwardedChain(h http.Header) ([]netip.Addr, bool) {
	if raw := h.Get("X-Real-IP"); raw != "" {
		addr, ok := parseAddress(raw)
		if !ok {
			return nil, false
		}
		return []netip.Addr{addr}, true
	}
	if raw := h.Get("Forwarded"); raw != "" {
		elements, ok := splitQuoted(raw, ',')
		if !ok {
			return nil, false
		}
		chain := make([]netip.Addr, 0, len(elements))
		for _, element := range elements {
			params, ok := splitQuoted(element, ';')
			if !ok {
				return nil, false
			}
			var forwardedFor string
			for _, param := range params {
				key, value, found := strings.Cut(param, "=")
				if found && strings.EqualFold(strings.TrimSpace(key), "for") {
					forwardedFor = strings.TrimSpace(value)
					break
				}
			}
			addr, ok := parseAddress(forwardedFor)
			if !ok {
				return nil, false
			}
			chain = append(chain, addr)
		}
		return chain, len(chain) > 0
	}
	if raw := h.Get("X-Forwarded-For"); raw != "" {
		parts := strings.Split(raw, ",")
		chain := make([]netip.Addr, 0, len(parts))
		for _, part := range parts {
			addr, ok := parseAddress(part)
			if !ok {
				return nil, false
			}
			chain = append(chain, addr)
		}
		return chain, len(chain) > 0
	}
	return nil, false
}

// parseAddress accepts the address forms proxies emit: bare, RFC 7239 quoted,
// bracketed IPv6, and host:port. RFC 7239 obfuscated identifiers ("_secret")
// and "unknown" name no host, so they are rejected and the caller falls back
// rather than treating them as a client.
func parseAddress(raw string) (netip.Addr, bool) {
	raw = strings.TrimSpace(raw)
	if len(raw) >= 2 && raw[0] == '"' {
		value, err := strconv.Unquote(raw)
		if err != nil {
			return netip.Addr{}, false
		}
		raw = value
	}
	if raw == "" || strings.EqualFold(raw, "unknown") || strings.HasPrefix(raw, "_") {
		return netip.Addr{}, false
	}
	if addr, err := netip.ParseAddr(raw); err == nil {
		return addr.Unmap(), true
	}
	if len(raw) >= 2 && raw[0] == '[' && raw[len(raw)-1] == ']' {
		if addr, err := netip.ParseAddr(raw[1 : len(raw)-1]); err == nil {
			return addr.Unmap(), true
		}
	}
	if addrPort, err := netip.ParseAddrPort(raw); err == nil {
		return addrPort.Addr().Unmap(), true
	}
	return netip.Addr{}, false
}

func contains(prefixes []netip.Prefix, addr netip.Addr) bool {
	for _, prefix := range prefixes {
		if prefix.Contains(addr) {
			return true
		}
	}
	return false
}

// splitQuoted splits on separator outside RFC 7239 quoted-strings, reporting
// false for an unterminated quote or trailing escape so a truncated header is
// rejected instead of parsed as something shorter.
func splitQuoted(raw string, separator byte) ([]string, bool) {
	var parts []string
	start, quoted, escaped := 0, false, false
	for i := range len(raw) {
		switch {
		case escaped:
			escaped = false
		case quoted && raw[i] == '\\':
			escaped = true
		case raw[i] == '"':
			quoted = !quoted
		case raw[i] == separator && !quoted:
			parts = append(parts, strings.TrimSpace(raw[start:i]))
			start = i + 1
		}
	}
	if quoted || escaped {
		return nil, false
	}
	parts = append(parts, strings.TrimSpace(raw[start:]))
	return parts, true
}
