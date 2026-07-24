// Package origin provides the canonical identity used for HTTP endpoint catalogs.
package origin

import (
	"net"
	"net/url"
	"strings"
)

// Key normalizes scheme and host casing and removes explicit default ports.
// Non-origin values such as the relative self marker pass through untouched.
func Key(raw string) string {
	u, err := url.Parse(raw)
	if err != nil || u.Scheme == "" || u.Hostname() == "" {
		return raw
	}
	scheme := strings.ToLower(u.Scheme)
	host := strings.ToLower(u.Hostname())
	port := u.Port()
	if (scheme == "http" && port == "80") || (scheme == "https" && port == "443") {
		port = ""
	}
	if port != "" {
		host = net.JoinHostPort(host, port)
	} else if strings.Contains(host, ":") {
		// Hostname strips the brackets an IPv6 literal needs to be a valid authority.
		host = "[" + host + "]"
	}
	return scheme + "://" + host
}

// Equal reports whether two origins normalize to the same key.
func Equal(a, b string) bool {
	return Key(a) == Key(b)
}
