// Package origin forwards to wire.OriginKey until the native client imports it directly.
package origin

import "github.com/zR-JB/graphite-meter/go/internal/wire"

func Key(raw string) string {
	if key, ok := wire.OriginKey(raw); ok {
		return key
	}
	return raw
}

func Equal(a, b string) bool { return a == b || wire.SameOrigin(a, b) }
