package endpoint

import (
	"net/http"
	"net/netip"

	"github.com/zR-JB/graphite-meter/go/internal/auth"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
)

func uploadOwner(r *http.Request, trusted []netip.Prefix) string {
	if p, ok := auth.PrincipalFromContext(r.Context()); ok {
		return "principal:" + p.Subject
	}
	addr := transport.ResolveClientAddress(r, trusted).Addr.Unmap()
	if !addr.IsValid() {
		return "unknown"
	}
	if addr.Is6() {
		return netip.PrefixFrom(addr, 64).Masked().String()
	}
	return addr.String()
}

func writeUploadAccessError(w http.ResponseWriter, access uploadAccess) {
	switch access {
	case uploadAccessInvalid:
		http.Error(w, "unknown upload id", http.StatusBadRequest)
	case uploadAccessGlobalFull:
		w.Header().Set("Retry-After", "1")
		http.Error(w, "upload capacity exhausted", http.StatusServiceUnavailable)
	case uploadAccessClientFull:
		w.Header().Set("Retry-After", "1")
		http.Error(w, "client upload capacity exhausted", http.StatusTooManyRequests)
	case uploadAccessOwnerMismatch:
		http.Error(w, "upload id belongs to another client", http.StatusForbidden)
	}
}
