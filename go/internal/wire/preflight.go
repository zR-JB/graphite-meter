// Package wire holds the JSON discovery/probe contracts and message-bus frames.
package wire

import (
	"encoding/json/v2"
	"fmt"
	"net/url"
	"strconv"
	"strings"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/route"
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
		Probe: route.Probe, Download: route.Download, Upload: route.Upload,
		UploadSession: route.UploadSession, UploadProgress: route.UploadProgress,
		WTSession: route.WTSession, WTDownload: route.WTDownload, WTUpload: route.WTUpload,
	}
}

// DefaultLatencyRoutes returns the latency-target counterpart of DefaultThroughputRoutes, pinned the same way.
func DefaultLatencyRoutes() LatencyRoutes {
	return LatencyRoutes{Probe: route.Probe, Ping: route.Ping, WTSession: route.WTSession, WTPing: route.WTPing}
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
	u, err := targetOrigin(raw.BaseURL)
	if err != nil {
		return err
	}
	if raw.Transport != TransportFetchStream && raw.Transport != TransportWebTransport && raw.Transport != TransportWebTransportDatagram {
		return fmt.Errorf("unsupported throughput transport %q", raw.Transport)
	}
	if raw.Protocol != "http1" && raw.Protocol != "http2" && raw.Protocol != "http3" && raw.Protocol != "negotiated" {
		return fmt.Errorf("unsupported throughput protocol %q", raw.Protocol)
	}
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
	u, err := targetOrigin(raw.BaseURL)
	if err != nil {
		return err
	}
	if raw.Transport != TransportWebSocket && raw.Transport != TransportWebTransport {
		return fmt.Errorf("unsupported latency transport %q", raw.Transport)
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

// ProbeLoad is measurement-handler occupancy at probe time.
type ProbeLoad struct {
	Active int `json:"active"`
	Max    int `json:"max"`
}

// targetOrigin validates the published origin-only contract without restricting listener topology.
func targetOrigin(raw string) (*url.URL, error) {
	if raw == "." {
		return &url.URL{}, nil
	}
	u, err := url.Parse(raw)
	if err != nil {
		return nil, err
	}
	if len(raw) > 2048 || (u.Scheme != "http" && u.Scheme != "https") || u.Hostname() == "" || u.User != nil || u.Path != "" || u.RawQuery != "" || u.ForceQuery || u.Fragment != "" || strings.ContainsAny(raw, "#\\ \t\r\n") {
		return nil, fmt.Errorf("target baseUrl must be an HTTP(S) origin")
	}
	if port := u.Port(); port != "" {
		n, err := strconv.Atoi(port)
		if err != nil || n < 0 || n > 65535 {
			return nil, fmt.Errorf("invalid target port")
		}
	}
	return u, nil
}

// Validate bounds discovery metadata before a client constructs its target catalog.
func (p Preflight) Validate() error {
	if len(p.Server.Name) > 256 || len(p.Server.Location) > 256 || len(p.EngineVersion) > 256 || len(p.Generation) == 0 || len(p.Generation) > 256 {
		return fmt.Errorf("invalid discovery metadata")
	}
	if p.Capabilities.ThroughputTargets == nil || p.Capabilities.LatencyTargets == nil || len(p.Capabilities.ThroughputTargets) > 32 || len(p.Capabilities.LatencyTargets) > 32 {
		return fmt.Errorf("invalid discovery target lists")
	}
	return nil
}

// Validate checks protocol evidence and optional occupancy without deriving measurements.
func (p Probe) Validate() error {
	if len(p.ClientIP) == 0 || len(p.ClientIP) > 64 || (p.ClientIPVersion != 4 && p.ClientIPVersion != 6) || (p.ClientIPSource != "socket" && p.ClientIPSource != "forwarded") || (p.ProtocolNegotiated != "http/1.1" && p.ProtocolNegotiated != "h2" && p.ProtocolNegotiated != "h3") {
		return fmt.Errorf("invalid probe evidence")
	}
	if p.Load != nil && (p.Load.Active < 0 || p.Load.Max < 1) {
		return fmt.Errorf("invalid probe occupancy")
	}
	return nil
}
