// Package wire holds the JSON discovery/probe contracts and message-bus frames.
package wire

// Preflight describes one logical server. Connection-specific facts live in Probe.
type Preflight struct {
	Server        ServerInfo   `json:"server"`
	EngineVersion string       `json:"engineVersion"`
	Generation    string       `json:"generation"`
	Capabilities  Capabilities `json:"capabilities"`
}

type ServerInfo struct {
	Name     string `json:"name"`
	Host     string `json:"host"`
	Port     int    `json:"port"`
	Location string `json:"location,omitempty"`
}

// Capabilities advertises independently selectable throughput and latency paths.
type Capabilities struct {
	ThroughputTargets []ThroughputTarget `json:"throughputTargets"`
	LatencyTargets    []LatencyTarget    `json:"latencyTargets"`
}

type ThroughputTarget struct {
	ID        string           `json:"id"`
	Origin    string           `json:"origin"`
	Transport string           `json:"transport"`
	Protocol  string           `json:"protocol"`
	TLS       bool             `json:"tls"`
	Routes    ThroughputRoutes `json:"routes"`
}

type ThroughputRoutes struct {
	Probe          string `json:"probe"`
	Download       string `json:"download"`
	Upload         string `json:"upload"`
	UploadSession  string `json:"uploadSession"`
	UploadProgress string `json:"uploadProgress"`
}

// LatencyTarget is an independently selectable interactive latency path.
type LatencyTarget struct {
	ID        string        `json:"id"`
	Origin    string        `json:"origin"`
	Transport string        `json:"transport"`
	Protocol  string        `json:"protocol"`
	TLS       bool          `json:"tls"`
	Routes    LatencyRoutes `json:"routes"`
}

type LatencyRoutes struct {
	Probe string `json:"probe"`
	Ping  string `json:"ping"`
}

func DefaultThroughputRoutes() ThroughputRoutes {
	return ThroughputRoutes{Probe: "/probe", Download: "/download", Upload: "/upload", UploadSession: "/upload/session", UploadProgress: "/upload/progress"}
}

func DefaultLatencyRoutes() LatencyRoutes {
	return LatencyRoutes{Probe: "/probe", Ping: "/ws/ping"}
}

// Probe describes one independently selected target path.
type Probe struct {
	ClientIP           string `json:"clientIp"`
	ClientIPVersion    int    `json:"clientIpVersion"`
	ClientIPSource     string `json:"clientIpSource"`
	ProtocolNegotiated string `json:"protocolNegotiated"`
}
