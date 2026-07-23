// Package wire holds the JSON discovery/probe contracts and message-bus frames.
package wire

import (
	"encoding/json"
	"net/url"
)

type Preflight struct {
	Server        ServerInfo   `json:"server"`
	EngineVersion string       `json:"engineVersion"`
	Generation    string       `json:"generation"`
	Capabilities  Capabilities `json:"capabilities"`
}

type ServerInfo struct {
	Name     string `json:"name"`
	Location string `json:"location,omitempty"`
}

type Capabilities struct {
	ThroughputTargets []ThroughputTarget `json:"throughput"`
	LatencyTargets    []LatencyTarget    `json:"latency"`
}

// The non-JSON fields are normalized client-side conveniences. The wire shape
// intentionally contains only baseUrl and the deterministic/negotiated protocol.
type ThroughputTarget struct {
	ID        string           `json:"-"`
	Origin    string           `json:"baseUrl"`
	Transport string           `json:"-"`
	Protocol  string           `json:"protocol"`
	TLS       bool             `json:"-"`
	Routes    ThroughputRoutes `json:"-"`
}
type ThroughputRoutes struct{ Probe, Download, Upload, UploadSession, UploadProgress string }

type LatencyTarget struct {
	ID        string        `json:"-"`
	Origin    string        `json:"baseUrl"`
	Transport string        `json:"-"`
	Protocol  string        `json:"-"`
	TLS       bool          `json:"-"`
	Routes    LatencyRoutes `json:"-"`
}
type LatencyRoutes struct{ Probe, Ping string }

// The route defaults a discovered target carries, filled in client-side because
// Routes is json:"-" — the paths are never transmitted. They are pinned against
// the paths the server mounts (go/internal/server/listeners.go) and the client
// table (client/src/lib/runner/real/backendPure.ts) by api/routes.txt; the
// assertion lives in go/internal/server/routes_test.go.
func DefaultThroughputRoutes() ThroughputRoutes {
	return ThroughputRoutes{"/probe", "/download", "/upload", "/upload/session", "/upload/progress"}
}
func DefaultLatencyRoutes() LatencyRoutes { return LatencyRoutes{"/probe", "/ws/ping"} }

func (t *ThroughputTarget) UnmarshalJSON(data []byte) error {
	var raw struct {
		BaseURL  string `json:"baseUrl"`
		Protocol string `json:"protocol"`
	}
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}
	t.ID, t.Origin, t.Transport, t.Protocol, t.Routes = raw.BaseURL, raw.BaseURL, "fetch-stream", raw.Protocol, DefaultThroughputRoutes()
	u, _ := url.Parse(raw.BaseURL)
	t.TLS = u.Scheme == "https"
	return nil
}

func (t *LatencyTarget) UnmarshalJSON(data []byte) error {
	var raw struct {
		BaseURL string `json:"baseUrl"`
	}
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}
	t.ID, t.Origin, t.Transport, t.Protocol, t.Routes = raw.BaseURL, raw.BaseURL, "websocket", "http1", DefaultLatencyRoutes()
	u, _ := url.Parse(raw.BaseURL)
	t.TLS = u.Scheme == "https"
	return nil
}

type Probe struct {
	ClientIP           string `json:"clientIp"`
	ClientIPVersion    int    `json:"clientIpVersion"`
	ClientIPSource     string `json:"clientIpSource"`
	ProtocolNegotiated string `json:"protocolNegotiated"`
}
