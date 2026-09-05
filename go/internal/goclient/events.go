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
	EventDone
)

type StagePhase string

const (
	StagePreparing StagePhase = "prepare"
	StageWarmup    StagePhase = "warmup"
	StageMeasuring StagePhase = "measure"
	StageFinished  StagePhase = "finished"
)

type Event struct {
	Kind                                  EventKind
	At                                    time.Time
	Stage                                 string
	Phase                                 StagePhase
	Direction                             Direction
	Message                               string
	ThroughputTarget, LatencyTarget       string
	ThroughputProtocol, LatencyProtocol   string
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
	Lost      bool // Compatibility event name: true only for an application probe deadline expiry.
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
	Err        error // Non-nil marks an incomplete stage summary and preserves its failure.
}

// LatencyStats summarizes one stage's application probes. Durations use the client monotonic clock.
type LatencyStats struct {
	Min, Max, P10, P50, P90, P95, Mean time.Duration
	Jitter                             time.Duration
	Count                              int // Successful replies within the measured stage and probe deadline.
	JitterPairs                        int // Zero means variation is unavailable, not zero.
	Timeouts                           int
	Unresolved                         int
	SendFailures                       int
	TimeoutAfter                       time.Duration
	Elapsed                            time.Duration
}

// TimeoutRatio excludes interrupted/unresolved probes and local send failures; an empty population is unavailable.
func (s LatencyStats) TimeoutRatio() (float64, bool) {
	resolved := s.Count + s.Timeouts
	if resolved == 0 {
		return 0, false
	}
	return float64(s.Timeouts) / float64(resolved), true
}

// HasObservations distinguishes a measured partial population from a failure before any probes were measured.
func (s LatencyStats) HasObservations() bool {
	return s.Count+s.Timeouts+s.Unresolved+s.SendFailures > 0
}
