// Package wire holds the JSON discovery/probe contracts and message-bus frames.
package wire

import (
	"encoding/json"
	"net/url"
)

// Preflight is the discovery document a server serves at /preflight: who it is
// and which measurement targets it offers.
type Preflight struct {
	Server        ServerInfo   `json:"server"`
	EngineVersion string       `json:"engineVersion"`
	Generation    string       `json:"generation"`
	Capabilities  Capabilities `json:"capabilities"`
}

// ServerInfo identifies the server behind a Preflight document.
type ServerInfo struct {
	Name     string `json:"name"`
	Location string `json:"location,omitempty"`
}

// Capabilities lists the measurement targets a server offers.
type Capabilities struct {
	ThroughputTargets []ThroughputTarget `json:"throughput"`
	LatencyTargets    []LatencyTarget    `json:"latency"`
}

// ThroughputTarget is one download/upload endpoint. The non-JSON fields are
// normalized client-side conveniences. The wire shape intentionally contains
// only baseUrl and the deterministic/negotiated protocol.
type ThroughputTarget struct {
	ID        string           `json:"-"`
	Origin    string           `json:"baseUrl"`
	Transport string           `json:"-"`
	Protocol  string           `json:"protocol"`
	TLS       bool             `json:"-"`
	Routes    ThroughputRoutes `json:"-"`
}

// ThroughputRoutes are the paths a ThroughputTarget serves.
type ThroughputRoutes struct{ Probe, Download, Upload, UploadSession, UploadProgress string }

// LatencyTarget is one ping endpoint. Only baseUrl crosses the wire; the rest
// is filled in client-side.
type LatencyTarget struct {
	ID        string        `json:"-"`
	Origin    string        `json:"baseUrl"`
	Transport string        `json:"-"`
	Protocol  string        `json:"-"`
	TLS       bool          `json:"-"`
	Routes    LatencyRoutes `json:"-"`
}

// LatencyRoutes are the paths a LatencyTarget serves.
type LatencyRoutes struct{ Probe, Ping string }

// DefaultThroughputRoutes returns the paths a discovered target serves. They
// never cross the wire. api/routes.txt pins them against the server mounts
// (internal/server/listeners.go) and the client table
// (client/src/lib/runner/real/backendPure.ts); internal/server/routes_test.go asserts it.
func DefaultThroughputRoutes() ThroughputRoutes {
	return ThroughputRoutes{"/probe", "/download", "/upload", "/upload/session", "/upload/progress"}
}

// DefaultLatencyRoutes returns the latency-target counterpart of
// DefaultThroughputRoutes, pinned the same way.
func DefaultLatencyRoutes() LatencyRoutes { return LatencyRoutes{"/probe", "/ws/ping"} }

// UnmarshalJSON reads the wire shape and derives the client-side fields.
func (t *ThroughputTarget) UnmarshalJSON(data []byte) error {
	var raw struct {
		BaseURL  string `json:"baseUrl"`
		Protocol string `json:"protocol"`
	}
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}
	u, err := url.Parse(raw.BaseURL)
	if err != nil {
		return err
	}
	t.ID, t.Origin, t.Transport, t.Protocol, t.Routes = raw.BaseURL, raw.BaseURL, "fetch-stream", raw.Protocol, DefaultThroughputRoutes()
	t.TLS = u.Scheme == "https"
	return nil
}

// UnmarshalJSON reads the wire shape and derives the client-side fields.
func (t *LatencyTarget) UnmarshalJSON(data []byte) error {
	var raw struct {
		BaseURL string `json:"baseUrl"`
	}
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}
	u, err := url.Parse(raw.BaseURL)
	if err != nil {
		return err
	}
	t.ID, t.Origin, t.Transport, t.Protocol, t.Routes = raw.BaseURL, raw.BaseURL, "websocket", "http1", DefaultLatencyRoutes()
	t.TLS = u.Scheme == "https"
	return nil
}

// Probe is a target's /probe response: how the server sees this client.
type Probe struct {
	ClientIP           string `json:"clientIp"`
	ClientIPVersion    int    `json:"clientIpVersion"`
	ClientIPSource     string `json:"clientIpSource"`
	ProtocolNegotiated string `json:"protocolNegotiated"`
}
