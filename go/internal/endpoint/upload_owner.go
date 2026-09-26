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
	return transport.AddressBucket(transport.ResolveClientAddress(r, trusted).Addr)
}

// UploadOwner separates delegated browser access without multiplying admission budgets.
func UploadOwner(r *http.Request, trusted []netip.Prefix) string {
	if p, ok := auth.PrincipalFromContext(r.Context()); ok {
		if owner := p.MeasurementOwner(); owner != "" {
			return owner
		}
	}
	return ClientKey(r, trusted)
}

// SessionKey buckets the session budget by login, else by client key.
func SessionKey(r *http.Request, clientKey string) string {
	if p, ok := auth.PrincipalFromContext(r.Context()); ok && p.LoginID() != "" {
		return "login:" + p.LoginID()
	}
	return clientKey
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

func uploadAccessMessage(access uploadAccess) string { return uploadAccessInfos[access].message }

func uploadAccessCode(access uploadAccess) string { return uploadAccessInfos[access].code }

// uploadRefusalError preserves the classified refusal across transports that do not have an HTTP status line.
type uploadRefusalError struct{ access uploadAccess }

func (e *uploadRefusalError) Error() string {
	return "upload refused: " + uploadAccessMessage(e.access)
}

func writeUploadAccessError(w http.ResponseWriter, access uploadAccess) {
	info := uploadAccessInfos[access]
	w.Header().Set("X-Graphite-Upload-Refusal", info.code)
	if info.retry {
		w.Header().Set("Retry-After", "1")
	}
	http.Error(w, info.message, info.status)
}
