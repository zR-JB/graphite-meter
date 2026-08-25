package goclient

import (
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

type TransferStreamPolicy struct {
	AutomaticMax int
	// Forced is exact per active direction. Zero selects automatic.
	Forced int
}

const (
	defaultAutomaticStreams = 6
	maxTransferStreams      = 128

	defaultLatencyDuration = 4 * time.Second
	// defaultTransferDuration covers the download, upload, and bidirectional
	// stages, which run the same window.
	defaultTransferDuration = 10 * time.Second
)

// streamCounts is the lane count a stage runs per direction. The two differ:
// an automatic multiplexed upload can take more lanes than its download.
type streamCounts struct{ down, up int }

func (s streamCounts) of(dir Direction) int {
	if dir == Up {
		return s.up
	}
	return s.down
}

// multiplexedStreams is the automatic lane count per direction over a
// multiplexed connection, the same table the browser reads in
// client/src/lib/runner/real/streamPolicy.ts. Download takes one: the
// connection already carries it at full rate. Upload splits by protocol --
// under loss h2 gains 10.1% going from 1 to 4 lanes while h3 loses 9.3% over
// the same range, disjoint IQRs both ways. See docs/BENCHMARKS.md.
var multiplexedStreams = map[string]streamCounts{
	"http2": {down: 1, up: 4},
	"http3": {down: 1, up: 1},
}

// Resolve is the lane count for one direction: the forced count when one is
// set, otherwise the automatic count the protocol calls for.
func (p TransferStreamPolicy) Resolve(protocol string, dir Direction) int {
	if p.Forced > 0 {
		return p.Forced
	}
	if lanes, ok := multiplexedStreams[protocol]; ok {
		return lanes.of(dir)
	}
	return p.AutomaticMax
}

// lanes is what a stage opens in each direction over the transport that will
// carry it. A WebTransport session runs one continuous stream per direction
// whatever the protocol underneath, so it resolves before the protocol table.
func (p TransferStreamPolicy) lanes(protocol, transport string) streamCounts {
	if transport == wire.TransportWebTransport {
		n := p.ResolveWebTransport()
		return streamCounts{down: n, up: n}
	}
	return streamCounts{down: p.Resolve(protocol, Down), up: p.Resolve(protocol, Up)}
}

// MaxPingInterval is the widest cadence the latency bus is measured at. The
// server reaps a WebTransport ping bus that has received nothing for
// wire.WTIdleBound, and this client has no keepalive of its own: its pings are
// the only traffic the bus carries. Half the bound leaves room for a lost
// datagram, so one drop cannot take the bus down mid-stage.
const MaxPingInterval = wire.WTIdleBound / 2

// ValidatePingInterval reports whether d is a cadence the bus survives. The
// message is user-facing: the TUI and the CLI both print it.
func ValidatePingInterval(d time.Duration) error {
	if d <= 0 {
		return fmt.Errorf("ping interval must be greater than zero")
	}
	if d > MaxPingInterval {
		return fmt.Errorf("ping interval must be at most %v, half the server's %v WebTransport idle bound", MaxPingInterval, wire.WTIdleBound)
	}
	return nil
}

// PingIntervalBoundApplies reports whether MaxPingInterval governs a run whose
// latency bus is named by transport. The bound belongs to the datagram bus and
// to nothing else: the server reaps a WebTransport ping bus it has heard nothing
// from, and this client's pings are its only traffic. The WebSocket bus has no
// idle timer, so a run pinned to it takes any positive cadence. Automatic
// selection is unresolved and must defer the bound until Prepare verifies its
// final bus; rejecting it earlier prevents a valid WebSocket fallback.
func PingIntervalBoundApplies(latencyTransport string) bool {
	return latencyTransport == wire.TransportWebTransport
}

// ValidateThroughputTransport reports whether name is a transport this client
// can select a transfer over. wire.TransportWebTransportDatagram is a known name
// refused later, by selectTarget, where the reason it cannot carry a transfer
// can be given. The message is user-facing: the CLI prints it beside the flag
// and Prepare returns it.
func ValidateThroughputTransport(name string) error {
	switch name {
	case "", "auto", wire.TransportFetchStream, wire.TransportWebTransport, wire.TransportWebTransportDatagram:
		return nil
	}
	return fmt.Errorf("invalid throughput transport %q: use auto, %s, or %s", name, wire.TransportFetchStream, wire.TransportWebTransport)
}

// ValidateLatencyTransport reports whether name is a bus this client can run the
// ping chain over. The empty name is what an unset field carries; normalized()
// reads it as automatic.
func ValidateLatencyTransport(name string) error {
	switch name {
	case "", "auto", wire.TransportWebSocket, wire.TransportWebTransport:
		return nil
	}
	return fmt.Errorf("invalid latency transport %q: use auto, %s, or %s", name, wire.TransportWebSocket, wire.TransportWebTransport)
}

// ResolveWebTransport is the WebTransport count: one continuous stream per
// direction, since nothing turns around per request; a forced count passes,
// clamped to wire.WTMaxStreams, which the server refuses an upload lane past.
func (p TransferStreamPolicy) ResolveWebTransport() int {
	if p.Forced > 0 {
		return min(p.Forced, wire.WTMaxStreams)
	}
	return 1
}

// Label describes the resolved policy for the transport that will carry it, so
// it reports what a session delivers rather than what was asked of it. The
// protocol is accepted in either spelling: a caller displaying a run holds the
// negotiated evidence ("h3"), not the target's name for it.
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
	// A multiplexed protocol resolves a count per direction, so one number would
	// name a lane count the client does not open.
	if lanes, ok := multiplexedStreams[protocol]; ok {
		return fmt.Sprintf("Automatic · %d download / %d upload", lanes.down, lanes.up)
	}
	if protocol == "http1" || protocol == "http1-clear" || protocol == "http1-tls" {
		return fmt.Sprintf("Automatic · up to %d per direction", p.AutomaticMax)
	}
	return "Automatic"
}

