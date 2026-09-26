package endpoint

import (
	"encoding/json/v2"
	"net/http"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/auth"
)

// SocketTokenMinter mints a single-use socket token or classifies the refusal.
type SocketTokenMinter func(r *http.Request) (token string, expires time.Time, mint auth.WTMint)

// SocketToken serves /wt/session or /ws/session; a nil mint (public mode) answers an empty token.
func SocketToken(mint SocketTokenMinter) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		var response struct {
			Token   string `json:"token"`
			Expires int64  `json:"expires"`
		}
		if mint != nil {
			token, expires, result := mint(r)
			switch result {
			case auth.WTMintInvalidTarget:
				http.Error(w, "invalid socket target", http.StatusBadRequest)
				return
			case auth.WTMintAtCapacity:
				// Capacity, not permission: the login is intact and its oldest outstanding token expires within the token lifetime.
				w.Header().Set("Retry-After", "1")
				http.Error(w, "webtransport token capacity reached", http.StatusTooManyRequests)
				return
			case auth.WTMintNoSession:
				http.Error(w, "no session to bind a token to", http.StatusForbidden)
				return
			}
			response.Token, response.Expires = token, expires.UnixMilli()
		}
		w.Header().Set("Content-Type", "application/json")
		w.Header().Set("Cache-Control", "no-store")
		_ = json.MarshalWrite(w, response)
	})
}
