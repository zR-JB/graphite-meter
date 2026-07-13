// Package wire holds the JSON discovery/probe contracts and message-bus frames.
package wire

// Preflight describes one logical server. Connection-specific facts live in Probe.
type Preflight struct {
	Server        ServerInfo   `json:"server"`
	EngineVersion string       `json:"engineVersion"`
	Capabilities  Capabilities `json:"capabilities"`
}

type ServerInfo struct {
	Name     string `json:"name"`
	Host     string `json:"host"`
	Port     int    `json:"port"`
	Location string `json:"location,omitempty"`
}

// Capabilities separates bulk byte paths from interactive message channels.
// A run can bind each role independently without inventing another server.
type Capabilities struct {
	Transfers []TransferTarget `json:"transfers"`
	Channels  []ChannelTarget  `json:"channels"`
}

type TransferTarget struct {
	ID        string         `json:"id"`
	Origin    string         `json:"origin"`
	Transport string         `json:"transport"`
	Protocol  string         `json:"protocol"`
	TLS       bool           `json:"tls"`
	Routes    TransferRoutes `json:"routes"`
}

type TransferRoutes struct {
	Probe         string `json:"probe"`
	Download      string `json:"download"`
	Upload        string `json:"upload"`
	UploadSession string `json:"uploadSession"`
}

// ChannelTarget is an independently selectable latency/progress path. Protocol
// names the actual browser-facing wire protocol, not a companion bulk target.
type ChannelTarget struct {
	ID        string        `json:"id"`
	Origin    string        `json:"origin"`
	Transport string        `json:"transport"`
	Protocol  string        `json:"protocol"`
	TLS       bool          `json:"tls"`
	Routes    ChannelRoutes `json:"routes"`
}

type ChannelRoutes struct {
	Latency        *string `json:"latency"`
	UploadProgress *string `json:"uploadProgress"`
}

func DefaultTransferRoutes() TransferRoutes {
	return TransferRoutes{Probe: "/probe", Download: "/download", Upload: "/upload", UploadSession: "/upload/session"}
}

func DefaultWebSocketRoutes() ChannelRoutes {
	latency, progress := "/ws/ping", "/ws/upload"
	return ChannelRoutes{Latency: &latency, UploadProgress: &progress}
}

// Probe describes the path used to reach one selected transfer target.
type Probe struct {
	ClientIP           string `json:"clientIp"`
	ClientIPVersion    int    `json:"clientIpVersion"`
	ClientIPSource     string `json:"clientIpSource"`
	ProtocolNegotiated string `json:"protocolNegotiated"`
}
