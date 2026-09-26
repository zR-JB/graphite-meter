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

// Stage names one scheduled measurement stage.
type Stage string

const (
	StageLatency       Stage = "latency"
	StageDownload      Stage = "download"
	StageUpload        Stage = "upload"
	StageBidirectional Stage = "bidirectional"
)

// Phase is a stage's position: connect, warm up, measure.
type Phase int

const (
	PhasePreparing Phase = iota
	PhaseWarmup
	PhaseMeasuring
	PhaseFinished
)

// Outcome classifies a run; every value but OutcomeRunning is final.
type Outcome string

const (
	OutcomeRunning    Outcome = "running"
	OutcomeComplete   Outcome = "complete"   // Every stage finished with every selected server.
	OutcomePartial    Outcome = "partial"    // Every stage finished; a server or latency population dropped out.
	OutcomeIncomplete Outcome = "incomplete" // A stage ended without its result after measurement began.
	OutcomeStopped    Outcome = "stopped"    // The operator cancelled the run.
	OutcomeFailed     Outcome = "failed"     // The run ended before any stage measured.
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

// Event is one run message. Live samples may be dropped; other kinds are delivered.
type Event struct {
	Kind       EventKind
	At         time.Time
	Stage      Stage
	Phase      Phase
	Direction  Direction
	ServerID   string // Latency samples and server failures.
	Throughput ThroughputSample
	Latency    LatencySample
	Result     *Result        // The combined transfer result of one stage direction.
	Servers    *RunDetails    // Membership and per-server results; EventDone carries the final copy.
	Failure    *ServerFailure // A server or its latency population left the run.
	Err        error          // EventDone: why the run did not complete.
}

// Outcome classifies a terminal event, including one without server details.
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

// ThroughputSample is the latest combined window rate.
type ThroughputSample struct {
	Unavailable bool // No window covers every participant, for example right after a dropout.
	BytesPerSec float64
	TotalBytes  uint64 // Moved in this stage and direction so far, across every participant.
}

// LatencySample is one resolved probe to the server named by its event.
type LatencySample struct {
	RTT       time.Duration
	UnderLoad bool
	TimedOut  bool
}

// Result is one stage population; latency populations have no direction.
type Result struct {
	Stage       Stage
	Direction   Direction
	Unavailable bool
	MeanBps     float64 // Bytes per second over the measured window.
	PeakBps     float64 // Highest sampled window, never a sum of independent peaks.
	TotalBytes  uint64  // Every byte moved in the stage, including windows without a rate.
	Samples     int
	Latency     LatencyStats
	Elapsed     time.Duration // Length of the measured window behind MeanBps or the latency population.
	Err         error         // Non-nil marks an incomplete stage summary and preserves its failure.
}

// ReceiverTimed reports whether the rate uses the receiver's clock.
func (r Result) ReceiverTimed() bool { return r.Direction == Up }

// LatencyStats summarizes one stage's application probes. Durations use the client monotonic clock.
type LatencyStats struct {
	ReflectorTiming                    *ReflectorTimingStats // Nil when no valid timing pairs were observed.
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

// ReflectorTimingStats contains means over one paired population of successful in-window replies.
// Adjusted RTT removes only the instrumented server application handling interval.
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

// HasObservations distinguishes a measured partial population from a failure before any probes were measured.
func (s LatencyStats) HasObservations() bool {
	return s.Count+s.Timeouts+s.Unresolved+s.SendFailures > 0
}
