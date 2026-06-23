// Package wire holds the cross-language contract types and (in later stages)
// the message-bus protocol codec. These structs are the Go side of API Surface
// A — the GET /preflight JSON body. They are hand-written and kept honest by a
// conformance test against api/preflight.schema.json (preflight_test.go).
package wire

// Preflight is the GET /preflight response body. It mirrors
// api/preflight.schema.json exactly; the JSON tags are load-bearing.
type Preflight struct {
	ClientIP           string       `json:"clientIp"`
	Server             ServerInfo   `json:"server"`
	PreTestPingMs      float64      `json:"preTestPingMs"`
	EngineVersion      string       `json:"engineVersion"`
	ProtocolNegotiated string       `json:"protocolNegotiated"`
	Capabilities       Capabilities `json:"capabilities"`
}

// ServerInfo identifies the measurement server.
type ServerInfo struct {
	Name     string `json:"name"`
	Host     string `json:"host"`
	Port     int    `json:"port"`
	Location string `json:"location,omitempty"`
}

// Capabilities tells the client what this server currently supports so it
// negotiates rather than assumes.
type Capabilities struct {
	Origins    Origins    `json:"origins"`
	Transports Transports `json:"transports"`
	Endpoints  Endpoints  `json:"endpoints"`
}

// Origins are the externally-reachable per-transport origins. A nil pointer
// marshals to JSON null, meaning "that origin is not enabled".
type Origins struct {
	H1  *string `json:"h1"`
	TLS *string `json:"tls"`
	H3  *string `json:"h3"`
}

// Transports flags which client-side transports the server can service. Honest
// per build stage.
type Transports struct {
	XHRStream    bool `json:"xhrStream"`
	WebSocket    bool `json:"websocket"`
	WebTransport bool `json:"webtransport"`
}

// Endpoints advertises the stable endpoint paths so the client never hardcodes
// them.
type Endpoints struct {
	Download      string `json:"download"`
	Upload        string `json:"upload"`
	UploadSession string `json:"uploadSession"`
	WSPing        string `json:"wsPing"`
	WSUpload      string `json:"wsUpload"`
	WTPing        string `json:"wtPing"`
	WTDownload    string `json:"wtDownload"`
	WTUpload      string `json:"wtUpload"`
}

// DefaultEndpoints returns the canonical endpoint paths (see docs/ARCHITECTURE.md
// §2 and api/preflight.schema.json).
func DefaultEndpoints() Endpoints {
	return Endpoints{
		Download:      "/download",
		Upload:        "/upload",
		UploadSession: "/upload/session",
		WSPing:        "/ws/ping",
		WSUpload:      "/ws/upload",
		WTPing:        "/wt/ping",
		WTDownload:    "/wt/download",
		WTUpload:      "/wt/upload",
	}
}
