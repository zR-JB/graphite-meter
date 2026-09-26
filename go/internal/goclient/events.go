package goclient

import (
	"context"
	"errors"
	"time"
)

type Direction string

const (
	Down Direction = "down"
	Up   Direction = "up"
)

type Stage string

const (
	StageLatency       Stage = "latency"
	StageDownload      Stage = "download"
	StageUpload        Stage = "upload"
	StageBidirectional Stage = "bidirectional"
)

type Phase int

const (
	PhasePreparing Phase = iota
	PhaseWarmup
	PhaseMeasuring
	PhaseFinished
)

type Outcome string

const (
	OutcomeRunning    Outcome = "running"
	OutcomeComplete   Outcome = "complete"
	OutcomePartial    Outcome = "partial"
	OutcomeIncomplete Outcome = "incomplete"
	OutcomeStopped    Outcome = "stopped"
	OutcomeFailed     Outcome = "failed"
)

type EventKind int

const (
	EventStage EventKind = iota
	EventThroughput
	EventLatency
	EventResult
	EventServers
	EventServerFailure
	EventDone
)

type Event struct {
	Kind       EventKind
	At         time.Time
	Stage      Stage
	Phase      Phase
	Direction  Direction
	ServerID   string
	Throughput ThroughputSample
	Latency    LatencySample
	Result     *Result
	Servers    *RunDetails
	Failure    *ServerFailure
	Err        error
}

func (e Event) Outcome() Outcome {
	switch {
	case e.Servers != nil:
		return e.Servers.Outcome
	case errors.Is(e.Err, context.Canceled):
		return OutcomeStopped
	case e.Err != nil:
		return OutcomeFailed
	}
	return OutcomeComplete
}

type ThroughputSample struct {
	Unavailable bool
	BytesPerSec float64
	TotalBytes  uint64
}

type LatencySample struct {
	RTT      time.Duration
	TimedOut bool
}

type Result struct {
	Stage       Stage
	Direction   Direction
	Unavailable bool
	MeanBps     float64
	PeakBps     float64
	TotalBytes  uint64
	Samples     int
	Latency     LatencyStats
	Elapsed     time.Duration
	Err         error
}

func (r Result) ReceiverTimed() bool { return r.Direction == Up }

// LatencyStats summarizes one stage's application probes. Durations use the client monotonic clock.
type LatencyStats struct {
	ReflectorTiming *ReflectorTimingStats // Nil when no valid timing pairs were observed.
	P50, P95        time.Duration
	Jitter          time.Duration
	Count           int // Successful replies within the measured stage and probe deadline.
	JitterPairs     int // Zero means variation is unavailable, not zero.
	Timeouts        int
	Unresolved      int
	SendFailures    int
	Elapsed         time.Duration
}

// ReflectorTimingStats contains means over one paired population of successful in-window replies.
type ReflectorTimingStats struct {
	Count                                     int
	MeanRawRTT, MeanHandling, MeanAdjustedRTT time.Duration
}

// TimeoutRatio excludes interrupted/unresolved probes and local send failures; an empty population is unavailable.
func (s LatencyStats) TimeoutRatio() (float64, bool) {
	resolved := s.Count + s.Timeouts
	if resolved == 0 {
		return 0, false
	}
	return float64(s.Timeouts) / float64(resolved), true
}
