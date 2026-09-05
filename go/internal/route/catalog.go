// Package route describes the fixed measurement routes shared by server policy and clients.
package route

import "net/http"

const (
	Preflight      = "/preflight"
	Probe          = "/probe"
	Download       = "/download"
	Upload         = "/upload"
	UploadSession  = "/upload/session"
	UploadProgress = "/upload/progress"
	WTSession      = "/wt/session"
	Ping           = "/ws/ping"
	WTDownload     = "/wt/download"
	WTUpload       = "/wt/upload"
	WTPing         = "/wt/ping"
)

type Kind string

const (
	HTTP         Kind = "http"
	WebSocket    Kind = "ws"
	WebTransport Kind = "wt"
)

// Admission identifies the operation budget; WebTransport ping uses the request budget, not the session budget.
type Admission uint8

const (
	Unmetered Admission = iota
	Request
	Session
)

// Spec is policy metadata, not an HTTP dispatcher. Concrete handlers retain their method behavior.
type Spec struct {
	Kind        Kind
	Admission   Admission
	corsMethods [2]string
}

// AllowsCORSMethod checks preflight permission, not dispatch; HEAD and OPTIONS remain excluded.
func (s Spec) AllowsCORSMethod(method string) bool {
	return method != "" && (method == s.corsMethods[0] || method == s.corsMethods[1])
}

var catalog = map[string]Spec{
	Preflight:      {HTTP, Unmetered, [2]string{http.MethodGet}},
	Probe:          {HTTP, Unmetered, [2]string{http.MethodGet}},
	Download:       {HTTP, Request, [2]string{http.MethodGet}},
	Upload:         {HTTP, Request, [2]string{http.MethodPost}},
	UploadSession:  {HTTP, Unmetered, [2]string{http.MethodPost}},
	UploadProgress: {HTTP, Request, [2]string{http.MethodGet, http.MethodDelete}},
	WTSession:      {HTTP, Unmetered, [2]string{http.MethodPost}},
	Ping:           {WebSocket, Request, [2]string{http.MethodGet}},
	WTDownload:     {WebTransport, Session, [2]string{http.MethodConnect}},
	WTUpload:       {WebTransport, Session, [2]string{http.MethodConnect}},
	WTPing:         {WebTransport, Request, [2]string{http.MethodConnect}},
}

// Lookup matches an exact measurement path and returns a value copy of its fixed policy.
func Lookup(path string) (Spec, bool) {
	spec, ok := catalog[path]
	return spec, ok
}
