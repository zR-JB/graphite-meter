// Package route describes the fixed measurement routes shared by server policy and clients.
package route

import (
	"iter"
	"net/http"
	"slices"
)

const (
	Servers          = "/servers"
	UploadCheckpoint = "/upload/checkpoint"
	Preflight        = "/preflight"
	Probe            = "/probe"
	Download         = "/download"
	Upload           = "/upload"
	UploadSession    = "/upload/session"
	UploadProgress   = "/upload/progress"
	WTSession        = "/wt/session"
	WSSession        = "/ws/session"
	Ping             = "/ws/ping"
	WTDownload       = "/wt/download"
	WTUpload         = "/wt/upload"
	WTPing           = "/wt/ping"
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

// Spec is a route's fixed policy: how it is reached, which budget it
// spends, and the methods it dispatches. A GET route also serves HEAD.
type Spec struct {
	Kind      Kind
	Admission Admission
	methods   []string
}

// Methods lists the methods the route dispatches, excluding the HEAD a GET implies and CORS preflight.
func (s Spec) Methods() iter.Seq[string] { return slices.Values(s.methods) }

// AllowsCORSMethod checks preflight permission for a requested method; HEAD and OPTIONS remain excluded.
func (s Spec) AllowsCORSMethod(method string) bool { return slices.Contains(s.methods, method) }

var catalog = map[string]Spec{
	Servers:          {HTTP, Unmetered, []string{http.MethodGet}},
	UploadCheckpoint: {HTTP, Unmetered, []string{http.MethodPost}},
	Preflight:        {HTTP, Unmetered, []string{http.MethodGet}},
	Probe:            {HTTP, Unmetered, []string{http.MethodGet}},
	Download:         {HTTP, Request, []string{http.MethodGet}},
	Upload:           {HTTP, Request, []string{http.MethodPost}},
	UploadSession:    {HTTP, Unmetered, []string{http.MethodPost}},
	UploadProgress:   {HTTP, Request, []string{http.MethodGet, http.MethodDelete}},
	WTSession:        {HTTP, Unmetered, []string{http.MethodPost}},
	WSSession:        {HTTP, Unmetered, []string{http.MethodPost}},
	Ping:             {WebSocket, Request, []string{http.MethodGet}},
	WTDownload:       {WebTransport, Session, []string{http.MethodConnect}},
	WTUpload:         {WebTransport, Session, []string{http.MethodConnect}},
	WTPing:           {WebTransport, Request, []string{http.MethodConnect}},
}

// Lookup matches an exact measurement path and returns its fixed policy.
func Lookup(path string) (Spec, bool) {
	spec, ok := catalog[path]
	return spec, ok
}
