package endpoint

import (
	"net/http"
	"net/netip"

	"github.com/zR-JB/graphite-meter/go/internal/auth"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
)

// ClientKey is the stable per-client key both upload ownership and admission
// accounting bucket by: the authenticated subject when present, else the
// client's IPv4 address or IPv6 /64. A single IPv6 allocation routinely covers
// 2^64 addresses, so the /64 is the meaningful unit.
func ClientKey(r *http.Request, trusted []netip.Prefix) string {
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

// SessionKey buckets the per-client session budget. A login is the unit, not a
// subject: one device's held sessions must not starve the same user's others.
// The login branch is pinned by TestSessionKeyUsesTheLoginNotTheSubject in
// internal/auth: only that package can build a principal with a non-empty
// LoginID, so no test here or in server catches the branch going away.
func SessionKey(r *http.Request, trusted []netip.Prefix) string {
	if p, ok := auth.PrincipalFromContext(r.Context()); ok && p.LoginID() != "" {
		return "login:" + p.LoginID()
	}
	return ClientKey(r, trusted)
}

// uploadAccessMessage is the reason a refused upload reports, over HTTP as the
// status text and over a stream as an error record.
func uploadAccessMessage(access uploadAccess) string {
	switch access {
	case uploadAccessInvalid:
		return "unknown upload id"
	case uploadAccessGlobalFull:
		return "upload capacity exhausted"
	case uploadAccessClientFull:
		return "client upload capacity exhausted"
	case uploadAccessOwnerMismatch:
		return "upload id belongs to another client"
	}
	return ""
}

// sessionOwner reads the client key from an HTTP request, or from the session
// that owns a WebTransport stream, which carries no request of its own.
func sessionOwner(s transport.Session, trusted []netip.Prefix) string {
	if _, r, ok := s.HTTP(); ok {
		return ClientKey(r, trusted)
	}
	if owner, ok := s.(interface{ ClientOwner() string }); ok {
		return owner.ClientOwner()
	}
	return ""
}

func writeUploadAccessError(w http.ResponseWriter, access uploadAccess) {
	switch access {
	case uploadAccessInvalid:
		http.Error(w, uploadAccessMessage(access), http.StatusBadRequest)
	case uploadAccessGlobalFull:
		w.Header().Set("Retry-After", "1")
		http.Error(w, uploadAccessMessage(access), http.StatusServiceUnavailable)
	case uploadAccessClientFull:
		w.Header().Set("Retry-After", "1")
		http.Error(w, uploadAccessMessage(access), http.StatusTooManyRequests)
	case uploadAccessOwnerMismatch:
		http.Error(w, uploadAccessMessage(access), http.StatusForbidden)
	default:
		// Every refusal must map to a status. One that reaches here would
		// otherwise write nothing at all, and Handle's nil return would let the
		// client read a bare 200 with an empty body as a completed upload.
		http.Error(w, "upload refused", http.StatusInternalServerError)
	}
}
