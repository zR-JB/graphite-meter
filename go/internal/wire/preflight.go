// Package wire holds the JSON discovery/probe contracts and message-bus frames.
package wire

import (
	"cmp"
	"encoding/json/v2"
	"net/url"
	"time"
)

// Preflight is the discovery document a server serves at /preflight: who it is and which measurement targets it offers.
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

// Transport names the mechanism that reaches a target.
const (
	TransportFetchStream          = "fetch-stream"
	TransportWebSocket            = "websocket"
	TransportWebTransport         = "webtransport"
	TransportWebTransportDatagram = "webtransport-datagram"
)

// WTMaxStreams is the published ceiling on a WebTransport session's concurrent streams per direction.
const WTMaxStreams = 16

// WTIdleBound is the published inactivity target for a WebTransport session, per api/wire.md.
const WTIdleBound = 30 * time.Second

// ThroughputTarget is one download/upload endpoint.
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

// LatencyTarget is one ping endpoint.
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

// DefaultThroughputRoutes returns the paths a discovered target serves.
func DefaultThroughputRoutes() ThroughputRoutes {
	return ThroughputRoutes{
		Probe: "/probe", Download: "/download", Upload: "/upload",
		UploadSession: "/upload/session", UploadProgress: "/upload/progress",
		WTSession: "/wt/session", WTDownload: "/wt/download", WTUpload: "/wt/upload",
	}
}

// DefaultLatencyRoutes returns the latency-target counterpart of DefaultThroughputRoutes, pinned the same way.
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
	raw.Transport = cmp.Or(raw.Transport, TransportFetchStream)
	t.ID, t.Origin, t.Transport, t.Protocol, t.Routes = raw.BaseURL, raw.BaseURL, raw.Transport, raw.Protocol, DefaultThroughputRoutes()
	t.TLS = u.Scheme == "https"
	return nil
}

// UnmarshalJSON reads the wire shape and derives the client-side fields.
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
	raw.Transport = cmp.Or(raw.Transport, TransportWebSocket)
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

// ProbeLoad is measurement-handler occupancy at probe time.
type ProbeLoad struct {
	Active int `json:"active"`
	Max    int `json:"max"`
}
