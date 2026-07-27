package goclient

import (
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

type Direction string

const (
	Down Direction = "down"
	Up   Direction = "up"
)

type EventKind int

const (
	EventPreflight EventKind = iota
	EventStage
	EventThroughput
	EventLatency
	EventResult
	EventComplete
	EventError
)

type Event struct {
	Kind                                EventKind
	At                                  time.Time
	Stage                               string
	Direction                           Direction
	Message                             string
	ThroughputTarget, LatencyTarget     string
	ThroughputProtocol, LatencyProtocol string
	// The transports the run committed to. A consumer needs the throughput one
	// to describe the lanes a session delivers rather than the count that was
	// requested, and the latency one because an evicted ping means packet loss
	// on a datagram bus and a stalled queue on a WebSocket.
	ThroughputTransport, LatencyTransport string

	Preflight    *wire.Preflight
	Probe        *wire.Probe
	LatencyProbe *wire.Probe
	Throughput   ThroughputSample
	Latency      LatencySample
	Result       *Result
	Err          error
}

type ThroughputSample struct {
	Stage       string
	Direction   Direction
	BytesPerSec float64
	TotalBytes  uint64
	StreamCount int
	ServerAuth  bool
}

type LatencySample struct {
	Stage     string
	RTT       time.Duration
	UnderLoad bool
	Lost      bool
}

type Result struct {
	Stage      string
	Direction  Direction
	MeanBps    float64
	PeakBps    float64
	TotalBytes uint64
	Samples    int
	ServerAuth bool
	Latency    LatencyStats
	Elapsed    time.Duration
}

type LatencyStats struct {
	Min    time.Duration
	P50    time.Duration
	P95    time.Duration
	Mean   time.Duration
	Jitter time.Duration
	Loss   float64
	Count  int
}