type Config struct {
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
	if c.BaseURL == "" {
		c.BaseURL = "http://127.0.0.1:7246"
	}
	if c.ThroughputTarget == "" {
		c.ThroughputTarget = "auto"
	}
	if c.ThroughputProtocol == "" {
		c.ThroughputProtocol = "auto"
	}
	if c.ThroughputTransport == "" {
		c.ThroughputTransport = "auto"
	}
	if c.LatencyTarget == "" {
		c.LatencyTarget = "auto"
	}
	if c.LatencyTransport == "" {
		c.LatencyTransport = "auto"
	}
	c.Warmup = max(c.Warmup, 0)
	if c.LatencyDuration <= 0 {
		c.LatencyDuration = defaultLatencyDuration
	}
	if c.DownloadDuration <= 0 {
		c.DownloadDuration = defaultTransferDuration
	}
	if c.UploadDuration <= 0 {
		c.UploadDuration = defaultTransferDuration
	}
	if c.BidirectionalDuration <= 0 {
		c.BidirectionalDuration = defaultTransferDuration
	}
	c.TransferStreams.Forced = max(c.TransferStreams.Forced, 0)
	if c.TransferStreams.AutomaticMax < 1 {
		c.TransferStreams.AutomaticMax = defaultAutomaticStreams
	}
	c.TransferStreams.AutomaticMax = min(c.TransferStreams.AutomaticMax, maxTransferStreams)
	c.TransferStreams.Forced = min(c.TransferStreams.Forced, maxTransferStreams)
	if c.PingInterval <= 0 {
		c.PingInterval = 250 * time.Millisecond
	}
	if c.DownloadBytesPerStream <= 0 {
		c.DownloadBytesPerStream = 64 * 1024 * 1024 * 1024
	}
	if c.UploadBytesPerStream <= 0 {
		c.UploadBytesPerStream = 64 * 1024 * 1024 * 1024
	}
	if c.MaxIdleConnsPerHost <= 0 {
		c.MaxIdleConnsPerHost = 256
	}
	if c.ResponseHeaderTimeout <= 0 {
		c.ResponseHeaderTimeout = 10 * time.Second
	}
	if c.ExpectContinueTimeout <= 0 {
		c.ExpectContinueTimeout = time.Second
	}
	return c
}
