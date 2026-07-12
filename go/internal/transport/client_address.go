package transport

import (
	"net/http"
	"net/netip"
	"strconv"
	"strings"
)

type ClientIPSource string

const (
	ClientIPSocket    ClientIPSource = "socket"
	ClientIPForwarded ClientIPSource = "forwarded"
)

type ClientAddress struct {
	Addr   netip.Addr
	Source ClientIPSource
}

func ResolveClientAddress(r *http.Request, trusted []netip.Prefix) ClientAddress {
	peer, ok := parseAddress(r.RemoteAddr)
	if !ok {
		return ClientAddress{Source: ClientIPSocket}
	}
	result := ClientAddress{Addr: peer, Source: ClientIPSocket}
	if !contains(trusted, peer) {
		return result
	}

	chain, present, valid := forwardedChain(r.Header)
	if !present || !valid {
		return result
	}
	current := peer
	for i := len(chain) - 1; i >= 0 && contains(trusted, current); i-- {
		current = chain[i]
	}
	return ClientAddress{Addr: current, Source: ClientIPForwarded}
}

func forwardedChain(h http.Header) ([]netip.Addr, bool, bool) {
	if raw := h.Get("Forwarded"); raw != "" {
		values, ok := splitQuoted(raw, ',')
		if !ok {
			return nil, true, false
		}
		chain := make([]netip.Addr, 0, len(values))
		for _, element := range values {
			params, ok := splitQuoted(element, ';')
			if !ok {
				return nil, true, false
			}
			var value string
			for _, param := range params {
				key, v, found := strings.Cut(param, "=")
				if found && strings.EqualFold(strings.TrimSpace(key), "for") {
					value = strings.TrimSpace(v)
					break
				}
			}
			addr, ok := parseAddress(value)
			if !ok {
				return nil, true, false
			}
			chain = append(chain, addr)
		}
		return chain, true, len(chain) > 0
	}
	if raw := h.Get("X-Forwarded-For"); raw != "" {
		parts := strings.Split(raw, ",")
		chain := make([]netip.Addr, 0, len(parts))
		for _, part := range parts {
			addr, ok := parseAddress(part)
			if !ok {
				return nil, true, false
			}
			chain = append(chain, addr)
		}
		return chain, true, len(chain) > 0
	}
	if raw := h.Get("X-Real-IP"); raw != "" {
		addr, ok := parseAddress(raw)
		if !ok {
			return nil, true, false
		}
		return []netip.Addr{addr}, true, true
	}
	return nil, false, false
}

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

func splitQuoted(raw string, separator byte) ([]string, bool) {
	var parts []string
	start, quoted, escaped := 0, false, false
	for i := 0; i < len(raw); i++ {
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
