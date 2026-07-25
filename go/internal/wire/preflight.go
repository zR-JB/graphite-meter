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

// Transport names the mechanism that reaches a target. One origin can offer
// several, so a client picks a target by (baseUrl, transport). The datagram
// variant serves throughput as unreliable datagrams on the same session
// routes: goodput with visible loss, advertised as its own path.
const (
	TransportFetchStream          = "fetch-stream"
	TransportWebSocket            = "websocket"
	TransportWebTransport         = "webtransport"
	TransportWebTransportDatagram = "webtransport-datagram"
)

// ThroughputTarget is one download/upload endpoint. The non-JSON fields are
// normalized client-side conveniences. The wire shape intentionally contains
// only baseUrl, the transport, and the deterministic/negotiated protocol.
type ThroughputTarget struct {
	ID        string           `json:"-"`
	Origin    string           `json:"baseUrl"`
	Transport string           `json:"transport"`
	Protocol  string           `json:"protocol"`
	TLS       bool             `json:"-"`
	Routes    ThroughputRoutes `json:"-"`
}

// ThroughputRoutes are the paths a ThroughputTarget serves.
type ThroughputRoutes struct {
	Probe, Download, Upload, UploadSession, UploadProgress string
	WTSession, WTDownload, WTUpload                        string
}

// LatencyTarget is one ping endpoint. Only baseUrl and the transport cross the
// wire; the rest is filled in client-side.
type LatencyTarget struct {
	ID        string        `json:"-"`
	Origin    string        `json:"baseUrl"`
	Transport string        `json:"transport"`
	Protocol  string        `json:"-"`
	TLS       bool          `json:"-"`
	Routes    LatencyRoutes `json:"-"`
}

// LatencyRoutes are the paths a LatencyTarget serves.
type LatencyRoutes struct{ Probe, Ping, WTSession, WTPing string }

// DefaultThroughputRoutes returns the paths a discovered target serves. They
// never cross the wire. api/routes.txt pins them against the server mounts
// (internal/server/listeners.go) and the client table
// (client/src/lib/runner/real/backendPure.ts); internal/server/routes_test.go asserts it.
func DefaultThroughputRoutes() ThroughputRoutes {
	return ThroughputRoutes{
		Probe: "/probe", Download: "/download", Upload: "/upload",
		UploadSession: "/upload/session", UploadProgress: "/upload/progress",
		WTSession: "/wt/session", WTDownload: "/wt/download", WTUpload: "/wt/upload",
	}
}

// DefaultLatencyRoutes returns the latency-target counterpart of
// DefaultThroughputRoutes, pinned the same way.
func DefaultLatencyRoutes() LatencyRoutes {
	return LatencyRoutes{Probe: "/probe", Ping: "/ws/ping", WTSession: "/wt/session", WTPing: "/wt/ping"}
}

// UnmarshalJSON reads the wire shape and derives the client-side fields.
func (t *ThroughputTarget) UnmarshalJSON(data []byte) error {
	var raw struct {
		BaseURL   string `json:"baseUrl"`
		Transport string `json:"transport"`
		Protocol  string `json:"protocol"`
	}
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}
	u, err := url.Parse(raw.BaseURL)
	if err != nil {
		return err
	}
	if raw.Transport == "" {
		raw.Transport = TransportFetchStream
	}
	t.ID, t.Origin, t.Transport, t.Protocol, t.Routes = raw.BaseURL, raw.BaseURL, raw.Transport, raw.Protocol, DefaultThroughputRoutes()
	t.TLS = u.Scheme == "https"
	return nil
}

// UnmarshalJSON reads the wire shape and derives the client-side fields. The
// protocol follows the transport: WebSocket rides HTTP/1.1, WebTransport HTTP/3.
func (t *LatencyTarget) UnmarshalJSON(data []byte) error {
	var raw struct {
		BaseURL   string `json:"baseUrl"`
		Transport string `json:"transport"`
	}
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}
	u, err := url.Parse(raw.BaseURL)
	if err != nil {
		return err
	}
	if raw.Transport == "" {
		raw.Transport = TransportWebSocket
	}
	protocol := "http1"
	if raw.Transport == TransportWebTransport {
		protocol = "http3"
	}
	t.ID, t.Origin, t.Transport, t.Protocol, t.Routes = raw.BaseURL, raw.BaseURL, raw.Transport, protocol, DefaultLatencyRoutes()
	t.TLS = u.Scheme == "https"
	return nil
}

// Probe is a target's /probe response: how the server sees this client.
type Probe struct {
	ClientIP           string     `json:"clientIp"`
	ClientIPVersion    int        `json:"clientIpVersion"`
	ClientIPSource     string     `json:"clientIpSource"`
	ProtocolNegotiated string     `json:"protocolNegotiated"`
	Load               *ProbeLoad `json:"load,omitempty"`
}

// ProbeLoad is the server's measurement occupancy at probe time: concurrent
// tests contend for bandwidth and CPU, so a busy server means results may be
// affected. Admission still refuses outright overload with 429/503.
type ProbeLoad struct {
	Active int `json:"active"`
	Max    int `json:"max"`
}
