// Package wire holds the JSON discovery/probe contracts and message-bus frames.
package wire

import (
	"encoding/json/v2"
	"fmt"
	"net/url"
	"slices"
	"strconv"
	"strings"
	"time"
)

// Preflight is the discovery document a server serves at /preflight: who it is and which measurement targets it offers.
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
	UploadCheckpoint  bool               `json:"uploadCheckpoint,omitzero"`
	ThroughputTargets []ThroughputTarget `json:"throughput"`
	LatencyTargets    []LatencyTarget    `json:"latency"`
}

const (
	TransportFetchStream          = "fetch-stream"
	TransportWebSocket            = "websocket"
	TransportWebTransport         = "webtransport"
	TransportWebTransportDatagram = "webtransport-datagram"
)

// WTMaxStreams is the published ceiling on a WebTransport session's concurrent streams per direction.
const WTMaxStreams = 16

// IdleBound is the published inactivity bound of every lane, per api/wire.md#lane-endings.
const IdleBound = 30 * time.Second

type ThroughputTarget struct {
	ID        string `json:"-"`
	Origin    string `json:"baseUrl"`
	Transport string `json:"transport"`
	Protocol  string `json:"protocol"`
}

type LatencyTarget struct {
	ID        string `json:"-"`
	Origin    string `json:"baseUrl"`
	Transport string `json:"transport"`
	Protocol  string `json:"-"`
}

// TLS reports whether the target's origin is HTTPS.
func (t ThroughputTarget) TLS() bool { return strings.HasPrefix(t.Origin, "https://") }

func (t LatencyTarget) TLS() bool { return strings.HasPrefix(t.Origin, "https://") }

func (t *ThroughputTarget) UnmarshalJSON(data []byte) error {
	var raw struct {
		BaseURL   string `json:"baseUrl"`
		Transport string `json:"transport"`
		Protocol  string `json:"protocol"`
	}
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}
	_, err := targetOrigin(raw.BaseURL)
	switch {
	case err != nil:
		return err
	case !slices.Contains([]string{TransportFetchStream, TransportWebTransport, TransportWebTransportDatagram},
		raw.Transport):
		return fmt.Errorf("unsupported throughput transport %q", raw.Transport)
	case !slices.Contains([]string{"http1", "http2", "http3", "negotiated"}, raw.Protocol):
		return fmt.Errorf("unsupported throughput protocol %q", raw.Protocol)
	}
	t.ID, t.Origin, t.Transport, t.Protocol = raw.BaseURL, raw.BaseURL, raw.Transport, raw.Protocol
	return nil
}

func (t *LatencyTarget) UnmarshalJSON(data []byte) error {
	var raw struct {
		BaseURL   string `json:"baseUrl"`
		Transport string `json:"transport"`
	}
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}
	_, err := targetOrigin(raw.BaseURL)
	protocol := map[string]string{TransportWebSocket: "http1", TransportWebTransport: "http3"}[raw.Transport]
	switch {
	case err != nil:
		return err
	case protocol == "":
		return fmt.Errorf("unsupported latency transport %q", raw.Transport)
	}
	t.ID, t.Origin, t.Transport, t.Protocol = raw.BaseURL, raw.BaseURL, raw.Transport, protocol
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
	if len(raw) > 2048 || !SafeText(raw) || u.Scheme != "http" && u.Scheme != "https" || u.Hostname() == "" ||
		u.User != nil || u.Path != "" || u.RawQuery != "" || u.ForceQuery || u.Fragment != "" ||
		strings.ContainsAny(raw, "#\\ \t\r\n") {
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
	if len(p.Server.Name) > 256 || len(p.Server.Location) > 256 || len(p.EngineVersion) > 256 ||
		len(p.Generation) == 0 || len(p.Generation) > 256 ||
		!SafeText(p.Server.Name+p.Server.Location+p.EngineVersion+p.Generation) {
		return fmt.Errorf("invalid discovery metadata")
	}
	throughput, latency := p.Capabilities.ThroughputTargets, p.Capabilities.LatencyTargets
	if throughput == nil || latency == nil || len(throughput) > 32 || len(latency) > 32 {
		return fmt.Errorf("invalid discovery target lists")
	}
	return nil
}

// Validate checks protocol evidence and optional occupancy without deriving measurements.
func (p Probe) Validate() error {
	if len(p.ClientIP) == 0 || len(p.ClientIP) > 64 || p.ClientIPVersion != 4 && p.ClientIPVersion != 6 ||
		p.ClientIPSource != "socket" && p.ClientIPSource != "forwarded" ||
		!slices.Contains([]string{"http/1.1", "h2", "h3"}, p.ProtocolNegotiated) {
		return fmt.Errorf("invalid probe evidence")
	}
	if p.Load != nil && (p.Load.Active < 0 || p.Load.Max < 1) {
		return fmt.Errorf("invalid probe occupancy")
	}
	return nil
}
