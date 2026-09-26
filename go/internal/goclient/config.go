package goclient

import (
	"cmp"
	"fmt"
	"slices"
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
	AutomaticMax int
	Forced       int
}

const (
	maxTransferStreams = 128

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

func validatePingInterval(d time.Duration) error {
	if d > MaxPingInterval {
		return fmt.Errorf(
			"ping interval must be at most %v, half the server's %v WebTransport idle bound",
			MaxPingInterval,
			wire.WTIdleBound,
		)
	}
	return nil
}

func (c Config) Validate() error {
	c = c.normalized()
	fetch, ws, wt := wire.TransportFetchStream, wire.TransportWebSocket, wire.TransportWebTransport
	switch {
	case !slices.Contains([]string{"auto", "http1", "http2", "http3"}, c.ThroughputProtocol):
		return fmt.Errorf("invalid throughput protocol %q: use auto, http1, http2, or http3", c.ThroughputProtocol)
	case !slices.Contains([]string{"auto", fetch, wt}, c.ThroughputTransport):
		return fmt.Errorf("invalid throughput transport %q: use auto, %s, or %s", c.ThroughputTransport, fetch, wt)
	case !slices.Contains([]string{"auto", ws, wt}, c.LatencyTransport):
		return fmt.Errorf("invalid latency transport %q: use auto, %s, or %s", c.LatencyTransport, ws, wt)
	case c.LatencyTransport == wire.TransportWebTransport:
		return validatePingInterval(c.PingInterval)
	}
	return nil
}

type Config struct {
	BaseURL               string
	ServerIDs             []string
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
	c.TransferStreams.AutomaticMax = min(
		positive(c.TransferStreams.AutomaticMax, d.TransferStreams.AutomaticMax),
		maxTransferStreams,
	)
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

func planRunStreams(cfg Config, servers []PreparedServer) (map[string]streamCounts, error) {
	plan := map[string]streamCounts{}
	var total streamCounts
	for _, server := range servers {
		target := server.Connection.ThroughputTarget
		lanes := cfg.TransferStreams.lanes(target.Protocol, target.Transport)
		plan[server.Server.ID] = lanes
		total.down += lanes.down
		total.up += lanes.up
	}
	if total.down > maxTransferStreams || total.up > maxTransferStreams {
		return nil, fmt.Errorf("the run exceeds %d streams per direction; reduce forced streams", maxTransferStreams)
	}
	return plan, nil
}
