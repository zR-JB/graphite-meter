// Package wire holds the JSON discovery/probe contracts and message-bus frames.
package wire

// Preflight is logical-server discovery. Connection-specific facts live in Probe.
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

type Capabilities struct {
	Targets Targets `json:"targets"`
}

// Targets are null when disabled. Each enabled target fully describes its routes.
type Targets struct {
	HTTP1 *Target `json:"http1"`
	HTTP2 *Target `json:"http2"`
	HTTP3 *Target `json:"http3"`
}

type Target struct {
	Origin string `json:"origin"`
	Routes Routes `json:"routes"`
}

type Routes struct {
	Probe         string           `json:"probe"`
	Download      string           `json:"download"`
	Upload        string           `json:"upload"`
	UploadSession string           `json:"uploadSession"`
	WebSocket     *WebSocketRoutes `json:"websocket"`
	WebTransport  *string          `json:"webtransport"`
}

type WebSocketRoutes struct {
	Ping           string `json:"ping"`
	UploadProgress string `json:"uploadProgress"`
}

func DefaultRoutes(websocket bool) Routes {
	r := Routes{Probe: "/probe", Download: "/download", Upload: "/upload", UploadSession: "/upload/session"}
	if websocket {
		r.WebSocket = &WebSocketRoutes{Ping: "/ws/ping", UploadProgress: "/ws/upload"}
	}
	return r
}

// Probe describes the path used to reach one selected target.
type Probe struct {
	ClientIP           string `json:"clientIp"`
	ClientIPVersion    int    `json:"clientIpVersion"`
	ClientIPSource     string `json:"clientIpSource"`
	ProtocolNegotiated string `json:"protocolNegotiated"`
}
