package goclient

import (
	"cmp"
	"fmt"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
	"time"
)

type StageSet struct {
	Latency       bool
	Download      bool
	Upload        bool
	Bidirectional bool
}

type StagePlan struct {
	Name       string
	Duration   time.Duration
	Directions []Direction
}

// Plan lists enabled stages in execution order, before transport preparation.
func (c Config) Plan() []StagePlan {
	var plan []StagePlan
	add := func(enabled bool, name string, duration time.Duration, directions ...Direction) {
		if enabled {
			plan = append(plan, StagePlan{name, duration, directions})
		}
	}
	add(c.Stages.Latency, "latency", c.LatencyDuration)
	add(c.Stages.Download, "download", c.DownloadDuration, Down)
	add(c.Stages.Upload, "upload", c.UploadDuration, Up)
	add(c.Stages.Bidirectional, "bidirectional", c.BidirectionalDuration, Down, Up)
	return plan
}

type TransferStreamPolicy struct {
	AutomaticMax int
	Forced       int
}

const (
	defaultAutomaticStreams = 6
	maxTransferStreams      = 128

	defaultLatencyDuration  = 4 * time.Second
	defaultTransferDuration = 10 * time.Second
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

func (p TransferStreamPolicy) Resolve(protocol string, dir Direction) int {
	if p.Forced > 0 {
		return p.Forced
	}
	if lanes, ok := multiplexedStreams[protocol]; ok {
		return lanes.of(dir)
	}
	return p.AutomaticMax
}

func (p TransferStreamPolicy) lanes(protocol, transport string) streamCounts {
	if transport == wire.TransportWebTransport {
		n := p.ResolveWebTransport()
		return streamCounts{down: n, up: n}
	}
	return streamCounts{down: p.Resolve(protocol, Down), up: p.Resolve(protocol, Up)}
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

func (p TransferStreamPolicy) ResolveWebTransport() int {
	if p.Forced > 0 {
		return min(p.Forced, wire.WTMaxStreams)
	}
	return 1
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
	if protocol == "http1" || protocol == "http1-clear" || protocol == "http1-tls" {
		return fmt.Sprintf("Automatic · up to %d per direction", p.AutomaticMax)
	}
	return "Automatic"
}

type Config struct {
	ServerIDs              []string
	server                 *wire.ServerEntry
	BaseURL                string
	ThroughputTarget       string
	ThroughputProtocol     string
	ThroughputTransport    string
	LatencyTarget          string
	LatencyTransport       string
	Stages                 StageSet
	Warmup                 time.Duration
	LatencyDuration        time.Duration
	DownloadDuration       time.Duration
	UploadDuration         time.Duration
	BidirectionalDuration  time.Duration
	TransferStreams        TransferStreamPolicy
	PingInterval           time.Duration
	LoadedLatency          bool
	DownloadBytesPerStream int64
	UploadBytesPerStream   int64
	InsecureSkipTLSVerify  bool
	AuthToken              string
	AuthOrigin             string
	MaxIdleConnsPerHost    int
	ResponseHeaderTimeout  time.Duration
	ExpectContinueTimeout  time.Duration
}

func DefaultConfig() Config {
	return Config{
		BaseURL:                "http://127.0.0.1:7246",
		ThroughputTarget:       "auto",
		ThroughputProtocol:     "auto",
		ThroughputTransport:    "auto",
		LatencyTarget:          "auto",
		LatencyTransport:       "auto",
		Stages:                 StageSet{Latency: true, Download: true, Upload: true},
		Warmup:                 800 * time.Millisecond,
		LatencyDuration:        defaultLatencyDuration,
		DownloadDuration:       defaultTransferDuration,
		UploadDuration:         defaultTransferDuration,
		BidirectionalDuration:  defaultTransferDuration,
		TransferStreams:        TransferStreamPolicy{AutomaticMax: defaultAutomaticStreams},
		PingInterval:           250 * time.Millisecond,
		LoadedLatency:          true,
		DownloadBytesPerStream: 64 * 1024 * 1024 * 1024,
		UploadBytesPerStream:   64 * 1024 * 1024 * 1024,
		MaxIdleConnsPerHost:    256,
		ResponseHeaderTimeout:  10 * time.Second,
		ExpectContinueTimeout:  time.Second,
	}
}

func (c Config) normalized() Config {
	c.BaseURL = cmp.Or(c.BaseURL, "http://127.0.0.1:7246")
	c.ThroughputTarget = cmp.Or(c.ThroughputTarget, "auto")
	c.ThroughputProtocol = cmp.Or(c.ThroughputProtocol, "auto")
	c.ThroughputTransport = cmp.Or(c.ThroughputTransport, "auto")
	c.LatencyTarget = cmp.Or(c.LatencyTarget, "auto")
	c.LatencyTransport = cmp.Or(c.LatencyTransport, "auto")
	c.Warmup = max(c.Warmup, 0)
	c.LatencyDuration = positiveDuration(c.LatencyDuration, defaultLatencyDuration)
	c.DownloadDuration = positiveDuration(c.DownloadDuration, defaultTransferDuration)
	c.UploadDuration = positiveDuration(c.UploadDuration, defaultTransferDuration)
	c.BidirectionalDuration = positiveDuration(c.BidirectionalDuration, defaultTransferDuration)
	c.TransferStreams.Forced = max(c.TransferStreams.Forced, 0)
	c.TransferStreams.AutomaticMax = positiveInt(c.TransferStreams.AutomaticMax, defaultAutomaticStreams)
	c.TransferStreams.AutomaticMax = min(c.TransferStreams.AutomaticMax, maxTransferStreams)
	c.TransferStreams.Forced = min(c.TransferStreams.Forced, maxTransferStreams)
	c.PingInterval = positiveDuration(c.PingInterval, 250*time.Millisecond)
	c.DownloadBytesPerStream = positiveInt64(c.DownloadBytesPerStream, 64*1024*1024*1024)
	c.UploadBytesPerStream = positiveInt64(c.UploadBytesPerStream, 64*1024*1024*1024)
	c.MaxIdleConnsPerHost = positiveInt(c.MaxIdleConnsPerHost, 256)
	c.ResponseHeaderTimeout = positiveDuration(c.ResponseHeaderTimeout, 10*time.Second)
	c.ExpectContinueTimeout = positiveDuration(c.ExpectContinueTimeout, time.Second)
	return c
}

func positiveDuration(value, fallback time.Duration) time.Duration {
	if value > 0 {
		return value
	}
	return fallback
}

func positiveInt64(value, fallback int64) int64 {
	if value > 0 {
		return value
	}
	return fallback
}

func positiveInt(value, fallback int) int {
	if value > 0 {
		return value
	}
	return fallback
}
