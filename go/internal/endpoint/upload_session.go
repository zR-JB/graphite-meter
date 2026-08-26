package endpoint

import (
	"encoding/json/v2"
	"net/http"

	"github.com/zR-JB/graphite-meter/go/internal/transport"
)

// UploadSession mints the short-lived upload correlation token during the upload
// warmup phase. The token is then echoed as ?id= by POST /upload lanes and the
// /upload/progress progress stream for this upload stage only.
type UploadSession struct {
	store *UploadStore
}

type uploadSessionResponse struct {
	UploadID string `json:"uploadId"`
}

// NewUploadSession builds the token-minting endpoint. A nil store leaves the
// route mounted but answering 503, so a client gets a clear refusal rather than
// a 404 it would misread as an old server.
func NewUploadSession(store *UploadStore) *UploadSession {
	return &UploadSession{store: store}
}

func (u *UploadSession) ID() string { return "upload-session" }

func (u *UploadSession) Handle(s transport.Session) error {
	w, r, ok := s.HTTP()
	if !ok {
		return transport.ErrUnsupported
	}
	if r.Method != http.MethodPost {
		w.WriteHeader(http.StatusMethodNotAllowed)
		return nil
	}
	if u.store == nil {
		http.Error(w, "upload sessions unavailable", http.StatusServiceUnavailable)
		return nil
	}
	id := u.store.Mint()
	if id == "" {
		http.Error(w, "upload session mint failed", http.StatusServiceUnavailable)
		return nil
	}
	w.Header().Set("Content-Type", "application/json")
	w.Header().Set("Cache-Control", "no-store")
	return json.MarshalWrite(w, uploadSessionResponse{UploadID: id})
}
