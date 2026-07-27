package endpoint

import (
	"encoding/json"
	"net/http"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/transport"
)

// WTTokenMinter mints one CONNECT token for the request's authenticated
// session. ok is false when the request carries no session-backed principal.
type WTTokenMinter func(r *http.Request) (token string, expires time.Time, ok bool)

// WTSession mints the single-use token a browser's WebTransport CONNECT
// carries, since the CONNECT itself can send neither cookies nor headers. With
// authentication off the token is empty, so the client flow is uniform.
type WTSession struct {
	mint WTTokenMinter
}

// NewWTSession builds the mint endpoint. mint may be nil (authentication off).
func NewWTSession(mint WTTokenMinter) *WTSession { return &WTSession{mint: mint} }

func (e *WTSession) ID() string { return "wt-session" }

type wtSessionResponse struct {
	Token   string `json:"token"`
	Expires int64  `json:"expires,omitempty"`
}

func (e *WTSession) Handle(s transport.Session) error {
	w, r, ok := s.HTTP()
	if !ok {
		return transport.ErrUnsupported
	}
	if r.Method != http.MethodPost {
		w.WriteHeader(http.StatusMethodNotAllowed)
		return nil
	}
	response := wtSessionResponse{}
	if e.mint != nil {
		token, expires, minted := e.mint(r)
		if !minted {
			http.Error(w, "no session to bind a token to", http.StatusForbidden)
			return nil
		}
		response.Token, response.Expires = token, expires.UnixMilli()
	}
	w.Header().Set("Content-Type", "application/json")
	w.Header().Set("Cache-Control", "no-store")
	return json.NewEncoder(w).Encode(response)
}
