package endpoint

import (
	"net/http"
	"net/netip"

	"github.com/zR-JB/graphite-meter/go/internal/auth"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
)

// ClientKey keys upload ownership and admission by subject, IPv4 address, or IPv6 /64.
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

// SessionKey buckets the per-client session budget.
func SessionKey(r *http.Request, trusted []netip.Prefix) string {
	if p, ok := auth.PrincipalFromContext(r.Context()); ok && p.LoginID() != "" {
		return "login:" + p.LoginID()
	}
	return ClientKey(r, trusted)
}

func optionalPrefixes(values [][]netip.Prefix) []netip.Prefix {
	if len(values) != 0 {
		return values[0]
	}
	return nil
}

type uploadAccessInfo struct {
	message string
	code    string
	status  int
	retry   bool
}

var uploadAccessInfos = [...]uploadAccessInfo{
	uploadAccessOK:            {},
	uploadAccessInvalid:       {message: "unknown upload id", code: "invalid", status: http.StatusBadRequest},
	uploadAccessGlobalFull:    {message: "upload capacity exhausted", code: "globalFull", status: http.StatusServiceUnavailable, retry: true},
	uploadAccessClientFull:    {message: "client upload capacity exhausted", code: "clientFull", status: http.StatusTooManyRequests, retry: true},
	uploadAccessOwnerMismatch: {message: "upload id belongs to another client", code: "ownerMismatch", status: http.StatusForbidden},
}

func (access uploadAccess) info() uploadAccessInfo {
	if int(access) < len(uploadAccessInfos) {
		return uploadAccessInfos[access]
	}
	return uploadAccessInfo{}
}

func uploadAccessMessage(access uploadAccess) string { return access.info().message }

func uploadAccessCode(access uploadAccess) string { return access.info().code }

// uploadRefusalError preserves the classified refusal across transports that do not have an HTTP status line.
type uploadRefusalError struct{ access uploadAccess }

func (e *uploadRefusalError) Error() string {
	return "upload refused: " + uploadAccessMessage(e.access)
}

// sessionOwner reads the client key from an HTTP request, or from the session that owns a WebTransport stream.
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
	info := access.info()
	if info.code == "" {
		http.Error(w, "upload refused", http.StatusInternalServerError)
		return
	}
	w.Header().Set("X-Graphite-Upload-Refusal", info.code)
	if info.retry {
		w.Header().Set("Retry-After", "1")
	}
	http.Error(w, info.message, info.status)
}
