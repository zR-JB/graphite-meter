package endpoint

import (
	"encoding/json/v2"
	"net/http"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/auth"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
)

// WTTokenMinter mints one CONNECT token for the request's authenticated
// session, and says why when it does not. The two refusals are not one thing:
// a request with no session-backed principal may not have a token at all, while
// a session at its token cap is intact and gets a slot back within the token
// lifetime.
type WTTokenMinter func(r *http.Request) (token string, expires time.Time, mint auth.WTMint)

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
	Expires int64  `json:"expires,omitzero"`
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
		token, expires, mint := e.mint(r)
		switch mint {
		case auth.WTMintAtCapacity:
			// Capacity, not permission: the login is intact and its oldest
			// outstanding token expires within the token lifetime, so the caller's
			// one correct move is to wait and ask again. The cap is per session
			// rather than server-wide, so this is the status the per-client
			// admission bucket and a client-full upload already answer with --
			// 503 is what this codebase reserves for exhausting the whole server.
			// No Graphite-Meter-Auth marker: nothing here is an auth failure, and
			// marking it would send the user to a login they do not need.
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
