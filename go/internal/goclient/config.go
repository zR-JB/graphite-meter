package goclient

import (
	"cmp"
	"fmt"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

type StageSet struct {
	Latency       bool
	Download      bool
	Upload        bool
	Bidirectional bool
}

type StagePlan struct {
	Name       Stage
	Duration   time.Duration
	Directions []Direction
}

// Plan lists enabled stages in execution order, before transport preparation.
func (c Config) Plan() []StagePlan {
	var plan []StagePlan
	add := func(enabled bool, name Stage, duration time.Duration, directions ...Direction) {
		if enabled {
			plan = append(plan, StagePlan{name, duration, directions})
		}
	}
	add(c.Stages.Latency, StageLatency, c.LatencyDuration)
	add(c.Stages.Download, StageDownload, c.DownloadDuration, Down)
	add(c.Stages.Upload, StageUpload, c.UploadDuration, Up)
	add(c.Stages.Bidirectional, StageBidirectional, c.BidirectionalDuration, Down, Up)
	return plan
}

func (c Config) needsLatency() bool {
	return c.Stages.Latency || (c.LoadedLatency && (c.Stages.Download || c.Stages.Upload || c.Stages.Bidirectional))
}

type TransferStreamPolicy struct {
	AutomaticMax int // Ceiling for automatic HTTP/1 lanes per direction.
	Forced       int // Exact lanes per server and direction; zero is automatic.
}

const (
	maxTransferStreams = 128

	// A transfer request never ends by its own length within a stage.
	transferBytesPerStream = 64 << 30
	maxIdleConnsPerHost    = 256
	responseHeaderTimeout  = 10 * time.Second
	expectContinueTimeout  = time.Second
)

type streamCounts struct{ down, up int }

func (s streamCounts) of(dir Direction) int {
	if dir == Up {
		return s.up
	}
	return s.down
}

// Multiplexed protocols share one connection; their automatic lane counts are fixed.
var multiplexedStreams = map[string]streamCounts{
	"http2": {down: 1, up: 4},
	"http3": {down: 1, up: 1},
}

func (p TransferStreamPolicy) lanes(protocol, transport string) streamCounts {
	switch {
	case transport == wire.TransportWebTransport:
		n := 1
		if p.Forced > 0 {
			n = min(p.Forced, wire.WTMaxStreams)
		}
		return streamCounts{down: n, up: n}
	case p.Forced > 0:
		return streamCounts{down: p.Forced, up: p.Forced}
	}
	if lanes, ok := multiplexedStreams[protocol]; ok {
		return lanes
	}
	return streamCounts{down: p.AutomaticMax, up: p.AutomaticMax}
}

// Label describes the lanes a path would use, in the words of the stream settings.
func (p TransferStreamPolicy) Label(protocol, transport string) string {
	protocol = protocolFromEvidence(protocol)
	webTransport := transport == wire.TransportWebTransport
	if p.Forced > 0 {
		if webTransport && p.Forced > wire.WTMaxStreams {
			return fmt.Sprintf("Forced · %d per direction (capped from %d by the session)", wire.WTMaxStreams, p.Forced)
		}
		return fmt.Sprintf("Forced · %d per direction", p.Forced)
	}
	if webTransport {
		return "Automatic · 1 continuous stream per direction"
	}
	if lanes, ok := multiplexedStreams[protocol]; ok {
		return fmt.Sprintf("Automatic · %d download / %d upload", lanes.down, lanes.up)
	}
	if protocol == "http1" {
		return fmt.Sprintf("Automatic · up to %d per direction", p.AutomaticMax)
	}
	return "Automatic"
}

const MaxPingInterval = wire.WTIdleBound / 2

func ValidatePingInterval(d time.Duration) error {
	if d <= 0 {
		return fmt.Errorf("ping interval must be greater than zero")
	}
	if d > MaxPingInterval {
		return fmt.Errorf("ping interval must be at most %v, half the server's %v WebTransport idle bound", MaxPingInterval, wire.WTIdleBound)
	}
	return nil
}

func PingIntervalBoundApplies(latencyTransport string) bool {
	return latencyTransport == wire.TransportWebTransport
}

func ValidateThroughputTransport(name string) error {
	switch name {
	case "", "auto", wire.TransportFetchStream, wire.TransportWebTransport, wire.TransportWebTransportDatagram:
		return nil
	}
	return fmt.Errorf("invalid throughput transport %q: use auto, %s, or %s", name, wire.TransportFetchStream, wire.TransportWebTransport)
}

func ValidateLatencyTransport(name string) error {
	switch name {
	case "", "auto", wire.TransportWebSocket, wire.TransportWebTransport:
		return nil
	}
	return fmt.Errorf("invalid latency transport %q: use auto, %s, or %s", name, wire.TransportWebSocket, wire.TransportWebTransport)
}

type Config struct {
	BaseURL               string   // Origin of the operator's server catalogue.
	ServerIDs             []string // Selected catalogue IDs; empty uses the operator's defaults.
	ThroughputTarget      string
	ThroughputProtocol    string
	ThroughputTransport   string
	LatencyTarget         string
	LatencyTransport      string
	Stages                StageSet
	Warmup                time.Duration
	LatencyDuration       time.Duration
	DownloadDuration      time.Duration
	UploadDuration        time.Duration
	BidirectionalDuration time.Duration
	TransferStreams       TransferStreamPolicy
	PingInterval          time.Duration
	LoadedLatency         bool
	InsecureSkipTLSVerify bool

	// A prepared server's own configuration: its catalogue identity and the grant issued by its origin.
	server *wire.ServerEntry
	grant  string
}

func DefaultConfig() Config {
	return Config{
		BaseURL:               "http://127.0.0.1:7246",
		ThroughputTarget:      "auto",
		ThroughputProtocol:    "auto",
		ThroughputTransport:   "auto",
		LatencyTarget:         "auto",
		LatencyTransport:      "auto",
		Stages:                StageSet{Latency: true, Download: true, Upload: true},
		Warmup:                800 * time.Millisecond,
		LatencyDuration:       4 * time.Second,
		DownloadDuration:      10 * time.Second,
		UploadDuration:        10 * time.Second,
		BidirectionalDuration: 10 * time.Second,
		TransferStreams:       TransferStreamPolicy{AutomaticMax: 6},
		PingInterval:          250 * time.Millisecond,
		LoadedLatency:         true,
	}
}

// normalized fills unset and out-of-range values from DefaultConfig.
func (c Config) normalized() Config {
	d := DefaultConfig()
	c.BaseURL = cmp.Or(c.BaseURL, d.BaseURL)
	c.ThroughputTarget = cmp.Or(c.ThroughputTarget, d.ThroughputTarget)
	c.ThroughputProtocol = cmp.Or(c.ThroughputProtocol, d.ThroughputProtocol)
	c.ThroughputTransport = cmp.Or(c.ThroughputTransport, d.ThroughputTransport)
	c.LatencyTarget = cmp.Or(c.LatencyTarget, d.LatencyTarget)
	c.LatencyTransport = cmp.Or(c.LatencyTransport, d.LatencyTransport)
	c.Warmup = max(c.Warmup, 0)
	c.LatencyDuration = positive(c.LatencyDuration, d.LatencyDuration)
	c.DownloadDuration = positive(c.DownloadDuration, d.DownloadDuration)
	c.UploadDuration = positive(c.UploadDuration, d.UploadDuration)
	c.BidirectionalDuration = positive(c.BidirectionalDuration, d.BidirectionalDuration)
	c.TransferStreams.AutomaticMax = min(positive(c.TransferStreams.AutomaticMax, d.TransferStreams.AutomaticMax), maxTransferStreams)
	c.TransferStreams.Forced = min(max(c.TransferStreams.Forced, 0), maxTransferStreams)
	c.PingInterval = positive(c.PingInterval, d.PingInterval)
	return c
}

func positive[T int | time.Duration](value, fallback T) T {
	if value > 0 {
		return value
	}
	return fallback
}
