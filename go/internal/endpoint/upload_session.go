package endpoint

import (
	"encoding/json"
	"net/http"

	"github.com/zR-JB/graphite-meter/go/internal/transport"
)

// UploadSession mints the short-lived upload correlation token during the upload
// warmup phase. The token is then echoed as ?id= by POST /upload lanes and the
// /ws/upload progress socket for this upload stage only.
type UploadSession struct {
	store *UploadStore
}

type uploadSessionResponse struct {
	UploadID string `json:"uploadId"`
}

func NewUploadSession(store *UploadStore) *UploadSession {
	return &UploadSession{store: store}
}

func (u *UploadSession) ID() string                 { return "upload-session" }
func (u *UploadSession) Capabilities() Capabilities { return Capabilities{HTTP: true} }

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
	enc := json.NewEncoder(w)
	enc.SetEscapeHTML(false)
	return enc.Encode(uploadSessionResponse{UploadID: id})
}
