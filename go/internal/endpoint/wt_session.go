package endpoint

import (
	"encoding/json/v2"
	"net/http"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/auth"
)

// WTTokenMinter mints a CONNECT token for an authenticated session and classifies refusal.
type WTTokenMinter func(r *http.Request) (token string, expires time.Time, mint auth.WTMint)

// WTSession mints the single-use WebTransport CONNECT token.
type WTSession struct {
	mint WTTokenMinter
}

// NewWTSession builds the mint endpoint. mint may be nil (authentication off).
func NewWTSession(mint WTTokenMinter) *WTSession { return &WTSession{mint: mint} }

type wtSessionResponse struct {
	Token   string `json:"token"`
	Expires int64  `json:"expires,omitzero"`
}

func (e *WTSession) HandleHTTP(w http.ResponseWriter, r *http.Request) error {
	if r.Method != http.MethodPost {
		w.WriteHeader(http.StatusMethodNotAllowed)
		return nil
	}
	response := wtSessionResponse{}
	if e.mint != nil {
		token, expires, mint := e.mint(r)
		switch mint {
		case auth.WTMintAtCapacity:
			// Capacity, not permission: the login is intact and its oldest outstanding token expires within the token lifetime.
			w.Header().Set("Retry-After", "1")
			http.Error(w, "webtransport token capacity reached", http.StatusTooManyRequests)
			return nil
		case auth.WTMintNoSession:
			http.Error(w, "no session to bind a token to", http.StatusForbidden)
			return nil
		}
		response.Token, response.Expires = token, expires.UnixMilli()
	}
	w.Header().Set("Content-Type", "application/json")
	w.Header().Set("Cache-Control", "no-store")
	return json.MarshalWrite(w, response)
}
